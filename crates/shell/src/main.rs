// Release builds on Windows are GUI apps: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! The browser as a plain executable. On Windows this runs without Chromium's sandbox, which
//! needs the bundle's bootstrap.exe launcher (see `crates/winmain`); `cargo run` is for development.

fn main() {
    std::process::exit(unskilled::run(std::ptr::null_mut()));
}
