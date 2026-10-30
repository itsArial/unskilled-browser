//! CRX3 (Chrome extension package): `Cr24`, version 3, a protobuf header with the signing keys,
//! then a ZIP. We read the header to find the publisher key, which determines the extension's ID.
//! Signatures are not verified here: the user picks the file, and the ID is recomputed from the
//! key itself rather than trusted from the header.

use sha2::{Digest, Sha256};

pub struct Crx<'a> {
    /// DER SubjectPublicKeyInfo of the key the extension ID comes from.
    pub public_key: Vec<u8>,
    pub zip: &'a [u8],
}

pub fn parse(data: &[u8]) -> Result<Crx<'_>, String> {
    if data.get(..4) != Some(b"Cr24") {
        return Err("not a crx file".into());
    }
    let word = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    match word(4) {
        Some(3) => {}
        Some(2) => return Err("crx2 packages are no longer supported by Chrome; get a newer copy".into()),
        _ => return Err("unknown crx version".into()),
    }
    let header_len = word(8).ok_or("truncated crx")? as usize;
    let header = data.get(12..12 + header_len).ok_or("truncated crx header")?;
    let zip = &data[12 + header_len..];

    // CrxFileHeader: 2/3 = AsymmetricKeyProof { 1: public_key, 2: signature },
    // 10000 = signed_header_data (SignedData { 1: crx_id }).
    let mut keys = Vec::new();
    let mut crx_id = None;
    for (field, value) in Proto::new(header) {
        match field {
            2 | 3 => {
                if let Some((_, key)) = Proto::new(value).find(|(f, _)| *f == 1) {
                    keys.push(key.to_vec());
                }
            }
            10000 => crx_id = Proto::new(value).find(|(f, _)| *f == 1).map(|(_, id)| id.to_vec()),
            _ => {}
        }
    }
    let crx_id = crx_id.ok_or("crx header has no id")?;
    let public_key = keys
        .into_iter()
        .find(|k| Sha256::digest(k)[..16] == crx_id[..])
        .ok_or("no key in the crx header matches its id")?;
    Ok(Crx { public_key, zip })
}

/// Chrome's extension ID: the first 16 bytes of SHA-256(`bytes`), each nibble as `a`–`p`.
pub fn id_from(bytes: &[u8]) -> String {
    Sha256::digest(bytes)[..16].iter().flat_map(|b| [b >> 4, b & 15]).map(|n| (b'a' + n) as char).collect()
}

/// Length-delimited fields of a protobuf message (other wire types are skipped).
struct Proto<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Proto<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.data.get(self.pos)?;
            self.pos += 1;
            value |= ((byte & 0x7F) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
}

impl<'a> Iterator for Proto<'a> {
    type Item = (u64, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let key = self.varint()?;
            match key & 7 {
                0 => {
                    self.varint()?;
                }
                1 => self.pos += 8,
                5 => self.pos += 4,
                2 => {
                    let len = self.varint()? as usize;
                    let value = self.data.get(self.pos..self.pos.checked_add(len)?)?;
                    self.pos += len;
                    return Some((key >> 3, value));
                }
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_crx3() {
        let crx = parse(include_bytes!("../tests/fixtures/ext.crx")).unwrap();
        assert_eq!(crx.public_key, (0..200).collect::<Vec<u8>>());
        assert_eq!(id_from(&crx.public_key), "bjabnkbmjpgjjleipglcgdgogfmlphdk");
        assert_eq!(&crx.zip[..4], b"PK\x03\x04");
    }

    #[test]
    fn rejects_other_files() {
        assert!(parse(b"PK\x03\x04").is_err());
        assert!(parse(b"Cr24\x02\x00\x00\x00").is_err());
    }
}
