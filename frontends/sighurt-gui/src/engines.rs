//! Binding the compiled-in engines to the Wayland host's targets.
//!
//! The runtime lists each engine's name, capabilities and settings. This module supplies what
//! is the Wayland frontend's: how to start the runtime on the host's target lease and frame
//! permit, the present mode its targets are created with, and the renderer label. Shutdown
//! order is the runtime's descriptor order (Servo, then Wasm, then WPE, then the demo, then NetSurf).
//!
//! Must not own: when a runtime starts (the registry, on first use, except Servo at launch) or
//! what it is asked to do.

use sighurt_runtime::registry::{Binding, Registry};
use sighurt_wayland::{FramePermit, PresentMode, TargetLease};

/// How one engine's views present on the host, and the binding the runtime starts it with.
struct WaylandEngine {
    binding: Binding<TargetLease, FramePermit>,
    mode: PresentMode,
}

/// The engines of this build, none started, and how each one's views present.
pub fn build() -> (
    Registry<TargetLease, FramePermit>,
    Vec<(&'static str, PresentMode)>,
) {
    let engines: Vec<WaylandEngine> = vec![
        #[cfg(feature = "engine-servo")]
        WaylandEngine {
            binding: Binding {
                name: "servo",
                renderer: "gl, straight into the page's subsurface",
                start: servo,
            },
            mode: PresentMode::Gl,
        },
        #[cfg(feature = "engine-wasm")]
        WaylandEngine {
            binding: Binding {
                name: "wasm",
                renderer: "cpu (tiny-skia), wl_shm",
                start: wasm,
            },
            mode: PresentMode::Shm,
        },
        #[cfg(feature = "engine-wpe")]
        WaylandEngine {
            binding: Binding {
                name: "wpe",
                renderer: "WebKit: its DMA-BUFs straight into the page's subsurface",
                start: wpe,
            },
            mode: PresentMode::Gl,
        },
        #[cfg(feature = "engine-demo")]
        WaylandEngine {
            binding: Binding {
                name: "demo",
                renderer: "cpu, wl_shm",
                start: demo,
            },
            mode: PresentMode::Shm,
        },
        #[cfg(feature = "engine-netsurf")]
        WaylandEngine {
            binding: Binding {
                name: "netsurf",
                renderer: "cpu (NetSurf), wl_shm",
                start: netsurf,
            },
            mode: PresentMode::Shm,
        },
    ];
    let modes = engines
        .iter()
        .map(|engine| (engine.binding.name, engine.mode))
        .collect();
    let bindings = engines.into_iter().map(|engine| engine.binding).collect();
    (Registry::bind(bindings), modes)
}

/// One engine's runtime, bound to the host's target and permit, so it can sit in the registry's
/// `Runtime`.
#[cfg(any(
    feature = "engine-servo",
    feature = "engine-wasm",
    feature = "engine-wpe",
    feature = "engine-demo",
    feature = "engine-netsurf"
))]
struct Bound<R>(R);

#[cfg(any(
    feature = "engine-servo",
    feature = "engine-wasm",
    feature = "engine-wpe",
    feature = "engine-demo",
    feature = "engine-netsurf"
))]
impl<R> sighurt_api::engine::EngineRuntime for Bound<R>
where
    R: sighurt_api::engine::EngineRuntime<Target = TargetLease, FramePermit = FramePermit>,
{
    type Target = TargetLease;
    type FramePermit = FramePermit;

    fn capabilities(&self) -> sighurt_api::engine::EngineCapabilities {
        self.0.capabilities()
    }

    fn open_view(
        &mut self,
        view: sighurt_api::ids::ViewId,
        options: sighurt_api::engine::ViewOptions,
        target: Self::Target,
    ) -> Result<(), sighurt_api::engine::EngineError> {
        self.0.open_view(view, options, target)
    }

    fn command(
        &mut self,
        view: sighurt_api::ids::ViewId,
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

    fn site_data(
        &mut self,
        request: sighurt_api::engine::SiteDataRequest,
    ) -> Result<(), sighurt_api::engine::EngineError> {
        self.0.site_data(request)
    }

    fn present(
        &mut self,
        view: sighurt_api::ids::ViewId,
        permit: Self::FramePermit,
    ) -> Result<sighurt_api::engine::PresentOutcome, sighurt_api::engine::EngineError> {
        self.0.present(view, permit)
    }

    fn close_view(&mut self, view: sighurt_api::ids::ViewId) {
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
) -> Result<sighurt_runtime::registry::Runtime<TargetLease, FramePermit>, String> {
    use sighurt_engine_servo::ServoRuntime;
    use sighurt_runtime::registry::{self, Runtime};

    let runtime = ServoRuntime::new(registry::servo_config(launch.settings), waker);
    // The first caller installs Servo's logger; a later start logs through whatever was
    // installed at launch (decision 26).
    if !registry::claim_logger() {
        runtime.setup_logging();
    } else {
        log::warn!("Servo started after the logger: its script-thread panics are not caught");
    }
    Ok(Runtime::Servo(Box::new(Bound(runtime))))
}

#[cfg(feature = "engine-servo")]
impl sighurt_runtime::registry::Running<TargetLease, FramePermit>
    for Bound<sighurt_engine_servo::ServoRuntime>
{
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
fn wasm(
    waker: sighurt_api::engine::EngineWaker,
    launch: sighurt_runtime::registry::Launch<'_>,
) -> Result<sighurt_runtime::registry::Runtime<TargetLease, FramePermit>, String> {
    use sighurt_engine_wasm::WasmRuntime;
    use sighurt_runtime::registry::{self, Runtime};

    WasmRuntime::<TargetLease>::new(registry::wasm_config(launch)?, waker)
        .map(|runtime| Runtime::Wasm(Box::new(Bound(runtime))))
        .map_err(|e| e.to_string())
}

#[cfg(feature = "engine-wasm")]
impl sighurt_runtime::registry::Running<TargetLease, FramePermit>
    for Bound<sighurt_engine_wasm::WasmRuntime<TargetLease>>
{
}

#[cfg(feature = "engine-wpe")]
fn wpe(
    waker: sighurt_api::engine::EngineWaker,
    launch: sighurt_runtime::registry::Launch<'_>,
) -> Result<sighurt_runtime::registry::Runtime<TargetLease, FramePermit>, String> {
    use sighurt_engine_wpe::WpeRuntime;
    use sighurt_runtime::registry::{self, Runtime};

    // shell.nix sets them; without them https: and media fail inside WebKit.
    for variable in ["GIO_EXTRA_MODULES", "GST_PLUGIN_SYSTEM_PATH_1_0"] {
        if std::env::var_os(variable).is_none() {
            log::warn!("wpe: {variable} is not set: run sig in the repository's nix-shell");
        }
    }
    WpeRuntime::new(registry::wpe_config(launch.settings), waker)
        .map(|runtime| Runtime::Wpe(Box::new(Bound(runtime))))
        .map_err(|e| e.to_string())
}

#[cfg(feature = "engine-wpe")]
impl sighurt_runtime::registry::Running<TargetLease, FramePermit>
    for Bound<sighurt_engine_wpe::WpeRuntime>
{
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
fn demo(
    _waker: sighurt_api::engine::EngineWaker,
    _launch: sighurt_runtime::registry::Launch<'_>,
) -> Result<sighurt_runtime::registry::Runtime<TargetLease, FramePermit>, String> {
    use sighurt_engine_demo::DemoRuntime;
    use sighurt_runtime::registry::Runtime;

    Ok(Runtime::Demo(Box::new(Bound(
        DemoRuntime::<TargetLease>::new(),
    ))))
}

#[cfg(feature = "engine-demo")]
impl sighurt_runtime::registry::Running<TargetLease, FramePermit>
    for Bound<sighurt_engine_demo::DemoRuntime<TargetLease>>
{
}

#[cfg(feature = "engine-netsurf")]
fn netsurf(
    waker: sighurt_api::engine::EngineWaker,
    launch: sighurt_runtime::registry::Launch<'_>,
) -> Result<sighurt_runtime::registry::Runtime<TargetLease, FramePermit>, String> {
    use sighurt_engine_netsurf::NetSurfRuntime;
    use sighurt_runtime::registry::{self, Runtime};

    NetSurfRuntime::<TargetLease>::new(registry::netsurf_config(launch.settings), waker)
        .map(|runtime| Runtime::Netsurf(Box::new(Bound(runtime))))
        .map_err(|e| e.to_string())
}

#[cfg(feature = "engine-netsurf")]
impl sighurt_runtime::registry::Running<TargetLease, FramePermit>
    for Bound<sighurt_engine_netsurf::NetSurfRuntime<TargetLease>>
{
}
