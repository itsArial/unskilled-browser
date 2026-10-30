//! The shell's side of `engine::Host`: what internal pages, login forms and the identity gate need
//! from the profile and the vault. Calls that need the rest of the shell (a new tab, a PIN prompt)
//! are queued as `Event`s and handled on the next frame.

use crate::{clipboard, updater};
use profile::{Decision, Prefs, Profile, SearchEngine};
use serde_json::{Value, json};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::{Arc, RwLock},
};
use zeroize::Zeroizing;

pub const SOURCE: &str = "https://github.com/itsArial/unskilled-browser";

pub enum Event {
    OpenUrl(String),
    /// An internal page wants a saved password; the PIN prompt decides the ticket.
    Authorize(u64),
    LoginSubmitted { browser: i32, url: String, username: String, password: Zeroizing<String> },
    LoginForm { browser: i32, url: String, frame: String },
    /// An internal page asked to restart the browser (extension changes).
    Restart,
}

/// A Web Store download started from the extensions page.
enum StoreJob {
    Pending,
    Done(String),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Use {
    Reveal,
    Copy,
}

/// What an internal page asked for that needs the PIN first.
enum Guarded {
    Password { credential: u64, use_: Use },
    /// New preferences that turn the PIN-before-passwords guard off.
    Prefs(Box<Prefs>),
}

/// A guarded request, waiting on (or decided by) the PIN prompt.
enum Ticket {
    Pending(Guarded),
    Granted(Option<Zeroizing<String>>),
    Denied,
}

pub struct Host {
    pub profile: RefCell<Profile>,
    pub vault: Arc<vault::Vault>,
    events: RefCell<Vec<Event>>,
    tickets: RefCell<HashMap<u64, Ticket>>,
    next_ticket: Cell<u64>,
    /// `"<site> <provider>"` → share, mirrored for CEF's IO thread.
    identity: Arc<RwLock<HashMap<String, bool>>>,
    wake: Rc<dyn Fn()>,
    extensions: Rc<RefCell<extensions::Manager>>,
    store_jobs: Rc<RefCell<HashMap<u64, StoreJob>>>,
    /// Extensions changed since Chromium started: they apply on restart.
    restart_needed: Rc<Cell<bool>>,
    updates: updater::Shared,
}

impl Host {
    pub fn new(profile: Profile, vault: Arc<vault::Vault>, wake: Rc<dyn Fn()>, extensions: extensions::Manager) -> Rc<Self> {
        let host = Rc::new(Self {
            profile: RefCell::new(profile),
            vault,
            events: RefCell::new(Vec::new()),
            tickets: RefCell::new(HashMap::new()),
            next_ticket: Cell::new(1),
            identity: Arc::new(RwLock::new(HashMap::new())),
            wake,
            extensions: Rc::new(RefCell::new(extensions)),
            store_jobs: Rc::new(RefCell::new(HashMap::new())),
            restart_needed: Rc::new(Cell::new(false)),
            updates: updater::Shared::default(),
        });
        host.apply_prefs();
        host
    }

    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut self.events.borrow_mut())
    }

    fn push(&self, event: Event) {
        self.events.borrow_mut().push(event);
        (self.wake)();
    }

    pub fn prefs(&self) -> Prefs {
        self.profile.borrow().prefs().clone()
    }

    pub fn search(&self) -> SearchEngine {
        self.profile.borrow().prefs().search
    }

    /// Pushes the preferences Chromium enforces (cookies, identity gate) into the engine.
    pub fn apply_prefs(&self) {
        let prefs = self.prefs();
        engine::set_third_party_cookies_blocked(prefs.block_third_party_cookies);
        engine::set_sandboxes_enabled(prefs.isolate_sites);
        self.sync_identity();
        if prefs.identity_gate {
            let decisions = self.identity.clone();
            engine::set_identity_gate(Some(Arc::new(move |site: &str, provider: &str| {
                decisions.read().unwrap().get(&format!("{site} {provider}")).copied()
            })));
        } else {
            engine::set_identity_gate(None);
        }
    }

    fn sync_identity(&self) {
        let map = self
            .profile
            .borrow()
            .identity_decisions()
            .into_iter()
            .map(|(site, provider, d)| (format!("{site} {provider}"), d == Decision::Allow))
            .collect();
        *self.identity.write().unwrap() = map;
    }

    pub fn set_identity(&self, site: &str, provider: &str, decision: Option<Decision>) {
        self.profile.borrow_mut().set_identity_decision(site, provider, decision);
        self.sync_identity();
    }

    // ── password tickets (settings page) ───────────────────────────────────

    /// The PIN prompt for `ticket` finished.
    pub fn decide(&self, ticket: u64, granted: bool) {
        let mut tickets = self.tickets.borrow_mut();
        let Some(Ticket::Pending(guarded)) = tickets.remove(&ticket) else { return };
        let outcome = match (granted, guarded) {
            (false, _) => Ticket::Denied,
            (true, Guarded::Password { credential, use_ }) => Ticket::Granted(self.use_password(credential, use_)),
            (true, Guarded::Prefs(prefs)) => {
                self.set_prefs(*prefs);
                Ticket::Granted(None)
            }
        };
        tickets.insert(ticket, outcome);
    }

    /// Reveal returns the password; copy puts it on the clipboard (cleared after 30 s).
    fn use_password(&self, credential: u64, use_: Use) -> Option<Zeroizing<String>> {
        let mut profile = self.profile.borrow_mut();
        let password = Zeroizing::new(profile.passwords().iter().find(|c| c.id == credential)?.password.clone());
        profile.mark_used(credential);
        match use_ {
            Use::Reveal => Some(password),
            Use::Copy => {
                clipboard::copy_secret(&password);
                None
            }
        }
    }

    fn password_call(&self, params: &Value, use_: Use) -> Value {
        let Some(credential) = params["id"].as_u64() else { return json!({"error": "bad request"}) };
        if !self.prefs().confirm_password_use {
            return match self.use_password(credential, use_) {
                Some(password) => json!({"password": password.as_str()}),
                None => json!({"ok": true}),
            };
        }
        self.guard(Guarded::Password { credential, use_ })
    }

    /// Parks `guarded` until the PIN prompt answers; the page polls `authResult`.
    fn guard(&self, guarded: Guarded) -> Value {
        let ticket = self.next_ticket.get();
        self.next_ticket.set(ticket + 1);
        self.tickets.borrow_mut().insert(ticket, Ticket::Pending(guarded));
        self.push(Event::Authorize(ticket));
        json!({"needsAuth": true, "ticket": ticket})
    }

    fn set_prefs(&self, prefs: Prefs) {
        self.profile.borrow_mut().set_prefs(prefs);
        self.apply_prefs();
    }

    fn auth_result(&self, params: &Value) -> Value {
        let Some(ticket) = params["ticket"].as_u64() else { return json!({"error": "bad request"}) };
        let mut tickets = self.tickets.borrow_mut();
        match tickets.get(&ticket) {
            None => json!({"denied": true}),
            Some(Ticket::Pending(_)) => json!({"pending": true}),
            Some(_) => match tickets.remove(&ticket) {
                Some(Ticket::Granted(Some(password))) => json!({"password": password.as_str()}),
                Some(Ticket::Granted(None)) => json!({"ok": true}),
                _ => json!({"denied": true}),
            },
        }
    }

    /// The opt-in check at start: only says something when there is an update.
    pub fn check_updates_at_start(&self) {
        if self.prefs().check_updates && updater::install_root().is_some() {
            updater::check(&self.updates, true);
        }
    }

    /// What the extensions page shows.
    fn extensions_state(&self) -> Value {
        extensions_json(&self.extensions.borrow(), self.restart_needed.get())
    }

    /// Downloads a package from the Chrome Web Store (`url` is a store link or an ID).
    fn install_from_store(&self, url: &str) -> Value {
        let Some(id) = store_id(url) else {
            return json!({"error": "that doesn't look like a Chrome Web Store link"});
        };
        let ticket = self.next_ticket.get();
        self.next_ticket.set(ticket + 1);
        self.store_jobs.borrow_mut().insert(ticket, StoreJob::Pending);
        let download = format!(
            "https://clients2.google.com/service/update2/crx?response=redirect&prodversion={}&acceptformat=crx3&x=id%3D{id}%26installsource%3Dondemand%26uc",
            engine::CHROMIUM_VERSION
        );
        let (jobs, manager, restart, wake) =
            (self.store_jobs.clone(), self.extensions.clone(), self.restart_needed.clone(), self.wake.clone());
        engine::fetch(
            &download,
            128 * 1024 * 1024,
            Box::new(move |result| {
                let job = match result.and_then(|bytes| manager.borrow_mut().install(&bytes)) {
                    Ok(e) => {
                        restart.set(true);
                        StoreJob::Done(e.name)
                    }
                    Err(e) => StoreJob::Failed(e),
                };
                jobs.borrow_mut().insert(ticket, job);
                wake();
            }),
        );
        json!({"ticket": ticket})
    }

    /// What the settings and bookmarks pages show.
    fn state(&self) -> Value {
        let profile = self.profile.borrow();
        let protection = match self.vault.state() {
            vault::State::New { protection } | vault::State::Locked { protection } => protection,
        };
        json!({
            "prefs": profile.prefs(),
            "bookmarks": profile.bookmarks(),
            "passwords": profile.passwords().iter().map(|c| json!({
                "id": c.id, "origin": c.origin, "username": c.username, "lastUsed": c.last_used,
            })).collect::<Vec<_>>(),
            "identity": profile.identity_decisions().into_iter().map(|(site, provider, decision)| json!({
                "site": site,
                "provider": provider,
                "providerName": profile::PROVIDERS.iter().find(|p| p.id == provider).map_or(provider.as_str(), |p| p.name),
                "decision": decision,
            })).collect::<Vec<_>>(),
            "sandboxes": engine::sandboxes(),
            "engines": SearchEngine::ALL.iter().map(|e| json!({"id": e, "label": e.label()})).collect::<Vec<_>>(),
            "vault": if protection == vault::Protection::Tpm { "tpm" } else { "passphrase" },
            "tpmAvailable": self.vault.can_upgrade(),
            "version": env!("CARGO_PKG_VERSION"),
            "updates": updater::install_root().is_some(),
            "update": self.updates.lock().unwrap().json(),
            "chromium": engine::CHROMIUM_VERSION,
            "source": SOURCE,
        })
    }
}

impl engine::Host for Host {
    fn webui(&self, method: &str, params: &str) -> String {
        let params: Value = serde_json::from_str(params).unwrap_or(Value::Null);
        let reply = match method {
            "state" => self.state(),
            "setPrefs" => match serde_json::from_value::<Prefs>(params["prefs"].clone()) {
                // Switching the PIN guard off is itself guarded.
                Ok(prefs) if self.prefs().confirm_password_use && !prefs.confirm_password_use => self.guard(Guarded::Prefs(Box::new(prefs))),
                Ok(prefs) => {
                    self.set_prefs(prefs);
                    json!({"ok": true})
                }
                Err(e) => json!({"error": e.to_string()}),
            },
            "setIdentity" => {
                let (Some(site), Some(provider)) = (params["site"].as_str(), params["provider"].as_str()) else {
                    return json!({"error": "bad request"}).to_string();
                };
                let decision = serde_json::from_value::<Option<Decision>>(params["decision"].clone()).ok().flatten();
                self.set_identity(site, provider, decision);
                json!({"ok": true})
            }
            "revealPassword" => self.password_call(&params, Use::Reveal),
            "copyPassword" => self.password_call(&params, Use::Copy),
            "authResult" => self.auth_result(&params),
            "removePassword" => {
                if let Some(id) = params["id"].as_u64() {
                    self.profile.borrow_mut().remove_login(id);
                }
                json!({"ok": true})
            }
            "importPasswords" => match profile::import_passwords_csv(params["text"].as_str().unwrap_or_default()) {
                Ok(rows) => json!({"added": self.profile.borrow_mut().import_logins(rows)}),
                Err(e) => json!({"error": e}),
            },
            "importBookmarks" => {
                let items = profile::import_bookmarks_html(params["text"].as_str().unwrap_or_default());
                json!({"added": self.profile.borrow_mut().import_bookmarks(items)})
            }
            "forgetSandbox" => {
                if let Some(site) = params["site"].as_str() {
                    engine::forget_sandbox(site);
                }
                json!({"ok": true})
            }
            "extensions" => self.extensions_state(),
            "installExtension" => {
                let bytes = B64.decode(params["data"].as_str().unwrap_or_default()).unwrap_or_default();
                let installed = self.extensions.borrow_mut().install(&bytes);
                match installed {
                    Ok(e) => {
                        self.restart_needed.set(true);
                        json!({"name": e.name, "state": self.extensions_state()})
                    }
                    Err(e) => json!({"error": e}),
                }
            }
            "installFromStore" => self.install_from_store(params["url"].as_str().unwrap_or_default()),
            "storeResult" => {
                let ticket = params["ticket"].as_u64().unwrap_or_default();
                let mut jobs = self.store_jobs.borrow_mut();
                match jobs.get(&ticket) {
                    Some(StoreJob::Pending) => json!({"pending": true}),
                    _ => match jobs.remove(&ticket) {
                        Some(StoreJob::Done(name)) => json!({"name": name}),
                        Some(StoreJob::Failed(e)) => json!({"error": e}),
                        _ => json!({"error": "unknown download"}),
                    },
                }
            }
            "setExtensionEnabled" => {
                if let Some(id) = params["id"].as_str() {
                    self.extensions.borrow_mut().set_enabled(id, params["enabled"].as_bool().unwrap_or(true));
                    self.restart_needed.set(true);
                }
                self.extensions_state()
            }
            "removeExtension" => {
                if let Some(id) = params["id"].as_str() {
                    self.extensions.borrow_mut().remove(id);
                    self.restart_needed.set(true);
                }
                self.extensions_state()
            }
            "checkUpdates" => {
                updater::check(&self.updates, false);
                self.updates.lock().unwrap().json()
            }
            "updateStatus" => self.updates.lock().unwrap().json(),
            "restart" => {
                self.push(Event::Restart);
                json!({"ok": true})
            }
            "removeBookmark" => {
                if let Some(id) = params["id"].as_u64() {
                    self.profile.borrow_mut().remove_bookmark(id);
                }
                json!({"ok": true})
            }
            "openUrl" => {
                let url = params["url"].as_str().unwrap_or_default();
                if ["https://", "http://", "unskilled://"].iter().any(|s| url.starts_with(s)) {
                    self.push(Event::OpenUrl(url.to_owned()));
                }
                json!({"ok": true})
            }
            _ => json!({"error": "unknown method"}),
        };
        reply.to_string()
    }

    fn login_submitted(&self, browser: i32, url: &str, username: &str, password: &str) {
        if self.profile.borrow().should_offer(url, username, password) {
            self.push(Event::LoginSubmitted {
                browser,
                url: url.to_owned(),
                username: username.to_owned(),
                password: Zeroizing::new(password.to_owned()),
            });
        }
    }

    fn login_form(&self, browser: i32, url: &str, frame: &str) {
        self.push(Event::LoginForm { browser, url: url.to_owned(), frame: frame.to_owned() });
    }
}

fn extensions_json(manager: &extensions::Manager, restart_needed: bool) -> Value {
    json!({
        "restartNeeded": restart_needed,
        "extensions": manager.list().iter().map(|e| json!({
            "id": e.id,
            "name": e.name,
            "version": e.version,
            "description": e.description,
            "enabled": e.enabled,
            "icon": manager.icon_data_url(&e.id),
        })).collect::<Vec<_>>(),
    })
}

/// The extension ID in a Web Store link (`…/detail/<name>/<id>`), or a bare ID.
fn store_id(input: &str) -> Option<String> {
    let is_id = |s: &str| s.len() == 32 && s.bytes().all(|b| (b'a'..=b'p').contains(&b));
    input.split(['/', '?', '#']).map(str::trim).find(|s| is_id(s)).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_store_links() {
        let id = "ddkjiahejlhfcafbddmgiahcphecmpfh";
        assert_eq!(store_id(&format!("https://chromewebstore.google.com/detail/ublock-origin-lite/{id}?hl=en")).as_deref(), Some(id));
        assert_eq!(store_id(&format!("https://chrome.google.com/webstore/detail/x/{id}")).as_deref(), Some(id));
        assert_eq!(store_id(id).as_deref(), Some(id));
        assert_eq!(store_id("https://example.com/detail/nope"), None);
    }
}
