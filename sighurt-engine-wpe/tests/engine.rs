//! The WPE adapter against a real compositor and a real WebKit (plan §25, engine tests): the
//! navigation ids and history WebKit's back-forward list gives (same-document entries included),
//! failed navigations, hidden views, a crashed web process, an unsupported target and the
//! runtime started twice each have their own outcome; what a page may only ask for (new tabs,
//! files, permissions, downloads, other schemes, closing) reaches the browser as requests that
//! fail closed; a page asks before it goes where the user's click takes it, and the answer
//! decides; the configured user agent is sent and seen; the display path copies nothing on
//! DMA-BUF; the profile is locked while WebKit uses it.
//!
//! WebKit runs once per process, so this is one test, one scenario after another. It needs a
//! Wayland compositor and is `#[ignore]`d: `tests/headless.sh` runs it in a private headless sway.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Driver, Layout, State};
use sighurt_api::engine::{EngineCommand, EngineError, EngineRuntime, EngineWaker, ViewOptions};
use sighurt_api::events::{EngineEvent, HistoryEffect, NavigationError, OpenDisposition};
use sighurt_api::geometry::SurfacePoint;
use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, SeatId, ViewId};
use sighurt_api::input::{
    ButtonState, InputEvent, InputKind, Modifiers, PointerButton, PointerEvent,
};
use sighurt_api::services::{DownloadSource, Permission, Service, ServiceError, ServiceReply};
use sighurt_engine_wpe::{Transport, WpeConfig, WpeRuntime};
use sighurt_wayland::PresentMode;

const WAIT: Duration = Duration::from_secs(20);
/// The user agent the runtime is configured with.
const AGENT: &str = "SighurtTest/1.0 (sighurt-engine-wpe)";

fn page(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pages")
        .join(name);
    url::Url::from_file_path(path).unwrap().to_string()
}

/// The latest commit of `view`: navigation, URL, document and effect.
fn commit(
    state: &State,
    view: ViewId,
) -> Option<(
    NavigationId,
    String,
    DocumentGeneration,
    Option<HistoryEffect>,
)> {
    state.events_of(view).rev().find_map(|event| match event {
        EngineEvent::NavigationCommitted {
            navigation,
            url,
            document,
            history,
            ..
        } => Some((*navigation, url.clone(), *document, *history)),
        _ => None,
    })
}

fn committed(state: &State, view: ViewId, navigation: NavigationId) -> bool {
    state.events_of(view).any(
        |e| matches!(e, EngineEvent::NavigationCommitted { navigation: n, .. } if *n == navigation),
    )
}

fn failed(state: &State, view: ViewId, navigation: NavigationId) -> Option<NavigationError> {
    state.events_of(view).find_map(|e| match e {
        EngineEvent::NavigationFailed {
            navigation: n,
            error,
            ..
        } if *n == navigation => Some(error.clone()),
        _ => None,
    })
}

/// The latest history of `view`.
fn history(state: &State, view: ViewId) -> Option<(Vec<String>, usize)> {
    state.events_of(view).rev().find_map(|event| match event {
        EngineEvent::HistoryChanged {
            entries, current, ..
        } => Some((entries.clone(), *current)),
        _ => None,
    })
}

fn titled(state: &State, view: ViewId, title: &str) -> bool {
    state.title(view).as_deref() == Some(title)
}

fn count(state: &State, view: ViewId, what: fn(&EngineEvent) -> bool) -> usize {
    state.events_of(view).filter(|e| what(e)).count()
}

/// A left click at `(x, y)` of `view`, as the host would send it: a user's activation.
fn click(state: &mut State, view: ViewId, x: f64, y: f64) {
    let seat = SeatId::next();
    let position = SurfacePoint { x, y };
    let modifiers = Modifiers::default();
    let mut send = |kind| {
        state.command(
            view,
            EngineCommand::Input(InputEvent {
                seat,
                kind: InputKind::Pointer(kind),
            }),
        )
    };
    send(PointerEvent::Enter { position });
    send(PointerEvent::Motion {
        position,
        modifiers,
    });
    for state in [ButtonState::Pressed, ButtonState::Released] {
        send(PointerEvent::Button {
            button: PointerButton::Left,
            state,
            position,
            modifiers,
        });
    }
}

/// Runs the loop until the input method's state satisfies `done`, for at most [`WAIT`].
fn until_ime(
    d: &mut Driver,
    ime: &mut input_method::InputMethod,
    done: fn(&input_method::State) -> bool,
) -> bool {
    let end = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < end {
        if done(ime.poll()) {
            return true;
        }
        d.run_for(Duration::from_millis(50));
    }
    false
}

/// What `view`'s pages asked to navigate to: the asking document, the request and the URL.
fn requested(state: &State, view: ViewId) -> Vec<(DocumentGeneration, RequestId, String)> {
    let requests = state.events_of(view).filter_map(|event| match event {
        EngineEvent::NavigationRequested {
            document,
            request,
            url,
            ..
        } => Some((*document, *request, url.clone())),
        _ => None,
    });
    requests.collect()
}

/// A page asks before it goes where the user's click takes it (a link, a GET form, in a frame
/// too) and goes ahead without asking otherwise (the browser's loads, scripts, frames loading,
/// POST forms); the browser's answer decides, and what is still held when the document goes or
/// the view closes never happens. `a` is hidden meanwhile.
fn the_page_asks_before_it_navigates(d: &mut Driver, server: &str, a: ViewId) {
    let url = |path: &str| format!("{server}{path}");
    let asked_now = |d: &Driver| d.state.runtime.diagnostics().asked;
    let answer = |request, allow| EngineCommand::AnswerNavigation { request, allow };
    d.state.navigations = None;
    d.state.set_visible(a, false);

    // A click on a link asks, for the link's document, and nothing loads meanwhile.
    let v = d.state.open(&url("/link?/allowed"), true);
    d.state.set_focus(Some(v));
    assert!(d.run_until(WAIT, |s| commit(s, v).is_some() && s.submitted(v) > 0));
    d.run_for(Duration::from_millis(300));
    let (first, _, document, _) = commit(&d.state, v).unwrap();
    click(&mut d.state, v, 100.0, 100.0);
    assert!(
        d.run_until(WAIT, |s| requested(s, v).len() == 1),
        "a click on a link asks"
    );
    let (by, request, to) = requested(&d.state, v)[0].clone();
    assert_eq!((by, to), (document, url("/allowed")));
    d.run_for(Duration::from_millis(500));
    assert!(
        !common::served("/allowed"),
        "nothing loads before the answer"
    );
    assert_eq!(asked_now(d), 1);
    // Allowed, it commits with the page's own id.
    d.state.command(v, answer(request, true));
    assert!(d.run_until(WAIT, |s| commit(s, v)
        .is_some_and(|c| c.1 == url("/allowed"))));
    let (own, _, _, effect) = commit(&d.state, v).unwrap();
    assert_ne!(own, first, "the page's own navigation");
    assert_eq!((effect, asked_now(d)), (Some(HistoryEffect::Push), 0));
    // The browser's own navigations never ask: back, a reload.
    let back = d.state.traverse(v, -1);
    assert!(d.run_until(WAIT, |s| committed(s, v, back)));
    let reload = d.state.reload(v);
    assert!(d.run_until(WAIT, |s| committed(s, v, reload)));
    assert_eq!(requested(&d.state, v).len(), 1);

    // Denied, nothing loads and the page stays.
    let stay = d.state.navigate(v, &url("/link?/denied"));
    assert!(d.run_until(WAIT, |s| committed(s, v, stay)));
    d.run_for(Duration::from_millis(300));
    click(&mut d.state, v, 100.0, 100.0);
    assert!(d.run_until(WAIT, |s| requested(s, v).len() == 2));
    let (_, request, _) = requested(&d.state, v)[1].clone();
    d.state.command(v, answer(request, false));
    d.run_for(Duration::from_millis(1500));
    assert!(
        !common::served("/denied"),
        "a denied navigation loads nothing"
    );
    assert_eq!((commit(&d.state, v).unwrap().0, asked_now(d)), (stay, 0));

    // A new document denies what the old one asked for, and a late answer does nothing.
    click(&mut d.state, v, 100.0, 100.0);
    assert!(d.run_until(WAIT, |s| requested(s, v).len() == 3));
    let (_, late, _) = requested(&d.state, v)[2].clone();
    let away = d.state.navigate(v, &url("/away"));
    assert!(d.run_until(WAIT, |s| committed(s, v, away)));
    assert_eq!(asked_now(d), 0, "denied with its document");
    d.state.command(v, answer(late, true));
    d.run_for(Duration::from_millis(1000));
    assert!(!common::served("/denied"));
    assert_eq!(commit(&d.state, v).unwrap().0, away);

    // A GET form the user sends asks; a script's navigation, even one a click ran, and a POST
    // form go ahead.
    for (page, to, asks) in [
        ("/get?/got", "/got", true),
        ("/script?/scripted", "/scripted", false),
        ("/post?/posted", "/posted", false),
    ] {
        let on = d.state.navigate(v, &url(page));
        assert!(d.run_until(WAIT, |s| committed(s, v, on)));
        d.run_for(Duration::from_millis(300));
        let before = requested(&d.state, v).len();
        click(&mut d.state, v, 100.0, 100.0);
        if asks {
            assert!(
                d.run_until(WAIT, |s| requested(s, v).len() > before),
                "{page} asks"
            );
            let (_, request, _) = requested(&d.state, v)[before].clone();
            d.state.command(v, answer(request, true));
        }
        assert!(
            d.run_until(WAIT, |_| common::served(to)),
            "{page} goes to {to}"
        );
        d.run_for(Duration::from_millis(300));
        assert_eq!(
            requested(&d.state, v).len(),
            before + usize::from(asks),
            "{page}"
        );
    }

    // At most 16 wait: of 17 links clicked in 17 frames, the 17th goes ahead without asking.
    // The frames loading ask nothing.
    let frames = d.state.navigate(v, &url("/frames?17"));
    assert!(d.run_until(WAIT, |s| committed(s, v, frames)));
    d.run_for(Duration::from_millis(1000));
    let before = requested(&d.state, v).len();
    for i in 0..17 {
        click(&mut d.state, v, 100.0, 15.0 + 30.0 * f64::from(i));
        d.run_for(Duration::from_millis(150));
    }
    assert!(
        d.run_until(WAIT, |_| common::served("/landed17")),
        "the 17th goes ahead"
    );
    d.run_for(Duration::from_millis(500));
    let held = requested(&d.state, v)[before..].to_vec();
    assert_eq!((held.len(), asked_now(d)), (16, 16));
    assert!(!common::served("/landed1"));
    for (_, request, _) in held {
        d.state.command(v, answer(request, true));
    }
    assert!(
        d.run_until(WAIT, |_| (1..=16)
            .all(|i| common::served(&format!("/landed{i}")))),
        "allowed, the 16 go ahead"
    );

    // Closing the view denies what it holds, and nothing of the view comes after.
    let last = d.state.navigate(v, &url("/link?/closed"));
    assert!(d.run_until(WAIT, |s| committed(s, v, last)));
    d.run_for(Duration::from_millis(300));
    click(&mut d.state, v, 100.0, 100.0);
    assert!(d.run_until(WAIT, |s| requested(s, v)
        .last()
        .is_some_and(|(_, _, to)| *to == url("/closed"))));
    d.state.close(v);
    let closed = |e: &EngineEvent| matches!(e, EngineEvent::ViewClosed { .. });
    assert!(d.run_until(WAIT, |s| s.events_of(v).any(closed)));
    d.run_for(Duration::from_millis(1000));
    assert!(!common::served("/closed"), "closing denied it");
    assert_eq!(d.state.events_of(v).skip_while(|e| !closed(e)).count(), 1);
    assert_eq!(asked_now(d), 0);

    d.state.navigations = Some(true);
    d.state.set_visible(a, true);
    d.state.set_focus(Some(a));
}

/// The requests `view` made for `service`s that `pick` selects.
fn asked(
    state: &State,
    view: ViewId,
    pick: fn(&Service) -> bool,
) -> Vec<(Service, DocumentGeneration)> {
    state
        .requests
        .iter()
        .filter(|(r, _)| r.view == view && pick(&r.service))
        .map(|(r, _)| (r.service.clone(), r.document))
        .collect()
}

#[test]
#[ignore = "needs a compositor: run tests/headless.sh"]
fn wpe_views_navigate_present_ask_and_fail_distinctly() {
    let profile = std::env::temp_dir().join(format!("sighurt-wpe-profile-{}", std::process::id()));
    let config = WpeConfig {
        profile: Some(profile.clone()),
        user_agent: Some(AGENT.into()),
        ..WpeConfig::default()
    };
    let mut d = Driver::new(config, Layout::Stack);
    // WebKit's UI-process state is global: one runtime per process.
    let again = WpeRuntime::new(WpeConfig::default(), EngineWaker::new(|| {}));
    assert!(
        matches!(again, Err(EngineError::Runtime(_))),
        "a second runtime is refused"
    );

    // A page loads, commits with the browser's id and presents.
    let a = d.state.open(&page("history.html"), true);
    assert!(
        d.run_until(WAIT, |s| s.submitted(a) > 0 && commit(s, a).is_some()),
        "the first page loads and presents"
    );
    let (first, url, doc, effect) = commit(&d.state, a).unwrap();
    assert!(url.ends_with("history.html"), "{url}");
    assert_eq!(effect, Some(HistoryEffect::Push));
    assert!(d
        .state
        .events_of(a)
        .any(|e| *e == EngineEvent::ViewReady { view: a }));
    assert!(
        d.state.events_of(a).any(
            |e| matches!(e, EngineEvent::NavigationStarted { navigation, .. } if *navigation == first)
        ),
        "the browser's navigation starts with its id"
    );

    // Same-document entries: a fragment and pushState, the page's own ids, the same document.
    assert!(
        d.run_until(WAIT, |s| history(s, a).is_some_and(|(e, _)| e.len() == 3)),
        "the page adds two same-document entries"
    );
    let (entries, current) = history(&d.state, a).unwrap();
    assert_eq!(current, 2);
    assert!(
        entries[1].ends_with("#one") && entries[2].ends_with("#two"),
        "{entries:?}"
    );
    let (own, url, same, effect) = commit(&d.state, a).unwrap();
    assert!(url.ends_with("#two"));
    assert_eq!((same, effect), (doc, Some(HistoryEffect::Push)));
    assert_ne!(own, first, "the page's own navigation has its own id");

    // A cross-document navigation: a new document and a fourth entry.
    let to_second = d.state.navigate(a, &page("second.html"));
    assert!(d.run_until(WAIT, |s| committed(s, a, to_second)));
    let (_, _, second_doc, effect) = commit(&d.state, a).unwrap();
    assert_ne!(second_doc, doc);
    assert_eq!(effect, Some(HistoryEffect::Push));
    assert_eq!(
        history(&d.state, a).map(|(e, c)| (e.len(), c)),
        Some((4, 3))
    );

    // Back into the first document (shown again: a new generation), then back within it.
    let back = d.state.traverse(a, -1);
    assert!(d.run_until(WAIT, |s| committed(s, a, back)), "back commits");
    let (_, url, back_doc, effect) = commit(&d.state, a).unwrap();
    assert!(url.ends_with("#two"), "{url}");
    assert_eq!(effect, Some(HistoryEffect::Traverse));
    assert_ne!(back_doc, second_doc);
    let back = d.state.traverse(a, -1);
    assert!(
        d.run_until(WAIT, |s| committed(s, a, back)),
        "a same-document back commits"
    );
    let (_, url, one_doc, effect) = commit(&d.state, a).unwrap();
    assert!(url.ends_with("#one"), "{url}");
    assert_eq!((one_doc, effect), (back_doc, Some(HistoryEffect::Traverse)));
    // Forward by two, to the second page: another document again.
    let forward = d.state.traverse(a, 2);
    assert!(d.run_until(WAIT, |s| committed(s, a, forward)));
    let (_, url, forward_doc, _) = commit(&d.state, a).unwrap();
    assert!(url.ends_with("second.html"), "{url}");
    assert_ne!(forward_doc, one_doc);
    // Beyond the end: cancelled, not sent to WebKit.
    let beyond = d.state.traverse(a, 1);
    d.run_for(Duration::from_millis(200));
    assert_eq!(
        failed(&d.state, a, beyond),
        Some(NavigationError::Cancelled)
    );

    // Reload: a new document in the same entry.
    let reload = d.state.reload(a);
    assert!(
        d.run_until(WAIT, |s| committed(s, a, reload)),
        "reload commits"
    );
    let (_, _, reloaded, effect) = commit(&d.state, a).unwrap();
    assert_ne!(reloaded, forward_doc);
    assert_eq!(effect, Some(HistoryEffect::Replace));
    assert_eq!(
        history(&d.state, a).unwrap().0.len(),
        4,
        "a reload adds no entry"
    );

    // Two navigations in a row: the second commits with its own id.
    let first_of_two = d.state.navigate(a, &page("plain.html"));
    let second_of_two = d.state.navigate(a, &format!("{}#x", page("second.html")));
    assert!(d.run_until(WAIT, |s| committed(s, a, second_of_two)));
    assert!(commit(&d.state, a).unwrap().1.ends_with("second.html#x"));
    println!("first of two: {:?}", failed(&d.state, a, first_of_two));

    // A page that asks to stay (beforeunload), after the user clicked in it: the browser's
    // navigation still commits, as in Servo (WebKit's own answer would cancel it unseen).
    let stay = d.state.navigate(a, &page("beforeunload.html"));
    assert!(d.run_until(WAIT, |s| committed(s, a, stay)));
    click(&mut d.state, a, 50.0, 50.0);
    assert!(
        d.run_until(WAIT, |s| titled(s, a, "clicked")),
        "the click reached the page"
    );
    let away = d.state.navigate(a, &page("plain.html"));
    assert!(
        d.run_until(WAIT, |s| committed(s, a, away)),
        "a page asking to stay doesn't keep the browser's navigation from committing"
    );

    let server = common::serve();
    let url = |path: &str| format!("{server}{path}");
    // The configured user agent is what WebKit's requests send and its pages see.
    let agent = d.state.navigate(a, &url("/agent"));
    assert!(
        d.run_until(WAIT, |s| committed(s, a, agent) && titled(s, a, AGENT)),
        "{:?}",
        d.state.title(a)
    );
    assert_eq!(common::agent_of("/agent").as_deref(), Some(AGENT));
    let on = d.state.navigate(a, &url("/start"));
    assert!(d.run_until(WAIT, |s| committed(s, a, on)));
    let (_, _, start_doc, _) = commit(&d.state, a).unwrap();

    // Failed navigations keep the document: a host that doesn't exist fails with its error, a
    // 204 response and Stop end it as cancelled.
    let nowhere = d.state.navigate(a, "http://nowhere.invalid/");
    assert!(d.run_until(WAIT, |s| failed(s, a, nowhere).is_some()));
    assert!(matches!(
        failed(&d.state, a, nowhere),
        Some(NavigationError::Load(_))
    ));
    let no_content = d.state.navigate(a, &url("/204"));
    assert!(d.run_until(WAIT, |s| failed(s, a, no_content).is_some()));
    assert_eq!(
        failed(&d.state, a, no_content),
        Some(NavigationError::Cancelled)
    );
    let slow = d.state.navigate(a, &url("/slow/stopped"));
    d.run_for(Duration::from_millis(500));
    d.state.command(a, EngineCommand::Stop);
    assert!(
        d.run_until(WAIT, |s| failed(s, a, slow).is_some()),
        "Stop ends the navigation"
    );
    assert_eq!(failed(&d.state, a, slow), Some(NavigationError::Cancelled));
    assert_eq!(
        commit(&d.state, a).unwrap().2,
        start_doc,
        "the document stays"
    );
    // An address WebKit doesn't open fails the browser's navigation.
    let mail = d.state.navigate(a, "mailto:someone@example.test");
    d.run_for(Duration::from_millis(300));
    assert!(matches!(
        failed(&d.state, a, mail),
        Some(NavigationError::Load(_))
    ));

    // Downloads: an attachment and content WebKit can't show become the browser's downloads,
    // for the document that was shown, and the navigation ends without a commit.
    let downloads = |s: &State| asked(s, a, |s| matches!(s, Service::Download { .. })).len();
    for (path, name) in [("/attachment", Some("report.txt")), ("/data.bin", None)] {
        let before = downloads(&d.state);
        let download = d.state.navigate(a, &url(path));
        assert!(
            d.run_until(WAIT, |s| failed(s, a, download).is_some()
                && downloads(s) > before),
            "{path} ends as a download"
        );
        assert_eq!(
            failed(&d.state, a, download),
            Some(NavigationError::Cancelled)
        );
        let (service, document) = asked(&d.state, a, |s| matches!(s, Service::Download { .. }))
            .pop()
            .expect("a download request");
        assert_eq!(document, start_doc);
        let Service::Download {
            source,
            suggested_name,
        } = service
        else {
            unreachable!()
        };
        assert_eq!(source, DownloadSource::Url(url(path)));
        if let Some(name) = name {
            assert_eq!(suggested_name.as_deref(), Some(name));
        }
    }

    // The page's own navigation to another scheme is handed to the browser, and so is its
    // window.close(); neither is carried out.
    let mailto = d.state.navigate(a, &url("/mailto"));
    assert!(d.run_until(WAIT, |s| committed(s, a, mailto)));
    let (_, _, mailto_doc, _) = commit(&d.state, a).unwrap();
    assert!(d.run_until(WAIT, |s| s.events_of(a).any(|e| matches!(e,
        EngineEvent::OpenRequested { url, document, disposition: OpenDisposition::CurrentView, .. }
            if url.starts_with("mailto:") && *document == mailto_doc))));
    // A page may close only a tab with one entry (WebKit's rule): a new one.
    let c = d.state.open(&url("/close"), false);
    assert!(d.run_until(WAIT, |s| s
        .events_of(c)
        .any(|e| matches!(e, EngineEvent::CloseRequested { .. }))));
    d.state.close(c);

    // Permissions are the browser's answer, for the document that asked: refused, then
    // granted.
    for grant in [false, true] {
        d.state.grant = grant;
        let geo = d.state.navigate(a, &url("/geolocation"));
        assert!(d.run_until(WAIT, |s| committed(s, a, geo)));
        assert!(
            d.run_until(WAIT, |s| s
                .title(a)
                .is_some_and(|t| t.starts_with("geolocation "))),
            "the page hears the answer"
        );
        let title = d.state.title(a).unwrap();
        println!("geolocation, granted {grant}: {title}");
        // 1 is PERMISSION_DENIED; granted, there is no position to give (2).
        assert_eq!(title == "geolocation error 1", !grant, "{title}");
        let (service, document) = asked(&d.state, a, |s| matches!(s, Service::Permission(_)))
            .pop()
            .unwrap();
        assert_eq!(service, Service::Permission(Permission::Geolocation));
        assert_eq!(document, commit(&d.state, a).unwrap().2);
    }
    // Camera and microphone requests map the same way, but WebKit asks only when a device
    // exists, and the test machines' sandboxed web process sees none.
    d.state.grant = false;

    // A tab and a file picker only for a user's click.
    let links = d.state.navigate(a, &page("links.html"));
    assert!(d.run_until(WAIT, |s| committed(s, a, links) && titled(s, a, "links")));
    d.run_for(Duration::from_millis(300));
    let (_, _, links_doc, _) = commit(&d.state, a).unwrap();
    click(&mut d.state, a, 20.0, 20.0);
    assert!(
        d.run_until(WAIT, |s| s.events_of(a).any(|e| matches!(e,
            EngineEvent::OpenRequested { url, document, disposition: OpenDisposition::NewView, .. }
                if url.ends_with("second.html") && *document == links_doc))),
        "a target=_blank link asks for a new tab"
    );
    assert!(
        commit(&d.state, a).unwrap().1.ends_with("links.html"),
        "and doesn't navigate"
    );
    d.state.files = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")];
    click(&mut d.state, a, 20.0, 80.0);
    assert!(
        d.run_until(WAIT, |s| titled(s, a, "files Cargo.toml")),
        "the picked file reaches the page: {:?}",
        d.state.title(a)
    );
    let picks = asked(&d.state, a, |s| matches!(s, Service::PickFiles { .. }));
    assert_eq!(picks.len(), 1);
    // Without a click, nothing: the page can't open the picker itself. (Not with `eval`: WebKit
    // runs the browser's scripts as a user's gesture.)
    let fragment = d.state.navigate(a, &format!("{}#pick", page("links.html")));
    assert!(d.run_until(WAIT, |s| committed(s, a, fragment)
        && titled(s, a, "clicked by script")));
    d.run_for(Duration::from_millis(500));
    assert_eq!(
        asked(&d.state, a, |s| matches!(s, Service::PickFiles { .. })).len(),
        1
    );
    let answered: Vec<_> = d
        .state
        .requests
        .iter()
        .filter(|(r, _)| matches!(r.service, Service::Permission(_)))
        .filter_map(|(_, answer)| answer.clone())
        .collect();
    assert!(
        answered.contains(&Err(ServiceError::Denied)) && answered.contains(&Ok(ServiceReply::Done))
    );

    the_page_asks_before_it_navigates(&mut d, &server, a);

    // A hidden view (a background tab) loads but presents nothing; shown, it presents.
    let b = d.state.open(&page("second.html"), false);
    assert!(
        d.run_until(WAIT, |s| commit(s, b).is_some()),
        "a hidden view loads"
    );
    d.run_for(Duration::from_millis(500));
    assert_eq!(d.state.submitted(b), 0, "a hidden view presents nothing");
    let a_frames = d.state.submitted(a);
    d.state.set_visible(a, false);
    d.state.set_visible(b, true);
    d.state.set_focus(Some(b));
    assert!(
        d.run_until(WAIT, |s| s.submitted(b) > 0),
        "a shown view presents"
    );
    d.run_for(Duration::from_millis(500));
    assert_eq!(
        d.state.submitted(a),
        a_frames,
        "a hidden view stops presenting"
    );

    // A crashed web process fails its view's document, not the view or the others; a reload
    // brings it back with a new document.
    d.state.set_visible(a, true);
    d.state.layout = Layout::Split;
    d.state.place();
    d.state.runtime.terminate_web_process(b).unwrap();
    assert!(d.run_until(WAIT, |s| s
        .events_of(b)
        .any(|e| matches!(e, EngineEvent::ViewFailed { .. }))));
    let (_, _, before_crash, _) = commit(&d.state, b).unwrap();
    let a_frames = d.state.submitted(a);
    let still = d.state.navigate(a, &page("plain.html"));
    assert!(
        d.run_until(WAIT, |s| committed(s, a, still)
            && s.submitted(a) > a_frames),
        "the other view goes on"
    );
    let reload = d.state.reload(b);
    assert!(
        d.run_until(WAIT, |s| committed(s, b, reload)),
        "a reload starts a new web process"
    );
    assert_ne!(commit(&d.state, b).unwrap().2, before_crash);
    let b_frames = d.state.submitted(b);
    assert!(
        d.run_until(WAIT, |s| s.submitted(b) > b_frames),
        "and presents"
    );
    let failures = |e: &EngineEvent| {
        matches!(
            e,
            EngineEvent::ViewClosed { .. } | EngineEvent::RuntimeFailed { .. }
        )
    };
    assert_eq!(
        count(&d.state, a, failures) + count(&d.state, b, failures),
        0
    );

    // A real input method (zwp_input_method_v2, through the host's text input): it sees the
    // field, its composition shows and its commit is typed; a password field reaches it as a
    // password, without its text.
    d.state.close(b);
    d.state.layout = Layout::Stack;
    d.state.set_focus(Some(a));
    let input = d.state.navigate(a, &page("input.html"));
    assert!(d.run_until(WAIT, |s| committed(s, a, input)));
    let mut ime = input_method::InputMethod::new();
    assert!(
        until_ime(&mut d, &mut ime, |s| s.active),
        "the input method is active for the field"
    );
    assert_eq!(ime.poll().purpose, Some(input_method::NORMAL));
    ime.preedit("å");
    d.run_for(Duration::from_millis(300));
    ime.commit("åäö");
    assert!(
        d.run_until(WAIT, |s| titled(s, a, "field åäö")),
        "the commit is typed: {:?}",
        d.state.title(a)
    );
    assert!(
        until_ime(&mut d, &mut ime, |s| s.surrounding.as_deref()
            == Some("åäö")),
        "it sees the field's text: {:?}",
        ime.poll().surrounding
    );
    let password = d.state.navigate(a, &page("password.html"));
    assert!(d.run_until(WAIT, |s| committed(s, a, password)));
    until_ime(&mut d, &mut ime, |s| {
        s.purpose == Some(input_method::PASSWORD)
    });
    let seen = ime.poll();
    assert_eq!(seen.purpose, Some(input_method::PASSWORD));
    assert_eq!(
        seen.hint & input_method::SENSITIVE_AND_HIDDEN,
        input_method::SENSITIVE_AND_HIDDEN
    );
    ime.commit("secret");
    assert!(
        d.run_until(WAIT, |s| titled(s, a, "password 6")),
        "{:?}",
        d.state.title(a)
    );
    assert_ne!(
        ime.poll().surrounding.as_deref(),
        Some("secret"),
        "no password text"
    );
    drop(ime);

    // A target WebKit can't present to is refused, and the runtime goes on.
    let lease = d
        .state
        .host
        .create_target(d.state.window, PresentMode::Shm, Some(ViewId::next()))
        .unwrap();
    let refused = d
        .state
        .runtime
        .open_view(ViewId::next(), ViewOptions::default(), lease);
    assert!(
        matches!(refused, Err(EngineError::Unsupported(_))),
        "{refused:?}"
    );

    // The closed view is gone.
    let closed = |e: &EngineEvent| matches!(e, EngineEvent::ViewClosed { .. });
    assert!(d.run_until(WAIT, |s| s.events_of(b).any(closed)));
    assert_eq!(d.state.runtime.diagnostics().views, 1);

    let diagnostics = d.state.runtime.diagnostics();
    println!("{diagnostics:?}");
    println!("renderer: {:?}", diagnostics.renderer());
    assert!(diagnostics.frames() > 0);
    match diagnostics.transport {
        Some(Transport::Dmabuf) => assert_eq!(diagnostics.present.shm_copies, 0, "no copies"),
        Some(Transport::Shm) => assert!(diagnostics.present.shm_copies > 0, "copies counted"),
        None => panic!("no transport"),
    }
    assert_eq!(diagnostics.profile.as_deref(), Some(&*profile));

    // The runtime holds the profile's lock until it has shut down.
    let lock = std::fs::File::open(profile.join("lock")).unwrap();
    assert!(
        lock.try_lock().is_err(),
        "the profile is locked while WebKit runs"
    );
    let state = d.shutdown();
    assert!(!state.host.is_lost(), "no protocol error");
    assert_eq!(state.runtime.diagnostics().views, 0);
    lock.try_lock()
        .expect("the profile is free once the runtime has shut down");
    std::fs::remove_dir_all(&profile).unwrap();
}

/// An input method for the seat, as the host's own tests drive one. Test code: blocking
/// roundtrips are fine.
mod input_method {
    use smithay_client_toolkit::reexports::protocols_misc::zwp_input_method_v2::client::{
        zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
        zwp_input_method_v2::{self, ZwpInputMethodV2},
    };
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::wl_registry::WlRegistry;
    use wayland_client::protocol::wl_seat::WlSeat;
    use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};

    /// `zwp_text_input_v3.content_purpose`: normal and password.
    pub const NORMAL: u32 = 0;
    pub const PASSWORD: u32 = 8;
    /// `zwp_text_input_v3.content_hint`: `hidden_text | sensitive_data`.
    pub const SENSITIVE_AND_HIDDEN: u32 = 0x40 | 0x80;

    #[derive(Default)]
    pub struct State {
        pub active: bool,
        pub hint: u32,
        pub purpose: Option<u32>,
        pub surrounding: Option<String>,
        dones: u32,
    }

    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as wayland_client::Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    wayland_client::delegate_noop!(State: ignore WlSeat);
    wayland_client::delegate_noop!(State: ZwpInputMethodManagerV2);

    impl Dispatch<ZwpInputMethodV2, ()> for State {
        fn event(
            state: &mut Self,
            _: &ZwpInputMethodV2,
            event: zwp_input_method_v2::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                zwp_input_method_v2::Event::Activate => state.active = true,
                zwp_input_method_v2::Event::Deactivate => state.active = false,
                zwp_input_method_v2::Event::ContentType { hint, purpose } => {
                    state.hint = match hint {
                        WEnum::Value(h) => h.bits(),
                        WEnum::Unknown(u) => u,
                    };
                    state.purpose = Some(match purpose {
                        WEnum::Value(p) => p as u32,
                        WEnum::Unknown(u) => u,
                    });
                }
                zwp_input_method_v2::Event::SurroundingText { text, .. } => {
                    state.surrounding = Some(text);
                }
                zwp_input_method_v2::Event::Done => state.dones += 1,
                _ => {}
            }
        }
    }

    pub struct InputMethod {
        _conn: Connection,
        queue: EventQueue<State>,
        im: ZwpInputMethodV2,
        state: State,
    }

    impl InputMethod {
        pub fn new() -> Self {
            let conn = Connection::connect_to_env().unwrap();
            let (globals, mut queue) = registry_queue_init::<State>(&conn).unwrap();
            let qh = queue.handle();
            let seat: WlSeat = globals.bind(&qh, 1..=1, ()).unwrap();
            let manager: ZwpInputMethodManagerV2 = globals.bind(&qh, 1..=1, ()).unwrap();
            let im = manager.get_input_method(&seat, &qh, ());
            let mut state = State::default();
            queue.roundtrip(&mut state).unwrap();
            Self {
                _conn: conn,
                queue,
                im,
                state,
            }
        }

        /// What the compositor told the input method so far.
        pub fn poll(&mut self) -> &State {
            self.queue.roundtrip(&mut self.state).unwrap();
            &self.state
        }

        pub fn preedit(&mut self, text: &str) {
            self.im
                .set_preedit_string(text.into(), 0, text.len() as i32);
            self.im.commit(self.state.dones);
            self.poll();
        }

        pub fn commit(&mut self, text: &str) {
            self.im.commit_string(text.into());
            self.im.commit(self.state.dones);
            self.poll();
        }
    }
}
