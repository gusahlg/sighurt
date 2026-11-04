//! The GPU preparation without a GPU: the shader against naga, the vertex layout against the
//! shader, and prepared frames drawn by a reference rasterizer here and compared with the CPU
//! backend.

use sighurt_api::geometry::{Metrics, Scale, WindowRect};
use sighurt_api::ids::LayoutEpoch;

use super::*;
use crate::backend::{Backend, Output, PixelBuffer};
use crate::cpu::CpuBackend;
use crate::text::tests::fonts;
use crate::{Fonts, Pen, TextStyle};

const RECT: WindowRect = WindowRect {
    x: 5,
    y: 9,
    width: 157,
    height: 47,
};

fn metrics(scale: u32) -> Metrics {
    Metrics::new(
        LayoutEpoch::next(),
        RECT,
        Scale::from_fractional(scale).unwrap(),
    )
}

fn module() -> naga::Module {
    naga::front::wgsl::parse_str(SHADER).expect("the WGSL parses")
}

#[test]
fn the_shader_validates() {
    let module = module();
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    );
    validator.validate(&module).expect("the WGSL validates");
    let names: Vec<_> = module
        .entry_points
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, [VERTEX_ENTRY, FRAGMENT_ENTRY]);
}

/// The Rust vertex layout is the shader's vertex input: each location's type matches the
/// attribute's format, and the offsets are `Vertex`'s fields'.
#[test]
fn the_vertex_layout_is_the_shaders() {
    use naga::{Binding, ScalarKind, TypeInner, VectorSize};
    let module = module();
    let entry = module
        .entry_points
        .iter()
        .find(|e| e.name == VERTEX_ENTRY)
        .unwrap();
    let [argument] = entry.function.arguments.as_slice() else {
        panic!("one argument");
    };
    let TypeInner::Struct { members, .. } = &module.types[argument.ty].inner else {
        panic!("a struct of inputs");
    };
    assert_eq!(members.len(), VERTEX_ATTRIBUTES.len());
    for member in members {
        let Some(Binding::Location { location, .. }) = member.binding else {
            panic!("{:?} has a location", member.name);
        };
        let attribute = VERTEX_ATTRIBUTES
            .iter()
            .find(|a| a.location == location)
            .unwrap_or_else(|| panic!("location {location} is declared"));
        let format = match &module.types[member.ty].inner {
            TypeInner::Vector { size, scalar }
                if scalar.kind == ScalarKind::Float && scalar.width == 4 =>
            {
                match size {
                    VectorSize::Bi => AttributeFormat::Float32x2,
                    VectorSize::Quad => AttributeFormat::Float32x4,
                    VectorSize::Tri => panic!("no vec3 attributes"),
                }
            }
            TypeInner::Scalar(scalar) if scalar.kind == ScalarKind::Uint && scalar.width == 4 => {
                AttributeFormat::Uint32
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(format, attribute.format, "{:?}", member.name);
    }
    assert_eq!(VERTEX_STRIDE, 68);
    let offsets: Vec<u64> = VERTEX_ATTRIBUTES.iter().map(|a| a.offset).collect();
    assert_eq!(offsets, [0, 8, 16, 32, 48, 64]);
    assert_eq!(
        uniform(800, 44)[..8],
        bytemuck::cast::<[f32; 2], [u8; 8]>([800.0, 44.0])
    );
}

/// The atlas pages, as the backend's textures would hold them after the uploads.
struct Pages {
    mask: (u32, Vec<u8>),
    color: (u32, Vec<u8>),
}

impl Pages {
    fn new() -> Self {
        Self {
            mask: (0, Vec::new()),
            color: (0, Vec::new()),
        }
    }

    /// What a backend does with a prepared frame's pages: new textures at new sizes, then the
    /// uploads.
    fn apply(&mut self, prepared: &Prepared) {
        for (page, size, bpp) in [
            (&mut self.mask, prepared.mask_size, 1),
            (&mut self.color, prepared.color_size, 4),
        ] {
            if page.0 != size {
                *page = (size, vec![0; (size * size * bpp) as usize]);
            }
        }
        for upload in &prepared.uploads {
            let (size, data, bpp) = match upload.page {
                PageKind::Mask => (self.mask.0, &mut self.mask.1, 1),
                PageKind::Color => (self.color.0, &mut self.color.1, 4),
            };
            assert!(upload.x + upload.width <= size && upload.y + upload.height <= size);
            assert_eq!(
                upload.data.len(),
                (upload.width * upload.height * bpp) as usize
            );
            let row = (upload.width * bpp) as usize;
            for (r, source) in upload.data.chunks_exact(row).enumerate() {
                let start =
                    ((upload.y as usize + r) * size as usize + upload.x as usize) * bpp as usize;
                data[start..start + row].copy_from_slice(source);
            }
        }
    }
}

/// The shader, on the CPU: premultiplied RGBA, `f32`, source-over.
fn draw(prepared: &Prepared, pages: &Pages) -> Vec<[f32; 4]> {
    let (w, h) = prepared.size;
    let mut pixels = vec![prepared.clear; (w * h) as usize];
    for batch in &prepared.batches {
        let [sx, sy, sw, sh] = batch.scissor();
        for quad in
            prepared.indices[batch.indices.start as usize..batch.indices.end as usize].chunks(6)
        {
            let v0 = prepared.vertices[quad[0] as usize];
            let v3 = prepared.vertices[quad[5] as usize];
            let (x0, y0) = (
                v0.position[0].max(sx as f32) as u32,
                v0.position[1].max(sy as f32) as u32,
            );
            let x1 = (v3.position[0] as u32).min(sx + sw);
            let y1 = (v3.position[1] as u32).min(sy + sh);
            for y in y0..y1 {
                for x in x0..x1 {
                    let u = v0.uv[0] + (x as f32 + 0.5 - v0.position[0]);
                    let v = v0.uv[1] + (y as f32 + 0.5 - v0.position[1]);
                    let (tu, tv) = (u.floor() as u32, v.floor() as u32);
                    let source = match v0.kind {
                        SOLID => v0.color,
                        ROUNDED => {
                            let c = rounded(x as f32 + 0.5, y as f32 + 0.5, v0.rect, v0.params[0]);
                            v0.color.map(|v| v * c)
                        }
                        MASK => {
                            assert!(tu < pages.mask.0 && tv < pages.mask.0, "uv inside the page");
                            let c =
                                f32::from(pages.mask.1[(tv * pages.mask.0 + tu) as usize]) / 255.0;
                            v0.color.map(|v| v * c)
                        }
                        COLOR => {
                            assert!(tu < pages.color.0 && tv < pages.color.0);
                            let i = ((tv * pages.color.0 + tu) * 4) as usize;
                            let t = &pages.color.1[i..i + 4];
                            [0, 1, 2, 3].map(|k| f32::from(t[k]) / 255.0)
                        }
                        other => panic!("kind {other}"),
                    };
                    let target = &mut pixels[(y * w + x) as usize];
                    let keep = 1.0 - source[3];
                    *target = [0, 1, 2, 3].map(|k| source[k] + target[k] * keep);
                }
            }
        }
    }
    pixels
}

fn rounded(px: f32, py: f32, rect: [f32; 4], radius: f32) -> f32 {
    let (cx, cy) = ((rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0);
    let (hx, hy) = ((rect[2] - rect[0]) / 2.0, (rect[3] - rect[1]) / 2.0);
    let r = radius.clamp(0.0, hx.min(hy));
    let (qx, qy) = ((px - cx).abs() - (hx - r), (py - cy).abs() - (hy - r));
    let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
    (0.5 - d).clamp(0.0, 1.0)
}

/// The CPU backend's rendering of `scene` as RGBA bytes.
fn cpu(scene: &Scene, metrics: &Metrics, fonts: &FontSystemHandle) -> Vec<[u8; 4]> {
    let size = metrics.buffer();
    let mut data = vec![0u8; size.width as usize * size.height as usize * 4];
    let buffer = PixelBuffer {
        data: &mut data,
        width: size.width,
        height: size.height,
        stride: size.width as usize * 4,
    };
    let mut backend = CpuBackend::new(fonts.clone());
    let _ = backend
        .render(crate::SurfaceKey(1), metrics, scene, Output::Pixels(buffer))
        .unwrap();
    data.as_chunks::<4>()
        .0
        .iter()
        .map(|&[b, g, r, a]| [r, g, b, a])
        .collect()
}

const BG: Rgba = Rgba::rgb(0x0a0a0b);

/// Every primitive: rectangles at fractional edges, rounded and bordered ones, a stroked and a
/// filled icon, text in two sizes, clipped text, translucent colors.
fn fixture(fonts: &mut Fonts, rect: WindowRect) -> Scene {
    let mut pen = Pen::new(rect);
    pen.clear(BG);
    pen.fill([5.0, 9.0, 157.0, 20.0], Rgba::rgb(0x202024));
    pen.fill([7.3, 11.6, 13.25, 5.5], Rgba::rgb(0xff0000));
    pen.fill([21.0, 11.0, 9.0, 9.0], Rgba::rgb_alpha(0x60a5fa80));
    pen.fill_rounded([34.0, 11.0, 30.0, 14.0], 5.0, Rgba::rgb(0x3b3b44));
    pen.bordered(
        [68.0, 10.5, 40.0, 16.0],
        6.0,
        Rgba::rgb(0x111114),
        Rgba::rgb(0x60a5fa),
    );
    pen.stroke(1.5, Rgba::rgb(0xe0e0e0), |path| {
        path.move_to(115.0, 18.0);
        path.line_to(121.0, 12.0);
        path.move_to(115.0, 18.0);
        path.line_to(121.0, 24.0);
    });
    pen.shape(Rgba::rgb(0x50fa7b), |path| {
        path.push_circle(130.0, 18.0, 4.0)
    });
    let line = fonts.line("Blåbär — Sighurt 0.10", TextStyle::SM);
    pen.text(
        &line,
        (8.0, 30.0),
        Rgba::rgb(0xf0f0f0),
        [5.0, 30.0, 157.0, 20.0],
    );
    let small = fonts.line("status: example.com/ü", TextStyle::XS);
    pen.text(
        &small,
        (100.0, 38.0),
        Rgba::rgb_alpha(0xa0a0a0c0),
        [100.0, 38.0, 40.0, 16.0],
    );
    pen.clip([10.0, 40.0, 30.0, 10.0], |pen| {
        pen.fill([5.0, 35.0, 60.0, 20.0], Rgba::rgb(0x803030));
    });
    pen.finish()
}

/// Whether pixel `(x, y)` is near a rounded corner of the fixture, where the analytic corners
/// may differ from tiny-skia's cubic ones.
fn near_corner(scene: &Scene, mapping: &Mapping, x: u32, y: u32) -> bool {
    scene.items.iter().any(|item| {
        let (rect, radius) = match item.kind {
            Kind::RoundedRect { rect, radius, .. } | Kind::Bordered { rect, radius, .. } => {
                (rect, radius)
            }
            _ => return false,
        };
        let Some(r) = mapping.snap(rect) else {
            return false;
        };
        let reach = radius * mapping.scale() + 2.0;
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let near_x = px < r.left + reach || px > r.right - reach;
        let near_y = py < r.top + reach || py > r.bottom - reach;
        near_x
            && near_y
            && px > r.left - 1.0
            && px < r.right + 1.0
            && py > r.top - 1.0
            && py < r.bottom + 1.0
    })
}

/// A prepared frame, drawn as the shader draws it, is the CPU backend's frame: the same
/// pixels within one level (float against 8-bit blending) everywhere but at rounded corners,
/// where the analytic coverage may differ from tiny-skia's by a bounded amount; at scales 1,
/// 1.25, 1.5, 1.75 and 2.
#[test]
fn prepared_frames_draw_what_the_cpu_backend_draws() {
    let mut fonts = fonts();
    let handle = fonts.handle();
    let mut atlas = Atlas::new();
    let mut pages = Pages::new();
    for scale in [120, 150, 180, 210, 240] {
        let metrics = metrics(scale);
        let scene = fixture(&mut fonts, metrics.rect());
        let mapping = Mapping::new(&metrics);
        // Twice: the second frame finds its glyphs placed and uploads only its icons.
        for frame in 0..2 {
            let prepared = prepare(&scene, &metrics, &mut atlas, &handle).unwrap();
            if frame == 1 {
                assert!(
                    prepared
                        .uploads
                        .iter()
                        .all(|u| u.page == PageKind::Mask && u.y >= prepared.mask_size * 3 / 4),
                    "{:?}",
                    prepared
                        .uploads
                        .iter()
                        .map(|u| (u.x, u.y))
                        .collect::<Vec<_>>()
                );
            }
            pages.apply(&prepared);
            let ours = draw(&prepared, &pages);
            let theirs = cpu(&scene, &metrics, &handle);
            let (w, _) = prepared.size;
            let mut corner_max = 0u8;
            for (i, (ours, theirs)) in ours.iter().zip(&theirs).enumerate() {
                let ours = ours.map(|v| (v * 255.0).round() as u8);
                let diff = (0..4).map(|k| ours[k].abs_diff(theirs[k])).max().unwrap();
                let (x, y) = (i as u32 % w, i as u32 / w);
                if near_corner(&scene, &mapping, x, y) {
                    corner_max = corner_max.max(diff);
                    assert!(
                        diff <= 16,
                        "corner ({x}, {y}) at {scale}: {ours:?} vs {theirs:?}"
                    );
                } else {
                    assert!(diff <= 2, "({x}, {y}) at {scale}: {ours:?} vs {theirs:?}");
                }
            }
            let _ = corner_max;
        }
    }
}

/// Quads: one per rectangle, two per bordered one, one per icon and per glyph with an image;
/// batches follow the clips; every texture coordinate is inside its page.
#[test]
fn quads_batches_and_uvs() {
    let mut fonts = fonts();
    let handle = fonts.handle();
    let metrics = metrics(150);
    let scene = fixture(&mut fonts, metrics.rect());
    let mut atlas = Atlas::new();
    let prepared = prepare(&scene, &metrics, &mut atlas, &handle).unwrap();
    let glyphs = prepared.vertices.iter().filter(|v| v.kind == MASK).count() / 4 - 2;
    let rects = prepared.vertices.iter().filter(|v| v.kind == SOLID).count() / 4;
    let rounded = prepared
        .vertices
        .iter()
        .filter(|v| v.kind == ROUNDED)
        .count()
        / 4;
    assert_eq!((rects, rounded), (4, 3));
    assert!(glyphs > 20, "{glyphs}");
    assert_eq!(prepared.quads() * 4, prepared.vertices.len());
    for v in &prepared.vertices {
        let size = match v.kind {
            MASK => prepared.mask_size,
            COLOR => prepared.color_size,
            _ => continue,
        } as f32;
        assert!((0.0..=size).contains(&v.uv[0]) && (0.0..=size).contains(&v.uv[1]));
        assert_eq!(v.uv.map(f32::fract), [0.0, 0.0], "whole texels");
        assert_eq!(v.position.map(f32::fract), [0.0, 0.0], "whole pixels");
    }
    // Unclipped, the text line, the clipped status text, the clipped rectangle.
    let clips: Vec<[u32; 4]> = prepared.batches.iter().map(Batch::scissor).collect();
    let buffer = metrics.buffer();
    assert_eq!(clips[0], [0, 0, buffer.width, buffer.height]);
    assert_eq!(clips.len(), 4, "{clips:?}");
    let ranges: Vec<_> = prepared.batches.iter().map(|b| b.indices.clone()).collect();
    assert!(ranges.windows(2).all(|w| w[0].end == w[1].start));
    assert_eq!(ranges.last().unwrap().end as usize, prepared.indices.len());
}

/// Empty metrics prepare nothing and touch nothing.
#[test]
fn empty_metrics_prepare_nothing() {
    let fonts = fonts();
    let metrics = Metrics::new(
        LayoutEpoch::next(),
        WindowRect::default(),
        Scale::from_fractional(120).unwrap(),
    );
    let mut atlas = Atlas::new();
    let prepared = prepare(&Scene::default(), &metrics, &mut atlas, &fonts.handle()).unwrap();
    assert!(prepared.vertices.is_empty() && prepared.uploads.is_empty());
}

/// A page that fills grows, then starts over at its largest size; a frame that still doesn't
/// fit is an error, counted, and the next frame that fits renders again.
#[test]
fn the_atlas_is_bounded_and_says_when_it_overflows() {
    let mut fonts = fonts();
    let handle = fonts.handle();
    let metrics = metrics(240);
    let mut atlas = Atlas::with_sizes(64, 128);
    let few = {
        let mut pen = Pen::new(metrics.rect());
        let line = fonts.line("Ab", TextStyle::SM);
        pen.text(
            &line,
            (6.0, 10.0),
            Rgba::rgb(0xffffff),
            [5.0, 9.0, 150.0, 40.0],
        );
        pen.finish()
    };
    prepare(&few, &metrics, &mut atlas, &handle).unwrap();
    assert_eq!(atlas.stats().grows, 0);
    let many = {
        let mut pen = Pen::new(metrics.rect());
        let line = fonts.line("The quick brown fox jumps", TextStyle::HEADING);
        pen.text(
            &line,
            (6.0, 10.0),
            Rgba::rgb(0xffffff),
            [5.0, 9.0, 150.0, 40.0],
        );
        pen.finish()
    };
    let prepared = prepare(&many, &metrics, &mut atlas, &handle);
    match prepared {
        Ok(prepared) => assert_eq!(prepared.mask_size, 128),
        Err(full) => assert_eq!(
            full,
            AtlasFull {
                page: PageKind::Mask,
                size: 128
            }
        ),
    }
    assert!(atlas.stats().grows >= 1);
    let overflows = atlas.stats().overflows;
    let huge = {
        let mut pen = Pen::new(metrics.rect());
        let mut fonts = fonts;
        let line = fonts.line("WMQ@#%&BRGHKN", TextStyle::HEADING.at(32));
        pen.text(
            &line,
            (6.0, 10.0),
            Rgba::rgb(0xffffff),
            [5.0, 9.0, 150.0, 40.0],
        );
        pen.finish()
    };
    let error = prepare(&huge, &metrics, &mut atlas, &handle).unwrap_err();
    assert_eq!(error.page, PageKind::Mask);
    let stats = atlas.stats();
    assert_eq!((stats.mask_size, stats.overflows), (128, overflows + 1));
    assert!(stats.resets >= 1);
    assert!(stats.bytes <= 128 * 128 * 5);
    prepare(&few, &metrics, &mut atlas, &handle).expect("a frame that fits renders again");
}

/// A new scale starts the atlas over.
#[test]
fn a_new_scale_starts_over() {
    let mut fonts = fonts();
    let handle = fonts.handle();
    let mut atlas = Atlas::new();
    for scale in [120, 240] {
        let metrics = metrics(scale);
        let scene = fixture(&mut fonts, metrics.rect());
        prepare(&scene, &metrics, &mut atlas, &handle).unwrap();
    }
    assert_eq!(atlas.stats().resets, 1);
}
