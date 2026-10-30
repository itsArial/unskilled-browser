//! Built-in content blocker: uBlock Origin's lists and scriptlets on Brave's adblock-rust engine.
//! Blocks ads (including YouTube's), trackers and malware, strips tracking parameters, and
//! defuses anti-adblock walls the same way uBO does.

mod lists;
mod preprocess;

pub use lists::{Category, LISTS};

use adblock::{
    Engine, FilterSet,
    lists::{FilterFormat, ParseOptions},
    resources::{PermissionMask, Resource},
};
use base64::Engine as _;
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const RESOURCES: &str = include_str!("../assets/resources.json");
const PAGE_SCRIPT: &str = include_str!("../assets/page.js");
const TRUSTED: PermissionMask = PermissionMask::from_bits(1);
const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Document,
    Subdocument,
    Stylesheet,
    Script,
    Image,
    Font,
    Media,
    Xhr,
    Fetch,
    Ping,
    WebSocket,
    Other,
}

impl ResourceKind {
    fn as_adblock(self) -> &'static str {
        match self {
            Self::Document => "main_frame",
            Self::Subdocument => "sub_frame",
            Self::Stylesheet => "stylesheet",
            Self::Script => "script",
            Self::Image => "image",
            Self::Font => "font",
            Self::Media => "media",
            Self::Xhr | Self::Fetch => "xmlhttprequest",
            Self::Ping => "ping",
            Self::WebSocket => "websocket",
            Self::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Subdocument => "frame",
            Self::Stylesheet => "css",
            Self::Script => "script",
            Self::Image => "image",
            Self::Font => "font",
            Self::Media => "media",
            Self::Xhr => "xhr",
            Self::Fetch => "fetch",
            Self::Ping => "ping",
            Self::WebSocket => "websocket",
            Self::Other => "other",
        }
    }
}

pub struct Request<'a> {
    pub url: &'a str,
    /// Document that issued the request (decides first- vs third-party).
    pub frame_url: &'a str,
    /// The tab's top-level page (decides site exceptions).
    pub top_url: &'a str,
    pub kind: ResourceKind,
    pub method: &'a str,
}

/// Which rule fired, for the tracker log.
#[derive(Clone, Debug)]
pub struct Match {
    pub rule: String,
    pub list: &'static str,
    pub category: Category,
}

pub enum Verdict {
    Allow,
    Block(Match),
    /// Serve this instead (a neutered script, a 1×1 gif, …) so pages that probe for it don't break.
    Redirect { mime: String, body: Vec<u8>, matched: Match },
    /// Same request with tracking parameters removed.
    Rewrite(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Loading,
    Ready { lists: usize, filters: usize, updated_unix: u64 },
    Failed(String),
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Settings {
    paused: bool,
    exceptions: BTreeSet<String>,
}

struct Loaded {
    engine: Engine,
    /// `source_index` → list.
    sources: Vec<&'static lists::List>,
}

pub struct Shield {
    dir: PathBuf,
    loaded: RwLock<Option<Arc<Loaded>>>,
    settings: Mutex<Settings>,
    /// Where settings persist — inside the encrypted vault, since exceptions name sites you visit.
    settings_path: Mutex<Option<PathBuf>>,
    status: Mutex<Status>,
    ready: std::sync::Condvar,
    update_requested: AtomicBool,
    on_change: Box<dyn Fn() + Send + Sync>,
    /// Exceptions for generic cosmetic rules, cached for the last page asked about.
    generic_exceptions: Mutex<Option<(String, HashSet<String>)>>,
}

impl Shield {
    /// `dir` holds the (public) filter lists. Settings stay in memory until `use_settings`.
    pub fn new(dir: PathBuf, on_change: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        let _ = fs::create_dir_all(&dir);
        let shield = Arc::new(Self {
            dir,
            loaded: RwLock::new(None),
            settings: Mutex::new(Settings::default()),
            settings_path: Mutex::new(None),
            status: Mutex::new(Status::Loading),
            ready: std::sync::Condvar::new(),
            update_requested: AtomicBool::new(false),
            on_change: Box::new(on_change),
            generic_exceptions: Mutex::new(None),
        });
        let worker = shield.clone();
        thread::Builder::new()
            .name("shield-lists".into())
            .spawn(move || worker.run_updates())
            .expect("spawn shield thread");
        shield
    }

    // ── network ────────────────────────────────────────────────────────────

    pub fn check(&self, req: &Request) -> Verdict {
        if self.bypassed(req.top_url) {
            return Verdict::Allow;
        }
        let Some(loaded) = self.loaded() else { return Verdict::Allow };
        let Ok(request) = adblock::request::Request::new(req.url, req.frame_url, req.kind.as_adblock(), req.method)
        else {
            return Verdict::Allow;
        };
        let result = loaded.engine.check_network_request(&request);

        // Top-level navigations are never blocked, only cleaned.
        if req.kind != ResourceKind::Document && result.should_block() {
            let matched = loaded.describe(result.filter.as_ref());
            if let Some((mime, body)) = result.redirect.as_deref().and_then(decode_data_url) {
                return Verdict::Redirect { mime, body, matched };
            }
            return Verdict::Block(matched);
        }
        match result.rewritten_url {
            Some(url) if url != req.url => Verdict::Rewrite(url),
            _ => Verdict::Allow,
        }
    }

    // ── cosmetic ───────────────────────────────────────────────────────────

    /// JS for the page's main world at document start: `(function (query) { ... })`.
    pub fn page_script(&self, frame_url: &str, top_url: &str) -> Option<String> {
        if !frame_url.starts_with("http://") && !frame_url.starts_with("https://") {
            return None;
        }
        if self.bypassed(top_url) {
            return None;
        }
        let loaded = self.loaded()?;
        let resources = loaded.engine.url_cosmetic_resources(frame_url);
        let selectors: Vec<&String> = resources.hide_selectors.iter().collect();
        let procedural: Vec<&String> = resources.procedural_actions.iter().collect();
        let generic = !resources.generichide;
        if selectors.is_empty() && procedural.is_empty() && resources.injected_script.is_empty() && !generic {
            return None;
        }
        Some(
            PAGE_SCRIPT
                .replace("__SELECTORS__", &serde_json::to_string(&selectors).unwrap())
                .replace("__PROCEDURAL__", &serde_json::to_string(&procedural).unwrap())
                .replace("__GENERIC__", if generic { "true" } else { "false" })
                .replace("__SCRIPTLETS__", &resources.injected_script),
        )
    }

    /// Generic class/id hide rules for elements the page script found. JSON array of selectors.
    pub fn generic_selectors_json(&self, frame_url: &str, classes: &[String], ids: &[String]) -> String {
        let Some(loaded) = self.loaded() else { return "[]".into() };
        let mut cache = self.generic_exceptions.lock().unwrap();
        if cache.as_ref().is_none_or(|(url, _)| url != frame_url) {
            let exceptions = loaded.engine.url_cosmetic_resources(frame_url).exceptions;
            *cache = Some((frame_url.to_owned(), exceptions));
        }
        let exceptions = &cache.as_ref().unwrap().1;
        let selectors = loaded.engine.hidden_class_id_selectors(classes, ids, exceptions);
        serde_json::to_string(&selectors).unwrap()
    }

    // ── settings ───────────────────────────────────────────────────────────

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    /// Blocks until filters are loaded (or loading failed), up to `timeout`. Lets the browser hold
    /// the very first navigations so no page loads before protection is up.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let status = self.status.lock().unwrap();
        let (status, _) = self.ready.wait_timeout_while(status, timeout, |s| *s == Status::Loading).unwrap();
        *status != Status::Loading
    }

    /// Loads settings from (and saves future changes to) `path`. A plaintext settings file from
    /// before the vault existed is migrated and deleted.
    pub fn use_settings(&self, path: PathBuf) {
        let legacy = self.dir.join("settings.json");
        let read = |p: &std::path::Path| fs::read(p).ok().and_then(|b| serde_json::from_slice::<Settings>(&b).ok());
        let settings = read(&path).or_else(|| read(&legacy)).unwrap_or_default();
        *self.settings.lock().unwrap() = settings;
        *self.settings_path.lock().unwrap() = Some(path);
        self.edit_settings(|_| {});
        let _ = fs::remove_file(legacy);
    }

    pub fn is_paused(&self) -> bool {
        self.settings.lock().unwrap().paused
    }

    pub fn set_paused(&self, paused: bool) {
        self.edit_settings(|s| s.paused = paused);
    }

    pub fn is_excepted(&self, url: &str) -> bool {
        let Some(host) = host_of(url).map(str::to_ascii_lowercase) else { return false };
        let settings = self.settings.lock().unwrap();
        settings.exceptions.iter().any(|site| host == *site || host.ends_with(&format!(".{site}")))
    }

    pub fn set_excepted(&self, url: &str, excepted: bool) {
        let Some(site) = site_of(url) else { return };
        self.edit_settings(|s| {
            if excepted {
                s.exceptions.insert(site);
            } else {
                // Also drop broader entries that would keep covering this host.
                let host = host_of(url).unwrap_or_default().to_ascii_lowercase();
                s.exceptions.retain(|e| !(host == *e || host.ends_with(&format!(".{e}"))));
            }
        });
    }

    pub fn update_now(&self) {
        self.update_requested.store(true, Ordering::Relaxed);
    }

    fn bypassed(&self, top_url: &str) -> bool {
        self.is_paused() || self.is_excepted(top_url)
    }

    fn edit_settings(&self, f: impl FnOnce(&mut Settings)) {
        {
            let mut settings = self.settings.lock().unwrap();
            f(&mut settings);
            if let Some(path) = self.settings_path.lock().unwrap().as_ref() {
                let json = serde_json::to_vec_pretty(&*settings).unwrap();
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Err(e) = fs::write(path, json) {
                    log::warn!("shield: saving settings failed: {e}");
                }
            }
        }
        (self.on_change)();
    }

    fn loaded(&self) -> Option<Arc<Loaded>> {
        self.loaded.read().unwrap().clone()
    }

    fn set_status(&self, status: Status) {
        *self.status.lock().unwrap() = status;
        self.ready.notify_all();
        (self.on_change)();
    }

    // ── list maintenance ───────────────────────────────────────────────────

    fn run_updates(self: Arc<Self>) {
        // Instant start from cache, then refresh whatever is stale.
        let mut first = true;
        loop {
            let force = self.update_requested.swap(false, Ordering::Relaxed);
            let mut texts = Vec::new();
            let mut changed = first;
            let mut errors = Vec::new();
            for list in LISTS {
                let cached = lists::cached(&self.dir, list);
                let stale = force || cached.as_ref().is_none_or(|(_, age)| *age > list.max_age);
                // On the first pass, start from the cache and refresh right after.
                if stale && !(first && cached.is_some()) {
                    match lists::download(&self.dir, list) {
                        Ok(text) => {
                            texts.push((list, text));
                            changed = true;
                            continue;
                        }
                        Err(e) => {
                            log::warn!("shield: {} update failed: {e}", list.id);
                            errors.push(list.id);
                        }
                    }
                }
                if let Some((text, _)) = cached {
                    texts.push((list, text));
                }
            }

            if changed && !texts.is_empty() {
                let started = Instant::now();
                let (loaded, filters) = build(texts);
                log::info!(
                    "shield: {} lists, {filters} filters, built in {:?}",
                    loaded.sources.len(),
                    started.elapsed()
                );
                let lists = loaded.sources.len();
                *self.loaded.write().unwrap() = Some(Arc::new(loaded));
                *self.generic_exceptions.lock().unwrap() = None;
                self.set_status(Status::Ready { lists, filters, updated_unix: lists::unix_now() });
            } else if self.loaded().is_none() {
                self.set_status(Status::Failed(format!("no lists available ({})", errors.join(", "))));
            }

            if first {
                first = false;
                continue;
            }
            // Sleep, waking early for `update_now`.
            let until = Instant::now() + CHECK_INTERVAL;
            while Instant::now() < until && !self.update_requested.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

impl Loaded {
    fn describe(&self, filter: Option<&adblock::sourcemap::FilterRuleDebugInfo>) -> Match {
        let list = filter
            .and_then(|f| f.source_location.as_ref())
            .and_then(|loc| self.sources.get(loc.source_index as usize).copied());
        Match {
            rule: filter.and_then(|f| f.raw_line.clone()).unwrap_or_default(),
            list: list.map_or("", |l| l.title),
            category: list.map_or(Category::Mixed, |l| l.category),
        }
    }
}

fn build(texts: Vec<(&'static lists::List, String)>) -> (Loaded, usize) {
    // Debug mode keeps each rule's text and source list for the tracker log.
    let mut set = FilterSet::new(true);
    let mut sources = Vec::new();
    let mut filters = 0;
    for (list, text) in texts {
        let text = preprocess::preprocess(&text);
        filters += text.lines().filter(|l| !l.is_empty() && !l.starts_with('!') && !l.starts_with('#')).count();
        let record = set.add_filter_list(
            text,
            ParseOptions {
                format: if list.hosts { FilterFormat::Hosts } else { FilterFormat::Standard },
                permissions: if list.trusted { TRUSTED } else { PermissionMask::default() },
                ..Default::default()
            },
        );
        if sources.len() <= record.source_index {
            sources.resize(record.source_index + 1, list);
        }
        sources[record.source_index] = list;
    }
    let mut engine = Engine::new_with_filter_set(set);
    engine.use_resources(resources());
    (Loaded { engine, sources }, filters)
}

fn resources() -> Vec<Resource> {
    serde_json::from_str(RESOURCES).expect("embedded resources.json is valid")
}

/// `data:<mime>;base64,<payload>` → (mime, bytes).
fn decode_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let (mime, base64) = match meta.strip_suffix(";base64") {
        Some(mime) => (mime, true),
        None => (meta, false),
    };
    let body = if base64 {
        base64::engine::general_purpose::STANDARD.decode(payload).ok()?
    } else {
        payload.as_bytes().to_vec()
    };
    Some((mime.to_owned(), body))
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host.starts_with('[') { host } else { host.split(':').next()? };
    (!host.is_empty()).then_some(host)
}

/// Host without a leading `www.` — the unit of a site exception.
pub fn site_of(url: &str) -> Option<String> {
    let host = host_of(url)?.to_ascii_lowercase();
    Some(host.strip_prefix("www.").map(str::to_owned).unwrap_or(host))
}

#[cfg(test)]
mod tests;
