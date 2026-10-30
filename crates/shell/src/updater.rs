//! Self-update for copies set up by the Windows installer (scripts/installer.nsi). The install is
//! `<root>\versions\<version>\` plus a `current` junction the shortcuts go through, like the Linux
//! install's symlink: a new release is unpacked next to the running version, `current` is flipped
//! to it, and the update watcher (update.rs) offers "update · restart".
//!
//! Nothing is fetched unless the user asks: the button in settings, or the opt-in check at start.
//! The download is checked against the release's published SHA-256 (it arrives over TLS from
//! GitHub; the checksum catches a truncated or corrupted download, not a compromised release).

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const RELEASES: &str = "https://api.github.com/repos/itsArial/unskilled-browser/releases?per_page=20";
/// The Windows bundle zip, compressed.
const MAX_BUNDLE: usize = 512 * 1024 * 1024;
const ASSET_SUFFIX: &str = "-windows-x86_64.zip";

#[derive(Clone, Default)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Downloading(String),
    Installing(String),
    /// Installed; applies on restart.
    Ready(String),
    Failed(String),
}

impl Status {
    pub fn json(&self) -> Value {
        match self {
            Self::Idle => json!({"state": "idle"}),
            Self::Checking => json!({"state": "checking"}),
            Self::UpToDate => json!({"state": "current"}),
            Self::Downloading(v) => json!({"state": "downloading", "version": v}),
            Self::Installing(v) => json!({"state": "installing", "version": v}),
            Self::Ready(v) => json!({"state": "ready", "version": v}),
            Self::Failed(e) => json!({"state": "failed", "error": e}),
        }
    }

    fn busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Downloading(_) | Self::Installing(_))
    }
}

/// Shared with the thread that unpacks a download.
pub type Shared = Arc<Mutex<Status>>;

fn set(status: &Shared, value: Status) {
    *status.lock().unwrap() = value;
}

/// `<root>` when this copy runs from an installed layout (`<root>\versions\<v>\unskilled.exe`).
/// Windows only: Linux installs update through scripts/install.sh.
pub fn install_root() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let exe = fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    let versions = exe.parent()?.parent()?;
    (versions.file_name()? == "versions").then(|| versions.parent().map(Path::to_path_buf))?
}

/// Looks for a newer release and installs it. `quiet` (the automatic check) leaves the status
/// alone unless an update is actually found.
pub fn check(status: &Shared, quiet: bool) {
    let Some(root) = install_root() else {
        return set(status, Status::Failed("this copy wasn't set up by the installer".into()));
    };
    if status.lock().unwrap().busy() {
        return;
    }
    if !quiet {
        set(status, Status::Checking);
    }
    let status = status.clone();
    engine::fetch(
        RELEASES,
        4 * 1024 * 1024,
        Box::new(move |result| match result.and_then(|body| newest(&body, env!("CARGO_PKG_VERSION"))) {
            Ok(Some(release)) if installed(&root, &release.version) => set(&status, Status::Ready(release.version)),
            Ok(Some(release)) => download(root, release, status),
            Ok(None) if !quiet => set(&status, Status::UpToDate),
            Err(e) if !quiet => set(&status, Status::Failed(e)),
            Err(e) => log::warn!("update check: {e}"),
            Ok(None) => {}
        }),
    );
}

struct Release {
    version: String,
    zip: String,
    sha256: String,
}

/// The newest release above `ours` that has a Windows bundle and its checksum.
fn newest(body: &[u8], ours: &str) -> Result<Option<Release>, String> {
    let releases: Value = serde_json::from_slice(body).map_err(|_| "GitHub sent something unexpected".to_string())?;
    let ours = version_key(ours);
    let mut best: Option<((u64, u64, u64), Release)> = None;
    for release in releases.as_array().ok_or("GitHub sent something unexpected")? {
        if release["draft"].as_bool().unwrap_or(true) {
            continue;
        }
        let Some(version) = release["tag_name"].as_str().map(|t| t.trim_start_matches('v').to_owned()) else { continue };
        let key = version_key(&version);
        if key <= ours || best.as_ref().is_some_and(|(b, _)| key <= *b) {
            continue;
        }
        let asset = |suffix: &str| {
            release["assets"].as_array()?.iter().find_map(|a| {
                a["name"].as_str()?.ends_with(suffix).then(|| a["browser_download_url"].as_str().map(str::to_owned))?
            })
        };
        let (Some(zip), Some(sha256)) = (asset(ASSET_SUFFIX), asset(&format!("{ASSET_SUFFIX}.sha256"))) else { continue };
        best = Some((key, Release { version, zip, sha256 }));
    }
    Ok(best.map(|(_, r)| r))
}

/// `1.2.3` (any `-suffix` ignored) as a comparable key.
fn version_key(version: &str) -> (u64, u64, u64) {
    let mut parts = version.split('-').next().unwrap_or_default().split('.').map(|p| p.parse().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// `version` is already unpacked and `current` points at it (downloaded, not yet restarted).
fn installed(root: &Path, version: &str) -> bool {
    let dir = root.join("versions").join(version);
    dir.join("unskilled.exe").is_file() && fs::canonicalize(root.join("current")).ok() == fs::canonicalize(&dir).ok()
}

fn download(root: PathBuf, release: Release, status: Shared) {
    let Release { version, zip, sha256 } = release;
    set(&status, Status::Downloading(version.clone()));
    engine::fetch(
        &sha256,
        4096,
        Box::new(move |sum| {
            // `<hex>  <file name>`, as sha256sum writes it.
            let expected = match sum {
                Ok(text) => String::from_utf8_lossy(&text).split_whitespace().next().unwrap_or_default().to_ascii_lowercase(),
                Err(e) => return set(&status, Status::Failed(e)),
            };
            if expected.len() != 64 {
                return set(&status, Status::Failed("the release's checksum file is malformed".into()));
            }
            engine::fetch(
                &zip,
                MAX_BUNDLE,
                Box::new(move |bundle| match bundle {
                    Err(e) => set(&status, Status::Failed(e)),
                    Ok(bytes) => {
                        set(&status, Status::Installing(version.clone()));
                        std::thread::spawn(move || {
                            let result = install(&root, &version, &bytes, &expected);
                            if let Err(e) = &result {
                                log::warn!("update to {version} failed: {e}");
                            }
                            set(&status, result.map_or_else(Status::Failed, |()| Status::Ready(version)));
                        });
                    }
                }),
            );
        }),
    );
}

/// Verifies `bundle`, unpacks it to `<root>\versions\<version>` and points `current` at it.
fn install(root: &Path, version: &str, bundle: &[u8], expected: &str) -> Result<(), String> {
    let actual: String = Sha256::digest(bundle).iter().map(|b| format!("{b:02x}")).collect();
    if actual != expected {
        return Err("the download doesn't match the release's checksum".into());
    }
    let versions = root.join("versions");
    remove_unused(&versions);
    let partial = versions.join(format!("{version}.partial"));
    let _ = fs::remove_dir_all(&partial);
    let entries = extensions::unzip(bundle).map_err(|e| e.to_string())?;
    for entry in entries {
        // Everything sits in one top folder, `unskilled-browser-<version>-windows-x86_64/`.
        let Some((_, path)) = entry.path.split_once('/').filter(|(_, p)| !p.is_empty()) else { continue };
        let path = partial.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("unpacking: {e}"))?;
        }
        fs::write(&path, &entry.data).map_err(|e| format!("unpacking: {e}"))?;
    }
    if !partial.join("unskilled.exe").is_file() {
        return Err("the download has no unskilled.exe".into());
    }
    let dir = versions.join(version);
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&partial, &dir).map_err(|e| format!("installing: {e}"))?;
    point_current(root, &dir).map_err(|e| format!("switching versions: {e}"))?;
    register_version(version);
    Ok(())
}

/// Removes versions left behind by earlier updates, in the background. Call once the vault is
/// open: then no other copy of the browser is running.
pub fn clean_up() {
    if let Some(root) = install_root() {
        std::thread::spawn(move || remove_unused(&root.join("versions")));
    }
}

/// Deletes every version but ours and the one `current` points at. Only one browser runs at a
/// time (the vault lock), so nothing else uses them.
fn remove_unused(versions: &Path) {
    let ours = std::env::current_exe().ok().and_then(|e| fs::canonicalize(e).ok()).and_then(|e| e.parent().map(Path::to_path_buf));
    let current = versions.parent().and_then(|root| fs::canonicalize(root.join("current")).ok());
    let Ok(dirs) = fs::read_dir(versions) else { return };
    for dir in dirs.flatten().map(|d| d.path()) {
        let canonical = fs::canonicalize(&dir).ok();
        if canonical.is_some() && (canonical == ours || canonical == current) {
            continue;
        }
        let _ = fs::remove_dir_all(&dir);
    }
}

/// Re-points the `current` junction: a new junction is made beside it, then swapped in. Removing a
/// junction removes the link only, never the version it points to.
#[cfg(windows)]
fn point_current(root: &Path, dir: &Path) -> std::io::Result<()> {
    let (current, next) = (root.join("current"), root.join("current.new"));
    let _ = fs::remove_dir(&next);
    let made = crate::console_tool("cmd").args(["/c", "mklink", "/J"]).arg(plain(&next)).arg(plain(dir)).status()?;
    if !made.success() {
        return Err(std::io::Error::other("mklink failed"));
    }
    match fs::remove_dir(&current) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    fs::rename(&next, &current)
}

#[cfg(not(windows))]
fn point_current(_root: &Path, _dir: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("self-update is for Windows installs"))
}

/// The version "Installed apps" shows for us.
#[cfg(windows)]
fn register_version(version: &str) {
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\unskilled-browser";
    let _ = crate::console_tool("reg").args(["add", key, "/v", "DisplayVersion", "/d", version, "/f"]).status();
}

#[cfg(not(windows))]
fn register_version(_version: &str) {}

/// `path` without the `\\?\` prefix `canonicalize` adds, which cmd's tools don't take.
pub fn plain(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_newest_windows_release() {
        let body = br#"[
            {"draft": false, "tag_name": "v0.3.0", "assets": [
                {"name": "unskilled-browser-0.3.0-linux-x86_64.tar.gz", "browser_download_url": "l"}]},
            {"draft": true, "tag_name": "v0.9.0", "assets": []},
            {"draft": false, "tag_name": "v0.2.1", "assets": [
                {"name": "unskilled-browser-0.2.1-windows-x86_64.zip", "browser_download_url": "z"},
                {"name": "unskilled-browser-0.2.1-windows-x86_64.zip.sha256", "browser_download_url": "s"}]},
            {"draft": false, "tag_name": "v0.2.0", "assets": [
                {"name": "unskilled-browser-0.2.0-windows-x86_64.zip", "browser_download_url": "z0"},
                {"name": "unskilled-browser-0.2.0-windows-x86_64.zip.sha256", "browser_download_url": "s0"}]}
        ]"#;
        let release = newest(body, "0.2.0").unwrap().unwrap();
        assert_eq!((release.version.as_str(), release.zip.as_str(), release.sha256.as_str()), ("0.2.1", "z", "s"));
        assert!(newest(body, "0.2.1").unwrap().is_none());
        assert!(newest(b"{}", "0.2.0").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn flips_the_current_junction() {
        let root = std::env::temp_dir().join(format!("unskilled-junction-{}", std::process::id()));
        let (a, b) = (root.join("versions").join("1.0.0"), root.join("versions").join("1.1.0"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(b.join("unskilled.exe"), b"").unwrap();
        point_current(&root, &a).unwrap();
        point_current(&root, &b).unwrap();
        assert_eq!(fs::canonicalize(root.join("current")).unwrap(), fs::canonicalize(&b).unwrap());
        assert!(a.exists() && !root.join("current.new").exists());
        assert!(installed(&root, "1.1.0") && !installed(&root, "1.0.0"));
        let _ = fs::remove_dir(root.join("current"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn compares_versions() {
        assert!(version_key("0.10.0") > version_key("0.9.9"));
        assert_eq!(version_key("1.2.3-rc1"), (1, 2, 3));
        assert_eq!(plain(Path::new(r"\\?\C:\Users\a b\x")), PathBuf::from(r"C:\Users\a b\x"));
    }
}
