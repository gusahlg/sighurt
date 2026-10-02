//! What an engine reports to the browser.
//!
//! An engine reports facts; it never changes browser state directly. Events are collected
//! while the engine runs ([`EngineRuntime::pump`](crate::engine::EngineRuntime::pump)) and
//! handled after the engine call returns, so no callback mutates the browser re-entrantly.
//!
//! Every event about a view carries its [`ViewId`], navigation events their [`NavigationId`],
//! and requests made on behalf of a document its [`DocumentGeneration`]. The core drops an
//! event whose view is closed or no longer belongs to the tab, and a request whose document
//! was replaced. Page-supplied strings (titles, status text, URLs, history entries) are
//! untrusted data for the chrome to display contained, never to act on. The core bounds what
//! it keeps: text to 16 KiB, and URLs to 2 MiB, dropping a longer URL rather than shortening
//! it, since a shortened URL points somewhere else.
//!
//! # Navigations
//!
//! Navigation events are about a view's top-level document; subframe navigations are not
//! reported. A navigation the browser asked for keeps the [`NavigationId`] of its
//! [`EngineCommand`](crate::engine::EngineCommand). One the page started itself gets an id the
//! adapter allocates, and its [`NavigationStarted`](EngineEvent::NavigationStarted) is
//! optional, since some engines learn of it only when it commits.
//!
//! Before an engine performs a navigation its page started (a link, a form, a script), it asks
//! the browser with [`NavigationRequested`](EngineEvent::NavigationRequested), since the
//! browser's routing may give the URL to another engine; one it can't perform itself it hands
//! over with [`OpenRequested`](EngineEvent::OpenRequested). An engine that knows which of them
//! the user started may ask about those alone (WebKit's user-gesture flag: a link or a GET form
//! the user clicked); the others stay in it. HTTP redirects are part of the
//! navigation that got them, not navigations of their own.
//!
//! A [`NavigationCommitted`](EngineEvent::NavigationCommitted), or a `NavigationStarted` with a
//! newer id, supersedes every navigation of that view that was pending before it: the core
//! drops those without waiting for a [`Cancelled`](NavigationError::Cancelled), which engines
//! need not send for them. An engine that cannot tell which navigation committed (Servo 0.5's
//! `WebView::load` returns no id) attributes the commit to the latest navigation the browser
//! asked for that is still pending, or allocates a new id, and documents that limit in its
//! adapter. Nothing guesses by comparing URLs. A commit says what it did to the engine's own
//! history ([`HistoryEffect`]) when the engine knows, since an id the adapter allocated says
//! only that the page started it, not whether it added an entry or moved to another.

use std::fmt;

use crate::ids::{DocumentGeneration, NavigationId, RequestId, ViewId};
use crate::input::TextInputState;
use crate::services::ServiceRequest;

/// Something an engine reports.
#[derive(Clone, Debug, PartialEq)]
pub enum EngineEvent {
    /// The view is set up and can present, so the core may show it. It has accepted commands
    /// since [`open_view`](crate::engine::EngineRuntime::open_view) returned.
    ViewReady {
        /// The view.
        view: ViewId,
    },
    /// A navigation started. For a navigation the page started itself (a link, a script),
    /// `navigation` was allocated by the adapter, and this event is optional.
    NavigationStarted {
        /// The view.
        view: ViewId,
        /// The navigation.
        navigation: NavigationId,
        /// Where it goes, as requested (before redirects).
        url: String,
    },
    /// A navigation committed: `url` is now the view's address. A same-document navigation
    /// (fragment, history API) keeps the view's `document`; a new document gets a new one. It
    /// supersedes the view's earlier pending navigations (see the module documentation).
    NavigationCommitted {
        /// The view.
        view: ViewId,
        /// The navigation.
        navigation: NavigationId,
        /// The committed URL, after redirects.
        url: String,
        /// The document now shown.
        document: DocumentGeneration,
        /// What the commit did to the engine's session history, when the engine can tell;
        /// `None` from an engine that keeps no history of its own or doesn't know.
        history: Option<HistoryEffect>,
    },
    /// A navigation ended without committing. The previous document stays.
    NavigationFailed {
        /// The view.
        view: ViewId,
        /// The navigation.
        navigation: NavigationId,
        /// Why.
        error: NavigationError,
    },
    /// The view started or finished loading its document and subresources.
    LoadingChanged {
        /// The view.
        view: ViewId,
        /// Whether it is loading now.
        loading: bool,
    },
    /// The engine's own session history for this view changed. The engine owns that history,
    /// including same-document entries. The core stores the entries to display and save them
    /// (a restored tab reloads them by URL, losing document state), moves through them only by
    /// [`Traverse`](crate::engine::EngineCommand::Traverse) deltas, and never matches URLs to
    /// identify an entry.
    HistoryChanged {
        /// The view.
        view: ViewId,
        /// Each entry's URL, oldest first.
        entries: Vec<String>,
        /// The index of the current entry in `entries`.
        current: usize,
    },
    /// The document's title changed. `None` when it has none.
    TitleChanged {
        /// The view.
        view: ViewId,
        /// The new title.
        title: Option<String>,
    },
    /// The status text changed, such as the target of a hovered link. `None` clears it.
    StatusTextChanged {
        /// The view.
        view: ViewId,
        /// The new status text.
        text: Option<String>,
    },
    /// The pointer should look different over the view.
    CursorChanged {
        /// The view.
        view: ViewId,
        /// The new cursor.
        cursor: Cursor,
    },
    /// The focused editable element changed. `None` when nothing editable has focus, which
    /// turns the input method off.
    TextInputChanged {
        /// The view.
        view: ViewId,
        /// The editor's state.
        state: Option<TextInputState>,
    },
    /// The view has something new to paint. This says nothing about when, or whether, pixels
    /// reach the screen; submission is [`PresentOutcome`](crate::engine::PresentOutcome).
    FrameReady {
        /// The view.
        view: ViewId,
    },
    /// The view asks for a privileged service.
    ServiceRequested(ServiceRequest),
    /// The view no longer wants the result of a service request, e.g. because the page closed
    /// the dialog that asked. The browser drops it and never answers it.
    ServiceCancelled {
        /// The view.
        view: ViewId,
        /// The withdrawn request.
        request: RequestId,
    },
    /// The page asks to navigate to `url`, which the engine would load itself: a link, a form
    /// or a script of the top-level document, or of a frame from an engine that can't tell
    /// them apart (Servo 0.5, WebKit 2.52). The engine holds the navigation until the browser
    /// answers `request` with
    /// [`AnswerNavigation`](crate::engine::EngineCommand::AnswerNavigation): allowed, it goes
    /// ahead as a navigation the page started (see the module documentation); denied, it never
    /// happens, and the browser may open `url` elsewhere.
    /// When the view closes, `document` is replaced or gone, or the runtime shuts down, the
    /// engine denies whatever it still holds, and a later answer for it does nothing. An engine
    /// holds at most 16 per view, and lets the page's navigations beyond them go ahead without
    /// asking. Not replaceable: the browser answers every one.
    NavigationRequested {
        /// The view.
        view: ViewId,
        /// The document that asked.
        document: DocumentGeneration,
        /// Identifies this request in the answer.
        request: RequestId,
        /// Where it goes.
        url: String,
    },
    /// The page asks the browser to open `url`: a policy request. The core routes `url` like
    /// any other, so it may go to another engine, and decides whether to allow it; a page can
    /// never open a `sig://` page this way. Navigations the engine performs itself are asked
    /// about with `NavigationRequested` and reported as `NavigationStarted` and
    /// `NavigationCommitted` instead; this is for the ones it hands over, such as a link in a
    /// Wasm guest or a URL Servo shouldn't load.
    OpenRequested {
        /// The view that asked.
        view: ViewId,
        /// The document that asked.
        document: DocumentGeneration,
        /// What to open.
        url: String,
        /// Where.
        disposition: OpenDisposition,
    },
    /// The page asks to close its view (`window.close()`). A policy request, not a closure.
    CloseRequested {
        /// The view that asked.
        view: ViewId,
        /// The document that asked.
        document: DocumentGeneration,
    },
    /// The view is closed and its resources released. Nothing more arrives for it.
    ViewClosed {
        /// The view.
        view: ViewId,
    },
    /// The view's current document is gone: its content crashed, or a Wasm guest trapped. The
    /// view stays open and accepts `Navigate` and `Reload`; a later `NavigationCommitted` shows
    /// it recovered. A view that cannot continue at all is reported as `ViewFailed` followed by
    /// `ViewClosed`, by which time the engine has released its target. Other views of the
    /// runtime are unaffected.
    ViewFailed {
        /// The view.
        view: ViewId,
        /// What happened, for the user.
        message: String,
    },
    /// The whole runtime failed: every view it had is unusable, and it may not be possible to
    /// start it again without restarting the browser.
    RuntimeFailed {
        /// What happened, for the user.
        message: String,
    },
}

impl EngineEvent {
    /// The view the event is about, or `None` for [`EngineEvent::RuntimeFailed`].
    pub fn view(&self) -> Option<ViewId> {
        match self {
            Self::ViewReady { view }
            | Self::NavigationStarted { view, .. }
            | Self::NavigationCommitted { view, .. }
            | Self::NavigationFailed { view, .. }
            | Self::LoadingChanged { view, .. }
            | Self::HistoryChanged { view, .. }
            | Self::TitleChanged { view, .. }
            | Self::StatusTextChanged { view, .. }
            | Self::CursorChanged { view, .. }
            | Self::TextInputChanged { view, .. }
            | Self::FrameReady { view }
            | Self::ServiceCancelled { view, .. }
            | Self::NavigationRequested { view, .. }
            | Self::OpenRequested { view, .. }
            | Self::CloseRequested { view, .. }
            | Self::ViewClosed { view }
            | Self::ViewFailed { view, .. } => Some(*view),
            Self::ServiceRequested(request) => Some(request.view),
            Self::RuntimeFailed { .. } => None,
        }
    }
}

/// What a commit did to the engine's own session history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HistoryEffect {
    /// A new entry after the current one, dropping the entries that were after it: the
    /// browser's `Navigate`, a link, a form, a script navigation, a fragment, `pushState`.
    Push,
    /// The current entry, replaced: a reload, `replaceState`, `location.replace`.
    Replace,
    /// Another existing entry became the current one: back or forward, the browser's or the
    /// page's own (`history.back()`).
    Traverse,
}

/// Where a page wants a URL opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpenDisposition {
    /// In the page's own tab, replacing its document: a link, a guest's `navigate` call. The
    /// tab switches engines if the router sends the URL elsewhere.
    CurrentView,
    /// In a new tab: `target="_blank"`, `window.open`, a middle click. Allowed only shortly
    /// after the user clicked or typed in the view.
    NewView,
}

/// Why a navigation ended without committing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NavigationError {
    /// Stopped, or replaced by a newer navigation. Not an error to show.
    Cancelled,
    /// The URL could not be loaded: a network error, an unsupported scheme, content the engine
    /// cannot show. The message is for the user. This is a failed load, not a failed engine.
    Load(String),
}

impl fmt::Display for NavigationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::Load(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for NavigationError {}

/// A pointer appearance, from the CSS `cursor` keywords.
#[allow(missing_docs)] // Each variant is the CSS keyword of the same name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cursor {
    #[default]
    Default,
    None,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    EResize,
    NResize,
    NeResize,
    NwResize,
    SResize,
    SeResize,
    SwResize,
    WResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ColResize,
    RowResize,
    AllScroll,
    ZoomIn,
    ZoomOut,
}

impl Cursor {
    /// The CSS keyword. Most XDG cursor themes name their cursors the same way.
    pub fn css_name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::None => "none",
            Self::ContextMenu => "context-menu",
            Self::Help => "help",
            Self::Pointer => "pointer",
            Self::Progress => "progress",
            Self::Wait => "wait",
            Self::Cell => "cell",
            Self::Crosshair => "crosshair",
            Self::Text => "text",
            Self::VerticalText => "vertical-text",
            Self::Alias => "alias",
            Self::Copy => "copy",
            Self::Move => "move",
            Self::NoDrop => "no-drop",
            Self::NotAllowed => "not-allowed",
            Self::Grab => "grab",
            Self::Grabbing => "grabbing",
            Self::EResize => "e-resize",
            Self::NResize => "n-resize",
            Self::NeResize => "ne-resize",
            Self::NwResize => "nw-resize",
            Self::SResize => "s-resize",
            Self::SeResize => "se-resize",
            Self::SwResize => "sw-resize",
            Self::WResize => "w-resize",
            Self::EwResize => "ew-resize",
            Self::NsResize => "ns-resize",
            Self::NeswResize => "nesw-resize",
            Self::NwseResize => "nwse-resize",
            Self::ColResize => "col-resize",
            Self::RowResize => "row-resize",
            Self::AllScroll => "all-scroll",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
        }
    }
}
