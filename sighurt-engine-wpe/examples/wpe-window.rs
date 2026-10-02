//! A window with WPE WebKit views through the real adapter and the real host, before `sig` is
//! wired: a CPU-painted chrome strip (painted only when its size changes) above the pages. Every
//! engine event is printed; `mark` and the end print the transport, frames, copies, wakeups and
//! memory.
//!
//! ```sh
//! tools/cargo build -p sighurt-engine-wpe --example wpe-window
//! tools/heavy tools/headless-sway --renderer gles2 -- target/debug/examples/wpe-window --exit-after 10 PAGE...
//! ```
//!
//! Each PAGE (a URL, or a file) opens a view. With `--split` they share the page area side by
//! side, all shown; otherwise they are tabs, and only the first is shown. Actions on the selected
//! view come from `--at SECONDS:ACTION` (repeatable) and standard input, one per line: `back`,
//! `forward`, `reload`, `stop`, `hide`, `show`, `open=URL`, `zoom=FACTOR`, `eval=SCRIPT` (prints
//! the result), `crash` (ends the view's web process), `select=N` (show, select and focus the
//! Nth view, hiding the others in tabs), `new=URL` (another view, hidden in tabs), `close=N`,
//! `focus=chrome` / `focus=page` (where the keyboard goes), `grant=yes` / `grant=no` (how
//! permission requests are answered), `mark` (prints wakeups, diagnostics and memory so far),
//! `quit`. `--shm` forces the shared-memory path; `--profile DIR` keeps WebKit's profile there;
//! `--file PATH` (repeatable) is what file pickers answer. `tests/window-checks.sh` drives it in
//! a private compositor.

#[path = "../tests/common/mod.rs"]
mod common;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use calloop::channel::{channel, Event};
use calloop::timer::{TimeoutAction, Timer};
use common::{Driver, Layout, State};
use sighurt_api::engine::EngineCommand;
use sighurt_api::ids::ViewId;
use sighurt_engine_wpe::WpeConfig;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut actions, mut pages, mut files) = (Vec::new(), Vec::new(), Vec::new());
    let mut config = WpeConfig::default();
    let mut layout = Layout::Stack;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--exit-after" => actions.extend(args.next().map(|s| format!("{s}:quit"))),
            "--at" => actions.extend(args.next()),
            "--split" => layout = Layout::Split,
            "--shm" => config.force_shm = true,
            "--profile" => config.profile = args.next().map(Into::into),
            "--file" => files.extend(args.next().map(Into::into)),
            page => pages.push(url_of(page)),
        }
    }
    if pages.is_empty() {
        eprintln!(
            "usage: wpe-window [--split] [--shm] [--exit-after S] [--at S:ACTION]... PAGE..."
        );
        std::process::exit(2);
    }

    let mut driver = Driver::new(config, layout);
    driver.state.files = files;
    let handle = driver.event_loop.handle();
    for action in actions {
        let Some((at, what)) = action.split_once(':') else {
            eprintln!("--at {action}: expected SECONDS:ACTION");
            std::process::exit(2);
        };
        let (at, what) = (at.parse::<f64>().unwrap_or(0.0), what.to_string());
        let timer = Timer::from_duration(Duration::from_secs_f64(at));
        handle
            .insert_source(timer, move |_, _, state: &mut State| {
                state.actions.push(what.clone());
                TimeoutAction::Drop
            })
            .expect("a timer");
    }
    // Standard input, line by line, from a thread of its own.
    let (lines, source) = channel::<String>();
    handle
        .insert_source(source, |event, _, state: &mut State| {
            if let Event::Msg(line) = event {
                state.actions.push(line);
            }
        })
        .expect("the stdin channel");
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            if lines.send(line.trim().to_string()).is_err() {
                break;
            }
        }
    });
    let mut selected = None;
    for (index, page) in pages.iter().enumerate() {
        let visible = layout == Layout::Split || index == 0;
        let view = driver.state.open(page, visible);
        selected = selected.or(Some(view));
    }

    while driver.state.running {
        driver.iterate(None);
        let actions = std::mem::take(&mut driver.state.actions);
        for action in &actions {
            println!("[{:8.3}] action {action}", driver.state.secs());
            act(&mut driver, &mut selected, action);
        }
        if actions.iter().any(|action| action != "mark") {
            // What the actions asked for is carried out now, not at the next wakeup. (A mark
            // asks for nothing, so the idle check counts only the wakeups that print them.)
            driver.iterate(Some(Duration::ZERO));
        }
    }
    report(&driver.state, "end");
    let state = driver.shutdown();
    println!(
        "shut down: {}",
        match state.host.is_lost() {
            true => "the compositor connection was lost",
            false => "the compositor reported no error",
        }
    );
}

fn act(driver: &mut Driver, selected: &mut Option<ViewId>, action: &str) {
    let (verb, argument) = action.split_once('=').unwrap_or((action, ""));
    let state = &mut driver.state;
    let nth = |state: &State| {
        let index = argument.parse::<usize>().ok()?.checked_sub(1)?;
        Some(state.pages.get(index)?.view)
    };
    match verb {
        "quit" => state.running = false,
        "mark" => report(state, "mark"),
        "grant" => state.grant = argument == "yes",
        "new" => drop(state.open(&url_of(argument), state.layout == Layout::Split)),
        "select" => {
            let Some(chosen) = nth(state) else {
                return;
            };
            if state.layout == Layout::Stack {
                let others: Vec<_> = state.pages.iter().map(|p| p.view).collect();
                for other in others.into_iter().filter(|v| *v != chosen) {
                    state.set_visible(other, false);
                }
            }
            state.set_visible(chosen, true);
            state.set_focus(Some(chosen));
            *selected = Some(chosen);
        }
        "close" => {
            if let Some(chosen) = nth(state) {
                state.close(chosen);
                if *selected == Some(chosen) {
                    *selected = None;
                }
            }
        }
        _ => {
            let Some(view) = *selected else {
                println!("[{:8.3}] no view is selected", state.secs());
                return;
            };
            match verb {
                "back" => drop(state.traverse(view, -1)),
                "forward" => drop(state.traverse(view, 1)),
                "reload" => drop(state.reload(view)),
                "stop" => state.command(view, EngineCommand::Stop),
                "hide" => state.set_visible(view, false),
                "show" => state.set_visible(view, true),
                "open" => drop(state.navigate(view, &url_of(argument))),
                "zoom" => state.command(
                    view,
                    EngineCommand::SetZoom(argument.parse().unwrap_or(1.0)),
                ),
                "focus" => state.set_focus((argument == "page").then_some(view)),
                "crash" => {
                    if let Err(error) = state.runtime.terminate_web_process(view) {
                        println!("[{:8.3}] crash failed: {error}", state.secs());
                    }
                }
                "eval" => {
                    let result = driver.eval(view, argument);
                    println!("[{:8.3}] script {view:?}: {result:?}", driver.state.secs());
                }
                other => println!("[{:8.3}] unknown action {other}", state.secs()),
            }
        }
    }
}

/// `page` as a URL: itself if it has a scheme, else a file.
fn url_of(page: &str) -> String {
    if url::Url::parse(page).is_ok() {
        return page.to_string();
    }
    let path = std::fs::canonicalize(page).unwrap_or_else(|error| {
        eprintln!("{page}: {error}");
        std::process::exit(2)
    });
    format!("file://{}", path.display())
}

fn report(state: &State, label: &str) {
    let d = state.runtime.diagnostics();
    let t = state.secs();
    println!(
        "[{t:8.3}] {label}: wakeups {} chrome-paints {} transport {:?} device {:?} driver {:?} \
         formats {} glib-dispatches {} stale-frames {} rounded-frames {} failed-imports {}",
        state.iterations,
        state.chrome_paints,
        d.transport,
        d.device,
        d.driver,
        d.formats,
        d.glib_dispatches,
        d.stale_frames,
        d.rounded_frames,
        d.failed_imports,
    );
    println!("[{t:8.3}] {label}: renderer {:?}", d.renderer());
    let p = d.present;
    let mean = d.total_present.as_secs_f64() * 1000.0 / d.frames().max(1) as f64;
    println!(
        "[{t:8.3}] {label}: submitted {} dmabuf-frames {} dmabuf-imports {} shm-copies {} shm-bytes {} \
         dropped {} releases {} present-mean-ms {mean:.3} present-max-ms {:.3}",
        d.frames(),
        p.dmabuf_frames,
        p.dmabuf_imports,
        p.shm_copies,
        p.shm_bytes,
        p.dropped,
        p.releases,
        d.max_present.as_secs_f64() * 1000.0,
    );
    for page in &state.pages {
        println!(
            "[{t:8.3}] {label}: {:?} frames {} pacing {}",
            page.view,
            page.frames.len(),
            pacing(&page.frames)
        );
    }
    let memory = memory();
    let total: u64 = memory.values().sum();
    let parts: Vec<String> = memory
        .iter()
        .map(|(name, kib)| format!("{name} {kib}"))
        .collect();
    println!("[{t:8.3}] {label}: pss-kib {total} ({})", parts.join(", "));
}

/// Frame intervals in milliseconds: median, 95th percentile and maximum, over the frames that
/// came within a second of the one before (animation, not idle gaps).
fn pacing(frames: &[Instant]) -> String {
    let mut gaps: Vec<f64> = frames
        .windows(2)
        .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0)
        .filter(|&ms| ms < 1000.0)
        .collect();
    if gaps.is_empty() {
        return "-".into();
    }
    gaps.sort_by(f64::total_cmp);
    let at = |q: f64| gaps[((gaps.len() - 1) as f64 * q).round() as usize];
    format!(
        "n={} median={:.2}ms p95={:.2}ms max={:.2}ms",
        gaps.len(),
        at(0.5),
        at(0.95),
        gaps[gaps.len() - 1]
    )
}

/// Proportional set size of this process and its descendants (WebKit's web, network and
/// sandbox processes), by command name, in KiB.
fn memory() -> HashMap<String, u64> {
    let mut by_name = HashMap::new();
    let mut pids = vec![std::process::id()];
    while let Some(pid) = pids.pop() {
        let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        let pss = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Pss:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            })
            .unwrap_or(0);
        *by_name.entry(name.trim().to_string()).or_insert(0) += pss;
        let tasks = std::fs::read_dir(format!("/proc/{pid}/task"))
            .into_iter()
            .flatten();
        for task in tasks.flatten() {
            let children =
                std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
            pids.extend(
                children
                    .split_whitespace()
                    .filter_map(|c| c.parse::<u32>().ok()),
            );
        }
    }
    by_name
}
