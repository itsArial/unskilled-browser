//! Platforms without TPM support here (Windows, macOS): the vault falls back to a passphrase.

#[derive(Debug)]
pub enum TpmError {
    Unavailable(String),
    WrongPin,
    LockedOut,
    Other(String),
}

impl std::fmt::Display for TpmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(e) | Self::Other(e) => write!(f, "TPM unavailable: {e}"),
            Self::WrongPin => write!(f, "wrong PIN"),
            Self::LockedOut => write!(f, "TPM locked"),
        }
    }
}

pub struct Sealed {
    pub public: Vec<u8>,
    pub private: Vec<u8>,
}

pub fn available() -> bool {
    false
}

pub fn seal(_secret: &[u8], _auth: &[u8]) -> Result<Sealed, TpmError> {
    Err(TpmError::Unavailable("not supported on this platform".into()))
}

pub fn unseal(_sealed: &Sealed, _auth: &[u8]) -> Result<Vec<u8>, TpmError> {
    Err(TpmError::Unavailable("not supported on this platform".into()))
}
