//! A demo engine that exists to test the host, the chrome and the engine contract without
//! building Servo (`sig` with `--no-default-features --features engine-demo`).
//!
//! Implements `sighurt_api::engine::EngineRuntime` with CPU drawing through a
//! [`CpuTarget`](sighurt_api::present::CpuTarget) ([`DemoRuntime`]). `sig` binds the default,
//! the Wayland host's shared-memory target lease. Its pages, by URL ([`Page`]):
//!
//! - `demo:pattern`: a static pattern. It paints once per size and then does nothing while idle.
//! - `demo:animation`: an animation paced by the host's frame callbacks: after each presented
//!   frame it asks for the next one (`FrameReady`), and the host issues the permit when the
//!   compositor's callback comes. It never runs a timer, and stops while hidden.
//! - `demo:slow`: a slow frame. After each resize (and each click) its frame for the new layout
//!   is ready only [`SLOW_FRAME`] later, longer than the host's resize grace period, so the
//!   host's slow-resize fallback shows. It waits with a pump deadline and never blocks the loop.
//! - `demo:resize`: follows every resize and scale change: a checkerboard of 16-logical-pixel
//!   squares framed by a one-buffer-pixel red border, and crosshairs where the pointer is, with
//!   the pointer's surface-local and buffer positions as status text. A screenshot shows
//!   whether buffer size, scale and input coordinates agree.
//! - `demo:error`: a recoverable error: the navigation fails and the previous page stays.
//! - `demo:crash`: another: a click or key press fails the view (`ViewFailed`), and reloading
//!   recovers it.
//! - `demo:fullscreen`: an element fullscreen, as a video's button asks for it: a click or Enter
//!   reports `FullscreenChanged { fullscreen: true }`, and `ExitFullscreen` leaves it
//!   (`FullscreenChanged { fullscreen: false }`). `demo:fullscreen-unasked` reports `true` as
//!   it commits, with no click: what a page may not do by itself.
//!
//! Both `demo:page` and `demo://page` name a page; the address bar keeps only the second form as
//! typed (it reads `demo:pattern` as a search, as it does any text without `://`), and routing
//! needs `[routing.demo] schemes = ["demo"]` in the configuration.
//!
//! Every page shows a square in its top-left corner, green while the page has the keyboard, and
//! typing into a page appends the text to its title and shows it in the status line, so a
//! screenshot shows where keys went. Focusing the text field, or setting its value, through a
//! semantic action reports `TextInputChanged`; a new document clears it. `CAPABILITIES.text_input`
//! stays false: that flag is IME composition, which this engine does not do.
//!
//! Find in page searches the page's own text, the strings of its content tree ([`snapshot`]):
//! the matches are counted in order, `Next` and `Previous` walk them and wrap around, and each
//! answers with `FindResult` (found, current and total). Nothing is highlighted, and a new
//! document ends the search.
//!
//! The engine has no history of its own (`history_traversal: false`, so the core gives it a view
//! per page) and no page zoom. Visiting a page records the site name `demo` (a page that fails
//! to load does not). [`EngineRuntime::site_data`] lists those names and forgets them, in
//! memory: a forget answers on the next pump with `durable` false and `profile` true, since
//! the engine has a profile and nothing of it is on disk. Everything happens on the owner
//! thread, synchronously, so it needs no waker; the slow page's deadline comes back from `pump`.
//!
//! Must not own: anything a real engine would not; it goes through the same contract.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod paint;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::mem::{discriminant, Discriminant};
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, FindCommand, PresentOutcome,
    PumpStatus, SiteDataKinds, SiteDataRequest, SiteKey, ViewOptions,
};
use sighurt_api::events::{
    Cursor, EngineEvent, NavigationError, OpenDisposition, SiteDataOutcome, SiteEntry, MAX_SITES,
};
use sighurt_api::geometry::{Metrics, SurfacePoint, SurfaceRect};
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, RequestId, ViewId};
use sighurt_api::input::{ButtonState, InputKind, Key, PointerEvent, TextInputState, TextPurpose};
use sighurt_api::present::CpuTarget;
use sighurt_api::semantic::{NodeId, NodeKind, SemanticAction, SemanticNode, SemanticSnapshot};
use sighurt_wayland::TargetLease;

/// The engine's name in the configuration and the registry.
pub const NAME: &str = "demo";

/// What the demo engine supports: content as structure (a small fixed tree, see [`snapshot`]),
/// find in page over that tree's text, cookies and storage in memory
/// ([`EngineRuntime::site_data`]), and element fullscreen (`demo:fullscreen`). None of the
/// other optional operations. `text_input` is IME composition, not the text field's focus, which is
/// reported as `TextInputChanged`.
pub const CAPABILITIES: EngineCapabilities = EngineCapabilities {
    history_traversal: false,
    text_input: false,
    accessibility: false,
    screenshots: false,
    find_in_page: true,
    page_zoom: false,
    pointer_lock: false,
    native_popups: false,
    process_per_view: false,
    semantics: true,
    site_data: SiteDataKinds {
        cookies: true,
        storage: true,
        caches: false,
        hsts: false,
    },
    fullscreen: true,
};

/// How long `demo:slow` takes to have a frame for a new layout: longer than the host's resize
/// grace period (150 ms), so the fallback shows.
pub const SLOW_FRAME: Duration = Duration::from_millis(400);

/// Most events one pump hands over.
const MAX_EVENTS_PER_PUMP: usize = 256;
/// Characters of typed text a page keeps in its title.
const MAX_TYPED: usize = 40;

/// A page of the demo engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// A static pattern.
    Pattern,
    /// An animation paced by frame callbacks.
    Animation,
    /// A page whose frame for each new layout comes [`SLOW_FRAME`] late.
    Slow,
    /// A checkerboard with crosshairs at the pointer.
    Resize,
    /// A page that fails its view when clicked or typed into.
    Crash,
    /// A page whose element goes fullscreen when clicked, or on Enter.
    Fullscreen,
    /// A page whose element goes fullscreen as it commits, unasked.
    FullscreenUnasked,
}

impl Page {
    /// The page for `url` (`demo:pattern`, `demo://animation`, ...), or why it can't be shown.
    pub fn from_url(url: &str) -> Result<Self, NavigationError> {
        let rest = match url.split_once(':') {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case(NAME) => rest,
            _ => return Err(NavigationError::Load(format!("{url} is not a demo page"))),
        };
        let name = rest.trim_start_matches('/').split(['/', '?', '#']).next();
        match name.unwrap_or_default().to_ascii_lowercase().as_str() {
            "" | "pattern" => Ok(Self::Pattern),
            "animation" => Ok(Self::Animation),
            "slow" => Ok(Self::Slow),
            "resize" => Ok(Self::Resize),
            "crash" => Ok(Self::Crash),
            "fullscreen" => Ok(Self::Fullscreen),
            "fullscreen-unasked" => Ok(Self::FullscreenUnasked),
            "error" => Err(NavigationError::Load(
                "demo:error fails to load on purpose; the previous page stays".into(),
            )),
            other => Err(NavigationError::Load(format!(
                "the demo engine has no page called {other:?}"
            ))),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Pattern => "Demo: pattern",
            Self::Animation => "Demo: animation",
            Self::Slow => "Demo: slow frame",
            Self::Resize => "Demo: resize",
            Self::Crash => "Demo: click to crash",
            Self::Fullscreen => "Demo: click for fullscreen",
            Self::FullscreenUnasked => "Demo: fullscreen, unasked",
        }
    }
}

/// One view: its target and what its page shows.
struct View<T: CpuTarget> {
    target: T,
    metrics: Metrics,
    visible: bool,
    focused: bool,
    /// The committed page and its URL.
    page: Option<(Page, String)>,
    /// The view's document failed (`demo:crash`); reloading recovers it.
    failed: bool,
    typed: String,
    /// The text field has focus, as a semantic `Focus` or `SetValue` left it.
    field: bool,
    pointer: Option<SurfacePoint>,
    /// The animation's clock.
    started: Instant,
    /// `demo:slow`: the layout whose frame is ready, and when the next one will be.
    rendered: Option<LayoutEpoch>,
    ready_at: Option<(LayoutEpoch, Instant)>,
    /// Something new to paint, reported as `FrameReady` at the next pump.
    dirty: bool,
    /// The committed document, which semantic actions must name.
    document: Option<DocumentGeneration>,
    /// The frontend asked for content snapshots.
    semantics: bool,
    /// The demo form's state: the checkbox and the select's choice.
    agreed: bool,
    colour: u32,
    /// The search in the page: its id, how many matches it has and which one is selected
    /// (from 1; 0 with none).
    find: Option<(RequestId, u32, u32)>,
    /// The page's element is fullscreen (`demo:fullscreen`).
    fullscreen: bool,
}

impl<T: CpuTarget> View<T> {
    fn new(target: T, options: ViewOptions) -> Self {
        Self {
            metrics: target.metrics(),
            target,
            visible: options.visible,
            focused: false,
            page: None,
            failed: false,
            typed: String::new(),
            field: false,
            pointer: None,
            started: Instant::now(),
            rendered: None,
            ready_at: None,
            dirty: false,
            document: None,
            semantics: false,
            agreed: false,
            colour: 0,
            find: None,
            fullscreen: false,
        }
    }

    /// The page's content as the frontend asked for it, if it did and there is a page.
    fn snapshot_event(&self, id: ViewId) -> Option<EngineEvent> {
        let (page, _) = self.page.as_ref()?;
        let document = self.document.filter(|_| self.semantics)?;
        let snapshot = snapshot(
            document,
            page.title(),
            &self.typed,
            self.agreed,
            self.colour,
        );
        Some(EngineEvent::SemanticsChanged {
            view: id,
            snapshot: Box::new(snapshot),
        })
    }

    fn page(&self) -> Option<Page> {
        self.page.as_ref().map(|(page, _)| *page)
    }

    /// Carries out a search command. A move or stop for another search does nothing.
    fn find(&mut self, id: ViewId, command: FindCommand, queue: &mut Vec<EngineEvent>) {
        let (search, current) = match command {
            FindCommand::Search {
                search,
                document,
                text,
                case_sensitive,
            } => {
                let Some((page, _)) = self
                    .page
                    .as_ref()
                    .filter(|_| self.document == Some(document))
                else {
                    return;
                };
                let snapshot = snapshot(
                    document,
                    page.title(),
                    &self.typed,
                    self.agreed,
                    self.colour,
                );
                let total = count_matches(&snapshot, &text, case_sensitive);
                self.find = Some((search, total, u32::from(total > 0)));
                (search, u32::from(total > 0))
            }
            FindCommand::Next { search } | FindCommand::Previous { search } => {
                let next = matches!(command, FindCommand::Next { .. });
                let Some((_, total, current)) = self.find.as_mut().filter(|(s, ..)| *s == search)
                else {
                    return;
                };
                if *total > 0 {
                    *current = match next {
                        true => *current % *total + 1,
                        false => (*current + *total - 2) % *total + 1,
                    };
                }
                (search, *current)
            }
            FindCommand::Stop { search } => {
                if self.find.is_some_and(|(s, ..)| s == search) {
                    self.find = None;
                }
                return;
            }
        };
        let (Some(document), Some((_, total, _))) = (self.document, self.find) else {
            return;
        };
        queue.push(EngineEvent::FindResult {
            view: id,
            document,
            search,
            found: total > 0,
            current: (total > 0).then_some(current),
            total: Some(total),
        });
    }

    fn title(&self) -> Option<String> {
        let page = self.page()?;
        Some(match self.typed.is_empty() {
            true => page.title().to_owned(),
            false => format!("{} · {}", page.title(), self.typed),
        })
    }

    /// The status line: on `demo:resize` where the pointer is, elsewhere what was typed.
    fn status(&self) -> Option<String> {
        match self.page()? {
            Page::Resize => self.pointer.map(|p| {
                let b = self.metrics.to_buffer(p);
                format!(
                    "demo:resize: pointer at {:.1}, {:.1} on the page, buffer pixel {:.1}, {:.1}",
                    p.x, p.y, b.x, b.y
                )
            }),
            _ if self.typed.is_empty() => None,
            _ => Some(format!("Typed into the page: {}", self.typed)),
        }
    }

    /// Reports a change of what was typed: in the title, and in the status line.
    fn typed_changed(&self, id: ViewId, queue: &mut Vec<EngineEvent>) {
        queue.push(EngineEvent::TitleChanged {
            view: id,
            title: self.title(),
        });
        queue.push(EngineEvent::StatusTextChanged {
            view: id,
            text: self.status(),
        });
    }

    /// `demo:slow` starts rendering a frame for its current layout.
    fn start_slow_frame(&mut self, now: Instant) {
        if self.page() == Some(Page::Slow) && !self.metrics.is_empty() {
            self.rendered = None;
            self.ready_at = Some((self.metrics.epoch(), now + SLOW_FRAME));
        }
    }
}

/// The demo engine's runtime: every demo view of the browser.
///
/// Views present into `T`. The default is the Wayland host's shared-memory [`TargetLease`].
pub struct DemoRuntime<T: CpuTarget = TargetLease> {
    views: BTreeMap<ViewId, View<T>>,
    /// Events not handed over yet, in order.
    queue: Vec<EngineEvent>,
    /// Hosts whose pages stored something. Visiting a demo page adds `demo`.
    sites: BTreeSet<String>,
    /// Site-data requests accepted and not yet answered. The next pump answers them.
    site_jobs: Vec<SiteDataRequest>,
    shut_down: bool,
}

impl<T: CpuTarget> Default for DemoRuntime<T> {
    fn default() -> Self {
        Self {
            views: BTreeMap::new(),
            queue: Vec::new(),
            sites: BTreeSet::new(),
            site_jobs: Vec::new(),
            shut_down: false,
        }
    }
}

impl<T: CpuTarget> DemoRuntime<T> {
    /// A runtime with no views.
    pub fn new() -> Self {
        Self::default()
    }

    fn running(&self) -> Result<(), EngineError> {
        match self.shut_down {
            true => Err(EngineError::Runtime("the demo engine was shut down".into())),
            false => Ok(()),
        }
    }

    /// Loads `url` into `view` as `navigation`: it commits at once, or fails.
    fn navigate(&mut self, id: ViewId, navigation: NavigationId, url: String) {
        let page = Page::from_url(&url);
        let view = self.views.get_mut(&id).expect("checked by the caller");
        let queue = &mut self.queue;
        queue.push(EngineEvent::NavigationStarted {
            view: id,
            navigation,
            url: url.clone(),
        });
        queue.push(EngineEvent::LoadingChanged {
            view: id,
            loading: true,
        });
        match page {
            Ok(page) => {
                self.sites.insert("demo".into());
                view.page = Some((page, url.clone()));
                view.failed = false;
                view.typed.clear();
                view.field = false;
                view.pointer = None;
                view.started = Instant::now();
                view.dirty = true;
                view.start_slow_frame(Instant::now());
                let document = DocumentGeneration::next();
                view.document = Some(document);
                view.find = None;
                view.agreed = false;
                view.colour = 0;
                // The old document's element went with it.
                view.fullscreen = page == Page::FullscreenUnasked;
                queue.push(EngineEvent::NavigationCommitted {
                    view: id,
                    navigation,
                    url,
                    document,
                    history: None,
                });
                if view.fullscreen {
                    queue.push(EngineEvent::FullscreenChanged {
                        view: id,
                        document,
                        fullscreen: true,
                    });
                }
                queue.push(EngineEvent::TitleChanged {
                    view: id,
                    title: view.title(),
                });
                let cursor = match page {
                    Page::Resize => Cursor::Crosshair,
                    Page::Crash | Page::Fullscreen => Cursor::Pointer,
                    _ => Cursor::Default,
                };
                queue.push(EngineEvent::CursorChanged { view: id, cursor });
                queue.push(EngineEvent::StatusTextChanged {
                    view: id,
                    text: None,
                });
                queue.push(EngineEvent::TextInputChanged {
                    view: id,
                    state: None,
                });
                queue.extend(view.snapshot_event(id));
            }
            Err(error) => queue.push(EngineEvent::NavigationFailed {
                view: id,
                navigation,
                error,
            }),
        }
        queue.push(EngineEvent::LoadingChanged {
            view: id,
            loading: false,
        });
    }

    /// A semantic action on the demo page's fixed tree ([`snapshot`]).
    fn act(&mut self, id: ViewId, action: SemanticAction) -> Result<(), EngineError> {
        let view = self.views.get_mut(&id).expect("checked by the caller");
        match action {
            SemanticAction::Activate(NodeId(node @ (NODE_LINK_ANIMATION | NODE_LINK_RESIZE))) => {
                let url = match node {
                    NODE_LINK_ANIMATION => "demo:animation",
                    _ => "demo:resize",
                };
                self.queue.push(EngineEvent::OpenRequested {
                    view: id,
                    document: view.document.expect("checked by the caller"),
                    url: url.into(),
                    disposition: OpenDisposition::CurrentView,
                });
                return Ok(());
            }
            SemanticAction::Activate(NodeId(NODE_CHECKBOX)) => view.agreed = !view.agreed,
            SemanticAction::Activate(NodeId(NODE_BUTTON)) => {
                view.typed = "pressed".into();
                view.typed_changed(id, &mut self.queue);
            }
            SemanticAction::SetValue {
                node: NodeId(NODE_FIELD),
                value,
            } => {
                view.typed = value;
                keep_last_chars(&mut view.typed, MAX_TYPED);
                view.field = true;
                view.typed_changed(id, &mut self.queue);
                self.queue.push(EngineEvent::TextInputChanged {
                    view: id,
                    state: Some(text_field_state()),
                });
            }
            SemanticAction::SelectOption {
                node: NodeId(NODE_SELECT),
                index,
            } if index < 3 => view.colour = index,
            SemanticAction::Focus(NodeId(node)) => {
                let focused = node == NODE_FIELD;
                if view.field != focused {
                    view.field = focused;
                    self.queue.push(EngineEvent::TextInputChanged {
                        view: id,
                        state: focused.then(text_field_state),
                    });
                }
                return Ok(());
            }
            SemanticAction::ScrollIntoView(_) => return Ok(()),
            _ => {
                return Err(EngineError::View {
                    view: id,
                    message: "the demo page has no such control".into(),
                })
            }
        }
        self.queue.extend(view.snapshot_event(id));
        Ok(())
    }

    fn input(&mut self, id: ViewId, kind: InputKind) {
        let view = self.views.get_mut(&id).expect("checked by the caller");
        let queue = &mut self.queue;
        let mut activated = false;
        let (mut enter, mut pressed) = (false, false);
        match kind {
            InputKind::Pointer(pointer) => match pointer {
                PointerEvent::Enter { position } | PointerEvent::Motion { position, .. } => {
                    view.pointer = Some(position);
                }
                PointerEvent::Leave => view.pointer = None,
                PointerEvent::Button {
                    state: ButtonState::Pressed,
                    position,
                    ..
                } => {
                    view.pointer = Some(position);
                    activated = true;
                    pressed = true;
                }
                PointerEvent::Button { .. } | PointerEvent::Scroll { .. } => {}
            },
            InputKind::Key(key) if key.state == ButtonState::Pressed => {
                // An Alt shortcut nobody bound types nothing, as in the chrome's address line,
                // unless the layout used Alt to make the character.
                let shortcut = key.modifiers.alt && !key.consumed.alt;
                let typed = match (&key.key, key.text) {
                    (Key::Named(name), _) if name == "Backspace" => view.typed.pop().is_some(),
                    (_, Some(text)) if !shortcut => {
                        view.typed.push_str(&text);
                        true
                    }
                    _ => false,
                };
                if typed {
                    keep_last_chars(&mut view.typed, MAX_TYPED);
                    view.typed_changed(id, queue);
                }
                enter = matches!(&key.key, Key::Named(name) if name == "Enter");
                activated = true;
            }
            InputKind::TextInput(text) => {
                if let Some(commit) = text.commit {
                    view.typed.push_str(&commit);
                    keep_last_chars(&mut view.typed, MAX_TYPED);
                    view.typed_changed(id, queue);
                }
            }
            InputKind::Key(_) | InputKind::Focus(_) => {}
        }
        match view.page() {
            Some(Page::Resize) => {
                view.dirty = true;
                let text = view.status();
                queue.push(EngineEvent::StatusTextChanged { view: id, text });
            }
            Some(Page::Slow) if activated => view.start_slow_frame(Instant::now()),
            Some(Page::Fullscreen) if (pressed || enter) && !view.fullscreen => {
                view.fullscreen = true;
                queue.push(EngineEvent::FullscreenChanged {
                    view: id,
                    document: view.document.expect("a page has a document"),
                    fullscreen: true,
                });
            }
            Some(Page::Crash) if activated && !view.failed => {
                view.failed = true;
                view.dirty = true;
                queue.push(EngineEvent::ViewFailed {
                    view: id,
                    message: "demo:crash crashed on purpose; reload to recover".into(),
                });
            }
            _ => {}
        }
    }
}

const NODE_LINK_ANIMATION: u64 = 5;
const NODE_LINK_RESIZE: u64 = 6;
const NODE_FIELD: u64 = 7;
const NODE_CHECKBOX: u64 = 8;
const NODE_SELECT: u64 = 9;
const NODE_BUTTON: u64 = 10;

/// The content every demo page reports: a heading, a paragraph, two links to other demo pages, a
/// text field holding `typed`, a checkbox, a select and a button. Node ids are fixed, so actions
/// name them.
fn snapshot(
    document: DocumentGeneration,
    title: &str,
    typed: &str,
    agreed: bool,
    colour: u32,
) -> SemanticSnapshot {
    let node = |id: u64, kind: NodeKind| SemanticNode {
        id: NodeId(id),
        kind,
        ..SemanticNode::default()
    };
    let text = |id: u64, kind: NodeKind, text: &str| SemanticNode {
        text: Some(text.into()),
        ..node(id, kind)
    };
    let link = |id: u64, label: &str, href: &str| SemanticNode {
        href: Some(href.into()),
        ..text(id, NodeKind::Link, label)
    };
    let mut root = node(1, NodeKind::Document);
    // Each control sits in a paragraph of its own (ids 20 to 25), so each is a line.
    root.children = [2, 3, 20, 21, 22, 23, 24, 25].map(NodeId).to_vec();
    let mut paragraph = node(3, NodeKind::Paragraph);
    paragraph.children = vec![NodeId(4)];
    let mut field = node(NODE_FIELD, NodeKind::TextField);
    field.label = Some("Name".into());
    field.value = Some(typed.into());
    let mut checkbox = node(NODE_CHECKBOX, NodeKind::Checkbox);
    checkbox.label = Some("Agree".into());
    checkbox.checked = Some(agreed);
    let mut select = node(NODE_SELECT, NodeKind::Select);
    select.label = Some("Colour".into());
    select.options = ["red", "green", "blue"].map(String::from).to_vec();
    select.selected = Some(colour.min(2));
    SemanticSnapshot {
        document,
        root: NodeId(1),
        nodes: vec![
            root,
            text(2, NodeKind::Heading(1), title),
            paragraph,
            text(4, NodeKind::Text, "This page has content for text mode."),
            link(NODE_LINK_ANIMATION, "Animation page", "demo:animation"),
            link(NODE_LINK_RESIZE, "Resize page", "demo:resize"),
            field,
            checkbox,
            select,
            text(NODE_BUTTON, NodeKind::Button, "Press me"),
        ]
        .into_iter()
        .chain((20..26).map(|id| {
            let mut line = node(id, NodeKind::Paragraph);
            line.children = vec![NodeId(id - 15)];
            line
        }))
        .collect(),
        focus: None,
        truncated: false,
    }
}

/// How many times `text` occurs, without overlapping, in the strings of `snapshot`: its nodes'
/// text, labels, values and options. Without `case_sensitive`, case does not matter.
fn count_matches(snapshot: &SemanticSnapshot, text: &str, case_sensitive: bool) -> u32 {
    let fold = |s: &str| match case_sensitive {
        true => s.to_owned(),
        false => s.to_lowercase(),
    };
    let needle = fold(text);
    if needle.is_empty() {
        return 0;
    }
    let strings = snapshot.nodes.iter().flat_map(|node| {
        let own = [&node.text, &node.label, &node.value];
        own.into_iter().flatten().chain(&node.options)
    });
    let count: usize = strings.map(|s| fold(s).matches(&needle).count()).sum();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The text field's input state. The caret is unused: the terminal has no input method.
fn text_field_state() -> TextInputState {
    TextInputState {
        caret: SurfaceRect::default(),
        surrounding: None,
        purpose: TextPurpose::Normal,
        multiline: false,
    }
}

/// Keeps the last `max` characters of `text`.
fn keep_last_chars(text: &mut String, max: usize) {
    let count = text.chars().count();
    if count > max {
        let cut = text.char_indices().nth(count - max).map_or(0, |(i, _)| i);
        text.drain(..cut);
    }
}

/// Drops every replaceable event that a later one of the same kind for the same view
/// supersedes; the later keeps its place.
fn coalesce(events: &mut Vec<EngineEvent>) {
    fn replaceable(event: &EngineEvent) -> bool {
        matches!(
            event,
            EngineEvent::TitleChanged { .. }
                | EngineEvent::StatusTextChanged { .. }
                | EngineEvent::CursorChanged { .. }
                | EngineEvent::LoadingChanged { .. }
                | EngineEvent::HistoryChanged { .. }
                | EngineEvent::TextInputChanged { .. }
                | EngineEvent::FrameReady { .. }
                | EngineEvent::SemanticsChanged { .. }
                | EngineEvent::FindResult { .. }
        )
    }
    let mut seen: HashSet<(Discriminant<EngineEvent>, Option<ViewId>)> = HashSet::new();
    let mut keep: Vec<bool> = events
        .iter()
        .rev()
        .map(|event| !replaceable(event) || seen.insert((discriminant(event), event.view())))
        .collect();
    keep.reverse();
    let mut keep = keep.into_iter();
    events.retain(|_| keep.next().unwrap_or(true));
}

impl<T: CpuTarget> EngineRuntime for DemoRuntime<T> {
    type Target = T;
    type FramePermit = T::Permit;

    fn capabilities(&self) -> EngineCapabilities {
        CAPABILITIES
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: ViewOptions,
        target: T,
    ) -> Result<(), EngineError> {
        self.running()?;
        if self.views.contains_key(&view) {
            return Err(EngineError::View {
                view,
                message: "this view is already open".into(),
            });
        }
        self.views.insert(view, View::new(target, options));
        self.queue.push(EngineEvent::ViewReady { view });
        Ok(())
    }

    fn command(&mut self, id: ViewId, command: EngineCommand) -> Result<(), EngineError> {
        self.running()?;
        let view = self
            .views
            .get_mut(&id)
            .ok_or(EngineError::UnknownView(id))?;
        match command {
            EngineCommand::Navigate { navigation, url } => self.navigate(id, navigation, url),
            EngineCommand::Reload { navigation } => match view.page.clone() {
                Some((_, url)) => self.navigate(id, navigation, url),
                None => self.queue.push(EngineEvent::NavigationFailed {
                    view: id,
                    navigation,
                    error: NavigationError::Load("there is nothing to reload".into()),
                }),
            },
            // Every navigation commits or fails at once: there is never anything to stop.
            EngineCommand::Stop => {}
            EngineCommand::Traverse { .. } => {
                return Err(EngineError::Unsupported("history traversal"))
            }
            EngineCommand::SetZoom(_) => return Err(EngineError::Unsupported("page zoom")),
            EngineCommand::SetMetrics(metrics) => {
                view.metrics = metrics;
                view.dirty = true;
                view.start_slow_frame(Instant::now());
            }
            EngineCommand::SetVisibility(visible) => {
                view.visible = visible;
                view.dirty |= visible;
            }
            EngineCommand::SetFocus(focused) => {
                view.dirty |= view.focused != focused;
                view.focused = focused;
            }
            EngineCommand::Input(event) => self.input(id, event.kind),
            // The demo engine asks for no services and has no links, so there is nothing to
            // resolve or answer.
            EngineCommand::ResolveServiceRequest { .. }
            | EngineCommand::AnswerNavigation { .. } => {}
            EngineCommand::SetSemantics(on) => {
                view.semantics = on;
                self.queue.extend(view.snapshot_event(id).filter(|_| on));
            }
            EngineCommand::Semantic { document, action } => {
                // An action on a replaced document is dropped.
                if view.document == Some(document) {
                    self.act(id, action)?;
                }
            }
            EngineCommand::Find(command) => view.find(id, command, &mut self.queue),
            EngineCommand::ExitFullscreen => {
                if std::mem::take(&mut view.fullscreen) {
                    self.queue.push(EngineEvent::FullscreenChanged {
                        view: id,
                        document: view.document.expect("a page has a document"),
                        fullscreen: false,
                    });
                }
            }
        }
        Ok(())
    }

    fn site_data(&mut self, request: SiteDataRequest) -> Result<(), EngineError> {
        self.running()?;
        self.site_jobs.push(request);
        Ok(())
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        if self.shut_down {
            return PumpStatus::Idle;
        }
        let jobs = std::mem::take(&mut self.site_jobs);
        let mut answers = Vec::with_capacity(jobs.len());
        for job in jobs {
            answers.push(self.answer_site(job));
        }
        self.queue.extend(answers);
        let now = Instant::now();
        let mut deadline: Option<Instant> = None;
        for (&id, view) in &mut self.views {
            if let Some((epoch, at)) = view.ready_at {
                if at <= now {
                    view.ready_at = None;
                    view.rendered = Some(epoch);
                    view.dirty = true;
                } else {
                    deadline = Some(deadline.map_or(at, |d| d.min(at)));
                }
            }
            // A hidden or empty view paints nothing; showing it or giving it room marks it
            // dirty again.
            if std::mem::take(&mut view.dirty)
                && view.visible
                && view.page.is_some()
                && !view.metrics.is_empty()
            {
                self.queue.push(EngineEvent::FrameReady { view: id });
            }
        }
        coalesce(&mut self.queue);
        let count = self.queue.len().min(MAX_EVENTS_PER_PUMP);
        events.extend(self.queue.drain(..count));
        match (self.queue.is_empty(), deadline) {
            (false, _) => PumpStatus::Pending { deadline: None },
            (true, Some(at)) => PumpStatus::Pending { deadline: Some(at) },
            (true, None) => PumpStatus::Idle,
        }
    }

    fn present(&mut self, id: ViewId, permit: T::Permit) -> Result<PresentOutcome, EngineError> {
        self.running()?;
        let view = self
            .views
            .get_mut(&id)
            .ok_or(EngineError::UnknownView(id))?;
        let Some(page) = view.page() else {
            return Ok(PresentOutcome::NotReady);
        };
        // The permit is for one layout epoch. `demo:slow` shows nothing until its frame for
        // that epoch is ready; dropping the permit presents nothing.
        if page == Page::Slow && view.rendered != Some(T::permit_epoch(&permit)) {
            return Ok(PresentOutcome::NotReady);
        }
        // The host issues the permit for the target's current metrics. `SetMetrics` for that
        // layout can still be queued, so the view's copy may be one layout behind.
        let metrics = view.target.metrics();
        let scene = paint::Scene {
            page,
            failed: view.failed,
            focused: view.focused,
            pointer: view.pointer,
            elapsed: view.started.elapsed(),
        };
        view.target
            .submit(permit, &mut |frame| {
                paint::paint(frame, &scene, metrics);
                Ok(())
            })
            .map_err(|message| EngineError::View { view: id, message })?;
        // An animation asks for its next frame as soon as this one is submitted; the host's
        // frame callback paces it.
        view.dirty |= page == Page::Animation && !view.failed;
        Ok(PresentOutcome::Submitted)
    }

    fn close_view(&mut self, id: ViewId) {
        // Dropping the view drops its target. A Wayland lease retires the host's target.
        if self.views.remove(&id).is_some() {
            self.queue.retain(|event| event.view() != Some(id));
            self.queue.push(EngineEvent::ViewClosed { view: id });
        }
    }

    fn shutdown(&mut self) {
        self.views.clear();
        self.queue.clear();
        self.site_jobs.clear();
        self.shut_down = true;
    }
}

impl<T: CpuTarget> DemoRuntime<T> {
    /// The kinds a forget or a list reports: cookies and storage, in memory.
    fn stored_kinds() -> SiteDataKinds {
        CAPABILITIES.site_data
    }

    /// The answer to `request`, from the sites visited so far.
    fn answer_site(&mut self, request: SiteDataRequest) -> EngineEvent {
        let kinds = Self::stored_kinds();
        match request {
            SiteDataRequest::List { request } => {
                let mut sites: Vec<SiteEntry> = self
                    .sites
                    .iter()
                    .map(|name| SiteEntry {
                        name: name.clone(),
                        kinds,
                    })
                    .collect();
                let more = sites.len() > MAX_SITES;
                sites.truncate(MAX_SITES);
                EngineEvent::SitesListed {
                    request,
                    sites,
                    more,
                }
            }
            SiteDataRequest::Forget { request, site } => EngineEvent::SiteDataDone {
                request,
                result: Ok(self.forget(site)),
            },
            SiteDataRequest::ForgetAll { request } => {
                let removed: Vec<String> = self.sites.iter().cloned().collect();
                self.sites.clear();
                EngineEvent::SiteDataDone {
                    request,
                    result: Ok(SiteDataOutcome {
                        removed,
                        kinds,
                        durable: false,
                        profile: true,
                    }),
                }
            }
        }
    }

    /// Forgets `site` and says what went. Nothing is on disk; the engine still has a profile.
    fn forget(&mut self, site: SiteKey) -> SiteDataOutcome {
        let host = match site {
            SiteKey::Host(host) => Some(host),
            SiteKey::LocalFiles => None,
        };
        let removed: Vec<String> = self
            .sites
            .iter()
            .filter(|name| host.as_ref().is_some_and(|host| site_covers(name, host)))
            .cloned()
            .collect();
        for name in &removed {
            self.sites.remove(name);
        }
        SiteDataOutcome {
            removed,
            kinds: Self::stored_kinds(),
            durable: false,
            profile: true,
        }
    }
}

/// `name` is `host`, or a name under it (`www.example.com` under `example.com`).
fn site_covers(name: &str, host: &str) -> bool {
    !host.is_empty()
        && (name == host
            || name
                .strip_suffix(host)
                .is_some_and(|rest| rest.ends_with('.')))
}

#[cfg(test)]
mod tests {
    use sighurt_api::ids::RequestId;

    use super::*;

    #[test]
    fn urls_name_pages() {
        assert_eq!(Page::from_url("demo:pattern"), Ok(Page::Pattern));
        assert_eq!(Page::from_url("demo://animation"), Ok(Page::Animation));
        assert_eq!(Page::from_url("DEMO:Slow?x#y"), Ok(Page::Slow));
        assert_eq!(Page::from_url("demo:"), Ok(Page::Pattern));
        assert_eq!(Page::from_url("demo://fullscreen"), Ok(Page::Fullscreen));
        assert_eq!(
            Page::from_url("demo:fullscreen-unasked"),
            Ok(Page::FullscreenUnasked)
        );
        assert!(matches!(
            Page::from_url("demo:error"),
            Err(NavigationError::Load(_))
        ));
        assert!(matches!(
            Page::from_url("demo:nope"),
            Err(NavigationError::Load(_))
        ));
        assert!(Page::from_url("https://demo.test/").is_err());
    }

    #[test]
    fn replaceable_events_keep_only_the_latest_per_view() {
        let (a, b) = (ViewId::next(), ViewId::next());
        let title = |view, title: &str| EngineEvent::TitleChanged {
            view,
            title: Some(title.into()),
        };
        let mut events = vec![
            title(a, "1"),
            EngineEvent::ViewReady { view: a },
            title(b, "b"),
            title(a, "2"),
            EngineEvent::FrameReady { view: a },
            EngineEvent::FrameReady { view: a },
        ];
        coalesce(&mut events);
        assert_eq!(
            events,
            vec![
                EngineEvent::ViewReady { view: a },
                title(b, "b"),
                title(a, "2"),
                EngineEvent::FrameReady { view: a },
            ]
        );
    }

    #[test]
    fn the_demo_tree_is_well_formed() {
        let snapshot = snapshot(DocumentGeneration::next(), "t", "x", true, 1);
        assert_eq!(snapshot.check(), Ok(()));
    }

    #[test]
    fn typed_text_is_bounded() {
        let mut text = "åäö".repeat(30);
        keep_last_chars(&mut text, MAX_TYPED);
        assert_eq!(text.chars().count(), MAX_TYPED);
        assert!(text.ends_with("åäö"));
    }

    /// A target that is never presented. The field's focus does not need pixels.
    struct Mem {
        metrics: Metrics,
    }

    struct Permit {
        epoch: LayoutEpoch,
    }

    impl CpuTarget for Mem {
        type Permit = Permit;

        fn metrics(&self) -> Metrics {
            self.metrics
        }

        fn permit_epoch(permit: &Self::Permit) -> LayoutEpoch {
            permit.epoch
        }

        fn submit(
            &mut self,
            _: Self::Permit,
            _: &mut dyn FnMut(sighurt_api::present::Frame<'_>) -> Result<(), String>,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    fn pump_all(runtime: &mut DemoRuntime<Mem>) -> Vec<EngineEvent> {
        let mut all = Vec::new();
        loop {
            let mut batch = Vec::new();
            let status = runtime.pump(&mut batch);
            all.extend(batch);
            match status {
                PumpStatus::Pending { deadline: None } => {}
                PumpStatus::Idle | PumpStatus::Pending { deadline: Some(_) } => break,
            }
        }
        all
    }

    fn last_text_input(events: &[EngineEvent]) -> Option<bool> {
        events.iter().rev().find_map(|event| match event {
            EngineEvent::TextInputChanged { state, .. } => Some(state.is_some()),
            _ => None,
        })
    }

    #[test]
    fn find_counts_the_pages_text_and_walks_the_matches_around() {
        use sighurt_api::geometry::{Scale, WindowRect};

        let mut runtime = DemoRuntime::new();
        let view = ViewId::next();
        let metrics = Metrics::new(
            LayoutEpoch::next(),
            WindowRect {
                x: 0,
                y: 0,
                width: 80,
                height: 40,
            },
            Scale::ONE,
        );
        runtime
            .open_view(view, ViewOptions::default(), Mem { metrics })
            .unwrap();
        let navigate = |runtime: &mut DemoRuntime<Mem>| {
            runtime
                .command(
                    view,
                    EngineCommand::Navigate {
                        navigation: NavigationId::next(),
                        url: "demo://pattern".into(),
                    },
                )
                .unwrap();
            pump_all(runtime)
                .iter()
                .find_map(|event| match event {
                    EngineEvent::NavigationCommitted { document, .. } => Some(*document),
                    _ => None,
                })
                .expect("committed")
        };
        let document = navigate(&mut runtime);
        let find = |runtime: &mut DemoRuntime<Mem>, command| {
            runtime.command(view, EngineCommand::Find(command)).unwrap();
            pump_all(runtime)
                .into_iter()
                .filter_map(|event| match event {
                    EngineEvent::FindResult {
                        document: d,
                        found,
                        current,
                        total,
                        ..
                    } => Some((d, found, current, total)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let search = RequestId::next();
        let start = |text: &str, case_sensitive| FindCommand::Search {
            search,
            document,
            text: text.into(),
            case_sensitive,
        };
        // "page": the paragraph and both links.
        let three = (document, true, Some(1), Some(3));
        assert_eq!(find(&mut runtime, start("page", false)), [three]);
        assert_eq!(
            find(&mut runtime, start("PAGE", false)),
            [three],
            "case does not matter"
        );
        assert_eq!(
            find(&mut runtime, start("PAGE", true)),
            [(document, false, None, Some(0))],
            "unless asked"
        );
        find(&mut runtime, start("page", false));
        let next = FindCommand::Next { search };
        let previous = FindCommand::Previous { search };
        assert_eq!(find(&mut runtime, next.clone())[0].2, Some(2));
        assert_eq!(find(&mut runtime, next.clone())[0].2, Some(3));
        assert_eq!(find(&mut runtime, next.clone())[0].2, Some(1), "wraps");
        assert_eq!(find(&mut runtime, previous.clone())[0].2, Some(3), "back");
        // Another search's move does nothing.
        let other = RequestId::next();
        assert!(find(&mut runtime, FindCommand::Next { search: other }).is_empty());
        // A new document ends the search.
        navigate(&mut runtime);
        assert!(find(&mut runtime, next.clone()).is_empty());
        // A search for an old document is dropped.
        assert!(find(&mut runtime, start("page", false)).is_empty());
        // Stop ends it.
        let document = navigate(&mut runtime);
        let start = FindCommand::Search {
            search,
            document,
            text: "Press".into(),
            case_sensitive: true,
        };
        assert_eq!(find(&mut runtime, start)[0].3, Some(1));
        assert!(find(&mut runtime, FindCommand::Stop { search }).is_empty());
        assert!(find(&mut runtime, previous).is_empty());
    }

    #[test]
    fn focusing_the_field_reports_text_input_and_a_new_document_clears_it() {
        use sighurt_api::engine::EngineRuntime;
        use sighurt_api::geometry::{Scale, WindowRect};

        let mut runtime = DemoRuntime::new();
        let view = ViewId::next();
        let metrics = Metrics::new(
            LayoutEpoch::next(),
            WindowRect {
                x: 0,
                y: 0,
                width: 80,
                height: 40,
            },
            Scale::ONE,
        );
        runtime
            .open_view(view, ViewOptions::default(), Mem { metrics })
            .unwrap();
        runtime
            .command(
                view,
                EngineCommand::Navigate {
                    navigation: NavigationId::next(),
                    url: "demo://pattern".into(),
                },
            )
            .unwrap();
        let events = pump_all(&mut runtime);
        assert_eq!(last_text_input(&events), Some(false));
        let document = events
            .iter()
            .find_map(|event| match event {
                EngineEvent::NavigationCommitted { document, .. } => Some(*document),
                _ => None,
            })
            .expect("committed");

        let semantic = |action| EngineCommand::Semantic { document, action };
        runtime
            .command(view, semantic(SemanticAction::Focus(NodeId(NODE_FIELD))))
            .unwrap();
        assert_eq!(last_text_input(&pump_all(&mut runtime)), Some(true));
        runtime
            .command(
                view,
                semantic(SemanticAction::SetValue {
                    node: NodeId(NODE_FIELD),
                    value: "zq".into(),
                }),
            )
            .unwrap();
        assert_eq!(last_text_input(&pump_all(&mut runtime)), Some(true));
        runtime
            .command(
                view,
                semantic(SemanticAction::Focus(NodeId(NODE_LINK_ANIMATION))),
            )
            .unwrap();
        assert_eq!(last_text_input(&pump_all(&mut runtime)), Some(false));
        runtime
            .command(view, semantic(SemanticAction::Focus(NodeId(NODE_FIELD))))
            .unwrap();
        runtime
            .command(
                view,
                EngineCommand::Navigate {
                    navigation: NavigationId::next(),
                    url: "demo://animation".into(),
                },
            )
            .unwrap();
        assert_eq!(last_text_input(&pump_all(&mut runtime)), Some(false));
    }

    fn open_page(runtime: &mut DemoRuntime<Mem>, url: &str) -> ViewId {
        use sighurt_api::geometry::{Scale, WindowRect};
        let view = ViewId::next();
        let metrics = Metrics::new(
            LayoutEpoch::next(),
            WindowRect {
                x: 0,
                y: 0,
                width: 80,
                height: 40,
            },
            Scale::ONE,
        );
        runtime
            .open_view(view, ViewOptions::default(), Mem { metrics })
            .unwrap();
        runtime
            .command(
                view,
                EngineCommand::Navigate {
                    navigation: NavigationId::next(),
                    url: url.into(),
                },
            )
            .unwrap();
        view
    }

    fn site_events(events: &[EngineEvent]) -> Vec<&EngineEvent> {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    EngineEvent::SitesListed { .. } | EngineEvent::SiteDataDone { .. }
                )
            })
            .collect()
    }

    #[test]
    fn visiting_a_page_stores_demo_and_a_forget_answers_on_the_next_pump() {
        let mut runtime = DemoRuntime::new();
        let view = open_page(&mut runtime, "demo://pattern");
        // Accepted, and not answered until the pump.
        let request = RequestId::next();
        runtime
            .site_data(SiteDataRequest::Forget {
                request,
                site: SiteKey::Host("demo".into()),
            })
            .unwrap();
        let events = pump_all(&mut runtime);
        let reports = site_events(&events);
        assert_eq!(reports.len(), 1, "{events:?}");
        match reports[0] {
            EngineEvent::SiteDataDone {
                request: id,
                result: Ok(outcome),
            } => {
                assert_eq!(*id, request);
                assert_eq!(outcome.removed, ["demo"]);
                assert!(!outcome.durable);
                assert!(outcome.profile);
                assert!(outcome.kinds.cookies && outcome.kinds.storage);
                assert!(!outcome.kinds.caches && !outcome.kinds.hsts);
            }
            other => panic!("{other:?}"),
        }

        // A second visit does not store the name twice, and the first forget took it.
        open_page(&mut runtime, "demo://animation");
        let again = RequestId::next();
        runtime
            .site_data(SiteDataRequest::Forget {
                request: again,
                site: SiteKey::Host("demo".into()),
            })
            .unwrap();
        // Two forgets queued before one pump both arrive: the reports are not replaceable.
        let other = RequestId::next();
        runtime
            .site_data(SiteDataRequest::Forget {
                request: other,
                site: SiteKey::Host("other.test".into()),
            })
            .unwrap();
        let events = pump_all(&mut runtime);
        let dones: Vec<_> = site_events(&events)
            .into_iter()
            .filter_map(|event| match event {
                EngineEvent::SiteDataDone {
                    request,
                    result: Ok(outcome),
                } => Some((*request, outcome.removed.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            dones,
            vec![(again, vec!["demo".into()]), (other, Vec::new())]
        );

        // Closing the view keeps the record; a failed page never adds one.
        runtime.close_view(view);
        let _ = pump_all(&mut runtime);
        let mut fresh = DemoRuntime::new();
        open_page(&mut fresh, "demo:error");
        let list = RequestId::next();
        fresh
            .site_data(SiteDataRequest::List { request: list })
            .unwrap();
        let events = pump_all(&mut fresh);
        assert!(
            matches!(
                site_events(&events).as_slice(),
                [EngineEvent::SitesListed { sites, more: false, .. }] if sites.is_empty()
            ),
            "{events:?}"
        );

        fresh.shutdown();
        let error = fresh.site_data(SiteDataRequest::List {
            request: RequestId::next(),
        });
        assert!(matches!(error, Err(EngineError::Runtime(_))), "{error:?}");
    }

    #[test]
    fn list_names_the_stored_site_and_forget_all_clears_it() {
        let mut runtime = DemoRuntime::new();
        open_page(&mut runtime, "demo:pattern");
        let _ = pump_all(&mut runtime);
        let list = RequestId::next();
        runtime
            .site_data(SiteDataRequest::List { request: list })
            .unwrap();
        let events = pump_all(&mut runtime);
        match site_events(&events).as_slice() {
            [EngineEvent::SitesListed {
                request,
                sites,
                more: false,
            }] => {
                assert_eq!(*request, list);
                assert_eq!(sites.len(), 1);
                assert_eq!(sites[0].name, "demo");
                assert_eq!(sites[0].kinds, CAPABILITIES.site_data);
            }
            other => panic!("{other:?}"),
        }
        runtime
            .site_data(SiteDataRequest::ForgetAll {
                request: RequestId::next(),
            })
            .unwrap();
        let events = pump_all(&mut runtime);
        assert!(
            matches!(
                site_events(&events).as_slice(),
                [EngineEvent::SiteDataDone { result: Ok(outcome), .. }]
                    if outcome.removed == ["demo"] && !outcome.durable && outcome.profile
            ),
            "{events:?}"
        );
        runtime
            .site_data(SiteDataRequest::List {
                request: RequestId::next(),
            })
            .unwrap();
        let events = pump_all(&mut runtime);
        assert!(
            matches!(
                site_events(&events).as_slice(),
                [EngineEvent::SitesListed { sites, more: false, .. }] if sites.is_empty()
            ),
            "{events:?}"
        );
    }
}
