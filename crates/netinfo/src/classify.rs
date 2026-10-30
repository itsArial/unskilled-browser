//! Is a site served by a CDN / reverse proxy (real server hidden) or straight from its server?
//! Response headers are the strongest evidence; the network owner fills in the rest.

/// How a site's traffic reaches it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edge {
    /// A CDN or reverse proxy answers; the origin server's address is hidden.
    Proxy { provider: &'static str, evidence: String },
    /// The connection goes to the operator's own server, hosted at `host` (a cloud or ISP).
    Direct { host: String },
    Unknown,
}

/// (header, value substring or "" for presence, provider)
const HEADER_HINTS: &[(&str, &str, &str)] = &[
    ("cf-ray", "", "Cloudflare"),
    ("server", "cloudflare", "Cloudflare"),
    ("x-amz-cf-id", "", "Amazon CloudFront"),
    ("via", "cloudfront", "Amazon CloudFront"),
    ("x-fastly-request-id", "", "Fastly"),
    ("x-served-by", "cache-", "Fastly"),
    ("akamai-grn", "", "Akamai"),
    ("server", "akamai", "Akamai"),
    ("x-akamai-transformed", "", "Akamai"),
    ("x-azure-ref", "", "Azure Front Door"),
    ("x-msedge-ref", "", "Azure CDN"),
    ("x-iinfo", "", "Imperva"),
    ("x-cdn", "imperva", "Imperva"),
    ("x-sucuri-id", "", "Sucuri"),
    ("server", "bunnycdn", "Bunny CDN"),
    ("cdn-pullzone", "", "Bunny CDN"),
    ("x-77-nzt", "", "CDN77"),
    ("server", "ddos-guard", "DDoS-Guard"),
    ("x-vercel-id", "", "Vercel"),
    ("x-nf-request-id", "", "Netlify"),
    ("server", "gws", "Google Front End"),
    ("x-goog-", "", "Google Front End"),
];

/// Networks that only ever act as a CDN / proxy edge.
const PROXY_NETWORKS: &[(u32, &str)] = &[
    (13335, "Cloudflare"),
    (209242, "Cloudflare"),
    (54113, "Fastly"),
    (20940, "Akamai"),
    (16625, "Akamai"),
    (16702, "Akamai"),
    (21342, "Akamai"),
    (12222, "Akamai"),
    (60068, "CDN77"),
    (200325, "Bunny CDN"),
    (19551, "Imperva"),
    (30148, "Sucuri"),
    (22822, "Edgio"),
    (199524, "G-Core"),
    (57724, "DDoS-Guard"),
    (262254, "DDoS-Guard"),
];

/// Friendlier names for common hosting networks.
const HOSTS: &[(u32, &str)] = &[
    (16509, "Amazon AWS"),
    (14618, "Amazon AWS"),
    (15169, "Google"),
    (396982, "Google Cloud"),
    (8075, "Microsoft"),
    (32934, "Meta"),
    (13414, "X / Twitter"),
    (16276, "OVHcloud"),
    (24940, "Hetzner"),
    (14061, "DigitalOcean"),
    (63949, "Akamai Linode"),
    (20473, "Vultr"),
    (31898, "Oracle Cloud"),
    (36351, "IBM Cloud"),
    (51167, "Contabo"),
    (12876, "Scaleway"),
];

/// `asn`/`owner` describe the connected IP; `headers` are the response headers (any case).
pub fn classify(asn: Option<u32>, owner: Option<&str>, headers: &[(String, String)]) -> Edge {
    for &(name, needle, provider) in HEADER_HINTS {
        for (key, value) in headers {
            let key = key.to_ascii_lowercase();
            let matches_key = if name.ends_with('-') { key.starts_with(name) } else { key == name };
            if matches_key && (needle.is_empty() || value.to_ascii_lowercase().contains(needle)) {
                return Edge::Proxy { provider, evidence: format!("{key}: {value}") };
            }
        }
    }
    if let Some(asn) = asn {
        if let Some(&(_, provider)) = PROXY_NETWORKS.iter().find(|(n, _)| *n == asn) {
            return Edge::Proxy { provider, evidence: format!("network AS{asn}") };
        }
        let host = HOSTS
            .iter()
            .find(|(n, _)| *n == asn)
            .map(|(_, h)| h.to_string())
            .or_else(|| owner.map(str::to_owned))
            .unwrap_or_else(|| format!("AS{asn}"));
        return Edge::Direct { host };
    }
    Edge::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(k: &str, v: &str) -> Vec<(String, String)> {
        vec![(k.into(), v.into())]
    }

    #[test]
    fn headers_beat_networks() {
        assert!(matches!(classify(Some(16509), None, &h("CF-RAY", "8a1b-SOF")), Edge::Proxy { provider: "Cloudflare", .. }));
        assert!(matches!(classify(Some(16509), None, &h("x-amz-cf-id", "abc")), Edge::Proxy { provider: "Amazon CloudFront", .. }));
        assert!(matches!(classify(None, None, &h("X-Served-By", "cache-fra-123")), Edge::Proxy { provider: "Fastly", .. }));
    }

    #[test]
    fn networks_and_direct() {
        assert!(matches!(classify(Some(13335), None, &[]), Edge::Proxy { provider: "Cloudflare", .. }));
        assert_eq!(classify(Some(24940), Some("HETZNER-AS"), &[]), Edge::Direct { host: "Hetzner".into() });
        assert_eq!(classify(Some(64500), Some("SMALL-ISP"), &[]), Edge::Direct { host: "SMALL-ISP".into() });
        assert_eq!(classify(None, None, &[]), Edge::Unknown);
    }
}
