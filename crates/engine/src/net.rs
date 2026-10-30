//! Network interception: every request goes through the shield before it leaves the browser.

use cef::*;
use shield::{Category, ResourceKind, Shield, Verdict};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

/// Custom scheme the renderer uses to fetch the document-start page script synchronously.
pub const SHIELD_SCHEME: &str = "unskilled-shield";
const LOG_LIMIT: usize = 2000;

/// One request the shield stopped or altered.
#[derive(Clone, Debug)]
pub struct BlockEvent {
    pub at: SystemTime,
    pub url: String,
    pub host: String,
    pub kind: ResourceKind,
    pub action: Action,
    pub rule: String,
    pub list: &'static str,
    pub category: Category,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Blocked,
    /// Replaced with a harmless stand-in so the page can't tell.
    Neutered,
    /// Tracking parameters removed.
    Cleaned,
}

/// Per-tab network state shared between the UI thread and CEF's IO thread.
#[derive(Default)]
pub struct TabNet {
    pub top_url: String,
    pub events: VecDeque<BlockEvent>,
    pub blocked: usize,
    /// A sign-in navigation waiting for the user to decide (see `Tab::answer_identity`).
    pub identity_prompt: Option<IdentityPrompt>,
}

/// "`site` wants to use your `provider_name` sign-in" — the navigation is paused until answered.
pub struct IdentityPrompt {
    pub site: String,
    pub provider: &'static str,
    pub provider_name: &'static str,
    callback: Callback,
    strip: Arc<AtomicBool>,
}

impl IdentityPrompt {
    /// Lets the navigation continue, with (`share`) or without the provider's cookies.
    pub(crate) fn answer(self, share: bool) {
        self.strip.store(!share, Ordering::Relaxed);
        self.callback.cont();
    }
}

pub type SharedNet = Arc<Mutex<TabNet>>;
/// A replacement response body and its MIME type.
type Stand = Arc<Mutex<Option<(String, Arc<Vec<u8>>)>>>;

/// Browser id → its tab's network state, for handlers that only receive a browser.
static TABS: LazyLock<Mutex<HashMap<i32, SharedNet>>> = LazyLock::new(Default::default);
static SHIELD: Mutex<Option<Arc<Shield>>> = Mutex::new(None);

pub fn set_shield(shield: Arc<Shield>) {
    *SHIELD.lock().unwrap() = Some(shield);
}

fn shield() -> Option<Arc<Shield>> {
    SHIELD.lock().unwrap().clone()
}

pub fn register_tab(browser_id: i32, net: SharedNet) {
    TABS.lock().unwrap().insert(browser_id, net);
}

pub fn unregister_tab(browser_id: i32) {
    TABS.lock().unwrap().remove(&browser_id);
}

fn tab(browser: Option<&mut Browser>) -> Option<SharedNet> {
    let id = browser?.identifier();
    TABS.lock().unwrap().get(&id).cloned()
}

fn kind_of(t: ResourceType) -> ResourceKind {
    match t {
        // Pages behind a service worker (YouTube) arrive as navigation preloads.
        ResourceType::MAIN_FRAME | ResourceType::NAVIGATION_PRELOAD_MAIN_FRAME => ResourceKind::Document,
        ResourceType::SUB_FRAME | ResourceType::NAVIGATION_PRELOAD_SUB_FRAME => ResourceKind::Subdocument,
        ResourceType::STYLESHEET => ResourceKind::Stylesheet,
        ResourceType::SCRIPT | ResourceType::WORKER | ResourceType::SHARED_WORKER | ResourceType::SERVICE_WORKER => {
            ResourceKind::Script
        }
        ResourceType::IMAGE | ResourceType::FAVICON => ResourceKind::Image,
        ResourceType::FONT_RESOURCE => ResourceKind::Font,
        ResourceType::MEDIA => ResourceKind::Media,
        ResourceType::XHR => ResourceKind::Xhr,
        ResourceType::PING | ResourceType::CSP_REPORT => ResourceKind::Ping,
        _ => ResourceKind::Other,
    }
}

fn host_of(url: &str) -> String {
    shield::site_of(url).unwrap_or_default()
}

// ── per-request handler ──────────────────────────────────────────────────────

wrap_resource_request_handler! {
    pub struct Interceptor {
        net: Option<SharedNet>,
        // Set in `on_before_resource_load` when the response is replaced.
        redirect: Stand,
        // Origin of whoever started the request (the page, for navigations it triggers).
        initiator: Option<String>,
        // Withhold cookies (identity gate: the provider sees you signed out).
        strip: Arc<AtomicBool>,
    }

    impl ResourceRequestHandler {
        fn cookie_access_filter(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
        ) -> Option<CookieAccessFilter> {
            Some(CookieGate::new(self.strip.clone()))
        }

        fn on_before_resource_load(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            callback: Option<&mut Callback>,
        ) -> ReturnValue {
            let (Some(request), Some(shield)) = (request, shield()) else { return ReturnValue::CONTINUE };
            let url = CefString::from(&request.url()).to_string();
            if !url.starts_with("http://") && !url.starts_with("https://") && !url.starts_with("ws") {
                return ReturnValue::CONTINUE;
            }
            let kind = kind_of(request.resource_type());

            if let Some(paused) = self.identity_gate(&url, kind, callback.as_deref()) {
                return paused;
            }

            // Filters still loading (first seconds after launch): hold page loads until they're up,
            // so no page ever runs without its scriptlets.
            if kind == ResourceKind::Document && shield.status() == shield::Status::Loading
                && let Some(callback) = callback.cloned() {
                    std::thread::spawn(move || {
                        shield.wait_ready(std::time::Duration::from_secs(10));
                        callback.cont();
                    });
                    return ReturnValue::CONTINUE_ASYNC;
                }
            let method = CefString::from(&request.method()).to_string();

            let top_url = match (&self.net, kind) {
                (Some(net), ResourceKind::Document) => {
                    // A new page: reset this tab's counters.
                    let mut net = net.lock().unwrap();
                    net.top_url = url.clone();
                    net.events.clear();
                    net.blocked = 0;
                    url.clone()
                }
                (Some(net), _) => net.lock().unwrap().top_url.clone(),
                (None, _) => String::new(),
            };
            let frame_url = frame
                .map(|f| CefString::from(&f.url()).to_string())
                .filter(|u| u.starts_with("http"))
                .unwrap_or_else(|| top_url.clone());

            let verdict = shield.check(&shield::Request {
                url: &url,
                frame_url: &frame_url,
                top_url: &top_url,
                kind,
                method: &method,
            });
            let (action, matched, result) = match verdict {
                Verdict::Allow => return ReturnValue::CONTINUE,
                Verdict::Block(m) => (Action::Blocked, Some(m), ReturnValue::CANCEL),
                Verdict::Redirect { mime, body, matched } => {
                    *self.redirect.lock().unwrap() = Some((mime, Arc::new(body)));
                    (Action::Neutered, Some(matched), ReturnValue::CONTINUE)
                }
                Verdict::Rewrite(clean) => {
                    request.set_url(Some(&CefString::from(clean.as_str())));
                    (Action::Cleaned, None, ReturnValue::CONTINUE)
                }
            };

            log::debug!("shield: {action:?} {url} [{}]", matched.as_ref().map_or("", |m| m.rule.as_str()));
            if let Some(net) = &self.net {
                let mut net = net.lock().unwrap();
                if action != Action::Cleaned {
                    net.blocked += 1;
                }
                if net.events.len() >= LOG_LIMIT {
                    net.events.pop_front();
                }
                let (rule, list, category) = matched.map_or((String::new(), "removeparam", Category::Trackers), |m| {
                    (m.rule, m.list, m.category)
                });
                net.events.push_back(BlockEvent {
                    at: SystemTime::now(),
                    host: host_of(&url),
                    url,
                    kind,
                    action,
                    rule,
                    list,
                    category,
                });
            }
            result
        }

        fn resource_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let (mime, body) = self.redirect.lock().unwrap().take()?;
            Some(StaticResponse::plain(mime, body))
        }
    }
}

pub fn interceptor(browser: Option<&mut Browser>, initiator: Option<String>) -> ResourceRequestHandler {
    Interceptor::new(tab(browser), Arc::new(Mutex::new(None)), initiator, Arc::new(AtomicBool::new(false)))
}

impl Interceptor {
    /// "Sign in with Google/Apple/…" from another site: unless the user already decided for this
    /// site, the navigation pauses and the tab asks. Embedded sign-in widgets (frames) are never
    /// asked about — they just don't get the provider's cookies unless the site was allowed.
    /// Returns `Some` when the request was paused.
    fn identity_gate(&self, url: &str, kind: ResourceKind, callback: Option<&Callback>) -> Option<ReturnValue> {
        if !matches!(kind, ResourceKind::Document | ResourceKind::Subdocument) {
            return None;
        }
        let lookup = crate::webui::identity_gate()?;
        let provider = profile::provider_for(url)?;
        // Navigations typed by the user (no initiator) or from the provider's own sites are fine.
        let site = self.initiator.as_deref().and_then(profile::site_of)?;
        if provider.owns(&site) || profile::site_of(url).as_deref() == Some(site.as_str()) {
            return None;
        }
        match lookup(&site, provider.id) {
            Some(share) => self.strip.store(!share, Ordering::Relaxed),
            None if kind == ResourceKind::Subdocument => self.strip.store(true, Ordering::Relaxed),
            None => {
                let (Some(net), Some(callback)) = (&self.net, callback) else {
                    self.strip.store(true, Ordering::Relaxed);
                    return None;
                };
                let previous = net.lock().unwrap().identity_prompt.replace(IdentityPrompt {
                    site,
                    provider: provider.id,
                    provider_name: provider.name,
                    callback: callback.clone(),
                    strip: self.strip.clone(),
                });
                if let Some(previous) = previous {
                    previous.answer(false);
                }
                wake();
                return Some(ReturnValue::CONTINUE_ASYNC);
            }
        }
        None
    }
}

wrap_cookie_access_filter! {
    struct CookieGate {
        strip: Arc<AtomicBool>,
    }

    impl CookieAccessFilter {
        fn can_send_cookie(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _cookie: Option<&Cookie>,
        ) -> ::std::os::raw::c_int {
            (!self.strip.load(Ordering::Relaxed)) as _
        }

        fn can_save_cookie(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _response: Option<&mut Response>,
            _cookie: Option<&Cookie>,
        ) -> ::std::os::raw::c_int {
            (!self.strip.load(Ordering::Relaxed)) as _
        }
    }
}

/// Redraws the UI from CEF's IO thread (set by the engine at startup).
static WAKE: Mutex<Option<crate::devtools::Wake>> = Mutex::new(None);

pub(crate) fn set_wake(wake: crate::devtools::Wake) {
    *WAKE.lock().unwrap() = Some(wake);
}

fn wake() {
    if let Some(wake) = WAKE.lock().unwrap().as_ref() {
        wake();
    }
}

// ── in-memory responses ──────────────────────────────────────────────────────

wrap_resource_handler! {
    pub struct StaticResponse {
        mime: String,
        body: Arc<Vec<u8>>,
        offset: Arc<Mutex<usize>>,
        headers: Arc<Vec<(String, String)>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            if let Some(handle) = handle_request {
                *handle = 1;
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            if let Some(response) = response {
                response.set_status(200);
                response.set_status_text(Some(&CefString::from("OK")));
                response.set_mime_type(Some(&CefString::from(self.mime.as_str())));
                // Stand-ins are often loaded cross-origin with `crossorigin`.
                response.set_header_by_name(
                    Some(&CefString::from("Access-Control-Allow-Origin")),
                    Some(&CefString::from("*")),
                    1,
                );
                response.set_header_by_name(
                    Some(&CefString::from("Cache-Control")),
                    Some(&CefString::from("no-store")),
                    1,
                );
                for (name, value) in self.headers.iter() {
                    response.set_header_by_name(Some(&CefString::from(name.as_str())), Some(&CefString::from(value.as_str())), 1);
                }
            }
            if let Some(length) = response_length {
                *length = self.body.len() as i64;
            }
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> ::std::os::raw::c_int {
            let mut offset = self.offset.lock().unwrap();
            let remaining = &self.body[*offset..];
            let n = remaining.len().min(bytes_to_read.max(0) as usize);
            if n > 0 {
                // SAFETY: CEF guarantees `data_out` points to `bytes_to_read` writable bytes.
                unsafe { std::ptr::copy_nonoverlapping(remaining.as_ptr(), data_out, n) };
            }
            *offset += n;
            if let Some(read) = bytes_read {
                *read = n as _;
            }
            (n > 0) as _
        }
    }
}

impl StaticResponse {
    pub fn plain(mime: String, body: Arc<Vec<u8>>) -> ResourceHandler {
        Self::with_headers(mime, body, Vec::new())
    }

    pub fn with_headers(mime: String, body: Arc<Vec<u8>>, headers: Vec<(String, String)>) -> ResourceHandler {
        Self::new(mime, body, Arc::new(Mutex::new(0)), Arc::new(headers))
    }
}

// ── page script channel ──────────────────────────────────────────────────────

wrap_scheme_handler_factory! {
    pub struct ShieldScheme;

    impl SchemeHandlerFactory {
        fn create(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let request = request?;
            let url = CefString::from(&request.url()).to_string();
            let frame_url = query_param(&url, "u").unwrap_or_default();
            let top_url = tab(browser)
                .map(|net| net.lock().unwrap().top_url.clone())
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| frame_url.clone());
            Some(PageScript::new(frame_url, top_url, Arc::new(Mutex::new(None)), Arc::new(Mutex::new(0))))
        }
    }
}

fn page_script(frame_url: &str, top_url: &str) -> Arc<Vec<u8>> {
    let script = shield().and_then(|s| s.page_script(frame_url, top_url)).unwrap_or_default();
    log::debug!("shield: page script for {frame_url}: {} bytes", script.len());
    Arc::new(script.into_bytes())
}

// The document-start script. While filters are still loading (first seconds after launch) the
// answer waits for them; the renderer is blocked on this request, so the page can't run a single
// script unprotected.
wrap_resource_handler! {
    pub struct PageScript {
        frame_url: String,
        top_url: String,
        body: Arc<Mutex<Option<Arc<Vec<u8>>>>>,
        offset: Arc<Mutex<usize>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut ::std::os::raw::c_int>,
            callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            let Some(handle) = handle_request else { return 0 };
            let ready = shield().is_none_or(|s| s.status() != shield::Status::Loading);
            if ready {
                *self.body.lock().unwrap() = Some(page_script(&self.frame_url, &self.top_url));
                *handle = 1;
                return 1;
            }
            let (Some(callback), Some(shield)) = (callback.cloned(), shield()) else { return 0 };
            let (frame_url, top_url, body) = (self.frame_url.clone(), self.top_url.clone(), self.body.clone());
            std::thread::spawn(move || {
                shield.wait_ready(std::time::Duration::from_secs(10));
                *body.lock().unwrap() = Some(page_script(&frame_url, &top_url));
                callback.cont();
            });
            *handle = 0;
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let len = self.body.lock().unwrap().as_ref().map_or(0, |b| b.len());
            if let Some(response) = response {
                response.set_status(200);
                response.set_mime_type(Some(&CefString::from("application/javascript")));
                response.set_header_by_name(
                    Some(&CefString::from("Access-Control-Allow-Origin")),
                    Some(&CefString::from("*")),
                    1,
                );
            }
            if let Some(length) = response_length {
                *length = len as i64;
            }
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> ::std::os::raw::c_int {
            let body = self.body.lock().unwrap().clone().unwrap_or_default();
            let mut offset = self.offset.lock().unwrap();
            let remaining = &body[(*offset).min(body.len())..];
            let n = remaining.len().min(bytes_to_read.max(0) as usize);
            if n > 0 {
                // SAFETY: CEF guarantees `data_out` points to `bytes_to_read` writable bytes.
                unsafe { std::ptr::copy_nonoverlapping(remaining.as_ptr(), data_out, n) };
            }
            *offset += n;
            if let Some(read) = bytes_read {
                *read = n as _;
            }
            (n > 0) as _
        }
    }
}

/// The shield's scheme in a sandbox's own context (the page script is fetched through it).
pub(crate) fn register_scheme_on(context: &RequestContext) {
    context.register_scheme_handler_factory(
        Some(&CefString::from(SHIELD_SCHEME)),
        Some(&CefString::from("")),
        Some(&mut ShieldScheme::new()),
    );
}

pub fn register_scheme() {
    register_scheme_handler_factory(
        Some(&CefString::from(SHIELD_SCHEME)),
        Some(&CefString::from("")),
        Some(&mut ShieldScheme::new()),
    );
}

/// Answers the page script's generic cosmetic queries (sent from the renderer).
pub fn generic_selectors(frame_url: &str, classes: &[String], ids: &[String]) -> String {
    shield().map_or_else(|| "[]".into(), |s| s.generic_selectors_json(frame_url, classes, ids))
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

/// Decodes `encodeURIComponent` output (which never produces `+`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
