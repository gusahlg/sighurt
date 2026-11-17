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

/// The shader, on the CPU: premultiplied RGBA, `f32`, source-over, into an 8-bit target.
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
                    // The target is 8 bits a channel (Bgra8Unorm): each draw is stored rounded.
                    *target = [0, 1, 2, 3]
                        .map(|k| ((source[k] + target[k] * keep) * 255.0).round() / 255.0);
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
                        diff <= 32,
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
    // A frame that fits renders again, its glyphs uploaded anew: nothing the failed frame
    // placed is taken for uploaded.
    let mut pages = Pages::new();
    let prepared = prepare(&few, &metrics, &mut atlas, &handle).expect("it fits");
    assert!(prepared
        .uploads
        .iter()
        .any(|u| u.data.iter().any(|&t| t != 0)));
    pages.apply(&prepared);
    let ours = draw(&prepared, &pages);
    let theirs = cpu(&few, &metrics, &handle);
    for (ours, theirs) in ours.iter().zip(&theirs) {
        let ours = ours.map(|v| (v * 255.0).round() as u8);
        assert!(
            (0..4).all(|k| ours[k].abs_diff(theirs[k]) <= 2),
            "{ours:?} {theirs:?}"
        );
    }
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

#[path = "../../../../tools/fuzz/mod.rs"]
mod fuzz;
use crate as render;
#[path = "../../tests/support/scenes.rs"]
mod scenes;

/// Compares what the reference rasterizer draws of `prepared` with the CPU backend's frame:
/// `tolerance` levels off the corners, 64 at them (the GPU comparison's
/// RANDOM_CORNER_TOLERANCE, which says why), 16 on a path's ink (its PATH_TOLERANCE). Returns the corner
/// pixels beyond 2 and the pixels compared.
fn compare_prepared(
    name: &str,
    scene: &Scene,
    metrics: &Metrics,
    prepared: &Prepared,
    pages: &Pages,
    fonts: &FontSystemHandle,
    tolerance: u8,
) -> (usize, usize) {
    let ours = draw(prepared, pages);
    let theirs = cpu(scene, metrics, fonts);
    assert_eq!(ours.len(), theirs.len(), "{name}");
    let mapping = Mapping::new(metrics);
    let w = prepared.size.0;
    let mut corners = 0;
    for (i, (ours, theirs)) in ours.iter().zip(&theirs).enumerate() {
        let ours = ours.map(|v| (v * 255.0).round() as u8);
        let diff = (0..4).map(|k| ours[k].abs_diff(theirs[k])).max().unwrap();
        let (x, y) = (i as u32 % w, i as u32 / w);
        if diff <= tolerance {
            continue;
        }
        // One of tiny-skia's 16 samples a pixel on a path's ink: the GPU comparison's
        // PATH_TOLERANCE (tests/support/compare.rs) says why.
        if diff <= 16 && in_path_ink(scene, &mapping, x, y) {
            continue;
        }
        assert!(
            near_corner(scene, &mapping, x, y) && diff <= 64,
            "{name}: ({x}, {y}) of {w}: {ours:?} vs {theirs:?}"
        );
        corners += 1;
    }
    (corners, ours.len())
}

/// Whether `(x, y)` is in the ink bounds of one of `scene`'s paths.
fn in_path_ink(scene: &Scene, mapping: &Mapping, x: u32, y: u32) -> bool {
    scene.items.iter().any(|item| {
        let Kind::Path { path, stroke, .. } = item.kind else {
            return false;
        };
        crate::shape::ink_bounds(scene.path(path), stroke, mapping).is_some_and(|b| {
            let (x, y) = (i64::from(x), i64::from(y));
            (i64::from(b.x)..i64::from(b.x) + i64::from(b.width)).contains(&x)
                && (i64::from(b.y)..i64::from(b.y) + i64::from(b.height)).contains(&y)
        })
    })
}

/// Seeded random scenes (`SIG_FUZZ_ITERATIONS`, 15 % of 2000 by default: 300) prepared with
/// one atlas in sequence, as a backend does (it grows, starts over at scale changes, and the
/// icon ring alternates), drawn by the reference rasterizer and compared with the CPU
/// backend. `SIG_FUZZ_SEED` and `SIG_FUZZ_CASE` replay.
#[test]
fn random_scenes_prepare_to_what_the_cpu_backend_draws() {
    let mut shaping = fonts();
    let handle = shaping.handle();
    let mut atlas = Atlas::new();
    let mut pages = Pages::new();
    let (mut scenes, mut pixels, mut corners) = (0, 0, 0);
    fuzz::run(
        "gpu preparation random scenes",
        fuzz::iterations(15),
        std::time::Duration::from_secs(120),
        |case| {
            let (metrics, scene) = scenes::random_scene(&mut case.rng, &mut shaping);
            case.note(|| {
                format!(
                    "{:?} at {} with {} items",
                    metrics.rect(),
                    metrics.scale().numerator(),
                    scene.items.len()
                )
            });
            let prepared = prepare(&scene, &metrics, &mut atlas, &handle)
                .unwrap_or_else(|full| panic!("{full}"));
            pages.apply(&prepared);
            let name = format!("random case {}", case.index);
            // Dozens of translucent layers: the random comparisons' RANDOM_TOLERANCE.
            let (c, p) = compare_prepared(&name, &scene, &metrics, &prepared, &pages, &handle, 3);
            scenes += 1;
            pixels += p;
            corners += c;
        },
    );
    println!(
        "preparation: {scenes} scenes, {pixels} pixels compared, {corners} corner pixels beyond 2"
    );
}

/// Every item kind alone under each clip case at four scales, prepared with one atlas.
#[test]
fn every_item_kind_under_every_clip_prepares_to_what_the_cpu_backend_draws() {
    let mut shaping = fonts();
    let handle = shaping.handle();
    let mut atlas = Atlas::new();
    let mut pages = Pages::new();
    let mut corners = 0;
    let all = scenes::kind_scenes(&mut shaping);
    for (name, metrics, scene) in &all {
        let prepared = prepare(scene, metrics, &mut atlas, &handle).unwrap();
        pages.apply(&prepared);
        corners += compare_prepared(name, scene, metrics, &prepared, &pages, &handle, 2).0;
    }
    println!(
        "preparation: {} fixed scenes, {corners} corner pixels beyond 2",
        all.len()
    );
}

/// Seeded random scenes with the paths and the long corner arcs `random_scene` leaves out:
/// they differ from the CPU backend's by more than the tolerances (the reproductions below).
/// Run with `--ignored`; `SIG_FUZZ_SEED` and `SIG_FUZZ_CASE` replay a failing case.
#[test]
#[ignore = "known CPU/GPU-preparation mismatches, reported"]
fn random_hard_scenes_prepare_to_what_the_cpu_backend_draws() {
    let mut shaping = fonts();
    let handle = shaping.handle();
    let mut atlas = Atlas::new();
    let mut pages = Pages::new();
    fuzz::run(
        "gpu preparation random hard scenes",
        fuzz::iterations(15),
        std::time::Duration::from_secs(120),
        |case| {
            let (metrics, scene) = scenes::random_hard_scene(&mut case.rng, &mut shaping);
            case.note(|| format!("{:?} at {}", metrics.rect(), metrics.scale().numerator()));
            let prepared = prepare(&scene, &metrics, &mut atlas, &handle).unwrap();
            pages.apply(&prepared);
            let name = format!("random case {}", case.index);
            compare_prepared(&name, &scene, &metrics, &prepared, &pages, &handle, 3);
        },
    );
}

/// The largest channel difference between the CPU backend's frame of `scene` and the prepared
/// frame as the reference rasterizer draws it, over every pixel.
fn worst_difference(scene: &Scene, metrics: &Metrics, fonts: &FontSystemHandle) -> u8 {
    let mut atlas = Atlas::new();
    let mut pages = Pages::new();
    let prepared = prepare(scene, metrics, &mut atlas, fonts).unwrap();
    pages.apply(&prepared);
    let ours = draw(&prepared, &pages);
    let theirs = cpu(scene, metrics, fonts);
    ours.iter()
        .zip(&theirs)
        .map(|(o, t)| {
            let o = o.map(|v| (v * 255.0).round() as u8);
            (0..4).map(|k| o[k].abs_diff(t[k])).max().unwrap()
        })
        .max()
        .unwrap()
}

fn reproduction(rect: WindowRect, scale: u32, clear: Rgba, draw: impl FnOnce(&mut Pen)) -> u8 {
    let fonts = fonts();
    let metrics = Metrics::new(
        LayoutEpoch::next(),
        rect,
        Scale::from_fractional(scale).unwrap(),
    );
    let mut pen = Pen::new(rect);
    pen.clear(clear);
    draw(&mut pen);
    worst_difference(&pen.finish(), &metrics, &fonts.handle())
}

const SURFACE: WindowRect = WindowRect {
    x: 0,
    y: 0,
    width: 125,
    height: 83,
};

/// A filled polygon that crosses itself and the surface's right edge: the prepared frame
/// rasterized it over its whole ink bounds and differed from the CPU backend (tiny-skia
/// painting it into the target, clipped there) by up to 9 levels; within the buffer it agrees
/// (20643a3).
#[test]
fn a_self_crossing_polygon_differs_at_its_edges() {
    let worst = reproduction(SURFACE, 126, Rgba::rgb(0xd51571), |pen| {
        pen.shape(Rgba::rgb(0x3f0986), |p| {
            p.move_to(131.758, 12.7764);
            p.line_to(103.2556, 2.5775);
            p.line_to(105.0964, 6.8918);
            p.line_to(109.5137, 18.8708);
            p.line_to(125.9628, 2.9291);
            p.line_to(125.879, 24.0104);
            p.line_to(104.7483, 32.0074);
        });
    });
    assert!(worst <= 2, "worst difference {worst}");
}

/// A filled lens of two quads crossing the surface's right edge: 38 levels off before the
/// prepared frame rasterized paths within the buffer (20643a3); now within 2.
#[test]
fn a_curve_differs_where_it_is_nearly_horizontal() {
    let worst = reproduction(SURFACE, 126, Rgba::rgb(0xd51571), |pen| {
        pen.shape(Rgba::rgb(0x3f0986), |p| {
            p.move_to(101.9762, 17.7684);
            p.quad_to(119.1262, 11.3684, 136.2762, 17.7684);
            p.quad_to(119.1262, 23.7684, 101.9762, 17.7684);
            p.close();
        });
    });
    assert!(worst <= 2, "worst difference {worst}");
}

/// A stroke's round cap, at alpha 128 over a light green: one pixel at the cap's top differs
/// by 4 levels, within the path tolerance.
#[test]
fn a_round_caps_top_pixel_differs() {
    let rect = WindowRect {
        x: 1526,
        y: 1944,
        width: 791,
        height: 41,
    };
    let worst = reproduction(rect, 140, Rgba::rgb(0x96e74c), |pen| {
        pen.stroke(3.57601, Rgba::rgb_alpha(0x4a5aa980), |p| {
            p.move_to(2295.9668, 1971.5178);
            p.line_to(2292.9783, 1984.4263);
            p.line_to(2291.4016, 1991.8363);
            p.line_to(2298.2078, 2005.2723);
        });
    });
    assert!(worst <= 16, "worst difference {worst}");
}

/// A rounded rectangle with a long arc: radius 20.43 at scale 1.75 (36 buffer pixels, clamped
/// to half its width), which also crosses the surface's edge: the analytic corner and
/// tiny-skia's sampled one differ by 33 levels (12 with the same corner inside the surface,
/// 18 unclamped), within the random comparisons' corner tolerance, not the fixtures' 16.
#[test]
fn a_long_corner_arc_differs_by_more_than_the_fixtures_corner_tolerance() {
    let rect = WindowRect {
        x: 651,
        y: 592,
        width: 17,
        height: 107,
    };
    let worst = reproduction(rect, 210, Rgba::rgb(0xb40f50), |pen| {
        pen.fill_rounded(
            [649.5287, 589.5909, 37.0, 127.0],
            20.43,
            Rgba::rgb(0x571ed2),
        );
    });
    assert!(worst > 16 && worst <= 64, "worst difference {worst}");
}
