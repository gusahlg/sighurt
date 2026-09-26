//! Runs the `sig-wasm` binary against tiny guests and checks what it tells the browser.

use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use sighurt_ipc::{DrawOp, FromEngine, ToEngine};

/// Draws a background, a red rectangle and a translucent blue one covering the canvas.
const STATIC_GUEST: &str = r#"
(module
  (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
  (import "oxide" "api_canvas_rect" (func $rect (param f32 f32 f32 f32 i32 i32 i32 i32)))
  (import "oxide" "api_canvas_dimensions" (func $dims (result i64)))
  (func (export "start_app") (local $d i64)
    (call $clear (i32.const 0x11) (i32.const 0x22) (i32.const 0x33) (i32.const 0xff))
    (call $rect (f32.const 10) (f32.const 20) (f32.const 30) (f32.const 40)
                (i32.const 255) (i32.const 0) (i32.const 0) (i32.const 255))
    (local.set $d (call $dims))
    (call $rect (f32.const 0) (f32.const 0)
                (f32.convert_i64_u (i64.shr_u (local.get $d) (i64.const 32)))
                (f32.convert_i64_u (i64.and (local.get $d) (i64.const 0xffffffff)))
                (i32.const 0) (i32.const 0) (i32.const 255) (i32.const 128))))
"#;

/// Draws a rectangle one pixel further right every frame.
const LIVE_GUEST: &str = r#"
(module
  (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
  (import "oxide" "api_canvas_rect" (func $rect (param f32 f32 f32 f32 i32 i32 i32 i32)))
  (global $frames (mut i32) (i32.const 0))
  (func (export "start_app"))
  (func (export "on_frame") (param $dt i32)
    (global.set $frames (i32.add (global.get $frames) (i32.const 1)))
    (call $clear (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 255))
    (call $rect (f32.convert_i32_u (global.get $frames)) (f32.const 0) (f32.const 1) (f32.const 1)
                (i32.const 255) (i32.const 255) (i32.const 255) (i32.const 255))))
"#;

/// Logs the text typed since its previous frame, whenever there is some.
const TYPING_GUEST: &str = r#"
(module
  (import "oxide" "api_text_input" (func $text (param i32 i32) (result i32)))
  (import "oxide" "api_log" (func $log (param i32 i32)))
  (memory (export "memory") 1)
  (func (export "start_app"))
  (func (export "on_frame") (param $dt i32) (local $n i32)
    (local.set $n (call $text (i32.const 0) (i32.const 64)))
    (if (local.get $n) (then (call $log (i32.const 0) (local.get $n))))))
"#;

/// Asks for the camera every frame until the request is answered, and logs "denied" if it is.
const CAMERA_GUEST: &str = r#"
(module
  (import "oxide" "api_camera_open" (func $open (result i32)))
  (import "oxide" "api_log" (func $log (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "denied")
  (global $done (mut i32) (i32.const 0))
  (func (export "start_app"))
  (func (export "on_frame") (param $dt i32)
    (if (i32.eqz (global.get $done)) (then
      (if (i32.eq (call $open) (i32.const -1)) (then
        (global.set $done (i32.const 1))
        (call $log (i32.const 0) (i32.const 6))))))))
"#;

/// Stores "one" under "k" in the persistent KV store and keeps running.
const KV_WRITER: &str = r#"
(module
  (import "oxide" "api_kv_store_set" (func $set (param i32 i32 i32 i32) (result i32)))
  (import "oxide" "api_log" (func $log (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "kone")
  (data (i32.const 16) "stored")
  (func (export "start_app")
    (if (i32.eqz (call $set (i32.const 0) (i32.const 1) (i32.const 1) (i32.const 3)))
      (then (call $log (i32.const 16) (i32.const 6)))))
  (func (export "on_frame") (param $dt i32)))
"#;

/// Logs the value stored under "k", or "missing".
const KV_READER: &str = r#"
(module
  (import "oxide" "api_kv_store_get" (func $get (param i32 i32 i32 i32) (result i32)))
  (import "oxide" "api_log" (func $log (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "k")
  (data (i32.const 16) "missing")
  (func (export "start_app") (local $n i32)
    (local.set $n (call $get (i32.const 0) (i32.const 1) (i32.const 32) (i32.const 16)))
    (if (i32.gt_s (local.get $n) (i32.const 0))
      (then (call $log (i32.const 32) (local.get $n)))
      (else (call $log (i32.const 16) (i32.const 7))))))
"#;

struct Engine {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<FromEngine>,
    _dir: Option<tempfile::TempDir>,
    url: String,
}

impl Engine {
    /// Starts `sig-wasm`, checks its Hello and loads `guest` into a 1600×1200 viewport at scale 2.
    fn load(guest: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = Self::load_in(dir.path(), "app", guest);
        engine._dir = Some(dir);
        engine
    }

    /// Like [`Engine::load`], with the guest at `dir/{name}.wasm`. Engines loading guests from
    /// one directory share an origin; `dir/data` is their data directory.
    fn load_in(dir: &std::path::Path, name: &str, guest: &str) -> Self {
        let path = dir.join(format!("{name}.wasm"));
        std::fs::write(&path, wat::parse_str(guest).unwrap()).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_sig-wasm"))
            .env("SIGHURT_AOT_CACHE", "off")
            .env("XDG_DATA_HOME", dir.join("data"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        // Read on a thread so that a silent engine fails the test instead of hanging it.
        let (tx, messages) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(Some(msg)) = FromEngine::read_from(&mut stdout) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        let mut engine = Self {
            stdin: child.stdin.take(),
            child,
            messages,
            _dir: None,
            url: url::Url::from_file_path(&path).unwrap().to_string(),
        };
        assert!(matches!(
            engine.next(),
            FromEngine::Hello {
                version: sighurt_ipc::VERSION,
                ..
            }
        ));
        engine.send(ToEngine::Resize {
            width: 1600,
            height: 1200,
            scale: 2.0,
        });
        engine.send(ToEngine::Navigate {
            url: engine.url.clone(),
        });
        engine
    }

    fn send(&mut self, msg: ToEngine) {
        msg.write_to(self.stdin.as_mut().unwrap()).unwrap();
    }

    fn next(&self) -> FromEngine {
        self.messages
            .recv_timeout(Duration::from_secs(30))
            .expect("sig-wasm went quiet")
    }

    /// Messages up to and including the next `Loading { loading: false }`.
    fn until_loaded(&self) -> Vec<FromEngine> {
        let mut seen = Vec::new();
        loop {
            let msg = self.next();
            let done = msg == FromEngine::Loading { loading: false };
            seen.push(msg);
            if done {
                return seen;
            }
        }
    }

    /// Closes stdin and waits for the engine to exit.
    fn close(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "sig-wasm did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32, color: u32) -> DrawOp {
    DrawOp::Rect {
        x,
        y,
        w,
        h,
        radius: 0.0,
        color,
    }
}

#[test]
fn draws_a_static_guest_and_zooms_it() {
    let mut engine = Engine::load(STATIC_GUEST);
    let seen = engine.until_loaded();
    assert_eq!(seen[0], FromEngine::Loading { loading: true });
    assert!(seen.contains(&FromEngine::Url {
        url: engine.url.clone()
    }));
    // The canvas is the viewport in logical pixels.
    let draw = FromEngine::Draw {
        ops: vec![
            DrawOp::Clear { color: 0x112233ff },
            rect(10.0, 20.0, 30.0, 40.0, 0xff0000ff),
            rect(0.0, 0.0, 800.0, 600.0, 0x0000ff80),
        ],
    };
    assert!(seen.contains(&draw), "{seen:#?}");

    // Zoom scales the draw list, and the canvas shrinks to match.
    engine.send(ToEngine::Zoom { factor: 2.0 });
    assert_eq!(
        engine.next(),
        FromEngine::Draw {
            ops: vec![
                DrawOp::Clear { color: 0x112233ff },
                rect(20.0, 40.0, 60.0, 80.0, 0xff0000ff),
                rect(0.0, 0.0, 1600.0, 1200.0, 0x0000ff80),
            ],
        }
    );
    engine.close();
}

#[test]
fn runs_frames_of_a_live_guest() {
    let engine = Engine::load(LIVE_GUEST);
    engine.until_loaded();
    let mut xs = Vec::new();
    while xs.len() < 5 {
        if let FromEngine::Draw { ops } = engine.next() {
            let [_, DrawOp::Rect { x, .. }] = ops[..] else {
                panic!("unexpected draw list {ops:?}");
            };
            xs.push(x);
        }
    }
    assert!(xs.windows(2).all(|pair| pair[1] > pair[0]), "{xs:?}");
    engine.close();
}

#[test]
fn reports_a_failed_load() {
    let mut engine = Engine::load(STATIC_GUEST);
    engine.until_loaded();
    engine.send(ToEngine::Navigate {
        url: "file:///nonexistent/app.wasm".into(),
    });
    let seen = engine.until_loaded();
    assert!(
        seen.iter().any(
            |msg| matches!(msg, FromEngine::Error { message } if message.contains("nonexistent"))
        ),
        "{seen:#?}"
    );
    // The failed document replaced the page.
    assert!(seen.contains(&FromEngine::Draw { ops: vec![] }));
    engine.close();
}

#[test]
fn delivers_typed_text_once() {
    let mut engine = Engine::load(TYPING_GUEST);
    engine.until_loaded();
    let key = |key: &str, code: &str, modifiers| ToEngine::Key {
        down: true,
        key: key.into(),
        code: code.into(),
        modifiers,
        repeat: false,
    };
    for msg in [
        key("h", "KeyH", 0),
        key(" ", "Space", 0),
        key("Enter", "Enter", 0),
        key("é", "Quote", 0),
        key("c", "KeyC", sighurt_ipc::MOD_CTRL),
        key("!", "Digit1", sighurt_ipc::MOD_SHIFT),
    ] {
        engine.send(msg);
    }
    // Each frame logs only what was typed since the one before, so the logs add up to the text.
    let mut typed = String::new();
    while typed.chars().count() < 4 {
        if let FromEngine::Console { message, .. } = engine.next() {
            typed.push_str(&message);
        }
    }
    assert_eq!(typed, "h é!");
    engine.close();
}

#[test]
fn draws_and_answers_permission_prompts() {
    let mut engine = Engine::load(CAMERA_GUEST);
    engine.until_loaded();
    // The engine draws the prompt over the page.
    loop {
        if let FromEngine::Draw { ops } = engine.next() {
            let asks =
                |op: &DrawOp| matches!(op, DrawOp::Text { text, .. } if text.contains("camera"));
            if ops.iter().any(asks) {
                break;
            }
        }
    }
    // Escape blocks the request: the prompt goes away and the guest's next try is denied. (Never
    // allow it here, which would open a real camera.)
    engine.send(ToEngine::Key {
        down: true,
        key: "Escape".into(),
        code: "Escape".into(),
        modifiers: 0,
        repeat: false,
    });
    let (mut cleared, mut denied) = (false, false);
    while !(cleared && denied) {
        match engine.next() {
            FromEngine::Draw { ops } => cleared = ops.is_empty(),
            FromEngine::Console { message, .. } => denied |= message == "denied",
            _ => {}
        }
    }
    engine.close();
}

/// Pages are processes of their own: two of the same origin use the persistent store at once.
#[test]
fn pages_share_the_persistent_store() {
    let dir = tempfile::tempdir().unwrap();
    let logs = |engine: &Engine| loop {
        if let FromEngine::Console { message, .. } = engine.next() {
            return message;
        }
    };
    let writer = Engine::load_in(dir.path(), "writer", KV_WRITER);
    assert_eq!(logs(&writer), "stored");
    // The writer is still running.
    let reader = Engine::load_in(dir.path(), "reader", KV_READER);
    assert_eq!(logs(&reader), "one");
    reader.close();
    writer.close();
}
