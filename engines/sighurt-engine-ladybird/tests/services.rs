//! Ladybird's requests of the host go through `sighurt_api::services`, and the focused editable
//! field is reported as text input. The test plays the core: it answers each service request and
//! checks what the page then does. One process, one Ladybird (see tests/headless.rs).
//!
//!   tools/heavy cargo test -p sighurt-engine-ladybird --test services -- --nocapture

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, PumpStatus, ViewOptions};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{Metrics, Scale, SurfacePoint, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, SeatId, ViewId};
use sighurt_api::input::{
    ButtonState, InputEvent, InputKind, Key, KeyCode, KeyEvent, Modifiers, PointerButton,
    PointerEvent, TextInputEvent,
};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_api::services::{
    ClipboardData, DownloadSource, PickMode, Service, ServiceError, ServiceReply, ServiceRequest,
};
use sighurt_engine_ladybird::{ffi, LadybirdConfig, LadybirdRuntime};

struct Mem(Rc<RefCell<Metrics>>);

impl CpuTarget for Mem {
    type Permit = LayoutEpoch;
    fn metrics(&self) -> Metrics {
        *self.0.borrow()
    }
    fn permit_epoch(permit: &LayoutEpoch) -> LayoutEpoch {
        *permit
    }
    fn submit(
        &mut self,
        _permit: LayoutEpoch,
        _paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        Ok(())
    }
}

struct Driver {
    runtime: LadybirdRuntime<Mem>,
    wakes: mpsc::Receiver<()>,
    waker: EngineWaker,
    seen: Vec<EngineEvent>,
    cursor: usize,
    deadline: Option<Instant>,
    view: ViewId,
    seat: SeatId,
}

impl Driver {
    fn pump(&mut self) {
        self.waker.take();
        let mut events = Vec::new();
        self.deadline = match self.runtime.pump(&mut events) {
            PumpStatus::Pending { deadline } => Some(deadline.unwrap_or_else(Instant::now)),
            PumpStatus::Idle => None,
        };
        self.seen.extend(events);
    }

    /// Waits for an event after the last one taken that `pick` accepts; returns what it picked.
    fn next<T>(
        &mut self,
        timeout: Duration,
        what: &str,
        mut pick: impl FnMut(&EngineEvent) -> Option<T>,
    ) -> T {
        let end = Instant::now() + timeout;
        loop {
            while self.cursor < self.seen.len() {
                let event = &self.seen[self.cursor];
                self.cursor += 1;
                if let Some(found) = pick(event) {
                    return found;
                }
            }
            let now = Instant::now();
            assert!(
                now < end,
                "timed out waiting for {what}; saw {:#?}",
                &self.seen[self.cursor.saturating_sub(10)..]
            );
            let wait = self.deadline.map_or(end - now, |d| {
                d.saturating_duration_since(now).min(end - now)
            });
            let _ = self.wakes.recv_timeout(wait);
            self.pump();
        }
    }

    fn input(&mut self, kind: InputKind) {
        let event = InputEvent {
            seat: self.seat,
            kind,
        };
        self.runtime
            .command(self.view, EngineCommand::Input(event))
            .unwrap();
    }

    fn click(&mut self, x: f64, y: f64) {
        let position = SurfacePoint { x, y };
        let modifiers = Modifiers::default();
        self.input(InputKind::Pointer(PointerEvent::Motion {
            position,
            modifiers,
        }));
        for state in [ButtonState::Pressed, ButtonState::Released] {
            self.input(InputKind::Pointer(PointerEvent::Button {
                button: PointerButton::Left,
                state,
                position,
                modifiers,
            }));
        }
    }

    fn key(&mut self, code: u32, name: &str, state: ButtonState, control: bool) {
        self.input(InputKind::Key(KeyEvent {
            code: KeyCode(code),
            key: Key::Character(name.into()),
            state,
            repeat: false,
            modifiers: Modifiers {
                control,
                ..Modifiers::default()
            },
            consumed: Modifiers::default(),
            text: None,
        }));
    }

    /// Ctrl+`name`.
    fn chord(&mut self, code: u32, name: &str) {
        self.key(29, "Control", ButtonState::Pressed, true);
        self.key(code, name, ButtonState::Pressed, true);
        self.key(code, name, ButtonState::Released, true);
        self.key(29, "Control", ButtonState::Released, false);
    }

    fn answer(&mut self, request: &ServiceRequest, result: Result<ServiceReply, ServiceError>) {
        self.runtime
            .command(
                self.view,
                EngineCommand::ResolveServiceRequest {
                    request: request.id,
                    result,
                },
            )
            .unwrap();
    }

    fn title(&mut self, wanted: &str) {
        let wanted = wanted.to_owned();
        self.next(
            Duration::from_secs(15),
            &format!("title {wanted}"),
            |e| match e {
                EngineEvent::TitleChanged { title: Some(t), .. } if *t == wanted => Some(()),
                _ => None,
            },
        );
    }

    fn service(&mut self, what: &str, accept: impl Fn(&Service) -> bool) -> ServiceRequest {
        self.next(Duration::from_secs(20), what, |e| match e {
            EngineEvent::ServiceRequested(request) if accept(&request.service) => {
                Some(request.clone())
            }
            _ => None,
        })
    }
}

#[test]
fn services_and_text_input() {
    if ffi::load(None).is_err() {
        eprintln!("skipped: no Ladybird shim (nix/build-ladybird-shim.sh)");
        return;
    }
    let dir =
        std::env::temp_dir().join(format!("sighurt-ladybird-services-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let picked = dir.join("picked.txt");
    std::fs::write(&picked, "file contents").unwrap();
    let page = dir.join("page.html");
    std::fs::write(
        &page,
        "<!doctype html><title>start</title><body style='margin:0'>\
         <input id=i style='position:absolute;left:10px;top:10px;width:300px;height:30px' \
                oninput='document.title=\"v:\"+this.value'>\
         <a id=d download='hello.txt' href='data:text/plain,hello' \
            style='position:absolute;left:10px;top:60px;width:200px;height:30px;display:block'>download</a>\
         <a id=m href='mailto:a@example.org' \
            style='position:absolute;left:10px;top:110px;width:200px;height:30px;display:block'>mail</a>\
         <input id=f type=file style='position:absolute;left:10px;top:160px;width:300px;height:30px' \
                onchange='document.title=\"f:\"+this.files[0].name'>",
    )
    .unwrap();

    let (wake_tx, wakes) = mpsc::channel();
    let waker = EngineWaker::new(move || {
        let _ = wake_tx.send(());
    });
    let runtime = LadybirdRuntime::<Mem>::new(LadybirdConfig::default(), waker.clone())
        .unwrap_or_else(|e| panic!("LadybirdRuntime::new: {e}"));
    let view = ViewId::next();
    let metrics = Rc::new(RefCell::new(Metrics::new(
        LayoutEpoch::next(),
        WindowRect {
            x: 0,
            y: 0,
            width: 640,
            height: 480,
        },
        Scale::ONE,
    )));
    let mut d = Driver {
        runtime,
        wakes,
        waker,
        seen: Vec::new(),
        cursor: 0,
        deadline: None,
        view,
        seat: SeatId::next(),
    };
    d.runtime
        .open_view(view, ViewOptions::default(), Mem(Rc::clone(&metrics)))
        .unwrap();
    d.runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: format!("file://{}", page.display()),
            },
        )
        .unwrap();
    d.title("start");

    // 1. Focusing an editable field is reported with its caret, and leaving it clears it.
    d.click(20.0, 25.0);
    let state = d.next(
        Duration::from_secs(10),
        "TextInputChanged(Some)",
        |e| match e {
            EngineEvent::TextInputChanged { state: Some(s), .. } => Some(s.clone()),
            _ => None,
        },
    );
    assert!(
        state.caret.x >= 0 && state.caret.x < 320 && state.caret.y >= 0 && state.caret.y < 50,
        "{state:?}"
    );

    // 2. Text from the input method is typed into it.
    d.input(InputKind::TextInput(TextInputEvent {
        commit: Some("hello".into()),
        ..TextInputEvent::default()
    }));
    d.title("v:hello");

    // 3. Copying asks the host (ClipboardWrite); nothing reaches a system clipboard by itself.
    d.chord(30, "a");
    d.chord(46, "c");
    let write = d.service("a clipboard write", |s| {
        matches!(s, Service::ClipboardWrite { .. })
    });
    let Service::ClipboardWrite { items, .. } = &write.service else {
        unreachable!()
    };
    assert!(
        items
            .iter()
            .any(|i| i.mime_type == "text/plain" && i.data == b"hello"),
        "{items:?}"
    );
    assert!(
        write.origin.is_none()
            || write.origin.as_deref() == Some("null")
            || write
                .origin
                .as_deref()
                .is_some_and(|o| o.starts_with("file")),
        "{:?}",
        write.origin
    );
    d.answer(&write, Ok(ServiceReply::Done));

    // 4. Pasting asks the host (ClipboardRead); what it grants is pasted, over the selection.
    d.chord(47, "v");
    let read = d.service("a clipboard read", |s| {
        matches!(s, Service::ClipboardRead { .. })
    });
    d.answer(
        &read,
        Ok(ServiceReply::Clipboard(ClipboardData {
            mime_type: "text/plain".into(),
            data: b"PASTED".to_vec(),
        })),
    );
    d.title("v:PASTED");

    // 5. Clicking elsewhere ends the text input.
    d.click(400.0, 400.0);
    d.next(
        Duration::from_secs(10),
        "TextInputChanged(None)",
        |e| match e {
            EngineEvent::TextInputChanged { state: None, .. } => Some(()),
            _ => None,
        },
    );

    // 6. A download is handed to the host as data, never written to a folder of the user's.
    d.click(20.0, 75.0);
    let download = d.service("a download", |s| matches!(s, Service::Download { .. }));
    let Service::Download {
        source: DownloadSource::Data(bytes),
        suggested_name,
    } = &download.service
    else {
        panic!("{download:?}")
    };
    assert_eq!(bytes, b"hello");
    assert_eq!(suggested_name.as_deref(), Some("hello.txt"));
    d.answer(&download, Ok(ServiceReply::Done));

    // 7. A mailto: link is a request to open an external URL; denied, nothing opens.
    d.click(20.0, 125.0);
    // The click is a navigation the user started: the engine asks the browser first.
    let asked = d.next(
        Duration::from_secs(10),
        "a navigation request for the mailto: link",
        |e| match e {
            EngineEvent::NavigationRequested { request, url, .. } if url.starts_with("mailto:") => {
                Some(*request)
            }
            _ => None,
        },
    );
    d.runtime
        .command(
            d.view,
            EngineCommand::AnswerNavigation {
                request: asked,
                allow: true,
            },
        )
        .unwrap();
    let open = d.service("an external URL", |s| {
        matches!(s, Service::OpenExternal { .. })
    });
    assert_eq!(
        open.service,
        Service::OpenExternal {
            url: "mailto:a@example.org".into()
        }
    );
    d.answer(&open, Err(ServiceError::Denied));

    // 8. A file input asks the host to pick; the page gets the file it names.
    d.click(20.0, 175.0);
    let pick = d.service("a file picker", |s| matches!(s, Service::PickFiles { .. }));
    assert!(matches!(
        pick.service,
        Service::PickFiles {
            mode: PickMode::OpenFile,
            ..
        }
    ));
    d.answer(&pick, Ok(ServiceReply::Files(vec![picked.clone()])));
    d.title("f:picked.txt");

    d.runtime.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
