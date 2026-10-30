//! Filter list catalog: uBlock Origin's defaults plus anti-adblock and tracking-parameter lists.

use std::{
    fs,
    io::Read,
    path::Path,
    time::{Duration, SystemTime},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Ads,
    Trackers,
    Malware,
    AntiAdblock,
    /// Cookie-consent banners and walls.
    Cookies,
    /// uBO's own lists mix ads, trackers and fixes.
    Mixed,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ads => "ad",
            Self::Trackers => "tracker",
            Self::Malware => "malware",
            Self::AntiAdblock => "anti-adblock",
            Self::Cookies => "cookie banner",
            Self::Mixed => "ad / tracker",
        }
    }
}

pub struct List {
    pub id: &'static str,
    pub title: &'static str,
    pub category: Category,
    /// uBO's own lists may use trusted scriptlets (e.g. YouTube's `trusted-replace-*`).
    pub trusted: bool,
    pub hosts: bool,
    /// Refresh interval. uBO's own lists change several times a day (YouTube's anti-adblock
    /// countermeasures land in quick fixes), third-party lists daily.
    pub max_age: Duration,
    pub urls: &'static [&'static str],
}

const HOURS: u64 = 60 * 60;
const FAST: Duration = Duration::from_secs(4 * HOURS);
const DAILY: Duration = Duration::from_secs(24 * HOURS);

pub const LISTS: &[List] = &[
    List {
        id: "ublock-filters",
        title: "uBlock filters",
        category: Category::Mixed,
        trusted: true,
        max_age: FAST,
        hosts: false,
        urls: &[
            "https://ublockorigin.github.io/uAssetsCDN/filters/filters.min.txt",
            "https://ublockorigin.pages.dev/filters/filters.min.txt",
        ],
    },
    List {
        id: "ublock-badware",
        title: "uBlock filters – badware risks",
        category: Category::Malware,
        trusted: true,
        max_age: FAST,
        hosts: false,
        urls: &[
            "https://ublockorigin.github.io/uAssetsCDN/filters/badware.min.txt",
            "https://ublockorigin.pages.dev/filters/badware.min.txt",
        ],
    },
    List {
        id: "ublock-privacy",
        title: "uBlock filters – privacy",
        category: Category::Trackers,
        trusted: true,
        max_age: FAST,
        hosts: false,
        urls: &[
            "https://ublockorigin.github.io/uAssetsCDN/filters/privacy.min.txt",
            "https://ublockorigin.pages.dev/filters/privacy.min.txt",
        ],
    },
    List {
        id: "ublock-quick-fixes",
        title: "uBlock filters – quick fixes",
        category: Category::Mixed,
        trusted: true,
        max_age: FAST,
        hosts: false,
        urls: &[
            "https://ublockorigin.github.io/uAssetsCDN/filters/quick-fixes.min.txt",
            "https://ublockorigin.pages.dev/filters/quick-fixes.min.txt",
        ],
    },
    List {
        id: "ublock-unbreak",
        title: "uBlock filters – unbreak",
        category: Category::Mixed,
        trusted: true,
        max_age: FAST,
        hosts: false,
        urls: &[
            "https://ublockorigin.github.io/uAssetsCDN/filters/unbreak.min.txt",
            "https://ublockorigin.pages.dev/filters/unbreak.min.txt",
        ],
    },
    List {
        id: "ublock-removeparam",
        title: "uBlock filters – tracking parameters",
        category: Category::Trackers,
        trusted: true,
        max_age: FAST,
        hosts: false,
        urls: &[
            "https://ublockorigin.github.io/uAssetsCDN/filters/privacy-removeparam.txt",
            "https://ublockorigin.pages.dev/filters/privacy-removeparam.txt",
        ],
    },
    List {
        id: "easylist",
        title: "EasyList",
        category: Category::Ads,
        trusted: false,
        max_age: DAILY,
        hosts: false,
        urls: &[
            "https://cdn.jsdelivr.net/gh/uBlockOrigin/uAssetsCDN@main/thirdparties/easylist.txt",
            "https://ublockorigin.pages.dev/thirdparties/easylist.txt",
        ],
    },
    List {
        id: "easyprivacy",
        title: "EasyPrivacy",
        category: Category::Trackers,
        trusted: false,
        max_age: DAILY,
        hosts: false,
        urls: &[
            "https://cdn.jsdelivr.net/gh/uBlockOrigin/uAssetsCDN@main/thirdparties/easyprivacy.txt",
            "https://ublockorigin.pages.dev/thirdparties/easyprivacy.txt",
        ],
    },
    List {
        id: "plowe",
        title: "Peter Lowe's ad and tracking server list",
        category: Category::Ads,
        trusted: false,
        max_age: DAILY,
        hosts: true,
        urls: &["https://pgl.yoyo.org/adservers/serverlist.php?hostformat=hosts&showintro=1&mimetype=plaintext"],
    },
    List {
        id: "urlhaus",
        title: "Online Malicious URL Blocklist",
        category: Category::Malware,
        trusted: false,
        max_age: DAILY,
        hosts: false,
        urls: &[
            "https://curbengh.github.io/malware-filter/urlhaus-filter-vivaldi-online.txt",
            "https://malware-filter.gitlab.io/urlhaus-filter/urlhaus-filter-vivaldi-online.txt",
        ],
    },
    List {
        id: "antiadblock",
        title: "Adblock Warning Removal List",
        category: Category::AntiAdblock,
        trusted: false,
        max_age: DAILY,
        hosts: false,
        urls: &["https://easylist-downloads.adblockplus.org/antiadblockfilters.txt"],
    },
    // Consent banners: removed (and their scroll locks undone) instead of clicked through. Under
    // GDPR, no answer is no consent.
    List {
        id: "ublock-cookies",
        title: "uBlock filters – Cookie Notices",
        category: Category::Cookies,
        trusted: true,
        max_age: DAILY,
        hosts: false,
        urls: &[
            "https://ublockorigin.github.io/uAssetsCDN/filters/annoyances-cookies.txt",
            "https://ublockorigin.pages.dev/filters/annoyances-cookies.txt",
        ],
    },
    List {
        id: "easylist-cookies",
        title: "EasyList Cookie",
        category: Category::Cookies,
        trusted: false,
        max_age: DAILY,
        hosts: false,
        urls: &[
            "https://ublockorigin.github.io/uAssetsCDN/thirdparties/easylist-cookies.txt",
            "https://ublockorigin.pages.dev/thirdparties/easylist-cookies.txt",
        ],
    },
];

const MAX_BYTES: u64 = 32 * 1024 * 1024;

fn cache_path(dir: &Path, list: &List) -> std::path::PathBuf {
    dir.join("lists").join(format!("{}.txt", list.id))
}

/// Cached copy and its age.
pub fn cached(dir: &Path, list: &List) -> Option<(String, Duration)> {
    let path = cache_path(dir, list);
    let age = fs::metadata(&path).ok()?.modified().ok()?.elapsed().unwrap_or_default();
    Some((fs::read_to_string(path).ok()?, age))
}

/// Downloads `list`, trying mirrors in order, and caches it.
pub fn download(dir: &Path, list: &List) -> Result<String, String> {
    let mut last_error = String::new();
    for url in list.urls {
        match fetch(url) {
            Ok(text) if looks_like_a_list(&text) => {
                let path = cache_path(dir, list);
                let _ = fs::create_dir_all(path.parent().unwrap());
                if let Err(e) = fs::write(&path, &text) {
                    log::warn!("shield: caching {} failed: {e}", list.id);
                }
                return Ok(text);
            }
            Ok(_) => last_error = format!("{url}: not a filter list"),
            Err(e) => last_error = format!("{url}: {e}"),
        }
    }
    Err(last_error)
}

fn fetch(url: &str) -> Result<String, String> {
    let response = ureq::get(url).call().map_err(|e| e.to_string())?;
    let mut text = String::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_BYTES)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    Ok(text)
}

/// Guards against caching captive-portal pages or error HTML as a list.
fn looks_like_a_list(text: &str) -> bool {
    text.len() > 64 && !text.trim_start().starts_with('<')
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
