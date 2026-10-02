//! The page: one Servo runtime with one WebView, presenting through surfman's
//! `WindowRenderingContext` straight into the host's page subsurface.
//!
//! Servo draws with WebRender into the EGL window surface's back buffer (`WebView::paint`), and
//! `RenderingContext::present` swaps it, which attaches and commits the page `wl_surface` inside
//! the driver. The host never attaches or commits a buffer on that surface (§9). Nothing reads
//! pixels back: [`PageContext`] counts every `read_to_image`, and the audited GL counts every
//! `glReadPixels`/`glGetTexImage` WebRender makes on this context.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use dpi::PhysicalSize;
use euclid::default::Size2D;
use gleam::gl::{self, Gl, ProfilingGl};
use raw_window_handle::{
    DisplayHandle, RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
    WindowHandle,
};
use servo::{
    Code, ConsoleLogLevel, DeviceIntRect, DevicePoint, EventLoopWaker, InputEvent, Key, KeyState,
    KeyboardEvent, LoadStatus, Location, Modifiers, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseMoveEvent, NamedKey, Opts, Preferences, RefreshDriver, RenderingContext,
    RgbaImage, Servo, ServoBuilder, ServoDelegate, ServoError, WebView, WebViewBuilder,
    WebViewDelegate, WebViewPoint, WindowRenderingContext,
};
use sighurt_api::engine::EngineWaker;
use sighurt_api::geometry::{BufferPoint, BufferSize, Scale};
use sighurt_api::input::{KeyCode, KeyLocation};
use smithay_client_toolkit::seat::keyboard::{self, Keysym};
use surfman::{Connection as SurfmanConnection, Surface, SurfaceTexture};
use url::Url;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};

use crate::egl::{self, Egl};
use crate::log;

/// What the GL context turned out to be, for the diagnostics.
pub struct GlInfo {
    pub renderer: String,
    pub vendor: String,
    pub version: String,
    pub egl_vendor: String,
    pub egl_version: String,
    pub egl_client_apis: String,
    /// A software rasterizer (llvmpipe, softpipe, swrast): the explicitly diagnosed fallback.
    pub software: bool,
}

pub struct Page {
    pub webview: WebView,
    servo: Servo,
    context: Rc<PageContext>,
    pub driver: Rc<FrameDriver>,
    pub delegate: Rc<Delegate>,
    waker: EngineWaker,
    egl: egl::Display,
    /// Set by every call into Servo, so the owner pumps it in the same loop turn.
    pub called: bool,
    /// Keys the page has seen go down and not up, with the key value each press sent; released
    /// on blur with that value, so a page tracking keys by `KeyboardEvent.key` sees them go up.
    pressed: HashMap<u32, Key>,
    focused: bool,
}

impl Page {
    /// Creates the rendering context on `surface`, then Servo and one WebView loading `url`.
    /// `wake` gets the owner loop's attention (it pings an event source).
    pub fn new(
        connection: &Connection,
        surface: &WlSurface,
        size: BufferSize,
        scale: Scale,
        url: Url,
        wake: impl Fn() + Send + Sync + 'static,
        audit_gl: bool,
    ) -> Result<(Self, GlInfo), String> {
        let driver = Rc::new(FrameDriver::default());
        let context = Rc::new(PageContext::new(
            connection,
            surface,
            size,
            driver.clone(),
            audit_gl,
        )?);
        let egl = Egl::load()?.current_display()?;
        egl.set_swap_interval(0)?;
        let info = context.info(&egl);

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let waker = EngineWaker::new(wake);
        let preferences = Preferences {
            // Servo 0.5's parallel styling races with 3 threads (see the old sig-servo).
            layout_threads: 2,
            ..Preferences::default()
        };
        let servo = ServoBuilder::default()
            .opts(Opts {
                hard_fail: false,
                ..Opts::default()
            })
            .preferences(preferences)
            .event_loop_waker(Box::new(Waker(waker.clone())))
            .build();
        servo.setup_logging();
        let delegate = Rc::new(Delegate::default());
        servo.set_delegate(delegate.clone());
        let webview = WebViewBuilder::new(&servo, context.clone())
            .url(url)
            .hidpi_scale_factor(euclid::Scale::new(scale.factor() as f32))
            .delegate(delegate.clone())
            .build();
        let page = Self {
            webview,
            servo,
            context,
            driver,
            delegate,
            waker,
            egl,
            called: true,
            pressed: HashMap::new(),
            focused: false,
        };
        Ok((page, info))
    }

    /// Whether Servo asked to be pumped, or the owner called it this turn. Clears both: a wake
    /// that arrives during the pump pings the loop again.
    pub fn take_work(&mut self) -> bool {
        std::mem::take(&mut self.called) | self.waker.take()
    }

    pub fn spin(&self) {
        self.servo.spin_event_loop();
    }

    /// WebRender draws the current scene into the EGL back buffer.
    pub fn paint(&self) {
        self.webview.paint();
    }

    /// Swaps buffers: the driver attaches and commits the page surface.
    pub fn present(&self) {
        self.context.present();
    }

    pub fn readbacks(&self) -> (u64, Option<u64>) {
        (
            self.context.read_to_image_calls.get(),
            self.context.audited_reads.as_ref().map(|reads| reads.get()),
        )
    }

    /// New metrics: resizes the EGL window (at the next frame) and Servo's viewport.
    pub fn resize(&mut self, size: BufferSize, scale: Scale) {
        // Through the WebView: resizing the context first would make Servo skip its viewport.
        self.webview
            .resize(PhysicalSize::new(size.width, size.height));
        self.webview
            .set_hidpi_scale_factor(euclid::Scale::new(scale.factor() as f32));
        self.called = true;
    }

    /// A hidden page stops animations and slows its timers; it presents nothing meanwhile.
    pub fn set_hidden(&mut self, hidden: bool) {
        self.webview.set_throttled(hidden);
        self.called = true;
    }

    /// Gives or takes keyboard focus. Taking it first releases every key the page holds down,
    /// so the page never sees a key stuck after focus moved (the real release goes elsewhere).
    pub fn set_focus(&mut self, focused: bool) {
        if focused == self.focused {
            return;
        }
        self.focused = focused;
        if focused {
            self.webview.focus();
        } else {
            for (code, key) in std::mem::take(&mut self.pressed) {
                let key = KeyboardEvent::new_without_event(
                    KeyState::Up,
                    key,
                    code_of(code),
                    location_of(code),
                    Modifiers::empty(),
                    false,
                    false,
                );
                self.webview.notify_input_event(InputEvent::Keyboard(key));
            }
            self.webview.blur();
        }
        self.called = true;
    }

    pub fn key(
        &mut self,
        state: KeyState,
        event: &keyboard::KeyEvent,
        modifiers: keyboard::Modifiers,
        repeat: bool,
    ) {
        let key = key_of(event, modifiers);
        match state {
            KeyState::Down => {
                self.pressed.insert(event.raw_code, key.clone());
            }
            KeyState::Up => {
                if self.pressed.remove(&event.raw_code).is_none() {
                    return;
                }
            }
        }
        let key = KeyboardEvent::new_without_event(
            state,
            key,
            code_of(event.raw_code),
            location_of(event.raw_code),
            modifiers_of(modifiers),
            repeat,
            false,
        );
        self.webview.notify_input_event(InputEvent::Keyboard(key));
        self.called = true;
    }

    /// Whether the page holds `code` down (so its release belongs to the page).
    pub fn holds(&self, code: u32) -> bool {
        self.pressed.contains_key(&code)
    }

    /// Pointer input at `point`, in the page's buffer (device) pixels, converted once from the
    /// surface-local position Wayland gave the page surface.
    pub fn pointer_move(&mut self, point: BufferPoint) {
        let event = MouseMoveEvent::new(device(point));
        self.webview
            .notify_input_event(InputEvent::MouseMove(event));
        self.called = true;
    }

    pub fn pointer_button(&mut self, point: BufferPoint, button: u32, pressed: bool) {
        let action = if pressed {
            MouseButtonAction::Down
        } else {
            MouseButtonAction::Up
        };
        // Linux evdev codes: BTN_LEFT is 0x110.
        let button = match button {
            0x110 => MouseButton::Left,
            0x111 => MouseButton::Right,
            0x112 => MouseButton::Middle,
            0x113 => MouseButton::Back,
            0x114 => MouseButton::Forward,
            other => MouseButton::Other(other as u16),
        };
        let event = MouseButtonEvent::new(action, button, device(point));
        self.webview
            .notify_input_event(InputEvent::MouseButton(event));
        self.called = true;
    }

    /// Shuts down in the order the FFI lifetimes need (§9, §20): the WebView and Servo, which
    /// release every clone of the rendering context; then the context, destroying the EGL
    /// window surface and the GL context; then the EGL display. The caller destroys the
    /// Wayland surface only after this returned `Ok`.
    pub fn shutdown(self) -> Result<(), String> {
        let Page {
            webview,
            servo,
            context,
            driver,
            delegate,
            egl,
            ..
        } = self;
        drop(webview);
        log!("teardown: WebView dropped; Servo shutting down");
        // Servo 0.5 shuts down only in `Drop`: it tells the constellation to exit and spins
        // until the constellation, WebGL and paint threads are done.
        drop(servo);
        drop(delegate);
        drop(driver);
        log!("teardown: Servo shut down");
        let context = Rc::try_unwrap(context).map_err(|context| {
            format!(
                "the rendering context is still referenced {} times; leaving the page surface alive",
                Rc::strong_count(&context)
            )
        })?;
        drop(context);
        log!("teardown: GL context and EGL window surface destroyed");
        egl.terminate()?;
        log!("teardown: EGL display terminated");
        Ok(())
    }
}

fn device(point: BufferPoint) -> WebViewPoint {
    WebViewPoint::Device(DevicePoint::new(point.x as f32, point.y as f32))
}

fn code_of(raw: u32) -> Code {
    KeyCode(raw)
        .w3c_code()
        .and_then(|code| code.parse().ok())
        .unwrap_or(Code::Unidentified)
}

fn location_of(raw: u32) -> Location {
    match KeyCode(raw).location() {
        KeyLocation::Standard => Location::Standard,
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
    }
}

fn modifiers_of(m: keyboard::Modifiers) -> Modifiers {
    let mut modifiers = Modifiers::empty();
    for (on, flag) in [
        (m.shift, Modifiers::SHIFT),
        (m.ctrl, Modifiers::CONTROL),
        (m.alt, Modifiers::ALT),
        (m.logo, Modifiers::META),
        (m.caps_lock, Modifiers::CAPS_LOCK),
        (m.num_lock, Modifiers::NUM_LOCK),
    ] {
        if on {
            modifiers |= flag;
        }
    }
    modifiers
}

/// The W3C key value: the text a press types, else the key's name. Enough for M1's routing
/// checks; the full mapping (dead keys, AltGr) is M3's.
fn key_of(event: &keyboard::KeyEvent, modifiers: keyboard::Modifiers) -> Key {
    if let Some(text) = typed_text(event, modifiers) {
        return Key::Character(text);
    }
    let name = match event.keysym {
        Keysym::Return | Keysym::KP_Enter => "Enter",
        Keysym::BackSpace => "Backspace",
        Keysym::Tab | Keysym::ISO_Left_Tab => "Tab",
        Keysym::Escape => "Escape",
        Keysym::Delete => "Delete",
        Keysym::Left => "ArrowLeft",
        Keysym::Right => "ArrowRight",
        Keysym::Up => "ArrowUp",
        Keysym::Down => "ArrowDown",
        Keysym::Home => "Home",
        Keysym::End => "End",
        Keysym::Shift_L | Keysym::Shift_R => "Shift",
        Keysym::Control_L | Keysym::Control_R => "Control",
        Keysym::Alt_L | Keysym::Alt_R => "Alt",
        Keysym::Super_L | Keysym::Super_R => "Meta",
        Keysym::ISO_Level3_Shift => "AltGraph",
        _ => {
            return match event.keysym.key_char() {
                Some(c) if !c.is_control() => Key::Character(c.to_string()),
                _ => Key::Named(NamedKey::Unidentified),
            }
        }
    };
    name.parse().unwrap_or(Key::Named(NamedKey::Unidentified))
}

/// What a press types: xkb's text without control characters, and none while Control or Logo
/// is held.
pub fn typed_text(event: &keyboard::KeyEvent, modifiers: keyboard::Modifiers) -> Option<String> {
    let text = event.utf8.as_ref()?;
    (!text.is_empty() && !text.chars().any(char::is_control) && !modifiers.ctrl && !modifiers.logo)
        .then(|| text.clone())
}

/// Servo's frame clock, driven by the page surface's Wayland frame callbacks.
///
/// Servo registers for "the next frame" while it animates; the host starts that frame when the
/// compositor's frame callback for the last presented page frame arrives, or at once when no
/// page frame is in flight. While the page is hidden nothing starts, so animations stop.
#[derive(Default)]
pub struct FrameDriver {
    observers: RefCell<Vec<Box<dyn Fn() + Send>>>,
}

impl FrameDriver {
    pub fn is_waiting(&self) -> bool {
        !self.observers.borrow().is_empty()
    }

    /// Starts the frame Servo waits for: each observer marks Servo ready and wakes the loop.
    pub fn start_frame(&self) {
        for observer in self.observers.take() {
            observer();
        }
    }
}

impl RefreshDriver for FrameDriver {
    fn observe_next_frame(&self, start_frame: Box<dyn Fn() + Send + 'static>) {
        self.observers.borrow_mut().push(start_frame);
    }
}

#[derive(Clone)]
struct Waker(EngineWaker);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        self.0.wake();
    }
}

/// Servo's callbacks: a new frame is noted for the owner loop; the rest is logged.
#[derive(Default)]
pub struct Delegate {
    pub frame_ready: Cell<bool>,
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.frame_ready.set(true);
    }

    fn notify_page_title_changed(&self, _: WebView, title: Option<String>) {
        log!("page: title {title:?}");
    }

    fn notify_focus_changed(&self, _: WebView, focused: bool) {
        log!("page: servo reports focus {focused}");
    }

    fn notify_load_status_changed(&self, _: WebView, status: LoadStatus) {
        log!("page: load status {status:?}");
    }

    fn notify_animating_changed(&self, _: WebView, animating: bool) {
        log!("page: animating {animating}");
    }

    fn notify_crashed(&self, _: WebView, reason: String, _: Option<String>) {
        log!("page: crashed: {reason}");
    }

    fn show_console_message(&self, _: WebView, _: ConsoleLogLevel, message: String) {
        log!("page console: {message}");
    }
}

impl ServoDelegate for Delegate {
    fn notify_error(&self, error: ServoError) {
        log!("servo: error {error:?}");
    }
}

/// Servo's `WindowRenderingContext` on the page subsurface, with every readback counted.
pub struct PageContext {
    window: WindowRenderingContext,
    gl: Rc<dyn Gl>,
    read_to_image_calls: Cell<u64>,
    /// `glReadPixels`/`glGetTexImage` calls on this context, when auditing.
    audited_reads: Option<Rc<Cell<u64>>>,
}

impl PageContext {
    fn new(
        connection: &Connection,
        surface: &WlSurface,
        size: BufferSize,
        driver: Rc<FrameDriver>,
        audit_gl: bool,
    ) -> Result<Self, String> {
        let display = NonNull::new(connection.backend().display_ptr() as *mut c_void)
            .ok_or("no libwayland display (wayland-client needs its `system` backend)")?;
        let surface = NonNull::new(surface.id().as_ptr() as *mut c_void)
            .ok_or("the page surface has no libwayland proxy")?;
        // SAFETY: both pointers are libwayland's own, from this process' connection. The
        // display outlives the context, and the surface outlives it too: `Page::shutdown`
        // destroys the context before the host destroys the surface or closes the connection.
        let (display, window) = unsafe {
            (
                DisplayHandle::borrow_raw(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                    display,
                ))),
                WindowHandle::borrow_raw(RawWindowHandle::Wayland(WaylandWindowHandle::new(
                    surface,
                ))),
            )
        };
        let window = WindowRenderingContext::new_with_refresh_driver(
            display,
            window,
            PhysicalSize::new(size.width, size.height),
            driver,
        )
        .map_err(|e| format!("cannot create the page's rendering context: {e:?}"))?;
        let (gl, audited_reads) = if audit_gl {
            let reads = Rc::new(Cell::new(0));
            let counter = reads.clone();
            let gl = ProfilingGl::wrap(window.gleam_gl_api(), Duration::ZERO, move |name, _| {
                if name.starts_with("read_pixels") || name.starts_with("get_tex_image") {
                    counter.set(counter.get() + 1);
                }
            });
            (gl, Some(reads))
        } else {
            (window.gleam_gl_api(), None)
        };
        Ok(Self {
            window,
            gl,
            read_to_image_calls: Cell::new(0),
            audited_reads,
        })
    }

    fn info(&self, egl: &egl::Display) -> GlInfo {
        let gl = self.window.gleam_gl_api();
        let renderer = gl.get_string(gl::RENDERER);
        let (egl_vendor, egl_version, egl_client_apis) = egl.strings();
        let lower = renderer.to_lowercase();
        GlInfo {
            software: ["llvmpipe", "softpipe", "swrast", "software"]
                .iter()
                .any(|name| lower.contains(name)),
            renderer,
            vendor: gl.get_string(gl::VENDOR),
            version: gl.get_string(gl::VERSION),
            egl_vendor,
            egl_version,
            egl_client_apis,
        }
    }
}

impl RenderingContext for PageContext {
    fn prepare_for_rendering(&self) {
        self.window.prepare_for_rendering();
    }

    fn read_to_image(&self, rect: DeviceIntRect) -> Option<RgbaImage> {
        self.read_to_image_calls
            .set(self.read_to_image_calls.get() + 1);
        log!("page: Servo read the page back ({rect:?})");
        self.window.read_to_image(rect)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.window.size()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        self.window.resize(size);
    }

    fn present(&self) {
        self.window.present();
    }

    fn make_current(&self) -> Result<(), surfman::Error> {
        self.window.make_current()
    }

    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        self.gl.clone()
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.window.glow_gl_api()
    }

    fn create_texture(&self, surface: Surface) -> Option<(SurfaceTexture, u32, Size2D<i32>)> {
        self.window.create_texture(surface)
    }

    fn destroy_texture(&self, texture: SurfaceTexture) -> Option<Surface> {
        self.window.destroy_texture(texture)
    }

    fn connection(&self) -> Option<SurfmanConnection> {
        self.window.connection()
    }

    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        self.window.refresh_driver()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    #[test]
    fn a_frame_starts_every_waiting_observer_once() {
        let driver = FrameDriver::default();
        let started = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let started = started.clone();
            driver.observe_next_frame(Box::new(move || {
                started.fetch_add(1, Ordering::SeqCst);
            }));
        }
        assert!(driver.is_waiting());
        driver.start_frame();
        assert_eq!(started.load(Ordering::SeqCst), 2);
        assert!(!driver.is_waiting());
        driver.start_frame();
        assert_eq!(started.load(Ordering::SeqCst), 2, "a frame is started once");
    }

    fn press(keysym: Keysym, utf8: &str) -> keyboard::KeyEvent {
        keyboard::KeyEvent {
            time: 0,
            raw_code: 30,
            keysym,
            utf8: Some(utf8.into()),
        }
    }

    #[test]
    fn keys_type_text_only_without_shortcut_modifiers() {
        let plain = keyboard::Modifiers::default();
        let ctrl = keyboard::Modifiers {
            ctrl: true,
            ..plain
        };
        assert_eq!(typed_text(&press(Keysym::a, "a"), plain), Some("a".into()));
        assert_eq!(typed_text(&press(Keysym::Return, "\r"), plain), None);
        assert_eq!(typed_text(&press(Keysym::a, "\u{1}"), ctrl), None);
        assert_eq!(
            key_of(&press(Keysym::Return, "\r"), plain),
            Key::Named(NamedKey::Enter)
        );
        assert_eq!(
            key_of(&press(Keysym::a, "\u{1}"), ctrl),
            Key::Character("a".into())
        );
        assert_eq!(code_of(30), Code::KeyA);
    }
}
