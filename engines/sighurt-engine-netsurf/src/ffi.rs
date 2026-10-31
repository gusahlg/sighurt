//! The C API in `csrc/frontend/ns_sighurt.h`. Nothing else in this crate is unsafe.
//!
//! The embedded stylesheets and messages are borrowed by the core until shutdown. They are not
//! freed on the C side.

#![allow(missing_docs)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};

const CSS_DEFAULT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/res/default.css"));
const CSS_QUIRKS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/res/quirks.css"));
const CSS_INTERNAL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/res/internal.css"));
const CSS_ADBLOCK: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/res/adblock.css"));
const MESSAGES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/res/en/Messages"));

#[repr(C)]
#[doc(hidden)]
pub struct NshBytes {
    pub data: *const u8,
    pub len: usize,
}

#[repr(C)]
#[doc(hidden)]
pub struct NshConfig {
    pub user_agent: *const c_char,
    pub ca_bundle: *const c_char,
    pub profile_dir: *const c_char,
    pub font_sans: *const c_char,
    pub font_sans_bold: *const c_char,
    pub font_serif: *const c_char,
    pub font_mono: *const c_char,
    pub css_default: NshBytes,
    pub css_quirks: NshBytes,
    pub css_internal: NshBytes,
    pub css_adblock: NshBytes,
    pub messages: NshBytes,
}

fn bytes(data: &'static [u8]) -> NshBytes {
    NshBytes {
        data: data.as_ptr(),
        len: data.len(),
    }
}

fn optional(text: &Option<CString>) -> *const c_char {
    text.as_ref()
        .map(|s| s.as_ptr())
        .unwrap_or(std::ptr::null())
}

/// What [`init`] is given. Strings are copied by the core where it keeps them.
pub struct Init {
    /// `[engines.netsurf] user-agent`, or none for NetSurf's own.
    pub user_agent: Option<CString>,
    /// A CA bundle path, or none.
    pub ca_bundle: Option<CString>,
    /// Profile directory, or none (no cookie file yet).
    pub profile_dir: Option<CString>,
    pub font_sans: CString,
    pub font_sans_bold: CString,
    pub font_serif: CString,
    pub font_mono: CString,
}

impl Init {
    /// Fonts from `SIGHURT_NETSURF_FONT_*` and the CA bundle from `SSL_CERT_FILE` when set.
    pub fn from_env() -> Self {
        // The environment overrides what the build recorded (the nix store paths of its fonts).
        let font = |var: &str, built: Option<&str>| {
            let value = std::env::var(var)
                .ok()
                .or_else(|| built.map(str::to_string))
                .unwrap_or_else(|| panic!("{var} is not set and the build recorded no font"));
            CString::new(value).unwrap_or_else(|_| panic!("{var} contains a NUL"))
        };
        let optional_env = |var: &str| {
            std::env::var(var)
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| CString::new(v).expect("path contains a NUL"))
        };
        Self {
            user_agent: None,
            ca_bundle: optional_env("SSL_CERT_FILE"),
            profile_dir: None,
            font_sans: font(
                "SIGHURT_NETSURF_FONT_SANS",
                option_env!("SIGHURT_NETSURF_BUILT_FONT_SANS"),
            ),
            font_sans_bold: font(
                "SIGHURT_NETSURF_FONT_SANS_BOLD",
                option_env!("SIGHURT_NETSURF_BUILT_FONT_SANS_BOLD"),
            ),
            font_serif: font(
                "SIGHURT_NETSURF_FONT_SERIF",
                option_env!("SIGHURT_NETSURF_BUILT_FONT_SERIF"),
            ),
            font_mono: font(
                "SIGHURT_NETSURF_FONT_MONO",
                option_env!("SIGHURT_NETSURF_BUILT_FONT_MONO"),
            ),
        }
    }
}

/// Starts NetSurf. 0 on success. Non-zero if this process already started it, or init failed.
pub fn init(config: &Init) -> i32 {
    let c = NshConfig {
        user_agent: optional(&config.user_agent),
        ca_bundle: optional(&config.ca_bundle),
        profile_dir: optional(&config.profile_dir),
        font_sans: config.font_sans.as_ptr(),
        font_sans_bold: config.font_sans_bold.as_ptr(),
        font_serif: config.font_serif.as_ptr(),
        font_mono: config.font_mono.as_ptr(),
        css_default: bytes(CSS_DEFAULT),
        css_quirks: bytes(CSS_QUIRKS),
        css_internal: bytes(CSS_INTERNAL),
        css_adblock: bytes(CSS_ADBLOCK),
        messages: bytes(MESSAGES),
    };
    unsafe { raw::nsh_init(&c) }
}

/// [`init`] with [`Init::from_env`].
pub fn init_default() -> i32 {
    init(&Init::from_env())
}

/// Shuts NetSurf down. A second call, or a call before a successful init, does nothing.
pub fn shutdown() {
    unsafe { raw::nsh_shutdown() }
}

/// 0 not started, 1 running, 2 finished.
pub fn state() -> i32 {
    unsafe { raw::nsh_state() }
}

/// The C string, if the event carried one. Valid until the next event call.
pub fn event_text(text: *const c_char, len: usize) -> Option<String> {
    if text.is_null() || len == 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), len) };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

pub use raw::*;

#[allow(non_camel_case_types)]
mod raw {
    use std::os::raw::{c_char, c_int, c_uint};

    use super::NshConfig;

    pub type View = ffi_view;

    #[repr(C)]
    pub struct ffi_view {
        _private: [u8; 0],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Rect {
        pub x0: c_int,
        pub y0: c_int,
        pub x1: c_int,
        pub y1: c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Event {
        pub kind: u32,
        pub view_id: u64,
        pub i0: i32,
        pub i1: i32,
        pub i2: i32,
        pub i3: i32,
        pub u0: u64,
        pub text: *const c_char,
        pub text_len: usize,
    }

    pub const FD_MAX: usize = 64;

    #[repr(C)]
    pub struct PollInfo {
        pub timeout_ms: c_int,
        pub maxfd: c_int,
        pub nread: c_int,
        pub nwrite: c_int,
        pub nexcept: c_int,
        pub read_fds: [c_int; FD_MAX],
        pub write_fds: [c_int; FD_MAX],
        pub except_fds: [c_int; FD_MAX],
    }

    unsafe extern "C" {
        pub fn nsh_init(cfg: *const NshConfig) -> c_int;
        pub fn nsh_shutdown();
        pub fn nsh_state() -> c_int;

        pub fn nsh_view_open(view_id: u64, w: c_int, h: c_int, scale_120: c_int) -> *mut View;
        pub fn nsh_view_close(view: *mut View);
        pub fn nsh_view_navigate(view: *mut View, url: *const c_char) -> c_int;
        pub fn nsh_view_follow(view: *mut View, url: *const c_char) -> c_int;
        pub fn nsh_events_overflowed() -> c_int;
        pub fn nsh_view_reload(view: *mut View);
        pub fn nsh_view_stop(view: *mut View);
        pub fn nsh_view_back(view: *mut View);
        pub fn nsh_view_forward(view: *mut View);
        pub fn nsh_view_back_available(view: *const View) -> c_int;
        pub fn nsh_view_forward_available(view: *const View) -> c_int;
        pub fn nsh_view_resize(view: *mut View, w: c_int, h: c_int, scale_120: c_int);
        pub fn nsh_view_set_scale(view: *mut View, zoom: f64);
        pub fn nsh_view_set_hidden(view: *mut View, hidden: c_int);
        pub fn nsh_view_hidden(view: *const View) -> c_int;
        pub fn nsh_view_width(view: *const View) -> c_int;
        pub fn nsh_view_height(view: *const View) -> c_int;
        pub fn nsh_view_scale_120(view: *const View) -> c_int;
        pub fn nsh_view_mouse(view: *mut View, x: c_int, y: c_int, state: c_uint, mods: c_uint);
        pub fn nsh_view_scroll(view: *mut View, x: c_int, y: c_int, dx: c_int, dy: c_int);
        pub fn nsh_view_key(view: *mut View, key: u32) -> c_int;
        pub fn nsh_view_link_at(
            view: *mut View,
            x: c_int,
            y: c_int,
            buf: *mut c_char,
            buflen: usize,
        ) -> c_int;
        pub fn nsh_view_form_at(view: *mut View, x: c_int, y: c_int) -> c_int;
        pub fn nsh_view_selection(view: *mut View, buf: *mut c_char, buflen: usize) -> c_int;
        pub fn nsh_view_paint(
            view: *mut View,
            bgra: *mut u32,
            w: c_int,
            h: c_int,
            stride: usize,
            clip: *const Rect,
        ) -> c_int;
        pub fn nsh_view_dirty_rect(view: *const View, out: *mut Rect) -> c_int;
        pub fn nsh_poll(info: *mut PollInfo) -> c_int;
        pub fn nsh_next_event(out: *mut Event) -> usize;
        pub fn nsh_clipboard_set_shadow(utf8: *const c_char, len: usize);
        pub fn nsh_download_answer(id: u64, accept: c_int);
        pub fn nsh_file_answer(view_id: u64, path: *const c_char);
        pub fn nsh_download_info(
            id: u64,
            url: *mut c_char,
            url_len: usize,
            name: *mut c_char,
            name_len: usize,
            mime: *mut c_char,
            mime_len: usize,
            data: *mut *const u8,
            len: *mut usize,
        ) -> c_int;
    }

    pub const EV_TITLE: u32 = 1;
    pub const EV_URL: u32 = 2;
    pub const EV_STATUS: u32 = 3;
    pub const EV_POINTER: u32 = 4;
    pub const EV_LOADING: u32 = 5;
    pub const EV_FRAME: u32 = 6;
    pub const EV_CARET: u32 = 7;
    pub const EV_REMOVE_CARET: u32 = 8;
    pub const EV_NEW_CONTENT: u32 = 9;
    pub const EV_LAUNCH: u32 = 10;
    pub const EV_CLIPBOARD: u32 = 11;
    pub const EV_DOWNLOAD: u32 = 12;
    pub const EV_OPEN: u32 = 13;
    pub const EV_FILE: u32 = 14;
    pub const EV_CONSOLE: u32 = 15;
    pub const EV_FAILED: u32 = 16;
    pub const EV_ASK: u32 = 17;

    pub const MOUSE_HOVER: c_uint = 0;
    pub const MOUSE_PRESS_1: c_uint = 1 << 0;
    pub const MOUSE_PRESS_2: c_uint = 1 << 1;
    pub const MOUSE_CLICK_1: c_uint = 1 << 2;
    pub const MOUSE_CLICK_2: c_uint = 1 << 3;
    pub const MOUSE_DOUBLE_CLICK: c_uint = 1 << 4;
    pub const MOUSE_TRIPLE_CLICK: c_uint = 1 << 5;
    pub const MOUSE_DRAG_1: c_uint = 1 << 6;
    pub const MOUSE_DRAG_2: c_uint = 1 << 7;
    pub const MOUSE_DRAG_ON: c_uint = 1 << 8;
    pub const MOUSE_HOLDING_1: c_uint = 1 << 9;
    pub const MOUSE_HOLDING_2: c_uint = 1 << 10;
    pub const MOUSE_MOD_1: c_uint = 1 << 11;
    pub const MOUSE_MOD_2: c_uint = 1 << 12;
    pub const MOUSE_MOD_3: c_uint = 1 << 13;
    pub const MOUSE_LEAVE: c_uint = 1 << 14;

    pub const KEY_SELECT_ALL: u32 = 1;
    pub const KEY_COPY_SELECTION: u32 = 3;
    pub const KEY_DELETE_LEFT: u32 = 8;
    pub const KEY_TAB: u32 = 9;
    pub const KEY_NL: u32 = 10;
    pub const KEY_SHIFT_TAB: u32 = 11;
    pub const KEY_CR: u32 = 13;
    pub const KEY_DELETE_LINE: u32 = 21;
    pub const KEY_PASTE: u32 = 22;
    pub const KEY_CUT_SELECTION: u32 = 24;
    pub const KEY_CLEAR_SELECTION: u32 = 26;
    pub const KEY_ESCAPE: u32 = 27;
    pub const KEY_LEFT: u32 = 28;
    pub const KEY_RIGHT: u32 = 29;
    pub const KEY_UP: u32 = 30;
    pub const KEY_DOWN: u32 = 31;
    pub const KEY_DELETE_RIGHT: u32 = 127;
    pub const KEY_LINE_START: u32 = 128;
    pub const KEY_LINE_END: u32 = 129;
    pub const KEY_TEXT_START: u32 = 130;
    pub const KEY_TEXT_END: u32 = 131;
    pub const KEY_WORD_LEFT: u32 = 132;
    pub const KEY_DELETE_WORD_LEFT: u32 = 133;
    pub const KEY_WORD_RIGHT: u32 = 134;
    pub const KEY_DELETE_WORD_RIGHT: u32 = 135;
    pub const KEY_PAGE_UP: u32 = 136;
    pub const KEY_PAGE_DOWN: u32 = 137;
    pub const KEY_DELETE_LINE_END: u32 = 138;
    pub const KEY_DELETE_LINE_START: u32 = 139;
    pub const KEY_UNDO: u32 = 140;
    pub const KEY_REDO: u32 = 141;
}

/// One NetSurf window. Not `Send`: every call is on the owner thread, after [`init`] and
/// before [`shutdown`] (which closes any view still open: do not use a handle after it).
pub struct Handle(std::ptr::NonNull<raw::View>);

impl Handle {
    /// Opens a window of `w` x `h` buffer pixels at `scale_120`.
    pub fn open(view_id: u64, w: u32, h: u32, scale_120: u32) -> Option<Self> {
        let clamp = |n: u32| n.clamp(1, i32::MAX as u32) as c_int;
        // SAFETY: the C side validates its state; a null return is a failure.
        let ptr = unsafe { raw::nsh_view_open(view_id, clamp(w), clamp(h), clamp(scale_120)) };
        std::ptr::NonNull::new(ptr).map(Self)
    }

    /// Closes the window. The handle is gone.
    pub fn close(self) {
        // SAFETY: the handle is live and is consumed here.
        unsafe { raw::nsh_view_close(self.0.as_ptr()) }
    }

    /// Starts loading `url`. False if the core refused it.
    pub fn navigate(&self, url: &str) -> bool {
        let Ok(url) = CString::new(url) else {
            return false;
        };
        // SAFETY: live handle, a NUL-terminated string for the call.
        unsafe { raw::nsh_view_navigate(self.0.as_ptr(), url.as_ptr()) != 0 }
    }

    /// Navigates to `url` as the page's own navigation (the current document is the referrer).
    pub fn follow(&self, url: &str) -> bool {
        let Ok(url) = CString::new(url) else {
            return false;
        };
        // SAFETY: live handle, a NUL-terminated string for the call.
        unsafe { raw::nsh_view_follow(self.0.as_ptr(), url.as_ptr()) != 0 }
    }

    /// Reloads.
    pub fn reload(&self) {
        // SAFETY: live handle.
        unsafe { raw::nsh_view_reload(self.0.as_ptr()) }
    }

    /// Stops loading.
    pub fn stop(&self) {
        // SAFETY: live handle.
        unsafe { raw::nsh_view_stop(self.0.as_ptr()) }
    }

    /// Resizes to `w` x `h` buffer pixels at `scale_120`.
    pub fn resize(&self, w: u32, h: u32, scale_120: u32) {
        // SAFETY: live handle.
        unsafe { raw::nsh_view_resize(self.0.as_ptr(), w as c_int, h as c_int, scale_120 as c_int) }
    }

    /// Page zoom, 1.0 for 100 %.
    pub fn set_zoom(&self, zoom: f64) {
        // SAFETY: live handle.
        unsafe { raw::nsh_view_set_scale(self.0.as_ptr(), zoom) }
    }

    /// Hides or shows: a hidden window paints nothing.
    pub fn set_hidden(&self, hidden: bool) {
        // SAFETY: live handle.
        unsafe { raw::nsh_view_set_hidden(self.0.as_ptr(), c_int::from(hidden)) }
    }

    /// Mouse state at a buffer position.
    pub fn mouse(&self, x: i32, y: i32, state: u32, mods: u32) {
        // SAFETY: live handle.
        unsafe { raw::nsh_view_mouse(self.0.as_ptr(), x, y, state, mods) }
    }

    /// Scrolls the deepest scrollable thing at a buffer position, in pixels.
    pub fn scroll(&self, x: i32, y: i32, dx: i32, dy: i32) {
        // SAFETY: live handle.
        unsafe { raw::nsh_view_scroll(self.0.as_ptr(), x, y, dx, dy) }
    }

    /// Sends a key (an `KEY_*` value or a Unicode scalar). True if the core consumed it.
    pub fn key(&self, key: u32) -> bool {
        // SAFETY: live handle.
        unsafe { raw::nsh_view_key(self.0.as_ptr(), key) != 0 }
    }

    /// The link under a buffer position, if any.
    pub fn link_at(&self, x: i32, y: i32) -> Option<String> {
        let mut buf = vec![0u8; 4096];
        // SAFETY: live handle; the C side writes at most `buf.len()` bytes with a NUL.
        let found = unsafe {
            raw::nsh_view_link_at(self.0.as_ptr(), x, y, buf.as_mut_ptr().cast(), buf.len())
        };
        if found == 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).into_owned())
    }

    /// Paints the whole view into `pixels` (ARGB8888, premultiplied: opaque), `stride` bytes per
    /// row. Errs if the memory is not four-byte aligned or too small.
    pub fn paint(&self, pixels: &mut [u8], w: u32, h: u32, stride: usize) -> Result<bool, String> {
        if !(pixels.as_ptr() as usize).is_multiple_of(4) || !stride.is_multiple_of(4) {
            return Err("the frame is not aligned to four bytes".into());
        }
        if stride < w as usize * 4 || pixels.len() < stride * h as usize {
            return Err("the frame is smaller than its size".into());
        }
        // SAFETY: live handle; the buffer holds `h` rows of `stride` bytes, aligned for u32, and
        // the core writes only inside `w` x `h`.
        let drew = unsafe {
            raw::nsh_view_paint(
                self.0.as_ptr(),
                pixels.as_mut_ptr().cast(),
                w as c_int,
                h as c_int,
                stride,
                std::ptr::null(),
            )
        };
        Ok(drew != 0)
    }
}

/// What the core wants from the owner loop after [`poll`].
pub struct Poll {
    /// Milliseconds to the next page-timed callback: `None` nothing, `Some(0)` work is due.
    pub timeout_ms: Option<u32>,
    /// Descriptors to wait on, as `(fd, poll events)`.
    pub fds: Vec<(i32, i16)>,
}

/// Runs due callbacks and polls the fetchers.
pub fn poll() -> Poll {
    let mut info = std::mem::MaybeUninit::<raw::PollInfo>::zeroed();
    // SAFETY: `info` is a writable PollInfo; zero is a valid value of it, and the C side fills it.
    let info = unsafe {
        raw::nsh_poll(info.as_mut_ptr());
        info.assume_init()
    };
    let mut fds: Vec<(i32, i16)> = Vec::new();
    let mut add = |list: &[c_int], count: c_int, events: i16| {
        for &fd in list.iter().take(count.max(0) as usize) {
            match fds.iter_mut().find(|(have, _)| *have == fd) {
                Some(entry) => entry.1 |= events,
                None => fds.push((fd, events)),
            }
        }
    };
    add(&info.read_fds, info.nread, libc::POLLIN);
    add(&info.write_fds, info.nwrite, libc::POLLOUT);
    add(&info.except_fds, info.nexcept, libc::POLLPRI);
    Poll {
        timeout_ms: u32::try_from(info.timeout_ms).ok(),
        fds,
    }
}

/// One event from the core, with its text copied.
pub struct OwnedEvent {
    pub kind: u32,
    pub view_id: u64,
    pub i0: i32,
    pub i1: i32,
    pub i2: i32,
    pub i3: i32,
    pub u0: u64,
    pub text: Option<String>,
}

/// The next queued event, if any.
pub fn next_event() -> Option<OwnedEvent> {
    let mut event = Event {
        kind: 0,
        view_id: 0,
        i0: 0,
        i1: 0,
        i2: 0,
        i3: 0,
        u0: 0,
        text: std::ptr::null(),
        text_len: 0,
    };
    // SAFETY: a writable Event; the text pointer it receives is valid until the next call, and
    // is copied here.
    if unsafe { raw::nsh_next_event(&mut event) } == 0 {
        return None;
    }
    Some(OwnedEvent {
        kind: event.kind,
        view_id: event.view_id,
        i0: event.i0,
        i1: event.i1,
        i2: event.i2,
        i3: event.i3,
        u0: event.u0,
        text: event_text(event.text, event.text_len),
    })
}

/// Whether the core's event queue overflowed (65536 waiting) and lost events.
pub fn events_overflowed() -> bool {
    // SAFETY: reads a flag.
    unsafe { raw::nsh_events_overflowed() != 0 }
}

/// Releases a download (aborts it if it runs, frees what it holds).
pub fn release_download(id: u64) {
    // SAFETY: the C side ignores an id it does not know.
    unsafe { raw::nsh_download_answer(id, 0) }
}

/// Replaces the clipboard text `NS_KEY_PASTE` reads.
pub fn set_clipboard_shadow(text: &str) {
    // SAFETY: a pointer and length of a live string; the C side copies.
    unsafe { raw::nsh_clipboard_set_shadow(text.as_ptr().cast(), text.len()) }
}

/// A finished download's buffered bytes, name and type, and then releases them.
pub fn take_download(id: u64) -> Option<(String, Option<String>, Vec<u8>)> {
    let (mut url, mut name, mut mime) = ([0u8; 2048], [0u8; 512], [0u8; 128]);
    let mut data: *const u8 = std::ptr::null();
    let mut len = 0usize;
    // SAFETY: writable buffers with their lengths; `data` and `len` are filled by the C side and
    // the bytes are copied before the release below.
    let found = unsafe {
        raw::nsh_download_info(
            id,
            url.as_mut_ptr().cast(),
            url.len(),
            name.as_mut_ptr().cast(),
            name.len(),
            mime.as_mut_ptr().cast(),
            mime.len(),
            &mut data,
            &mut len,
        )
    };
    if found == 0 {
        return None;
    }
    let bytes = if data.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: the C side owns `len` bytes at `data` until the release below.
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    // SAFETY: releases the buffered body.
    unsafe { raw::nsh_download_answer(id, 0) };
    let text = |b: &[u8]| {
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).into_owned()
    };
    let name = text(&name);
    Some((text(&url), (!name.is_empty()).then_some(name), bytes))
}

/// Answers a file input: `None` cancels.
pub fn answer_file(view_id: u64, path: Option<&str>) {
    let path = path.and_then(|p| CString::new(p).ok());
    // SAFETY: a NUL-terminated string or null.
    unsafe {
        raw::nsh_file_answer(
            view_id,
            path.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
        )
    }
}

/// A borrowed C string for a URL or path passed into the core for one call.
pub fn c_str(text: &str) -> CString {
    CString::new(text).expect("text contains a NUL")
}
