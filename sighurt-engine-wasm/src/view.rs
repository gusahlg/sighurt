//! One view on the owner thread: the page logic that turns browser commands and input into
//! guest state, and guest state into [`EngineEvent`]s.
//!
//! Every navigation loads a new document with a fresh [`HostState`] (see [`document_state`]).
//! The load (fetch, compile, `start_app`) runs on a worker; until it finishes the previous
//! document keeps running, and a load that a newer navigation, a stop or a close superseded is
//! cancelled (its guest code stops at the next epoch tick) and discarded when it finishes. A
//! committed document gets a new [`DocumentGeneration`]; the old one is cancelled the same way,
//! and a step of it that finishes afterwards is discarded. What a replaced or closed document
//! leaves is dropped off the owner thread ([`Jobs::retire`]).
//!
//! Positions arrive surface-local, in logical pixels. The guest sees canvas coordinates:
//! logical pixels divided by the page zoom.
//!
//! ## History
//!
//! The view owns its session history across documents ([`Session`]). A document starts with
//! its URL as its one entry; guest `push_state` adds same-document entries after it (of the
//! app's own origin only), and `Traverse` (like the guest's `history_back` /
//! `history_forward`) walks them without re-fetching the module: it moves the stack and the URL
//! the guest reads back through `get_url` / `get_state`. The guest ABI has no popstate event,
//! so guests notice by polling those. A traversal to an entry of another document loads that
//! document's module again, with the entries it had and the one traversed to current, as a new
//! document; so does a reload. Every change is reported with `NavigationCommitted` and
//! `HistoryChanged` listing the whole session; while another document is loading, the one
//! about to be replaced reports none, so it cannot supersede that navigation.
//!
//! ## Background policy
//!
//! A hidden view runs one step a second instead of about sixty: its timers and events are
//! delivered late but in order, and its audio and network connections carry on. It presents
//! nothing until it is shown again.
//!
//! ## The permission prompt
//!
//! The engine draws the prompt over the page, with a paint budget of its own, and it answers
//! Enter, Escape and clicks only once a frame with it was painted, and only when the view is
//! big enough to show all of it: a page can't hide it and still have it answered.
//!
//! Owns: a view's page state (document, history, pointer, prompt, what the browser was told).
//! Must not own: guest execution (workers do it) or painting (the rasterizer).

use std::mem;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use sighurt_api::engine::{EngineCommand, EngineError, ViewOptions};
use sighurt_api::events::{Cursor, EngineEvent, NavigationError, OpenDisposition};
use sighurt_api::geometry::{LogicalSize, Metrics, SurfacePoint};
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, Key, KeyEvent, PointerButton, PointerEvent,
    ScrollDelta,
};

use crate::capabilities::{HostState, MAX_URL};
use crate::draw::{DisplayList, DrawOp};
use crate::engine::WasmEngine;
use crate::keys;
use crate::live::LiveGuest;
use crate::navigation::{HistoryEntry, NavigationStack, Place, Session};
use crate::permissions::{self, PendingPermission};
use crate::raster::{self, Frame, Scene};
use crate::runtime::Loaded;

/// Logical pixels scrolled per wheel notch, as in Chrome and the pre-rewrite browser.
const WHEEL_NOTCH: f64 = 100.0;
/// Most key presses and bytes of typed text a guest gets per frame; more is dropped.
const MAX_FRAME_KEYS: usize = 256;
const MAX_FRAME_TEXT: usize = 4096;
/// The page zoom a view accepts; others are clamped to it.
const ZOOM: std::ops::RangeInclusive<f64> = 0.1..=10.0;

/// Locks `mutex` even if a panicking guest call poisoned it: the owner thread must not panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Events on their way to the owner, coalescing the replaceable ones (see
/// [`EngineRuntime::pump`](sighurt_api::engine::EngineRuntime::pump)).
#[derive(Default)]
pub struct Outbox(Vec<EngineEvent>);

impl Outbox {
    pub fn push(&mut self, event: EngineEvent) {
        use EngineEvent::*;
        let replaceable = matches!(
            event,
            TitleChanged { .. }
                | StatusTextChanged { .. }
                | CursorChanged { .. }
                | LoadingChanged { .. }
                | HistoryChanged { .. }
                | TextInputChanged { .. }
                | FrameReady { .. }
        );
        if replaceable {
            let kind = mem::discriminant(&event);
            self.0.retain(|queued| {
                mem::discriminant(queued) != kind || queued.view() != event.view()
            });
        }
        self.0.push(event);
    }

    pub fn take(&mut self) -> Vec<EngineEvent> {
        mem::take(&mut self.0)
    }
}

/// The work a view hands to the runtime's workers.
pub trait Jobs {
    /// Loads `url` into `state` for `navigation`, unless `state`'s budget is cancelled first.
    fn load(&self, view: ViewId, navigation: NavigationId, state: HostState, url: String);
    /// Runs `live`'s steps from now on, the first at `at` with `serial` (see
    /// [`LiveGuest::due`]).
    fn run(&self, live: &Arc<LiveGuest>, at: Instant, serial: u64);
    /// Drops `garbage` off the owner thread.
    fn retire(&self, garbage: Box<dyn Send>);
}

/// Whether a view has a frame for a permit.
#[derive(Debug, PartialEq, Eq)]
pub enum Readiness {
    NotReady,
    Unchanged,
    Paint,
}

struct Document {
    generation: DocumentGeneration,
    /// The URL its module was loaded from, which Reload loads again even after `push_state`.
    url: String,
}

/// Where a loading document goes in the session history when it commits.
#[derive(Clone, Copy)]
enum Enter {
    /// After the current entry, dropping those after it.
    New,
    /// In place of the document on screen.
    Reload,
    /// At a past document's place.
    Traverse(Place),
}

struct Loading {
    navigation: NavigationId,
    /// The URL its module is loaded from.
    url: String,
    enter: Enter,
    /// The new document's state (see [`document_state`]).
    state: HostState,
}

/// What the browser was last told.
#[derive(Default)]
struct Shown {
    title: Option<String>,
    status: Option<String>,
    cursor: Cursor,
}

pub struct View {
    id: ViewId,
    /// The latest metrics from the host.
    metrics: Metrics,
    /// The last non-empty logical size: what the canvas follows.
    size: LogicalSize,
    visible: bool,
    /// Logical focus (`SetFocus`) and the window's keyboard focus (`FocusEvent`).
    focused: bool,
    active: bool,
    zoom: f64,
    /// State of the document on screen. Its page-level parts (input, focus, permission
    /// decisions) carry over to every document of the view.
    page: HostState,
    document: Option<Document>,
    /// The document's guest, while it runs frames.
    live: Option<Arc<LiveGuest>>,
    loading: Option<Loading>,
    /// The session history beyond the document on screen.
    session: Session,
    /// The document's latest display list, and a serial that changes whenever what the view
    /// shows does (a new list, zoom, the prompt).
    list: Option<Arc<DisplayList>>,
    serial: u64,
    presented: Option<(LayoutEpoch, u64)>,
    /// The pointer's surface-local position, while it is over the view.
    pointer: Option<SurfacePoint>,
    /// Link under the pointer when a button went down; releasing over the same link opens it.
    pressed_link: Option<String>,
    /// The permission prompt took the current left click.
    prompt_grab: bool,
    /// The permission request the prompt shows.
    prompt: Option<PendingPermission>,
    /// The prompt was painted, whole, in the last frame presented: only then does it answer.
    prompt_shown: bool,
    /// The version of the history the browser was last told about.
    history_version: u64,
    shown: Shown,
}

impl View {
    pub fn new(id: ViewId, engine: WasmEngine, metrics: Metrics, options: ViewOptions) -> Self {
        let size = if metrics.is_empty() {
            // The canvas's own default until the host has room for the view.
            LogicalSize {
                width: 800,
                height: 600,
            }
        } else {
            metrics.rect().size()
        };
        let view = Self {
            id,
            page: HostState {
                engine: Some(engine),
                ..HostState::default()
            },
            metrics,
            size,
            visible: options.visible,
            focused: false,
            active: true,
            zoom: clamp_zoom(options.zoom).unwrap_or(1.0),
            document: None,
            live: None,
            loading: None,
            session: Session::default(),
            list: None,
            serial: 0,
            presented: None,
            pointer: None,
            pressed_link: None,
            prompt_grab: false,
            prompt: None,
            prompt_shown: false,
            history_version: 0,
            shown: Shown::default(),
        };
        view.layout();
        view.update_focus();
        view
    }

    pub fn command(
        &mut self,
        command: EngineCommand,
        jobs: &impl Jobs,
        out: &mut Outbox,
    ) -> Result<(), EngineError> {
        match command {
            EngineCommand::Navigate { navigation, url } => {
                let history = first_entry(&url);
                self.load(navigation, url, Enter::New, history, jobs, out)
            }
            EngineCommand::Reload { navigation } => {
                // Restarts the load in progress, or else reloads the document on screen.
                let plan = match (&self.loading, &self.document) {
                    (Some(loading), _) => Some((loading.url.clone(), loading.enter)),
                    (None, Some(document)) => Some((document.url.clone(), Enter::Reload)),
                    (None, None) => None,
                };
                match plan {
                    Some((url, enter)) => {
                        let history = self.history_for(enter, &url);
                        self.load(navigation, url, enter, history, jobs, out)
                    }
                    None => out.push(EngineEvent::NavigationFailed {
                        view: self.id,
                        navigation,
                        error: NavigationError::Load("there is nothing to reload".into()),
                    }),
                }
            }
            EngineCommand::Stop => {
                // Abandons the load in progress; the document on screen stays.
                if let Some(navigation) = self.cancel_load(jobs) {
                    out.push(EngineEvent::NavigationFailed {
                        view: self.id,
                        navigation,
                        error: NavigationError::Cancelled,
                    });
                    out.push(EngineEvent::LoadingChanged {
                        view: self.id,
                        loading: false,
                    });
                    // What the document did meanwhile can be reported now.
                    self.report_history(None, out);
                }
            }
            EngineCommand::Traverse { navigation, delta } => {
                self.traverse(navigation, delta, jobs, out)
            }
            EngineCommand::SetMetrics(metrics) => {
                self.metrics = metrics;
                if !metrics.is_empty() {
                    self.size = metrics.rect().size();
                    self.layout();
                    // At least a new scale to paint at; a live guest redraws for a new size.
                    self.invalidate(out);
                }
            }
            EngineCommand::SetVisibility(visible) => {
                if visible && !self.visible {
                    self.invalidate(out);
                }
                self.visible = visible;
                if let Some(live) = &self.live {
                    if let Some((at, serial)) = live.set_visible(visible) {
                        jobs.run(live, at, serial);
                    }
                }
            }
            EngineCommand::SetFocus(focused) => {
                self.focused = focused;
                self.update_focus();
                if !focused {
                    self.release_keys();
                }
            }
            EngineCommand::SetZoom(zoom) => {
                self.zoom = clamp_zoom(zoom)
                    .ok_or(EngineError::Unsupported("a zoom that isn't a number"))?;
                self.layout();
                self.invalidate(out);
            }
            EngineCommand::Input(event) => self.input(event, out),
            // The engine makes no service requests yet, so there is nothing to resolve.
            EngineCommand::ResolveServiceRequest { .. } => {}
        }
        Ok(())
    }

    /// The history a document loaded from `url` starts with, entering the session at `enter`.
    fn history_for(&self, enter: Enter, url: &str) -> NavigationStack {
        match enter {
            Enter::New => first_entry(url),
            Enter::Reload => lock(&self.page.navigation).clone(),
            Enter::Traverse(place) => self
                .session
                .document(place)
                .map_or_else(|| first_entry(url), |past| past.history),
        }
    }

    /// Starts loading the module at `url` as a new document with `history`, superseding any
    /// load in progress; it goes into the session at `enter` when it commits.
    fn load(
        &mut self,
        navigation: NavigationId,
        url: String,
        enter: Enter,
        history: NavigationStack,
        jobs: &impl Jobs,
        out: &mut Outbox,
    ) {
        self.cancel_load(jobs);
        let shown = history
            .current()
            .map_or_else(|| url.clone(), |entry| entry.url.clone());
        let state = document_state(&self.page, &url, history);
        jobs.load(self.id, navigation, state.clone(), url.clone());
        out.push(EngineEvent::NavigationStarted {
            view: self.id,
            navigation,
            url: shown,
        });
        out.push(EngineEvent::LoadingChanged {
            view: self.id,
            loading: true,
        });
        self.loading = Some(Loading {
            navigation,
            url,
            enter,
            state,
        });
    }

    /// Cancels the load in progress, if any, and returns its navigation.
    fn cancel_load(&mut self, jobs: &impl Jobs) -> Option<NavigationId> {
        let loading = self.loading.take()?;
        loading.state.budget.cancel();
        jobs.retire(Box::new(loading.state));
        Some(loading.navigation)
    }

    /// Moves `delta` entries through the session history, or nowhere if there aren't that
    /// many. Within the document on screen it commits at once, superseding a load in progress;
    /// to another document's entry it loads that document.
    fn traverse(
        &mut self,
        navigation: NavigationId,
        delta: i32,
        jobs: &impl Jobs,
        out: &mut Outbox,
    ) {
        let place = match &self.document {
            Some(_) if delta != 0 => self.session.place(&lock(&self.page.navigation), delta),
            _ => None,
        };
        match place {
            None => out.push(EngineEvent::NavigationFailed {
                view: self.id,
                navigation,
                error: NavigationError::Cancelled,
            }),
            Some(Place::Current(entry)) => {
                go_to(&self.page, entry);
                if self.cancel_load(jobs).is_some() {
                    out.push(EngineEvent::LoadingChanged {
                        view: self.id,
                        loading: false,
                    });
                }
                self.report_history(Some(navigation), out);
            }
            Some(place) => {
                let Some(past) = self.session.document(place) else {
                    return;
                };
                let enter = Enter::Traverse(place);
                self.load(navigation, past.url, enter, past.history, jobs, out);
            }
        }
    }

    /// A load finished. Swaps in the document of the newest load, if it succeeded; the previous
    /// document goes into the session history, and its module and state are cancelled and
    /// dropped off this thread, which ends everything it had running.
    pub fn loaded(
        &mut self,
        navigation: NavigationId,
        result: Result<Loaded, String>,
        jobs: &impl Jobs,
        out: &mut Outbox,
    ) {
        if self.loading.as_ref().map(|loading| loading.navigation) != Some(navigation) {
            // Superseded, stopped or traversed away from.
            return jobs.retire(Box::new(result));
        }
        let Some(loading) = self.loading.take() else {
            return;
        };
        match result {
            Err(message) => {
                jobs.retire(Box::new(loading.state));
                out.push(EngineEvent::NavigationFailed {
                    view: self.id,
                    navigation,
                    error: NavigationError::Load(message),
                });
                self.report_history(None, out);
            }
            Ok(loaded) => {
                // A prompt the previous document raised no longer concerns anyone.
                lock(&loading.state.permissions).pending = None;
                let old = mem::replace(&mut self.page, loading.state);
                old.budget.cancel();
                if let Some(document) = self.document.take() {
                    let history = mem::take(&mut *lock(&old.navigation));
                    match loading.enter {
                        Enter::New => self.session.leave(document.url, history),
                        Enter::Reload => {}
                        Enter::Traverse(place) => self.session.enter(place, document.url, history),
                    }
                }
                let old_guest = self.live.take().and_then(|live| live.end());
                jobs.retire(Box::new((old, old_guest, self.list.take())));
                let generation = DocumentGeneration::next();
                self.document = Some(Document {
                    generation,
                    url: loading.url,
                });
                self.live = loaded.guest.map(|guest| {
                    let live = LiveGuest::new(self.id, generation, guest, self.visible);
                    let live = Arc::new(live);
                    let (at, serial) = live.due();
                    jobs.run(&live, at, serial);
                    live
                });
                self.list = loaded.list;
                self.prompt = None;
                self.prompt_shown = false;
                self.report_history(Some(navigation), out);
                self.report_title(out);
                self.invalidate(out);
                self.after_run(out);
            }
        }
        out.push(EngineEvent::LoadingChanged {
            view: self.id,
            loading: false,
        });
    }

    /// A step that changed something finished: it drew `list`, or failed with `error`.
    pub fn stepped(
        &mut self,
        document: DocumentGeneration,
        list: Option<Arc<DisplayList>>,
        error: Option<String>,
        jobs: &impl Jobs,
        out: &mut Outbox,
    ) {
        let current = self.document.as_ref().map(|document| document.generation);
        if current != Some(document) {
            // A document that was replaced.
            return jobs.retire(Box::new(list));
        }
        if let Some(list) = list {
            self.list = Some(list);
            self.invalidate(out);
        }
        if let Some(message) = error {
            // The guest ended with the step.
            self.live = None;
            out.push(EngineEvent::ViewFailed {
                view: self.id,
                message,
            });
        }
        self.after_run(out);
    }

    /// Reports what the guest did in a load or step: a navigation it asked for, history it
    /// changed, a permission prompt it raised, links it moved under the pointer.
    fn after_run(&mut self, out: &mut Outbox) {
        let requested = lock(&self.page.pending_navigation).take();
        if let (Some(url), Some(document)) = (requested, &self.document) {
            if url.len() <= MAX_URL {
                out.push(EngineEvent::OpenRequested {
                    view: self.id,
                    document: document.generation,
                    url,
                    disposition: OpenDisposition::CurrentView,
                });
            }
        }
        if self.loading.is_none() {
            self.report_history(None, out);
        }
        let prompt = lock(&self.page.permissions).pending.clone();
        let same =
            |a: &PendingPermission, b: &PendingPermission| a.origin == b.origin && a.kind == b.kind;
        let changed = match (&prompt, &self.prompt) {
            (Some(a), Some(b)) => !same(a, b),
            (a, b) => a.is_some() != b.is_some(),
        };
        if changed {
            self.prompt = prompt;
            self.prompt_shown = false;
            self.invalidate(out);
        }
        self.update_hover(out);
    }

    /// Tells the browser about the document's history, if it changed since it was last told
    /// or `navigation` (the browser's own) caused it; a change the guest made gets a new id.
    fn report_history(&mut self, navigation: Option<NavigationId>, out: &mut Outbox) {
        let Some(document) = &self.document else {
            return;
        };
        let history = lock(&self.page.navigation);
        let version = history.version();
        if navigation.is_none() && version == self.history_version {
            return;
        }
        // Not while loading: a traversal in progress holds places in the session.
        debug_assert!(self.loading.is_none());
        self.session.trim(&history);
        let (entries, current) = self.session.urls(&history);
        drop(history);
        self.history_version = version;
        let Some(url) = entries.get(current).cloned() else {
            return;
        };
        out.push(EngineEvent::NavigationCommitted {
            view: self.id,
            navigation: navigation.unwrap_or_else(NavigationId::next),
            url,
            document: document.generation,
        });
        out.push(EngineEvent::HistoryChanged {
            view: self.id,
            entries,
            current,
        });
    }

    /// The app's name from its manifest is the document's title.
    fn report_title(&mut self, out: &mut Outbox) {
        let title = lock(&self.page.manifest)
            .as_ref()
            .map(|manifest| manifest.name.trim().to_string())
            .filter(|title| !title.is_empty());
        if title != self.shown.title {
            self.shown.title.clone_from(&title);
            out.push(EngineEvent::TitleChanged {
                view: self.id,
                title,
            });
        }
    }

    /// What the view shows changed: it needs presenting again.
    fn invalidate(&mut self, out: &mut Outbox) {
        self.serial += 1;
        out.push(EngineEvent::FrameReady { view: self.id });
    }

    /// Sizes the canvases of the document on screen and the one loading to the view.
    fn layout(&self) {
        let (width, height) = self.canvas_size();
        let loading = self.loading.as_ref().map(|loading| &loading.state);
        for state in std::iter::once(&self.page).chain(loading) {
            let mut canvas = lock(&state.canvas);
            canvas.width = width;
            canvas.height = height;
        }
    }

    /// The canvas size, in canvas pixels: the view's logical size divided by the zoom.
    fn canvas_size(&self) -> (u32, u32) {
        let size = |logical: u32| (f64::from(logical) / self.zoom) as u32;
        (size(self.size.width), size(self.size.height))
    }

    fn update_focus(&self) {
        self.page
            .focused
            .store(self.focused && self.active, Ordering::Relaxed);
    }

    /// Forgets the keys the guest saw go down: their releases will go elsewhere.
    fn release_keys(&self) {
        let mut input = lock(&self.page.input_state);
        input.keys_down.clear();
        input.modifiers_shift = false;
        input.modifiers_ctrl = false;
        input.modifiers_alt = false;
    }

    fn live(&self) -> bool {
        self.live.is_some()
    }

    /// Adds to the input of the guest's next step, if it runs frames.
    fn frame_input(&self, add: impl FnOnce(&mut crate::runtime::FrameInput)) {
        if let Some(live) = &self.live {
            live.input(add);
        }
    }

    /// Whether the view has a frame for a permit at `epoch`.
    pub fn readiness(&self, epoch: LayoutEpoch) -> Readiness {
        if self.metrics.is_empty() || epoch != self.metrics.epoch() {
            return Readiness::NotReady;
        }
        let Some(list) = &self.list else {
            return Readiness::NotReady;
        };
        // A live guest redraws at a new size; until it has, its frame is for the old layout.
        if self.live() && list.size != self.canvas_size() {
            return Readiness::NotReady;
        }
        if self.presented == Some((epoch, self.serial)) {
            return Readiness::Unchanged;
        }
        Readiness::Paint
    }

    /// Paints the view's frame into `frame`, which has the size of its metrics.
    pub fn paint(&mut self, frame: Frame<'_>) -> Result<(), String> {
        let (rect, buffer) = (self.metrics.rect(), self.metrics.buffer());
        let bx = buffer.width as f32 / rect.width as f32;
        let by = buffer.height as f32 / rect.height as f32;
        let zoom = self.zoom as f32;
        let overlay = self.prompt_ops();
        let scene = Scene {
            list: self.list.as_deref(),
            list_scale: (bx * zoom, by * zoom),
            overlay: &overlay,
            overlay_scale: (bx, by),
        };
        raster::paint(frame, &scene)?;
        self.presented = Some((self.metrics.epoch(), self.serial));
        let [x, y, w, h] = PROMPT;
        let fits = rect.width as f32 >= x + w + x && rect.height as f32 >= y + h + y;
        self.prompt_shown = self.prompt.is_some() && fits;
        Ok(())
    }

    /// Closes the view: its documents stop at once, and all of it is dropped off the owner
    /// thread.
    pub fn close(mut self, jobs: &impl Jobs) {
        self.page.budget.cancel();
        if let Some(loading) = &self.loading {
            loading.state.budget.cancel();
        }
        let guest = self.live.take().and_then(|live| live.end());
        jobs.retire(Box::new((self, guest)));
    }

    fn input(&mut self, event: InputEvent, out: &mut Outbox) {
        match event.kind {
            InputKind::Pointer(pointer) => match pointer {
                PointerEvent::Enter { position } | PointerEvent::Motion { position, .. } => {
                    self.pointer_moved(position, out)
                }
                PointerEvent::Leave => {
                    self.pointer = None;
                    self.update_hover(out);
                }
                PointerEvent::Button {
                    button,
                    state,
                    position,
                    ..
                } => {
                    self.pointer_moved(position, out);
                    self.button(button, state == ButtonState::Pressed, out);
                }
                PointerEvent::Scroll {
                    delta, position, ..
                } => {
                    self.pointer_moved(position, out);
                    self.scroll(delta);
                }
            },
            InputKind::Key(key) => self.key(&key, out),
            InputKind::TextInput(text) => {
                if let Some(commit) = text.commit {
                    self.type_text(&commit);
                }
            }
            InputKind::Focus(FocusEvent::Activated) => {
                self.active = true;
                self.update_focus();
            }
            InputKind::Focus(FocusEvent::Deactivated) => {
                self.active = false;
                self.update_focus();
                self.release_keys();
                // Pointer captures end too.
                lock(&self.page.input_state).mouse_buttons_down = [false; 3];
                self.prompt_grab = false;
                self.pressed_link = None;
            }
        }
    }

    /// The pointer position in canvas coordinates.
    fn canvas_pointer(&self) -> Option<(f32, f32)> {
        let position = self.pointer?;
        Some((
            (position.x / self.zoom) as f32,
            (position.y / self.zoom) as f32,
        ))
    }

    fn pointer_moved(&mut self, position: SurfacePoint, out: &mut Outbox) {
        self.pointer = Some(position);
        if let Some((x, y)) = self.canvas_pointer() {
            let mut input = lock(&self.page.input_state);
            input.mouse_x = x;
            input.mouse_y = y;
        }
        self.update_hover(out);
    }

    /// Shows the target of the link under the pointer, and a pointer cursor over links and the
    /// prompt's buttons.
    fn update_hover(&mut self, out: &mut Outbox) {
        let link = self.link_at_pointer();
        let cursor = if link.is_some() || self.prompt_button_at_pointer().is_some() {
            Cursor::Pointer
        } else {
            Cursor::Default
        };
        if link != self.shown.status {
            self.shown.status.clone_from(&link);
            out.push(EngineEvent::StatusTextChanged {
                view: self.id,
                text: link,
            });
        }
        if cursor != self.shown.cursor {
            self.shown.cursor = cursor;
            out.push(EngineEvent::CursorChanged {
                view: self.id,
                cursor,
            });
        }
    }

    /// URL of the topmost guest hyperlink under the pointer.
    fn link_at_pointer(&self) -> Option<String> {
        if self.prompt_at_pointer(PROMPT) {
            return None;
        }
        let (x, y) = self.canvas_pointer()?;
        lock(&self.page.hyperlinks).at(x, y).map(str::to_string)
    }

    fn button(&mut self, button: PointerButton, down: bool, out: &mut Outbox) {
        // The prompt answers left clicks on it; the guest doesn't see them.
        if button == PointerButton::Left
            && (self.prompt_grab || (down && self.prompt_at_pointer(PROMPT)))
        {
            self.prompt_grab = down;
            if let Some(allow) = self.prompt_button_at_pointer().filter(|_| !down) {
                self.answer_prompt(allow, out);
            }
            return;
        }
        let index = match button {
            PointerButton::Left => 0,
            PointerButton::Right => 1,
            PointerButton::Middle => 2,
            _ => return,
        };
        lock(&self.page.input_state).mouse_buttons_down[index] = down;
        if !down {
            self.frame_input(|frame| frame.clicked[index] = true);
        }
        // Clicking a link opens it; with the middle button, in a new tab.
        if matches!(button, PointerButton::Left | PointerButton::Middle) {
            let link = self.link_at_pointer();
            if down {
                self.pressed_link = link;
            } else if let (Some(url), Some(document)) = (link, &self.document) {
                if self.pressed_link.as_ref() == Some(&url) {
                    let disposition = match button {
                        PointerButton::Middle => OpenDisposition::NewView,
                        _ => OpenDisposition::CurrentView,
                    };
                    out.push(EngineEvent::OpenRequested {
                        view: self.id,
                        document: document.generation,
                        url,
                        disposition,
                    });
                }
            }
        }
    }

    /// Scrolls by a delta with DOM signs (positive scrolls down).
    fn scroll(&mut self, delta: ScrollDelta) {
        let (dx, dy) = match delta {
            ScrollDelta::Pixels { x, y } => (x, y),
            ScrollDelta::Wheel { x, y } => (
                f64::from(x) / 120.0 * WHEEL_NOTCH,
                f64::from(y) / 120.0 * WHEEL_NOTCH,
            ),
        };
        let (dx, dy) = ((dx / self.zoom) as f32, (dy / self.zoom) as f32);
        // The guest's delta is positive when scrolling up or left.
        self.frame_input(|frame| {
            frame.scroll.0 -= dx;
            frame.scroll.1 -= dy;
        });
        let viewport = self.canvas_size();
        let mut scroll = lock(&self.page.scroll);
        let (x, y) = (scroll.x + dx, scroll.y + dy);
        scroll.set(x, y, viewport);
    }

    fn key(&mut self, event: &KeyEvent, out: &mut Outbox) {
        let down = event.state == ButtonState::Pressed;
        {
            let mut input = lock(&self.page.input_state);
            (
                input.modifiers_shift,
                input.modifiers_ctrl,
                input.modifiers_alt,
            ) = keys::guest_modifiers(event.modifiers);
        }
        // Enter / Escape answer the permission prompt; the guest never sees them.
        if down && keys::plain(event.modifiers) && self.prompt_answers() {
            let allow = match &event.key {
                Key::Named(name) if name == "Enter" => Some(true),
                Key::Named(name) if name == "Escape" => Some(false),
                _ => None,
            };
            if let Some(allow) = allow {
                return self.answer_prompt(allow, out);
            }
        }
        let code = keys::key_code(&event.key, event.code);
        let mut input = lock(&self.page.input_state);
        if !down {
            if let Some(code) = code {
                input.keys_down.retain(|&held| held != code);
            }
            return;
        }
        if let Some(code) = code {
            if !input.keys_down.contains(&code) {
                input.keys_down.push(code);
            }
            self.frame_input(|frame| {
                if frame.keys_pressed.len() < MAX_FRAME_KEYS {
                    frame.keys_pressed.push(code);
                }
            });
        }
        drop(input);
        if let Some(text) = &event.text {
            self.type_text(text);
        }
    }

    /// Text typed or committed by an input method, for the guest's `text_input()`.
    fn type_text(&mut self, text: &str) {
        self.frame_input(|frame| {
            for c in text.chars().filter(|c| !c.is_control()) {
                if frame.text.len() + c.len_utf8() > MAX_FRAME_TEXT {
                    break;
                }
                frame.text.push(c);
            }
        });
    }

    fn answer_prompt(&mut self, allow: bool, out: &mut Outbox) {
        permissions::resolve_pending(&self.page.permissions, allow);
        self.prompt = None;
        self.prompt_shown = false;
        self.prompt_grab = false;
        self.invalidate(out);
        self.update_hover(out);
    }

    /// Whether the prompt is up and was shown to the user, so it may be answered.
    fn prompt_answers(&self) -> bool {
        self.prompt.is_some() && self.prompt_shown
    }

    /// Whether the prompt is showing and the pointer is in `rect` of it.
    fn prompt_at_pointer(&self, [x, y, w, h]: Rect) -> bool {
        let Some(pointer) = self.pointer else {
            return false;
        };
        let (px, py) = (pointer.x as f32, pointer.y as f32);
        (x..=x + w).contains(&px) && (y..=y + h).contains(&py) && self.prompt_answers()
    }

    /// The decision of the prompt button under the pointer.
    fn prompt_button_at_pointer(&self) -> Option<bool> {
        if self.prompt_at_pointer(PROMPT_ALLOW) {
            Some(true)
        } else if self.prompt_at_pointer(PROMPT_BLOCK) {
            Some(false)
        } else {
            None
        }
    }

    /// The permission prompt, drawn over the page's top-left corner in logical pixels while a
    /// request waits.
    fn prompt_ops(&self) -> Vec<DrawOp> {
        let Some(request) = &self.prompt else {
            return Vec::new();
        };
        let [x, y, w, h] = PROMPT;
        let button = |[bx, by, bw, bh]: Rect, fill, fg, weight, text: &str| {
            [
                rect([bx, by, bw, bh], 4.0, fill),
                label(
                    bx + bw / 2.0,
                    by + (bh - 16.8) / 2.0,
                    14.0,
                    fg,
                    weight,
                    1,
                    text,
                ),
            ]
        };
        let mut ops = vec![
            rect([x - 1.0, y - 1.0, w + 2.0, h + 2.0], 7.0, 0x3f3f46ff),
            rect(PROMPT, 6.0, 0x18181bff),
            DrawOp::PushClip { x, y, w, h },
            label(
                x + 12.0,
                y + 12.0,
                12.0,
                0xa1a1aaff,
                400,
                0,
                shortened(&request.origin, MAX_PROMPT_ORIGIN),
            ),
            label(
                x + 12.0,
                y + 32.0,
                14.0,
                0xfafafaff,
                400,
                0,
                &format!("wants to: {}", request.kind.description()),
            ),
        ];
        ops.extend(button(PROMPT_BLOCK, 0x27272aff, 0xfafafaff, 400, "Block"));
        ops.extend(button(PROMPT_ALLOW, 0xfafafaff, 0x18181bff, 600, "Allow"));
        ops.push(label(
            x + 12.0,
            y + 92.0,
            11.0,
            0x71717aff,
            400,
            0,
            "Enter to allow \u{00b7} Esc to block",
        ));
        ops.push(DrawOp::PopClip);
        ops
    }
}

/// `zoom` within [`ZOOM`], or `None` if it isn't a number.
fn clamp_zoom(zoom: f64) -> Option<f64> {
    (!zoom.is_nan()).then(|| zoom.clamp(*ZOOM.start(), *ZOOM.end()))
}

/// `[x, y, w, h]` in logical pixels.
type Rect = [f32; 4];

/// The permission prompt's card and buttons.
const PROMPT: Rect = [8.0, 8.0, 320.0, 116.0];
const PROMPT_BLOCK: Rect = [180.0, 64.0, 64.0, 28.0];
const PROMPT_ALLOW: Rect = [252.0, 64.0, 64.0, 28.0];

fn rect([x, y, w, h]: Rect, radius: f32, color: u32) -> DrawOp {
    DrawOp::Rect {
        x,
        y,
        w,
        h,
        radius,
        color,
    }
}

fn label(x: f32, y: f32, size: f32, color: u32, weight: u16, align: u8, text: &str) -> DrawOp {
    DrawOp::Text {
        x,
        y,
        size,
        color,
        family: String::new(),
        weight,
        italic: false,
        align,
        text: text.into(),
    }
}

/// How much of an origin the prompt shows. The card clips it too; this bounds the shaping.
const MAX_PROMPT_ORIGIN: usize = 256;

/// `text` cut to at most `max` bytes at a character boundary.
fn shortened(text: &str, max: usize) -> &str {
    &text[..text.floor_char_boundary(max)]
}

/// The history of a document loaded from `url`: that one entry.
fn first_entry(url: &str) -> NavigationStack {
    let mut history = NavigationStack::new();
    history.push(HistoryEntry::new(url));
    history
}

/// Makes entry `index` of `state`'s document history current, and its URL the one the guest
/// reads back.
fn go_to(state: &HostState, index: usize) {
    let mut history = lock(&state.navigation);
    history.go_to(index);
    let url = history.current().map(|entry| entry.url.clone());
    drop(history);
    if let Some(url) = url {
        *lock(&state.current_url) = url;
    }
}

/// Guest state for a new document loaded from `url`, with `history`, derived from the page's
/// current one. Everything the previous document's module could start (timers, event
/// listeners, connections, audio, workers, canvas, console, scroll) begins empty, so none of it
/// outlives that document. The page-level parts carry over: the engine, input, canvas size,
/// focus and permission decisions, plus the session storage when `url` has the same origin. A
/// part whose lock a panicking host function poisoned starts over instead, so one engine bug
/// doesn't break every later document. The guest reads its current entry's URL back.
pub fn document_state(page: &HostState, url: &str, history: NavigationStack) -> HostState {
    fn carry<T: Default>(part: &Arc<Mutex<T>>) -> Arc<Mutex<T>> {
        match part.is_poisoned() {
            true => Arc::default(),
            false => part.clone(),
        }
    }
    let mut state = HostState {
        engine: page.engine.clone(),
        permissions: carry(&page.permissions),
        input_state: carry(&page.input_state),
        focused: page.focused.clone(),
        ..Default::default()
    };
    {
        let viewport = lock(&page.canvas);
        let mut canvas = lock(&state.canvas);
        canvas.width = viewport.width;
        canvas.height = viewport.height;
    }
    let origin = crate::url::app_origin_of(url);
    if *lock(&page.module_origin) == origin {
        state.storage = carry(&page.storage);
        *lock(&state.module_origin) = origin;
    }
    let shown = history
        .current()
        .map_or_else(|| url.to_string(), |entry| entry.url.clone());
    *lock(&state.navigation) = history;
    *lock(&state.current_url) = shown;
    state
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::capabilities::{set_module_origin, TimerEntry};

    fn push_state(host_state: &HostState, url: &str) {
        host_state
            .navigation
            .lock()
            .unwrap()
            .push(HistoryEntry::new(url));
        *host_state.current_url.lock().unwrap() = url.to_string();
    }

    fn current_url(host_state: &HostState) -> String {
        host_state.current_url.lock().unwrap().clone()
    }

    fn new_document(page: &HostState, url: &str) -> HostState {
        document_state(page, url, first_entry(url))
    }

    #[test]
    fn a_new_document_has_no_history() {
        let hs = new_document(&HostState::default(), "https://a.test/app.wasm");
        let nav = hs.navigation.lock().unwrap();
        assert!(!nav.can_go_back());
        assert!(!nav.can_go_forward());
        assert_eq!(nav.len(), 1);
        drop(nav);
        assert_eq!(current_url(&hs), "https://a.test/app.wasm");
    }

    #[test]
    fn going_to_an_entry_sets_the_url_the_guest_reads() {
        let hs = new_document(&HostState::default(), "https://a.test/app.wasm");
        push_state(&hs, "https://a.test/about");
        push_state(&hs, "https://a.test/contact");
        go_to(&hs, 1);
        assert_eq!(current_url(&hs), "https://a.test/about");
        go_to(&hs, 0);
        assert_eq!(current_url(&hs), "https://a.test/app.wasm");
        // No such entry: nothing moves.
        go_to(&hs, 3);
        assert_eq!(current_url(&hs), "https://a.test/app.wasm");
    }

    #[test]
    fn a_restored_document_reads_its_current_entry() {
        let mut history = first_entry("https://a.test/app.wasm");
        history.push(HistoryEntry::new("https://a.test/app.wasm#b").with_state(vec![1]));
        history.push(HistoryEntry::new("https://a.test/app.wasm#c"));
        history.go_to(1);
        let hs = document_state(&HostState::default(), "https://a.test/app.wasm", history);
        assert_eq!(current_url(&hs), "https://a.test/app.wasm#b");
        let nav = hs.navigation.lock().unwrap();
        assert_eq!(
            (nav.len(), nav.current().unwrap().state.as_slice()),
            (3, &[1][..])
        );
    }

    #[test]
    fn a_new_document_does_not_inherit_guest_state() {
        let page = HostState::default();
        set_module_origin(&page, "https://a.test/app.wasm");
        page.timers.lock().unwrap().push(TimerEntry {
            id: 1,
            fire_at: Instant::now(),
            interval: Some(Duration::from_millis(10)),
            callback_id: 7,
        });
        page.storage.lock().unwrap().insert("k".into(), "v".into());
        push_state(&page, "https://a.test/about");
        {
            let mut canvas = page.canvas.lock().unwrap();
            canvas.width = 640;
            canvas.height = 480;
        }

        let doc = new_document(&page, "https://b.test/other.wasm");
        assert!(doc.timers.lock().unwrap().is_empty());
        assert!(doc.storage.lock().unwrap().is_empty());
        assert!(!Arc::ptr_eq(&doc.events, &page.events));
        let canvas = doc.canvas.lock().unwrap();
        assert_eq!((canvas.width, canvas.height), (640, 480));
        assert!(canvas.commands.is_empty());
        drop(canvas);
        assert!(Arc::ptr_eq(&doc.permissions, &page.permissions));
        assert!(Arc::ptr_eq(&doc.input_state, &page.input_state));
        assert!(Arc::ptr_eq(&doc.focused, &page.focused));
        // Its own budget: cancelling the old document doesn't cancel it.
        page.budget.cancel();
        assert!(!doc.budget.is_cancelled());
        assert_eq!(current_url(&doc), "https://b.test/other.wasm");
        assert_eq!(doc.navigation.lock().unwrap().len(), 1);
    }

    #[test]
    fn session_storage_survives_a_same_origin_document() {
        let page = HostState::default();
        set_module_origin(&page, "https://a.test/app.wasm");
        page.storage.lock().unwrap().insert("k".into(), "v".into());

        let doc = new_document(&page, "https://a.test/app.wasm");
        // The loader sets the origin again before the module runs; it must not clear anything.
        set_module_origin(&doc, "https://a.test/app.wasm");
        assert_eq!(
            doc.storage.lock().unwrap().get("k").map(String::as_str),
            Some("v")
        );
    }

    #[test]
    fn replaceable_events_keep_only_the_latest_per_view() {
        let (a, b) = (ViewId::next(), ViewId::next());
        let mut out = Outbox::default();
        let title = |view, title: &str| EngineEvent::TitleChanged {
            view,
            title: Some(title.into()),
        };
        out.push(title(a, "1"));
        out.push(EngineEvent::ViewReady { view: a });
        out.push(title(b, "x"));
        out.push(title(a, "2"));
        out.push(EngineEvent::ViewReady { view: a });
        assert_eq!(
            out.take(),
            [
                EngineEvent::ViewReady { view: a },
                title(b, "x"),
                title(a, "2"),
                EngineEvent::ViewReady { view: a },
            ]
        );
    }

    /// Jobs that run nothing and keep count of what they were given.
    #[derive(Default)]
    struct Recorded {
        loads: std::cell::RefCell<Vec<HostState>>,
        retired: std::cell::Cell<usize>,
    }

    impl Jobs for Recorded {
        fn load(&self, _: ViewId, _: NavigationId, state: HostState, _: String) {
            self.loads.borrow_mut().push(state);
        }

        fn run(&self, _: &Arc<LiveGuest>, _: Instant, _: u64) {}

        fn retire(&self, _: Box<dyn Send>) {
            self.retired.set(self.retired.get() + 1);
        }
    }

    #[test]
    fn replaced_loads_and_closed_views_stop_and_are_dropped_elsewhere() {
        use sighurt_api::geometry::{Scale, WindowRect};
        let engine = crate::engine::tests::engine_with(Default::default());
        let rect = WindowRect {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let metrics = Metrics::new(
            LayoutEpoch::next(),
            rect,
            Scale::from_fractional(120).unwrap(),
        );
        let mut view = View::new(ViewId::next(), engine, metrics, ViewOptions::default());
        let (jobs, mut out) = (Recorded::default(), Outbox::default());
        for url in ["https://a.test/app.wasm", "https://b.test/app.wasm"] {
            let navigation = NavigationId::next();
            let url = url.to_string();
            let command = EngineCommand::Navigate { navigation, url };
            view.command(command, &jobs, &mut out).unwrap();
        }
        // The second navigation cancelled the first load and handed its state away.
        let loads = jobs.loads.take();
        assert!(loads[0].budget.is_cancelled());
        assert!(!loads[1].budget.is_cancelled());
        assert_eq!(jobs.retired.get(), 1);
        let page = view.page.budget.clone();
        view.close(&jobs);
        assert!(page.is_cancelled() && loads[1].budget.is_cancelled());
        assert_eq!(jobs.retired.get(), 2);
    }
}
