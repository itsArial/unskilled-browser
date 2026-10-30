//! Packs the session directory into one compressed blob, keeping only an allowlist: logins and
//! site storage survive a restart; history, caches and every other trace are dropped by
//! construction (a new Chromium version adding a history file can't leak it).

use std::{
    fs,
    io::{self, Read},
    path::{Component, Path},
};

/// Paths (relative to the session root) that survive. Directories end with `/`.
const KEEP: &[&str] = &[
    // Our own settings (shield exceptions, …).
    "settings/",
    "profile/First Run",
    // Sessions: cookies and the storage sites keep logins in.
    "profile/Default/Cookies",
    "profile/Default/Cookies-journal",
    "profile/Default/Local Storage/",
    "profile/Default/IndexedDB/",
    "profile/Default/WebStorage/",
    "profile/Default/File System/",
    "profile/Default/Storage/",
    "profile/Default/Service Worker/Database/",
    "profile/Default/Service Worker/ScriptCache/",
    "profile/Default/ClientCertificates/",
    // Extensions (P4).
    "profile/Default/Extensions/",
    "profile/Default/Extension Rules/",
    "profile/Default/Extension Scripts/",
    "profile/Default/Extension State/",
    "profile/Default/Local Extension Settings/",
];

/// Database bookkeeping that is recreated on open.
const SKIP_NAMES: &[&str] = &["LOG", "LOG.old", "LOCK"];

const MAGIC: &[u8; 8] = b"UNSKARC1";

pub fn keep(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if SKIP_NAMES.contains(&name) {
        return false;
    }
    KEEP.iter().any(|k| if k.ends_with('/') { rel.starts_with(k) } else { rel == *k })
}

/// Allowlisted regular files under `root` → compressed archive bytes.
pub fn pack(root: &Path) -> io::Result<Vec<u8>> {
    let mut raw = Vec::from(&MAGIC[..]);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            // Never follow symlinks (Chromium's Singleton* locks are symlinks).
            let Ok(meta) = fs::symlink_metadata(&path) else { continue };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let Some(rel) = path.strip_prefix(root).ok().and_then(|r| r.to_str()) else { continue };
            if !keep(rel) {
                continue;
            }
            // Files can vanish while Chromium runs (checkpoints); skip those.
            let Ok(data) = fs::read(&path) else { continue };
            raw.extend_from_slice(&(rel.len() as u32).to_le_bytes());
            raw.extend_from_slice(rel.as_bytes());
            raw.extend_from_slice(&(data.len() as u64).to_le_bytes());
            raw.extend_from_slice(&data);
        }
    }
    zstd::encode_all(&raw[..], 3)
}

pub fn unpack(archive: &[u8], root: &Path) -> io::Result<()> {
    let mut raw = Vec::new();
    zstd::Decoder::new(archive)?.read_to_end(&mut raw)?;
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "corrupt archive");
    let mut rest = raw.strip_prefix(&MAGIC[..]).ok_or_else(bad)?;
    while !rest.is_empty() {
        let (len, r) = rest.split_at_checked(4).ok_or_else(bad)?;
        let len = u32::from_le_bytes(len.try_into().unwrap()) as usize;
        let (path, r) = r.split_at_checked(len).ok_or_else(bad)?;
        let (size, r) = r.split_at_checked(8).ok_or_else(bad)?;
        let size = u64::from_le_bytes(size.try_into().unwrap()) as usize;
        let (data, r) = r.split_at_checked(size).ok_or_else(bad)?;
        rest = r;

        let rel = Path::new(std::str::from_utf8(path).map_err(|_| bad())?);
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(bad());
        }
        let target = root.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist() {
        assert!(keep("profile/Default/Cookies"));
        assert!(keep("profile/Default/Local Storage/leveldb/000003.log"));
        assert!(keep("settings/shield.json"));
        assert!(!keep("profile/Default/History"));
        assert!(!keep("profile/Default/Favicons"));
        assert!(!keep("profile/Default/Cache/Cache_Data/index"));
        assert!(!keep("profile/Default/Service Worker/CacheStorage/x"));
        assert!(!keep("profile/Default/Preferences"));
        assert!(!keep("profile/Local State"));
        assert!(!keep("profile/Default/Local Storage/leveldb/LOG"));
        assert!(!keep("profile/Default/TransportSecurity"));
    }

    #[test]
    #[cfg(unix)]
    fn round_trip_drops_history() {
        let base = std::env::temp_dir().join(format!("unskilled-archive-test-{}", std::process::id()));
        let src = base.join("src");
        let dst = base.join("dst");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(src.join("profile/Default/Local Storage/leveldb")).unwrap();
        fs::write(src.join("profile/Default/Cookies"), b"session=abc").unwrap();
        fs::write(src.join("profile/Default/History"), b"https://secret.example").unwrap();
        fs::write(src.join("profile/Default/Local Storage/leveldb/000003.log"), b"login").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", src.join("profile/SingletonLock")).unwrap();

        let archive = pack(&src).unwrap();
        unpack(&archive, &dst).unwrap();
        assert_eq!(fs::read(dst.join("profile/Default/Cookies")).unwrap(), b"session=abc");
        assert_eq!(fs::read(dst.join("profile/Default/Local Storage/leveldb/000003.log")).unwrap(), b"login");
        assert!(!dst.join("profile/Default/History").exists());
        assert!(!dst.join("profile/SingletonLock").exists());
        let _ = fs::remove_dir_all(&base);
    }
}
