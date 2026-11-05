//! The core's entry point: [`BrowserState`], the [`BrowserEvent`]s it accepts and the
//! [`Effect`]s [`update`] returns.
//!
//! `update` applies one event and returns what the executable should do about it. It does no
//! I/O and reads no clock: an event that needs the time carries it. Every change of browser
//! state therefore goes through one function and can be replayed in a test.
//!
//! # Driving it
//!
//! The executable owns one `BrowserState`. It sends [`BrowserEvent::Start`] once, then:
//!
//! - every command from the chrome as [`BrowserEvent::Command`], a click on a tab or its close
//!   button as [`BrowserEvent::SelectTab`] or [`BrowserEvent::CloseTab`], and focus the chrome
//!   moves itself (a click in the address bar, Escape) as [`BrowserEvent::Focus`];
//! - every keyboard event as [`BrowserEvent::Keyboard`], every pointer event on a page target
//!   as [`BrowserEvent::Pointer`], and the mouse's side buttons wherever the pointer is as
//!   [`BrowserEvent::Button`]. The core asks its keymap ([`crate::keys`]) about key presses and
//!   `[input.buttons]` about side buttons, and routes the rest to the page or, as
//!   [`Effect::ChromeInput`], to the chrome;
//! - every event of every engine pump as [`BrowserEvent::Engine`], in order. The core drops the
//!   ones about views it has closed or replaced and requests from replaced documents, and
//!   answers every navigation a view it knows asks about (`NavigationRequested`): a link the
//!   user clicked goes where routing sends its address, perhaps to another engine.
//!   `SiteDataDone` finishes a pending `site.forget`: a notice, and a reload of the tabs in
//!   that engine whose host matches. `RuntimeFailed` finishes the ones that engine was asked,
//!   as failed, and a failed engine is asked nothing more. `SitesListed` is ignored until a command lists sites.
//!   `FrameReady` is the executable's to schedule presentation from, and `SemanticsChanged`
//!   page content for a frontend that shows text; the core ignores both;
//! - what the window became when it went in or out of fullscreen, as
//!   [`BrowserEvent::Fullscreen`] (it is asked with [`Effect::SetFullscreen`]; see
//!   [`crate::fullscreen`]);
//! - what its own calls and services produced: [`BrowserEvent::OpenFailed`],
//!   [`BrowserEvent::CommandFailed`], [`BrowserEvent::FindResult`], [`BrowserEvent::ServiceDone`],
//!   [`BrowserEvent::PermissionAnswered`], [`BrowserEvent::ConfigReloaded`],
//!   [`BrowserEvent::VisitsForgotten`].
//!
//! # Visited pages
//!
//! The core decides which pages are history ([`Effect::Visited`]); the executable keeps them,
//! with the time, in its history file. A visit is a commit of a navigation that pushes a new
//! document (not a reload, a traversal, or a same-document `pushState`, `replaceState` or
//! fragment) in the view the active tab shows or is switching to (not one restoring a saved
//! page, the session's or one back or forward), of an http or https address
//! with no user name or password, at most [`MAX_VISIT_URL`] bytes once its fragment is
//! dropped, in an engine that did not start with `storage = "ephemeral"`, while `[history]
//! remember` is on. Its title is the title the same view reports next for that document: the
//! view remembers which visit its document is, and any other commit, a forget or turning
//! `remember` off ends that. `history.clear` and `site.forget` forget visits
//! ([`Effect::ForgetVisits`]) whatever `remember` says; the executable answers each with
//! [`BrowserEvent::VisitsForgotten`], which becomes a notice (not for a site's forget that
//! found no visit: the engines' answers speak for the site).
//!
//! It carries out the returned effects in order: an [`Effect::OpenView`] comes before the
//! commands for its view, and a view is sent nothing after its [`Effect::CloseView`]. After any
//! update, the chrome draws from [`BrowserState::tabs`], [`BrowserState::focus`] and
//! [`BrowserState::keymap_indicator`].
//!
//! Must not own: I/O, timers, threads, surface metrics or anything the chrome draws. It is not a
//! framework: no event bus, no generic UI tree, no plugin registry.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use sighurt_api::commands::{Command, SentKey};
use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, FindCommand, SiteDataKinds, SiteDataRequest,
    SiteKey, ViewOptions,
};
use sighurt_api::events::{
    Cursor, EngineEvent, HistoryEffect, NavigationError, OpenDisposition, SiteDataOutcome,
};
use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, SeatId, TabId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, Key, KeyEvent, KeyboardFocus, PointerButton,
    PointerEvent, ScrollDelta, TextInputState,
};
use sighurt_api::keymap::{Browser, KeyContext, KeymapKind, TabInfo};
use sighurt_api::semantic::SemanticAction;
use sighurt_api::services::{
    Permission, Service, ServiceError, ServiceReply, ServiceRequest, ServiceResult,
};
use sighurt_api::settings::Table;
use url::Url;

use crate::config::{Config, ConfigError, EngineSettings, Storage};
use crate::fullscreen::Fullscreen;
use crate::keys::{KeyOutcome, KeyState};
use crate::navigation::{
    NavigationKind, Record, Segment, Switch, SwitchInto, MAX_SEGMENTS, RETAINED_VIEWS,
};
use crate::permissions::{self, Pending, Requests, Requirement, MAX_PENDING_REQUESTS};
use crate::router::{self, Route};
use crate::session::{self, Session, SessionSegment, SessionTab};
use crate::tabs::{
    bounded, clamp, display_title, link, ClosedTab, Content, LoadError, Tab, TabSummary, View,
    MAX_CLOSED, MAX_TEXT, MAX_URL,
};

/// Which frontend is running. The same enum a keymap sees
/// ([`sighurt_api::keymap::Frontend`]): [`crate::config::Config::parse`] checks
/// `chrome.<action>` against that frontend's chrome.
pub use sighurt_api::keymap::Frontend;

/// What the executable tells the core about the world it runs in: what the build has, which
/// the configuration is checked against ([`Config::parse`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    /// The engines compiled into this build, by name ("servo", "wasm", "demo"), with what each
    /// supports (`EngineRuntime::capabilities`). An engine that can't traverse its own history
    /// gets a view per page, each page a segment of the tab's history; one without page zoom is
    /// never sent `SetZoom`.
    pub engines: BTreeMap<String, EngineCapabilities>,
    /// The web engines, in the build's descriptor order: engines that open http(s) pages.
    /// `page.switch-engine` cycles the ones compiled in and enabled. Empty when the build
    /// names none.
    pub web_engines: Vec<String>,
    /// Which settings of `[engines.<engine>]` each engine of the build honors, by engine
    /// ("user-agent", "storage"). An engine left out honors none.
    pub engine_settings: BTreeMap<String, &'static [&'static str]>,
    /// The user's home directory, for `~/` in typed addresses and `[downloads] directory`.
    pub home_dir: Option<PathBuf>,
    /// The build's keymaps besides `bindings` ([`crate::keys::KIND`]), which the core always
    /// has.
    pub keymaps: Vec<KeymapKind>,
    /// The build's chromes, as the core checks them.
    pub chromes: Vec<ChromeChecks>,
    /// The color names `[ui.colors]` takes: the chrome's (`sighurt_ui::Colors::NAMES`).
    pub color_names: &'static [&'static str],
    /// The names `[ui] renderer` takes: every renderer of the project
    /// ([`crate::config::RENDERERS`]), the same in both frontends and `--dump`, so one file
    /// serves every build. A window built without the one named uses `auto` and says so.
    pub renderers: &'static [&'static str],
    /// Which frontend is running. [`Frontend::Gui`] when left out, which keeps the window's
    /// chrome checks.
    pub frontend: Frontend,
    /// The engine this run starts with, overriding `[engines] default`. [`Config::parse`]
    /// applies it on top of every file it reads, so a reload keeps it. `None` uses the
    /// process-wide choice from [`crate::config::set_starting_engine`], if one was set.
    pub starting_engine: Option<String>,
}

/// What the core needs of a chrome of the build: its name, the check of its
/// `[chrome.<name>]` table, and its `chrome.<action>` commands (`sighurt_ui`'s `ChromeKind`
/// without the chrome itself). Compared and printed by its name.
#[derive(Clone, Copy)]
pub struct ChromeChecks {
    /// Its name in `[ui] chrome`.
    pub name: &'static str,
    /// Everything wrong with its table, one line each; empty if the table is valid.
    pub check: fn(&Table) -> Vec<String>,
    /// The actions it takes as `chrome.<action>` commands.
    pub commands: &'static [&'static str],
}

impl std::fmt::Debug for ChromeChecks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChromeChecks({})", self.name)
    }
}

impl PartialEq for ChromeChecks {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for ChromeChecks {}

/// Something to open in a new tab: a command-line argument, or a URL from the desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    /// A URL, or what the user typed. The executable turns paths that exist into `file:` URLs
    /// first ([`router::file_url`]).
    pub input: String,
    /// The engine the user chose, if they chose one.
    pub engine: Option<String>,
}

/// Everything that can change browser state.
#[derive(Clone, Debug, PartialEq)]
pub enum BrowserEvent {
    /// The browser starts: restore `session`, then open each of `open` in a new tab and select
    /// the first; open a home tab if that leaves no tab.
    Start {
        /// The saved session, if it could be read; what reading it left out
        /// ([`Session::notices`]) is told the user. A damaged one is refused with a notice.
        session: Option<Session>,
        /// What the command line asked for.
        open: Vec<OpenRequest>,
    },
    /// A command from the chrome: a button, the address bar.
    Command(Command),
    /// The chrome selects this tab (a click on it), as `tab.select` would. A tab that is gone
    /// is not selected.
    SelectTab(TabId),
    /// The chrome closes this tab (its close button), as `tab.close` would. A tab that is gone
    /// is not closed.
    CloseTab(TabId),
    /// Open something in a new tab and select it; with [`session::MAX_TABS`] tabs open, say so
    /// instead.
    Open(OpenRequest),
    /// Keyboard input for the window: a key, input-method text, or window activation.
    Keyboard {
        /// The event.
        event: InputEvent,
        /// When it happened.
        at: Instant,
    },
    /// Pointer input on a page view's target, in its surface-local coordinates.
    Pointer {
        /// The view whose target it is.
        view: ViewId,
        /// The event.
        event: InputEvent,
        /// When it happened.
        at: Instant,
    },
    /// The frontend ends a button press it captured in `view` (the view hid, the mode changed,
    /// focus left): `event` is a `PointerEvent::Button` release. The one pointer event a view
    /// that is no longer shown still gets, so its page does not keep a button pressed; any
    /// other pointer event, a press, or a side button is dropped.
    PointerRelease {
        /// The view that had the press.
        view: ViewId,
        /// The event.
        event: InputEvent,
        /// When it happened.
        at: Instant,
    },
    /// A side button of the mouse (`PointerButton::Back` or `Forward`) went down or up, over
    /// `view`'s target or over the chrome (`None`). `[input.buttons]` binds them to commands;
    /// an unbound one goes to the view shown, like any button. A side button that comes as
    /// [`BrowserEvent::Pointer`] is handled the same way.
    Button {
        /// The view whose target it is over, if any.
        view: Option<ViewId>,
        /// The event: a `PointerEvent::Button`.
        event: InputEvent,
        /// When it happened.
        at: Instant,
    },
    /// The chrome moved the keyboard focus itself.
    Focus(KeyboardFocus),
    /// Whether typing the keymap leaves for the page reaches the page's text fields. A frontend
    /// that takes the page's keys itself ([`Effect::PageTextInput`] would mislead it) sends
    /// `false`: a field the page focuses then keeps no character binding from running, and
    /// `Effect::PageTextInput` is `None`. `true` at start; the window never sends it.
    PageTyping(bool),
    /// The window is fullscreen now, or no longer: what the compositor made it, whether the
    /// core asked for it ([`Effect::SetFullscreen`]) or not. Sent at least whenever it changes;
    /// an unchanged state does nothing. The terminal never sends it.
    Fullscreen(bool),
    /// Something an engine reported.
    Engine {
        /// The engine whose pump reported it.
        engine: String,
        /// What it reported.
        event: EngineEvent,
        /// When it was pumped.
        at: Instant,
    },
    /// `EngineRuntime::open_view` refused to open a view. No events will come for it.
    OpenFailed {
        /// The view.
        view: ViewId,
        /// Why.
        error: EngineError,
    },
    /// `EngineRuntime::command` refused a command.
    CommandFailed {
        /// The view it was for.
        view: ViewId,
        /// The command.
        command: EngineCommand,
        /// Why.
        error: EngineError,
    },
    /// A frontend that shows a page as text acts on a node of it (TUI.md §3, §8.4): the user
    /// chose `action` in the snapshot of `document` that the frontend showed. Dropped unless
    /// `view` is the view shown and `document` still its document. An action the user's input
    /// caused ([`SemanticAction::is_activation`]) counts as a click in the page, over `link`
    /// when it activates a link: the address of the node in that snapshot.
    Semantic {
        /// The view.
        view: ViewId,
        /// The document of the snapshot the action was chosen in.
        document: DocumentGeneration,
        /// The action.
        action: SemanticAction,
        /// The link the action activates, if any: its node's address in the snapshot.
        link: Option<String>,
        /// When the user acted.
        at: Instant,
    },
    /// What a search ([`Effect::Find`]) found, from whoever searched: a frontend that shows the
    /// page as text and searched it itself ([`Effect::Find`]), or the runtime saying that
    /// nobody can (`supported: false`: the frontend declined and the view's engine lacks
    /// [`EngineCapabilities::find_in_page`]). An engine's own result comes as
    /// [`EngineEvent::FindResult`] and counts the same. Dropped unless `search` is the view's
    /// search: ids are never reused and the search ends with its document, so `search` alone
    /// tells a late result.
    FindResult {
        /// The view.
        view: ViewId,
        /// The search, as [`FindCommand::Search`] named it.
        search: RequestId,
        /// Whether there is a match.
        found: bool,
        /// Which match is selected, from 1, when known.
        current: Option<u32>,
        /// How many matches there are, when known.
        total: Option<u32>,
        /// Whether anything could search the view at all.
        supported: bool,
    },
    /// A service the core asked for ([`Effect::Service`]) is done, or failed.
    ServiceDone {
        /// The request.
        request: RequestId,
        /// The answer for the page.
        result: ServiceResult,
    },
    /// The user answered a permission prompt ([`Effect::Prompt`]).
    PermissionAnswered {
        /// The request the prompt was for.
        request: RequestId,
        /// Whether the user allowed it.
        allow: bool,
    },
    /// The configuration file was read again and parsed ([`Config::parse`]). An error keeps
    /// the configuration in use.
    ConfigReloaded(Result<Box<Config>, ConfigError>),
    /// The executable forgot the visits [`Effect::ForgetVisits`] with `request` named.
    VisitsForgotten {
        /// The request.
        request: RequestId,
        /// What was forgotten, and where.
        outcome: ForgetOutcome,
    },
}

/// What forgetting visited pages did ([`BrowserEvent::VisitsForgotten`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgetOutcome {
    /// `removed` visits are gone, and the history file no longer holds them: it was written and
    /// synced, or this `sig` keeps no history file (no state directory).
    Done {
        /// How many visits were forgotten.
        removed: usize,
    },
    /// `removed` visits are gone from this `sig` only: another `sig` owns the history file,
    /// which still holds them.
    HereOnly {
        /// How many visits were forgotten.
        removed: usize,
    },
    /// `removed` visits are gone from this `sig`, but writing the history file failed, so it
    /// may still hold them.
    NotWritten {
        /// How many visits were forgotten.
        removed: usize,
        /// Why the file was not written, for the user.
        error: String,
    },
}

/// What the executable should do, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Create a hidden target for `view` in the page area and open the view on `engine`
    /// (`EngineRuntime::open_view`). Report a refusal as [`BrowserEvent::OpenFailed`].
    OpenView {
        /// The new view.
        view: ViewId,
        /// The engine to open it on.
        engine: String,
        /// How it starts.
        options: ViewOptions,
    },
    /// Hand `command` to `view`'s engine (`EngineRuntime::command`), and report a refusal as
    /// [`BrowserEvent::CommandFailed`]. `SetVisibility` also maps or unmaps the view's target:
    /// only the view shown in the page area is mapped.
    Engine {
        /// The view.
        view: ViewId,
        /// The command.
        command: EngineCommand,
    },
    /// Find in `view`'s page: offer `command` to the frontend first (one that shows the page
    /// as text searches its own text), then to the view's engine as [`EngineCommand::Find`] if
    /// it has [`EngineCapabilities::find_in_page`]; with neither, answer a `Search`, `Next` or
    /// `Previous` with [`BrowserEvent::FindResult`] and `supported: false`. Results come back as
    /// [`BrowserEvent::FindResult`] or [`EngineEvent::FindResult`]; a refusal of the engine
    /// command as [`BrowserEvent::CommandFailed`].
    Find {
        /// The view.
        view: ViewId,
        /// What to do.
        command: FindCommand,
    },
    /// The chrome opens its find field (`page.find` without text; `Focus(Chrome)` comes first),
    /// holding `text`, the search the page shown has, if any. Each edit there runs `page.find
    /// <text>`, an empty one `page.find-stop`; Enter keeps the search and gives the page the
    /// keyboard, Escape ends it. A frontend with no find field ignores it.
    FindField(String),
    /// Close `view` (`EngineRuntime::close_view`) and retire its target once the engine is
    /// done with it. The core sends it nothing more.
    CloseView {
        /// The view.
        view: ViewId,
    },
    /// The keyboard focus moves. `Chrome`: the chrome focuses its address line and selects its
    /// text. `Page`: the chrome leaves the address line, discarding an edit.
    Focus(KeyboardFocus),
    /// The address line, just focused (`Focus(Chrome)` comes first), holds this text instead of
    /// the address, the caret at its end, as if it had been typed: `location.focus <text>`.
    LineText(String),
    /// Keyboard input for the chrome: the address bar has focus, or the page area shows no
    /// engine view.
    ChromeInput(InputEvent),
    /// Run the chrome's own command `chrome.<action>` (holds the action): hand it to the chrome,
    /// or say that the chrome running has no such command (it is checked against the chrome
    /// the configuration names, which takes effect at the next start).
    ChromeCommand(String),
    /// The pointer's shape over the page area.
    PageCursor(Cursor),
    /// The text field of the focused page, for the host's input method; `None` when it has
    /// none or the page doesn't have the keyboard.
    PageTextInput(Option<TextInputState>),
    /// Perform `service` for `view`, then report [`BrowserEvent::ServiceDone`].
    Service {
        /// The request, to report the answer with.
        request: RequestId,
        /// The view that asked.
        view: ViewId,
        /// What it asked for; allowed.
        service: Service,
    },
    /// Stop working on `request` and don't report it: its page withdrew it or is gone. Dismiss
    /// its prompt if one shows.
    CancelService(RequestId),
    /// Ask the user whether the page in `tab` may use `permission`, and report the answer with
    /// [`BrowserEvent::PermissionAnswered`].
    Prompt {
        /// The request.
        request: RequestId,
        /// The tab that asks.
        tab: TabId,
        /// Who asks, as its engine defines it.
        origin: Option<String>,
        /// What it asks for.
        permission: Permission,
    },
    /// List or forget site data ([`EngineRuntime::site_data`](sighurt_api::engine::EngineRuntime::site_data)).
    /// `engine` is `None` to ask every engine that keeps any kind, or one engine's name. The
    /// runtime starts an engine that has not started. The outcome comes back as
    /// [`EngineEvent::SiteDataDone`] or [`EngineEvent::SitesListed`].
    SiteData {
        /// One engine, or every engine that declares a kind.
        engine: Option<String>,
        /// What to do.
        request: SiteDataRequest,
    },
    /// Put `text` on the clipboard (`clipboard.copy`). The runtime carries it out and
    /// refuses text over its own limit.
    ClipboardWrite(String),
    /// Tell the user something: a blocked pop-up, a configuration error or notice.
    Notice {
        /// The tab it is about, if any.
        tab: Option<TabId>,
        /// What to say.
        message: String,
    },
    /// Ask for the window to be fullscreen, or not. The compositor decides: report what the
    /// window became as [`BrowserEvent::Fullscreen`], and assume nothing until then. Only the
    /// window gets it.
    SetFullscreen(bool),
    /// Something the chrome shows changed: draw it again.
    RedrawChrome,
    /// The session changed: save [`BrowserState::session`], atomically. Waiting a moment and
    /// saving only the latest is fine, except right before [`Effect::Quit`].
    SaveSession,
    /// Record a visit to `url` in the history, at the time this is carried out (`commit`), or
    /// give the visit already recorded for `url` this `title` (not `commit`: the title of the
    /// document whose commit was the visit; a visit no longer there stays gone). `url` is an
    /// http or https address without a fragment or user name, at most [`MAX_VISIT_URL`] bytes;
    /// a title is at most 16 KiB, page text to contain before it is kept. Comes only while
    /// `[history] remember` is on (see the module documentation).
    Visited {
        /// The address.
        url: String,
        /// The page's title; `None` at the commit, before the page has said.
        title: Option<String>,
        /// A commit: a new visit. Otherwise a title for the visit of `url`.
        commit: bool,
    },
    /// Forget the visits to `host` and the hosts under it, or every visit, in memory and in the
    /// history file at once (also with `[history] remember` off), dropping a save not written
    /// yet; then report [`BrowserEvent::VisitsForgotten`] with `request`.
    ForgetVisits {
        /// The request, to report the outcome with.
        request: RequestId,
        /// The host whose visits go (`www.example.com` goes with `example.com`), or `None` for
        /// all of them.
        host: Option<String>,
    },
    /// Quit: save the session now, shut every runtime down, then the host.
    Quit,
}

/// The effects of one update, and what it changed.
#[derive(Default)]
pub(crate) struct Fx {
    effects: Vec<Effect>,
    /// Something the chrome shows changed.
    pub(crate) chrome: bool,
    /// Something the session saves changed.
    pub(crate) session: bool,
    pub(crate) quit: bool,
}

impl Fx {
    pub(crate) fn push(&mut self, effect: Effect) {
        self.effects.push(effect);
    }
}

/// Who asks for a navigation: the user, with the engine they chose if they chose one, or the
/// page the tab shows.
#[derive(Clone, Copy, Debug)]
pub(crate) enum By<'a> {
    User(Option<&'a str>),
    Page,
}

/// What the core asks a view to load.
enum Request {
    Navigate(String),
    Reload,
    Traverse(i32),
}

/// The browser: its tabs, their views and history, and the policy that governs them.
#[derive(Debug)]
pub struct BrowserState {
    pub(crate) config: Config,
    env: Environment,
    pub(crate) tabs: Vec<Tab>,
    /// The index of the active tab.
    pub(crate) active: usize,
    focus: KeyboardFocus,
    /// Every view the core has opened and not seen closed.
    pub(crate) views: BTreeMap<ViewId, View>,
    /// The keymap and the presses it took.
    keys: KeyState,
    /// The side buttons `[input.buttons]` took, whose releases the core owes nobody.
    buttons: HashSet<(SeatId, PointerButton)>,
    /// The seat whose keyboard the core heard last: the one `page.send-key` presses keys on.
    seat: SeatId,
    /// What the keymap said about itself after the last update.
    indicator: Option<String>,
    /// The chrome `sig` runs: the configuration's when it started.
    chrome: String,
    /// `[ui] renderer` when `sig` started: what the window chose its renderer by.
    renderer: String,
    /// The settings each engine of the build starts with: the configuration's when `sig`
    /// started.
    launched: BTreeMap<String, EngineSettings>,
    requests: Requests,
    /// Engines whose runtime failed, and why. They open nothing more.
    failed: BTreeMap<String, String>,
    /// `site.forget` requests still waiting for engines to answer. At most
    /// [`MAX_SITE_REQUESTS`].
    site_requests: BTreeMap<RequestId, SiteForget>,
    /// Forgets of visited pages the executable has not answered, with the host each names. At
    /// most [`MAX_VISIT_REQUESTS`].
    visit_requests: BTreeMap<RequestId, Option<String>>,
    /// What the executable was last told the page area looks like.
    cursor: Cursor,
    text_input: Option<TextInputState>,
    /// [`BrowserEvent::PageTyping`].
    page_typing: bool,
    /// The tabs closed since the browser started, the latest last: what `tab.reopen` restores.
    /// At most [`MAX_CLOSED`]; never saved.
    pub(crate) closed: VecDeque<ClosedTab>,
    /// The window's fullscreen and the page's ([`crate::fullscreen`]).
    pub(crate) fullscreen: Fullscreen,
    /// Whether the window has the keyboard: until the frontend says it lost it, it has.
    window_focused: bool,
}

/// Applies `event` to `state` and returns what the executable should do.
pub fn update(state: &mut BrowserState, event: BrowserEvent) -> Vec<Effect> {
    let mut fx = Fx::default();
    match event {
        BrowserEvent::Start { session, open } => state.start(session, open, &mut fx),
        BrowserEvent::Command(command) => state.run_command(command, &mut fx),
        BrowserEvent::SelectTab(tab) => state.tab_command(tab, Command::TabSelect, &mut fx),
        BrowserEvent::CloseTab(tab) => {
            state.tab_command(tab, |n| Command::TabClose(Some(n)), &mut fx)
        }
        BrowserEvent::Open(request) => {
            if let Some(tab) = state.open_tab(request, false, &mut fx) {
                state.active = tab;
                state.move_focus(KeyboardFocus::Page, &mut fx);
            }
        }
        BrowserEvent::Keyboard { event, at } => state.keyboard(event, at, &mut fx),
        BrowserEvent::Pointer { view, event, at } => state.pointer(view, event, at, &mut fx),
        BrowserEvent::Button { view, event, at } => state.button(view, event, at, &mut fx),
        BrowserEvent::PointerRelease { view, event, .. } => {
            let release = matches!(
                &event.kind,
                InputKind::Pointer(PointerEvent::Button {
                    button: PointerButton::Left | PointerButton::Right | PointerButton::Middle,
                    state: ButtonState::Released,
                    ..
                })
            );
            // A view being closed is sent nothing after its `CloseView`.
            let open = state.views.get(&view).is_some_and(|v| !v.closing);
            if release && open {
                fx.push(Effect::Engine {
                    view,
                    command: EngineCommand::Input(event),
                });
            }
        }
        BrowserEvent::Semantic {
            view,
            document,
            action,
            link,
            at,
        } => state.semantic(view, document, action, link, at, &mut fx),
        BrowserEvent::Focus(focus) => {
            state.focus = focus;
            fx.chrome = true;
        }
        BrowserEvent::PageTyping(on) => state.page_typing = on,
        BrowserEvent::Fullscreen(on) => state.window_fullscreen(on, &mut fx),
        BrowserEvent::Engine { engine, event, at } => {
            state.engine_event(&engine, event, at, &mut fx)
        }
        BrowserEvent::OpenFailed { view, error } => {
            if let Some(engine) = state.views.get(&view).map(|v| v.engine.clone()) {
                let reason = format!("The {engine} engine couldn't open the page: {error}");
                state.view_gone(view, Some(clamp(reason)), &mut fx);
                if let EngineError::Runtime(message) = error {
                    state.runtime_failed(&engine, clamp(message), &mut fx);
                }
            }
        }
        BrowserEvent::CommandFailed {
            view,
            command,
            error,
        } => state.command_failed(view, command, error, &mut fx),
        BrowserEvent::FindResult {
            view,
            search,
            found,
            current,
            total,
            supported,
        } => state.find_result(
            view,
            None,
            search,
            (found, current, total),
            supported,
            &mut fx,
        ),
        BrowserEvent::ServiceDone { request, result } => {
            if let Some(pending) = state.requests.remove(request) {
                if state.is_document(pending.view, pending.document) {
                    fx.push(answer(pending.view, request, result));
                }
            }
        }
        BrowserEvent::PermissionAnswered { request, allow } => {
            state.permission_answered(request, allow, &mut fx)
        }
        BrowserEvent::ConfigReloaded(Ok(config)) => state.reconfigure(*config, &mut fx),
        BrowserEvent::ConfigReloaded(Err(error)) => fx.push(Effect::Notice {
            tab: None,
            message: format!("{error}\nThe configuration in use stays."),
        }),
        BrowserEvent::VisitsForgotten { request, outcome } => {
            state.visits_forgotten(request, outcome, &mut fx)
        }
    }
    state.settle(&mut fx);
    let mut effects = fx.effects;
    if fx.chrome {
        effects.push(Effect::RedrawChrome);
    }
    if fx.session || fx.quit {
        effects.push(Effect::SaveSession);
    }
    if fx.quit {
        effects.push(Effect::Quit);
    }
    effects
}

/// The origin of the page at `url` (scheme, host and port), if it has one.
fn page_origin(url: Option<&str>) -> Option<String> {
    let origin = url::Url::parse(url?).ok()?.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

/// The answer to `request`, for `view`'s engine.
fn answer(view: ViewId, request: RequestId, result: ServiceResult) -> Effect {
    Effect::Engine {
        view,
        command: EngineCommand::ResolveServiceRequest { request, result },
    }
}

/// The most `site.forget` requests left unfinished. Each one is fanned out to every engine.
const MAX_SITE_REQUESTS: usize = 16;

/// The most forgets of visited pages left unanswered. The executable answers each at once, or
/// once it has written the history file.
const MAX_VISIT_REQUESTS: usize = 64;

/// The longest address recorded as a visit, in bytes, without its fragment. A longer one is not
/// recorded, never shortened.
pub const MAX_VISIT_URL: usize = 8 * 1024;

/// `url` as a visit records it: an http or https address with no user name or password, its
/// fragment dropped, at most [`MAX_VISIT_URL`] bytes. `None` for anything else.
fn visit_url(url: &str) -> Option<String> {
    let mut url = Url::parse(url).ok()?;
    let web = matches!(url.scheme(), "http" | "https");
    if !web || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    url.set_fragment(None);
    let url = String::from(url);
    (url.len() <= MAX_VISIT_URL).then_some(url)
}

/// A `site.forget` the core has sent and not heard the end of.
#[derive(Debug)]
struct SiteForget {
    key: SiteKey,
    /// Engines that have not answered.
    waiting: BTreeSet<String>,
}

fn notice(message: String, fx: &mut Fx) {
    fx.push(Effect::Notice {
        tab: None,
        message: clamp(message),
    });
}

/// The site of a page address: `file:` is one jar, `sig:` keeps nothing, and anything else is
/// its host as `url` displays it.
fn site_of_address(address: &str) -> Result<SiteKey, String> {
    let url = Url::parse(address).map_err(|_| format!("{address:?} is not an address."))?;
    match url.scheme() {
        "file" => Ok(SiteKey::LocalFiles),
        "sig" => Err("This page is sig:, which keeps no site data.".into()),
        _ => match url.host() {
            Some(host) => Ok(SiteKey::Host(host.to_string())),
            None => Err("This page has no host to forget.".into()),
        },
    }
}

/// `text` as a host, the way [`site_of_address`] reads one. No wildcards.
fn site_of_host(text: &str) -> Result<SiteKey, String> {
    match text.contains('*') {
        true => Err(format!("{text:?} is not a host name.")),
        false => match url::Host::parse(text) {
            Ok(host) => Ok(SiteKey::Host(host.to_string())),
            Err(_) => Err(format!("{text:?} is not a host name.")),
        },
    }
}

/// The kinds a notice names, in a fixed order.
fn kind_list(kinds: SiteDataKinds) -> String {
    let mut names = Vec::new();
    if kinds.cookies {
        names.push("cookies");
    }
    if kinds.storage {
        names.push("storage");
    }
    if kinds.caches {
        names.push("caches");
    }
    if kinds.hsts {
        names.push("hsts");
    }
    match names.as_slice() {
        [] => "none".to_owned(),
        names => names.join(", "),
    }
}

/// Whether `address` is the forgotten site: `file:` for [`SiteKey::LocalFiles`], or a host
/// equal to the key or to one of `removed`, or under it (`www.example.com` under `example.com`).
fn page_matches(address: &str, key: &SiteKey, removed: &[String]) -> bool {
    let Ok(url) = Url::parse(address) else {
        return false;
    };
    if matches!(key, SiteKey::LocalFiles) && url.scheme() == "file" {
        return true;
    }
    let Some(host) = url.host().map(|host| host.to_string()) else {
        return false;
    };
    let key = match key {
        SiteKey::Host(host) => host.as_str(),
        SiteKey::LocalFiles => "",
    };
    host_under(&host, key) || removed.iter().any(|name| host_under(&host, name))
}

/// `host` is `key`, or a name under it. An empty key matches nothing.
fn host_under(host: &str, key: &str) -> bool {
    !key.is_empty()
        && (host == key
            || host
                .strip_suffix(key)
                .is_some_and(|rest| rest.ends_with('.')))
}

/// What one tab shows, as [`BrowserState::summary`] and a keymap both read it.
struct Shown<'a> {
    view: Option<&'a View>,
    url: Option<&'a str>,
    title: Option<&'a str>,
    content: Content<'a>,
    reloads: bool,
    requested: Option<&'a str>,
    loading: bool,
    switching: Option<&'a View>,
    can_go_back: bool,
    can_go_forward: bool,
    /// The engine of a page that is shown. `None` for a `sig:` page, a blank tab or an error.
    engine: Option<&'a str>,
}

/// The per-tab arithmetic of [`BrowserState::summary`]: the current view, the committed
/// address and title, whether it is loading, and back and forward.
fn shown<'a>(tab: &'a Tab, views: &'a BTreeMap<ViewId, View>, env: &Environment) -> Shown<'a> {
    let view = tab.current_view().and_then(|id| views.get(&id));
    let (url, title, content, reloads) = match tab.current() {
        Some(Segment::View(id)) => (
            view.and_then(|v| v.url.as_deref()),
            view.and_then(|v| v.title.as_deref()),
            Content::View(*id),
            false,
        ),
        Some(Segment::Internal(url)) => (Some(url.as_str()), None, Content::Internal(url), false),
        Some(Segment::Record(record)) => {
            (record.url(), record.title.as_deref(), Content::Blank, true)
        }
        None => (None, None, Content::Blank, false),
    };
    let switching = tab.switch.and_then(|switch| views.get(&switch.view));
    let pending = switching.or(view).and_then(|v| v.navigations.latest());
    let requested = pending.and_then(|navigation| navigation.url.as_deref());
    let loading = switching.is_some() || pending.is_some() || view.is_some_and(|v| v.loading);
    // A failed view is an error, not a page of that engine. A switch that has not replaced
    // the page leaves the previous view's engine, which is what the tab still shows.
    let engine = match tab.current() {
        Some(Segment::View(_)) => view
            .filter(|view| view.failure.is_none())
            .map(|view| view.engine()),
        _ => None,
    };
    Shown {
        view,
        url,
        title,
        content,
        reloads,
        requested,
        loading,
        switching,
        can_go_back: tab_can_go(tab, views, env, true),
        can_go_forward: tab_can_go(tab, views, env, false),
        engine,
    }
}

/// Whether back or forward has somewhere to go: a neighbouring segment, or the view's own
/// history when its engine can traverse it.
fn tab_can_go(tab: &Tab, views: &BTreeMap<ViewId, View>, env: &Environment, back: bool) -> bool {
    let view = tab.current_view().and_then(|id| views.get(&id));
    let segment = if back {
        tab.current > 0
    } else {
        tab.current + 1 < tab.segments.len()
    };
    segment || view.is_some_and(|view| history_can_traverse(view, env, back))
}

/// Whether back or forward stays in `view`'s own history.
fn history_can_traverse(view: &View, env: &Environment, back: bool) -> bool {
    let history = &view.history;
    let can = if back {
        history.can_go_back()
    } else {
        history.can_go_forward()
    };
    can && engine_capabilities(env, &view.engine).history_traversal
}

/// What `engine` supports. An engine the build lacks supports nothing.
fn engine_capabilities(env: &Environment, engine: &str) -> EngineCapabilities {
    env.engines.get(engine).copied().unwrap_or_default()
}

/// The browser a keymap reads for one key press. Borrowed from the state before the press is
/// applied, and dropped before any command runs (guarantee 7).
struct CoreBrowser<'a> {
    tabs: &'a [Tab],
    views: &'a BTreeMap<ViewId, View>,
    active: usize,
    context: KeyContext,
    env: &'a Environment,
    /// The view whose element is fullscreen, if any.
    fullscreen: Option<ViewId>,
}

impl Browser for CoreBrowser<'_> {
    fn frontend(&self) -> Frontend {
        self.env.frontend
    }

    fn context(&self) -> KeyContext {
        self.context
    }

    fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    fn active_tab(&self) -> usize {
        self.active
    }

    fn tab(&self, index: usize) -> Option<TabInfo<'_>> {
        let tab = self.tabs.get(index)?;
        let shown = shown(tab, self.views, self.env);
        Some(TabInfo::new(
            tab.id,
            shown.title,
            shown.url,
            shown.loading,
            shown.engine,
            tab.zoom,
            shown.can_go_back,
            shown.can_go_forward,
            shown.view.and_then(|view| view.hovered.as_deref()),
            self.fullscreen.is_some() && self.fullscreen == tab.current_view(),
        ))
    }
}

impl BrowserState {
    /// A browser with no tabs yet, using `config`. [`BrowserEvent::Start`] opens them.
    pub fn new(config: Config, env: Environment) -> Self {
        Self {
            keys: KeyState::new(config.keymap, &config.keymap_settings),
            buttons: HashSet::new(),
            seat: SeatId::next(),
            indicator: None,
            chrome: config.chrome().to_owned(),
            renderer: config.renderer().to_owned(),
            launched: env
                .engines
                .keys()
                .map(|engine| (engine.clone(), config.engine_settings(engine)))
                .collect(),
            config,
            env,
            tabs: Vec::new(),
            active: 0,
            focus: KeyboardFocus::Page,
            views: BTreeMap::new(),
            requests: Requests::default(),
            failed: BTreeMap::new(),
            site_requests: BTreeMap::new(),
            visit_requests: BTreeMap::new(),
            cursor: Cursor::Default,
            text_input: None,
            page_typing: true,
            closed: VecDeque::new(),
            fullscreen: Fullscreen::default(),
            window_focused: true,
        }
    }

    /// The configuration in use.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Which part of the window has the keyboard.
    pub fn focus(&self) -> KeyboardFocus {
        self.focus
    }

    /// What the keymap says about itself, for the chrome to show ("NORMAL"). The executable
    /// contains it before a chrome sees it, as it does page text.
    pub fn keymap_indicator(&self) -> Option<&str> {
        self.indicator.as_deref()
    }

    /// The index of the active tab in [`Self::tabs`].
    pub fn active(&self) -> usize {
        self.active
    }

    /// The tabs, in tab-strip order, as the chrome shows them.
    pub fn tabs(&self) -> impl ExactSizeIterator<Item = TabSummary<'_>> {
        self.tabs.iter().map(|tab| self.summary(tab))
    }

    /// A view the core knows, for diagnostics.
    pub fn view(&self, view: ViewId) -> Option<&View> {
        self.views.get(&view)
    }

    /// Every view the core knows, closing ones included: each says its tab, engine and state.
    pub fn views(&self) -> impl Iterator<Item = &View> {
        self.views.values()
    }

    /// A snapshot of the tabs and their history, to save.
    pub fn session(&self) -> Session {
        let segment = |segment: &Segment| match segment {
            Segment::Internal(url) => SessionSegment {
                engine: None,
                entries: vec![url.clone()],
                current: 0,
                title: None,
            },
            Segment::View(view) => saved(self.views[view].record()),
            Segment::Record(record) => saved(record.clone()),
        };
        let tab = |tab: &Tab| {
            let mut segments: Vec<_> = tab.segments.iter().map(segment).collect();
            // A tab still loading its first page keeps the address it is loading.
            let loading = tab.switch.map(|switch| &self.views[&switch.view]);
            if let (true, Some(view)) = (segments.is_empty(), loading) {
                let url = view.navigations.latest().and_then(|n| n.url.clone());
                segments.extend(url.map(|url| SessionSegment {
                    engine: Some(view.engine.clone()),
                    entries: vec![url],
                    current: 0,
                    title: None,
                }));
            }
            SessionTab {
                current: tab.current,
                zoom: tab.zoom,
                segments,
            }
        };
        Session {
            version: session::VERSION,
            active: self.active,
            tabs: self.tabs.iter().map(tab).collect(),
            notices: Vec::new(),
        }
    }

    fn summary<'a>(&'a self, tab: &'a Tab) -> TabSummary<'a> {
        let shown = shown(tab, &self.views, &self.env);
        TabSummary {
            id: tab.id,
            title: display_title(shown.title, shown.url.or(shown.requested)),
            url: shown.url,
            requested: shown.requested,
            loading: shown.loading,
            can_go_back: shown.can_go_back,
            can_go_forward: shown.can_go_forward,
            zoom: tab.zoom,
            can_zoom: shown
                .view
                .is_some_and(|view| self.capabilities(&view.engine).page_zoom),
            status: shown.view.and_then(|view| view.status.as_deref()),
            content: shown.content,
            error: tab.error.as_ref(),
            reloads: shown.reloads,
            switching_to: shown.switching.map(|view| view.engine()),
            find: shown.view.and_then(|view| view.find.as_ref()),
        }
    }

    /// What `engine` supports.
    pub(crate) fn capabilities(&self, engine: &str) -> EngineCapabilities {
        engine_capabilities(&self.env, engine)
    }

    /// The build's web engines, in descriptor order.
    pub(crate) fn web_engines(&self) -> &[String] {
        &self.env.web_engines
    }

    /// Whether `name` is an engine compiled into this build.
    pub(crate) fn has_engine(&self, name: &str) -> bool {
        self.env.engines.contains_key(name)
    }

    /// Whether back or forward stays in `view`'s own history.
    fn can_traverse(&self, view: &View, back: bool) -> bool {
        history_can_traverse(view, &self.env, back)
    }

    // ── Tabs ──────────────────────────────────────────────────────────────────────────────

    fn start(&mut self, session: Option<Session>, open: Vec<OpenRequest>, fx: &mut Fx) {
        let session = session.filter(|session| match session.check() {
            Ok(()) => true,
            Err(error) => {
                fx.push(Effect::Notice {
                    tab: None,
                    message: format!("The saved tabs can't be restored: {error}."),
                });
                false
            }
        });
        if let Some(session) = session {
            notify(session.notices, fx);
            let first = self.tabs.len();
            for saved in session.tabs {
                self.tabs.push(restored(saved));
                if self.tabs[self.tabs.len() - 1].segments.is_empty() {
                    self.open_home(self.tabs.len() - 1, fx);
                }
            }
            if self.tabs.len() > first {
                self.active = first + session.active;
            }
        }
        let mut opened = None;
        for request in open {
            if let Some(tab) = self.open_tab(request, false, fx) {
                opened.get_or_insert(tab);
            }
        }
        if let Some(tab) = opened {
            self.active = tab;
        }
        if self.tabs.is_empty() {
            self.new_tab(false, fx);
            self.open_home(0, fx);
        }
        notify(self.config.notices().to_vec(), fx);
        fx.chrome = true;
        fx.session = true;
    }

    /// Takes a reloaded configuration into use. The keymap of the same kind is configured
    /// again, keeping its state; another replaces it. The chrome stays the one `sig` started
    /// with, and the engines keep the settings they started with.
    fn reconfigure(&mut self, config: Config, fx: &mut Fx) {
        self.keys.configure(config.keymap, &config.keymap_settings);
        let mut notices = config.notices().to_vec();
        if config.chrome() != self.chrome {
            notices.push(format!(
                "[ui] chrome = {:?} applies when sig starts again; the {} chrome stays until \
                 then.",
                config.chrome(),
                self.chrome
            ));
        }
        if config.renderer() != self.renderer && self.env.frontend == Frontend::Gui {
            notices.push(format!(
                "[ui] renderer = {:?} applies when sig starts again; the chrome is drawn as \
                 [ui] renderer = {:?} says until then.",
                config.renderer(),
                self.renderer
            ));
        }
        for (engine, settings) in &self.launched {
            if config.engine_settings(engine) != *settings {
                notices.push(format!(
                    "[engines.{engine}] applies when sig starts again; the {engine} engine keeps \
                     the settings it started with until then."
                ));
            }
        }
        notify(notices, fx);
        if !config.remember_history() {
            self.end_visits(None);
        }
        self.config = config;
        fx.chrome = true;
    }

    /// Adds a tab at the end that opens `request`, and returns its index; `None` when there
    /// are [`session::MAX_TABS`] already.
    fn open_tab(
        &mut self,
        request: OpenRequest,
        opened_by_page: bool,
        fx: &mut Fx,
    ) -> Option<usize> {
        let tab = self.new_tab(opened_by_page, fx)?;
        let url = self.fixup(&request.input);
        self.navigate(tab, url, By::User(request.engine.as_deref()), fx);
        Some(tab)
    }

    /// Adds an empty tab at the end, at the configured zoom, and returns its index. The active
    /// tab doesn't change. With [`session::MAX_TABS`] open already, it says so and adds none:
    /// the session saves every tab, and can read back only so many.
    pub(crate) fn new_tab(&mut self, opened_by_page: bool, fx: &mut Fx) -> Option<usize> {
        if self.tabs.len() >= session::MAX_TABS {
            notice(
                format!(
                    "sig holds at most {} tabs: close one to open another.",
                    session::MAX_TABS
                ),
                fx,
            );
            return None;
        }
        self.tabs.push(Tab::new(opened_by_page, self.config.zoom()));
        Some(self.tabs.len() - 1)
    }

    /// Closes tab `index`, unless it is the last one. A tab with an address is kept for
    /// `tab.reopen`, dropping the oldest beyond [`MAX_CLOSED`].
    pub(crate) fn close_tab(&mut self, index: usize, fx: &mut Fx) {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return;
        }
        if let Some(closed) = self.closed_tab(index) {
            if self.closed.len() == MAX_CLOSED {
                self.closed.pop_front();
            }
            self.closed.push_back(closed);
        }
        let tab = self.tabs.remove(index);
        for view in tab.views() {
            self.close_view(view, fx);
        }
        if self.active > index || self.active == self.tabs.len() {
            self.active -= 1;
        }
        fx.chrome = true;
        fx.session = true;
    }

    /// What `tab.reopen` keeps of tab `index`: the address it shows, its title, the engine the
    /// user chose for it and its zoom. `None` when it has no address.
    fn closed_tab(&self, index: usize) -> Option<ClosedTab> {
        let tab = &self.tabs[index];
        let shown = shown(tab, &self.views, &self.env);
        Some(ClosedTab {
            url: self.address(index)?,
            title: shown.title.map(str::to_owned),
            engine: self.chosen_engine(index),
            zoom: tab.zoom,
        })
    }

    /// The engine the user chose for the page tab `index` shows, if they chose one.
    pub(crate) fn chosen_engine(&self, index: usize) -> Option<String> {
        let view = self.tabs[index]
            .current_view()
            .and_then(|id| self.views.get(&id));
        view.filter(|view| view.chosen)
            .map(|view| view.engine.clone())
    }

    pub(crate) fn tab_index(&self, tab: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == tab)
    }

    /// What `input` means as an address.
    pub(crate) fn fixup(&self, input: &str) -> String {
        let config = &self.config;
        let home = self.env.home_dir.as_deref();
        router::fixup(input, config.search(), config.keywords(), home)
    }

    pub(crate) fn open_home(&mut self, tab: usize, fx: &mut Fx) {
        let home = self.fixup(self.config.home());
        self.navigate(tab, home, By::User(None), fx);
    }

    /// The address tab `tab` shows, to open again: the committed one, or while nothing has
    /// committed, the one it is loading or failed to open.
    pub(crate) fn address(&self, tab: usize) -> Option<String> {
        let summary = self.summary(&self.tabs[tab]);
        let failed = summary.error.and_then(|error| error.url.as_deref());
        let address = summary.url.or(summary.requested).or(failed);
        address.map(str::to_owned)
    }

    /// Moves the keyboard focus and tells the chrome.
    pub(crate) fn move_focus(&mut self, focus: KeyboardFocus, fx: &mut Fx) {
        self.focus = focus;
        fx.push(Effect::Focus(focus));
        fx.chrome = true;
    }

    // ── Navigation ────────────────────────────────────────────────────────────────────────

    /// Opens `url` in tab `tab` as a new navigation, in the engine the user chose or the one
    /// routing picks: in the tab's current view if that is the engine's, in a new view that
    /// replaces it once it has committed otherwise, or as an internal page. The view is chosen
    /// when the user chose its engine, and no longer when the user navigates it without
    /// choosing; the page's own navigations leave that as it is. A page's navigation does
    /// nothing while the tab loads a new view the user asked for: the page is being left.
    pub(crate) fn navigate(&mut self, tab: usize, url: String, by: By<'_>, fx: &mut Fx) {
        let explicit = match by {
            By::User(engine) => engine,
            By::Page if self.tabs[tab].switch.is_some_and(|s| !s.by_page) => return,
            By::Page => None,
        };
        fx.chrome = true;
        self.cancel_switch(tab, fx);
        let error = |url: Option<String>, message: String| Some(LoadError { url, message });
        if url.len() > MAX_URL {
            self.tabs[tab].error = error(None, "The address is too long.".into());
            return;
        }
        let parsed = match Url::parse(&url) {
            Ok(parsed) => parsed,
            Err(e) => {
                let message = clamp(format!("{url} is not an address ({e})."));
                self.tabs[tab].error = error(Some(url), message);
                return;
            }
        };
        self.tabs[tab].error = None;
        let engine = match router::route(&parsed, &self.config, explicit) {
            Route::Internal => return self.append(tab, Segment::Internal(parsed.into()), fx),
            Route::Engine(engine) => engine,
            Route::Error(message) => {
                self.tabs[tab].error = error(Some(url), clamp(message));
                return;
            }
        };
        if let Err(message) = self.usable(&engine) {
            self.tabs[tab].error = error(Some(url), message);
            return;
        }
        let url = String::from(parsed);
        // An engine without history of its own gets a new view, so back reaches this page.
        let traverses = self.capabilities(&engine).history_traversal;
        match self.tabs[tab].current_view() {
            Some(view) if self.views[&view].engine == engine && traverses => {
                if let By::User(_) = by {
                    self.views.get_mut(&view).expect("current").chosen = explicit.is_some();
                }
                self.request(view, Request::Navigate(url), fx)
            }
            _ => self.open_view(tab, engine, url, SwitchInto::Append, by, fx),
        }
    }

    /// Whether this build can open views on `engine` now.
    fn usable(&self, engine: &str) -> Result<(), String> {
        if !self.env.engines.contains_key(engine) {
            return Err(format!(
                "The {engine} engine is not part of this build of sig."
            ));
        }
        match self.failed.get(engine) {
            Some(why) => Err(clamp(format!(
                "The {engine} engine stopped working ({why}). Restart sig to use it again."
            ))),
            None => Ok(()),
        }
    }

    /// Sends `request` to `view` as a navigation the core tracks.
    fn request(&mut self, view: ViewId, request: Request, fx: &mut Fx) {
        let navigation = NavigationId::next();
        let (kind, url, command) = match request {
            Request::Navigate(url) => (
                NavigationKind::Navigate,
                Some(url.clone()),
                EngineCommand::Navigate { navigation, url },
            ),
            Request::Reload => (
                NavigationKind::Reload,
                None,
                EngineCommand::Reload { navigation },
            ),
            Request::Traverse(delta) => (
                NavigationKind::Traverse,
                None,
                EngineCommand::Traverse { navigation, delta },
            ),
        };
        if let Some(v) = self.views.get_mut(&view) {
            v.navigations.request(navigation, kind, url);
        }
        fx.push(Effect::Engine { view, command });
        fx.chrome = true;
    }

    /// Opens a hidden view on `engine` that loads `url`, for tab `tab` to switch to, `by` the
    /// user (chosen if they chose the engine) or the page.
    fn open_view(
        &mut self,
        tab: usize,
        engine: String,
        url: String,
        into: SwitchInto,
        by: By<'_>,
        fx: &mut Fx,
    ) {
        let zooms = self.capabilities(&engine).page_zoom;
        let t = &mut self.tabs[tab];
        let zoom = if zooms { t.zoom } else { 1.0 };
        let mut view = View::new(engine.clone(), t.id, zoom);
        view.chosen = matches!(by, By::User(Some(_)));
        let id = view.id;
        t.switch = Some(Switch {
            view: id,
            into,
            by_page: matches!(by, By::Page),
        });
        let options = ViewOptions {
            visible: false,
            zoom,
        };
        self.views.insert(id, view);
        fx.push(Effect::OpenView {
            view: id,
            engine,
            options,
        });
        self.request(id, Request::Navigate(url), fx);
    }

    /// Drops the view tab `tab` is switching to, if any. Returns whether there was one.
    fn cancel_switch(&mut self, tab: usize, fx: &mut Fx) -> bool {
        let Some(switch) = self.tabs[tab].switch.take() else {
            return false;
        };
        self.close_view(switch.view, fx);
        fx.chrome = true;
        true
    }

    /// Drops tab `tab`'s switch because its page was stopped. A record the tab shows says so
    /// instead of loading again by itself; reloading loads it.
    fn abandon_switch(&mut self, tab: usize, fx: &mut Fx) {
        let t = &self.tabs[tab];
        let shows_record = matches!(t.current(), Some(Segment::Record(_)));
        if self.cancel_switch(tab, fx) && shows_record {
            self.tabs[tab].error.get_or_insert(LoadError {
                url: None,
                message: "Loading stopped.".into(),
            });
        }
    }

    /// Completes tab `tab`'s switch if its view has committed and can present.
    fn switch_progress(&mut self, tab: usize, fx: &mut Fx) {
        let Some(switch) = self.tabs[tab].switch else {
            return;
        };
        let view = &self.views[&switch.view];
        if !(view.ready && view.document.is_some()) {
            return;
        }
        self.tabs[tab].switch = None;
        let segment = Segment::View(switch.view);
        match switch.into {
            SwitchInto::Append => self.append(tab, segment, fx),
            SwitchInto::Restore(i) => {
                let t = &mut self.tabs[tab];
                let Segment::Record(record) = std::mem::replace(&mut t.segments[i], segment) else {
                    unreachable!("a restore switch always points at a record");
                };
                let (before, after) = record.split();
                let mut at = i;
                if let Some(before) = before {
                    t.segments.insert(at, Segment::Record(before));
                    at += 1;
                }
                if let Some(after) = after {
                    t.segments.insert(at + 1, Segment::Record(after));
                }
                t.current = at;
                t.error = None;
                self.bound_history(tab, fx);
            }
        }
    }

    /// Makes `segment` tab `tab`'s current segment, after the one it shows now, discarding the
    /// segments after that.
    fn append(&mut self, tab: usize, segment: Segment, fx: &mut Fx) {
        let t = &mut self.tabs[tab];
        let left = t.current_view();
        let dropped: Vec<_> = if t.segments.is_empty() {
            Vec::new()
        } else {
            t.segments.drain(t.current + 1..).collect()
        };
        t.segments.push(segment);
        t.current = t.segments.len() - 1;
        t.error = None;
        if let Some(view) = left.and_then(|view| self.views.get_mut(&view)) {
            view.history.end_here();
        }
        self.close_segments(dropped, fx);
        self.bound_history(tab, fx);
    }

    /// Keeps tab `tab`'s history within [`MAX_SEGMENTS`] and [`RETAINED_VIEWS`], dropping the
    /// oldest segments and turning the live segments farthest from the current one into
    /// records. A view of an engine with a process per view counts as all of
    /// [`RETAINED_VIEWS`]: kept alone, or not at all.
    fn bound_history(&mut self, tab: usize, fx: &mut Fx) {
        let t = &mut self.tabs[tab];
        let mut dropped = Vec::new();
        while t.segments.len() > MAX_SEGMENTS {
            // From the end farther from the current segment.
            if t.current > t.segments.len() / 2 {
                t.current -= 1;
                dropped.push(t.segments.remove(0));
            } else {
                dropped.extend(t.segments.pop());
            }
        }
        self.close_segments(dropped, fx);
        loop {
            let t = &self.tabs[tab];
            let cost = |view: &ViewId| match self
                .capabilities(&self.views[view].engine)
                .process_per_view
            {
                true => RETAINED_VIEWS,
                false => 1,
            };
            let retained: Vec<(usize, usize)> = t
                .segments
                .iter()
                .enumerate()
                .filter_map(|(i, segment)| match segment {
                    Segment::View(view) if i != t.current => Some((i, cost(view))),
                    _ => None,
                })
                .collect();
            if retained.iter().map(|(_, cost)| cost).sum::<usize>() <= RETAINED_VIEWS {
                break;
            }
            // Farthest first; of two as far, the older.
            let current = t.current;
            let (farthest, _) = retained
                .into_iter()
                .max_by_key(|&(i, _)| (i.abs_diff(current), std::cmp::Reverse(i)))
                .expect("more than RETAINED_VIEWS");
            self.evict(tab, farthest, fx);
        }
        fx.chrome = true;
        fx.session = true;
    }

    /// Turns segment `i` of tab `tab`, a live view, into a record and closes the view.
    fn evict(&mut self, tab: usize, i: usize, fx: &mut Fx) {
        let Segment::View(view) = self.tabs[tab].segments[i] else {
            return;
        };
        self.tabs[tab].segments[i] = Segment::Record(self.views[&view].record());
        self.close_view(view, fx);
    }

    fn close_segments(&mut self, segments: Vec<Segment>, fx: &mut Fx) {
        for segment in segments {
            if let Segment::View(view) = segment {
                self.close_view(view, fx);
            }
        }
    }

    /// The page in tab `tab`'s current segment made a new navigation: the segments after it
    /// are gone.
    fn prune(&mut self, tab: usize, fx: &mut Fx) {
        let t = &mut self.tabs[tab];
        if matches!(t.switch, Some(Switch { into: SwitchInto::Restore(i), .. }) if i > t.current) {
            self.cancel_switch(tab, fx);
        }
        let t = &mut self.tabs[tab];
        let dropped: Vec<_> = t.segments.drain(t.current + 1..).collect();
        if !dropped.is_empty() {
            self.close_segments(dropped, fx);
            fx.session = true;
        }
    }

    /// Goes back or forward in tab `tab`: through its view's own history while it has
    /// entries (and its engine can traverse them), else into the neighbouring segment. A new
    /// page still loading is just given up, unless it is the record the tab shows, which back
    /// and forward leave.
    pub(crate) fn traverse(&mut self, tab: usize, back: bool, fx: &mut Fx) {
        let shows_record = matches!(self.tabs[tab].current(), Some(Segment::Record(_)));
        if !shows_record && self.cancel_switch(tab, fx) {
            return;
        }
        let t = &self.tabs[tab];
        if let Some(view) = t.current_view() {
            if self.can_traverse(&self.views[&view], back) {
                return self.request(view, Request::Traverse(if back { -1 } else { 1 }), fx);
            }
        }
        let target = if back {
            t.current.checked_sub(1)
        } else {
            Some(t.current + 1).filter(|&i| i < t.segments.len())
        };
        if let Some(i) = target {
            self.cancel_switch(tab, fx);
            self.activate(tab, i, fx);
        }
    }

    /// Shows segment `i` of tab `tab`: at once if it is live or internal, after loading its
    /// URL again if it is a record.
    fn activate(&mut self, tab: usize, i: usize, fx: &mut Fx) {
        fx.chrome = true;
        fx.session = true;
        let t = &mut self.tabs[tab];
        t.error = None;
        let Segment::Record(record) = &t.segments[i] else {
            t.current = i;
            // A page whose document failed before it was left says so again.
            if let Some(failure) = t
                .current_view()
                .and_then(|view| self.views[&view].failure.clone())
            {
                t.error = Some(LoadError {
                    url: None,
                    message: failure,
                });
            }
            return;
        };
        let (engine, url) = (record.engine.clone(), record.url().map(str::to_owned));
        let restore = match url {
            None => Err("This page's address was too long to keep.".to_owned()),
            Some(url) if self.config.is_enabled(&engine) => self.usable(&engine).map(|()| url),
            Some(_) => Err(format!(
                "The {engine} engine is not enabled in the configuration."
            )),
        };
        match restore {
            Ok(url) => self.open_view(tab, engine, url, SwitchInto::Restore(i), By::User(None), fx),
            Err(message) => {
                let t = &mut self.tabs[tab];
                t.current = i;
                t.error = Some(LoadError { url: None, message });
            }
        }
    }

    /// Tries a failed address again, or reloads what tab `tab` shows.
    pub(crate) fn reload(&mut self, tab: usize, fx: &mut Fx) {
        if let Some(LoadError { url: Some(url), .. }) = self.tabs[tab].error.take() {
            return self.navigate(tab, url, By::User(None), fx);
        }
        fx.chrome = true;
        match self.tabs[tab].current() {
            Some(Segment::View(view)) => {
                let view = *view;
                self.request(view, Request::Reload, fx);
            }
            Some(Segment::Record(_)) => {
                self.cancel_switch(tab, fx);
                let current = self.tabs[tab].current;
                self.activate(tab, current, fx);
            }
            Some(Segment::Internal(_)) | None => {}
        }
    }

    pub(crate) fn stop(&mut self, tab: usize, fx: &mut Fx) {
        self.abandon_switch(tab, fx);
        if let Some(view) = self.tabs[tab].current_view() {
            fx.push(Effect::Engine {
                view,
                command: EngineCommand::Stop,
            });
        }
    }

    // ── Views ─────────────────────────────────────────────────────────────────────────────

    /// Asks for `view` to be closed and forgets everything pending for it. It stays known,
    /// closing, until its engine says it has closed.
    fn close_view(&mut self, view: ViewId, fx: &mut Fx) {
        let Some(v) = self.views.get_mut(&view) else {
            return;
        };
        if v.closing {
            return;
        }
        v.closing = true;
        v.navigations.clear();
        for request in self.requests.take(view, |_| true) {
            fx.push(Effect::CancelService(request));
        }
        fx.push(Effect::CloseView { view });
    }

    /// `view` is gone without the core closing it: its engine closed it, failed to open it, or
    /// failed altogether. Its segment becomes a record; the tab says what happened.
    fn view_gone(&mut self, view: ViewId, reason: Option<String>, fx: &mut Fx) {
        let Some(v) = self.views.remove(&view) else {
            return;
        };
        for request in self.requests.take(view, |_| true) {
            fx.push(Effect::CancelService(request));
        }
        let Some(tab) = self.tab_index(v.tab) else {
            return;
        };
        fx.chrome = true;
        fx.session = true;
        let reason = reason.or(v.failure.clone());
        let t = &mut self.tabs[tab];
        if t.switch.is_some_and(|switch| switch.view == view) {
            t.switch = None;
            t.error = Some(LoadError {
                url: v.navigations.latest().and_then(|n| n.url.clone()),
                message: reason.unwrap_or_else(|| "The page closed before it loaded.".into()),
            });
        } else if let Some(i) = t.segments.iter().position(|s| *s == Segment::View(view)) {
            t.segments[i] = Segment::Record(v.record());
            if i == t.current {
                t.error = Some(LoadError {
                    url: None,
                    message: reason.unwrap_or_else(|| "The page closed.".into()),
                });
            }
        }
    }

    /// `view`'s document is gone; the view may recover with a reload.
    fn view_failed(&mut self, view: ViewId, message: String, fx: &mut Fx) {
        let v = self.views.get_mut(&view).expect("checked by the caller");
        let url = v.navigations.latest().and_then(|n| n.url.clone());
        v.failure = Some(message.clone());
        v.document = None;
        // The status text was the document's: it must not hide why the view failed.
        v.status = None;
        v.hovered = None;
        v.answers.clear();
        v.loading = false;
        v.navigations.clear();
        let tab = v.tab;
        self.end_find(view, fx);
        self.cancel_requests(view, None, fx);
        let Some(tab) = self.tab_index(tab) else {
            return;
        };
        fx.chrome = true;
        let t = &mut self.tabs[tab];
        if t.switch.is_some_and(|switch| switch.view == view) {
            self.cancel_switch(tab, fx);
            self.tabs[tab].error = Some(LoadError { url, message });
        } else if t.current_view() == Some(view) {
            t.error = Some(LoadError { url: None, message });
        } else if let Some(i) = t.segments.iter().position(|s| *s == Segment::View(view)) {
            // A failed page in the history is of no use alive.
            self.evict(tab, i, fx);
        }
    }

    /// Answers `view`'s pending requests with `Cancelled`, except those of `keep`.
    fn cancel_requests(&mut self, view: ViewId, keep: Option<DocumentGeneration>, fx: &mut Fx) {
        for request in self.requests.take(view, |p| Some(p.document) != keep) {
            fx.push(Effect::CancelService(request));
            fx.push(answer(view, request, Err(ServiceError::Cancelled)));
        }
    }

    fn runtime_failed(&mut self, engine: &str, message: String, fx: &mut Fx) {
        let reason = format!("The {engine} engine stopped working: {message}");
        self.failed.insert(engine.to_owned(), message);
        let views: Vec<_> = self
            .views
            .values()
            .filter(|v| v.engine == engine)
            .map(|v| (v.id, v.closing))
            .collect();
        for (view, closing) in views {
            if !closing {
                fx.push(Effect::CloseView { view });
            }
            self.view_gone(view, Some(clamp(reason.clone())), fx);
        }
        // It will answer no `site.forget` it was asked: each fails for it now.
        let asked: Vec<RequestId> = self
            .site_requests
            .iter()
            .filter(|(_, pending)| pending.waiting.contains(engine))
            .map(|(&request, _)| request)
            .collect();
        for request in asked {
            let why = "it stopped working".to_owned();
            self.site_answered(engine, request, Err(why), fx);
        }
        fx.push(Effect::Notice {
            tab: None,
            message: clamp(format!("{reason}. Restart sig to use it again.")),
        });
    }

    fn command_failed(
        &mut self,
        view: ViewId,
        command: EngineCommand,
        error: EngineError,
        fx: &mut Fx,
    ) {
        let Some(v) = self.views.get(&view).filter(|v| !v.closing) else {
            return;
        };
        let (engine, tab) = (v.engine.clone(), v.tab);
        match error {
            EngineError::UnknownView(_) => self.view_gone(
                view,
                Some(format!("The {engine} engine lost the page.")),
                fx,
            ),
            EngineError::Runtime(message) => self.runtime_failed(&engine, clamp(message), fx),
            EngineError::View { message, .. } => self.view_failed(view, clamp(message), fx),
            EngineError::Unsupported(what) => {
                let message = format!("The {engine} engine doesn't support {what}.");
                match command {
                    EngineCommand::Navigate { navigation, .. }
                    | EngineCommand::Reload { navigation }
                    | EngineCommand::Traverse { navigation, .. } => {
                        self.navigation_failed(view, navigation, NavigationError::Load(message), fx)
                    }
                    // An engine that declared find but refuses it can't search after all.
                    EngineCommand::Find(
                        FindCommand::Search { search, .. }
                        | FindCommand::Next { search }
                        | FindCommand::Previous { search },
                    ) => self.find_result(view, None, search, (false, None, None), false, fx),
                    _ => fx.push(Effect::Notice {
                        tab: Some(tab),
                        message,
                    }),
                }
            }
        }
    }

    // ── Site data ─────────────────────────────────────────────────────────────────────────

    /// Forgets `host`, or the page the active tab shows. Asks every engine that keeps any kind.
    pub(crate) fn site_forget(&mut self, host: Option<String>, fx: &mut Fx) {
        let key = match host {
            Some(text) => match site_of_host(&text) {
                Ok(key) => key,
                Err(message) => return notice(message, fx),
            },
            None => match self.address(self.active) {
                None => return notice("This tab has no page to forget.".into(), fx),
                Some(address) => match site_of_address(&address) {
                    Ok(key) => key,
                    Err(message) => return notice(message, fx),
                },
            },
        };
        // The site's visited pages go too, whatever the engines can forget.
        if let SiteKey::Host(host) = &key {
            self.forget_visits(Some(host.clone()), fx);
        }
        let keepers: Vec<&String> = self
            .env
            .engines
            .iter()
            .filter(|(_, capabilities)| capabilities.site_data.any())
            .map(|(name, _)| name)
            .collect();
        if keepers.is_empty() {
            return notice(
                "No engine in this build can forget site data yet, so nothing was forgotten."
                    .into(),
                fx,
            );
        }
        // A failed engine answers nothing.
        let waiting: BTreeSet<String> = keepers
            .into_iter()
            .filter(|name| !self.failed.contains_key(*name))
            .cloned()
            .collect();
        if waiting.is_empty() {
            return notice(
                "The engines that keep site data stopped working: restart sig to forget a site."
                    .into(),
                fx,
            );
        }
        if self.site_requests.len() >= MAX_SITE_REQUESTS {
            return notice(
                format!(
                    "Too many site.forget requests are still unfinished ({MAX_SITE_REQUESTS})."
                ),
                fx,
            );
        }
        let request = RequestId::next();
        self.site_requests.insert(
            request,
            SiteForget {
                key: key.clone(),
                waiting,
            },
        );
        fx.push(Effect::SiteData {
            engine: None,
            request: SiteDataRequest::Forget { request, site: key },
        });
    }

    /// `engine` finished `request`. An answer for a request it was not waiting on is dropped,
    /// and so is a second answer from the same engine.
    fn site_answered(
        &mut self,
        engine: &str,
        request: RequestId,
        result: Result<SiteDataOutcome, String>,
        fx: &mut Fx,
    ) {
        let Some(pending) = self.site_requests.get_mut(&request) else {
            return;
        };
        if !pending.waiting.remove(engine) {
            return;
        }
        let key = pending.key.clone();
        if pending.waiting.is_empty() {
            self.site_requests.remove(&request);
        }
        let label = match &key {
            SiteKey::Host(host) => host.clone(),
            SiteKey::LocalFiles => "local files".into(),
        };
        let removed = result.as_ref().ok().map(|outcome| outcome.removed.clone());
        let message = match &result {
            Ok(outcome) if outcome.profile => {
                let kinds = kind_list(outcome.kinds);
                let removed = match outcome.removed.as_slice() {
                    [] => "nothing".to_owned(),
                    names => names.join(", "),
                };
                format!("{label} forgotten in {engine}: {kinds} (removed: {removed})")
            }
            Ok(_) => format!("{label}: nothing on disk changed in {engine} (no profile)"),
            Err(message) => {
                format!("{label}: nothing on disk changed in {engine} ({message})")
            }
        };
        notice(message, fx);
        // An accepted forget reloads the page, so it cannot write the data back, whether or
        // not this instance has a profile. A failure leaves the page as it is.
        if let Some(removed) = removed {
            self.reload_forgotten(engine, &key, &removed, fx);
        }
    }

    /// Reloads every tab whose current view runs in `engine` and whose host is `key`, or is
    /// under it, or is one of `removed` the same way. `file:` pages match [`SiteKey::LocalFiles`].
    fn reload_forgotten(&mut self, engine: &str, key: &SiteKey, removed: &[String], fx: &mut Fx) {
        let tabs: Vec<usize> = (0..self.tabs.len())
            .filter(|&tab| self.tab_matches_site(tab, engine, key, removed))
            .collect();
        for tab in tabs {
            self.reload(tab, fx);
        }
    }

    fn tab_matches_site(
        &self,
        tab: usize,
        engine: &str,
        key: &SiteKey,
        removed: &[String],
    ) -> bool {
        let Some(view) = self.tabs[tab].current_view() else {
            return false;
        };
        if self
            .views
            .get(&view)
            .is_none_or(|view| view.engine != engine)
        {
            return false;
        }
        let Some(address) = self.address(tab) else {
            return false;
        };
        page_matches(&address, key, removed)
    }

    // ── Engine events ─────────────────────────────────────────────────────────────────────

    fn engine_event(&mut self, engine: &str, event: EngineEvent, at: Instant, fx: &mut Fx) {
        let Some(id) = event.view() else {
            self.runtime_event(engine, event, fx);
            return;
        };
        // Events about views that are gone, being closed, or someone else's, are late.
        let Some(view) = self.views.get_mut(&id).filter(|v| v.engine == engine) else {
            return;
        };
        if view.closing {
            if matches!(event, EngineEvent::ViewClosed { .. }) {
                self.views.remove(&id);
            }
            return;
        }
        match event {
            EngineEvent::ViewReady { .. } => {
                view.ready = true;
                let tab = view.tab;
                if let Some(tab) = self.tab_index(tab) {
                    self.switch_progress(tab, fx);
                }
                fx.chrome = true;
            }
            EngineEvent::NavigationStarted {
                navigation, url, ..
            } => {
                view.navigations.started(navigation, bounded(url));
                fx.chrome = true;
            }
            EngineEvent::NavigationCommitted {
                navigation,
                url,
                document,
                history,
                ..
            } => self.committed(id, navigation, url, document, history, fx),
            EngineEvent::NavigationFailed {
                navigation, error, ..
            } => self.navigation_failed(id, navigation, error, fx),
            EngineEvent::LoadingChanged { loading, .. } => {
                view.loading = loading;
                fx.chrome = true;
            }
            EngineEvent::HistoryChanged {
                entries, current, ..
            } => {
                fx.chrome |= view.history.report(entries, current);
            }
            EngineEvent::TitleChanged { title, .. } => {
                view.title = title.map(clamp);
                fx.chrome = true;
                // The title of the document whose commit was the visit, if it still is.
                let visit = view
                    .visit
                    .as_ref()
                    .filter(|(_, d)| view.document == Some(*d));
                if let Some((url, _)) = visit.filter(|_| self.config.remember_history()) {
                    fx.push(Effect::Visited {
                        url: url.clone(),
                        title: view.title.clone(),
                        commit: false,
                    });
                }
            }
            EngineEvent::StatusTextChanged { text, .. } => {
                view.hovered = text.as_deref().and_then(link);
                view.status = text.map(clamp);
                fx.chrome = true;
            }
            EngineEvent::CursorChanged { cursor, .. } => view.cursor = cursor,
            EngineEvent::TextInputChanged { state, .. } => {
                view.text_input = state.map(|mut state| {
                    // Surrounding text is a page's; keep it within bounds or not at all.
                    state.surrounding = state.surrounding.filter(|s| s.text.len() <= MAX_TEXT);
                    state
                })
            }
            EngineEvent::FrameReady { .. } | EngineEvent::SemanticsChanged { .. } => {}
            EngineEvent::FindResult {
                document,
                search,
                found,
                current,
                total,
                ..
            } => self.find_result(
                id,
                Some(document),
                search,
                (found, current, total),
                true,
                fx,
            ),
            EngineEvent::FullscreenChanged {
                document,
                fullscreen,
                ..
            } => self.page_fullscreen_changed(id, document, fullscreen, at, fx),
            EngineEvent::ServiceRequested(request) => self.service_requested(request, at, fx),
            EngineEvent::ServiceCancelled { request, .. } => {
                if self.requests.get(request).is_some_and(|p| p.view == id) {
                    self.requests.remove(request);
                    fx.push(Effect::CancelService(request));
                }
            }
            EngineEvent::NavigationRequested {
                document,
                request,
                url,
                ..
            } => self.navigation_requested(id, document, request, url, at, fx),
            EngineEvent::OpenRequested {
                document,
                url,
                disposition,
                ..
            } => self.open_requested(id, document, url, disposition, at, fx),
            EngineEvent::CloseRequested { document, .. } => self.close_requested(id, document, fx),
            EngineEvent::ViewClosed { .. } => self.view_gone(id, None, fx),
            EngineEvent::ViewFailed { message, .. } => self.view_failed(id, clamp(message), fx),
            EngineEvent::RuntimeFailed { .. }
            | EngineEvent::SitesListed { .. }
            | EngineEvent::SiteDataDone { .. } => {
                unreachable!("handled above: no view")
            }
        }
    }

    /// An event about the runtime rather than a view.
    fn runtime_event(&mut self, engine: &str, event: EngineEvent, fx: &mut Fx) {
        match event {
            EngineEvent::RuntimeFailed { message } => {
                self.runtime_failed(engine, clamp(message), fx);
            }
            // List is not sent this slice.
            EngineEvent::SitesListed { .. } => {}
            EngineEvent::SiteDataDone { request, result } => {
                let mut event = EngineEvent::SiteDataDone { request, result };
                let result = match event.check() {
                    Ok(()) => match event {
                        EngineEvent::SiteDataDone { result, .. } => result,
                        _ => unreachable!("the same event"),
                    },
                    Err(error) => Err(format!("the report was not usable ({error})")),
                };
                self.site_answered(engine, request, result, fx);
            }
            _ => unreachable!("view() is None only for runtime-level events"),
        }
    }

    fn committed(
        &mut self,
        id: ViewId,
        navigation: NavigationId,
        url: String,
        document: DocumentGeneration,
        history: Option<HistoryEffect>,
        fx: &mut Fx,
    ) {
        fx.chrome = true;
        fx.session = true;
        let view = self.views.get_mut(&id).expect("checked by the caller");
        let new_document = view.document != Some(document);
        let new_entry = view
            .navigations
            .committed(navigation)
            .is_new(new_document, history);
        // A new document in a new entry may be a visit; any other commit ends the last one.
        let visit = (new_document && new_entry)
            .then(|| visit_url(&url))
            .flatten();
        view.visit = None;
        view.url = bounded(url);
        view.failure = None;
        if new_entry {
            view.history.unlimit();
        }
        let tab = view.tab;
        if new_document {
            // A new document: nothing of the old one's carries over.
            view.document = Some(document);
            view.answers.clear();
            view.activation = None;
            view.link = None;
            view.hovered = None;
            self.end_find(id, fx);
            self.cancel_requests(id, Some(document), fx);
        }
        let Some(tab) = self.tab_index(tab) else {
            return;
        };
        let t = &mut self.tabs[tab];
        let switching = t.switch.filter(|switch| switch.view == id);
        // Restoring a saved page (a session's, or one back or forward) is not a visit.
        let restoring = switching.is_some_and(|s| matches!(s.into, SwitchInto::Restore(_)));
        let shown = (switching.is_some() || t.current_view() == Some(id)) && !restoring;
        let switching = switching.is_some();
        if switching {
            self.switch_progress(tab, fx);
        } else if t.current_view() == Some(id) {
            t.error = None;
            if new_entry {
                self.prune(tab, fx);
            }
        }
        if shown && tab == self.active {
            self.record_visit(id, visit, document, fx);
        }
    }

    /// The commit of `document` in `id` was a visit to `url`, if it was one (see the module
    /// documentation): record it, unless `[history] remember` is off or the view's engine
    /// started with ephemeral storage.
    fn record_visit(
        &mut self,
        id: ViewId,
        url: Option<String>,
        document: DocumentGeneration,
        fx: &mut Fx,
    ) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let ephemeral = self
            .launched
            .get(&view.engine)
            .is_some_and(|settings| settings.storage == Storage::Ephemeral);
        let Some(url) = url.filter(|_| self.config.remember_history() && !ephemeral) else {
            return;
        };
        view.visit = Some((url.clone(), document));
        fx.push(Effect::Visited {
            url,
            title: None,
            commit: true,
        });
    }

    /// Forgets the visits to `host` (and the hosts under it), or all of them: ends the views'
    /// visits ([`Self::end_visits`]) and asks the executable to forget them.
    pub(crate) fn forget_visits(&mut self, host: Option<String>, fx: &mut Fx) {
        if self.visit_requests.len() >= MAX_VISIT_REQUESTS {
            return notice(
                format!(
                    "Too many requests to forget visited pages are still unfinished \
                     ({MAX_VISIT_REQUESTS})."
                ),
                fx,
            );
        }
        self.end_visits(host.as_deref());
        let request = RequestId::next();
        self.visit_requests.insert(request, host.clone());
        fx.push(Effect::ForgetVisits { request, host });
    }

    /// No view's document is a visit to `host` (or a host under it), or to anything, any more:
    /// its later title changes nothing.
    fn end_visits(&mut self, host: Option<&str>) {
        for view in self.views.values_mut() {
            let gone = view.visit.as_ref().is_some_and(|(url, _)| {
                host.is_none_or(|host| {
                    let visited = Url::parse(url).ok();
                    let visited = visited.as_ref().and_then(Url::host_str);
                    visited.is_some_and(|visited| host_under(visited, host))
                })
            });
            if gone {
                view.visit = None;
            }
        }
    }

    /// The executable answered a forget of visits: say what happened. A site's forget that
    /// found no visit says nothing: the engines' answers say what happened to the site.
    fn visits_forgotten(&mut self, request: RequestId, outcome: ForgetOutcome, fx: &mut Fx) {
        let Some(host) = self.visit_requests.remove(&request) else {
            return;
        };
        let none = matches!(
            outcome,
            ForgetOutcome::Done { removed: 0 } | ForgetOutcome::HereOnly { removed: 0 }
        );
        if none && host.is_some() {
            return;
        }
        let what = |removed: usize| {
            let pages = match removed {
                1 => "1 visited page".to_owned(),
                n => format!("{n} visited pages"),
            };
            match &host {
                Some(host) => format!("{pages} of {host}"),
                None => pages,
            }
        };
        notice(
            match outcome {
                ForgetOutcome::Done { removed } => format!("Forgot {}.", what(removed)),
                ForgetOutcome::HereOnly { removed } => format!(
                    "Forgot {} here only: another sig owns the history file, which still \
                     holds them.",
                    what(removed)
                ),
                ForgetOutcome::NotWritten { removed, error } => format!(
                    "Forgot {} here, but the history file could not be written ({error}), so \
                     it may still hold them.",
                    what(removed)
                ),
            },
            fx,
        );
    }

    fn navigation_failed(
        &mut self,
        id: ViewId,
        navigation: NavigationId,
        error: NavigationError,
        fx: &mut Fx,
    ) {
        let view = self.views.get_mut(&id).expect("checked by the caller");
        // Not pending: superseded, or never asked for.
        let Some(navigation) = view.navigations.ended(navigation) else {
            return;
        };
        let tab = view.tab;
        // A new view left with nothing to load and nothing loaded will never show anything.
        let idle = view.navigations.latest().is_none() && view.document.is_none();
        fx.chrome = true;
        let Some(tab) = self.tab_index(tab) else {
            return;
        };
        let switching = self.tabs[tab]
            .switch
            .is_some_and(|switch| switch.view == id);
        match error {
            NavigationError::Load(message) => {
                let error = LoadError {
                    url: navigation.url,
                    message: clamp(message),
                };
                if switching {
                    self.cancel_switch(tab, fx);
                    self.tabs[tab].error = Some(error);
                } else if self.tabs[tab].current_view() == Some(id) {
                    self.tabs[tab].error = Some(error);
                }
            }
            // Stopped, perhaps because the engine is downloading what it found: not an error.
            NavigationError::Cancelled if switching && idle => self.abandon_switch(tab, fx),
            NavigationError::Cancelled => {}
        }
    }

    fn open_requested(
        &mut self,
        id: ViewId,
        document: DocumentGeneration,
        url: String,
        disposition: OpenDisposition,
        at: Instant,
        fx: &mut Fx,
    ) {
        let Some(tab) = self.asking_tab(id, document) else {
            return;
        };
        let Some(url) = bounded(url) else {
            return;
        };
        let tab_id = Some(self.tabs[tab].id);
        if self.refused(id, &url, fx) {
            return;
        }
        match disposition {
            OpenDisposition::CurrentView => self.navigate(tab, url, By::Page, fx),
            OpenDisposition::NewView => {
                let view = self.views.get_mut(&id).expect("checked by the caller");
                if permissions::take_activation(&mut view.activation, at) {
                    if let Some(tab) = self.new_tab(true, fx) {
                        self.navigate(tab, url, By::Page, fx);
                    }
                } else {
                    fx.push(Effect::Notice {
                        tab: tab_id,
                        message: clamp(format!("Blocked a pop-up to {url}: pages open tabs only when you click or type in them.")),
                    });
                }
            }
        }
    }

    /// Whether a page in `view` may not open `url`, telling the user if so: Sighurt's own pages
    /// never, and local files only from a local page.
    fn refused(&self, view: ViewId, url: &str, fx: &mut Fx) -> bool {
        let v = &self.views[&view];
        let is_file = |url: &str| Url::parse(url).is_ok_and(|url| url.scheme() == "file");
        let why = match Url::parse(url).as_ref().map(Url::scheme) {
            Ok("sig") => "Pages can't open Sighurt's own pages.",
            Ok("file") if !v.url.as_deref().is_some_and(is_file) => {
                "Only local pages can open local files."
            }
            _ => return false,
        };
        fx.push(Effect::Notice {
            tab: Some(v.tab),
            message: clamp(format!("A page tried to open {url}. {why}")),
        });
        true
    }

    /// A page asks to navigate its view to `url` itself, which its engine holds until the
    /// answer: allowed, unless the user clicked a link to `url` in the page (the link the page's
    /// status text named at the press) within the last [`ACTIVATION`](permissions::ACTIVATION),
    /// routing gives `url` to another engine that can open it, and the user didn't choose the
    /// view's engine. Then it is denied, the click is used up, and the tab opens `url` as
    /// routing says, in a new view that replaces this one once it commits (unless the tab is
    /// loading a page the user asked for: then nothing happens). So a link the user clicks goes
    /// where the address, typed, would go, while what a page does by itself (a redirect, a frame
    /// loading, a script, even right after a click) stays in its engine. Denied as well: a
    /// request of a document the tab doesn't show, and what pages may not open at all. Every
    /// request gets one answer.
    fn navigation_requested(
        &mut self,
        id: ViewId,
        document: DocumentGeneration,
        request: RequestId,
        url: String,
        at: Instant,
        fx: &mut Fx,
    ) {
        let answer = |allow| Effect::Engine {
            view: id,
            command: EngineCommand::AnswerNavigation { request, allow },
        };
        let Some(tab) = self.asking_tab(id, document) else {
            return fx.push(answer(false));
        };
        if self.refused(id, &url, fx) {
            return fx.push(answer(false));
        }
        let parsed = match url.len() <= MAX_URL {
            true => Url::parse(&url).ok(),
            false => None,
        };
        let route = parsed.map(|url| router::route(&url, &self.config, None));
        let view = &self.views[&id];
        let elsewhere = matches!(&route, Some(Route::Engine(engine))
            if *engine != view.engine && self.usable(engine).is_ok());
        let view = self.views.get_mut(&id).expect("checked by the caller");
        let clicked = view
            .link
            .as_deref()
            .is_some_and(|link| same_url(link, &url));
        if view.chosen
            || !elsewhere
            || !clicked
            || !permissions::take_activation(&mut view.activation, at)
        {
            return fx.push(answer(true));
        }
        fx.push(answer(false));
        self.navigate(tab, url, By::Page, fx);
    }

    /// A page asks to close itself: only a tab a page opened, that has shown nothing else.
    fn close_requested(&mut self, id: ViewId, document: DocumentGeneration, fx: &mut Fx) {
        let Some(tab) = self.asking_tab(id, document) else {
            return;
        };
        let t = &self.tabs[tab];
        let alone = t.segments.len() == 1 && !self.views[&id].history.can_go_back();
        if t.opened_by_page && alone {
            self.close_tab(tab, fx);
        }
    }

    /// The index of the tab whose current page is `document` in `view`: the only page that may
    /// ask for anything.
    fn asking_tab(&self, view: ViewId, document: DocumentGeneration) -> Option<usize> {
        let v = self.views.get(&view)?;
        let tab = self.tab_index(v.tab)?;
        let current = v.document == Some(document) && self.tabs[tab].current_view() == Some(view);
        current.then_some(tab)
    }

    /// Whether `document` is still `view`'s, so an answer for it applies.
    fn is_document(&self, view: ViewId, document: DocumentGeneration) -> bool {
        self.views
            .get(&view)
            .is_some_and(|v| !v.closing && v.document == Some(document))
    }

    // ── Services ──────────────────────────────────────────────────────────────────────────

    fn service_requested(&mut self, request: ServiceRequest, at: Instant, fx: &mut Fx) {
        let ServiceRequest {
            id,
            view,
            document,
            origin,
            service,
        } = request;
        let refuse = |error| answer(view, id, Err(error));
        if !self.is_document(view, document) {
            return fx.push(refuse(ServiceError::Cancelled));
        }
        let Some(tab) = self.asking_tab(view, document) else {
            // A page in the history, or one still loading to replace the shown one.
            return fx.push(refuse(ServiceError::Denied));
        };
        if self.requests.count(view) >= MAX_PENDING_REQUESTS {
            return fx.push(refuse(ServiceError::Denied));
        }
        let origin = origin.and_then(bounded);
        let v = self.views.get_mut(&view).expect("checked above");
        let allowed = match permissions::requirement(&service) {
            Requirement::Refused => false,
            Requirement::Activation { consume: true } => {
                permissions::take_activation(&mut v.activation, at)
            }
            Requirement::Activation { consume: false } => permissions::is_active(v.activation, at),
            Requirement::Throttle => permissions::throttle(&mut self.tabs[tab].last_download, at),
            Requirement::Prompt(permission) => {
                // Without an origin the engine can't say which frame asks: no grant is
                // reused for it, and refusals are remembered under the page's origin.
                let known = origin.is_some();
                let remembered = origin.clone().or_else(|| page_origin(v.url.as_deref()));
                let refused = remembered
                    .is_some_and(|origin| self.tabs[tab].refused.contains(&(origin, permission)));
                let answered = v
                    .answers
                    .iter()
                    .find(|(p, _)| *p == permission)
                    .map(|&(_, allow)| allow);
                match (answered, refused) {
                    (Some(true), _) if known => fx.push(answer(view, id, Ok(ServiceReply::Done))),
                    (Some(false), _) | (_, true) => fx.push(refuse(ServiceError::Denied)),
                    (Some(true) | None, false) => {
                        fx.push(Effect::Prompt {
                            request: id,
                            tab: self.tabs[tab].id,
                            origin: origin.clone(),
                            permission,
                        });
                        let pending = Pending {
                            view,
                            document,
                            origin,
                            prompt: Some(permission),
                        };
                        self.requests.insert(id, pending);
                    }
                }
                return;
            }
        };
        if !allowed {
            return fx.push(refuse(ServiceError::Denied));
        }
        let pending = Pending {
            view,
            document,
            origin,
            prompt: None,
        };
        self.requests.insert(id, pending);
        fx.push(Effect::Service {
            request: id,
            view,
            service,
        });
    }

    fn permission_answered(&mut self, request: RequestId, allow: bool, fx: &mut Fx) {
        let Some(pending) = self.requests.get(request).cloned() else {
            return;
        };
        let Some(permission) = pending.prompt else {
            return;
        };
        let view = pending.view;
        if !self.is_document(view, pending.document) {
            self.requests.remove(request);
            return;
        }
        let v = self.views.get_mut(&view).expect("checked above");
        let known = pending.origin.is_some();
        // A grant to a request of unknown origin is that request's alone: another frame of the
        // document may be asking the next time.
        if allow && !known {
            self.requests.remove(request);
            fx.push(answer(view, request, Ok(ServiceReply::Done)));
            return;
        }
        v.answers.push((permission, allow));
        let remembered = pending
            .origin
            .clone()
            .or_else(|| page_origin(v.url.as_deref()));
        let tab = v.tab;
        let tab = self.tab_index(tab);
        if let (false, Some(origin), Some(tab)) = (allow, remembered, tab) {
            self.tabs[tab].refused.push((origin, permission));
        }
        // The answer holds for every request of this document for the same permission.
        let same = |p: &Pending| p.document == pending.document && p.prompt == Some(permission);
        for id in self.requests.take(view, same) {
            if id != request {
                fx.push(Effect::CancelService(id));
            }
            let result = match allow {
                true => Ok(ServiceReply::Done),
                false => Err(ServiceError::Denied),
            };
            fx.push(answer(view, id, result));
        }
    }

    // ── Input ─────────────────────────────────────────────────────────────────────────────

    /// The view shown in the page area: the active tab's current one.
    pub(crate) fn shown_view(&self) -> Option<ViewId> {
        let view = self.tabs.get(self.active)?.current_view()?;
        (!self.views[&view].closing).then_some(view)
    }

    /// Whether the window has the keyboard, as far as the frontend said.
    pub(crate) fn window_focused(&self) -> bool {
        self.window_focused
    }

    /// Which frontend runs.
    pub(crate) fn frontend(&self) -> Frontend {
        self.env.frontend
    }

    /// The view that has the keyboard, if the page has it.
    fn focused_view(&self) -> Option<ViewId> {
        self.shown_view()
            .filter(|_| self.focus == KeyboardFocus::Page)
    }

    /// What a key press happens in: where the keyboard is, and whether that part takes typed
    /// text (the chrome's line, or a text field of the page).
    fn key_context(&self) -> KeyContext {
        let typing = self.focus == KeyboardFocus::Chrome
            || (self.page_typing
                && self
                    .focused_view()
                    .is_some_and(|view| self.views[&view].text_input.is_some()));
        KeyContext::new(self.focus, typing)
    }

    fn keyboard(&mut self, event: InputEvent, at: Instant, fx: &mut Fx) {
        self.seat = event.seat;
        match &event.kind {
            InputKind::Key(key) => {
                // The keymap reads the tabs through a borrow of the fields it needs, which
                // ends before any command of this press changes them.
                let context = self.key_context();
                let outcome = {
                    let browser = CoreBrowser {
                        tabs: &self.tabs,
                        views: &self.views,
                        active: self.active,
                        context,
                        env: &self.env,
                        fullscreen: self.page_fullscreen(),
                    };
                    self.keys.handle(context, event.seat, key, &browser)
                };
                match outcome {
                    KeyOutcome::Run(commands) => {
                        // `settle` runs once, after this press. When several commands run, tell
                        // the engines which view is shown after each one, so a later
                        // `page.send-key` follows that view's focus and visibility, as when the
                        // commands run one by one. One command is left entirely to `settle`.
                        let between = commands.len() > 1;
                        for command in commands {
                            self.key_ran(&command, at);
                            self.run_command(command, fx);
                            if between {
                                self.settle_engines(fx);
                            }
                        }
                        return;
                    }
                    KeyOutcome::Consumed => return,
                    // The one guarantee a configuration cannot remove: while the page shown
                    // holds an element fullscreen, an unmodified Escape press that the keymap
                    // let through leaves it, and its repeats and release are taken with it
                    // (`KeyState`). A press the page got before fullscreen began is not taken:
                    // only a fresh press counts.
                    KeyOutcome::Deliver if self.escape_leaves_fullscreen(key) => {
                        self.keys.take(event.seat, key.code);
                        self.run_command(Command::PageExitFullscreen, fx);
                        return;
                    }
                    // The press goes where the keyboard is after the command.
                    KeyOutcome::RunThenDeliver(command) => {
                        self.key_ran(&command, at);
                        self.run_command(command, fx);
                    }
                    KeyOutcome::Deliver => {}
                }
            }
            InputKind::Focus(FocusEvent::Deactivated) => {
                self.window_focused = false;
                self.keys.forget(event.seat);
            }
            InputKind::Focus(FocusEvent::Activated) => self.window_focused = true,
            InputKind::TextInput(_) => {}
            // Pointer input comes with the view it is for.
            InputKind::Pointer(_) => return,
        }
        match self.focused_view() {
            Some(view) => {
                if matches!(&event.kind, InputKind::Key(key) if key.state == ButtonState::Pressed) {
                    self.views
                        .get_mut(&view)
                        .expect("shown")
                        .activated(at, None);
                }
                fx.push(Effect::Engine {
                    view,
                    command: EngineCommand::Input(event),
                });
            }
            None => fx.push(Effect::ChromeInput(event)),
        }
    }

    /// Whether `key` is a fresh, unmodified Escape press for a page that has the keyboard and
    /// holds an element fullscreen: the press the core's override takes.
    fn escape_leaves_fullscreen(&self, key: &KeyEvent) -> bool {
        let m = key.modifiers;
        let unmodified = !(m.shift || m.control || m.alt || m.alt_graph || m.logo);
        key.state == ButtonState::Pressed
            && !key.repeat
            && unmodified
            && matches!(&key.key, Key::Named(name) if name == "Escape")
            && self.focused_view().is_some()
            && self.focused_view() == self.page_fullscreen()
    }

    /// A key press ran `command`. A key it sends the page counts as the user's key press there,
    /// as a key delivered to the page does.
    fn key_ran(&mut self, command: &Command, at: Instant) {
        if let (Command::PageSendKey(_), Some(view)) = (command, self.focused_view()) {
            self.views
                .get_mut(&view)
                .expect("shown")
                .activated(at, None);
        }
    }

    /// Presses and releases `key` in the page, back to back so that no key stays held, if the
    /// page has the keyboard; otherwise does nothing.
    pub(crate) fn send_key(&mut self, key: &SentKey, fx: &mut Fx) {
        let Some(view) = self.focused_view() else {
            return;
        };
        for state in [ButtonState::Pressed, ButtonState::Released] {
            let event = InputEvent {
                seat: self.seat,
                kind: InputKind::Key(key.event(state)),
            };
            fx.push(Effect::Engine {
                view,
                command: EngineCommand::Input(event),
            });
        }
    }

    /// Pointer input on `view`'s target: side buttons are `[input.buttons]`'s first, the rest
    /// the page's.
    fn pointer(&mut self, view: ViewId, event: InputEvent, at: Instant, fx: &mut Fx) {
        let side = matches!(
            &event.kind,
            InputKind::Pointer(PointerEvent::Button {
                button: PointerButton::Back | PointerButton::Forward,
                ..
            })
        );
        if side {
            return self.button(Some(view), event, at, fx);
        }
        self.page_pointer(view, event, at, fx);
    }

    /// An action on a node of the shown page's content, chosen in `document`'s snapshot: one the
    /// user's input caused is their activation, as a press in the page is
    /// ([`Self::page_pointer`]), and gives the page the keyboard.
    fn semantic(
        &mut self,
        view: ViewId,
        document: DocumentGeneration,
        action: SemanticAction,
        link: Option<String>,
        at: Instant,
        fx: &mut Fx,
    ) {
        if self.shown_view() != Some(view) {
            return;
        }
        let v = self.views.get_mut(&view).expect("shown");
        if v.document != Some(document) {
            return;
        }
        if action.is_activation() {
            let link = link
                .filter(|l| matches!(action, SemanticAction::Activate(_)) && l.len() <= MAX_URL);
            v.activated(at, link);
            if self.focus != KeyboardFocus::Page {
                self.move_focus(KeyboardFocus::Page, fx);
            }
        }
        fx.push(Effect::Engine {
            view,
            command: EngineCommand::Semantic { document, action },
        });
    }

    /// A side button over `view`, or over the chrome: a press `[input.buttons]` binds runs its
    /// command and is taken, with its release, even if the binding changes in between. An
    /// unbound one goes to the page shown, if it is over it.
    fn button(&mut self, view: Option<ViewId>, event: InputEvent, at: Instant, fx: &mut Fx) {
        let InputKind::Pointer(PointerEvent::Button { button, state, .. }) = &event.kind else {
            return;
        };
        let taken = (event.seat, *button);
        match state {
            ButtonState::Pressed => match self.config.button(*button).cloned() {
                Some(command) => {
                    self.buttons.insert(taken);
                    return self.run_command(command, fx);
                }
                None => _ = self.buttons.remove(&taken),
            },
            ButtonState::Released if self.buttons.remove(&taken) => return,
            ButtonState::Released => {}
        }
        if let Some(view) = view {
            self.page_pointer(view, event, at, fx);
        }
    }

    /// Pointer input for `view`'s page, if it is the one shown: a press there is the user's
    /// activation and gives the page the keyboard; scrolling goes `[input] scroll-speed` times
    /// as far.
    fn page_pointer(&mut self, view: ViewId, mut event: InputEvent, at: Instant, fx: &mut Fx) {
        // A target that is no longer shown gets nothing more.
        if self.shown_view() != Some(view) {
            return;
        }
        match &mut event.kind {
            InputKind::Pointer(PointerEvent::Button {
                state: ButtonState::Pressed,
                ..
            }) => {
                let v = self.views.get_mut(&view).expect("shown");
                v.activated(at, v.status.clone());
                if self.focus != KeyboardFocus::Page {
                    self.move_focus(KeyboardFocus::Page, fx);
                }
            }
            InputKind::Pointer(PointerEvent::Scroll { delta, .. }) => {
                *delta = scaled(*delta, self.config.scroll_speed());
            }
            _ => {}
        }
        fx.push(Effect::Engine {
            view,
            command: EngineCommand::Input(event),
        });
    }

    // ── Presentation ──────────────────────────────────────────────────────────────────────

    /// What [`Self::settle`] tells the engines. The active tab loads a record it shows, every
    /// other view blurs and hides, and the shown view is visible, focused when the page has the
    /// keyboard, and at the tab's zoom. A press that runs several commands does this after each
    /// one, so a later command's input follows the focus and visibility an earlier command
    /// needs; a single command leaves it to `settle`. Once the engines have been told, doing it
    /// again adds nothing.
    fn settle_engines(&mut self, fx: &mut Fx) {
        let unloaded = self.tabs.get(self.active).filter(|t| {
            matches!(t.current(), Some(Segment::Record(_)))
                && t.switch.is_none()
                && t.error.is_none()
        });
        if let Some(current) = unloaded.map(|t| t.current) {
            self.activate(self.active, current, fx);
        }
        let shown = self.shown_view();
        let focused = self.focused_view();
        // Blur and hide first, so two views never have the keyboard at once.
        for view in self.views.values_mut().filter(|v| !v.closing) {
            let id = view.id;
            let command = |command| Effect::Engine { view: id, command };
            if view.focused && focused != Some(view.id) {
                view.focused = false;
                fx.push(command(EngineCommand::SetFocus(false)));
            }
            if view.visible && shown != Some(view.id) {
                view.visible = false;
                fx.push(command(EngineCommand::SetVisibility(false)));
            }
        }
        if let Some(id) = shown {
            let zoom = self.tabs[self.active].zoom;
            let zooms = self.capabilities(&self.views[&id].engine).page_zoom;
            let view = self.views.get_mut(&id).expect("shown");
            let mut command = |command| fx.push(Effect::Engine { view: id, command });
            if zooms && view.zoom != zoom {
                view.zoom = zoom;
                command(EngineCommand::SetZoom(zoom));
            }
            if !view.visible {
                view.visible = true;
                command(EngineCommand::SetVisibility(true));
            }
            if focused.is_some() && !view.focused {
                view.focused = true;
                command(EngineCommand::SetFocus(true));
            }
        }
    }

    /// Brings the engines, the host and the keymap in line with the state after an update: the
    /// active tab loads a record it shows, only its view is visible and has the keyboard (when
    /// the page has it), at the tab's zoom, the host knows the page's cursor and text field, and
    /// the keymap knows the context; a changed keymap indicator redraws the chrome.
    fn settle(&mut self, fx: &mut Fx) {
        self.settle_fullscreen(fx);
        self.settle_engines(fx);
        let shown = self.shown_view();
        let focused = self.focused_view();
        let cursor = shown.map_or(Cursor::Default, |id| self.views[&id].cursor);
        if cursor != self.cursor {
            self.cursor = cursor;
            fx.push(Effect::PageCursor(cursor));
        }
        let text_input = focused
            .filter(|_| self.page_typing)
            .and_then(|id| self.views[&id].text_input.clone());
        if text_input != self.text_input {
            self.text_input.clone_from(&text_input);
            fx.push(Effect::PageTextInput(text_input));
        }
        self.keys.set_context(self.key_context());
        if self.keys.indicator() != self.indicator.as_deref() {
            self.indicator = self.keys.indicator().map(str::to_owned);
            fx.chrome = true;
        }
    }
}

/// `delta`, `speed` times as far. A wheel's 120ths are rounded, but never to nothing.
fn scaled(delta: ScrollDelta, speed: f64) -> ScrollDelta {
    let wheel = |value: i32| match (f64::from(value) * speed).round() as i32 {
        0 => value.signum(),
        scaled => scaled,
    };
    match delta {
        ScrollDelta::Pixels { x, y } => ScrollDelta::Pixels {
            x: x * speed,
            y: y * speed,
        },
        ScrollDelta::Wheel { x, y } => ScrollDelta::Wheel {
            x: wheel(x),
            y: wheel(y),
        },
    }
}

/// Whether `a` and `b` are the same URL, as the `url` crate normalizes them.
fn same_url(a: &str, b: &str) -> bool {
    a == b || matches!((Url::parse(a), Url::parse(b)), (Ok(a), Ok(b)) if a == b)
}

/// Tells the user `notices` about the configuration, as one notice.
fn notify(notices: Vec<String>, fx: &mut Fx) {
    if !notices.is_empty() {
        fx.push(Effect::Notice {
            tab: None,
            message: notices.join("\n"),
        });
    }
}

/// A saved tab, restored with a new id, its own zoom and its engine segments as records.
fn restored(saved: SessionTab) -> Tab {
    let mut tab = Tab::new(false, crate::commands::clamp_zoom(saved.zoom));
    tab.segments = saved
        .segments
        .into_iter()
        .map(|segment| match segment.engine {
            None => Segment::Internal(segment.entries.into_iter().next().unwrap_or_default()),
            Some(engine) => Segment::Record(Record {
                engine,
                entries: segment.entries,
                current: segment.current,
                title: segment.title,
            }),
        })
        .collect();
    tab.current = saved.current;
    tab
}

fn saved(record: Record) -> SessionSegment {
    SessionSegment {
        engine: Some(record.engine),
        entries: record.entries,
        current: record.current,
        title: record.title,
    }
}
