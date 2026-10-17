//! Binding the compiled-in engines to the terminal's CPU target.
//!
//! The runtime lists each engine's name, capabilities and settings. This module supplies how
//! to start the runtime on [`TermTarget`] and [`TermPermit`]. Servo renders offscreen and reads
//! one frame back into that target. Wasm and the demo engine paint straight into it. WPE WebKit
//! is bound so the registry's list stays complete, and its start fails: the terminal has no
//! presenter for it yet, and a page routed there shows the core's error.
//!
//! Must not own: when a runtime starts (the registry, on first use, except Servo at launch) or
//! what it is asked to do.

#[cfg(any(
    feature = "engine-servo",
    feature = "engine-wasm",
    feature = "engine-demo"
))]
use sighurt_api::engine::EngineRuntime;
#[cfg(any(
    feature = "engine-servo",
    feature = "engine-wasm",
    feature = "engine-demo"
))]
use sighurt_api::ids::ViewId;
use sighurt_runtime::registry::{Binding, Registry};

use crate::target::{TermPermit, TermTarget};

/// The engines of this build, none started.
pub fn build() -> Registry<TermTarget, TermPermit> {
    let bindings: Vec<Binding<TermTarget, TermPermit>> = vec![
        #[cfg(feature = "engine-servo")]
        Binding {
            name: "servo",
            renderer: "offscreen, kitty graphics",
            start: servo,
        },
        #[cfg(feature = "engine-wasm")]
        Binding {
            name: "wasm",
            renderer: "cpu, kitty graphics",
            start: wasm,
        },
        #[cfg(feature = "engine-wpe")]
        Binding {
            name: "wpe",
            renderer: "unavailable in the terminal",
            start: wpe,
        },
        #[cfg(feature = "engine-demo")]
        Binding {
            name: "demo",
            renderer: "cpu, kitty graphics",
            start: demo,
        },
    ];
    Registry::bind(bindings)
}

/// One engine's runtime, bound to [`TermTarget`].
#[cfg(any(feature = "engine-wasm", feature = "engine-demo"))]
struct Bound<R>(R);

#[cfg(any(feature = "engine-wasm", feature = "engine-demo"))]
impl<R> EngineRuntime for Bound<R>
where
    R: EngineRuntime<Target = TermTarget, FramePermit = TermPermit>,
{
    type Target = TermTarget;
    type FramePermit = TermPermit;

    fn capabilities(&self) -> sighurt_api::engine::EngineCapabilities {
        self.0.capabilities()
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: sighurt_api::engine::ViewOptions,
        target: Self::Target,
    ) -> Result<(), sighurt_api::engine::EngineError> {
        self.0.open_view(view, options, target)
    }

    fn command(
        &mut self,
        view: ViewId,
        command: sighurt_api::engine::EngineCommand,
    ) -> Result<(), sighurt_api::engine::EngineError> {
        self.0.command(view, command)
    }

    fn pump(
        &mut self,
        events: &mut Vec<sighurt_api::events::EngineEvent>,
    ) -> sighurt_api::engine::PumpStatus {
        self.0.pump(events)
    }

    fn present(
        &mut self,
        view: ViewId,
        permit: Self::FramePermit,
    ) -> Result<sighurt_api::engine::PresentOutcome, sighurt_api::engine::EngineError> {
        self.0.present(view, permit)
    }

    fn close_view(&mut self, view: ViewId) {
        self.0.close_view(view);
    }

    fn shutdown(&mut self) {
        self.0.shutdown();
    }
}

/// Servo's runtime wants `Offscreen<TermTarget>` as its target. The session's target is
/// [`TermTarget`], so this wrapper puts the offscreen framebuffer around it at `open_view`.
#[cfg(feature = "engine-servo")]
struct ServoBound(sighurt_engine_servo::ServoRuntime<sighurt_engine_servo::Offscreen<TermTarget>>);

#[cfg(feature = "engine-servo")]
impl EngineRuntime for ServoBound {
    type Target = TermTarget;
    type FramePermit = TermPermit;

    fn capabilities(&self) -> sighurt_api::engine::EngineCapabilities {
        self.0.capabilities()
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: sighurt_api::engine::ViewOptions,
        target: Self::Target,
    ) -> Result<(), sighurt_api::engine::EngineError> {
        self.0
            .open_view(view, options, sighurt_engine_servo::Offscreen(target))
    }

    fn command(
        &mut self,
        view: ViewId,
        command: sighurt_api::engine::EngineCommand,
    ) -> Result<(), sighurt_api::engine::EngineError> {
        self.0.command(view, command)
    }

    fn pump(
        &mut self,
        events: &mut Vec<sighurt_api::events::EngineEvent>,
    ) -> sighurt_api::engine::PumpStatus {
        self.0.pump(events)
    }

    fn present(
        &mut self,
        view: ViewId,
        permit: Self::FramePermit,
    ) -> Result<sighurt_api::engine::PresentOutcome, sighurt_api::engine::EngineError> {
        self.0.present(view, permit)
    }

    fn close_view(&mut self, view: ViewId) {
        self.0.close_view(view);
    }

    fn shutdown(&mut self) {
        self.0.shutdown();
    }
}

#[cfg(feature = "engine-servo")]
fn servo(
    waker: sighurt_api::engine::EngineWaker,
    launch: sighurt_runtime::registry::Launch<'_>,
) -> Result<sighurt_runtime::registry::Runtime<TermTarget, TermPermit>, String> {
    use sighurt_engine_servo::ServoRuntime;
    use sighurt_runtime::registry::{self, Runtime};

    let runtime = ServoRuntime::new(registry::servo_config(launch.settings), waker);
    // The first caller installs Servo's logger; a later start logs through whatever was
    // installed at launch (decision 26). Stderr is already the log file.
    if !registry::claim_logger() {
        runtime.setup_logging();
    } else {
        log::warn!("Servo started after the logger: its script-thread panics are not caught");
    }
    Ok(Runtime::Servo(Box::new(ServoBound(runtime))))
}

#[cfg(feature = "engine-servo")]
impl sighurt_runtime::registry::Running<TermTarget, TermPermit> for ServoBound {
    fn renderer(&self) -> Option<(&'static str, Option<String>)> {
        use sighurt_engine_servo::OffscreenAdapter;
        let diagnostics = self.0.diagnostics();
        let mode = match diagnostics.offscreen_adapter {
            Some(OffscreenAdapter::Hardware) => "hardware",
            Some(OffscreenAdapter::Software) => "software",
            None => match diagnostics.gl {
                Some(ref gl) if gl.software => "software",
                Some(_) => "hardware",
                None => return None,
            },
        };
        Some((mode, diagnostics.gl.map(|gl| gl.renderer)))
    }

    fn readbacks(&self) -> u64 {
        let diagnostics = self.0.diagnostics();
        diagnostics.read_to_image + diagnostics.gl_reads.unwrap_or(0)
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
fn wasm(
    waker: sighurt_api::engine::EngineWaker,
    launch: sighurt_runtime::registry::Launch<'_>,
) -> Result<sighurt_runtime::registry::Runtime<TermTarget, TermPermit>, String> {
    use sighurt_engine_wasm::WasmRuntime;
    use sighurt_runtime::registry::{self, Runtime};

    WasmRuntime::<TermTarget>::new(registry::wasm_config(launch)?, waker)
        .map(|runtime| Runtime::Wasm(Box::new(Bound(runtime))))
        .map_err(|error| error.to_string())
}

#[cfg(feature = "engine-wasm")]
impl sighurt_runtime::registry::Running<TermTarget, TermPermit>
    for Bound<sighurt_engine_wasm::WasmRuntime<TermTarget>>
{
}

#[cfg(feature = "engine-wpe")]
fn wpe(
    _waker: sighurt_api::engine::EngineWaker,
    _launch: sighurt_runtime::registry::Launch<'_>,
) -> Result<sighurt_runtime::registry::Runtime<TermTarget, TermPermit>, String> {
    Err("WPE WebKit isn't available in the terminal yet".into())
}

#[cfg(feature = "engine-demo")]
fn demo(
    _waker: sighurt_api::engine::EngineWaker,
    _launch: sighurt_runtime::registry::Launch<'_>,
) -> Result<sighurt_runtime::registry::Runtime<TermTarget, TermPermit>, String> {
    use sighurt_engine_demo::DemoRuntime;
    use sighurt_runtime::registry::Runtime;

    Ok(Runtime::Demo(Box::new(Bound(
        DemoRuntime::<TermTarget>::new(),
    ))))
}

#[cfg(feature = "engine-demo")]
impl sighurt_runtime::registry::Running<TermTarget, TermPermit>
    for Bound<sighurt_engine_demo::DemoRuntime<TermTarget>>
{
}
