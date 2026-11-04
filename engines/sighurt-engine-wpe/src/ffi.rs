//! The C API of WPE WebKit 2.52 that this crate calls: the WPE Platform API (displays, views,
//! buffers, events, the clipboard, input methods) and WebKit's `WebKitWebView` with the objects
//! its signals hand over. Written by hand from the installed headers
//! (`$SIGHURT_WPE_PREFIX/include/wpe-webkit-2.0`); the class structs mirror the C layouts exactly,
//! since GObject subclasses are allocated and initialized from them. What WebKit exposes as
//! GObject properties is read through glib-rs instead of being declared here.
//!
//! The library is not linked: [`load`] opens it when the runtime first starts (`dlopen`, by the
//! path the build script found), and each function here calls through the address it found. So
//! a program with the engine compiled in maps WebKit, and the seventy libraries only it needs
//! (GStreamer, libsoup, ICU, speech synthesis, ...), only once a WPE page is opened; every
//! function but `load` panics before that.
//!
//! Must not own: any logic. Everything here is a declaration, and the loading.

#![allow(non_camel_case_types, non_snake_case)]

use std::ffi::{c_char, c_double, c_int, c_uint, c_void};
use std::sync::OnceLock;

use glib::ffi::{
    gboolean, gpointer, GArray, GBytes, GDestroyNotify, GError, GList, GPtrArray, GType,
};
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
    WPEClipboardContent,
    WPEDRMDevice,
    WPEEvent,
    WPEKeymap,
    WebKitAuthenticationRequest,
    WebKitBackForwardList,
    WebKitBackForwardListItem,
    WebKitDownload,
    WebKitFileChooserRequest,
    WebKitFindController,
    WebKitHitTestResult,
    WebKitNavigationAction,
    WebKitPermissionRequest,
    WebKitPolicyDecision,
    WebKitScriptDialog,
    WebKitURIRequest,
    WebKitURIResponse,
    WebKitUserContentManager,
    WebKitWebView,
    JSCValue,
    GAsyncResult,
);

/// `WPE_DECLARE_DERIVABLE_TYPE` instance structs: the parent and a private pointer.
macro_rules! instance {
    ($($name:ident),* $(,)?) => {
        $(#[repr(C)] pub struct $name { pub parent_instance: GObject, pub priv_: *mut c_void })*
    };
}

instance!(
    WPEDisplay,
    WPEView,
    WPEToplevel,
    WPEClipboard,
    WPEInputMethodContext
);

#[repr(C)]
pub struct WPEDisplayClass {
    pub parent_class: GObjectClass,
    pub connect: Option<unsafe extern "C" fn(*mut WPEDisplay, *mut *mut GError) -> gboolean>,
    pub create_view: Option<unsafe extern "C" fn(*mut WPEDisplay) -> *mut WPEView>,
    pub get_egl_display:
        Option<unsafe extern "C" fn(*mut WPEDisplay, *mut *mut GError) -> gpointer>,
    pub get_keymap: Option<unsafe extern "C" fn(*mut WPEDisplay) -> *mut WPEKeymap>,
    pub get_clipboard: Option<unsafe extern "C" fn(*mut WPEDisplay) -> *mut WPEClipboard>,
    pub get_preferred_buffer_formats:
        Option<unsafe extern "C" fn(*mut WPEDisplay) -> *mut WPEBufferFormats>,
    pub get_n_screens: Option<unsafe extern "C" fn(*mut WPEDisplay) -> c_uint>,
    pub get_screen: Option<unsafe extern "C" fn(*mut WPEDisplay, c_uint) -> gpointer>,
    pub get_drm_device: Option<unsafe extern "C" fn(*mut WPEDisplay) -> *mut WPEDRMDevice>,
    pub use_explicit_sync: Option<unsafe extern "C" fn(*mut WPEDisplay) -> gboolean>,
    pub create_input_method_context:
        Option<unsafe extern "C" fn(*mut WPEDisplay, *mut WPEView) -> *mut WPEInputMethodContext>,
    pub create_toplevel: Option<unsafe extern "C" fn(*mut WPEDisplay, c_uint) -> *mut WPEToplevel>,
    pub create_gamepad_manager: Option<unsafe extern "C" fn(*mut WPEDisplay) -> gpointer>,
    pub padding: [gpointer; 32],
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
pub struct WPEClipboardClass {
    pub parent_class: GObjectClass,
    pub read: Option<unsafe extern "C" fn(*mut WPEClipboard, *const c_char) -> *mut GBytes>,
    pub changed: Option<
        unsafe extern "C" fn(*mut WPEClipboard, *mut GPtrArray, gboolean, *mut WPEClipboardContent),
    >,
    pub padding: [gpointer; 32],
}

#[repr(C)]
pub struct WPEInputMethodContextClass {
    pub parent_class: GObjectClass,
    pub get_preedit_string: Option<
        unsafe extern "C" fn(
            *mut WPEInputMethodContext,
            *mut *mut c_char,
            *mut *mut GList,
            *mut c_uint,
        ),
    >,
    pub filter_key_event:
        Option<unsafe extern "C" fn(*mut WPEInputMethodContext, *mut WPEEvent) -> gboolean>,
    pub focus_in: Option<unsafe extern "C" fn(*mut WPEInputMethodContext)>,
    pub focus_out: Option<unsafe extern "C" fn(*mut WPEInputMethodContext)>,
    pub set_cursor_area:
        Option<unsafe extern "C" fn(*mut WPEInputMethodContext, c_int, c_int, c_int, c_int)>,
    pub set_surrounding: Option<
        unsafe extern "C" fn(*mut WPEInputMethodContext, *const c_char, c_uint, c_uint, c_uint),
    >,
    pub reset: Option<unsafe extern "C" fn(*mut WPEInputMethodContext)>,
    pub padding: [gpointer; 32],
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

// WPEInputPurpose
pub const WPE_INPUT_PURPOSE_DIGITS: c_uint = 2;
pub const WPE_INPUT_PURPOSE_NUMBER: c_uint = 3;
pub const WPE_INPUT_PURPOSE_PHONE: c_uint = 4;
pub const WPE_INPUT_PURPOSE_URL: c_uint = 5;
pub const WPE_INPUT_PURPOSE_EMAIL: c_uint = 6;
pub const WPE_INPUT_PURPOSE_PASSWORD: c_uint = 8;
pub const WPE_INPUT_PURPOSE_PIN: c_uint = 9;

// WebKitLoadEvent
pub const WEBKIT_LOAD_STARTED: c_uint = 0;
pub const WEBKIT_LOAD_REDIRECTED: c_uint = 1;
pub const WEBKIT_LOAD_COMMITTED: c_uint = 2;

// WebKitFindOptions
pub const WEBKIT_FIND_OPTIONS_CASE_INSENSITIVE: c_uint = 1 << 0;
pub const WEBKIT_FIND_OPTIONS_WRAP_AROUND: c_uint = 1 << 4;

// WebKitPolicyDecisionType
pub const WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION: c_uint = 0;
pub const WEBKIT_POLICY_DECISION_TYPE_RESPONSE: c_uint = 2;

// WebKitNavigationType
pub const WEBKIT_NAVIGATION_TYPE_LINK_CLICKED: c_uint = 0;
pub const WEBKIT_NAVIGATION_TYPE_FORM_SUBMITTED: c_uint = 1;

// WebKitScriptDialogType
pub const WEBKIT_SCRIPT_DIALOG_BEFORE_UNLOAD_CONFIRM: c_uint = 3;

// WebKitCacheModel
pub const WEBKIT_CACHE_MODEL_DOCUMENT_BROWSER: c_uint = 2;

// WebKitCookiePersistentStorage. SQLite is the Mozilla cookie-jar format.
pub const WEBKIT_COOKIE_PERSISTENT_STORAGE_SQLITE: c_uint = 1;

// WebKitCookieAcceptPolicy. Only cookies set by the main document.
pub const WEBKIT_COOKIE_POLICY_ACCEPT_NO_THIRD_PARTY: c_uint = 2;

// WebKitWebProcessTerminationReason
pub const WEBKIT_WEB_PROCESS_CRASHED: c_uint = 0;
pub const WEBKIT_WEB_PROCESS_EXCEEDED_MEMORY_LIMIT: c_uint = 1;

// Error codes that end a load without it being an error to show.
pub const WEBKIT_NETWORK_ERROR_CANCELLED: c_int = 302;
pub const WEBKIT_POLICY_ERROR_FRAME_LOAD_INTERRUPTED_BY_POLICY_CHANGE: c_int = 102;

/// `SOUP_STATUS_NO_CONTENT`: a response that keeps the current document.
pub const HTTP_NO_CONTENT: c_uint = 204;

pub type GAsyncReadyCallback =
    Option<unsafe extern "C" fn(*mut GObject, *mut GAsyncResult, gpointer)>;

/// Declares WPE WebKit's functions: a table of their addresses, filled by [`load`], and a
/// function of the same name and signature for each, which calls through it.
macro_rules! functions {
    ($(pub fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;)*) => {
        struct Functions {
            $($name: unsafe extern "C" fn($($ty),*) $(-> $ret)?,)*
        }

        static FUNCTIONS: OnceLock<Functions> = OnceLock::new();

        /// Opens WPE WebKit's library and finds every function this crate calls, once; fails
        /// with what is missing.
        pub fn load() -> Result<(), String> {
            if FUNCTIONS.get().is_some() {
                return Ok(());
            }
            let library = open(LIBRARY)?;
            let functions = Functions {
                $($name: unsafe {
                    std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($ty),*) $(-> $ret)?>(
                        symbol(library, concat!(stringify!($name), "\0"))?,
                    )
                },)*
            };
            let _ = FUNCTIONS.set(functions);
            Ok(())
        }

        $(
            /// WPE WebKit's function of this name.
            ///
            /// # Safety
            ///
            /// As the C function's; [`load`] must have succeeded.
            #[allow(clippy::too_many_arguments)] // C's signature.
            pub unsafe fn $name($($arg: $ty),*) $(-> $ret)? {
                let functions = FUNCTIONS.get().expect("WPE WebKit is loaded (ffi::load)");
                unsafe { (functions.$name)($($arg),*) }
            }
        )*
    };
}

/// WPE WebKit's library, as the build script found it.
const LIBRARY: &str = concat!(env!("SIGHURT_WPE_LIBRARY"), "\0");

/// Opens the library at `path` (NUL-terminated), resolving everything it needs now.
fn open(path: &str) -> Result<*mut c_void, String> {
    let library = unsafe { libc::dlopen(path.as_ptr().cast(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if library.is_null() {
        return Err(format!("WPE WebKit can't be loaded: {}", dl_error()));
    }
    Ok(library)
}

/// The address of `name` (NUL-terminated) in `library`.
fn symbol(library: *mut c_void, name: &str) -> Result<*mut c_void, String> {
    let address = unsafe { libc::dlsym(library, name.as_ptr().cast()) };
    match address.is_null() {
        true => Err(format!("WPE WebKit has no {}", name.trim_end_matches('\0'))),
        false => Ok(address),
    }
}

/// What the dynamic loader last said went wrong.
fn dl_error() -> String {
    let error = unsafe { libc::dlerror() };
    match error.is_null() {
        true => "unknown error".into(),
        false => unsafe { std::ffi::CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned(),
    }
}

functions! {
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

    // WPEClipboard
    pub fn wpe_clipboard_get_type() -> GType;
    pub fn wpe_clipboard_content_get_text(content: *mut WPEClipboardContent) -> *const c_char;
    pub fn wpe_clipboard_content_get_bytes(
        content: *mut WPEClipboardContent,
        format: *const c_char,
    ) -> *mut GBytes;

    // WPEInputMethodContext
    pub fn wpe_input_method_context_get_type() -> GType;
    pub fn wpe_input_method_context_get_input_purpose(
        context: *mut WPEInputMethodContext,
    ) -> c_uint;

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
    pub fn webkit_network_session_new(
        data_directory: *const c_char,
        cache_directory: *const c_char,
    ) -> *mut GObject;
    pub fn webkit_network_session_new_ephemeral() -> *mut GObject;
    pub fn webkit_network_session_get_cookie_manager(session: *mut GObject) -> *mut GObject;
    pub fn webkit_network_session_set_persistent_credential_storage_enabled(
        session: *mut GObject,
        enabled: gboolean,
    );
    pub fn webkit_cookie_manager_set_persistent_storage(
        manager: *mut GObject,
        filename: *const c_char,
        storage: c_uint,
    );
    pub fn webkit_cookie_manager_set_accept_policy(manager: *mut GObject, policy: c_uint);
    pub fn webkit_web_view_get_type() -> GType;
    pub fn webkit_web_view_get_wpe_view(web_view: *mut WebKitWebView) -> *mut WPEView;
    pub fn webkit_web_view_get_settings(web_view: *mut WebKitWebView) -> *mut GObject;
    pub fn webkit_web_view_load_uri(web_view: *mut WebKitWebView, uri: *const c_char);
    pub fn webkit_web_view_reload(web_view: *mut WebKitWebView);
    pub fn webkit_web_view_stop_loading(web_view: *mut WebKitWebView);
    pub fn webkit_web_view_get_back_forward_list(
        web_view: *mut WebKitWebView,
    ) -> *mut WebKitBackForwardList;
    pub fn webkit_web_view_go_to_back_forward_list_item(
        web_view: *mut WebKitWebView,
        item: *mut WebKitBackForwardListItem,
    );
    pub fn webkit_web_view_get_uri(web_view: *mut WebKitWebView) -> *const c_char;
    pub fn webkit_web_view_get_title(web_view: *mut WebKitWebView) -> *const c_char;
    pub fn webkit_web_view_is_loading(web_view: *mut WebKitWebView) -> gboolean;
    pub fn webkit_web_view_set_zoom_level(web_view: *mut WebKitWebView, zoom: c_double);
    pub fn webkit_web_view_get_find_controller(
        web_view: *mut WebKitWebView,
    ) -> *mut WebKitFindController;
    pub fn webkit_find_controller_search(
        find_controller: *mut WebKitFindController,
        search_text: *const c_char,
        find_options: c_uint,
        max_match_count: c_uint,
    );
    pub fn webkit_find_controller_search_next(find_controller: *mut WebKitFindController);
    pub fn webkit_find_controller_search_previous(find_controller: *mut WebKitFindController);
    pub fn webkit_find_controller_search_finish(find_controller: *mut WebKitFindController);
    pub fn webkit_web_view_execute_editing_command(
        web_view: *mut WebKitWebView,
        command: *const c_char,
    );
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
    pub fn webkit_web_view_get_user_content_manager(
        web_view: *mut WebKitWebView,
    ) -> *mut WebKitUserContentManager;
    pub fn webkit_user_content_manager_register_script_message_handler(
        manager: *mut WebKitUserContentManager,
        name: *const c_char,
        world_name: *const c_char,
    ) -> gboolean;

    // The back-forward list
    pub fn webkit_back_forward_list_get_back_list(list: *mut WebKitBackForwardList) -> *mut GList;
    pub fn webkit_back_forward_list_get_forward_list(
        list: *mut WebKitBackForwardList,
    ) -> *mut GList;
    pub fn webkit_back_forward_list_get_nth_item(
        list: *mut WebKitBackForwardList,
        index: c_int,
    ) -> *mut WebKitBackForwardListItem;
    pub fn webkit_back_forward_list_item_get_uri(
        item: *mut WebKitBackForwardListItem,
    ) -> *const c_char;

    // Policy decisions
    pub fn webkit_policy_decision_use(decision: *mut WebKitPolicyDecision);
    pub fn webkit_policy_decision_ignore(decision: *mut WebKitPolicyDecision);
    pub fn webkit_policy_decision_download(decision: *mut WebKitPolicyDecision);
    pub fn webkit_navigation_policy_decision_get_navigation_action(
        decision: *mut WebKitPolicyDecision,
    ) -> *mut WebKitNavigationAction;
    pub fn webkit_navigation_action_get_request(
        action: *mut WebKitNavigationAction,
    ) -> *mut WebKitURIRequest;
    pub fn webkit_navigation_action_get_navigation_type(
        action: *mut WebKitNavigationAction,
    ) -> c_uint;
    pub fn webkit_navigation_action_is_user_gesture(
        action: *mut WebKitNavigationAction,
    ) -> gboolean;
    pub fn webkit_navigation_action_is_redirect(action: *mut WebKitNavigationAction) -> gboolean;
    pub fn webkit_uri_request_get_uri(request: *mut WebKitURIRequest) -> *const c_char;
    pub fn webkit_uri_request_get_http_method(request: *mut WebKitURIRequest) -> *const c_char;
    pub fn webkit_response_policy_decision_get_response(
        decision: *mut WebKitPolicyDecision,
    ) -> *mut WebKitURIResponse;
    pub fn webkit_response_policy_decision_is_mime_type_supported(
        decision: *mut WebKitPolicyDecision,
    ) -> gboolean;
    pub fn webkit_uri_response_get_uri(response: *mut WebKitURIResponse) -> *const c_char;
    pub fn webkit_uri_response_get_status_code(response: *mut WebKitURIResponse) -> c_uint;
    pub fn webkit_uri_response_get_suggested_filename(
        response: *mut WebKitURIResponse,
    ) -> *const c_char;

    // Requests a page makes
    pub fn webkit_permission_request_allow(request: *mut WebKitPermissionRequest);
    pub fn webkit_permission_request_deny(request: *mut WebKitPermissionRequest);
    pub fn webkit_user_media_permission_is_for_audio_device(
        request: *mut WebKitPermissionRequest,
    ) -> gboolean;
    pub fn webkit_user_media_permission_is_for_video_device(
        request: *mut WebKitPermissionRequest,
    ) -> gboolean;
    pub fn webkit_user_media_permission_is_for_display_device(
        request: *mut WebKitPermissionRequest,
    ) -> gboolean;
    pub fn webkit_file_chooser_request_get_mime_types(
        request: *mut WebKitFileChooserRequest,
    ) -> *const *const c_char;
    pub fn webkit_file_chooser_request_get_select_multiple(
        request: *mut WebKitFileChooserRequest,
    ) -> gboolean;
    pub fn webkit_file_chooser_request_select_files(
        request: *mut WebKitFileChooserRequest,
        files: *const *const c_char,
    );
    pub fn webkit_file_chooser_request_cancel(request: *mut WebKitFileChooserRequest);
    pub fn webkit_download_get_web_view(download: *mut WebKitDownload) -> *mut WebKitWebView;
    pub fn webkit_download_get_request(download: *mut WebKitDownload) -> *mut WebKitURIRequest;
    pub fn webkit_download_get_response(download: *mut WebKitDownload) -> *mut WebKitURIResponse;
    pub fn webkit_download_cancel(download: *mut WebKitDownload);
    pub fn webkit_hit_test_result_context_is_link(result: *mut WebKitHitTestResult) -> gboolean;
    pub fn webkit_hit_test_result_get_link_uri(result: *mut WebKitHitTestResult) -> *const c_char;
    pub fn webkit_authentication_request_cancel(request: *mut WebKitAuthenticationRequest);
    pub fn webkit_script_dialog_get_dialog_type(dialog: *mut WebKitScriptDialog) -> c_uint;
    pub fn webkit_script_dialog_confirm_set_confirmed(
        dialog: *mut WebKitScriptDialog,
        confirmed: gboolean,
    );
    pub fn webkit_web_context_get_default() -> *mut GObject;
    pub fn webkit_web_context_new() -> *mut GObject;
    pub fn webkit_web_context_set_cache_model(context: *mut GObject, model: c_uint);
    pub fn webkit_web_context_get_geolocation_manager(context: *mut GObject) -> *mut GObject;
    pub fn webkit_geolocation_manager_failed(manager: *mut GObject, message: *const c_char);

    // JavaScriptCore
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

/// The name of `instance`'s type, such as "WebKitGeolocationPermissionRequest".
///
/// # Safety
///
/// `instance` must be a live GObject instance.
pub unsafe fn type_name<T>(instance: *mut T) -> String {
    unsafe {
        let class = (*instance.cast::<glib::gobject_ffi::GTypeInstance>()).g_class;
        let name = glib::gobject_ffi::g_type_name((*class).g_type);
        std::ffi::CStr::from_ptr(name)
            .to_string_lossy()
            .into_owned()
    }
}

/// A C string WebKit owns, copied; `None` for null.
///
/// # Safety
///
/// `s` must be null or a valid NUL-terminated string.
pub unsafe fn string(s: *const c_char) -> Option<String> {
    (!s.is_null()).then(|| {
        unsafe { std::ffi::CStr::from_ptr(s) }
            .to_string_lossy()
            .into_owned()
    })
}
