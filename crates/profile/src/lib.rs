//! The user's own data: bookmarks, saved passwords, sign-in decisions and preferences.
//! Everything lives as JSON in the vault's settings directory, so it's encrypted at rest and only
//! readable while the browser is unlocked. Nothing here is ever sent anywhere.

mod identity;
mod import;

pub use identity::{Decision, Provider, PROVIDERS, provider_for, site_of};
pub use import::{import_bookmarks_html, import_passwords_csv};

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub added: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Credential {
    pub id: u64,
    /// `https://github.com` — logins are matched by origin.
    pub origin: String,
    pub username: String,
    pub password: String,
    pub created: u64,
    #[serde(default)]
    pub last_used: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SearchEngine {
    #[default]
    DuckDuckGo,
    Leta,
    Brave,
    Startpage,
}

impl SearchEngine {
    pub const ALL: [Self; 4] = [Self::DuckDuckGo, Self::Leta, Self::Brave, Self::Startpage];

    pub fn label(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Leta => "Mullvad Leta",
            Self::Brave => "Brave Search",
            Self::Startpage => "Startpage",
        }
    }

    /// Search URL prefix; the encoded query is appended.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "https://duckduckgo.com/?q=",
            Self::Leta => "https://leta.mullvad.net/search?q=",
            Self::Brave => "https://search.brave.com/search?q=",
            Self::Startpage => "https://www.startpage.com/do/search?q=",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub search: SearchEngine,
    /// Ask before a site may use your Google/Apple/… sign-in.
    pub identity_gate: bool,
    /// Third-party cookies (cross-site trackers, embedded sign-in widgets) are blocked.
    pub block_third_party_cookies: bool,
    /// Offer to save passwords when you sign in somewhere.
    pub offer_passwords: bool,
    /// Ask for the vault PIN/passphrase every time a saved password is filled, shown or copied.
    pub confirm_password_use: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            search: SearchEngine::default(),
            identity_gate: true,
            block_third_party_cookies: true,
            offer_passwords: true,
            confirm_password_use: true,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Data {
    prefs: Prefs,
    bookmarks: Vec<Bookmark>,
    passwords: Vec<Credential>,
    /// Sites where saving passwords was declined for good.
    never_save: Vec<String>,
    /// `"<site> <provider>"` → decision.
    identity: BTreeMap<String, Decision>,
    next_id: u64,
}

/// The profile, loaded from (and saved back to) one file in the vault.
pub struct Profile {
    path: Option<PathBuf>,
    data: Data,
}

impl Profile {
    /// Loads `dir/profile.json` (missing = empty profile).
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("profile.json");
        let data = fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path: Some(path), data }
    }

    /// A profile that is never written (tests, or before unlock).
    pub fn in_memory() -> Self {
        Self { path: None, data: Data::default() }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        let json = serde_json::to_vec_pretty(&self.data).expect("profile serializes");
        if let Err(e) = fs::write(&tmp, json).and_then(|_| fs::rename(&tmp, path)) {
            log::warn!("profile: saving failed: {e}");
        }
    }

    fn next_id(&mut self) -> u64 {
        self.data.next_id += 1;
        self.data.next_id
    }

    // ── preferences ────────────────────────────────────────────────────────

    pub fn prefs(&self) -> &Prefs {
        &self.data.prefs
    }

    pub fn set_prefs(&mut self, prefs: Prefs) {
        self.data.prefs = prefs;
        self.save();
    }

    // ── bookmarks ──────────────────────────────────────────────────────────

    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.data.bookmarks
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.data.bookmarks.iter().any(|b| same_page(&b.url, url))
    }

    /// Adds the page, or removes it if it was already bookmarked. Returns whether it's now saved.
    pub fn toggle_bookmark(&mut self, url: &str, title: &str) -> bool {
        if self.is_bookmarked(url) {
            self.data.bookmarks.retain(|b| !same_page(&b.url, url));
            self.save();
            return false;
        }
        self.add_bookmark(url, title);
        true
    }

    pub fn add_bookmark(&mut self, url: &str, title: &str) {
        if url.is_empty() || self.is_bookmarked(url) {
            return;
        }
        let id = self.next_id();
        let title = if title.trim().is_empty() { url.to_owned() } else { title.trim().to_owned() };
        self.data.bookmarks.push(Bookmark { id, title, url: url.to_owned(), added: now() });
        self.save();
    }

    pub fn rename_bookmark(&mut self, id: u64, title: &str) {
        if let Some(b) = self.data.bookmarks.iter_mut().find(|b| b.id == id) {
            b.title = title.trim().to_owned();
            self.save();
        }
    }

    pub fn remove_bookmark(&mut self, id: u64) {
        self.data.bookmarks.retain(|b| b.id != id);
        self.save();
    }

    /// Imports bookmarks, skipping ones already saved. Returns how many were added.
    pub fn import_bookmarks(&mut self, items: Vec<(String, String)>) -> usize {
        let mut added = 0;
        for (title, url) in items {
            if !self.is_bookmarked(&url) && (url.starts_with("http://") || url.starts_with("https://")) {
                let id = self.next_id();
                self.data.bookmarks.push(Bookmark { id, title, url, added: now() });
                added += 1;
            }
        }
        self.save();
        added
    }

    // ── passwords ──────────────────────────────────────────────────────────

    pub fn passwords(&self) -> &[Credential] {
        &self.data.passwords
    }

    /// Saved logins for a page's origin (`https://github.com/login` → `https://github.com`).
    pub fn logins_for(&self, url: &str) -> Vec<&Credential> {
        let Some(origin) = origin_of(url) else { return Vec::new() };
        self.data.passwords.iter().filter(|c| c.origin == origin).collect()
    }

    /// Whether a just-submitted login is new or changed (and saving it is worth offering).
    pub fn should_offer(&self, url: &str, username: &str, password: &str) -> bool {
        let Some(origin) = origin_of(url) else { return false };
        if password.is_empty() || !self.data.prefs.offer_passwords {
            return false;
        }
        if site_of(url).is_some_and(|s| self.data.never_save.contains(&s)) {
            return false;
        }
        !self.data.passwords.iter().any(|c| c.origin == origin && c.username == username && c.password == password)
    }

    /// Saves (or updates the password of) a login.
    pub fn save_login(&mut self, url: &str, username: &str, password: &str) {
        let Some(origin) = origin_of(url) else { return };
        if let Some(existing) = self.data.passwords.iter_mut().find(|c| c.origin == origin && c.username == username) {
            existing.password = password.to_owned();
        } else {
            let id = self.next_id();
            self.data.passwords.push(Credential {
                id,
                origin,
                username: username.to_owned(),
                password: password.to_owned(),
                created: now(),
                last_used: 0,
            });
        }
        self.save();
    }

    pub fn never_save_on(&mut self, url: &str) {
        if let Some(site) = site_of(url)
            && !self.data.never_save.contains(&site) {
                self.data.never_save.push(site);
                self.save();
            }
    }

    pub fn mark_used(&mut self, id: u64) {
        if let Some(c) = self.data.passwords.iter_mut().find(|c| c.id == id) {
            c.last_used = now();
            self.save();
        }
    }

    pub fn remove_login(&mut self, id: u64) {
        self.data.passwords.retain(|c| c.id != id);
        self.save();
    }

    /// Imports `(url, username, password)` rows. Returns how many were new.
    pub fn import_logins(&mut self, rows: Vec<(String, String, String)>) -> usize {
        let before = self.data.passwords.len();
        for (url, username, password) in rows {
            let Some(origin) = origin_of(&url) else { continue };
            if password.is_empty() || self.data.passwords.iter().any(|c| c.origin == origin && c.username == username) {
                continue;
            }
            let id = self.next_id();
            self.data.passwords.push(Credential { id, origin, username, password, created: now(), last_used: 0 });
        }
        self.save();
        self.data.passwords.len() - before
    }

    // ── sign-in decisions ──────────────────────────────────────────────────

    pub fn identity_decisions(&self) -> Vec<(String, String, Decision)> {
        self.data
            .identity
            .iter()
            .filter_map(|(key, d)| key.split_once(' ').map(|(site, p)| (site.to_owned(), p.to_owned(), *d)))
            .collect()
    }

    pub fn identity_decision(&self, site: &str, provider: &str) -> Option<Decision> {
        self.data.identity.get(&format!("{site} {provider}")).copied()
    }

    pub fn set_identity_decision(&mut self, site: &str, provider: &str, decision: Option<Decision>) {
        let key = format!("{site} {provider}");
        match decision {
            Some(d) => self.data.identity.insert(key, d),
            None => self.data.identity.remove(&key),
        };
        self.save();
    }
}

/// `https://Example.com:8443/a?b` → `https://example.com:8443`.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    (!host.is_empty()).then(|| format!("{scheme}://{}", host.to_ascii_lowercase()))
}

/// Same page, ignoring a trailing slash and the fragment.
fn same_page(a: &str, b: &str) -> bool {
    let norm = |u: &str| u.split('#').next().unwrap_or(u).trim_end_matches('/').to_owned();
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarks_toggle_and_dedupe() {
        let mut p = Profile::in_memory();
        assert!(p.toggle_bookmark("https://github.com/", "GitHub"));
        assert!(p.is_bookmarked("https://github.com"));
        assert!(!p.toggle_bookmark("https://github.com#top", "GitHub"));
        assert!(p.bookmarks().is_empty());
        let added = p.import_bookmarks(vec![
            ("A".into(), "https://a.example".into()),
            ("A again".into(), "https://a.example/".into()),
            ("js".into(), "javascript:alert(1)".into()),
        ]);
        assert_eq!(added, 1);
    }

    #[test]
    fn logins_by_origin() {
        let mut p = Profile::in_memory();
        assert!(p.should_offer("https://github.com/login", "me", "pw1"));
        p.save_login("https://github.com/login", "me", "pw1");
        assert!(!p.should_offer("https://github.com/session", "me", "pw1"));
        assert!(p.should_offer("https://github.com/session", "me", "pw2"), "changed password");
        p.save_login("https://github.com/session", "me", "pw2");
        assert_eq!(p.passwords().len(), 1);
        assert_eq!(p.logins_for("https://github.com/x").len(), 1);
        assert!(p.logins_for("https://gist.github.com/").is_empty(), "other origins don't match");
        p.never_save_on("https://example.org/login");
        assert!(!p.should_offer("https://www.example.org/login", "a", "b"));
    }

    #[test]
    fn persists() {
        let dir = std::env::temp_dir().join(format!("unskilled-profile-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut p = Profile::load(&dir);
        p.add_bookmark("https://example.com", "Example");
        p.set_identity_decision("claude.ai", "google", Some(Decision::Allow));
        let p = Profile::load(&dir);
        assert_eq!(p.bookmarks().len(), 1);
        assert_eq!(p.identity_decision("claude.ai", "google"), Some(Decision::Allow));
        let _ = fs::remove_dir_all(&dir);
    }
}
