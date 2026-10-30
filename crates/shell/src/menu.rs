//! Right-click menu: Chromium's menu model (back, copy, paste, …) plus our own commands, drawn
//! by us — windowless Chromium has no native window to draw its own.

use crate::theme;
use ui::{Color, Rect, Scene, Spring, SpringConfig, Weight};

const ROW_H: f32 = 30.0;
const SEP_H: f32 = 9.0;
const TEXT: f32 = 12.5;
const PAD_X: f32 = 14.0;

/// One line of a menu.
pub struct Row<'a> {
    pub label: &'a str,
    /// Shortcut shown dimmed on the right.
    pub hint: &'a str,
    pub enabled: bool,
    pub separator: bool,
}

impl<'a> Row<'a> {
    pub fn item(label: &'a str, hint: &'a str) -> Self {
        Self { label, hint, enabled: true, separator: false }
    }

    pub fn separator() -> Self {
        Self { label: "", hint: "", enabled: false, separator: true }
    }
}

/// Chromium's context menu as rows.
pub fn rows(menu: &engine::ContextMenu) -> Vec<Row<'_>> {
    menu.items.iter().map(|i| Row { label: &i.label, hint: "", enabled: i.enabled, separator: i.separator }).collect()
}

pub enum MenuHit {
    Item(usize),
    Inside,
}

pub struct MenuView {
    open: Spring,
    card: Rect,
    rows: Vec<(usize, Rect)>,
    hover: Option<usize>,
}

impl MenuView {
    pub fn new() -> Self {
        Self { open: Spring::new(0.0, SpringConfig::SNAP), card: Rect::default(), rows: Vec::new(), hover: None }
    }

    /// A new menu appeared: animate it in.
    pub fn reset(&mut self) {
        self.open.snap_to(0.0);
        self.open.set(1.0);
        self.hover = None;
    }

    pub fn step(&mut self, dt: f32) -> bool {
        self.open.step(dt)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<MenuHit> {
        if !self.card.contains(x, y) {
            return None;
        }
        self.rows.iter().find(|(_, r)| r.contains(x, y)).map_or(Some(MenuHit::Inside), |&(i, _)| Some(MenuHit::Item(i)))
    }

    pub fn set_hover(&mut self, x: f32, y: f32) {
        self.hover = match self.hit(x, y) {
            Some(MenuHit::Item(i)) => Some(i),
            _ => None,
        };
    }

    /// `at` is the click position in window coordinates.
    pub fn draw(&mut self, scene: &mut Scene, items: &[Row], at: (f32, f32), bounds: (f32, f32)) {
        let a = self.open.value.clamp(0.0, 1.0);
        let width = items
            .iter()
            .map(|i| {
                let hint = if i.hint.is_empty() { 0.0 } else { scene.text.measure(i.hint, TEXT - 1.5, Weight::Regular) + 28.0 };
                scene.text.measure(i.label, TEXT, Weight::Regular) + hint
            })
            .fold(160.0f32, f32::max)
            + 2.0 * PAD_X;
        let height: f32 = items.iter().map(|i| if i.separator { SEP_H } else { ROW_H }).sum::<f32>() + 12.0;
        // Keep the menu on screen, flipping left/up near the edges.
        let x = if at.0 + width > bounds.0 - 8.0 { (at.0 - width).max(8.0) } else { at.0 };
        let y = if at.1 + height > bounds.1 - 8.0 { (at.1 - height).max(8.0) } else { at.1 };
        let card = Rect::new(x, y + (1.0 - a) * -4.0, width, height);
        self.card = card;

        scene.glow(card, 12.0, 18.0, Color::rgba(0, 0, 0, 180).with_alpha(a));
        scene.fill(card, 12.0, theme::RAISED.with_alpha(a));
        scene.stroke(card, 12.0, 1.0, theme::HAIRLINE.with_alpha(a));

        self.rows.clear();
        let mut cy = card.y + 6.0;
        for (i, item) in items.iter().enumerate() {
            if item.separator {
                scene.fill(Rect::new(card.x + 10.0, cy + SEP_H * 0.5, card.w - 20.0, 1.0), 0.0, theme::HAIRLINE.with_alpha(a));
                cy += SEP_H;
                continue;
            }
            let row = Rect::new(card.x + 4.0, cy, card.w - 8.0, ROW_H);
            if item.enabled {
                self.rows.push((i, row));
            }
            if self.hover == Some(i) && item.enabled {
                scene.fill(row, 8.0, theme::TEXT.with_alpha(a));
            }
            let color = match (item.enabled, self.hover == Some(i)) {
                (false, _) => theme::TEXT_FAINT,
                (true, true) => theme::ON_LIGHT,
                (true, false) => theme::TEXT,
            };
            scene.text(row.x + PAD_X - 4.0, row.center().1, item.label, TEXT, Weight::Regular, color.with_alpha(a));
            if !item.hint.is_empty() {
                let hint = if self.hover == Some(i) && item.enabled { theme::ON_LIGHT } else { theme::TEXT_FAINT };
                scene.text_right(row.right() - PAD_X + 4.0, row.center().1, item.hint, TEXT - 1.5, Weight::Regular, hint.with_alpha(a));
            }
            cy += ROW_H;
        }
    }
}
