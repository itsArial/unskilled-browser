//! Debug panel: where the current site really lives (resolved IP, network owner, CDN or direct,
//! TLS), every host the page talked to, and every tracker it tried. Opens Chrome DevTools too.

use crate::{panel, theme, vpn::VpnStatus};
use engine::{BlockEvent, Tls};
use netinfo::{Asn, Edge};
use ui::{Color, Rect, Scene, Spring, SpringConfig, Weight};

const WIDTH: f32 = 470.0;
const PAD: f32 = 18.0;
const TEXT: f32 = 12.0;
const SMALL: f32 = 11.0;
const LINE: f32 = 17.0;
const ROW_H: f32 = 30.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Site,
    Requests,
    Trackers,
}

pub struct SiteInfo {
    pub host: String,
    pub ip: String,
    pub port: u16,
    pub protocol: String,
    pub status: u16,
    pub tls: Option<Tls>,
    pub asn: Option<Asn>,
    pub edge: Edge,
    pub server: Option<String>,
}

pub struct HostRow {
    pub host: String,
    pub requests: usize,
    pub failed: usize,
    pub bytes: u64,
    pub ips: Vec<String>,
    pub asn: Option<Asn>,
    pub edge: Edge,
    pub protocols: Vec<String>,
    pub sample: Vec<String>,
}

pub struct InspectorData {
    /// `None` on the start page or before the first response.
    pub site: Option<SiteInfo>,
    pub hosts: Vec<HostRow>,
    pub exit: VpnStatus,
    pub lookups: netinfo::Status,
    pub events: Vec<BlockEvent>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InspectorHit {
    Tab(Tab),
    DevTools,
    Row(usize),
    Inside,
}

pub struct Inspector {
    pub open: Spring,
    tab: Tab,
    scroll: Spring,
    expanded: Option<String>,
    hover: Option<InspectorHit>,
    card: Rect,
    tabs: Vec<(Tab, Rect)>,
    devtools: Rect,
    list: Rect,
    rows: Vec<(usize, Rect)>,
}

impl Inspector {
    pub fn new() -> Self {
        Self {
            open: Spring::new(0.0, SpringConfig::from_response(0.28, 0.86)),
            tab: Tab::Site,
            scroll: Spring::new(0.0, SpringConfig::GLIDE),
            expanded: None,
            hover: None,
            card: Rect::default(),
            tabs: Vec::new(),
            devtools: Rect::default(),
            list: Rect::default(),
            rows: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.target > 0.5
    }

    pub fn toggle(&mut self) {
        self.open.set(if self.is_open() { 0.0 } else { 1.0 });
    }

    pub fn close(&mut self) {
        self.open.set(0.0);
    }

    pub fn step(&mut self, dt: f32) -> bool {
        self.open.step(dt) | self.scroll.step(dt)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<InspectorHit> {
        if self.open.value < 0.05 || !self.card.contains(x, y) {
            return None;
        }
        if let Some(&(tab, _)) = self.tabs.iter().find(|(_, r)| r.contains(x, y)) {
            return Some(InspectorHit::Tab(tab));
        }
        if self.devtools.contains(x, y) {
            return Some(InspectorHit::DevTools);
        }
        if self.list.contains(x, y)
            && let Some(&(i, _)) = self.rows.iter().find(|(_, r)| r.contains(x, y)) {
                return Some(InspectorHit::Row(i));
            }
        Some(InspectorHit::Inside)
    }

    pub fn set_hover(&mut self, hit: Option<InspectorHit>) {
        self.hover = hit;
    }

    pub fn select(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.scroll.snap_to(0.0);
            self.expanded = None;
        }
    }

    /// Expands/collapses a row; `key` identifies it across frames.
    pub fn toggle_row(&mut self, key: String) {
        self.expanded = if self.expanded.as_deref() == Some(&key) { None } else { Some(key) };
    }

    pub fn row_key(&self, data: &InspectorData, i: usize) -> Option<String> {
        match self.tab {
            Tab::Site => None,
            Tab::Requests => data.hosts.get(i).map(|h| h.host.clone()),
            Tab::Trackers => data.events.get(i).map(|e| format!("{:?}{}", e.at, e.url)),
        }
    }

    pub fn over_list(&self, x: f32, y: f32) -> bool {
        self.open.value > 0.5 && self.list.contains(x, y)
    }

    pub fn scroll_by(&mut self, dy: f32) {
        let t = (self.scroll.target - dy).max(0.0);
        self.scroll.set(t);
    }

    pub fn draw(&mut self, scene: &mut Scene, width: f32, height: f32, top: f32, data: &InspectorData) {
        let open = self.open.value.clamp(0.0, 1.0);
        if open < 0.01 {
            self.card = Rect::default();
            return;
        }
        let a = open;
        let x = width - 14.0 - WIDTH;
        let y = top + (1.0 - open) * -10.0;
        let card = Rect::new(x, y, WIDTH, (height - top - 14.0).max(240.0));
        self.card = card;
        scene.glow(card, 20.0, 28.0, Color::rgba(0, 0, 0, 200).with_alpha(a));
        scene.fill(card, 20.0, theme::RAISED.with_alpha(a));
        scene.stroke(card, 20.0, 1.0, theme::HAIRLINE.with_alpha(a));

        let ix = x + PAD;
        let iw = WIDTH - 2.0 * PAD;
        let mut cy = y + PAD + 8.0;
        scene.text(ix, cy, "inspector", 12.5, Weight::Medium, theme::TEXT_DIM.with_alpha(a));

        // DevTools button.
        let label = "devtools  f12";
        let bw = scene.text.measure(label, SMALL, Weight::Regular) + 24.0;
        let button = Rect::new(ix + iw - bw, cy - 12.0, bw, 24.0);
        let hover = self.hover == Some(InspectorHit::DevTools);
        scene.stroke(button, 12.0, 1.0, if hover { theme::HAIRLINE_HOVER } else { theme::HAIRLINE }.with_alpha(a));
        scene.text_centered(button.center().0, cy, label, SMALL, Weight::Regular, if hover { theme::TEXT } else { theme::TEXT_DIM }.with_alpha(a));
        self.devtools = button;
        cy += 26.0;

        // Segmented tabs.
        self.tabs.clear();
        let names = [(Tab::Site, "site"), (Tab::Requests, "requests"), (Tab::Trackers, "trackers")];
        let seg_w = iw / names.len() as f32;
        let seg = Rect::new(ix, cy, iw, 30.0);
        scene.stroke(seg, 15.0, 1.0, theme::HAIRLINE.with_alpha(a));
        for (i, (tab, name)) in names.iter().enumerate() {
            let r = Rect::new(ix + i as f32 * seg_w, cy, seg_w, 30.0);
            let active = self.tab == *tab;
            if active {
                scene.fill(r.inset(2.0), 13.0, theme::TEXT.with_alpha(a));
            }
            let count = match tab {
                Tab::Site => String::new(),
                Tab::Requests => format!(" {}", data.hosts.len()),
                Tab::Trackers => format!(" {}", data.events.len()),
            };
            let color = if active { theme::ON_LIGHT } else { theme::TEXT_DIM };
            scene.text_centered(r.center().0, r.center().1, &format!("{name}{count}"), TEXT, Weight::Regular, color.with_alpha(a));
            self.tabs.push((*tab, r));
        }
        cy += 44.0;

        let list = Rect::new(ix, cy, iw, card.bottom() - cy - PAD);
        self.list = list;
        self.rows.clear();
        match self.tab {
            Tab::Site => self.draw_site(scene, list, data, a),
            Tab::Requests => self.draw_hosts(scene, list, data, a),
            Tab::Trackers => self.draw_trackers(scene, list, data, a),
        }
    }

    fn draw_site(&mut self, scene: &mut Scene, list: Rect, data: &InspectorData, a: f32) {
        let mut lines: Vec<(&str, String, Color)> = Vec::new();
        match &data.site {
            None => lines.push(("site", "no page loaded yet".into(), theme::TEXT_FAINT)),
            Some(site) => {
                lines.push(("site", site.host.clone(), theme::TEXT));
                let ip = if site.ip.is_empty() { "not resolved (cache or service worker)".into() } else { format!("{}  port {}", site.ip, site.port) };
                lines.push(("resolved ip", ip, theme::TEXT));
                let network = match (&site.asn, data.lookups) {
                    (Some(asn), _) => format!("AS{}  {}  ·  {}", asn.number, asn.name, asn.country),
                    (None, netinfo::Status::Loading) => "looking up (downloading the offline database)".into(),
                    (None, netinfo::Status::Unavailable) => "offline database unavailable".into(),
                    (None, netinfo::Status::Ready) => "unknown network".into(),
                };
                lines.push(("network", network, theme::TEXT));
                match &site.edge {
                    Edge::Proxy { provider, evidence } => {
                        lines.push(("served by", format!("{provider} — CDN / reverse proxy"), theme::TEXT));
                        lines.push(("", "the real server's address is hidden behind it".into(), theme::TEXT_DIM));
                        lines.push(("evidence", evidence.clone(), theme::TEXT_DIM));
                    }
                    Edge::Direct { host } => {
                        lines.push(("served by", "direct — no CDN or proxy in front".into(), theme::TEXT));
                        lines.push(("hosted at", host.clone(), theme::TEXT));
                    }
                    Edge::Unknown => lines.push(("served by", "unknown".into(), theme::TEXT_DIM)),
                }
                if let Some(server) = &site.server {
                    lines.push(("server", server.clone(), theme::TEXT_DIM));
                }
                let mut proto = site.protocol.clone();
                if let Some(tls) = &site.tls {
                    proto = format!("{proto}  ·  {}  ·  {}", tls.protocol, tls.cipher);
                    if !tls.key_exchange.is_empty() {
                        proto.push_str(&format!("  ·  {}", tls.key_exchange));
                    }
                } else if site.port != 0 {
                    proto.push_str("  ·  not encrypted");
                }
                lines.push(("protocol", proto, theme::TEXT));
                if let Some(tls) = &site.tls {
                    lines.push(("certificate", format!("{} — issued by {}", tls.subject, tls.issuer), theme::TEXT));
                    lines.push(("valid until", date(tls.valid_to), theme::TEXT_DIM));
                }
                if site.status != 0 {
                    lines.push(("status", site.status.to_string(), theme::TEXT_DIM));
                }
            }
        }
        lines.push(("", String::new(), theme::TEXT));
        match &data.exit {
            VpnStatus::Connected { relay, location, ipv4 } => {
                lines.push(("you appear as", format!("{ipv4}  ·  {location}"), theme::TEXT));
                lines.push(("via", format!("Mullvad  {relay}"), theme::TEXT_DIM));
                lines.push(("dns", "resolved inside the Mullvad tunnel".into(), theme::TEXT_DIM));
            }
            VpnStatus::Connecting => lines.push(("you appear as", "Mullvad is connecting…".into(), theme::TEXT_DIM)),
            _ => lines.push(("you appear as", "your real IP — Mullvad is not connected".into(), theme::ACCENT)),
        }

        let label_w = 104.0;
        let value_w = list.w - label_w;
        let char_w = scene.text.measure("0", TEXT, Weight::Regular);
        let per_line = (value_w / char_w).floor().max(8.0) as usize;
        let mut y = list.y + 8.0;
        scene.clipped(list, |scene| {
            for (label, value, color) in &lines {
                scene.text(list.x, y, label, SMALL, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
                let chars: Vec<char> = value.chars().collect();
                if chars.is_empty() {
                    y += LINE * 0.6;
                    continue;
                }
                for chunk in chars.chunks(per_line).take(3) {
                    let text: String = chunk.iter().collect();
                    scene.text(list.x + label_w, y, &text, TEXT, Weight::Regular, color.with_alpha(a));
                    y += LINE;
                }
                y += 3.0;
            }
        });
    }

    fn draw_hosts(&mut self, scene: &mut Scene, list: Rect, data: &InspectorData, a: f32) {
        if data.hosts.is_empty() {
            scene.text(list.x, list.y + 10.0, "no requests yet", SMALL, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
            return;
        }
        let details: Vec<Vec<String>> = data
            .hosts
            .iter()
            .map(|h| if self.expanded.as_deref() == Some(&h.host) { host_details(h) } else { Vec::new() })
            .collect();
        let heights: Vec<f32> = details.iter().map(|d| ROW_H + d.len() as f32 * LINE + if d.is_empty() { 0.0 } else { 8.0 }).collect();
        let mut y = self.scrolled(list, &heights);
        let hover = self.hover;
        let mut rows = Vec::new();
        scene.clipped(list, |scene| {
            for (i, (host, h)) in data.hosts.iter().zip(&heights).enumerate() {
                if y > list.bottom() {
                    break;
                }
                if y + h >= list.y {
                    let row = Rect::new(list.x, y, list.w, ROW_H);
                    rows.push((i, row));
                    if hover == Some(InspectorHit::Row(i)) || !details[i].is_empty() {
                        scene.fill(Rect::new(list.x, y, list.w, *h), 10.0, theme::SURFACE.with_alpha(a));
                    }
                    let cy = row.center().1;
                    let (tag, tag_color) = match &host.edge {
                        Edge::Proxy { provider, .. } => (short(provider), theme::TEXT),
                        Edge::Direct { host } => (short(host), theme::TEXT_DIM),
                        Edge::Unknown => ("—".to_owned(), theme::TEXT_FAINT),
                    };
                    let right = format!("{}  {}", host.requests, size(host.bytes));
                    let rx = scene.text_right(row.right() - 8.0, cy, &right, SMALL, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
                    let tag_x = rx - 12.0 - scene.text.measure(&tag, SMALL, Weight::Regular);
                    scene.text(tag_x, cy, &tag, SMALL, Weight::Regular, tag_color.with_alpha(a));
                    let name = scene.text.fit(&host.host, TEXT, Weight::Regular, tag_x - row.x - 18.0);
                    let color = if host.failed == host.requests { theme::TEXT_FAINT } else { theme::TEXT };
                    scene.text(row.x + 8.0, cy, &name, TEXT, Weight::Regular, color.with_alpha(a));
                    let mut ly = y + ROW_H + 2.0;
                    for line in &details[i] {
                        scene.text(row.x + 12.0, ly, line, SMALL, Weight::Regular, theme::TEXT_DIM.with_alpha(a));
                        ly += LINE;
                    }
                }
                y += h;
            }
        });
        self.rows = rows;
    }

    fn draw_trackers(&mut self, scene: &mut Scene, list: Rect, data: &InspectorData, a: f32) {
        if data.events.is_empty() {
            scene.text(list.x, list.y + 10.0, "no tracker or ad requests on this page", SMALL, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
            return;
        }
        let char_w = scene.text.measure("0", SMALL, Weight::Regular);
        let chars = ((list.w - 20.0) / char_w).floor().max(8.0) as usize;
        let details: Vec<Vec<(String, bool)>> = data
            .events
            .iter()
            .map(|e| {
                let key = format!("{:?}{}", e.at, e.url);
                if self.expanded.as_deref() == Some(&key) { panel::detail_lines(e, chars) } else { Vec::new() }
            })
            .collect();
        let heights: Vec<f32> = details.iter().map(|d| ROW_H + d.len() as f32 * 15.0 + if d.is_empty() { 0.0 } else { 10.0 }).collect();
        let mut y = self.scrolled(list, &heights);
        let hover = self.hover;
        let mut rows = Vec::new();
        scene.clipped(list, |scene| {
            for (i, (event, h)) in data.events.iter().zip(&heights).enumerate() {
                if y > list.bottom() {
                    break;
                }
                if y + h >= list.y {
                    let row = Rect::new(list.x, y, list.w, ROW_H);
                    rows.push((i, row));
                    if hover == Some(InspectorHit::Row(i)) || !details[i].is_empty() {
                        scene.fill(Rect::new(list.x, y, list.w, *h), 10.0, theme::SURFACE.with_alpha(a));
                    }
                    panel::draw_row(scene, event, row, a);
                    let mut ly = y + ROW_H + 4.0;
                    for (line, dim) in &details[i] {
                        let color = if *dim { theme::TEXT_FAINT } else { theme::TEXT_DIM };
                        scene.text(row.x + 10.0, ly, line, 11.5, Weight::Regular, color.with_alpha(a));
                        ly += 15.0;
                    }
                }
                y += h;
            }
        });
        self.rows = rows;
    }

    /// First row's y after scrolling, clamping the scroll to the content.
    fn scrolled(&mut self, list: Rect, heights: &[f32]) -> f32 {
        let total: f32 = heights.iter().sum();
        let max = (total - list.h).max(0.0);
        if self.scroll.target > max {
            self.scroll.set(max);
        }
        list.y - self.scroll.value.min(max + 40.0)
    }
}

fn host_details(h: &HostRow) -> Vec<String> {
    let mut out = Vec::new();
    if !h.ips.is_empty() {
        out.push(format!("ip       {}", h.ips.join(", ")));
    }
    if let Some(asn) = &h.asn {
        out.push(format!("network  AS{} {} · {}", asn.number, asn.name, asn.country));
    }
    match &h.edge {
        Edge::Proxy { provider, evidence } => out.push(format!("edge     {provider} ({evidence})")),
        Edge::Direct { host } => out.push(format!("edge     direct, hosted at {host}")),
        Edge::Unknown => {}
    }
    if !h.protocols.is_empty() {
        out.push(format!("protocol {}", h.protocols.join(", ")));
    }
    if h.failed > 0 {
        out.push(format!("failed   {} of {} (blocked or aborted)", h.failed, h.requests));
    }
    for url in &h.sample {
        out.push(trim(url, 58));
    }
    out
}

fn short(name: &str) -> String {
    trim(name, 18)
}

fn trim(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

fn size(bytes: u64) -> String {
    match bytes {
        0 => "—".into(),
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.0} kB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / 1048576.0),
    }
}

/// Unix seconds → `YYYY-MM-DD` (UTC).
fn date(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_dates_and_sizes() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_800_000_000), "2027-01-15");
        assert_eq!(size(1536), "2 kB");
        assert_eq!(trim("abcdef", 4), "abc…");
    }
}
