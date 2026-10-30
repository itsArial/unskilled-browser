//! `unskilled.dll`: the Windows bundle's browser. Since M138 a Chromium sandbox on Windows needs
//! CEF's prebuilt bootstrap.exe (shipped renamed to `unskilled.exe`, see `build.rs`): it sets up
//! the sandbox before any of our code runs, loads this DLL and hands the sandbox info over here,
//! in the browser and in every renderer, GPU and utility process.
#![cfg(windows)]

use std::ffi::c_void;

/// The entry point bootstrap.exe calls; the signature is CEF's `include/cef_sandbox_win.h`.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn RunWinMain(
    _instance: *mut c_void,
    _command_line: *const u16,
    _show: i32,
    sandbox_info: *mut c_void,
    _version_info: *mut c_void,
) -> i32 {
    unskilled::run(sandbox_info)
}
