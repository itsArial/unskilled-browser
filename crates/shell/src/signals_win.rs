//! Logoff, shutdown and console Ctrl+C/close → a clean shutdown, as SIGTERM is on Linux: the
//! vault must still be saved and its working copy erased.
//!
//! Windows ends a session through the windows, not with a signal: WM_QUERYENDSESSION asks, then
//! WM_ENDSESSION announces it, and the process may be killed as soon as that returns. So the
//! handler asks the app to quit (the same event as SIGTERM) and dispatches messages until the app
//! has finished its shutdown — all of which runs while that event is dispatched. winit surfaces
//! neither message, so the window is subclassed.

use std::{
    sync::{Condvar, Mutex},
    time::Duration,
};
use windows_sys::{
    Win32::{
        Foundation::{FALSE, HWND, LPARAM, LRESULT, TRUE, WPARAM},
        System::{
            Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler},
            Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy},
        },
        UI::{
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::{
                DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, PostQuitMessage, TranslateMessage, WM_ENDSESSION,
                WM_QUERYENDSESSION, WM_QUIT,
            },
        },
    },
    core::BOOL,
};
use winit::{
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::Window,
};

/// Shown on Windows' "apps are preventing shutdown" screen if the save takes a while.
const REASON: &str = "Saving the vault";
/// How long a closing console window waits for the save (Windows ends the process after ~5 s).
const CONSOLE_GRACE: Duration = Duration::from_secs(10);

type OnQuit = Box<dyn Fn() + Send>;
static ON_QUIT: Mutex<Option<OnQuit>> = Mutex::new(None);
static FINISHED: Mutex<bool> = Mutex::new(false);
static FINISHED_CHANGED: Condvar = Condvar::new();

/// Installs the console handler (a console exists in debug builds only); `on_quit` runs for each
/// event, on the thread that saw it.
pub fn install(on_quit: impl Fn() + Send + 'static) {
    *ON_QUIT.lock().unwrap() = Some(Box::new(on_quit));
    // SAFETY: registers a plain function; it only touches the statics above.
    unsafe { SetConsoleCtrlHandler(Some(on_console), TRUE) };
}

/// Watches `window` for the end of the Windows session.
pub fn watch(window: &Window) {
    let Ok(RawWindowHandle::Win32(handle)) = window.window_handle().map(|h| h.as_raw()) else { return };
    let hwnd = handle.hwnd.get() as HWND;
    // SAFETY: subclasses our own window from its own thread; the procedure is a plain function.
    if unsafe { SetWindowSubclass(hwnd, Some(on_message), 1, 0) } == FALSE {
        log::warn!("signals: can't watch the window; logoff will not save the vault");
    }
}

/// The app's shutdown is done: the vault is saved (or kept for recovery).
pub fn finished() {
    *FINISHED.lock().unwrap() = true;
    FINISHED_CHANGED.notify_all();
}

fn quit() {
    if let Some(on_quit) = &*ON_QUIT.lock().unwrap() {
        on_quit();
    }
}

fn is_finished() -> bool {
    *FINISHED.lock().unwrap()
}

unsafe extern "system" fn on_console(event: u32) -> BOOL {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            quit();
            TRUE
        }
        // The console window is closing: the process ends once this returns.
        CTRL_CLOSE_EVENT => {
            quit();
            let finished = FINISHED.lock().unwrap();
            let _ = FINISHED_CHANGED.wait_timeout_while(finished, CONSOLE_GRACE, |finished| !*finished);
            TRUE
        }
        _ => FALSE,
    }
}

unsafe extern "system" fn on_message(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => {
            let reason: Vec<u16> = REASON.encode_utf16().chain([0]).collect();
            // SAFETY: our window, a NUL-terminated string that outlives the call.
            unsafe { ShutdownBlockReasonCreate(hwnd, reason.as_ptr()) };
            TRUE as LRESULT
        }
        WM_ENDSESSION => {
            if wparam != 0 {
                end_session();
            }
            // SAFETY: our window.
            unsafe { ShutdownBlockReasonDestroy(hwnd) };
            0
        }
        // SAFETY: forwards to the next procedure in the chain with the arguments we were given.
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

/// Queues the quit and dispatches until the shutdown it triggers has run.
fn end_session() {
    log::info!("session ending, shutting down");
    quit();
    // SAFETY: a zeroed MSG is valid, and this is the standard dispatch loop on the window's thread.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while !is_finished() && PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != FALSE {
            if msg.message == WM_QUIT {
                PostQuitMessage(msg.wParam as i32);
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if !is_finished() {
        // Arrived while the app was busy (inside Chromium's own message pump): the quit runs as
        // soon as that returns, but Windows may not wait for it.
        log::warn!("session ending mid-task; the vault save may be cut short");
    }
}
