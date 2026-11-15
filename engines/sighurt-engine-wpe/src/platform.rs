//! Sighurt's WPE platform: a `WPEDisplay` and a `WPEView` implemented in Rust (GObject subclasses
//! through glib-rs), so that WebKit renders into buffers it hands to Sighurt instead of drawing on
//! a Wayland connection, DRM device or headless target of its own.
//!
//! WebKit creates one [`View`] per `WebKitWebView` through [`Display`]'s `create_view`. It gives
//! the view each finished frame with `render_buffer` (a DMA-BUF or shared-memory `WPEBuffer`); the
//! view only hands it to its [`Sink`], and the engine presents it on the host's target later.
//! What WebKit learns back goes through `wpe_view_buffer_rendered` (it may render the next frame)
//! and `wpe_view_buffer_released` (it may reuse the buffer), which the engine calls. The display
//! also gives WebKit the clipboard ([`crate::clipboard`]) and each view's input-method context
//! ([`crate::ime`]), and its one [`Screen`] (listed by the display, and the screen of every
//! view's [`Toplevel`]) with the output's refresh rate, so WebKit paces animations at that
//! rate ([`crate::screen`]). Each view has its own `Toplevel`, which carries the view's size,
//! scale and activity and answers `get_screen`; the host places views, so it does no more.
//!
//! Must not own: Wayland objects, presentation, or any browser state. A view's callbacks run
//! inside WebKit's code on the owner thread; they queue and return.

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::{c_char, c_uint, CStr, CString};
use std::ptr::{self, NonNull};
use std::rc::Rc;

use glib::ffi::{gboolean, gpointer, GError, GFALSE, GTRUE};
use glib::prelude::*;
use glib::subclass::prelude::*;
use glib::translate::*;

use crate::clipboard::{Clipboard, ClipboardSink, WpeClipboard};
use crate::ffi;
use crate::ime::{Ime, InputMethod, WpeInputMethodContext};
use crate::screen::Screen;

glib::wrapper! {
    /// WebKit's `WPEDisplay`, the abstract parent of [`Display`].
    pub struct WpeDisplay(Object<ffi::WPEDisplay, ffi::WPEDisplayClass>);
    match fn { type_ => || ffi::wpe_display_get_type(), }
}

glib::wrapper! {
    /// WebKit's `WPEToplevel`, the parent of [`Toplevel`].
    pub struct WpeToplevel(Object<ffi::WPEToplevel, ffi::WPEToplevelClass>);
    match fn { type_ => || ffi::wpe_toplevel_get_type(), }
}

glib::wrapper! {
    /// WebKit's `WPEView`, the abstract parent of [`View`].
    pub struct WpeView(Object<ffi::WPEView, ffi::WPEViewClass>);
    match fn { type_ => || ffi::wpe_view_get_type(), }
}

/// The `WPEDisplay` virtual methods a subclass provides.
pub trait DisplayImpl: ObjectImpl + ObjectSubclass<Type: IsA<WpeDisplay>> {
    /// A new view for a `WebKitWebView` (transfer full).
    fn create_view(&self) -> WpeView;
    /// The DMA-BUF formats the compositor takes, or null (transfer full).
    fn preferred_buffer_formats(&self) -> *mut ffi::WPEBufferFormats;
    /// The DRM device WebKit renders on, or null for shared memory (transfer none).
    fn drm_device(&self) -> *mut ffi::WPEDRMDevice;
    /// The clipboard (transfer full).
    fn clipboard(&self) -> *mut ffi::WPEClipboard;
    /// An input-method context for `view` (transfer full).
    fn input_method(&self, view: *mut ffi::WPEView) -> *mut ffi::WPEInputMethodContext;
    /// How many screens the display has.
    fn n_screens(&self) -> c_uint;
    /// Screen `index` (transfer none), or null.
    fn screen(&self, index: c_uint) -> *mut ffi::WPEScreen;
}

unsafe impl<T: DisplayImpl> IsSubclassable<T> for WpeDisplay {
    fn class_init(class: &mut glib::Class<Self>) {
        Self::parent_class_init::<T>(class);
        let klass = class.as_mut();
        klass.connect = Some(display_connect);
        klass.create_view = Some(display_create_view::<T>);
        klass.get_preferred_buffer_formats = Some(display_formats::<T>);
        klass.get_drm_device = Some(display_drm_device::<T>);
        klass.get_clipboard = Some(display_clipboard::<T>);
        klass.create_input_method_context = Some(display_input_method::<T>);
        klass.get_n_screens = Some(display_n_screens::<T>);
        klass.get_screen = Some(display_screen::<T>);
    }
}

unsafe extern "C" fn display_connect(_: *mut ffi::WPEDisplay, _: *mut *mut GError) -> gboolean {
    GTRUE
}

unsafe extern "C" fn display_create_view<T: DisplayImpl>(
    display: *mut ffi::WPEDisplay,
) -> *mut ffi::WPEView {
    let instance = unsafe { &*(display as *mut T::Instance) };
    instance.imp().create_view().into_glib_ptr()
}

unsafe extern "C" fn display_formats<T: DisplayImpl>(
    display: *mut ffi::WPEDisplay,
) -> *mut ffi::WPEBufferFormats {
    let instance = unsafe { &*(display as *mut T::Instance) };
    instance.imp().preferred_buffer_formats()
}

unsafe extern "C" fn display_drm_device<T: DisplayImpl>(
    display: *mut ffi::WPEDisplay,
) -> *mut ffi::WPEDRMDevice {
    let instance = unsafe { &*(display as *mut T::Instance) };
    instance.imp().drm_device()
}

unsafe extern "C" fn display_clipboard<T: DisplayImpl>(
    display: *mut ffi::WPEDisplay,
) -> *mut ffi::WPEClipboard {
    let instance = unsafe { &*(display as *mut T::Instance) };
    instance.imp().clipboard()
}

unsafe extern "C" fn display_input_method<T: DisplayImpl>(
    display: *mut ffi::WPEDisplay,
    view: *mut ffi::WPEView,
) -> *mut ffi::WPEInputMethodContext {
    let instance = unsafe { &*(display as *mut T::Instance) };
    instance.imp().input_method(view)
}

unsafe extern "C" fn display_n_screens<T: DisplayImpl>(display: *mut ffi::WPEDisplay) -> c_uint {
    let instance = unsafe { &*(display as *mut T::Instance) };
    instance.imp().n_screens()
}

unsafe extern "C" fn display_screen<T: DisplayImpl>(
    display: *mut ffi::WPEDisplay,
    index: c_uint,
) -> gpointer {
    let instance = unsafe { &*(display as *mut T::Instance) };
    instance.imp().screen(index).cast()
}

/// The `WPEToplevel` virtual methods a subclass provides.
pub trait ToplevelImpl: ObjectImpl + ObjectSubclass<Type: IsA<WpeToplevel>> {
    /// The screen the toplevel is on (transfer none), or null.
    fn screen(&self) -> *mut ffi::WPEScreen;
}

unsafe impl<T: ToplevelImpl> IsSubclassable<T> for WpeToplevel {
    fn class_init(class: &mut glib::Class<Self>) {
        Self::parent_class_init::<T>(class);
        class.as_mut().get_screen = Some(toplevel_screen::<T>);
    }
}

unsafe extern "C" fn toplevel_screen<T: ToplevelImpl>(
    toplevel: *mut ffi::WPEToplevel,
) -> *mut ffi::WPEScreen {
    let instance = unsafe { &*(toplevel as *mut T::Instance) };
    instance.imp().screen()
}

/// The `WPEView` virtual methods a subclass provides.
pub trait ViewImpl: ObjectImpl + ObjectSubclass<Type: IsA<WpeView>> {
    /// A finished frame to show; `false` refuses it.
    fn render_buffer(&self, buffer: Buffer, damage: &[ffi::WPERectangle]) -> bool;
    /// The page's cursor, by its CSS name.
    fn set_cursor(&self, name: &str);
}

unsafe impl<T: ViewImpl> IsSubclassable<T> for WpeView {
    fn class_init(class: &mut glib::Class<Self>) {
        Self::parent_class_init::<T>(class);
        let klass = class.as_mut();
        klass.render_buffer = Some(view_render_buffer::<T>);
        klass.set_cursor_from_name = Some(view_set_cursor::<T>);
        klass.can_be_mapped = Some(view_can_be_mapped);
    }
}

unsafe extern "C" fn view_render_buffer<T: ViewImpl>(
    view: *mut ffi::WPEView,
    buffer: *mut ffi::WPEBuffer,
    rects: *const ffi::WPERectangle,
    n_rects: c_uint,
    error: *mut *mut GError,
) -> gboolean {
    let instance = unsafe { &*(view as *mut T::Instance) };
    let damage = match rects.is_null() {
        true => &[][..],
        false => unsafe { std::slice::from_raw_parts(rects, n_rects as usize) },
    };
    // SAFETY: WebKit passes a live buffer; `Buffer` takes its own reference.
    let buffer = unsafe { Buffer::from_glib_none(buffer) };
    if instance.imp().render_buffer(buffer, damage) {
        return GTRUE;
    }
    if !error.is_null() {
        let e = glib::Error::new(glib::FileError::Failed, "the Sighurt view is closed");
        unsafe { *error = e.into_glib_ptr() };
    }
    GFALSE
}

unsafe extern "C" fn view_set_cursor<T: ViewImpl>(view: *mut ffi::WPEView, name: *const c_char) {
    let instance = unsafe { &*(view as *mut T::Instance) };
    if let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() {
        instance.imp().set_cursor(name);
    }
}

/// Mapping is the host's business: the engine maps and unmaps the view itself.
unsafe extern "C" fn view_can_be_mapped(_: *mut ffi::WPEView) -> gboolean {
    GTRUE
}

/// A `WPEBuffer`, holding a reference.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Buffer(NonNull<ffi::WPEBuffer>);

impl Buffer {
    /// # Safety
    ///
    /// `buffer` must be a live `WPEBuffer`.
    unsafe fn from_glib_none(buffer: *mut ffi::WPEBuffer) -> Self {
        unsafe { glib::gobject_ffi::g_object_ref(buffer.cast()) };
        Self(NonNull::new(buffer).expect("a buffer"))
    }

    pub fn as_ptr(&self) -> *mut ffi::WPEBuffer {
        self.0.as_ptr()
    }

    pub fn size(&self) -> (u32, u32) {
        let (w, h) = unsafe {
            (
                ffi::wpe_buffer_get_width(self.as_ptr()),
                ffi::wpe_buffer_get_height(self.as_ptr()),
            )
        };
        (w.max(0) as u32, h.max(0) as u32)
    }

    pub fn is_dma_buf(&self) -> bool {
        unsafe { ffi::is_a(self.as_ptr(), ffi::wpe_buffer_dma_buf_get_type()) }
    }
}

impl Clone for Buffer {
    fn clone(&self) -> Self {
        unsafe { Self::from_glib_none(self.as_ptr()) }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe { glib::gobject_ffi::g_object_unref(self.as_ptr().cast()) };
    }
}

/// What a view tells the engine, from inside WebKit's calls.
pub enum Call {
    /// A finished frame, with its damage in buffer pixels (empty: everything).
    Frame(Buffer, Vec<ffi::WPERectangle>),
    /// The cursor changed, by CSS name.
    Cursor(String),
    /// The view's input-method context changed.
    Ime(Ime),
}

/// Where a view's calls go. Only queues: it runs inside WebKit's code.
pub type Sink = Rc<dyn Fn(Call)>;

/// The compositor's DMA-BUF support, as WebKit needs it.
pub struct Dmabuf {
    /// The DRM device's primary and render nodes.
    pub primary: CString,
    pub render: Option<CString>,
    /// `(fourcc, modifier)` pairs the compositor imports.
    pub formats: Vec<(u32, u64)>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Display {
        pub dmabuf: RefCell<Option<Dmabuf>>,
        pub device: Cell<Option<NonNull<ffi::WPEDRMDevice>>>,
        pub clipboard: RefCell<Option<ClipboardSink>>,
        pub clipboard_object: glib::WeakRef<Clipboard>,
        pub screen: OnceCell<Screen>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Display {
        const NAME: &'static str = "SighurtWPEDisplay";
        type Type = super::Display;
        type ParentType = WpeDisplay;
    }

    impl ObjectImpl for Display {
        fn dispose(&self) {
            if let Some(screen) = self.screen.get() {
                screen.shutdown();
            }
            if let Some(device) = self.device.take() {
                unsafe { ffi::wpe_drm_device_unref(device.as_ptr()) };
            }
        }
    }

    impl DisplayImpl for Display {
        fn create_view(&self) -> WpeView {
            let display = self.obj();
            let view: super::View = glib::Object::builder()
                .property("display", display.upcast_ref::<WpeDisplay>())
                .build();
            // One toplevel per view: the host places views, so WebKit's toplevels only carry
            // the view's size, scale and activity.
            let toplevel: super::Toplevel = glib::Object::builder()
                .property("display", display.upcast_ref::<WpeDisplay>())
                .property("max-views", 1u32)
                .build();
            toplevel.imp().screen.replace(self.screen.get().cloned());
            let toplevel_ptr = toplevel.upcast_ref::<WpeToplevel>().to_glib_none().0;
            unsafe {
                ffi::wpe_toplevel_state_changed(toplevel_ptr, ffi::WPE_TOPLEVEL_STATE_ACTIVE);
                ffi::wpe_view_set_toplevel(view.as_ptr(), toplevel_ptr);
            }
            view.upcast()
        }

        fn preferred_buffer_formats(&self) -> *mut ffi::WPEBufferFormats {
            let dmabuf = self.dmabuf.borrow();
            let Some(dmabuf) = dmabuf.as_ref() else {
                return ptr::null_mut();
            };
            unsafe {
                let builder = ffi::wpe_buffer_formats_builder_new(self.drm_device());
                ffi::wpe_buffer_formats_builder_append_group(
                    builder,
                    ptr::null_mut(),
                    ffi::WPE_BUFFER_FORMAT_USAGE_RENDERING,
                );
                for &(format, modifier) in &dmabuf.formats {
                    ffi::wpe_buffer_formats_builder_append_format(builder, format, modifier);
                }
                ffi::wpe_buffer_formats_builder_end(builder)
            }
        }

        fn drm_device(&self) -> *mut ffi::WPEDRMDevice {
            if let Some(device) = self.device.get() {
                return device.as_ptr();
            }
            let dmabuf = self.dmabuf.borrow();
            let Some(dmabuf) = dmabuf.as_ref() else {
                return ptr::null_mut();
            };
            let render = dmabuf.render.as_ref().map_or(ptr::null(), |r| r.as_ptr());
            let device = unsafe { ffi::wpe_drm_device_new(dmabuf.primary.as_ptr(), render) };
            self.device.set(NonNull::new(device));
            device
        }

        fn clipboard(&self) -> *mut ffi::WPEClipboard {
            let sink = self
                .clipboard
                .borrow()
                .clone()
                .expect("set before WebKit is used");
            let clipboard = Clipboard::new(self.obj().upcast_ref(), sink);
            self.clipboard_object.set(Some(&clipboard));
            clipboard.upcast::<WpeClipboard>().into_glib_ptr()
        }

        fn n_screens(&self) -> c_uint {
            self.screen.get().map_or(0, |_| 1)
        }

        fn screen(&self, index: c_uint) -> *mut ffi::WPEScreen {
            match (index, self.screen.get()) {
                (0, Some(screen)) => screen.as_ptr(),
                _ => ptr::null_mut(),
            }
        }

        fn input_method(&self, view: *mut ffi::WPEView) -> *mut ffi::WPEInputMethodContext {
            let view: WpeView = unsafe { from_glib_none(view) };
            let context = InputMethod::new(&view);
            if let Ok(view) = view.downcast::<super::View>() {
                view.imp().input_method.set(Some(&context));
            }
            context.upcast::<WpeInputMethodContext>().into_glib_ptr()
        }
    }

    #[derive(Default)]
    pub struct Toplevel {
        pub screen: RefCell<Option<Screen>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Toplevel {
        const NAME: &'static str = "SighurtWPEToplevel";
        type Type = super::Toplevel;
        type ParentType = WpeToplevel;
    }

    impl ObjectImpl for Toplevel {}

    impl ToplevelImpl for Toplevel {
        fn screen(&self) -> *mut ffi::WPEScreen {
            self.screen
                .borrow()
                .as_ref()
                .map_or(ptr::null_mut(), Screen::as_ptr)
        }
    }

    #[derive(Default)]
    pub struct View {
        pub sink: RefCell<Option<Sink>>,
        pub input_method: glib::WeakRef<InputMethod>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for View {
        const NAME: &'static str = "SighurtWPEView";
        type Type = super::View;
        type ParentType = WpeView;
    }

    impl ObjectImpl for View {}

    impl ViewImpl for View {
        fn render_buffer(&self, buffer: Buffer, damage: &[ffi::WPERectangle]) -> bool {
            let sink = self.sink.borrow().clone();
            match sink {
                Some(sink) => {
                    sink(Call::Frame(buffer, damage.to_vec()));
                    true
                }
                None => false,
            }
        }

        fn set_cursor(&self, name: &str) {
            self.obj().call(Call::Cursor(name.to_owned()));
        }
    }
}

glib::wrapper! {
    /// Sighurt's `WPEDisplay`: one per process, the primary display, made before any web view.
    pub struct Display(ObjectSubclass<imp::Display>) @extends WpeDisplay;
}

glib::wrapper! {
    /// Sighurt's `WPEToplevel`: one per view, on the display's screen.
    pub struct Toplevel(ObjectSubclass<imp::Toplevel>) @extends WpeToplevel;
}

glib::wrapper! {
    /// Sighurt's `WPEView`: one per `WebKitWebView`.
    pub struct View(ObjectSubclass<imp::View>) @extends WpeView;
}

impl Display {
    /// The display, connected and made primary (WebKit's web processes learn the DRM device
    /// from the primary display), with DMA-BUF support or, with `None`, shared memory only.
    /// The clipboard reports to `clipboard`. The display's one screen refreshes at `mhz` and is
    /// `size` logical pixels (what pages read as `screen.*`).
    pub fn new(
        dmabuf: Option<Dmabuf>,
        clipboard: ClipboardSink,
        mhz: u32,
        size: (i32, i32),
    ) -> Result<Self, glib::Error> {
        let display: Self = glib::Object::new();
        let _ = display.imp().screen.set(Screen::new(mhz, size));
        display.imp().dmabuf.replace(dmabuf);
        display.imp().clipboard.replace(Some(clipboard));
        let raw = display.as_ptr();
        let mut error = ptr::null_mut();
        if unsafe { ffi::wpe_display_connect(raw, &mut error) } == GFALSE {
            return Err(unsafe { from_glib_full(error) });
        }
        unsafe { ffi::wpe_display_set_primary(raw) };
        Ok(display)
    }

    pub fn as_ptr(&self) -> *mut ffi::WPEDisplay {
        self.upcast_ref::<WpeDisplay>().to_glib_none().0
    }

    /// The display's screen.
    pub fn screen(&self) -> &Screen {
        self.imp().screen.get().expect("made with the display")
    }

    /// The clipboard WebKit uses, once it asked for it.
    pub fn clipboard(&self) -> Option<Clipboard> {
        self.imp().clipboard_object.upgrade()
    }
}

impl View {
    /// Where the view's calls go from now on; `None` refuses further frames.
    pub fn set_sink(&self, sink: Option<Sink>) {
        self.imp().sink.replace(sink);
    }

    /// Hands `call` to the sink, if the view has one.
    pub fn call(&self, call: Call) {
        let sink = self.imp().sink.borrow().clone();
        if let Some(sink) = sink {
            sink(call);
        }
    }

    /// The input-method context WebKit made for the view, while it lives.
    pub fn input_method(&self) -> Option<InputMethod> {
        self.imp().input_method.upgrade()
    }

    pub fn as_ptr(&self) -> *mut ffi::WPEView {
        self.upcast_ref::<WpeView>().to_glib_none().0
    }
}
