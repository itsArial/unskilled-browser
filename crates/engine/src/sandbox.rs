//! Per-site sandboxes. Every site (`profile::sandbox_of`: the registrable domain, with a sign-in
//! provider's own sites grouped) gets its own Chromium request context — its own cookie jar,
//! storage, cache and connection pool, on disk as `profile/sandbox-<site>` (Chromium only accepts
//! profiles directly inside its data directory). A tab that navigates
//! to another site moves into that site's sandbox (see `tab::Requests::on_before_browse`), so
//! sites never share state, not even first-party cookies set while bouncing through a tracker.
//! Internal pages and the start page use Chromium's global context.

use cef::*;
use std::{cell::RefCell, collections::HashMap, fs, path::PathBuf, rc::Rc};

struct Sandboxes {
    root: PathBuf,
    contexts: HashMap<String, RequestContext>,
    enabled: bool,
    third_party_blocked: bool,
}

thread_local! {
    static SANDBOXES: RefCell<Option<Sandboxes>> = const { RefCell::new(None) };
}

/// Sandbox directories are `PREFIX<site>` inside the profile directory.
const PREFIX: &str = "sandbox-";
/// Marks a sandbox whose data is erased the next time the browser starts.
const FORGET: &str = ".forget";

/// Sets up the registry in `profile_dir`, erasing sandboxes marked as forgotten.
pub(crate) fn init(profile_dir: &std::path::Path) {
    let root = profile_dir.to_path_buf();
    for entry in fs::read_dir(&root).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(dir) = name.strip_suffix(FORGET).filter(|d| d.starts_with(PREFIX)) {
            let _ = fs::remove_dir_all(root.join(dir));
            let _ = fs::remove_file(entry.path());
        }
    }
    SANDBOXES.with_borrow_mut(|s| {
        *s = Some(Sandboxes { root, contexts: HashMap::new(), enabled: true, third_party_blocked: true })
    });
}

/// Turns per-site sandboxes on or off (off: every page shares the global context).
pub fn set_enabled(enabled: bool) {
    SANDBOXES.with_borrow_mut(|s| {
        if let Some(s) = s {
            s.enabled = enabled;
        }
    });
}

/// The sandbox a page at `url` belongs in; `None` = the shared global context.
pub fn key_for(url: &str) -> Option<String> {
    let enabled = SANDBOXES.with_borrow(|s| s.as_ref().is_some_and(|s| s.enabled));
    enabled.then(|| profile::sandbox_of(url)).flatten()
}

/// `google.com` → `sandbox-google.com`, safe as a directory name.
fn dir_name(key: &str) -> String {
    let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    format!("{PREFIX}{safe}")
}

/// The request context for sandbox `key`, created on first use. `None` key = global context.
pub(crate) fn context(key: Option<&str>) -> Option<RequestContext> {
    let key = key?;
    SANDBOXES.with_borrow_mut(|s| {
        let s = s.as_mut()?;
        if let Some(context) = s.contexts.get(key) {
            return Some(context.clone());
        }
        let dir = s.root.join(dir_name(key));
        let _ = fs::create_dir_all(&dir);
        // Same reason as the global profile: keep session cookies (logins) across restarts.
        let _ = fs::write(dir.join("Preferences"), r#"{"session":{"restore_on_startup":1}}"#);
        let settings = RequestContextSettings {
            cache_path: CefString::from(dir.to_string_lossy().as_ref()),
            persist_session_cookies: 1,
            ..Default::default()
        };
        let context = request_context_create_context(Some(&settings), None)?;
        crate::net::register_scheme_on(&context);
        apply_cookie_policy(&context, s.third_party_blocked);
        log::debug!("sandbox: created {key}");
        s.contexts.insert(key.to_owned(), context.clone());
        Some(context)
    })
}

/// Blocks (or allows) third-party cookies in every sandbox, now and for new ones.
pub(crate) fn set_third_party_cookies_blocked(blocked: bool) {
    SANDBOXES.with_borrow_mut(|s| {
        if let Some(s) = s {
            s.third_party_blocked = blocked;
            for context in s.contexts.values() {
                apply_cookie_policy(context, blocked);
            }
        }
    });
}

pub(crate) fn apply_cookie_policy(context: &RequestContext, blocked: bool) {
    // 1 = block third-party cookies, 0 = allow.
    for (name, value) in [("profile.cookie_controls_mode", blocked as i32), ("profile.block_third_party_cookies", blocked as i32)] {
        let Some(mut pref) = value_create() else { continue };
        if name.ends_with("cookies") {
            pref.set_bool(value);
        } else {
            pref.set_int(value);
        }
        let mut error = CefString::from("");
        if context.set_preference(Some(&CefString::from(name)), Some(&mut pref), Some(&mut error)) == 0 {
            log::debug!("engine: preference {name}: {error}");
        }
    }
}

/// Sandboxes with data on disk, by name.
pub fn list() -> Vec<String> {
    let root = SANDBOXES.with_borrow(|s| s.as_ref().map(|s| s.root.clone()));
    let Some(root) = root else { return Vec::new() };
    let mut names: Vec<String> = fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|name| !root.join(format!("{name}{FORGET}")).exists())
        .filter_map(|name| name.strip_prefix(PREFIX).map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// Erases a site's sandbox: its cookies go now (logged out immediately), everything else when
/// the browser next starts (Chromium keeps its storage files open while running).
pub fn forget(key: &str) {
    let found = SANDBOXES.with_borrow(|s| {
        let s = s.as_ref()?;
        Some((s.contexts.get(key).cloned(), s.root.clone(), dir_name(key)))
    });
    let Some((context, root, name)) = found else { return };
    if let Some(manager) = context.and_then(|c| c.cookie_manager(None)) {
        manager.delete_cookies(None, None, None);
    }
    if root.join(&name).is_dir() {
        let _ = fs::write(root.join(format!("{name}{FORGET}")), b"");
    }
}

/// Runs its closure once the last reference is gone: when the visitor and every cookie write it
/// started have finished (CEF calls both on the UI thread).
struct Finish(RefCell<Option<Box<dyn FnOnce()>>>);

impl Drop for Finish {
    fn drop(&mut self) {
        if let Some(done) = self.0.get_mut().take() {
            done();
        }
    }
}

/// "Allow" on the sign-in prompt: copies `provider`'s cookies from its own sandbox into sandbox
/// `into`, then runs `done`. The identity gate still decides on every later visit whether those
/// cookies are sent to the provider's sign-in pages.
pub(crate) fn share_sign_in(provider: &'static profile::Provider, into: &str, done: Box<dyn FnOnce()>) {
    let finish = Rc::new(Finish(RefCell::new(Some(done))));
    if into == provider.sandbox() {
        return;
    }
    let from = context(Some(provider.sandbox())).and_then(|c| c.cookie_manager(None));
    let to = context(Some(into)).and_then(|c| c.cookie_manager(None));
    if let (Some(from), Some(to)) = (from, to) {
        from.visit_all_cookies(Some(&mut Copier::new(provider, to, finish)));
    }
}

wrap_cookie_visitor! {
    struct Copier {
        provider: &'static profile::Provider,
        into: CookieManager,
        finish: Rc<Finish>,
    }

    impl CookieVisitor {
        fn visit(
            &self,
            cookie: Option<&Cookie>,
            _count: ::std::os::raw::c_int,
            _total: ::std::os::raw::c_int,
            _delete_cookie: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            if let Some(cookie) = cookie {
                let domain = cookie.domain.to_string();
                if self.provider.sets_cookies_on(&domain) {
                    let url = format!("https://{}{}", domain.trim_start_matches('.'), cookie.path);
                    self.into.set_cookie(Some(&CefString::from(url.as_str())), Some(cookie), Some(&mut Written::new(self.finish.clone())));
                }
            }
            1
        }
    }
}

wrap_set_cookie_callback! {
    struct Written {
        finish: Rc<Finish>,
    }

    impl SetCookieCallback {
        fn on_complete(&self, _success: ::std::os::raw::c_int) {
            let _ = &self.finish;
        }
    }
}
