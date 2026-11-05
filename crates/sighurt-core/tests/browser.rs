//! The core driven as the executable drives it: events in, effects out, with a fake engine
//! playing the engines' part. No display, no engine, no I/O.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, FindCommand, ViewOptions,
};
use sighurt_api::events::{Cursor, EngineEvent, HistoryEffect, NavigationError, OpenDisposition};
use sighurt_api::geometry::{SurfacePoint, SurfaceRect};
use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, SeatId, TabId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, Key, KeyCode, KeyEvent, KeyboardFocus,
    Modifiers, PointerButton, PointerEvent, ScrollDelta, TextInputState, TextPurpose,
};
use sighurt_api::semantic::{NodeId, SemanticAction};
use sighurt_api::services::{
    ClipboardData, DownloadSource, Permission, Selection, Service, ServiceError, ServiceReply,
    ServiceRequest,
};
use sighurt_core::config::{Config, ConfigError};
use sighurt_core::navigation::RETAINED_VIEWS;
use sighurt_core::session::{Session, MAX_TABS};
use sighurt_core::tabs::{Content, TabSummary, ViewState, MAX_URL};
use sighurt_core::{
    update, BrowserEvent, BrowserState, ChromeChecks, Effect, Environment, OpenRequest,
};

/// A view as the fake engine keeps it: its session history, like Servo's.
struct FakeView {
    engine: String,
    entries: Vec<String>,
    current: usize,
    document: DocumentGeneration,
}

/// The engines of a build: servo and wasm traverse their history and zoom, the demo engine
/// does neither. Only wasm finds in page.
fn engines(names: &[&str]) -> BTreeMap<String, EngineCapabilities> {
    let full = EngineCapabilities {
        history_traversal: true,
        page_zoom: true,
        ..EngineCapabilities::default()
    };
    let capabilities = |name| match name {
        "demo" => EngineCapabilities::default(),
        "wasm" => EngineCapabilities {
            find_in_page: true,
            ..full
        },
        _ => full,
    };
    names
        .iter()
        .map(|&name| (name.to_owned(), capabilities(name)))
        .collect()
}

/// The browser core with a fake engine for every view it opens.
struct Harness {
    state: BrowserState,
    now: Instant,
    views: BTreeMap<ViewId, FakeView>,
    seat: SeatId,
}

/// A build with every engine (Servo honoring its settings, Wasm `storage`), the test keymap
/// `script`, and two chromes with no options: the standard one with the command
/// `chrome.toggle-tabs`, and the line chrome with none.
fn environment() -> Environment {
    let chrome = |name, commands| ChromeChecks {
        name,
        check: |_| Vec::new(),
        commands,
    };
    let settings: [(&str, &'static [&'static str]); 2] = [
        (
            "servo",
            &[
                "user-agent",
                "storage",
                "experimental-features",
                "preferences",
            ],
        ),
        ("wasm", &["storage"]),
    ];
    Environment {
        engines: engines(&["servo", "wasm", "demo"]),
        engine_settings: settings.map(|(e, s)| (e.to_owned(), s)).into(),
        keymaps: vec![script::KIND],
        chromes: vec![chrome("standard", &["toggle-tabs"]), chrome("line", &[])],
        renderers: sighurt_core::config::RENDERERS,
        ..Environment::default()
    }
}

fn command(text: &str) -> BrowserEvent {
    BrowserEvent::Command(text.parse().unwrap())
}

/// The configuration file was read again: `config` is what parsing it gave.
fn config_reloaded(config: Result<Config, ConfigError>) -> BrowserEvent {
    BrowserEvent::ConfigReloaded(config.map(Box::new))
}

fn open(input: &str) -> OpenRequest {
    OpenRequest {
        input: input.into(),
        engine: None,
    }
}

impl Harness {
    /// A browser with `config` (without its version line) and every engine compiled in.
    fn new(config: &str) -> Self {
        let env = environment();
        let config = Config::parse(Some(&format!("version = 1\n{config}")), &env).unwrap();
        Self {
            state: BrowserState::new(config, env),
            now: Instant::now(),
            views: BTreeMap::new(),
            seat: SeatId::next(),
        }
    }

    /// Starts with a tab for each of `urls`, letting the engines load them.
    fn start(config: &str, urls: &[&str]) -> Self {
        let mut harness = Self::new(config);
        harness.apply(BrowserEvent::Start {
            session: None,
            open: urls.iter().map(|url| open(url)).collect(),
        });
        harness
    }

    /// A browser whose build is `env`, with `config` (without its version line).
    fn with_env(config: &str, env: Environment) -> Self {
        let config = Config::parse(Some(&format!("version = 1\n{config}")), &env).unwrap();
        Self {
            state: BrowserState::new(config, env),
            now: Instant::now(),
            views: BTreeMap::new(),
            seat: SeatId::next(),
        }
    }

    /// Applies one event; the engines don't answer.
    fn update(&mut self, event: BrowserEvent) -> Vec<Effect> {
        let effects = update(&mut self.state, event);
        for effect in &effects {
            if let Effect::OpenView { view, engine, .. } = effect {
                let fake = FakeView {
                    engine: engine.clone(),
                    entries: Vec::new(),
                    current: 0,
                    document: DocumentGeneration::next(),
                };
                self.views.insert(*view, fake);
            }
        }
        effects
    }

    /// Applies `event`, then lets the fake engines carry out the effects, and so on until
    /// nothing more happens. Returns every effect.
    fn apply(&mut self, event: BrowserEvent) -> Vec<Effect> {
        let mut all = Vec::new();
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            let effects = self.update(event);
            for effect in &effects {
                queue.extend(self.answer(effect));
            }
            all.extend(effects);
            assert!(all.len() < 10_000, "the core and the engines never settle");
        }
        all
    }

    /// What the fake engine reports after `effect`. A search the engine can't do is answered
    /// as the runtime answers it; one it can is left for the test to answer.
    fn answer(&mut self, effect: &Effect) -> Vec<BrowserEvent> {
        if let Effect::Find { view, command } = effect {
            let finds = self.views[view].engine == "wasm";
            return match command {
                FindCommand::Search { search, .. }
                | FindCommand::Next { search }
                | FindCommand::Previous { search }
                    if !finds =>
                {
                    vec![BrowserEvent::FindResult {
                        view: *view,
                        search: *search,
                        found: false,
                        current: None,
                        total: None,
                        supported: false,
                    }]
                }
                _ => Vec::new(),
            };
        }
        let (view, events) = match effect {
            Effect::OpenView { view, .. } => (*view, vec![EngineEvent::ViewReady { view: *view }]),
            Effect::CloseView { view } => (*view, vec![EngineEvent::ViewClosed { view: *view }]),
            Effect::Engine { view, command } => (*view, self.navigate(*view, command)),
            _ => return Vec::new(),
        };
        let engine = self.views[&view].engine.clone();
        events
            .into_iter()
            .map(|event| BrowserEvent::Engine {
                engine: engine.clone(),
                event,
                at: self.now,
            })
            .collect()
    }

    /// The fake engine carrying out a navigation command, the way Servo would: a URL that
    /// differs only in its fragment stays in the same document.
    fn navigate(&mut self, id: ViewId, command: &EngineCommand) -> Vec<EngineEvent> {
        let view = self.views.get_mut(&id).unwrap();
        let (navigation, same_document) = match command {
            EngineCommand::Navigate { navigation, url } => {
                let same = view
                    .entries
                    .get(view.current)
                    .is_some_and(|current| without_fragment(current) == without_fragment(url));
                view.entries.truncate(view.current + 1);
                view.entries.push(url.clone());
                view.current = view.entries.len() - 1;
                (*navigation, same)
            }
            EngineCommand::Reload { navigation } => (*navigation, false),
            EngineCommand::Traverse { navigation, delta } => {
                let Some(target) = view
                    .current
                    .checked_add_signed(*delta as isize)
                    .filter(|&i| i < view.entries.len())
                else {
                    let error = NavigationError::Cancelled;
                    let navigation = *navigation;
                    return vec![EngineEvent::NavigationFailed {
                        view: id,
                        navigation,
                        error,
                    }];
                };
                let same = without_fragment(&view.entries[target])
                    == without_fragment(&view.entries[view.current]);
                view.current = target;
                (*navigation, same)
            }
            _ => return Vec::new(),
        };
        if !same_document {
            view.document = DocumentGeneration::next();
        }
        vec![
            EngineEvent::NavigationCommitted {
                view: id,
                navigation,
                url: view.entries[view.current].clone(),
                document: view.document,
                history: None,
            },
            EngineEvent::HistoryChanged {
                view: id,
                entries: view.entries.clone(),
                current: view.current,
            },
        ]
    }

    /// Reports `event` from its view's engine.
    fn engine(&mut self, event: EngineEvent) -> Vec<Effect> {
        let engine = self.views[&event.view().unwrap()].engine.clone();
        self.update(BrowserEvent::Engine {
            engine,
            event,
            at: self.now,
        })
    }

    /// The document the fake engine shows in `view`.
    fn document(&self, view: ViewId) -> DocumentGeneration {
        self.views[&view].document
    }

    fn tab(&self, index: usize) -> TabSummary<'_> {
        self.state.tabs().nth(index).unwrap()
    }

    fn active(&self) -> TabSummary<'_> {
        self.tab(self.state.active())
    }

    /// The view the active tab shows.
    fn shown(&self) -> ViewId {
        match self.active().content {
            Content::View(view) => view,
            other => panic!("the active tab shows {other:?}"),
        }
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let event = InputEvent {
            seat: self.seat,
            kind: InputKind::Key(key),
        };
        self.update(BrowserEvent::Keyboard {
            event,
            at: self.now,
        })
    }

    fn click(&mut self, view: ViewId, button: PointerButton, state: ButtonState) -> Vec<Effect> {
        let event = InputEvent {
            seat: self.seat,
            kind: InputKind::Pointer(PointerEvent::Button {
                button,
                state,
                position: SurfacePoint { x: 5.0, y: 5.0 },
                modifiers: Modifiers::default(),
            }),
        };
        self.update(BrowserEvent::Pointer {
            view,
            event,
            at: self.now,
        })
    }

    /// A click on a link to `url` in `view`: the pointer over it (the page's status text
    /// names it), then the press.
    fn click_link(&mut self, view: ViewId, url: &str) -> Vec<Effect> {
        self.engine(EngineEvent::StatusTextChanged {
            view,
            text: Some(url.into()),
        });
        self.click(view, PointerButton::Left, ButtonState::Pressed)
    }

    fn request(&mut self, view: ViewId, service: Service) -> (RequestId, Vec<Effect>) {
        let id = RequestId::next();
        let request = ServiceRequest {
            id,
            view,
            document: self.document(view),
            origin: Some("https://a.test".into()),
            service,
        };
        (id, self.engine(EngineEvent::ServiceRequested(request)))
    }

    fn tick(&mut self, duration: Duration) {
        self.now += duration;
    }
}

fn without_fragment(url: &str) -> &str {
    url.split('#').next().unwrap()
}

/// The views `effects` open, with their engines and options.
fn opened(effects: &[Effect]) -> Vec<(ViewId, &str, ViewOptions)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::OpenView {
                view,
                engine,
                options,
            } => Some((*view, engine.as_str(), *options)),
            _ => None,
        })
        .collect()
}

/// The commands `effects` send to `view`.
fn commands(effects: &[Effect], view: ViewId) -> Vec<&EngineCommand> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Engine { view: to, command } if *to == view => Some(command),
            _ => None,
        })
        .collect()
}

fn closed(effects: &[Effect]) -> Vec<ViewId> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::CloseView { view } => Some(*view),
            _ => None,
        })
        .collect()
}

/// The navigation `effects` send to `view`.
fn navigation(effects: &[Effect], view: ViewId) -> (NavigationId, String) {
    commands(effects, view)
        .into_iter()
        .find_map(|command| match command {
            EngineCommand::Navigate { navigation, url } => Some((*navigation, url.clone())),
            _ => None,
        })
        .expect("a navigation")
}

/// The answers `effects` give `view`'s service requests.
fn answers(
    effects: &[Effect],
    view: ViewId,
) -> Vec<(RequestId, Result<ServiceReply, ServiceError>)> {
    commands(effects, view)
        .into_iter()
        .filter_map(|command| match command {
            EngineCommand::ResolveServiceRequest { request, result } => {
                Some((*request, result.clone()))
            }
            _ => None,
        })
        .collect()
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

fn press(code: u32, key: &str, modifiers: Modifiers) -> KeyEvent {
    KeyEvent {
        code: KeyCode(code),
        key: if key.chars().count() == 1 {
            Key::Character(key.into())
        } else {
            Key::Named(key.into())
        },
        state: ButtonState::Pressed,
        repeat: false,
        modifiers,
        consumed: Modifiers::default(),
        text: None,
    }
}

fn release(press: &KeyEvent) -> KeyEvent {
    KeyEvent {
        state: ButtonState::Released,
        ..press.clone()
    }
}

const CTRL: Modifiers = Modifiers {
    shift: false,
    control: true,
    alt: false,
    alt_graph: false,
    logo: false,
    caps_lock: false,
    num_lock: false,
};

// ── Starting, tabs and commands ─────────────────────────────────────────────────────────────

#[test]
fn starts_with_the_home_page_or_what_the_command_line_names() {
    let b = Harness::start("", &[]);
    assert_eq!(b.state.tabs().len(), 1);
    assert_eq!(b.active().content, Content::Internal("sig://home"));
    assert!(b.views.is_empty(), "an internal page needs no engine");

    let mut b = Harness::new("");
    let effects = b.apply(BrowserEvent::Start {
        session: None,
        open: vec![open("a.test/x"), open("/tmp/app.wasm"), open("rust servo")],
    });
    let engines: Vec<_> = opened(&effects).into_iter().map(|(_, e, _)| e).collect();
    assert_eq!(engines, ["servo", "wasm", "servo"]);
    assert_eq!(b.state.active(), 0);
    assert_eq!(b.tab(0).url, Some("https://a.test/x"));
    assert_eq!(b.tab(1).url, Some("file:///tmp/app.wasm"));
    assert!(b
        .tab(2)
        .url
        .unwrap()
        .starts_with("https://duckduckgo.com/html/?q=rust%20servo"));
    // Only the active tab's view is shown and has the keyboard.
    let shown = b.shown();
    assert_eq!(b.state.view(shown).unwrap().state(), ViewState::LiveVisible);
    let Content::View(background) = b.tab(1).content else {
        panic!()
    };
    assert_eq!(
        b.state.view(background).unwrap().state(),
        ViewState::LiveHidden
    );
}

#[test]
fn tabs_keep_their_numbering_and_closing_rules() {
    let mut b = Harness::start("", &["sig://home", "sig://home", "sig://home"]);
    let ids: Vec<TabId> = b.state.tabs().map(|t| t.id).collect();
    b.apply(command("tab.select 3"));
    b.apply(command("tab.close 3"));
    assert_eq!(b.state.active(), 1);
    b.apply(command("tab.close 1"));
    assert_eq!((b.state.active(), b.active().id), (0, ids[1]));
    // The last tab stays.
    b.apply(command("tab.close"));
    assert_eq!(b.state.tabs().len(), 1);

    let mut b = Harness::start("", &["sig://home", "sig://home"]);
    b.apply(command("tab.previous"));
    assert_eq!(b.state.active(), 1, "wraps around");
    b.apply(command("tab.next"));
    assert_eq!(b.state.active(), 0);
    b.apply(command("tab.select 9"));
    assert_eq!(b.state.active(), 0, "no such tab");
    let effects = b.apply(command("tab.new"));
    assert_eq!((b.state.active(), b.state.tabs().len()), (2, 3));
    assert!(effects.contains(&Effect::Focus(KeyboardFocus::Chrome)));
    let effects = b.apply(command("tab.last"));
    assert!(effects.contains(&Effect::Focus(KeyboardFocus::Page)));
}

/// The tabs' committed addresses, in order.
fn urls(b: &Harness) -> Vec<String> {
    b.state
        .tabs()
        .map(|tab| tab.url.unwrap_or("").to_owned())
        .collect()
}

#[test]
fn tab_new_opens_an_address_as_page_open_does() {
    let mut b = Harness::start(
        "[keywords]\nw = \"https://w.test/?q={}\"",
        &["https://a.test/"],
    );
    let effects = b.apply(command("tab.new w rust"));
    assert_eq!((b.state.active(), b.state.tabs().len()), (1, 2));
    assert_eq!(b.active().url, Some("https://w.test/?q=rust"));
    // The page gets the keyboard, not the address line.
    assert!(effects.contains(&Effect::Focus(KeyboardFocus::Page)));
    assert!(!effects.contains(&Effect::Focus(KeyboardFocus::Chrome)));

    b.apply(command("tab.new rust servo"));
    assert_eq!(b.state.active(), 2);
    assert!(b
        .active()
        .url
        .unwrap()
        .starts_with("https://duckduckgo.com/html/?q=rust%20servo"));

    // Routing, as for a typed address: a Wasm file goes to the wasm engine.
    let effects = b.apply(command("tab.new /tmp/app.wasm"));
    let engines: Vec<_> = opened(&effects).into_iter().map(|(_, e, _)| e).collect();
    assert_eq!(engines, ["wasm"]);
    assert_eq!(b.state.tabs().len(), 4);

    // Without an address, the home page and the address line, as before.
    let effects = b.apply(command("tab.new"));
    assert_eq!(b.active().content, Content::Internal("sig://home"));
    assert!(effects.contains(&Effect::Focus(KeyboardFocus::Chrome)));
}

#[test]
fn tab_duplicate_opens_the_address_after_the_selected_tab() {
    let mut b = Harness::start(
        "",
        &["https://a.test/", "https://b.test/x", "https://c.test/"],
    );
    b.apply(command("tab.select 2"));
    b.apply(command("page.zoom-in"));
    let original = b.shown();
    let effects = b.apply(command("tab.duplicate"));
    assert_eq!(
        urls(&b),
        [
            "https://a.test/",
            "https://b.test/x",
            "https://b.test/x",
            "https://c.test/"
        ]
    );
    assert_eq!(b.state.active(), 2);
    assert_ne!(b.shown(), original, "a view of its own");
    assert_eq!(b.active().zoom, b.tab(1).zoom);
    assert!(effects.contains(&Effect::Focus(KeyboardFocus::Page)));
    // Its history is not copied.
    assert!(!b.active().can_go_back);

    // A page opened in an engine the user chose stays in that engine.
    let mut b = Harness::start("", &["https://a.test/"]);
    b.apply(command("page.open-in wasm"));
    let effects = b.apply(command("tab.duplicate"));
    let engines: Vec<_> = opened(&effects).into_iter().map(|(_, e, _)| e).collect();
    assert_eq!(engines, ["wasm"]);
}

#[test]
fn tab_duplicate_of_a_tab_with_no_page_is_a_home_tab() {
    let mut b = Harness::start("", &["https://a.test/"]);
    // A tab whose first page never committed.
    b.update(command("tab.new https://slow.test/"));
    assert_eq!(b.active().url, None);
    b.apply(command("tab.duplicate"));
    assert_eq!(b.state.tabs().len(), 3);
    assert_eq!(b.state.active(), 2);
    assert_eq!(b.active().content, Content::Internal("sig://home"));
}

#[test]
fn tab_move_wraps_and_clamps_and_the_selection_follows() {
    let mut b = Harness::start(
        "",
        &["https://a.test/", "https://b.test/", "https://c.test/"],
    );
    let ids: Vec<TabId> = b.state.tabs().map(|t| t.id).collect();
    let order = |b: &Harness| b.state.tabs().map(|t| t.id).collect::<Vec<_>>();
    let effects = b.apply(command("tab.move right"));
    assert_eq!(order(&b), [ids[1], ids[0], ids[2]]);
    assert_eq!(b.state.active(), 1);
    assert_eq!(b.active().id, ids[0]);
    assert!(effects.contains(&Effect::SaveSession));
    assert!(effects.contains(&Effect::RedrawChrome));
    // The keyboard stays where it was.
    assert!(!effects.iter().any(|e| matches!(e, Effect::Focus(_))));

    b.apply(command("tab.move 3"));
    assert_eq!(order(&b), [ids[1], ids[2], ids[0]]);
    // The last tab right wraps to the start, the first left to the end.
    b.apply(command("tab.move right"));
    assert_eq!(order(&b), [ids[0], ids[1], ids[2]]);
    assert_eq!(b.state.active(), 0);
    b.apply(command("tab.move left"));
    assert_eq!(order(&b), [ids[1], ids[2], ids[0]]);
    assert_eq!((b.state.active(), b.active().id), (2, ids[0]));

    // Past the end is the end; 0 is not a position.
    b.apply(command("tab.select 1"));
    b.apply(command("tab.move 999"));
    assert_eq!(order(&b), [ids[2], ids[0], ids[1]]);
    assert_eq!(b.state.active(), 2);
    assert!("tab.move 0"
        .parse::<sighurt_api::commands::Command>()
        .is_err());
    // Where it already is: nothing changes, nothing is saved.
    let effects = b.apply(command("tab.move 3"));
    assert!(!effects.contains(&Effect::SaveSession));

    // The session saves the new order.
    let saved: Vec<String> = b
        .state
        .session()
        .tabs
        .iter()
        .map(|tab| tab.segments[0].entries[0].clone())
        .collect();
    assert_eq!(
        saved,
        ["https://c.test/", "https://a.test/", "https://b.test/"]
    );

    // One tab: nothing to move.
    let mut b = Harness::start("", &["https://a.test/"]);
    let effects = b.apply(command("tab.move left"));
    assert_eq!(b.state.tabs().len(), 1);
    assert!(!effects.contains(&Effect::SaveSession));
}

#[test]
fn tab_reopen_opens_the_address_of_the_tab_closed_last() {
    let mut b = Harness::start("", &["https://a.test/", "https://b.test/"]);
    let effects = b.apply(command("tab.reopen"));
    assert_eq!(
        notices(&effects),
        [
            "No closed tab to reopen: tab.reopen brings back the last 10 tabs closed since sig \
          started."
        ]
    );
    assert_eq!(b.state.tabs().len(), 2);

    // Its address, in a new tab at the end, selected, at its zoom: not its history.
    b.apply(command("tab.select 1"));
    b.apply(command("page.open https://a.test/2"));
    b.apply(command("page.zoom-in"));
    let zoom = b.active().zoom;
    b.apply(command("tab.close 1"));
    assert_eq!(urls(&b), ["https://b.test/"]);
    let effects = b.apply(command("tab.reopen"));
    assert_eq!(urls(&b), ["https://b.test/", "https://a.test/2"]);
    assert_eq!(b.state.active(), 1);
    assert_eq!(b.active().zoom, zoom);
    assert!(effects.contains(&Effect::Focus(KeyboardFocus::Page)));
    assert!(!b.active().can_go_back, "the history is not restored");
    // Reopened once: there is nothing left.
    let effects = b.apply(command("tab.reopen"));
    assert_eq!(notices(&effects).len(), 1);

    // In the engine the user chose for it.
    b.apply(command("page.open-in wasm https://w.test/app.wasm"));
    b.apply(command("tab.close"));
    let effects = b.apply(command("tab.reopen"));
    let engines: Vec<_> = opened(&effects).into_iter().map(|(_, e, _)| e).collect();
    assert_eq!(engines, ["wasm"]);
    assert_eq!(b.active().url, Some("https://w.test/app.wasm"));
}

#[test]
fn tab_reopen_keeps_the_last_ten_closed() {
    let pages: Vec<String> = (0..12).map(|i| format!("https://p{i}.test/")).collect();
    let mut all: Vec<&str> = pages.iter().map(String::as_str).collect();
    all.push("https://last.test/");
    let mut b = Harness::start("", &all);
    // Close the first 11, oldest first.
    for _ in 0..11 {
        b.apply(command("tab.close 1"));
    }
    assert_eq!(b.state.tabs().len(), 2);
    for i in (1..11).rev() {
        b.apply(command("tab.reopen"));
        assert_eq!(b.active().url, Some(pages[i].as_str()));
    }
    // p0 was the eleventh: dropped.
    let effects = b.apply(command("tab.reopen"));
    assert_eq!(notices(&effects).len(), 1);
    assert_eq!(b.state.tabs().len(), 12);
}

#[test]
fn tab_reopen_needs_room_and_a_close_that_closed() {
    // The last tab stays, so closing it keeps nothing to reopen.
    let mut b = Harness::start("", &["https://a.test/"]);
    b.apply(command("tab.close"));
    assert_eq!(notices(&b.apply(command("tab.reopen"))).len(), 1);

    // With MAX_TABS open, reopening says so and opens nothing.
    let mut b = Harness::start("", &["sig://home"]);
    b.apply(command("tab.new https://a.test/"));
    b.apply(command("tab.close"));
    while b.state.tabs().len() < MAX_TABS {
        b.update(command("tab.new"));
    }
    let effects = b.update(command("tab.reopen"));
    assert!(notices(&effects)[0].contains("at most"), "{effects:?}");
    assert_eq!(b.state.tabs().len(), MAX_TABS);
    assert!(opened(&effects).is_empty());
}

#[test]
fn tab_reopen_opens_what_the_tab_was_loading() {
    let mut b = Harness::start("", &["https://a.test/"]);
    // A tab still loading its first page keeps the address it was loading.
    b.update(command("tab.new https://slow.test/"));
    assert_eq!(b.active().url, None);
    b.apply(command("tab.close"));
    b.apply(command("tab.reopen"));
    assert_eq!(b.active().url, Some("https://slow.test/"));
}

#[test]
fn location_focus_with_text_hands_the_line_its_text_after_the_focus() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let effects = b.apply(command("location.focus :"));
    let focus = effects
        .iter()
        .position(|e| *e == Effect::Focus(KeyboardFocus::Chrome));
    let text = effects
        .iter()
        .position(|e| *e == Effect::LineText(":".into()));
    assert!(focus.is_some() && focus < text, "{effects:?}");
    assert_eq!(b.state.focus(), KeyboardFocus::Chrome);
    // Without text, only the focus.
    let effects = b.apply(command("location.focus"));
    assert!(effects.contains(&Effect::Focus(KeyboardFocus::Chrome)));
    assert!(!effects.iter().any(|e| matches!(e, Effect::LineText(_))));
}

/// The find commands of the effects, by view.
fn finds(effects: &[Effect]) -> Vec<(ViewId, &FindCommand)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Find { view, command } => Some((*view, command)),
            _ => None,
        })
        .collect()
}

/// The search a `Search` among the effects started.
fn started(effects: &[Effect]) -> RequestId {
    let searches: Vec<RequestId> = finds(effects)
        .into_iter()
        .filter_map(|(_, command)| match command {
            FindCommand::Search { search, .. } => Some(*search),
            _ => None,
        })
        .collect();
    assert_eq!(searches.len(), 1, "{effects:?}");
    searches[0]
}

/// `view`'s search as the chrome sees it: found, current, total, supported.
type FindView = (String, Option<bool>, Option<u32>, Option<u32>, bool);

fn find_state(b: &Harness) -> Option<FindView> {
    b.active().find.map(|find| {
        (
            find.text.clone(),
            find.found,
            find.current,
            find.total,
            find.supported,
        )
    })
}

/// An engine's result for `search` in `view`'s document.
fn found(view: ViewId, document: DocumentGeneration, search: RequestId, n: u32) -> EngineEvent {
    EngineEvent::FindResult {
        view,
        document,
        search,
        found: n > 0,
        current: (n > 0).then_some(1),
        total: Some(n),
    }
}

#[test]
fn find_goes_to_the_view_with_smart_case_and_its_results_fill_the_chrome() {
    let mut b = Harness::start("", &["file:///tmp/app.wasm"]);
    let view = b.shown();
    let document = b.document(view);
    let effects = b.apply(command("page.find hello"));
    assert!(notices(&effects).is_empty(), "{effects:?}");
    let search = started(&effects);
    assert_eq!(
        finds(&effects),
        [(
            view,
            &FindCommand::Search {
                search,
                document,
                text: "hello".into(),
                case_sensitive: false,
            }
        )]
    );
    assert!(effects.contains(&Effect::RedrawChrome));
    assert_eq!(
        find_state(&b),
        Some(("hello".into(), None, None, None, true))
    );
    let effects = b.engine(found(view, document, search, 12));
    assert!(effects.contains(&Effect::RedrawChrome));
    assert_eq!(
        find_state(&b),
        Some(("hello".into(), Some(true), Some(1), Some(12), true))
    );
    // Upper case anywhere makes it case-sensitive.
    let effects = b.apply(command("page.find Hello"));
    assert!(matches!(
        finds(&effects)[..],
        [(
            _,
            FindCommand::Search {
                case_sensitive: true,
                ..
            }
        )]
    ));
    let search = started(&effects);
    // The moves name the search.
    let effects = b.apply(command("page.find-next"));
    assert_eq!(finds(&effects), [(view, &FindCommand::Next { search })]);
    let effects = b.apply(command("page.find-previous"));
    assert_eq!(finds(&effects), [(view, &FindCommand::Previous { search })]);
    // No match: no current one.
    b.engine(EngineEvent::FindResult {
        view,
        document,
        search,
        found: false,
        current: Some(3),
        total: Some(0),
    });
    assert_eq!(
        find_state(&b),
        Some(("Hello".into(), Some(false), None, Some(0), true))
    );
    // Stop ends it.
    let effects = b.apply(command("page.find-stop"));
    assert_eq!(finds(&effects), [(view, &FindCommand::Stop { search })]);
    assert_eq!(find_state(&b), None);
    // A result after the stop is late.
    b.engine(found(view, document, search, 4));
    assert_eq!(find_state(&b), None);
    // Stopping again sends nothing.
    assert!(finds(&b.apply(command("page.find-stop"))).is_empty());
}

#[test]
fn a_late_result_of_a_replaced_search_or_another_document_is_dropped() {
    let mut b = Harness::start("", &["file:///tmp/app.wasm"]);
    let view = b.shown();
    let document = b.document(view);
    let a = started(&b.apply(command("page.find a")));
    let ab = started(&b.apply(command("page.find ab")));
    assert_ne!(a, ab);
    b.engine(found(view, document, ab, 2));
    // A's result arrives after B's: dropped.
    b.engine(found(view, document, a, 9));
    assert_eq!(
        find_state(&b),
        Some(("ab".into(), Some(true), Some(1), Some(2), true))
    );
    // A result for another document is dropped too.
    b.engine(found(view, DocumentGeneration::next(), ab, 7));
    assert_eq!(find_state(&b).unwrap().3, Some(2));
    // And one from the frontend for an old search.
    b.apply(BrowserEvent::FindResult {
        view,
        search: a,
        found: true,
        current: Some(5),
        total: Some(5),
        supported: true,
    });
    assert_eq!(find_state(&b).unwrap().3, Some(2));
    // The frontend's answer for the search counts like the engine's.
    b.apply(BrowserEvent::FindResult {
        view,
        search: ab,
        found: true,
        current: Some(2),
        total: Some(3),
        supported: true,
    });
    assert_eq!(
        find_state(&b),
        Some(("ab".into(), Some(true), Some(2), Some(3), true))
    );
}

#[test]
fn a_new_document_or_a_failed_one_ends_the_search() {
    let mut b = Harness::start("", &["file:///tmp/app.wasm"]);
    let view = b.shown();
    let document = b.document(view);
    let search = started(&b.apply(command("page.find x")));
    // A fragment navigation stays in the document and keeps the search.
    let effects = b.apply(command("page.open file:///tmp/app.wasm#part"));
    assert!(finds(&effects).is_empty(), "{effects:?}");
    assert_eq!(b.document(view), document);
    assert!(find_state(&b).is_some());
    // A new document ends it, and whoever searched is told.
    let effects = b.apply(command("page.open file:///tmp/other.wasm"));
    assert_eq!(b.shown(), view, "the same view");
    assert_eq!(finds(&effects), [(view, &FindCommand::Stop { search })]);
    assert_eq!(find_state(&b), None);
    // The old document's late result is dropped.
    b.engine(found(view, document, search, 3));
    assert_eq!(find_state(&b), None);
    let moves = b.apply(command("page.find-next"));
    assert_eq!(
        notices(&moves),
        ["Nothing to find: page.find <text> first."]
    );
    assert!(finds(&moves).is_empty());
    // A failed document ends it as well.
    let search = started(&b.apply(command("page.find y")));
    let effects = b.engine(EngineEvent::ViewFailed {
        view,
        message: "trapped".into(),
    });
    assert_eq!(finds(&effects), [(view, &FindCommand::Stop { search })]);
    assert!(b.state.view(view).unwrap().find().is_none());
    // A failed view has no document to search.
    assert_eq!(
        notices(&b.apply(command("page.find y"))),
        ["The page has not loaded yet: there is nothing to find in."]
    );
}

#[test]
fn a_page_nobody_can_search_says_so_once() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    // The core asks anyway: the frontend may search its text itself.
    let effects = b.apply(command("page.find hello"));
    assert_eq!(finds(&effects).len(), 1);
    assert_eq!(notices(&effects), ["servo can't find in page."]);
    assert_eq!(
        find_state(&b),
        Some(("hello".into(), None, None, None, false))
    );
    // Typing on in the find field: asked again, but not told again.
    let effects = b.apply(command("page.find hello w"));
    assert_eq!(finds(&effects).len(), 1);
    assert!(notices(&effects).is_empty(), "{effects:?}");
    // The moves have nothing to move in.
    for moves in ["page.find-next", "page.find-previous"] {
        let effects = b.apply(command(moves));
        assert_eq!(notices(&effects), ["servo can't find in page."]);
        assert!(finds(&effects).is_empty());
    }
    // An engine that declared find and then refuses it counts as unable.
    let mut b = Harness::start("", &["file:///tmp/app.wasm"]);
    let wasm = b.shown();
    let effects = b.apply(command("page.find z"));
    let FindCommand::Search { .. } = finds(&effects)[0].1 else {
        panic!("{effects:?}")
    };
    let command = EngineCommand::Find(finds(&effects)[0].1.clone());
    let effects = b.apply(BrowserEvent::CommandFailed {
        view: wasm,
        command,
        error: EngineError::Unsupported("find"),
    });
    assert_eq!(notices(&effects), ["wasm can't find in page."]);
    assert!(!b.active().find.unwrap().supported);
    let _ = view;
}

#[test]
fn find_without_text_opens_the_find_field_with_the_search() {
    let mut b = Harness::start("", &["file:///tmp/app.wasm"]);
    let effects = b.apply(command("page.find"));
    let focus = effects
        .iter()
        .position(|e| *e == Effect::Focus(KeyboardFocus::Chrome));
    let field = effects
        .iter()
        .position(|e| *e == Effect::FindField(String::new()));
    assert!(focus.is_some() && focus < field, "{effects:?}");
    assert!(finds(&effects).is_empty());
    b.apply(command("page.find Some text"));
    let effects = b.apply(command("page.find"));
    assert!(effects.contains(&Effect::FindField("Some text".into())));
}

#[test]
fn find_says_what_it_cannot_do() {
    use sighurt_api::commands::{Command, MAX_FIND};
    let mut b = Harness::start("", &["file:///tmp/app.wasm"]);
    for moves in ["page.find-next", "page.find-previous"] {
        let effects = b.apply(command(moves));
        assert_eq!(
            notices(&effects),
            ["Nothing to find: page.find <text> first."]
        );
        assert!(finds(&effects).is_empty());
    }
    assert!(b.apply(command("page.find-stop")).is_empty());
    // Only a chrome can make a longer one; the core refuses it.
    let long = BrowserEvent::Command(Command::PageFind(Some("a".repeat(MAX_FIND + 1))));
    let effects = b.apply(long);
    assert_eq!(
        notices(&effects),
        ["page.find searches for at most 1024 bytes of text."]
    );
    assert!(finds(&effects).is_empty());
    // Whitespace is not searched for: it ends the search.
    let search = started(&b.apply(command("page.find a")));
    let blank = BrowserEvent::Command(Command::PageFind(Some("  \t ".into())));
    let effects = b.apply(blank);
    assert_eq!(
        finds(&effects),
        [(b.shown(), &FindCommand::Stop { search })]
    );
    assert!(notices(&effects).is_empty());
    b.apply(command("tab.new"));
    for find in ["page.find hello", "page.find"] {
        assert_eq!(
            notices(&b.apply(command(find))),
            ["This tab shows no page to find in."]
        );
    }
}

#[test]
fn history_and_fullscreen_say_what_is_missing() {
    let mut b = Harness::start("", &["https://a.test/"]);
    // history.clear asks the executable to forget, which answers (tests/history.rs).
    let effects = b.apply(command("history.clear"));
    assert!(notices(&effects).is_empty());
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, Effect::ForgetVisits { host: None, .. })));
    assert_eq!(
        notices(&b.apply(command("browser.fullscreen"))),
        ["browser.fullscreen is not available in this frontend."]
    );
    assert_eq!(
        notices(&b.apply(command("page.exit-fullscreen"))),
        ["The page shown is not in fullscreen."]
    );
}

#[test]
fn quitting_saves_the_session_first() {
    let mut b = Harness::start("", &[]);
    let effects = b.apply(command("browser.quit"));
    let tail: Vec<_> = effects.iter().rev().take(2).collect();
    assert!(
        matches!(tail[..], [Effect::Quit, Effect::SaveSession]),
        "{effects:?}"
    );
}

#[test]
fn zoom_belongs_to_the_tab_and_follows_it_across_engines() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let servo = b.shown();
    let effects = b.apply(command("page.zoom-in"));
    assert_eq!(commands(&effects, servo), [&EngineCommand::SetZoom(1.1)]);
    assert_eq!(b.active().zoom, 1.1);
    let effects = b.apply(command("page.open file:///tmp/app.wasm"));
    let (_, engine, options) = opened(&effects)[0];
    assert_eq!(
        (engine, options.zoom, options.visible),
        ("wasm", 1.1, false)
    );
    b.apply(command("page.zoom-reset"));
    assert_eq!(b.active().zoom, 1.0);
}

// ── Navigation and history ──────────────────────────────────────────────────────────────────

#[test]
fn a_page_stays_until_its_replacement_commits() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let effects = b.update(command("page.open https://b.test/"));
    let (_, url) = navigation(&effects, view);
    assert_eq!(url, "https://b.test/");
    let tab = b.active();
    assert_eq!(
        tab.url,
        Some("https://a.test/"),
        "the address is what committed"
    );
    assert_eq!(tab.requested, Some("https://b.test/"));
    assert!(tab.loading);
    for effect in &effects {
        for event in b.answer(effect) {
            b.update(event);
        }
    }
    assert_eq!(b.active().url, Some("https://b.test/"));
    assert_eq!(b.active().requested, None);
    assert_eq!(b.shown(), view, "same engine, same view");
}

#[test]
fn an_engine_switch_keeps_the_tab_and_rejects_the_old_view() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let tab = b.active().id;
    let servo = b.shown();
    let effects = b.update(command("page.open file:///tmp/app.wasm"));
    let (wasm, _, _) = opened(&effects)[0];
    let (navigation, url) = navigation(&effects, wasm);
    // Ready, but nothing committed: the old page stays.
    b.engine(EngineEvent::ViewReady { view: wasm });
    assert_eq!(b.shown(), servo);
    assert_eq!(b.active().requested, Some("file:///tmp/app.wasm"));
    let document = b.document(wasm);
    let effects = b.engine(EngineEvent::NavigationCommitted {
        view: wasm,
        navigation,
        url,
        document,
        history: None,
    });
    assert_eq!(b.active().id, tab);
    assert_eq!(b.shown(), wasm);
    assert_eq!(
        commands(&effects, servo),
        [
            &EngineCommand::SetFocus(false),
            &EngineCommand::SetVisibility(false)
        ]
    );
    assert_eq!(
        commands(&effects, wasm),
        [
            &EngineCommand::SetVisibility(true),
            &EngineCommand::SetFocus(true)
        ]
    );
    // The old view is kept for going back, but what it says doesn't reach the tab.
    let title = EngineEvent::TitleChanged {
        view: servo,
        title: Some("Old page".into()),
    };
    b.engine(title);
    assert_eq!(b.active().title, "app.wasm");
    let (_, effects) = b.request(servo, Service::Permission(Permission::Camera));
    assert!(matches!(
        answers(&effects, servo)[..],
        [(_, Err(ServiceError::Denied))]
    ));
    b.click(servo, PointerButton::Left, ButtonState::Pressed);
    let popup = EngineEvent::OpenRequested {
        view: servo,
        document: b.document(servo),
        url: "https://popup.test/".into(),
        disposition: OpenDisposition::NewView,
    };
    b.engine(popup);
    assert_eq!(b.state.tabs().len(), 1);
}

#[test]
fn same_document_history_is_the_engines_not_url_guesses() {
    let mut b = Harness::start("", &["sig://home"]);
    b.apply(command("page.open https://a.test/"));
    let view = b.shown();
    let document = b.document(view);
    // The page moves between same-document entries itself, back to a URL it had before.
    let urls = ["https://a.test/", "https://a.test/#x", "https://a.test/"];
    for (i, url) in urls.iter().enumerate().skip(1) {
        b.engine(EngineEvent::NavigationCommitted {
            view,
            navigation: NavigationId::next(),
            url: url.to_string(),
            document,
            history: None,
        });
        b.engine(EngineEvent::HistoryChanged {
            view,
            entries: urls[..=i].iter().map(|u| u.to_string()).collect(),
            current: i,
        });
    }
    // Back goes through the engine's entries, however alike their URLs are.
    for current in [1, 0] {
        let effects = b.update(command("page.back"));
        assert!(matches!(
            commands(&effects, view)[..],
            [EngineCommand::Traverse { delta: -1, .. }]
        ));
        b.engine(EngineEvent::HistoryChanged {
            view,
            entries: urls.iter().map(|u| u.to_string()).collect(),
            current,
        });
        assert_eq!(b.shown(), view);
    }
    // Only at the engine's first entry does back leave the segment.
    let effects = b.apply(command("page.back"));
    assert!(commands(&effects, view).contains(&&EngineCommand::SetVisibility(false)));
    assert_eq!(b.active().content, Content::Internal("sig://home"));
    assert!(b.active().can_go_forward);
}

#[test]
fn back_and_forward_cross_retained_segments() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let servo = b.shown();
    b.apply(command("page.open https://b.test/"));
    b.apply(command("page.open https://c.test/"));
    b.apply(command("page.back"));
    assert_eq!(b.active().url, Some("https://b.test/"));
    // Leave servo at b, with c ahead of it in servo's own history.
    b.apply(command("page.open file:///tmp/app.wasm"));
    let wasm = b.shown();
    assert_ne!(wasm, servo);

    // Back: the retained servo view, at b, shown at once without loading anything.
    let effects = b.apply(command("page.back"));
    assert!(opened(&effects).is_empty() && closed(&effects).is_empty());
    assert!(!commands(&effects, servo)
        .iter()
        .any(|c| matches!(c, EngineCommand::Navigate { .. })));
    assert_eq!(b.shown(), servo);
    assert_eq!(b.active().url, Some("https://b.test/"));
    // Back again goes through servo's own history.
    let effects = b.apply(command("page.back"));
    assert!(matches!(
        commands(&effects, servo)[0],
        EngineCommand::Traverse { delta: -1, .. }
    ));
    assert_eq!(b.active().url, Some("https://a.test/"));
    // Forward to b in servo, then on to the app: c was left behind when the app opened.
    b.apply(command("page.forward"));
    assert_eq!(b.active().url, Some("https://b.test/"));
    let effects = b.apply(command("page.forward"));
    assert!(commands(&effects, servo)
        .iter()
        .all(|c| !matches!(c, EngineCommand::Traverse { .. })));
    assert_eq!(b.shown(), wasm);
    assert!(!b.active().can_go_forward);

    // A new navigation after going back discards what came after, once it commits.
    b.apply(command("page.back"));
    let effects = b.update(command("page.open https://d.test/"));
    assert!(
        closed(&effects).is_empty(),
        "nothing is discarded before the commit"
    );
    let effects = b.apply(command("page.open https://d.test/"));
    assert_eq!(closed(&effects), [wasm]);
    assert!(!b.active().can_go_forward);
}

#[test]
fn internal_pages_are_segments_of_their_own() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let servo = b.shown();
    let effects = b.apply(command("browser.home"));
    assert!(commands(&effects, servo)
        .iter()
        .all(|c| !matches!(c, EngineCommand::Navigate { .. })));
    assert_eq!(b.active().content, Content::Internal("sig://home"));
    // Going on from the home page to the same engine opens a new view after it.
    let effects = b.apply(command("page.open https://b.test/"));
    assert_eq!(opened(&effects).len(), 1);
    b.apply(command("page.back"));
    b.apply(command("page.back"));
    assert_eq!(b.shown(), servo);
    assert_eq!(b.active().url, Some("https://a.test/"));
}

#[test]
fn old_segments_become_records_that_reload() {
    let mut b = Harness::start("", &["https://a.test/1"]);
    b.apply(command("page.open https://a.test/2"));
    let first = b.shown();
    b.engine(EngineEvent::TitleChanged {
        view: first,
        title: Some("Two".into()),
    });
    // Alternate engines so every step is a segment with a view of its own.
    let urls = [
        "file:///tmp/1.wasm",
        "https://b.test/",
        "file:///tmp/2.wasm",
    ];
    let mut closed_views = Vec::new();
    for url in urls {
        closed_views.extend(closed(&b.apply(command(&format!("page.open {url}")))));
    }
    assert_eq!(
        closed_views,
        [first],
        "only {RETAINED_VIEWS} views are kept besides the shown one"
    );
    assert!(b.state.view(first).is_none());
    for _ in 0..urls.len() - 1 {
        b.apply(command("page.back"));
    }
    // The next step back is the record: loaded again in a new view, which the tab says.
    let effects = b.update(command("page.back"));
    let (view, engine, _) = opened(&effects)[0];
    assert_eq!(engine, "servo");
    assert_eq!(navigation(&effects, view).1, "https://a.test/2");
    let tab = b.active();
    assert_eq!(
        (tab.url, tab.requested),
        (Some("file:///tmp/1.wasm"), Some("https://a.test/2"))
    );
    for effect in &effects {
        for event in b.answer(effect) {
            b.update(event);
        }
    }
    assert_eq!(b.shown(), view);
    assert_eq!(b.active().url, Some("https://a.test/2"));
    // The record's earlier entry is still there, as a record of its own.
    let effects = b.apply(command("page.back"));
    let (older, _, _) = opened(&effects)[0];
    assert_eq!(navigation(&effects, older).1, "https://a.test/1");
}

#[test]
fn a_view_with_a_process_of_its_own_is_kept_alone() {
    let mut env = environment();
    let process = EngineCapabilities {
        process_per_view: true,
        ..env.engines["servo"]
    };
    env.engines.insert("wpe".into(), process);
    let mut b = Harness::new("");
    let config = "version = 1\n[routing.wpe]\nhosts = [\"w.test\"]";
    b.state = BrowserState::new(Config::parse(Some(config), &env).unwrap(), env);
    b.apply(BrowserEvent::Start {
        session: None,
        open: vec![open("https://a.test/")],
    });
    let servo = b.shown();
    b.apply(command("page.open https://w.test/"));
    let wpe = b.shown();
    assert_eq!(shown_engine(&b), "wpe");
    // Kept alone: the older Servo page becomes a record when the tab moves on.
    let effects = b.apply(command("page.open https://b.test/"));
    assert_eq!(closed(&effects), [servo]);
    assert!(b.state.view(wpe).is_some());
    // Back shows the WebKit page at once; back again loads the record.
    let effects = b.apply(command("page.back"));
    assert_eq!((b.shown(), opened(&effects).len()), (wpe, 0));
    let effects = b.apply(command("page.back"));
    assert_eq!(opened(&effects)[0].1, "servo");
    assert_eq!(b.active().url, Some("https://a.test/"));
}

#[test]
fn failed_loads_are_not_failed_engines() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let servo = b.shown();
    // A new engine's view fails to load: the old page stays, with the error.
    let effects = b.update(command("page.open file:///tmp/broken.wasm"));
    let (wasm, _, _) = opened(&effects)[0];
    let (navigation, _) = navigation(&effects, wasm);
    let effects = b.engine(EngineEvent::NavigationFailed {
        view: wasm,
        navigation,
        error: NavigationError::Load("not a Wasm module".into()),
    });
    assert_eq!(closed(&effects), [wasm]);
    assert_eq!(b.shown(), servo);
    let error = b.active().error.unwrap().clone();
    assert_eq!(error.url.as_deref(), Some("file:///tmp/broken.wasm"));
    assert_eq!(error.message, "not a Wasm module");
    // Reloading tries the failed address again.
    let effects = b.update(command("page.reload"));
    assert_eq!(opened(&effects)[0].1, "wasm");
    assert!(b.active().error.is_none());

    // The shown page's document crashes: the view stays, and reloading recovers it. The
    // document's status text goes with it, so the status strip says why.
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    b.engine(EngineEvent::StatusTextChanged {
        view,
        text: Some("typed".into()),
    });
    b.engine(EngineEvent::ViewFailed {
        view,
        message: "script crashed".into(),
    });
    assert_eq!(b.state.view(view).unwrap().state(), ViewState::Failed);
    assert_eq!(b.active().error.unwrap().url, None);
    assert_eq!(b.active().status, None);
    let effects = b.apply(command("page.reload"));
    assert!(matches!(
        commands(&effects, view)[0],
        EngineCommand::Reload { .. }
    ));
    assert_eq!(b.state.view(view).unwrap().state(), ViewState::LiveVisible);
    assert!(b.active().error.is_none());
}

#[test]
fn stale_and_superseded_navigations_are_ignored() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let first = navigation(&b.update(command("page.open https://b.test/")), view).0;
    let second = navigation(&b.update(command("page.open https://c.test/")), view).0;
    // The first was superseded by the second: its failure means nothing now.
    b.engine(EngineEvent::NavigationFailed {
        view,
        navigation: first,
        error: NavigationError::Load("gone".into()),
    });
    assert!(b.active().error.is_none());
    assert_eq!(b.active().requested, Some("https://c.test/"));
    let document = DocumentGeneration::next();
    b.engine(EngineEvent::NavigationCommitted {
        view,
        navigation: second,
        url: "https://c.test/".into(),
        document,
        history: None,
    });
    assert_eq!(b.active().url, Some("https://c.test/"));
    b.engine(EngineEvent::NavigationFailed {
        view,
        navigation: second,
        error: NavigationError::Load("too late".into()),
    });
    assert!(b.active().error.is_none());
}

/// A tab whose history is servo (a, b, c) and then a Wasm app, back at servo's b.
fn back_from_an_app() -> (Harness, ViewId, ViewId) {
    let mut b = Harness::start("", &["https://a.test/"]);
    let servo = b.shown();
    b.apply(command("page.open https://b.test/"));
    b.apply(command("page.open https://c.test/"));
    b.apply(command("page.back"));
    b.apply(command("page.open file:///tmp/app.wasm"));
    let wasm = b.shown();
    b.apply(command("page.back"));
    assert_eq!(
        (b.shown(), b.active().url),
        (servo, Some("https://b.test/"))
    );
    (b, servo, wasm)
}

#[test]
fn quick_traversals_are_known_when_they_commit() {
    let (mut b, _, wasm) = back_from_an_app();
    // A held back key repeats before the engine answers: both traversals go out, and the
    // engine carries out both (the second finds no entry before the first).
    let mut effects = b.update(command("page.back"));
    effects.extend(b.update(command("page.back")));
    let mut after = Vec::new();
    for effect in &effects {
        for event in b.answer(effect) {
            after.extend(b.update(event));
        }
    }
    assert_eq!(closed(&after), [], "a traversal discards nothing");
    assert_eq!(b.active().url, Some("https://a.test/"));
    assert!(b.active().can_go_forward);
    assert!(b.state.view(wasm).is_some());
}

#[test]
fn only_a_new_document_the_page_opened_discards_later_segments() {
    let (mut b, servo, wasm) = back_from_an_app();
    let document = b.document(servo);
    // `replaceState`, a fragment, the page's own traversal: the same document commits.
    let effects = b.engine(EngineEvent::NavigationCommitted {
        view: servo,
        navigation: NavigationId::next(),
        url: "https://b.test/#x".into(),
        document,
        history: None,
    });
    assert_eq!(closed(&effects), []);
    assert_eq!(b.active().url, Some("https://b.test/#x"));
    assert!(b.active().can_go_forward);
    // A link to another document is a new navigation: what came after it is gone.
    let effects = b.engine(EngineEvent::NavigationCommitted {
        view: servo,
        navigation: NavigationId::next(),
        url: "https://d.test/".into(),
        document: DocumentGeneration::next(),
        history: None,
    });
    assert_eq!(closed(&effects), [wasm]);
    let urls = ["https://a.test/", "https://b.test/#x", "https://d.test/"];
    b.engine(EngineEvent::HistoryChanged {
        view: servo,
        entries: urls.iter().map(|u| u.to_string()).collect(),
        current: 2,
    });
    assert!(!b.active().can_go_forward);
}

#[test]
fn what_the_engine_says_a_page_commit_did_decides_what_it_discards() {
    // The page's own `history.back()` into another document keeps what came after.
    let (mut b, servo, wasm) = back_from_an_app();
    let effects = b.engine(EngineEvent::NavigationCommitted {
        view: servo,
        navigation: NavigationId::next(),
        url: "https://a.test/".into(),
        document: DocumentGeneration::next(),
        history: Some(HistoryEffect::Traverse),
    });
    assert_eq!(closed(&effects), []);
    assert!(b.state.view(wasm).is_some());
    // Its `pushState` is a new entry: what came after it is gone.
    let document = b.document(servo);
    let effects = b.engine(EngineEvent::NavigationCommitted {
        view: servo,
        navigation: NavigationId::next(),
        url: "https://a.test/p".into(),
        document,
        history: Some(HistoryEffect::Push),
    });
    assert_eq!(closed(&effects), [wasm]);
}

#[test]
fn a_new_view_whose_navigation_is_cancelled_is_given_up() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let servo = b.shown();
    let effects = b.update(command("page.open file:///tmp/app.wasm"));
    let (wasm, _, _) = opened(&effects)[0];
    let (navigation, _) = navigation(&effects, wasm);
    b.engine(EngineEvent::ViewReady { view: wasm });
    // The engine stops it itself, say because what it found is a download.
    let effects = b.engine(EngineEvent::NavigationFailed {
        view: wasm,
        navigation,
        error: NavigationError::Cancelled,
    });
    assert_eq!(closed(&effects), [wasm]);
    let tab = b.active();
    assert!(!tab.loading && tab.error.is_none());
    assert_eq!(b.shown(), servo);
}

#[test]
fn a_record_that_stops_loading_waits_to_be_reloaded() {
    let session = Harness::start("", &["https://a.test/"]).state.session();
    for stopped_by_the_user in [true, false] {
        let mut b = Harness::new("");
        let effects = b.update(BrowserEvent::Start {
            session: Some(session.clone()),
            open: Vec::new(),
        });
        let (view, _, _) = opened(&effects)[0];
        let (navigation, _) = navigation(&effects, view);
        let effects = match stopped_by_the_user {
            true => b.update(command("page.stop")),
            false => b.engine(EngineEvent::NavigationFailed {
                view,
                navigation,
                error: NavigationError::Cancelled,
            }),
        };
        assert_eq!(closed(&effects), [view]);
        assert!(
            opened(&effects).is_empty(),
            "it doesn't load again by itself"
        );
        let tab = b.active();
        assert!(tab.reloads && !tab.loading);
        assert_eq!(tab.error.unwrap().message, "Loading stopped.");
        let effects = b.update(command("page.reload"));
        assert_eq!(opened(&effects).len(), 1);
    }
    // Back from a record that is still loading leaves it.
    let mut b = Harness::start("", &["sig://home"]);
    b.apply(command("page.open https://a.test/"));
    let mut restored = Harness::new("");
    let effects = restored.update(BrowserEvent::Start {
        session: Some(b.state.session()),
        open: Vec::new(),
    });
    let (view, _, _) = opened(&effects)[0];
    let effects = restored.update(command("page.back"));
    assert_eq!(closed(&effects), [view]);
    assert!(opened(&effects).is_empty());
    assert_eq!(restored.active().content, Content::Internal("sig://home"));
    assert!(restored.active().can_go_forward);
}

#[test]
fn engine_capabilities_choose_what_the_core_does() {
    let mut b = Harness::start("[routing.demo]\nschemes = [\"demo\"]", &["https://a.test/"]);
    let servo = b.shown();
    b.apply(command("page.zoom-in"));
    // The demo engine can neither zoom nor traverse its history.
    let effects = b.apply(command("page.open demo://one"));
    let (demo, _, options) = opened(&effects)[0];
    assert_eq!(options.zoom, 1.0);
    assert!(!b.active().can_zoom);
    let effects = b.apply(command("page.zoom-in"));
    assert_eq!(commands(&effects, demo), Vec::<&EngineCommand>::new());
    assert_eq!(b.active().zoom, 1.25, "the tab keeps its zoom");
    // So each of its pages gets a view of its own...
    let effects = b.apply(command("page.open demo://two"));
    assert_eq!(opened(&effects).len(), 1);
    b.apply(command("page.back"));
    assert_eq!(b.shown(), demo);
    // ...and back never asks it to traverse, whatever history it reports.
    b.engine(EngineEvent::HistoryChanged {
        view: demo,
        entries: vec!["demo://zero".into(), "demo://one".into()],
        current: 1,
    });
    let effects = b.apply(command("page.back"));
    assert!(commands(&effects, demo)
        .iter()
        .all(|c| !matches!(c, EngineCommand::Traverse { .. })));
    assert_eq!(b.shown(), servo);
    assert!(b.active().can_zoom);
    assert!(commands(&effects, servo).contains(&&EngineCommand::SetZoom(1.25)));
}

#[test]
fn routing_needs_no_network_and_respects_the_configuration() {
    let mut b = Harness::start(
        "[engines]\nenabled = [\"servo\"]",
        &["https://a.test/app.wasm"],
    );
    assert_eq!(b.state.view(b.shown()).unwrap().engine(), "servo");
    b.apply(command("page.open file:///tmp/app.wasm"));
    let error = &b.active().error.unwrap().message;
    assert!(error.contains("not enabled"), "{error}");
    b.apply(command("page.open ftp://a.test/"));
    assert!(b
        .active()
        .error
        .unwrap()
        .message
        .contains("no engine opens ftp"));

    // An explicit choice, from the command line.
    let mut b = Harness::start("", &[]);
    let request = OpenRequest {
        input: "https://a.test/app.wasm".into(),
        engine: Some("wasm".into()),
    };
    let effects = b.apply(BrowserEvent::Open(request));
    assert_eq!(opened(&effects)[0].1, "wasm");
    assert_eq!(b.state.tabs().len(), 2);

    // An engine this build doesn't have.
    let mut b = Harness::new("");
    b.state = BrowserState::new(
        Config::default(),
        Environment {
            engines: engines(&["demo"]),
            ..Environment::default()
        },
    );
    b.apply(BrowserEvent::Start {
        session: None,
        open: vec![open("https://a.test/")],
    });
    assert!(b
        .active()
        .error
        .unwrap()
        .message
        .contains("not part of this build"));
}

// ── Late events ─────────────────────────────────────────────────────────────────────────────

/// Every kind of event a view can report, for `view`.
fn every_event(view: ViewId, document: DocumentGeneration) -> Vec<EngineEvent> {
    let request = ServiceRequest {
        id: RequestId::next(),
        view,
        document,
        origin: None,
        service: Service::Permission(Permission::Microphone),
    };
    vec![
        EngineEvent::ViewReady { view },
        EngineEvent::NavigationStarted {
            view,
            navigation: NavigationId::next(),
            url: "https://late.test/".into(),
        },
        EngineEvent::NavigationCommitted {
            view,
            navigation: NavigationId::next(),
            url: "https://late.test/".into(),
            document: DocumentGeneration::next(),
            history: None,
        },
        EngineEvent::NavigationFailed {
            view,
            navigation: NavigationId::next(),
            error: NavigationError::Load("late".into()),
        },
        EngineEvent::LoadingChanged {
            view,
            loading: true,
        },
        EngineEvent::HistoryChanged {
            view,
            entries: vec!["https://late.test/".into()],
            current: 0,
        },
        EngineEvent::TitleChanged {
            view,
            title: Some("Late".into()),
        },
        EngineEvent::StatusTextChanged {
            view,
            text: Some("late".into()),
        },
        EngineEvent::CursorChanged {
            view,
            cursor: Cursor::Wait,
        },
        EngineEvent::TextInputChanged { view, state: None },
        EngineEvent::FrameReady { view },
        EngineEvent::ServiceRequested(request),
        EngineEvent::ServiceCancelled {
            view,
            request: RequestId::next(),
        },
        EngineEvent::NavigationRequested {
            view,
            document,
            request: RequestId::next(),
            url: "https://late.test/".into(),
        },
        EngineEvent::OpenRequested {
            view,
            document,
            url: "https://late.test/".into(),
            disposition: OpenDisposition::CurrentView,
        },
        EngineEvent::CloseRequested { view, document },
        EngineEvent::ViewFailed {
            view,
            message: "late".into(),
        },
        EngineEvent::ViewClosed { view },
    ]
}

#[test]
fn a_closed_tab_ignores_late_events() {
    let mut b = Harness::start("", &["sig://home", "https://a.test/"]);
    b.apply(command("tab.select 2"));
    let view = b.shown();
    let document = b.document(view);
    let (request, effects) = b.request(view, Service::Permission(Permission::Camera));
    assert!(effects.iter().any(|e| matches!(e, Effect::Prompt { .. })));
    // Closing the tab closes its view and drops what it asked for.
    let effects = b.update(command("tab.close"));
    assert_eq!(closed(&effects), [view]);
    assert!(effects.contains(&Effect::CancelService(request)));
    let before: Vec<_> = b.state.tabs().map(|t| format!("{t:?}")).collect();
    // Everything the view says from now on is late, its closure included.
    for event in every_event(view, document) {
        let effects = b.engine(event.clone());
        assert_eq!(effects, [], "{event:?}");
    }
    assert_eq!(
        b.update(BrowserEvent::PermissionAnswered {
            request,
            allow: true
        }),
        []
    );
    let done = BrowserEvent::ServiceDone {
        request,
        result: Ok(ServiceReply::Done),
    };
    assert_eq!(b.update(done), []);
    let after: Vec<_> = b.state.tabs().map(|t| format!("{t:?}")).collect();
    assert_eq!(before, after);
    assert!(
        b.state.view(view).is_none(),
        "gone once its engine said it closed"
    );
}

#[test]
fn a_replaced_view_is_forgotten() {
    // The view that loaded the first page is evicted from the history; later its engine still
    // reports things about it.
    let mut b = Harness::start("", &["https://a.test/"]);
    let old = b.shown();
    let document = b.document(old);
    for url in [
        "file:///tmp/1.wasm",
        "https://b.test/",
        "file:///tmp/2.wasm",
    ] {
        b.apply(command(&format!("page.open {url}")));
    }
    assert!(b.state.view(old).is_none());
    let before: Vec<_> = b.state.tabs().map(|t| format!("{t:?}")).collect();
    for event in every_event(old, document) {
        assert_eq!(b.engine(event.clone()), [], "{event:?}");
    }
    let after: Vec<_> = b.state.tabs().map(|t| format!("{t:?}")).collect();
    assert_eq!(before, after);
    // Nor does another engine get to speak for a view.
    let wasm = b.shown();
    let event = BrowserEvent::Engine {
        engine: "servo".into(),
        event: EngineEvent::TitleChanged {
            view: wasm,
            title: Some("Impostor".into()),
        },
        at: b.now,
    };
    b.update(event);
    assert_eq!(b.active().title, "2.wasm");
}

// ── Policy ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn permissions_belong_to_one_document() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let (request, effects) = b.request(view, Service::Permission(Permission::Camera));
    let prompt = Effect::Prompt {
        request,
        tab: b.active().id,
        origin: Some("https://a.test".into()),
        permission: Permission::Camera,
    };
    assert_eq!(effects, [prompt]);
    let effects = b.update(BrowserEvent::PermissionAnswered {
        request,
        allow: true,
    });
    assert_eq!(answers(&effects, view), [(request, Ok(ServiceReply::Done))]);
    // The same document asks again: it has it.
    let (again, effects) = b.request(view, Service::Permission(Permission::Camera));
    assert_eq!(answers(&effects, view), [(again, Ok(ServiceReply::Done))]);
    // A fragment navigation keeps the document, and the grant.
    b.apply(command("page.open https://a.test/#part"));
    let (_, effects) = b.request(view, Service::Permission(Permission::Camera));
    assert!(answers(&effects, view)[0].1.is_ok());
    // A new document, even of the same origin, starts without it.
    b.apply(command("page.open https://a.test/other"));
    let (_, effects) = b.request(view, Service::Permission(Permission::Camera));
    assert!(matches!(effects[..], [Effect::Prompt { .. }]));
}

#[test]
fn a_request_without_an_origin_is_always_asked_about() {
    // Servo can't say which frame asks: a third-party frame of the page could be.
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let ask = |b: &mut Harness| {
        let request = ServiceRequest {
            id: RequestId::next(),
            view,
            document: b.document(view),
            origin: None,
            service: Service::Permission(Permission::Camera),
        };
        let id = request.id;
        (id, b.engine(EngineEvent::ServiceRequested(request)))
    };
    let (first, effects) = ask(&mut b);
    assert!(matches!(effects[..], [Effect::Prompt { origin: None, .. }]));
    let (second, _) = ask(&mut b);
    let effects = b.update(BrowserEvent::PermissionAnswered {
        request: first,
        allow: true,
    });
    // The grant covers the request answered, not the other one waiting, nor a later one.
    assert_eq!(answers(&effects, view), [(first, Ok(ServiceReply::Done))]);
    assert!(!effects.contains(&Effect::CancelService(second)));
    let (_, effects) = ask(&mut b);
    assert!(matches!(effects[..], [Effect::Prompt { .. }]));
    // A refusal holds for the document, and for the page's origin in the tab.
    let effects = b.update(BrowserEvent::PermissionAnswered {
        request: second,
        allow: false,
    });
    assert!(answers(&effects, view).contains(&(second, Err(ServiceError::Denied))));
    let (_, effects) = ask(&mut b);
    assert!(matches!(
        answers(&effects, view)[..],
        [(_, Err(ServiceError::Denied))]
    ));
    b.apply(command("page.reload"));
    let (_, effects) = ask(&mut b);
    assert!(matches!(
        answers(&effects, view)[..],
        [(_, Err(ServiceError::Denied))]
    ));
}

#[test]
fn an_answer_for_a_replaced_document_is_dropped() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let (request, _) = b.request(view, Service::Permission(Permission::Geolocation));
    // The page navigates while the prompt shows: the request is cancelled, both ways.
    let effects = b.apply(command("page.open https://b.test/"));
    assert!(effects.contains(&Effect::CancelService(request)));
    assert_eq!(
        answers(&effects, view),
        [(request, Err(ServiceError::Cancelled))]
    );
    // The user answers the prompt anyway: it applies to nothing.
    let effects = b.update(BrowserEvent::PermissionAnswered {
        request,
        allow: true,
    });
    assert_eq!(effects, []);
    let (_, effects) = b.request(view, Service::Permission(Permission::Geolocation));
    assert!(
        matches!(effects[..], [Effect::Prompt { .. }]),
        "not granted: {effects:?}"
    );
    // A request made for a document that is already gone is cancelled.
    let stale = ServiceRequest {
        id: RequestId::next(),
        view,
        document: DocumentGeneration::next(),
        origin: None,
        service: Service::Permission(Permission::Camera),
    };
    let id = stale.id;
    let effects = b.engine(EngineEvent::ServiceRequested(stale));
    assert_eq!(
        answers(&effects, view),
        [(id, Err(ServiceError::Cancelled))]
    );
}

#[test]
fn a_refusal_is_remembered_for_the_origin() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let (first, _) = b.request(view, Service::Permission(Permission::Microphone));
    let (second, effects) = b.request(view, Service::Permission(Permission::Microphone));
    assert!(matches!(effects[..], [Effect::Prompt { .. }]));
    let effects = b.update(BrowserEvent::PermissionAnswered {
        request: first,
        allow: false,
    });
    // One answer settles both requests of the document; the other prompt goes away.
    assert!(effects.contains(&Effect::CancelService(second)));
    let denied: Vec<_> = answers(&effects, view)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(denied, [first, second]);
    // A new document of the same origin in the tab is refused without asking.
    b.apply(command("page.reload"));
    let (_, effects) = b.request(view, Service::Permission(Permission::Microphone));
    assert!(matches!(
        answers(&effects, view)[..],
        [(_, Err(ServiceError::Denied))]
    ));
}

#[test]
fn services_need_activation_and_fail_closed() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let read = || Service::ClipboardRead {
        selection: Selection::Clipboard,
        mime_types: vec!["text/plain".into()],
        limit: 1 << 20,
    };
    let (_, effects) = b.request(view, read());
    assert!(matches!(
        answers(&effects, view)[..],
        [(_, Err(ServiceError::Denied))]
    ));
    // Ctrl+V in the page: a key press routed to it.
    b.key(press(47, "v", CTRL));
    let (request, effects) = b.request(view, read());
    assert!(matches!(&effects[..], [Effect::Service { request: r, .. }] if *r == request));
    // Reading used the activation up.
    let (_, effects) = b.request(view, read());
    assert!(matches!(
        answers(&effects, view)[..],
        [(_, Err(ServiceError::Denied))]
    ));
    // The executable's answer reaches the page.
    let data = ClipboardData {
        mime_type: "text/plain".into(),
        data: b"hi".to_vec(),
    };
    let effects = b.update(BrowserEvent::ServiceDone {
        request,
        result: Ok(ServiceReply::Clipboard(data)),
    });
    assert_eq!(answers(&effects, view).len(), 1);
    // Writing needs activation but doesn't use it up; the activation lasts five seconds.
    b.click(view, PointerButton::Left, ButtonState::Pressed);
    let write = || Service::ClipboardWrite {
        selection: Selection::Primary,
        items: Vec::new(),
    };
    for _ in 0..2 {
        let (_, effects) = b.request(view, write());
        assert!(matches!(effects[..], [Effect::Service { .. }]));
    }
    b.tick(Duration::from_secs(5));
    let (_, effects) = b.request(view, write());
    assert!(matches!(
        answers(&effects, view)[..],
        [(_, Err(ServiceError::Denied))]
    ));
    // Scripts are never external URLs.
    b.click(view, PointerButton::Left, ButtonState::Pressed);
    let (_, effects) = b.request(
        view,
        Service::OpenExternal {
            url: "javascript:x".into(),
        },
    );
    assert!(matches!(
        answers(&effects, view)[..],
        [(_, Err(ServiceError::Denied))]
    ));
    // A page can't queue requests without end.
    let mut last = Vec::new();
    for _ in 0..20 {
        last = b
            .request(view, Service::Permission(Permission::ScreenCapture))
            .1;
    }
    assert!(matches!(
        answers(&last, view)[..],
        [(_, Err(ServiceError::Denied))]
    ));
}

#[test]
fn page_downloads_are_throttled() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let download = || Service::Download {
        source: DownloadSource::Url("https://a.test/x.zip".into()),
        suggested_name: None,
    };
    let allowed = |effects: &[Effect]| matches!(effects, [Effect::Service { .. }]);
    assert!(allowed(&b.request(view, download()).1));
    b.tick(Duration::from_millis(500));
    assert!(!allowed(&b.request(view, download()).1));
    b.tick(Duration::from_millis(500));
    assert!(allowed(&b.request(view, download()).1));
}

#[test]
fn pages_open_tabs_only_after_a_click_or_key() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let popup = |b: &Harness, url: &str| EngineEvent::OpenRequested {
        view,
        document: b.document(view),
        url: url.into(),
        disposition: OpenDisposition::NewView,
    };
    let effects = b.engine(popup(&b, "https://popup.test/"));
    assert_eq!(notices(&effects).len(), 1);
    assert_eq!(b.state.tabs().len(), 1);
    // A click lets the page open one tab, in the background.
    b.click(view, PointerButton::Left, ButtonState::Pressed);
    b.tick(Duration::from_secs(1));
    let event = BrowserEvent::Engine {
        engine: "servo".into(),
        event: popup(&b, "https://popup.test/"),
        at: b.now,
    };
    b.apply(event);
    assert_eq!(b.state.tabs().len(), 2);
    assert_eq!(b.state.active(), 0);
    assert_eq!(b.tab(1).url, Some("https://popup.test/"));
    b.engine(popup(&b, "https://second.test/"));
    assert_eq!(b.state.tabs().len(), 2, "one click, one tab");
    // Too late.
    b.click(view, PointerButton::Left, ButtonState::Pressed);
    b.tick(Duration::from_secs(6));
    b.engine(popup(&b, "https://late.test/"));
    assert_eq!(b.state.tabs().len(), 2);
    // Never Sighurt's own pages, not even in the page's own tab.
    b.click(view, PointerButton::Left, ButtonState::Pressed);
    for disposition in [OpenDisposition::NewView, OpenDisposition::CurrentView] {
        let effects = b.engine(EngineEvent::OpenRequested {
            view,
            document: b.document(view),
            url: "sig://home".into(),
            disposition,
        });
        assert_eq!(notices(&effects).len(), 1);
    }
    assert_eq!(b.state.tabs().len(), 2);
    assert_eq!(b.active().url, Some("https://a.test/"));
    // A page of the tab it opened may close that tab; the user's own tab stays.
    b.apply(command("tab.select 2"));
    let popup_view = b.shown();
    b.apply(command("tab.select 1"));
    b.engine(EngineEvent::CloseRequested {
        view,
        document: b.document(view),
    });
    assert_eq!(b.state.tabs().len(), 2);
    b.apply(BrowserEvent::Engine {
        engine: "servo".into(),
        event: EngineEvent::CloseRequested {
            view: popup_view,
            document: b.document(popup_view),
        },
        at: b.now,
    });
    assert_eq!(b.state.tabs().len(), 1);
    // A tab its page closed can be reopened too.
    b.apply(command("tab.reopen"));
    assert_eq!(b.active().url, Some("https://popup.test/"));
}

#[test]
fn only_local_pages_open_local_files() {
    let open = |b: &Harness, view| EngineEvent::OpenRequested {
        view,
        document: b.document(view),
        url: "file:///tmp/app.wasm".into(),
        disposition: OpenDisposition::CurrentView,
    };
    let mut b = Harness::start("", &["https://a.test/"]);
    let event = open(&b, b.shown());
    let effects = b.engine(event);
    assert_eq!(notices(&effects).len(), 1);
    assert!(opened(&effects).is_empty());
    assert_eq!(b.active().requested, None);
    // A local app may open another.
    let mut b = Harness::start("", &["file:///tmp/index.wasm"]);
    let event = open(&b, b.shown());
    b.apply(BrowserEvent::Engine {
        engine: "wasm".into(),
        event,
        at: b.now,
    });
    assert_eq!(b.active().url, Some("file:///tmp/app.wasm"));
}

#[test]
fn a_page_opens_links_in_its_tab_through_the_router() {
    let mut b = Harness::start("", &["file:///tmp/index.wasm"]);
    let index = b.shown();
    let effects = b.apply(BrowserEvent::Engine {
        engine: "wasm".into(),
        event: EngineEvent::OpenRequested {
            view: index,
            document: b.document(index),
            url: "https://a.test/".into(),
            disposition: OpenDisposition::CurrentView,
        },
        at: b.now,
    });
    assert_eq!(opened(&effects)[0].1, "servo");
    assert_eq!(b.active().url, Some("https://a.test/"));
    assert!(b.active().can_go_back);
}

// ── Links and other navigations a page starts ───────────────────────────────────────────────

/// The page in `view` asks to navigate to `url` itself, and the fake engines carry out what
/// follows. Returns whether the core allowed it, and every effect; there is exactly one answer.
fn asks(b: &mut Harness, view: ViewId, url: &str) -> (bool, Vec<Effect>) {
    let document = b.document(view);
    asks_for(b, view, document, url)
}

/// [`asks`], for `document`.
fn asks_for(
    b: &mut Harness,
    view: ViewId,
    document: DocumentGeneration,
    url: &str,
) -> (bool, Vec<Effect>) {
    let request = RequestId::next();
    let event = BrowserEvent::Engine {
        engine: b.views[&view].engine.clone(),
        event: EngineEvent::NavigationRequested {
            view,
            document,
            request,
            url: url.into(),
        },
        at: b.now,
    };
    let effects = b.apply(event);
    let answers: Vec<bool> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Engine {
                view: to,
                command: EngineCommand::AnswerNavigation { request: r, allow },
            } if *to == view && *r == request => Some(*allow),
            _ => None,
        })
        .collect();
    assert_eq!(answers.len(), 1, "one answer for each request: {effects:?}");
    (answers[0], effects)
}

fn shown_engine(b: &Harness) -> &str {
    b.state.view(b.shown()).unwrap().engine()
}

#[test]
fn a_clicked_link_to_a_host_another_engine_claims_opens_there() {
    let mut b = Harness::start(
        "[routing.wasm]\nhosts = [\"*.apps.test\"]",
        &["https://a.test/"],
    );
    let servo = b.shown();
    b.click_link(servo, "https://x.apps.test/");
    b.tick(Duration::from_millis(4900));
    let (allowed, effects) = asks(&mut b, servo, "https://x.apps.test/");
    assert!(!allowed, "the page's engine doesn't load it");
    let (wasm, engine, _) = opened(&effects)[0];
    assert_eq!(
        (engine, navigation(&effects, wasm).1.as_str()),
        ("wasm", "https://x.apps.test/")
    );
    assert_eq!(b.shown(), wasm, "it replaced the page once it committed");
    assert!(!b.state.view(wasm).unwrap().chosen());
    // Back returns to the page, kept.
    let effects = b.apply(command("page.back"));
    assert_eq!((b.shown(), opened(&effects).len()), (servo, 0));
    // The click was used up: the same link asked about again stays in the page's engine.
    let (allowed, effects) = asks(&mut b, servo, "https://x.apps.test/");
    assert!(allowed && opened(&effects).is_empty());
    // The same address written differently is the same link.
    b.click_link(servo, "HTTPS://X.Apps.Test:443/");
    let (allowed, effects) = asks(&mut b, servo, "https://x.apps.test/");
    assert!(!allowed);
    assert_eq!(opened(&effects)[0].1, "wasm");
}

#[test]
fn only_the_link_the_user_clicked_leaves_the_engine() {
    let mut b = Harness::start(
        "[routing.wasm]\nhosts = [\"apps.test\"]",
        &["https://a.test/"],
    );
    let servo = b.shown();
    // A click on another link, or on no link, lets no frame or script take the tab elsewhere.
    b.click_link(servo, "https://a.test/next");
    let (allowed, effects) = asks(&mut b, servo, "https://apps.test/");
    assert!(allowed && opened(&effects).is_empty());
    b.engine(EngineEvent::StatusTextChanged {
        view: servo,
        text: None,
    });
    b.click(servo, PointerButton::Left, ButtonState::Pressed);
    assert!(asks(&mut b, servo, "https://apps.test/").0);
    // A key press (Enter on a focused link) names no link: what follows stays too.
    b.click_link(servo, "https://apps.test/");
    b.key(press(28, "Enter", Modifiers::default()));
    let (allowed, effects) = asks(&mut b, servo, "https://apps.test/");
    assert!(allowed && opened(&effects).is_empty());
    assert_eq!(shown_engine(&b), "servo");
}

#[test]
fn what_a_page_does_without_a_click_or_key_stays_in_its_engine() {
    let mut b = Harness::start(
        "[routing.wasm]\nhosts = [\"apps.test\"]",
        &["https://a.test/"],
    );
    let servo = b.shown();
    // A script's redirect, a frame loading: no click at all.
    let (allowed, effects) = asks(&mut b, servo, "https://apps.test/");
    assert!(allowed && opened(&effects).is_empty());
    // A click longer ago than 5 s.
    b.click_link(servo, "https://apps.test/");
    b.tick(Duration::from_secs(5));
    assert!(asks(&mut b, servo, "https://apps.test/").0);
    // What the page's own engine opens is allowed, and leaves the click for the link.
    b.click_link(servo, "https://apps.test/");
    let (allowed, effects) = asks(&mut b, servo, "https://b.test/");
    assert!(allowed && opened(&effects).is_empty());
    assert!(!asks(&mut b, servo, "https://apps.test/").0);
    assert_eq!(shown_engine(&b), "wasm");
}

#[test]
fn a_chosen_views_links_stay_in_its_engine_until_an_address_is_typed() {
    let mut b = Harness::start(
        "[routing.wasm]\nhosts = [\"apps.test\"]",
        &["https://a.test/"],
    );
    b.apply(command("page.open-in servo https://b.test/"));
    let servo = b.shown();
    assert!(b.state.view(servo).unwrap().chosen());
    b.click_link(servo, "https://apps.test/");
    let (allowed, effects) = asks(&mut b, servo, "https://apps.test/");
    assert!(allowed && opened(&effects).is_empty());
    // Typed, an address unchooses the view: the next link the user clicks leaves.
    b.apply(command("page.open https://c.test/"));
    assert_eq!(b.shown(), servo);
    b.click_link(servo, "https://apps.test/");
    let (allowed, effects) = asks(&mut b, servo, "https://apps.test/");
    assert!(!allowed);
    assert_eq!(opened(&effects)[0].1, "wasm");
}

/// The semantic action `action` on the page in `view`, chosen in `document`'s snapshot.
fn act(
    b: &mut Harness,
    view: ViewId,
    document: DocumentGeneration,
    action: SemanticAction,
    link: Option<&str>,
) -> Vec<Effect> {
    let event = BrowserEvent::Semantic {
        view,
        document,
        action,
        link: link.map(Into::into),
        at: b.now,
    };
    b.apply(event)
}

fn sent_semantic(effects: &[Effect]) -> Vec<(DocumentGeneration, SemanticAction)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Engine {
                command: EngineCommand::Semantic { document, action },
                ..
            } => Some((*document, action.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_link_activated_in_text_is_a_click_on_it() {
    let mut b = Harness::start(
        "[routing.wasm]\nhosts = [\"apps.test\"]",
        &["https://a.test/"],
    );
    let servo = b.shown();
    let document = b.document(servo);
    let effects = act(
        &mut b,
        servo,
        document,
        SemanticAction::Activate(NodeId(7)),
        Some("https://apps.test/"),
    );
    assert_eq!(
        sent_semantic(&effects),
        [(document, SemanticAction::Activate(NodeId(7)))]
    );
    // The engine asks about the link it follows: it goes where routing sends it.
    let (allowed, effects) = asks(&mut b, servo, "https://apps.test/");
    assert!(!allowed);
    assert_eq!(opened(&effects)[0].1, "wasm");
}

#[test]
fn what_a_frontend_does_on_its_own_is_no_click() {
    let mut b = Harness::start(
        "[routing.wasm]\nhosts = [\"apps.test\"]",
        &["https://a.test/"],
    );
    let servo = b.shown();
    let document = b.document(servo);
    // Focus and scrolling go to the engine, but count as no activation and name no link.
    for action in [
        SemanticAction::Focus(NodeId(7)),
        SemanticAction::ScrollIntoView(NodeId(7)),
    ] {
        let effects = act(
            &mut b,
            servo,
            document,
            action.clone(),
            Some("https://apps.test/"),
        );
        assert_eq!(sent_semantic(&effects), [(document, action)]);
    }
    let (allowed, effects) = asks(&mut b, servo, "https://apps.test/");
    assert!(allowed && opened(&effects).is_empty());
    // A form action is the user's activation, but the link is only an activated link's.
    act(
        &mut b,
        servo,
        document,
        SemanticAction::Submit(NodeId(8)),
        Some("https://apps.test/"),
    );
    let (allowed, effects) = asks(&mut b, servo, "https://apps.test/");
    assert!(allowed && opened(&effects).is_empty());
}

#[test]
fn an_action_for_a_replaced_document_or_a_hidden_view_is_dropped() {
    let mut b = Harness::start("", &["https://a.test/", "https://b.test/"]);
    let first = b.shown();
    let old = b.document(first);
    b.apply(command("page.open https://a.test/next"));
    assert_eq!(b.shown(), first);
    assert_ne!(b.document(first), old, "a new document");
    let effects = act(
        &mut b,
        first,
        old,
        SemanticAction::Activate(NodeId(1)),
        None,
    );
    assert!(
        sent_semantic(&effects).is_empty(),
        "the snapshot's document is gone"
    );
    let current = b.document(first);
    b.apply(command("tab.next"));
    let effects = act(
        &mut b,
        first,
        current,
        SemanticAction::Activate(NodeId(1)),
        None,
    );
    assert!(sent_semantic(&effects).is_empty(), "the view isn't shown");
}

#[test]
fn a_link_to_a_host_no_rule_claims_returns_to_the_default_engine() {
    let mut b = Harness::start(
        "[routing.wasm]\nhosts = [\"apps.test\"]",
        &["https://apps.test/"],
    );
    let wasm = b.shown();
    assert_eq!(shown_engine(&b), "wasm", "a host rule put the page there");
    b.click_link(wasm, "https://a.test/");
    let (allowed, effects) = asks(&mut b, wasm, "https://a.test/");
    assert!(!allowed);
    assert_eq!(opened(&effects)[0].1, "servo");
    assert_eq!(shown_engine(&b), "servo");
}

#[test]
fn a_page_cannot_cancel_or_replace_the_users_switch() {
    let config = "[routing.wasm]\nhosts = [\"b.test\"]";
    // Completes the switch the user started with `effects`.
    let finish = |b: &mut Harness, effects: &[Effect]| {
        for effect in effects {
            for event in b.answer(effect) {
                b.apply(event);
            }
        }
    };
    for by_link in [false, true] {
        let mut b = Harness::start(config, &["https://a.test/"]);
        let servo = b.shown();
        b.click_link(servo, "https://b.test/evil");
        let typed = b.update(command("page.open https://b.test/bank"));
        let (wasm, engine, _) = opened(&typed)[0];
        assert_eq!(engine, "wasm");
        // While the user's page loads, the old page sends the tab elsewhere: by a link the
        // user clicked a moment ago, or by itself (Servo hands over other schemes this way).
        let effects = if by_link {
            let (allowed, effects) = asks(&mut b, servo, "https://b.test/evil");
            assert!(!allowed, "the page that is being left goes nowhere");
            effects
        } else {
            b.engine(EngineEvent::OpenRequested {
                view: servo,
                document: b.document(servo),
                url: "x-trap:1".into(),
                disposition: OpenDisposition::CurrentView,
            })
        };
        assert!(opened(&effects).is_empty() && closed(&effects).is_empty());
        assert!(b.active().error.is_none());
        finish(&mut b, &typed);
        assert_eq!(b.shown(), wasm);
        assert_eq!(b.active().url, Some("https://b.test/bank"));
    }
}

#[test]
fn only_the_page_a_tab_shows_may_navigate() {
    let mut b = Harness::start(
        "[routing.wasm]\nhosts = [\"apps.test\"]",
        &["https://a.test/", "https://bg.test/"],
    );
    let view = b.shown();
    let old = b.document(view);
    b.apply(command("page.open https://b.test/"));
    assert_ne!(b.document(view), old);
    assert!(
        !asks_for(&mut b, view, old, "https://c.test/").0,
        "a replaced document"
    );
    // The page moves to another engine; kept in the tab's history, it may not navigate.
    b.click_link(view, "https://apps.test/");
    asks(&mut b, view, "https://apps.test/");
    assert_eq!(shown_engine(&b), "wasm");
    assert!(
        !asks(&mut b, view, "https://d.test/").0,
        "a page in the history"
    );
    // A background tab's page may.
    let Content::View(background) = b.tab(1).content else {
        panic!("the second tab shows its page");
    };
    assert!(asks(&mut b, background, "https://e.test/").0);
}

#[test]
fn an_engine_the_build_lacks_takes_neither_typed_addresses_nor_links() {
    // One file serves several builds: the rules of an engine this one lacks are left out.
    let mut b = Harness::start(
        "[routing.wpe]\nhosts = [\"wpe.test\"]",
        &["https://wpe.test/"],
    );
    assert!(b.active().error.is_none());
    assert_eq!(shown_engine(&b), "servo");
    let servo = b.shown();
    b.apply(command("page.open https://wpe.test/typed"));
    assert_eq!(
        (b.shown(), b.active().url),
        (servo, Some("https://wpe.test/typed"))
    );
    b.click_link(servo, "https://wpe.test/clicked");
    assert!(asks(&mut b, servo, "https://wpe.test/clicked").0);
    // Only the user's explicit choice names it, and is told why it can't.
    b.apply(command("page.open-in wpe https://wpe.test/"));
    let error = b.active().error.unwrap().message.clone();
    assert_eq!(error, "The wpe engine is not part of this build of sig.");
}

#[test]
fn links_routing_cant_place_stay_and_what_pages_may_not_open_is_denied() {
    let mut b = Harness::start(
        "[engines]\nenabled = [\"servo\", \"demo\", \"wpe\"]\n\
         [routing.wasm]\nhosts = [\"apps.test\"]\n\
         [routing.wpe]\nhosts = [\"wpe.test\"]\n\
         [routing.demo]\nschemes = [\"demo\"]\nhosts = [\"demo.test\"]",
        &["https://a.test/"],
    );
    let servo = b.shown();
    // A disabled engine, one the build lacks, schemes no engine opens, the page's own engine
    // by scheme, and an address too long to keep: the page's engine loads them.
    let long = format!("https://a.test/{}", "x".repeat(MAX_URL));
    let urls = [
        "https://apps.test/",
        "https://wpe.test/",
        "javascript:void(0)",
        "blob:https://a.test/1",
        "data:text/html,x",
        long.as_str(),
    ];
    for url in urls {
        b.click_link(servo, url);
        let (allowed, effects) = asks(&mut b, servo, url);
        assert!(allowed && opened(&effects).is_empty(), "{url:.40}");
        assert!(b.active().error.is_none(), "{url:.40}");
    }
    // What no page may open is denied, with a word for the user.
    for url in ["sig://home", "file:///etc/passwd"] {
        b.click_link(servo, url);
        let (allowed, effects) = asks(&mut b, servo, url);
        assert!(!allowed && opened(&effects).is_empty(), "{url}");
        assert_eq!(notices(&effects).len(), 1, "{url}");
    }
    // None of it used the click up: the page may still open a tab.
    let popup = EngineEvent::OpenRequested {
        view: servo,
        document: b.document(servo),
        url: "https://popup.test/".into(),
        disposition: OpenDisposition::NewView,
    };
    b.apply(BrowserEvent::Engine {
        engine: "servo".into(),
        event: popup,
        at: b.now,
    });
    assert_eq!(b.state.tabs().len(), 2);
}

// ── Engines and runtimes failing ────────────────────────────────────────────────────────────

#[test]
fn a_failed_runtime_takes_its_views_and_nothing_else() {
    let mut b = Harness::start("", &["https://a.test/", "file:///tmp/app.wasm"]);
    let servo = b.shown();
    let effects = b.update(BrowserEvent::Engine {
        engine: "wasm".into(),
        event: EngineEvent::RuntimeFailed {
            message: "trap handler".into(),
        },
        at: b.now,
    });
    assert_eq!(closed(&effects).len(), 1);
    assert_eq!(notices(&effects).len(), 1);
    assert!(b.tab(1).error.unwrap().message.contains("stopped working"));
    assert_eq!(b.shown(), servo);
    // Wasm opens nothing more; the web still works.
    b.apply(command("page.open file:///tmp/other.wasm"));
    assert!(b.active().error.unwrap().message.contains("Restart sig"));
    b.apply(command("page.open https://b.test/"));
    assert_eq!(b.active().url, Some("https://b.test/"));
}

#[test]
fn refused_opens_and_commands_are_reported() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let servo = b.shown();
    let effects = b.update(command("page.open file:///tmp/app.wasm"));
    let (wasm, _, _) = opened(&effects)[0];
    b.update(BrowserEvent::OpenFailed {
        view: wasm,
        error: EngineError::Runtime("no GPU".into()),
    });
    assert!(b.active().error.unwrap().message.contains("no GPU"));
    assert_eq!(b.shown(), servo);
    assert!(b.state.view(wasm).is_none());
    // A runtime that can't open views has failed as a whole.
    b.apply(command("page.open file:///tmp/other.wasm"));
    assert!(b.active().error.unwrap().message.contains("Restart sig"));

    let effects = b.update(BrowserEvent::CommandFailed {
        view: servo,
        command: EngineCommand::SetZoom(1.1),
        error: EngineError::Unsupported("page zoom"),
    });
    assert_eq!(notices(&effects).len(), 1);
    b.update(BrowserEvent::CommandFailed {
        view: servo,
        command: EngineCommand::Stop,
        error: EngineError::UnknownView(servo),
    });
    assert!(b.state.view(servo).is_none());
    assert_eq!(b.active().content, Content::Blank);
    assert!(b.active().reloads);
}

// ── Input ───────────────────────────────────────────────────────────────────────────────────

#[test]
fn bound_keys_never_reach_the_page_by_halves() {
    let mut b = Harness::start(
        "[keys]\n\"ctrl-l\" = \"location.focus\"",
        &["https://a.test/"],
    );
    let view = b.shown();
    let ctrl = press(29, "Control", CTRL);
    let effects = b.key(ctrl.clone());
    assert!(matches!(
        commands(&effects, view)[..],
        [EngineCommand::Input(_)]
    ));
    let ctrl_l = press(38, "l", CTRL);
    let effects = b.key(ctrl_l.clone());
    assert!(effects.contains(&Effect::Focus(KeyboardFocus::Chrome)));
    // The page gets no L, and loses focus, which releases the Ctrl it saw go down.
    assert_eq!(commands(&effects, view), [&EngineCommand::SetFocus(false)]);
    assert_eq!(b.state.focus(), KeyboardFocus::Chrome);
    assert_eq!(b.key(release(&ctrl_l)), []);
    let effects = b.key(release(&ctrl));
    assert!(matches!(effects[..], [Effect::ChromeInput(_)]));
    // A click in the page gives it the keyboard back.
    let effects = b.click(view, PointerButton::Left, ButtonState::Pressed);
    assert!(effects.contains(&Effect::Focus(KeyboardFocus::Page)));
    assert!(commands(&effects, view).contains(&&EngineCommand::SetFocus(true)));
}

#[test]
fn typing_keeps_its_characters_and_altgr_is_not_ctrl_alt() {
    let config = "[keys]\n\"j\" = \"tab.next\"\n\"ctrl-alt-2\" = \"tab.new\"";
    let mut b = Harness::start(config, &["https://a.test/", "sig://home"]);
    let view = b.shown();
    let j = press(36, "j", Modifiers::default());
    b.key(j.clone());
    assert_eq!(b.state.active(), 1, "not typing: j is bound");
    b.apply(command("tab.select 1"));
    // A text field of the page has focus: j types.
    b.engine(EngineEvent::TextInputChanged {
        view,
        state: Some(TextInputState {
            caret: SurfaceRect::default(),
            surrounding: None,
            purpose: TextPurpose::Normal,
            multiline: false,
        }),
    });
    let effects = b.key(j);
    assert!(matches!(
        commands(&effects, view)[..],
        [EngineCommand::Input(_)]
    ));
    assert_eq!(b.state.active(), 0);
    // AltGr + 2 types "@" on a Swedish layout.
    let altgr = Modifiers {
        alt_graph: true,
        ..Modifiers::default()
    };
    let at = KeyEvent {
        consumed: altgr,
        ..press(3, "@", altgr)
    };
    let effects = b.key(at);
    assert!(matches!(
        commands(&effects, view)[..],
        [EngineCommand::Input(_)]
    ));
    assert_eq!(b.state.tabs().len(), 2);
}

#[test]
fn a_key_held_before_its_modifier_stays_with_the_page() {
    let mut b = Harness::start("[keys]\n\"ctrl-r\" = \"page.reload\"", &["https://a.test/"]);
    let view = b.shown();
    let is_input = |effects: &[Effect]| {
        matches!(
            commands(effects, view)[..],
            [EngineCommand::Input(InputEvent {
                kind: InputKind::Key(_),
                ..
            })]
        )
    };
    let r = press(19, "r", Modifiers::default());
    assert!(is_input(&b.key(r.clone())));
    // Ctrl goes down while R repeats: the repeats and the release are still the page's.
    let repeat = KeyEvent {
        repeat: true,
        modifiers: CTRL,
        ..r
    };
    assert!(is_input(&b.key(repeat.clone())));
    let up = KeyEvent {
        repeat: false,
        ..release(&repeat)
    };
    assert!(is_input(&b.key(up)));
}

#[test]
fn window_activation_reaches_whoever_has_the_keyboard() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let deactivated = |b: &Harness| BrowserEvent::Keyboard {
        event: InputEvent {
            seat: b.seat,
            kind: InputKind::Focus(FocusEvent::Deactivated),
        },
        at: b.now,
    };
    let effects = b.update(deactivated(&b));
    assert!(matches!(
        commands(&effects, view)[..],
        [EngineCommand::Input(_)]
    ));
    b.apply(command("location.focus"));
    let effects = b.update(deactivated(&b));
    assert!(matches!(effects[..], [Effect::ChromeInput(_)]));
}

#[test]
fn the_mouse_back_button_goes_back() {
    let mut b = Harness::start("", &["https://a.test/"]);
    b.apply(command("page.open https://b.test/"));
    let view = b.shown();
    let effects = b.click(view, PointerButton::Back, ButtonState::Pressed);
    assert!(matches!(
        commands(&effects, view)[..],
        [EngineCommand::Traverse { delta: -1, .. }]
    ));
    assert_eq!(
        b.click(view, PointerButton::Back, ButtonState::Released),
        []
    );
}

#[test]
fn the_host_follows_the_shown_page() {
    let mut b = Harness::start("", &["https://a.test/", "https://b.test/"]);
    let first = b.shown();
    let effects = b.engine(EngineEvent::CursorChanged {
        view: first,
        cursor: Cursor::Pointer,
    });
    assert_eq!(effects, [Effect::PageCursor(Cursor::Pointer)]);
    let text = TextInputState {
        caret: SurfaceRect::default(),
        surrounding: None,
        purpose: TextPurpose::Url,
        multiline: false,
    };
    let effects = b.engine(EngineEvent::TextInputChanged {
        view: first,
        state: Some(text.clone()),
    });
    assert_eq!(effects, [Effect::PageTextInput(Some(text))]);
    // Another tab: its page's cursor, and no text field.
    let effects = b.apply(command("tab.next"));
    assert!(effects.contains(&Effect::PageCursor(Cursor::Default)));
    assert!(effects.contains(&Effect::PageTextInput(None)));
    // A hidden page's cursor changes nothing.
    assert_eq!(
        b.engine(EngineEvent::CursorChanged {
            view: first,
            cursor: Cursor::Wait,
        }),
        []
    );
}

// ── Configuration and session ───────────────────────────────────────────────────────────────

#[test]
fn an_invalid_reload_keeps_the_configuration_in_use() {
    let mut b = Harness::start("[keys]\n\"ctrl-t\" = \"tab.new\"", &["sig://home"]);
    let broken = "version = 1\n[keys]\n\"ctrl-t\" = \"tab.nope\"";
    let broken = Config::parse(Some(broken), &environment());
    let effects = b.update(config_reloaded(broken));
    let message = notices(&effects)[0];
    assert!(
        message.contains("tab.nope") && message.contains("stays"),
        "{message}"
    );
    b.key(press(20, "t", CTRL));
    assert_eq!(b.state.tabs().len(), 2, "the old bindings still work");

    let reloaded = "version = 1\n[keys]\n\"ctrl-n\" = \"tab.new\"";
    let reloaded = Config::parse(Some(reloaded), &environment());
    b.update(config_reloaded(reloaded));
    b.key(press(20, "t", CTRL));
    b.key(press(49, "n", CTRL));
    assert_eq!(b.state.tabs().len(), 3, "the new bindings apply at once");
}

#[test]
fn a_saved_session_comes_back_as_records() {
    let mut b = Harness::start("", &["https://a.test/", "sig://home"]);
    b.apply(command("page.open https://b.test/"));
    b.engine(EngineEvent::TitleChanged {
        view: b.shown(),
        title: Some("Bee".into()),
    });
    b.apply(command("page.zoom-in"));
    b.apply(command("tab.next"));
    let text = b.state.session().to_text();

    let session = Session::parse(&text).unwrap();
    let mut restored = Harness::new("");
    let effects = restored.update(BrowserEvent::Start {
        session: Some(session),
        open: Vec::new(),
    });
    assert_eq!(restored.state.tabs().len(), 2);
    assert_eq!(restored.state.active(), 1);
    assert_eq!(restored.active().content, Content::Internal("sig://home"));
    assert!(
        opened(&effects).is_empty(),
        "background tabs load when they are shown"
    );
    let first = restored.tab(0);
    assert!(first.reloads);
    assert_eq!(
        (first.title.as_ref(), first.url, first.zoom),
        ("Bee", Some("https://b.test/"), 1.1)
    );
    // Shown, it loads its page again in a new view.
    let effects = restored.apply(command("tab.select 1"));
    let (view, _, _) = opened(&effects)[0];
    assert_eq!(navigation(&effects, view).1, "https://b.test/");
    assert!(!restored.active().reloads);
    assert!(
        restored.active().can_go_back,
        "a.test is a record behind it"
    );
}

#[test]
fn a_damaged_session_is_refused() {
    let mut session = Harness::start("", &["https://a.test/"]).state.session();
    session.active = 7;
    let mut b = Harness::new("");
    let effects = b.apply(BrowserEvent::Start {
        session: Some(session),
        open: Vec::new(),
    });
    assert_eq!(notices(&effects).len(), 1);
    assert_eq!(b.state.tabs().len(), 1);
    assert_eq!(b.active().content, Content::Internal("sig://home"));
    b.apply(command("tab.new"));
}

#[test]
fn no_more_tabs_open_than_a_session_holds() {
    let mut b = Harness::start("", &[]);
    for _ in 1..MAX_TABS {
        b.update(command("tab.new"));
    }
    assert_eq!(b.state.tabs().len(), MAX_TABS);
    let refused = "sig holds at most 1000 tabs: close one to open another.";
    let effects = b.update(command("tab.new"));
    assert_eq!(notices(&effects), [refused]);
    let effects = b.update(BrowserEvent::Open(open("https://a.test/")));
    assert_eq!(notices(&effects), [refused]);
    assert!(opened(&effects).is_empty());
    assert_eq!(b.state.tabs().len(), MAX_TABS);
    assert_eq!(b.state.active(), MAX_TABS - 1);
    // What is saved is read back whole.
    let session = Session::parse(&b.state.session().to_text()).unwrap();
    assert_eq!((session.tabs.len(), session.notices.len()), (MAX_TABS, 0));
}

#[test]
fn what_reading_the_session_left_out_is_told() {
    let session = Harness::start("", &["https://a.test/"]).state.session();
    let text = format!(
        "{}\n[[tabs]]\ncurrent = 0\nzoom = \"big\"\n",
        session.to_text()
    );
    let mut b = Harness::new("");
    let effects = b.apply(BrowserEvent::Start {
        session: Some(Session::parse(&text).unwrap()),
        open: Vec::new(),
    });
    assert_eq!(
        notices(&effects),
        ["A saved tab was damaged and is left out."]
    );
    assert_eq!(b.state.tabs().len(), 1);
}

#[test]
fn a_tab_still_loading_its_first_page_is_saved_with_it() {
    let mut b = Harness::new("");
    b.update(BrowserEvent::Start {
        session: None,
        open: vec![open("https://slow.test/")],
    });
    let session = b.state.session();
    assert_eq!(session.tabs[0].segments[0].entries, ["https://slow.test/"]);
    let mut restored = Harness::new("");
    let effects = restored.update(BrowserEvent::Start {
        session: Some(Session::parse(&session.to_text()).unwrap()),
        open: Vec::new(),
    });
    let (view, _, _) = opened(&effects)[0];
    assert_eq!(navigation(&effects, view).1, "https://slow.test/");
}

#[test]
fn a_failed_page_says_so_when_it_is_shown_again() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let servo = b.shown();
    b.engine(EngineEvent::ViewFailed {
        view: servo,
        message: "crashed".into(),
    });
    b.apply(command("page.open file:///tmp/app.wasm"));
    assert!(b.active().error.is_none());
    b.apply(command("page.back"));
    assert_eq!(b.shown(), servo);
    assert_eq!(b.active().error.unwrap().message, "crashed");
}

// ── Keymaps, tabs by id and configuration notices ──────────────────────────────────────────

/// `script`, a keymap for these tests. `[keymap.script]` says what a press of a key that types
/// a character does: `"take"` takes a fresh press and passes its repeats, `"run <command>"`
/// runs the command, `"then <command>"` runs it and passes the press on, `"all <commands>"`
/// runs those commands (separated by `;`) in order, `"close-titled <title>"` closes the tab
/// whose own title it read, and `"copy-link"` copies the link the pointer is over in the
/// selected tab, or takes the key when there is none. It counts the presses it is asked about, and shows the count
/// as its indicator. What the core tells it goes into a log of the thread, so each test sees
/// its own.
mod script {
    use std::cell::RefCell;
    use std::num::NonZeroUsize;

    use sighurt_api::commands::{Command, CommandSet};
    use sighurt_api::input::{Key, KeyEvent};
    use sighurt_api::keymap::{Browser, KeyAction, KeyContext, Keymap, KeymapKind};
    use sighurt_api::settings::Table;

    thread_local! {
        static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// What the core told this thread's keymaps since the last call.
    pub fn log() -> Vec<String> {
        LOG.with(|log| log.take())
    }

    fn note(line: String) {
        LOG.with(|log| log.borrow_mut().push(line));
    }

    pub const KIND: KeymapKind = KeymapKind {
        name: "script",
        check,
        new: || Box::<Script>::default(),
    };

    enum Rule {
        Take,
        Run(Command),
        Then(Command),
        All(Vec<Command>),
        CloseTitled(String),
        CopyLink,
    }

    fn rule(text: &str, commands: &CommandSet<'_>) -> Result<Rule, String> {
        if let Some(rest) = text.strip_prefix("all ") {
            let mut found = Vec::new();
            for part in rest.split(';') {
                let part = part.trim();
                if part.is_empty() {
                    return Err("all: expected commands separated by ';'".into());
                }
                found.push(commands.parse(part)?);
            }
            return Ok(Rule::All(found));
        }
        if let Some(title) = text.strip_prefix("close-titled ") {
            return match title.is_empty() {
                true => Err("close-titled: expected a title".into()),
                false => Ok(Rule::CloseTitled(title.to_owned())),
            };
        }
        match text.split_once(' ') {
            None if text == "take" => Ok(Rule::Take),
            None if text == "copy-link" => Ok(Rule::CopyLink),
            Some(("run", command)) => commands.parse(command).map(Rule::Run),
            Some(("then", command)) => commands.parse(command).map(Rule::Then),
            _ => Err(format!(
                "{text:?}: expected take, run, then, all, close-titled or copy-link"
            )),
        }
    }

    /// `tab.close` of the tab whose own title is `title`, or a taken key when none has it.
    fn close_titled(browser: &dyn Browser, title: &str) -> KeyAction {
        let found = (0..browser.tab_count())
            .find(|&index| browser.tab(index).and_then(|tab| tab.title) == Some(title));
        match found {
            Some(index) => KeyAction::Run(Command::TabClose(Some(
                NonZeroUsize::new(index + 1).expect("tab numbers start at 1"),
            ))),
            None => KeyAction::Take,
        }
    }

    fn check(table: &Table, commands: &CommandSet<'_>) -> Vec<String> {
        let problem = |text: Option<&str>| match text {
            Some(text) => rule(text, commands).err(),
            None => Some("expected a string".into()),
        };
        let problems = table
            .iter()
            .map(|(key, value)| (key, problem(value.as_str())));
        problems
            .filter_map(|(key, problem)| problem.map(|p| format!("{key:?}: {p}")))
            .collect()
    }

    #[derive(Default)]
    struct Script {
        rules: Vec<(String, Rule)>,
        presses: usize,
        indicator: String,
    }

    impl Keymap for Script {
        fn configure(&mut self, settings: &Table) {
            note(format!("configure after {} presses", self.presses));
            let rule = |text: Option<&str>| rule(text.unwrap(), &CommandSet::ANY).unwrap();
            let rules = settings
                .iter()
                .map(|(key, text)| (key.clone(), rule(text.as_str())));
            self.rules = rules.collect();
        }

        fn press(
            &mut self,
            key: &KeyEvent,
            _context: KeyContext,
            browser: &dyn Browser,
        ) -> KeyAction {
            self.presses += 1;
            self.indicator = self.presses.to_string();
            let Key::Character(text) = &key.key else {
                return KeyAction::Pass;
            };
            match self.rules.iter().find(|(typed, _)| typed == text) {
                Some((_, Rule::Take)) if !key.repeat => KeyAction::Take,
                Some((_, Rule::Run(command))) => KeyAction::Run(command.clone()),
                Some((_, Rule::Then(command))) => KeyAction::RunThenPass(command.clone()),
                Some((_, Rule::All(commands))) => KeyAction::RunAll(commands.clone()),
                Some((_, Rule::CloseTitled(title))) => close_titled(browser, title),
                Some((_, Rule::CopyLink)) => {
                    let tab = browser.tab(browser.active_tab());
                    match tab.and_then(|tab| tab.link) {
                        Some(link) => KeyAction::Run(Command::ClipboardCopy(link.into())),
                        None => KeyAction::Take,
                    }
                }
                _ => KeyAction::Pass,
            }
        }

        fn context(&mut self, context: KeyContext) {
            note(format!("{:?} typing {}", context.focus, context.typing));
        }

        fn reset(&mut self) {
            note("reset".into());
        }

        fn indicator(&self) -> Option<&str> {
            (self.presses > 0).then_some(self.indicator.as_str())
        }
    }
}

fn typed(key: &str) -> KeyEvent {
    press(30, key, Modifiers::default())
}

/// Whether `effects` give the view or the chrome any input.
fn delivers(effects: &[Effect], view: ViewId) -> bool {
    let to_view = commands(effects, view)
        .into_iter()
        .any(|command| matches!(command, EngineCommand::Input(_)));
    to_view || effects.iter().any(|e| matches!(e, Effect::ChromeInput(_)))
}

#[test]
fn a_taken_press_takes_its_release_whatever_the_keymap_says() {
    let config = "[input]\nkeymap = \"script\"\n[keymap.script]\n\"t\" = \"take\"";
    let mut b = Harness::start(config, &["https://a.test/"]);
    let view = b.shown();
    // The keymap takes the press, then passes its repeat: the repeat and the release are
    // still taken.
    let t = typed("t");
    let repeat = KeyEvent {
        repeat: true,
        ..t.clone()
    };
    for event in [t.clone(), repeat, release(&t)] {
        let effects = b.key(event.clone());
        assert!(!delivers(&effects, view), "{event:?}: {effects:?}");
    }
    // A press it passes reaches the page, and so does its release.
    let u = typed("u");
    assert!(delivers(&b.key(u.clone()), view));
    assert!(delivers(&b.key(release(&u)), view));
}

#[test]
fn run_then_pass_delivers_to_the_new_focus() {
    let config = "[input]\nkeymap = \"script\"\n[keymap.script]\n\":\" = \"then location.focus\"";
    let mut b = Harness::start(config, &["https://a.test/"]);
    let view = b.shown();
    let colon = typed(":");
    let effects = b.key(colon.clone());
    // The command runs first, then the press goes where the keyboard is now: the chrome.
    let focus = effects
        .iter()
        .position(|e| *e == Effect::Focus(KeyboardFocus::Chrome));
    let input = effects.iter().position(|e| {
        matches!(e, Effect::ChromeInput(InputEvent { kind: InputKind::Key(key), .. }) if *key == colon)
    });
    assert!(focus.is_some() && focus < input, "{effects:?}");
    assert!(!commands(&effects, view)
        .iter()
        .any(|c| matches!(c, EngineCommand::Input(_))));
    // The press was passed, so its release follows it.
    let effects = b.key(release(&colon));
    assert!(
        matches!(effects[..], [Effect::ChromeInput(_)]),
        "{effects:?}"
    );
}

#[test]
fn the_keymap_hears_context_changes() {
    let mut b = Harness::start("[input]\nkeymap = \"script\"", &["https://a.test/"]);
    let view = b.shown();
    assert_eq!(
        script::log(),
        ["configure after 0 presses", "Page typing false"]
    );
    b.apply(command("location.focus"));
    assert_eq!(script::log(), ["Chrome typing true"]);
    b.apply(command("tab.select 1"));
    assert_eq!(script::log(), ["Page typing false"]);
    b.engine(EngineEvent::TextInputChanged {
        view,
        state: Some(TextInputState {
            caret: SurfaceRect::default(),
            surrounding: None,
            purpose: TextPurpose::Normal,
            multiline: false,
        }),
    });
    assert_eq!(script::log(), ["Page typing true"]);
    // Nothing changed: the keymap is told nothing.
    b.apply(command("page.zoom-in"));
    assert_eq!(script::log(), [] as [String; 0]);
    b.update(BrowserEvent::Keyboard {
        event: InputEvent {
            seat: b.seat,
            kind: InputKind::Focus(FocusEvent::Deactivated),
        },
        at: b.now,
    });
    assert_eq!(script::log(), ["reset"]);
}

#[test]
fn a_keymap_reads_the_tabs_it_decides_with() {
    let config = "[input]\nkeymap = \"script\"\n[keymap.script]\n\"x\" = \"close-titled Gone\"";
    let mut b = Harness::start(
        config,
        &["https://a.test/", "https://b.test/", "https://c.test/"],
    );
    let views: Vec<_> = (0..3)
        .map(|index| match b.tab(index).content {
            Content::View(view) => view,
            other => panic!("tab {index} shows {other:?}"),
        })
        .collect();
    for (view, title) in views.iter().zip(["Keep", "Gone", "Stay"]) {
        b.engine(EngineEvent::TitleChanged {
            view: *view,
            title: Some((*title).into()),
        });
    }
    b.key(typed("x"));
    assert_eq!(b.state.tabs().len(), 2);
    assert_eq!(b.tab(0).title, "Keep");
    assert_eq!(b.tab(1).title, "Stay");
}

#[test]
fn a_keymap_reads_the_hovered_link() {
    let config = "[input]\nkeymap = \"script\"\n[keymap.script]\n\"y\" = \"copy-link\"";
    let mut b = Harness::start(config, &["https://a.test/"]);
    let view = b.shown();
    let copied = |effects: &[Effect]| {
        effects.iter().find_map(|e| match e {
            Effect::ClipboardWrite(text) => Some(text.clone()),
            _ => None,
        })
    };
    assert_eq!(copied(&b.key(typed("y"))), None);
    let status = |b: &mut Harness, text: &str| {
        b.engine(EngineEvent::StatusTextChanged {
            view,
            text: Some(text.into()),
        });
    };
    // A link, as opening it would load it.
    status(&mut b, "HTTPS://A.test/link?x");
    assert_eq!(
        copied(&b.key(typed("y"))).as_deref(),
        Some("https://a.test/link?x")
    );
    // Status text that is not an http, https or file URL is no link, nor is a longer one.
    for text in [
        "Loading…",
        "javascript:alert(1)",
        "tab.close",
        "mailto:a@b.test",
        &format!("https://a.test/{}", "x".repeat(8 * 1024)),
    ] {
        status(&mut b, text);
        assert_eq!(copied(&b.key(typed("y"))), None, "{text}");
    }
    status(&mut b, "file:///tmp/a%20b.html");
    assert_eq!(
        copied(&b.key(typed("y"))).as_deref(),
        Some("file:///tmp/a%20b.html")
    );
    // A new document has no hovered link until the page says so.
    b.apply(command("page.open https://b.test/"));
    assert_eq!(copied(&b.key(typed("y"))), None);
}

#[test]
fn run_all_runs_in_order_in_one_update() {
    let config =
        "[input]\nkeymap = \"script\"\n[keymap.script]\n\"x\" = \"all tab.close 3; tab.close 2\"";
    let mut b = Harness::start(
        config,
        &[
            "https://a.test/",
            "https://b.test/",
            "https://c.test/",
            "https://d.test/",
        ],
    );
    // One update: closing 3 then 2 leaves the first and the last. The other order would
    // leave the first and the third.
    b.key(typed("x"));
    assert_eq!(b.state.tabs().len(), 2);
    assert_eq!(b.tab(0).url, Some("https://a.test/"));
    assert_eq!(b.tab(1).url, Some("https://d.test/"));
}

#[test]
fn a_run_all_shows_and_focuses_a_tab_before_sending_it_a_key() {
    // `tab.select 2; page.send-key enter; tab.select 1` in one press, against those commands
    // as three updates. RedrawChrome and SaveSession are once per update, so the comparison
    // is everything else: tab 2 is shown and focused before Enter, then hidden again.
    #[derive(Debug, PartialEq)]
    enum Step {
        Focus(KeyboardFocus),
        Engine(usize, EngineCommand),
        Key(usize, KeyEvent),
        Other(String),
    }

    fn tab_views(b: &Harness) -> Vec<ViewId> {
        (0..b.state.tabs().len())
            .map(|index| match b.tab(index).content {
                Content::View(view) => view,
                other => panic!("tab {index} shows {other:?}"),
            })
            .collect()
    }

    fn steps(effects: &[Effect], views: &[ViewId]) -> Vec<Step> {
        let tab_of = |view: ViewId| {
            views.iter().position(|id| *id == view).unwrap_or_else(|| {
                panic!("an effect for a view that is not a tab's current one: {view:?}")
            })
        };
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::RedrawChrome | Effect::SaveSession => None,
                Effect::Focus(focus) => Some(Step::Focus(*focus)),
                Effect::Engine {
                    view,
                    command:
                        EngineCommand::Input(InputEvent {
                            kind: InputKind::Key(key),
                            ..
                        }),
                } => Some(Step::Key(tab_of(*view), key.clone())),
                Effect::Engine { view, command } => {
                    Some(Step::Engine(tab_of(*view), command.clone()))
                }
                other => Some(Step::Other(format!("{other:?}"))),
            })
            .collect()
    }

    let urls = ["https://a.test/", "https://b.test/"];
    let config = "\
[input]
keymap = \"script\"
[keymap.script]
\"x\" = \"all tab.select 2; page.send-key enter; tab.select 1\"";
    let mut together = Harness::start(config, &urls);
    let together_views = tab_views(&together);
    let combined = together.key(typed("x"));

    let mut apart = Harness::start("", &urls);
    let apart_views = tab_views(&apart);
    let mut stepped = Vec::new();
    for text in ["tab.select 2", "page.send-key enter", "tab.select 1"] {
        stepped.extend(apart.update(command(text)));
    }

    let combined_steps = steps(&combined, &together_views);
    let stepped_steps = steps(&stepped, &apart_views);
    assert_eq!(
        combined_steps, stepped_steps,
        "one press should do what the commands do one by one"
    );

    let tab2 = |step: &Step| matches!(step, Step::Key(1, _));
    let keys: Vec<usize> = combined_steps
        .iter()
        .enumerate()
        .filter(|(_, step)| tab2(step))
        .map(|(index, _)| index)
        .collect();
    let find = |want: fn(&Step) -> bool| {
        combined_steps
            .iter()
            .position(want)
            .expect("tab 2's focus effects")
    };
    let show = find(|step| matches!(step, Step::Engine(1, EngineCommand::SetVisibility(true))));
    let focus = find(|step| matches!(step, Step::Engine(1, EngineCommand::SetFocus(true))));
    let hide = find(|step| matches!(step, Step::Engine(1, EngineCommand::SetVisibility(false))));
    let blur = find(|step| matches!(step, Step::Engine(1, EngineCommand::SetFocus(false))));
    assert_eq!(
        keys.len(),
        2,
        "press and release, to tab 2: {combined_steps:?}"
    );
    assert!(
        show < keys[0]
            && focus < keys[0]
            && keys[1] == keys[0] + 1
            && keys[1] < hide
            && keys[1] < blur,
        "tab 2 is shown and focused before Enter, and hidden after it: {combined_steps:?}"
    );
    assert_eq!(together.state.active(), apart.state.active());
    assert_eq!(
        together.state.view(together_views[0]).unwrap().state(),
        ViewState::LiveVisible
    );
    assert_eq!(
        together.state.view(together_views[1]).unwrap().state(),
        ViewState::LiveHidden
    );
}

#[test]
fn clipboard_copy_is_an_effect() {
    let mut b = Harness::start("", &["sig://home"]);
    let effects = b.update(command("clipboard.copy hello there"));
    assert!(effects.contains(&Effect::ClipboardWrite("hello there".into())));
}

#[test]
fn a_changed_indicator_redraws_the_chrome() {
    let mut b = Harness::start("[input]\nkeymap = \"script\"", &["https://a.test/"]);
    assert_eq!(b.state.keymap_indicator(), None);
    let effects = b.key(typed("a"));
    assert!(effects.contains(&Effect::RedrawChrome));
    assert_eq!(b.state.keymap_indicator(), Some("1"));
    // A release isn't the keymap's, and changes nothing the chrome shows.
    assert!(!b.key(release(&typed("a"))).contains(&Effect::RedrawChrome));
}

#[test]
fn a_reload_that_keeps_the_keymap_configures_it_and_keeps_its_state() {
    let script = |key| {
        format!("version = 1\n[input]\nkeymap = \"script\"\n[keymap.script]\n\"{key}\" = \"run tab.new\"")
    };
    let mut b = Harness::start(&script("x")[12..], &["sig://home"]);
    b.key(typed("x"));
    assert_eq!(b.state.tabs().len(), 2);
    script::log();
    let reloaded = Config::parse(Some(&script("y")), &environment());
    b.update(config_reloaded(reloaded));
    assert_eq!(script::log(), ["configure after 1 presses"]);
    b.key(typed("x"));
    b.key(typed("y"));
    assert_eq!(b.state.tabs().len(), 3, "the new table applies");
    assert_eq!(b.state.keymap_indicator(), Some("3"), "the count goes on");
}

#[test]
fn a_switch_replaces_the_keymap_and_a_held_keys_release_is_still_taken() {
    let config = "[input]\nkeymap = \"script\"\n[keymap.script]\n\"x\" = \"run page.reload\"";
    let mut b = Harness::start(config, &["https://a.test/"]);
    let view = b.shown();
    let x = typed("x");
    b.key(x.clone());
    assert_eq!(b.state.keymap_indicator(), Some("1"));
    // x is held while the configuration goes back to the bindings keymap.
    let bindings = Config::parse(
        Some("version = 1\n[keys]\n\"x\" = \"tab.new\""),
        &environment(),
    );
    let effects = b.update(config_reloaded(bindings));
    assert!(effects.contains(&Effect::RedrawChrome));
    assert_eq!(b.state.keymap_indicator(), None);
    assert!(
        !delivers(&b.key(release(&x)), view),
        "the release stays taken"
    );
    // The new keymap reads [keys].
    b.key(x);
    assert_eq!(b.state.tabs().len(), 2);
}

#[test]
fn closing_a_tab_by_id_after_the_list_shifted() {
    let mut b = Harness::start(
        "",
        &["https://a.test/", "https://b.test/", "https://c.test/"],
    );
    let ids: Vec<TabId> = b.state.tabs().map(|tab| tab.id).collect();
    // The chrome drew three tabs; the first closes before a click on the third's close button
    // arrives. By number, "tab 3" would now be no tab at all.
    b.apply(command("tab.close 1"));
    b.apply(BrowserEvent::CloseTab(ids[2]));
    let left: Vec<TabId> = b.state.tabs().map(|tab| tab.id).collect();
    assert_eq!(left, [ids[1]]);
}

#[test]
fn selecting_a_closed_tab_by_id_does_nothing() {
    let mut b = Harness::start(
        "",
        &["https://a.test/", "https://b.test/", "https://c.test/"],
    );
    let ids: Vec<TabId> = b.state.tabs().map(|tab| tab.id).collect();
    b.apply(command("tab.close 2"));
    assert_eq!(b.apply(BrowserEvent::SelectTab(ids[1])), []);
    assert_eq!(b.state.active(), 0);
    // An open tab is selected wherever it is now, as tab.select selects it.
    let effects = b.apply(BrowserEvent::SelectTab(ids[2]));
    assert_eq!((b.state.active(), b.active().id), (1, ids[2]));
    assert!(effects.contains(&Effect::Focus(KeyboardFocus::Page)));
}

#[test]
fn configuration_notices_are_shown_at_start_and_on_reload() {
    let mut b = Harness::new("[routing.wpe]\nschemes = [\"wpe-test\"]");
    let effects = b.apply(BrowserEvent::Start {
        session: None,
        open: Vec::new(),
    });
    let missing = "[routing.wpe] is for an engine this build doesn't have (it has: demo, servo, \
                   wasm); its rules are not used";
    assert_eq!(notices(&effects), [missing]);
    let reloaded = "version = 1\n[routing.wpe]\nschemes = [\"wpe-test\"]\n[keymap.script]";
    let effects = b.update(config_reloaded(Config::parse(
        Some(reloaded),
        &environment(),
    )));
    let unused = "[keymap.script] is not used: [input] keymap is \"bindings\", which reads [keys]";
    assert_eq!(notices(&effects), [format!("{missing}\n{unused}")]);
}

#[test]
fn a_reload_that_changes_the_chrome_says_it_applies_at_restart() {
    let mut b = Harness::start("", &["sig://home"]);
    let line = Config::parse(Some("version = 1\n[ui]\nchrome = \"line\""), &environment());
    let effects = b.update(config_reloaded(line));
    assert_eq!(
        notices(&effects),
        ["[ui] chrome = \"line\" applies when sig starts again; the standard chrome stays until \
          then."]
    );
    // Back to the chrome it runs: nothing to say.
    let back = Config::parse(Some("version = 1"), &environment());
    assert_eq!(notices(&b.update(config_reloaded(back))), [] as [&str; 0]);
}

#[test]
fn a_reload_that_changes_the_renderer_says_it_applies_at_restart() {
    let mut b = Harness::start("", &["sig://home"]);
    let cpu = Config::parse(
        Some(
            "version = 1
[ui]
renderer = \"cpu\"",
        ),
        &environment(),
    );
    let effects = b.update(config_reloaded(cpu));
    assert_eq!(
        notices(&effects),
        [
            "[ui] renderer = \"cpu\" applies when sig starts again; the chrome is drawn as [ui] \
          renderer = \"auto\" says until then."
        ]
    );
    let back = Config::parse(Some("version = 1"), &environment());
    assert_eq!(notices(&b.update(config_reloaded(back))), [] as [&str; 0]);
    // The terminal draws no chrome with it: nothing to say there.
    let env = Environment {
        frontend: sighurt_core::Frontend::Terminal,
        ..environment()
    };
    let config = Config::parse(Some("version = 1"), &env).unwrap();
    let mut state = BrowserState::new(config, env.clone());
    let vulkan = Config::parse(
        Some(
            "version = 1
[ui]
renderer = \"vulkan\"",
        ),
        &env,
    );
    let effects = update(&mut state, config_reloaded(vulkan));
    assert_eq!(notices(&effects), [] as [&str; 0]);
}

// ── Chrome commands, sent keys, side buttons and scrolling ─────────────────────────────────

const ALT: Modifiers = Modifiers {
    control: false,
    alt: true,
    ..CTRL
};

#[test]
fn chrome_commands_go_to_the_chrome() {
    let mut b = Harness::start(
        "[keys]\n\"ctrl-b\" = \"chrome.toggle-tabs\"",
        &["https://a.test/"],
    );
    let toggle = Effect::ChromeCommand("toggle-tabs".into());
    let effects = b.key(press(48, "b", CTRL));
    assert_eq!(
        effects,
        std::slice::from_ref(&toggle),
        "the keyboard stays where it is"
    );
    assert_eq!(b.update(command("chrome.toggle-tabs")), [toggle]);
    // Typed after a colon, an action the configuration's chrome lacks still goes on: the
    // executable knows the chrome that runs, and says so if it lacks it.
    assert_eq!(
        b.update(command("chrome.zoom")),
        [Effect::ChromeCommand("zoom".into())]
    );
}

/// The keys `effects` give `view`, in order: code, key and state.
fn keys_sent(effects: &[Effect], view: ViewId) -> Vec<(KeyCode, Key, ButtonState, Option<String>)> {
    commands(effects, view)
        .into_iter()
        .filter_map(|command| match command {
            EngineCommand::Input(InputEvent {
                kind: InputKind::Key(key),
                ..
            }) => Some((key.code, key.key.clone(), key.state, key.text.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_sent_key_is_pressed_and_released_in_the_page_back_to_back() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let effects = b.update(command("page.send-key end"));
    let end = |state| (KeyCode(107), Key::Named("End".into()), state, None);
    assert_eq!(
        keys_sent(&effects, view),
        [end(ButtonState::Pressed), end(ButtonState::Released)]
    );
    assert_eq!(
        effects.len(),
        2,
        "nothing between the press and its release: {effects:?}"
    );
    // Space types a space as it goes down.
    let effects = b.update(command("page.send-key shift-space"));
    let space = |state, text: Option<&str>| {
        let text = text.map(str::to_owned);
        (KeyCode(57), Key::Character(" ".into()), state, text)
    };
    assert_eq!(
        keys_sent(&effects, view),
        [
            space(ButtonState::Pressed, Some(" ")),
            space(ButtonState::Released, None)
        ]
    );
}

#[test]
fn a_sent_key_needs_the_page_to_have_the_keyboard() {
    let mut b = Harness::start(
        "[keys]\n\"alt-j\" = \"page.send-key down\"",
        &["https://a.test/", "sig://home"],
    );
    let view = b.shown();
    // The chrome has the keyboard: nothing reaches the page, nor the chrome.
    b.apply(command("location.focus"));
    let effects = b.update(command("page.send-key down"));
    assert_eq!(effects, []);
    let effects = b.key(press(36, "j", ALT));
    assert!(keys_sent(&effects, view).is_empty(), "{effects:?}");
    assert!(!effects.iter().any(|e| matches!(e, Effect::ChromeInput(_))));
    // The page has the keyboard, but the tab shows a page of Sighurt's own.
    b.apply(command("tab.select 2"));
    assert_eq!(b.state.focus(), KeyboardFocus::Page);
    assert_eq!(b.update(command("page.send-key down")), []);
    // Back to the web page: it reaches it.
    b.apply(command("tab.select 1"));
    assert_eq!(
        keys_sent(&b.update(command("page.send-key down")), view).len(),
        2
    );
}

#[test]
fn a_key_sent_by_a_key_press_is_the_users_activation() {
    let mut b = Harness::start(
        "[keys]\n\"alt-j\" = \"page.send-key down\"",
        &["https://a.test/"],
    );
    let view = b.shown();
    let document = b.document(view);
    let popup = |b: &mut Harness| {
        b.engine(EngineEvent::OpenRequested {
            view,
            document,
            url: "https://pop.test/".into(),
            disposition: OpenDisposition::NewView,
        })
    };
    // As a command (the command line, a button), no key was pressed in the page.
    b.update(command("page.send-key down"));
    assert!(notices(&popup(&mut b))[0].contains("Blocked a pop-up"));
    assert_eq!(b.state.tabs().len(), 1);
    // A key press ran it: the page may open one tab.
    let effects = b.key(press(36, "j", ALT));
    assert_eq!(keys_sent(&effects, view).len(), 2);
    popup(&mut b);
    assert_eq!(b.state.tabs().len(), 2);
}

#[test]
fn a_view_shows_when_a_key_press_activated_it_and_a_command_does_not() {
    let mut b = Harness::start(
        "[keys]\n\"alt-j\" = \"page.send-key down\"",
        &["https://a.test/"],
    );
    let view = b.shown();
    let activation = |b: &Harness| b.state.view(view).unwrap().activation();
    b.update(command("page.send-key down"));
    assert_eq!(activation(&b), None);
    b.key(press(36, "j", ALT));
    assert_eq!(activation(&b), Some(b.now));
}

/// A side button `button` going `state` over `view`, or over the chrome.
fn side(
    b: &mut Harness,
    view: Option<ViewId>,
    button: PointerButton,
    state: ButtonState,
) -> Vec<Effect> {
    let event = InputEvent {
        seat: b.seat,
        kind: InputKind::Pointer(PointerEvent::Button {
            button,
            state,
            position: SurfacePoint { x: 5.0, y: 5.0 },
            modifiers: Modifiers::default(),
        }),
    };
    let at = b.now;
    b.update(BrowserEvent::Button { view, event, at })
}

fn reload(b: &mut Harness, config: &str) {
    let config = Config::parse(Some(&format!("version = 1\n{config}")), &environment());
    b.update(config_reloaded(config));
}

/// Whether `effects` give `view` pointer input.
fn pointed(effects: &[Effect], view: ViewId) -> bool {
    commands(effects, view).into_iter().any(|command| {
        matches!(
            command,
            EngineCommand::Input(InputEvent {
                kind: InputKind::Pointer(_),
                ..
            })
        )
    })
}

#[test]
fn a_bound_buttons_release_stays_taken_after_a_reload_unbinds_it() {
    let mut b = Harness::start("", &["https://a.test/"]);
    b.apply(command("page.open https://b.test/"));
    let view = b.shown();
    let (back, pressed, released) = (
        PointerButton::Back,
        ButtonState::Pressed,
        ButtonState::Released,
    );
    let effects = side(&mut b, Some(view), back, pressed);
    assert!(matches!(
        commands(&effects, view)[..],
        [EngineCommand::Traverse { delta: -1, .. }]
    ));
    // Unbound while held: its release is still taken.
    reload(&mut b, "[input.buttons]\nback = \"\"");
    assert_eq!(side(&mut b, Some(view), back, released), []);
    // Unbound, the next press and its release reach the page, as any button's do.
    assert!(pointed(&side(&mut b, Some(view), back, pressed), view));
    assert!(pointed(&side(&mut b, Some(view), back, released), view));
    // A press the page got keeps its release there when the button is bound meanwhile.
    assert!(pointed(&side(&mut b, Some(view), back, pressed), view));
    reload(&mut b, "");
    assert!(pointed(&side(&mut b, Some(view), back, released), view));
}

#[test]
fn side_buttons_over_the_chrome_run_their_commands_or_go_nowhere() {
    let mut b = Harness::start(
        "[input.buttons]\nforward = \"tab.new\"",
        &["https://a.test/"],
    );
    let view = b.shown();
    let (forward, pressed, released) = (
        PointerButton::Forward,
        ButtonState::Pressed,
        ButtonState::Released,
    );
    b.apply(BrowserEvent::Button {
        view: None,
        event: InputEvent {
            seat: b.seat,
            kind: InputKind::Pointer(PointerEvent::Button {
                button: forward,
                state: pressed,
                position: SurfacePoint::default(),
                modifiers: Modifiers::default(),
            }),
        },
        at: b.now,
    });
    assert_eq!(
        b.state.tabs().len(),
        2,
        "forward = tab.new, over the chrome"
    );
    assert_eq!(side(&mut b, None, forward, released), []);
    // Unbound over the chrome: no page gets it.
    reload(&mut b, "[input.buttons]\nforward = \"\"");
    b.apply(command("tab.select 1"));
    let effects = side(&mut b, None, forward, pressed);
    assert!(
        !pointed(&effects, view) && b.state.tabs().len() == 2,
        "{effects:?}"
    );
    // Nor a page no longer shown.
    b.apply(command("tab.select 2"));
    assert!(!pointed(&side(&mut b, Some(view), forward, pressed), view));
}

#[test]
fn scrolling_goes_scroll_speed_times_as_far() {
    let scroll = |b: &mut Harness, view: ViewId, delta: ScrollDelta| {
        let event = InputEvent {
            seat: b.seat,
            kind: InputKind::Pointer(PointerEvent::Scroll {
                delta,
                position: SurfacePoint::default(),
                modifiers: Modifiers::default(),
            }),
        };
        let at = b.now;
        let effects = b.update(BrowserEvent::Pointer { view, event, at });
        match commands(&effects, view)[..] {
            [EngineCommand::Input(InputEvent {
                kind: InputKind::Pointer(PointerEvent::Scroll { delta, .. }),
                ..
            })] => *delta,
            _ => panic!("{effects:?}"),
        }
    };
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    let wheel = ScrollDelta::Wheel { x: -15, y: 120 };
    assert_eq!(scroll(&mut b, view, wheel), wheel, "1.0 leaves it as it is");
    reload(&mut b, "[input]\nscroll-speed = 2.5");
    assert_eq!(
        scroll(&mut b, view, ScrollDelta::Pixels { x: -4.0, y: 10.0 }),
        ScrollDelta::Pixels { x: -10.0, y: 25.0 }
    );
    assert_eq!(
        scroll(&mut b, view, wheel),
        ScrollDelta::Wheel { x: -38, y: 300 }
    );
    // Slower: a high-resolution wheel's smallest step still scrolls, and no step is made up.
    reload(&mut b, "[input]\nscroll-speed = 0.1");
    assert_eq!(
        scroll(&mut b, view, ScrollDelta::Wheel { x: 0, y: 4 }),
        ScrollDelta::Wheel { x: 0, y: 1 }
    );
    assert_eq!(
        scroll(&mut b, view, ScrollDelta::Wheel { x: -3, y: 120 }),
        ScrollDelta::Wheel { x: -1, y: 12 }
    );
    assert_eq!(
        scroll(&mut b, view, ScrollDelta::Pixels { x: 0.0, y: 10.0 }),
        ScrollDelta::Pixels { x: 0.0, y: 1.0 }
    );
}

// ── Zoom, keywords, page.open-in and engine settings ──────────────────────────────────────

#[test]
fn new_tabs_start_at_the_configured_zoom_and_reset_returns_to_it() {
    let mut b = Harness::new("zoom = 1.25");
    let effects = b.apply(BrowserEvent::Start {
        session: None,
        open: vec![open("https://a.test/")],
    });
    let servo = b.shown();
    assert_eq!(b.active().zoom, 1.25);
    assert_eq!(opened(&effects)[0].2.zoom, 1.25, "the view opens at it");
    b.apply(command("page.zoom-in"));
    assert_eq!(b.active().zoom, 1.5);
    let effects = b.apply(command("page.zoom-reset"));
    assert_eq!(b.active().zoom, 1.25);
    assert_eq!(commands(&effects, servo), [&EngineCommand::SetZoom(1.25)]);
    b.apply(command("tab.new"));
    assert_eq!(b.active().zoom, 1.25, "a new tab, on the home page");

    // A reload leaves the open tabs as they are; new tabs and resets take the new zoom.
    reload(&mut b, "zoom = 0.9");
    assert_eq!((b.tab(0).zoom, b.tab(1).zoom), (1.25, 1.25));
    b.apply(command("tab.new"));
    assert_eq!(b.active().zoom, 0.9);
    b.apply(command("tab.select 1"));
    b.apply(command("page.zoom-reset"));
    assert_eq!(b.active().zoom, 0.9);

    // A restored tab keeps the zoom it was saved with.
    b.apply(command("page.zoom-in"));
    let session = Session::parse(&b.state.session().to_text()).unwrap();
    let mut restored = Harness::new("zoom = 2.0");
    restored.apply(BrowserEvent::Start {
        session: Some(session),
        open: vec![open("https://b.test/")],
    });
    let zooms: Vec<f64> = restored.state.tabs().map(|tab| tab.zoom).collect();
    assert_eq!(zooms, [1.0, 1.25, 0.9, 2.0]);
}

#[test]
fn keywords_open_their_templates() {
    let mut b = Harness::start(
        "[keywords]\nw = \"https://w.test/?q={}\"",
        &["https://a.test/"],
    );
    let view = b.shown();
    let effects = b.apply(command("page.open w räksmörgås"));
    assert_eq!(
        navigation(&effects, view).1,
        "https://w.test/?q=r%C3%A4ksm%C3%B6rg%C3%A5s"
    );
    // They apply as soon as they are loaded.
    reload(&mut b, "[keywords]\nw = \"https://other.test/{}\"");
    let effects = b.apply(command("page.open w rust"));
    assert_eq!(navigation(&effects, b.shown()).1, "https://other.test/rust");
}

#[test]
fn open_in_reopens_the_page_in_the_engine_named_and_back_returns() {
    let mut b = Harness::start(
        "[routing.wasm]\norigins = [\"https://a.test\"]",
        &["https://a.test/"],
    );
    let servo = b.shown();
    assert!(!b.state.view(servo).unwrap().chosen());
    let effects = b.apply(command("page.open-in wasm"));
    let (wasm, engine, _) = opened(&effects)[0];
    assert_eq!(engine, "wasm");
    assert_eq!(navigation(&effects, wasm).1, "https://a.test/");
    assert_eq!(
        b.shown(),
        wasm,
        "the new view replaced the old once it committed"
    );
    assert!(b.state.view(wasm).unwrap().chosen());

    // Back returns to the page in the engine it was in, its view kept; forward to the other.
    let effects = b.apply(command("page.back"));
    assert_eq!((b.shown(), opened(&effects).len()), (servo, 0));
    b.apply(command("page.forward"));
    assert_eq!(b.shown(), wasm);

    // The page's own navigations leave the view chosen (routing sends this one to wasm).
    let effects = b.engine(EngineEvent::OpenRequested {
        view: wasm,
        document: b.document(wasm),
        url: "https://a.test/next.wasm".into(),
        disposition: OpenDisposition::CurrentView,
    });
    navigation(&effects, wasm);
    assert!(b.state.view(wasm).unwrap().chosen());
    // Typing an address the same engine opens makes it unchosen, in the same view...
    let effects = b.apply(command("page.open file:///tmp/app.wasm"));
    assert_eq!((opened(&effects).len(), b.shown()), (0, wasm));
    assert!(!b.state.view(wasm).unwrap().chosen());
    // ...and choosing the same engine again makes it chosen again.
    let effects = b.apply(command("page.open-in wasm file:///tmp/two.wasm"));
    assert_eq!(opened(&effects).len(), 0);
    assert_eq!(navigation(&effects, wasm).1, "file:///tmp/two.wasm");
    assert!(b.state.view(wasm).unwrap().chosen());

    // An address of its own, in an engine routing wouldn't pick for it.
    let effects = b.apply(command("page.open-in servo file:///tmp/app.wasm"));
    let (view, engine, _) = opened(&effects)[0];
    assert_eq!(
        (engine, navigation(&effects, view).1.as_str()),
        ("servo", "file:///tmp/app.wasm")
    );
    assert!(b.state.view(view).unwrap().chosen());
    // What the user types goes through the router as ever: a keyword, a search.
    let effects = b.apply(command("page.open-in wasm rust borrow"));
    let (view, _, _) = opened(&effects)[0];
    assert!(navigation(&effects, view).1.contains("?q=rust%20borrow"));
}

#[test]
fn open_in_takes_the_address_still_loading() {
    let mut b = Harness::new("");
    b.update(BrowserEvent::Start {
        session: None,
        open: vec![open("https://slow.test/")],
    });
    let effects = b.update(command("page.open-in wasm"));
    let (view, engine, _) = opened(&effects)[0];
    assert_eq!(
        (engine, navigation(&effects, view).1.as_str()),
        ("wasm", "https://slow.test/")
    );
}

#[test]
fn open_in_says_why_it_cant() {
    let error = |b: &Harness| b.active().error.map(|e| e.message.clone());
    let mut b = Harness::start("", &["sig://home"]);
    b.apply(command("page.open-in servo"));
    let message = error(&b).unwrap();
    assert!(
        message.contains("sig: pages are Sighurt's own"),
        "{message}"
    );
    b.apply(command("page.open https://a.test/"));
    let servo = b.shown();
    let effects = b.apply(command("page.open-in wpe"));
    assert_eq!(
        error(&b).as_deref(),
        Some("The wpe engine is not part of this build of sig.")
    );
    assert_eq!(
        (opened(&effects).len(), b.shown()),
        (0, servo),
        "the page stays"
    );

    let mut b = Harness::start(
        "[engines]\nenabled = [\"servo\", \"wasm\"]",
        &["https://a.test/"],
    );
    b.apply(command("page.open-in demo"));
    let message = error(&b).unwrap();
    assert!(
        message.contains("the demo engine is not enabled"),
        "{message}"
    );
    // An engine opens the web's schemes and file:, and those routed to it.
    b.apply(command("page.open-in wasm data:text/html,x"));
    let message = error(&b).unwrap();
    assert!(
        message.contains("the wasm engine doesn't open data: URLs"),
        "{message}"
    );
}

/// Servo and WPE are web engines, in that order; Wasm and the demo engine are not.
fn web_env() -> Environment {
    let mut env = environment();
    env.engines = engines(&["servo", "wasm", "wpe", "demo"]);
    env.web_engines = vec!["servo".into(), "wpe".into()];
    env
}

fn web_start(config: &str, urls: &[&str]) -> Harness {
    let mut harness = Harness::with_env(config, web_env());
    harness.apply(BrowserEvent::Start {
        session: None,
        open: urls.iter().map(|url| open(url)).collect(),
    });
    harness
}

/// One saved history segment: its engine, entries and which entry is current.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SavedSegment {
    engine: Option<String>,
    entries: Vec<String>,
    current: usize,
}

/// The active tab's saved history.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SavedHistory {
    /// Which segment is current.
    current: usize,
    segments: Vec<SavedSegment>,
}

/// The active tab's saved history: which segment, then each segment's engine and entries.
fn history(b: &Harness) -> SavedHistory {
    let session = b.state.session();
    let tab = &session.tabs[session.active];
    let segments = tab
        .segments
        .iter()
        .map(|segment| SavedSegment {
            engine: segment.engine.clone(),
            entries: segment.entries.clone(),
            current: segment.current,
        })
        .collect();
    SavedHistory {
        current: tab.current,
        segments,
    }
}

fn shown_choice(b: &Harness) -> (String, bool) {
    let view = b.state.view(b.shown()).unwrap();
    (view.engine().to_owned(), view.chosen())
}

#[test]
fn switch_engine_cycles_web_engines_in_descriptor_order() {
    let mut b = web_start("", &["https://a.test/"]);
    assert_eq!(shown_choice(&b).0, "servo");
    b.apply(command("page.switch-engine"));
    assert_eq!(shown_choice(&b), ("wpe".into(), true));
    b.apply(command("page.switch-engine"));
    assert_eq!(shown_choice(&b), ("servo".into(), true));
    // Wasm sits between them in the build and is not a web engine.
    let engines: Vec<_> = history(&b)
        .segments
        .into_iter()
        .map(|segment| segment.engine)
        .collect();
    assert_eq!(
        engines,
        [
            Some("servo".into()),
            Some("wpe".into()),
            Some("servo".into())
        ]
    );

    // Enabled order is not the cycle order: the descriptor order is.
    let mut b = web_start(
        "[engines]\ndefault = \"wpe\"\nenabled = [\"wpe\", \"servo\", \"wasm\"]",
        &["https://a.test/"],
    );
    assert_eq!(shown_choice(&b).0, "wpe");
    b.apply(command("page.switch-engine"));
    assert_eq!(shown_choice(&b).0, "servo");

    // A page in an engine that is not a web engine moves to the first one.
    let mut env = web_env();
    env.web_engines = vec!["wpe".into(), "servo".into()];
    let mut b = Harness::with_env("", env);
    b.apply(BrowserEvent::Start {
        session: None,
        open: vec![open("https://a.test/")],
    });
    b.apply(command("page.open-in wasm"));
    assert_eq!(shown_choice(&b).0, "wasm");
    b.apply(command("page.switch-engine"));
    assert_eq!(shown_choice(&b).0, "wpe");
}

/// While a switch is pending (the new view not committed yet) the tab says which engine it is
/// switching to, and another `page.switch-engine` does nothing: no second view, no notice.
#[test]
fn switch_engine_while_one_is_pending_does_nothing() {
    let mut b = web_start("", &["https://a.test/"]);
    assert_eq!(b.state.tabs().next().unwrap().switching_to, None);
    let shown = b.shown();

    // The engines don't answer, so the switch stays pending.
    let first = b.update(command("page.switch-engine"));
    assert_eq!(opened(&first).len(), 1);
    assert_eq!(opened(&first)[0].1, "wpe");
    assert_eq!(b.state.tabs().next().unwrap().switching_to, Some("wpe"));
    assert_eq!(b.shown(), shown);

    let again = b.update(command("page.switch-engine"));
    assert!(opened(&again).is_empty(), "{again:?}");
    assert!(notices(&again).is_empty());
    assert!(!again
        .iter()
        .any(|effect| matches!(effect, Effect::CloseView { .. })));
    assert_eq!(b.state.tabs().next().unwrap().switching_to, Some("wpe"));
    assert_eq!(b.shown(), shown);

    // Once the engines answer, the switch completes and the next one is allowed again.
    let mut queue: VecDeque<BrowserEvent> = first.iter().flat_map(|e| b.answer(e)).collect();
    while let Some(event) = queue.pop_front() {
        let effects = b.update(event);
        queue.extend(effects.iter().flat_map(|e| b.answer(e)));
    }
    assert_eq!(b.state.tabs().next().unwrap().switching_to, None);
    assert_eq!(shown_choice(&b).0, "wpe");
    let next = b.update(command("page.switch-engine"));
    assert_eq!(opened(&next).len(), 1);
}

/// `page.open-in` is the same pending switch: the tab names the engine until that view fails
/// or commits, and `page.switch-engine` does nothing while it is pending.
#[test]
fn open_in_is_a_pending_switch_that_blocks_switch_engine() {
    let mut b = web_start("", &["https://a.test/"]);
    let shown = b.shown();
    assert_eq!(b.state.tabs().next().unwrap().switching_to, None);

    let first = b.update(command("page.open-in wpe"));
    assert_eq!(opened(&first)[0].1, "wpe");
    assert_eq!(b.state.tabs().next().unwrap().switching_to, Some("wpe"));
    assert_eq!(b.shown(), shown);

    let again = b.update(command("page.switch-engine"));
    assert!(opened(&again).is_empty(), "{again:?}");
    assert!(notices(&again).is_empty());
    assert!(!again
        .iter()
        .any(|effect| matches!(effect, Effect::CloseView { .. })));
    assert_eq!(b.state.tabs().next().unwrap().switching_to, Some("wpe"));
    assert_eq!(b.shown(), shown);

    // The new view failing ends the switch; the page stays, and another switch is allowed.
    let (view, _, _) = opened(&first)[0];
    b.engine(EngineEvent::ViewFailed {
        view,
        message: "stopped".into(),
    });
    assert_eq!(b.state.tabs().next().unwrap().switching_to, None);
    assert_eq!(b.shown(), shown);
    let next = b.update(command("page.switch-engine"));
    assert_eq!(opened(&next).len(), 1);
    assert_eq!(opened(&next)[0].1, "wpe");
    assert_eq!(b.state.tabs().next().unwrap().switching_to, Some("wpe"));
}

#[test]
fn switch_engine_leaves_the_page_and_says_why() {
    let stayed = |b: &mut Harness, config_note: &str| {
        let before = b.shown();
        let effects = b.apply(command("page.switch-engine"));
        assert!(
            opened(&effects).is_empty() && notices(&effects).len() == 1,
            "{config_note}"
        );
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Focus(_))),
            "{config_note}"
        );
        assert_eq!(b.shown(), before, "{config_note}");
        assert!(b.active().error.is_none(), "{config_note}");
        notices(&effects)[0].to_owned()
    };
    let mut b = web_start(
        "[engines]\nenabled = [\"servo\", \"wasm\"]",
        &["https://a.test/"],
    );
    let message = stayed(&mut b, "one web engine");
    assert!(
        message.contains("Only the servo engine") && message.contains("no other web engine"),
        "{message}"
    );

    let mut env = web_env();
    env.web_engines.clear();
    let mut b = Harness::with_env("", env);
    b.apply(BrowserEvent::Start {
        session: None,
        open: vec![open("https://a.test/")],
    });
    let message = stayed(&mut b, "no web engine");
    assert!(message.contains("No web engine is enabled"), "{message}");

    let mut b = web_start("", &["sig://home"]);
    let effects = b.apply(command("page.switch-engine"));
    let message = notices(&effects)[0];
    assert!(
        opened(&effects).is_empty() && message.contains("http, https and file"),
        "{message}"
    );
    assert!(matches!(b.active().content, Content::Internal(_)));

    let mut b = web_start("", &["data:text/html,x"]);
    assert_eq!(shown_choice(&b).0, "servo");
    let message = stayed(&mut b, "data:");
    assert!(message.contains("http, https and file"), "{message}");
    assert_eq!(shown_choice(&b).0, "servo");
}

#[test]
fn switch_engine_matches_page_open_in() {
    let run = |command_text: &str| {
        let mut b = web_start("", &["https://a.test/"]);
        b.apply(command(command_text));
        let chosen = shown_choice(&b);
        let saved = history(&b);
        b.apply(command("page.back"));
        let back = (shown_choice(&b), history(&b));
        b.apply(command("page.forward"));
        let forward = (shown_choice(&b), history(&b));
        (chosen, saved, back, forward)
    };
    let switched = run("page.switch-engine");
    let opened_in = run("page.open-in wpe");
    assert_eq!(switched, opened_in);
    assert_eq!(switched.0, ("wpe".into(), true));

    // A page still loading, and a file, the same way.
    let loading = |command_text: &str| {
        let mut b = Harness::with_env("", web_env());
        b.update(BrowserEvent::Start {
            session: None,
            open: vec![open("https://slow.test/")],
        });
        let effects = b.update(command(command_text));
        let (view, engine, _) = opened(&effects)[0];
        (engine.to_owned(), navigation(&effects, view).1)
    };
    assert_eq!(
        loading("page.switch-engine"),
        loading("page.open-in wpe https://slow.test/")
    );
    let file = |command_text: &str| {
        let mut b = web_start("", &["file:///tmp/page.html"]);
        b.apply(command(command_text));
        (shown_choice(&b), history(&b))
    };
    assert_eq!(file("page.switch-engine"), file("page.open-in wpe"));
}

#[test]
fn a_typed_address_after_switching_engines_routes_as_after_open_in() {
    let after = |command_text: &str| {
        let mut b = web_start("[routing.wpe]\nhosts = [\"b.test\"]", &["https://a.test/"]);
        b.apply(command(command_text));
        let wpe = b.shown();
        // The page's own navigation leaves the view chosen (routing sends b.test to wpe).
        let effects = b.engine(EngineEvent::OpenRequested {
            view: wpe,
            document: b.document(wpe),
            url: "https://b.test/next".into(),
            disposition: OpenDisposition::CurrentView,
        });
        navigation(&effects, wpe);
        let stayed = shown_choice(&b);
        // An address the same engine opens unchooses it, in the same view.
        let effects = b.apply(command("page.open https://b.test/"));
        let same = (opened(&effects).len(), b.shown() == wpe, shown_choice(&b));
        // An address routing gives another engine opens there, unchosen.
        b.apply(command("page.open https://d.test/"));
        let routed = shown_choice(&b);
        (stayed, same, routed)
    };
    let switched = after("page.switch-engine");
    let opened_in = after("page.open-in wpe");
    assert_eq!(switched, opened_in);
    assert_eq!(switched.0, ("wpe".into(), true));
    assert_eq!(switched.1, (0, true, ("wpe".into(), false)));
    assert_eq!(switched.2, ("servo".into(), false));
}

#[test]
fn the_starting_engine_overrides_the_default_across_a_reload() {
    let mut env = web_env();
    env.starting_engine = Some("wpe".into());
    let mut b = Harness::with_env("", env.clone());
    b.apply(BrowserEvent::Start {
        session: None,
        open: vec![open("https://a.test/")],
    });
    assert_eq!(shown_choice(&b), ("wpe".into(), false));
    let config = Config::parse(Some("version = 1\n[engines]\ndefault = \"servo\""), &env).unwrap();
    assert_eq!(config.default_engine(), "wpe");
    b.apply(config_reloaded(Ok(config)));
    b.apply(command("page.open https://c.test/"));
    assert_eq!(shown_choice(&b), ("wpe".into(), false));
}

#[test]
fn a_reload_that_changes_engine_settings_says_they_apply_at_restart() {
    let mut b = Harness::start("[engines.servo]\nstorage = \"ephemeral\"", &["sig://home"]);
    let reloaded = |b: &mut Harness, config: &str| {
        let config = Config::parse(Some(&format!("version = 1\n{config}")), &environment());
        let effects = b.update(config_reloaded(config));
        notices(&effects)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let same = "[engines.servo]\nstorage = \"ephemeral\"";
    assert_eq!(reloaded(&mut b, same), [] as [String; 0]);
    let agent = "[engines.servo]\nstorage = \"ephemeral\"\nuser-agent = \"S\"\n\
                 [engines.wasm]\nstorage = \"ephemeral\"";
    assert_eq!(
        reloaded(&mut b, agent),
        [
            "[engines.servo] applies when sig starts again; the servo engine keeps the settings \
          it started with until then.\n[engines.wasm] applies when sig starts again; the wasm \
          engine keeps the settings it started with until then."
        ]
    );
    // Back to what the engines started with: nothing to say.
    assert_eq!(reloaded(&mut b, same), [] as [String; 0]);
}

#[test]
fn a_reload_that_changes_servo_features_says_they_apply_at_restart() {
    let mut b = Harness::start("[engines.servo]\nstorage = \"ephemeral\"", &["sig://home"]);
    let reloaded = |b: &mut Harness, config: &str| {
        let config = Config::parse(Some(&format!("version = 1\n{config}")), &environment());
        let effects = b.update(config_reloaded(config));
        notices(&effects)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let started = "[engines.servo]\nstorage = \"ephemeral\"";
    let notice = "[engines.servo] applies when sig starts again; the servo engine keeps the \
                  settings it started with until then.";
    assert_eq!(reloaded(&mut b, started), [] as [String; 0]);
    // The flag, written as false, is the setting the engine started with.
    let off = "[engines.servo]\nstorage = \"ephemeral\"\nexperimental-features = false";
    assert_eq!(reloaded(&mut b, off), [] as [String; 0]);
    let features = "[engines.servo]\nstorage = \"ephemeral\"\nexperimental-features = true";
    assert_eq!(reloaded(&mut b, features), [notice.to_owned()]);
    assert_eq!(reloaded(&mut b, started), [] as [String; 0]);
    let prefs = "[engines.servo]\nstorage = \"ephemeral\"\n\
                 [engines.servo.preferences]\ndom_adoptedstylesheet_enabled = true";
    assert_eq!(reloaded(&mut b, prefs), [notice.to_owned()]);
    assert_eq!(reloaded(&mut b, started), [] as [String; 0]);
}

#[test]
fn only_a_captured_buttons_release_reaches_a_view_that_is_no_longer_shown() {
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    b.apply(command("tab.new"));
    b.apply(command("page.open https://b.test/"));
    let send = |b: &mut Harness, kind: PointerEvent| {
        let event = InputEvent {
            seat: b.seat,
            kind: InputKind::Pointer(kind),
        };
        let at = b.now;
        b.update(BrowserEvent::PointerRelease { view, event, at })
    };
    let position = SurfacePoint { x: 5.0, y: 5.0 };
    let modifiers = Modifiers::default();
    let button = |button, state| PointerEvent::Button {
        button,
        state,
        position,
        modifiers,
    };
    let released = send(&mut b, button(PointerButton::Left, ButtonState::Released));
    assert!(pointed(&released, view), "{released:?}");
    for kind in [
        button(PointerButton::Left, ButtonState::Pressed),
        button(PointerButton::Back, ButtonState::Released),
        PointerEvent::Motion {
            position,
            modifiers,
        },
    ] {
        let effects = send(&mut b, kind);
        assert!(!pointed(&effects, view), "{effects:?}");
    }
    // The ordinary path still drops a release for a view not shown.
    let effects = b.click(view, PointerButton::Left, ButtonState::Released);
    assert!(!pointed(&effects, view), "{effects:?}");
}

#[test]
fn a_view_being_closed_gets_no_release() {
    let mut b = Harness::start("", &["https://a.test/", "https://b.test/"]);
    let view = b.shown();
    // Closed, and its engine has not said it is gone yet.
    let effects = b.update(command("tab.close"));
    assert_eq!(closed(&effects), [view]);
    let event = InputEvent {
        seat: b.seat,
        kind: InputKind::Pointer(PointerEvent::Button {
            button: PointerButton::Left,
            state: ButtonState::Released,
            position: SurfacePoint { x: 5.0, y: 5.0 },
            modifiers: Modifiers::default(),
        }),
    };
    let at = b.now;
    let effects = b.update(BrowserEvent::PointerRelease { view, event, at });
    assert!(effects.is_empty(), "{effects:?}");
}
