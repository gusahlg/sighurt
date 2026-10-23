//! Semantic snapshots from Servo, with no display server.
//!
//! One test: Servo initializes once per process. The view is an offscreen CPU target.
//! Permits are presented so Servo keeps producing frames; a DOM change shows up as a frame,
//! and that frame is what makes the runtime check its dirty flag. Nothing here checks pixels.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, PumpStatus, ViewOptions};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{BufferSize, Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_api::semantic::{NodeId, NodeKind, SemanticAction, SemanticNode, SemanticSnapshot};
use sighurt_engine_servo::{Offscreen, ServoConfig, ServoRuntime};

const LOAD_WAIT: Duration = Duration::from_secs(90);
const SNAPSHOT_WAIT: Duration = Duration::from_secs(30);
const ACTION_WAIT: Duration = Duration::from_secs(20);

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
        // Servo's readback uses `PACK_ROW_LENGTH`. A tightly packed row is a valid multiple of
        // 4, and the offscreen presenter still wants the padding the other test gives it.
        let stride = padded_stride(permit.size.width);
        let mut pixels = vec![0u8; stride * permit.size.height as usize];
        paint(Frame {
            pixels: &mut pixels,
            size: permit.size,
            stride,
        })?;
        Ok(())
    }
}

/// At least 16 bytes of padding past a tightly packed row, so `PACK_ROW_LENGTH` is not zero.
fn padded_stride(width: u32) -> usize {
    let row = 4 * width as usize;
    let aligned = row.div_ceil(16) * 16;
    if aligned == row {
        aligned + 16
    } else {
        aligned
    }
}

struct Session {
    runtime: ServoRuntime<Offscreen<Mem>>,
    view: ViewId,
    epoch: LayoutEpoch,
    size: BufferSize,
    latest: Option<SemanticSnapshot>,
    snapshots: u32,
    loading: Option<bool>,
    committed: Vec<String>,
    recent: Vec<String>,
}

impl Session {
    fn open() -> Self {
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("DISPLAY");
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let metrics = Metrics::new(
            LayoutEpoch::next(),
            WindowRect {
                x: 0,
                y: 0,
                width: 320,
                height: 180,
            },
            Scale::ONE,
        );
        let epoch = metrics.epoch();
        let size = metrics.buffer();
        let mut runtime =
            ServoRuntime::<Offscreen<Mem>>::new(ServoConfig::default(), EngineWaker::new(|| {}));
        runtime.setup_logging();
        let view = ViewId::next();
        runtime
            .open_view(view, ViewOptions::default(), Offscreen(Mem { metrics }))
            .unwrap_or_else(|error| panic!("open_view: {error:?}"));
        Self {
            runtime,
            view,
            epoch,
            size,
            latest: None,
            snapshots: 0,
            loading: None,
            committed: Vec::new(),
            recent: Vec::new(),
        }
    }

    fn command(&mut self, command: EngineCommand) {
        self.runtime
            .command(self.view, command)
            .unwrap_or_else(|error| panic!("command: {error:?}"));
    }

    fn navigate(&mut self, name: &str) {
        let page = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/pages/{name}"));
        let url = url::Url::from_file_path(&page).expect("file url");
        println!("load {url}");
        self.command(EngineCommand::Navigate {
            navigation: NavigationId::next(),
            url: url.to_string(),
        });
    }

    fn present(&mut self) {
        self.runtime
            .present(
                self.view,
                Permit {
                    epoch: self.epoch,
                    size: self.size,
                },
            )
            .unwrap_or_else(|error| panic!("present: {error:?}"));
    }

    /// One pump. A frame is presented before the next wait, so the refresh driver keeps running.
    fn pump_once(&mut self) -> (PumpStatus, bool) {
        let mut events = Vec::new();
        let status = self.runtime.pump(&mut events);
        let mut frame = false;
        for event in events {
            if matches!(event, EngineEvent::FrameReady { .. }) {
                frame = true;
            }
            self.on_event(event);
        }
        if frame {
            self.present();
        }
        (status, frame)
    }

    fn on_event(&mut self, event: EngineEvent) {
        match &event {
            EngineEvent::SemanticsChanged { snapshot, .. } => {
                self.snapshots += 1;
                println!(
                    "snapshot {} nodes={} doc={}",
                    self.snapshots,
                    snapshot.nodes.len(),
                    snapshot.document.serial()
                );
                self.recent
                    .push(format!("SemanticsChanged nodes={}", snapshot.nodes.len()));
                self.latest = Some(snapshot.as_ref().clone());
            }
            EngineEvent::LoadingChanged { loading, .. } => {
                self.loading = Some(*loading);
                self.recent.push(format!("LoadingChanged {loading}"));
            }
            EngineEvent::NavigationCommitted { url, .. } => {
                println!("committed {url}");
                self.recent.push(format!("committed {url}"));
                self.committed.push(url.clone());
            }
            EngineEvent::NavigationRequested { request, url, .. } => {
                println!("navigation requested {url}");
                self.recent.push(format!("requested {url}"));
                let request = *request;
                self.command(EngineCommand::AnswerNavigation {
                    request,
                    allow: true,
                });
            }
            EngineEvent::NavigationFailed { error, .. } => {
                println!("navigation failed {error}");
                self.recent.push(format!("failed {error}"));
            }
            other => {
                self.recent.push(format!("{other:?}"));
            }
        }
        if self.recent.len() > 24 {
            self.recent.remove(0);
        }
    }

    fn wait_until(&mut self, timeout: Duration, mut done: impl FnMut(&Self) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if done(self) {
                return true;
            }
            let (status, presented) = self.pump_once();
            if presented {
                continue;
            }
            match status {
                PumpStatus::Idle => std::thread::sleep(Duration::from_millis(10)),
                PumpStatus::Pending { deadline: None } => {}
                PumpStatus::Pending {
                    deadline: Some(deadline),
                } => {
                    let wait = deadline.saturating_duration_since(Instant::now());
                    std::thread::sleep(wait.min(Duration::from_millis(50)));
                }
            }
        }
        done(self)
    }

    fn pump_for(&mut self, duration: Duration) {
        let start = Instant::now();
        while start.elapsed() < duration {
            let (status, presented) = self.pump_once();
            if presented {
                continue;
            }
            match status {
                PumpStatus::Idle => std::thread::sleep(Duration::from_millis(10)),
                PumpStatus::Pending { deadline: None } => {}
                PumpStatus::Pending {
                    deadline: Some(deadline),
                } => {
                    let wait = deadline.saturating_duration_since(Instant::now());
                    let left = duration.saturating_sub(start.elapsed());
                    std::thread::sleep(wait.min(Duration::from_millis(50)).min(left));
                }
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.runtime.shutdown();
    }
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

fn heading(snapshot: &SemanticSnapshot) -> Option<String> {
    snapshot.nodes.iter().find_map(|node| match node.kind {
        NodeKind::Heading(1) => Some(text_of(snapshot, node.id)),
        _ => None,
    })
}

fn link(snapshot: &SemanticSnapshot) -> Option<&SemanticNode> {
    snapshot.nodes.iter().find(|node| {
        node.kind == NodeKind::Link
            && node
                .href
                .as_deref()
                .is_some_and(|href| href.contains("semantic-next.html"))
    })
}

fn field(snapshot: &SemanticSnapshot) -> Option<&SemanticNode> {
    snapshot
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::TextField && node.label.as_deref() == Some("Name"))
}

fn describe(snapshot: &SemanticSnapshot) -> String {
    let mut lines = vec![format!(
        "doc={} nodes={}",
        snapshot.document.serial(),
        snapshot.nodes.len()
    )];
    for (index, node) in snapshot.nodes.iter().take(40).enumerate() {
        lines.push(format!(
            "{index} {:?} text={:?} label={:?} value={:?} href={:?}",
            node.kind, node.text, node.label, node.value, node.href
        ));
    }
    lines.join("\n")
}

fn base_page(snapshot: &SemanticSnapshot) -> bool {
    heading(snapshot).as_deref() == Some("Servo heading")
        && blob(snapshot).contains("A paragraph of text.")
        && link(snapshot).is_some()
        && field(snapshot).is_some()
}

#[test]
fn servo_page_snapshots_change_and_actions() {
    let mut session = Session::open();
    println!("phase: load with semantics off");
    session.navigate("semantic.html");
    let loaded = session.wait_until(LOAD_WAIT, |session| {
        session.loading == Some(false)
            && session
                .committed
                .iter()
                .any(|url| url.contains("semantic.html"))
    });
    assert!(
        loaded,
        "page did not load; last events: {:?}",
        session.recent
    );
    let before = session.snapshots;
    session.pump_for(Duration::from_millis(800));
    assert_eq!(
        session.snapshots, before,
        "semantics is off, so a loaded page emits no snapshot; last events: {:?}",
        session.recent
    );

    println!("phase: semantics on");
    session.command(EngineCommand::SetSemantics(true));
    let mut raced = false;
    let ready = session.wait_until(SNAPSHOT_WAIT, |session| {
        let Some(snapshot) = &session.latest else {
            return false;
        };
        if blob(snapshot).contains("Added later") {
            raced = true;
            return true;
        }
        base_page(snapshot)
    });
    assert!(
        ready,
        "no snapshot of the page\n{}",
        session
            .latest
            .as_ref()
            .map(describe)
            .unwrap_or_else(|| format!("last events: {:?}", session.recent))
    );
    assert!(
        !raced,
        "the first snapshot already contained the delayed paragraph\n{}",
        session.latest.as_ref().map(describe).unwrap_or_default()
    );
    let first = session.latest.clone().expect("snapshot");
    assert_eq!(heading(&first).as_deref(), Some("Servo heading"));
    assert!(blob(&first).contains("A paragraph of text."));
    let href = link(&first)
        .and_then(|node| node.href.clone())
        .expect("link");
    assert!(href.contains("semantic-next.html"), "link href {href}");
    assert!(field(&first).is_some(), "text field\n{}", describe(&first));
    assert_eq!(field(&first).and_then(|node| node.value.clone()), None);

    println!("phase: dom change");
    let changed = session.wait_until(SNAPSHOT_WAIT, |session| {
        session
            .latest
            .as_ref()
            .is_some_and(|snapshot| blob(snapshot).contains("Added later"))
    });
    assert!(
        changed,
        "no snapshot contained the appended paragraph\n{}",
        session
            .latest
            .as_ref()
            .map(describe)
            .unwrap_or_else(|| format!("last events: {:?}", session.recent))
    );

    println!("phase: set value");
    let document = session.latest.as_ref().expect("snapshot").document;
    let node = field(session.latest.as_ref().expect("snapshot"))
        .expect("field")
        .id;
    session.command(EngineCommand::Semantic {
        document,
        action: SemanticAction::SetValue {
            node,
            value: "Ada".into(),
        },
    });
    let valued = session.wait_until(ACTION_WAIT, |session| {
        session
            .latest
            .as_ref()
            .and_then(field)
            .and_then(|node| node.value.as_deref())
            == Some("Ada")
    });
    assert!(
        valued,
        "the field value did not become Ada\n{}",
        session
            .latest
            .as_ref()
            .map(describe)
            .unwrap_or_else(|| format!("last events: {:?}", session.recent))
    );

    println!("phase: activate link");
    let document = session.latest.as_ref().expect("snapshot").document;
    let node = link(session.latest.as_ref().expect("snapshot"))
        .expect("link")
        .id;
    session.command(EngineCommand::Semantic {
        document,
        action: SemanticAction::Activate(node),
    });
    let navigated = session.wait_until(LOAD_WAIT, |session| {
        session
            .committed
            .iter()
            .any(|url| url.contains("semantic-next.html"))
    });
    assert!(
        navigated,
        "activating the link did not commit semantic-next.html; commits {:?} last events {:?}",
        session.committed, session.recent
    );

    println!("phase: semantics off");
    let before = session.snapshots;
    session.command(EngineCommand::SetSemantics(false));
    session.pump_for(Duration::from_millis(1500));
    assert_eq!(
        session.snapshots, before,
        "semantics is off, so the next page emits no snapshot; last events: {:?}",
        session.recent
    );

    session.runtime.shutdown();
    assert!(
        !session.runtime.diagnostics().leaked,
        "a clean page shuts Servo down"
    );
}
