//! One WPE view: its `WebKitWebView` and Sighurt `WPEView`, its target and presentation state,
//! its navigations, and the requests it made; and the WebKit signals it listens to.
//!
//! The signal handlers only record [`Note`]s (and the replaceable [`Facts`]) in the runtime's
//! [`Inbox`], with what WebKit says at that moment (its address, a [`Snapshot`] of its
//! back-forward list); the runtime turns them into events after WebKit returns. The few
//! decisions taken inside a handler are WebKit's own policy, answered at once: which navigations
//! WebKit performs (web schemes; anything else is refused, and goes to the browser when it is
//! the browser's own or the user started it, as WebKit's user-gesture flag says) and which of
//! the page's it holds for the browser's answer first (`asks`: links and forms the user
//! clicked, as a [`PageNavigation`] that refuses when dropped), that a response it can't
//! show becomes a download (which the browser performs), that `window.open` and
//! `target=_blank` make no WebKit view (the browser opens a tab), that HTTP authentication and
//! context menus are refused, that a page asking to stay when it is left (`beforeunload`) is
//! left, and that a failed load shows no WebKit error page (the browser shows the failure).
//!
//! Must not own: browser policy (the core decides about every request) or presentation timing
//! (the host's permits).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_int;
use std::rc::Rc;
use std::time::Instant;

use glib::prelude::*;
use glib::translate::*;
use sighurt_api::events::NavigationError;
use sighurt_api::geometry::{BufferSize, SurfaceRect};
use sighurt_api::ids::{LayoutEpoch, RequestId, ViewId};
use sighurt_api::input::SurroundingText;
use sighurt_api::services::{
    Permission, PickMode, Service, ServiceReply, ServiceRequest, ServiceResult,
};

use crate::clipboard::ClipboardCall;
use crate::ffi;
use crate::ime::Ime;
use crate::input::Input;
use crate::navigation::{Navigator, Snapshot};
use crate::platform::{Buffer, View};
use crate::target::{WaylandSlot, WpeTarget};

/// Service requests a view may have outstanding; WebKit's beyond them are refused.
pub const MAX_REQUESTS: usize = 16;

/// Navigations a view holds for the browser's answer; the page's beyond them go ahead without
/// asking.
pub const MAX_HELD: usize = 16;

/// Schemes WebKit loads itself. A navigation of the page's to any other goes to the browser.
const WEB_SCHEMES: [&str; 6] = ["http", "https", "file", "data", "blob", "about"];

/// The `GObject` data key holding a web view's [`ViewId`].
const VIEW_KEY: &str = "sighurt-view";

/// What WebKit said about one view, in order.
pub enum Note {
    Frame(Buffer, Vec<ffi::WPERectangle>),
    /// A document load started.
    Started,
    /// A document load committed at this address.
    Committed(String, Snapshot<glib::Object>),
    /// A document load failed; `provisional` if before it committed.
    Failed {
        provisional: bool,
        error: NavigationError,
    },
    /// The back-forward list changed; `true` if an entry was added.
    ListChanged(bool, Snapshot<glib::Object>),
    /// WebKit's address changed.
    Url(String, Snapshot<glib::Object>),
    /// Loading started (`true`) or stopped, with the address and list then (only when it
    /// stopped: nothing reads them for a start).
    Loading(bool, String, Snapshot<glib::Object>),
    /// A navigation to a URL WebKit doesn't load: the browser's, or the page's, and whether
    /// the user started it (WebKit's user gesture).
    Elsewhere(String, bool),
    /// A navigation the page asks for, held for the browser's answer, to this URL.
    Navigation(PageNavigation, String),
    /// WebKit's web process ended, for this reason.
    Terminated(u32),
    /// The page called `window.close()`.
    Close,
    /// The page asks for a new tab with this URL.
    NewView(String),
    /// A permission request, and the permissions it needs (none: refuse it).
    Permission(glib::Object, Vec<Permission>),
    /// A file chooser.
    Files(glib::Object, PickMode, Vec<String>),
    /// A download WebKit was about to make: its URL and suggested name.
    Download(String, Option<String>),
    Ime(Ime),
    Script(Result<String, String>),
    /// A semantic script finished.
    Semantic(crate::semantic::Tag, Result<String, String>),
    /// The isolated world's observer saw the DOM change.
    Mutated,
}

/// The replaceable things WebKit said about one view: only the latest matters.
#[derive(Default)]
pub struct Facts {
    pub title: bool,
    pub status: Option<Option<String>>,
    pub cursor: Option<String>,
}

/// What every view's handlers record, for the runtime.
#[derive(Default)]
pub struct Inbox {
    pub notes: Vec<(ViewId, Note)>,
    pub facts: HashMap<ViewId, Facts>,
    /// The clipboard's calls, for the view that had the user's latest input.
    pub clipboard: Vec<ClipboardCall>,
}

pub type Shared = Rc<RefCell<Inbox>>;

/// What the focused editable element tells the input method.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextInput {
    pub caret: SurfaceRect,
    pub surrounding: Option<SurroundingText>,
}

/// A service request the view made, until it is answered.
pub struct Request {
    /// The browser's ids for it: one per permission it needs.
    pub ids: Vec<RequestId>,
    pub kind: RequestKind,
}

pub enum RequestKind {
    /// WebKit's permission request, allowed once every id is granted.
    Permission(glib::Object),
    /// WebKit's file chooser request.
    Files(glib::Object),
    /// A paste waiting for the clipboard.
    ClipboardRead,
    /// Nothing to complete: a clipboard write or a download.
    Done,
}

/// WebKit's decision about a navigation the page asks for (`asks`), until the browser
/// answers. Dropped unanswered (with its note, its view, its document or the runtime), it is
/// ignored: WebKit would let it go ahead.
pub struct PageNavigation(Option<glib::Object>);

impl PageNavigation {
    /// Lets it go ahead.
    pub fn allow(mut self) {
        if let Some(decision) = self.0.take() {
            unsafe { ffi::webkit_policy_decision_use(decision.as_ptr().cast()) };
        }
    }
}

impl Drop for PageNavigation {
    fn drop(&mut self) {
        if let Some(decision) = self.0.take() {
            unsafe { ffi::webkit_policy_decision_ignore(decision.as_ptr().cast()) };
        }
    }
}

/// What an answer asks of the runtime.
pub enum Answer {
    /// Nothing more.
    None,
    /// Paste this text.
    Paste(String),
    /// Withdraw these requests of the same WebKit request.
    Withdraw(Vec<RequestId>),
}

/// One view. `close` drops WebKit's objects first and the target last; the field order is not
/// that sequence.
pub struct Page<T: WpeTarget> {
    pub id: ViewId,
    pub web_view: glib::Object,
    pub view: View,
    pub input: Input,
    /// The latest frame WebKit rendered that is not yet shown.
    pub pending: Option<(Buffer, Vec<ffi::WPERectangle>)>,
    /// The epoch of the frame last submitted.
    pub presented: Option<LayoutEpoch>,
    pub navigator: Navigator<glib::Object>,
    pub loading: bool,
    pub requests: Vec<Request>,
    /// Navigations the committed document asked for, waiting for the browser's answer, with
    /// when each was asked.
    pub held: Vec<(RequestId, PageNavigation, Instant)>,
    /// The focused editable element, while there is one.
    pub text_input: Option<TextInput>,
    /// What the frontend asked of the view's semantic snapshots.
    pub semantics: crate::semantic::Semantics,
    /// The layout epoch last applied. A CPU permit must name it. A Wayland permit does not.
    pub(crate) epoch: LayoutEpoch,
    /// The buffer size of [`epoch`](Self::epoch).
    pub(crate) buffer: BufferSize,
    /// Wayland surface and shared-memory buffers. None when the target has no display.
    pub(crate) slot: Option<WaylandSlot>,
    /// Dropped after WebKit's objects and, on Wayland, after `slot`.
    pub(crate) target: T,
}

impl<T: WpeTarget> Page<T> {
    pub fn wpe(&self) -> *mut ffi::WPEView {
        self.view.as_ptr()
    }

    pub fn web(&self) -> *mut ffi::WebKitWebView {
        self.web_view.as_ptr().cast()
    }

    /// The origin of the committed page, for service requests: scheme, host and port.
    pub fn origin(&self) -> Option<String> {
        let url = url::Url::parse(self.navigator.url()?).ok()?;
        let origin = url.origin();
        origin.is_tuple().then(|| origin.ascii_serialization())
    }

    /// The service requests for `services`, made for the committed document as one request of
    /// `kind`; `None` without a document or beyond [`MAX_REQUESTS`], which refuses it.
    pub fn ask(
        &mut self,
        services: Vec<Service>,
        kind: RequestKind,
    ) -> Option<Vec<ServiceRequest>> {
        let document = self.navigator.document()?;
        let outstanding: usize = self.requests.iter().map(|r| r.ids.len()).sum();
        if services.is_empty() || outstanding + services.len() > MAX_REQUESTS {
            return None;
        }
        let requests: Vec<ServiceRequest> = services
            .into_iter()
            .map(|service| ServiceRequest {
                id: RequestId::next(),
                view: self.id,
                document,
                origin: self.origin(),
                service,
            })
            .collect();
        self.requests.push(Request {
            ids: requests.iter().map(|r| r.id).collect(),
            kind,
        });
        Some(requests)
    }

    /// Completes WebKit's request with the browser's answer to `id`.
    pub fn answer(&mut self, id: RequestId, result: ServiceResult) -> Answer {
        let Some(index) = self.requests.iter().position(|r| r.ids.contains(&id)) else {
            return Answer::None;
        };
        let request = &mut self.requests[index];
        request.ids.retain(|&i| i != id);
        match (&request.kind, result) {
            (RequestKind::Permission(_), Ok(_)) if !request.ids.is_empty() => Answer::None,
            (RequestKind::Permission(object), Ok(_)) => {
                unsafe { ffi::webkit_permission_request_allow(object.as_ptr().cast()) };
                self.requests.remove(index);
                Answer::None
            }
            (RequestKind::Permission(object), Err(_)) => {
                unsafe { ffi::webkit_permission_request_deny(object.as_ptr().cast()) };
                Answer::Withdraw(self.requests.remove(index).ids)
            }
            (RequestKind::Files(object), Ok(ServiceReply::Files(paths))) => {
                let paths: Vec<std::ffi::CString> = paths
                    .iter()
                    .filter_map(|p| std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok())
                    .collect();
                let mut pointers: Vec<*const std::ffi::c_char> =
                    paths.iter().map(|p| p.as_ptr()).collect();
                pointers.push(std::ptr::null());
                unsafe {
                    ffi::webkit_file_chooser_request_select_files(
                        object.as_ptr().cast(),
                        pointers.as_ptr(),
                    )
                };
                self.requests.remove(index);
                Answer::None
            }
            (RequestKind::Files(object), _) => {
                unsafe { ffi::webkit_file_chooser_request_cancel(object.as_ptr().cast()) };
                self.requests.remove(index);
                Answer::None
            }
            (RequestKind::ClipboardRead, Ok(ServiceReply::Clipboard(data))) => {
                self.requests.remove(index);
                match String::from_utf8(data.data) {
                    Ok(text) => Answer::Paste(text),
                    Err(_) => Answer::None,
                }
            }
            (RequestKind::ClipboardRead | RequestKind::Done, _) => {
                self.requests.remove(index);
                Answer::None
            }
        }
    }

    /// Whether a paste already waits for the clipboard.
    pub fn reading_clipboard(&self) -> bool {
        self.requests
            .iter()
            .any(|r| matches!(r.kind, RequestKind::ClipboardRead))
    }
}

/// The view a web view belongs to, from the data [`connect`] gave it.
pub fn view_of(web_view: &glib::Object) -> Option<ViewId> {
    unsafe { web_view.data::<ViewId>(VIEW_KEY).map(|v| *v.as_ref()) }
}

/// WebKit's back-forward list of `web_view`, oldest entry first.
pub fn snapshot(web_view: &glib::Object) -> Snapshot<glib::Object> {
    let list = unsafe { ffi::webkit_web_view_get_back_forward_list(web_view.as_ptr().cast()) };
    let count = |glist: *mut glib::ffi::GList| unsafe {
        let n = glib::ffi::g_list_length(glist);
        glib::ffi::g_list_free(glist);
        n as c_int
    };
    let back = count(unsafe { ffi::webkit_back_forward_list_get_back_list(list) });
    let forward = count(unsafe { ffi::webkit_back_forward_list_get_forward_list(list) });
    let mut snapshot = Snapshot::default();
    for index in -back..=forward {
        let item = unsafe { ffi::webkit_back_forward_list_get_nth_item(list, index) };
        if item.is_null() {
            continue;
        }
        if index == 0 {
            snapshot.current = Some(snapshot.items.len());
        }
        let url = unsafe { ffi::string(ffi::webkit_back_forward_list_item_get_uri(item)) };
        snapshot.urls.push(url.unwrap_or_default());
        snapshot
            .items
            .push(unsafe { from_glib_none(item.cast::<glib::gobject_ffi::GObject>()) });
    }
    snapshot
}

/// WebKit's address of `web_view`.
pub fn address(web_view: &glib::Object) -> String {
    unsafe { ffi::string(ffi::webkit_web_view_get_uri(web_view.as_ptr().cast())) }
        .unwrap_or_default()
}

/// The value of an enum-typed signal argument.
fn enum_arg(value: &glib::Value) -> u32 {
    unsafe { glib::gobject_ffi::g_value_get_enum(value.to_glib_none().0) as u32 }
}

/// The object of an object-typed signal argument (transfer none), or null.
fn object_arg<T>(value: &glib::Value) -> *mut T {
    unsafe { glib::gobject_ffi::g_value_get_object(value.to_glib_none().0).cast() }
}

/// The pointer of a boxed signal argument (transfer none).
fn boxed_arg<T>(value: &glib::Value) -> *mut T {
    unsafe { glib::gobject_ffi::g_value_get_boxed(value.to_glib_none().0).cast() }
}

/// Connects `web_view`'s signals to `inbox` for `view`.
pub fn connect(web_view: &glib::Object, inbox: &Shared, view: ViewId) {
    unsafe { web_view.set_data(VIEW_KEY, view) };
    let weak = Rc::downgrade(inbox);
    let note = move |note: Note| {
        if let Some(inbox) = weak.upgrade() {
            inbox.borrow_mut().notes.push((view, note));
        }
    };
    let weak = Rc::downgrade(inbox);
    let fact = move |set: &dyn Fn(&mut Facts)| {
        if let Some(inbox) = weak.upgrade() {
            set(inbox.borrow_mut().facts.entry(view).or_default());
        }
    };
    let raw = |object: &glib::Object| object.as_ptr().cast::<ffi::WebKitWebView>();

    let n = note.clone();
    web_view.connect_local("load-changed", false, move |args| {
        let web: glib::Object = args[0].get().ok()?;
        match enum_arg(&args[1]) {
            ffi::WEBKIT_LOAD_STARTED => n(Note::Started),
            ffi::WEBKIT_LOAD_COMMITTED => n(Note::Committed(address(&web), snapshot(&web))),
            _ => {}
        }
        None
    });
    let n = note.clone();
    web_view.connect_local("load-failed", false, move |args| {
        let provisional = matches!(
            enum_arg(&args[1]),
            ffi::WEBKIT_LOAD_STARTED | ffi::WEBKIT_LOAD_REDIRECTED
        );
        let error = match args[3].get::<glib::Error>() {
            Ok(e) if is_cancellation(&e) => NavigationError::Cancelled,
            Ok(e) => NavigationError::Load(e.message().to_owned()),
            Err(_) => NavigationError::Load("the page could not be loaded".into()),
        };
        n(Note::Failed { provisional, error });
        // Handled: no WebKit error page; the browser shows the failure.
        Some(true.to_value())
    });
    let n = note.clone();
    web_view.connect_notify_local(Some("uri"), move |web, _| {
        n(Note::Url(address(web), snapshot(web)));
    });
    let n = note.clone();
    web_view.connect_notify_local(Some("is-loading"), move |web, _| {
        let loading = unsafe { ffi::webkit_web_view_is_loading(raw(web)) } != 0;
        let note = match loading {
            true => Note::Loading(true, String::new(), Snapshot::default()),
            false => Note::Loading(false, address(web), snapshot(web)),
        };
        n(note);
    });
    let list: glib::Object = unsafe {
        from_glib_none(
            ffi::webkit_web_view_get_back_forward_list(raw(web_view))
                .cast::<glib::gobject_ffi::GObject>(),
        )
    };
    let n = note.clone();
    let web = web_view.downgrade();
    list.connect_local("changed", false, move |args| {
        let added = !object_arg::<glib::gobject_ffi::GObject>(&args[1]).is_null();
        if let Some(web) = web.upgrade() {
            n(Note::ListChanged(added, snapshot(&web)));
        }
        None
    });
    let n = note.clone();
    web_view.connect_local("decide-policy", false, move |args| {
        let decision: *mut ffi::WebKitPolicyDecision = object_arg(&args[1]);
        match enum_arg(&args[2]) {
            ffi::WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION => {
                let action = unsafe {
                    ffi::webkit_navigation_policy_decision_get_navigation_action(decision)
                };
                let request = unsafe { ffi::webkit_navigation_action_get_request(action) };
                let url = unsafe { ffi::string(ffi::webkit_uri_request_get_uri(request)) }
                    .unwrap_or_default();
                if !loads_itself(&url) {
                    // Noted first: ignoring it may end the browser's request at once. WebKit
                    // doesn't say which frame navigates: whether the user started it decides.
                    let gesture =
                        unsafe { ffi::webkit_navigation_action_is_user_gesture(action) } != 0;
                    n(Note::Elsewhere(url, gesture));
                    unsafe { ffi::webkit_policy_decision_ignore(decision) };
                    return Some(true.to_value());
                }
                if !asks(action, request) {
                    return Some(false.to_value()); // WebKit's own: it goes ahead.
                }
                let decision: glib::Object = args[1].get().ok()?;
                n(Note::Navigation(PageNavigation(Some(decision)), url));
                Some(true.to_value())
            }
            ffi::WEBKIT_POLICY_DECISION_TYPE_RESPONSE => {
                let (supported, status) = unsafe {
                    let response = ffi::webkit_response_policy_decision_get_response(decision);
                    (
                        ffi::webkit_response_policy_decision_is_mime_type_supported(decision) != 0,
                        ffi::webkit_uri_response_get_status_code(response),
                    )
                };
                if supported || status == ffi::HTTP_NO_CONTENT {
                    // WebKit's own: show it, or download an attachment.
                    return Some(false.to_value());
                }
                // Content WebKit can't show is a download (WebKit's default ignores it).
                unsafe { ffi::webkit_policy_decision_download(decision) };
                Some(true.to_value())
            }
            _ => Some(false.to_value()),
        }
    });
    let n = note.clone();
    let web_type: glib::Type = unsafe { from_glib(ffi::webkit_web_view_get_type()) };
    web_view.connect_local("create", false, move |args| {
        let action: *mut ffi::WebKitNavigationAction = boxed_arg(&args[1]);
        let url = unsafe {
            ffi::string(ffi::webkit_uri_request_get_uri(
                ffi::webkit_navigation_action_get_request(action),
            ))
        };
        if let Some(url) = url.filter(|u| !u.is_empty()) {
            n(Note::NewView(url));
        }
        // No WebKit view: the browser opens the tab.
        Some(glib::Value::from_type(web_type))
    });
    let n = note.clone();
    web_view.connect_local("close", false, move |_| {
        n(Note::Close);
        None
    });
    let n = note.clone();
    web_view.connect_local("permission-request", false, move |args| {
        let request: glib::Object = args[1].get().ok()?;
        n(Note::Permission(request.clone(), permissions(&request)));
        Some(true.to_value())
    });
    let n = note.clone();
    web_view.connect_local("run-file-chooser", false, move |args| {
        let request: glib::Object = args[1].get().ok()?;
        let raw = request.as_ptr().cast::<ffi::WebKitFileChooserRequest>();
        let multiple = unsafe { ffi::webkit_file_chooser_request_get_select_multiple(raw) } != 0;
        let mode = if multiple {
            PickMode::OpenFiles
        } else {
            PickMode::OpenFile
        };
        let mut filters = Vec::new();
        let types = unsafe { ffi::webkit_file_chooser_request_get_mime_types(raw) };
        if !types.is_null() {
            for i in 0.. {
                match unsafe { ffi::string(*types.add(i)) } {
                    Some(t) => filters.push(t),
                    None => break,
                }
            }
        }
        n(Note::Files(request, mode, filters));
        Some(true.to_value())
    });
    web_view.connect_local("authenticate", false, |args| {
        let request: *mut ffi::WebKitAuthenticationRequest = object_arg(&args[1]);
        // No credentials from the browser yet: refused.
        unsafe { ffi::webkit_authentication_request_cancel(request) };
        Some(true.to_value())
    });
    web_view.connect_local("context-menu", false, |_| Some(true.to_value()));
    web_view.connect_local("script-dialog", false, |args| {
        let dialog: *mut ffi::WebKitScriptDialog = boxed_arg(&args[1]);
        let kind = unsafe { ffi::webkit_script_dialog_get_dialog_type(dialog) };
        if kind != ffi::WEBKIT_SCRIPT_DIALOG_BEFORE_UNLOAD_CONFIRM {
            // WebKit's own: an alert closes, a confirm or prompt is cancelled.
            return Some(false.to_value());
        }
        // A page asking to stay when it is left is left, as in Servo: WebKit's own answer
        // (stay) would cancel the browser's navigation without a word.
        unsafe { ffi::webkit_script_dialog_confirm_set_confirmed(dialog, glib::ffi::GTRUE) };
        Some(true.to_value())
    });
    let f = fact.clone();
    web_view.connect_notify_local(Some("title"), move |_, _| f(&|facts| facts.title = true));
    let f = fact.clone();
    web_view.connect_local("mouse-target-changed", false, move |args| {
        let hit: *mut ffi::WebKitHitTestResult = object_arg(&args[1]);
        let link = unsafe {
            (ffi::webkit_hit_test_result_context_is_link(hit) != 0)
                .then(|| ffi::string(ffi::webkit_hit_test_result_get_link_uri(hit)))
                .flatten()
        };
        f(&|facts| facts.status = Some(link.clone()));
        None
    });
    web_view.connect_local("web-process-terminated", false, move |args| {
        note(Note::Terminated(enum_arg(&args[1])));
        None
    });
}

/// Connects the network session's downloads to `inbox`: each is cancelled and handed to the
/// browser as a download request of its view. Returns the handler, for `disconnect`.
pub fn connect_downloads(session: &glib::Object, inbox: &Shared) -> glib::SignalHandlerId {
    let weak = Rc::downgrade(inbox);
    session.connect_local("download-started", false, move |args| {
        let download: glib::Object = args[1].get().ok()?;
        let weak = weak.clone();
        download.connect_local("decide-destination", false, move |args| {
            let download: glib::Object = args[0].get().ok()?;
            let raw = download.as_ptr().cast::<ffi::WebKitDownload>();
            let name = args[1].get::<String>().ok().filter(|n| !n.is_empty());
            let (web, url) = unsafe {
                let response = ffi::webkit_download_get_response(raw);
                let url = match response.is_null() {
                    false => ffi::string(ffi::webkit_uri_response_get_uri(response)),
                    true => ffi::string(ffi::webkit_uri_request_get_uri(
                        ffi::webkit_download_get_request(raw),
                    )),
                };
                (ffi::webkit_download_get_web_view(raw), url)
            };
            unsafe { ffi::webkit_download_cancel(raw) };
            let web: Option<glib::Object> = (!web.is_null())
                .then(|| unsafe { from_glib_none(web.cast::<glib::gobject_ffi::GObject>()) });
            if let (Some(view), Some(url), Some(inbox)) =
                (web.as_ref().and_then(view_of), url, weak.upgrade())
            {
                inbox
                    .borrow_mut()
                    .notes
                    .push((view, Note::Download(url, name)));
            }
            Some(true.to_value())
        });
        None
    })
}

/// Keeps WebKit from finding the user's position itself (GeoClue over D-Bus) for `context`'s
/// pages: the browser has no position service yet, so a page the browser granted geolocation
/// hears that none is available. Returns WebKit's manager and the handler, for `disconnect`.
pub fn refuse_positions(context: &glib::Object) -> (glib::Object, glib::SignalHandlerId) {
    let manager: glib::Object = unsafe {
        from_glib_none(ffi::webkit_web_context_get_geolocation_manager(
            context.as_ptr(),
        ))
    };
    let handler = manager.connect_local("start", false, |args| {
        let manager: glib::Object = args[0].get().ok()?;
        let message = c"Sighurt has no position service";
        unsafe { ffi::webkit_geolocation_manager_failed(manager.as_ptr(), message.as_ptr()) };
        Some(true.to_value())
    });
    (manager, handler)
}

/// Whether the browser is asked before WebKit performs `action`, a navigation it would load
/// itself: a link the user clicked or a form the user sent with GET, as WebKit's user-gesture
/// flag and navigation type say, and not a redirect. WebKit's policy decision doesn't say which
/// frame navigates, and a click-made frame loads as a script's navigation would ("other" with a
/// gesture), so neither scripts nor frames loading are asked about, even after a click; they go
/// ahead in WebKit, as the browser's own loads, reloads and traversals do (no gesture). A link
/// clicked inside a frame is asked about like any other. A form sent with POST isn't, since
/// another engine would get only its address.
fn asks(action: *mut ffi::WebKitNavigationAction, request: *mut ffi::WebKitURIRequest) -> bool {
    let (kind, gesture, redirect) = unsafe {
        (
            ffi::webkit_navigation_action_get_navigation_type(action),
            ffi::webkit_navigation_action_is_user_gesture(action) != 0,
            ffi::webkit_navigation_action_is_redirect(action) != 0,
        )
    };
    let get = || {
        let method = unsafe { ffi::string(ffi::webkit_uri_request_get_http_method(request)) };
        method.is_none_or(|m| m.eq_ignore_ascii_case("GET"))
    };
    gesture
        && !redirect
        && match kind {
            ffi::WEBKIT_NAVIGATION_TYPE_LINK_CLICKED => true,
            ffi::WEBKIT_NAVIGATION_TYPE_FORM_SUBMITTED => get(),
            _ => false,
        }
}

/// Whether WebKit loads `url` itself rather than handing it to the browser.
fn loads_itself(url: &str) -> bool {
    url.split_once(':')
        .is_some_and(|(scheme, _)| WEB_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()))
}

/// A cancelled or superseded load, or one that became a download: nothing to show.
fn is_cancellation(error: &glib::Error) -> bool {
    let domain = |name: &str| glib::Quark::from_str(name) == error.domain();
    (domain("WebKitNetworkError") && error.code() == ffi::WEBKIT_NETWORK_ERROR_CANCELLED)
        || (domain("WebKitPolicyError")
            && error.code() == ffi::WEBKIT_POLICY_ERROR_FRAME_LOAD_INTERRUPTED_BY_POLICY_CHANGE)
}

/// The permissions WebKit's `request` needs; none for requests the api has no permission for.
fn permissions(request: &glib::Object) -> Vec<Permission> {
    let raw = request.as_ptr().cast::<ffi::WebKitPermissionRequest>();
    match unsafe { ffi::type_name(raw) }.as_str() {
        "WebKitGeolocationPermissionRequest" => vec![Permission::Geolocation],
        "WebKitUserMediaPermissionRequest" => unsafe {
            if ffi::webkit_user_media_permission_is_for_display_device(raw) != 0 {
                return vec![Permission::ScreenCapture];
            }
            let mut needs = Vec::new();
            if ffi::webkit_user_media_permission_is_for_video_device(raw) != 0 {
                needs.push(Permission::Camera);
            }
            if ffi::webkit_user_media_permission_is_for_audio_device(raw) != 0 {
                needs.push(Permission::Microphone);
            }
            needs
        },
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webkit_loads_web_schemes_and_hands_over_the_rest() {
        for url in [
            "https://a.test/",
            "HTTP://a.test",
            "file:///tmp/x",
            "data:,x",
            "about:blank",
        ] {
            assert!(loads_itself(url), "{url}");
        }
        for url in [
            "mailto:a@b.test",
            "sig://home",
            "tel:123",
            "javascript-ish",
            "",
        ] {
            assert!(!loads_itself(url), "{url}");
        }
    }
}
