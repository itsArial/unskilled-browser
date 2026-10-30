//! ZIP reader: the central directory, stored and DEFLATE entries, CRC-32 checks. No ZIP64,
//! encryption or multi-disk archives — extensions don't use them. Entry names are validated so an
//! archive can't write outside the folder it's extracted to.

use crate::inflate;
use std::fmt;

#[derive(Debug)]
pub enum Error {
    NotZip,
    Unsupported(&'static str),
    Corrupt(String),
    UnsafePath(String),
    TooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotZip => write!(f, "not a zip archive"),
            Self::Unsupported(what) => write!(f, "unsupported zip feature: {what}"),
            Self::Corrupt(why) => write!(f, "corrupt zip: {why}"),
            Self::UnsafePath(path) => write!(f, "unsafe path in archive: {path}"),
            Self::TooLarge => write!(f, "archive expands too far"),
        }
    }
}

/// Total uncompressed size we accept, and the most entries.
const MAX_TOTAL: usize = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;

pub struct Entry {
    /// `/`-separated, validated relative path.
    pub path: String,
    pub data: Vec<u8>,
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, Error> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| Error::Corrupt("truncated".into()))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, Error> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| Error::Corrupt("truncated".into()))
}

/// Every file in the archive (directories are implied by paths).
pub fn read(zip: &[u8]) -> Result<Vec<Entry>, Error> {
    // End of central directory: the last 0x06054b50 within the trailing comment window.
    let min = zip.len().saturating_sub(22 + 0xFFFF);
    let eocd = (min..zip.len().saturating_sub(21))
        .rev()
        .find(|&i| zip[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or(Error::NotZip)?;
    let count = u16_at(zip, eocd + 10)? as usize;
    let mut at = u32_at(zip, eocd + 16)? as usize;
    if count == 0xFFFF || at == 0xFFFF_FFFF {
        return Err(Error::Unsupported("zip64"));
    }
    if count > MAX_ENTRIES {
        return Err(Error::TooLarge);
    }

    let mut entries = Vec::with_capacity(count);
    let mut total = 0usize;
    for _ in 0..count {
        if u32_at(zip, at)? != 0x0201_4b50 {
            return Err(Error::Corrupt("bad central directory entry".into()));
        }
        let flags = u16_at(zip, at + 8)?;
        let method = u16_at(zip, at + 10)?;
        let crc = u32_at(zip, at + 16)?;
        let packed = u32_at(zip, at + 20)? as usize;
        let size = u32_at(zip, at + 24)? as usize;
        let name_len = u16_at(zip, at + 28)? as usize;
        let extra_len = u16_at(zip, at + 30)? as usize;
        let comment_len = u16_at(zip, at + 32)? as usize;
        let local = u32_at(zip, at + 42)? as usize;
        let name = zip.get(at + 46..at + 46 + name_len).ok_or_else(|| Error::Corrupt("truncated name".into()))?;
        let name = String::from_utf8_lossy(name).into_owned();
        at += 46 + name_len + extra_len + comment_len;

        if flags & 1 != 0 {
            return Err(Error::Unsupported("encrypted entries"));
        }
        if name.ends_with('/') {
            continue;
        }
        let path = safe_path(&name)?;
        total = total.checked_add(size).filter(|&t| t <= MAX_TOTAL).ok_or(Error::TooLarge)?;

        // The local header repeats the name and its own extra field; the data follows it.
        if u32_at(zip, local)? != 0x0403_4b50 {
            return Err(Error::Corrupt(format!("bad local header for {path}")));
        }
        let start = local + 30 + u16_at(zip, local + 26)? as usize + u16_at(zip, local + 28)? as usize;
        let raw = zip.get(start..start + packed).ok_or_else(|| Error::Corrupt(format!("truncated data for {path}")))?;
        let data = match method {
            0 => raw.to_vec(),
            8 => inflate::inflate(raw, size).map_err(|e| Error::Corrupt(format!("{path}: {e}")))?,
            _ => return Err(Error::Unsupported("compression method")),
        };
        if data.len() != size || crc32(&data) != crc {
            return Err(Error::Corrupt(format!("{path}: checksum mismatch")));
        }
        entries.push(Entry { path, data });
    }
    Ok(entries)
}

/// Accepts `a/b/c.js`; rejects absolute paths, `..`, drive letters and empty segments.
fn safe_path(name: &str) -> Result<String, Error> {
    let name = name.replace('\\', "/");
    let bad = name.starts_with('/')
        || name.contains(':')
        || name.split('/').any(|s| s.is_empty() || s == "." || s == "..");
    if bad { Err(Error::UnsafePath(name)) } else { Ok(name) }
}

/// CRC-32 (IEEE 802.3, the one ZIP uses).
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        table
    });
    !data.iter().fold(!0u32, |c, &b| table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_entries() {
        let entries = read(include_bytes!("../tests/fixtures/ext.zip")).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, ["manifest.json", "_locales/en/messages.json", "icons/48.png", "content/cs.js"]);
        let cs = entries.iter().find(|e| e.path == "content/cs.js").unwrap();
        assert_eq!(cs.data, include_bytes!("../tests/fixtures/dynamic.txt"));
    }

    #[test]
    fn rejects_unsafe_paths() {
        for bad in ["/etc/passwd", "../x", "a/../../x", "C:/x", "a//b"] {
            assert!(safe_path(bad).is_err(), "{bad}");
        }
        assert_eq!(safe_path("a\\b.js").unwrap(), "a/b.js");
    }

    #[test]
    fn crc() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
