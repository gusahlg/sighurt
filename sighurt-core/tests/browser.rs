//! The core driven as the executable drives it: events in, effects out, with a fake engine
//! playing the engines' part. No display, no engine, no I/O.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCapabilities, EngineCommand, EngineError, ViewOptions};
use sighurt_api::events::{Cursor, EngineEvent, NavigationError, OpenDisposition};
use sighurt_api::geometry::{SurfacePoint, SurfaceRect};
use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, SeatId, TabId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, Key, KeyCode, KeyEvent, Modifiers,
    PointerButton, PointerEvent, TextInputState, TextPurpose,
};
use sighurt_api::services::{
    ClipboardData, DownloadSource, Permission, Selection, Service, ServiceError, ServiceReply,
    ServiceRequest,
};
use sighurt_core::config::Config;
use sighurt_core::navigation::RETAINED_VIEWS;
use sighurt_core::session::Session;
use sighurt_core::tabs::{Content, TabSummary, ViewState};
use sighurt_core::{update, BrowserEvent, BrowserState, Effect, Environment, Focus, OpenRequest};

/// A view as the fake engine keeps it: its session history, like Servo's.
struct FakeView {
    engine: String,
    entries: Vec<String>,
    current: usize,
    document: DocumentGeneration,
}

/// The engines of a build: servo and wasm traverse their history and zoom, the demo engine
/// does neither.
fn engines(names: &[&str]) -> BTreeMap<String, EngineCapabilities> {
    let full = EngineCapabilities {
        history_traversal: true,
        page_zoom: true,
        ..EngineCapabilities::default()
    };
    let capabilities = |name| match name {
        "demo" => EngineCapabilities::default(),
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

fn command(text: &str) -> BrowserEvent {
    BrowserEvent::Command(text.parse().unwrap())
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
        let config = Config::parse(Some(&format!("version = 1\n{config}"))).unwrap();
        let env = Environment {
            engines: engines(&["servo", "wasm", "demo"]),
            home_dir: None,
        };
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

    /// What the fake engine reports after `effect`.
    fn answer(&mut self, effect: &Effect) -> Vec<BrowserEvent> {
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
    assert!(effects.contains(&Effect::Focus(Focus::Location)));
    let effects = b.apply(command("tab.last"));
    assert!(effects.contains(&Effect::Focus(Focus::Page)));
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

    // The shown page's document crashes: the view stays, and reloading recovers it.
    let mut b = Harness::start("", &["https://a.test/"]);
    let view = b.shown();
    b.engine(EngineEvent::ViewFailed {
        view,
        message: "script crashed".into(),
    });
    assert_eq!(b.state.view(view).unwrap().state(), ViewState::Failed);
    assert_eq!(b.active().error.unwrap().url, None);
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
            home_dir: None,
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
    assert!(effects.contains(&Effect::Focus(Focus::Location)));
    // The page gets no L, and loses focus, which releases the Ctrl it saw go down.
    assert_eq!(commands(&effects, view), [&EngineCommand::SetFocus(false)]);
    assert_eq!(b.state.focus(), Focus::Location);
    assert_eq!(b.key(release(&ctrl_l)), []);
    let effects = b.key(release(&ctrl));
    assert!(matches!(effects[..], [Effect::ChromeInput(_)]));
    // A click in the page gives it the keyboard back.
    let effects = b.click(view, PointerButton::Left, ButtonState::Pressed);
    assert!(effects.contains(&Effect::Focus(Focus::Page)));
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
    let broken = Config::parse(Some("version = 1\n[keys]\n\"ctrl-t\" = \"tab.nope\""));
    let effects = b.update(BrowserEvent::ConfigReloaded(broken));
    let message = notices(&effects)[0];
    assert!(
        message.contains("tab.nope") && message.contains("stays"),
        "{message}"
    );
    b.key(press(20, "t", CTRL));
    assert_eq!(b.state.tabs().len(), 2, "the old bindings still work");

    let reloaded = Config::parse(Some("version = 1\n[keys]\n\"ctrl-n\" = \"tab.new\""));
    b.update(BrowserEvent::ConfigReloaded(reloaded));
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
