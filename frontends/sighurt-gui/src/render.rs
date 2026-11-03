//! The chrome's renderers as `sig` runs them: the ones compiled into this build
//! ([`RENDERERS`]), and starting the one `[ui] renderer` names when `sig` starts.
//!
//! `[ui] renderer` takes every renderer name of the project (the core checks it against
//! `sighurt_core::config::RENDERERS`), so one configuration serves every build: a name this
//! build lacks, or a renderer that fails to start, gives a notice and the window uses `auto`.
//! `auto` is the CPU backend until the GPU backends are measured (docs/rewrite/RENDER.md
//! §14.5). The renderer is chosen once; a change takes effect at the next start.
//!
//! Must not own: what a renderer does (sighurt-render and the backend crates), the chrome's
//! targets (the owner loop's), or which surfaces are drawn (the chrome's).

use sighurt_render::{Backend, CpuBackend, FontSystemHandle};

/// A renderer a build can offer.
#[derive(Clone, Copy)]
pub struct RendererKind {
    /// Its name in `[ui] renderer`.
    pub name: &'static str,
    /// Starts it, rasterizing glyphs from the font system the chrome shapes with. "Started"
    /// means it can present; why it can't, otherwise.
    pub start: fn(FontSystemHandle) -> Result<Box<dyn Backend>, String>,
}

/// The renderers compiled into this build: the CPU one always.
pub const RENDERERS: &[RendererKind] = &[RendererKind {
    name: "cpu",
    start: |fonts| Ok(Box::new(CpuBackend::new(fonts))),
}];

/// What `auto` tries, in order: the first that starts.
const AUTO: &[&str] = &["cpu"];

/// Starts the renderer `name` (`[ui] renderer`) with `fonts`, or `auto`'s when this build lacks
/// it or it can't start; then also says why, for a notice.
pub fn start(name: &str, fonts: &FontSystemHandle) -> (Box<dyn Backend>, Option<String>) {
    let mut notice = None;
    if name != "auto" {
        match RENDERERS.iter().find(|kind| kind.name == name) {
            Some(kind) => match (kind.start)(fonts.clone()) {
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
        if let Ok(backend) = (kind.start)(fonts.clone()) {
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
            assert_eq!(backend.name(), "cpu", "{name}");
            let compiled = RENDERERS.iter().any(|kind| kind.name == *name);
            assert_eq!(
                notice.is_some(),
                *name != "auto" && !compiled,
                "{name}: {notice:?}"
            );
        }
        let (_, notice) = start("wgpu", &fonts);
        assert_eq!(
            notice.as_deref(),
            Some(
                "[ui] renderer = \"wgpu\" is not in this build (it has: cpu); the chrome is \
                 drawn as \"auto\" says."
            )
        );
    }
}
