// The chrome's one program, shared by every GPU backend (sighurt_render::gpu documents the
// layout). Coordinates are buffer pixels, origin top-left; the viewport uniform is the
// configured size. Output is premultiplied, blended source-over by the pipeline
// (One, OneMinusSrcAlpha) in encoded space: the target is Bgra8Unorm, not sRGB.

struct Viewport {
    size: vec2<f32>,
    unused: vec2<f32>,
}

@group(0) @binding(0) var<uniform> viewport: Viewport;
// Glyph and icon coverage, one byte a texel.
@group(0) @binding(1) var mask_page: texture_2d<f32>;
// Color glyphs, premultiplied RGBA.
@group(0) @binding(2) var color_page: texture_2d<f32>;

const SOLID: u32 = 0u;
const ROUNDED: u32 = 1u;
const MASK: u32 = 2u;
const COLOR: u32 = 3u;

struct VertexInput {
    // Buffer pixels.
    @location(0) position: vec2<f32>,
    // Texels of the atlas page (MASK, COLOR), at the same scale as `position`.
    @location(1) uv: vec2<f32>,
    // Premultiplied.
    @location(2) color: vec4<f32>,
    // ROUNDED: the rectangle's left, top, right, bottom in buffer pixels.
    @location(3) rect: vec4<f32>,
    // ROUNDED: x is the corners' radius in buffer pixels.
    @location(4) params: vec4<f32>,
    @location(5) kind: u32,
}

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) rect: vec4<f32>,
    @location(3) @interpolate(flat) params: vec4<f32>,
    @location(4) @interpolate(flat) kind: u32,
}

@vertex
fn vs_main(in: VertexInput) -> Varyings {
    var out: Varyings;
    let ndc = vec2<f32>(
        in.position.x / viewport.size.x * 2.0 - 1.0,
        1.0 - in.position.y / viewport.size.y * 2.0,
    );
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.rect = in.rect;
    out.params = in.params;
    out.kind = in.kind;
    return out;
}

// How much of the pixel centred at `p` a rectangle with rounded corners covers: its signed
// distance, antialiased over one pixel.
fn rounded_coverage(p: vec2<f32>, rect: vec4<f32>, radius: f32) -> f32 {
    let center = (rect.xy + rect.zw) * 0.5;
    let half = (rect.zw - rect.xy) * 0.5;
    let r = clamp(radius, 0.0, min(half.x, half.y));
    let q = abs(p - center) - (half - vec2<f32>(r, r));
    let d = length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
    return clamp(0.5 - d, 0.0, 1.0);
}

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4<f32> {
    // Tiles sit on whole texels and quads on whole pixels, one texel a pixel: the texel
    // under this pixel's centre, no filtering.
    let texel = vec2<i32>(floor(in.uv));
    switch in.kind {
        case ROUNDED: {
            return in.color * rounded_coverage(in.position.xy, in.rect, in.params.x);
        }
        case MASK: {
            return in.color * textureLoad(mask_page, texel, 0).r;
        }
        case COLOR: {
            return textureLoad(color_page, texel, 0);
        }
        case SOLID, default: {
            return in.color;
        }
    }
}
