//! The wgpu backend against the CPU backend (RENDER.md §12): the standard chrome's fixtures
//! rendered by both, offscreen on the GPU and read back (`render_to_image`, test-only). Every
//! pixel within 2 levels a channel, except near rounded corners (an analytic signed distance
//! against tiny-skia's cubic corners), where the difference is bounded and counted. Needs a
//! Vulkan adapter: run it under tools/heavy; without one it says so and passes.

#[path = "../../../ui/sighurt-ui/tests/common/mod.rs"]
mod common;

use common::*;
use sighurt_render::scene::{Kind, Mapping};
use sighurt_render::{Fonts, Pen, Rgba, Scene, TextStyle};
use sighurt_render_wgpu::WgpuBackend;
use sighurt_ui::chrome::{CHROME, PAGE, STATUS};
use sighurt_ui::TabContent;

/// Off-corner channel tolerance: float blending on the GPU against 8-bit on the CPU.
const TOLERANCE: u8 = 2;
/// The most a corner pixel may differ.
const CORNER_TOLERANCE: u8 = 16;

fn gpu(fonts: sighurt_render::FontSystemHandle) -> Option<WgpuBackend> {
    match WgpuBackend::headless(fonts) {
        Ok(backend) => Some(backend),
        Err(why) => {
            eprintln!("skipped: no Vulkan adapter ({why})");
            None
        }
    }
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

/// Compares the two renderings of `scene`; returns how many corner pixels differed beyond the
/// tolerance, and the largest difference anywhere.
fn compare(
    name: &str,
    backend: &mut WgpuBackend,
    scene: &Scene,
    metrics: &sighurt_api::geometry::Metrics,
) -> (usize, u8) {
    let cpu = render(scene, metrics);
    let gpu = backend.render_to_image(metrics, scene).expect("rendered");
    assert_eq!(cpu.rgba.len(), gpu.len(), "{name}");
    let mapping = Mapping::new(metrics);
    let (mut corners, mut worst) = (0, 0);
    for (i, (a, b)) in cpu
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(gpu.as_chunks::<4>().0)
        .enumerate()
    {
        let diff = (0..4).map(|k| a[k].abs_diff(b[k])).max().unwrap();
        worst = worst.max(diff);
        if diff <= TOLERANCE {
            continue;
        }
        let (x, y) = (i as u32 % cpu.width, i as u32 / cpu.width);
        assert!(
            near_corner(scene, &mapping, x, y) && diff <= CORNER_TOLERANCE,
            "{name}: ({x}, {y}) cpu {a:?} wgpu {b:?}"
        );
        corners += 1;
    }
    (corners, worst)
}

#[test]
fn the_standard_chromes_fixtures_match_the_cpu_backend() {
    let Some(mut backend) = gpu(font_system()) else {
        return;
    };
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
                let (corners, worst) = compare(&label, &mut backend, &scene, &metrics);
                println!("{label}: {corners} corner pixels beyond {TOLERANCE}, worst {worst}");
            }
        }
    }
}

/// Colored glyphs (premultiplied at upload), clipped text at odd sizes and fractional scales,
/// translucent colors, icons: with the system's fonts, so an emoji font if one is installed.
#[test]
fn text_and_icons_match_the_cpu_backend() {
    let mut fonts = Fonts::system();
    let Some(mut backend) = gpu(fonts.handle()) else {
        return;
    };
    for scale in [120, 140, 150, 210, 240] {
        let rect = sighurt_api::geometry::WindowRect {
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
        let cpu_backend_fonts = fonts.handle();
        // The CPU side of the comparison must use the same font system.
        let cpu = {
            use sighurt_render::{Backend, CpuBackend, Output, PixelBuffer, SurfaceKey};
            let size = metrics.buffer();
            let mut data = vec![0u8; size.width as usize * size.height as usize * 4];
            let buffer = PixelBuffer {
                data: &mut data,
                width: size.width,
                height: size.height,
                stride: size.width as usize * 4,
            };
            let mut cpu = CpuBackend::new(cpu_backend_fonts);
            let _ = cpu
                .render(SurfaceKey(1), &metrics, &scene, Output::Pixels(buffer))
                .unwrap();
            argb_to_rgba(&data)
        };
        let gpu = backend.render_to_image(&metrics, &scene).expect("rendered");
        let worst = cpu
            .iter()
            .zip(&gpu)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(worst <= TOLERANCE, "at {scale}: {worst}");
    }
}
