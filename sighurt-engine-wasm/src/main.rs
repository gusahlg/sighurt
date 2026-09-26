//! `sig-wasm` — a Sighurt content engine for Oxide-format WebAssembly apps.
//!
//! Sighurt spawns this process once per page and speaks the [`sighurt_ipc`] protocol over
//! stdin/stdout. A thread reads the browser's messages into a channel. The main thread runs the
//! [`page::Page`]: it waits on the channel, runs guest frames at about 60 Hz while an app is live
//! (and sleeps until the next message when none is), and answers with draw lists for the browser
//! to paint. Apps load on a loader thread that reports back through the same channel.

mod draw;
mod keys;
mod page;

use std::io::{BufWriter, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use page::{Msg, Output, Page};
use sighurt_ipc::{FromEngine, ToEngine};

fn main() {
    let out: Output = Arc::new(Mutex::new(protocol_output()));
    report_panics(out.clone());
    let hello = FromEngine::Hello {
        version: sighurt_ipc::VERSION,
        name: "wasm".into(),
    };
    let _ = hello.write_to(&mut *out.lock().unwrap());

    let (tx, rx) = mpsc::channel();
    let stdin_tx = tx.clone();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        while let Ok(Some(msg)) = ToEngine::read_from(&mut stdin) {
            if stdin_tx.send(Msg::Host(msg)).is_err() {
                return;
            }
        }
        let _ = stdin_tx.send(Msg::Closed);
    });

    let mut page = match Page::new(out.clone(), tx) {
        Ok(page) => page,
        Err(e) => {
            let message = format!("sig-wasm failed to start: {e:#}");
            let _ = FromEngine::Error { message }.write_to(&mut *out.lock().unwrap());
            std::process::exit(1);
        }
    };

    'run: loop {
        let first = match page.next_tick() {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(msg) => Some(msg),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(msg) => Some(msg),
                Err(_) => break,
            },
        };
        // Handle everything that is already queued before running a frame.
        for msg in first.into_iter().chain(rx.try_iter()) {
            match msg {
                Msg::Host(msg) => page.handle(msg),
                Msg::Loaded(loaded) => page.finish_load(*loaded),
                Msg::Closed => break 'run,
            }
        }
        page.update();
        if page.closed() {
            break;
        }
    }
}

/// Makes panics on the threads that run guest code visible to the browser, and ends the engine:
/// a host function that panics leaves the page in no state to continue.
fn report_panics(out: Output) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        if !matches!(
            std::thread::current().name(),
            Some("main" | page::LOADER_THREAD)
        ) {
            return;
        }
        // `try_lock`, so a panic in the middle of sending a message can't deadlock here or
        // interleave with it.
        if let Ok(mut out) = out.try_lock() {
            let message = format!("sig-wasm crashed: {info}");
            let _ = FromEngine::Error { message }.write_to(&mut *out);
        }
        std::process::exit(101);
    }));
}

/// Takes ownership of the real stdout for the protocol and points file descriptor 1 at stderr,
/// so stray `println!`s inside the engine or its dependencies cannot corrupt the message stream.
#[cfg(unix)]
fn protocol_output() -> BufWriter<Box<dyn Write + Send>> {
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    // SAFETY: plain descriptor duplication on this process' own stdio; `dup` hands us a fresh
    // descriptor that nothing else owns.
    let file = unsafe {
        let fd = libc::dup(std::io::stdout().as_raw_fd());
        assert!(fd >= 0, "failed to duplicate stdout");
        libc::dup2(std::io::stderr().as_raw_fd(), libc::STDOUT_FILENO);
        File::from_raw_fd(fd)
    };
    BufWriter::new(Box::new(file))
}

#[cfg(not(unix))]
fn protocol_output() -> BufWriter<Box<dyn Write + Send>> {
    BufWriter::new(Box::new(std::io::stdout()))
}
