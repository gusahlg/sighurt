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
//! - every command from the chrome as [`BrowserEvent::Command`], and focus the chrome moves
//!   itself (a click in the address bar, Escape) as [`BrowserEvent::Focus`];
//! - every keyboard event as [`BrowserEvent::Keyboard`] and every pointer event on a page
//!   target as [`BrowserEvent::Pointer`]. The core matches key bindings and routes the rest to
//!   the page or, as [`Effect::ChromeInput`], to the chrome;
//! - every event of every engine pump as [`BrowserEvent::Engine`], in order. The core drops the
//!   ones about views it has closed or replaced and requests from replaced documents.
//!   `FrameReady` is the executable's to schedule presentation from; the core ignores it;
//! - what its own calls and services produced: [`BrowserEvent::OpenFailed`],
//!   [`BrowserEvent::CommandFailed`], [`BrowserEvent::ServiceDone`],
//!   [`BrowserEvent::PermissionAnswered`], [`BrowserEvent::ConfigReloaded`].
//!
//! It carries out the returned effects in order: an [`Effect::OpenView`] comes before the
//! commands for its view, and a view is sent nothing after its [`Effect::CloseView`]. After any
//! update, the chrome draws from [`BrowserState::tabs`] and [`BrowserState::focus`].
//!
//! Must not own: I/O, timers, threads, surface metrics or anything the chrome draws. It is not a
//! framework: no event bus, no generic UI tree, no plugin registry.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use sighurt_api::commands::Command;
use sighurt_api::engine::{EngineCapabilities, EngineCommand, EngineError, ViewOptions};
use sighurt_api::events::{Cursor, EngineEvent, NavigationError, OpenDisposition};
use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, TabId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, PointerButton, PointerEvent, TextInputState,
};
use sighurt_api::services::{
    Permission, Service, ServiceError, ServiceReply, ServiceRequest, ServiceResult,
};
use url::Url;

use crate::config::{Config, ConfigError};
use crate::keys::{KeyOutcome, KeyState};
use crate::navigation::{
    NavigationKind, Record, Segment, Switch, SwitchInto, MAX_SEGMENTS, RETAINED_VIEWS,
};
use crate::permissions::{self, Pending, Requests, Requirement, MAX_PENDING_REQUESTS};
use crate::router::{self, Route};
use crate::session::{self, Session, SessionSegment, SessionTab};
use crate::tabs::{
    bounded, clamp, display_title, Content, LoadError, Tab, TabSummary, View, MAX_TEXT, MAX_URL,
};

/// Which part of the window has the keyboard: the key-binding context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Focus {
    /// The active tab's page, or the internal page or error the chrome shows in its place.
    #[default]
    Page,
    /// The address bar.
    Location,
}

/// What the executable tells the core about the world it runs in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    /// The engines compiled into this build, by name ("servo", "wasm", "demo"), with what each
    /// supports (`EngineRuntime::capabilities`). An engine that can't traverse its own history
    /// gets a view per page, each page a segment of the tab's history; one without page zoom is
    /// never sent `SetZoom`.
    pub engines: BTreeMap<String, EngineCapabilities>,
    /// The user's home directory, for `~/` in typed addresses.
    pub home_dir: Option<PathBuf>,
}

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
        /// The saved session, if it could be read. A damaged one is refused with a notice.
        session: Option<Session>,
        /// What the command line asked for.
        open: Vec<OpenRequest>,
    },
    /// A command from the chrome: a button, the address bar, the tab strip.
    Command(Command),
    /// Open something in a new tab and select it.
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
    /// The chrome moved the keyboard focus itself.
    Focus(Focus),
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
    ConfigReloaded(Result<Config, ConfigError>),
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
    /// Close `view` (`EngineRuntime::close_view`) and retire its target once the engine is
    /// done with it. The core sends it nothing more.
    CloseView {
        /// The view.
        view: ViewId,
    },
    /// The keyboard focus moves. `Location`: the chrome focuses the address bar and selects its
    /// text. `Page`: the chrome leaves the address bar, discarding an edit.
    Focus(Focus),
    /// Keyboard input for the chrome: the address bar has focus, or the page area shows no
    /// engine view.
    ChromeInput(InputEvent),
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
    /// Tell the user something: a blocked pop-up, a configuration error.
    Notice {
        /// The tab it is about, if any.
        tab: Option<TabId>,
        /// What to say.
        message: String,
    },
    /// Something the chrome shows changed: draw it again.
    RedrawChrome,
    /// The session changed: save [`BrowserState::session`], atomically. Waiting a moment and
    /// saving only the latest is fine, except right before [`Effect::Quit`].
    SaveSession,
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
    focus: Focus,
    /// Every view the core has opened and not seen closed.
    views: BTreeMap<ViewId, View>,
    keys: KeyState,
    requests: Requests,
    /// Engines whose runtime failed, and why. They open nothing more.
    failed: BTreeMap<String, String>,
    /// What the executable was last told the page area looks like.
    cursor: Cursor,
    text_input: Option<TextInputState>,
}

/// Applies `event` to `state` and returns what the executable should do.
pub fn update(state: &mut BrowserState, event: BrowserEvent) -> Vec<Effect> {
    let mut fx = Fx::default();
    match event {
        BrowserEvent::Start { session, open } => state.start(session, open, &mut fx),
        BrowserEvent::Command(command) => state.run_command(command, &mut fx),
        BrowserEvent::Open(request) => {
            let tab = state.open_tab(request, false, &mut fx);
            state.active = tab;
            state.move_focus(Focus::Page, &mut fx);
        }
        BrowserEvent::Keyboard { event, at } => state.keyboard(event, at, &mut fx),
        BrowserEvent::Pointer { view, event, at } => state.pointer(view, event, at, &mut fx),
        BrowserEvent::Focus(focus) => {
            state.focus = focus;
            fx.chrome = true;
        }
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
        BrowserEvent::ConfigReloaded(Ok(config)) => {
            state.config = config;
            fx.chrome = true;
        }
        BrowserEvent::ConfigReloaded(Err(error)) => fx.push(Effect::Notice {
            tab: None,
            message: format!("{error}\nThe configuration in use stays."),
        }),
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

/// The answer to `request`, for `view`'s engine.
fn answer(view: ViewId, request: RequestId, result: ServiceResult) -> Effect {
    Effect::Engine {
        view,
        command: EngineCommand::ResolveServiceRequest { request, result },
    }
}

impl BrowserState {
    /// A browser with no tabs yet, using `config`. [`BrowserEvent::Start`] opens them.
    pub fn new(config: Config, env: Environment) -> Self {
        Self {
            config,
            env,
            tabs: Vec::new(),
            active: 0,
            focus: Focus::Page,
            views: BTreeMap::new(),
            keys: KeyState::default(),
            requests: Requests::default(),
            failed: BTreeMap::new(),
            cursor: Cursor::Default,
            text_input: None,
        }
    }

    /// The configuration in use.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Which part of the window has the keyboard.
    pub fn focus(&self) -> Focus {
        self.focus
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
        }
    }

    fn summary<'a>(&'a self, tab: &'a Tab) -> TabSummary<'a> {
        let view = tab.current_view().and_then(|id| self.views.get(&id));
        let (url, title, content, reloads) = match tab.current() {
            Some(Segment::View(id)) => (
                view.and_then(|v| v.url.as_deref()),
                view.and_then(|v| v.title.as_deref()),
                Content::View(*id),
                false,
            ),
            Some(Segment::Internal(url)) => {
                (Some(url.as_str()), None, Content::Internal(url), false)
            }
            Some(Segment::Record(record)) => {
                (record.url(), record.title.as_deref(), Content::Blank, true)
            }
            None => (None, None, Content::Blank, false),
        };
        let switching = tab.switch.and_then(|switch| self.views.get(&switch.view));
        let pending = switching.or(view).and_then(|v| v.navigations.latest());
        let requested = pending.and_then(|navigation| navigation.url.as_deref());
        let loading = switching.is_some() || pending.is_some() || view.is_some_and(|v| v.loading);
        TabSummary {
            id: tab.id,
            title: display_title(title, url.or(requested)),
            url,
            requested,
            loading,
            can_go_back: self.can_go(tab, true),
            can_go_forward: self.can_go(tab, false),
            zoom: tab.zoom,
            can_zoom: view.is_some_and(|v| self.capabilities(&v.engine).page_zoom),
            status: view.and_then(|v| v.status.as_deref()),
            content,
            error: tab.error.as_ref(),
            reloads,
        }
    }

    fn can_go(&self, tab: &Tab, back: bool) -> bool {
        let view = tab.current_view().and_then(|id| self.views.get(&id));
        let segment = if back {
            tab.current > 0
        } else {
            tab.current + 1 < tab.segments.len()
        };
        segment || view.is_some_and(|v| self.can_traverse(v, back))
    }

    /// What `engine` supports.
    fn capabilities(&self, engine: &str) -> EngineCapabilities {
        self.env.engines.get(engine).copied().unwrap_or_default()
    }

    /// Whether back or forward stays in `view`'s own history.
    fn can_traverse(&self, view: &View, back: bool) -> bool {
        let history = &view.history;
        let can = if back {
            history.can_go_back()
        } else {
            history.can_go_forward()
        };
        can && self.capabilities(&view.engine).history_traversal
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
            let tab = self.open_tab(request, false, fx);
            opened.get_or_insert(tab);
        }
        if let Some(tab) = opened {
            self.active = tab;
        }
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new(false));
            self.open_home(0, fx);
        }
        fx.chrome = true;
        fx.session = true;
    }

    /// Adds a tab at the end that opens `request`, and returns its index.
    fn open_tab(&mut self, request: OpenRequest, opened_by_page: bool, fx: &mut Fx) -> usize {
        let tab = self.new_tab(opened_by_page);
        let url = self.fixup(&request.input);
        self.navigate(tab, url, request.engine.as_deref(), fx);
        tab
    }

    /// Adds an empty tab at the end and returns its index. The active tab doesn't change.
    pub(crate) fn new_tab(&mut self, opened_by_page: bool) -> usize {
        self.tabs.push(Tab::new(opened_by_page));
        self.tabs.len() - 1
    }

    /// Closes tab `index`, unless it is the last one.
    pub(crate) fn close_tab(&mut self, index: usize, fx: &mut Fx) {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return;
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

    fn tab_index(&self, tab: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == tab)
    }

    /// What `input` means as an address.
    pub(crate) fn fixup(&self, input: &str) -> String {
        router::fixup(input, self.config.search(), self.env.home_dir.as_deref())
    }

    pub(crate) fn open_home(&mut self, tab: usize, fx: &mut Fx) {
        let home = self.fixup(self.config.home());
        self.navigate(tab, home, None, fx);
    }

    /// Moves the keyboard focus and tells the chrome.
    pub(crate) fn move_focus(&mut self, focus: Focus, fx: &mut Fx) {
        self.focus = focus;
        fx.push(Effect::Focus(focus));
        fx.chrome = true;
    }

    // ── Navigation ────────────────────────────────────────────────────────────────────────

    /// Opens `url` in tab `tab` as a new navigation, in the engine `explicit` names or the one
    /// routing picks: in the tab's current view if that is the engine's, in a new view that
    /// replaces it once it has committed otherwise, or as an internal page.
    pub(crate) fn navigate(
        &mut self,
        tab: usize,
        url: String,
        explicit: Option<&str>,
        fx: &mut Fx,
    ) {
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
                self.request(view, Request::Navigate(url), fx)
            }
            _ => self.open_view(tab, engine, url, SwitchInto::Append, fx),
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

    /// Opens a hidden view on `engine` that loads `url`, for tab `tab` to switch to.
    fn open_view(
        &mut self,
        tab: usize,
        engine: String,
        url: String,
        into: SwitchInto,
        fx: &mut Fx,
    ) {
        let zooms = self.capabilities(&engine).page_zoom;
        let t = &mut self.tabs[tab];
        let zoom = if zooms { t.zoom } else { 1.0 };
        let view = View::new(engine.clone(), t.id, zoom);
        let id = view.id;
        t.switch = Some(Switch { view: id, into });
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
    /// records.
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
            let retained = t
                .segments
                .iter()
                .enumerate()
                .filter(|(i, segment)| *i != t.current && matches!(segment, Segment::View(_)))
                .map(|(i, _)| i);
            if retained.clone().count() <= RETAINED_VIEWS {
                break;
            }
            // Farthest first; of two as far, the older.
            let current = t.current;
            let farthest = retained
                .max_by_key(|&i| (i.abs_diff(current), std::cmp::Reverse(i)))
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
            Ok(url) => self.open_view(tab, engine, url, SwitchInto::Restore(i), fx),
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
            return self.navigate(tab, url, None, fx);
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
        v.answers.clear();
        v.loading = false;
        v.navigations.clear();
        let tab = v.tab;
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
                    _ => fx.push(Effect::Notice {
                        tab: Some(tab),
                        message,
                    }),
                }
            }
        }
    }

    // ── Engine events ─────────────────────────────────────────────────────────────────────

    fn engine_event(&mut self, engine: &str, event: EngineEvent, at: Instant, fx: &mut Fx) {
        let Some(id) = event.view() else {
            if let EngineEvent::RuntimeFailed { message } = event {
                self.runtime_failed(engine, clamp(message), fx);
            }
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
                ..
            } => self.committed(id, navigation, url, document, fx),
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
            }
            EngineEvent::StatusTextChanged { text, .. } => {
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
            EngineEvent::FrameReady { .. } => {}
            EngineEvent::ServiceRequested(request) => self.service_requested(request, at, fx),
            EngineEvent::ServiceCancelled { request, .. } => {
                if self.requests.get(request).is_some_and(|p| p.view == id) {
                    self.requests.remove(request);
                    fx.push(Effect::CancelService(request));
                }
            }
            EngineEvent::OpenRequested {
                document,
                url,
                disposition,
                ..
            } => self.open_requested(id, document, url, disposition, at, fx),
            EngineEvent::CloseRequested { document, .. } => self.close_requested(id, document, fx),
            EngineEvent::ViewClosed { .. } => self.view_gone(id, None, fx),
            EngineEvent::ViewFailed { message, .. } => self.view_failed(id, clamp(message), fx),
            EngineEvent::RuntimeFailed { .. } => unreachable!("handled above: it has no view"),
        }
    }

    fn committed(
        &mut self,
        id: ViewId,
        navigation: NavigationId,
        url: String,
        document: DocumentGeneration,
        fx: &mut Fx,
    ) {
        fx.chrome = true;
        fx.session = true;
        let view = self.views.get_mut(&id).expect("checked by the caller");
        let new_document = view.document != Some(document);
        let new_entry = view.navigations.committed(navigation).is_new(new_document);
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
            self.cancel_requests(id, Some(document), fx);
        }
        let Some(tab) = self.tab_index(tab) else {
            return;
        };
        let t = &mut self.tabs[tab];
        if t.switch.is_some_and(|switch| switch.view == id) {
            self.switch_progress(tab, fx);
        } else if t.current_view() == Some(id) {
            t.error = None;
            if new_entry {
                self.prune(tab, fx);
            }
        }
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
        let is_file = |url: &str| Url::parse(url).is_ok_and(|url| url.scheme() == "file");
        let refusal = match Url::parse(&url).as_ref().map(Url::scheme) {
            Ok("sig") => Some("Pages can't open Sighurt's own pages."),
            Ok("file") if !self.views[&id].url.as_deref().is_some_and(is_file) => {
                Some("Only local pages can open local files.")
            }
            _ => None,
        };
        if let Some(why) = refusal {
            return fx.push(Effect::Notice {
                tab: tab_id,
                message: clamp(format!("A page tried to open {url}. {why}")),
            });
        }
        match disposition {
            OpenDisposition::CurrentView => self.navigate(tab, url, None, fx),
            OpenDisposition::NewView => {
                let view = self.views.get_mut(&id).expect("checked by the caller");
                if permissions::take_activation(&mut view.activation, at) {
                    let tab = self.new_tab(true);
                    self.navigate(tab, url, None, fx);
                } else {
                    fx.push(Effect::Notice {
                        tab: tab_id,
                        message: clamp(format!("Blocked a pop-up to {url}: pages open tabs only when you click or type in them.")),
                    });
                }
            }
        }
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
                let refused = origin.as_ref().is_some_and(|origin| {
                    self.tabs[tab]
                        .refused
                        .contains(&(origin.clone(), permission))
                });
                let answered = v.answers.iter().find(|(p, _)| *p == permission);
                match (answered, refused) {
                    (Some((_, true)), _) => fx.push(answer(view, id, Ok(ServiceReply::Done))),
                    (Some((_, false)), _) | (None, true) => fx.push(refuse(ServiceError::Denied)),
                    (None, false) => {
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
        v.answers.push((permission, allow));
        let tab = v.tab;
        let tab = self.tab_index(tab);
        if let (false, Some(origin), Some(tab)) = (allow, pending.origin, tab) {
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
    fn shown_view(&self) -> Option<ViewId> {
        let view = self.tabs.get(self.active)?.current_view()?;
        (!self.views[&view].closing).then_some(view)
    }

    /// The view that has the keyboard, if the page has it.
    fn focused_view(&self) -> Option<ViewId> {
        self.shown_view().filter(|_| self.focus == Focus::Page)
    }

    fn keyboard(&mut self, event: InputEvent, at: Instant, fx: &mut Fx) {
        match &event.kind {
            InputKind::Key(key) => {
                let typing = self.focus == Focus::Location
                    || self
                        .focused_view()
                        .is_some_and(|view| self.views[&view].text_input.is_some());
                let bindings = self.config.keys();
                match self
                    .keys
                    .handle(bindings, self.focus, typing, event.seat, key)
                {
                    KeyOutcome::Run(command) => return self.run_command(command, fx),
                    KeyOutcome::Consumed => return,
                    KeyOutcome::Deliver => {}
                }
            }
            InputKind::Focus(FocusEvent::Deactivated) => self.keys.forget(event.seat),
            InputKind::Focus(FocusEvent::Activated) | InputKind::TextInput(_) => {}
            // Pointer input comes with the view it is for.
            InputKind::Pointer(_) => return,
        }
        match self.focused_view() {
            Some(view) => {
                if matches!(&event.kind, InputKind::Key(key) if key.state == ButtonState::Pressed) {
                    self.views.get_mut(&view).expect("shown").activation = Some(at);
                }
                fx.push(Effect::Engine {
                    view,
                    command: EngineCommand::Input(event),
                });
            }
            None => fx.push(Effect::ChromeInput(event)),
        }
    }

    fn pointer(&mut self, view: ViewId, event: InputEvent, at: Instant, fx: &mut Fx) {
        // A target that is no longer shown gets nothing more.
        if self.shown_view() != Some(view) {
            return;
        }
        if let InputKind::Pointer(PointerEvent::Button { button, state, .. }) = &event.kind {
            match (button, state) {
                // The mouse's back and forward buttons are the browser's, as they always were.
                (PointerButton::Back | PointerButton::Forward, ButtonState::Pressed) => {
                    let back = *button == PointerButton::Back;
                    return self.run_command(
                        if back {
                            Command::PageBack
                        } else {
                            Command::PageForward
                        },
                        fx,
                    );
                }
                (PointerButton::Back | PointerButton::Forward, ButtonState::Released) => return,
                (_, ButtonState::Pressed) => {
                    self.views.get_mut(&view).expect("shown").activation = Some(at);
                    if self.focus != Focus::Page {
                        self.move_focus(Focus::Page, fx);
                    }
                }
                (_, ButtonState::Released) => {}
            }
        }
        fx.push(Effect::Engine {
            view,
            command: EngineCommand::Input(event),
        });
    }

    // ── Presentation ──────────────────────────────────────────────────────────────────────

    /// Brings the engines and the host in line with the state after an update: the active tab
    /// loads a record it shows, only its view is visible and has the keyboard (when the page
    /// has it), at the tab's zoom, and the host knows the page's cursor and text field.
    fn settle(&mut self, fx: &mut Fx) {
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
        let cursor = shown.map_or(Cursor::Default, |id| self.views[&id].cursor);
        if cursor != self.cursor {
            self.cursor = cursor;
            fx.push(Effect::PageCursor(cursor));
        }
        let text_input = focused.and_then(|id| self.views[&id].text_input.clone());
        if text_input != self.text_input {
            self.text_input.clone_from(&text_input);
            fx.push(Effect::PageTextInput(text_input));
        }
    }
}

/// A saved tab, restored with a new id and its engine segments as records.
fn restored(saved: SessionTab) -> Tab {
    let mut tab = Tab::new(false);
    tab.zoom = crate::commands::clamp_zoom(saved.zoom);
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
