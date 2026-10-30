//! Extensions panel. First (built-in) extension: the shield — per-site protection, global pause,
//! list status and a log of every tracker/ad request the current page attempted.

use crate::{clock, theme};
use engine::{Action, BlockEvent};
use shield::{Category, Status};
use ui::{Color, Rect, Scene, Spring, SpringConfig, Weight};

const WIDTH: f32 = 392.0;
const PAD: f32 = 18.0;
const ROW_H: f32 = 28.0;
const SMALL: f32 = 11.5;
const LABEL: f32 = 12.5;
const LINE: f32 = 15.0;

/// Everything the panel shows, gathered by the shell each frame it's open.
pub struct PanelData {
    pub site: String,
    pub blocked: usize,
    pub excepted: bool,
    pub paused: bool,
    pub status: Status,
    /// Newest first.
    pub events: Vec<BlockEvent>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PanelHit {
    SiteToggle,
    PauseToggle,
    Update,
    Event(usize),
    Inside,
}

pub struct Panel {
    pub open: Spring,
    site_knob: Spring,
    pause_knob: Spring,
    scroll: Spring,
    expanded: Option<(std::time::SystemTime, String)>,
    hover: Option<PanelHit>,
    card: Rect,
    site_toggle: Rect,
    pause_toggle: Rect,
    update: Rect,
    log: Rect,
    rows: Vec<(usize, Rect)>,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            open: Spring::new(0.0, SpringConfig::from_response(0.28, 0.86)),
            site_knob: Spring::new(1.0, SpringConfig::SETTLE),
            pause_knob: Spring::new(0.0, SpringConfig::SETTLE),
            scroll: Spring::new(0.0, SpringConfig::GLIDE),
            expanded: None,
            hover: None,
            card: Rect::default(),
            site_toggle: Rect::default(),
            pause_toggle: Rect::default(),
            update: Rect::default(),
            log: Rect::default(),
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
        let mut moving = false;
        for s in [&mut self.open, &mut self.site_knob, &mut self.pause_knob, &mut self.scroll] {
            moving |= s.step(dt);
        }
        moving
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<PanelHit> {
        if self.open.value < 0.05 || !self.card.contains(x, y) {
            return None;
        }
        if self.site_toggle.contains(x, y) {
            return Some(PanelHit::SiteToggle);
        }
        if self.pause_toggle.contains(x, y) {
            return Some(PanelHit::PauseToggle);
        }
        if self.update.contains(x, y) {
            return Some(PanelHit::Update);
        }
        if self.log.contains(x, y) {
            for &(i, r) in &self.rows {
                if r.contains(x, y) {
                    return Some(PanelHit::Event(i));
                }
            }
        }
        Some(PanelHit::Inside)
    }

    pub fn set_hover(&mut self, hit: Option<PanelHit>) {
        self.hover = hit;
    }

    /// Expands or collapses a log row.
    pub fn toggle_event(&mut self, event: &BlockEvent) {
        let key = (event.at, event.url.clone());
        self.expanded = if self.expanded.as_ref() == Some(&key) { None } else { Some(key) };
    }

    pub fn over_log(&self, x: f32, y: f32) -> bool {
        self.open.value > 0.5 && self.log.contains(x, y)
    }

    pub fn scroll_by(&mut self, dy: f32) {
        let t = (self.scroll.target - dy).max(0.0);
        self.scroll.set(t);
    }

    pub fn draw(&mut self, scene: &mut Scene, width: f32, height: f32, top: f32, data: &PanelData) {
        let open = self.open.value.clamp(0.0, 1.0);
        if open < 0.01 {
            self.card = Rect::default();
            return;
        }
        self.site_knob.set(if data.excepted { 0.0 } else { 1.0 });
        self.pause_knob.set(if data.paused { 1.0 } else { 0.0 });

        let x = width - 14.0 - WIDTH;
        let y = top + (1.0 - open) * -10.0;
        let h = (height - top - 14.0).clamp(200.0, 640.0);
        let card = Rect::new(x, y, WIDTH, h);
        self.card = card;
        let a = open;

        scene.glow(card, 20.0, 28.0, Color::rgba(0, 0, 0, 200).with_alpha(a));
        scene.fill(card, 20.0, theme::RAISED.with_alpha(a));
        scene.stroke(card, 20.0, 1.0, theme::HAIRLINE.with_alpha(a));

        let inner_x = x + PAD;
        let inner_w = WIDTH - 2.0 * PAD;
        let mut cy = y + PAD + 8.0;

        // Header.
        scene.text(inner_x, cy, "extensions", LABEL, Weight::Medium, theme::TEXT_DIM.with_alpha(a));
        let status = match &data.status {
            Status::Loading => "loading lists".to_owned(),
            Status::Ready { filters, .. } => format!("{}k rules", filters / 1000),
            Status::Failed(_) => "lists offline".to_owned(),
        };
        let status_color = if matches!(data.status, Status::Failed(_)) { theme::ACCENT } else { theme::TEXT_FAINT };
        scene.text_right(inner_x + inner_w, cy, &status, SMALL, Weight::Regular, status_color.with_alpha(a));
        cy += 20.0;

        // Shield card.
        let shield_card = Rect::new(inner_x, cy, inner_w, 172.0);
        scene.fill(shield_card, 14.0, theme::SURFACE.with_alpha(a));
        scene.stroke(shield_card, 14.0, 1.0, theme::HAIRLINE.with_alpha(a * 0.8));
        let sx = shield_card.x + 14.0;
        let mut sy = shield_card.y + 22.0;
        let w = scene.text(sx, sy, "shield", LABEL, Weight::Medium, theme::TEXT.with_alpha(a));
        let active = !data.paused && !data.excepted;
        scene.circle(sx + w + 8.0, sy, 3.0, if active { theme::TEXT } else { theme::ACCENT }.with_alpha(a));
        sy += 38.0;

        let count = data.blocked.to_string();
        let count_w = scene.text(sx, sy, &count, 34.0, Weight::Light, theme::TEXT.with_alpha(a));
        let label_w = inner_w - 28.0 - count_w - 12.0;
        let label = scene.text.fit(&format!("blocked on {}", data.site), SMALL, Weight::Regular, label_w);
        scene.text(sx + count_w + 12.0, sy + 6.0, &label, SMALL, Weight::Regular, theme::TEXT_DIM.with_alpha(a));
        sy += 36.0;

        let row_w = inner_w - 28.0;
        self.site_toggle = self.toggle_row(scene, sx, sy, row_w, "protect this site", self.site_knob.value, false, a);
        sy += 32.0;
        self.pause_toggle = self.toggle_row(scene, sx, sy, row_w, "pause everywhere", self.pause_knob.value, true, a);
        cy = shield_card.bottom() + 12.0;

        // Update button.
        let update = Rect::new(inner_x, cy, inner_w, 30.0);
        let hover = self.hover == Some(PanelHit::Update);
        scene.stroke(update, 15.0, 1.0, if hover { theme::HAIRLINE_HOVER } else { theme::HAIRLINE }.with_alpha(a));
        let (ux, uy) = update.center();
        scene.text_centered(ux, uy, "update filter lists", SMALL, Weight::Regular, if hover { theme::TEXT } else { theme::TEXT_DIM }.with_alpha(a));
        self.update = update;
        cy = update.bottom() + 24.0;

        // Attempt log.
        let w = scene.text(inner_x, cy, "attempts", LABEL, Weight::Medium, theme::TEXT_DIM.with_alpha(a));
        scene.text(inner_x + w + 8.0, cy, &data.events.len().to_string(), LABEL, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
        cy += 14.0;
        let footer_h = 30.0;
        let log = Rect::new(inner_x, cy, inner_w, (card.bottom() - cy - footer_h).max(0.0));
        self.log = log;
        self.draw_log(scene, log, data, a);

        scene.text(inner_x, card.bottom() - footer_h * 0.5, "web store extensions arrive in p4", SMALL, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
    }

    #[allow(clippy::too_many_arguments)]
    fn toggle_row(&self, scene: &mut Scene, x: f32, y: f32, w: f32, label: &str, knob: f32, danger: bool, a: f32) -> Rect {
        let track = Rect::new(x + w - 40.0, y - 11.0, 40.0, 22.0);
        scene.text(x, y, label, SMALL, Weight::Regular, theme::TEXT_DIM.with_alpha(a));
        let on_color = if danger { theme::ACCENT } else { theme::TEXT };
        scene.fill(track, 11.0, theme::HAIRLINE.lerp(on_color, knob).with_alpha(a));
        let kx = track.x + 11.0 + knob * 18.0;
        scene.circle(kx, track.center().1, 8.0, theme::TEXT_DIM.lerp(theme::ON_LIGHT, knob).with_alpha(a));
        Rect::new(x, y - 15.0, w, 30.0)
    }

    fn draw_log(&mut self, scene: &mut Scene, log: Rect, data: &PanelData, a: f32) {
        self.rows.clear();
        if data.events.is_empty() {
            scene.text(log.x, log.y + 14.0, "nothing attempted yet", SMALL, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
            return;
        }
        let char_w = scene.text.measure("0", SMALL, Weight::Regular);
        let chars = ((log.w - 16.0) / char_w).floor().max(8.0) as usize;
        let heights: Vec<f32> = data
            .events
            .iter()
            .map(|e| if self.is_expanded(e) { ROW_H + detail_lines(e, chars).len() as f32 * LINE + 10.0 } else { ROW_H })
            .collect();
        let total: f32 = heights.iter().sum();
        let max_scroll = (total - log.h).max(0.0);
        if self.scroll.target > max_scroll {
            self.scroll.set(max_scroll);
        }
        let mut y = log.y - self.scroll.value.min(max_scroll + 40.0);

        scene.clipped(log, |scene| {
            for (i, (event, h)) in data.events.iter().zip(&heights).enumerate() {
                if y > log.bottom() {
                    break;
                }
                let row = Rect::new(log.x, y, log.w, *h);
                if row.bottom() >= log.y {
                    self.rows.push((i, Rect::new(log.x, y, log.w, ROW_H)));
                    let hovered = self.hover == Some(PanelHit::Event(i));
                    if hovered || self.is_expanded(event) {
                        scene.fill(row, 10.0, theme::SURFACE.with_alpha(a));
                    }
                    draw_row(scene, event, Rect::new(log.x, y, log.w, ROW_H), a);
                    if self.is_expanded(event) {
                        let mut ly = y + ROW_H + 4.0;
                        for (line, dim) in detail_lines(event, chars) {
                            let color = if dim { theme::TEXT_FAINT } else { theme::TEXT_DIM };
                            scene.text(log.x + 10.0, ly, &line, SMALL, Weight::Regular, color.with_alpha(a));
                            ly += LINE;
                        }
                    }
                }
                y += h;
            }
        });
    }

    fn is_expanded(&self, e: &BlockEvent) -> bool {
        self.expanded.as_ref().is_some_and(|(at, url)| *at == e.at && *url == e.url)
    }
}

fn tag(e: &BlockEvent) -> (&'static str, Color) {
    if e.action == Action::Cleaned {
        return ("url", theme::TEXT_DIM);
    }
    match e.category {
        Category::Ads => ("ad", theme::TEXT),
        Category::Trackers => ("trk", theme::TEXT),
        Category::Malware => ("mal", theme::ACCENT),
        Category::AntiAdblock => ("abp", theme::ACCENT),
        Category::Mixed => ("ads", theme::TEXT),
    }
}

pub(crate) fn draw_row(scene: &mut Scene, e: &BlockEvent, row: Rect, a: f32) {
    let (label, color) = tag(e);
    let cy = row.center().1;
    let pill = Rect::new(row.x + 6.0, cy - 9.0, 38.0, 18.0);
    scene.stroke(pill, 9.0, 1.0, color.with_alpha(a * 0.7));
    scene.text_centered(pill.center().0, cy, label, 10.5, Weight::Medium, color.with_alpha(a));

    let kind = match e.action {
        Action::Blocked => e.kind.label(),
        Action::Neutered => "neutered",
        Action::Cleaned => "cleaned",
    };
    let kind_x = scene.text_right(row.right() - 10.0, cy, kind, SMALL, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
    let host = scene.text.fit(&e.host, SMALL, Weight::Regular, kind_x - pill.right() - 22.0);
    scene.text(pill.right() + 10.0, cy, &host, SMALL, Weight::Regular, theme::TEXT.with_alpha(a));
}

/// Wrapped detail lines: (text, dim label?).
pub(crate) fn detail_lines(e: &BlockEvent, chars: usize) -> Vec<(String, bool)> {
    let chars = chars.max(8);
    let mut out = Vec::new();
    let mut field = |label: &str, value: &str| {
        out.push((label.to_owned(), true));
        let v: Vec<char> = value.chars().collect();
        for chunk in v.chunks(chars).take(4) {
            out.push((chunk.iter().collect(), false));
        }
    };
    field("url", &e.url);
    if !e.rule.is_empty() {
        field("rule", &e.rule);
    }
    let action = match e.action {
        Action::Blocked => "blocked",
        Action::Neutered => "served a harmless stand-in",
        Action::Cleaned => "tracking parameters removed",
    };
    field("action", action);
    field("list", &format!("{}  ({})", e.list, e.category.label()));
    field("time", &clock::hms(e.at));
    out
}
