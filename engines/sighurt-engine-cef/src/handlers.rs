//! The CEF handler objects of one view: the client and the render, life-span, load, display,
//! request, download, dialog, permission and menu handlers.
//!
//! Every callback runs on CEF's UI thread. It does the least that tells the owner what happened:
//! it updates the view's shared state, queues events and wakes the owner loop. It never calls a
//! host service and never blocks. Reference-counted arguments CEF passes to a callback are
//! owned references (CEF's C API contract), so each is wrapped in a [`Ref`] and released when
//! the callback ends; a handler CEF asks for is returned with a reference added.
//!
//! Anything the engine does not support is refused: dialogs are suppressed, permissions denied,
//! menus emptied, authentication and certificate errors cancelled, popups handed to the browser
//! as `OpenRequested`.
//!
//! Must not own: policy beyond refusing, or any state of its own.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use sighurt_api::events::{EngineEvent, OpenDisposition};
use sighurt_api::geometry::SurfaceRect;
use sighurt_api::ids::RequestId;
use sighurt_api::input::{TextInputState, TextPurpose};
use sighurt_api::services::{DownloadSource, Service, ServiceRequest};

use crate::capi::*;
use crate::ffi::{self, add_ref_raw, call, new_object, read, release_raw, take, zeroed, Ref};
use crate::keys;
use crate::nav::Browse;
use crate::shared::{lock, ViewShared, MAX_SELECTION};

type View = Arc<ViewShared>;

/// Runs a callback body, turning a panic into the default answer: unwinding into CEF's C++
/// would abort the browser.
fn guard<R: Default>(body: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_default()
}

/// The handlers of one view, held by its client; their references are dropped with it.
pub struct ClientData {
    view: View,
    render: *mut CefRenderHandler,
    life: *mut CefLifeSpanHandler,
    load: *mut CefLoadHandler,
    display: *mut CefDisplayHandler,
    request: *mut CefRequestHandler,
    download: *mut CefDownloadHandler,
    jsdialog: *mut CefJsdialogHandler,
    permission: *mut CefPermissionHandler,
    dialog: *mut CefDialogHandler,
    menu: *mut CefContextMenuHandler,
}

impl Drop for ClientData {
    fn drop(&mut self) {
        // SAFETY: each pointer is a handler we made and hold one reference to.
        unsafe {
            release_raw(self.render);
            release_raw(self.life);
            release_raw(self.load);
            release_raw(self.display);
            release_raw(self.request);
            release_raw(self.download);
            release_raw(self.jsdialog);
            release_raw(self.permission);
            release_raw(self.dialog);
            release_raw(self.menu);
        }
    }
}

/// Makes the client of `view` with all its handlers, one reference owned by the caller.
pub fn make_client(view: &View) -> *mut CefClient {
    let v = || Arc::clone(view);
    let mut render: CefRenderHandler = zeroed();
    render.get_view_rect = Some(get_view_rect);
    render.get_screen_info = Some(get_screen_info);
    render.on_paint = Some(on_paint);
    render.on_text_selection_changed = Some(on_text_selection_changed);
    render.on_ime_composition_range_changed = Some(on_ime_composition_range_changed);
    let mut life: CefLifeSpanHandler = zeroed();
    life.on_before_popup = Some(on_before_popup);
    life.on_after_created = Some(on_after_created);
    life.do_close = Some(do_close);
    life.on_before_close = Some(on_before_close);
    let mut load: CefLoadHandler = zeroed();
    load.on_loading_state_change = Some(on_loading_state_change);
    load.on_load_start = Some(on_load_start);
    load.on_load_error = Some(on_load_error);
    let mut display: CefDisplayHandler = zeroed();
    display.on_address_change = Some(on_address_change);
    display.on_title_change = Some(on_title_change);
    display.on_status_message = Some(on_status_message);
    display.on_cursor_change = Some(on_cursor_change);
    let mut request: CefRequestHandler = zeroed();
    request.on_before_browse = Some(on_before_browse);
    request.on_render_process_terminated = Some(on_render_process_terminated);
    let mut download: CefDownloadHandler = zeroed();
    download.on_before_download = Some(on_before_download);
    let mut jsdialog: CefJsdialogHandler = zeroed();
    jsdialog.on_jsdialog = Some(on_jsdialog);
    jsdialog.on_before_unload_dialog = Some(on_before_unload_dialog);
    let mut permission: CefPermissionHandler = zeroed();
    permission.on_request_media_access_permission = Some(on_request_media_access_permission);
    permission.on_show_permission_prompt = Some(on_show_permission_prompt);
    let mut dialog: CefDialogHandler = zeroed();
    dialog.on_file_dialog = Some(on_file_dialog);
    let mut menu: CefContextMenuHandler = zeroed();
    menu.on_before_context_menu = Some(on_before_context_menu);

    let data = ClientData {
        view: v(),
        render: new_object(render, v()),
        life: new_object(life, v()),
        load: new_object(load, v()),
        display: new_object(display, v()),
        request: new_object(request, v()),
        download: new_object(download, v()),
        jsdialog: new_object(jsdialog, v()),
        permission: new_object(permission, v()),
        dialog: new_object(dialog, v()),
        menu: new_object(menu, v()),
    };
    let mut client: CefClient = zeroed();
    client.get_render_handler = Some(get_render_handler);
    client.get_life_span_handler = Some(get_life_span_handler);
    client.get_load_handler = Some(get_load_handler);
    client.get_display_handler = Some(get_display_handler);
    client.get_request_handler = Some(get_request_handler);
    client.get_download_handler = Some(get_download_handler);
    client.get_jsdialog_handler = Some(get_jsdialog_handler);
    client.get_permission_handler = Some(get_permission_handler);
    client.get_dialog_handler = Some(get_dialog_handler);
    client.get_context_menu_handler = Some(get_context_menu_handler);
    client.on_process_message_received = Some(on_process_message_received);
    new_object(client, data)
}

macro_rules! getters {
    ($($name:ident, $field:ident, $handler:ident;)*) => {$(
        unsafe extern "C" fn $name(this: *mut CefClient) -> *mut $handler {
            // SAFETY: `this` is a client made by `make_client`; CEF takes ownership of the
            // reference we add.
            unsafe {
                let handler = ffi::data::<CefClient, ClientData>(this).$field;
                add_ref_raw(handler);
                handler
            }
        }
    )*};
}
getters! {
    get_render_handler, render, CefRenderHandler;
    get_life_span_handler, life, CefLifeSpanHandler;
    get_load_handler, load, CefLoadHandler;
    get_display_handler, display, CefDisplayHandler;
    get_request_handler, request, CefRequestHandler;
    get_download_handler, download, CefDownloadHandler;
    get_jsdialog_handler, jsdialog, CefJsdialogHandler;
    get_permission_handler, permission, CefPermissionHandler;
    get_dialog_handler, dialog, CefDialogHandler;
    get_context_menu_handler, menu, CefContextMenuHandler;
}

/// The view of a handler object.
///
/// # Safety
/// `p` is a handler made by `make_client`.
unsafe fn view<'a, V>(p: *mut V) -> &'a View {
    // SAFETY: by the contract above.
    unsafe { ffi::data::<V, View>(p) }
}

/// Takes ownership of a reference argument so it is released at the end of the callback.
macro_rules! own {
    ($($p:expr),*) => {$(
        // CEF passes an owned reference (or null) for each of these.
        let _ = Ref::from_owned($p);
    )*};
}

// ---- render ----

unsafe extern "C" fn get_view_rect(
    this: *mut CefRenderHandler,
    browser: *mut CefBrowser,
    rect: *mut CefRect,
) {
    own!(browser);
    guard(|| {
        // SAFETY: `this` is ours, `rect` is CEF's out-parameter.
        let geometry = unsafe { *lock(&view(this).geometry) };
        unsafe {
            *rect = CefRect {
                x: 0,
                y: 0,
                width: i32::try_from(geometry.width).unwrap_or(i32::MAX).max(1),
                height: i32::try_from(geometry.height).unwrap_or(i32::MAX).max(1),
            };
        }
    });
}

unsafe extern "C" fn get_screen_info(
    this: *mut CefRenderHandler,
    browser: *mut CefBrowser,
    info: *mut CefScreenInfo,
) -> i32 {
    own!(browser);
    guard(|| {
        // SAFETY: as above.
        unsafe {
            let geometry = *lock(&view(this).geometry);
            let rect = CefRect {
                x: 0,
                y: 0,
                width: i32::try_from(geometry.width).unwrap_or(i32::MAX).max(1),
                height: i32::try_from(geometry.height).unwrap_or(i32::MAX).max(1),
            };
            let info = &mut *info;
            info.device_scale_factor = geometry.scale;
            info.depth = 24;
            info.depth_per_component = 8;
            info.is_monochrome = 0;
            info.rect = rect;
            info.available_rect = rect;
        }
        1
    })
}

unsafe extern "C" fn on_paint(
    this: *mut CefRenderHandler,
    browser: *mut CefBrowser,
    kind: i32,
    count: usize,
    rects: *const CefRect,
    buffer: *const c_void,
    width: i32,
    height: i32,
) {
    own!(browser);
    guard(|| {
        // Only the page itself: `<select>` popups paint as PET_POPUP and are not supported.
        let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else {
            return;
        };
        if kind != 0 || buffer.is_null() {
            return;
        }
        // SAFETY: CEF's buffer is `width * height * 4` bytes, valid for this call; `rects` is
        // `count` rectangles.
        let (pixels, rects) = unsafe {
            (
                std::slice::from_raw_parts(buffer.cast::<u8>(), w as usize * h as usize * 4),
                if rects.is_null() {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(rects, count)
                },
            )
        };
        let view = unsafe { view(this) };
        lock(&view.staged).apply(pixels, w, h, rects);
        view.shared.push(EngineEvent::FrameReady { view: view.id });
    });
}

unsafe extern "C" fn on_text_selection_changed(
    this: *mut CefRenderHandler,
    browser: *mut CefBrowser,
    text: *const CefStringUtf16,
    _range: *const CefRange,
) {
    own!(browser);
    guard(|| {
        // SAFETY: as above.
        let mut selected = unsafe { read(text) };
        if selected.len() > MAX_SELECTION {
            let mut end = MAX_SELECTION;
            while !selected.is_char_boundary(end) {
                end -= 1;
            }
            selected.truncate(end);
        }
        *lock(&unsafe { view(this) }.selection) = selected;
    });
}

unsafe extern "C" fn on_ime_composition_range_changed(
    this: *mut CefRenderHandler,
    browser: *mut CefBrowser,
    _selected: *const CefRange,
    count: usize,
    bounds: *const CefRect,
) {
    own!(browser);
    guard(|| {
        if count == 0 || bounds.is_null() {
            return;
        }
        // SAFETY: `bounds` is `count` rectangles; the last is the caret's character.
        let last = unsafe { *bounds.add(count - 1) };
        let view = unsafe { view(this) };
        *lock(&view.caret) = SurfaceRect {
            x: last.x,
            y: last.y,
            width: u32::try_from(last.width.max(1)).unwrap_or(1),
            height: u32::try_from(last.height.max(1)).unwrap_or(1),
        };
    });
}

// ---- the renderer's focus message ----

/// The input purpose of a `cef_dom_form_control_type_t`, and whether the field takes lines.
fn purpose(control: i32) -> (TextPurpose, bool) {
    match control {
        9 => (TextPurpose::Date, false),
        10 => (TextPurpose::DateTime, false),
        11 => (TextPurpose::Email, false),
        16 => (TextPurpose::Number, false),
        17 => (TextPurpose::Password, false),
        23 => (TextPurpose::Phone, false),
        25 => (TextPurpose::Time, false),
        26 => (TextPurpose::Url, false),
        31 => (TextPurpose::Normal, true),
        _ => (TextPurpose::Normal, false),
    }
}

/// `sig.focus` from `sig-cef-helper`: which DOM node holds the focus, as text-input state. No
/// surrounding text is ever sent (a password field's content stays in the page).
unsafe extern "C" fn on_process_message_received(
    this: *mut CefClient,
    browser: *mut CefBrowser,
    frame: *mut CefFrame,
    _source: i32,
    message: *mut CefProcessMessage,
) -> i32 {
    own!(browser, frame);
    guard(|| {
        // SAFETY: an owned reference.
        let Some(message) = Ref::from_owned(message) else {
            return 0;
        };
        let view = unsafe { &ffi::data::<CefClient, ClientData>(this).view };
        let name = unsafe { take(call!(message.as_ptr(), get_name)) };
        if name != "sig.focus" {
            return 0;
        }
        let Some(args) = Ref::from_owned(unsafe { call!(message.as_ptr(), get_argument_list) })
        else {
            return 1;
        };
        let int = |index: usize| unsafe { call!(args.as_ptr(), get_int, index) };
        let state = (int(0) != 0).then(|| {
            let (purpose, multiline) = purpose(int(1));
            TextInputState {
                caret: SurfaceRect {
                    x: int(2),
                    y: int(3),
                    width: u32::try_from(int(4).max(1)).unwrap_or(1),
                    height: u32::try_from(int(5).max(1)).unwrap_or(1),
                },
                surrounding: None,
                purpose,
                multiline,
            }
        });
        view.shared.push(EngineEvent::TextInputChanged {
            view: view.id,
            state,
        });
        1
    })
}

// ---- life span ----

unsafe extern "C" fn on_before_popup(
    this: *mut CefLifeSpanHandler,
    browser: *mut CefBrowser,
    frame: *mut CefFrame,
    _popup_id: i32,
    target_url: *const CefStringUtf16,
    _target_frame_name: *const CefStringUtf16,
    _disposition: i32,
    _user_gesture: i32,
    _features: *const c_void,
    _window_info: *mut c_void,
    _client: *mut *mut CefClient,
    _settings: *mut c_void,
    _extra_info: *mut *mut CefDictionaryValue,
    _no_javascript_access: *mut i32,
) -> i32 {
    own!(browser, frame);
    guard(|| {
        // SAFETY: as above. The popup is never created: the browser decides where the URL goes.
        let view = unsafe { view(this) };
        let url = unsafe { read(target_url) };
        let document = lock(&view.nav).document();
        if let Some(document) = document {
            view.shared.push(EngineEvent::OpenRequested {
                view: view.id,
                document,
                url,
                disposition: OpenDisposition::NewView,
            });
        }
        1
    })
}

unsafe extern "C" fn on_after_created(this: *mut CefLifeSpanHandler, browser: *mut CefBrowser) {
    guard(|| {
        // SAFETY: `browser` is an owned reference: kept in the slot.
        let view = unsafe { view(this) };
        let Some(browser) = Ref::from_owned(browser) else {
            return;
        };
        let Some(host) = (unsafe { Ref::from_owned(call!(browser.as_ptr(), get_host)) }) else {
            return;
        };
        let mut slot = lock(&view.slot);
        if view.is_closing() {
            // Closed before it existed.
            // SAFETY: the host is live.
            unsafe { call!(host.as_ptr(), close_browser, 1) };
            slot.browser = Some(browser);
            slot.host = Some(host);
            return;
        }
        view.shared.push(EngineEvent::ViewReady { view: view.id });
        let pending = std::mem::take(&mut slot.pending);
        for action in &pending {
            action.perform(&browser, &host);
        }
        slot.browser = Some(browser);
        slot.host = Some(host);
    });
}

unsafe extern "C" fn do_close(this: *mut CefLifeSpanHandler, browser: *mut CefBrowser) -> i32 {
    own!(browser);
    guard(|| {
        // SAFETY: as above. The page's `window.close()` is a request; our own close goes ahead.
        let view = unsafe { view(this) };
        if view.is_closing() {
            return 0;
        }
        if let Some(document) = lock(&view.nav).document() {
            view.shared.push(EngineEvent::CloseRequested {
                view: view.id,
                document,
            });
        }
        1
    })
}

unsafe extern "C" fn on_before_close(this: *mut CefLifeSpanHandler, browser: *mut CefBrowser) {
    own!(browser);
    guard(|| {
        // SAFETY: as above.
        let view = unsafe { view(this) };
        {
            let mut slot = lock(&view.slot);
            slot.closed = true;
            slot.pending.clear();
            slot.browser = None;
            slot.host = None;
        }
        lock(&view.staged).release();
        view.shared
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        view.shared.forget(view.id);
        view.shared.push(EngineEvent::ViewClosed { view: view.id });
    });
}

// ---- load ----

unsafe extern "C" fn on_loading_state_change(
    this: *mut CefLoadHandler,
    browser: *mut CefBrowser,
    loading: i32,
    _can_go_back: i32,
    _can_go_forward: i32,
) {
    own!(browser);
    guard(|| {
        // SAFETY: as above.
        let view = unsafe { view(this) };
        let mut events = Vec::new();
        lock(&view.nav).loading_changed(loading != 0, &mut events);
        view.shared.push_all(events);
    });
}

/// The navigation entries' addresses and the current index, read on the UI thread (CEF calls
/// the visitor before `get_navigation_entries` returns there).
fn entries(browser: *mut CefBrowser) -> (Vec<String>, usize) {
    type Found = Arc<Mutex<(Vec<String>, usize)>>;
    unsafe extern "C" fn visit(
        this: *mut CefNavigationEntryVisitor,
        entry: *mut CefNavigationEntry,
        current: i32,
        index: i32,
        _total: i32,
    ) -> i32 {
        // SAFETY: `entry` is an owned reference; `this` is the visitor made below.
        unsafe {
            let Some(entry) = Ref::from_owned(entry) else {
                return 1;
            };
            let found = ffi::data::<CefNavigationEntryVisitor, Found>(this);
            let url = take(call!(entry.as_ptr(), get_url));
            let mut found = lock(found);
            found.0.push(url);
            if current != 0 {
                found.1 = usize::try_from(index).unwrap_or(0);
            }
        }
        1
    }
    let found: Found = Arc::default();
    let mut visitor: CefNavigationEntryVisitor = zeroed();
    visitor.visit = Some(visit);
    let visitor = new_object(visitor, Arc::clone(&found));
    // SAFETY: `browser` is live for this callback.
    unsafe {
        if let Some(host) = Ref::from_owned(call!(browser, get_host)) {
            // CEF adopts the visitor's reference.
            call!(host.as_ptr(), get_navigation_entries, visitor, 0);
        } else {
            release_raw(visitor);
        }
    }
    let found = lock(&found);
    (found.0.clone(), found.1)
}

unsafe extern "C" fn on_load_start(
    this: *mut CefLoadHandler,
    browser: *mut CefBrowser,
    frame: *mut CefFrame,
    transition: i32,
) {
    guard(|| {
        // SAFETY: as above; `browser` is released by the `Ref` below.
        let _browser = Ref::from_owned(browser);
        let Some(frame) = Ref::from_owned(frame) else {
            return;
        };
        if unsafe { call!(frame.as_ptr(), is_main) } == 0 {
            return;
        }
        let view = unsafe { view(this) };
        let url = unsafe { take(call!(frame.as_ptr(), get_url)) };
        let (list, current) = entries(browser);
        let mut events = Vec::new();
        lock(&view.nav).commit(&url, transition, list, current, &mut events);
        view.shared.push_all(events);
    });
}

unsafe extern "C" fn on_load_error(
    this: *mut CefLoadHandler,
    browser: *mut CefBrowser,
    frame: *mut CefFrame,
    code: i32,
    text: *const CefStringUtf16,
    _url: *const CefStringUtf16,
) {
    own!(browser);
    guard(|| {
        // SAFETY: as above.
        let Some(frame) = Ref::from_owned(frame) else {
            return;
        };
        if unsafe { call!(frame.as_ptr(), is_main) } == 0 {
            return;
        }
        let view = unsafe { view(this) };
        let text = unsafe { read(text) };
        let mut events = Vec::new();
        lock(&view.nav).failed(code, &text, &mut events);
        view.shared.push_all(events);
    });
}

// ---- display ----

unsafe extern "C" fn on_address_change(
    this: *mut CefDisplayHandler,
    browser: *mut CefBrowser,
    frame: *mut CefFrame,
    url: *const CefStringUtf16,
) {
    guard(|| {
        // SAFETY: as above.
        let _browser = Ref::from_owned(browser);
        let Some(frame) = Ref::from_owned(frame) else {
            return;
        };
        if unsafe { call!(frame.as_ptr(), is_main) } == 0 {
            return;
        }
        let view = unsafe { view(this) };
        let url = unsafe { read(url) };
        let (list, current) = entries(browser);
        let mut events = Vec::new();
        lock(&view.nav).address_changed(&url, list, current, &mut events);
        view.shared.push_all(events);
    });
}

unsafe extern "C" fn on_title_change(
    this: *mut CefDisplayHandler,
    browser: *mut CefBrowser,
    title: *const CefStringUtf16,
) {
    own!(browser);
    guard(|| {
        // SAFETY: as above.
        let view = unsafe { view(this) };
        let title = unsafe { read(title) };
        view.shared.push(EngineEvent::TitleChanged {
            view: view.id,
            title: (!title.is_empty()).then_some(title),
        });
    });
}

unsafe extern "C" fn on_status_message(
    this: *mut CefDisplayHandler,
    browser: *mut CefBrowser,
    value: *const CefStringUtf16,
) {
    own!(browser);
    guard(|| {
        // SAFETY: as above.
        let view = unsafe { view(this) };
        let text = unsafe { read(value) };
        view.shared.push(EngineEvent::StatusTextChanged {
            view: view.id,
            text: (!text.is_empty()).then_some(text),
        });
    });
}

unsafe extern "C" fn on_cursor_change(
    this: *mut CefDisplayHandler,
    browser: *mut CefBrowser,
    _cursor: *mut c_void,
    kind: i32,
    _custom: *const CefCursorInfo,
) -> i32 {
    own!(browser);
    guard(|| {
        // SAFETY: as above. The host draws the cursor, by name.
        let view = unsafe { view(this) };
        view.shared.push(EngineEvent::CursorChanged {
            view: view.id,
            cursor: keys::cursor(kind),
        });
        1
    })
}

// ---- request ----

unsafe extern "C" fn on_before_browse(
    this: *mut CefRequestHandler,
    browser: *mut CefBrowser,
    frame: *mut CefFrame,
    request: *mut CefRequest,
    user_gesture: i32,
    is_redirect: i32,
) -> i32 {
    own!(browser);
    guard(|| {
        // SAFETY: as above.
        let frame = Ref::from_owned(frame);
        let request = Ref::from_owned(request);
        let (Some(frame), Some(request)) = (frame, request) else {
            return 0;
        };
        if unsafe { call!(frame.as_ptr(), is_main) } == 0 {
            return 0;
        }
        let view = unsafe { view(this) };
        let url = unsafe { take(call!(request.as_ptr(), get_url)) };
        let transition = unsafe { call!(request.as_ptr(), get_transition_type) };
        let mut events = Vec::new();
        let answer = lock(&view.nav).before_browse(
            &url,
            user_gesture != 0,
            is_redirect != 0,
            transition,
            &mut events,
        );
        view.shared.push_all(events);
        i32::from(answer == Browse::Hold)
    })
}

unsafe extern "C" fn on_render_process_terminated(
    this: *mut CefRequestHandler,
    browser: *mut CefBrowser,
    status: i32,
    _error_code: i32,
    _error: *const CefStringUtf16,
) {
    own!(browser);
    guard(|| {
        // SAFETY: as above.
        let view = unsafe { view(this) };
        lock(&view.nav).crashed();
        view.shared.push(EngineEvent::ViewFailed {
            view: view.id,
            message: format!("the page's content process ended (status {status})"),
        });
    });
}

// ---- downloads, dialogs, permissions, menus ----

/// The origin of the committed page, for a service request.
pub(crate) fn origin(view: &ViewShared) -> Option<String> {
    let nav = lock(&view.nav);
    let origin = url::Url::parse(nav.url()).ok()?.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

unsafe extern "C" fn on_before_download(
    this: *mut CefDownloadHandler,
    browser: *mut CefBrowser,
    item: *mut CefDownloadItem,
    suggested: *const CefStringUtf16,
    callback: *mut CefBeforeDownloadCallback,
) -> i32 {
    own!(browser, callback);
    guard(|| {
        // SAFETY: as above. The browser fetches it through its download service; returning
        // false without continuing cancels CEF's own download (Alloy style).
        let Some(item) = Ref::from_owned(item) else {
            return 0;
        };
        let view = unsafe { view(this) };
        let url = unsafe { take(call!(item.as_ptr(), get_url)) };
        let suggested = unsafe { read(suggested) };
        let document = lock(&view.nav).document();
        if let Some(document) = document {
            view.shared
                .push(EngineEvent::ServiceRequested(ServiceRequest {
                    id: RequestId::next(),
                    view: view.id,
                    document,
                    origin: origin(view),
                    service: Service::Download {
                        source: DownloadSource::Url(url),
                        suggested_name: (!suggested.is_empty()).then_some(suggested),
                    },
                }));
        }
        0
    })
}

unsafe extern "C" fn on_jsdialog(
    _this: *mut CefJsdialogHandler,
    browser: *mut CefBrowser,
    _origin: *const CefStringUtf16,
    _kind: i32,
    _message: *const CefStringUtf16,
    _default: *const CefStringUtf16,
    callback: *mut CefJsdialogCallback,
    suppress: *mut i32,
) -> i32 {
    own!(browser, callback);
    // Suppressed, not answered: an answer would tell a spamming page its dialog was seen.
    // SAFETY: `suppress` is CEF's out-parameter.
    unsafe { *suppress = 1 };
    0
}

unsafe extern "C" fn on_before_unload_dialog(
    _this: *mut CefJsdialogHandler,
    browser: *mut CefBrowser,
    _message: *const CefStringUtf16,
    _is_reload: i32,
    callback: *mut CefJsdialogCallback,
) -> i32 {
    own!(browser);
    guard(|| {
        // The page is left without asking, as with the other engines.
        // SAFETY: an owned reference.
        if let Some(callback) = Ref::from_owned(callback) {
            let empty = ffi::empty_string();
            unsafe { call!(callback.as_ptr(), cont, 1, &empty) };
        }
        1
    })
}

unsafe extern "C" fn on_request_media_access_permission(
    _this: *mut CefPermissionHandler,
    browser: *mut CefBrowser,
    frame: *mut CefFrame,
    _origin: *const CefStringUtf16,
    _permissions: u32,
    callback: *mut CefMediaAccessCallback,
) -> i32 {
    own!(browser, frame);
    guard(|| {
        // Camera, microphone and screen capture fail closed.
        // SAFETY: an owned reference.
        if let Some(callback) = Ref::from_owned(callback) {
            unsafe { call!(callback.as_ptr(), cancel) };
        }
        1
    })
}

unsafe extern "C" fn on_show_permission_prompt(
    _this: *mut CefPermissionHandler,
    browser: *mut CefBrowser,
    _prompt: u64,
    _origin: *const CefStringUtf16,
    _permissions: u32,
    callback: *mut CefPermissionPromptCallback,
) -> i32 {
    own!(browser);
    guard(|| {
        // Every permission prompt (location, clipboard, notifications) is denied.
        // SAFETY: an owned reference. `CEF_PERMISSION_RESULT_DENY` is 1.
        if let Some(callback) = Ref::from_owned(callback) {
            unsafe { call!(callback.as_ptr(), cont, 1) };
        }
        1
    })
}

unsafe extern "C" fn on_file_dialog(
    _this: *mut CefDialogHandler,
    browser: *mut CefBrowser,
    _mode: i32,
    _title: *const CefStringUtf16,
    _default: *const CefStringUtf16,
    _filters: *mut c_void,
    _extensions: *mut c_void,
    _descriptions: *mut c_void,
    callback: *mut CefFileDialogCallback,
) -> i32 {
    own!(browser);
    guard(|| {
        // No file chooser without the browser's policy: cancelled.
        // SAFETY: an owned reference.
        if let Some(callback) = Ref::from_owned(callback) {
            unsafe { call!(callback.as_ptr(), cancel) };
        }
        1
    })
}

unsafe extern "C" fn on_before_context_menu(
    _this: *mut CefContextMenuHandler,
    browser: *mut CefBrowser,
    frame: *mut CefFrame,
    params: *mut CefContextMenuParams,
    model: *mut CefMenuModel,
) {
    own!(browser, frame, params);
    guard(|| {
        // No native menu: an empty model shows nothing.
        // SAFETY: an owned reference.
        if let Some(model) = Ref::from_owned(model) {
            unsafe { call!(model.as_ptr(), clear) };
        }
    });
}
