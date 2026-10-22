//! Semantic snapshots from WPE, with no display server.
//!
//! One test: WebKit's UI-process state is once per process. Ignored so `cargo test` does not
//! start WebKit. Run it the way `tests/offscreen.sh` runs the presenter test: a private D-Bus
//! session, no `WAYLAND_DISPLAY` or `DISPLAY`, and `--include-ignored --test-threads 1`.
//!
//! The view is an offscreen CPU target. Permits are presented so WebKit does not stall on an
//! unacknowledged frame. Nothing here checks pixels.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, ViewOptions};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{BufferSize, Metrics, Scale, WindowRect};
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, ViewId};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_api::semantic::{
    NodeId, NodeKind, SemanticAction, SemanticNode, SemanticSnapshot, MAX_NODES,
};
use sighurt_engine_wpe::{Offscreen, WpeConfig, WpeRuntime};

const QUIET: Duration = Duration::from_millis(400);

struct Permit {
    epoch: LayoutEpoch,
    size: BufferSize,
}

struct Mem {
    metrics: Metrics,
}

impl CpuTarget for Mem {
    type Permit = Permit;

    fn metrics(&self) -> Metrics {
        self.metrics
    }

    fn permit_epoch(permit: &Permit) -> LayoutEpoch {
        permit.epoch
    }

    fn submit(
        &mut self,
        permit: Permit,
        paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        let stride = 4 * permit.size.width as usize;
        let mut pixels = vec![0u8; stride * permit.size.height as usize];
        paint(Frame {
            pixels: &mut pixels,
            size: permit.size,
            stride,
        })?;
        Ok(())
    }
}

struct Session {
    runtime: WpeRuntime<Offscreen<Mem>>,
    view: ViewId,
    epoch: LayoutEpoch,
    size: BufferSize,
    latest: Option<SemanticSnapshot>,
    snapshots: u32,
    loading: bool,
    urls: Vec<String>,
    asked: Vec<String>,
    before: Vec<u32>,
}

impl Session {
    fn open(before: Vec<u32>) -> Self {
        let metrics = Metrics::new(
            LayoutEpoch::next(),
            WindowRect {
                x: 0,
                y: 0,
                width: 640,
                height: 480,
            },
            Scale::ONE,
        );
        let epoch = metrics.epoch();
        let size = metrics.buffer();
        let mut runtime =
            WpeRuntime::<Offscreen<Mem>>::new(WpeConfig::default(), EngineWaker::new(|| {}))
                .unwrap_or_else(|error| panic!("WpeRuntime::new: {error}"));
        let view = ViewId::next();
        runtime
            .open_view(view, ViewOptions::default(), Offscreen(Mem { metrics }))
            .unwrap_or_else(|error| panic!("open_view: {error}"));
        runtime
            .command(view, EngineCommand::SetSemantics(true))
            .unwrap_or_else(|error| panic!("set semantics: {error}"));
        Self {
            runtime,
            view,
            epoch,
            size,
            latest: None,
            snapshots: 0,
            loading: false,
            urls: Vec::new(),
            asked: Vec::new(),
            before,
        }
    }

    fn step(&mut self) {
        self.runtime
            .present(
                self.view,
                Permit {
                    epoch: self.epoch,
                    size: self.size,
                },
            )
            .unwrap_or_else(|error| panic!("present: {error}"));
        let mut events = Vec::new();
        self.runtime.pump(&mut events);
        for event in events {
            self.on_event(event);
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    fn on_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::SemanticsChanged { snapshot, .. } => {
                self.snapshots += 1;
                println!(
                    "snapshot {} nodes={} truncated={} doc={}",
                    self.snapshots,
                    snapshot.nodes.len(),
                    snapshot.truncated,
                    snapshot.document.serial()
                );
                self.latest = Some(*snapshot);
            }
            EngineEvent::LoadingChanged { loading, .. } => self.loading = loading,
            EngineEvent::NavigationStarted { url, .. } => {
                println!("navigation started {url}");
                self.urls.push(url);
            }
            EngineEvent::NavigationCommitted { url, .. } => {
                println!("navigation committed {url}");
                self.urls.push(url);
            }
            EngineEvent::NavigationRequested { request, url, .. } => {
                println!("navigation requested {url}");
                self.asked.push(url);
                self.runtime
                    .command(
                        self.view,
                        EngineCommand::AnswerNavigation {
                            request,
                            allow: true,
                        },
                    )
                    .unwrap_or_else(|error| panic!("answer navigation: {error}"));
            }
            EngineEvent::NavigationFailed { error, .. } => {
                println!("navigation failed {error}");
            }
            _ => {}
        }
    }

    fn navigate(&mut self, name: &str) {
        self.latest = None;
        self.urls.clear();
        self.asked.clear();
        self.loading = true;
        let url = page_url(name);
        println!("load {url}");
        self.runtime
            .command(
                self.view,
                EngineCommand::Navigate {
                    navigation: NavigationId::next(),
                    url,
                },
            )
            .unwrap_or_else(|error| panic!("navigate {name}: {error}"));
    }

    /// Pumps until `pred` holds and, when `quiet` is non-zero, no newer snapshot arrives for
    /// that long. A late result from the previous document does not match `pred`.
    fn wait_page(
        &mut self,
        timeout: Duration,
        quiet: Duration,
        mut pred: impl FnMut(&SemanticSnapshot) -> bool,
    ) -> bool {
        let start = Instant::now();
        let mut changed = Instant::now();
        let mut seen = self.snapshots;
        while start.elapsed() < timeout {
            self.step();
            if self.snapshots != seen {
                seen = self.snapshots;
                changed = Instant::now();
            }
            let matched = self.latest.as_ref().is_some_and(&mut pred);
            if matched && changed.elapsed() >= quiet {
                return true;
            }
        }
        false
    }

    fn show(&self) -> String {
        match &self.latest {
            Some(snapshot) => describe(snapshot),
            None => format!(
                "no snapshot; loading={} urls={:?} asked={:?}",
                self.loading, self.urls, self.asked
            ),
        }
    }

    fn wait_until_url(&mut self, needle: &str, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.urls.iter().any(|url| url.contains(needle)) {
                return true;
            }
            self.step();
        }
        self.urls.iter().any(|url| url.contains(needle))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.runtime.shutdown();
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            reap();
            if webkit_procs(&self.before).is_empty() {
                reap();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        reap();
        let left = webkit_procs(&self.before);
        if !left.is_empty() {
            eprintln!("web processes still running: {}", format_procs(&left));
        }
    }
}

fn page_url(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../sighurt-semantic/tests/pages")
        .join(name);
    let path = path
        .canonicalize()
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    format!("file://{}", path.display())
}

fn open_page(
    session: &mut Session,
    name: &str,
    timeout: Duration,
    pred: impl FnMut(&SemanticSnapshot) -> bool,
) -> Duration {
    println!("--- {name}");
    let started = Instant::now();
    session.navigate(name);
    let ok = session.wait_page(timeout, Duration::ZERO, pred);
    let elapsed = started.elapsed();
    println!(
        "{name} in {:.0} ms ({} snapshots)",
        elapsed.as_secs_f64() * 1e3,
        session.snapshots
    );
    assert!(
        ok,
        "{name} not ready after {:.0} ms\n{}",
        elapsed.as_secs_f64() * 1e3,
        session.show()
    );
    elapsed
}

fn blob(snapshot: &SemanticSnapshot) -> String {
    let mut out = String::new();
    for node in &snapshot.nodes {
        for text in [&node.text, &node.label, &node.value, &node.href]
            .into_iter()
            .flatten()
        {
            out.push_str(text);
            out.push('\n');
        }
        for option in &node.options {
            out.push_str(option);
            out.push('\n');
        }
    }
    out
}

fn text_of(snapshot: &SemanticSnapshot, id: NodeId) -> String {
    let Some(node) = snapshot.nodes.iter().find(|node| node.id == id) else {
        return String::new();
    };
    let mut parts = Vec::new();
    if let Some(text) = &node.text {
        let text = text.trim();
        if !text.is_empty() {
            parts.push(text.to_owned());
        }
    }
    for child in &node.children {
        let text = text_of(snapshot, *child);
        if !text.is_empty() {
            parts.push(text);
        }
    }
    parts.join(" ")
}

fn headings(snapshot: &SemanticSnapshot) -> Vec<(u8, String)> {
    snapshot
        .nodes
        .iter()
        .filter_map(|node| match node.kind {
            NodeKind::Heading(level) => Some((level, text_of(snapshot, node.id))),
            _ => None,
        })
        .collect()
}

fn field<'a>(
    snapshot: &'a SemanticSnapshot,
    kind: NodeKind,
    label: &str,
) -> Option<&'a SemanticNode> {
    snapshot
        .nodes
        .iter()
        .find(|node| node.kind == kind && node.label.as_deref() == Some(label))
}

fn must_field<'a>(snapshot: &'a SemanticSnapshot, kind: NodeKind, label: &str) -> &'a SemanticNode {
    field(snapshot, kind, label)
        .unwrap_or_else(|| panic!("no {kind:?} labelled {label}\n{}", describe(snapshot)))
}

fn describe(snapshot: &SemanticSnapshot) -> String {
    let mut lines = vec![format!(
        "doc={} nodes={} truncated={}",
        snapshot.document.serial(),
        snapshot.nodes.len(),
        snapshot.truncated
    )];
    for (index, node) in snapshot.nodes.iter().take(60).enumerate() {
        lines.push(format!(
            "{index} {:?} text={:?} label={:?} value={:?} href={:?} checked={:?} password={} selected={:?} options={:?}",
            node.kind,
            node.text,
            node.label,
            node.value,
            node.href,
            node.checked,
            node.password,
            node.selected,
            node.options,
        ));
    }
    if snapshot.nodes.len() > 60 {
        lines.push(format!("... {} more nodes", snapshot.nodes.len() - 60));
    }
    lines.join("\n")
}

fn assert_links(snapshot: &SemanticSnapshot) {
    let hrefs: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Link)
        .map(|node| node.href.clone().unwrap_or_default())
        .collect();
    assert_eq!(
        hrefs,
        [
            "https://example.com/abs",
            "https://example.com/dir/rel",
            "https://other.example/x",
            "mailto:ada@example.com",
            "https://example.com/dir/image.html",
        ],
        "links\n{}",
        describe(snapshot)
    );
    assert!(
        snapshot
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::Image && node.text.as_deref() == Some("A dot")),
        "image alt\n{}",
        describe(snapshot)
    );
}

fn assert_forms(snapshot: &SemanticSnapshot) {
    let text = |label| {
        let node = must_field(snapshot, NodeKind::TextField, label);
        assert!(!node.password, "{label} is a password field");
        node.value.clone().unwrap_or_default()
    };
    assert_eq!(text("Name"), "Ada");
    assert_eq!(text("Nickname"), "A");
    assert_eq!(text("City"), "Oslo");
    assert_eq!(text("postal"), "0150");
    let secret = must_field(snapshot, NodeKind::TextField, "Secret");
    assert!(secret.password, "password flag");
    assert!(secret.value.is_none(), "password value {:?}", secret.value);
    assert!(
        !blob(snapshot).contains("SIGHURT_PASSWORD_SENTINEL"),
        "password sentinel leaked\n{}",
        describe(snapshot)
    );
    let bio = must_field(snapshot, NodeKind::TextArea, "Biography");
    let bio = bio.value.as_deref().unwrap_or("");
    assert!(
        bio.contains("Line one") && bio.contains("Line two"),
        "biography {bio:?}"
    );
    assert_eq!(
        must_field(snapshot, NodeKind::Checkbox, "Yes").checked,
        Some(true)
    );
    assert_eq!(
        must_field(snapshot, NodeKind::Checkbox, "No").checked,
        Some(false)
    );
    assert_eq!(
        must_field(snapshot, NodeKind::Radio, "Red").checked,
        Some(true)
    );
    assert_eq!(
        must_field(snapshot, NodeKind::Radio, "Blue").checked,
        Some(false)
    );
    let pet = must_field(snapshot, NodeKind::Select, "Pet");
    assert_eq!(pet.options, ["Cat", "Dog", "Rat"]);
    assert_eq!(pet.selected, Some(1));
    let buttons: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Button)
        .filter_map(|node| node.text.clone())
        .collect();
    assert!(
        buttons.iter().any(|text| text == "Save"),
        "buttons {buttons:?}"
    );
    assert!(
        buttons.iter().any(|text| text == "Send"),
        "buttons {buttons:?}"
    );
}

fn name_value(snapshot: &SemanticSnapshot) -> Option<String> {
    field(snapshot, NodeKind::TextField, "Name").and_then(|node| node.value.clone())
}

struct Proc {
    pid: u32,
    ppid: u32,
    comm: String,
    cmdline: String,
    state: char,
}

fn processes() -> Vec<Proc> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let Some((ppid, state)) = stat_ppid_state(&stat) else {
            continue;
        };
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let cmdline = String::from_utf8_lossy(
            &std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default(),
        )
        .replace('\0', " ");
        out.push(Proc {
            pid,
            ppid,
            comm,
            cmdline: cmdline.trim().to_string(),
            state,
        });
    }
    out
}

fn stat_ppid_state(stat: &str) -> Option<(u32, char)> {
    let rest = stat.rsplit_once(')')?.1.trim_start();
    let mut parts = rest.split_whitespace();
    let state = parts.next()?.chars().next()?;
    let ppid = parts.next()?.parse().ok()?;
    Some((ppid, state))
}

fn is_webkit(proc: &Proc) -> bool {
    let text = format!("{} {}", proc.comm, proc.cmdline);
    text.contains("WPEWebProcess") || text.contains("WPENetworkProcess")
}

fn webkit_procs(before: &[u32]) -> Vec<Proc> {
    let all = processes();
    let root = std::process::id();
    let mut family = vec![root];
    let mut index = 0;
    while index < family.len() {
        let parent = family[index];
        for proc in &all {
            if proc.ppid == parent && !family.contains(&proc.pid) {
                family.push(proc.pid);
            }
        }
        index += 1;
    }
    all.into_iter()
        .filter(|proc| proc.state != 'Z' && is_webkit(proc))
        .filter(|proc| family.contains(&proc.pid) || !before.contains(&proc.pid))
        .collect()
}

fn format_procs(procs: &[Proc]) -> String {
    if procs.is_empty() {
        return "none".into();
    }
    procs
        .iter()
        .map(|proc| format!("{}:{}:{}", proc.pid, proc.state, proc.comm))
        .collect::<Vec<_>>()
        .join(",")
}

fn reap() {
    loop {
        let status = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
        if status <= 0 {
            break;
        }
    }
}

#[test]
#[ignore = "starts WebKit; run like tests/offscreen.sh"]
fn semantic_snapshots_and_actions_without_a_display() {
    assert!(
        std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none(),
        "this test refuses a display; run it like tests/offscreen.sh"
    );
    assert!(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(),
        "this test needs a private session bus; run it like tests/offscreen.sh"
    );
    if std::env::var_os("JSC_SIGNAL_FOR_GC").is_none() {
        // SAFETY: the test process has no other threads yet that read this variable.
        unsafe { std::env::set_var("JSC_SIGNAL_FOR_GC", "12") };
    }
    unsafe {
        std::env::remove_var("LIBGL_ALWAYS_SOFTWARE");
        std::env::remove_var("DRI_PRIME");
    }
    let before: Vec<u32> = processes().into_iter().map(|proc| proc.pid).collect();
    let mut session = Session::open(before.clone());

    println!("--- headings.html");
    session.navigate("headings.html");
    let headings_ok = session.wait_page(Duration::from_secs(45), QUIET, |snapshot| {
        headings(snapshot)
            == [
                (1, "Title".to_owned()),
                (2, "Section".to_owned()),
                (3, "Subsection".to_owned()),
                (4, "Detail".to_owned()),
                (5, "Minor".to_owned()),
                (6, "Smallest".to_owned()),
                (3, "Promoted heading".to_owned()),
            ]
    });
    assert!(headings_ok, "headings\n{}", session.show());
    // Content a script puts in the body (example.com builds its page that way) is in the snapshot.
    open_page(
        &mut session,
        "scripted.html",
        Duration::from_secs(20),
        |snapshot| headings(snapshot) == [(1, "Inserted heading".to_owned())],
    );
    {
        let snapshot = session.latest.as_ref().expect("scripted snapshot");
        let all = blob(snapshot);
        assert!(
            all.contains("Static paragraph.")
                && all.contains("Inserted paragraph.")
                && all.contains("Learn more"),
            "scripted page\n{}",
            session.show()
        );
    }
    let mark = "MUTATION_MARK_42";
    session
        .runtime
        .evaluate(
            session.view,
            &format!("document.body.appendChild(document.createTextNode('{mark}'))"),
        )
        .unwrap_or_else(|error| panic!("mutate: {error}"));
    let mutated = Instant::now();
    let mark_ok = session.wait_page(Duration::from_secs(5), Duration::ZERO, |snapshot| {
        blob(snapshot).contains(mark)
    });
    let mutation_ms = mutated.elapsed().as_secs_f64() * 1e3;
    println!("mutation mark after {mutation_ms:.0} ms");
    assert!(mark_ok, "mutation mark missing\n{}", session.show());
    assert!(
        mutated.elapsed() >= Duration::from_millis(200),
        "mutation snapshot arrived in {mutation_ms:.0} ms; the observer waits 250 ms"
    );

    open_page(
        &mut session,
        "links.html",
        Duration::from_secs(20),
        |snapshot| {
            snapshot
                .nodes
                .iter()
                .any(|node| node.href.as_deref() == Some("https://example.com/abs"))
        },
    );
    assert_links(session.latest.as_ref().expect("links snapshot"));
    let (document, link) = {
        let snapshot = session.latest.as_ref().expect("links snapshot");
        let link = snapshot
            .nodes
            .iter()
            .find(|node| node.href.as_deref() == Some("https://example.com/abs"))
            .expect("absolute link");
        (snapshot.document, link.id)
    };
    session.urls.clear();
    session.asked.clear();
    session
        .runtime
        .command(
            session.view,
            EngineCommand::Semantic {
                document,
                action: SemanticAction::Activate(link),
            },
        )
        .unwrap_or_else(|error| panic!("activate: {error}"));
    let navigated = session.wait_until_url("example.com/abs", Duration::from_secs(15));
    assert!(
        navigated,
        "activate did not navigate; urls={:?} asked={:?}",
        session.urls, session.asked
    );

    open_page(
        &mut session,
        "forms.html",
        Duration::from_secs(20),
        |snapshot| {
            field(snapshot, NodeKind::TextField, "Name")
                .is_some_and(|node| node.value.as_deref() == Some("Ada"))
                && field(snapshot, NodeKind::TextField, "Secret").is_some_and(|node| node.password)
        },
    );
    assert_forms(session.latest.as_ref().expect("forms snapshot"));
    let (document, name) = {
        let snapshot = session.latest.as_ref().expect("forms snapshot");
        let name = must_field(snapshot, NodeKind::TextField, "Name");
        (snapshot.document, name.id)
    };
    session
        .runtime
        .command(
            session.view,
            EngineCommand::Semantic {
                document,
                action: SemanticAction::SetValue {
                    node: name,
                    value: "Grace Hopper".to_owned(),
                },
            },
        )
        .unwrap_or_else(|error| panic!("set value: {error}"));
    let set = session.wait_page(Duration::from_secs(10), Duration::ZERO, |snapshot| {
        name_value(snapshot).as_deref() == Some("Grace Hopper")
    });
    assert!(set, "set value\n{}", session.show());
    let stale = DocumentGeneration::next();
    assert_ne!(stale, document);
    session
        .runtime
        .command(
            session.view,
            EngineCommand::Semantic {
                document: stale,
                action: SemanticAction::SetValue {
                    node: name,
                    value: "SHOULD_NOT_STICK".to_owned(),
                },
            },
        )
        .unwrap_or_else(|error| panic!("stale set value: {error}"));
    let stale_from = Instant::now();
    while stale_from.elapsed() < Duration::from_millis(800) {
        session.step();
        let snapshot = session.latest.as_ref().expect("forms snapshot");
        assert_eq!(
            name_value(snapshot).as_deref(),
            Some("Grace Hopper"),
            "stale action wrote the field\n{}",
            describe(snapshot)
        );
    }

    open_page(
        &mut session,
        "hidden.html",
        Duration::from_secs(20),
        |snapshot| {
            let text = blob(snapshot);
            text.contains("Visible") && text.contains("but this shows")
        },
    );
    let hidden = blob(session.latest.as_ref().expect("hidden snapshot"));
    assert!(
        !hidden.contains("SIGHURT_HIDDEN_SENTINEL"),
        "hidden sentinel leaked\n{}",
        session.show()
    );

    let huge = open_page(
        &mut session,
        "huge-list.html",
        Duration::from_secs(8),
        |snapshot| {
            snapshot.truncated
                && snapshot.nodes.len() <= MAX_NODES
                && blob(snapshot).contains("Many items")
                && blob(snapshot).contains("item 0")
        },
    );
    let huge_nodes = session.latest.as_ref().expect("huge snapshot").nodes.len();
    println!(
        "huge list truncated in {:.0} ms with {huge_nodes} nodes",
        huge.as_secs_f64() * 1e3
    );
    assert!(
        huge < Duration::from_secs(8),
        "huge list took {:.0} ms",
        huge.as_secs_f64() * 1e3
    );

    open_page(
        &mut session,
        "harden.html",
        Duration::from_secs(20),
        |snapshot| {
            field(snapshot, NodeKind::TextField, "Throwing").is_some_and(|node| node.password)
                && field(snapshot, NodeKind::TextField, "Locked")
                    .is_some_and(|node| node.value.as_deref() == Some("keep-disabled"))
                && field(snapshot, NodeKind::TextField, "Frozen")
                    .is_some_and(|node| node.value.as_deref() == Some("keep-readonly"))
                && snapshot
                    .nodes
                    .iter()
                    .any(|node| node.kind == NodeKind::Button && node.text.as_deref() == Some("Go"))
        },
    );
    let harden = session.latest.as_ref().expect("harden snapshot");
    assert!(
        !blob(harden).contains("SIGHURT_THROWING_PASSWORD_SENTINEL"),
        "throwing password leaked\n{}",
        describe(harden)
    );
    let (document, locked, frozen, go) = {
        let snapshot = session.latest.as_ref().expect("harden snapshot");
        let locked = must_field(snapshot, NodeKind::TextField, "Locked");
        assert!(locked.disabled, "locked disabled flag");
        let frozen = must_field(snapshot, NodeKind::TextField, "Frozen");
        let go = snapshot
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Button && node.text.as_deref() == Some("Go"))
            .expect("Go button");
        (snapshot.document, locked.id, frozen.id, go.id)
    };
    for (node, value, stuck) in [
        (locked, "changed-disabled", "keep-disabled"),
        (frozen, "changed-readonly", "keep-readonly"),
    ] {
        session
            .runtime
            .command(
                session.view,
                EngineCommand::Semantic {
                    document,
                    action: SemanticAction::SetValue {
                        node,
                        value: value.to_owned(),
                    },
                },
            )
            .unwrap_or_else(|error| panic!("set {stuck}: {error}"));
        let from = Instant::now();
        let label = if stuck == "keep-disabled" {
            "Locked"
        } else {
            "Frozen"
        };
        while from.elapsed() < Duration::from_millis(800) {
            session.step();
            let snapshot = session.latest.as_ref().expect("harden snapshot");
            let current = field(snapshot, NodeKind::TextField, label);
            assert_eq!(
                current.and_then(|item| item.value.as_deref()),
                Some(stuck),
                "wrote a refused field\n{}",
                describe(snapshot)
            );
        }
    }
    session.urls.clear();
    session
        .runtime
        .command(
            session.view,
            EngineCommand::Semantic {
                document,
                action: SemanticAction::Submit(go),
            },
        )
        .unwrap_or_else(|error| panic!("submit: {error}"));
    let submitted = Instant::now();
    while submitted.elapsed() < Duration::from_millis(1000) {
        session.step();
        assert!(
            !session
                .urls
                .iter()
                .any(|url| url.contains("sighurt-harden-submitted")),
            "invalid submit navigated: {:?}",
            session.urls
        );
    }

    open_page(
        &mut session,
        "shadow.html",
        Duration::from_secs(20),
        |snapshot| {
            let text = blob(snapshot);
            text.contains("Open shadow text")
                && text.contains("Slotted light")
                && text.contains("Fallback text")
        },
    );
    let shadow = blob(session.latest.as_ref().expect("shadow snapshot"));
    assert!(
        !shadow.contains("SIGHURT_CLOSED_SHADOW_SENTINEL"),
        "closed shadow leaked\n{}",
        session.show()
    );

    open_page(
        &mut session,
        "iframe.html",
        Duration::from_secs(30),
        |snapshot| {
            let text = blob(snapshot);
            text.contains("Inner heading")
                && text.contains("Same origin body")
                && text.contains("https://example.com/sighurt-cross-frame")
        },
    );
    let frames = blob(session.latest.as_ref().expect("iframe snapshot"));
    assert!(
        !frames.contains("Cross origin body"),
        "cross-origin frame leaked\n{}",
        session.show()
    );

    open_page(
        &mut session,
        "inherit.html",
        Duration::from_secs(20),
        |snapshot| {
            field(snapshot, NodeKind::TextField, "Inside")
                .is_some_and(|node| node.value.as_deref() == Some("keep-fieldset"))
                && field(snapshot, NodeKind::Select, "Choice").is_some_and(|node| {
                    node.options.iter().any(|option| option == "Beta")
                        && node.options.iter().any(|option| option == "Gamma")
                        && node.selected == Some(0)
                })
                && field(snapshot, NodeKind::TextField, "Untyped").is_some_and(|node| node.password)
                && !blob(snapshot).contains("SIGHURT_UNDEFINED_TYPE_SENTINEL")
        },
    );
    let (document, inside, choice, beta, gamma) = {
        let snapshot = session.latest.as_ref().expect("inherit snapshot");
        let inside = must_field(snapshot, NodeKind::TextField, "Inside");
        let choice = must_field(snapshot, NodeKind::Select, "Choice");
        let beta = choice
            .options
            .iter()
            .position(|option| option == "Beta")
            .expect("Beta") as u32;
        let gamma = choice
            .options
            .iter()
            .position(|option| option == "Gamma")
            .expect("Gamma") as u32;
        assert!(
            !blob(snapshot).contains("SIGHURT_UNDEFINED_TYPE_SENTINEL"),
            "untyped input exported its value\n{}",
            describe(snapshot)
        );
        (snapshot.document, inside.id, choice.id, beta, gamma)
    };
    session
        .runtime
        .command(
            session.view,
            EngineCommand::Semantic {
                document,
                action: SemanticAction::SetValue {
                    node: inside,
                    value: "changed-fieldset".to_owned(),
                },
            },
        )
        .unwrap_or_else(|error| panic!("set fieldset child: {error}"));
    let fieldset_from = Instant::now();
    while fieldset_from.elapsed() < Duration::from_millis(800) {
        session.step();
        let snapshot = session.latest.as_ref().expect("inherit snapshot");
        assert_eq!(
            field(snapshot, NodeKind::TextField, "Inside").and_then(|node| node.value.as_deref()),
            Some("keep-fieldset"),
            "fieldset child accepted SetValue\n{}",
            describe(snapshot)
        );
    }
    for index in [beta, gamma] {
        session
            .runtime
            .command(
                session.view,
                EngineCommand::Semantic {
                    document,
                    action: SemanticAction::SelectOption {
                        node: choice,
                        index,
                    },
                },
            )
            .unwrap_or_else(|error| panic!("select {index}: {error}"));
        let select_from = Instant::now();
        while select_from.elapsed() < Duration::from_millis(800) {
            session.step();
            let snapshot = session.latest.as_ref().expect("inherit snapshot");
            assert_eq!(
                field(snapshot, NodeKind::Select, "Choice").and_then(|node| node.selected),
                Some(0),
                "selected a disabled option {index}\n{}",
                describe(snapshot)
            );
        }
    }

    let before = session.before.clone();
    drop(session);
    let left = webkit_procs(&before);
    assert!(
        left.is_empty(),
        "web processes still running: {}",
        format_procs(&left)
    );
}
