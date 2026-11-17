//! What the Vulkan backend costs the process, stage by stage (RENDER.md §16): resident memory,
//! threads and time after the loader and instance, after the device, and after rendering the
//! standard chrome's fixture, with the largest resident mappings. A measurement, not a check:
//! `#[ignore]`d; run it on a machine with a GPU, alone, through tools/heavy:
//! `cargo test -p sighurt-render-vulkan --test cost -- --ignored --nocapture`.

#[path = "../../../ui/sighurt-ui/tests/common/mod.rs"]
mod common;
#[path = "../../sighurt-render/tests/support/cost.rs"]
mod cost;

use common::*;
use sighurt_render_vulkan::VulkanBackend;
use sighurt_ui::chrome::CHROME;

#[test]
#[ignore = "a measurement: run it alone with --ignored --nocapture"]
fn what_the_backend_costs() {
    let mut probe = cost::Probe::new("vulkan");
    let fonts = font_system();
    let mut chrome = chrome(1280, 800, three_tabs());
    let metrics = metrics(rect_of(&chrome, CHROME), 120);
    let scene = scene(&mut chrome, CHROME, &metrics);
    probe.stage("before");
    let backend = VulkanBackend::new(fonts.clone(), true);
    probe.stage("loader and instance");
    drop(backend);
    let mut backend = match VulkanBackend::headless(fonts) {
        Ok(backend) => backend,
        Err(why) => return eprintln!("skipped: no Vulkan device ({why})"),
    };
    probe.stage("instance and device");
    backend.render_to_image(&metrics, &scene).expect("rendered");
    probe.stage("first frame");
    for _ in 0..100 {
        backend.render_to_image(&metrics, &scene).expect("rendered");
    }
    probe.stage("100 more frames");
    probe.mappings(15);
    drop(backend);
    probe.stage("dropped");
}
