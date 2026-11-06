//! Sighurt's own pages (`sig://help`, `sig://terminal`): routing, who may open them, the view
//! each gets, what they may ask for, the terminals' lifetime, the configuration's commands and
//! the input held after a tab comes to the front by itself. No display and no engine.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCapabilities, EngineCommand};
use sighurt_api::events::{EngineEvent, OpenDisposition};
use sighurt_api::ids::{DocumentGeneration, PtySession, RequestId, SeatId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, Key, KeyCode, KeyEvent, KeyboardFocus,
    Modifiers, PointerButton, PointerEvent,
};
use sighurt_api::keymap::Frontend;
use sighurt_api::services::{
    PageGrant, PageInfo, PageRequest, PtyProgram, Service, ServiceError, ServiceReply,
    ServiceRequest, ServiceResult,
};
use sighurt_core::config::Config;
use sighurt_core::session::{Session, SessionSegment, SessionTab, VERSION};
use sighurt_core::tabs::Content;
use sighurt_core::{update, BrowserEvent, BrowserState, Effect, Environment, OpenRequest};

const GRANTS_HELP: &[PageGrant] = &[PageGrant::Info];
const GRANTS_TERMINAL: &[PageGrant] = &[PageGrant::Pty];
const GRANTS_CONFIG: &[PageGrant] = &[PageGrant::ConfigRead, PageGrant::ConfigWrite];

/// A build with Servo, the Wasm engine and the three pages.
fn environment() -> Environment {
    let capabilities = EngineCapabilities {
        history_traversal: true,
        page_zoom: true,
        ..EngineCapabilities::default()
    };
    Environment {
        engines: ["servo", "wasm"]
            .map(|e| (e.to_owned(), capabilities))
            .into(),
        pages: [
            ("help", GRANTS_HELP),
            ("terminal", GRANTS_TERMINAL),
            ("config", GRANTS_CONFIG),
        ]
        .map(|(name, grants)| (name.to_owned(), grants))
        .into(),
        ..Environment::default()
    }
}

struct Harness {
    state: BrowserState,
    now: Instant,
    /// Each view's committed URL and document.
    views: BTreeMap<ViewId, (String, DocumentGeneration)>,
}

impl Harness {
    fn new(config: &str) -> Self {
        Self::with(config, environment())
    }

    fn with(config: &str, env: Environment) -> Self {
        let config = Config::parse(Some(&format!("version = 1\n{config}")), &env).unwrap();
        Self {
            state: BrowserState::new(config, env),
            now: Instant::now(),
            views: BTreeMap::new(),
        }
    }

    fn start(config: &str, urls: &[&str]) -> Self {
        let mut harness = Self::new(config);
        harness.apply(BrowserEvent::Start {
            session: None,
            open: urls
                .iter()
                .map(|url| OpenRequest {
                    input: (*url).into(),
                    engine: None,
                })
                .collect(),
        });
        harness
    }

    /// Applies `event` and everything the fake engines answer, returning every effect.
    fn apply(&mut self, event: BrowserEvent) -> Vec<Effect> {
        let mut all = Vec::new();
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            let effects = update(&mut self.state, event);
            for effect in &effects {
                queue.extend(self.answer(effect));
            }
            all.extend(effects);
            assert!(all.len() < 10_000, "the core and the engines never settle");
        }
        all
    }

    fn answer(&mut self, effect: &Effect) -> Vec<BrowserEvent> {
        let (view, events) = match effect {
            Effect::OpenView { view, .. } => (*view, vec![EngineEvent::ViewReady { view: *view }]),
            Effect::Engine {
                view,
                command: EngineCommand::Navigate { navigation, url },
            } => {
                let document = DocumentGeneration::next();
                self.views.insert(*view, (url.clone(), document));
                let committed = EngineEvent::NavigationCommitted {
                    view: *view,
                    navigation: *navigation,
                    url: url.clone(),
                    document,
                    history: None,
                };
                (*view, vec![committed])
            }
            Effect::Engine {
                view,
                command: EngineCommand::Reload { navigation },
            } => {
                let document = DocumentGeneration::next();
                let entry = self.views.get_mut(view).unwrap();
                entry.1 = document;
                let committed = EngineEvent::NavigationCommitted {
                    view: *view,
                    navigation: *navigation,
                    url: entry.0.clone(),
                    document,
                    history: None,
                };
                (*view, vec![committed])
            }
            _ => return Vec::new(),
        };
        let engine = self.engine_of(view);
        events
            .into_iter()
            .map(|event| BrowserEvent::Engine {
                engine: engine.clone(),
                event,
                at: self.now,
            })
            .collect()
    }

    fn engine_of(&self, view: ViewId) -> String {
        self.state.view(view).unwrap().engine().to_owned()
    }

    fn engine(&mut self, view: ViewId, event: EngineEvent) -> Vec<Effect> {
        let engine = self.engine_of(view);
        let at = self.now;
        self.apply(BrowserEvent::Engine { engine, event, at })
    }

    /// The view the active tab shows.
    fn shown(&self) -> ViewId {
        let tab = self.state.tabs().nth(self.state.active()).unwrap();
        match tab.content {
            Content::View(view) => view,
            other => panic!("the tab shows no view: {other:?}"),
        }
    }

    fn document(&self, view: ViewId) -> DocumentGeneration {
        self.state.view(view).unwrap().document().unwrap()
    }

    /// A click in `view`'s page.
    fn click(&mut self, view: ViewId) -> Vec<Effect> {
        let at = self.now;
        let event = InputEvent {
            seat: SeatId::next(),
            kind: InputKind::Pointer(PointerEvent::Button {
                button: PointerButton::Left,
                state: ButtonState::Pressed,
                position: Default::default(),
                modifiers: Modifiers::default(),
            }),
        };
        self.apply(BrowserEvent::Pointer { view, event, at })
    }

    /// `view`'s current document asks for `service`, as `origin`.
    fn ask(
        &mut self,
        view: ViewId,
        origin: Option<&str>,
        service: Service,
    ) -> (RequestId, Vec<Effect>) {
        let id = RequestId::next();
        let request = ServiceRequest {
            id,
            view,
            document: self.document(view),
            origin: origin.map(Into::into),
            service,
        };
        (
            id,
            self.engine(view, EngineEvent::ServiceRequested(request)),
        )
    }

    fn done(&mut self, request: RequestId, result: ServiceResult) -> Vec<Effect> {
        self.apply(BrowserEvent::ServiceDone { request, result })
    }

    fn command(&mut self, text: &str) -> Vec<Effect> {
        let names: Vec<String> = self.state.config().commands().keys().cloned().collect();
        let command = sighurt_api::commands::CommandSet::ANY
            .with_custom(&names)
            .parse(text)
            .unwrap();
        self.apply(BrowserEvent::Command(command))
    }
}

fn pty_open() -> Service {
    Service::Page(PageRequest::PtyOpen {
        program: PtyProgram::Shell,
        cols: 80,
        rows: 24,
    })
}

fn info() -> Service {
    Service::Page(PageRequest::Info(PageInfo::Commands))
}

/// Whether `effects` performs `request`.
fn performs(effects: &[Effect], request: RequestId) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::Service { request: r, .. } if *r == request))
}

/// The answer `effects` gives `request`, if any.
fn answer(effects: &[Effect], request: RequestId) -> Option<&ServiceResult> {
    effects.iter().find_map(|effect| match effect {
        Effect::Engine {
            command: EngineCommand::ResolveServiceRequest { request: r, result },
            ..
        } if *r == request => Some(result),
        _ => None,
    })
}

fn cancels(effects: &[Effect], request: RequestId) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::CancelService(r) if *r == request))
}

fn opened(effects: &[Effect]) -> Vec<(String, String)> {
    let mut engines = BTreeMap::new();
    let mut found = Vec::new();
    for effect in effects {
        match effect {
            Effect::OpenView { view, engine, .. } => _ = engines.insert(*view, engine.clone()),
            Effect::Engine {
                view,
                command: EngineCommand::Navigate { url, .. },
            } => found.push((engines.get(view).cloned().unwrap_or_default(), url.clone())),
            _ => {}
        }
    }
    found
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

// ── Routing ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn the_builds_pages_open_in_the_wasm_engine_at_their_canonical_address() {
    let mut h = Harness::start("", &["https://a.test/"]);
    for (typed, url, page) in [
        ("sig://help", "sig://help", "help"),
        // The same page: its view loads it.
        ("SIG://Help/keys?x#y", "sig://help/keys?x#y", "help"),
        ("sig://terminal", "sig://terminal", "terminal"),
    ] {
        let effects = h.command(&format!("page.open {typed}"));
        let urls: Vec<String> = opened(&effects).into_iter().map(|(_, url)| url).collect();
        assert_eq!(urls, [url], "{typed}");
        let view = h.shown();
        assert_eq!(h.engine_of(view), "wasm");
        assert_eq!(h.state.view(view).unwrap().page(), Some(page));
    }
    // sig://home, a page the build lacks, and forms that name no page are the chrome's.
    for url in [
        "sig://home",
        "sig://nope",
        "sig:help",
        "sig://me@help",
        "sig://help:80",
    ] {
        let effects = h.command(&format!("page.open {url}"));
        assert!(opened(&effects).is_empty(), "{url}");
        let tab = h.state.tabs().nth(h.state.active()).unwrap();
        assert!(matches!(tab.content, Content::Internal(_)), "{url}");
    }
}

#[test]
fn without_the_wasm_engine_a_page_is_a_clear_error() {
    let mut env = environment();
    env.engines.remove("wasm");
    env.pages.clear();
    let mut h = Harness::with("", env);
    h.apply(BrowserEvent::Start {
        session: None,
        open: Vec::new(),
    });
    h.command("page.open sig://help");
    let tab = h.state.tabs().nth(h.state.active()).unwrap();
    let error = tab.error.unwrap().message.as_str();
    assert!(error.contains("wasm engine"), "{error}");
    // Enabled in the build, not in the configuration.
    let mut h = Harness::start("[engines]\nenabled = [\"servo\"]", &[]);
    h.command("page.open sig://help");
    let tab = h.state.tabs().nth(h.state.active()).unwrap();
    assert!(tab.error.unwrap().message.contains("[engines] enabled"));
}

// ── Who opens a page ─────────────────────────────────────────────────────────────────────────

#[test]
fn no_web_page_opens_an_internal_page_by_any_path() {
    for url in [
        "sig://terminal",
        "SIG://terminal",
        "sig:terminal",
        " sig://help ",
    ] {
        let mut h = Harness::start("", &["https://a.test/"]);
        let view = h.shown();
        let document = h.document(view);
        h.click(view);
        for disposition in [OpenDisposition::CurrentView, OpenDisposition::NewView] {
            let effects = h.engine(
                view,
                EngineEvent::OpenRequested {
                    view,
                    document,
                    url: url.into(),
                    disposition,
                },
            );
            assert!(opened(&effects).is_empty(), "{url} {disposition:?}");
            assert_eq!(h.state.tabs().len(), 1);
        }
        let request = RequestId::next();
        let effects = h.engine(
            view,
            EngineEvent::NavigationRequested {
                view,
                document,
                request,
                url: url.into(),
            },
        );
        assert!(opened(&effects).is_empty(), "{url}");
        let allowed = effects.iter().any(|e| {
            matches!(
                e,
                Effect::Engine {
                    command: EngineCommand::AnswerNavigation { allow: true, .. },
                    ..
                }
            )
        });
        assert!(!allowed, "{url}");
    }
}

#[test]
fn an_internal_page_opens_another_and_the_web() {
    let mut h = Harness::start("", &["sig://help"]);
    let view = h.shown();
    let document = h.document(view);
    let effects = h.engine(
        view,
        EngineEvent::OpenRequested {
            view,
            document,
            url: "sig://config".into(),
            disposition: OpenDisposition::CurrentView,
        },
    );
    assert_eq!(
        opened(&effects),
        [("wasm".to_owned(), "sig://config".to_owned())]
    );
    let config = h.shown();
    assert_ne!(config, view, "a page per view");
    assert_eq!(h.state.view(config).unwrap().page(), Some("config"));
    let document = h.document(config);
    let effects = h.engine(
        config,
        EngineEvent::OpenRequested {
            view: config,
            document,
            url: "https://a.test/".into(),
            disposition: OpenDisposition::CurrentView,
        },
    );
    assert_eq!(
        opened(&effects),
        [("servo".to_owned(), "https://a.test/".to_owned())]
    );
}

#[test]
fn a_view_is_never_reused_across_page_identity() {
    let mut h = Harness::start("[routing.wasm]\nextensions = [\"wasm\"]", &["sig://help"]);
    let help = h.shown();
    // Another path of the same page: the same view.
    let effects = h.command("page.open sig://help/keys");
    assert!(!effects.iter().any(|e| matches!(e, Effect::OpenView { .. })));
    assert_eq!(h.shown(), help);
    // A Wasm app, another page: views of their own.
    for url in ["file:///tmp/app.wasm", "sig://terminal", "sig://help"] {
        let before = h.shown();
        let effects = h.command(&format!("page.open {url}"));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::OpenView { .. })),
            "{url}"
        );
        assert_ne!(h.shown(), before, "{url}");
    }
    // The app's view has no page; a page's view names its page and no engine.
    let tabs: Vec<_> = h.state.tabs().collect();
    assert_eq!(tabs.len(), 1);
}

#[test]
fn a_restored_page_is_routed_again_and_starts_nothing() {
    let segment = |engine: &str, url: &str| SessionSegment {
        engine: Some(engine.into()),
        entries: vec![url.into()],
        current: 0,
        title: None,
    };
    let tab = |segment| SessionTab {
        current: 0,
        zoom: 1.0,
        segments: vec![segment],
    };
    let session = Session {
        version: VERSION,
        active: 0,
        tabs: vec![
            // A saved file says servo: the address decides.
            tab(segment("servo", "sig://terminal")),
            tab(segment("wasm", "sig://help/keys")),
        ],
        notices: Vec::new(),
    };
    let mut h = Harness::new("[pages.terminal]\nenabled = true");
    let effects = h.apply(BrowserEvent::Start {
        session: Some(session),
        open: Vec::new(),
    });
    assert_eq!(
        opened(&effects),
        [("wasm".to_owned(), "sig://terminal".to_owned())]
    );
    assert_eq!(h.state.view(h.shown()).unwrap().page(), Some("terminal"));
    assert!(!effects.iter().any(|e| matches!(e, Effect::Service { .. })));
    let effects = h.command("tab.next");
    assert_eq!(
        opened(&effects),
        [("wasm".to_owned(), "sig://help/keys".to_owned())]
    );
    assert!(!effects.iter().any(|e| matches!(e, Effect::Service { .. })));
}

// ── What a page may ask for ──────────────────────────────────────────────────────────────────

#[test]
fn info_is_the_help_pages_and_no_one_elses() {
    let mut h = Harness::start("", &["sig://help"]);
    let help = h.shown();
    let (id, effects) = h.ask(help, Some("sig://help"), info());
    assert!(performs(&effects, id));
    let effects = h.done(id, Ok(ServiceReply::Text("x".into())));
    assert_eq!(
        answer(&effects, id),
        Some(&Ok(ServiceReply::Text("x".into())))
    );
    // A spoofed origin, or the right one from a page that isn't help.
    let (id, effects) = h.ask(help, Some("sig://config"), info());
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));
    let (id, effects) = h.ask(help, None, info());
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));
    h.command("tab.new https://a.test/");
    let web = h.shown();
    let (id, effects) = h.ask(web, Some("sig://help"), info());
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));
    // The terminal has no grant to Info, nor help to a terminal or the configuration.
    h.command("tab.new sig://terminal");
    let terminal = h.shown();
    let (id, effects) = h.ask(terminal, Some("sig://terminal"), info());
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));
    h.command("tab.select 1");
    h.click(help);
    for service in [pty_open(), Service::Page(PageRequest::ConfigRead)] {
        let (id, effects) = h.ask(help, Some("sig://help"), service);
        assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));
    }
}

#[test]
fn a_terminal_needs_the_setting_the_front_the_users_action_and_room() {
    // The setting off.
    let mut h = Harness::start("", &["sig://terminal"]);
    let view = h.shown();
    h.click(view);
    let (id, effects) = h.ask(view, Some("sig://terminal"), pty_open());
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));

    let mut h = Harness::start("[pages.terminal]\nenabled = true", &["sig://terminal"]);
    let view = h.shown();
    // No click or key press in it.
    let (id, effects) = h.ask(view, Some("sig://terminal"), pty_open());
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));
    // An old click.
    h.click(view);
    h.now += Duration::from_secs(6);
    let (id, effects) = h.ask(view, Some("sig://terminal"), pty_open());
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));
    // A click: allowed, and the click is used up.
    h.click(view);
    let (first, effects) = h.ask(view, Some("sig://terminal"), pty_open());
    assert!(performs(&effects, first));
    h.click(view);
    let (second, effects) = h.ask(view, Some("sig://terminal"), pty_open());
    assert_eq!(
        answer(&effects, second),
        Some(&Err(ServiceError::Denied)),
        "one a document"
    );
    // A stale document.
    let old = h.document(view);
    h.command("page.reload");
    let request = ServiceRequest {
        id: RequestId::next(),
        view,
        document: old,
        origin: Some("sig://terminal".into()),
        service: pty_open(),
    };
    let id = request.id;
    let effects = h.engine(view, EngineEvent::ServiceRequested(request));
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Cancelled)));
}

#[test]
fn eight_terminals_at_most_until_their_programs_are_reaped() {
    let mut h = Harness::start("[pages.terminal]\nenabled = true", &[]);
    let mut started = Vec::new();
    for _ in 0..9 {
        h.command("tab.new sig://terminal");
        let view = h.shown();
        h.click(view);
        started.push(h.ask(view, Some("sig://terminal"), pty_open()));
    }
    for (id, effects) in &started[..8] {
        assert!(performs(effects, *id));
    }
    let (ninth, effects) = &started[8];
    assert_eq!(answer(effects, *ninth), Some(&Err(ServiceError::Denied)));
    // Closing one keeps its place until its program was reaped.
    let (first, _) = started[0];
    h.done(first, Ok(ServiceReply::Pty(PtySession::next())));
    // Tab 1 is the home tab sig started with.
    h.command("tab.select 2");
    let effects = h.command("tab.close");
    assert!(cancels(&effects, first));
    // The ninth tab, which was refused, still is while the first program is being ended...
    h.command("tab.last");
    let view = h.shown();
    h.click(view);
    let (id, effects) = h.ask(view, Some("sig://terminal"), pty_open());
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));
    // ...and has room once it was reaped.
    h.done(first, Ok(ServiceReply::PtyEnded { status: None }));
    h.click(view);
    let (id, effects) = h.ask(view, Some("sig://terminal"), pty_open());
    assert!(performs(&effects, id));
}

#[test]
fn a_terminal_ends_with_its_document_tab_history_entry_setting_and_quit() {
    let start = |h: &mut Harness| {
        let view = h.shown();
        h.click(view);
        let (id, effects) = h.ask(view, Some("sig://terminal"), pty_open());
        assert!(performs(&effects, id));
        let effects = h.done(id, Ok(ServiceReply::Pty(PtySession::next())));
        assert!(matches!(
            answer(&effects, id),
            Some(Ok(ServiceReply::Pty(_)))
        ));
        id
    };
    let on = "[pages.terminal]\nenabled = true";
    // Reload: a new document.
    let mut h = Harness::start(on, &["sig://terminal"]);
    let id = start(&mut h);
    assert!(cancels(&h.command("page.reload"), id));
    // Its page becomes a history entry.
    let mut h = Harness::start(on, &["sig://terminal"]);
    let id = start(&mut h);
    assert!(cancels(&h.command("page.open https://a.test/"), id));
    // A tab in the background keeps it; closing the tab ends it.
    let mut h = Harness::start(on, &["sig://terminal"]);
    let id = start(&mut h);
    assert!(!cancels(&h.command("tab.new https://a.test/"), id));
    h.command("tab.select 1");
    assert!(cancels(&h.command("tab.close"), id));
    // Quit.
    let mut h = Harness::start(on, &["sig://terminal"]);
    let id = start(&mut h);
    assert!(cancels(&h.command("browser.quit"), id));
    // The page closes it.
    let mut h = Harness::start(on, &["sig://terminal"]);
    let id = start(&mut h);
    let view = h.shown();
    let effects = h.engine(view, EngineEvent::ServiceCancelled { view, request: id });
    assert!(cancels(&effects, id));
    // The setting goes off.
    let mut h = Harness::start(on, &["sig://terminal"]);
    let id = start(&mut h);
    let off = Config::parse(Some("version = 1"), &environment()).unwrap();
    assert!(cancels(
        &h.apply(BrowserEvent::ConfigReloaded(Ok(Box::new(off)))),
        id
    ));
    // The end reaches the page while its document lasts.
    let mut h = Harness::start(on, &["sig://terminal"]);
    let id = start(&mut h);
    let ended = Ok(ServiceReply::PtyEnded { status: Some(0) });
    assert_eq!(answer(&h.done(id, ended.clone()), id), Some(&ended));
}

#[test]
fn writing_the_configuration_needs_the_front_and_the_users_action() {
    let write = || {
        Service::Page(PageRequest::ConfigWrite {
            text: "version = 1".into(),
            replaces: String::new(),
        })
    };
    let mut h = Harness::start("", &["sig://config"]);
    let view = h.shown();
    let (id, effects) = h.ask(view, Some("sig://config"), write());
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));
    let (id, effects) = h.ask(
        view,
        Some("sig://config"),
        Service::Page(PageRequest::ConfigRead),
    );
    assert!(performs(&effects, id), "reading needs no click");
    h.click(view);
    let (id, effects) = h.ask(view, Some("sig://config"), write());
    assert!(performs(&effects, id));
    // From a background tab: no.
    h.click(view);
    h.command("tab.new https://a.test/");
    let (id, effects) = h.ask(view, Some("sig://config"), write());
    assert_eq!(answer(&effects, id), Some(&Err(ServiceError::Denied)));
}

// ── The configuration's commands ─────────────────────────────────────────────────────────────

#[test]
fn help_opens_sig_help_in_a_new_tab() {
    let mut h = Harness::start("", &["https://a.test/"]);
    let effects = h.command("help");
    assert_eq!(
        opened(&effects),
        [("wasm".to_owned(), "sig://help".to_owned())]
    );
    assert_eq!(h.state.tabs().len(), 2);
    assert_eq!(h.state.active(), 1);
    // Several, in order.
    let mut h = Harness::start(
        "[commands]\ntwo = [\"tab.new https://b.test/\", \"tab.new https://c.test/\"]",
        &[],
    );
    let effects = h.command("two");
    let urls: Vec<String> = opened(&effects).into_iter().map(|(_, url)| url).collect();
    assert_eq!(urls, ["https://b.test/", "https://c.test/"]);
}

#[test]
fn the_commands_table_is_checked_when_it_loads() {
    let env = environment();
    let problems = |text: &str| {
        Config::parse(Some(&format!("version = 1\n[commands]\n{text}")), &env)
            .err()
            .map(|e| e.problems().join("\n"))
            .unwrap_or_default()
    };
    assert_eq!(problems("open-it = \"tab.new\""), "");
    assert_eq!(problems("x = [\"page.reload\", \"tab.next\"]"), "");
    for (bad, why) in [
        ("tab = \"tab.new\"", "not tab"),
        ("\"a.b\" = \"tab.new\"", "no dot"),
        ("Help = \"tab.new\"", "lower-case"),
        ("x = \"tab.explode\"", "unknown command"),
        ("x = \"help\"", "unknown command"),
        ("x = []", "list of 1 to 8"),
        ("x = 3", "in quotes"),
        ("x = [\"tab.new\", 3]", "in quotes"),
        ("x = \"tab.select\"", "needs an argument"),
    ] {
        assert!(problems(bad).contains(why), "{bad}: {}", problems(bad));
    }
    let nine = ["\"page.reload\""; 9].join(", ");
    assert!(problems(&format!("x = [{nine}]")).contains("list of 1 to 8"));
    let many: String = (0..65)
        .map(|i| format!("c{i} = \"page.reload\"\n"))
        .collect();
    assert!(problems(&many).contains("at most 64"));
    // Keys may bind a command the table defines, and not one it doesn't.
    let keys = |text: &str| {
        Config::parse(
            Some(&format!(
                "version = 1\n[input]\nkeymap = \"bindings\"\n{text}"
            )),
            &env,
        )
    };
    assert!(keys("[keys]\n\"f2\" = \"help\"").is_ok());
    let error = keys("[keys]\n\"f2\" = \"hlep\"").unwrap_err().to_string();
    assert!(error.contains("unknown command \"hlep\""), "{error}");
    // The built-in `help` merges with the user's commands.
    let config = Config::parse(Some("version = 1\n[commands]\nx = \"page.reload\""), &env).unwrap();
    assert!(config.command("help").is_some() && config.command("x").is_some());
}

// ── Input after a tab came to the front by itself ────────────────────────────────────────────

fn key(c: char) -> BrowserEvent {
    BrowserEvent::Keyboard {
        event: InputEvent {
            seat: SeatId::next(),
            kind: InputKind::Key(KeyEvent {
                code: KeyCode(30),
                key: Key::Character(c.into()),
                state: ButtonState::Pressed,
                repeat: false,
                modifiers: Modifiers::default(),
                consumed: Modifiers::default(),
                text: Some(c.into()),
            }),
        },
        at: Instant::now(),
    }
}

fn delivered(effects: &[Effect]) -> bool {
    effects.iter().any(|e| {
        matches!(
            e,
            Effect::Engine {
                command: EngineCommand::Input(InputEvent {
                    kind: InputKind::Key(_),
                    ..
                }),
                ..
            }
        )
    })
}

#[test]
fn typing_waits_for_the_user_after_a_page_closes_its_tab() {
    // `bindings` with no keys, so every key goes to the page.
    let mut h = Harness::start("[input]\nkeymap = \"bindings\"", &["https://a.test/"]);
    let first = h.shown();
    let document = h.document(first);
    h.click(first);
    h.engine(
        first,
        EngineEvent::OpenRequested {
            view: first,
            document,
            url: "https://pop.test/".into(),
            disposition: OpenDisposition::NewView,
        },
    );
    h.command("tab.select 2");
    let popup = h.shown();
    assert!(delivered(&h.apply(key('a'))));
    let document = h.document(popup);
    let effects = h.engine(
        popup,
        EngineEvent::CloseRequested {
            view: popup,
            document,
        },
    );
    assert!(notices(&effects)
        .iter()
        .any(|n| n.contains("came to the front by itself")));
    assert_eq!(h.shown(), first);
    h.apply(BrowserEvent::Focus(KeyboardFocus::Page));
    // However long it takes, typing doesn't reach the page...
    h.now += Duration::from_secs(60);
    assert!(!delivered(&h.apply(key('b'))));
    // ...until the user clicks it, or runs a command.
    h.click(first);
    assert!(delivered(&h.apply(key('c'))));
}

/// `name`'s key on `seat`: pressed afresh, repeating, or released.
fn key_on(seat: SeatId, code: u32, name: &str, state: ButtonState, repeat: bool) -> BrowserEvent {
    let key = match name.chars().count() {
        1 => Key::Character(name.into()),
        _ => Key::Named(name.into()),
    };
    let text = (name.chars().count() == 1).then(|| name.to_owned());
    BrowserEvent::Keyboard {
        event: InputEvent {
            seat,
            kind: InputKind::Key(KeyEvent {
                code: KeyCode(code),
                key,
                state,
                repeat,
                modifiers: Modifiers::default(),
                consumed: Modifiers::default(),
                text,
            }),
        },
        at: Instant::now(),
    }
}

/// Whether `effects` sends `view` a key.
fn delivered_to(effects: &[Effect], view: ViewId) -> bool {
    effects.iter().any(|e| {
        matches!(
            e,
            Effect::Engine {
                view: v,
                command: EngineCommand::Input(InputEvent {
                    kind: InputKind::Key(_),
                    ..
                }),
            } if *v == view
        )
    })
}

/// A page-opened tab in front, with the keyboard: returns the opener's view and the tab's.
fn popup_in_front(h: &mut Harness) -> (ViewId, ViewId) {
    let first = h.shown();
    let document = h.document(first);
    h.click(first);
    h.engine(
        first,
        EngineEvent::OpenRequested {
            view: first,
            document,
            url: "https://pop.test/".into(),
            disposition: OpenDisposition::NewView,
        },
    );
    h.command("tab.select 2");
    h.apply(BrowserEvent::Focus(KeyboardFocus::Page));
    (first, h.shown())
}

/// The popup closes itself: the opener comes to the front by itself, its click long past.
fn popup_closes(h: &mut Harness, popup: ViewId) {
    let document = h.document(popup);
    let effects = h.engine(
        popup,
        EngineEvent::CloseRequested {
            view: popup,
            document,
        },
    );
    assert!(notices(&effects)
        .iter()
        .any(|n| n.contains("came to the front by itself")));
    h.now += Duration::from_secs(6);
}

#[test]
fn enter_held_while_its_tab_closes_itself_reaches_no_other_page() {
    // Enter is `page.send-key enter`, as in the example configuration's NORMAL, and each of its
    // repeats runs it again. Every other key goes to the page.
    let config = "[input]\nkeymap = \"bindings\"\n[keys]\n\"enter\" = \"page.send-key enter\"";
    let mut h = Harness::start(config, &["https://a.test/"]);
    let (first, popup) = popup_in_front(&mut h);
    let seat = SeatId::next();
    let enter = |state, repeat| key_on(seat, 28, "Enter", state, repeat);
    assert!(delivered_to(
        &h.apply(enter(ButtonState::Pressed, false)),
        popup
    ));
    popup_closes(&mut h, popup);
    assert_eq!(h.shown(), first);
    for _ in 0..3 {
        assert!(!delivered(&h.apply(enter(ButtonState::Pressed, true))));
    }
    assert!(!delivered(&h.apply(enter(ButtonState::Released, false))));
    // The repeats were no key press in the page: it may not open a tab...
    let document = h.document(first);
    h.engine(
        first,
        EngineEvent::OpenRequested {
            view: first,
            document,
            url: "https://b.test/".into(),
            disposition: OpenDisposition::NewView,
        },
    );
    assert_eq!(h.state.tabs().len(), 1);
    // ...and they lifted no hold: typing still doesn't reach it.
    let b = |state| key_on(seat, 48, "b", state, false);
    assert!(!delivered(&h.apply(b(ButtonState::Pressed))));
    assert!(!delivered(&h.apply(b(ButtonState::Released))));
    // Enter pressed afresh is a command the user ran: it takes the page, and reaches it.
    assert!(delivered_to(
        &h.apply(enter(ButtonState::Pressed, false)),
        first
    ));
    h.apply(enter(ButtonState::Released, false));
    assert!(delivered_to(&h.apply(b(ButtonState::Pressed)), first));
}

#[test]
fn a_key_the_chrome_got_does_not_repeat_into_the_tab_it_opened() {
    // No bindings: Enter goes to the address line, which runs a command that opens a tab.
    let mut h = Harness::start("[input]\nkeymap = \"bindings\"", &["https://a.test/"]);
    h.apply(BrowserEvent::Focus(KeyboardFocus::Chrome));
    let seat = SeatId::next();
    let enter = |state, repeat| key_on(seat, 28, "Enter", state, repeat);
    let effects = h.apply(enter(ButtonState::Pressed, false));
    assert!(effects.iter().any(|e| matches!(e, Effect::ChromeInput(_))));
    assert!(!delivered(&effects));
    for command in ["tab.new https://b.test/", "page.open https://c.test/"] {
        h.command(command);
        let shown = h.shown();
        assert!(!delivered(&h.apply(enter(ButtonState::Pressed, true))));
        assert!(!delivered(&h.apply(enter(ButtonState::Pressed, true))));
        // Its own keys do reach it.
        let c = key_on(seat, 46, "c", ButtonState::Pressed, false);
        assert!(delivered_to(&h.apply(c), shown), "{command}");
    }
    assert!(!delivered(&h.apply(enter(ButtonState::Released, false))));
}

fn config_write() -> Service {
    Service::Page(PageRequest::ConfigWrite {
        text: "version = 1".into(),
        replaces: String::new(),
    })
}

fn window_focus(focus: FocusEvent) -> BrowserEvent {
    BrowserEvent::Keyboard {
        event: InputEvent {
            seat: SeatId::next(),
            kind: InputKind::Focus(focus),
        },
        at: Instant::now(),
    }
}

#[test]
fn an_unfocused_window_writes_no_configuration_and_opens_no_terminal() {
    let on = "[pages.terminal]\nenabled = true";
    for (url, service) in [
        ("sig://config", config_write()),
        ("sig://terminal", pty_open()),
    ] {
        let mut h = Harness::start(on, &[url]);
        let view = h.shown();
        h.click(view);
        h.apply(window_focus(FocusEvent::Deactivated));
        let (id, effects) = h.ask(view, Some(url), service.clone());
        assert_eq!(
            answer(&effects, id),
            Some(&Err(ServiceError::Denied)),
            "{url}"
        );
        // The refusal used nothing up: back in front, the click still counts.
        h.apply(window_focus(FocusEvent::Activated));
        let (id, effects) = h.ask(view, Some(url), service.clone());
        assert!(performs(&effects, id), "{url}");
        // The terminal reports no focus unless it was asked to: there the window has it.
        let env = Environment {
            frontend: Frontend::Terminal,
            ..environment()
        };
        let mut h = Harness::with(on, env);
        h.apply(BrowserEvent::Start {
            session: None,
            open: vec![OpenRequest {
                input: url.into(),
                engine: None,
            }],
        });
        let view = h.shown();
        h.click(view);
        let (id, effects) = h.ask(view, Some(url), service);
        assert!(performs(&effects, id), "{url} in the terminal");
    }
}
