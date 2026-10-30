//! The shell's side of `engine::Host`: what internal pages, login forms and the identity gate need
//! from the profile and the vault. Calls that need the rest of the shell (a new tab, a PIN prompt)
//! are queued as `Event`s and handled on the next frame.

use crate::clipboard;
use profile::{Decision, Prefs, Profile, SearchEngine};
use serde_json::{Value, json};
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
}

impl Host {
    pub fn new(profile: Profile, vault: Arc<vault::Vault>, wake: Rc<dyn Fn()>) -> Rc<Self> {
        let host = Rc::new(Self {
            profile: RefCell::new(profile),
            vault,
            events: RefCell::new(Vec::new()),
            tickets: RefCell::new(HashMap::new()),
            next_ticket: Cell::new(1),
            identity: Arc::new(RwLock::new(HashMap::new())),
            wake,
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
