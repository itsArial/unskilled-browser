//! Sign-in providers ("Sign in with Google", Apple, Microsoft, …). When another site sends you
//! to one of these, the browser asks first; until you agree, the provider sees you signed out,
//! so the site can't learn you have an account there and the provider can't learn you visited it.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// Share the sign-in with this site, every time.
    Allow,
    /// Never share: the provider always sees you signed out from this site.
    SignedOut,
}

pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    hosts: &'static [&'static str],
    /// Path fragments that mark the sign-in endpoints on hosts that also serve normal pages.
    paths: &'static [&'static str],
    /// The provider's own sites: going from one to its sign-in page is not a third party asking.
    owners: &'static [&'static str],
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "google",
        name: "Google",
        hosts: &["accounts.google.com"],
        paths: &[],
        owners: &["google.com", "youtube.com", "gmail.com", "android.com", "googleusercontent.com", "blogger.com"],
    },
    Provider {
        id: "apple",
        name: "Apple",
        hosts: &["appleid.apple.com", "idmsa.apple.com"],
        paths: &[],
        owners: &["apple.com", "icloud.com"],
    },
    Provider {
        id: "microsoft",
        name: "Microsoft",
        hosts: &["login.microsoftonline.com", "login.live.com", "login.microsoft.com"],
        paths: &[],
        owners: &["microsoft.com", "live.com", "outlook.com", "office.com", "bing.com", "xbox.com", "skype.com", "microsoftonline.com", "msn.com"],
    },
    Provider {
        id: "facebook",
        name: "Facebook",
        hosts: &["www.facebook.com", "m.facebook.com", "facebook.com"],
        paths: &["/dialog/oauth", "/login.php"],
        owners: &["facebook.com", "instagram.com", "messenger.com"],
    },
    Provider {
        id: "github",
        name: "GitHub",
        hosts: &["github.com"],
        paths: &["/login/oauth"],
        owners: &["github.com"],
    },
    Provider {
        id: "discord",
        name: "Discord",
        hosts: &["discord.com"],
        paths: &["/oauth2", "/api/oauth2"],
        owners: &["discord.com", "discord.gg"],
    },
    Provider {
        id: "x",
        name: "X",
        hosts: &["twitter.com", "x.com", "api.twitter.com", "api.x.com"],
        paths: &["/i/oauth2", "/oauth/"],
        owners: &["twitter.com", "x.com"],
    },
    Provider {
        id: "linkedin",
        name: "LinkedIn",
        hosts: &["www.linkedin.com", "linkedin.com"],
        paths: &["/oauth/"],
        owners: &["linkedin.com"],
    },
];

/// The sign-in provider `url` belongs to, if it's one of their sign-in endpoints.
pub fn provider_for(url: &str) -> Option<&'static Provider> {
    let (host, path) = host_and_path(url)?;
    PROVIDERS
        .iter()
        .find(|p| p.hosts.contains(&host.as_str()) && (p.paths.is_empty() || p.paths.iter().any(|f| path.contains(f))))
}

impl Provider {
    /// Whether a request from `site` to this provider is the provider talking to itself.
    pub fn owns(&self, site: &str) -> bool {
        self.owners.contains(&site)
    }

    /// The sandbox the provider's own sites share (see `sandbox_of`).
    pub fn sandbox(&self) -> &'static str {
        self.owners[0]
    }

    /// Whether a cookie set on `domain` is the provider's (its sign-in hosts or its own sites).
    pub fn sets_cookies_on(&self, domain: &str) -> bool {
        let domain = domain.trim_start_matches('.');
        self.hosts.contains(&domain) || site_of(domain).is_some_and(|s| self.owns(&s))
    }
}

/// The sandbox a page lives in: its site, except that a provider's own sites share one
/// (google.com, youtube.com and gmail.com are one account). `None` for non-web URLs.
pub fn sandbox_of(url: &str) -> Option<String> {
    host_and_path(url)?;
    let site = site_of(url)?;
    Some(PROVIDERS.iter().find(|p| p.owns(&site)).map_or(site, |p| p.owners[0].to_owned()))
}

fn host_and_path(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.split(':').next()?.to_ascii_lowercase();
    let path = rest[end..].split(['?', '#']).next().unwrap_or("/").to_owned();
    Some((host, path))
}

/// Second-level suffixes where the registrable domain has three labels.
const MULTI_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "me.uk", "com.au", "net.au", "org.au", "co.nz", "co.jp", "ne.jp",
    "co.kr", "com.br", "com.mx", "com.tr", "co.in", "co.za", "com.cn", "com.hk", "com.sg", "com.ar", "co.il",
];

/// The registrable domain ("site") of a URL or origin: `https://mail.google.com/x` → `google.com`.
pub fn site_of(url: &str) -> Option<String> {
    let host = match host_and_path(url) {
        Some((host, _)) => host,
        None => url.trim().to_ascii_lowercase(),
    };
    if host.is_empty() || host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return (!host.is_empty()).then_some(host);
    }
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    let take = if labels.len() >= 3 && MULTI_SUFFIXES.contains(&labels[labels.len() - 2..].join(".").as_str()) { 3 } else { 2 };
    Some(labels[labels.len().saturating_sub(take)..].join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandboxes() {
        assert_eq!(sandbox_of("https://www.youtube.com/watch?v=1").as_deref(), Some("google.com"));
        assert_eq!(sandbox_of("https://mail.google.com/").as_deref(), Some("google.com"));
        assert_eq!(sandbox_of("https://news.ycombinator.com/").as_deref(), Some("ycombinator.com"));
        assert_eq!(sandbox_of("https://www.bbc.co.uk/news").as_deref(), Some("bbc.co.uk"));
        assert_eq!(sandbox_of("unskilled://settings"), None);
        assert_eq!(sandbox_of("about:blank"), None);
    }

    #[test]
    fn detects_sign_in_endpoints() {
        assert_eq!(provider_for("https://accounts.google.com/o/oauth2/v2/auth?client_id=x").unwrap().id, "google");
        assert_eq!(provider_for("https://www.facebook.com/v19.0/dialog/oauth?x").unwrap().id, "facebook");
        assert!(provider_for("https://www.facebook.com/some.page").is_none(), "ordinary pages aren't gated");
        assert_eq!(provider_for("https://github.com/login/oauth/authorize?x").unwrap().id, "github");
        assert!(provider_for("https://github.com/rust-lang/rust").is_none());
        assert!(provider_for("https://example.com/").is_none());
    }

    #[test]
    fn owners_and_sites() {
        let google = provider_for("https://accounts.google.com/").unwrap();
        assert!(google.owns(&site_of("https://www.youtube.com/watch").unwrap()));
        assert!(!google.owns("claude.ai"));
        assert_eq!(site_of("https://mail.google.com/").as_deref(), Some("google.com"));
        assert_eq!(site_of("https://www.bbc.co.uk/news").as_deref(), Some("bbc.co.uk"));
        assert_eq!(site_of("https://claude.ai").as_deref(), Some("claude.ai"));
    }
}
