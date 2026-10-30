fn main() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        // Installed bundles keep libcef.so next to the binary.
        Ok("linux") => println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN"),
        Ok("windows") => windows_resources(),
        _ => {}
    }
}

/// The eyepatch as the .exe's icon.
#[cfg(windows)]
fn windows_resources() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icons/unskilled-browser.ico");
    res.compile().expect("windows resources");
}

#[cfg(not(windows))]
fn windows_resources() {}
