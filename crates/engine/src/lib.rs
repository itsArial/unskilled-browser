//! Chromium (CEF 156) embedded windowless: pages render into GPU textures the shell composites.

mod devtools;
mod inspect;
mod net;
mod renderer;
mod sandbox;
mod tab;
mod webui;

pub use net::{Action, BlockEvent, SharedNet, TabNet};
pub use devtools::Wake;
pub use webui::{Host, IdentityLookup, set_host, set_identity_gate};
pub use sandbox::{forget as forget_sandbox, key_for as sandbox_for, list as sandboxes, set_enabled as set_sandboxes_enabled};
pub use inspect::{Inspect, Request, Tls};
pub use tab::{Button, ContextMenu, Cursor, Gpu, Key, KeyKind, MenuItem, Tab, TabState, commands, modifiers};

use cef::{args::Args, *};
use std::{path::PathBuf, sync::Arc};

/// Called (from any thread) when CEF wants `pump()` run after `delay_ms`.
pub type SchedulePump = Arc<dyn Fn(i64) + Send + Sync>;

pub struct Config {
    /// Profile directory (cookies, storage, cache).
    pub profile_dir: PathBuf,
    pub shield: Arc<shield::Shield>,
    pub schedule_pump: SchedulePump,
    /// Redraws the UI from any thread (identity prompts arrive on CEF's IO thread).
    pub wake: Wake,
}

/// The Chromium this build embeds (CEF 156).
pub const CHROMIUM_VERSION: &str = "156.0.8078.48";

/// Chromium relaunches this executable for its renderer, GPU and utility processes.
/// Call first thing in `main`; returns the exit code when this is one of those processes.
pub fn subprocess() -> Option<i32> {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = Args::new();
    let cmd = args.as_cmd_line()?;
    let is_browser = cmd.has_switch(Some(&CefString::from("type"))) == 0;
    let mut app = BrowserApp::new(None);
    let code = execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    (!is_browser).then_some(code)
}

pub fn initialize(config: Config) -> Result<(), String> {
    let _ = std::fs::create_dir_all(&config.profile_dir);
    write_preferences(&config.profile_dir);
    net::set_shield(config.shield);

    let args = Args::new();
    let profile = CefString::from(config.profile_dir.to_string_lossy().as_ref());
    let settings = Settings {
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        root_cache_path: profile.clone(),
        cache_path: profile,
        persist_session_cookies: 1,
        // The shell handles SIGTERM/SIGINT itself so the vault is saved before exit.
        disable_signal_handlers: 1,
        log_severity: LogSeverity::ERROR,
        locale: CefString::from("en-US"),
        // Opaque white under pages that don't paint a background.
        background_color: 0xFFFF_FFFF,
        ..Default::default()
    };
    let mut app = BrowserApp::new(Some(config.schedule_pump));
    if cef::initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()) != 1 {
        return Err("CEF failed to initialize".into());
    }
    net::register_scheme();
    webui::register_scheme();
    sandbox::init(&config.profile_dir);
    net::set_wake(config.wake);
    Ok(())
}

/// Blocks (or allows) third-party cookies for every page, in every sandbox.
pub fn set_third_party_cookies_blocked(blocked: bool) {
    if let Some(context) = request_context_get_global_context() {
        sandbox::apply_cookie_policy(&context, blocked);
    }
    sandbox::set_third_party_cookies_blocked(blocked);
}

/// A fresh Preferences file on every start (the vault never keeps Chromium's, which records site
/// engagement). "Continue where you left off" is what makes Chromium keep session cookies — the
/// logins we want — across restarts; its tab-restore files are dropped by the vault, so nothing
/// else comes back.
fn write_preferences(profile_dir: &std::path::Path) {
    let dir = profile_dir.join("Default");
    let _ = std::fs::create_dir_all(&dir);
    let prefs = r#"{"session":{"restore_on_startup":1}}"#;
    if let Err(e) = std::fs::write(dir.join("Preferences"), prefs) {
        log::warn!("engine: writing preferences failed: {e}");
    }
}

/// Runs pending Chromium work on the UI thread.
pub fn pump() {
    do_message_loop_work();
}

pub fn shutdown() {
    cef::shutdown();
}

wrap_app! {
    struct BrowserApp {
        schedule_pump: Option<SchedulePump>,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(cmd) = command_line else { return };
            if process_type.is_some_and(|t| !t.to_string().is_empty()) {
                return;
            }
            let switch = |name: &str| cmd.append_switch(Some(&CefString::from(name)));
            let value = |name: &str, value: &str| {
                cmd.append_switch_with_value(Some(&CefString::from(name)), Some(&CefString::from(value)))
            };
            // No phoning home: component updates, variations, crash uploads, pings, sync.
            switch("disable-background-networking");
            switch("disable-component-update");
            switch("disable-domain-reliability");
            switch("disable-breakpad");
            switch("disable-sync");
            switch("no-pings");
            switch("no-default-browser-check");
            switch("disable-client-side-phishing-detection");
            // Merge with CEF's own list: a second `--disable-features` would replace it.
            let ours = "Translate,OptimizationHints,MediaRouter,DialMediaRouteProvider,\
                        AutofillServerCommunication,InterestFeedContentSuggestions,\
                        CertificateTransparencyComponentUpdater,PrivacySandboxSettings4,\
                        BrowsingTopics,Fledge,InterestGroupStorage,AttributionReportingCrossAppWeb,\
                        FedCm";
            let existing = CefString::from(&cmd.switch_value(Some(&CefString::from("disable-features")))).to_string();
            let merged = if existing.is_empty() { ours.to_owned() } else { format!("{existing},{ours}") };
            value("disable-features", &merged);
            // WebRTC may only use the default route (the VPN), never leak local addresses.
            value("force-webrtc-ip-handling-policy", "default_public_interface_only");
            // Windowless rendering opens no native windows. The platform only matters for GPU
            // buffer allocation: shared (DMA-BUF) frames need X11/EGL — Chromium's Wayland
            // backend can't combine them with Vulkan — and headless has no GPU buffers at all.
            #[cfg(target_os = "linux")]
            {
                let ozone = std::env::var("UNSKILLED_OZONE").unwrap_or_else(|_| {
                    if std::env::var_os("DISPLAY").is_some() { "x11".into() } else { "headless".into() }
                });
                value("ozone-platform", &ozone);
                // ANGLE on Vulkan fences its DMA-BUF writes; on GL we'd import half-drawn frames.
                value("use-angle", "vulkan");
            }
            // Hardware-decoded video frames (NV12 shared images) crash the GPU process in
            // windowless mode on this stack; decode in software until that's solved.
            switch("disable-accelerated-video-decode");
            // Dev hook: extra Chromium switches, e.g. `UNSKILLED_CHROMIUM_FLAGS="use-angle=vulkan"`.
            if let Ok(extra) = std::env::var("UNSKILLED_CHROMIUM_FLAGS") {
                for flag in extra.split_whitespace() {
                    match flag.trim_start_matches('-').split_once('=') {
                        Some((k, v)) => value(k, v),
                        None => switch(flag.trim_start_matches('-')),
                    }
                }
            }
            switch("hide-crash-restore-bubble");
            switch("noerrdialogs");
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            let Some(registrar) = registrar else { return };
            // Standard + secure + CORS + CSP-bypassing: the renderer can fetch the page script
            // synchronously from any https page regardless of its CSP.
            let options = sys::cef_scheme_options_t::CEF_SCHEME_OPTION_STANDARD as i32
                | sys::cef_scheme_options_t::CEF_SCHEME_OPTION_SECURE as i32
                | sys::cef_scheme_options_t::CEF_SCHEME_OPTION_CORS_ENABLED as i32
                | sys::cef_scheme_options_t::CEF_SCHEME_OPTION_CSP_BYPASSING as i32;
            registrar.add_custom_scheme(Some(&CefString::from(net::SHIELD_SCHEME)), options);
            // Internal pages: isolated, so web pages can't link to or frame them.
            let pages = sys::cef_scheme_options_t::CEF_SCHEME_OPTION_STANDARD as i32
                | sys::cef_scheme_options_t::CEF_SCHEME_OPTION_SECURE as i32
                | sys::cef_scheme_options_t::CEF_SCHEME_OPTION_DISPLAY_ISOLATED as i32;
            registrar.add_custom_scheme(Some(&CefString::from(webui::SCHEME)), pages);
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(Pump::new(self.schedule_pump.clone()))
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(renderer::Renderer::new())
        }
    }
}

wrap_browser_process_handler! {
    struct Pump {
        schedule: Option<SchedulePump>,
    }

    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            if let Some(schedule) = &self.schedule {
                schedule(delay_ms);
            }
        }
    }
}
