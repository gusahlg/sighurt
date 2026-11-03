//! `site.forget`: the host it names, the notice each engine's answer produces, and which tabs
//! that answer reloads. No display and no engine; the core is driven the way the `browser`
//! tests drive it.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, SiteDataKinds, SiteDataRequest, SiteKey,
};
use sighurt_api::events::{EngineEvent, SiteDataOutcome};
use sighurt_api::ids::{DocumentGeneration, RequestId, ViewId};
use sighurt_core::config::Config;
use sighurt_core::session::{Session, SessionSegment, SessionTab, VERSION};
use sighurt_core::{update, BrowserEvent, BrowserState, Effect, Environment, OpenRequest};

struct FakeView {
    entries: Vec<String>,
    current: usize,
    document: DocumentGeneration,
}

struct Harness {
    state: BrowserState,
    now: Instant,
    views: BTreeMap<ViewId, FakeView>,
}

fn kinds() -> SiteDataKinds {
    SiteDataKinds {
        cookies: true,
        storage: true,
        caches: false,
        hsts: false,
    }
}

fn capabilities(site_data: bool) -> EngineCapabilities {
    EngineCapabilities {
        history_traversal: true,
        page_zoom: true,
        site_data: if site_data {
            kinds()
        } else {
            SiteDataKinds::NONE
        },
        ..EngineCapabilities::default()
    }
}

/// `keepers` keep cookies and storage. Every other name keeps none.
fn environment(keepers: &[&str], others: &[&str]) -> Environment {
    let engines = keepers
        .iter()
        .map(|name| ((*name).to_owned(), capabilities(true)))
        .chain(
            others
                .iter()
                .map(|name| ((*name).to_owned(), capabilities(false))),
        )
        .collect();
    Environment {
        engines,
        ..Environment::default()
    }
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
    fn new(env: Environment) -> Self {
        let config =
            Config::parse(Some("version = 1\n[engines]\ndefault = \"servo\"\n"), &env).unwrap();
        Self {
            state: BrowserState::new(config, env),
            now: Instant::now(),
            views: BTreeMap::new(),
        }
    }

    /// Servo keeps site data. `urls` are loaded in it.
    fn servo(urls: &[&str]) -> Self {
        let mut harness = Self::new(environment(&["servo"], &["demo"]));
        harness.apply(BrowserEvent::Start {
            session: None,
            open: urls.iter().copied().map(open).collect(),
        });
        harness
    }

    fn update(&mut self, event: BrowserEvent) -> Vec<Effect> {
        let effects = update(&mut self.state, event);
        for effect in &effects {
            if let Effect::OpenView { view, .. } = effect {
                self.views.insert(
                    *view,
                    FakeView {
                        entries: Vec::new(),
                        current: 0,
                        document: DocumentGeneration::next(),
                    },
                );
            }
        }
        effects
    }

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

    fn answer(&mut self, effect: &Effect) -> Vec<BrowserEvent> {
        let (view, events) = match effect {
            Effect::OpenView { view, .. } => (*view, vec![EngineEvent::ViewReady { view: *view }]),
            Effect::Engine { view, command } => (*view, self.navigate(*view, command)),
            _ => return Vec::new(),
        };
        let engine = self
            .state
            .view(view)
            .map(|view| view.engine().to_owned())
            .unwrap_or_else(|| "servo".into());
        events
            .into_iter()
            .map(|event| BrowserEvent::Engine {
                engine: engine.clone(),
                event,
                at: self.now,
            })
            .collect()
    }

    fn navigate(&mut self, id: ViewId, command: &EngineCommand) -> Vec<EngineEvent> {
        let view = self.views.get_mut(&id).unwrap();
        let navigation = match command {
            EngineCommand::Navigate { navigation, url } => {
                view.entries.truncate(view.current + 1);
                view.entries.push(url.clone());
                view.current = view.entries.len() - 1;
                view.document = DocumentGeneration::next();
                *navigation
            }
            EngineCommand::Reload { navigation } => {
                view.document = DocumentGeneration::next();
                *navigation
            }
            _ => return Vec::new(),
        };
        vec![EngineEvent::NavigationCommitted {
            view: id,
            navigation,
            url: view.entries[view.current].clone(),
            document: view.document,
            history: None,
        }]
    }

    fn done(
        &mut self,
        engine: &str,
        request: RequestId,
        result: Result<SiteDataOutcome, String>,
    ) -> Vec<Effect> {
        self.update(BrowserEvent::Engine {
            engine: engine.into(),
            event: EngineEvent::SiteDataDone { request, result },
            at: self.now,
        })
    }
}

fn outcome(removed: &[&str], profile: bool) -> SiteDataOutcome {
    SiteDataOutcome {
        removed: removed.iter().map(|name| (*name).to_owned()).collect(),
        kinds: kinds(),
        durable: false,
        profile,
    }
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

fn requests(effects: &[Effect]) -> Vec<SiteDataRequest> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SiteData { engine, request } => {
                assert_eq!(*engine, None, "site.forget asks every engine");
                Some(request.clone())
            }
            _ => None,
        })
        .collect()
}

fn host_of(request: &SiteDataRequest) -> &SiteKey {
    match request {
        SiteDataRequest::Forget { site, .. } => site,
        other => panic!("not a forget: {other:?}"),
    }
}

fn reloaded(harness: &Harness, effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Engine {
                view,
                command: EngineCommand::Reload { .. },
            } => harness.views[view]
                .entries
                .get(harness.views[view].current)
                .cloned(),
            _ => None,
        })
        .collect()
}

#[test]
fn the_host_comes_from_the_page_or_from_the_argument() {
    let key = |url: &str| {
        let mut harness = Harness::servo(&[url]);
        let effects = harness.apply(command("site.forget"));
        assert!(
            notices(&effects).is_empty(),
            "{url}: {:?}",
            notices(&effects)
        );
        host_of(&requests(&effects)[0]).clone()
    };
    assert_eq!(
        key("https://example.com/a"),
        SiteKey::Host("example.com".into())
    );
    assert_eq!(
        key("https://bücher.example/a"),
        SiteKey::Host("xn--bcher-kva.example".into())
    );
    assert_eq!(key("https://[::1]/a"), SiteKey::Host("[::1]".into()));
    assert_eq!(key("http://localhost/a"), SiteKey::Host("localhost".into()));
    assert_eq!(key("http://LOCALHOST/a"), SiteKey::Host("localhost".into()));
    assert_eq!(key("http://127.0.0.1/a"), SiteKey::Host("127.0.0.1".into()));
    assert_eq!(key("file:///tmp/a"), SiteKey::LocalFiles);
    assert_eq!(key("file://localhost/tmp/a"), SiteKey::LocalFiles);

    let mut harness = Harness::servo(&["https://example.com/"]);
    let argued = |harness: &mut Harness, text: &str| {
        let effects = harness.apply(command(&format!("site.forget {text}")));
        assert!(
            notices(&effects).is_empty(),
            "{text}: {:?}",
            notices(&effects)
        );
        host_of(&requests(&effects)[0]).clone()
    };
    assert_eq!(
        argued(&mut harness, "example.com"),
        SiteKey::Host("example.com".into())
    );
    assert_eq!(
        argued(&mut harness, "Bücher.example"),
        SiteKey::Host("xn--bcher-kva.example".into())
    );
    assert_eq!(argued(&mut harness, "[::1]"), SiteKey::Host("[::1]".into()));
    assert_eq!(
        argued(&mut harness, "LOCALHOST"),
        SiteKey::Host("localhost".into())
    );

    let effects = harness.apply(command("site.forget *.example.com"));
    assert_eq!(notices(&effects), ["\"*.example.com\" is not a host name."]);
    assert!(requests(&effects).is_empty());
    let effects = harness.apply(command("site.forget https://example.com"));
    assert_eq!(
        notices(&effects),
        ["\"https://example.com\" is not a host name."]
    );
    assert!(requests(&effects).is_empty());
}

#[test]
fn a_sig_page_and_a_tab_with_no_page_are_notices() {
    let mut harness = Harness::new(environment(&["servo"], &[]));
    harness.apply(BrowserEvent::Start {
        session: None,
        open: Vec::new(),
    });
    let effects = harness.apply(command("site.forget"));
    assert_eq!(
        notices(&effects),
        ["This page is sig:, which keeps no site data."]
    );
    assert!(requests(&effects).is_empty());

    let saved = Session {
        version: VERSION,
        active: 0,
        tabs: vec![SessionTab {
            current: 0,
            zoom: 1.0,
            segments: vec![SessionSegment {
                engine: Some("servo".into()),
                entries: Vec::new(),
                current: 0,
                title: None,
            }],
        }],
        notices: Vec::new(),
    };
    let mut harness = Harness::new(environment(&["servo"], &[]));
    harness.apply(BrowserEvent::Start {
        session: Some(saved),
        open: Vec::new(),
    });
    let effects = harness.apply(command("site.forget"));
    assert_eq!(notices(&effects), ["This tab has no page to forget."]);
    assert!(requests(&effects).is_empty());
}

#[test]
fn no_engine_keeps_site_data_is_a_notice_and_nothing_is_sent() {
    let mut harness = Harness::new(environment(&[], &["servo"]));
    harness.apply(BrowserEvent::Start {
        session: None,
        open: vec![open("https://example.com/")],
    });
    let effects = harness.apply(command("site.forget"));
    assert_eq!(
        notices(&effects),
        ["No engine in this build can forget site data yet, so nothing was forgotten."]
    );
    assert!(requests(&effects).is_empty());
}

#[test]
fn the_answer_names_what_was_forgotten_and_reloads_that_page() {
    let mut harness = Harness::servo(&["https://example.com/a"]);
    let effects = harness.apply(command("site.forget"));
    let request = requests(&effects)[0].request();
    let long = "x".repeat(254);
    let effects = harness.done(
        "servo",
        request,
        Ok(outcome(
            &["example.com", long.as_str(), "Local files"],
            true,
        )),
    );
    assert_eq!(
        notices(&effects),
        ["example.com forgotten in servo: cookies, storage (removed: example.com, Local files)"]
    );
    assert_eq!(reloaded(&harness, &effects), ["https://example.com/a"]);

    // The request is finished. Another answer, from servo or from an engine that was not
    // asked, is dropped. So is a list: this slice does not ask for one.
    assert!(harness
        .done("servo", request, Ok(outcome(&["example.com"], true)))
        .is_empty());
    assert!(harness
        .done("demo", request, Ok(outcome(&["example.com"], true)))
        .is_empty());
    assert!(harness
        .update(BrowserEvent::Engine {
            engine: "servo".into(),
            event: EngineEvent::SitesListed {
                request,
                sites: Vec::new(),
                more: false,
            },
            at: harness.now,
        })
        .is_empty());
    assert!(harness
        .done(
            "servo",
            RequestId::next(),
            Ok(outcome(&["example.com"], true))
        )
        .is_empty());
}

#[test]
fn an_error_or_no_profile_says_nothing_on_disk_changed() {
    let mut harness = Harness::servo(&["https://example.com/a"]);
    let request = requests(&harness.apply(command("site.forget")))[0].request();
    let effects = harness.done("servo", request, Err("the jar is locked".into()));
    assert_eq!(
        notices(&effects),
        ["example.com: nothing on disk changed in servo (the jar is locked)"]
    );
    assert!(reloaded(&harness, &effects).is_empty());

    let request = requests(&harness.apply(command("site.forget")))[0].request();
    let effects = harness.done("servo", request, Ok(outcome(&["example.com"], false)));
    assert_eq!(
        notices(&effects),
        ["example.com: nothing on disk changed in servo (no profile)"]
    );
    // The page is reloaded anyway, so it cannot write the data back into memory.
    assert_eq!(reloaded(&harness, &effects), ["https://example.com/a"]);

    let request = requests(&harness.apply(command("site.forget")))[0].request();
    let effects = harness.done("servo", request, Ok(outcome(&vec!["a"; 1025], true)));
    let said = notices(&effects);
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("nothing on disk changed"), "{said:?}");
    assert!(said[0].contains("1025 sites"), "{said:?}");
    assert!(reloaded(&harness, &effects).is_empty());
}

#[test]
fn exactly_the_matching_tabs_of_that_engine_reload() {
    let mut harness = Harness::new(environment(&["servo", "wpe"], &["demo"]));
    let pages = [
        "https://example.com/a",
        "https://www.example.com/b",
        "https://login.example.com/c",
        "https://other.test/d",
        "https://notexample.com/e",
        "https://elsewhere.test/f",
    ];
    harness.apply(BrowserEvent::Start {
        session: None,
        open: pages.into_iter().map(open).collect(),
    });
    for input in ["https://example.com/w", "https://other.test/w"] {
        harness.apply(BrowserEvent::Open(OpenRequest {
            input: input.into(),
            engine: Some("wpe".into()),
        }));
    }
    let effects = harness.apply(command("site.forget example.com"));
    let request = requests(&effects)[0].request();
    assert!(
        matches!(host_of(&requests(&effects)[0]), SiteKey::Host(host) if host == "example.com")
    );

    let effects = harness.done(
        "servo",
        request,
        Ok(outcome(&["example.com", "other.test"], true)),
    );
    assert_eq!(
        notices(&effects),
        ["example.com forgotten in servo: cookies, storage (removed: example.com, other.test)"]
    );
    assert_eq!(
        reloaded(&harness, &effects),
        [
            "https://example.com/a",
            "https://www.example.com/b",
            "https://login.example.com/c",
            "https://other.test/d",
        ]
    );
    // Servo already answered. Saying it again does nothing, and wpe's pages are still there.
    assert!(harness
        .done("servo", request, Ok(outcome(&["example.com"], true)))
        .is_empty());

    let effects = harness.done("wpe", request, Ok(outcome(&["example.com"], true)));
    assert_eq!(
        notices(&effects),
        ["example.com forgotten in wpe: cookies, storage (removed: example.com)"]
    );
    // other.test/w is in wpe, and this answer did not remove it: only example.com/w reloads.
    assert_eq!(reloaded(&harness, &effects), ["https://example.com/w"]);
    assert!(harness
        .done("wpe", request, Ok(outcome(&["example.com"], true)))
        .is_empty());
}

#[test]
fn sixteen_unfinished_forgets_is_the_limit() {
    let mut harness = Harness::servo(&["https://example.com/"]);
    let mut pending = Vec::new();
    for i in 0..16 {
        let effects = harness.apply(command(&format!("site.forget host{i}.example")));
        assert!(notices(&effects).is_empty(), "{:?}", notices(&effects));
        pending.push(requests(&effects)[0].request());
    }
    let effects = harness.apply(command("site.forget extra.example"));
    assert_eq!(
        notices(&effects),
        ["Too many site.forget requests are still unfinished (16)."]
    );
    assert!(requests(&effects).is_empty());

    harness.done("servo", pending[0], Ok(outcome(&[], true)));
    let effects = harness.apply(command("site.forget extra.example"));
    assert!(notices(&effects).is_empty(), "{:?}", notices(&effects));
    assert_eq!(
        host_of(&requests(&effects)[0]),
        &SiteKey::Host("extra.example".into())
    );
}

#[test]
fn a_failed_engine_fails_the_forgets_it_was_asked() {
    let mut harness = Harness::servo(&["https://example.com/"]);
    for i in 0..16 {
        harness.apply(command(&format!("site.forget host{i}.example")));
    }
    let effects = harness.apply(BrowserEvent::Engine {
        engine: "servo".into(),
        event: EngineEvent::RuntimeFailed {
            message: "gone".into(),
        },
        at: harness.now,
    });
    let failed = notices(&effects)
        .into_iter()
        .filter(|notice| notice.ends_with("nothing on disk changed in servo (it stopped working)"))
        .count();
    assert_eq!(failed, 16, "{:?}", notices(&effects));
    // None is left waiting, and the failed engine is asked nothing more.
    let effects = harness.apply(command("site.forget extra.example"));
    assert_eq!(
        notices(&effects),
        ["The engines that keep site data stopped working: restart sig to forget a site."]
    );
    assert!(requests(&effects).is_empty());
}

#[test]
fn local_files_reloads_file_pages() {
    let mut harness = Harness::servo(&["file:///tmp/a", "https://example.com/"]);
    harness.apply(BrowserEvent::Open(open("file:///tmp/b")));
    // The active tab is the one just opened, a file page.
    let effects = harness.apply(command("site.forget"));
    assert_eq!(host_of(&requests(&effects)[0]), &SiteKey::LocalFiles);
    let effects = harness.done(
        "servo",
        requests(&effects)[0].request(),
        Ok(outcome(&[], true)),
    );
    assert_eq!(
        notices(&effects),
        ["local files forgotten in servo: cookies, storage (removed: nothing)"]
    );
    assert_eq!(
        reloaded(&harness, &effects),
        ["file:///tmp/a", "file:///tmp/b"]
    );
}
