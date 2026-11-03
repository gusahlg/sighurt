//! The Wayland owner loop, built on calloop, and the wiring of the host, the chrome and the
//! session.
//!
//! Its sources are the Wayland connection (calloop-wayland-source's prepare-read, read,
//! dispatch and flush cycle, inside the host), one ping that engine wakers and service threads
//! share, the signal pipe, the configuration watch, and the host's own timers (key repeat,
//! layout grace periods, clipboard transfers). Pump deadlines and the session's save delay are
//! the loop's wait timeout, not timers of their own.
//!
//! One iteration, after the loop wakes (plan §19):
//!
//! 1. Collect ready input and completions: at most [`MAX_HOST_EVENTS`] host events a pass, with
//!    consecutive pointer motion over the same target coalesced to the latest; key transitions
//!    and everything else keep their order.
//! 2. Hand them to the core (`sighurt_core::update`, inside the session) and carry out its
//!    effects.
//! 3. Deliver engine commands (as effects) and pump every runtime that was called, woke, or
//!    whose deadline passed.
//! 4. Hand their events to the core after the pumps return.
//! 5. Rebuild the chrome's snapshot only when the core said the chrome changed; invalidate only
//!    the chrome surfaces it asks to repaint; apply a placement only when it differs from the
//!    last (or answers a configure).
//! 6. Present whatever the host has permits for: the chrome paints its surfaces, engines present
//!    their views. Presenting is a call, so the runtimes are pumped again; steps 1–6 repeat until
//!    a pass does nothing, at most [`MAX_PASSES`] times.
//! 7. Flush, and sleep until an event or a real deadline. With work left over, the next
//!    iteration starts without sleeping.
//!
//! The permit list, the placement built each refresh and the buffer-peak sample are reused
//! across passes, so a steady frame does not allocate them.
//!
//! Nothing ticks at a fixed rate: an idle browser sleeps until the compositor, an engine, a
//! service or a signal wakes it. Downloads and session writes run on threads of their own.
//!
//! The chrome is the one `[ui] chrome` names when `sig` starts ([`chrome::CHROMES`]), driven
//! through `sighurt_ui::extension::Chrome`: its surfaces are SHM targets by name
//! ([`ChromeTargets`](crate::chrome::ChromeTargets)), created when it first lists them.
//!
//! A view's target outlives the view while its engine still holds the lease (Servo keeps a
//! closing view until its GL context is free, and every lease after a crash). The host refuses
//! a placement that leaves out a live target, so such a target is placed hidden until its lease
//! drops (`place_retiring`).
//!
//! Shutdown (plan §20): stop taking input, cancel downloads, write the session (at once, whole),
//! close every view and shut every runtime down (each releases its target leases), wait a moment
//! for the downloads (deleting the files of those still waiting on a server), delete what
//! engines kept only for this run (`storage = "ephemeral"`), retire the chrome's leases, close
//! the window, and only then drop the host. If a runtime could not shut down (Servo after a
//! crash, decision 54), the frontend is asked to clean up and the process ends with `_exit`,
//! releasing nothing that runtime may still use. With `[session] restore` off, the session is
//! neither read nor written.
//!
//! Must not own: browser policy (the core) or which effect runs (the session). Drawing belongs
//! to the chrome and the engines.

use std::collections::{BTreeMap, VecDeque};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::ping::make_ping;
use calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction};
use sighurt_api::engine::{EngineCommand, EngineWaker};
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::{RequestId, SeatId, TargetId, ViewId, WindowId};
use sighurt_api::input::{InputEvent, InputKind, KeyboardFocus, PointerButton, PointerEvent};
use sighurt_api::services::{ClipboardData, Selection, ServiceError};
use sighurt_core::tabs::{Content, TabSummary};
use sighurt_core::{BrowserEvent, BrowserState, OpenRequest};
use sighurt_runtime::diagnostics::{Diagnostics, Report, TabReport, TargetReport};
use sighurt_runtime::frontend::Frontend;
use sighurt_runtime::persistence::{self, ConfigWatch, Paths, SessionWriter};
use sighurt_runtime::registry::State;
use sighurt_runtime::services::{Clipboard, Services};
use sighurt_runtime::session::{Session, SHUTDOWN_BOUND};
use sighurt_runtime::view::ViewEntry;
use sighurt_ui::extension::{
    Chrome, ChromeAction, ChromeEvent, SurfaceName, TextField, MAX_SURFACES,
};
use sighurt_ui::{Colors, EngineRow, HomeInfo, PixelBuffer, Snapshot};
use sighurt_wayland::{
    FramePermit, Host, HostEvent, InputRegion, Placement, PresentMode, TargetLease,
    TargetPlacement, WindowOptions,
};

use crate::chrome::{self, ChromeTargets, Placing};
use crate::engines;

/// Most host events handled in one pass.
pub const MAX_HOST_EVENTS: usize = 256;
/// Most passes of steps 1–6 in one iteration before the loop goes round without sleeping.
pub const MAX_PASSES: usize = 8;
/// How long a notice of `sig`'s own comes before the page's status text in the status strip.
const NOTICE_FIRST: Duration = Duration::from_secs(5);
/// The most clipboard text the address line takes.
const MAX_PASTE: usize = 1024 * 1024;
/// Clipboard types for text, most preferred first.
const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

/// What the command line asks the window to do.
pub struct Options {
    /// URLs and paths to open, as the core takes them.
    pub open: Vec<OpenRequest>,
    /// Where to write the diagnostics as JSON.
    pub diagnostics: Option<PathBuf>,
}

/// The Wayland window: the host, the chrome and the targets. The session is beside it, so the
/// two can be borrowed apart.
struct Gui {
    host: Host,
    window: WindowId,
    chrome: Box<dyn Chrome>,
    /// The chrome's name: the configuration's `[ui] chrome` when `sig` started.
    chrome_name: &'static str,
    /// The chrome's surfaces as targets, and their leases by name.
    chrome_targets: ChromeTargets,
    leases: BTreeMap<SurfaceName, TargetLease>,
    /// The chrome's latest text field for the input method, and the target it was set on.
    chrome_text: Option<TextField>,
    chrome_text_target: Option<TargetId>,
    /// The pointer's shape over the chrome, for targets made later.
    chrome_cursor: Cursor,
    /// How each engine's views present. Built with the registry, so the two name the same engines.
    modes: Vec<(&'static str, PresentMode)>,
    /// The window's size, from its latest configure.
    size: Option<LogicalSize>,
    /// The placement last applied, and whether the next must be applied even if equal.
    placed: Option<Placement>,
    /// Built every refresh and compared with `placed`. Cloned only when a placement is applied.
    placement_buf: Placement,
    /// The chrome half of `placement_buf`, reused each refresh.
    placing_buf: Placing,
    place_now: bool,
    snapshot_dirty: bool,
    /// The latest notice or download status, for the status strip.
    notice: Option<String>,
    /// Until when that notice comes before the page's own status text.
    notice_first: Option<Instant>,
    /// The chrome's clipboard read in flight.
    paste: Option<RequestId>,
    keyboard_target: Option<TargetId>,
    /// The target whose text field the input method has.
    page_text_target: Option<TargetId>,
    /// Targets of forgotten views whose leases their engines still hold, placed hidden.
    retiring: Vec<TargetId>,
}

/// The owner loop's state: the window and the session, and what the loop's sources report.
struct App {
    gui: Gui,
    session: Session<Gui>,
    config_changed: bool,
    signals: Vec<u8>,
    /// Permits for this pass. Reused so a steady frame does not allocate the list.
    permits: Vec<FramePermit>,
    /// Buffer peaks sampled after a presentation. Reused.
    buffer_scratch: Vec<(usize, usize, bool)>,
}

/// Runs the Wayland frontend until it quits.
pub fn run(options: Options, signals: OwnedFd) -> Result<ExitCode, String> {
    let mut event_loop: EventLoop<'static, App> =
        EventLoop::try_new().map_err(|e| format!("cannot create the event loop: {e}"))?;
    let handle = event_loop.handle();
    let host = Host::new(&handle, |app: &mut App| &mut app.gui.host).map_err(|e| e.to_string())?;
    let (ping, wake_source) = make_ping().map_err(|e| e.to_string())?;
    handle
        .insert_source(wake_source, |(), &mut (), _| {})
        .map_err(|e| e.error.to_string())?;
    let mut app = App::new(host, ping, options)?;
    // From here on every way out goes through `App::shutdown`, which releases in §20's order.
    match watch(&handle, signals, app.session.paths()) {
        Ok(()) => app.start(),
        Err(error) => app.fail(error),
    }
    let mut busy = true;
    while !app.session.has_quit() {
        let timeout = if busy {
            Some(Duration::ZERO)
        } else {
            app.timeout()
        };
        if let Err(error) = event_loop.dispatch(timeout, &mut app) {
            app.fail(format!("the event loop failed: {error}"));
            break;
        }
        if timeout != Some(Duration::ZERO) {
            app.session.diagnostics_mut().counters.wakeups += 1;
        }
        busy = app.iterate();
    }
    let code = app.shutdown();
    drop(event_loop);
    Ok(code)
}

/// Adds the signal pipe and the configuration watch to the loop.
fn watch(handle: &LoopHandle<'static, App>, signals: OwnedFd, paths: &Paths) -> Result<(), String> {
    let source = Generic::new(signals, Interest::READ, Mode::Level);
    handle
        .insert_source(source, |_, fd, app| {
            let mut bytes = [0u8; 16];
            while let Ok(n) = rustix::io::read(&*fd, &mut bytes) {
                if n == 0 {
                    break;
                }
                app.signals.extend_from_slice(&bytes[..n]);
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| e.error.to_string())?;
    let Some(config) = &paths.config else {
        return Ok(());
    };
    match ConfigWatch::new(config) {
        Ok(watch) => {
            let source = Generic::new(watch, Interest::READ, Mode::Level);
            handle
                .insert_source(source, |_, watch, app| {
                    app.config_changed |= watch.changed();
                    Ok(PostAction::Continue)
                })
                .map_err(|e| e.error.to_string())?;
        }
        // No directory yet: the file is read at startup only.
        Err(error) => log::info!("not watching {}: {error}", config.display()),
    }
    Ok(())
}

impl Clipboard for Gui {
    fn read(
        &mut self,
        request: RequestId,
        selection: Selection,
        mime_types: &[String],
        limit: usize,
    ) {
        self.host
            .clipboard_read(request, selection, mime_types, limit);
    }

    fn write(
        &mut self,
        selection: Selection,
        items: Vec<ClipboardData>,
    ) -> Result<(), ServiceError> {
        self.host.clipboard_write(selection, items)
    }

    fn cancel(&mut self, request: RequestId) {
        self.host.clipboard_cancel(request);
    }
}

impl Frontend for Gui {
    type Target = TargetLease;
    type Permit = FramePermit;

    fn open_target(
        &mut self,
        view: ViewId,
        engine: &str,
    ) -> Result<(TargetId, TargetLease), String> {
        let mode = self
            .modes
            .iter()
            .find(|(name, _)| *name == engine)
            .map(|(_, mode)| *mode)
            .ok_or_else(|| format!("the {engine} engine is not part of this build"))?;
        let lease = self
            .host
            .create_target(self.window, mode, Some(view))
            .map_err(|e| e.to_string())?;
        let id = lease.id();
        Ok((id, lease))
    }

    fn place(&mut self, views: &BTreeMap<ViewId, ViewEntry>, diag: &mut Diagnostics) -> bool {
        if self.fill_placement(views, diag).is_none() {
            return false;
        }
        if !self.place_now && self.placed.as_ref() == Some(&self.placement_buf) {
            return false;
        }
        let placement = self.placement_buf.clone();
        match self.host.apply_placement(placement) {
            Ok(()) => {
                diag.counters.placements += 1;
                self.placed = Some(self.placement_buf.clone());
                self.place_now = false;
                true
            }
            Err(error) => {
                // A stale size means a newer configure is queued; it brings its own placement.
                diag.counters.placement_errors += 1;
                diag.error(format!("placement refused: {error}"));
                false
            }
        }
    }

    fn forget_target(&mut self, target: TargetId) {
        // Listed, hidden, until the engine drops the lease.
        self.retiring.push(target);
        if self.page_text_target == Some(target) {
            self.host.set_text_input(self.window, target, None);
            self.page_text_target = None;
        }
        if self.keyboard_target == Some(target) {
            self.host.set_keyboard_focus(self.window, None);
            self.keyboard_target = None;
        }
    }

    fn focus(&mut self, focus: KeyboardFocus, queue: &mut VecDeque<BrowserEvent>) {
        let actions = self.chrome.handle(ChromeEvent::Focus(focus));
        self.chrome_actions(queue, actions);
    }

    fn chrome_input(&mut self, event: InputEvent, queue: &mut VecDeque<BrowserEvent>) {
        let event = match event.kind {
            InputKind::Key(key) => ChromeEvent::Key(key),
            InputKind::TextInput(text) => ChromeEvent::TextInput(text),
            // Window focus reached the chrome already, whoever has the keyboard.
            InputKind::Focus(_) | InputKind::Pointer(_) => return,
        };
        let actions = self.chrome.handle(event);
        self.chrome_actions(queue, actions);
    }

    fn chrome_command(&mut self, action: String, queue: &mut VecDeque<BrowserEvent>) {
        match chrome::command(&chrome::chrome(self.chrome_name), action) {
            Ok(event) => {
                let actions = self.chrome.handle(event);
                self.chrome_actions(queue, actions);
            }
            Err(notice) => self.show_notice(notice),
        }
    }

    fn page_cursor(&mut self, target: Option<TargetId>, cursor: Cursor) {
        if let Some(target) = target {
            self.host.set_cursor(target, cursor);
        }
    }

    fn page_text_input(
        &mut self,
        shown: Option<TargetId>,
        state: Option<sighurt_api::input::TextInputState>,
    ) {
        let target = match &state {
            Some(_) => shown,
            None => self.page_text_target,
        };
        if let Some(target) = target {
            self.host.set_text_input(self.window, target, state.clone());
            self.page_text_target = state.map(|_| target);
        }
    }

    fn notice(&mut self, message: String) {
        self.show_notice(message);
    }

    fn redraw(&mut self) {
        self.snapshot_dirty = true;
    }

    fn show(&mut self, _view: ViewId, _visible: bool) {
        // `ViewEntry::visible` is already updated. The next `place` maps or unmaps the target,
        // which is when the core's show and hide took effect before.
    }

    fn frame_ready(&mut self, target: TargetId) {
        self.host.invalidate(target);
    }

    fn before_forced_exit(&mut self) {
        // Wayland is released by dropping the host, and a leaked engine cannot allow that
        // (decision 54): its threads may still use the connection. Nothing to restore. The
        // terminal frontend restores the terminal here, before `_exit` skips destructors.
    }
}

impl Gui {
    /// Shows `message` in the status strip until another replaces it, for [`NOTICE_FIRST`]
    /// before anything the page says there. Not logged: notices can hold page data (an origin,
    /// a URL, a downloaded file's name).
    fn show_notice(&mut self, message: String) {
        self.notice = Some(message);
        self.notice_first = Some(Instant::now() + NOTICE_FIRST);
        self.snapshot_dirty = true;
    }

    /// Carries out what the chrome asks for.
    fn chrome_actions(&mut self, queue: &mut VecDeque<BrowserEvent>, actions: Vec<ChromeAction>) {
        for action in actions {
            match action {
                ChromeAction::Command(command) => queue.push_back(BrowserEvent::Command(command)),
                ChromeAction::SelectTab(tab) => queue.push_back(BrowserEvent::SelectTab(tab)),
                ChromeAction::CloseTab(tab) => queue.push_back(BrowserEvent::CloseTab(tab)),
                ChromeAction::Focus(focus) => queue.push_back(BrowserEvent::Focus(focus)),
                ChromeAction::Copy(text) => {
                    let item = ClipboardData {
                        mime_type: TEXT_TYPES[0].into(),
                        data: text.into_bytes(),
                    };
                    if let Err(error) = self.host.clipboard_write(Selection::Clipboard, vec![item])
                    {
                        self.show_notice(format!("Copying failed: {error}"));
                    }
                }
                ChromeAction::Paste(request) => {
                    if let Some(old) = self.paste.replace(request) {
                        self.host.clipboard_cancel(old);
                    }
                    let types = TEXT_TYPES.map(String::from);
                    self.host
                        .clipboard_read(request, Selection::Clipboard, &types, MAX_PASTE);
                }
                ChromeAction::TextInput(field) => {
                    self.chrome_text = field;
                    self.sync_chrome_text();
                }
                ChromeAction::Cursor(cursor) => {
                    self.chrome_cursor = cursor;
                    for lease in self.leases.values() {
                        self.host.set_cursor(lease.id(), cursor);
                    }
                }
                ChromeAction::Repaint(surface) => {
                    if let Some(target) = self.target_of(surface) {
                        self.host.invalidate(target);
                    }
                }
            }
        }
    }

    /// Gives the host the chrome's text field, on its surface's target once that has one, and
    /// takes it from the target it was on before.
    fn sync_chrome_text(&mut self) {
        let wanted = self
            .chrome_text
            .as_ref()
            .and_then(|field| self.target_of(field.surface));
        if let Some(old) = self.chrome_text_target.filter(|&old| Some(old) != wanted) {
            self.host.set_text_input(self.window, old, None);
        }
        if let Some(target) = wanted {
            let state = self.chrome_text.as_ref().map(|field| field.state.clone());
            self.host.set_text_input(self.window, target, state);
        }
        self.chrome_text_target = wanted;
    }

    /// The window's layout for the host: every view (only the shown one visible) at the page
    /// rectangle, the targets of forgotten views still leased (hidden), the chrome's surfaces
    /// above them in its order. A surface name the chrome hasn't used before gets a target here.
    /// Fills `placement_buf` from `views` and the chrome. `None` until the window has a size.
    fn fill_placement(
        &mut self,
        views: &BTreeMap<ViewId, ViewEntry>,
        diag: &mut Diagnostics,
    ) -> Option<()> {
        let size = self.size?;
        let window = self.window;
        self.chrome_targets
            .place_into(self.chrome.layout(), size, &mut self.placing_buf);
        for &name in &self.placing_buf.refused {
            diag.error(format!(
                "the chrome uses more than {MAX_SURFACES} surfaces: {name} is not shown"
            ));
        }
        let page = self.placing_buf.page;
        self.placement_buf.window = window;
        self.placement_buf.size = size;
        self.placement_buf.targets.clear();
        for entry in views.values() {
            self.placement_buf.targets.push(TargetPlacement {
                target: entry.target,
                rect: page,
                visible: entry.visible,
                input: InputRegion::Whole,
            });
        }
        let host = &self.host;
        place_retiring(
            &mut self.retiring,
            |target| host.is_live(target),
            page,
            &mut self.placement_buf.targets,
        );
        let surfaces = self.placing_buf.surfaces.len();
        let mut created = false;
        for index in 0..surfaces {
            let name = self.placing_buf.surfaces[index].name;
            let rect = self.placing_buf.surfaces[index].rect;
            let visible = self.placing_buf.surfaces[index].visible;
            let input = self.placing_buf.surfaces[index].input.clone();
            let target = match self.leases.get(name) {
                Some(lease) => lease.id(),
                None => match self.host.create_target(self.window, PresentMode::Shm, None) {
                    Ok(lease) => {
                        let target = lease.id();
                        self.host.set_cursor(target, self.chrome_cursor);
                        // Its first frame, whatever the chrome asked to repaint before.
                        self.host.invalidate(target);
                        self.leases.insert(name, lease);
                        created = true;
                        target
                    }
                    Err(error) => {
                        diag.error(format!("no target for the chrome's {name}: {error}"));
                        continue;
                    }
                },
            };
            self.placement_buf.targets.push(TargetPlacement {
                target,
                rect,
                visible,
                input,
            });
        }
        if created {
            self.sync_chrome_text();
        }
        Some(())
    }

    /// Gives the host's logical keyboard focus to whatever the core says has the keyboard: the
    /// chrome's surface with its text field (or its first surface), the shown view, or the
    /// chrome's surface at the page's rectangle while it draws the page.
    fn sync_keyboard_focus(&mut self, focus: KeyboardFocus, shown: Option<TargetId>) {
        let chrome_text = self.chrome_text_target;
        let wanted = {
            let layout = self.chrome.layout();
            let leases = &self.leases;
            let listed = |rect: Option<WindowRect>| {
                layout
                    .surfaces
                    .iter()
                    .filter(|surface| rect.is_none_or(|rect| surface.rect == rect))
                    .find_map(|surface| leases.get(surface.name).map(TargetLease::id))
            };
            match focus {
                KeyboardFocus::Chrome => chrome_text.or_else(|| listed(None)),
                KeyboardFocus::Page => shown.or_else(|| {
                    let page = layout.page;
                    listed(Some(page).filter(|page| !page.size().is_empty()))
                }),
            }
        };
        if wanted != self.keyboard_target {
            self.keyboard_target = wanted;
            self.host.set_keyboard_focus(self.window, wanted);
        }
    }

    /// The chrome's surface that `target` is.
    fn surface_of(&self, target: TargetId) -> Option<SurfaceName> {
        self.leases
            .iter()
            .find(|(_, lease)| lease.id() == target)
            .map(|(&name, _)| name)
    }

    /// The target of the chrome's surface `surface`, once it has one.
    fn target_of(&self, surface: SurfaceName) -> Option<TargetId> {
        self.leases.get(surface).map(TargetLease::id)
    }
}

impl App {
    fn new(mut host: Host, ping: calloop::ping::Ping, options: Options) -> Result<Self, String> {
        let paths = Paths::from_env();
        let (mut registry, modes) = engines::build();
        let env = registry.environment(chrome::checks_all(), &Colors::NAMES);
        // A configuration file that can't be used stops sig before anything starts.
        let config = persistence::startup_config(paths.config.as_deref(), &env)?;
        let servo_wake = ping.clone();
        registry.start_at_launch(&config, || EngineWaker::new(move || servo_wake.ping()));
        // Servo's logger, if it started; otherwise env_logger. The ping's clones, in the
        // wakers below, are what keep it alive.
        sighurt_runtime::registry::install_logger();
        let kind = chrome::chrome(config.chrome());
        let mut chrome = (kind.new)();
        let window = WindowId::next();
        let bg = chrome::theme(&config).colors.bg;
        host.create_window(
            window,
            WindowOptions {
                title: "Sighurt".into(),
                app_id: "sighurt".into(),
                background: [bg.r, bg.g, bg.b],
                ..WindowOptions::default()
            },
        );
        let configured = chrome.handle(chrome::configure(&config, kind.name));
        let session_writer = match &paths.session {
            Some(path) => match SessionWriter::new(path.clone()) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    log::warn!("cannot save the session: {error}");
                    None
                }
            },
            None => None,
        };
        let services_wake = ping.clone();
        let services = Services::new(move || services_wake.ping());
        let wake = std::sync::Arc::new(move || ping.ping());
        let diag = Diagnostics::new(options.diagnostics);
        let mut session = Session::new(
            BrowserState::new(config, env.clone()),
            env,
            registry,
            services,
            paths,
            session_writer,
            diag,
            wake,
        );
        let mut gui = Gui {
            host,
            window,
            chrome,
            chrome_name: kind.name,
            chrome_targets: ChromeTargets::default(),
            leases: BTreeMap::new(),
            chrome_text: None,
            chrome_text_target: None,
            chrome_cursor: Cursor::Default,
            modes,
            size: None,
            placed: None,
            placement_buf: Placement {
                window,
                size: LogicalSize::default(),
                targets: Vec::new(),
            },
            placing_buf: Placing::default(),
            place_now: false,
            snapshot_dirty: true,
            notice: None,
            notice_first: None,
            paste: None,
            keyboard_target: None,
            page_text_target: None,
            retiring: Vec::new(),
        };
        gui.chrome_actions(session.queue_mut(), configured);
        session.queue_mut().push_back(BrowserEvent::Start {
            session: None,
            open: options.open,
        });
        Ok(Self {
            gui,
            session,
            config_changed: false,
            signals: Vec::new(),
            permits: Vec::new(),
            buffer_scratch: Vec::new(),
        })
    }

    /// Restores the session, unless `[session] restore` is off, and opens the command line's
    /// pages.
    fn start(&mut self) {
        let restore = self.session.core().config().restore_session();
        let path = self.session.paths().session.as_deref().filter(|_| restore);
        let restored = match path.map(persistence::load_session) {
            Some(Ok(session)) => session,
            Some(Err(error)) => {
                self.gui
                    .show_notice(format!("The saved tabs can't be restored: {error}"));
                None
            }
            None => None,
        };
        if let Some(BrowserEvent::Start { session: slot, .. }) =
            self.session.queue_mut().front_mut()
        {
            *slot = restored;
        }
        self.session.run_core(&mut self.gui);
    }

    /// One iteration of the loop, after it woke. Returns whether work is left that shouldn't
    /// wait: the next iteration then starts at once.
    fn iterate(&mut self) -> bool {
        self.session.diagnostics_mut().counters.iterations += 1;
        self.handle_signals();
        if std::mem::take(&mut self.config_changed) {
            self.reload_config();
        }
        if self
            .gui
            .notice_first
            .is_some_and(|until| until <= Instant::now())
        {
            self.gui.notice_first = None;
            self.gui.snapshot_dirty = true;
        }
        let mut busy = true;
        for _ in 0..MAX_PASSES {
            let mut worked = self.drain_host_events();
            worked |= self.session.service_completions(&mut self.gui);
            worked |= self.session.pump(&mut self.gui);
            worked |= self.refresh_chrome();
            worked |= self.present();
            if !worked {
                busy = false;
                break;
            }
            if self.session.has_quit() {
                break;
            }
        }
        if self
            .session
            .session_due()
            .is_some_and(|due| due <= Instant::now())
        {
            self.session.save_session();
        }
        self.gui.host.flush();
        busy && !self.session.has_quit()
    }

    /// How long the loop may sleep: until the earliest pump deadline, session save or notice
    /// giving way to the page's status, or until something happens.
    fn timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let pump = match self.session.registry().next_pump() {
            Some(None) => return Some(Duration::ZERO),
            Some(Some(at)) => Some(at),
            None => None,
        };
        let next = [pump, self.session.session_due(), self.gui.notice_first]
            .into_iter()
            .flatten()
            .min();
        next.map(|at| at.saturating_duration_since(now))
    }

    /// `sig` can't go on: reports `error` and quits with a failure.
    fn fail(&mut self, error: String) {
        self.session.diagnostics_mut().error(error);
        self.session.set_quit(ExitCode::FAILURE);
    }

    fn handle_signals(&mut self) {
        for signal in std::mem::take(&mut self.signals) {
            match i32::from(signal) {
                libc::SIGUSR1 => report(&self.gui, &mut self.session, false),
                libc::SIGTERM | libc::SIGINT => {
                    self.session.queue_mut().push_back(BrowserEvent::Command(
                        sighurt_api::commands::Command::BrowserQuit,
                    ));
                    self.session.run_core(&mut self.gui);
                }
                _ => {}
            }
        }
    }

    /// Handles the host's events, at most [`MAX_HOST_EVENTS`]. Returns whether there were any.
    fn drain_host_events(&mut self) -> bool {
        let mut batch: Vec<HostEvent> = Vec::new();
        while batch.len() < MAX_HOST_EVENTS {
            let Some(event) = self.gui.host.next_event() else {
                break;
            };
            if push_coalesced(&mut batch, event) {
                self.session.diagnostics_mut().counters.motions_coalesced += 1;
            }
        }
        let peaks = &mut self.session.diagnostics_mut().peaks;
        peaks.max_host_queue = peaks.max_host_queue.max(batch.len() as u64);
        let worked = !batch.is_empty();
        for event in batch {
            self.host_event(event);
            self.session.run_core(&mut self.gui);
        }
        worked
    }

    fn host_event(&mut self, event: HostEvent) {
        let at = Instant::now();
        match event {
            HostEvent::WindowConfigured { size, .. } => {
                self.gui.size = Some(size);
                let actions = self.gui.chrome.handle(ChromeEvent::Resize(size));
                self.gui.chrome_actions(self.session.queue_mut(), actions);
                self.gui.place_now = true;
            }
            HostEvent::CloseRequested { .. } => self.session.queue_mut().push_back(
                BrowserEvent::Command(sighurt_api::commands::Command::BrowserQuit),
            ),
            HostEvent::MetricsChanged { target, metrics } => {
                if let Some(view) = self.session.view_of(target) {
                    self.session
                        .engine_command(view, EngineCommand::SetMetrics(metrics), false);
                }
            }
            HostEvent::Input { target, event, .. } => {
                self.session.diagnostics_mut().counters.input_events += 1;
                self.input(target, event, at);
            }
            HostEvent::ClipboardRead { request, result } => {
                if self.gui.paste == Some(request) {
                    self.gui.paste = None;
                    if let Ok(data) = result {
                        let text = String::from_utf8_lossy(&data.data).into_owned();
                        let actions = self
                            .gui
                            .chrome
                            .handle(ChromeEvent::Pasted { request, text });
                        self.gui.chrome_actions(self.session.queue_mut(), actions);
                    }
                } else {
                    self.session.clipboard_done(request, result);
                }
            }
            HostEvent::ConnectionLost { error } => {
                self.fail(format!("the Wayland connection is gone: {error}"))
            }
        }
    }

    /// Routes input: pointer input to the chrome surface or page it is over, except the mouse's
    /// side buttons, which go to the core wherever the pointer is (`[input.buttons]`); keys,
    /// input-method text and window focus to the core, which binds keys and hands the rest to
    /// the page or the chrome.
    fn input(&mut self, target: Option<TargetId>, event: InputEvent, at: Instant) {
        match &event.kind {
            InputKind::Pointer(pointer) => {
                let side = matches!(
                    pointer,
                    PointerEvent::Button {
                        button: PointerButton::Back | PointerButton::Forward,
                        ..
                    }
                );
                if side {
                    let view = target.and_then(|t| self.session.view_of(t));
                    self.session
                        .queue_mut()
                        .push_back(BrowserEvent::Button { view, event, at });
                } else if let Some(surface) = target.and_then(|t| self.gui.surface_of(t)) {
                    let event = pointer.clone();
                    let actions = self
                        .gui
                        .chrome
                        .handle(ChromeEvent::Pointer { surface, event });
                    self.gui.chrome_actions(self.session.queue_mut(), actions);
                } else if let Some(view) = target.and_then(|t| self.session.view_of(t)) {
                    self.session
                        .queue_mut()
                        .push_back(BrowserEvent::Pointer { view, event, at });
                }
            }
            InputKind::Focus(focus) => {
                let actions = self.gui.chrome.handle(ChromeEvent::WindowFocus(*focus));
                self.gui.chrome_actions(self.session.queue_mut(), actions);
                self.session
                    .queue_mut()
                    .push_back(BrowserEvent::Keyboard { event, at });
            }
            InputKind::Key(_) | InputKind::TextInput(_) => self
                .session
                .queue_mut()
                .push_back(BrowserEvent::Keyboard { event, at }),
        }
    }

    /// Brings the chrome, the placement and the keyboard focus up to date. Returns whether
    /// anything changed.
    fn refresh_chrome(&mut self) -> bool {
        let mut worked = false;
        // The chrome's actions can change the core, which can change the snapshot again.
        for _ in 0..4 {
            if !std::mem::take(&mut self.gui.snapshot_dirty) {
                break;
            }
            let snapshot = self.snapshot();
            let actions = self.gui.chrome.handle(ChromeEvent::Snapshot(snapshot));
            self.gui.chrome_actions(self.session.queue_mut(), actions);
            self.session.run_core(&mut self.gui);
            worked = true;
            // A navigation whose tab the user left is no longer waited for.
            self.session.release_inactive_navigations();
        }
        worked |= self.session.place(&mut self.gui);
        let shown = self.session.shown_view().map(|(_, entry)| entry.target);
        let focus = self.session.core().focus();
        self.gui.sync_keyboard_focus(focus, shown);
        worked
    }

    /// What the chrome shows: the core's tabs, the status line and the home page, contained.
    fn snapshot(&self) -> Snapshot {
        let status = |tab: &TabSummary<'_>| self.status(tab).map(|(_, text)| text.to_owned());
        chrome::snapshot(self.session.core(), self.home_info(), status)
    }

    /// What `sig://home` says: the configuration file and the engines of this build.
    fn home_info(&self) -> HomeInfo {
        let listed: Vec<_> = self
            .session
            .registry()
            .engines()
            .map(|engine| {
                let failed = match engine.state() {
                    State::Failed(why) => Some(why.to_owned()),
                    _ => None,
                };
                (engine.name, engine.renderer, failed)
            })
            .collect();
        let config_path = self
            .session
            .paths()
            .config
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());
        let config = self.session.core().config();
        let engines = listed
            .into_iter()
            .map(|(name, renderer, failed)| {
                let mut detail = vec![format!("compiled in, {renderer}")];
                if name == config.default_engine() {
                    detail.push("default".into());
                }
                let enabled = config.is_enabled(name);
                if !enabled {
                    detail.push("disabled in the configuration".into());
                }
                if let Some(why) = &failed {
                    detail.push(format!("failed: {why}"));
                }
                EngineRow {
                    name: name.to_owned(),
                    detail: detail.join(" · "),
                    available: enabled && failed.is_none(),
                }
            })
            .collect();
        HomeInfo {
            config_path,
            engines,
        }
    }

    /// What the status strip shows for `tab`, and where it comes from. A tab's error is shown
    /// there unless the page area shows it (a tab with nothing else to show).
    fn status<'a>(&'a self, tab: &TabSummary<'a>) -> Option<(&'static str, &'a str)> {
        let error = tab
            .error
            .filter(|_| !matches!(tab.content, Content::Blank))
            .map(|e| e.message.as_str());
        status_line(
            self.gui.notice.as_deref(),
            self.gui.notice_first.is_some(),
            tab.status,
            error,
        )
    }

    /// Hands each permit the host issues to its presenter. Returns whether there were any.
    fn present(&mut self) -> bool {
        let mut permits = std::mem::take(&mut self.permits);
        self.gui.host.fill_permits(&mut permits);
        let worked = !permits.is_empty();
        for permit in permits.drain(..) {
            let target = permit.target();
            if let Some(surface) = self.gui.surface_of(target) {
                self.paint_chrome(surface, permit);
            } else if let Some(view) = self.session.view_of(target) {
                let size = permit.metrics().buffer();
                let _ = self.session.present(view, permit, size);
            }
            // Otherwise the target is being retired: dropping the permit presents nothing.
        }
        self.permits = permits;
        if worked {
            // Buffers are allocated while presenting, so this sees every peak.
            self.record_buffers();
        }
        worked
    }

    /// Samples each target's buffer peaks into the reused scratch list.
    fn record_buffers(&mut self) {
        self.buffer_scratch.clear();
        let mut bytes = 0usize;
        let leases = &self.gui.leases;
        let scratch = &mut self.buffer_scratch;
        self.gui.host.for_each_buffer(|id, stats| {
            let chrome = leases.values().any(|lease| lease.id() == id);
            scratch.push((stats.buffers, stats.retiring_generations, chrome));
            bytes += stats.bytes;
        });
        self.session
            .diagnostics_mut()
            .buffers(self.buffer_scratch.iter().copied(), bytes);
    }

    fn paint_chrome(&mut self, surface: SurfaceName, permit: FramePermit) {
        let metrics = permit.metrics();
        let mut frame = match permit.shm_frame() {
            Ok(frame) => frame,
            Err(error) => {
                self.session
                    .diagnostics_mut()
                    .error(format!("no buffer for the chrome's {surface}: {error}"));
                return;
            }
        };
        let (size, stride) = (frame.size(), frame.stride());
        let started = Instant::now();
        let buffer = PixelBuffer {
            data: frame.pixels(),
            width: size.width,
            height: size.height,
            stride,
        };
        match self.gui.chrome.paint(surface, &metrics, buffer) {
            Ok(()) => {
                frame.submit();
                self.session
                    .diagnostics_mut()
                    .chrome(surface, started.elapsed());
            }
            // The buffer goes back unsubmitted.
            Err(error) => self
                .session
                .diagnostics_mut()
                .error(format!("painting the chrome's {surface} failed: {error}")),
        }
    }

    fn reload_config(&mut self) {
        self.session.diagnostics_mut().counters.config_reloads += 1;
        match persistence::load_config(
            self.session.paths().config.as_deref(),
            self.session.environment(),
        ) {
            Ok(config) => {
                // Saving the session again: the tabs as they are now go to the file soon.
                if config.restore_session() && !self.session.core().config().restore_session() {
                    self.session.session_changed();
                }
                let actions = self
                    .gui
                    .chrome
                    .handle(chrome::configure(&config, self.gui.chrome_name));
                self.gui.chrome_actions(self.session.queue_mut(), actions);
                self.session
                    .queue_mut()
                    .push_back(BrowserEvent::ConfigReloaded(Ok(Box::new(config))));
                self.gui.notice_first = None;
                if self.gui.notice.take().is_some() {
                    self.gui.snapshot_dirty = true;
                }
            }
            Err(persistence::LoadError::Config(error)) => {
                log::warn!("the configuration in use stays: {error}");
                self.session
                    .queue_mut()
                    .push_back(BrowserEvent::ConfigReloaded(Err(error)));
            }
            Err(persistence::LoadError::Other(message)) => {
                log::warn!("the configuration in use stays: {message}");
                self.gui
                    .show_notice(format!("{message}\nThe configuration in use stays."))
            }
        }
        self.gui.snapshot_dirty = true;
        self.session.run_core(&mut self.gui);
    }

    /// Shuts down in the order of plan §20 and returns the exit code. [`Session::shutdown`]
    /// cancels downloads, writes the session, writes the final report, closes the views, shuts
    /// the engines and services down and deletes scratch. If an engine could not shut down
    /// (decision 54), that ends the process with `_exit` after this window is asked to clean
    /// up, and the rest of this function does not run: dropping the window would release
    /// Wayland objects the engine may still use. Otherwise this window drops its leases, closes
    /// itself and drops the host.
    fn shutdown(mut self) -> ExitCode {
        // A page can keep Servo from ever shutting down; the window must not stay up for it.
        self.session.set_shutdown_bound(SHUTDOWN_BOUND);
        let code = self
            .session
            .shutdown(&mut self.gui, |session, gui| report(gui, session, true));
        let leases = std::mem::take(&mut self.gui.leases);
        drop(leases);
        let window = self.gui.window;
        self.gui.host.close_window(window);
        self.gui.host.flush();
        drop(self.gui.host);
        code
    }
}

/// Lists the `retiring` targets that are still `live` in `out`, hidden at the page rectangle,
/// and forgets the others. A view's target can outlive the view: Servo keeps a closing view's
/// lease until its GL context is free, and leaks every lease after a crash. The host refuses a
/// placement without a live target, so each stays listed, hidden (unmapped, never waited
/// for), until its lease drops; leaving it out would refuse every placement until then.
fn place_retiring(
    retiring: &mut Vec<TargetId>,
    live: impl Fn(TargetId) -> bool,
    page: WindowRect,
    out: &mut Vec<TargetPlacement>,
) {
    retiring.retain(|&target| live(target));
    out.extend(retiring.iter().map(|&target| TargetPlacement {
        target,
        rect: page,
        visible: false,
        input: InputRegion::Whole,
    }));
}

/// The diagnostics report. Called on SIGUSR1 and, from [`Session::shutdown`], at exit, while
/// the runtimes can still say how they render.
fn report(gui: &Gui, session: &mut Session<Gui>, at_exit: bool) {
    // Readbacks made since the engines' last frames, each as big as the page area.
    let scale = gui
        .leases
        .values()
        .next()
        .map(|lease| lease.metrics().scale());
    let page = scale.map(|scale| scale.buffer_size(gui.chrome.layout().page.size()));
    let page_bytes = page.map_or(0, |b| u64::from(b.width) * u64::from(b.height) * 4);
    session.account_readbacks(page_bytes);
    let host = gui.host.diagnostics();
    let targets = host
        .targets
        .iter()
        .map(|t| {
            let (role, engine, tab) = match (gui.surface_of(t.target), session.view_of(t.target)) {
                (Some(surface), _) => (surface, None, None),
                (None, Some(view)) => {
                    let (engine, tab) = session.view_report(view);
                    ("view", Some(engine), tab)
                }
                (None, None) => ("retiring", None, None),
            };
            let (rect, buffer) = (t.metrics.rect(), t.metrics.buffer());
            TargetReport {
                role,
                engine,
                tab,
                width: rect.width,
                height: rect.height,
                buffer_width: buffer.width,
                buffer_height: buffer.height,
                scale: t.metrics.scale().numerator(),
                mapped: t.mapped,
                shown: t.shown,
                awaiting_callback: t.awaiting_callback,
                permits: t.stats.permits,
                submissions: t.stats.submissions,
                frame_callbacks: t.stats.frame_callbacks,
                releases: t.stats.releases,
                presented: t.stats.presented,
                discarded: t.stats.discarded,
                late: t.stats.late,
                buffers: t.buffers.buffers,
                buffer_bytes: t.buffers.bytes,
                retiring_generations: t.buffers.retiring_generations,
                retiring_bytes: t.buffers.retiring_bytes,
            }
        })
        .collect();
    let engines = session.engine_reports();
    let tabs = {
        let core = session.core();
        core.tabs()
            .map(|tab| TabReport {
                loading: tab.loading,
                can_go_back: tab.can_go_back,
                can_go_forward: tab.can_go_forward,
                reloads: tab.reloads,
                error: tab.error.is_some(),
                engine: chrome::engine(core, &tab).map(str::to_owned),
            })
            .collect()
    };
    let active_tab = session.core().active() + 1;
    let status = {
        let core = session.core();
        let index = core.active();
        core.tabs().nth(index).and_then(|tab| {
            let error = tab
                .error
                .filter(|_| !matches!(tab.content, Content::Blank))
                .map(|error| error.message.as_str());
            status_line(
                gui.notice.as_deref(),
                gui.notice_first.is_some(),
                tab.status,
                error,
            )
            .map(|(from, _)| from)
        })
    };
    let counters = session.diagnostics().counters.clone();
    let peaks = session.diagnostics().peaks.clone();
    let last_error = session.diagnostics().last_error();
    let indicator = session.core().keymap_indicator().map(str::to_owned);
    session.diagnostics_mut().keymap_indicator = indicator;
    let report = Report {
        counters,
        peaks,
        targets,
        engines,
        tabs,
        active_tab,
        status,
        capabilities: format!("{:?}", host.capabilities),
        pending_host_events: host.pending_events,
        last_error,
    };
    session.diagnostics_mut().report(&report, at_exit);
}

/// What the status strip shows, and where it comes from: a notice of `sig`'s own while it is
/// `fresh`, so no page can hide it; then the page's status text (its hovered link), then why a
/// navigation failed while the page stayed, then an older notice.
fn status_line<'a>(
    notice: Option<&'a str>,
    fresh: bool,
    page: Option<&'a str>,
    error: Option<&'a str>,
) -> Option<(&'static str, &'a str)> {
    let notice = notice.map(|text| ("notice", text));
    let rest = page
        .map(|text| ("page", text))
        .or(error.map(|text| ("error", text)));
    if fresh {
        notice.or(rest)
    } else {
        rest.or(notice)
    }
}

/// Adds `event` to `batch`, or replaces the batch's last event with it when both are pointer
/// motion over the same target from the same seat (step 1 of plan §19): only the latest
/// position matters. Anything else, a button or key in between included, keeps its place and
/// order. Returns whether it replaced an event.
fn push_coalesced(batch: &mut Vec<HostEvent>, event: HostEvent) -> bool {
    if let (Some(last), Some(motion)) = (batch.last_mut(), motion_of(&event)) {
        if motion_of(last) == Some(motion) {
            *last = event;
            return true;
        }
    }
    batch.push(event);
    false
}

/// The target and seat of a pointer motion event, for coalescing.
fn motion_of(event: &HostEvent) -> Option<(Option<TargetId>, SeatId)> {
    match event {
        HostEvent::Input {
            target,
            event:
                InputEvent {
                    seat,
                    kind: InputKind::Pointer(PointerEvent::Motion { .. }),
                },
            ..
        } => Some((*target, *seat)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use sighurt_api::geometry::SurfacePoint;
    use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, Modifiers, PointerButton};

    use super::*;

    fn input(target: TargetId, seat: SeatId, pointer: PointerEvent) -> HostEvent {
        HostEvent::Input {
            window: WindowId::next(),
            target: Some(target),
            event: InputEvent {
                seat,
                kind: InputKind::Pointer(pointer),
            },
        }
    }

    fn motion(target: TargetId, seat: SeatId, x: f64) -> HostEvent {
        let position = SurfacePoint { x, y: 0.0 };
        let modifiers = Modifiers::default();
        input(
            target,
            seat,
            PointerEvent::Motion {
                position,
                modifiers,
            },
        )
    }

    fn press(target: TargetId, seat: SeatId) -> HostEvent {
        input(
            target,
            seat,
            PointerEvent::Button {
                button: PointerButton::Left,
                state: ButtonState::Pressed,
                position: SurfacePoint::default(),
                modifiers: Modifiers::default(),
            },
        )
    }

    /// The x of each motion in `batch`, and `None` for anything else.
    fn xs(batch: &[HostEvent]) -> Vec<Option<f64>> {
        batch
            .iter()
            .map(|event| match event {
                HostEvent::Input {
                    event:
                        InputEvent {
                            kind: InputKind::Pointer(PointerEvent::Motion { position, .. }),
                            ..
                        },
                    ..
                } => Some(position.x),
                _ => None,
            })
            .collect()
    }

    fn coalesce(events: Vec<HostEvent>) -> (Vec<HostEvent>, usize) {
        let mut batch = Vec::new();
        let dropped = events
            .into_iter()
            .map(|event| push_coalesced(&mut batch, event))
            .filter(|&replaced| replaced)
            .count();
        (batch, dropped)
    }

    #[test]
    fn a_forgotten_views_target_stays_placed_hidden_until_its_lease_drops() {
        let (closing, dropped) = (TargetId::next(), TargetId::next());
        let page = WindowRect {
            x: 0,
            y: 40,
            width: 800,
            height: 560,
        };
        let mut retiring = vec![closing, dropped];
        let mut out = Vec::new();
        // Servo parked `closing` with its lease; the other engine dropped its lease at once.
        place_retiring(&mut retiring, |t| t == closing, page, &mut out);
        assert_eq!(retiring, vec![closing]);
        assert_eq!(
            out,
            vec![TargetPlacement {
                target: closing,
                rect: page,
                visible: false,
                input: InputRegion::Whole,
            }]
        );
        // Its lease dropped: it leaves the placement.
        out.clear();
        place_retiring(&mut retiring, |_| false, page, &mut out);
        assert!(retiring.is_empty() && out.is_empty());
    }

    #[test]
    fn motion_over_one_target_collapses_to_the_latest() {
        let (a, seat) = (TargetId::next(), SeatId::next());
        let (batch, dropped) = coalesce((1..=5).map(|x| motion(a, seat, x.into())).collect());
        assert_eq!((xs(&batch), dropped), (vec![Some(5.0)], 4));
    }

    #[test]
    fn a_button_between_motions_keeps_its_place() {
        let (a, seat) = (TargetId::next(), SeatId::next());
        let events = vec![
            motion(a, seat, 1.0),
            motion(a, seat, 2.0),
            press(a, seat),
            motion(a, seat, 3.0),
            motion(a, seat, 4.0),
        ];
        let (batch, dropped) = coalesce(events);
        assert_eq!(xs(&batch), [Some(2.0), None, Some(4.0)]);
        assert_eq!(dropped, 2);
    }

    #[test]
    fn a_key_between_motions_keeps_its_place() {
        let (a, seat) = (TargetId::next(), SeatId::next());
        let key = HostEvent::Input {
            window: WindowId::next(),
            target: None,
            event: InputEvent {
                seat,
                kind: InputKind::Key(KeyEvent {
                    code: KeyCode(30),
                    key: Key::Character("a".into()),
                    state: ButtonState::Pressed,
                    repeat: false,
                    modifiers: Modifiers::default(),
                    consumed: Modifiers::default(),
                    text: Some("a".into()),
                }),
            },
        };
        let (batch, _) = coalesce(vec![motion(a, seat, 1.0), key, motion(a, seat, 2.0)]);
        assert_eq!(xs(&batch), [Some(1.0), None, Some(2.0)]);
    }

    #[test]
    fn other_targets_and_seats_are_not_merged() {
        let (a, b, seat, other) = (
            TargetId::next(),
            TargetId::next(),
            SeatId::next(),
            SeatId::next(),
        );
        let events = vec![
            motion(a, seat, 1.0),
            motion(b, seat, 2.0),
            motion(b, other, 3.0),
            motion(a, seat, 4.0),
        ];
        let (batch, dropped) = coalesce(events);
        assert_eq!(xs(&batch), [Some(1.0), Some(2.0), Some(3.0), Some(4.0)]);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn a_fresh_notice_comes_before_the_page() {
        let (notice, page, error) = (Some("bad config"), Some("typed x"), Some("failed"));
        assert_eq!(
            status_line(notice, true, page, error),
            Some(("notice", "bad config"))
        );
        assert_eq!(
            status_line(notice, false, page, error),
            Some(("page", "typed x"))
        );
        assert_eq!(
            status_line(notice, false, None, error),
            Some(("error", "failed"))
        );
        assert_eq!(
            status_line(notice, false, None, None),
            Some(("notice", "bad config"))
        );
        assert_eq!(
            status_line(None, true, page, error),
            Some(("page", "typed x"))
        );
        assert_eq!(status_line(None, false, None, None), None);
    }
}
