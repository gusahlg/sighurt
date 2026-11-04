//! What an engine reports to the browser.
//!
//! An engine reports facts; it never changes browser state directly. Events are collected
//! while the engine runs ([`EngineRuntime::pump`](crate::engine::EngineRuntime::pump)) and
//! handled after the engine call returns, so no callback mutates the browser re-entrantly.
//!
//! Every event about a view carries its [`ViewId`], navigation events their [`NavigationId`],
//! and requests made on behalf of a document its [`DocumentGeneration`]. A site-data report
//! ([`EngineEvent::SitesListed`], [`EngineEvent::SiteDataDone`]) is about the runtime and
//! carries no view. The core drops an event whose view is closed or no longer belongs to the
//! tab, and a request whose document was replaced. Page-supplied strings (titles, status text,
//! URLs, history entries) are
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
use std::net::{Ipv4Addr, Ipv6Addr};

use crate::engine::SiteDataKinds;
use crate::ids::{DocumentGeneration, NavigationId, RequestId, ViewId};

/// The most sites one report lists, or names as removed.
pub const MAX_SITES: usize = 1024;

/// The most bytes in a site name. A longer one is dropped, never cut: cutting a host would
/// name a different site.
pub const MAX_SITE_NAME: usize = 253;

/// The most bytes of an error message kept from a site-data report. A message is text for the
/// user, not a name, so [`EngineEvent::check`] cuts it instead of refusing the report.
pub const MAX_SITE_MESSAGE: usize = 16 * 1024;
use crate::input::TextInputState;
use crate::semantic::SemanticSnapshot;
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
    /// The view's content, after [`EngineCommand::SetSemantics`](crate::engine::EngineCommand)
    /// turned it on: a whole snapshot that replaces the last ([`crate::semantic`]). Page data the
    /// core never reads; the runtime hands it to the frontend after checking it.
    SemanticsChanged {
        /// The view.
        view: ViewId,
        /// The content, stamped with the document it came from.
        snapshot: Box<SemanticSnapshot>,
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
    /// The sites that have data, answering [`crate::engine::SiteDataRequest::List`]. At most
    /// [`MAX_SITES`] entries; `more` is set when the engine has further sites it did not
    /// include. Not replaceable: the browser keeps each answer. A name that is not a host, or
    /// longer than [`MAX_SITE_NAME`] bytes, is dropped by [`EngineEvent::check`], never
    /// shortened.
    SitesListed {
        /// The request this answers.
        request: RequestId,
        /// The sites, after [`EngineEvent::check`].
        sites: Vec<SiteEntry>,
        /// Whether the engine has more sites than `sites` holds.
        more: bool,
    },
    /// What the view's search found, after a [`FindCommand`](crate::engine::FindCommand)
    /// `Search`, `Next` or `Previous`. Replaceable: only the latest counts. `current` and
    /// `total` are 1-based when the engine knows them: WebKit reports only `total`, NetSurf
    /// only `found`. Numbers only, so nothing to contain. The core drops it when `search` is
    /// not the view's current search or `document` is no longer its document.
    FindResult {
        /// The view.
        view: ViewId,
        /// The document searched.
        document: DocumentGeneration,
        /// The search, as its `Search` named it.
        search: RequestId,
        /// Whether there is a match.
        found: bool,
        /// Which match is selected, from 1, when the engine knows.
        current: Option<u32>,
        /// How many matches there are, when the engine knows.
        total: Option<u32>,
    },
    /// The document entered or left element fullscreen inside the view (a video, a game): a
    /// fact, not a request. The browser answers with its policy: the window's own state, or
    /// [`ExitFullscreen`](crate::engine::EngineCommand::ExitFullscreen). Ordered, not
    /// replaceable: entering and leaving both matter.
    FullscreenChanged {
        /// The view.
        view: ViewId,
        /// The document whose element it is.
        document: DocumentGeneration,
        /// Whether an element is fullscreen now.
        fullscreen: bool,
    },
    /// [`crate::engine::SiteDataRequest::Forget`] or [`ForgetAll`](crate::engine::SiteDataRequest::ForgetAll)
    /// finished. Not replaceable. [`EngineEvent::check`] drops an empty `removed` name or one
    /// longer than [`MAX_SITE_NAME`] bytes, and cuts an error message to [`MAX_SITE_MESSAGE`].
    SiteDataDone {
        /// The request this finishes.
        request: RequestId,
        /// What was removed, or why the forget failed. `Ok` with `profile` false means this
        /// instance has no profile, so nothing on disk changed.
        result: Result<SiteDataOutcome, String>,
    },
}

impl EngineEvent {
    /// The view the event is about, or `None` for [`EngineEvent::RuntimeFailed`] and the
    /// site-data reports ([`EngineEvent::SitesListed`], [`EngineEvent::SiteDataDone`]).
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
            | Self::SemanticsChanged { view, .. }
            | Self::ServiceCancelled { view, .. }
            | Self::NavigationRequested { view, .. }
            | Self::OpenRequested { view, .. }
            | Self::CloseRequested { view, .. }
            | Self::ViewClosed { view }
            | Self::ViewFailed { view, .. }
            | Self::FindResult { view, .. }
            | Self::FullscreenChanged { view, .. } => Some(*view),
            Self::ServiceRequested(request) => Some(request.view),
            Self::RuntimeFailed { .. } | Self::SitesListed { .. } | Self::SiteDataDone { .. } => {
                None
            }
        }
    }

    /// Drops site names that are not usable, and rejects a report that is still over
    /// [`MAX_SITES`]. Other events are unchanged.
    ///
    /// A [`SitesListed`](Self::SitesListed) name that is not a canonical host (lower-case
    /// ASCII, an IPv4 address as `Ipv4Addr` displays it, or an IPv6 address in the brackets
    /// `Ipv6Addr` displays), or longer than [`MAX_SITE_NAME`] bytes, is removed. It is never
    /// rewritten or cut. A [`SiteDataDone`](Self::SiteDataDone) `removed` name is the engine's
    /// own record, so one that is not a host stays (`"Local files"`); only an empty name or
    /// one over [`MAX_SITE_NAME`] bytes is removed. An error message longer than
    /// [`MAX_SITE_MESSAGE`] is cut, since it is text for the user. Checking again changes
    /// nothing.
    pub fn check(&mut self) -> Result<(), SiteDataError> {
        match self {
            Self::SitesListed { sites, .. } => {
                sites.retain(|entry| is_site_host(&entry.name));
                if sites.len() > MAX_SITES {
                    return Err(SiteDataError::TooMany(sites.len()));
                }
            }
            Self::SiteDataDone { result, .. } => match result {
                Ok(outcome) => {
                    outcome
                        .removed
                        .retain(|name| !name.is_empty() && name.len() <= MAX_SITE_NAME);
                    if outcome.removed.len() > MAX_SITES {
                        return Err(SiteDataError::TooMany(outcome.removed.len()));
                    }
                }
                Err(message) => cut_message(message),
            },
            _ => {}
        }
        Ok(())
    }
}

/// One site an engine listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteEntry {
    /// The site's name: a host, as [`EngineEvent::check`] accepts it.
    pub name: String,
    /// Which kinds of data it has.
    pub kinds: SiteDataKinds,
}

/// What a forget removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteDataOutcome {
    /// The engine's own records that were removed. Not necessarily hosts (`"Local files"`).
    /// Empty, or longer than [`MAX_SITE_NAME`] bytes, is dropped by [`EngineEvent::check`].
    pub removed: Vec<String>,
    /// Which kinds this operation covered. Anything else was not touched.
    pub kinds: SiteDataKinds,
    /// Whether the deletion is committed where the engine keeps it. `false` when it is only in
    /// memory, or not committed yet.
    pub durable: bool,
    /// Whether this instance has a profile. `false` means nothing on disk changed.
    pub profile: bool,
}

/// Why a site-data report is not usable. Names that fail the rules are dropped first; this is
/// what is still wrong afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SiteDataError {
    /// More than [`MAX_SITES`] names remain.
    TooMany(usize),
}

impl fmt::Display for SiteDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooMany(n) => write!(f, "{n} sites (at most {MAX_SITES})"),
        }
    }
}

impl std::error::Error for SiteDataError {}

/// `name` is a host in canonical form: nothing is rewritten, so `Example.COM`, a trailing dot,
/// an expanded IPv6 address and a Unicode label are not.
fn is_site_host(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_SITE_NAME {
        return false;
    }
    if let Some(inner) = name
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return inner
            .parse::<Ipv6Addr>()
            .is_ok_and(|addr| format!("[{addr}]") == name);
    }
    if let Ok(addr) = name.parse::<Ipv4Addr>() {
        return addr.to_string() == name;
    }
    let mut any = false;
    let mut last_numeric = false;
    for label in name.split('.') {
        any = true;
        let bytes = label.as_bytes();
        if bytes.is_empty() || bytes.len() > 63 {
            return false;
        }
        if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
            return false;
        }
        if !bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        {
            return false;
        }
        last_numeric = bytes.iter().all(|b| b.is_ascii_digit());
    }
    // A final numeric label is an IPv4 address to the URL host parser, and `name` is not one.
    any && !last_numeric
}

/// Cuts `message` to [`MAX_SITE_MESSAGE`] bytes, on a character boundary.
fn cut_message(message: &mut String) {
    if message.len() <= MAX_SITE_MESSAGE {
        return;
    }
    let mut end = MAX_SITE_MESSAGE;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
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

#[cfg(test)]
mod tests {
    use crate::ids::RequestId;

    use super::*;

    fn listed(names: &[&str]) -> EngineEvent {
        EngineEvent::SitesListed {
            request: RequestId::next(),
            sites: names
                .iter()
                .map(|name| SiteEntry {
                    name: (*name).into(),
                    kinds: SiteDataKinds::NONE,
                })
                .collect(),
            more: false,
        }
    }

    fn done(removed: Vec<String>) -> EngineEvent {
        EngineEvent::SiteDataDone {
            request: RequestId::next(),
            result: Ok(SiteDataOutcome {
                removed,
                kinds: SiteDataKinds::NONE,
                durable: false,
                profile: true,
            }),
        }
    }

    fn names(event: &EngineEvent) -> Vec<&str> {
        match event {
            EngineEvent::SitesListed { sites, .. } => {
                sites.iter().map(|site| site.name.as_str()).collect()
            }
            EngineEvent::SiteDataDone {
                result: Ok(outcome),
                ..
            } => outcome.removed.iter().map(String::as_str).collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn site_reports_have_no_view_and_drop_unusable_names() {
        let mut listed = listed(&[
            "example.com",
            "localhost",
            "[::1]",
            "127.0.0.1",
            "xn--bcher-kva.example",
            "demo",
            "NOT_A_HOST",
            "Example.COM",
            "*.example.com",
            "example.com.",
            "bücher.example",
            "[0:0:0:0:0:0:0:1]",
            "127.0.0.01",
            "123",
            &"a".repeat(254),
        ]);
        assert_eq!(listed.view(), None);
        assert_eq!(listed.check(), Ok(()));
        assert_eq!(
            names(&listed),
            [
                "example.com",
                "localhost",
                "[::1]",
                "127.0.0.1",
                "xn--bcher-kva.example",
                "demo",
            ]
        );
        assert_eq!(listed.check(), Ok(()));
        assert_eq!(names(&listed).len(), 6, "a second check drops nothing more");

        let mut removed = done(vec![
            "example.com".into(),
            "Local files".into(),
            String::new(),
            "a".repeat(254),
        ]);
        assert_eq!(removed.view(), None);
        assert_eq!(removed.check(), Ok(()));
        assert_eq!(names(&removed), ["example.com", "Local files"]);
        assert_eq!(removed.check(), Ok(()));
    }

    #[test]
    fn more_than_1024_names_is_refused_and_a_long_message_is_cut() {
        let mut sites = listed(&vec!["example.com"; MAX_SITES]);
        assert_eq!(sites.check(), Ok(()));
        let mut over = listed(&vec!["example.com"; MAX_SITES + 1]);
        assert!(matches!(over.check(), Err(SiteDataError::TooMany(n)) if n == MAX_SITES + 1));

        let mut mixed = listed(&["example.com"; MAX_SITES]);
        if let EngineEvent::SitesListed { sites, .. } = &mut mixed {
            sites.push(SiteEntry {
                name: "NOT A HOST".into(),
                kinds: SiteDataKinds::NONE,
            });
        }
        assert_eq!(mixed.check(), Ok(()));
        assert_eq!(names(&mixed).len(), MAX_SITES);

        let mut removed = done(vec!["x".into(); MAX_SITES + 1]);
        assert!(matches!(
            removed.check(),
            Err(SiteDataError::TooMany(n)) if n == MAX_SITES + 1
        ));

        let mut failed = EngineEvent::SiteDataDone {
            request: RequestId::next(),
            result: Err("e".repeat(MAX_SITE_MESSAGE + 32)),
        };
        assert_eq!(failed.check(), Ok(()));
        match &failed {
            EngineEvent::SiteDataDone {
                result: Err(message),
                ..
            } => assert_eq!(message.len(), MAX_SITE_MESSAGE),
            _ => unreachable!(),
        }
        assert_eq!(failed.check(), Ok(()));
    }

    #[test]
    fn a_host_of_253_bytes_stays_and_a_longer_label_does_not() {
        let label = "a".repeat(63);
        let name = format!("{label}.{label}.{label}.{}", "b".repeat(61));
        assert_eq!(name.len(), 253);
        let mut event = listed(&[&name, &"a".repeat(64)]);
        assert_eq!(event.check(), Ok(()));
        assert_eq!(names(&event), [name.as_str()]);
    }
}
