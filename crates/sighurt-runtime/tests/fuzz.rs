//! Seeded random-input tests of the runtime's files: the saved session as `load_session` reads
//! it (lenient parse, `session.toml.broken-*` for what can't be read, at most `MAX_TABS`
//! tabs), the session writer, and the configuration file as `startup_config` and `load_config`
//! read it. No panic, a return in time, nothing lost silently: a file that can't be read is
//! kept aside byte for byte. `tools/fuzz/mod.rs` describes the environment variables.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::fs;
use std::time::Duration;

use fuzz::{iterations, run, Rng};
use sighurt_core::session::{Session, SessionSegment, SessionTab, MAX_TABS, VERSION};
use sighurt_core::Environment;
use sighurt_runtime::persistence::{
    load_config, load_session, startup_config, write_atomically, SessionWriter,
};

fn session(rng: &mut Rng) -> Session {
    // A page of an engine holds a stretch of history; an internal page, exactly one entry.
    let segment = |rng: &mut Rng| {
        let engine = rng
            .one_in(3)
            .then(|| rng.pick(&["servo", "wasm", "demo"]).to_string());
        let count = if engine.is_some() { rng.range(1, 3) } else { 1 };
        SessionSegment {
            engine,
            entries: (0..count)
                .map(|i| format!("https://a.test/{i}{}", rng.string(5)))
                .collect(),
            current: 0,
            title: rng.one_in(2).then(|| rng.string(40)),
        }
    };
    let tabs: Vec<SessionTab> = (0..rng.range(0, 6))
        .map(|_| SessionTab {
            current: 0,
            zoom: *rng.pick(&[1.0, 1.25, 0.5]),
            segments: (0..rng.range(1, 3)).map(|_| segment(rng)).collect(),
        })
        .collect();
    Session {
        version: VERSION,
        active: rng.below(tabs.len().max(1)),
        tabs,
        notices: Vec::new(),
    }
}

fn clear(dir: &std::path::Path) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let _ = fs::remove_file(entry.path());
    }
}

#[test]
fn fuzz_load_session_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.toml");
    run(
        "fuzz_load_session",
        iterations(100),
        Duration::from_secs(2),
        |case| {
            clear(dir.path());
            let valid = session(&mut case.rng);
            let text = valid.to_text();
            let bytes: Vec<u8> = match case.rng.below(4) {
                0 => text.clone().into_bytes(),
                1 => case.rng.mutate(&text).into_bytes(),
                2 => case.rng.mutate_bytes(text.as_bytes()),
                _ => {
                    let n = case.rng.below(300);
                    case.rng.bytes(n)
                }
            };
            case.note(|| format!("{:?}", String::from_utf8_lossy(&bytes)));
            fs::write(&path, &bytes).unwrap();
            match load_session(&path) {
                Ok(Some(read)) => {
                    assert!(read.tabs.len() <= MAX_TABS);
                    assert!(path.exists(), "a readable file stays");
                    if bytes == text.as_bytes() {
                        assert_eq!(read.tabs, valid.tabs, "round trip");
                        assert!(read.notices.is_empty());
                    }
                }
                Ok(None) => panic!("the file exists"),
                Err(message) => {
                    // Kept aside whole, never lost or replaced.
                    assert!(message.contains("kept as"), "{message}");
                    assert!(!path.exists());
                    let kept: Vec<_> = fs::read_dir(dir.path())
                        .unwrap()
                        .flatten()
                        .filter(|e| {
                            e.file_name()
                                .to_string_lossy()
                                .starts_with("session.toml.broken-")
                        })
                        .collect();
                    assert_eq!(kept.len(), 1, "{message}");
                    assert_eq!(fs::read(kept[0].path()).unwrap(), bytes);
                }
            }
        },
    );
}

#[test]
fn fuzz_session_writer_keeps_the_latest_whole() {
    let dir = tempfile::tempdir().unwrap();
    run(
        "fuzz_session_writer",
        iterations(100) / 20,
        Duration::from_secs(5),
        |case| {
            let path = dir
                .path()
                .join(format!("case{}", case.index))
                .join("session.toml");
            let writer = SessionWriter::new(path.clone()).unwrap();
            let mut last = String::new();
            for _ in 0..case.rng.range(1, 20) {
                last = session(&mut case.rng).to_text();
                writer.save(last.clone());
            }
            writer.finish();
            assert_eq!(fs::read_to_string(&path).unwrap(), last);
            let read = load_session(&path).unwrap().unwrap();
            assert!(read.notices.is_empty(), "{:?}", read.notices);
        },
    );
}

#[test]
fn fuzz_write_atomically_any_text() {
    let dir = tempfile::tempdir().unwrap();
    run(
        "fuzz_write_atomically",
        iterations(100) / 20,
        Duration::from_secs(5),
        |case| {
            let path = dir.path().join("f.toml");
            let text = case.rng.string(2000);
            write_atomically(&path, &text).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
            assert_eq!(
                fs::read_dir(dir.path()).unwrap().count(),
                1,
                "no temporary file left"
            );
        },
    );
}

#[test]
fn fuzz_config_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let env = Environment::default();
    run(
        "fuzz_config_files",
        iterations(100) / 2,
        Duration::from_secs(2),
        |case| {
            let seed = "version = 1\nhome = \"https://example.com/\"\n[tui]\nfps = 30\n[keys]\n\"ctrl-t\" = \"tab.new\"\n";
            let bytes = match case.rng.below(3) {
                0 => case.rng.mutate_bytes(seed.as_bytes()),
                1 => case.rng.mutate(seed).into_bytes(),
                _ => {
                    let n = case.rng.below(200);
                    case.rng.bytes(n)
                }
            };
            case.note(|| format!("{:?}", String::from_utf8_lossy(&bytes)));
            fs::write(&path, &bytes).unwrap();
            let started = startup_config(Some(&path), &env);
            let reloaded = load_config(Some(&path), &env);
            // The two agree on whether the file is usable; a failure names the file and ends with
            // the line the executable looks for.
            assert_eq!(started.is_ok(), reloaded.is_ok());
            if let Err(message) = started {
                assert!(
                    message.starts_with(&format!("{}: ", path.display()))
                        || message.starts_with("cannot read"),
                    "{message}"
                );
                assert!(message.ends_with(sighurt_runtime::persistence::DID_NOT_START));
            }
        },
    );
}
