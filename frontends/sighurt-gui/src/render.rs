//! The chrome's renderers as `sig` runs them: the ones compiled into this build
//! ([`RENDERERS`]), and starting the one `[ui] renderer` names when `sig` starts.
//!
//! `[ui] renderer` takes every renderer name of the project (the core checks it against
//! `sighurt_core::config::RENDERERS`), so one configuration serves every build: a name this
//! build lacks, or a renderer that fails to start, gives a notice and the window uses `auto`.
//! `auto` is the CPU backend until the GPU backends are measured to be at worst equal
//! (docs/rewrite/RENDER.md §14.5). The renderer is chosen once; a change takes effect at the
//! next start.
//!
//! Starting a native renderer here is provisional: the window doesn't exist yet, so whether it
//! can present is known only at its first bind, in the owner loop, which falls back to the CPU
//! renderer when it can't (`crate::app`).
//!
//! Must not own: what a renderer does (sighurt-render and the backend crates), the chrome's
//! targets (the owner loop's), or which surfaces are drawn (the chrome's).

use sighurt_render::{Backend, CpuBackend, FontSystemHandle};

/// A renderer a build can offer.
#[derive(Clone, Copy)]
pub struct RendererKind {
    /// Its name in `[ui] renderer`.
    pub name: &'static str,
    /// Starts it, rasterizing glyphs from the font system the chrome shapes with; the flag
    /// says `[ui] renderer` named it rather than `auto` choosing it. Why it can't, otherwise.
    pub start: fn(FontSystemHandle, bool) -> Result<Box<dyn Backend>, String>,
}

/// The renderers compiled into this build: the CPU one always.
pub const RENDERERS: &[RendererKind] = &[
    RendererKind {
        name: "cpu",
        start: |fonts, _| Ok(Box::new(CpuBackend::new(fonts))),
    },
    #[cfg(feature = "render-wgpu")]
    RendererKind {
        name: "wgpu",
        start: |fonts, explicit| {
            let backend = sighurt_render_wgpu::WgpuBackend::new(fonts, explicit)?;
            Ok(Box::new(backend))
        },
    },
    #[cfg(feature = "render-vulkan")]
    RendererKind {
        name: "vulkan",
        start: |fonts, explicit| {
            let backend = sighurt_render_vulkan::VulkanBackend::new(fonts, explicit)?;
            Ok(Box::new(backend))
        },
    },
];

/// What `auto` tries, in order: the first that starts.
const AUTO: &[&str] = &["cpu"];

/// Starts the renderer `name` (`[ui] renderer`) with `fonts`, or `auto`'s when this build lacks
/// it or it can't start; then also says why, for a notice.
pub fn start(name: &str, fonts: &FontSystemHandle) -> (Box<dyn Backend>, Option<String>) {
    let mut notice = None;
    if name != "auto" {
        match RENDERERS.iter().find(|kind| kind.name == name) {
            Some(kind) => match (kind.start)(fonts.clone(), true) {
                Ok(backend) => return (backend, None),
                Err(why) => {
                    notice = Some(format!(
                        "[ui] renderer = {name:?} can't start ({why}); the chrome is drawn as \
                         \"auto\" says."
                    ))
                }
            },
            None => {
                let has: Vec<_> = RENDERERS.iter().map(|kind| kind.name).collect();
                notice = Some(format!(
                    "[ui] renderer = {name:?} is not in this build (it has: {}); the chrome is \
                     drawn as \"auto\" says.",
                    has.join(", ")
                ));
            }
        }
    }
    for kind in AUTO
        .iter()
        .filter_map(|auto| RENDERERS.iter().find(|k| k.name == *auto))
    {
        if let Ok(backend) = (kind.start)(fonts.clone(), false) {
            return (backend, notice);
        }
    }
    // The CPU backend always starts.
    (Box::new(CpuBackend::new(fonts.clone())), notice)
}

#[cfg(test)]
mod tests {
    use sighurt_ui::Fonts;

    use super::*;

    #[test]
    fn every_project_renderer_name_starts_something() {
        let fonts = Fonts::from_data(Vec::<Vec<u8>>::new()).handle();
        for name in sighurt_core::config::RENDERERS {
            let (backend, notice) = start(name, &fonts);
            let compiled = RENDERERS.iter().any(|kind| kind.name == *name);
            if compiled && !["auto", "cpu"].contains(name) && backend.name() == "cpu" {
                // Compiled in, but it can't start here (no Vulkan loader): said so.
                let said = notice.as_ref().is_some_and(|n| n.contains("can't start"));
                assert!(said, "{name}: {notice:?}");
                continue;
            }
            let expected = if compiled && *name != "auto" {
                *name
            } else {
                "cpu"
            };
            assert_eq!(backend.name(), expected, "{name}");
            assert_eq!(
                notice.is_some(),
                *name != "auto" && !compiled,
                "{name}: {notice:?}"
            );
        }
        let (auto, _) = start("auto", &fonts);
        assert_eq!(
            auto.name(),
            "cpu",
            "auto is the CPU renderer until measured"
        );
    }
}
