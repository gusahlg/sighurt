//! The Wayland side: one window's surface graph, its layout transaction, input routing and
//! presentation scheduling.
//!
//! ```text
//! root wl_surface (xdg_toplevel)   opaque background, 1×1 SHM buffer scaled by a viewport
//! ├── chrome wl_subsurface         CPU strip, double-buffered SHM, committed by the host
//! └── page wl_subsurface           Servo's EGL window surface, committed by eglSwapBuffers
//! ```
//!
//! Both children are desynchronized, so the chrome and the page update independently. A layout
//! change makes them synchronized for one transaction, which the root commit applies at once
//! (§10, "Subsurface commit policy").

use std::collections::HashSet;
use std::time::{Duration, Instant};

use servo::KeyState;
use sighurt_api::geometry::{BufferSize, LogicalSize, Metrics, Scale, SurfacePoint, WindowRect};
use sighurt_api::ids::LayoutEpoch;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{self, KeyEvent, KeyboardHandler, Keysym};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shell::xdg::{XdgShell, XdgSurface};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::subcompositor::SubcompositorState;
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_subcompositor, delegate_xdg_shell, delegate_xdg_window,
    registry_handlers,
};
use url::Url;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_keyboard::WlKeyboard;
use wayland_client::protocol::wl_output::{Transform, WlOutput};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::chrome::{self, Chrome};
use crate::log;
use crate::pacing::FramePacer;
use crate::page::{typed_text, Page};
use crate::shm::{BufferKey, Owner, ShmBuffers};
use crate::stats::Stats;

/// The window's size before the compositor suggests one, and the smallest it lays out.
const DEFAULT_SIZE: LogicalSize = LogicalSize {
    width: 1024,
    height: 700,
};
const MIN_SIZE: LogicalSize = LogicalSize {
    width: 240,
    height: chrome::HEIGHT + 80,
};
const ROOT_COLOR: [u8; 4] = [0x40, 0x30, 0x30, 0xff]; // B G R X: a dark blue-grey

pub struct Options {
    /// The page to load; `None` runs the chrome alone.
    pub url: Option<Url>,
    /// Ignore fractional-scale-v1 and wp_viewporter: the integer-scale fallback.
    pub integer_scale: bool,
    pub audit_gl: bool,
    /// Ask for exactly this size (minimum and maximum), which tiling window managers usually
    /// honour by floating the window.
    pub fixed_size: Option<LogicalSize>,
    /// How far back the report counts the loop's wakeups before exit.
    pub idle_window: Duration,
}

/// Which part of the window gets the keyboard: the logical focus, separate from whether the
/// compositor gave the window keyboard focus (§12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Page,
    Chrome,
}

/// Why the chrome needs painting.
#[derive(Clone, Copy, Debug)]
enum ChromeCause {
    Layout,
    Focus,
    Text,
}

/// Why a page frame was presented.
#[derive(Clone, Copy, Debug)]
enum FrameCause {
    New,
    Layout,
    Remap,
}

/// A page frame callback's user data: which presented frame it belongs to.
pub struct PageFrame(u64);

/// The page subsurface. `role` is `None` while the page is hidden: destroying the
/// `wl_subsurface` unmaps the surface without touching its buffer state, which belongs to the
/// driver.
struct PageTarget {
    surface: WlSurface,
    role: Option<WlSubsurface>,
    viewport: Option<WpViewport>,
}

pub struct App {
    connection: Connection,
    qh: QueueHandle<App>,
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    compositor: CompositorState,
    subcompositor: SubcompositorState,
    shm: Shm,
    window: Window,
    root_viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    chrome_surface: WlSurface,
    chrome_role: WlSubsurface,
    chrome_viewport: Option<WpViewport>,
    page_target: Option<PageTarget>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    modifiers: keyboard::Modifiers,

    /// The latest configured size and preferred scale; applied by the next layout.
    size: Option<LogicalSize>,
    scale: Scale,
    layout_dirty: bool,
    chrome_metrics: Option<Metrics>,
    page_metrics: Option<Metrics>,
    root_buffers: ShmBuffers,
    root_buffer_size: Option<BufferSize>,
    /// The root's new size waits for the release of an old root buffer.
    root_pending: bool,
    chrome_buffers: ShmBuffers,
    chrome: Chrome,
    chrome_dirty: Option<ChromeCause>,

    page_url: Option<Url>,
    page: Option<Page>,
    ping: calloop::ping::Ping,
    audit_gl: bool,
    /// Whether the page surface has a buffer (the page presented at least once).
    page_presented: bool,
    pacer: FramePacer,

    focus: Focus,
    /// Whether the compositor gave the window keyboard focus.
    activated: bool,
    /// Keys consumed by browser shortcuts, whose releases are swallowed too.
    consumed: HashSet<u32>,

    /// Whether the ordered release ran (see [`App::release`]).
    released: bool,
    pub quit: bool,
    pub error: Option<String>,
    pub stats: Stats,
}

impl App {
    pub fn new(
        connection: Connection,
        globals: &GlobalList,
        qh: QueueHandle<App>,
        options: Options,
        ping: calloop::ping::Ping,
    ) -> Result<Self, String> {
        let required = |what: &str, e: &dyn std::fmt::Display| format!("{what} is required: {e}");
        let compositor =
            CompositorState::bind(globals, &qh).map_err(|e| required("wl_compositor", &e))?;
        let subcompositor =
            SubcompositorState::bind(compositor.wl_compositor().clone(), globals, &qh)
                .map_err(|e| required("wl_subcompositor", &e))?;
        let xdg = XdgShell::bind(globals, &qh).map_err(|e| required("xdg_wm_base", &e))?;
        let shm = Shm::bind(globals, &qh).map_err(|e| required("wl_shm", &e))?;
        let viewporter: Option<WpViewporter> = match options.integer_scale {
            true => None,
            false => globals.bind(&qh, 1..=1, ()).ok(),
        };
        // Fractional scales need a viewport to set the surface size.
        let fractional_manager: Option<WpFractionalScaleManagerV1> = match viewporter {
            Some(_) => globals.bind(&qh, 1..=1, ()).ok(),
            None => None,
        };
        log!(
            "wayland: wl_compositor v{}, wp_viewporter {}, wp_fractional_scale_v1 {}{}",
            compositor.wl_compositor().version(),
            yes_no(viewporter.is_some()),
            yes_no(fractional_manager.is_some()),
            if options.integer_scale {
                " (ignored: --integer-scale)"
            } else {
                ""
            }
        );

        let root = compositor.create_surface(&qh);
        let window = xdg.create_window(root, WindowDecorations::RequestServer, &qh);
        window.set_title("m1-mixed");
        window.set_app_id("sighurt-m1");
        match options.fixed_size {
            Some(size) => {
                window.set_min_size(Some((size.width, size.height)));
                window.set_max_size(Some((size.width, size.height)));
            }
            None => window.set_min_size(Some((MIN_SIZE.width, MIN_SIZE.height))),
        }
        let viewport = |surface: &WlSurface| {
            viewporter
                .as_ref()
                .map(|viewporter| viewporter.get_viewport(surface, &qh, ()))
        };
        let root_viewport = viewport(window.wl_surface());
        let fractional = fractional_manager
            .as_ref()
            .map(|manager| manager.get_fractional_scale(window.wl_surface(), &qh, ()));

        let (chrome_role, chrome_surface) =
            subcompositor.create_subsurface(window.wl_surface().clone(), &qh);
        chrome_role.set_position(0, 0);
        chrome_role.set_desync();
        let chrome_viewport = viewport(&chrome_surface);
        let page_target = options.url.as_ref().map(|_| {
            let (role, surface) = subcompositor.create_subsurface(window.wl_surface().clone(), &qh);
            role.set_position(0, chrome::HEIGHT as i32);
            role.set_desync();
            let viewport = viewport(&surface);
            PageTarget {
                surface,
                role: Some(role),
                viewport,
            }
        });
        // The first commit, without a buffer, asks for the first configure.
        window.commit();

        let chrome_text = match &options.url {
            Some(url) => url.to_string(),
            None => "chrome only: no page".into(),
        };
        Ok(Self {
            registry: RegistryState::new(globals),
            seats: SeatState::new(globals, &qh),
            outputs: OutputState::new(globals, &qh),
            connection,
            qh,
            compositor,
            subcompositor,
            shm,
            window,
            root_viewport,
            fractional,
            chrome_surface,
            chrome_role,
            chrome_viewport,
            page_target,
            keyboard: None,
            pointer: None,
            modifiers: keyboard::Modifiers::default(),
            size: options.fixed_size,
            scale: Scale::ONE,
            layout_dirty: false,
            chrome_metrics: None,
            page_metrics: None,
            root_buffers: ShmBuffers::new(Owner::Root, 1),
            root_buffer_size: None,
            root_pending: false,
            chrome_buffers: ShmBuffers::new(Owner::Chrome, 2),
            chrome: Chrome::new(chrome_text),
            chrome_dirty: None,
            page_url: options.url,
            page: None,
            ping,
            audit_gl: options.audit_gl,
            page_presented: false,
            pacer: FramePacer::default(),
            focus: Focus::Page,
            activated: false,
            consumed: HashSet::new(),
            released: false,
            quit: false,
            error: None,
            stats: Stats::new(options.idle_window),
        })
    }

    /// One loop turn after the loop woke up: apply the latest layout, pump Servo if it asked or
    /// was called, then present whatever is due. Nothing here runs on a timer.
    pub fn turn(&mut self) {
        self.stats.woke();
        if self.layout_dirty {
            self.apply_layout();
        } else if self.root_pending {
            self.paint_root();
            if !self.root_pending {
                self.window.commit();
            }
        }
        if let Some(page) = &mut self.page {
            if page.take_work() {
                self.stats.servo_spin.time(|| page.spin());
            }
        }
        if self.chrome_dirty.is_some() {
            self.paint_chrome();
        }
        self.present_page();
    }

    /// Hides or shows the page (F2 and `--hide-cycles`).
    pub fn toggle_page(&mut self) {
        let shown = self.page_shown();
        self.set_page_shown(!shown);
    }

    /// Readbacks so far: `read_to_image` calls and, when audited, GL read calls.
    pub fn readbacks(&self) -> Option<(u64, Option<u64>)> {
        self.page.as_ref().map(Page::readbacks)
    }

    /// The SHM report line: the most old-size buffers awaiting release at once, and how often
    /// a new size was refused because too many were, for the chrome and the root.
    pub fn shm_report(&self) -> String {
        let (chrome, root) = (&self.chrome_buffers, &self.root_buffers);
        format!(
            "at most {} chrome and {} root buffers of old sizes awaited release at once; a new \
             size was refused until a release {} and {} times",
            chrome.max_retiring, root.max_retiring, chrome.size_waits, root.size_waits
        )
    }

    /// Tears the window down: [`release`](Self::release), then the root, then a round trip
    /// that surfaces any protocol error the teardown caused.
    pub fn teardown(mut self) -> Result<(), String> {
        let page = self.release();
        let connection = self.connection.clone();
        // What `release` left: the xdg_toplevel, the xdg_surface and the root wl_surface.
        drop(self);
        connection
            .roundtrip()
            .map_err(|e| format!("teardown: {e}"))?;
        log!("teardown: chrome and window destroyed; the compositor reported no error");
        page
    }

    /// Destroys everything but the root, once, in the order §9 and §20 need: the page's
    /// Servo, GL context and EGL display before its `wl_subsurface` and `wl_surface`, children
    /// before the root. [`teardown`](Self::teardown) calls it, and so does `Drop` when the loop
    /// ended with an error or a panic, so no way out relies on the fields' drop order; the
    /// window, which `Drop` leaves to the fields, is the last thing destroyed.
    fn release(&mut self) -> Result<(), String> {
        if std::mem::replace(&mut self.released, true) {
            return Ok(());
        }
        if let Some(keyboard) = self.keyboard.take() {
            keyboard.release();
        }
        if let Some(pointer) = self.pointer.take() {
            pointer.release();
        }
        let page = match self.page.take() {
            Some(page) => page.shutdown(),
            None => Ok(()),
        };
        if let Some(target) = self.page_target.take() {
            if let Err(e) = &page {
                // Destroying the surface under a live EGL window surface would be a
                // use-after-free in the driver; leave it to the connection.
                log!("teardown: {e}");
            } else {
                if let Some(role) = target.role {
                    role.destroy();
                }
                if let Some(viewport) = target.viewport {
                    viewport.destroy();
                }
                target.surface.destroy();
                log!("teardown: page wl_subsurface and wl_surface destroyed");
            }
        }
        self.chrome_buffers.destroy();
        if let Some(viewport) = self.chrome_viewport.take() {
            viewport.destroy();
        }
        self.chrome_role.destroy();
        self.chrome_surface.destroy();
        self.root_buffers.destroy();
        if let Some(viewport) = self.root_viewport.take() {
            viewport.destroy();
        }
        if let Some(fractional) = self.fractional.take() {
            fractional.destroy();
        }
        page
    }

    fn page_shown(&self) -> bool {
        self.page_target
            .as_ref()
            .is_some_and(|target| target.role.is_some())
    }

    /// The layout transaction (§10): new metrics for every surface under a new epoch, children
    /// synchronized so the root commit shows the new root, chrome and page together.
    fn apply_layout(&mut self) {
        self.layout_dirty = false;
        let Some(size) = self.size else { return };
        let epoch = LayoutEpoch::next();
        let chrome_rect = WindowRect {
            x: 0,
            y: 0,
            width: size.width,
            height: chrome::HEIGHT,
        };
        let page_rect = WindowRect {
            x: 0,
            y: chrome::HEIGHT as i32,
            width: size.width,
            height: size.height - chrome::HEIGHT,
        };
        let chrome = Metrics::new(epoch, chrome_rect, self.scale);
        let page = Metrics::new(epoch, page_rect, self.scale);
        let unchanged = self
            .page_metrics
            .is_some_and(|old| (old.rect(), old.scale()) == (page.rect(), page.scale()));
        if unchanged {
            // Nothing moves, but an acknowledged configure still takes a commit.
            self.window.commit();
            return;
        }
        let start = Instant::now();
        self.chrome_metrics = Some(chrome);
        self.page_metrics = Some(page);

        // 1. Child commits wait for the root's.
        self.chrome_role.set_sync();
        let page_role = self.page_target.as_ref().and_then(|t| t.role.clone());
        if let Some(role) = &page_role {
            role.set_sync();
        }
        // 2. The root background.
        self.paint_root();
        // 3. The chrome, at its new size (cached until the root commit).
        self.chrome_dirty = Some(ChromeCause::Layout);
        self.paint_chrome();
        // 4. The page: its surface size, the engine's metrics, and a frame at the new size.
        if let Some(target) = &self.page_target {
            if let Some(role) = &target.role {
                role.set_position(page_rect.x, page_rect.y);
            }
            self.set_surface_size(&target.surface, target.viewport.as_ref(), page_rect);
        }
        let present = self.page_presented && page_role.is_some();
        if let Some(view) = &mut self.page {
            view.resize(page.buffer(), page.scale());
            if present {
                self.page_frame(FrameCause::Layout);
            }
        }
        // 5. One root commit shows it all.
        self.window
            .set_window_geometry(0, 0, size.width, size.height);
        self.window.commit();
        // 6. Independent leaf updates again.
        self.chrome_role.set_desync();
        if let Some(role) = &page_role {
            role.set_desync();
        }
        self.stats.layout.push(start.elapsed());
        log!(
            "layout: epoch {:?}, window {}x{} at {}/120, page buffer {}x{}, {:.2} ms{}",
            epoch,
            size.width,
            size.height,
            self.scale.numerator(),
            page.buffer().width,
            page.buffer().height,
            start.elapsed().as_secs_f64() * 1e3,
            if present { ", page frame included" } else { "" }
        );
        if self.page.is_none() && self.page_url.is_some() {
            self.create_page(page);
        }
    }

    /// The surface's logical size: a viewport destination, or the integer buffer scale.
    fn set_surface_size(
        &self,
        surface: &WlSurface,
        viewport: Option<&WpViewport>,
        rect: WindowRect,
    ) {
        match viewport {
            Some(viewport) => viewport.set_destination(rect.width as i32, rect.height as i32),
            None => surface.set_buffer_scale(self.scale.integer().unwrap_or(1) as i32),
        }
    }

    /// The root background at the configured size: with a viewport, one pixel stretched over
    /// the window; without, a whole-window buffer at the integer scale. When a new buffer size
    /// has to wait for a release, the root keeps its old buffer and buffer scale (a new scale
    /// on the old buffer could be an `invalid_size` error) and the next loop turn retries.
    fn paint_root(&mut self) {
        let Some(size) = self.size else { return };
        let rect = WindowRect {
            x: 0,
            y: 0,
            width: size.width,
            height: size.height,
        };
        let surface = self.window.wl_surface().clone();
        let opaque = self.compositor.wl_compositor().create_region(&self.qh, ());
        opaque.add(0, 0, size.width as i32, size.height as i32);
        surface.set_opaque_region(Some(&opaque));
        opaque.destroy();
        let buffer = match self.root_viewport {
            Some(_) => BufferSize {
                width: 1,
                height: 1,
            },
            None => Scale::from_integer(self.scale.integer().unwrap_or(1))
                .unwrap_or(Scale::ONE)
                .buffer_size(rect.size()),
        };
        if self.root_buffer_size != Some(buffer) {
            let Some((slot, pixels)) = self.root_buffers.acquire(&self.shm, buffer, &self.qh)
            else {
                self.root_pending = true;
                return;
            };
            for pixel in pixels.as_chunks_mut::<4>().0 {
                *pixel = ROOT_COLOR;
            }
            self.root_buffers.attach(slot, &surface);
            surface.damage_buffer(0, 0, buffer.width as i32, buffer.height as i32);
            self.root_buffer_size = Some(buffer);
        }
        self.root_pending = false;
        self.set_surface_size(&surface, self.root_viewport.as_ref(), rect);
    }

    /// Paints the whole chrome strip into a free buffer and commits it. With both buffers still
    /// held by the compositor, the invalidation stays and a release retries it.
    fn paint_chrome(&mut self) {
        let (Some(cause), Some(metrics)) = (self.chrome_dirty, self.chrome_metrics) else {
            return;
        };
        let buffer = metrics.buffer();
        let Some((slot, pixels)) = self.chrome_buffers.acquire(&self.shm, buffer, &self.qh) else {
            self.stats.chrome_buffer_waits += 1;
            return;
        };
        let start = Instant::now();
        self.chrome
            .paint(buffer, metrics.scale().factor() as f32, pixels);
        self.chrome_buffers.attach(slot, &self.chrome_surface);
        self.chrome_surface
            .damage_buffer(0, 0, buffer.width as i32, buffer.height as i32);
        self.set_surface_size(
            &self.chrome_surface,
            self.chrome_viewport.as_ref(),
            metrics.rect(),
        );
        self.chrome_surface.commit();
        self.stats.chrome_paint.push(start.elapsed());
        match cause {
            ChromeCause::Layout => self.stats.chrome_paints_layout += 1,
            ChromeCause::Focus => self.stats.chrome_paints_focus += 1,
            ChromeCause::Text => self.stats.chrome_paints_text += 1,
        }
        self.chrome_dirty = None;
    }

    fn create_page(&mut self, metrics: Metrics) {
        let (Some(target), Some(url)) = (&self.page_target, self.page_url.take()) else {
            return;
        };
        let ping = self.ping.clone();
        let start = Instant::now();
        match Page::new(
            &self.connection,
            &target.surface,
            metrics.buffer(),
            metrics.scale(),
            url,
            move || ping.ping(),
            self.audit_gl,
        ) {
            Ok((page, gl)) => {
                log!(
                    "page: Servo and the WebView created in {:.0} ms",
                    start.elapsed().as_secs_f64() * 1e3
                );
                log!(
                    "gl: renderer {:?}, vendor {:?}, version {:?}",
                    gl.renderer,
                    gl.vendor,
                    gl.version
                );
                log!(
                    "egl: vendor {:?}, version {:?}, client APIs {:?}",
                    gl.egl_vendor,
                    gl.egl_version,
                    gl.egl_client_apis
                );
                let scaling = match (&self.fractional, &target.viewport) {
                    (Some(_), _) => "fractional-scale-v1 with wp_viewporter",
                    (None, Some(_)) => "integer scale with wp_viewporter",
                    (None, None) => "integer scale with wl_surface.set_buffer_scale",
                };
                if gl.software {
                    log!(
                        "presentation: SOFTWARE FALLBACK: {:?} rasterizes on the CPU and the \
                         driver copies its frames into buffers for the page subsurface. This is \
                         not the GPU path. Scaling: {scaling}",
                        gl.renderer
                    );
                } else {
                    log!(
                        "presentation: GPU: WebRender draws into an EGL window surface on the page \
                         subsurface and eglSwapBuffers commits it (swap interval 0, paced by the \
                         host's frame callbacks); no readback. Scaling: {scaling}"
                    );
                }
                self.page = Some(page);
                self.update_focus();
            }
            Err(e) => {
                self.error = Some(e);
                self.quit = true;
            }
        }
    }

    /// Presents a page frame when Servo has one and the compositor wants one: the last frame's
    /// callback came back, or no frame is in flight. When Servo waits for a frame to start
    /// (an animation tick) and none is in flight, starts it now rather than waiting for a
    /// callback that nothing will trigger.
    fn present_page(&mut self) {
        if !self.pacer.ready() || !self.page_shown() {
            return;
        }
        let Some(page) = &self.page else { return };
        if page.delegate.frame_ready.replace(false) {
            self.page_frame(FrameCause::New);
        } else if page.driver.is_waiting() {
            page.driver.start_frame();
            self.stats.bootstrap_ticks += 1;
        }
    }

    /// Paints and presents one page frame, asking for its frame callback. The callback request
    /// is sent before the swap, so the driver's commit of the page surface carries it.
    fn page_frame(&mut self, cause: FrameCause) {
        let (Some(page), Some(target)) = (&self.page, &self.page_target) else {
            return;
        };
        // This paints Servo's latest scene, whatever asked for the frame.
        page.delegate.frame_ready.set(false);
        self.stats.page_paint.time(|| page.paint());
        target
            .surface
            .frame(&self.qh, PageFrame(self.pacer.presented()));
        self.stats.page_present.time(|| page.present());
        self.page_presented = true;
        match cause {
            FrameCause::New => self.stats.page_frames_new += 1,
            FrameCause::Layout => self.stats.page_frames_layout += 1,
            FrameCause::Remap => self.stats.page_frames_remap += 1,
        }
    }

    /// Hides the page by destroying its `wl_subsurface`, which unmaps the surface at once
    /// without touching its buffer state (the driver's). Shows it with a new `wl_subsurface`
    /// and a parent commit, then presents a frame at once: nothing waits for the frame
    /// callback that was in flight when it was hidden.
    fn set_page_shown(&mut self, shown: bool) {
        let Some(target) = &mut self.page_target else {
            return;
        };
        if shown == target.role.is_some() {
            return;
        }
        if shown {
            let start = Instant::now();
            let role = self
                .subcompositor
                .subsurface_from_surface(&target.surface, &self.qh)
                .expect("the page surface has a parent");
            role.set_position(0, chrome::HEIGHT as i32);
            role.set_desync();
            target.role = Some(role);
            self.window.commit();
            if let Some(page) = &mut self.page {
                page.set_hidden(false);
            }
            if self.page_presented {
                self.page_frame(FrameCause::Remap);
            }
            self.stats.shows += 1;
            self.stats.remap.push(start.elapsed());
            log!(
                "page: shown: new wl_subsurface, root committed, frame {} presented without \
                 waiting, {:.2} ms",
                self.pacer.last(),
                start.elapsed().as_secs_f64() * 1e3
            );
        } else {
            if let Some(role) = target.role.take() {
                role.destroy();
            }
            if let Some(frame) = self.pacer.abandon() {
                log!("page: hidden with frame {frame}'s callback outstanding; not waiting for it");
            } else {
                log!("page: hidden");
            }
            if let Some(page) = &mut self.page {
                page.set_hidden(true);
            }
            if self.focus == Focus::Page {
                self.focus = Focus::Chrome;
            }
            self.update_focus();
            self.stats.hides += 1;
        }
    }

    /// Recomputes who has the keyboard: the page when it has logical focus, is shown and the
    /// window is activated; the address field likewise.
    fn update_focus(&mut self) {
        let page_focused = self.activated && self.focus == Focus::Page && self.page_shown();
        if let Some(page) = &mut self.page {
            page.set_focus(page_focused);
        }
        let chrome_focused = self.activated && self.focus == Focus::Chrome;
        if self.chrome.focused != chrome_focused {
            self.chrome.focused = chrome_focused;
            self.chrome_dirty = Some(ChromeCause::Focus);
        }
    }

    fn set_focus(&mut self, focus: Focus) {
        if self.focus != focus {
            log!("focus: {focus:?}");
            self.focus = focus;
        }
        self.update_focus();
    }

    fn set_scale(&mut self, scale: Scale, source: &str) {
        if scale != self.scale {
            log!("scale: {}/120 from {source}", scale.numerator());
            self.scale = scale;
            self.layout_dirty = true;
            self.stats.scale_changes += 1;
        }
    }

    /// Routes a key: browser shortcuts first, then whoever has logical focus (§12).
    fn key(&mut self, event: KeyEvent, pressed: bool, repeat: bool) {
        let code = event.raw_code;
        if !pressed {
            if self.consumed.remove(&code) {
                return;
            }
            if let Some(page) = &mut self.page {
                if page.holds(code) {
                    page.key(KeyState::Up, &event, self.modifiers, false);
                }
            }
            return;
        }
        let ctrl = self.modifiers.ctrl;
        let shortcut = match event.keysym {
            Keysym::F2 => Some("page.toggle"),
            Keysym::l | Keysym::L if ctrl => Some("location.focus"),
            Keysym::q | Keysym::Q if ctrl => Some("window.close"),
            Keysym::Escape if self.focus == Focus::Chrome => Some("page.focus"),
            _ => None,
        };
        if let Some(shortcut) = shortcut {
            self.consumed.insert(code);
            if !repeat {
                log!("keyboard: shortcut {shortcut}");
                match shortcut {
                    "page.toggle" => self.toggle_page(),
                    "location.focus" => self.set_focus(Focus::Chrome),
                    "page.focus" => self.set_focus(Focus::Page),
                    _ => self.quit = true,
                }
            }
            return;
        }
        match self.focus {
            Focus::Page => {
                if let Some(page) = &mut self.page {
                    if self.page_target.as_ref().is_some_and(|t| t.role.is_some()) {
                        log!("keyboard: {:?} to the page", event.keysym);
                        page.key(KeyState::Down, &event, self.modifiers, repeat);
                    }
                }
            }
            Focus::Chrome => {
                if event.keysym == Keysym::BackSpace {
                    self.chrome.text.pop();
                } else if let Some(text) = typed_text(&event, self.modifiers) {
                    self.chrome.text.push_str(&text);
                } else {
                    return;
                }
                log!("keyboard: to the chrome, text now {:?}", self.chrome.text);
                self.chrome_dirty = Some(ChromeCause::Text);
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if !self.released {
            log!("teardown: the loop ended early; releasing in order");
            let _ = self.release();
        }
    }
}

fn yes_no(yes: bool) -> &'static str {
    if yes {
        "yes"
    } else {
        "no"
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        log!("window: close requested");
        self.quit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        serial: u32,
    ) {
        let current = self.size.unwrap_or(DEFAULT_SIZE);
        let (width, height) = configure.new_size;
        // Smaller than the minimum is laid out at the minimum; the compositor clips it.
        let size = LogicalSize {
            width: width.map_or(current.width, |w| w.get()).max(MIN_SIZE.width),
            height: height
                .map_or(current.height, |h| h.get())
                .max(MIN_SIZE.height),
        };
        log!(
            "window: configure {serial} (acknowledged): {}x{}{}",
            size.width,
            size.height,
            if configure.is_activated() {
                ", activated"
            } else {
                ""
            }
        );
        self.size = Some(size);
        self.layout_dirty = true;
        self.stats.configures += 1;
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        factor: i32,
    ) {
        // With fractional-scale-v1 the integer factor is only its rounded-up hint.
        if self.fractional.is_none() && surface == self.window.wl_surface() {
            if let Some(scale) = Scale::from_integer(factor.max(1) as u32) {
                self.set_scale(scale, "the integer buffer scale");
            }
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard if self.keyboard.is_none() => {
                self.keyboard = self.seats.get_keyboard(qh, &seat, None).ok();
            }
            Capability::Pointer if self.pointer.is_none() => {
                self.pointer = self.seats.get_pointer(qh, &seat).ok();
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard => {
                if let Some(keyboard) = self.keyboard.take() {
                    keyboard.release();
                }
                self.activated = false;
                self.update_focus();
            }
            Capability::Pointer => {
                if let Some(pointer) = self.pointer.take() {
                    pointer.release();
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        let which = if surface == self.window.wl_surface() {
            "the root surface (the xdg_toplevel)"
        } else {
            "a surface that is not the root"
        };
        log!("keyboard: enter on {which}");
        self.activated = true;
        self.update_focus();
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: &WlSurface,
        _: u32,
    ) {
        log!("keyboard: leave");
        self.activated = false;
        self.consumed.clear();
        self.update_focus();
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(event, true, false);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(event, true, true);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(event, false, false);
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        modifiers: keyboard::Modifiers,
        _: keyboard::RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl PointerHandler for App {
    /// Routes pointer events by the surface Wayland delivered them to. Positions are already
    /// local to that surface: the page's are converted once, by its metrics, to device pixels.
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let local = SurfacePoint {
                x: event.position.0,
                y: event.position.1,
            };
            let on_page = self
                .page_target
                .as_ref()
                .is_some_and(|target| target.surface == event.surface);
            if on_page {
                let Some(metrics) = self.page_metrics else {
                    continue;
                };
                let device = metrics.to_buffer(local);
                match event.kind {
                    PointerEventKind::Press { button, .. } => {
                        log!(
                            "pointer: press on the page at surface ({:.1}, {:.1}), device ({:.1}, \
                             {:.1}) at {}/120",
                            local.x,
                            local.y,
                            device.x,
                            device.y,
                            metrics.scale().numerator()
                        );
                        self.set_focus(Focus::Page);
                        if let Some(page) = &mut self.page {
                            page.pointer_button(device, button, true);
                        }
                    }
                    PointerEventKind::Release { button, .. } => {
                        if let Some(page) = &mut self.page {
                            page.pointer_button(device, button, false);
                        }
                    }
                    PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                        if let Some(page) = &mut self.page {
                            page.pointer_move(device);
                        }
                    }
                    _ => {}
                }
            } else if event.surface == self.chrome_surface {
                if let PointerEventKind::Press { .. } = event.kind {
                    log!(
                        "pointer: press on the chrome at surface ({:.1}, {:.1})",
                        local.x,
                        local.y
                    );
                    self.set_focus(Focus::Chrome);
                }
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WlBuffer, BufferKey> for App {
    fn event(
        app: &mut Self,
        buffer: &WlBuffer,
        event: wl_buffer::Event,
        key: &BufferKey,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            match key.owner {
                Owner::Root => app.root_buffers.release(*key, buffer),
                Owner::Chrome => app.chrome_buffers.release(*key, buffer),
            }
        }
    }
}

impl Dispatch<WlCallback, PageFrame> for App {
    fn event(
        app: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        frame: &PageFrame,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wl_callback::Event::Done {
            callback_data: time,
        } = event
        else {
            return;
        };
        if !app.pacer.done(frame.0) {
            app.stats.ignored_frame_callbacks += 1;
            return;
        }
        if let Some(last) = app.stats.last_frame_time.replace(time) {
            let interval = std::time::Duration::from_millis(u64::from(time.wrapping_sub(last)));
            // Gaps while idle are not frame intervals.
            if interval.as_millis() < 250 {
                app.stats.frame_interval.push(interval);
            }
        }
        // The compositor wants the next frame: start Servo's, if it waits for one.
        if let Some(page) = &app.page {
            page.driver.start_frame();
        }
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if let Some(scale) = Scale::from_fractional(scale) {
                app.set_scale(scale, "wp_fractional_scale_v1");
            }
        }
    }
}

/// Protocol objects without events.
macro_rules! no_events {
    ($($interface:ty),*) => {$(
        impl Dispatch<$interface, ()> for App {
            fn event(_: &mut Self, _: &$interface, _: <$interface as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}

no_events!(
    WpViewporter,
    WpViewport,
    WpFractionalScaleManagerV1,
    wayland_client::protocol::wl_region::WlRegion
);

delegate_compositor!(App);
delegate_subcompositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_keyboard!(App);
delegate_pointer!(App);
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_registry!(App);
