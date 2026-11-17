//! The comparison of a GPU backend with the CPU backend (RENDER.md §12), shared by the wgpu
//! and the Vulkan crates' `tests/compare.rs`: what the tolerance is, how two renderings are
//! compared (every channel within 2 levels, except near rounded corners, where an analytic
//! signed distance on the GPU meets tiny-skia's cubic corners and up to 16 is allowed and
//! counted), and the test bodies (the standard chrome's fixtures, the line chrome's surfaces,
//! every item kind under each clip case, seeded random scenes through one backend, text and
//! icons with the system's fonts). A backend implements [`Gpu`]; a test body takes a function
//! that opens one, or `None` (it then says "skipped" and passes).
//!
//! Included by path with the root modules `common` (sighurt-ui's tests/common/mod.rs), `fuzz`
//! (tools/fuzz/mod.rs) and `scenes` (support/scenes.rs) beside it, and `render` naming
//! sighurt-render. Must not own: a backend's device or how it reads pixels back.

#![allow(dead_code)]

use std::time::Duration;

use sighurt_api::geometry::{LogicalSize, Metrics, WindowRect};
use sighurt_chrome_line::{Line, LINE, PAGE as LINE_PAGE};
use sighurt_render::scene::{Kind, Mapping};
use sighurt_render::{
    Backend, CpuBackend, FontSystemHandle, Fonts, Output, Pen, PixelBuffer, RenderError,
    RenderOutcome, Rgba, Scene, SurfaceKey, TextStyle,
};
use sighurt_ui::chrome::{CHROME, PAGE, STATUS};
use sighurt_ui::extension::{Chrome, ChromeEvent, Theme};
use sighurt_ui::{Snapshot, TabContent};

use crate::common::{
    argb_to_rgba, chrome, font_system, metrics, rect_of, scene, showing, tab, three_tabs, SCALES,
};
use crate::fuzz;
use crate::scenes::{kind_scenes, random_hard_scene, random_scene};

/// Off-corner channel tolerance: float blending on the GPU against 8-bit on the CPU.
pub const TOLERANCE: u8 = 2;
/// The most a corner pixel of the chromes' fixtures may differ.
pub const CORNER_TOLERANCE: u8 = 16;
/// The most a corner pixel of a random scene may differ: the GPU's corner is a signed
/// distance antialiased linearly over a pixel, tiny-skia's a path it samples 4 × 4 a pixel, and
/// where the arc crosses a pixel steeply the two coverages differ by up to about an eighth
/// (seen: 33 at a radius of 5.4 buffer pixels), more where the corner also crosses the
/// buffer's edge, where tiny-skia clips the path to the pixmap and its antialiasing changes
/// (seen: 48 on a 4-pixel-high surface). The chromes' radii, inside their surfaces, stay
/// within 16.
pub const RANDOM_CORNER_TOLERANCE: u8 = 64;
/// Off-corner tolerance for a random scene: dozens of translucent layers, each rounded to 8
/// bits by both, but by tiny-skia's approximation of a division by 255 on the CPU (seen: 3).
pub const RANDOM_TOLERANCE: u8 = 3;
/// The most a pixel of a path's ink may differ: one of tiny-skia's 16 antialiasing samples
/// a pixel (4 × 4). Both backends rasterize the path with tiny-skia, but the GPU's coverage
/// tile is rasterized at a transform translated by the tile's position, whose rounding moves
/// the outline by about 10⁻⁴ pixel, now and then across a sample (seen: 8 levels inside a
/// thick stroke that overlaps itself, 10 on a hairline); and the GPU blends the 8-bit coverage
/// with the color in float, the CPU backend both in 8 bits (3 or 4 levels at alpha 128).
pub const PATH_TOLERANCE: u8 = 16;

/// A GPU backend that renders scenes offscreen and reads them back.
pub trait Gpu {
    /// Its name in failure messages.
    fn name(&self) -> &'static str;
    /// `scene` for `metrics` as rows of RGBA bytes. One backend renders many scenes in
    /// sequence, as `sig`'s does.
    fn image(&mut self, metrics: &Metrics, scene: &Scene) -> Result<Vec<u8>, RenderError>;
}

/// What a test compared.
#[derive(Default)]
pub struct Tally {
    pub scenes: usize,
    pub pixels: u64,
    /// Pixels beyond [`TOLERANCE`] (near a rounded corner, within [`CORNER_TOLERANCE`]).
    pub corners: usize,
    pub worst: u8,
}

impl Tally {
    fn report(&self, what: &str) {
        println!(
            "{what}: {} scenes, {} pixels compared, {} corner pixels beyond {TOLERANCE}, worst difference {}",
            self.scenes, self.pixels, self.corners, self.worst
        );
    }
}

/// The CPU backend's rendering of `scene` (a fresh backend on `fonts`) as RGBA bytes.
pub fn cpu_image(scene: &Scene, metrics: &Metrics, fonts: &FontSystemHandle) -> Vec<u8> {
    let size = metrics.buffer();
    let mut data = vec![0u8; size.width as usize * size.height as usize * 4];
    let buffer = PixelBuffer {
        data: &mut data,
        width: size.width,
        height: size.height,
        stride: size.width as usize * 4,
    };
    let mut cpu = CpuBackend::new(fonts.clone());
    let outcome = cpu.render(SurfaceKey(1), metrics, scene, Output::Pixels(buffer));
    assert_eq!(outcome, Ok(RenderOutcome::Submitted));
    argb_to_rgba(&data)
}

/// Whether `(x, y)` is within reach of a rounded corner of `scene`.
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
        let inside =
            px > r.left - 1.0 && px < r.right + 1.0 && py > r.top - 1.0 && py < r.bottom + 1.0;
        near_x && near_y && inside
    })
}

/// Whether `(x, y)` is in the ink bounds of one of `scene`'s paths.
pub fn in_path_ink(scene: &Scene, mapping: &Mapping, x: u32, y: u32) -> bool {
    scene.items.iter().any(|item| {
        let Kind::Path { path, stroke, .. } = item.kind else {
            return false;
        };
        sighurt_render::shape::ink_bounds(scene.path(path), stroke, mapping).is_some_and(|b| {
            let (x, y) = (i64::from(x), i64::from(y));
            (i64::from(b.x)..i64::from(b.x) + i64::from(b.width)).contains(&x)
                && (i64::from(b.y)..i64::from(b.y) + i64::from(b.height)).contains(&y)
        })
    })
}

/// Renders `scene` with `gpu` and with a fresh CPU backend on `fonts` and compares them,
/// panicking on the first pixel beyond the tolerances.
pub fn compare<G: Gpu>(
    name: &str,
    gpu: &mut G,
    fonts: &FontSystemHandle,
    scene: &Scene,
    metrics: &Metrics,
    tally: &mut Tally,
    (tolerance, corner_tolerance): (u8, u8),
) {
    let cpu = cpu_image(scene, metrics, fonts);
    let image = gpu
        .image(metrics, scene)
        .unwrap_or_else(|e| panic!("{name}: {} failed: {e}", gpu.name()));
    assert_eq!(cpu.len(), image.len(), "{name}");
    let mapping = Mapping::new(metrics);
    let width = metrics.buffer().width;
    tally.scenes += 1;
    tally.pixels += (cpu.len() / 4) as u64;
    for (i, (a, b)) in cpu
        .as_chunks::<4>()
        .0
        .iter()
        .zip(image.as_chunks::<4>().0)
        .enumerate()
    {
        let diff = (0..4).map(|k| a[k].abs_diff(b[k])).max().unwrap();
        tally.worst = tally.worst.max(diff);
        if diff <= tolerance {
            continue;
        }
        let (x, y) = (i as u32 % width, i as u32 / width);
        if diff <= PATH_TOLERANCE && in_path_ink(scene, &mapping, x, y) {
            continue;
        }
        assert!(
            near_corner(scene, &mapping, x, y) && diff <= corner_tolerance,
            "{name}: ({x}, {y}) of {width}x{} cpu {a:?} {} {b:?} (difference {diff})",
            metrics.buffer().height,
            gpu.name()
        );
        tally.corners += 1;
    }
}

/// The standard chrome's fixtures at every scale.
pub fn standard_chrome<G: Gpu>(open: impl Fn(FontSystemHandle) -> Option<G>) {
    let fonts = font_system();
    let Some(mut gpu) = open(fonts.clone()) else {
        return;
    };
    let mut tally = Tally::default();
    let fixtures = [
        ("three tabs", three_tabs()),
        ("home", showing("sig://home", TabContent::Internal)),
    ];
    for (name, snapshot) in fixtures {
        let mut chrome = chrome(1280, 800, snapshot);
        for surface in [CHROME, STATUS, PAGE] {
            for scale in SCALES {
                let rect = rect_of(&chrome, surface);
                if rect.size().is_empty() {
                    continue;
                }
                let metrics = metrics(rect, scale);
                let scene = scene(&mut chrome, surface, &metrics);
                let label = format!("{name} {surface} at {scale}");
                compare(
                    &label,
                    &mut gpu,
                    &fonts,
                    &scene,
                    &metrics,
                    &mut tally,
                    (TOLERANCE, CORNER_TOLERANCE),
                );
            }
        }
    }
    tally.report("standard chrome");
}

/// Five tabs, the second active and loading, a hovered link's status and a mode's indicator.
fn five_tabs() -> Snapshot {
    let mut tabs: Vec<_> = (0..5)
        .map(|i| {
            tab(
                &format!("Tab {i}"),
                "https://a.example/",
                TabContent::Engine,
            )
        })
        .collect();
    tabs[1].url = "https://sv.wikipedia.org/wiki/R%C3%A4ksm%C3%B6rg%C3%A5s#K%C3%A4llor".into();
    tabs[1].title = "Räksmörgås — Wikipedia".into();
    tabs[1].active = true;
    tabs[1].loading = true;
    for (i, name) in ["servo", "wpe", "", "wasm", "cef"].iter().enumerate() {
        tabs[i].engine = (!name.is_empty()).then(|| name.to_string());
    }
    Snapshot {
        tabs,
        status: Some("https://sv.wikipedia.org/wiki/Bl%C3%A5b%C3%A4r".into()),
        keymap: Some("NORMAL".into()),
        ..Snapshot::default()
    }
}

/// The line chrome's surfaces (`LINE`, `PAGE`) with five tabs and with the home page, at
/// every scale.
pub fn line_chrome<G: Gpu>(open: impl Fn(FontSystemHandle) -> Option<G>) {
    let fonts = font_system();
    let Some(mut gpu) = open(fonts.clone()) else {
        return;
    };
    let mut tally = Tally::default();
    let snapshots = [
        ("five tabs", five_tabs()),
        ("home", showing("sig://home", TabContent::Internal)),
    ];
    for (name, snapshot) in snapshots {
        let mut line: Box<dyn Chrome> = Box::new(Line::new(Fonts::new(fonts.clone())));
        line.handle(ChromeEvent::Configure {
            theme: Theme::default(),
            settings: Default::default(),
        });
        line.handle(ChromeEvent::Resize(LogicalSize {
            width: 700,
            height: 400,
        }));
        line.handle(ChromeEvent::Snapshot(snapshot));
        for surface in [LINE, LINE_PAGE] {
            let rect: WindowRect = line
                .layout()
                .surfaces
                .iter()
                .find(|s| s.name == surface)
                .expect("the line chrome's surface")
                .rect;
            for scale in SCALES {
                let metrics = metrics(rect, scale);
                let mut pen = Pen::new(metrics.rect());
                line.draw(surface, &mut pen);
                let scene = pen.finish();
                let label = format!("line chrome, {name}, {surface} at {scale}");
                compare(
                    &label,
                    &mut gpu,
                    &fonts,
                    &scene,
                    &metrics,
                    &mut tally,
                    (TOLERANCE, CORNER_TOLERANCE),
                );
            }
        }
    }
    tally.report("line chrome");
}

/// Every item kind alone under each clip case at every scale.
pub fn every_kind_and_clip<G: Gpu>(open: impl Fn(FontSystemHandle) -> Option<G>) {
    let fonts = font_system();
    let Some(mut gpu) = open(fonts.clone()) else {
        return;
    };
    let mut tally = Tally::default();
    for (name, metrics, scene) in kind_scenes(&mut Fonts::new(fonts.clone())) {
        compare(
            &name,
            &mut gpu,
            &fonts,
            &scene,
            &metrics,
            &mut tally,
            (TOLERANCE, CORNER_TOLERANCE),
        );
    }
    tally.report("kinds and clips");
}

/// Seeded random scenes (`SIG_FUZZ_ITERATIONS`, default 2000, 15 % of it: 300) rendered by
/// one backend in sequence: its atlas grows, starts over at scale changes and alternates its
/// icon ring across frames, as in `sig`. `SIG_FUZZ_SEED` and `SIG_FUZZ_CASE` replay.
pub fn random_scenes<G: Gpu>(open: impl Fn(FontSystemHandle) -> Option<G>) {
    random(open, "random scenes", random_scene);
}

/// [`random_scenes`] with the paths and long corner arcs that differ from the CPU backend's by
/// more than the tolerances (see `scenes::random_scene`).
pub fn random_hard_scenes<G: Gpu>(open: impl Fn(FontSystemHandle) -> Option<G>) {
    random(open, "random hard scenes", random_hard_scene);
}

fn random<G: Gpu>(
    open: impl Fn(FontSystemHandle) -> Option<G>,
    what: &str,
    generate: fn(&mut fuzz::Rng, &mut Fonts) -> (Metrics, Scene),
) {
    let fonts = font_system();
    let Some(mut gpu) = open(fonts.clone()) else {
        return;
    };
    let mut shaping = Fonts::new(fonts.clone());
    let mut tally = Tally::default();
    let name = format!("{} {what}", gpu.name());
    fuzz::run(
        &name,
        fuzz::iterations(15),
        Duration::from_secs(120),
        |case| {
            let (metrics, scene) = generate(&mut case.rng, &mut shaping);
            case.note(|| {
                format!(
                    "{:?} at {} with {} items",
                    metrics.rect(),
                    metrics.scale().numerator(),
                    scene.items.len()
                )
            });
            let label = format!("random case {}", case.index);
            // Dozens of layers and any radius: the random scenes' tolerances.
            let tolerances = (RANDOM_TOLERANCE, RANDOM_CORNER_TOLERANCE);
            compare(
                &label, &mut gpu, &fonts, &scene, &metrics, &mut tally, tolerances,
            );
        },
    );
    tally.report(&name);
}

/// Colored glyphs (premultiplied at upload), clipped text at odd sizes and fractional scales,
/// translucent colors, icons: with the system's fonts, so an emoji font if one is installed.
pub fn text_and_icons<G: Gpu>(open: impl Fn(FontSystemHandle) -> Option<G>) {
    let mut fonts = Fonts::system();
    let handle = fonts.handle();
    let Some(mut gpu) = open(handle.clone()) else {
        return;
    };
    for scale in [120, 140, 150, 210, 240] {
        let rect = WindowRect {
            x: 3,
            y: 7,
            width: 251,
            height: 37,
        };
        let metrics = metrics(rect, scale);
        let mut pen = Pen::new(rect);
        pen.clear(Rgba::rgb(0x18181b));
        let line = fonts.line("Grüße 😀🎉 ✓ fjord — 中文", TextStyle::SM);
        pen.text(
            &line,
            (5.3, 9.0),
            Rgba::rgb(0xf4f4f5),
            [5.0, 9.0, 150.0, 30.0],
        );
        let small = fonts.line("example.com/räksmörgås", TextStyle::XS);
        pen.text(
            &small,
            (150.0, 20.0),
            Rgba::rgb_alpha(0x60a5fac0),
            [150.0, 20.0, 61.0, 13.0],
        );
        pen.stroke(1.5, Rgba::rgb(0xe4e4e7), |p| {
            p.move_to(220.0, 12.0);
            p.line_to(226.0, 18.0);
            p.line_to(220.0, 24.0);
        });
        pen.shape(Rgba::rgb_alpha(0x50fa7b99), |p| {
            p.push_circle(240.0, 20.0, 5.5)
        });
        let scene = pen.finish();
        let cpu = cpu_image(&scene, &metrics, &handle);
        let image = gpu.image(&metrics, &scene).expect("rendered");
        let worst = cpu
            .iter()
            .zip(&image)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        println!("text and icons at {scale}: worst difference {worst}");
        assert!(worst <= TOLERANCE, "at {scale}: {worst}");
    }
}
