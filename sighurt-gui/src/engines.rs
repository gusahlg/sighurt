//! Binding the compiled-in engines to the Wayland host's targets.
//!
//! The runtime's registry holds `Box<dyn Running<TargetLease, FramePermit>>` and knows nothing
//! of Wayland. This module constructs each runtime, wraps it so the registry can ask how it
//! renders, and records the [`PresentMode`] its targets are created with. The mode stays beside
//! the engine it belongs to: the two cannot drift. Shutdown order is the order the engines are
//! listed here (Servo, then Wasm, then WPE, then the demo).
//!
//! Must not own: when a runtime starts (the registry, on first use, except Servo at launch) or
//! what it is asked to do.

use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::ids::ViewId;
use sighurt_wayland::{FramePermit, PresentMode, TargetLease};

#[cfg(any(
    feature = "engine-servo",
    feature = "engine-wasm",
    feature = "engine-wpe"
))]
use sighurt_runtime::registry;
use sighurt_runtime::registry::{Engine, Registry, Running, Runtime};

/// One engine's runtime, bound to the host's target and permit, so it can sit in [`Runtime`].
#[cfg(any(
    feature = "engine-servo",
    feature = "engine-wasm",
    feature = "engine-wpe",
    feature = "engine-demo"
))]
struct Bound<R>(R);

#[cfg(any(
    feature = "engine-servo",
    feature = "engine-wasm",
    feature = "engine-wpe",
    feature = "engine-demo"
))]
impl<R> EngineRuntime for Bound<R>
where
    R: EngineRuntime<Target = TargetLease, FramePermit = FramePermit>,
{
    type Target = TargetLease;
    type FramePermit = FramePermit;

    fn capabilities(&self) -> EngineCapabilities {
        self.0.capabilities()
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: ViewOptions,
        target: Self::Target,
    ) -> Result<(), EngineError> {
        self.0.open_view(view, options, target)
    }

    fn command(&mut self, view: ViewId, command: EngineCommand) -> Result<(), EngineError> {
        self.0.command(view, command)
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        self.0.pump(events)
    }

    fn present(
        &mut self,
        view: ViewId,
        permit: Self::FramePermit,
    ) -> Result<sighurt_api::engine::PresentOutcome, EngineError> {
        self.0.present(view, permit)
    }

    fn close_view(&mut self, view: ViewId) {
        self.0.close_view(view);
    }

    fn shutdown(&mut self) {
        self.0.shutdown();
    }
}

/// The engines of this build, none started, and how each one's views present.
pub fn build() -> (
    Registry<TargetLease, FramePermit>,
    Vec<(&'static str, PresentMode)>,
) {
    let mut engines = Vec::new();
    let mut modes = Vec::new();
    #[cfg(feature = "engine-servo")]
    {
        let (engine, mode) = servo();
        modes.push((engine.name, mode));
        engines.push(engine);
    }
    #[cfg(feature = "engine-wasm")]
    {
        let (engine, mode) = wasm();
        modes.push((engine.name, mode));
        engines.push(engine);
    }
    #[cfg(feature = "engine-wpe")]
    {
        let (engine, mode) = wpe();
        modes.push((engine.name, mode));
        engines.push(engine);
    }
    #[cfg(feature = "engine-demo")]
    {
        let (engine, mode) = demo();
        modes.push((engine.name, mode));
        engines.push(engine);
    }
    (Registry::new(engines), modes)
}

#[cfg(feature = "engine-servo")]
fn servo() -> (Engine<TargetLease, FramePermit>, PresentMode) {
    use sighurt_engine_servo::{ServoRuntime, CAPABILITIES};
    let engine = Engine::new(
        "servo",
        CAPABILITIES,
        "gl, straight into the page's subsurface",
        &["user-agent", "storage"],
        |waker, launch| {
            let runtime = ServoRuntime::new(registry::servo_config(launch.settings), waker);
            // The first caller installs Servo's logger; a later start logs through whatever
            // was installed at launch (decision 26).
            if !registry::claim_logger() {
                runtime.setup_logging();
            } else {
                log::warn!(
                    "Servo started after the logger: its script-thread panics are not caught"
                );
            }
            Ok(Runtime::Servo(Box::new(Bound(runtime))))
        },
    );
    (engine, PresentMode::Gl)
}

#[cfg(feature = "engine-servo")]
impl Running<TargetLease, FramePermit> for Bound<sighurt_engine_servo::ServoRuntime> {
    fn renderer(&self) -> Option<(&'static str, Option<String>)> {
        self.0.diagnostics().gl.map(|gl| {
            let mode = if gl.software { "software" } else { "hardware" };
            (mode, Some(gl.renderer))
        })
    }

    fn readbacks(&self) -> u64 {
        let d = self.0.diagnostics();
        d.read_to_image + d.gl_reads.unwrap_or(0)
    }

    fn paint_time(&self) -> Option<std::time::Duration> {
        Some(self.0.diagnostics().timing.paint)
    }

    fn gl_reads(&self) -> Option<u64> {
        self.0.diagnostics().gl_reads
    }

    fn leaked(&self) -> bool {
        self.0.diagnostics().leaked
    }
}

#[cfg(feature = "engine-wasm")]
fn wasm() -> (Engine<TargetLease, FramePermit>, PresentMode) {
    use sighurt_engine_wasm::WasmRuntime;
    // What `WasmRuntime::capabilities` reports; `Registry::start` warns if they ever differ.
    let capabilities = EngineCapabilities {
        history_traversal: true,
        page_zoom: true,
        ..EngineCapabilities::default()
    };
    let engine = Engine::new(
        "wasm",
        capabilities,
        "cpu (tiny-skia), wl_shm",
        // Not `user-agent`: guests make their own requests.
        &["storage"],
        |waker, launch| {
            WasmRuntime::<TargetLease>::new(registry::wasm_config(launch)?, waker)
                .map(|runtime| Runtime::Wasm(Box::new(Bound(runtime))))
                .map_err(|e| e.to_string())
        },
    );
    (engine, PresentMode::Shm)
}

#[cfg(feature = "engine-wasm")]
impl Running<TargetLease, FramePermit> for Bound<sighurt_engine_wasm::WasmRuntime<TargetLease>> {}

#[cfg(feature = "engine-wpe")]
fn wpe() -> (Engine<TargetLease, FramePermit>, PresentMode) {
    use sighurt_engine_wpe::{WpeRuntime, CAPABILITIES};
    let engine = Engine::new(
        "wpe",
        CAPABILITIES,
        // A native target: WebKit's frames are attached by the engine, not painted by the host.
        "WebKit: its DMA-BUFs straight into the page's subsurface",
        &["user-agent", "storage"],
        |waker, launch| {
            // shell.nix sets them; without them https: and media fail inside WebKit.
            for variable in ["GIO_EXTRA_MODULES", "GST_PLUGIN_SYSTEM_PATH_1_0"] {
                if std::env::var_os(variable).is_none() {
                    log::warn!("wpe: {variable} is not set: run sig in the repository's nix-shell");
                }
            }
            WpeRuntime::new(registry::wpe_config(launch.settings), waker)
                .map(|runtime| Runtime::Wpe(Box::new(Bound(runtime))))
                .map_err(|e| e.to_string())
        },
    );
    (engine, PresentMode::Gl)
}

#[cfg(feature = "engine-wpe")]
impl Running<TargetLease, FramePermit> for Bound<sighurt_engine_wpe::WpeRuntime> {
    fn renderer(&self) -> Option<(&'static str, Option<String>)> {
        use sighurt_engine_wpe::Transport;
        let diagnostics = self.0.diagnostics();
        let mode = match diagnostics.transport? {
            Transport::Dmabuf => "hardware",
            Transport::Shm => "fallback",
        };
        Some((mode, diagnostics.renderer()))
    }

    fn readbacks(&self) -> u64 {
        self.0.diagnostics().present.shm_copies
    }
}

#[cfg(feature = "engine-demo")]
fn demo() -> (Engine<TargetLease, FramePermit>, PresentMode) {
    use sighurt_engine_demo::{DemoRuntime, CAPABILITIES, NAME};
    let engine = Engine::new(NAME, CAPABILITIES, "cpu, wl_shm", &[], |_waker, _launch| {
        Ok(Runtime::Demo(Box::new(Bound(DemoRuntime::new()))))
    });
    (engine, PresentMode::Shm)
}

#[cfg(feature = "engine-demo")]
impl Running<TargetLease, FramePermit> for Bound<sighurt_engine_demo::DemoRuntime> {}

#[cfg(test)]
mod tests {
    use sighurt_core::config::Config;
    use sighurt_ui::Colors;

    use super::*;
    use crate::chrome;

    #[test]
    fn the_core_learns_what_each_engine_honors() {
        let (registry, _) = build();
        let env = registry.environment(chrome::checks_all(), &Colors::NAMES);
        assert_eq!(
            env.engine_settings.keys().collect::<Vec<_>>(),
            env.engines.keys().collect::<Vec<_>>()
        );
        // A valid value of each setting the core knows; an engine honoring another fails here.
        let valid = |name: &str| match name {
            "user-agent" => "\"Sighurt\"",
            "storage" => "\"ephemeral\"",
            other => panic!("no valid value of {other} here"),
        };
        for engine in registry.engines() {
            let settings: String = engine
                .honors
                .iter()
                .map(|name| format!("{name} = {}\n", valid(name)))
                .collect();
            let text = format!("version = 1\n[engines.{}]\n{settings}", engine.name);
            let config = Config::parse(Some(&text), &env);
            assert!(config.is_ok(), "{}: {config:?}", engine.name);
        }
        #[cfg(feature = "engine-servo")]
        assert_eq!(env.engine_settings["servo"], ["user-agent", "storage"]);
        #[cfg(feature = "engine-wasm")]
        assert_eq!(env.engine_settings["wasm"], ["storage"]);
        #[cfg(feature = "engine-wpe")]
        assert_eq!(env.engine_settings["wpe"], ["user-agent", "storage"]);
        #[cfg(feature = "engine-demo")]
        assert!(env.engine_settings["demo"].is_empty());
    }
}
