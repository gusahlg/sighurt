//! Fullscreen, the window's and a page's (`sighurt_core::fullscreen`): the core driven as the
//! executable drives it, with a fake engine whose views commit at once.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCapabilities, EngineCommand};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::SurfacePoint;
use sighurt_api::ids::{DocumentGeneration, SeatId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, Key, KeyCode, KeyEvent, Modifiers,
    PointerButton, PointerEvent,
};
use sighurt_api::keymap::{Browser, KeyAction, KeyContext, Keymap, KeymapKind};
use sighurt_core::config::Config;
use sighurt_core::fullscreen::FullscreenState;
use sighurt_core::tabs::Content;
use sighurt_core::{
    update, BrowserEvent, BrowserState, Effect, Environment, Frontend, OpenRequest,
};

/// A keymap that runs `page.exit-fullscreen` on Escape while the tab shown says it is
/// fullscreen, as the example configuration does, and records what it was shown.
const WATCHING: KeymapKind = KeymapKind {
    name: "watching",
    check: |_, _| Vec::new(),
    new: || Box::new(Watching),
};

struct Watching;

impl Keymap for Watching {
    fn configure(&mut self, _: &sighurt_api::settings::Table) {}

    fn press(&mut self, key: &KeyEvent, _: KeyContext, browser: &dyn Browser) -> KeyAction {
        let fullscreen = browser
            .tab(browser.active_tab())
            .is_some_and(|tab| tab.fullscreen);
        match &key.key {
            Key::Named(name) if name == "Escape" && fullscreen => {
                KeyAction::Run("page.exit-fullscreen".parse().unwrap())
            }
            Key::Named(name) if name == "Escape" => KeyAction::Take,
            _ => KeyAction::Pass,
        }
    }
}

struct Harness {
    state: BrowserState,
    now: Instant,
    /// Each view's engine and current document.
    views: BTreeMap<ViewId, (String, DocumentGeneration)>,
    seat: SeatId,
}

fn environment(frontend: Frontend) -> Environment {
    let servo = EngineCapabilities {
        history_traversal: true,
        fullscreen: true,
        ..EngineCapabilities::default()
    };
    Environment {
        engines: [
            ("servo".into(), servo),
            ("plain".into(), EngineCapabilities::default()),
        ]
        .into(),
        keymaps: vec![WATCHING],
        frontend,
        ..Environment::default()
    }
}

impl Harness {
    /// A browser in `frontend` with the `bindings` keymap and `keys` as its `[keys]`, showing
    /// a tab for each of `urls`, the first selected.
    fn start(frontend: Frontend, keymap: &str, urls: &[&str]) -> Self {
        let env = environment(frontend);
        let text = format!("version = 1\n[input]\nkeymap = \"{keymap}\"\n");
        let config = Config::parse(Some(&text), &env).unwrap();
        let mut h = Self {
            state: BrowserState::new(config, env),
            now: Instant::now(),
            views: BTreeMap::new(),
            seat: SeatId::next(),
        };
        h.apply(BrowserEvent::Start {
            session: None,
            open: urls
                .iter()
                .map(|url| OpenRequest {
                    input: (*url).into(),
                    engine: None,
                })
                .collect(),
        });
        h.apply(BrowserEvent::Command("tab.select 1".parse().unwrap()));
        h
    }

    fn window(urls: &[&str]) -> Self {
        Self::start(Frontend::Gui, "bindings", urls)
    }

    /// Applies `event`, then lets the fake engine answer opening views and navigations, until
    /// nothing more happens. Returns every effect.
    fn apply(&mut self, event: BrowserEvent) -> Vec<Effect> {
        let mut all = Vec::new();
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            let effects = update(&mut self.state, event);
            for effect in &effects {
                queue.extend(self.answer(effect));
            }
            all.extend(effects);
            assert!(all.len() < 10_000, "the core and the engine never settle");
        }
        all
    }

    fn answer(&mut self, effect: &Effect) -> Vec<BrowserEvent> {
        let (view, events) = match effect {
            Effect::OpenView { view, engine, .. } => {
                let document = DocumentGeneration::next();
                self.views.insert(*view, (engine.clone(), document));
                (*view, vec![EngineEvent::ViewReady { view: *view }])
            }
            Effect::CloseView { view } => (*view, vec![EngineEvent::ViewClosed { view: *view }]),
            Effect::Engine {
                view,
                command: EngineCommand::Navigate { navigation, url },
            } => {
                let document = DocumentGeneration::next();
                self.views.get_mut(view).unwrap().1 = document;
                let commit = EngineEvent::NavigationCommitted {
                    view: *view,
                    navigation: *navigation,
                    url: url.clone(),
                    document,
                    history: None,
                };
                (*view, vec![commit])
            }
            _ => return Vec::new(),
        };
        let engine = self.views[&view].0.clone();
        events
            .into_iter()
            .map(|event| BrowserEvent::Engine {
                engine: engine.clone(),
                event,
                at: self.now,
            })
            .collect()
    }

    /// The view the active tab shows.
    fn shown(&self) -> ViewId {
        let tab = self.state.tabs().nth(self.state.active()).unwrap();
        match tab.content {
            Content::View(view) => view,
            other => panic!("the active tab shows {other:?}"),
        }
    }

    fn document(&self, view: ViewId) -> DocumentGeneration {
        self.views[&view].1
    }

    fn engine(&mut self, event: EngineEvent) -> Vec<Effect> {
        let engine = self.views[&event.view().unwrap()].0.clone();
        self.apply(BrowserEvent::Engine {
            engine,
            event,
            at: self.now,
        })
    }

    /// The shown view's engine says its document's element is fullscreen, or no longer.
    fn page_says(&mut self, fullscreen: bool) -> Vec<Effect> {
        let view = self.shown();
        let document = self.document(view);
        self.engine(EngineEvent::FullscreenChanged {
            view,
            document,
            fullscreen,
        })
    }

    /// A click in the shown page: the user's activation.
    fn click(&mut self) -> Vec<Effect> {
        let view = self.shown();
        let event = InputEvent {
            seat: self.seat,
            kind: InputKind::Pointer(PointerEvent::Button {
                button: PointerButton::Left,
                state: ButtonState::Pressed,
                position: SurfacePoint { x: 5.0, y: 5.0 },
                modifiers: Modifiers::default(),
            }),
        };
        self.apply(BrowserEvent::Pointer {
            view,
            event,
            at: self.now,
        })
    }

    /// The user clicks in the page, which then asks for fullscreen; accepted.
    fn enter_page_fullscreen(&mut self) -> Vec<Effect> {
        self.click();
        let effects = self.page_says(true);
        assert!(self.fullscreen().page.is_some(), "{effects:?}");
        effects
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let event = InputEvent {
            seat: self.seat,
            kind: InputKind::Key(key),
        };
        self.apply(BrowserEvent::Keyboard {
            event,
            at: self.now,
        })
    }

    fn window_focus(&mut self, focus: FocusEvent) -> Vec<Effect> {
        let event = InputEvent {
            seat: self.seat,
            kind: InputKind::Focus(focus),
        };
        self.apply(BrowserEvent::Keyboard {
            event,
            at: self.now,
        })
    }

    fn command(&mut self, text: &str) -> Vec<Effect> {
        self.apply(BrowserEvent::Command(text.parse().unwrap()))
    }

    fn fullscreen(&self) -> FullscreenState {
        self.state.fullscreen()
    }
}

/// The window states `effects` ask for.
fn requests(effects: &[Effect]) -> Vec<bool> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SetFullscreen(on) => Some(*on),
            _ => None,
        })
        .collect()
}

/// Whether `effects` tell `view` to leave element fullscreen.
fn exits(effects: &[Effect], view: ViewId) -> usize {
    effects
        .iter()
        .filter(|effect| {
            matches!(effect, Effect::Engine { view: to, command: EngineCommand::ExitFullscreen }
                if *to == view)
        })
        .count()
}

fn notices(effects: &[Effect]) -> Vec<&str> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Notice { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

fn escape(modifiers: Modifiers) -> KeyEvent {
    KeyEvent {
        code: KeyCode(1),
        key: Key::Named("Escape".into()),
        state: ButtonState::Pressed,
        repeat: false,
        modifiers,
        consumed: Modifiers::default(),
        text: None,
    }
}

fn with_state(key: &KeyEvent, state: ButtonState, repeat: bool) -> KeyEvent {
    KeyEvent {
        state,
        repeat,
        ..key.clone()
    }
}

/// Whether `effects` hand `key`'s state to `view`'s page.
fn delivered(effects: &[Effect], view: ViewId) -> bool {
    effects.iter().any(|effect| {
        matches!(effect, Effect::Engine { view: to, command: EngineCommand::Input(InputEvent {
            kind: InputKind::Key(_), .. }) } if *to == view)
    })
}

// ── The window ────────────────────────────────────────────────────────────────────────────

#[test]
fn the_toggle_asks_and_the_window_answers() {
    let mut h = Harness::window(&["https://a.test/"]);
    assert_eq!(requests(&h.command("browser.fullscreen")), [true]);
    // Nothing is assumed until the window says.
    assert!(h.fullscreen().wanted && !h.fullscreen().window);
    let effects = h.apply(BrowserEvent::Fullscreen(true));
    assert!(requests(&effects).is_empty());
    assert!(effects.contains(&Effect::RedrawChrome));
    assert!(h.fullscreen().window);
    assert_eq!(requests(&h.command("browser.fullscreen")), [false]);
    h.apply(BrowserEvent::Fullscreen(false));
    assert_eq!(h.fullscreen(), FullscreenState::default());
    // An unchanged report changes nothing.
    assert!(h.apply(BrowserEvent::Fullscreen(false)).is_empty());
}

#[test]
fn two_toggles_before_an_answer_do_not_both_ask_for_fullscreen() {
    let mut h = Harness::window(&["https://a.test/"]);
    assert_eq!(requests(&h.command("browser.fullscreen")), [true]);
    assert_eq!(requests(&h.command("browser.fullscreen")), [false]);
    // The answer to the first, then to the second: the first is a superseded request's.
    assert!(requests(&h.apply(BrowserEvent::Fullscreen(true))).is_empty());
    assert!(!h.fullscreen().wanted);
    assert!(requests(&h.apply(BrowserEvent::Fullscreen(false))).is_empty());
    assert_eq!(h.fullscreen(), FullscreenState::default());
}

#[test]
fn the_compositor_ending_fullscreen_resets_what_the_user_wants() {
    let mut h = Harness::window(&["https://a.test/"]);
    h.command("browser.fullscreen");
    h.apply(BrowserEvent::Fullscreen(true));
    // The compositor's own exit: nothing asks for fullscreen again.
    let effects = h.apply(BrowserEvent::Fullscreen(false));
    assert!(requests(&effects).is_empty(), "{effects:?}");
    assert!(!h.fullscreen().wanted);
    // The next toggle asks for fullscreen, not for none.
    assert_eq!(requests(&h.command("browser.fullscreen")), [true]);
    h.apply(BrowserEvent::Fullscreen(true));
    // The compositor making it fullscreen by itself is taken as wanted.
    h.command("browser.fullscreen");
    h.apply(BrowserEvent::Fullscreen(false));
    assert!(requests(&h.apply(BrowserEvent::Fullscreen(true))).is_empty());
    assert!(h.fullscreen().wanted);
}

#[test]
fn the_terminal_has_no_window_fullscreen() {
    let mut h = Harness::start(Frontend::Terminal, "bindings", &["https://a.test/"]);
    let effects = h.command("browser.fullscreen");
    assert_eq!(
        notices(&effects),
        ["browser.fullscreen is not available in the terminal."]
    );
    assert!(requests(&effects).is_empty());
    assert_eq!(h.fullscreen(), FullscreenState::default());
}

// ── A page's fullscreen ───────────────────────────────────────────────────────────────────

#[test]
fn a_page_enters_fullscreen_after_a_click_with_a_notice() {
    let mut h = Harness::window(&["https://a.test/video"]);
    let view = h.shown();
    h.click();
    let effects = h.page_says(true);
    assert_eq!(requests(&effects), [true]);
    assert_eq!(
        notices(&effects),
        ["a.test is fullscreen. Escape leaves it."]
    );
    assert_eq!(exits(&effects, view), 0);
    assert_eq!(h.fullscreen().page, Some((view, h.document(view))));
    // The keymap sees the tab as fullscreen.
    let effects = h.key(escape(Modifiers::default()));
    assert_eq!(exits(&effects, view), 1);
    assert_eq!(requests(&effects), [false]);
    assert_eq!(h.fullscreen().page, None);
    // The engine confirming changes nothing more.
    let effects = h.page_says(false);
    assert!(requests(&effects).is_empty() && exits(&effects, view) == 0);
}

#[test]
fn a_page_without_the_users_activation_is_refused() {
    let mut h = Harness::window(&["https://a.test/"]);
    let view = h.shown();
    let effects = h.page_says(true);
    assert_eq!(exits(&effects, view), 1);
    assert!(requests(&effects).is_empty() && notices(&effects).is_empty());
    assert_eq!(h.fullscreen().page, None);
    // An activation too old is none.
    h.click();
    h.now += Duration::from_secs(6);
    let effects = h.page_says(true);
    assert_eq!(exits(&effects, view), 1);
    assert_eq!(h.fullscreen().page, None);
}

#[test]
fn one_activation_enters_once() {
    let mut h = Harness::window(&["https://a.test/"]);
    let view = h.shown();
    h.enter_page_fullscreen();
    h.command("page.exit-fullscreen");
    h.page_says(false);
    // The same click can't enter again.
    let effects = h.page_says(true);
    assert_eq!(exits(&effects, view), 1);
    assert!(requests(&effects).is_empty());
    assert_eq!(h.fullscreen().page, None);
    // A new one can.
    h.enter_page_fullscreen();
}

#[test]
fn only_the_shown_documents_fullscreen_is_accepted() {
    let mut h = Harness::window(&["https://a.test/", "https://b.test/"]);
    let first = h.shown();
    h.click();
    h.command("tab.select 2");
    // The background tab's page asks, with a fresh click to its name.
    let effects = h.engine(EngineEvent::FullscreenChanged {
        view: first,
        document: h.document(first),
        fullscreen: true,
    });
    assert_eq!(exits(&effects, first), 1);
    assert_eq!(h.fullscreen().page, None);
    // A document the shown view no longer shows.
    let shown = h.shown();
    h.click();
    let effects = h.engine(EngineEvent::FullscreenChanged {
        view: shown,
        document: DocumentGeneration::next(),
        fullscreen: true,
    });
    assert_eq!(exits(&effects, shown), 1);
    assert_eq!(h.fullscreen().page, None);
}

#[test]
fn a_window_without_the_keyboard_is_refused() {
    let mut h = Harness::window(&["https://a.test/"]);
    let view = h.shown();
    h.click();
    h.window_focus(FocusEvent::Deactivated);
    let effects = h.page_says(true);
    assert_eq!(exits(&effects, view), 1);
    assert_eq!(h.fullscreen().page, None);
    h.window_focus(FocusEvent::Activated);
    h.enter_page_fullscreen();
}

/// Each way out ends the page's fullscreen at once: the engine is told, and the window goes back
/// to what the user wants without waiting for the engine.
#[test]
fn every_way_out_tells_the_engine_and_restores_the_window() {
    type Leave = fn(&mut Harness) -> Vec<Effect>;
    let ways: [(&str, Leave); 5] = [
        ("tab change", |h| h.command("tab.select 2")),
        ("new document", |h| {
            h.command("page.open https://a.test/next")
        }),
        ("focus lost", |h| h.window_focus(FocusEvent::Deactivated)),
        ("exit command", |h| h.command("page.exit-fullscreen")),
        ("compositor", |h| h.apply(BrowserEvent::Fullscreen(false))),
    ];
    for (name, leave) in ways {
        let mut h = Harness::window(&["https://a.test/", "https://b.test/"]);
        let view = h.shown();
        h.enter_page_fullscreen();
        h.apply(BrowserEvent::Fullscreen(true));
        let effects = leave(&mut h);
        assert_eq!(h.fullscreen().page, None, "{name}");
        assert_eq!(exits(&effects, view), 1, "{name}: {effects:?}");
        let window = if name == "compositor" {
            vec![]
        } else {
            vec![false]
        };
        assert_eq!(requests(&effects), window, "{name}");
        assert!(!h.fullscreen().wanted, "{name}");
        // A later `true` with no new activation is refused.
        let document = h.document(view);
        let effects = h.engine(EngineEvent::FullscreenChanged {
            view,
            document,
            fullscreen: true,
        });
        assert_eq!(exits(&effects, view), 1, "{name}: re-entry");
        assert_eq!(h.fullscreen().page, None, "{name}: re-entry");
    }
}

#[test]
fn a_failed_or_closed_view_ends_it() {
    let mut h = Harness::window(&["https://a.test/", "https://b.test/"]);
    let view = h.shown();
    h.enter_page_fullscreen();
    let effects = h.engine(EngineEvent::ViewFailed {
        view,
        message: "crashed".into(),
    });
    assert_eq!(h.fullscreen().page, None);
    assert_eq!(exits(&effects, view), 1);
    assert_eq!(requests(&effects), [false]);

    let mut h = Harness::window(&["https://a.test/", "https://b.test/"]);
    let view = h.shown();
    h.enter_page_fullscreen();
    let effects = h.command("tab.close");
    assert_eq!(h.fullscreen().page, None);
    // A view being closed is sent nothing after its `CloseView`.
    assert_eq!(exits(&effects, view), 0);
    assert_eq!(requests(&effects), [false]);
}

#[test]
fn the_engine_leaving_by_itself_restores_the_window() {
    let mut h = Harness::window(&["https://a.test/"]);
    let view = h.shown();
    h.enter_page_fullscreen();
    let effects = h.page_says(false);
    assert_eq!(h.fullscreen().page, None);
    assert_eq!(exits(&effects, view), 0);
    assert_eq!(requests(&effects), [false]);
}

#[test]
fn the_users_toggle_during_a_pages_fullscreen_leaves_both() {
    for wanted in [false, true] {
        let mut h = Harness::window(&["https://a.test/"]);
        let view = h.shown();
        if wanted {
            h.command("browser.fullscreen");
            h.apply(BrowserEvent::Fullscreen(true));
        }
        let effects = h.enter_page_fullscreen();
        // Already fullscreen: nothing more to ask.
        assert_eq!(requests(&effects), if wanted { vec![] } else { vec![true] });
        h.apply(BrowserEvent::Fullscreen(true));
        let effects = h.command("browser.fullscreen");
        assert_eq!(exits(&effects, view), 1);
        assert_eq!(requests(&effects), [false]);
        assert_eq!(
            h.fullscreen(),
            FullscreenState {
                wanted: false,
                window: true,
                page: None
            }
        );
    }
}

#[test]
fn leaving_a_pages_fullscreen_keeps_the_users() {
    let mut h = Harness::window(&["https://a.test/"]);
    h.command("browser.fullscreen");
    h.apply(BrowserEvent::Fullscreen(true));
    h.enter_page_fullscreen();
    let effects = h.command("page.exit-fullscreen");
    assert!(requests(&effects).is_empty());
    assert!(h.fullscreen().wanted && h.fullscreen().window);
}

#[test]
fn exit_without_a_pages_fullscreen_says_so() {
    let mut h = Harness::window(&["https://a.test/"]);
    assert_eq!(
        notices(&h.command("page.exit-fullscreen")),
        ["The page shown is not in fullscreen."]
    );
}

#[test]
fn the_terminal_takes_a_pages_fullscreen_as_a_notice_only() {
    let mut h = Harness::start(Frontend::Terminal, "bindings", &["https://a.test/"]);
    let view = h.shown();
    h.click();
    let effects = h.page_says(true);
    assert_eq!(
        notices(&effects),
        ["a.test is fullscreen. Escape leaves it."]
    );
    assert!(requests(&effects).is_empty());
    assert!(h.fullscreen().page.is_some());
    let effects = h.key(escape(Modifiers::default()));
    assert_eq!(exits(&effects, view), 1);
    assert!(requests(&effects).is_empty());
}

// ── Escape ────────────────────────────────────────────────────────────────────────────────

#[test]
fn escape_leaves_a_pages_fullscreen_whatever_the_keymap_and_takes_its_release() {
    let mut h = Harness::window(&["https://a.test/"]);
    let view = h.shown();
    h.enter_page_fullscreen();
    // `bindings` with no `[keys]` binds nothing: the core's own Escape.
    let press = escape(Modifiers::default());
    let effects = h.key(press.clone());
    assert_eq!(exits(&effects, view), 1);
    assert!(!delivered(&effects, view));
    assert_eq!(h.fullscreen().page, None);
    // Its repeats and release go nowhere, fullscreen over or not.
    let repeat = with_state(&press, ButtonState::Pressed, true);
    assert!(!delivered(&h.key(repeat), view));
    let release = with_state(&press, ButtonState::Released, false);
    assert!(!delivered(&h.key(release.clone()), view));
    // Out of fullscreen, Escape is the page's again.
    assert!(delivered(&h.key(press), view));
    assert!(delivered(&h.key(release), view));
}

#[test]
fn a_modified_escape_is_left_to_the_page() {
    let mut h = Harness::window(&["https://a.test/"]);
    let view = h.shown();
    h.enter_page_fullscreen();
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let effects = h.key(escape(shift));
    assert!(delivered(&effects, view));
    assert!(h.fullscreen().page.is_some());
}

#[test]
fn an_escape_pressed_before_fullscreen_is_not_stolen() {
    let mut h = Harness::window(&["https://a.test/"]);
    let view = h.shown();
    let press = escape(Modifiers::default());
    assert!(delivered(&h.key(press.clone()), view));
    h.enter_page_fullscreen();
    // The held key's repeats and release still go where its press went.
    let repeat = with_state(&press, ButtonState::Pressed, true);
    let effects = h.key(repeat);
    assert!(delivered(&effects, view));
    assert!(h.fullscreen().page.is_some());
    let release = with_state(&press, ButtonState::Released, false);
    assert!(delivered(&h.key(release), view));
    assert!(h.fullscreen().page.is_some());
}

#[test]
fn the_keymap_sees_the_page_fullscreen_and_can_leave_it_itself() {
    let mut h = Harness::start(Frontend::Gui, "watching", &["https://a.test/"]);
    let view = h.shown();
    // Not fullscreen: the keymap takes Escape for itself.
    let press = escape(Modifiers::default());
    let effects = h.key(press.clone());
    assert!(!delivered(&effects, view) && exits(&effects, view) == 0);
    h.key(with_state(&press, ButtonState::Released, false));
    h.enter_page_fullscreen();
    let effects = h.key(press);
    assert_eq!(exits(&effects, view), 1);
    assert_eq!(h.fullscreen().page, None);
}

#[test]
fn an_engine_without_fullscreen_is_neither_heard_nor_told() {
    let mut h = Harness::window(&["https://a.test/"]);
    h.command("page.open-in plain https://a.test/");
    let view = h.shown();
    assert_eq!(h.views[&view].0, "plain");
    h.click();
    let effects = h.page_says(true);
    assert_eq!(exits(&effects, view), 0);
    assert!(requests(&effects).is_empty());
    assert_eq!(h.fullscreen().page, None);
}

/// Engines say `false` on their own when the document goes, after a denial, and to confirm an
/// exit the core already carried out: none of it is news.
#[test]
fn a_late_or_repeated_false_changes_nothing() {
    let mut h = Harness::window(&["https://a.test/"]);
    let view = h.shown();
    // Denied, then the engine's confirmation.
    h.page_says(true);
    let effects = h.page_says(false);
    assert!(effects
        .iter()
        .all(|e| !matches!(e, Effect::Engine { .. } | Effect::SetFullscreen(_))));
    // Accepted, left by the core, then confirmed twice.
    h.enter_page_fullscreen();
    h.command("page.exit-fullscreen");
    for _ in 0..2 {
        let effects = h.page_says(false);
        assert_eq!(exits(&effects, view), 0);
        assert!(requests(&effects).is_empty());
    }
    // The engine leaving before a new document commits, then the commit.
    h.enter_page_fullscreen();
    let effects = h.page_says(false);
    assert_eq!(requests(&effects), [false]);
    let effects = h.command("page.open https://a.test/next");
    assert_eq!(exits(&effects, view), 0);
    assert!(requests(&effects).is_empty());
}

/// The notices `effects` take back.
fn withdrawn(effects: &[Effect]) -> Vec<&str> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::WithdrawNotice(message) => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

/// "a.test is fullscreen" is no longer true once the page's fullscreen ended, however it ended:
/// the notice is taken back, once, with the very text it was shown with.
#[test]
fn every_way_out_takes_the_notice_back() {
    type Leave = fn(&mut Harness) -> Vec<Effect>;
    let ways: [(&str, Leave); 10] = [
        ("Escape", |h| h.key(escape(Modifiers::default()))),
        ("exit command", |h| h.command("page.exit-fullscreen")),
        ("tab change", |h| h.command("tab.select 2")),
        ("new document", |h| {
            h.command("page.open https://a.test/next")
        }),
        ("focus lost", |h| h.window_focus(FocusEvent::Deactivated)),
        ("compositor", |h| h.apply(BrowserEvent::Fullscreen(false))),
        ("the user's toggle", |h| h.command("browser.fullscreen")),
        ("the engine", |h| h.page_says(false)),
        ("view failed", |h| {
            let view = h.shown();
            h.engine(EngineEvent::ViewFailed {
                view,
                message: "crashed".into(),
            })
        }),
        ("tab closed", |h| h.command("tab.close")),
    ];
    for (name, leave) in ways {
        let mut h = Harness::window(&["https://a.test/", "https://b.test/"]);
        let entered = h.enter_page_fullscreen();
        assert_eq!(notices(&entered), [NOTICE], "{name}");
        h.apply(BrowserEvent::Fullscreen(true));
        assert_eq!(withdrawn(&leave(&mut h)), [NOTICE], "{name}");
        // Once: nothing is left to take back.
        assert!(withdrawn(&h.page_says(false)).is_empty(), "{name}");
        assert!(
            withdrawn(&h.command("page.exit-fullscreen")).is_empty(),
            "{name}"
        );
    }
}

const NOTICE: &str = "a.test is fullscreen. Escape leaves it.";

#[test]
fn a_refused_fullscreen_shows_and_takes_back_nothing() {
    let mut h = Harness::window(&["https://a.test/"]);
    let effects = h.page_says(true);
    assert!(notices(&effects).is_empty() && withdrawn(&effects).is_empty());
    assert!(withdrawn(&h.page_says(false)).is_empty());
}
