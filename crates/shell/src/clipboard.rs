//! System clipboard: `wl-copy`/`wl-paste` on Wayland, the Win32 clipboard on Windows.

#[cfg(unix)]
pub fn paste() -> String {
    std::process::Command::new("wl-paste")
        .arg("--no-newline")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[cfg(unix)]
pub fn copy(text: &str) {
    use std::io::Write;
    if let Ok(mut child) = std::process::Command::new("wl-copy").stdin(std::process::Stdio::piped()).spawn() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

#[cfg(windows)]
pub fn paste() -> String {
    arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default()
}

#[cfg(windows)]
pub fn copy(text: &str) {
    if let Ok(mut c) = arboard::Clipboard::new() {
        let _ = c.set_text(text);
    }
}

/// Copies a password and clears it again after 30 s, unless something else was copied since.
pub fn copy_secret(secret: &str) {
    copy(secret);
    let secret = zeroize::Zeroizing::new(secret.to_owned());
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        if *paste() == **secret {
            copy("");
        }
    });
}
