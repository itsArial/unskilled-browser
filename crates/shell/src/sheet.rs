//! Prompts the browser asks in its own chrome — sign-in sharing, saving a password, picking a
//! login, the PIN before a password is used. A card that drops from under the address bar.

use crate::theme;
use ui::{Color, Rect, Scene, Spring, SpringConfig, Weight};

const WIDTH: f32 = 430.0;
const PAD: f32 = 20.0;
const LINE: f32 = 18.0;
const BUTTON_H: f32 = 32.0;
const ROW_H: f32 = 36.0;
const DOT_GAP: f32 = 16.0;

/// What a sheet shows. The last button is the primary one.
pub struct Spec {
    pub title: String,
    pub body: String,
    /// Choices listed above the buttons (logins to pick from).
    pub rows: Vec<(String, String)>,
    pub input: Option<Input>,
    pub buttons: Vec<&'static str>,
}

pub struct Input {
    pub hint: &'static str,
    pub len: usize,
    pub busy: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SheetHit {
    Button(usize),
    Row(usize),
    Inside,
}

pub struct SheetView {
    open: Spring,
    shake: Spring,
    key: u64,
    card: Rect,
    buttons: Vec<Rect>,
    rows: Vec<Rect>,
    hover: Option<SheetHit>,
    spin: f32,
}

impl SheetView {
    pub fn new() -> Self {
        Self {
            open: Spring::new(0.0, SpringConfig::SNAP),
            shake: Spring::new(0.0, SpringConfig::from_response(0.35, 0.22)),
            key: 0,
            card: Rect::default(),
            buttons: Vec::new(),
            rows: Vec::new(),
            hover: None,
            spin: 0.0,
        }
    }

    /// Shows the sheet identified by `key` (a different key animates in afresh); `None` hides it.
    pub fn show(&mut self, key: Option<u64>) {
        match key {
            Some(key) if key != self.key || self.open.value < 0.01 && self.open.target < 0.5 => {
                self.key = key;
                self.open.snap_to(0.0);
                self.open.set(1.0);
                self.hover = None;
            }
            Some(_) => {}
            None => {
                self.open.set(0.0);
                self.card = Rect::default();
            }
        }
    }

    pub fn shake(&mut self) {
        self.shake.velocity = 320.0;
    }

    pub fn step(&mut self, dt: f32, busy: bool) -> bool {
        let mut moving = self.open.step(dt) | self.shake.step(dt);
        if busy {
            self.spin = (self.spin + dt * 1.4).fract();
            moving = true;
        }
        moving
    }

    pub fn visible(&self) -> bool {
        self.open.value > 0.01
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<SheetHit> {
        if !self.card.contains(x, y) {
            return None;
        }
        if let Some(i) = self.buttons.iter().position(|r| r.contains(x, y)) {
            return Some(SheetHit::Button(i));
        }
        if let Some(i) = self.rows.iter().position(|r| r.contains(x, y)) {
            return Some(SheetHit::Row(i));
        }
        Some(SheetHit::Inside)
    }

    pub fn set_hover(&mut self, at: Option<(f32, f32)>) {
        self.hover = at.and_then(|(x, y)| self.hit(x, y));
    }

    /// `anchor_x` centres the card (under the address bar), `top` is where it hangs from.
    pub fn draw(&mut self, scene: &mut Scene, spec: &Spec, anchor_x: f32, top: f32, bounds: f32) {
        let a = self.open.value.clamp(0.0, 1.0);
        let text_w = WIDTH - 2.0 * PAD;
        let body = wrap(scene, &spec.body, 12.5, text_w);
        let mut height = PAD + 22.0 + body.len() as f32 * LINE + 6.0;
        height += spec.rows.len() as f32 * ROW_H + if spec.rows.is_empty() { 0.0 } else { 10.0 };
        height += if spec.input.is_some() { 64.0 } else { 0.0 };
        height += 12.0 + BUTTON_H + PAD;

        let x = (anchor_x - WIDTH * 0.5).clamp(12.0, (bounds - WIDTH - 12.0).max(12.0)) + self.shake.value;
        let card = Rect::new(x, top + (1.0 - a) * -10.0, WIDTH, height);
        self.card = card;
        scene.glow(card, 16.0, 24.0, Color::rgba(0, 0, 0, 200).with_alpha(a));
        scene.fill(card, 16.0, theme::RAISED.with_alpha(a));
        scene.stroke(card, 16.0, 1.0, theme::HAIRLINE_HOVER.with_alpha(a * 0.8));

        let mut y = card.y + PAD + 8.0;
        let title = scene.text.fit(&spec.title, 14.0, Weight::Medium, text_w);
        scene.text(card.x + PAD, y, &title, 14.0, Weight::Medium, theme::TEXT.with_alpha(a));
        y += 24.0;
        for line in &body {
            scene.text(card.x + PAD, y, line, 12.5, Weight::Regular, theme::TEXT_DIM.with_alpha(a));
            y += LINE;
        }
        y += 6.0;

        self.rows.clear();
        for (i, (label, detail)) in spec.rows.iter().enumerate() {
            let row = Rect::new(card.x + PAD - 8.0, y - ROW_H * 0.5 + 4.0, text_w + 16.0, ROW_H - 4.0);
            let hovered = self.hover == Some(SheetHit::Row(i));
            scene.fill(row, 10.0, if hovered { theme::TEXT.with_alpha(a) } else { theme::SURFACE.with_alpha(a) });
            scene.stroke(row, 10.0, 1.0, theme::HAIRLINE.with_alpha(a));
            let (fg, dim) = if hovered { (theme::ON_LIGHT, theme::ON_LIGHT) } else { (theme::TEXT, theme::TEXT_DIM) };
            let cy = row.center().1;
            let label = scene.text.fit(label, 12.5, Weight::Regular, text_w * 0.6);
            scene.text(row.x + 12.0, cy, &label, 12.5, Weight::Regular, fg.with_alpha(a));
            let detail = scene.text.fit(detail, 11.0, Weight::Regular, text_w * 0.35);
            scene.text_right(row.right() - 12.0, cy, &detail, 11.0, Weight::Regular, dim.with_alpha(a * 0.8));
            self.rows.push(row);
            y += ROW_H;
        }
        if !spec.rows.is_empty() {
            y += 10.0;
        }

        if let Some(input) = &spec.input {
            let pill = Rect::new(card.x + PAD, y, text_w, 40.0);
            let border = if input.error.is_some() { theme::ACCENT } else { theme::HAIRLINE_HOVER };
            scene.fill(pill, 20.0, theme::SURFACE.with_alpha(a));
            scene.stroke(pill, 20.0, 1.0, border.with_alpha(a));
            let (px, py) = pill.center();
            let shown = input.len.min(16);
            if shown == 0 {
                scene.text_centered(px, py, input.hint, 12.5, Weight::Regular, theme::TEXT_FAINT.with_alpha(a));
            } else {
                let start = px - (shown as f32 - 1.0) * DOT_GAP * 0.5;
                for i in 0..shown {
                    let pulse = if input.busy {
                        0.35 + 0.65 * (1.0 - (i as f32 / shown as f32 - self.spin).rem_euclid(1.0))
                    } else {
                        1.0
                    };
                    scene.circle(start + i as f32 * DOT_GAP, py, 4.0, theme::TEXT.with_alpha(a * pulse));
                }
            }
            if let Some(error) = &input.error {
                scene.text(card.x + PAD, pill.bottom() + 12.0, error, 11.0, Weight::Regular, theme::ACCENT.with_alpha(a));
            }
            y += 64.0;
        }

        // Buttons, right-aligned; the last is the filled primary.
        self.buttons.clear();
        let mut right = card.right() - PAD;
        let by = y + 12.0 - 8.0;
        let mut rects: Vec<Rect> = Vec::new();
        for label in spec.buttons.iter().rev() {
            let w = scene.text.measure(label, 12.5, Weight::Regular) + 28.0;
            rects.push(Rect::new(right - w, by, w, BUTTON_H));
            right -= w + 8.0;
        }
        rects.reverse();
        let last = spec.buttons.len().saturating_sub(1);
        for (i, (rect, label)) in rects.iter().zip(&spec.buttons).enumerate() {
            let hovered = self.hover == Some(SheetHit::Button(i));
            let primary = i == last;
            let (fill, fg) = match (primary, hovered) {
                (true, false) => (theme::TEXT, theme::ON_LIGHT),
                (true, true) => (Color::rgb(255, 255, 255), theme::ON_LIGHT),
                (false, true) => (theme::HAIRLINE, theme::TEXT),
                (false, false) => (theme::RAISED, theme::TEXT_DIM),
            };
            scene.fill(*rect, BUTTON_H * 0.5, fill.with_alpha(a));
            if !primary {
                scene.stroke(*rect, BUTTON_H * 0.5, 1.0, theme::HAIRLINE_HOVER.with_alpha(a));
            }
            let (cx, cy) = rect.center();
            scene.text_centered(cx, cy, label, 12.5, Weight::Regular, fg.with_alpha(a));
        }
        self.buttons = rects;
    }
}

/// Greedy word wrap to `width`.
fn wrap(scene: &Scene, text: &str, px: f32, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
        if scene.text.measure(&candidate, px, Weight::Regular) > width && !line.is_empty() {
            lines.push(std::mem::replace(&mut line, word.to_owned()));
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}
