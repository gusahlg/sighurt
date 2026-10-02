//! A window with Servo views through the real adapter and the real host, before `sig` is wired:
//! like M1's experiment, but through `sighurt-engine-servo` and `sighurt-wayland`. Every engine
//! event is printed; a report at the end names the GL renderer and counts frames and readbacks.
//!
//! ```sh
//! SIG_WHAT="servo build (m4)" tools/remote tools/cargo build -p sighurt-engine-servo --example servo-window
//! tools/headless-sway --renderer gles2 -- target/debug/examples/servo-window --exit-after 10 PAGE...
//! ```
//!
//! Each PAGE (a URL, or a file) opens a view. With `--split` they share the window side by side,
//! all shown; otherwise they are tabs covering the window, and only the first is shown.
//! Actions on the view with the keyboard come from `--at SECONDS:ACTION` (repeatable) and from
//! standard input, one per line: `back`, `forward`, `reload`, `stop`, `hide`, `show`,
//! `open=URL`, `zoom=FACTOR`, `select=N` (show and focus the Nth view, hiding the others in
//! tabs), `new=URL` (another view), `close=N`, `mark` (print the loop's wakeups so far), `quit`.
//! `--audit-gl` counts WebRender's GL reads too; `--profile DIR` keeps Servo's profile there.
//! `tests/window-checks.sh` drives it in a headless compositor.

#[path = "../tests/common/mod.rs"]
mod common;

use std::time::Duration;

use calloop::channel::{channel, Event};
use calloop::timer::{TimeoutAction, Timer};
use common::{Driver, Layout, State};
use sighurt_api::engine::EngineCommand;
use sighurt_api::ids::NavigationId;
use sighurt_engine_servo::ServoConfig;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut exit_after, mut actions, mut pages) = (None, Vec::new(), Vec::new());
    let mut config = ServoConfig::default();
    let mut layout = Layout::Stack;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--exit-after" => exit_after = args.next().and_then(|s| s.parse::<f64>().ok()),
            "--at" => actions.extend(args.next()),
            "--split" => layout = Layout::Split,
            "--audit-gl" => config.audit_gl = true,
            "--profile" => config.profile = args.next().map(Into::into),
            page => pages.push(url_of(page)),
        }
    }
    if pages.is_empty() {
        eprintln!("usage: servo-window [--split] [--exit-after S] [--at S:ACTION]... PAGE...");
        std::process::exit(2);
    }

    actions.extend(exit_after.map(|seconds| format!("{seconds}:quit")));

    let mut driver = Driver::new(config, layout);
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
    for (index, page) in pages.iter().enumerate() {
        let visible = layout == Layout::Split || index == 0;
        driver.state.open(page, visible);
    }

    while driver.state.running {
        driver.iterate(None);
        for action in std::mem::take(&mut driver.state.actions) {
            println!("[{:8.3}] action {action}", driver.state.secs());
            act(&mut driver.state, &action);
        }
    }

    report(driver.shutdown());
}

fn act(state: &mut State, action: &str) {
    let (verb, argument) = action.split_once('=').unwrap_or((action, ""));
    let index = argument
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1));
    let nth = |state: &State, index: Option<usize>| Some(state.pages.get(index?)?.view);
    if verb == "quit" {
        state.running = false;
        return;
    }
    if verb == "mark" {
        println!("[{:8.3}] mark: {} wakeups", state.secs(), state.iterations);
        return;
    }
    let Some(view) = state.focus.or_else(|| nth(state, Some(0))) else {
        return;
    };
    match verb {
        "back" => drop(state.traverse(view, -1)),
        "forward" => drop(state.traverse(view, 1)),
        "reload" => state.command(
            view,
            EngineCommand::Reload {
                navigation: NavigationId::next(),
            },
        ),
        "stop" => state.command(view, EngineCommand::Stop),
        "hide" => state.set_visible(view, false),
        "show" => state.set_visible(view, true),
        "open" => drop(state.navigate(view, &url_of(argument))),
        "zoom" => state.command(
            view,
            EngineCommand::SetZoom(argument.parse().unwrap_or(1.0)),
        ),
        "new" => {
            let visible = state.layout == Layout::Split;
            state.open(&url_of(argument), visible);
        }
        "select" => {
            let Some(chosen) = nth(state, index) else {
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
        }
        "close" => {
            if let Some(chosen) = nth(state, index) {
                state.close(chosen);
            }
        }
        other => println!("unknown action {other}"),
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
    url::Url::from_file_path(path)
        .expect("an absolute path")
        .to_string()
}

fn report(state: State) {
    let diagnostics = state.runtime.diagnostics();
    println!("report: GL {:?}", diagnostics.gl);
    println!(
        "report: frames {}, read_to_image {}, GL reads {:?}, wakeups {}",
        diagnostics.frames, diagnostics.read_to_image, diagnostics.gl_reads, state.iterations
    );
    for page in &state.pages {
        println!(
            "report: {:?} presented {}",
            page.view,
            state.submitted(page.view)
        );
    }
    for target in state.host.diagnostics().targets {
        println!("report: {:?} {:?}", target.target, target.stats);
    }
    println!(
        "report: {}",
        match state.host.is_lost() {
            true => "the compositor connection was lost",
            false => "the compositor reported no error",
        }
    );
}
