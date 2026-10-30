use super::*;

struct Dirs {
    base: PathBuf,
}

impl Dirs {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("unskilled-vault-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        Self { base }
    }

    /// Passphrase protection: the TPM isn't used in tests.
    fn vault(&self) -> Result<Vault, Error> {
        Vault::open_with(self.base.join("vault"), self.base.join("ram"), false)
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

const PASS: &str = "correct horse battery";

#[test]
fn create_close_unlock_keeps_sessions_drops_history() {
    let dirs = Dirs::new("roundtrip");
    let vault = dirs.vault().unwrap();
    assert_eq!(vault.state(), State::New { protection: Protection::Passphrase });

    let session = vault.create(PASS).unwrap();
    let default = session.profile_dir().join("Default");
    fs::create_dir_all(&default).unwrap();
    fs::write(default.join("Cookies"), b"sid=123").unwrap();
    fs::write(default.join("History"), b"https://secret.example").unwrap();
    fs::create_dir_all(session.settings_dir()).unwrap();
    fs::write(session.settings_dir().join("shield.json"), b"{}").unwrap();
    let ram = session.root.clone();
    session.close().unwrap();
    assert!(!ram.exists(), "RAM copy must be erased on close");

    // Nothing readable on disk.
    let blob = fs::read(dirs.base.join("vault").join(DATA_FILE)).unwrap();
    assert!(!blob.windows(7).any(|w| w == b"sid=123"));

    assert_eq!(vault.state(), State::Locked { protection: Protection::Passphrase });
    let session = vault.unlock(PASS).unwrap();
    assert_eq!(fs::read(session.profile_dir().join("Default/Cookies")).unwrap(), b"sid=123");
    assert!(!session.profile_dir().join("Default/History").exists());
    assert!(session.settings_dir().join("shield.json").exists());
    session.close().unwrap();
}

#[test]
fn wrong_and_short_pins() {
    let dirs = Dirs::new("pins");
    let vault = dirs.vault().unwrap();
    assert!(matches!(vault.create("short"), Err(Error::TooShort(MIN_PASSPHRASE))));
    vault.create(PASS).unwrap().close().unwrap();
    assert!(matches!(vault.unlock("wrong horse battery"), Err(Error::WrongPin)));
    assert!(vault.verify(PASS).is_ok());
    assert!(matches!(vault.verify("wrong horse battery"), Err(Error::WrongPin)));
}

#[test]
fn single_instance() {
    let dirs = Dirs::new("lock");
    let _first = dirs.vault().unwrap();
    assert!(matches!(dirs.vault(), Err(Error::AlreadyRunning)));
}

#[test]
fn recovers_session_after_crash() {
    let dirs = Dirs::new("crash");
    let vault = dirs.vault().unwrap();
    let session = vault.create(PASS).unwrap();
    fs::create_dir_all(session.profile_dir().join("Default")).unwrap();
    fs::write(session.profile_dir().join("Default/Cookies"), b"newer").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("x", session.profile_dir().join("SingletonLock")).unwrap();
    // Simulated crash: no close(), the RAM copy stays behind.
    drop(session);

    let session = vault.unlock(PASS).unwrap();
    assert_eq!(fs::read(session.profile_dir().join("Default/Cookies")).unwrap(), b"newer");
    assert!(fs::symlink_metadata(session.profile_dir().join("SingletonLock")).is_err());
    session.close().unwrap();
}

#[test]
fn falls_back_to_previous_copy() {
    let dirs = Dirs::new("fallback");
    let vault = dirs.vault().unwrap();
    let session = vault.create(PASS).unwrap();
    fs::create_dir_all(session.profile_dir().join("Default")).unwrap();
    fs::write(session.profile_dir().join("Default/Cookies"), b"v1").unwrap();
    session.save().unwrap();
    session.close().unwrap();

    // Damage the newest copy; the previous save still has the data.
    let current = dirs.base.join("vault").join(DATA_FILE);
    let mut blob = fs::read(&current).unwrap();
    let mid = blob.len() / 2;
    blob[mid] ^= 0xFF;
    fs::write(&current, blob).unwrap();

    let session = vault.unlock(PASS).unwrap();
    assert_eq!(fs::read(session.profile_dir().join("Default/Cookies")).unwrap(), b"v1");
}

/// Debug aid: `UNSKILLED_VAULT_DIR=… UNSKILLED_VAULT_PASS=… cargo test -p unskilled-vault inspect -- --ignored --nocapture`
#[test]
#[ignore]
fn inspect() {
    let dir = PathBuf::from(std::env::var("UNSKILLED_VAULT_DIR").unwrap());
    let pass = std::env::var("UNSKILLED_VAULT_PASS").unwrap();
    let ram = std::env::temp_dir().join(format!("unskilled-vault-inspect-{}", std::process::id()));
    let vault = Vault::open_with(dir, ram.clone(), false).unwrap();
    let session = vault.unlock(&pass).unwrap();
    let mut stack = vec![ram.clone()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let data = fs::read(&p).unwrap();
                let hit = data.windows(9).any(|w| w == b"vaulttest");
                println!("{:>8}  {}{}", data.len(), p.strip_prefix(&ram).unwrap().display(), if hit { "  <- vaulttest" } else { "" });
            }
        }
    }
    drop(session);
    let _ = fs::remove_dir_all(ram);
}

/// Real TPM: seal, unlock, and one wrong PIN (each wrong PIN counts toward the TPM's lockout).
/// Needs `/dev/tpmrm0` access: `cargo test -p unskilled-vault tpm -- --ignored --nocapture`
#[test]
#[ignore]
fn tpm_round_trip() {
    assert!(tpm::available(), "no TPM access (tss group?)");
    let dirs = Dirs::new("tpm");
    let vault = Vault::open_with(dirs.base.join("vault"), dirs.base.join("ram"), true).unwrap();
    assert_eq!(vault.state(), State::New { protection: Protection::Tpm });
    assert!(matches!(vault.create("12345"), Err(Error::TooShort(MIN_PIN))));

    let started = std::time::Instant::now();
    let session = vault.create("482913").unwrap();
    println!("seal + create: {:?}", started.elapsed());
    fs::create_dir_all(session.profile_dir().join("Default")).unwrap();
    fs::write(session.profile_dir().join("Default/Cookies"), b"tpm-session").unwrap();
    session.close().unwrap();

    let key = fs::read_to_string(dirs.base.join("vault").join(KEY_FILE)).unwrap();
    assert!(key.contains("\"protection\": \"tpm\"") && !key.contains("wrapped"));

    let started = std::time::Instant::now();
    let session = vault.unlock("482913").unwrap();
    println!("unseal + unlock: {:?}", started.elapsed());
    assert_eq!(fs::read(session.profile_dir().join("Default/Cookies")).unwrap(), b"tpm-session");
    session.close().unwrap();

    let wrong = vault.unlock("000000");
    println!("wrong pin: {wrong:?}", wrong = wrong.as_ref().err());
    assert!(matches!(wrong, Err(Error::WrongPin)));
}

/// Real TPM: a passphrase vault switches to a PIN without losing data.
#[test]
#[ignore]
fn tpm_upgrade_from_passphrase() {
    assert!(tpm::available(), "no TPM access (tss group?)");
    let dirs = Dirs::new("upgrade");
    let passphrase_vault = Vault::open_with(dirs.base.join("vault"), dirs.base.join("ram"), false).unwrap();
    let session = passphrase_vault.create(PASS).unwrap();
    fs::create_dir_all(session.profile_dir().join("Default")).unwrap();
    fs::write(session.profile_dir().join("Default/Cookies"), b"kept").unwrap();
    session.close().unwrap();
    drop(passphrase_vault);

    let vault = Vault::open_with(dirs.base.join("vault"), dirs.base.join("ram"), true).unwrap();
    assert!(vault.can_upgrade());
    let session = vault.unlock(PASS).unwrap();
    vault.upgrade_to_pin(&session, "482913").unwrap();
    session.close().unwrap();

    assert_eq!(vault.state(), State::Locked { protection: Protection::Tpm });
    assert!(!vault.can_upgrade());
    let session = vault.unlock("482913").unwrap();
    assert_eq!(fs::read(session.profile_dir().join("Default/Cookies")).unwrap(), b"kept");
    session.close().unwrap();
}
