//! Text: JetBrains Mono (bundled, SIL OFL — the same face as the terminal and bar) rasterized with
//! swash into a glyph atlas. Scripts it doesn't cover fall back to installed Noto fonts.

use std::collections::HashMap;
use swash::{
    FontRef,
    scale::{Render, ScaleContext, Source},
    zeno::{Format, Vector},
};

static LIGHT: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Light.ttf");
static REGULAR: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
static MEDIUM: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf");
/// The eyepatch logo: 256×256 8-bit coverage.
static LOGO: &[u8] = include_bytes!("../assets/logo.gray");
const LOGO_SIZE: usize = 256;

/// Optional system fallbacks, loaded if present.
const FALLBACKS: &[&str] = &[
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansArabic-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansHebrew-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansDevanagari-Regular.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/local/share/fonts/NotoSansCJK/NotoSansCJK-Regular.ttc",
];

/// Subpixel horizontal positions per pixel.
const SUBPIXEL: f32 = 4.0;
const ATLAS_SIZE: u32 = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Weight {
    Light,
    #[default]
    Regular,
    Medium,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    face: u8,
    id: u16,
    /// Size in quarter physical pixels.
    size: u32,
    subpixel: u8,
}

#[derive(Clone, Copy)]
pub(crate) struct Glyph {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub u: u32,
    pub v: u32,
}

/// One positioned glyph, physical pixels.
pub(crate) struct Placed {
    pub x: f32,
    pub y: f32,
    pub glyph: Glyph,
}

/// Single-channel coverage atlas, packed in shelves. Cleared when full.
pub(crate) struct Atlas {
    pub size: u32,
    pub data: Vec<u8>,
    pub dirty: bool,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
}

impl Atlas {
    fn new() -> Self {
        Self {
            size: ATLAS_SIZE,
            data: vec![0; (ATLAS_SIZE * ATLAS_SIZE) as usize],
            dirty: true,
            shelf_x: 0,
            shelf_y: 0,
            shelf_h: 0,
        }
    }

    fn allocate(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let (w, h) = (w + 1, h + 1);
        if self.shelf_x + w > self.size {
            self.shelf_x = 0;
            self.shelf_y += self.shelf_h;
            self.shelf_h = 0;
        }
        if self.shelf_y + h > self.size || w > self.size {
            return None;
        }
        let at = (self.shelf_x, self.shelf_y);
        self.shelf_x += w;
        self.shelf_h = self.shelf_h.max(h);
        Some(at)
    }

    fn clear(&mut self) {
        self.data.fill(0);
        self.shelf_x = 0;
        self.shelf_y = 0;
        self.shelf_h = 0;
        self.dirty = true;
    }
}

pub struct Typesetter {
    /// Light, Regular, Medium, then fallbacks.
    faces: Vec<FontRef<'static>>,
    context: ScaleContext,
    cache: HashMap<GlyphKey, Option<Glyph>>,
    logos: HashMap<u32, Option<Glyph>>,
    pub(crate) atlas: Atlas,
}

impl Default for Typesetter {
    fn default() -> Self {
        Self::new()
    }
}

impl Typesetter {
    pub fn new() -> Self {
        let mut faces: Vec<FontRef<'static>> = [LIGHT, REGULAR, MEDIUM]
            .iter()
            .map(|data| FontRef::from_index(data, 0).expect("bundled font is valid"))
            .collect();
        for path in FALLBACKS {
            let Ok(bytes) = std::fs::read(path) else { continue };
            // Fallback faces live for the whole process.
            let data: &'static [u8] = Box::leak(bytes.into_boxed_slice());
            if let Some(face) = FontRef::from_index(data, 0) {
                faces.push(face);
            }
        }
        Self { faces, context: ScaleContext::new(), cache: HashMap::new(), logos: HashMap::new(), atlas: Atlas::new() }
    }

    pub(crate) fn atlas_size() -> u32 {
        ATLAS_SIZE
    }

    fn primary(weight: Weight) -> usize {
        match weight {
            Weight::Light => 0,
            Weight::Regular => 1,
            Weight::Medium => 2,
        }
    }

    /// The face that has a glyph for `c`: the requested weight first, then fallbacks.
    fn face_for(&self, c: char, weight: Weight) -> (usize, u16) {
        let primary = Self::primary(weight);
        let id = self.faces[primary].charmap().map(c);
        if id != 0 {
            return (primary, id);
        }
        for (i, face) in self.faces.iter().enumerate().skip(3) {
            let id = face.charmap().map(c);
            if id != 0 {
                return (i, id);
            }
        }
        (primary, 0)
    }

    fn advance(&self, face: usize, id: u16, px: f32) -> f32 {
        self.faces[face].glyph_metrics(&[]).scale(px).advance_width(id)
    }

    /// Width of `text` in pixels at `px` size (logical in, logical out).
    pub fn measure(&self, text: &str, px: f32, weight: Weight) -> f32 {
        text.chars()
            .map(|c| {
                let (face, id) = self.face_for(c, weight);
                self.advance(face, id, px)
            })
            .sum()
    }

    /// Distance from the vertical centre of capital letters to the baseline.
    pub fn cap_center(&self, px: f32, weight: Weight) -> f32 {
        self.faces[Self::primary(weight)].metrics(&[]).scale(px).cap_height * 0.5
    }

    /// The longest prefix of `text` that fits `width`, with `…` when truncated.
    pub fn fit(&self, text: &str, px: f32, weight: Weight, width: f32) -> String {
        if self.measure(text, px, weight) <= width {
            return text.to_owned();
        }
        let ellipsis = self.measure("…", px, weight);
        let mut used = 0.0;
        let mut out = String::new();
        for c in text.chars() {
            let (face, id) = self.face_for(c, weight);
            let w = self.advance(face, id, px);
            if used + w + ellipsis > width {
                break;
            }
            used += w;
            out.push(c);
        }
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
        if !out.is_empty() {
            out.push('…');
        }
        out
    }

    /// The longest suffix of `text` that fits `width` (keeps what's being typed visible).
    pub fn fit_tail(&self, text: &str, px: f32, weight: Weight, width: f32) -> String {
        let mut used = 0.0;
        let mut chars: Vec<char> = Vec::new();
        for c in text.chars().rev() {
            let (face, id) = self.face_for(c, weight);
            let w = self.advance(face, id, px);
            if used + w > width {
                break;
            }
            used += w;
            chars.push(c);
        }
        chars.iter().rev().collect()
    }

    /// Lays out `text` with its baseline at (`x`, `baseline`), all in physical pixels, rasterizing
    /// glyphs into the atlas as needed.
    pub(crate) fn layout(&mut self, text: &str, x: f32, baseline: f32, px: f32, weight: Weight) -> Vec<Placed> {
        let mut out = Vec::with_capacity(text.len());
        let mut pen = x;
        let baseline = baseline.round();
        for c in text.chars() {
            let (face, id) = self.face_for(c, weight);
            let advance = self.advance(face, id, px);
            if !c.is_whitespace() {
                let whole = pen.floor();
                let subpixel = ((pen - whole) * SUBPIXEL).floor() as u8;
                let key = GlyphKey { face: face as u8, id, size: (px * 4.0).round() as u32, subpixel };
                if let Some(glyph) = self.glyph(key) {
                    out.push(Placed {
                        x: whole + glyph.left as f32,
                        y: baseline - glyph.top as f32,
                        glyph,
                    });
                }
            }
            pen += advance;
        }
        out
    }

    /// The logo rendered at `px` × `px` physical pixels (box-filtered down from the source).
    pub(crate) fn logo(&mut self, px: u32) -> Option<Glyph> {
        if let Some(cached) = self.logos.get(&px) {
            return *cached;
        }
        let size = px.clamp(4, LOGO_SIZE as u32);
        let at = match self.atlas.allocate(size, size) {
            Some(at) => Some(at),
            None => {
                self.atlas.clear();
                self.cache.clear();
                self.logos.clear();
                self.atlas.allocate(size, size)
            }
        };
        let glyph = at.map(|(u, v)| {
            let scale = LOGO_SIZE as f32 / size as f32;
            for y in 0..size {
                for x in 0..size {
                    // Average the source pixels this output pixel covers.
                    let (x0, x1) = ((x as f32 * scale) as usize, (((x + 1) as f32 * scale) as usize).min(LOGO_SIZE));
                    let (y0, y1) = ((y as f32 * scale) as usize, (((y + 1) as f32 * scale) as usize).min(LOGO_SIZE));
                    let (mut sum, mut n) = (0u32, 0u32);
                    for sy in y0..y1.max(y0 + 1) {
                        for sx in x0..x1.max(x0 + 1) {
                            sum += LOGO[sy * LOGO_SIZE + sx] as u32;
                            n += 1;
                        }
                    }
                    let coverage = sum as f32 / (n.max(1) as f32 * 255.0);
                    // Small sizes: lift faint coverage so the thin strap doesn't vanish.
                    let coverage = if size < 96 { coverage.sqrt() } else { coverage };
                    self.atlas.data[((v + y) * self.atlas.size + u + x) as usize] = (coverage * 255.0).round() as u8;
                }
            }
            self.atlas.dirty = true;
            Glyph { left: 0, top: 0, width: size, height: size, u, v }
        });
        self.logos.insert(px, glyph);
        glyph
    }

    fn glyph(&mut self, key: GlyphKey) -> Option<Glyph> {
        if let Some(cached) = self.cache.get(&key) {
            return *cached;
        }
        let face = self.faces[key.face as usize];
        let px = key.size as f32 / 4.0;
        let mut scaler = self.context.builder(face).size(px).hint(true).build();
        let image = Render::new(&[Source::Outline])
            .format(Format::Alpha)
            .offset(Vector::new(key.subpixel as f32 / SUBPIXEL, 0.0))
            .render(&mut scaler, key.id);
        let glyph = image.and_then(|image| {
            let p = image.placement;
            if p.width == 0 || p.height == 0 {
                return None;
            }
            let (u, v) = match self.atlas.allocate(p.width, p.height) {
                Some(at) => at,
                None => {
                    // Atlas full: start over; glyphs get re-rasterized on demand.
                    self.atlas.clear();
                    self.cache.clear();
                    self.logos.clear();
                    self.atlas.allocate(p.width, p.height)?
                }
            };
            for row in 0..p.height {
                let src = (row * p.width) as usize;
                let dst = ((v + row) * self.atlas.size + u) as usize;
                self.atlas.data[dst..dst + p.width as usize].copy_from_slice(&image.data[src..src + p.width as usize]);
            }
            self.atlas.dirty = true;
            Some(Glyph { left: p.left, top: p.top, width: p.width, height: p.height, u, v })
        });
        self.cache.insert(key, glyph);
        glyph
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monospace_measure() {
        let t = Typesetter::new();
        let one = t.measure("a", 14.0, Weight::Regular);
        assert!(one > 5.0 && one < 12.0);
        assert!((t.measure("abcd", 14.0, Weight::Regular) - 4.0 * one).abs() < 0.01);
    }

    #[test]
    fn fit_truncates_with_ellipsis() {
        let t = Typesetter::new();
        let fitted = t.fit("github.com/unskilled/browser", 14.0, Weight::Regular, 80.0);
        assert!(fitted.ends_with('…'));
        assert!(t.measure(&fitted, 14.0, Weight::Regular) <= 80.0);
        assert_eq!(t.fit("short", 14.0, Weight::Regular, 500.0), "short");
    }

    #[test]
    fn rasterizes_into_atlas() {
        let mut t = Typesetter::new();
        let placed = t.layout("Hi", 10.0, 20.0, 16.0, Weight::Regular);
        assert_eq!(placed.len(), 2);
        assert!(placed[0].glyph.height > 5);
        // Cached: a second layout adds nothing new.
        let shelf = t.atlas.shelf_x;
        t.layout("Hi", 10.0, 20.0, 16.0, Weight::Regular);
        assert_eq!(t.atlas.shelf_x, shelf);
    }
}
