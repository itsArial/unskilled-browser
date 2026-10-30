//! Encrypted profile behind a PIN.
//!
//! At rest everything lives in one encrypted file. While the browser runs, the profile is
//! unpacked into RAM (`$XDG_RUNTIME_DIR`, a tmpfs) so plaintext never reaches the disk. On close
//! only an allowlist (logins, site storage) is packed back — history and caches are gone.
//! The data key is sealed in the TPM with the PIN as its password when we can use the TPM,
//! otherwise wrapped by an argon2id-stretched passphrase.

mod archive;
mod crypto;
#[cfg_attr(not(target_os = "linux"), path = "tpm_none.rs")]
mod tpm;

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use crypto::{Kdf, Key};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

const KEY_FILE: &str = "key.json";
const DATA_FILE: &str = "vault.bin";
const PREVIOUS_FILE: &str = "vault.bin.prev";
const DATA_AAD: &[u8] = b"unskilled-vault-data-v1";
const KEY_AAD: &[u8] = b"unskilled-vault-key-v1";

/// Minimum PIN length with a TPM (lockout protects it) and passphrase length without one.
pub const MIN_PIN: usize = 6;
pub const MIN_PASSPHRASE: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protection {
    Tpm,
    Passphrase,
}

impl Protection {
    pub fn min_len(self) -> usize {
        match self {
            Self::Tpm => MIN_PIN,
            Self::Passphrase => MIN_PASSPHRASE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// No vault yet; `protection` is what `create` will use.
    New { protection: Protection },
    Locked { protection: Protection },
}

#[derive(Debug)]
pub enum Error {
    WrongPin,
    LockedOut,
    TooShort(usize),
    TpmUnavailable(String),
    AlreadyRunning,
    Corrupt(String),
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongPin => write!(f, "wrong pin"),
            Self::LockedOut => write!(f, "too many wrong pins — the TPM is locked for a while"),
            Self::TooShort(n) => write!(f, "use at least {n} characters"),
            Self::TpmUnavailable(e) => write!(f, "the TPM that protects this vault is unavailable: {e}"),
            Self::AlreadyRunning => write!(f, "unskilled is already running"),
            Self::Corrupt(e) => write!(f, "vault is damaged: {e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<tpm::TpmError> for Error {
    fn from(e: tpm::TpmError) -> Self {
        match e {
            tpm::TpmError::WrongPin => Self::WrongPin,
            tpm::TpmError::LockedOut => Self::LockedOut,
            tpm::TpmError::Unavailable(e) => Self::TpmUnavailable(e),
            tpm::TpmError::Other(e) => Self::TpmUnavailable(e),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "protection", rename_all = "lowercase")]
enum KeyFile {
    Tpm { salt: String, public: String, private: String },
    Passphrase { salt: String, kdf: Kdf, wrapped: String },
}

pub struct Vault {
    dir: PathBuf,
    ram_root: PathBuf,
    /// Held for the vault's lifetime: one browser per vault.
    _lock: File,
    tpm: bool,
}

impl Vault {
    /// Opens (creating if needed) the vault in `dir`; the session will live in
    /// `$XDG_RUNTIME_DIR/unskilled-browser`. Fails if another instance holds it.
    pub fn open(dir: PathBuf) -> Result<Self, Error> {
        Self::open_with(dir, session_root()?, tpm::available())
    }

    fn open_with(dir: PathBuf, ram_root: PathBuf, tpm: bool) -> Result<Self, Error> {
        private_dir(&dir)?;
        let lock = private_file().create(true).truncate(false).write(true).open(dir.join("lock"))?;
        match lock.try_lock() {
            Ok(()) => Ok(Self { dir, ram_root, _lock: lock, tpm }),
            Err(fs::TryLockError::WouldBlock) => Err(Error::AlreadyRunning),
            Err(fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    /// The machine has a TPM we're not allowed to use (`/dev/tpmrm0` belongs to the `tss` group).
    pub fn tpm_denied(&self) -> bool {
        !self.tpm && Path::new("/dev/tpmrm0").exists()
    }

    pub fn state(&self) -> State {
        match self.key_file() {
            Some(KeyFile::Tpm { .. }) => State::Locked { protection: Protection::Tpm },
            Some(KeyFile::Passphrase { .. }) => State::Locked { protection: Protection::Passphrase },
            None => State::New { protection: if self.tpm { Protection::Tpm } else { Protection::Passphrase } },
        }
    }

    fn key_file(&self) -> Option<KeyFile> {
        serde_json::from_slice(&fs::read(self.dir.join(KEY_FILE)).ok()?).ok()
    }

    /// Creates a new vault protected by `pin` and opens an empty session.
    pub fn create(&self, pin: &str) -> Result<Session, Error> {
        let protection = match self.state() {
            State::New { protection } => protection,
            State::Locked { .. } => return Err(Error::Corrupt("a vault already exists".into())),
        };
        if pin.chars().count() < protection.min_len() {
            return Err(Error::TooShort(protection.min_len()));
        }
        let key = crypto::new_key();
        let salt = crypto::random::<16>();
        let key_file = match protection {
            Protection::Tpm => {
                let auth = Kdf::TPM_AUTH.derive(pin, &salt);
                let sealed = tpm::seal(key.as_slice(), auth.as_slice())?;
                KeyFile::Tpm { salt: B64.encode(salt), public: B64.encode(sealed.public), private: B64.encode(sealed.private) }
            }
            Protection::Passphrase => {
                let kek = Kdf::PASSPHRASE.derive(pin, &salt);
                KeyFile::Passphrase {
                    salt: B64.encode(salt),
                    kdf: Kdf::PASSPHRASE,
                    wrapped: B64.encode(crypto::encrypt(&kek, KEY_AAD, key.as_slice())),
                }
            }
        };
        // A leftover RAM session can't belong to a vault that didn't exist.
        let _ = fs::remove_dir_all(&self.ram_root);
        let session = Session::start(&self.dir, &self.ram_root, key)?;
        write_atomic(&self.dir.join(KEY_FILE), &serde_json::to_vec_pretty(&key_file).unwrap())?;
        session.save()?;
        Ok(session)
    }

    /// A passphrase vault on a machine whose TPM we can now use: it can switch to a PIN.
    pub fn can_upgrade(&self) -> bool {
        self.tpm && matches!(self.key_file(), Some(KeyFile::Passphrase { .. }))
    }

    /// Re-seals the open session's data key in the TPM under `pin`, replacing the passphrase.
    /// The encrypted data itself doesn't change.
    pub fn upgrade_to_pin(&self, session: &Session, pin: &str) -> Result<(), Error> {
        if !self.tpm {
            return Err(Error::TpmUnavailable("no access to /dev/tpmrm0".into()));
        }
        if pin.chars().count() < MIN_PIN {
            return Err(Error::TooShort(MIN_PIN));
        }
        let salt = crypto::random::<16>();
        let auth = Kdf::TPM_AUTH.derive(pin, &salt);
        let sealed = tpm::seal(session.key.as_slice(), auth.as_slice())?;
        let key_file = KeyFile::Tpm { salt: B64.encode(salt), public: B64.encode(sealed.public), private: B64.encode(sealed.private) };
        write_atomic(&self.dir.join(KEY_FILE), &serde_json::to_vec_pretty(&key_file).unwrap())?;
        Ok(())
    }

    /// Checks `pin` without touching the session (before a saved password is used). With a TPM a
    /// wrong PIN counts toward its lockout, exactly like at unlock.
    pub fn verify(&self, pin: &str) -> Result<(), Error> {
        match self.key_file().ok_or_else(|| Error::Corrupt("missing key file".into()))? {
            KeyFile::Tpm { salt, public, private } => {
                let auth = Kdf::TPM_AUTH.derive(pin, &decode(&salt)?);
                let sealed = tpm::Sealed { public: decode(&public)?, private: decode(&private)? };
                zeroize::Zeroizing::new(tpm::unseal(&sealed, auth.as_slice())?);
            }
            KeyFile::Passphrase { salt, kdf, wrapped } => {
                let kek = kdf.derive(pin, &decode(&salt)?);
                let secret = crypto::decrypt(&kek, KEY_AAD, &decode(&wrapped)?).ok_or(Error::WrongPin)?;
                zeroize::Zeroizing::new(secret);
            }
        }
        Ok(())
    }

    /// Unlocks with `pin` and unpacks the session into RAM.
    pub fn unlock(&self, pin: &str) -> Result<Session, Error> {
        let key_file = self.key_file().ok_or_else(|| Error::Corrupt("missing key file".into()))?;
        let key = match key_file {
            KeyFile::Tpm { salt, public, private } => {
                let salt = decode(&salt)?;
                let auth = Kdf::TPM_AUTH.derive(pin, &salt);
                let sealed = tpm::Sealed { public: decode(&public)?, private: decode(&private)? };
                let secret = zeroize::Zeroizing::new(tpm::unseal(&sealed, auth.as_slice())?);
                to_key(&secret)?
            }
            KeyFile::Passphrase { salt, kdf, wrapped } => {
                let kek = kdf.derive(pin, &decode(&salt)?);
                let secret = crypto::decrypt(&kek, KEY_AAD, &decode(&wrapped)?).ok_or(Error::WrongPin)?;
                to_key(&zeroize::Zeroizing::new(secret))?
            }
        };

        // A RAM session left by a crash is newer than the archive: adopt it.
        if self.ram_root.join("profile").exists() || self.ram_root.join("settings").exists() {
            log::warn!("vault: recovering the session left by an unclean exit");
            let session = Session { key, vault_dir: self.dir.clone(), root: self.ram_root.clone() };
            session.clear_locks();
            return Ok(session);
        }

        let session = Session::start(&self.dir, &self.ram_root, key)?;
        let mut last_error = None;
        for name in [DATA_FILE, PREVIOUS_FILE] {
            let Ok(blob) = fs::read(self.dir.join(name)) else { continue };
            match crypto::decrypt(&session.key, DATA_AAD, &blob).map(|plain| archive::unpack(&plain, &session.root)) {
                Some(Ok(())) => return Ok(session),
                Some(Err(e)) => last_error = Some(e.to_string()),
                None => last_error = Some(format!("{name} failed authentication")),
            }
            log::warn!("vault: {name} unusable, trying the previous copy");
        }
        match last_error {
            None => Ok(session),
            Some(e) => {
                let _ = fs::remove_dir_all(&session.root);
                Err(Error::Corrupt(e))
            }
        }
    }
}

/// An unlocked vault: the profile lives under `root` (RAM) until `close`.
pub struct Session {
    key: Key,
    vault_dir: PathBuf,
    root: PathBuf,
}

impl Session {
    fn start(vault_dir: &Path, root: &Path, key: Key) -> Result<Self, Error> {
        private_dir(root)?;
        Ok(Self { key, vault_dir: vault_dir.to_owned(), root: root.to_owned() })
    }

    /// Chromium's profile directory.
    pub fn profile_dir(&self) -> PathBuf {
        self.root.join("profile")
    }

    /// Our own settings (kept in full).
    pub fn settings_dir(&self) -> PathBuf {
        self.root.join("settings")
    }

    /// Chromium's single-instance locks from a crashed run would block startup.
    fn clear_locks(&self) {
        for name in ["SingletonLock", "SingletonCookie", "SingletonSocket"] {
            let _ = fs::remove_file(self.profile_dir().join(name));
        }
    }

    /// Encrypts the allowlisted state to disk. The previous copy is kept as a fallback.
    pub fn save(&self) -> Result<(), Error> {
        let plain = zeroize::Zeroizing::new(archive::pack(&self.root)?);
        let blob = crypto::encrypt(&self.key, DATA_AAD, &plain);
        let current = self.vault_dir.join(DATA_FILE);
        if current.exists() {
            fs::rename(&current, self.vault_dir.join(PREVIOUS_FILE))?;
        }
        write_atomic(&current, &blob)?;
        Ok(())
    }

    /// Saves and erases the RAM copy. On failure the RAM copy is kept, so the next launch can
    /// recover it.
    pub fn close(self) -> Result<(), Error> {
        self.save()?;
        fs::remove_dir_all(&self.root)?;
        Ok(())
    }
}

fn decode(s: &str) -> Result<Vec<u8>, Error> {
    B64.decode(s).map_err(|e| Error::Corrupt(e.to_string()))
}

fn to_key(secret: &[u8]) -> Result<Key, Error> {
    let bytes: [u8; 32] = secret.try_into().map_err(|_| Error::Corrupt("bad key length".into()))?;
    Ok(zeroize::Zeroizing::new(bytes))
}

fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = private_file().create(true).truncate(true).write(true).open(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// Where the open session lives: RAM (`$XDG_RUNTIME_DIR`, a tmpfs) on Linux.
#[cfg(unix)]
fn session_root() -> Result<PathBuf, Error> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|d| PathBuf::from(d).join("unskilled-browser"))
        .ok_or_else(|| Error::Io(io::Error::other("XDG_RUNTIME_DIR is not set; refusing to unpack to disk")))
}

/// Windows has no per-user tmpfs: the open session lives in the local app-data folder and is
/// erased on close. Encryption at rest still covers the vault whenever the browser is closed.
#[cfg(windows)]
fn session_root() -> Result<PathBuf, Error> {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("unskilled-browser").join("session"))
        .ok_or_else(|| Error::Io(io::Error::other("LOCALAPPDATA is not set")))
}

/// A directory only we can read (0700; on Windows the user profile's ACL already is).
fn private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

/// Options for a file only we can read (0600).
fn private_file() -> OpenOptions {
    #[allow(unused_mut)]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

#[cfg(test)]
mod tests;
