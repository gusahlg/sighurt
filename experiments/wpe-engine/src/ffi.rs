//! The C API of WPE WebKit 2.52 that this crate calls: the WPE Platform API (displays, views,
//! buffers, events) and WebKit's `WebKitWebView`. Written by hand from the installed headers
//! (`$SIGHURT_WPE_PREFIX/include/wpe-webkit-2.0`); the class structs mirror the C layouts exactly,
//! since GObject subclasses are allocated and initialized from them.
//!
//! Must not own: any logic. Everything here is a declaration.

#![allow(non_camel_case_types, non_snake_case)]

use std::ffi::{c_char, c_double, c_int, c_uint, c_void};

use glib::ffi::{gboolean, gpointer, GArray, GBytes, GDestroyNotify, GError, GType};
use glib::gobject_ffi::{GObject, GObjectClass};

macro_rules! opaque {
    ($($name:ident),* $(,)?) => {
        $(#[repr(C)] pub struct $name { _private: [u8; 0] })*
    };
}

opaque!(
    WPEBuffer,
    WPEBufferFormats,
    WPEBufferFormatsBuilder,
    WPEDRMDevice,
    WPEEvent,
    WPEKeymap,
    WebKitWebView,
    JSCValue,
    GAsyncResult,
);

/// `struct _WPEDisplay`: `WPE_DECLARE_DERIVABLE_TYPE` adds a private pointer to the parent.
#[repr(C)]
pub struct WPEDisplay {
    pub parent_instance: GObject,
    pub priv_: *mut c_void,
}

#[repr(C)]
pub struct WPEDisplayClass {
    pub parent_class: GObjectClass,
    pub connect: Option<unsafe extern "C" fn(*mut WPEDisplay, *mut *mut GError) -> gboolean>,
    pub create_view: Option<unsafe extern "C" fn(*mut WPEDisplay) -> *mut WPEView>,
    pub get_egl_display:
        Option<unsafe extern "C" fn(*mut WPEDisplay, *mut *mut GError) -> gpointer>,
    pub get_keymap: Option<unsafe extern "C" fn(*mut WPEDisplay) -> *mut WPEKeymap>,
    pub get_clipboard: Option<unsafe extern "C" fn(*mut WPEDisplay) -> gpointer>,
    pub get_preferred_buffer_formats:
        Option<unsafe extern "C" fn(*mut WPEDisplay) -> *mut WPEBufferFormats>,
    pub get_n_screens: Option<unsafe extern "C" fn(*mut WPEDisplay) -> c_uint>,
    pub get_screen: Option<unsafe extern "C" fn(*mut WPEDisplay, c_uint) -> gpointer>,
    pub get_drm_device: Option<unsafe extern "C" fn(*mut WPEDisplay) -> *mut WPEDRMDevice>,
    pub use_explicit_sync: Option<unsafe extern "C" fn(*mut WPEDisplay) -> gboolean>,
    pub create_input_method_context:
        Option<unsafe extern "C" fn(*mut WPEDisplay, *mut WPEView) -> gpointer>,
    pub create_toplevel: Option<unsafe extern "C" fn(*mut WPEDisplay, c_uint) -> *mut WPEToplevel>,
    pub create_gamepad_manager: Option<unsafe extern "C" fn(*mut WPEDisplay) -> gpointer>,
    pub padding: [gpointer; 32],
}

#[repr(C)]
pub struct WPEView {
    pub parent_instance: GObject,
    pub priv_: *mut c_void,
}

#[repr(C)]
pub struct WPEViewClass {
    pub parent_class: GObjectClass,
    pub buffers_changed: Option<unsafe extern "C" fn(*mut WPEView, *mut *mut WPEBuffer, c_uint)>,
    pub render_buffer: Option<
        unsafe extern "C" fn(
            *mut WPEView,
            *mut WPEBuffer,
            *const WPERectangle,
            c_uint,
            *mut *mut GError,
        ) -> gboolean,
    >,
    pub lock_pointer: Option<unsafe extern "C" fn(*mut WPEView) -> gboolean>,
    pub unlock_pointer: Option<unsafe extern "C" fn(*mut WPEView) -> gboolean>,
    pub set_cursor_from_name: Option<unsafe extern "C" fn(*mut WPEView, *const c_char)>,
    pub set_cursor_from_bytes: Option<
        unsafe extern "C" fn(*mut WPEView, *mut GBytes, c_uint, c_uint, c_uint, c_uint, c_uint),
    >,
    pub set_opaque_rectangles:
        Option<unsafe extern "C" fn(*mut WPEView, *mut WPERectangle, c_uint)>,
    pub can_be_mapped: Option<unsafe extern "C" fn(*mut WPEView) -> gboolean>,
    pub get_accessible: Option<unsafe extern "C" fn(*mut WPEView) -> gpointer>,
    pub padding: [gpointer; 32],
}

#[repr(C)]
pub struct WPEToplevel {
    pub parent_instance: GObject,
    pub priv_: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WPERectangle {
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
}

// WPEEventType
pub const WPE_EVENT_POINTER_DOWN: c_uint = 1;
pub const WPE_EVENT_POINTER_UP: c_uint = 2;
pub const WPE_EVENT_POINTER_MOVE: c_uint = 3;
pub const WPE_EVENT_POINTER_ENTER: c_uint = 4;
pub const WPE_EVENT_POINTER_LEAVE: c_uint = 5;
pub const WPE_EVENT_KEYBOARD_KEY_DOWN: c_uint = 7;
pub const WPE_EVENT_KEYBOARD_KEY_UP: c_uint = 8;

// WPEInputSource
pub const WPE_INPUT_SOURCE_MOUSE: c_uint = 0;
pub const WPE_INPUT_SOURCE_KEYBOARD: c_uint = 2;

// WPEModifiers
pub const WPE_MODIFIER_KEYBOARD_CONTROL: c_uint = 1 << 0;
pub const WPE_MODIFIER_KEYBOARD_SHIFT: c_uint = 1 << 1;
pub const WPE_MODIFIER_KEYBOARD_ALT: c_uint = 1 << 2;
pub const WPE_MODIFIER_KEYBOARD_META: c_uint = 1 << 3;
pub const WPE_MODIFIER_KEYBOARD_CAPS_LOCK: c_uint = 1 << 4;
pub const WPE_MODIFIER_POINTER_BUTTON1: c_uint = 1 << 8;

// WPEToplevelState
pub const WPE_TOPLEVEL_STATE_ACTIVE: c_uint = 1 << 2;

// WPEBufferFormatUsage
pub const WPE_BUFFER_FORMAT_USAGE_RENDERING: c_uint = 0;

// WPEPixelFormat
pub const WPE_PIXEL_FORMAT_ARGB8888: c_uint = 0;

// WebKitLoadEvent
pub const WEBKIT_LOAD_STARTED: c_uint = 0;
pub const WEBKIT_LOAD_COMMITTED: c_uint = 2;
pub const WEBKIT_LOAD_FINISHED: c_uint = 3;

// WebKitWebProcessTerminationReason
pub const WEBKIT_WEB_PROCESS_CRASHED: c_uint = 0;
pub const WEBKIT_WEB_PROCESS_EXCEEDED_MEMORY_LIMIT: c_uint = 1;

pub type GAsyncReadyCallback =
    Option<unsafe extern "C" fn(*mut GObject, *mut GAsyncResult, gpointer)>;

#[link(name = "WPEWebKit-2.0")]
unsafe extern "C" {
    // WPEDisplay
    pub fn wpe_display_get_type() -> GType;
    pub fn wpe_display_set_primary(display: *mut WPEDisplay);
    pub fn wpe_display_connect(display: *mut WPEDisplay, error: *mut *mut GError) -> gboolean;

    // WPEView
    pub fn wpe_view_get_type() -> GType;
    pub fn wpe_view_set_toplevel(view: *mut WPEView, toplevel: *mut WPEToplevel);
    pub fn wpe_view_resized(view: *mut WPEView, width: c_int, height: c_int);
    pub fn wpe_view_map(view: *mut WPEView);
    pub fn wpe_view_unmap(view: *mut WPEView);
    pub fn wpe_view_focus_in(view: *mut WPEView);
    pub fn wpe_view_focus_out(view: *mut WPEView);
    pub fn wpe_view_buffer_rendered(view: *mut WPEView, buffer: *mut WPEBuffer);
    pub fn wpe_view_buffer_released(view: *mut WPEView, buffer: *mut WPEBuffer);
    pub fn wpe_view_event(view: *mut WPEView, event: *mut WPEEvent);
    pub fn wpe_view_compute_press_count(
        view: *mut WPEView,
        x: c_double,
        y: c_double,
        button: c_uint,
        time: u32,
    ) -> c_uint;

    // WPEToplevel
    pub fn wpe_toplevel_get_type() -> GType;
    pub fn wpe_toplevel_resized(toplevel: *mut WPEToplevel, width: c_int, height: c_int);
    pub fn wpe_toplevel_scale_changed(toplevel: *mut WPEToplevel, scale: c_double);
    pub fn wpe_toplevel_state_changed(toplevel: *mut WPEToplevel, state: c_uint);

    // WPEBuffer
    pub fn wpe_buffer_get_width(buffer: *mut WPEBuffer) -> c_int;
    pub fn wpe_buffer_get_height(buffer: *mut WPEBuffer) -> c_int;
    pub fn wpe_buffer_set_user_data(
        buffer: *mut WPEBuffer,
        data: gpointer,
        destroy: GDestroyNotify,
    );
    pub fn wpe_buffer_get_user_data(buffer: *mut WPEBuffer) -> gpointer;
    pub fn wpe_buffer_dma_buf_get_type() -> GType;
    pub fn wpe_buffer_dma_buf_get_format(buffer: *mut WPEBuffer) -> u32;
    pub fn wpe_buffer_dma_buf_get_n_planes(buffer: *mut WPEBuffer) -> u32;
    pub fn wpe_buffer_dma_buf_get_fd(buffer: *mut WPEBuffer, plane: u32) -> c_int;
    pub fn wpe_buffer_dma_buf_get_offset(buffer: *mut WPEBuffer, plane: u32) -> u32;
    pub fn wpe_buffer_dma_buf_get_stride(buffer: *mut WPEBuffer, plane: u32) -> u32;
    pub fn wpe_buffer_dma_buf_get_modifier(buffer: *mut WPEBuffer) -> u64;
    pub fn wpe_buffer_shm_get_format(buffer: *mut WPEBuffer) -> c_uint;
    pub fn wpe_buffer_shm_get_data(buffer: *mut WPEBuffer) -> *mut GBytes;
    pub fn wpe_buffer_shm_get_stride(buffer: *mut WPEBuffer) -> c_uint;

    // Formats and devices
    pub fn wpe_buffer_formats_builder_new(
        device: *mut WPEDRMDevice,
    ) -> *mut WPEBufferFormatsBuilder;
    pub fn wpe_buffer_formats_builder_append_group(
        builder: *mut WPEBufferFormatsBuilder,
        device: *mut WPEDRMDevice,
        usage: c_uint,
    );
    pub fn wpe_buffer_formats_builder_append_format(
        builder: *mut WPEBufferFormatsBuilder,
        format: u32,
        modifier: u64,
    );
    pub fn wpe_buffer_formats_builder_end(
        builder: *mut WPEBufferFormatsBuilder,
    ) -> *mut WPEBufferFormats;
    pub fn wpe_buffer_formats_get_n_groups(formats: *mut WPEBufferFormats) -> c_uint;
    pub fn wpe_buffer_formats_get_group_n_formats(
        formats: *mut WPEBufferFormats,
        group: c_uint,
    ) -> c_uint;
    pub fn wpe_buffer_formats_get_format_modifiers(
        formats: *mut WPEBufferFormats,
        group: c_uint,
        format: c_uint,
    ) -> *mut GArray;
    pub fn wpe_drm_device_new(primary: *const c_char, render: *const c_char) -> *mut WPEDRMDevice;
    pub fn wpe_drm_device_unref(device: *mut WPEDRMDevice);

    // WPEEvent
    pub fn wpe_event_unref(event: *mut WPEEvent);
    pub fn wpe_event_pointer_button_new(
        kind: c_uint,
        view: *mut WPEView,
        source: c_uint,
        time: u32,
        modifiers: c_uint,
        button: c_uint,
        x: c_double,
        y: c_double,
        press_count: c_uint,
    ) -> *mut WPEEvent;
    pub fn wpe_event_pointer_move_new(
        kind: c_uint,
        view: *mut WPEView,
        source: c_uint,
        time: u32,
        modifiers: c_uint,
        x: c_double,
        y: c_double,
        delta_x: c_double,
        delta_y: c_double,
    ) -> *mut WPEEvent;
    pub fn wpe_event_scroll_new(
        view: *mut WPEView,
        source: c_uint,
        time: u32,
        modifiers: c_uint,
        delta_x: c_double,
        delta_y: c_double,
        precise_deltas: gboolean,
        is_stop: gboolean,
        x: c_double,
        y: c_double,
    ) -> *mut WPEEvent;
    pub fn wpe_event_keyboard_new(
        kind: c_uint,
        view: *mut WPEView,
        source: c_uint,
        time: u32,
        modifiers: c_uint,
        keycode: c_uint,
        keyval: c_uint,
    ) -> *mut WPEEvent;

    // WebKitWebView
    pub fn webkit_network_session_new_ephemeral() -> *mut GObject;
    pub fn webkit_web_view_get_type() -> GType;
    pub fn webkit_web_view_get_wpe_view(web_view: *mut WebKitWebView) -> *mut WPEView;
    pub fn webkit_web_view_load_uri(web_view: *mut WebKitWebView, uri: *const c_char);
    pub fn webkit_web_view_reload(web_view: *mut WebKitWebView);
    pub fn webkit_web_view_stop_loading(web_view: *mut WebKitWebView);
    pub fn webkit_web_view_go_back(web_view: *mut WebKitWebView);
    pub fn webkit_web_view_go_forward(web_view: *mut WebKitWebView);
    pub fn webkit_web_view_can_go_back(web_view: *mut WebKitWebView) -> gboolean;
    pub fn webkit_web_view_can_go_forward(web_view: *mut WebKitWebView) -> gboolean;
    pub fn webkit_web_view_get_uri(web_view: *mut WebKitWebView) -> *const c_char;
    pub fn webkit_web_view_get_title(web_view: *mut WebKitWebView) -> *const c_char;
    pub fn webkit_web_view_is_loading(web_view: *mut WebKitWebView) -> gboolean;
    pub fn webkit_web_view_set_zoom_level(web_view: *mut WebKitWebView, zoom: c_double);
    pub fn webkit_web_view_terminate_web_process(web_view: *mut WebKitWebView);
    pub fn webkit_web_view_evaluate_javascript(
        web_view: *mut WebKitWebView,
        script: *const c_char,
        length: isize,
        world_name: *const c_char,
        source_uri: *const c_char,
        cancellable: gpointer,
        callback: GAsyncReadyCallback,
        user_data: gpointer,
    );
    pub fn webkit_web_view_evaluate_javascript_finish(
        web_view: *mut WebKitWebView,
        result: *mut GAsyncResult,
        error: *mut *mut GError,
    ) -> *mut JSCValue;
    pub fn jsc_value_to_string(value: *mut JSCValue) -> *mut c_char;
    pub fn jsc_options_set_boolean(option: *const c_char, value: gboolean) -> gboolean;
    pub fn jsc_options_get_boolean(option: *const c_char, value: *mut gboolean) -> gboolean;
}

/// Whether `instance` is a `type_` (`G_TYPE_CHECK_INSTANCE_TYPE`).
///
/// # Safety
///
/// `instance` must be a live GObject instance.
pub unsafe fn is_a<T>(instance: *mut T, type_: GType) -> bool {
    unsafe {
        glib::gobject_ffi::g_type_check_instance_is_a(instance.cast(), type_) != glib::ffi::GFALSE
    }
}
