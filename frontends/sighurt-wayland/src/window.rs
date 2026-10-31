//! The root surface of a window: its `xdg_toplevel` role and configure handshake.
//!
//! smithay-client-toolkit acknowledges each configure as it arrives. A configure that names a
//! size is recorded, the window geometry is set to the whole root surface, and
//! [`HostEvent::WindowConfigured`] is reported when that size or the scale changed. A configure
//! that leaves both axes to the client is not a size: the root is committed at the client's
//! default so the window can map, and nothing is placed until a later configure names a size.
//! If none does within [`CLIENT_SIZE_GRACE`], the client default is reported, so a floating
//! window still appears. The application answers with a placement, whose layout transaction
//! commits the root with the new state. A configure that changes neither size nor scale
//! (activation, say) is committed at once, or by the transaction already under way.
//!
//! The root is a stable anchor that stays mapped while the window exists, and shows an opaque
//! background wherever no target covers it: a single-pixel buffer scaled with `wp_viewporter`
//! when both exist, a one-pixel SHM buffer scaled with the viewporter, or, without a viewporter,
//! a window-sized SHM buffer redrawn only when the window's size changes. A page animating never
//! touches it.
//!
//! Decorations are negotiated with xdg-decoration when the compositor offers it (server-side
//! requested). Without it the window is deliberately borderless: Sighurt draws no client-side
//! frame. A close request (the compositor's close button, a key binding) is reported as
//! [`HostEvent::CloseRequested`]; the browser decides whether to close.
//!
//! Must not own: chrome content, or a repaint when a page animates.

use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use calloop::RegistrationToken;
use sighurt_api::geometry::{BufferSize, LogicalSize, Scale};
use sighurt_api::ids::{TargetId, WindowId};
use sighurt_api::input::TextInputState;
use smithay_client_toolkit::compositor::CompositorHandler;
use smithay_client_toolkit::shell::xdg::window::{
    DecorationMode, Window as XdgWindow, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shell::{xdg::XdgSurface, WaylandSurface};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::{Transform, WlOutput};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use crate::connection::{Host, HostEvent, SurfaceOwner, TimerEvent};
use crate::placement::Transaction;
use crate::scale::WindowScale;
use crate::shm::{BufferData, BufferKey, BufferOwner, ShmBuffer, Swapchain};
use crate::targets::Env;

/// How a window starts.
#[derive(Clone, Debug)]
pub struct WindowOptions {
    /// The title the compositor shows.
    pub title: String,
    /// The application id, for the compositor's rules and the desktop entry.
    pub app_id: String,
    /// The opaque background behind all targets, as red, green, blue.
    pub background: [u8; 3],
    /// The size to take when the compositor leaves it to the client.
    pub size: LogicalSize,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            title: "Sighurt".into(),
            app_id: "sighurt".into(),
            background: [0x20, 0x20, 0x24],
            size: LogicalSize {
                width: 1024,
                height: 768,
            },
        }
    }
}

/// How long a sizeless configure may stand before the window is placed at
/// [`WindowOptions::size`]. A tiled compositor names a size within a few milliseconds of the
/// root's mapping commit; a floating one may never name one.
pub(crate) const CLIENT_SIZE_GRACE: Duration = Duration::from_millis(20);

/// What one configure says about the window's size. An axis the compositor left at zero arrives
/// here as `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigureDecision {
    /// Both axes are left to the client. Not a size to paint at.
    Unspecified,
    /// At least one axis is named. A missing axis keeps `fallback`.
    Named(LogicalSize),
}

/// Decides whether `new_size` starts a layout. `fallback` fills an axis the compositor omitted
/// once the other axis is named.
pub(crate) fn decide_configure(
    new_size: (Option<NonZeroU32>, Option<NonZeroU32>),
    fallback: LogicalSize,
) -> ConfigureDecision {
    let (width, height) = new_size;
    if width.is_none() && height.is_none() {
        ConfigureDecision::Unspecified
    } else {
        ConfigureDecision::Named(LogicalSize {
            width: width.map_or(fallback.width, |axis| axis.get()),
            height: height.map_or(fallback.height, |axis| axis.get()),
        })
    }
}

/// The root's background buffer.
enum Background {
    /// A single-pixel buffer, scaled by the root's viewport.
    Pixel { buffer: WlBuffer, attached: bool },
    /// A one-pixel SHM buffer, scaled by the root's viewport.
    Tiny { buffer: ShmBuffer, attached: bool },
    /// Window-sized SHM buffers, without a viewporter.
    Full {
        buffers: Swapchain<ShmBuffer>,
        size: LogicalSize,
    },
}

/// One window: its root surface, and the host's state for it.
pub(crate) struct Window {
    pub id: WindowId,
    pub size: LogicalSize,
    default_size: LogicalSize,
    pub configured: bool,
    /// A configure has named a size, or [`CLIENT_SIZE_GRACE`] has elapsed.
    /// Until then a scale change is stored and reported with the first size, and nothing is placed.
    layout_ready: bool,
    pub scale: WindowScale,
    /// The size and scale last reported as `WindowConfigured`.
    reported: Option<(LogicalSize, Scale)>,
    /// Armed while the first sizeless configure is waiting for a named size.
    fallback_timer: Option<RegistrationToken>,
    pub decorations: DecorationMode,
    /// Live targets, bottom to top once placed.
    pub targets: Vec<TargetId>,
    /// The target that gets keyboard input.
    pub focus: Option<TargetId>,
    /// The editor that wants an input method, and its state.
    pub text_input: Option<(TargetId, TextInputState)>,
    pub transaction: Option<Transaction>,
    /// Whether the children must be restacked at the next root commit.
    pub restack: bool,
    color: [u8; 3],
    fractional: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    background: Background,
    xdg: XdgWindow,
}

impl Window {
    /// The root surface.
    pub fn root(&self) -> &WlSurface {
        self.xdg.wl_surface()
    }

    /// Updates the background for the window's size, as pending root state for the next root
    /// commit.
    pub fn update_background(&mut self, env: &Env, viewporter: bool) {
        let (width, height) = (self.size.width as i32, self.size.height as i32);
        let root = self.xdg.wl_surface().clone();
        match &mut self.background {
            Background::Pixel { buffer, attached } => attach_once(&root, buffer, attached),
            Background::Tiny { buffer, attached } => {
                let wl_buffer = buffer.wl_buffer().clone();
                attach_once(&root, &wl_buffer, attached);
            }
            Background::Full { buffers, size } => {
                if *size == self.size {
                    return;
                }
                let buffer_size = BufferSize {
                    width: self.size.width,
                    height: self.size.height,
                };
                let Ok((.., bytes)) = crate::shm::layout(buffer_size) else {
                    return;
                };
                let owner = BufferOwner::Root(self.id);
                let acquired = buffers.acquire(buffer_size, bytes, |key| {
                    ShmBuffer::new(&env.shm, &env.qh, buffer_size, BufferData { owner, key })
                });
                // Without a free buffer the old one stays until the next resize.
                let Ok((key, mut buffer, _age)) = acquired else {
                    return;
                };
                fill(buffer.pixels(), self.color);
                root.set_buffer_scale(1);
                root.attach(Some(buffer.wl_buffer()), 0, 0);
                root.damage_buffer(0, 0, width, height);
                buffers.submit(key, buffer);
                *size = self.size;
            }
        }
        debug_assert_eq!(self.viewport.is_some(), viewporter);
        if let Some(viewport) = &self.viewport {
            viewport.set_destination(width, height);
        }
    }

    /// The compositor released background buffer `key`.
    pub fn background_released(&mut self, key: BufferKey) {
        if let Background::Full { buffers, .. } = &mut self.background {
            buffers.release(key);
        }
    }
}

fn attach_once(root: &WlSurface, buffer: &WlBuffer, attached: &mut bool) {
    if !*attached {
        root.attach(Some(buffer), 0, 0);
        root.damage_buffer(0, 0, 1, 1);
        *attached = true;
    }
}

/// Fills ARGB8888 `pixels` with an opaque `[r, g, b]`.
fn fill(pixels: &mut [u8], [r, g, b]: [u8; 3]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        *pixel = [b, g, r, 0xff];
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // Objects on the root go before it; the xdg window destroys its role objects and the
        // root surface when it drops, after this.
        if let Some(fractional) = self.fractional.take() {
            fractional.destroy();
        }
        if let Some(viewport) = self.viewport.take() {
            viewport.destroy();
        }
        if let Background::Pixel { buffer, .. } = &self.background {
            buffer.destroy();
        }
    }
}

impl Host {
    /// Opens a window. The root may map at the client size so the compositor can assign one.
    /// Targets are placed only after [`HostEvent::WindowConfigured`]: a configure that names a
    /// size, or the client default if none arrives within [`CLIENT_SIZE_GRACE`].
    pub fn create_window(&mut self, id: WindowId, options: WindowOptions) {
        let surface = self.compositor.create_surface(&self.qh);
        let xdg =
            self.xdg
                .create_window(surface.clone(), WindowDecorations::RequestServer, &self.qh);
        xdg.set_title(options.title);
        xdg.set_app_id(options.app_id);
        let globals = &self.globals;
        let fractional = globals
            .fractional
            .as_ref()
            .map(|m| m.get_fractional_scale(&surface, &self.qh, id));
        let viewport = globals
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(&surface, &self.qh, ()));
        let [r, g, b] = options.background;
        let background = match (&viewport, &globals.single_pixel) {
            (Some(_), Some(manager)) => {
                let channel = |c: u8| u32::from(c) * 0x0101_0101;
                let buffer = manager.create_u32_rgba_buffer(
                    channel(r),
                    channel(g),
                    channel(b),
                    u32::MAX,
                    &self.qh,
                    (),
                );
                Background::Pixel {
                    buffer,
                    attached: false,
                }
            }
            (Some(_), None) => {
                let one = BufferSize {
                    width: 1,
                    height: 1,
                };
                let data = BufferData {
                    owner: BufferOwner::Root(id),
                    key: BufferKey {
                        generation: u64::MAX,
                        index: 0,
                    },
                };
                match ShmBuffer::new(self.shm.wl_shm(), &self.qh, one, data) {
                    Ok(mut buffer) => {
                        fill(buffer.pixels(), options.background);
                        Background::Tiny {
                            buffer,
                            attached: false,
                        }
                    }
                    Err(_) => Background::Full {
                        buffers: Swapchain::new(),
                        size: LogicalSize::default(),
                    },
                }
            }
            (None, _) => Background::Full {
                buffers: Swapchain::new(),
                size: LogicalSize::default(),
            },
        };
        // The initial commit, without a buffer, asks for the first configure.
        xdg.commit();
        self.surfaces.insert(surface.id(), SurfaceOwner::Root(id));
        self.windows.insert(
            id,
            Window {
                id,
                size: LogicalSize::default(),
                default_size: options.size,
                configured: false,
                layout_ready: false,
                scale: WindowScale::default(),
                reported: None,
                fallback_timer: None,
                decorations: DecorationMode::Client,
                targets: Vec::new(),
                focus: None,
                text_input: None,
                transaction: None,
                restack: false,
                color: options.background,
                fractional,
                viewport,
                background,
                xdg,
            },
        );
    }

    /// Closes a window. Its targets are unmapped at once; their surfaces live on until their
    /// leases are dropped, and nothing more is shown on them.
    pub fn close_window(&mut self, id: WindowId) {
        let Some(mut window) = self.windows.remove(&id) else {
            return;
        };
        if let Some(timer) = window.transaction.take().and_then(|tx| tx.timer) {
            self.loop_ops.remove(timer);
        }
        if let Some(timer) = window.fallback_timer.take() {
            self.loop_ops.remove(timer);
        }
        for target in window.targets.iter().filter_map(|t| self.targets.get(t)) {
            let mut target = target.borrow_mut();
            target.unmap();
            target.want_shown = false;
        }
        self.surfaces.remove(&window.root().id());
        self.forget_surface(SurfaceOwner::Root(id));
        self.window_closed(id);
        drop(window);
    }

    /// Reports `window`'s size and scale once a size is known and either has changed; otherwise
    /// makes sure the latest acknowledged configure is committed. A sizeless configure before
    /// the first size is committed by [`Host::map_at_client_size`] and is not reported.
    fn report_configuration(&mut self, id: WindowId) {
        let viewporter = self.globals.viewporter.is_some();
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        if !window.configured || !window.layout_ready {
            return;
        }
        let now = (window.size, window.scale.effective(viewporter));
        if window.reported != Some(now) {
            window.reported = Some(now);
            // Replaceable: a scale event often comes just before the configure with the new
            // size, and a placement for the older one would be refused as stale.
            self.events.retain(
                |e| !matches!(e, HostEvent::WindowConfigured { window, .. } if *window == id),
            );
            self.events.push_back(HostEvent::WindowConfigured {
                window: id,
                size: now.0,
                scale: now.1,
            });
        } else if window.transaction.is_none() {
            window.root().commit();
        }
    }

    /// Applies a change of `window`'s scale.
    pub(crate) fn update_scale(&mut self, window: WindowId, change: impl FnOnce(&mut WindowScale)) {
        if let Some(w) = self.windows.get_mut(&window) {
            change(&mut w.scale);
            self.report_configuration(window);
        }
    }

    fn window_of(&self, xdg: &XdgWindow) -> Option<WindowId> {
        self.windows.values().find(|w| &w.xdg == xdg).map(|w| w.id)
    }

    /// Attaches the root's background and commits, so a sizeless configure still maps the window
    /// and the compositor can answer with a real size. No `WindowConfigured` is reported.
    fn map_at_client_size(&mut self, id: WindowId) {
        let viewporter = self.globals.viewporter.is_some();
        let env = self.env.clone();
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        window.update_background(&env, viewporter);
        window.root().commit();
    }

    /// Starts the one-shot wait for a named size, unless one is already running. If the timer
    /// cannot be set, the client size is accepted at once so the window does not stay blank.
    fn arm_client_size_wait(&mut self, id: WindowId) {
        if self
            .windows
            .get(&id)
            .is_some_and(|window| window.layout_ready || window.fallback_timer.is_some())
        {
            return;
        }
        let Some(token) = self.loop_ops.timer(
            Instant::now() + CLIENT_SIZE_GRACE,
            TimerEvent::ClientSize(id),
        ) else {
            self.accept_client_size(id);
            return;
        };
        if let Some(window) = self.windows.get_mut(&id) {
            window.fallback_timer = Some(token);
        } else {
            self.loop_ops.remove(token);
        }
    }

    /// Drops the client-size timer. A named size has arrived.
    fn cancel_client_size_wait(&mut self, id: WindowId) {
        let token = self
            .windows
            .get_mut(&id)
            .and_then(|window| window.fallback_timer.take());
        if let Some(token) = token {
            self.loop_ops.remove(token);
        }
    }

    /// The compositor never named a size: place at the client default already stored on `id`.
    pub(crate) fn accept_client_size(&mut self, id: WindowId) {
        let report = match self.windows.get_mut(&id) {
            Some(window) => {
                window.fallback_timer = None;
                if window.layout_ready {
                    false
                } else {
                    window.layout_ready = true;
                    true
                }
            }
            None => false,
        };
        if report {
            self.report_configuration(id);
        }
    }
}

impl WindowHandler for Host {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, xdg: &XdgWindow) {
        if let Some(window) = self.window_of(xdg) {
            self.events.push_back(HostEvent::CloseRequested { window });
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        xdg: &XdgWindow,
        configure: WindowConfigure,
        _: u32,
    ) {
        let Some(id) = self.window_of(xdg) else {
            return;
        };
        let decision = {
            let window = self.windows.get_mut(&id).expect("found above");
            let fallback = if window.configured {
                window.size
            } else {
                window.default_size
            };
            let decision = decide_configure(configure.new_size, fallback);
            window.decorations = configure.decoration_mode;
            window.configured = true;
            match decision {
                ConfigureDecision::Named(size) => {
                    window.size = size;
                    window.layout_ready = true;
                }
                ConfigureDecision::Unspecified if !window.layout_ready => {
                    window.size = fallback;
                }
                ConfigureDecision::Unspecified => {}
            }
            // The window is all of the root surface: no client-side shadow or frame.
            let (w, h) = (window.size.width, window.size.height);
            window
                .xdg
                .xdg_surface()
                .set_window_geometry(0, 0, w as i32, h as i32);
            decision
        };
        match decision {
            ConfigureDecision::Named(_) => {
                self.cancel_client_size_wait(id);
                self.report_configuration(id);
            }
            ConfigureDecision::Unspecified => {
                let waiting = self
                    .windows
                    .get(&id)
                    .is_some_and(|window| !window.layout_ready);
                if waiting {
                    self.map_at_client_size(id);
                    self.arm_client_size_wait(id);
                } else {
                    self.report_configuration(id);
                }
            }
        }
    }
}

impl CompositorHandler for Host {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        factor: i32,
    ) {
        if let Some(SurfaceOwner::Root(window)) = self.surfaces.get(&surface.id()).copied() {
            self.update_scale(window, |s| s.set_integer(factor));
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

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {
        // The host's frame callbacks carry their own data (`scheduler::FrameData`).
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn fallback() -> LogicalSize {
        LogicalSize {
            width: 1024,
            height: 768,
        }
    }

    fn axis(value: u32) -> Option<NonZeroU32> {
        NonZeroU32::new(value)
    }

    #[test]
    fn a_sizeless_configure_does_not_name_a_size() {
        assert_eq!(
            decide_configure((None, None), fallback()),
            ConfigureDecision::Unspecified
        );
    }

    #[test]
    fn a_named_configure_uses_the_compositor_size() {
        assert_eq!(
            decide_configure((axis(1280), axis(800)), fallback()),
            ConfigureDecision::Named(LogicalSize {
                width: 1280,
                height: 800,
            })
        );
    }

    #[test]
    fn one_named_axis_keeps_the_fallback_for_the_other() {
        assert_eq!(
            decide_configure((axis(1280), None), fallback()),
            ConfigureDecision::Named(LogicalSize {
                width: 1280,
                height: 768,
            })
        );
        assert_eq!(
            decide_configure((None, axis(800)), fallback()),
            ConfigureDecision::Named(LogicalSize {
                width: 1024,
                height: 800,
            })
        );
    }
}
