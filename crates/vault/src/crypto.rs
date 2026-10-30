//! XChaCha20-Poly1305 for data, argon2id for turning a PIN into key material.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use zeroize::Zeroizing;

pub type Key = Zeroizing<[u8; 32]>;
const NONCE: usize = 24;

pub fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("OS randomness");
    out
}

pub fn new_key() -> Key {
    Zeroizing::new(random::<32>())
}

/// `nonce ‖ ciphertext ‖ tag`, with `aad` authenticated but not stored.
pub fn encrypt(key: &Key, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(key.as_slice().try_into().expect("32-byte key"));
    let nonce = random::<NONCE>();
    let ciphertext = cipher
        .encrypt(&XNonce::from(nonce), Payload { msg: plaintext, aad })
        .expect("encryption cannot fail for in-memory buffers");
    let mut out = Vec::with_capacity(NONCE + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    out
}

/// `None` when the key is wrong or the data was tampered with.
pub fn decrypt(key: &Key, aad: &[u8], blob: &[u8]) -> Option<Vec<u8>> {
    if blob.len() < NONCE + 16 {
        return None;
    }
    let cipher = XChaCha20Poly1305::new(key.as_slice().try_into().ok()?);
    let (nonce, ciphertext) = blob.split_at(NONCE);
    let nonce = XNonce::try_from(nonce).ok()?;
    cipher.decrypt(&nonce, Payload { msg: ciphertext, aad }).ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Kdf {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Kdf {
    /// Without a TPM the KDF is the only thing slowing down guessing: make each guess expensive.
    pub const PASSPHRASE: Self = Self { m_kib: 256 * 1024, t: 3, p: 1 };
    /// With a TPM the lockout does the work; this just keeps the PIN itself off the bus.
    pub const TPM_AUTH: Self = Self { m_kib: 64 * 1024, t: 2, p: 1 };

    pub fn derive(self, pin: &str, salt: &[u8]) -> Key {
        let params = Params::new(self.m_kib, self.t, self.p, Some(32)).expect("valid argon2 params");
        let mut out = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(pin.as_bytes(), salt, out.as_mut_slice())
            .expect("argon2 with valid params");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tamper() {
        let key = new_key();
        let blob = encrypt(&key, b"aad", b"secret");
        assert_eq!(decrypt(&key, b"aad", &blob).unwrap(), b"secret");
        assert!(decrypt(&key, b"other", &blob).is_none());
        assert!(decrypt(&new_key(), b"aad", &blob).is_none());
        let mut bad = blob.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(decrypt(&key, b"aad", &bad).is_none());
    }

    #[test]
    fn kdf_is_deterministic_per_salt() {
        let k = Kdf { m_kib: 1024, t: 1, p: 1 };
        assert_eq!(*k.derive("1234", b"saltsalt"), *k.derive("1234", b"saltsalt"));
        assert_ne!(*k.derive("1234", b"saltsalt"), *k.derive("1235", b"saltsalt"));
        assert_ne!(*k.derive("1234", b"saltsalt"), *k.derive("1234", b"saltsal2"));
    }
}
