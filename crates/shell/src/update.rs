//! Notices when a newer build has been installed (scripts/install.sh on Linux, the installer or the
//! updater on Windows put each build in its own version directory and flip the `current` symlink or
//! junction), so the shell can offer a restart into it. The running version's files stay on disk
//! until it exits.

use std::{
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

const POLL: Duration = Duration::from_secs(20);

/// For an installed build: the `current` symlink next to our version directory.
fn current_link(exe: &Path) -> Option<PathBuf> {
    let version = exe.parent()?;
    let versions = version.parent()?;
    (versions.file_name()? == "versions").then(|| versions.parent().map(|root| root.join("current")))?
}

/// Our executable inside its version directory: started through `current`, Windows reports the
/// junction's path, so it is resolved (Linux already reports the real one).
fn own_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// What a restart should launch: the newest installed build, or ourselves when not installed.
pub fn restart_target() -> Option<PathBuf> {
    let exe = own_exe()?;
    let target = current_link(&exe).map_or(exe, |link| link.join(format!("unskilled{}", std::env::consts::EXE_SUFFIX)));
    Some(crate::updater::plain(&target))
}

/// Calls `on_update` once, when `current` points at a different version than ours.
pub fn watch(on_update: impl Fn() + Send + 'static) {
    let Some(exe) = own_exe() else { return };
    let (Some(link), Some(ours)) = (current_link(&exe), exe.parent().map(Path::to_path_buf)) else { return };
    let _ = thread::Builder::new().name("update-watch".into()).spawn(move || loop {
        thread::sleep(POLL);
        if std::fs::canonicalize(&link).is_ok_and(|current| current != ours) {
            on_update();
            return;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_current_link() {
        let exe = Path::new("/home/u/.local/lib/unskilled-browser/versions/20261030-1/unskilled");
        assert_eq!(current_link(exe), Some(PathBuf::from("/home/u/.local/lib/unskilled-browser/current")));
        assert_eq!(current_link(Path::new("/repo/target/release/unskilled")), None);
    }
}
