// Every shell primitive is an instanced rounded box, shaded from its signed distance field.

struct Globals {
    viewport: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var glyphs: texture_2d<f32>;
@group(0) @binding(2) var glyph_sampler: sampler;

struct Instance {
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
    // radius, stroke, softness, unused
    @location(2) params: vec4<f32>,
    @location(3) clip: vec4<f32>,
    @location(4) uv: vec4<f32>,
};

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) half_size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) params: vec4<f32>,
    @location(4) clip: vec4<f32>,
    @location(5) uv: vec2<f32>,
};

// Room for the antialiased edge outside the shape.
const AA_PAD: f32 = 1.0;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
    // Glyphs are pixel-aligned bitmaps: no antialiasing margin.
    let pad = select(AA_PAD, 0.0, inst.params.w > 0.5);
    let origin = inst.rect.xy - pad;
    let size = inst.rect.zw + 2.0 * pad;
    let p = origin + corner * size;

    var out: VsOut;
    out.position = vec4<f32>(
        p.x / globals.viewport.x * 2.0 - 1.0,
        1.0 - p.y / globals.viewport.y * 2.0,
        0.0,
        1.0,
    );
    out.half_size = inst.rect.zw * 0.5;
    out.local = p - (inst.rect.xy + out.half_size);
    out.color = inst.color;
    out.params = inst.params;
    out.clip = inst.clip;
    out.uv = mix(inst.uv.xy, inst.uv.zw, corner);
    return out;
}

fn sd_round_box(p: vec2<f32>, half_size: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half_size + r;
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let frag = in.position.xy;
    if frag.x < in.clip.x || frag.y < in.clip.y || frag.x > in.clip.x + in.clip.z || frag.y > in.clip.y + in.clip.w {
        discard;
    }

    if in.params.w > 0.5 {
        return in.color * textureSampleLevel(glyphs, glyph_sampler, in.uv, 0.0).r;
    }

    let radius = min(in.params.x, min(in.half_size.x, in.half_size.y));
    let d = sd_round_box(in.local, in.half_size, radius);
    let stroke = in.params.y;
    let softness = in.params.z;

    var alpha: f32;
    if softness > 0.0 {
        alpha = 1.0 - smoothstep(-softness, 0.0, d);
        alpha = alpha * alpha;
    } else {
        // One physical pixel of coverage-based antialiasing.
        alpha = clamp(0.5 - d, 0.0, 1.0);
        if stroke > 0.0 {
            alpha = alpha * clamp(0.5 + d + stroke, 0.0, 1.0);
        }
    }
    return in.color * alpha;
}

// ── textured layers (web pages) ─────────────────────────────────────────────

@group(1) @binding(0) var page: texture_2d<f32>;
@group(1) @binding(1) var page_sampler: sampler;

@fragment
fn fs_image(in: VsOut) -> @location(0) vec4<f32> {
    let frag = in.position.xy;
    if frag.x < in.clip.x || frag.y < in.clip.y || frag.x > in.clip.x + in.clip.z || frag.y > in.clip.y + in.clip.w {
        discard;
    }
    let radius = min(in.params.x, min(in.half_size.x, in.half_size.y));
    let d = sd_round_box(in.local, in.half_size, radius);
    let coverage = clamp(0.5 - d, 0.0, 1.0);
    let uv = (in.local + in.half_size) / (2.0 * in.half_size);
    let texel = textureSample(page, page_sampler, uv);
    // Pages are opaque; `color` carries the layer opacity in every channel.
    return vec4<f32>(texel.rgb, 1.0) * in.color * coverage;
}
