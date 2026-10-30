//! Internal pages (`unskilled://settings`, `unskilled://bookmarks`), served from the binary, and
//! the hooks the engine uses to reach the shell: the pages' bridge calls, password capture and
//! the identity gate's decisions.

use crate::net::StaticResponse;
use cef::*;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

pub const SCHEME: &str = "unskilled";

/// Implemented by the shell: what the engine needs from the rest of the browser. UI thread.
pub trait Host {
    /// A bridge call from an internal page: `method` with JSON `params`; returns JSON.
    fn webui(&self, method: &str, params: &str) -> String;
    /// A page submitted a login (`url` is the frame's).
    fn login_submitted(&self, browser: i32, url: &str, username: &str, password: &str);
    /// A page shows a login form in frame `frame`; the shell may offer saved logins.
    fn login_form(&self, browser: i32, url: &str, frame: &str);
}

thread_local! {
    static HOST: RefCell<Option<Rc<dyn Host>>> = const { RefCell::new(None) };
}

pub fn set_host(host: Rc<dyn Host>) {
    HOST.with_borrow_mut(|h| *h = Some(host));
}

pub(crate) fn host() -> Option<Rc<dyn Host>> {
    HOST.with_borrow(|h| h.clone())
}

/// Identity-gate decisions, readable from CEF's IO thread: `(site, provider)` → share sign-in?
pub type IdentityLookup = Arc<dyn Fn(&str, &str) -> Option<bool> + Send + Sync>;

static IDENTITY: Mutex<Option<IdentityLookup>> = Mutex::new(None);

/// Turns the identity gate on (with how to look decisions up) or off (`None`).
pub fn set_identity_gate(lookup: Option<IdentityLookup>) {
    *IDENTITY.lock().unwrap() = lookup;
}

pub(crate) fn identity_gate() -> Option<IdentityLookup> {
    IDENTITY.lock().unwrap().clone()
}

static FONT: &[u8] = include_bytes!("../../ui/assets/fonts/JetBrainsMono-Regular.ttf");
static FONT_MEDIUM: &[u8] = include_bytes!("../../ui/assets/fonts/JetBrainsMono-Medium.ttf");

/// `page` is the URL's host (`settings`), `path` the rest. Every page serves the shared assets
/// under its own origin, so the pages' CSP can stay `'self'`.
fn asset(page: &str, path: &str) -> Option<(&'static str, &'static [u8])> {
    Some(match (page, path) {
        ("settings", "") => ("text/html", include_bytes!("../assets/webui/settings.html")),
        ("bookmarks", "") => ("text/html", include_bytes!("../assets/webui/bookmarks.html")),
        ("extensions", "") => ("text/html", include_bytes!("../assets/webui/extensions.html")),
        (_, "assets/app.css") => ("text/css", include_bytes!("../assets/webui/app.css")),
        (_, "assets/app.js") => ("application/javascript", include_bytes!("../assets/webui/app.js")),
        ("settings", "assets/settings.js") => ("application/javascript", include_bytes!("../assets/webui/settings.js")),
        ("bookmarks", "assets/bookmarks.js") => ("application/javascript", include_bytes!("../assets/webui/bookmarks.js")),
        ("extensions", "assets/extensions.js") => ("application/javascript", include_bytes!("../assets/webui/extensions.js")),
        (_, "assets/font.ttf") => ("font/ttf", FONT),
        (_, "assets/font-medium.ttf") => ("font/ttf", FONT_MEDIUM),
        _ => return None,
    })
}

wrap_scheme_handler_factory! {
    pub struct Pages;

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let url = CefString::from(&request?.url()).to_string();
            let rest = url.strip_prefix("unskilled://")?.split(['?', '#']).next()?;
            let (page, path) = rest.split_once('/').unwrap_or((rest, ""));
            let (mime, body) = asset(page, path).unwrap_or(("text/html", b"<!doctype html><title>not found</title>not found"));
            Some(StaticResponse::with_headers(
                mime.into(),
                Arc::new(body.to_vec()),
                vec![
                    // Our pages run only our own scripts and fetch nothing from the web.
                    ("Content-Security-Policy".into(), "default-src 'self'; img-src 'self' data:; frame-ancestors 'none'".into()),
                    ("X-Content-Type-Options".into(), "nosniff".into()),
                ],
            ))
        }
    }
}

pub fn register_scheme() {
    register_scheme_handler_factory(Some(&CefString::from(SCHEME)), Some(&CefString::from("")), Some(&mut Pages::new()));
}
