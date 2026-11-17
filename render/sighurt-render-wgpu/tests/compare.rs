//! The wgpu backend against the CPU backend (RENDER.md §12): the standard chrome's and the line
//! chrome's surfaces, every item kind under each clip case, seeded random scenes and text with
//! the system's fonts, rendered by both, offscreen on the GPU and read back (`render_to_image`,
//! test-only). The comparison and the tests are shared with the Vulkan backend
//! (render/sighurt-render/tests/support/compare.rs): every pixel within 2 levels a channel,
//! except near rounded corners, where the difference is bounded and counted. Needs a Vulkan
//! adapter: run it under tools/heavy; without one it says so and passes.

#[path = "../../../ui/sighurt-ui/tests/common/mod.rs"]
mod common;
#[path = "../../sighurt-render/tests/support/compare.rs"]
mod compare;
#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;
#[path = "../../sighurt-render/tests/support/scenes.rs"]
mod scenes;

use sighurt_api::geometry::Metrics;
use sighurt_render as render;
use sighurt_render::{Backend, FontSystemHandle, RenderError, Scene};
use sighurt_render_wgpu::WgpuBackend;

struct Wgpu(WgpuBackend);

impl compare::Gpu for Wgpu {
    fn name(&self) -> &'static str {
        "wgpu"
    }

    fn image(&mut self, metrics: &Metrics, scene: &Scene) -> Result<Vec<u8>, RenderError> {
        self.0.render_to_image(metrics, scene)
    }
}

fn open(fonts: FontSystemHandle) -> Option<Wgpu> {
    match WgpuBackend::headless(fonts) {
        Ok(mut backend) => {
            for notice in backend.take_notices() {
                eprintln!("notice: {notice}");
            }
            eprintln!("on {}", backend.describe());
            Some(Wgpu(backend))
        }
        Err(why) => {
            eprintln!("skipped: no Vulkan adapter ({why})");
            None
        }
    }
}

#[test]
fn the_standard_chromes_fixtures_match_the_cpu_backend() {
    compare::standard_chrome(open);
}

#[test]
fn the_line_chromes_surfaces_match_the_cpu_backend() {
    compare::line_chrome(open);
}

#[test]
fn every_item_kind_under_every_clip_matches_the_cpu_backend() {
    compare::every_kind_and_clip(open);
}

#[test]
fn random_scenes_match_the_cpu_backend() {
    compare::random_scenes(open);
}

/// The paths and long corner arcs the other random test leaves out, which differ from the CPU
/// backend's by more than the tolerances: see `scenes::random_scene`. Run with `--ignored`.
#[test]
#[ignore = "known CPU/GPU mismatches, reported"]
fn random_hard_scenes_match_the_cpu_backend() {
    compare::random_hard_scenes(open);
}

/// Colored glyphs (premultiplied at upload), clipped text at odd sizes and fractional scales,
/// translucent colors, icons: with the system's fonts, so an emoji font if one is installed.
#[test]
fn text_and_icons_match_the_cpu_backend() {
    compare::text_and_icons(open);
}
