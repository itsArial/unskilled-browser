//! SIGTERM/SIGINT/SIGHUP → a clean shutdown (logout and shutdown send SIGTERM, and the vault
//! must still be saved and the RAM copy erased). Chromium installs its own handlers while it
//! initializes, so `install` runs again afterwards to take them back.

use std::sync::{
    OnceLock,
    atomic::{AtomicI32, Ordering},
};

static WRITE_END: AtomicI32 = AtomicI32::new(-1);
static STARTED: OnceLock<()> = OnceLock::new();

extern "C" fn on_signal(_: libc::c_int) {
    let fd = WRITE_END.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: write(2) is async-signal-safe; a full pipe just drops the byte.
        unsafe { libc::write(fd, b"q".as_ptr().cast(), 1) };
    }
}

/// Installs the handlers; `on_quit` runs on a helper thread for each signal.
pub fn install(on_quit: impl Fn() + Send + 'static) {
    STARTED.get_or_init(|| {
        let mut fds = [0; 2];
        // SAFETY: pipe2 fills `fds` with two descriptors we own.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            log::warn!("signals: pipe failed; SIGTERM will not save the vault");
            return;
        }
        WRITE_END.store(fds[1], Ordering::Relaxed);
        let read_end = fds[0];
        std::thread::Builder::new()
            .name("signals".into())
            .spawn(move || {
                let mut byte = 0u8;
                // SAFETY: blocking read into a 1-byte buffer we own.
                while unsafe { libc::read(read_end, (&mut byte as *mut u8).cast(), 1) } == 1 {
                    on_quit();
                }
            })
            .expect("spawn signal thread");
    });
    reinstall();
}

/// Re-registers our handler (after Chromium replaced it).
pub fn reinstall() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: a zeroed sigaction with our handler and an empty mask is valid.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}
