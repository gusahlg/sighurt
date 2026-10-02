//! A demo engine that exists to test the host, the chrome and the engine contract without
//! building Servo (`sig` with `--no-default-features --features engine-demo`).
//!
//! Implements `sighurt_api::engine::EngineRuntime` with CPU drawing into the Wayland host's
//! shared-memory targets ([`DemoRuntime`]). Its pages, by URL ([`Page`]):
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
//!
//! Both `demo:page` and `demo://page` name a page; the address bar keeps only the second form as
//! typed (it reads `demo:pattern` as a search, as it does any text without `://`), and routing
//! needs `[routing.demo] schemes = ["demo"]` in the configuration.
//!
//! Every page shows a square in its top-left corner, green while the page has the keyboard, and
//! typing into a page appends the text to its title and shows it in the status line, so a
//! screenshot shows where keys went.
//!
//! The engine has no history of its own (`history_traversal: false`, so the core gives it a view
//! per page) and no page zoom. Everything happens on the owner thread, synchronously, so it
//! needs no waker; the slow page's deadline comes back from `pump`.
//!
//! Must not own: anything a real engine would not; it goes through the same contract.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod paint;

use std::collections::{BTreeMap, HashSet};
use std::mem::{discriminant, Discriminant};
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, PresentOutcome, PumpStatus,
    ViewOptions,
};
use sighurt_api::events::{Cursor, EngineEvent, NavigationError};
use sighurt_api::geometry::{Metrics, SurfacePoint};
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, ViewId};
use sighurt_api::input::{ButtonState, InputKind, Key, PointerEvent};
use sighurt_wayland::{FramePermit, TargetLease};

/// The engine's name in the configuration and the registry.
pub const NAME: &str = "demo";

/// What the demo engine supports: none of the optional operations.
pub const CAPABILITIES: EngineCapabilities = EngineCapabilities {
    history_traversal: false,
    text_input: false,
    accessibility: false,
    screenshots: false,
    find_in_page: false,
    page_zoom: false,
    pointer_lock: false,
    native_popups: false,
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
        }
    }
}

/// One view: its target and what its page shows.
struct View {
    target: TargetLease,
    metrics: Metrics,
    visible: bool,
    focused: bool,
    /// The committed page and its URL.
    page: Option<(Page, String)>,
    /// The view's document failed (`demo:crash`); reloading recovers it.
    failed: bool,
    typed: String,
    pointer: Option<SurfacePoint>,
    /// The animation's clock.
    started: Instant,
    /// `demo:slow`: the layout whose frame is ready, and when the next one will be.
    rendered: Option<LayoutEpoch>,
    ready_at: Option<(LayoutEpoch, Instant)>,
    /// Something new to paint, reported as `FrameReady` at the next pump.
    dirty: bool,
}

impl View {
    fn new(target: TargetLease, options: ViewOptions) -> Self {
        Self {
            metrics: target.metrics(),
            target,
            visible: options.visible,
            focused: false,
            page: None,
            failed: false,
            typed: String::new(),
            pointer: None,
            started: Instant::now(),
            rendered: None,
            ready_at: None,
            dirty: false,
        }
    }

    fn page(&self) -> Option<Page> {
        self.page.as_ref().map(|(page, _)| *page)
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
#[derive(Default)]
pub struct DemoRuntime {
    views: BTreeMap<ViewId, View>,
    /// Events not handed over yet, in order.
    queue: Vec<EngineEvent>,
    shut_down: bool,
}

impl DemoRuntime {
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
                view.page = Some((page, url.clone()));
                view.failed = false;
                view.typed.clear();
                view.pointer = None;
                view.started = Instant::now();
                view.dirty = true;
                view.start_slow_frame(Instant::now());
                queue.push(EngineEvent::NavigationCommitted {
                    view: id,
                    navigation,
                    url,
                    document: DocumentGeneration::next(),
                    history: None,
                });
                queue.push(EngineEvent::TitleChanged {
                    view: id,
                    title: view.title(),
                });
                let cursor = match page {
                    Page::Resize => Cursor::Crosshair,
                    Page::Crash => Cursor::Pointer,
                    _ => Cursor::Default,
                };
                queue.push(EngineEvent::CursorChanged { view: id, cursor });
                queue.push(EngineEvent::StatusTextChanged {
                    view: id,
                    text: None,
                });
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

    fn input(&mut self, id: ViewId, kind: InputKind) {
        let view = self.views.get_mut(&id).expect("checked by the caller");
        let queue = &mut self.queue;
        let mut activated = false;
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

impl EngineRuntime for DemoRuntime {
    type Target = TargetLease;
    type FramePermit = FramePermit;

    fn capabilities(&self) -> EngineCapabilities {
        CAPABILITIES
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: ViewOptions,
        target: TargetLease,
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
            // The demo engine asks for no services, so there is nothing to resolve.
            EngineCommand::ResolveServiceRequest { .. } => {}
        }
        Ok(())
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        if self.shut_down {
            return PumpStatus::Idle;
        }
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

    fn present(&mut self, id: ViewId, permit: FramePermit) -> Result<PresentOutcome, EngineError> {
        self.running()?;
        let view = self
            .views
            .get_mut(&id)
            .ok_or(EngineError::UnknownView(id))?;
        if permit.target() != view.target.id() {
            return Err(EngineError::View {
                view: id,
                message: format!("a permit for {:?} is not this view's", permit.target()),
            });
        }
        let Some(page) = view.page() else {
            return Ok(PresentOutcome::NotReady);
        };
        let epoch = permit.metrics().epoch();
        if page == Page::Slow && view.rendered != Some(epoch) {
            return Ok(PresentOutcome::NotReady);
        }
        let mut frame = permit.shm_frame().map_err(|error| EngineError::View {
            view: id,
            message: error.to_string(),
        })?;
        let scene = paint::Scene {
            page,
            failed: view.failed,
            focused: view.focused,
            pointer: view.pointer,
            elapsed: view.started.elapsed(),
        };
        paint::paint(&mut frame, &scene);
        frame.submit();
        // An animation asks for its next frame as soon as this one is submitted; the host's
        // frame callback paces it.
        view.dirty |= page == Page::Animation && !view.failed;
        Ok(PresentOutcome::Submitted)
    }

    fn close_view(&mut self, id: ViewId) {
        // Dropping the view retires its target lease.
        if self.views.remove(&id).is_some() {
            self.queue.retain(|event| event.view() != Some(id));
            self.queue.push(EngineEvent::ViewClosed { view: id });
        }
    }

    fn shutdown(&mut self) {
        self.views.clear();
        self.queue.clear();
        self.shut_down = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_name_pages() {
        assert_eq!(Page::from_url("demo:pattern"), Ok(Page::Pattern));
        assert_eq!(Page::from_url("demo://animation"), Ok(Page::Animation));
        assert_eq!(Page::from_url("DEMO:Slow?x#y"), Ok(Page::Slow));
        assert_eq!(Page::from_url("demo:"), Ok(Page::Pattern));
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
    fn typed_text_is_bounded() {
        let mut text = "åäö".repeat(30);
        keep_last_chars(&mut text, MAX_TYPED);
        assert_eq!(text.chars().count(), MAX_TYPED);
        assert!(text.ends_with("åäö"));
    }
}
