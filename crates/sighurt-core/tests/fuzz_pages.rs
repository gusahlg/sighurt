//! Seeded random-input tests of the core with engines answering, in random orders: find in page
//! and fullscreen (`sighurt_core::fullscreen`) under tab commands, documents committing, views
//! failing and closing, the window's reports, its keyboard focus, clicks, Escape, and Enter bound
//! to `page.send-key enter` (its repeats run it again). A fake
//! engine answers the core's effects, each answer either at once or held back and delivered
//! later in a random order, and pages say on their own that they entered or left fullscreen,
//! for current documents and stale ones, for views alive and gone.
//!
//! After every event: a page is believed fullscreen only for the view the active tab shows,
//! with its current document, while the window has the keyboard; nothing is sent to a view
//! after the core closed it (`ExitFullscreen` and finds included); `ExitFullscreen` goes only to
//! an engine that has fullscreen; and the text of a search that reaches an engine is the trimmed
//! text the user gave, non-empty, at most `MAX_FIND` bytes, with no control character in it.
//! A key's repeats and release reach only a page its press reached (the chrome, a tab that came
//! to the front, or another view shown since get none of them).
//! `tools/fuzz/mod.rs` describes the environment variables.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fuzz::{iterations, run, Rng};
use sighurt_api::commands::{Command, MAX_FIND};
use sighurt_api::engine::{EngineCapabilities, EngineCommand, FindCommand};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::SurfacePoint;
use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, SeatId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, Key, KeyCode, KeyEvent, Modifiers,
    PointerButton, PointerEvent,
};
use sighurt_core::config::Config;
use sighurt_core::tabs::Content;
use sighurt_core::{update, BrowserEvent, BrowserState, Effect, Environment, OpenRequest};

const LIMIT: Duration = Duration::from_secs(5);

/// How often the cases reached what they check: a page fullscreen, an `ExitFullscreen`, a
/// search sent, a view closed. All must happen for the test to mean anything.
static REACHED: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];

/// Can find in page and go fullscreen.
const FULL: &str = "full";
/// Can neither.
const PLAIN: &str = "plain";

fn environment() -> Environment {
    let full = EngineCapabilities {
        history_traversal: true,
        fullscreen: true,
        find_in_page: true,
        ..EngineCapabilities::default()
    };
    Environment {
        engines: [
            (FULL.into(), full),
            (PLAIN.into(), EngineCapabilities::default()),
        ]
        .into(),
        ..Environment::default()
    }
}

struct Harness {
    state: BrowserState,
    /// Each open view's engine and its current document (the last the core was told of).
    views: BTreeMap<ViewId, (String, Option<DocumentGeneration>)>,
    /// Every view the core has asked to close.
    closed: BTreeSet<ViewId>,
    /// Every view ever opened, for events about views alive and gone.
    known: Vec<ViewId>,
    /// Every document ever committed, for stale events.
    documents: Vec<DocumentGeneration>,
    /// The searches the core started, per view.
    searches: BTreeMap<ViewId, Vec<RequestId>>,
    /// Answers held back, delivered later in a random order.
    held: Vec<BrowserEvent>,
    /// Whether the window has the keyboard.
    focused: bool,
    /// The physical key being delivered, while it is.
    key: Option<KeyEvent>,
    /// The views each key's press sent input to, from its press to its release.
    pressed: BTreeMap<u32, BTreeSet<ViewId>>,
    seat: SeatId,
    now: Instant,
    rng: Rng,
}

impl Harness {
    fn new(rng: Rng) -> Self {
        let env = environment();
        let text = format!(
            "version = 1\n[engines]\ndefault = \"{FULL}\"\n[input]\nkeymap = \"bindings\"\n\
             [keys]\n\"enter\" = \"page.send-key enter\"\n"
        );
        let config = Config::parse(Some(&text), &env).unwrap();
        Self {
            state: BrowserState::new(config, env),
            views: BTreeMap::new(),
            closed: BTreeSet::new(),
            known: Vec::new(),
            documents: Vec::new(),
            searches: BTreeMap::new(),
            held: Vec::new(),
            focused: true,
            key: None,
            pressed: BTreeMap::new(),
            seat: SeatId::next(),
            now: Instant::now(),
            rng,
        }
    }

    fn check_effect(&mut self, effect: &Effect) {
        match effect {
            Effect::Engine { view, command } => {
                assert!(
                    !self.closed.contains(view),
                    "{command:?} sent to {view:?} after it was closed"
                );
                if let (
                    EngineCommand::Input(InputEvent {
                        kind: InputKind::Key(_),
                        ..
                    }),
                    Some(key),
                ) = (command, &self.key)
                {
                    // A key whose press came before the window had the keyboard is not checked.
                    let views = self.pressed.get_mut(&key.code.0);
                    match views {
                        Some(views) if key.state == ButtonState::Pressed && !key.repeat => {
                            views.insert(*view);
                        }
                        Some(views) => assert!(
                            views.contains(view),
                            "{key:?} reached {view:?}, which its press did not"
                        ),
                        None => {}
                    }
                }
                if matches!(command, EngineCommand::ExitFullscreen) {
                    REACHED[1].fetch_add(1, Ordering::Relaxed);
                    let engine = self.views.get(view).map(|(e, _)| e.as_str());
                    assert_eq!(engine, Some(FULL), "ExitFullscreen to {engine:?}");
                }
            }
            Effect::Find { view, command } => {
                assert!(
                    !self.closed.contains(view),
                    "{command:?} for {view:?} after it was closed"
                );
                if let FindCommand::Search { search, text, .. } = command {
                    assert!(!text.is_empty(), "an empty search");
                    assert_eq!(text.trim(), text, "untrimmed");
                    assert!(text.len() <= MAX_FIND, "{} bytes", text.len());
                    assert!(
                        !text.chars().any(char::is_control),
                        "a control character reaches the engine: {text:?}"
                    );
                    self.searches.entry(*view).or_default().push(*search);
                    REACHED[2].fetch_add(1, Ordering::Relaxed);
                }
            }
            Effect::CloseView { view } => {
                self.closed.insert(*view);
                REACHED[3].fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// The fake engine's answers to `effect`.
    fn answer(&mut self, effect: &Effect) -> Vec<BrowserEvent> {
        let (view, events) = match effect {
            Effect::OpenView { view, engine, .. } => {
                self.views.insert(*view, (engine.clone(), None));
                self.known.push(*view);
                (*view, vec![EngineEvent::ViewReady { view: *view }])
            }
            Effect::CloseView { view } => (*view, vec![EngineEvent::ViewClosed { view: *view }]),
            Effect::Engine {
                view,
                command: EngineCommand::Navigate { navigation, url },
            } => (*view, vec![self.commit(*view, *navigation, url.clone())]),
            // What the runtime answers when nobody can search the view.
            Effect::Find {
                view,
                command: FindCommand::Search { search, .. },
            } if self.views.get(view).is_some_and(|(e, _)| e == PLAIN) => {
                return vec![BrowserEvent::FindResult {
                    view: *view,
                    search: *search,
                    found: false,
                    current: None,
                    total: None,
                    supported: false,
                }];
            }
            Effect::Find {
                view,
                command:
                    FindCommand::Search {
                        search, document, ..
                    },
            } if self.rng.one_in(2) => {
                let found = self.rng.one_in(2);
                let event = EngineEvent::FindResult {
                    view: *view,
                    document: *document,
                    search: *search,
                    found,
                    current: found.then_some(1),
                    total: found.then_some(3),
                };
                (*view, vec![event])
            }
            _ => return Vec::new(),
        };
        let Some((engine, _)) = self.views.get(&view) else {
            return Vec::new();
        };
        let engine = engine.clone();
        events
            .into_iter()
            .map(|event| BrowserEvent::Engine {
                engine: engine.clone(),
                event,
                at: self.now,
            })
            .collect()
    }

    /// A commit of a new document in `view`. The harness's record of the view's document is
    /// updated when the core is told, in [`Self::told`].
    fn commit(&mut self, view: ViewId, navigation: NavigationId, url: String) -> EngineEvent {
        let document = DocumentGeneration::next();
        self.documents.push(document);
        EngineEvent::NavigationCommitted {
            view,
            navigation,
            url,
            document,
            history: None,
        }
    }

    /// Records what an engine event about to be delivered tells the core.
    fn told(&mut self, event: &BrowserEvent) {
        if let BrowserEvent::Engine { event, .. } = event {
            match event {
                EngineEvent::NavigationCommitted { view, document, .. } => {
                    if let Some(entry) = self.views.get_mut(view) {
                        entry.1 = Some(*document);
                    }
                }
                EngineEvent::ViewClosed { view } => {
                    self.views.remove(view);
                    self.closed.insert(*view);
                }
                _ => {}
            }
        }
    }

    /// Applies `event`, answering the effects it makes (some answers held back), until nothing
    /// more happens, checking every effect and the state after each step, and recording what
    /// each engine event tells the core as it is delivered.
    fn deliver(&mut self, event: BrowserEvent) {
        let mut queue = VecDeque::from([event]);
        let mut steps = 0;
        while let Some(event) = queue.pop_front() {
            self.told(&event);
            let effects = update(&mut self.state, event);
            for effect in &effects {
                self.check_effect(effect);
                for answer in self.answer(effect) {
                    match self.rng.one_in(3) {
                        true => self.held.push(answer),
                        false => queue.push_back(answer),
                    }
                }
            }
            self.check_state();
            steps += 1;
            assert!(steps < 5_000, "the core and the engine never settle");
        }
    }

    /// The view the active tab shows, if it shows one.
    fn shown(&self) -> Option<ViewId> {
        let tab = self.state.tabs().nth(self.state.active())?;
        match tab.content {
            Content::View(view) => Some(view),
            _ => None,
        }
    }

    fn check_state(&self) {
        let fullscreen = self.state.fullscreen();
        let Some((view, document)) = fullscreen.page else {
            return;
        };
        REACHED[0].fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            Some(view),
            self.shown(),
            "a page believed fullscreen in a view not shown"
        );
        assert!(!self.closed.contains(&view), "fullscreen in a closed view");
        let current = self.state.view(view).and_then(|v| v.document());
        assert_eq!(current, Some(document), "fullscreen of another document");
        assert!(
            self.focused,
            "fullscreen while the window lost the keyboard"
        );
        assert_eq!(
            self.views.get(&view).map(|(e, _)| e.as_str()),
            Some(FULL),
            "fullscreen in an engine that has none"
        );
    }

    fn some_view(&mut self) -> Option<ViewId> {
        match self.rng.below(3) {
            0 if !self.known.is_empty() => Some(*self.rng.pick(&self.known)),
            _ => self.shown(),
        }
    }

    fn some_document(&mut self, view: ViewId) -> DocumentGeneration {
        let current = self.views.get(&view).and_then(|(_, d)| *d);
        match (current, self.rng.below(3)) {
            (Some(document), 0 | 1) => document,
            _ if !self.documents.is_empty() => *self.rng.pick(&self.documents),
            _ => DocumentGeneration::next(),
        }
    }

    fn engine_event(&mut self, event: EngineEvent) {
        let view = event.view().unwrap();
        let engine = self
            .views
            .get(&view)
            .map_or(FULL.to_owned(), |(e, _)| e.clone());
        self.deliver(BrowserEvent::Engine {
            engine,
            event,
            at: self.now,
        });
    }

    /// Delivers `key`, keeping track of the views its press reached.
    fn key(&mut self, key: KeyEvent) {
        let fresh = key.state == ButtonState::Pressed && !key.repeat;
        if fresh {
            self.pressed.insert(key.code.0, BTreeSet::new());
        }
        let event = self.input(InputKind::Key(key.clone()));
        let (code, released) = (key.code.0, key.state == ButtonState::Released);
        self.key = Some(key);
        self.deliver(BrowserEvent::Keyboard {
            event,
            at: self.now,
        });
        self.key = None;
        if released {
            self.pressed.remove(&code);
        }
    }

    fn input(&mut self, kind: InputKind) -> InputEvent {
        InputEvent {
            seat: self.seat,
            kind,
        }
    }
}

/// Text for `page.find`: words, whitespace of every kind, control characters, long runs.
fn find_text(rng: &mut Rng) -> String {
    let mut text = match rng.below(6) {
        0 => rng.string(20),
        1 => rng.string_from(
            &["a", "B", " ", "\t", "\n", "\r", "\u{1b}", "\0", "\u{85}"],
            12,
        ),
        2 => "x".repeat(rng.range(MAX_FIND - 3, MAX_FIND + 3)),
        3 => format!("{}é", "a".repeat(MAX_FIND - 1)),
        4 => rng.string_from(
            &["\u{7f}", "\u{9b}", "\u{200b}", "\u{2028}", " ", "word"],
            10,
        ),
        _ => rng
            .pick(&["hello", "Hello", "  pad  ", "a\u{0}b", "\u{1b}[31m"])
            .to_string(),
    };
    if rng.one_in(4) {
        text = rng.mutate(&text);
    }
    text
}

fn command(rng: &mut Rng) -> Command {
    let n = rng.range(1, 4);
    let text = match rng.below(24) {
        0 => format!("tab.new https://a.test/{}", rng.below(5)),
        1 => "tab.new".into(),
        2 => "tab.close".into(),
        3 => format!("tab.close {n}"),
        4 => format!("tab.select {n}"),
        5 => "tab.next".into(),
        6 => "tab.previous".into(),
        7 => "tab.duplicate".into(),
        8 => "tab.move left".into(),
        9 => "tab.reopen".into(),
        10 => format!("page.open https://b.test/{}", rng.below(5)),
        11 => format!("page.open-in {PLAIN} https://p.test/"),
        12 => "page.reload".into(),
        13 => "page.back".into(),
        14 => "page.switch-engine".into(),
        15 | 16 => {
            // A chrome can hand the core text the parser would refuse: build it directly.
            return Command::PageFind(Some(find_text(rng)));
        }
        17 => "page.find".into(),
        18 => "page.find-next".into(),
        19 => "page.find-previous".into(),
        20 => "page.find-stop".into(),
        21 => "browser.fullscreen".into(),
        22 => "page.exit-fullscreen".into(),
        _ => "browser.home".into(),
    };
    text.parse().unwrap()
}

fn escape(rng: &mut Rng) -> KeyEvent {
    KeyEvent {
        code: KeyCode(1),
        key: Key::Named("Escape".into()),
        state: *rng.pick(&[ButtonState::Pressed, ButtonState::Released]),
        repeat: rng.one_in(4),
        modifiers: Modifiers {
            shift: rng.one_in(5),
            ..Modifiers::default()
        },
        consumed: Modifiers::default(),
        text: None,
    }
}

#[test]
fn fuzz_find_and_fullscreen_in_random_orders() {
    run("fuzz_pages", iterations(100) / 4, LIMIT, |case| {
        let mut h = Harness::new(case.rng.clone());
        let open = (0..case.rng.range(1, 3))
            .map(|i| OpenRequest {
                input: format!("https://s.test/{i}"),
                engine: None,
            })
            .collect();
        h.deliver(BrowserEvent::Start {
            session: None,
            open,
        });
        for _ in 0..case.rng.range(5, 80) {
            let rng = &mut case.rng;
            match rng.below(16) {
                0..=4 => {
                    let command = command(rng);
                    case.note(|| format!("{command:?}"));
                    h.deliver(BrowserEvent::Command(command));
                }
                5 | 6 if !h.held.is_empty() => {
                    let at = rng.below(h.held.len());
                    let event = h.held.swap_remove(at);
                    case.note(|| format!("held {event:?}"));
                    h.deliver(event);
                }
                7 | 8 => {
                    // A click in the page shown, or in another view: the user's activation.
                    let Some(view) = h.some_view() else { continue };
                    let event = h.input(InputKind::Pointer(PointerEvent::Button {
                        button: PointerButton::Left,
                        state: ButtonState::Pressed,
                        position: SurfacePoint { x: 5.0, y: 5.0 },
                        modifiers: Modifiers::default(),
                    }));
                    case.note(|| format!("click in {view:?}"));
                    h.deliver(BrowserEvent::Pointer {
                        view,
                        event,
                        at: h.now,
                    });
                }
                9 | 10 => {
                    let Some(view) = h.some_view() else { continue };
                    let document = h.some_document(view);
                    let fullscreen = !rng.one_in(3);
                    case.note(|| format!("{view:?} {document:?} fullscreen {fullscreen}"));
                    h.engine_event(EngineEvent::FullscreenChanged {
                        view,
                        document,
                        fullscreen,
                    });
                }
                11 => {
                    let on = rng.one_in(2);
                    case.note(|| format!("the window is fullscreen: {on}"));
                    h.deliver(BrowserEvent::Fullscreen(on));
                }
                12 => {
                    h.focused = rng.one_in(2);
                    let focus = match h.focused {
                        true => FocusEvent::Activated,
                        false => FocusEvent::Deactivated,
                    };
                    case.note(|| format!("{focus:?}"));
                    // No release comes for a key held while the window lost the keyboard.
                    if !h.focused {
                        h.pressed.clear();
                    }
                    let event = h.input(InputKind::Focus(focus));
                    h.deliver(BrowserEvent::Keyboard { event, at: h.now });
                }
                13 => {
                    let mut key = escape(rng);
                    if rng.one_in(2) {
                        key.code = KeyCode(28);
                        key.key = Key::Named("Enter".into());
                    }
                    case.note(|| format!("{key:?}"));
                    h.key(key);
                }
                14 => {
                    let Some(view) = h.some_view() else { continue };
                    let event = match rng.below(3) {
                        0 => EngineEvent::ViewFailed {
                            view,
                            message: "gone".into(),
                        },
                        1 => EngineEvent::ViewClosed { view },
                        _ => {
                            let navigation = NavigationId::next();
                            h.commit(view, navigation, "https://own.test/".into())
                        }
                    };
                    case.note(|| format!("{event:?}"));
                    h.engine_event(event);
                }
                _ => {
                    // A late or stale result for one of the view's searches.
                    let Some(view) = h.some_view() else { continue };
                    let Some(search) = h.searches.get(&view).map(|s| *rng.pick(s)) else {
                        continue;
                    };
                    let document = h.some_document(view);
                    case.note(|| format!("find result {view:?} {search:?}"));
                    h.engine_event(EngineEvent::FindResult {
                        view,
                        document,
                        search,
                        found: true,
                        current: Some(2),
                        total: Some(2),
                    });
                }
            }
        }
        // Everything still held arrives, in a random order.
        while !h.held.is_empty() {
            let at = case.rng.below(h.held.len());
            let event = h.held.swap_remove(at);
            h.deliver(event);
        }
    });
    if std::env::var_os("SIG_FUZZ_CASE").is_none() {
        let reached = REACHED.each_ref().map(|n| n.load(Ordering::Relaxed));
        eprintln!("fuzz_pages reached: fullscreen, exits, searches, closes = {reached:?}");
        assert!(reached.iter().all(|&n| n > 0), "{reached:?}");
    }
}

/// What the fuzz test found: a NUL or a newline in pasted find text reached the engine (WPE
/// WebKit then refused the search as unsupported, and the view was taken for one that can't
/// find in page). Control characters are spaces now.
#[test]
fn a_find_with_control_characters_searches_with_spaces() {
    let mut h = Harness::new(Rng::new(1));
    h.deliver(BrowserEvent::Start {
        session: None,
        open: vec![OpenRequest {
            input: "https://s.test/".into(),
            engine: None,
        }],
    });
    while let Some(event) = h.held.pop() {
        h.deliver(event);
    }
    let view = h.shown().unwrap();
    h.deliver(BrowserEvent::Command(Command::PageFind(Some(
        "\u{1b}a\0b\ncd\t".into(),
    ))));
    let find = h.state.view(view).and_then(|v| v.find()).unwrap();
    assert_eq!(find.text, "a b cd");
    h.deliver(BrowserEvent::Command(Command::PageFind(Some(
        "\n\0\r".into(),
    ))));
    assert!(h.state.view(view).and_then(|v| v.find()).is_none());
}
