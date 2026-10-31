//! CEF 150 C API layouts this crate calls.
//!
//! Generated from the pinned headers (`CEF_API_VERSION` 15000) and checked
//! against `sizeof`. Vtable slots this crate does not fill stay null; CEF
//! skips a null slot. The slots it fills are real functions.
//!
//! Owns the C shapes. Must not own a CEF object or call one.

#![allow(non_camel_case_types)]
#![allow(clippy::upper_case_acronyms)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(dead_code)]
#![allow(missing_docs)]
#![allow(non_snake_case)]

use std::os::raw::c_void;

use libc::size_t;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefStringUtf16 {
    pub str_: *mut u16,
    pub length: usize,
    pub dtor: Option<unsafe extern "C" fn(*mut u16)>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefSize {
    pub width: i32,
    pub height: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefPoint {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefRange {
    pub from: i32,
    pub to: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CefInsets {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
}

#[repr(C)]
pub struct CefBaseRefCounted {
    pub size: size_t,
    pub add_ref: Option<unsafe extern "C" fn(*mut CefBaseRefCounted)>,
    pub release: Option<unsafe extern "C" fn(*mut CefBaseRefCounted) -> i32>,
    pub has_one_ref: Option<unsafe extern "C" fn(*mut CefBaseRefCounted) -> i32>,
    pub has_at_least_one_ref: Option<unsafe extern "C" fn(*mut CefBaseRefCounted) -> i32>,
}

#[repr(C)]
pub struct CefMainArgs {
    pub argc: i32,
    pub _pad: i32,
    pub argv: *mut *mut libc::c_char,
}

#[repr(C)]
pub struct CefMouseEvent {
    pub x: i32,
    pub y: i32,
    pub modifiers: u32,
}

#[repr(C)]
pub struct CefKeyEvent {
    pub size: size_t,
    pub type_: i32,
    pub modifiers: u32,
    pub windows_key_code: i32,
    pub native_key_code: i32,
    pub is_system_key: i32,
    pub character: u16,
    pub unmodified_character: u16,
    pub focus_on_editable_field: i32,
    pub _pad: i32,
}

#[repr(C)]
pub struct CefCursorInfo {
    pub hotspot: CefPoint,
    pub image_scale_factor: f32,
    pub buffer: *mut c_void,
    pub size: CefSize,
}

#[repr(C)]
pub struct CefScreenInfo {
    pub size: size_t,
    pub device_scale_factor: f32,
    pub depth: i32,
    pub depth_per_component: i32,
    pub is_monochrome: i32,
    pub rect: CefRect,
    pub available_rect: CefRect,
}

/// `cef_window_info_t` for Linux. 88 bytes at API 15000.
#[repr(C)]
pub struct CefWindowInfo {
    pub size: size_t,
    pub window_name: CefStringUtf16,
    pub bounds: CefRect,
    pub parent_window: usize,
    pub windowless_rendering_enabled: i32,
    pub shared_texture_enabled: i32,
    pub external_begin_frame_enabled: i32,
    pub _pad: i32,
    pub window: usize,
    pub runtime_style: i32,
    pub _pad2: i32,
}

/// `cef_settings_t` at API 15000. 448 bytes. Strings are `cef_string_utf16_t`.
#[repr(C)]
pub struct CefSettings {
    pub size: size_t,
    pub no_sandbox: i32,
    pub _pad0: i32,
    pub browser_subprocess_path: CefStringUtf16,
    pub framework_dir_path: CefStringUtf16,
    pub main_bundle_path: CefStringUtf16,
    pub multi_threaded_message_loop: i32,
    pub external_message_pump: i32,
    pub windowless_rendering_enabled: i32,
    pub command_line_args_disabled: i32,
    pub cache_path: CefStringUtf16,
    pub root_cache_path: CefStringUtf16,
    pub persist_session_cookies: i32,
    pub _pad1: i32,
    pub user_agent: CefStringUtf16,
    pub user_agent_product: CefStringUtf16,
    pub locale: CefStringUtf16,
    pub log_file: CefStringUtf16,
    pub log_severity: i32,
    pub log_items: i32,
    pub javascript_flags: CefStringUtf16,
    pub resources_dir_path: CefStringUtf16,
    pub locales_dir_path: CefStringUtf16,
    pub remote_debugging_port: i32,
    pub uncaught_exception_stack_size: i32,
    pub background_color: u32,
    pub _pad2: i32,
    pub accept_language_list: CefStringUtf16,
    pub cookieable_schemes_list: CefStringUtf16,
    pub cookieable_schemes_exclude_defaults: i32,
    pub _pad3: i32,
    pub chrome_policy_id: CefStringUtf16,
    pub chrome_app_icon_id: i32,
    pub disable_signal_handlers: i32,
    pub use_views_default_popup: i32,
    pub _pad4: i32,
}

/// The first field is the frame rate. The rest stays zero (`STATE_DEFAULT`).
#[repr(C)]
pub struct CefBrowserSettings {
    pub size: size_t,
    pub windowless_frame_rate: i32,
    pub _rest: [u8; 252],
}

const _: () = {
    assert!(std::mem::size_of::<CefStringUtf16>() == 24);
    assert!(std::mem::size_of::<CefRect>() == 16);
    assert!(std::mem::size_of::<CefBaseRefCounted>() == 40);
    assert!(std::mem::size_of::<CefMainArgs>() == 16);
    assert!(std::mem::size_of::<CefMouseEvent>() == 12);
    assert!(std::mem::size_of::<CefKeyEvent>() == 40);
    assert!(std::mem::size_of::<CefScreenInfo>() == 56);
    assert!(std::mem::size_of::<CefWindowInfo>() == 88);
    assert!(std::mem::size_of::<CefSettings>() == 448);
    assert!(std::mem::size_of::<CefBrowserSettings>() == 264);
    // `windowless_frame_rate` is followed by 4 bytes of padding, so `_rest` starts at 12.
    // `background_color` is at C offset 248 (the 150 headers); runtime.rs writes it there.
    assert!(std::mem::offset_of!(CefBrowserSettings, _rest) == 12);
    assert!(248 + 4 <= std::mem::size_of::<CefBrowserSettings>());
};

#[repr(C)]
pub struct CefDevToolsMessageObserver {
    pub base: CefBaseRefCounted,
    pub on_dev_tools_message: Option<
        unsafe extern "C" fn(
            self_: *mut CefDevToolsMessageObserver,
            browser: *mut CefBrowser,
            message: *const c_void,
            message_size: usize,
        ) -> i32,
    >,
    pub on_dev_tools_method_result: Option<
        unsafe extern "C" fn(
            self_: *mut CefDevToolsMessageObserver,
            browser: *mut CefBrowser,
            message_id: i32,
            success: i32,
            result: *const c_void,
            result_size: usize,
        ) -> (),
    >,
    pub on_dev_tools_event: Option<
        unsafe extern "C" fn(
            self_: *mut CefDevToolsMessageObserver,
            browser: *mut CefBrowser,
            method: *const CefStringUtf16,
            params: *const c_void,
            params_size: usize,
        ) -> (),
    >,
    pub on_dev_tools_agent_attached: Option<
        unsafe extern "C" fn(
            self_: *mut CefDevToolsMessageObserver,
            browser: *mut CefBrowser,
        ) -> (),
    >,
    pub on_dev_tools_agent_detached: Option<
        unsafe extern "C" fn(
            self_: *mut CefDevToolsMessageObserver,
            browser: *mut CefBrowser,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefValue {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefValue) -> i32>,
    pub is_owned: Option<unsafe extern "C" fn(self_: *mut CefValue) -> i32>,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefValue) -> i32>,
    pub is_same: Option<unsafe extern "C" fn(self_: *mut CefValue, that: *mut CefValue) -> i32>,
    pub is_equal: Option<unsafe extern "C" fn(self_: *mut CefValue, that: *mut CefValue) -> i32>,
    pub copy: Option<unsafe extern "C" fn(self_: *mut CefValue) -> *mut CefValue>,
    pub get_type: Option<unsafe extern "C" fn(self_: *mut CefValue) -> i32>,
    pub get_bool: Option<unsafe extern "C" fn(self_: *mut CefValue) -> i32>,
    pub get_int: Option<unsafe extern "C" fn(self_: *mut CefValue) -> i32>,
    pub get_double: Option<unsafe extern "C" fn(self_: *mut CefValue) -> f64>,
    pub get_string: Option<unsafe extern "C" fn(self_: *mut CefValue) -> *mut CefStringUtf16>,
    pub get_binary: Option<unsafe extern "C" fn(self_: *mut CefValue) -> *mut CefBinaryValue>,
    pub get_dictionary:
        Option<unsafe extern "C" fn(self_: *mut CefValue) -> *mut CefDictionaryValue>,
    pub get_list: Option<unsafe extern "C" fn(self_: *mut CefValue) -> *mut CefListValue>,
    pub set_null: Option<unsafe extern "C" fn(self_: *mut CefValue) -> i32>,
    pub set_bool: Option<unsafe extern "C" fn(self_: *mut CefValue, value: i32) -> i32>,
    pub set_int: Option<unsafe extern "C" fn(self_: *mut CefValue, value: i32) -> i32>,
    pub set_double: Option<unsafe extern "C" fn(self_: *mut CefValue, value: f64) -> i32>,
    pub set_string:
        Option<unsafe extern "C" fn(self_: *mut CefValue, value: *const CefStringUtf16) -> i32>,
    pub set_binary:
        Option<unsafe extern "C" fn(self_: *mut CefValue, value: *mut CefBinaryValue) -> i32>,
    pub set_dictionary:
        Option<unsafe extern "C" fn(self_: *mut CefValue, value: *mut CefDictionaryValue) -> i32>,
    pub set_list:
        Option<unsafe extern "C" fn(self_: *mut CefValue, value: *mut CefListValue) -> i32>,
}

#[repr(C)]
pub struct CefBinaryValue {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefBinaryValue) -> i32>,
    pub is_owned: Option<unsafe extern "C" fn(self_: *mut CefBinaryValue) -> i32>,
    pub is_same:
        Option<unsafe extern "C" fn(self_: *mut CefBinaryValue, that: *mut CefBinaryValue) -> i32>,
    pub is_equal:
        Option<unsafe extern "C" fn(self_: *mut CefBinaryValue, that: *mut CefBinaryValue) -> i32>,
    pub copy: Option<unsafe extern "C" fn(self_: *mut CefBinaryValue) -> *mut CefBinaryValue>,
    pub get_raw_data: Option<unsafe extern "C" fn(self_: *mut CefBinaryValue) -> *const c_void>,
    pub get_size: Option<unsafe extern "C" fn(self_: *mut CefBinaryValue) -> usize>,
    pub get_data: Option<
        unsafe extern "C" fn(
            self_: *mut CefBinaryValue,
            buffer: *mut c_void,
            buffer_size: usize,
            data_offset: usize,
        ) -> usize,
    >,
}

#[repr(C)]
pub struct CefDictionaryValue {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefDictionaryValue) -> i32>,
    pub is_owned: Option<unsafe extern "C" fn(self_: *mut CefDictionaryValue) -> i32>,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefDictionaryValue) -> i32>,
    pub is_same: Option<
        unsafe extern "C" fn(self_: *mut CefDictionaryValue, that: *mut CefDictionaryValue) -> i32,
    >,
    pub is_equal: Option<
        unsafe extern "C" fn(self_: *mut CefDictionaryValue, that: *mut CefDictionaryValue) -> i32,
    >,
    pub copy: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            exclude_empty_children: i32,
        ) -> *mut CefDictionaryValue,
    >,
    pub get_size: Option<unsafe extern "C" fn(self_: *mut CefDictionaryValue) -> usize>,
    pub clear: Option<unsafe extern "C" fn(self_: *mut CefDictionaryValue) -> i32>,
    pub has_key: Option<
        unsafe extern "C" fn(self_: *mut CefDictionaryValue, key: *const CefStringUtf16) -> i32,
    >,
    pub get_keys:
        Option<unsafe extern "C" fn(self_: *mut CefDictionaryValue, keys: *mut c_void) -> i32>,
    pub remove: Option<
        unsafe extern "C" fn(self_: *mut CefDictionaryValue, key: *const CefStringUtf16) -> i32,
    >,
    pub get_type: Option<
        unsafe extern "C" fn(self_: *mut CefDictionaryValue, key: *const CefStringUtf16) -> i32,
    >,
    pub get_value: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
        ) -> *mut CefValue,
    >,
    pub get_bool: Option<
        unsafe extern "C" fn(self_: *mut CefDictionaryValue, key: *const CefStringUtf16) -> i32,
    >,
    pub get_int: Option<
        unsafe extern "C" fn(self_: *mut CefDictionaryValue, key: *const CefStringUtf16) -> i32,
    >,
    pub get_double: Option<
        unsafe extern "C" fn(self_: *mut CefDictionaryValue, key: *const CefStringUtf16) -> f64,
    >,
    pub get_string: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
        ) -> *mut CefStringUtf16,
    >,
    pub get_binary: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
        ) -> *mut CefBinaryValue,
    >,
    pub get_dictionary: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
        ) -> *mut CefDictionaryValue,
    >,
    pub get_list: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
        ) -> *mut CefListValue,
    >,
    pub set_value: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
            value: *mut CefValue,
        ) -> i32,
    >,
    pub set_null: Option<
        unsafe extern "C" fn(self_: *mut CefDictionaryValue, key: *const CefStringUtf16) -> i32,
    >,
    pub set_bool: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
            value: i32,
        ) -> i32,
    >,
    pub set_int: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
            value: i32,
        ) -> i32,
    >,
    pub set_double: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
            value: f64,
        ) -> i32,
    >,
    pub set_string: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
            value: *const CefStringUtf16,
        ) -> i32,
    >,
    pub set_binary: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
            value: *mut CefBinaryValue,
        ) -> i32,
    >,
    pub set_dictionary: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
            value: *mut CefDictionaryValue,
        ) -> i32,
    >,
    pub set_list: Option<
        unsafe extern "C" fn(
            self_: *mut CefDictionaryValue,
            key: *const CefStringUtf16,
            value: *mut CefListValue,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefListValue {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefListValue) -> i32>,
    pub is_owned: Option<unsafe extern "C" fn(self_: *mut CefListValue) -> i32>,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefListValue) -> i32>,
    pub is_same:
        Option<unsafe extern "C" fn(self_: *mut CefListValue, that: *mut CefListValue) -> i32>,
    pub is_equal:
        Option<unsafe extern "C" fn(self_: *mut CefListValue, that: *mut CefListValue) -> i32>,
    pub copy: Option<unsafe extern "C" fn(self_: *mut CefListValue) -> *mut CefListValue>,
    pub set_size: Option<unsafe extern "C" fn(self_: *mut CefListValue, size: usize) -> i32>,
    pub get_size: Option<unsafe extern "C" fn(self_: *mut CefListValue) -> usize>,
    pub clear: Option<unsafe extern "C" fn(self_: *mut CefListValue) -> i32>,
    pub remove: Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> i32>,
    pub get_type: Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> i32>,
    pub get_value:
        Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> *mut CefValue>,
    pub get_bool: Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> i32>,
    pub get_int: Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> i32>,
    pub get_double: Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> f64>,
    pub get_string:
        Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> *mut CefStringUtf16>,
    pub get_binary:
        Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> *mut CefBinaryValue>,
    pub get_dictionary: Option<
        unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> *mut CefDictionaryValue,
    >,
    pub get_list:
        Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> *mut CefListValue>,
    pub set_value: Option<
        unsafe extern "C" fn(self_: *mut CefListValue, index: usize, value: *mut CefValue) -> i32,
    >,
    pub set_null: Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize) -> i32>,
    pub set_bool:
        Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize, value: i32) -> i32>,
    pub set_int:
        Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize, value: i32) -> i32>,
    pub set_double:
        Option<unsafe extern "C" fn(self_: *mut CefListValue, index: usize, value: f64) -> i32>,
    pub set_string: Option<
        unsafe extern "C" fn(
            self_: *mut CefListValue,
            index: usize,
            value: *const CefStringUtf16,
        ) -> i32,
    >,
    pub set_binary: Option<
        unsafe extern "C" fn(
            self_: *mut CefListValue,
            index: usize,
            value: *mut CefBinaryValue,
        ) -> i32,
    >,
    pub set_dictionary: Option<
        unsafe extern "C" fn(
            self_: *mut CefListValue,
            index: usize,
            value: *mut CefDictionaryValue,
        ) -> i32,
    >,
    pub set_list: Option<
        unsafe extern "C" fn(
            self_: *mut CefListValue,
            index: usize,
            value: *mut CefListValue,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefImage {
    pub base: CefBaseRefCounted,
    pub is_empty: Option<unsafe extern "C" fn(self_: *mut CefImage) -> i32>,
    pub is_same: Option<unsafe extern "C" fn(self_: *mut CefImage, that: *mut CefImage) -> i32>,
    pub add_bitmap: Option<
        unsafe extern "C" fn(
            self_: *mut CefImage,
            scale_factor: f32,
            pixel_width: i32,
            pixel_height: i32,
            color_type: i32,
            alpha_type: i32,
            pixel_data: *const c_void,
            pixel_data_size: usize,
        ) -> i32,
    >,
    pub add_png: Option<
        unsafe extern "C" fn(
            self_: *mut CefImage,
            scale_factor: f32,
            png_data: *const c_void,
            png_data_size: usize,
        ) -> i32,
    >,
    pub add_jpeg: Option<
        unsafe extern "C" fn(
            self_: *mut CefImage,
            scale_factor: f32,
            jpeg_data: *const c_void,
            jpeg_data_size: usize,
        ) -> i32,
    >,
    pub get_width: Option<unsafe extern "C" fn(self_: *mut CefImage) -> usize>,
    pub get_height: Option<unsafe extern "C" fn(self_: *mut CefImage) -> usize>,
    pub has_representation:
        Option<unsafe extern "C" fn(self_: *mut CefImage, scale_factor: f32) -> i32>,
    pub remove_representation:
        Option<unsafe extern "C" fn(self_: *mut CefImage, scale_factor: f32) -> i32>,
    pub get_representation_info: Option<
        unsafe extern "C" fn(
            self_: *mut CefImage,
            scale_factor: f32,
            actual_scale_factor: *mut c_void,
            pixel_width: *mut i32,
            pixel_height: *mut i32,
        ) -> i32,
    >,
    pub get_as_bitmap: Option<
        unsafe extern "C" fn(
            self_: *mut CefImage,
            scale_factor: f32,
            color_type: i32,
            alpha_type: i32,
            pixel_width: *mut i32,
            pixel_height: *mut i32,
        ) -> *mut CefBinaryValue,
    >,
    pub get_as_png: Option<
        unsafe extern "C" fn(
            self_: *mut CefImage,
            scale_factor: f32,
            with_transparency: i32,
            pixel_width: *mut i32,
            pixel_height: *mut i32,
        ) -> *mut CefBinaryValue,
    >,
    pub get_as_jpeg: Option<
        unsafe extern "C" fn(
            self_: *mut CefImage,
            scale_factor: f32,
            quality: i32,
            pixel_width: *mut i32,
            pixel_height: *mut i32,
        ) -> *mut CefBinaryValue,
    >,
}

#[repr(C)]
pub struct CefReadHandler {
    pub base: CefBaseRefCounted,
    pub read: Option<
        unsafe extern "C" fn(
            self_: *mut CefReadHandler,
            ptr: *mut c_void,
            size: usize,
            n: usize,
        ) -> usize,
    >,
    pub seek:
        Option<unsafe extern "C" fn(self_: *mut CefReadHandler, offset: i64, whence: i32) -> i32>,
    pub tell: Option<unsafe extern "C" fn(self_: *mut CefReadHandler) -> i64>,
    pub eof: Option<unsafe extern "C" fn(self_: *mut CefReadHandler) -> i32>,
    pub may_block: Option<unsafe extern "C" fn(self_: *mut CefReadHandler) -> i32>,
}

#[repr(C)]
pub struct CefStreamReader {
    pub base: CefBaseRefCounted,
    pub read: Option<
        unsafe extern "C" fn(
            self_: *mut CefStreamReader,
            ptr: *mut c_void,
            size: usize,
            n: usize,
        ) -> usize,
    >,
    pub seek:
        Option<unsafe extern "C" fn(self_: *mut CefStreamReader, offset: i64, whence: i32) -> i32>,
    pub tell: Option<unsafe extern "C" fn(self_: *mut CefStreamReader) -> i64>,
    pub eof: Option<unsafe extern "C" fn(self_: *mut CefStreamReader) -> i32>,
    pub may_block: Option<unsafe extern "C" fn(self_: *mut CefStreamReader) -> i32>,
}

#[repr(C)]
pub struct CefWriteHandler {
    pub base: CefBaseRefCounted,
    pub write: Option<
        unsafe extern "C" fn(
            self_: *mut CefWriteHandler,
            ptr: *const c_void,
            size: usize,
            n: usize,
        ) -> usize,
    >,
    pub seek:
        Option<unsafe extern "C" fn(self_: *mut CefWriteHandler, offset: i64, whence: i32) -> i32>,
    pub tell: Option<unsafe extern "C" fn(self_: *mut CefWriteHandler) -> i64>,
    pub flush: Option<unsafe extern "C" fn(self_: *mut CefWriteHandler) -> i32>,
    pub may_block: Option<unsafe extern "C" fn(self_: *mut CefWriteHandler) -> i32>,
}

#[repr(C)]
pub struct CefStreamWriter {
    pub base: CefBaseRefCounted,
    pub write: Option<
        unsafe extern "C" fn(
            self_: *mut CefStreamWriter,
            ptr: *const c_void,
            size: usize,
            n: usize,
        ) -> usize,
    >,
    pub seek:
        Option<unsafe extern "C" fn(self_: *mut CefStreamWriter, offset: i64, whence: i32) -> i32>,
    pub tell: Option<unsafe extern "C" fn(self_: *mut CefStreamWriter) -> i64>,
    pub flush: Option<unsafe extern "C" fn(self_: *mut CefStreamWriter) -> i32>,
    pub may_block: Option<unsafe extern "C" fn(self_: *mut CefStreamWriter) -> i32>,
}

#[repr(C)]
pub struct CefDragData {
    pub base: CefBaseRefCounted,
    pub clone: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> *mut CefDragData>,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> i32>,
    pub is_link: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> i32>,
    pub is_fragment: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> i32>,
    pub is_file: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> i32>,
    pub get_link_url: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> *mut CefStringUtf16>,
    pub get_link_title:
        Option<unsafe extern "C" fn(self_: *mut CefDragData) -> *mut CefStringUtf16>,
    pub get_link_metadata:
        Option<unsafe extern "C" fn(self_: *mut CefDragData) -> *mut CefStringUtf16>,
    pub get_fragment_text:
        Option<unsafe extern "C" fn(self_: *mut CefDragData) -> *mut CefStringUtf16>,
    pub get_fragment_html:
        Option<unsafe extern "C" fn(self_: *mut CefDragData) -> *mut CefStringUtf16>,
    pub get_fragment_base_url:
        Option<unsafe extern "C" fn(self_: *mut CefDragData) -> *mut CefStringUtf16>,
    pub get_file_name: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> *mut CefStringUtf16>,
    pub get_file_contents: Option<
        unsafe extern "C" fn(self_: *mut CefDragData, writer: *mut CefStreamWriter) -> usize,
    >,
    pub get_file_names:
        Option<unsafe extern "C" fn(self_: *mut CefDragData, names: *mut c_void) -> i32>,
    pub get_file_paths:
        Option<unsafe extern "C" fn(self_: *mut CefDragData, paths: *mut c_void) -> i32>,
    pub set_link_url:
        Option<unsafe extern "C" fn(self_: *mut CefDragData, url: *const CefStringUtf16) -> ()>,
    pub set_link_title:
        Option<unsafe extern "C" fn(self_: *mut CefDragData, title: *const CefStringUtf16) -> ()>,
    pub set_link_metadata:
        Option<unsafe extern "C" fn(self_: *mut CefDragData, data: *const CefStringUtf16) -> ()>,
    pub set_fragment_text:
        Option<unsafe extern "C" fn(self_: *mut CefDragData, text: *const CefStringUtf16) -> ()>,
    pub set_fragment_html:
        Option<unsafe extern "C" fn(self_: *mut CefDragData, html: *const CefStringUtf16) -> ()>,
    pub set_fragment_base_url: Option<
        unsafe extern "C" fn(self_: *mut CefDragData, base_url: *const CefStringUtf16) -> (),
    >,
    pub reset_file_contents: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> ()>,
    pub add_file: Option<
        unsafe extern "C" fn(
            self_: *mut CefDragData,
            path: *const CefStringUtf16,
            display_name: *const CefStringUtf16,
        ) -> (),
    >,
    pub clear_filenames: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> ()>,
    pub get_image: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> *mut CefImage>,
    pub get_image_hotspot: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> i32>,
    pub has_image: Option<unsafe extern "C" fn(self_: *mut CefDragData) -> i32>,
}

#[repr(C)]
pub struct CefDomvisitor {
    pub base: CefBaseRefCounted,
    pub visit: Option<
        unsafe extern "C" fn(self_: *mut CefDomvisitor, document: *mut CefDomdocument) -> (),
    >,
}

#[repr(C)]
pub struct CefDomdocument {
    pub base: CefBaseRefCounted,
    pub get_type: Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> i32>,
    pub get_document: Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> *mut CefDomnode>,
    pub get_body: Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> *mut CefDomnode>,
    pub get_head: Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> *mut CefDomnode>,
    pub get_title: Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> *mut CefStringUtf16>,
    pub get_element_by_id: Option<
        unsafe extern "C" fn(
            self_: *mut CefDomdocument,
            id: *const CefStringUtf16,
        ) -> *mut CefDomnode,
    >,
    pub get_focused_node:
        Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> *mut CefDomnode>,
    pub has_selection: Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> i32>,
    pub get_selection_start_offset: Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> i32>,
    pub get_selection_end_offset: Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> i32>,
    pub get_selection_as_markup:
        Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> *mut CefStringUtf16>,
    pub get_selection_as_text:
        Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> *mut CefStringUtf16>,
    pub get_base_url:
        Option<unsafe extern "C" fn(self_: *mut CefDomdocument) -> *mut CefStringUtf16>,
    pub get_complete_url: Option<
        unsafe extern "C" fn(
            self_: *mut CefDomdocument,
            partialURL: *const CefStringUtf16,
        ) -> *mut CefStringUtf16,
    >,
}

#[repr(C)]
pub struct CefDomnode {
    pub base: CefBaseRefCounted,
    pub get_type: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> i32>,
    pub is_text: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> i32>,
    pub is_element: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> i32>,
    pub is_editable: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> i32>,
    pub is_form_control_element: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> i32>,
    pub get_form_control_element_type: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> i32>,
    pub is_same: Option<unsafe extern "C" fn(self_: *mut CefDomnode, that: *mut CefDomnode) -> i32>,
    pub get_name: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefStringUtf16>,
    pub get_value: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefStringUtf16>,
    pub set_value:
        Option<unsafe extern "C" fn(self_: *mut CefDomnode, value: *const CefStringUtf16) -> i32>,
    pub get_as_markup: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefStringUtf16>,
    pub get_document: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefDomdocument>,
    pub get_parent: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefDomnode>,
    pub get_previous_sibling:
        Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefDomnode>,
    pub get_next_sibling: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefDomnode>,
    pub has_children: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> i32>,
    pub get_first_child: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefDomnode>,
    pub get_last_child: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefDomnode>,
    pub get_element_tag_name:
        Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefStringUtf16>,
    pub has_element_attributes: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> i32>,
    pub has_element_attribute: Option<
        unsafe extern "C" fn(self_: *mut CefDomnode, attrName: *const CefStringUtf16) -> i32,
    >,
    pub get_element_attribute: Option<
        unsafe extern "C" fn(
            self_: *mut CefDomnode,
            attrName: *const CefStringUtf16,
        ) -> *mut CefStringUtf16,
    >,
    pub get_element_attributes:
        Option<unsafe extern "C" fn(self_: *mut CefDomnode, attrMap: i32) -> ()>,
    pub set_element_attribute: Option<
        unsafe extern "C" fn(
            self_: *mut CefDomnode,
            attrName: *const CefStringUtf16,
            value: *const CefStringUtf16,
        ) -> i32,
    >,
    pub get_element_inner_text:
        Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> *mut CefStringUtf16>,
    pub get_element_bounds: Option<unsafe extern "C" fn(self_: *mut CefDomnode) -> i32>,
}

#[repr(C)]
pub struct CefSharedMemoryRegion {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefSharedMemoryRegion) -> i32>,
    pub size: Option<unsafe extern "C" fn(self_: *mut CefSharedMemoryRegion) -> usize>,
    pub memory: Option<unsafe extern "C" fn(self_: *mut CefSharedMemoryRegion) -> *mut c_void>,
}

#[repr(C)]
pub struct CefProcessMessage {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefProcessMessage) -> i32>,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefProcessMessage) -> i32>,
    pub copy: Option<unsafe extern "C" fn(self_: *mut CefProcessMessage) -> *mut CefProcessMessage>,
    pub get_name:
        Option<unsafe extern "C" fn(self_: *mut CefProcessMessage) -> *mut CefStringUtf16>,
    pub get_argument_list:
        Option<unsafe extern "C" fn(self_: *mut CefProcessMessage) -> *mut CefListValue>,
    pub get_shared_memory_region:
        Option<unsafe extern "C" fn(self_: *mut CefProcessMessage) -> *mut CefSharedMemoryRegion>,
}

#[repr(C)]
pub struct CefRequest {
    pub base: CefBaseRefCounted,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefRequest) -> i32>,
    pub get_url: Option<unsafe extern "C" fn(self_: *mut CefRequest) -> *mut CefStringUtf16>,
    pub set_url:
        Option<unsafe extern "C" fn(self_: *mut CefRequest, url: *const CefStringUtf16) -> ()>,
    pub get_method: Option<unsafe extern "C" fn(self_: *mut CefRequest) -> *mut CefStringUtf16>,
    pub set_method:
        Option<unsafe extern "C" fn(self_: *mut CefRequest, method: *const CefStringUtf16) -> ()>,
    pub set_referrer: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequest,
            referrer_url: *const CefStringUtf16,
            policy: i32,
        ) -> (),
    >,
    pub get_referrer_url:
        Option<unsafe extern "C" fn(self_: *mut CefRequest) -> *mut CefStringUtf16>,
    pub get_referrer_policy: Option<unsafe extern "C" fn(self_: *mut CefRequest) -> i32>,
    pub get_post_data: Option<unsafe extern "C" fn(self_: *mut CefRequest) -> *mut CefPostData>,
    pub set_post_data:
        Option<unsafe extern "C" fn(self_: *mut CefRequest, postData: *mut CefPostData) -> ()>,
    pub get_header_map: Option<unsafe extern "C" fn(self_: *mut CefRequest, headerMap: i32) -> ()>,
    pub set_header_map: Option<unsafe extern "C" fn(self_: *mut CefRequest, headerMap: i32) -> ()>,
    pub get_header_by_name: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequest,
            name: *const CefStringUtf16,
        ) -> *mut CefStringUtf16,
    >,
    pub set_header_by_name: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequest,
            name: *const CefStringUtf16,
            value: *const CefStringUtf16,
            overwrite: i32,
        ) -> (),
    >,
    pub set: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequest,
            url: *const CefStringUtf16,
            method: *const CefStringUtf16,
            postData: *mut CefPostData,
            headerMap: i32,
        ) -> (),
    >,
    pub get_flags: Option<unsafe extern "C" fn(self_: *mut CefRequest) -> i32>,
    pub set_flags: Option<unsafe extern "C" fn(self_: *mut CefRequest, flags: i32) -> ()>,
    pub get_first_party_for_cookies:
        Option<unsafe extern "C" fn(self_: *mut CefRequest) -> *mut CefStringUtf16>,
    pub set_first_party_for_cookies:
        Option<unsafe extern "C" fn(self_: *mut CefRequest, url: *const CefStringUtf16) -> ()>,
    pub get_resource_type: Option<unsafe extern "C" fn(self_: *mut CefRequest) -> i32>,
    pub get_transition_type: Option<unsafe extern "C" fn(self_: *mut CefRequest) -> i32>,
    pub get_identifier: Option<unsafe extern "C" fn(self_: *mut CefRequest) -> u64>,
}

#[repr(C)]
pub struct CefPostData {
    pub base: CefBaseRefCounted,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefPostData) -> i32>,
    pub has_excluded_elements: Option<unsafe extern "C" fn(self_: *mut CefPostData) -> i32>,
    pub get_element_count: Option<unsafe extern "C" fn(self_: *mut CefPostData) -> usize>,
    pub get_elements: Option<
        unsafe extern "C" fn(
            self_: *mut CefPostData,
            elementsCount: *mut c_void,
            elements: *mut *mut CefPostDataElement,
        ) -> (),
    >,
    pub remove_element: Option<
        unsafe extern "C" fn(self_: *mut CefPostData, element: *mut CefPostDataElement) -> i32,
    >,
    pub add_element: Option<
        unsafe extern "C" fn(self_: *mut CefPostData, element: *mut CefPostDataElement) -> i32,
    >,
    pub remove_elements: Option<unsafe extern "C" fn(self_: *mut CefPostData) -> ()>,
}

#[repr(C)]
pub struct CefPostDataElement {
    pub base: CefBaseRefCounted,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefPostDataElement) -> i32>,
    pub set_to_empty: Option<unsafe extern "C" fn(self_: *mut CefPostDataElement) -> ()>,
    pub set_to_file: Option<
        unsafe extern "C" fn(self_: *mut CefPostDataElement, fileName: *const CefStringUtf16) -> (),
    >,
    pub set_to_bytes: Option<
        unsafe extern "C" fn(
            self_: *mut CefPostDataElement,
            size: usize,
            bytes: *const c_void,
        ) -> (),
    >,
    pub get_type: Option<unsafe extern "C" fn(self_: *mut CefPostDataElement) -> i32>,
    pub get_file:
        Option<unsafe extern "C" fn(self_: *mut CefPostDataElement) -> *mut CefStringUtf16>,
    pub get_bytes_count: Option<unsafe extern "C" fn(self_: *mut CefPostDataElement) -> usize>,
    pub get_bytes: Option<
        unsafe extern "C" fn(
            self_: *mut CefPostDataElement,
            size: usize,
            bytes: *mut c_void,
        ) -> usize,
    >,
}

#[repr(C)]
pub struct CefStringVisitor {
    pub base: CefBaseRefCounted,
    pub visit: Option<
        unsafe extern "C" fn(self_: *mut CefStringVisitor, string: *const CefStringUtf16) -> (),
    >,
}

#[repr(C)]
pub struct CefFrame {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> i32>,
    pub undo: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> ()>,
    pub redo: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> ()>,
    pub cut: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> ()>,
    pub copy: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> ()>,
    pub paste: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> ()>,
    pub paste_and_match_style: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> ()>,
    pub del: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> ()>,
    pub select_all: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> ()>,
    pub view_source: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> ()>,
    pub get_source:
        Option<unsafe extern "C" fn(self_: *mut CefFrame, visitor: *mut CefStringVisitor) -> ()>,
    pub get_text:
        Option<unsafe extern "C" fn(self_: *mut CefFrame, visitor: *mut CefStringVisitor) -> ()>,
    pub load_request:
        Option<unsafe extern "C" fn(self_: *mut CefFrame, request: *mut CefRequest) -> ()>,
    pub load_url:
        Option<unsafe extern "C" fn(self_: *mut CefFrame, url: *const CefStringUtf16) -> ()>,
    pub execute_java_script: Option<
        unsafe extern "C" fn(
            self_: *mut CefFrame,
            code: *const CefStringUtf16,
            script_url: *const CefStringUtf16,
            start_line: i32,
        ) -> (),
    >,
    pub is_main: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> i32>,
    pub is_focused: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> i32>,
    pub get_name: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> *mut CefStringUtf16>,
    pub get_identifier: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> *mut CefStringUtf16>,
    pub get_parent: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> *mut CefFrame>,
    pub get_url: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> *mut CefStringUtf16>,
    pub get_browser: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> *mut CefBrowser>,
    pub get_v8_context: Option<unsafe extern "C" fn(self_: *mut CefFrame) -> *mut CefV8Context>,
    pub visit_dom:
        Option<unsafe extern "C" fn(self_: *mut CefFrame, visitor: *mut CefDomvisitor) -> ()>,
    pub create_urlrequest: Option<
        unsafe extern "C" fn(
            self_: *mut CefFrame,
            request: *mut CefRequest,
            client: *mut c_void,
        ) -> *mut c_void,
    >,
    pub send_process_message: Option<
        unsafe extern "C" fn(
            self_: *mut CefFrame,
            target_process: i32,
            message: *mut CefProcessMessage,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefX509CertPrincipal {
    pub base: CefBaseRefCounted,
    pub get_display_name:
        Option<unsafe extern "C" fn(self_: *mut CefX509CertPrincipal) -> *mut CefStringUtf16>,
    pub get_common_name:
        Option<unsafe extern "C" fn(self_: *mut CefX509CertPrincipal) -> *mut CefStringUtf16>,
    pub get_locality_name:
        Option<unsafe extern "C" fn(self_: *mut CefX509CertPrincipal) -> *mut CefStringUtf16>,
    pub get_state_or_province_name:
        Option<unsafe extern "C" fn(self_: *mut CefX509CertPrincipal) -> *mut CefStringUtf16>,
    pub get_country_name:
        Option<unsafe extern "C" fn(self_: *mut CefX509CertPrincipal) -> *mut CefStringUtf16>,
    pub get_organization_names:
        Option<unsafe extern "C" fn(self_: *mut CefX509CertPrincipal, names: *mut c_void) -> ()>,
    pub get_organization_unit_names:
        Option<unsafe extern "C" fn(self_: *mut CefX509CertPrincipal, names: *mut c_void) -> ()>,
}

#[repr(C)]
pub struct CefX509Certificate {
    pub base: CefBaseRefCounted,
    pub get_subject:
        Option<unsafe extern "C" fn(self_: *mut CefX509Certificate) -> *mut CefX509CertPrincipal>,
    pub get_issuer:
        Option<unsafe extern "C" fn(self_: *mut CefX509Certificate) -> *mut CefX509CertPrincipal>,
    pub get_serial_number:
        Option<unsafe extern "C" fn(self_: *mut CefX509Certificate) -> *mut CefBinaryValue>,
    pub get_valid_start: Option<unsafe extern "C" fn(self_: *mut CefX509Certificate) -> i64>,
    pub get_valid_expiry: Option<unsafe extern "C" fn(self_: *mut CefX509Certificate) -> i64>,
    pub get_derencoded:
        Option<unsafe extern "C" fn(self_: *mut CefX509Certificate) -> *mut CefBinaryValue>,
    pub get_pemencoded:
        Option<unsafe extern "C" fn(self_: *mut CefX509Certificate) -> *mut CefBinaryValue>,
    pub get_issuer_chain_size:
        Option<unsafe extern "C" fn(self_: *mut CefX509Certificate) -> usize>,
    pub get_derencoded_issuer_chain: Option<
        unsafe extern "C" fn(
            self_: *mut CefX509Certificate,
            chainCount: *mut c_void,
            chain: *mut *mut CefBinaryValue,
        ) -> (),
    >,
    pub get_pemencoded_issuer_chain: Option<
        unsafe extern "C" fn(
            self_: *mut CefX509Certificate,
            chainCount: *mut c_void,
            chain: *mut *mut CefBinaryValue,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefSslstatus {
    pub base: CefBaseRefCounted,
    pub is_secure_connection: Option<unsafe extern "C" fn(self_: *mut CefSslstatus) -> i32>,
    pub get_cert_status: Option<unsafe extern "C" fn(self_: *mut CefSslstatus) -> i32>,
    pub get_sslversion: Option<unsafe extern "C" fn(self_: *mut CefSslstatus) -> i32>,
    pub get_content_status: Option<unsafe extern "C" fn(self_: *mut CefSslstatus) -> i32>,
    pub get_x509_certificate:
        Option<unsafe extern "C" fn(self_: *mut CefSslstatus) -> *mut CefX509Certificate>,
}

#[repr(C)]
pub struct CefNavigationEntry {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefNavigationEntry) -> i32>,
    pub get_url:
        Option<unsafe extern "C" fn(self_: *mut CefNavigationEntry) -> *mut CefStringUtf16>,
    pub get_display_url:
        Option<unsafe extern "C" fn(self_: *mut CefNavigationEntry) -> *mut CefStringUtf16>,
    pub get_original_url:
        Option<unsafe extern "C" fn(self_: *mut CefNavigationEntry) -> *mut CefStringUtf16>,
    pub get_title:
        Option<unsafe extern "C" fn(self_: *mut CefNavigationEntry) -> *mut CefStringUtf16>,
    pub get_transition_type: Option<unsafe extern "C" fn(self_: *mut CefNavigationEntry) -> i32>,
    pub has_post_data: Option<unsafe extern "C" fn(self_: *mut CefNavigationEntry) -> i32>,
    pub get_completion_time: Option<unsafe extern "C" fn(self_: *mut CefNavigationEntry) -> i64>,
    pub get_http_status_code: Option<unsafe extern "C" fn(self_: *mut CefNavigationEntry) -> i32>,
    pub get_sslstatus:
        Option<unsafe extern "C" fn(self_: *mut CefNavigationEntry) -> *mut CefSslstatus>,
}

#[repr(C)]
pub struct CefCallback {
    pub base: CefBaseRefCounted,
    pub cont: Option<unsafe extern "C" fn(self_: *mut CefCallback) -> ()>,
    pub cancel: Option<unsafe extern "C" fn(self_: *mut CefCallback) -> ()>,
}

#[repr(C)]
pub struct CefCompletionCallback {
    pub base: CefBaseRefCounted,
    pub on_complete: Option<unsafe extern "C" fn(self_: *mut CefCompletionCallback) -> ()>,
}

#[repr(C)]
pub struct CefCookieManager {
    pub base: CefBaseRefCounted,
    pub visit_all_cookies: Option<
        unsafe extern "C" fn(self_: *mut CefCookieManager, visitor: *mut CefCookieVisitor) -> i32,
    >,
    pub visit_url_cookies: Option<
        unsafe extern "C" fn(
            self_: *mut CefCookieManager,
            url: *const CefStringUtf16,
            includeHttpOnly: i32,
            visitor: *mut CefCookieVisitor,
        ) -> i32,
    >,
    pub set_cookie: Option<
        unsafe extern "C" fn(
            self_: *mut CefCookieManager,
            url: *const CefStringUtf16,
            cookie: *const c_void,
            callback: *mut CefSetCookieCallback,
        ) -> i32,
    >,
    pub delete_cookies: Option<
        unsafe extern "C" fn(
            self_: *mut CefCookieManager,
            url: *const CefStringUtf16,
            cookie_name: *const CefStringUtf16,
            callback: *mut CefDeleteCookiesCallback,
        ) -> i32,
    >,
    pub flush_store: Option<
        unsafe extern "C" fn(
            self_: *mut CefCookieManager,
            callback: *mut CefCompletionCallback,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefCookieVisitor {
    pub base: CefBaseRefCounted,
    pub visit: Option<
        unsafe extern "C" fn(
            self_: *mut CefCookieVisitor,
            cookie: *const c_void,
            count: i32,
            total: i32,
            deleteCookie: *mut i32,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefSetCookieCallback {
    pub base: CefBaseRefCounted,
    pub on_complete:
        Option<unsafe extern "C" fn(self_: *mut CefSetCookieCallback, success: i32) -> ()>,
}

#[repr(C)]
pub struct CefDeleteCookiesCallback {
    pub base: CefBaseRefCounted,
    pub on_complete:
        Option<unsafe extern "C" fn(self_: *mut CefDeleteCookiesCallback, num_deleted: i32) -> ()>,
}

#[repr(C)]
pub struct CefMediaRouter {
    pub base: CefBaseRefCounted,
    pub add_observer: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaRouter,
            observer: *mut CefMediaObserver,
        ) -> *mut c_void,
    >,
    pub get_source: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaRouter,
            urn: *const CefStringUtf16,
        ) -> *mut CefMediaSource,
    >,
    pub notify_current_sinks: Option<unsafe extern "C" fn(self_: *mut CefMediaRouter) -> ()>,
    pub create_route: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaRouter,
            source: *mut CefMediaSource,
            sink: *mut CefMediaSink,
            callback: *mut CefMediaRouteCreateCallback,
        ) -> (),
    >,
    pub notify_current_routes: Option<unsafe extern "C" fn(self_: *mut CefMediaRouter) -> ()>,
}

#[repr(C)]
pub struct CefMediaObserver {
    pub base: CefBaseRefCounted,
    pub on_sinks: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaObserver,
            sinksCount: usize,
            sinks: *mut *const CefMediaSink,
        ) -> (),
    >,
    pub on_routes: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaObserver,
            routesCount: usize,
            routes: *mut *const CefMediaRoute,
        ) -> (),
    >,
    pub on_route_state_changed: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaObserver,
            route: *mut CefMediaRoute,
            state: i32,
        ) -> (),
    >,
    pub on_route_message_received: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaObserver,
            route: *mut CefMediaRoute,
            message: *const c_void,
            message_size: usize,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefMediaRoute {
    pub base: CefBaseRefCounted,
    pub get_id: Option<unsafe extern "C" fn(self_: *mut CefMediaRoute) -> *mut CefStringUtf16>,
    pub get_source: Option<unsafe extern "C" fn(self_: *mut CefMediaRoute) -> *mut CefMediaSource>,
    pub get_sink: Option<unsafe extern "C" fn(self_: *mut CefMediaRoute) -> *mut CefMediaSink>,
    pub send_route_message: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaRoute,
            message: *const c_void,
            message_size: usize,
        ) -> (),
    >,
    pub terminate: Option<unsafe extern "C" fn(self_: *mut CefMediaRoute) -> ()>,
}

#[repr(C)]
pub struct CefMediaRouteCreateCallback {
    pub base: CefBaseRefCounted,
    pub on_media_route_create_finished: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaRouteCreateCallback,
            result: i32,
            error: *const CefStringUtf16,
            route: *mut CefMediaRoute,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefMediaSink {
    pub base: CefBaseRefCounted,
    pub get_id: Option<unsafe extern "C" fn(self_: *mut CefMediaSink) -> *mut CefStringUtf16>,
    pub get_name: Option<unsafe extern "C" fn(self_: *mut CefMediaSink) -> *mut CefStringUtf16>,
    pub get_icon_type: Option<unsafe extern "C" fn(self_: *mut CefMediaSink) -> i32>,
    pub get_device_info: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaSink,
            callback: *mut CefMediaSinkDeviceInfoCallback,
        ) -> (),
    >,
    pub is_cast_sink: Option<unsafe extern "C" fn(self_: *mut CefMediaSink) -> i32>,
    pub is_dial_sink: Option<unsafe extern "C" fn(self_: *mut CefMediaSink) -> i32>,
    pub is_compatible_with:
        Option<unsafe extern "C" fn(self_: *mut CefMediaSink, source: *mut CefMediaSource) -> i32>,
}

#[repr(C)]
pub struct CefMediaSinkDeviceInfoCallback {
    pub base: CefBaseRefCounted,
    pub on_media_sink_device_info: Option<
        unsafe extern "C" fn(
            self_: *mut CefMediaSinkDeviceInfoCallback,
            device_info: *const c_void,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefMediaSource {
    pub base: CefBaseRefCounted,
    pub get_id: Option<unsafe extern "C" fn(self_: *mut CefMediaSource) -> *mut CefStringUtf16>,
    pub is_cast_source: Option<unsafe extern "C" fn(self_: *mut CefMediaSource) -> i32>,
    pub is_dial_source: Option<unsafe extern "C" fn(self_: *mut CefMediaSource) -> i32>,
}

#[repr(C)]
pub struct CefPreferenceObserver {
    pub base: CefBaseRefCounted,
    pub on_preference_changed: Option<
        unsafe extern "C" fn(self_: *mut CefPreferenceObserver, name: *const CefStringUtf16) -> (),
    >,
}

#[repr(C)]
pub struct CefPreferenceManager {
    pub base: CefBaseRefCounted,
    pub has_preference: Option<
        unsafe extern "C" fn(self_: *mut CefPreferenceManager, name: *const CefStringUtf16) -> i32,
    >,
    pub get_preference: Option<
        unsafe extern "C" fn(
            self_: *mut CefPreferenceManager,
            name: *const CefStringUtf16,
        ) -> *mut CefValue,
    >,
    pub get_all_preferences: Option<
        unsafe extern "C" fn(
            self_: *mut CefPreferenceManager,
            include_defaults: i32,
        ) -> *mut CefDictionaryValue,
    >,
    pub can_set_preference: Option<
        unsafe extern "C" fn(self_: *mut CefPreferenceManager, name: *const CefStringUtf16) -> i32,
    >,
    pub set_preference: Option<
        unsafe extern "C" fn(
            self_: *mut CefPreferenceManager,
            name: *const CefStringUtf16,
            value: *mut CefValue,
            error: *mut CefStringUtf16,
        ) -> i32,
    >,
    pub add_preference_observer: Option<
        unsafe extern "C" fn(
            self_: *mut CefPreferenceManager,
            name: *const CefStringUtf16,
            observer: *mut CefPreferenceObserver,
        ) -> *mut c_void,
    >,
}

#[repr(C)]
pub struct CefResolveCallback {
    pub base: CefBaseRefCounted,
    pub on_resolve_completed: Option<
        unsafe extern "C" fn(
            self_: *mut CefResolveCallback,
            result: i32,
            resolved_ips: *mut c_void,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefSettingObserver {
    pub base: CefBaseRefCounted,
    pub on_setting_changed: Option<
        unsafe extern "C" fn(
            self_: *mut CefSettingObserver,
            requesting_url: *const CefStringUtf16,
            top_level_url: *const CefStringUtf16,
            content_type: i32,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefBrowser {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> i32>,
    pub get_host: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> *mut CefBrowserHost>,
    pub can_go_back: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> i32>,
    pub go_back: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> ()>,
    pub can_go_forward: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> i32>,
    pub go_forward: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> ()>,
    pub is_loading: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> i32>,
    pub reload: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> ()>,
    pub reload_ignore_cache: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> ()>,
    pub stop_load: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> ()>,
    pub get_identifier: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> i32>,
    pub is_same: Option<unsafe extern "C" fn(self_: *mut CefBrowser, that: *mut CefBrowser) -> i32>,
    pub is_popup: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> i32>,
    pub has_document: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> i32>,
    pub get_main_frame: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> *mut CefFrame>,
    pub get_focused_frame: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> *mut CefFrame>,
    pub get_frame_by_identifier: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowser,
            identifier: *const CefStringUtf16,
        ) -> *mut CefFrame,
    >,
    pub get_frame_by_name: Option<
        unsafe extern "C" fn(self_: *mut CefBrowser, name: *const CefStringUtf16) -> *mut CefFrame,
    >,
    pub get_frame_count: Option<unsafe extern "C" fn(self_: *mut CefBrowser) -> usize>,
    pub get_frame_identifiers:
        Option<unsafe extern "C" fn(self_: *mut CefBrowser, identifiers: *mut c_void) -> ()>,
    pub get_frame_names:
        Option<unsafe extern "C" fn(self_: *mut CefBrowser, names: *mut c_void) -> ()>,
}

#[repr(C)]
pub struct CefRunFileDialogCallback {
    pub base: CefBaseRefCounted,
    pub on_file_dialog_dismissed: Option<
        unsafe extern "C" fn(self_: *mut CefRunFileDialogCallback, file_paths: *mut c_void) -> (),
    >,
}

#[repr(C)]
pub struct CefNavigationEntryVisitor {
    pub base: CefBaseRefCounted,
    pub visit: Option<
        unsafe extern "C" fn(
            self_: *mut CefNavigationEntryVisitor,
            entry: *mut CefNavigationEntry,
            current: i32,
            index: i32,
            total: i32,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefPdfPrintCallback {
    pub base: CefBaseRefCounted,
    pub on_pdf_print_finished: Option<
        unsafe extern "C" fn(
            self_: *mut CefPdfPrintCallback,
            path: *const CefStringUtf16,
            ok: i32,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefDownloadImageCallback {
    pub base: CefBaseRefCounted,
    pub on_download_image_finished: Option<
        unsafe extern "C" fn(
            self_: *mut CefDownloadImageCallback,
            image_url: *const CefStringUtf16,
            http_status_code: i32,
            image: *mut CefImage,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefBrowserHost {
    pub base: CefBaseRefCounted,
    pub get_browser: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> *mut CefBrowser>,
    pub close_browser:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, force_close: i32) -> ()>,
    pub try_close_browser: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
    pub is_ready_to_be_closed: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
    pub set_focus: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, focus: i32) -> ()>,
    pub get_window_handle: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> usize>,
    pub get_opener_window_handle: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> usize>,
    pub get_opener_identifier: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
    pub has_view: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
    pub get_client: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> *mut CefClient>,
    pub get_request_context:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> *mut c_void>,
    pub can_zoom: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, command: i32) -> i32>,
    pub zoom: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, command: i32) -> ()>,
    pub get_default_zoom_level: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> f64>,
    pub get_zoom_level: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> f64>,
    pub set_zoom_level:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, zoomLevel: f64) -> ()>,
    pub run_file_dialog: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            mode: i32,
            title: *const CefStringUtf16,
            default_file_path: *const CefStringUtf16,
            accept_filters: *mut c_void,
            callback: *mut CefRunFileDialogCallback,
        ) -> (),
    >,
    pub start_download:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, url: *const CefStringUtf16) -> ()>,
    pub download_image: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            image_url: *const CefStringUtf16,
            is_favicon: i32,
            max_image_size: u32,
            bypass_cache: i32,
            callback: *mut CefDownloadImageCallback,
        ) -> (),
    >,
    pub print: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> ()>,
    pub print_to_pdf: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            path: *const CefStringUtf16,
            settings: *const c_void,
            callback: *mut CefPdfPrintCallback,
        ) -> (),
    >,
    pub find: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            searchText: *const CefStringUtf16,
            forward: i32,
            matchCase: i32,
            findNext: i32,
        ) -> (),
    >,
    pub stop_finding:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, clearSelection: i32) -> ()>,
    pub show_dev_tools: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            windowInfo: *const c_void,
            client: *mut CefClient,
            settings: *const c_void,
            inspect_element_at: *const CefPoint,
        ) -> (),
    >,
    pub close_dev_tools: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> ()>,
    pub has_dev_tools: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
    pub send_dev_tools_message: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            message: *const c_void,
            message_size: usize,
        ) -> i32,
    >,
    pub execute_dev_tools_method: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            message_id: i32,
            method: *const CefStringUtf16,
            params: *mut CefDictionaryValue,
        ) -> i32,
    >,
    pub add_dev_tools_message_observer: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            observer: *mut CefDevToolsMessageObserver,
        ) -> *mut c_void,
    >,
    pub get_navigation_entries: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            visitor: *mut CefNavigationEntryVisitor,
            current_only: i32,
        ) -> (),
    >,
    pub replace_misspelling:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, word: *const CefStringUtf16) -> ()>,
    pub add_word_to_dictionary:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, word: *const CefStringUtf16) -> ()>,
    pub is_window_rendering_disabled:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
    pub was_resized: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> ()>,
    pub was_hidden: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, hidden: i32) -> ()>,
    pub notify_screen_info_changed: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> ()>,
    pub invalidate: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, type_: i32) -> ()>,
    pub send_external_begin_frame: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> ()>,
    pub send_key_event:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, event: *const CefKeyEvent) -> ()>,
    pub send_mouse_click_event: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            event: *const CefMouseEvent,
            type_: i32,
            mouseUp: i32,
            clickCount: i32,
        ) -> (),
    >,
    pub send_mouse_move_event: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            event: *const CefMouseEvent,
            mouseLeave: i32,
        ) -> (),
    >,
    pub send_mouse_wheel_event: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            event: *const CefMouseEvent,
            deltaX: i32,
            deltaY: i32,
        ) -> (),
    >,
    pub send_touch_event:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, event: *const c_void) -> ()>,
    pub send_capture_lost_event: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> ()>,
    pub notify_move_or_resize_started:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> ()>,
    pub get_windowless_frame_rate: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
    pub set_windowless_frame_rate:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, frame_rate: i32) -> ()>,
    pub ime_set_composition: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            text: *const CefStringUtf16,
            underlinesCount: usize,
            underlines: *const c_void,
            replacement_range: *const CefRange,
            selection_range: *const CefRange,
        ) -> (),
    >,
    pub ime_commit_text: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            text: *const CefStringUtf16,
            replacement_range: *const CefRange,
            relative_cursor_pos: i32,
        ) -> (),
    >,
    pub ime_finish_composing_text:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, keep_selection: i32) -> ()>,
    pub ime_cancel_composition: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> ()>,
    pub drag_target_drag_enter: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            drag_data: *mut CefDragData,
            event: *const CefMouseEvent,
            allowed_ops: i32,
        ) -> (),
    >,
    pub drag_target_drag_over: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            event: *const CefMouseEvent,
            allowed_ops: i32,
        ) -> (),
    >,
    pub drag_target_drag_leave: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> ()>,
    pub drag_target_drop:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, event: *const CefMouseEvent) -> ()>,
    pub drag_source_ended_at:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, x: i32, y: i32, op: i32) -> ()>,
    pub drag_source_system_drag_ended:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> ()>,
    pub get_visible_navigation_entry:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> *mut CefNavigationEntry>,
    pub set_accessibility_state:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, accessibility_state: i32) -> ()>,
    pub set_auto_resize_enabled: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserHost,
            enabled: i32,
            min_size: *const CefSize,
            max_size: *const CefSize,
        ) -> (),
    >,
    pub set_audio_muted: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, mute: i32) -> ()>,
    pub is_audio_muted: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
    pub is_fullscreen: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
    pub exit_fullscreen:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, will_cause_resize: i32) -> ()>,
    pub can_execute_chrome_command:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost, command_id: i32) -> i32>,
    pub execute_chrome_command: Option<
        unsafe extern "C" fn(self_: *mut CefBrowserHost, command_id: i32, disposition: i32) -> (),
    >,
    pub is_render_process_unresponsive:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
    pub get_runtime_style: Option<unsafe extern "C" fn(self_: *mut CefBrowserHost) -> i32>,
}

#[repr(C)]
pub struct CefAudioHandler {
    pub base: CefBaseRefCounted,
    pub get_audio_parameters: Option<
        unsafe extern "C" fn(
            self_: *mut CefAudioHandler,
            browser: *mut CefBrowser,
            params: *mut c_void,
        ) -> i32,
    >,
    pub on_audio_stream_started: Option<
        unsafe extern "C" fn(
            self_: *mut CefAudioHandler,
            browser: *mut CefBrowser,
            params: *const c_void,
            channels: i32,
        ) -> (),
    >,
    pub on_audio_stream_packet: Option<
        unsafe extern "C" fn(
            self_: *mut CefAudioHandler,
            browser: *mut CefBrowser,
            data: *mut *const c_void,
            frames: i32,
            pts: i64,
        ) -> (),
    >,
    pub on_audio_stream_stopped:
        Option<unsafe extern "C" fn(self_: *mut CefAudioHandler, browser: *mut CefBrowser) -> ()>,
    pub on_audio_stream_error: Option<
        unsafe extern "C" fn(
            self_: *mut CefAudioHandler,
            browser: *mut CefBrowser,
            message: *const CefStringUtf16,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefCommandHandler {
    pub base: CefBaseRefCounted,
    pub on_chrome_command: Option<
        unsafe extern "C" fn(
            self_: *mut CefCommandHandler,
            browser: *mut CefBrowser,
            command_id: i32,
            disposition: i32,
        ) -> i32,
    >,
    pub is_chrome_app_menu_item_visible: Option<
        unsafe extern "C" fn(
            self_: *mut CefCommandHandler,
            browser: *mut CefBrowser,
            command_id: i32,
        ) -> i32,
    >,
    pub is_chrome_app_menu_item_enabled: Option<
        unsafe extern "C" fn(
            self_: *mut CefCommandHandler,
            browser: *mut CefBrowser,
            command_id: i32,
        ) -> i32,
    >,
    pub is_chrome_page_action_icon_visible:
        Option<unsafe extern "C" fn(self_: *mut CefCommandHandler, icon_type: i32) -> i32>,
    pub is_chrome_toolbar_button_visible:
        Option<unsafe extern "C" fn(self_: *mut CefCommandHandler, button_type: i32) -> i32>,
}

#[repr(C)]
pub struct CefMenuModelDelegate {
    pub base: CefBaseRefCounted,
    pub execute_command: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModelDelegate,
            menu_model: *mut CefMenuModel,
            command_id: i32,
            event_flags: i32,
        ) -> (),
    >,
    pub mouse_outside_menu: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModelDelegate,
            menu_model: *mut CefMenuModel,
            screen_point: *const CefPoint,
        ) -> (),
    >,
    pub unhandled_open_submenu: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModelDelegate,
            menu_model: *mut CefMenuModel,
            is_rtl: i32,
        ) -> (),
    >,
    pub unhandled_close_submenu: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModelDelegate,
            menu_model: *mut CefMenuModel,
            is_rtl: i32,
        ) -> (),
    >,
    pub menu_will_show: Option<
        unsafe extern "C" fn(self_: *mut CefMenuModelDelegate, menu_model: *mut CefMenuModel) -> (),
    >,
    pub menu_closed: Option<
        unsafe extern "C" fn(self_: *mut CefMenuModelDelegate, menu_model: *mut CefMenuModel) -> (),
    >,
    pub format_label: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModelDelegate,
            menu_model: *mut CefMenuModel,
            label: *mut CefStringUtf16,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefMenuModel {
    pub base: CefBaseRefCounted,
    pub is_sub_menu: Option<unsafe extern "C" fn(self_: *mut CefMenuModel) -> i32>,
    pub clear: Option<unsafe extern "C" fn(self_: *mut CefMenuModel) -> i32>,
    pub get_count: Option<unsafe extern "C" fn(self_: *mut CefMenuModel) -> usize>,
    pub add_separator: Option<unsafe extern "C" fn(self_: *mut CefMenuModel) -> i32>,
    pub add_item: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            command_id: i32,
            label: *const CefStringUtf16,
        ) -> i32,
    >,
    pub add_check_item: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            command_id: i32,
            label: *const CefStringUtf16,
        ) -> i32,
    >,
    pub add_radio_item: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            command_id: i32,
            label: *const CefStringUtf16,
            group_id: i32,
        ) -> i32,
    >,
    pub add_sub_menu: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            command_id: i32,
            label: *const CefStringUtf16,
        ) -> *mut CefMenuModel,
    >,
    pub insert_separator_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> i32>,
    pub insert_item_at: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            index: usize,
            command_id: i32,
            label: *const CefStringUtf16,
        ) -> i32,
    >,
    pub insert_check_item_at: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            index: usize,
            command_id: i32,
            label: *const CefStringUtf16,
        ) -> i32,
    >,
    pub insert_radio_item_at: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            index: usize,
            command_id: i32,
            label: *const CefStringUtf16,
            group_id: i32,
        ) -> i32,
    >,
    pub insert_sub_menu_at: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            index: usize,
            command_id: i32,
            label: *const CefStringUtf16,
        ) -> *mut CefMenuModel,
    >,
    pub remove: Option<unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> i32>,
    pub remove_at: Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> i32>,
    pub get_index_of:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> i32>,
    pub get_command_id_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> i32>,
    pub set_command_id_at: Option<
        unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize, command_id: i32) -> i32,
    >,
    pub get_label: Option<
        unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> *mut CefStringUtf16,
    >,
    pub get_label_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> *mut CefStringUtf16>,
    pub set_label: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            command_id: i32,
            label: *const CefStringUtf16,
        ) -> i32,
    >,
    pub set_label_at: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            index: usize,
            label: *const CefStringUtf16,
        ) -> i32,
    >,
    pub get_type: Option<unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> i32>,
    pub get_type_at: Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> i32>,
    pub get_group_id:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> i32>,
    pub get_group_id_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> i32>,
    pub set_group_id: Option<
        unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32, group_id: i32) -> i32,
    >,
    pub set_group_id_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize, group_id: i32) -> i32>,
    pub get_sub_menu: Option<
        unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> *mut CefMenuModel,
    >,
    pub get_sub_menu_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> *mut CefMenuModel>,
    pub is_visible: Option<unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> i32>,
    pub is_visible_at: Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> i32>,
    pub set_visible: Option<
        unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32, visible: i32) -> i32,
    >,
    pub set_visible_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize, visible: i32) -> i32>,
    pub is_enabled: Option<unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> i32>,
    pub is_enabled_at: Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> i32>,
    pub set_enabled: Option<
        unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32, enabled: i32) -> i32,
    >,
    pub set_enabled_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize, enabled: i32) -> i32>,
    pub is_checked: Option<unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> i32>,
    pub is_checked_at: Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> i32>,
    pub set_checked: Option<
        unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32, checked: i32) -> i32,
    >,
    pub set_checked_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize, checked: i32) -> i32>,
    pub has_accelerator:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> i32>,
    pub has_accelerator_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> i32>,
    pub set_accelerator: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            command_id: i32,
            key_code: i32,
            shift_pressed: i32,
            ctrl_pressed: i32,
            alt_pressed: i32,
        ) -> i32,
    >,
    pub set_accelerator_at: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            index: usize,
            key_code: i32,
            shift_pressed: i32,
            ctrl_pressed: i32,
            alt_pressed: i32,
        ) -> i32,
    >,
    pub remove_accelerator:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, command_id: i32) -> i32>,
    pub remove_accelerator_at:
        Option<unsafe extern "C" fn(self_: *mut CefMenuModel, index: usize) -> i32>,
    pub get_accelerator: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            command_id: i32,
            key_code: *mut i32,
            shift_pressed: *mut i32,
            ctrl_pressed: *mut i32,
            alt_pressed: *mut i32,
        ) -> i32,
    >,
    pub get_accelerator_at: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            index: usize,
            key_code: *mut i32,
            shift_pressed: *mut i32,
            ctrl_pressed: *mut i32,
            alt_pressed: *mut i32,
        ) -> i32,
    >,
    pub set_color: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            command_id: i32,
            color_type: i32,
            color: u32,
        ) -> i32,
    >,
    pub set_color_at: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            index: i32,
            color_type: i32,
            color: u32,
        ) -> i32,
    >,
    pub get_color: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            command_id: i32,
            color_type: i32,
            color: *mut c_void,
        ) -> i32,
    >,
    pub get_color_at: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            index: i32,
            color_type: i32,
            color: *mut c_void,
        ) -> i32,
    >,
    pub set_font_list: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            command_id: i32,
            font_list: *const CefStringUtf16,
        ) -> i32,
    >,
    pub set_font_list_at: Option<
        unsafe extern "C" fn(
            self_: *mut CefMenuModel,
            index: i32,
            font_list: *const CefStringUtf16,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefRunContextMenuCallback {
    pub base: CefBaseRefCounted,
    pub cont: Option<
        unsafe extern "C" fn(
            self_: *mut CefRunContextMenuCallback,
            command_id: i32,
            event_flags: i32,
        ) -> (),
    >,
    pub cancel: Option<unsafe extern "C" fn(self_: *mut CefRunContextMenuCallback) -> ()>,
}

#[repr(C)]
pub struct CefRunQuickMenuCallback {
    pub base: CefBaseRefCounted,
    pub cont: Option<
        unsafe extern "C" fn(
            self_: *mut CefRunQuickMenuCallback,
            command_id: i32,
            event_flags: i32,
        ) -> (),
    >,
    pub cancel: Option<unsafe extern "C" fn(self_: *mut CefRunQuickMenuCallback) -> ()>,
}

#[repr(C)]
pub struct CefContextMenuHandler {
    pub base: CefBaseRefCounted,
    pub on_before_context_menu: Option<
        unsafe extern "C" fn(
            self_: *mut CefContextMenuHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            params: *mut CefContextMenuParams,
            model: *mut CefMenuModel,
        ) -> (),
    >,
    pub run_context_menu: Option<
        unsafe extern "C" fn(
            self_: *mut CefContextMenuHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            params: *mut CefContextMenuParams,
            model: *mut CefMenuModel,
            callback: *mut CefRunContextMenuCallback,
        ) -> i32,
    >,
    pub on_context_menu_command: Option<
        unsafe extern "C" fn(
            self_: *mut CefContextMenuHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            params: *mut CefContextMenuParams,
            command_id: i32,
            event_flags: i32,
        ) -> i32,
    >,
    pub on_context_menu_dismissed: Option<
        unsafe extern "C" fn(
            self_: *mut CefContextMenuHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
        ) -> (),
    >,
    pub run_quick_menu: Option<
        unsafe extern "C" fn(
            self_: *mut CefContextMenuHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            location: *const CefPoint,
            size: *const CefSize,
            edit_state_flags: i32,
            callback: *mut CefRunQuickMenuCallback,
        ) -> i32,
    >,
    pub on_quick_menu_command: Option<
        unsafe extern "C" fn(
            self_: *mut CefContextMenuHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            command_id: i32,
            event_flags: i32,
        ) -> i32,
    >,
    pub on_quick_menu_dismissed: Option<
        unsafe extern "C" fn(
            self_: *mut CefContextMenuHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefContextMenuParams {
    pub base: CefBaseRefCounted,
    pub get_xcoord: Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> i32>,
    pub get_ycoord: Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> i32>,
    pub get_type_flags: Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> i32>,
    pub get_link_url:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> *mut CefStringUtf16>,
    pub get_unfiltered_link_url:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> *mut CefStringUtf16>,
    pub get_source_url:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> *mut CefStringUtf16>,
    pub has_image_contents: Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> i32>,
    pub get_title_text:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> *mut CefStringUtf16>,
    pub get_page_url:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> *mut CefStringUtf16>,
    pub get_frame_url:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> *mut CefStringUtf16>,
    pub get_frame_charset:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> *mut CefStringUtf16>,
    pub get_media_type: Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> i32>,
    pub get_media_state_flags:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> i32>,
    pub get_selection_text:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> *mut CefStringUtf16>,
    pub get_misspelled_word:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> *mut CefStringUtf16>,
    pub get_dictionary_suggestions: Option<
        unsafe extern "C" fn(self_: *mut CefContextMenuParams, suggestions: *mut c_void) -> i32,
    >,
    pub is_editable: Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> i32>,
    pub is_spell_check_enabled:
        Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> i32>,
    pub get_edit_state_flags: Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> i32>,
    pub is_custom_menu: Option<unsafe extern "C" fn(self_: *mut CefContextMenuParams) -> i32>,
}

#[repr(C)]
pub struct CefFileDialogCallback {
    pub base: CefBaseRefCounted,
    pub cont: Option<
        unsafe extern "C" fn(self_: *mut CefFileDialogCallback, file_paths: *mut c_void) -> (),
    >,
    pub cancel: Option<unsafe extern "C" fn(self_: *mut CefFileDialogCallback) -> ()>,
}

#[repr(C)]
pub struct CefDialogHandler {
    pub base: CefBaseRefCounted,
    pub on_file_dialog: Option<
        unsafe extern "C" fn(
            self_: *mut CefDialogHandler,
            browser: *mut CefBrowser,
            mode: i32,
            title: *const CefStringUtf16,
            default_file_path: *const CefStringUtf16,
            accept_filters: *mut c_void,
            accept_extensions: *mut c_void,
            accept_descriptions: *mut c_void,
            callback: *mut CefFileDialogCallback,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefDisplayHandler {
    pub base: CefBaseRefCounted,
    pub on_address_change: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            url: *const CefStringUtf16,
        ) -> (),
    >,
    pub on_title_change: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            title: *const CefStringUtf16,
        ) -> (),
    >,
    pub on_favicon_urlchange: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            icon_urls: *mut c_void,
        ) -> (),
    >,
    pub on_fullscreen_mode_change: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            fullscreen: i32,
        ) -> (),
    >,
    pub on_tooltip: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            text: *mut CefStringUtf16,
        ) -> i32,
    >,
    pub on_status_message: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            value: *const CefStringUtf16,
        ) -> (),
    >,
    pub on_console_message: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            level: i32,
            message: *const CefStringUtf16,
            source: *const CefStringUtf16,
            line: i32,
        ) -> i32,
    >,
    pub on_auto_resize: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            new_size: *const CefSize,
        ) -> i32,
    >,
    pub on_loading_progress_change: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            progress: f64,
        ) -> (),
    >,
    pub on_cursor_change: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            cursor: *mut c_void,
            type_: i32,
            custom_cursor_info: *const CefCursorInfo,
        ) -> i32,
    >,
    pub on_media_access_change: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            has_video_access: i32,
            has_audio_access: i32,
        ) -> (),
    >,
    pub on_contents_bounds_change: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            new_bounds: *const CefRect,
        ) -> i32,
    >,
    pub get_root_window_screen_rect: Option<
        unsafe extern "C" fn(
            self_: *mut CefDisplayHandler,
            browser: *mut CefBrowser,
            rect: *mut CefRect,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefDownloadItem {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i32>,
    pub is_in_progress: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i32>,
    pub is_complete: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i32>,
    pub is_canceled: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i32>,
    pub is_interrupted: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i32>,
    pub get_interrupt_reason: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i32>,
    pub get_current_speed: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i64>,
    pub get_percent_complete: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i32>,
    pub get_total_bytes: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i64>,
    pub get_received_bytes: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i64>,
    pub get_start_time: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i64>,
    pub get_end_time: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i64>,
    pub get_full_path:
        Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> *mut CefStringUtf16>,
    pub get_id: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> u32>,
    pub get_url: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> *mut CefStringUtf16>,
    pub get_original_url:
        Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> *mut CefStringUtf16>,
    pub get_suggested_file_name:
        Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> *mut CefStringUtf16>,
    pub get_content_disposition:
        Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> *mut CefStringUtf16>,
    pub get_mime_type:
        Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> *mut CefStringUtf16>,
    pub is_paused: Option<unsafe extern "C" fn(self_: *mut CefDownloadItem) -> i32>,
}

#[repr(C)]
pub struct CefBeforeDownloadCallback {
    pub base: CefBaseRefCounted,
    pub cont: Option<
        unsafe extern "C" fn(
            self_: *mut CefBeforeDownloadCallback,
            download_path: *const CefStringUtf16,
            show_dialog: i32,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefDownloadItemCallback {
    pub base: CefBaseRefCounted,
    pub cancel: Option<unsafe extern "C" fn(self_: *mut CefDownloadItemCallback) -> ()>,
    pub pause: Option<unsafe extern "C" fn(self_: *mut CefDownloadItemCallback) -> ()>,
    pub resume: Option<unsafe extern "C" fn(self_: *mut CefDownloadItemCallback) -> ()>,
}

#[repr(C)]
pub struct CefDownloadHandler {
    pub base: CefBaseRefCounted,
    pub can_download: Option<
        unsafe extern "C" fn(
            self_: *mut CefDownloadHandler,
            browser: *mut CefBrowser,
            url: *const CefStringUtf16,
            request_method: *const CefStringUtf16,
        ) -> i32,
    >,
    pub on_before_download: Option<
        unsafe extern "C" fn(
            self_: *mut CefDownloadHandler,
            browser: *mut CefBrowser,
            download_item: *mut CefDownloadItem,
            suggested_name: *const CefStringUtf16,
            callback: *mut CefBeforeDownloadCallback,
        ) -> i32,
    >,
    pub on_download_updated: Option<
        unsafe extern "C" fn(
            self_: *mut CefDownloadHandler,
            browser: *mut CefBrowser,
            download_item: *mut CefDownloadItem,
            callback: *mut CefDownloadItemCallback,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefDragHandler {
    pub base: CefBaseRefCounted,
    pub on_drag_enter: Option<
        unsafe extern "C" fn(
            self_: *mut CefDragHandler,
            browser: *mut CefBrowser,
            dragData: *mut CefDragData,
            mask: i32,
        ) -> i32,
    >,
    pub on_draggable_regions_changed: Option<
        unsafe extern "C" fn(
            self_: *mut CefDragHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            regionsCount: usize,
            regions: *const c_void,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefFindHandler {
    pub base: CefBaseRefCounted,
    pub on_find_result: Option<
        unsafe extern "C" fn(
            self_: *mut CefFindHandler,
            browser: *mut CefBrowser,
            identifier: i32,
            count: i32,
            selectionRect: *const CefRect,
            activeMatchOrdinal: i32,
            finalUpdate: i32,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefFocusHandler {
    pub base: CefBaseRefCounted,
    pub on_take_focus: Option<
        unsafe extern "C" fn(
            self_: *mut CefFocusHandler,
            browser: *mut CefBrowser,
            next: i32,
        ) -> (),
    >,
    pub on_set_focus: Option<
        unsafe extern "C" fn(
            self_: *mut CefFocusHandler,
            browser: *mut CefBrowser,
            source: i32,
        ) -> i32,
    >,
    pub on_got_focus:
        Option<unsafe extern "C" fn(self_: *mut CefFocusHandler, browser: *mut CefBrowser) -> ()>,
}

#[repr(C)]
pub struct CefFrameHandler {
    pub base: CefBaseRefCounted,
    pub on_frame_created: Option<
        unsafe extern "C" fn(
            self_: *mut CefFrameHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
        ) -> (),
    >,
    pub on_frame_destroyed: Option<
        unsafe extern "C" fn(
            self_: *mut CefFrameHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
        ) -> (),
    >,
    pub on_frame_attached: Option<
        unsafe extern "C" fn(
            self_: *mut CefFrameHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            reattached: i32,
        ) -> (),
    >,
    pub on_frame_detached: Option<
        unsafe extern "C" fn(
            self_: *mut CefFrameHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
        ) -> (),
    >,
    pub on_main_frame_changed: Option<
        unsafe extern "C" fn(
            self_: *mut CefFrameHandler,
            browser: *mut CefBrowser,
            old_frame: *mut CefFrame,
            new_frame: *mut CefFrame,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefJsdialogCallback {
    pub base: CefBaseRefCounted,
    pub cont: Option<
        unsafe extern "C" fn(
            self_: *mut CefJsdialogCallback,
            success: i32,
            user_input: *const CefStringUtf16,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefJsdialogHandler {
    pub base: CefBaseRefCounted,
    pub on_jsdialog: Option<
        unsafe extern "C" fn(
            self_: *mut CefJsdialogHandler,
            browser: *mut CefBrowser,
            origin_url: *const CefStringUtf16,
            dialog_type: i32,
            message_text: *const CefStringUtf16,
            default_prompt_text: *const CefStringUtf16,
            callback: *mut CefJsdialogCallback,
            suppress_message: *mut i32,
        ) -> i32,
    >,
    pub on_before_unload_dialog: Option<
        unsafe extern "C" fn(
            self_: *mut CefJsdialogHandler,
            browser: *mut CefBrowser,
            message_text: *const CefStringUtf16,
            is_reload: i32,
            callback: *mut CefJsdialogCallback,
        ) -> i32,
    >,
    pub on_reset_dialog_state: Option<
        unsafe extern "C" fn(self_: *mut CefJsdialogHandler, browser: *mut CefBrowser) -> (),
    >,
    pub on_dialog_closed: Option<
        unsafe extern "C" fn(self_: *mut CefJsdialogHandler, browser: *mut CefBrowser) -> (),
    >,
}

#[repr(C)]
pub struct CefKeyboardHandler {
    pub base: CefBaseRefCounted,
    pub on_pre_key_event: Option<
        unsafe extern "C" fn(
            self_: *mut CefKeyboardHandler,
            browser: *mut CefBrowser,
            event: *const CefKeyEvent,
            os_event: *mut c_void,
            is_keyboard_shortcut: *mut i32,
        ) -> i32,
    >,
    pub on_key_event: Option<
        unsafe extern "C" fn(
            self_: *mut CefKeyboardHandler,
            browser: *mut CefBrowser,
            event: *const CefKeyEvent,
            os_event: *mut c_void,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefLifeSpanHandler {
    pub base: CefBaseRefCounted,
    pub on_before_popup: Option<
        unsafe extern "C" fn(
            self_: *mut CefLifeSpanHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            popup_id: i32,
            target_url: *const CefStringUtf16,
            target_frame_name: *const CefStringUtf16,
            target_disposition: i32,
            user_gesture: i32,
            popupFeatures: *const c_void,
            windowInfo: *mut c_void,
            client: *mut *mut CefClient,
            settings: *mut c_void,
            extra_info: *mut *mut CefDictionaryValue,
            no_javascript_access: *mut i32,
        ) -> i32,
    >,
    pub on_before_popup_aborted: Option<
        unsafe extern "C" fn(
            self_: *mut CefLifeSpanHandler,
            browser: *mut CefBrowser,
            popup_id: i32,
        ) -> (),
    >,
    pub on_before_dev_tools_popup: Option<
        unsafe extern "C" fn(
            self_: *mut CefLifeSpanHandler,
            browser: *mut CefBrowser,
            windowInfo: *mut c_void,
            client: *mut *mut CefClient,
            settings: *mut c_void,
            extra_info: *mut *mut CefDictionaryValue,
            use_default_window: *mut i32,
        ) -> (),
    >,
    pub on_after_created: Option<
        unsafe extern "C" fn(self_: *mut CefLifeSpanHandler, browser: *mut CefBrowser) -> (),
    >,
    pub do_close: Option<
        unsafe extern "C" fn(self_: *mut CefLifeSpanHandler, browser: *mut CefBrowser) -> i32,
    >,
    pub on_before_close: Option<
        unsafe extern "C" fn(self_: *mut CefLifeSpanHandler, browser: *mut CefBrowser) -> (),
    >,
}

#[repr(C)]
pub struct CefLoadHandler {
    pub base: CefBaseRefCounted,
    pub on_loading_state_change: Option<
        unsafe extern "C" fn(
            self_: *mut CefLoadHandler,
            browser: *mut CefBrowser,
            isLoading: i32,
            canGoBack: i32,
            canGoForward: i32,
        ) -> (),
    >,
    pub on_load_start: Option<
        unsafe extern "C" fn(
            self_: *mut CefLoadHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            transition_type: i32,
        ) -> (),
    >,
    pub on_load_end: Option<
        unsafe extern "C" fn(
            self_: *mut CefLoadHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            httpStatusCode: i32,
        ) -> (),
    >,
    pub on_load_error: Option<
        unsafe extern "C" fn(
            self_: *mut CefLoadHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            errorCode: i32,
            errorText: *const CefStringUtf16,
            failedUrl: *const CefStringUtf16,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefMediaAccessCallback {
    pub base: CefBaseRefCounted,
    pub cont: Option<
        unsafe extern "C" fn(self_: *mut CefMediaAccessCallback, allowed_permissions: u32) -> (),
    >,
    pub cancel: Option<unsafe extern "C" fn(self_: *mut CefMediaAccessCallback) -> ()>,
}

#[repr(C)]
pub struct CefPermissionPromptCallback {
    pub base: CefBaseRefCounted,
    pub cont:
        Option<unsafe extern "C" fn(self_: *mut CefPermissionPromptCallback, result: i32) -> ()>,
}

#[repr(C)]
pub struct CefPermissionHandler {
    pub base: CefBaseRefCounted,
    pub on_request_media_access_permission: Option<
        unsafe extern "C" fn(
            self_: *mut CefPermissionHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            requesting_origin: *const CefStringUtf16,
            requested_permissions: u32,
            callback: *mut CefMediaAccessCallback,
        ) -> i32,
    >,
    pub on_show_permission_prompt: Option<
        unsafe extern "C" fn(
            self_: *mut CefPermissionHandler,
            browser: *mut CefBrowser,
            prompt_id: u64,
            requesting_origin: *const CefStringUtf16,
            requested_permissions: u32,
            callback: *mut CefPermissionPromptCallback,
        ) -> i32,
    >,
    pub on_dismiss_permission_prompt: Option<
        unsafe extern "C" fn(
            self_: *mut CefPermissionHandler,
            browser: *mut CefBrowser,
            prompt_id: u64,
            result: i32,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefPrintSettings {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> i32>,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> i32>,
    pub set_orientation:
        Option<unsafe extern "C" fn(self_: *mut CefPrintSettings, landscape: i32) -> ()>,
    pub is_landscape: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> i32>,
    pub set_printer_printable_area: Option<
        unsafe extern "C" fn(
            self_: *mut CefPrintSettings,
            physical_size_device_units: *const CefSize,
            printable_area_device_units: *const CefRect,
            landscape_needs_flip: i32,
        ) -> (),
    >,
    pub set_device_name: Option<
        unsafe extern "C" fn(self_: *mut CefPrintSettings, name: *const CefStringUtf16) -> (),
    >,
    pub get_device_name:
        Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> *mut CefStringUtf16>,
    pub set_dpi: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings, dpi: i32) -> ()>,
    pub get_dpi: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> i32>,
    pub set_page_ranges: Option<
        unsafe extern "C" fn(
            self_: *mut CefPrintSettings,
            rangesCount: usize,
            ranges: *const CefRange,
        ) -> (),
    >,
    pub get_page_ranges_count: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> usize>,
    pub get_page_ranges: Option<
        unsafe extern "C" fn(
            self_: *mut CefPrintSettings,
            rangesCount: *mut c_void,
            ranges: *mut CefRange,
        ) -> (),
    >,
    pub set_selection_only:
        Option<unsafe extern "C" fn(self_: *mut CefPrintSettings, selection_only: i32) -> ()>,
    pub is_selection_only: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> i32>,
    pub set_collate: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings, collate: i32) -> ()>,
    pub will_collate: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> i32>,
    pub set_color_model:
        Option<unsafe extern "C" fn(self_: *mut CefPrintSettings, model: i32) -> ()>,
    pub get_color_model: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> i32>,
    pub set_copies: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings, copies: i32) -> ()>,
    pub get_copies: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> i32>,
    pub set_duplex_mode:
        Option<unsafe extern "C" fn(self_: *mut CefPrintSettings, mode: i32) -> ()>,
    pub get_duplex_mode: Option<unsafe extern "C" fn(self_: *mut CefPrintSettings) -> i32>,
}

#[repr(C)]
pub struct CefPrintDialogCallback {
    pub base: CefBaseRefCounted,
    pub cont: Option<
        unsafe extern "C" fn(
            self_: *mut CefPrintDialogCallback,
            settings: *mut CefPrintSettings,
        ) -> (),
    >,
    pub cancel: Option<unsafe extern "C" fn(self_: *mut CefPrintDialogCallback) -> ()>,
}

#[repr(C)]
pub struct CefPrintJobCallback {
    pub base: CefBaseRefCounted,
    pub cont: Option<unsafe extern "C" fn(self_: *mut CefPrintJobCallback) -> ()>,
}

#[repr(C)]
pub struct CefPrintHandler {
    pub base: CefBaseRefCounted,
    pub on_print_start:
        Option<unsafe extern "C" fn(self_: *mut CefPrintHandler, browser: *mut CefBrowser) -> ()>,
    pub on_print_settings: Option<
        unsafe extern "C" fn(
            self_: *mut CefPrintHandler,
            browser: *mut CefBrowser,
            settings: *mut CefPrintSettings,
            get_defaults: i32,
        ) -> (),
    >,
    pub on_print_dialog: Option<
        unsafe extern "C" fn(
            self_: *mut CefPrintHandler,
            browser: *mut CefBrowser,
            has_selection: i32,
            callback: *mut CefPrintDialogCallback,
        ) -> i32,
    >,
    pub on_print_job: Option<
        unsafe extern "C" fn(
            self_: *mut CefPrintHandler,
            browser: *mut CefBrowser,
            document_name: *const CefStringUtf16,
            pdf_file_path: *const CefStringUtf16,
            callback: *mut CefPrintJobCallback,
        ) -> i32,
    >,
    pub on_print_reset:
        Option<unsafe extern "C" fn(self_: *mut CefPrintHandler, browser: *mut CefBrowser) -> ()>,
    pub get_pdf_paper_size: Option<
        unsafe extern "C" fn(
            self_: *mut CefPrintHandler,
            browser: *mut CefBrowser,
            device_units_per_inch: i32,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefAccessibilityHandler {
    pub base: CefBaseRefCounted,
    pub on_accessibility_tree_change: Option<
        unsafe extern "C" fn(self_: *mut CefAccessibilityHandler, value: *mut CefValue) -> (),
    >,
    pub on_accessibility_location_change: Option<
        unsafe extern "C" fn(self_: *mut CefAccessibilityHandler, value: *mut CefValue) -> (),
    >,
}

#[repr(C)]
pub struct CefRenderHandler {
    pub base: CefBaseRefCounted,
    pub get_accessibility_handler:
        Option<unsafe extern "C" fn(self_: *mut CefRenderHandler) -> *mut CefAccessibilityHandler>,
    pub get_root_screen_rect: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            rect: *mut CefRect,
        ) -> i32,
    >,
    pub get_view_rect: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            rect: *mut CefRect,
        ) -> (),
    >,
    pub get_screen_point: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            viewX: i32,
            viewY: i32,
            screenX: *mut i32,
            screenY: *mut i32,
        ) -> i32,
    >,
    pub get_screen_info: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            screen_info: *mut CefScreenInfo,
        ) -> i32,
    >,
    pub on_popup_show: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            show: i32,
        ) -> (),
    >,
    pub on_popup_size: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            rect: *const CefRect,
        ) -> (),
    >,
    pub on_paint: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            type_: i32,
            dirtyRectsCount: usize,
            dirtyRects: *const CefRect,
            buffer: *const c_void,
            width: i32,
            height: i32,
        ) -> (),
    >,
    pub on_accelerated_paint: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            type_: i32,
            dirtyRectsCount: usize,
            dirtyRects: *const CefRect,
            info: *const c_void,
        ) -> (),
    >,
    pub get_touch_handle_size: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            orientation: i32,
            size: *mut CefSize,
        ) -> (),
    >,
    pub on_touch_handle_state_changed: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            state: *const c_void,
        ) -> (),
    >,
    pub start_dragging: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            drag_data: *mut CefDragData,
            allowed_ops: i32,
            x: i32,
            y: i32,
        ) -> i32,
    >,
    pub update_drag_cursor: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            operation: i32,
        ) -> (),
    >,
    pub on_scroll_offset_changed: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            x: f64,
            y: f64,
        ) -> (),
    >,
    pub on_ime_composition_range_changed: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            selected_range: *const CefRange,
            character_boundsCount: usize,
            character_bounds: *const CefRect,
        ) -> (),
    >,
    pub on_text_selection_changed: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            selected_text: *const CefStringUtf16,
            selected_range: *const CefRange,
        ) -> (),
    >,
    pub on_virtual_keyboard_requested: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderHandler,
            browser: *mut CefBrowser,
            input_mode: i32,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefAuthCallback {
    pub base: CefBaseRefCounted,
    pub cont: Option<
        unsafe extern "C" fn(
            self_: *mut CefAuthCallback,
            username: *const CefStringUtf16,
            password: *const CefStringUtf16,
        ) -> (),
    >,
    pub cancel: Option<unsafe extern "C" fn(self_: *mut CefAuthCallback) -> ()>,
}

#[repr(C)]
pub struct CefResponse {
    pub base: CefBaseRefCounted,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefResponse) -> i32>,
    pub get_error: Option<unsafe extern "C" fn(self_: *mut CefResponse) -> i32>,
    pub set_error: Option<unsafe extern "C" fn(self_: *mut CefResponse, error: i32) -> ()>,
    pub get_status: Option<unsafe extern "C" fn(self_: *mut CefResponse) -> i32>,
    pub set_status: Option<unsafe extern "C" fn(self_: *mut CefResponse, status: i32) -> ()>,
    pub get_status_text:
        Option<unsafe extern "C" fn(self_: *mut CefResponse) -> *mut CefStringUtf16>,
    pub set_status_text: Option<
        unsafe extern "C" fn(self_: *mut CefResponse, statusText: *const CefStringUtf16) -> (),
    >,
    pub get_mime_type: Option<unsafe extern "C" fn(self_: *mut CefResponse) -> *mut CefStringUtf16>,
    pub set_mime_type: Option<
        unsafe extern "C" fn(self_: *mut CefResponse, mimeType: *const CefStringUtf16) -> (),
    >,
    pub get_charset: Option<unsafe extern "C" fn(self_: *mut CefResponse) -> *mut CefStringUtf16>,
    pub set_charset:
        Option<unsafe extern "C" fn(self_: *mut CefResponse, charset: *const CefStringUtf16) -> ()>,
    pub get_header_by_name: Option<
        unsafe extern "C" fn(
            self_: *mut CefResponse,
            name: *const CefStringUtf16,
        ) -> *mut CefStringUtf16,
    >,
    pub set_header_by_name: Option<
        unsafe extern "C" fn(
            self_: *mut CefResponse,
            name: *const CefStringUtf16,
            value: *const CefStringUtf16,
            overwrite: i32,
        ) -> (),
    >,
    pub get_header_map: Option<unsafe extern "C" fn(self_: *mut CefResponse, headerMap: i32) -> ()>,
    pub set_header_map: Option<unsafe extern "C" fn(self_: *mut CefResponse, headerMap: i32) -> ()>,
    pub get_url: Option<unsafe extern "C" fn(self_: *mut CefResponse) -> *mut CefStringUtf16>,
    pub set_url:
        Option<unsafe extern "C" fn(self_: *mut CefResponse, url: *const CefStringUtf16) -> ()>,
}

#[repr(C)]
pub struct CefResourceSkipCallback {
    pub base: CefBaseRefCounted,
    pub cont:
        Option<unsafe extern "C" fn(self_: *mut CefResourceSkipCallback, bytes_skipped: i64) -> ()>,
}

#[repr(C)]
pub struct CefResourceReadCallback {
    pub base: CefBaseRefCounted,
    pub cont:
        Option<unsafe extern "C" fn(self_: *mut CefResourceReadCallback, bytes_read: i32) -> ()>,
}

#[repr(C)]
pub struct CefResourceHandler {
    pub base: CefBaseRefCounted,
    pub open: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceHandler,
            request: *mut CefRequest,
            handle_request: *mut i32,
            callback: *mut CefCallback,
        ) -> i32,
    >,
    pub process_request: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceHandler,
            request: *mut CefRequest,
            callback: *mut CefCallback,
        ) -> i32,
    >,
    pub get_response_headers: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceHandler,
            response: *mut CefResponse,
            response_length: *mut c_void,
            redirectUrl: *mut CefStringUtf16,
        ) -> (),
    >,
    pub skip: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceHandler,
            bytes_to_skip: i64,
            bytes_skipped: *mut c_void,
            callback: *mut CefResourceSkipCallback,
        ) -> i32,
    >,
    pub read: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceHandler,
            data_out: *mut c_void,
            bytes_to_read: i32,
            bytes_read: *mut i32,
            callback: *mut CefResourceReadCallback,
        ) -> i32,
    >,
    pub read_response: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceHandler,
            data_out: *mut c_void,
            bytes_to_read: i32,
            bytes_read: *mut i32,
            callback: *mut CefCallback,
        ) -> i32,
    >,
    pub cancel: Option<unsafe extern "C" fn(self_: *mut CefResourceHandler) -> ()>,
}

#[repr(C)]
pub struct CefResponseFilter {
    pub base: CefBaseRefCounted,
    pub init_filter: Option<unsafe extern "C" fn(self_: *mut CefResponseFilter) -> i32>,
    pub filter: Option<
        unsafe extern "C" fn(
            self_: *mut CefResponseFilter,
            data_in: *mut c_void,
            data_in_size: usize,
            data_in_read: *mut c_void,
            data_out: *mut c_void,
            data_out_size: usize,
            data_out_written: *mut c_void,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefResourceRequestHandler {
    pub base: CefBaseRefCounted,
    pub get_cookie_access_filter: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
        ) -> *mut CefCookieAccessFilter,
    >,
    pub on_before_resource_load: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            callback: *mut CefCallback,
        ) -> i32,
    >,
    pub get_resource_handler: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
        ) -> *mut CefResourceHandler,
    >,
    pub on_resource_redirect: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            response: *mut CefResponse,
            new_url: *mut CefStringUtf16,
        ) -> (),
    >,
    pub on_resource_response: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            response: *mut CefResponse,
        ) -> i32,
    >,
    pub get_resource_response_filter: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            response: *mut CefResponse,
        ) -> *mut CefResponseFilter,
    >,
    pub on_resource_load_complete: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            response: *mut CefResponse,
            status: i32,
            received_content_length: i64,
        ) -> (),
    >,
    pub on_protocol_execution: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            allow_os_execution: *mut i32,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefCookieAccessFilter {
    pub base: CefBaseRefCounted,
    pub can_send_cookie: Option<
        unsafe extern "C" fn(
            self_: *mut CefCookieAccessFilter,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            cookie: *const c_void,
        ) -> i32,
    >,
    pub can_save_cookie: Option<
        unsafe extern "C" fn(
            self_: *mut CefCookieAccessFilter,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            response: *mut CefResponse,
            cookie: *const c_void,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefSslinfo {
    pub base: CefBaseRefCounted,
    pub get_cert_status: Option<unsafe extern "C" fn(self_: *mut CefSslinfo) -> i32>,
    pub get_x509_certificate:
        Option<unsafe extern "C" fn(self_: *mut CefSslinfo) -> *mut CefX509Certificate>,
}

#[repr(C)]
pub struct CefUnresponsiveProcessCallback {
    pub base: CefBaseRefCounted,
    pub wait: Option<unsafe extern "C" fn(self_: *mut CefUnresponsiveProcessCallback) -> ()>,
    pub terminate: Option<unsafe extern "C" fn(self_: *mut CefUnresponsiveProcessCallback) -> ()>,
}

#[repr(C)]
pub struct CefSelectClientCertificateCallback {
    pub base: CefBaseRefCounted,
    pub select: Option<
        unsafe extern "C" fn(
            self_: *mut CefSelectClientCertificateCallback,
            cert: *mut CefX509Certificate,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefRequestHandler {
    pub base: CefBaseRefCounted,
    pub on_before_browse: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            user_gesture: i32,
            is_redirect: i32,
        ) -> i32,
    >,
    pub on_open_urlfrom_tab: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            target_url: *const CefStringUtf16,
            target_disposition: i32,
            user_gesture: i32,
        ) -> i32,
    >,
    pub get_resource_request_handler: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequestHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            is_navigation: i32,
            is_download: i32,
            request_initiator: *const CefStringUtf16,
            disable_default_handling: *mut i32,
        ) -> *mut CefResourceRequestHandler,
    >,
    pub get_auth_credentials: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequestHandler,
            browser: *mut CefBrowser,
            origin_url: *const CefStringUtf16,
            isProxy: i32,
            host: *const CefStringUtf16,
            port: i32,
            realm: *const CefStringUtf16,
            scheme: *const CefStringUtf16,
            callback: *mut CefAuthCallback,
        ) -> i32,
    >,
    pub on_certificate_error: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequestHandler,
            browser: *mut CefBrowser,
            cert_error: i32,
            request_url: *const CefStringUtf16,
            ssl_info: *mut CefSslinfo,
            callback: *mut CefCallback,
        ) -> i32,
    >,
    pub on_select_client_certificate: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequestHandler,
            browser: *mut CefBrowser,
            isProxy: i32,
            host: *const CefStringUtf16,
            port: i32,
            certificatesCount: usize,
            certificates: *mut *const CefX509Certificate,
            callback: *mut CefSelectClientCertificateCallback,
        ) -> i32,
    >,
    pub on_render_view_ready:
        Option<unsafe extern "C" fn(self_: *mut CefRequestHandler, browser: *mut CefBrowser) -> ()>,
    pub on_render_process_unresponsive: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequestHandler,
            browser: *mut CefBrowser,
            callback: *mut CefUnresponsiveProcessCallback,
        ) -> i32,
    >,
    pub on_render_process_responsive:
        Option<unsafe extern "C" fn(self_: *mut CefRequestHandler, browser: *mut CefBrowser) -> ()>,
    pub on_render_process_terminated: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequestHandler,
            browser: *mut CefBrowser,
            status: i32,
            error_code: i32,
            error_string: *const CefStringUtf16,
        ) -> (),
    >,
    pub on_document_available_in_main_frame:
        Option<unsafe extern "C" fn(self_: *mut CefRequestHandler, browser: *mut CefBrowser) -> ()>,
}

#[repr(C)]
pub struct CefClient {
    pub base: CefBaseRefCounted,
    pub get_audio_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefAudioHandler>,
    pub get_command_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefCommandHandler>,
    pub get_context_menu_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefContextMenuHandler>,
    pub get_dialog_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefDialogHandler>,
    pub get_display_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefDisplayHandler>,
    pub get_download_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefDownloadHandler>,
    pub get_drag_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefDragHandler>,
    pub get_find_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefFindHandler>,
    pub get_focus_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefFocusHandler>,
    pub get_frame_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefFrameHandler>,
    pub get_permission_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefPermissionHandler>,
    pub get_jsdialog_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefJsdialogHandler>,
    pub get_keyboard_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefKeyboardHandler>,
    pub get_life_span_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefLifeSpanHandler>,
    pub get_load_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefLoadHandler>,
    pub get_print_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefPrintHandler>,
    pub get_render_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefRenderHandler>,
    pub get_request_handler:
        Option<unsafe extern "C" fn(self_: *mut CefClient) -> *mut CefRequestHandler>,
    pub on_process_message_received: Option<
        unsafe extern "C" fn(
            self_: *mut CefClient,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            source_process: i32,
            message: *mut CefProcessMessage,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefCommandLine {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefCommandLine) -> i32>,
    pub is_read_only: Option<unsafe extern "C" fn(self_: *mut CefCommandLine) -> i32>,
    pub copy: Option<unsafe extern "C" fn(self_: *mut CefCommandLine) -> *mut CefCommandLine>,
    pub init_from_argv: Option<
        unsafe extern "C" fn(
            self_: *mut CefCommandLine,
            argc: i32,
            argv: *mut *const libc::c_char,
        ) -> (),
    >,
    pub init_from_string: Option<
        unsafe extern "C" fn(self_: *mut CefCommandLine, command_line: *const CefStringUtf16) -> (),
    >,
    pub reset: Option<unsafe extern "C" fn(self_: *mut CefCommandLine) -> ()>,
    pub get_argv: Option<unsafe extern "C" fn(self_: *mut CefCommandLine, argv: *mut c_void) -> ()>,
    pub get_command_line_string:
        Option<unsafe extern "C" fn(self_: *mut CefCommandLine) -> *mut CefStringUtf16>,
    pub get_program:
        Option<unsafe extern "C" fn(self_: *mut CefCommandLine) -> *mut CefStringUtf16>,
    pub set_program: Option<
        unsafe extern "C" fn(self_: *mut CefCommandLine, program: *const CefStringUtf16) -> (),
    >,
    pub has_switches: Option<unsafe extern "C" fn(self_: *mut CefCommandLine) -> i32>,
    pub has_switch: Option<
        unsafe extern "C" fn(self_: *mut CefCommandLine, name: *const CefStringUtf16) -> i32,
    >,
    pub get_switch_value: Option<
        unsafe extern "C" fn(
            self_: *mut CefCommandLine,
            name: *const CefStringUtf16,
        ) -> *mut CefStringUtf16,
    >,
    pub get_switches: Option<unsafe extern "C" fn(self_: *mut CefCommandLine, switches: i32) -> ()>,
    pub append_switch:
        Option<unsafe extern "C" fn(self_: *mut CefCommandLine, name: *const CefStringUtf16) -> ()>,
    pub append_switch_with_value: Option<
        unsafe extern "C" fn(
            self_: *mut CefCommandLine,
            name: *const CefStringUtf16,
            value: *const CefStringUtf16,
        ) -> (),
    >,
    pub has_arguments: Option<unsafe extern "C" fn(self_: *mut CefCommandLine) -> i32>,
    pub get_arguments:
        Option<unsafe extern "C" fn(self_: *mut CefCommandLine, arguments: *mut c_void) -> ()>,
    pub append_argument: Option<
        unsafe extern "C" fn(self_: *mut CefCommandLine, argument: *const CefStringUtf16) -> (),
    >,
    pub prepend_wrapper: Option<
        unsafe extern "C" fn(self_: *mut CefCommandLine, wrapper: *const CefStringUtf16) -> (),
    >,
    pub remove_switch:
        Option<unsafe extern "C" fn(self_: *mut CefCommandLine, name: *const CefStringUtf16) -> ()>,
}

#[repr(C)]
pub struct CefRequestContextHandler {
    pub base: CefBaseRefCounted,
    pub on_request_context_initialized: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequestContextHandler,
            request_context: *mut c_void,
        ) -> (),
    >,
    pub get_resource_request_handler: Option<
        unsafe extern "C" fn(
            self_: *mut CefRequestContextHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            request: *mut CefRequest,
            is_navigation: i32,
            is_download: i32,
            request_initiator: *const CefStringUtf16,
            disable_default_handling: *mut i32,
        ) -> *mut CefResourceRequestHandler,
    >,
}

#[repr(C)]
pub struct CefBrowserProcessHandler {
    pub base: CefBaseRefCounted,
    pub on_register_custom_preferences: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserProcessHandler,
            type_: i32,
            registrar: *mut c_void,
        ) -> (),
    >,
    pub on_context_initialized:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserProcessHandler) -> ()>,
    pub on_before_child_process_launch: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserProcessHandler,
            command_line: *mut CefCommandLine,
        ) -> (),
    >,
    pub on_already_running_app_relaunch: Option<
        unsafe extern "C" fn(
            self_: *mut CefBrowserProcessHandler,
            command_line: *mut CefCommandLine,
            current_directory: *const CefStringUtf16,
        ) -> i32,
    >,
    pub on_schedule_message_pump_work:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserProcessHandler, delay_ms: i64) -> ()>,
    pub get_default_client:
        Option<unsafe extern "C" fn(self_: *mut CefBrowserProcessHandler) -> *mut CefClient>,
    pub get_default_request_context_handler: Option<
        unsafe extern "C" fn(self_: *mut CefBrowserProcessHandler) -> *mut CefRequestContextHandler,
    >,
}

#[repr(C)]
pub struct CefTask {
    pub base: CefBaseRefCounted,
    pub execute: Option<unsafe extern "C" fn(self_: *mut CefTask) -> ()>,
}

#[repr(C)]
pub struct CefTaskRunner {
    pub base: CefBaseRefCounted,
    pub is_same:
        Option<unsafe extern "C" fn(self_: *mut CefTaskRunner, that: *mut CefTaskRunner) -> i32>,
    pub belongs_to_current_thread: Option<unsafe extern "C" fn(self_: *mut CefTaskRunner) -> i32>,
    pub belongs_to_thread:
        Option<unsafe extern "C" fn(self_: *mut CefTaskRunner, threadId: i32) -> i32>,
    pub post_task:
        Option<unsafe extern "C" fn(self_: *mut CefTaskRunner, task: *mut CefTask) -> i32>,
    pub post_delayed_task: Option<
        unsafe extern "C" fn(self_: *mut CefTaskRunner, task: *mut CefTask, delay_ms: i64) -> i32,
    >,
}

#[repr(C)]
pub struct CefV8Context {
    pub base: CefBaseRefCounted,
    pub get_task_runner:
        Option<unsafe extern "C" fn(self_: *mut CefV8Context) -> *mut CefTaskRunner>,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefV8Context) -> i32>,
    pub get_browser: Option<unsafe extern "C" fn(self_: *mut CefV8Context) -> *mut CefBrowser>,
    pub get_frame: Option<unsafe extern "C" fn(self_: *mut CefV8Context) -> *mut CefFrame>,
    pub get_global: Option<unsafe extern "C" fn(self_: *mut CefV8Context) -> *mut CefV8Value>,
    pub enter: Option<unsafe extern "C" fn(self_: *mut CefV8Context) -> i32>,
    pub exit: Option<unsafe extern "C" fn(self_: *mut CefV8Context) -> i32>,
    pub is_same:
        Option<unsafe extern "C" fn(self_: *mut CefV8Context, that: *mut CefV8Context) -> i32>,
    pub eval: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Context,
            code: *const CefStringUtf16,
            script_url: *const CefStringUtf16,
            start_line: i32,
            retval: *mut *mut CefV8Value,
            exception: *mut *mut CefV8Exception,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefV8Handler {
    pub base: CefBaseRefCounted,
    pub execute: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Handler,
            name: *const CefStringUtf16,
            object: *mut CefV8Value,
            argumentsCount: usize,
            arguments: *mut *const CefV8Value,
            retval: *mut *mut CefV8Value,
            exception: *mut CefStringUtf16,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefV8Accessor {
    pub base: CefBaseRefCounted,
    pub get: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Accessor,
            name: *const CefStringUtf16,
            object: *mut CefV8Value,
            retval: *mut *mut CefV8Value,
            exception: *mut CefStringUtf16,
        ) -> i32,
    >,
    pub set: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Accessor,
            name: *const CefStringUtf16,
            object: *mut CefV8Value,
            value: *mut CefV8Value,
            exception: *mut CefStringUtf16,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefV8Interceptor {
    pub base: CefBaseRefCounted,
    pub get_byname: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Interceptor,
            name: *const CefStringUtf16,
            object: *mut CefV8Value,
            retval: *mut *mut CefV8Value,
            exception: *mut CefStringUtf16,
        ) -> i32,
    >,
    pub get_byindex: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Interceptor,
            index: i32,
            object: *mut CefV8Value,
            retval: *mut *mut CefV8Value,
            exception: *mut CefStringUtf16,
        ) -> i32,
    >,
    pub set_byname: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Interceptor,
            name: *const CefStringUtf16,
            object: *mut CefV8Value,
            value: *mut CefV8Value,
            exception: *mut CefStringUtf16,
        ) -> i32,
    >,
    pub set_byindex: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Interceptor,
            index: i32,
            object: *mut CefV8Value,
            value: *mut CefV8Value,
            exception: *mut CefStringUtf16,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefV8Exception {
    pub base: CefBaseRefCounted,
    pub get_message:
        Option<unsafe extern "C" fn(self_: *mut CefV8Exception) -> *mut CefStringUtf16>,
    pub get_source_line:
        Option<unsafe extern "C" fn(self_: *mut CefV8Exception) -> *mut CefStringUtf16>,
    pub get_script_resource_name:
        Option<unsafe extern "C" fn(self_: *mut CefV8Exception) -> *mut CefStringUtf16>,
    pub get_line_number: Option<unsafe extern "C" fn(self_: *mut CefV8Exception) -> i32>,
    pub get_start_position: Option<unsafe extern "C" fn(self_: *mut CefV8Exception) -> i32>,
    pub get_end_position: Option<unsafe extern "C" fn(self_: *mut CefV8Exception) -> i32>,
    pub get_start_column: Option<unsafe extern "C" fn(self_: *mut CefV8Exception) -> i32>,
    pub get_end_column: Option<unsafe extern "C" fn(self_: *mut CefV8Exception) -> i32>,
}

#[repr(C)]
pub struct CefV8ArrayBufferReleaseCallback {
    pub base: CefBaseRefCounted,
    pub release_buffer: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8ArrayBufferReleaseCallback,
            buffer: *mut c_void,
        ) -> (),
    >,
}

#[repr(C)]
pub struct CefV8BackingStore {
    pub base: CefBaseRefCounted,
    pub data: Option<unsafe extern "C" fn(self_: *mut CefV8BackingStore) -> *mut c_void>,
    pub byte_length: Option<unsafe extern "C" fn(self_: *mut CefV8BackingStore) -> usize>,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefV8BackingStore) -> i32>,
}

#[repr(C)]
pub struct CefV8Value {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_undefined: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_null: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_bool: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_int: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_uint: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_double: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_date: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_string: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_object: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_array: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_array_buffer: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_function: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_promise: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub is_same: Option<unsafe extern "C" fn(self_: *mut CefV8Value, that: *mut CefV8Value) -> i32>,
    pub get_bool_value: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub get_int_value: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub get_uint_value: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> u32>,
    pub get_double_value: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> f64>,
    pub get_date_value: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i64>,
    pub get_string_value:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> *mut CefStringUtf16>,
    pub is_user_created: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub has_exception: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub get_exception: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> *mut CefV8Exception>,
    pub clear_exception: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub will_rethrow_exceptions: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub set_rethrow_exceptions:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value, rethrow: i32) -> i32>,
    pub has_value_bykey:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value, key: *const CefStringUtf16) -> i32>,
    pub has_value_byindex: Option<unsafe extern "C" fn(self_: *mut CefV8Value, index: i32) -> i32>,
    pub delete_value_bykey:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value, key: *const CefStringUtf16) -> i32>,
    pub delete_value_byindex:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value, index: i32) -> i32>,
    pub get_value_bykey: Option<
        unsafe extern "C" fn(self_: *mut CefV8Value, key: *const CefStringUtf16) -> *mut CefV8Value,
    >,
    pub get_value_byindex:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value, index: i32) -> *mut CefV8Value>,
    pub set_value_bykey: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Value,
            key: *const CefStringUtf16,
            value: *mut CefV8Value,
            attribute: i32,
        ) -> i32,
    >,
    pub set_value_byindex: Option<
        unsafe extern "C" fn(self_: *mut CefV8Value, index: i32, value: *mut CefV8Value) -> i32,
    >,
    pub set_value_byaccessor: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Value,
            key: *const CefStringUtf16,
            attribute: i32,
        ) -> i32,
    >,
    pub get_keys: Option<unsafe extern "C" fn(self_: *mut CefV8Value, keys: *mut c_void) -> i32>,
    pub set_user_data: Option<
        unsafe extern "C" fn(self_: *mut CefV8Value, user_data: *mut CefBaseRefCounted) -> i32,
    >,
    pub get_user_data:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> *mut CefBaseRefCounted>,
    pub get_externally_allocated_memory:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub adjust_externally_allocated_memory:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value, change_in_bytes: i32) -> i32>,
    pub get_array_length: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub get_array_buffer_release_callback: Option<
        unsafe extern "C" fn(self_: *mut CefV8Value) -> *mut CefV8ArrayBufferReleaseCallback,
    >,
    pub neuter_array_buffer: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> i32>,
    pub get_array_buffer_byte_length: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> usize>,
    pub get_array_buffer_data: Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> *mut c_void>,
    pub get_function_name:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> *mut CefStringUtf16>,
    pub get_function_handler:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value) -> *mut CefV8Handler>,
    pub execute_function: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Value,
            object: *mut CefV8Value,
            argumentsCount: usize,
            arguments: *mut *const CefV8Value,
        ) -> *mut CefV8Value,
    >,
    pub execute_function_with_context: Option<
        unsafe extern "C" fn(
            self_: *mut CefV8Value,
            context: *mut CefV8Context,
            object: *mut CefV8Value,
            argumentsCount: usize,
            arguments: *mut *const CefV8Value,
        ) -> *mut CefV8Value,
    >,
    pub resolve_promise:
        Option<unsafe extern "C" fn(self_: *mut CefV8Value, arg: *mut CefV8Value) -> i32>,
    pub reject_promise: Option<
        unsafe extern "C" fn(self_: *mut CefV8Value, errorMsg: *const CefStringUtf16) -> i32,
    >,
}

#[repr(C)]
pub struct CefV8StackTrace {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefV8StackTrace) -> i32>,
    pub get_frame_count: Option<unsafe extern "C" fn(self_: *mut CefV8StackTrace) -> i32>,
    pub get_frame: Option<
        unsafe extern "C" fn(self_: *mut CefV8StackTrace, index: i32) -> *mut CefV8StackFrame,
    >,
}

#[repr(C)]
pub struct CefV8StackFrame {
    pub base: CefBaseRefCounted,
    pub is_valid: Option<unsafe extern "C" fn(self_: *mut CefV8StackFrame) -> i32>,
    pub get_script_name:
        Option<unsafe extern "C" fn(self_: *mut CefV8StackFrame) -> *mut CefStringUtf16>,
    pub get_script_name_or_source_url:
        Option<unsafe extern "C" fn(self_: *mut CefV8StackFrame) -> *mut CefStringUtf16>,
    pub get_function_name:
        Option<unsafe extern "C" fn(self_: *mut CefV8StackFrame) -> *mut CefStringUtf16>,
    pub get_line_number: Option<unsafe extern "C" fn(self_: *mut CefV8StackFrame) -> i32>,
    pub get_column: Option<unsafe extern "C" fn(self_: *mut CefV8StackFrame) -> i32>,
    pub is_eval: Option<unsafe extern "C" fn(self_: *mut CefV8StackFrame) -> i32>,
    pub is_constructor: Option<unsafe extern "C" fn(self_: *mut CefV8StackFrame) -> i32>,
}

#[repr(C)]
pub struct CefRenderProcessHandler {
    pub base: CefBaseRefCounted,
    pub on_web_kit_initialized:
        Option<unsafe extern "C" fn(self_: *mut CefRenderProcessHandler) -> ()>,
    pub on_browser_created: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderProcessHandler,
            browser: *mut CefBrowser,
            extra_info: *mut CefDictionaryValue,
        ) -> (),
    >,
    pub on_browser_destroyed: Option<
        unsafe extern "C" fn(self_: *mut CefRenderProcessHandler, browser: *mut CefBrowser) -> (),
    >,
    pub get_load_handler:
        Option<unsafe extern "C" fn(self_: *mut CefRenderProcessHandler) -> *mut CefLoadHandler>,
    pub on_context_created: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderProcessHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            context: *mut CefV8Context,
        ) -> (),
    >,
    pub on_context_released: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderProcessHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            context: *mut CefV8Context,
        ) -> (),
    >,
    pub on_uncaught_exception: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderProcessHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            context: *mut CefV8Context,
            exception: *mut CefV8Exception,
            stackTrace: *mut CefV8StackTrace,
        ) -> (),
    >,
    pub on_focused_node_changed: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderProcessHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            node: *mut CefDomnode,
        ) -> (),
    >,
    pub on_process_message_received: Option<
        unsafe extern "C" fn(
            self_: *mut CefRenderProcessHandler,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            source_process: i32,
            message: *mut CefProcessMessage,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefResourceBundleHandler {
    pub base: CefBaseRefCounted,
    pub get_localized_string: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceBundleHandler,
            string_id: i32,
            string: *mut CefStringUtf16,
        ) -> i32,
    >,
    pub get_data_resource: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceBundleHandler,
            resource_id: i32,
            data: *mut *mut c_void,
            data_size: *mut c_void,
        ) -> i32,
    >,
    pub get_data_resource_for_scale: Option<
        unsafe extern "C" fn(
            self_: *mut CefResourceBundleHandler,
            resource_id: i32,
            scale_factor: i32,
            data: *mut *mut c_void,
            data_size: *mut c_void,
        ) -> i32,
    >,
}

#[repr(C)]
pub struct CefSchemeHandlerFactory {
    pub base: CefBaseRefCounted,
    pub create: Option<
        unsafe extern "C" fn(
            self_: *mut CefSchemeHandlerFactory,
            browser: *mut CefBrowser,
            frame: *mut CefFrame,
            scheme_name: *const CefStringUtf16,
            request: *mut CefRequest,
        ) -> *mut CefResourceHandler,
    >,
}

#[repr(C)]
pub struct CefApp {
    pub base: CefBaseRefCounted,
    pub on_before_command_line_processing: Option<
        unsafe extern "C" fn(
            self_: *mut CefApp,
            process_type: *const CefStringUtf16,
            command_line: *mut CefCommandLine,
        ) -> (),
    >,
    pub on_register_custom_schemes:
        Option<unsafe extern "C" fn(self_: *mut CefApp, registrar: *mut c_void) -> ()>,
    pub get_resource_bundle_handler:
        Option<unsafe extern "C" fn(self_: *mut CefApp) -> *mut CefResourceBundleHandler>,
    pub get_browser_process_handler:
        Option<unsafe extern "C" fn(self_: *mut CefApp) -> *mut CefBrowserProcessHandler>,
    pub get_render_process_handler:
        Option<unsafe extern "C" fn(self_: *mut CefApp) -> *mut CefRenderProcessHandler>,
}

const _: () = {
    assert!(std::mem::size_of::<CefDevToolsMessageObserver>() == 80);
    assert!(std::mem::size_of::<CefValue>() == 216);
    assert!(std::mem::size_of::<CefBinaryValue>() == 104);
    assert!(std::mem::size_of::<CefDictionaryValue>() == 272);
    assert!(std::mem::size_of::<CefListValue>() == 264);
    assert!(std::mem::size_of::<CefImage>() == 144);
    assert!(std::mem::size_of::<CefReadHandler>() == 80);
    assert!(std::mem::size_of::<CefStreamReader>() == 80);
    assert!(std::mem::size_of::<CefWriteHandler>() == 80);
    assert!(std::mem::size_of::<CefStreamWriter>() == 80);
    assert!(std::mem::size_of::<CefDragData>() == 256);
    assert!(std::mem::size_of::<CefDomvisitor>() == 48);
    assert!(std::mem::size_of::<CefDomdocument>() == 152);
    assert!(std::mem::size_of::<CefDomnode>() == 248);
    assert!(std::mem::size_of::<CefSharedMemoryRegion>() == 64);
    assert!(std::mem::size_of::<CefProcessMessage>() == 88);
    assert!(std::mem::size_of::<CefRequest>() == 216);
    assert!(std::mem::size_of::<CefPostData>() == 96);
    assert!(std::mem::size_of::<CefPostDataElement>() == 104);
    assert!(std::mem::size_of::<CefStringVisitor>() == 48);
    assert!(std::mem::size_of::<CefFrame>() == 248);
    assert!(std::mem::size_of::<CefX509CertPrincipal>() == 96);
    assert!(std::mem::size_of::<CefX509Certificate>() == 120);
    assert!(std::mem::size_of::<CefSslstatus>() == 80);
    assert!(std::mem::size_of::<CefNavigationEntry>() == 120);
    assert!(std::mem::size_of::<CefCallback>() == 56);
    assert!(std::mem::size_of::<CefCompletionCallback>() == 48);
    assert!(std::mem::size_of::<CefCookieManager>() == 80);
    assert!(std::mem::size_of::<CefCookieVisitor>() == 48);
    assert!(std::mem::size_of::<CefSetCookieCallback>() == 48);
    assert!(std::mem::size_of::<CefDeleteCookiesCallback>() == 48);
    assert!(std::mem::size_of::<CefMediaRouter>() == 80);
    assert!(std::mem::size_of::<CefMediaObserver>() == 72);
    assert!(std::mem::size_of::<CefMediaRoute>() == 80);
    assert!(std::mem::size_of::<CefMediaRouteCreateCallback>() == 48);
    assert!(std::mem::size_of::<CefMediaSink>() == 96);
    assert!(std::mem::size_of::<CefMediaSinkDeviceInfoCallback>() == 48);
    assert!(std::mem::size_of::<CefMediaSource>() == 64);
    assert!(std::mem::size_of::<CefPreferenceObserver>() == 48);
    assert!(std::mem::size_of::<CefPreferenceManager>() == 88);
    assert!(std::mem::size_of::<CefResolveCallback>() == 48);
    assert!(std::mem::size_of::<CefSettingObserver>() == 48);
    assert!(std::mem::size_of::<CefBrowser>() == 208);
    assert!(std::mem::size_of::<CefRunFileDialogCallback>() == 48);
    assert!(std::mem::size_of::<CefNavigationEntryVisitor>() == 48);
    assert!(std::mem::size_of::<CefPdfPrintCallback>() == 48);
    assert!(std::mem::size_of::<CefDownloadImageCallback>() == 48);
    assert!(std::mem::size_of::<CefBrowserHost>() == 584);
    assert!(std::mem::size_of::<CefAudioHandler>() == 80);
    assert!(std::mem::size_of::<CefCommandHandler>() == 80);
    assert!(std::mem::size_of::<CefMenuModelDelegate>() == 96);
    assert!(std::mem::size_of::<CefMenuModel>() == 488);
    assert!(std::mem::size_of::<CefRunContextMenuCallback>() == 56);
    assert!(std::mem::size_of::<CefRunQuickMenuCallback>() == 56);
    assert!(std::mem::size_of::<CefContextMenuHandler>() == 96);
    assert!(std::mem::size_of::<CefContextMenuParams>() == 200);
    assert!(std::mem::size_of::<CefFileDialogCallback>() == 56);
    assert!(std::mem::size_of::<CefDialogHandler>() == 48);
    assert!(std::mem::size_of::<CefDisplayHandler>() == 144);
    assert!(std::mem::size_of::<CefDownloadItem>() == 200);
    assert!(std::mem::size_of::<CefBeforeDownloadCallback>() == 48);
    assert!(std::mem::size_of::<CefDownloadItemCallback>() == 64);
    assert!(std::mem::size_of::<CefDownloadHandler>() == 64);
    assert!(std::mem::size_of::<CefDragHandler>() == 56);
    assert!(std::mem::size_of::<CefFindHandler>() == 48);
    assert!(std::mem::size_of::<CefFocusHandler>() == 64);
    assert!(std::mem::size_of::<CefFrameHandler>() == 80);
    assert!(std::mem::size_of::<CefJsdialogCallback>() == 48);
    assert!(std::mem::size_of::<CefJsdialogHandler>() == 72);
    assert!(std::mem::size_of::<CefKeyboardHandler>() == 56);
    assert!(std::mem::size_of::<CefLifeSpanHandler>() == 88);
    assert!(std::mem::size_of::<CefLoadHandler>() == 72);
    assert!(std::mem::size_of::<CefMediaAccessCallback>() == 56);
    assert!(std::mem::size_of::<CefPermissionPromptCallback>() == 48);
    assert!(std::mem::size_of::<CefPermissionHandler>() == 64);
    assert!(std::mem::size_of::<CefPrintSettings>() == 216);
    assert!(std::mem::size_of::<CefPrintDialogCallback>() == 56);
    assert!(std::mem::size_of::<CefPrintJobCallback>() == 48);
    assert!(std::mem::size_of::<CefPrintHandler>() == 88);
    assert!(std::mem::size_of::<CefAccessibilityHandler>() == 56);
    assert!(std::mem::size_of::<CefRenderHandler>() == 176);
    assert!(std::mem::size_of::<CefAuthCallback>() == 56);
    assert!(std::mem::size_of::<CefResponse>() == 176);
    assert!(std::mem::size_of::<CefResourceSkipCallback>() == 48);
    assert!(std::mem::size_of::<CefResourceReadCallback>() == 48);
    assert!(std::mem::size_of::<CefResourceHandler>() == 96);
    assert!(std::mem::size_of::<CefResponseFilter>() == 56);
    assert!(std::mem::size_of::<CefResourceRequestHandler>() == 104);
    assert!(std::mem::size_of::<CefCookieAccessFilter>() == 56);
    assert!(std::mem::size_of::<CefSslinfo>() == 56);
    assert!(std::mem::size_of::<CefUnresponsiveProcessCallback>() == 56);
    assert!(std::mem::size_of::<CefSelectClientCertificateCallback>() == 48);
    assert!(std::mem::size_of::<CefRequestHandler>() == 128);
    assert!(std::mem::size_of::<CefClient>() == 192);
    assert!(std::mem::size_of::<CefCommandLine>() == 208);
    assert!(std::mem::size_of::<CefRequestContextHandler>() == 56);
    assert!(std::mem::size_of::<CefBrowserProcessHandler>() == 96);
    assert!(std::mem::size_of::<CefTask>() == 48);
    assert!(std::mem::size_of::<CefTaskRunner>() == 80);
    assert!(std::mem::size_of::<CefV8Context>() == 112);
    assert!(std::mem::size_of::<CefV8Handler>() == 48);
    assert!(std::mem::size_of::<CefV8Accessor>() == 56);
    assert!(std::mem::size_of::<CefV8Interceptor>() == 72);
    assert!(std::mem::size_of::<CefV8Exception>() == 104);
    assert!(std::mem::size_of::<CefV8ArrayBufferReleaseCallback>() == 48);
    assert!(std::mem::size_of::<CefV8BackingStore>() == 64);
    assert!(std::mem::size_of::<CefV8Value>() == 456);
    assert!(std::mem::size_of::<CefV8StackTrace>() == 64);
    assert!(std::mem::size_of::<CefV8StackFrame>() == 104);
    assert!(std::mem::size_of::<CefRenderProcessHandler>() == 112);
    assert!(std::mem::size_of::<CefResourceBundleHandler>() == 64);
    assert!(std::mem::size_of::<CefSchemeHandlerFactory>() == 48);
    assert!(std::mem::size_of::<CefApp>() == 80);
};
