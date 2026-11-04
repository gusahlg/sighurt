//! Visited pages: which commits are visits (`Effect::Visited`), which title a visit gets, and
//! forgetting them (`Effect::ForgetVisits`, `BrowserEvent::VisitsForgotten`). No display and
//! no engine; the core is driven the way the `site` tests drive it.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use sighurt_api::engine::{EngineCapabilities, EngineCommand};
use sighurt_api::events::{EngineEvent, HistoryEffect};
use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, ViewId};
use sighurt_core::config::Config;
use sighurt_core::session::{Session, SessionSegment, SessionTab, VERSION};
use sighurt_core::{
    update, BrowserEvent, BrowserState, Effect, Environment, ForgetOutcome, OpenRequest,
};

struct FakeView {
    entries: Vec<String>,
    current: usize,
    document: DocumentGeneration,
}

struct Harness {
    state: BrowserState,
    now: Instant,
    views: BTreeMap<ViewId, FakeView>,
    /// Where the next `Navigate` ends up instead of its URL: a redirect.
    redirect: Option<String>,
}

fn environment() -> Environment {
    let capabilities = EngineCapabilities {
        history_traversal: true,
        ..EngineCapabilities::default()
    };
    Environment {
        engines: [("servo".to_owned(), capabilities)].into(),
        web_engines: vec!["servo".into()],
        engine_settings: [("servo".to_owned(), &["storage"][..])].into(),
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

/// The `Visited` effects, as (url, title, commit).
fn visits(effects: &[Effect]) -> Vec<(String, Option<String>, bool)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Visited { url, title, commit } => Some((url.clone(), title.clone(), *commit)),
            _ => None,
        })
        .collect()
}

fn committed(effects: &[Effect]) -> Vec<String> {
    visits(effects)
        .into_iter()
        .filter(|(_, _, commit)| *commit)
        .map(|(url, ..)| url)
        .collect()
}

fn forgets(effects: &[Effect]) -> Vec<(RequestId, Option<String>)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::ForgetVisits { request, host } => Some((*request, host.clone())),
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

impl Harness {
    fn new(config: &str) -> Self {
        let env = environment();
        let config = Config::parse(Some(config), &env).unwrap();
        Self {
            state: BrowserState::new(config, env),
            now: Instant::now(),
            views: BTreeMap::new(),
            redirect: None,
        }
    }

    /// Servo, with `urls` opened in tabs, the first selected.
    fn with(config: &str, urls: &[&str]) -> (Self, Vec<Effect>) {
        let mut harness = Self::new(config);
        let effects = harness.apply(BrowserEvent::Start {
            session: None,
            open: urls.iter().copied().map(open).collect(),
        });
        (harness, effects)
    }

    fn start(urls: &[&str]) -> (Self, Vec<Effect>) {
        Self::with("version = 1\n", urls)
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

    /// `event`, and everything the fake engine answers to what it causes.
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
        let events = match effect {
            Effect::OpenView { view, .. } => vec![EngineEvent::ViewReady { view: *view }],
            Effect::Engine { view, command } => self.navigate(*view, command),
            _ => return Vec::new(),
        };
        events.into_iter().map(|event| self.event(event)).collect()
    }

    fn event(&self, event: EngineEvent) -> BrowserEvent {
        BrowserEvent::Engine {
            engine: "servo".into(),
            event,
            at: self.now,
        }
    }

    fn navigate(&mut self, id: ViewId, command: &EngineCommand) -> Vec<EngineEvent> {
        let view = self.views.get_mut(&id).unwrap();
        let navigation = match command {
            EngineCommand::Navigate { navigation, url } => {
                let url = self.redirect.take().unwrap_or_else(|| url.clone());
                view.entries.truncate(view.current + 1);
                view.entries.push(url);
                view.current = view.entries.len() - 1;
                view.document = DocumentGeneration::next();
                *navigation
            }
            EngineCommand::Reload { navigation } => {
                view.document = DocumentGeneration::next();
                *navigation
            }
            EngineCommand::Traverse { navigation, delta } => {
                let target = view.current as i64 + i64::from(*delta);
                let Some(target) = usize::try_from(target)
                    .ok()
                    .filter(|&t| t < view.entries.len())
                else {
                    return Vec::new();
                };
                view.current = target;
                view.document = DocumentGeneration::next();
                *navigation
            }
            _ => return Vec::new(),
        };
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

    /// The view the active tab shows.
    fn shown(&self) -> ViewId {
        let tab = self.state.tabs().nth(self.state.active()).unwrap();
        match tab.content {
            sighurt_core::tabs::Content::View(view) => view,
            other => panic!("the tab shows no view: {other:?}"),
        }
    }

    /// The page in `view` commits `url` itself: a link (`new_document`, pushing an entry) or
    /// the history API (the same document).
    fn page_commit(
        &mut self,
        view: ViewId,
        url: &str,
        new_document: bool,
        history: HistoryEffect,
    ) -> Vec<Effect> {
        let fake = self.views.get_mut(&view).unwrap();
        if new_document {
            fake.document = DocumentGeneration::next();
        }
        if history == HistoryEffect::Push {
            fake.entries.truncate(fake.current + 1);
            fake.entries.push(url.into());
            fake.current = fake.entries.len() - 1;
        } else {
            fake.entries[fake.current] = url.into();
        }
        let event = EngineEvent::NavigationCommitted {
            view,
            navigation: NavigationId::next(),
            url: url.into(),
            document: fake.document,
            history: Some(history),
        };
        self.apply(self.event(event))
    }

    fn title(&mut self, view: ViewId, title: &str) -> Vec<Effect> {
        let event = EngineEvent::TitleChanged {
            view,
            title: Some(title.into()),
        };
        self.apply(self.event(event))
    }
}

#[test]
fn a_typed_address_is_a_visit_without_its_fragment() {
    let (_, effects) = Harness::start(&["https://example.com/a?q=1#part"]);
    assert_eq!(
        visits(&effects),
        [("https://example.com/a?q=1".to_owned(), None, true)]
    );
    let (mut harness, _) = Harness::start(&["http://example.com/"]);
    let effects = harness.apply(command("page.open https://example.org/b"));
    assert_eq!(committed(&effects), ["https://example.org/b"]);
}

#[test]
fn only_http_and_https_without_a_user_and_within_the_bound_are_visits() {
    let long = format!("https://example.com/{}", "a".repeat(8 * 1024));
    let fits = format!("https://example.com/{}", "a".repeat(8 * 1024 - 20));
    assert_eq!(fits.len(), 8 * 1024);
    for (url, visit) in [
        ("https://user:secret@example.com/", false),
        ("https://user@example.com/", false),
        ("file:///tmp/page.html", false),
        ("data:text/html,hi", false),
        ("blob:https://example.com/0a1b", false),
        ("javascript:alert(1)", false),
        ("ftp://example.com/", false),
        ("sig:home", false),
        (long.as_str(), false),
        (fits.as_str(), true),
        // The fragment goes before the bound is checked.
        (&format!("{fits}#{}", "f".repeat(100)), true),
    ] {
        // The engine ends up there, as after a redirect, whatever was asked.
        let (mut harness, _) = Harness::start(&["https://start.test/"]);
        harness.redirect = Some(url.to_owned());
        let effects = harness.apply(command("page.open https://asked.test/"));
        let expected: Vec<&str> = if visit { vec![fits.as_str()] } else { vec![] };
        assert_eq!(committed(&effects), expected, "{url:.60}");
    }
}

#[test]
fn reloads_traversals_and_same_document_commits_are_not_visits() {
    let (mut harness, _) = Harness::start(&["https://example.com/1"]);
    harness.apply(command("page.open https://example.com/2"));
    assert!(committed(&harness.apply(command("page.reload"))).is_empty());
    assert!(committed(&harness.apply(command("page.back"))).is_empty());
    assert!(committed(&harness.apply(command("page.forward"))).is_empty());
    let view = harness.shown();
    let effects = harness.page_commit(
        view,
        "https://example.com/2?tab=x",
        false,
        HistoryEffect::Push,
    );
    assert!(committed(&effects).is_empty(), "pushState");
    let effects = harness.page_commit(
        view,
        "https://example.com/2?tab=y",
        false,
        HistoryEffect::Replace,
    );
    assert!(committed(&effects).is_empty(), "replaceState");
    let effects = harness.page_commit(view, "https://example.com/3", true, HistoryEffect::Replace);
    assert!(committed(&effects).is_empty(), "location.replace");
    // A link the user followed is a visit.
    let effects = harness.page_commit(view, "https://example.com/4", true, HistoryEffect::Push);
    assert_eq!(committed(&effects), ["https://example.com/4"]);
}

#[test]
fn a_background_tab_and_a_restored_tab_are_not_visits() {
    // The second tab loads while the first is shown.
    let (mut harness, effects) =
        Harness::start(&["https://shown.test/", "https://background.test/"]);
    assert_eq!(committed(&effects), ["https://shown.test/"]);
    let shown = harness.shown();
    let background = *harness.views.keys().find(|&&v| v != shown).unwrap();
    let effects = harness.page_commit(
        background,
        "https://background.test/next",
        true,
        HistoryEffect::Push,
    );
    assert!(committed(&effects).is_empty());
    // Shown, its pages are visits.
    harness.apply(command("tab.select 2"));
    let effects = harness.page_commit(
        background,
        "https://background.test/last",
        true,
        HistoryEffect::Push,
    );
    assert_eq!(committed(&effects), ["https://background.test/last"]);

    let saved = Session {
        version: VERSION,
        active: 0,
        tabs: vec![SessionTab {
            current: 0,
            zoom: 1.0,
            segments: vec![SessionSegment {
                engine: Some("servo".into()),
                entries: vec!["https://saved.test/".into()],
                current: 0,
                title: None,
            }],
        }],
        notices: Vec::new(),
    };
    let mut harness = Harness::new("version = 1\n");
    let effects = harness.apply(BrowserEvent::Start {
        session: Some(saved),
        open: Vec::new(),
    });
    assert!(committed(&effects).is_empty(), "{:?}", visits(&effects));
    let effects = harness.apply(command("page.open https://next.test/"));
    assert_eq!(committed(&effects), ["https://next.test/"]);
}

#[test]
fn nothing_is_a_visit_with_remember_off_or_ephemeral_storage() {
    let (mut harness, effects) = Harness::with(
        "version = 1\n[history]\nremember = false\n",
        &["https://example.com/"],
    );
    assert!(visits(&effects).is_empty());
    assert!(visits(&harness.title(harness.shown(), "Example")).is_empty());

    let (mut harness, effects) = Harness::with(
        "version = 1\n[engines.servo]\nstorage = \"ephemeral\"\n",
        &["https://example.com/"],
    );
    assert!(visits(&effects).is_empty());
    assert!(visits(&harness.apply(command("page.open https://example.org/"))).is_empty());
}

#[test]
fn a_title_names_the_visit_of_its_document_only() {
    let (mut harness, _) = Harness::start(&["https://example.com/"]);
    let view = harness.shown();
    assert_eq!(
        visits(&harness.title(view, "Example")),
        [(
            "https://example.com/".to_owned(),
            Some("Example".to_owned()),
            false
        )]
    );
    // A same-document commit ends the visit: the page's later titles name nothing.
    harness.page_commit(view, "https://example.com/#top", false, HistoryEffect::Push);
    assert!(visits(&harness.title(view, "Top")).is_empty());
    // So does a reload.
    harness.apply(command("page.open https://example.com/b"));
    assert_eq!(visits(&harness.title(view, "B")).len(), 1);
    harness.apply(command("page.reload"));
    assert!(visits(&harness.title(view, "B again")).is_empty());
}

#[test]
fn a_title_after_a_forget_does_nothing() {
    let (mut harness, _) = Harness::start(&["https://www.example.com/"]);
    let view = harness.shown();
    let effects = harness.apply(command("site.forget example.com"));
    assert_eq!(forgets(&effects)[0].1.as_deref(), Some("example.com"));
    assert!(visits(&harness.title(view, "Late")).is_empty());

    // Another site's forget leaves the visit alone.
    let (mut harness, _) = Harness::start(&["https://example.com/"]);
    let view = harness.shown();
    harness.apply(command("site.forget example.org"));
    assert_eq!(visits(&harness.title(view, "Kept")).len(), 1);
    let effects = harness.apply(command("history.clear"));
    assert_eq!(forgets(&effects).len(), 1);
    assert_eq!(forgets(&effects)[0].1, None);
    assert!(visits(&harness.title(view, "Late")).is_empty());

    // Turning remember off ends it too, and it stays ended when it is on again.
    let (mut harness, _) = Harness::start(&["https://example.com/"]);
    let view = harness.shown();
    let env = environment();
    for text in [
        "version = 1\n[history]\nremember = false\n",
        "version = 1\n",
    ] {
        let config = Config::parse(Some(text), &env).unwrap();
        harness.apply(BrowserEvent::ConfigReloaded(Ok(Box::new(config))));
    }
    assert!(visits(&harness.title(view, "Late")).is_empty());
}

#[test]
fn every_forget_is_answered_with_a_notice() {
    let (mut harness, _) = Harness::with(
        "version = 1\n[history]\nremember = false\n",
        &["https://example.com/"],
    );
    // With remember off too: the file may hold what was saved before.
    let (request, host) = forgets(&harness.apply(command("history.clear")))[0].clone();
    assert_eq!(host, None);
    let answer = |harness: &mut Harness, request, outcome| {
        harness.apply(BrowserEvent::VisitsForgotten { request, outcome })
    };
    let effects = answer(&mut harness, request, ForgetOutcome::Done { removed: 3 });
    assert_eq!(notices(&effects), ["Forgot 3 visited pages."]);
    // Answered once: a second answer is not one.
    let effects = answer(&mut harness, request, ForgetOutcome::Done { removed: 3 });
    assert!(notices(&effects).is_empty());
    let effects = answer(
        &mut harness,
        RequestId::next(),
        ForgetOutcome::Done { removed: 1 },
    );
    assert!(notices(&effects).is_empty());

    // A site with no visits says nothing: the engines' answers do.
    let (request, _) = forgets(&harness.apply(command("site.forget example.com")))[0].clone();
    let effects = answer(&mut harness, request, ForgetOutcome::Done { removed: 0 });
    assert!(notices(&effects).is_empty());
    let (request, host) = forgets(&harness.apply(command("site.forget example.com")))[0].clone();
    assert_eq!(host.as_deref(), Some("example.com"));
    let effects = answer(
        &mut harness,
        request,
        ForgetOutcome::HereOnly { removed: 1 },
    );
    assert_eq!(
        notices(&effects),
        [
            "Forgot 1 visited page of example.com here only: another sig owns the history file, \
          which still holds them."
        ]
    );
    let (request, _) = forgets(&harness.apply(command("history.clear")))[0].clone();
    let effects = answer(
        &mut harness,
        request,
        ForgetOutcome::NotWritten {
            removed: 0,
            error: "disk full".into(),
        },
    );
    assert_eq!(
        notices(&effects),
        [
            "Forgot 0 visited pages here, but the history file could not be written (disk full), \
          so it may still hold them."
        ]
    );

    // A file: page's site has no visits to forget; sig: pages are refused as before.
    let (mut harness, _) = Harness::start(&["file:///tmp/a.html"]);
    assert!(forgets(&harness.apply(command("site.forget"))).is_empty());
}

#[test]
fn unanswered_forgets_are_bounded() {
    let (mut harness, _) = Harness::start(&["https://example.com/"]);
    for _ in 0..64 {
        assert_eq!(forgets(&harness.apply(command("history.clear"))).len(), 1);
    }
    let effects = harness.apply(command("history.clear"));
    assert!(forgets(&effects).is_empty());
    assert_eq!(
        notices(&effects),
        ["Too many requests to forget visited pages are still unfinished (64)."]
    );
}
