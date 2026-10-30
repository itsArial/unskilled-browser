use crate::{
    Color, Rect,
    text::{Typesetter, Weight},
};

/// One GPU instance: a rounded box described by a signed distance field, or a glyph.
/// Everything the shell draws — cards, pills, strokes, dots, glows, text — is one of these.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Quad {
    /// x, y, w, h in physical pixels.
    pub rect: [f32; 4],
    /// Premultiplied sRGB.
    pub color: [f32; 4],
    /// Corner radius, stroke width (0 = filled), edge softness (0 = crisp), kind (0 = shape, 1 = glyph).
    pub params: [f32; 4],
    /// Clip rectangle in physical pixels.
    pub clip: [f32; 4],
    /// Glyphs: atlas rectangle (u0, v0, u1, v1), normalized.
    pub uv: [f32; 4],
}

const KIND_GLYPH: f32 = 1.0;

const NO_CLIP: Rect = Rect::new(-1e6, -1e6, 2e6, 2e6);

/// Draw order: runs of quads interleaved with textured layers (web pages).
pub(crate) enum Item {
    Quads(std::ops::Range<u32>),
    Image { quad: Quad, texture: wgpu::Texture },
}

/// A frame's worth of draw calls, recorded in logical pixels and stored in physical pixels.
pub struct Scene {
    pub scale: f32,
    pub text: Typesetter,
    pub(crate) quads: Vec<Quad>,
    pub(crate) items: Vec<Item>,
    run_start: u32,
    clips: Vec<Rect>,
}

impl Scene {
    pub fn new(scale: f32) -> Self {
        Self {
            scale,
            text: Typesetter::new(),
            quads: Vec::with_capacity(1024),
            items: Vec::new(),
            run_start: 0,
            clips: Vec::new(),
        }
    }

    pub fn clear(&mut self, scale: f32) {
        self.scale = scale;
        self.quads.clear();
        self.items.clear();
        self.run_start = 0;
        self.clips.clear();
    }

    /// Closes the current run of quads so the next item draws on top of it.
    fn end_run(&mut self) {
        let end = self.quads.len() as u32;
        if end > self.run_start {
            self.items.push(Item::Quads(self.run_start..end));
        }
        self.run_start = end;
    }

    /// Finishes recording; called by the renderer.
    pub(crate) fn finish(&mut self) {
        self.end_run();
    }

    /// A texture (e.g. a live web page) stretched over `rect`, with rounded corners.
    /// The texture should be `rect` sized in physical pixels for a 1:1 mapping.
    pub fn image(&mut self, rect: Rect, radius: f32, opacity: f32, texture: &wgpu::Texture) {
        if opacity <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        self.end_run();
        let s = self.scale;
        let r = rect.scale(s);
        let c = self.current_clip().scale(s);
        let quad = Quad {
            rect: [r.x, r.y, r.w, r.h],
            color: [opacity; 4],
            params: [radius * s, 0.0, 0.0, 0.0],
            clip: [c.x, c.y, c.w, c.h],
            uv: [0.0; 4],
        };
        self.items.push(Item::Image { quad, texture: texture.clone() });
    }

    /// Everything drawn inside `f` is clipped to `rect` (intersected with any outer clip).
    pub fn clipped(&mut self, rect: Rect, f: impl FnOnce(&mut Self)) {
        let outer = self.current_clip();
        let x0 = rect.x.max(outer.x);
        let y0 = rect.y.max(outer.y);
        let x1 = rect.right().min(outer.right());
        let y1 = rect.bottom().min(outer.bottom());
        self.clips.push(Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)));
        f(self);
        self.clips.pop();
    }

    fn current_clip(&self) -> Rect {
        self.clips.last().copied().unwrap_or(NO_CLIP)
    }

    fn push(&mut self, rect: Rect, color: Color, radius: f32, stroke: f32, softness: f32) {
        if color.a <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let s = self.scale;
        let r = rect.scale(s);
        let c = self.current_clip().scale(s);
        self.quads.push(Quad {
            rect: [r.x, r.y, r.w, r.h],
            color: color.premultiplied(),
            params: [radius * s, stroke * s, softness * s, 0.0],
            clip: [c.x, c.y, c.w, c.h],
            uv: [0.0; 4],
        });
    }

    /// Draws `text` starting at `x` with its capital letters centred on `center_y` (logical
    /// pixels, `px` size). Returns the drawn width.
    pub fn text(&mut self, x: f32, center_y: f32, text: &str, px: f32, weight: Weight, color: Color) -> f32 {
        let width = self.text.measure(text, px, weight);
        if color.a <= 0.0 || text.is_empty() {
            return width;
        }
        let s = self.scale;
        let baseline = (center_y + self.text.cap_center(px, weight)) * s;
        let placed = self.text.layout(text, x * s, baseline, px * s, weight);
        let c = self.current_clip().scale(s);
        let size = self.text.atlas.size as f32;
        let color = color.premultiplied();
        for p in placed {
            let g = p.glyph;
            self.quads.push(Quad {
                rect: [p.x, p.y, g.width as f32, g.height as f32],
                color,
                params: [0.0, 0.0, 0.0, KIND_GLYPH],
                clip: [c.x, c.y, c.w, c.h],
                uv: [
                    g.u as f32 / size,
                    g.v as f32 / size,
                    (g.u + g.width) as f32 / size,
                    (g.v + g.height) as f32 / size,
                ],
            });
        }
        width
    }

    /// The eyepatch logo, `size` logical pixels square, top-left at (`x`, `y`), tinted `color`.
    pub fn logo(&mut self, x: f32, y: f32, size: f32, color: Color) {
        let s = self.scale;
        let px = (size * s).round() as u32;
        let Some(g) = self.text.logo(px) else { return };
        let c = self.current_clip().scale(s);
        let atlas = self.text.atlas.size as f32;
        self.quads.push(Quad {
            rect: [(x * s).round(), (y * s).round(), g.width as f32, g.height as f32],
            color: color.premultiplied(),
            params: [0.0, 0.0, 0.0, KIND_GLYPH],
            clip: [c.x, c.y, c.w, c.h],
            uv: [g.u as f32 / atlas, g.v as f32 / atlas, (g.u + g.width) as f32 / atlas, (g.v + g.height) as f32 / atlas],
        });
    }

    /// `text` right-aligned so it ends at `right`. Returns its left edge.
    pub fn text_right(&mut self, right: f32, center_y: f32, text: &str, px: f32, weight: Weight, color: Color) -> f32 {
        let x = right - self.text.measure(text, px, weight);
        self.text(x, center_y, text, px, weight, color);
        x
    }

    /// `text` centred on `cx`.
    pub fn text_centered(&mut self, cx: f32, center_y: f32, text: &str, px: f32, weight: Weight, color: Color) {
        let x = cx - self.text.measure(text, px, weight) * 0.5;
        self.text(x, center_y, text, px, weight, color);
    }

    pub fn fill(&mut self, rect: Rect, radius: f32, color: Color) {
        self.push(rect, color, radius, 0.0, 0.0);
    }

    /// A stroke drawn inside `rect`. Strokes are kept at least one physical pixel wide.
    pub fn stroke(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
        let width = width.max(1.0 / self.scale);
        self.push(rect, color, radius, width, 0.0);
    }

    pub fn circle(&mut self, cx: f32, cy: f32, radius: f32, color: Color) {
        let rect = Rect::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0);
        self.push(rect, color, radius, 0.0, 0.0);
    }

    /// Soft glow/shadow around a rounded rect; `spread` is the blur radius.
    pub fn glow(&mut self, rect: Rect, radius: f32, spread: f32, color: Color) {
        let r = Rect::new(rect.x - spread, rect.y - spread, rect.w + spread * 2.0, rect.h + spread * 2.0);
        self.push(r, color, radius + spread, 0.0, spread);
    }
}
