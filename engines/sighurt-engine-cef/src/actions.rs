//! What the owner asks of a live browser: one enum, performed against its handle. Called from
//! the owner thread; CEF forwards each call to its UI thread itself.
//!
//! Must not own: state, or the decision to do any of these.

use crate::capi::{CefBrowser, CefBrowserHost, CefKeyEvent, CefMouseEvent, CefRange};
use crate::ffi::{call, CefStr, Ref};

/// `KEYEVENT_*` of `cef_key_event_type_t`.
pub const KEY_RAW_DOWN: i32 = 0;
/// A key release.
pub const KEY_UP: i32 = 2;
/// A character typed.
pub const KEY_CHAR: i32 = 3;

/// An operation on one browser.
#[allow(missing_docs)] // The fields are what the variant's documentation says.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Load an address.
    Load(String),
    /// Reload.
    Reload,
    /// Stop loading.
    Stop,
    /// Go back (negative) or forward (positive) that many entries.
    History(i32),
    /// The size or scale changed: re-ask `GetViewRect` and `GetScreenInfo`.
    Resize,
    /// Hide or show.
    Hidden(bool),
    /// Give or take keyboard focus.
    Focus(bool),
    /// Page zoom, as a factor.
    Zoom(f64),
    /// Pointer motion; `leave` when it left the view.
    MouseMove {
        x: i32,
        y: i32,
        modifiers: u32,
        leave: bool,
    },
    /// A button press or release.
    MouseClick {
        x: i32,
        y: i32,
        modifiers: u32,
        button: i32,
        up: bool,
        count: i32,
    },
    /// A scroll, in CEF's convention (positive is up and left).
    Wheel {
        x: i32,
        y: i32,
        modifiers: u32,
        dx: i32,
        dy: i32,
    },
    /// A key event.
    Key {
        kind: i32,
        modifiers: u32,
        virtual_key: i32,
        native_key: i32,
        character: u16,
    },
    /// Insert committed text at the caret.
    ImeCommit(String),
    /// Show composition text.
    ImeSet(String),
    /// Drop the composition.
    ImeCancel,
    /// Close the browser (forced: its page is not asked).
    Close,
}

impl Action {
    /// Does it.
    pub fn perform(&self, browser: &Ref<CefBrowser>, host: &Ref<CefBrowserHost>) {
        let (b, h) = (browser.as_ptr(), host.as_ptr());
        // SAFETY: both pointers are live references we hold; every slot called is filled in a
        // live CEF object.
        unsafe {
            match self {
                Self::Load(url) => {
                    if let Some(frame) = Ref::adopt(call!(b, get_main_frame)) {
                        let url = CefStr::new(url);
                        call!(frame.as_ptr(), load_url, &url.raw());
                    }
                }
                Self::Reload => call!(b, reload),
                Self::Stop => call!(b, stop_load),
                Self::History(steps) => {
                    for _ in 0..steps.unsigned_abs().min(64) {
                        if *steps < 0 {
                            call!(b, go_back);
                        } else {
                            call!(b, go_forward);
                        }
                    }
                }
                Self::Resize => {
                    call!(h, notify_screen_info_changed);
                    call!(h, was_resized);
                }
                Self::Hidden(hidden) => {
                    call!(h, was_hidden, i32::from(*hidden));
                    if !hidden {
                        call!(h, invalidate, 0);
                    }
                }
                Self::Focus(focus) => call!(h, set_focus, i32::from(*focus)),
                Self::Zoom(factor) => {
                    // CEF's zoom level is logarithmic: 1.2 ^ level.
                    call!(h, set_zoom_level, factor.max(0.01).ln() / 1.2f64.ln());
                }
                Self::MouseMove {
                    x,
                    y,
                    modifiers,
                    leave,
                } => {
                    let event = mouse(*x, *y, *modifiers);
                    call!(h, send_mouse_move_event, &event, i32::from(*leave));
                }
                Self::MouseClick {
                    x,
                    y,
                    modifiers,
                    button,
                    up,
                    count,
                } => {
                    let event = mouse(*x, *y, *modifiers);
                    call!(
                        h,
                        send_mouse_click_event,
                        &event,
                        *button,
                        i32::from(*up),
                        *count
                    );
                }
                Self::Wheel {
                    x,
                    y,
                    modifiers,
                    dx,
                    dy,
                } => {
                    let event = mouse(*x, *y, *modifiers);
                    call!(h, send_mouse_wheel_event, &event, *dx, *dy);
                }
                Self::Key {
                    kind,
                    modifiers,
                    virtual_key,
                    native_key,
                    character,
                } => {
                    let event = CefKeyEvent {
                        size: std::mem::size_of::<CefKeyEvent>(),
                        type_: *kind,
                        modifiers: *modifiers,
                        windows_key_code: *virtual_key,
                        native_key_code: *native_key,
                        is_system_key: 0,
                        character: *character,
                        unmodified_character: *character,
                        focus_on_editable_field: 0,
                        _pad: 0,
                    };
                    call!(h, send_key_event, &event);
                }
                Self::ImeCommit(text) => {
                    let text = CefStr::new(text);
                    let invalid = CefRange { from: -1, to: -1 };
                    call!(h, ime_commit_text, &text.raw(), &invalid, 0);
                }
                Self::ImeSet(text) => {
                    let len = i32::try_from(text.encode_utf16().count()).unwrap_or(i32::MAX);
                    let text = CefStr::new(text);
                    let invalid = CefRange { from: -1, to: -1 };
                    let selection = CefRange { from: len, to: len };
                    call!(
                        h,
                        ime_set_composition,
                        &text.raw(),
                        0,
                        std::ptr::null(),
                        &invalid,
                        &selection
                    );
                }
                Self::ImeCancel => call!(h, ime_cancel_composition),
                Self::Close => call!(h, close_browser, 1),
            }
        }
    }
}

/// Sends one typed character to the page, as `KEYEVENT_CHAR`.
///
/// # Safety
/// `host` is a live browser host.
pub unsafe fn send_char(host: *mut CefBrowserHost, unit: u16) {
    let event = CefKeyEvent {
        size: std::mem::size_of::<CefKeyEvent>(),
        type_: KEY_CHAR,
        modifiers: 0,
        windows_key_code: i32::from(unit),
        native_key_code: 0,
        is_system_key: 0,
        character: unit,
        unmodified_character: unit,
        focus_on_editable_field: 0,
        _pad: 0,
    };
    // SAFETY: by the contract above.
    unsafe { call!(host, send_key_event, &event) };
}

fn mouse(x: i32, y: i32, modifiers: u32) -> CefMouseEvent {
    CefMouseEvent { x, y, modifiers }
}
