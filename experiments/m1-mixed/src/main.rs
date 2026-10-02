//! M1: CPU chrome and a Servo page in one Wayland window (sighurt-rewrite-plan.md §23).
//!
//! A standalone prototype, not the browser: one root `xdg_toplevel`, a CPU-painted chrome
//! subsurface, and a page subsurface that Servo 0.5 presents into directly through surfman's
//! `WindowRenderingContext` on this program's own libwayland connection. It exercises and
//! measures the M1 pass criteria; docs/rewrite/M1.md records the results.
//!
//! Keys: F2 hides/shows the page, Ctrl+L focuses the address field, Escape (in the address
//! field) returns to the page, Ctrl+Q quits. Clicking the chrome or the page focuses it.

mod chrome;
mod egl;
mod host;
mod pacing;
mod page;
mod shm;
mod stats;

use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use calloop::timer::{TimeoutAction, Timer};
use calloop::EventLoop;
use sighurt_api::geometry::LogicalSize;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use url::Url;
use wayland_client::globals::registry_queue_init;
use wayland_client::Connection;

use host::{App, Options};

const USAGE: &str = "\
usage: m1-mixed [OPTIONS] [PAGE]

PAGE is a file or URL to load (default: pages/page.html next to this crate).

  --chrome-only       no page: the root and the chrome strip only
  --integer-scale     ignore wp_fractional_scale_v1 and wp_viewporter (the integer fallback)
  --size WxH          ask for exactly this window size (tiling window managers float it)
  --exit-after SECS   exit cleanly after SECS seconds
  --hide-cycles N     hide and show the page N times, 0.5 s apart, starting 4 s in
  --idle-window SECS  count the loop's wakeups in the last SECS before exit (default 10)
  --no-gl-audit       don't count GL readbacks (the audit times every WebRender GL call)";

static START: OnceLock<Instant> = OnceLock::new();

/// Seconds since the program started, for log lines.
pub fn elapsed() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Prints a timestamped line to stderr.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!("[{:8.3}] {}", $crate::elapsed(), format_args!($($arg)*))
    };
}

struct Args {
    options: Options,
    exit_after: Option<Duration>,
    hide_cycles: u32,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut page = None;
    let mut chrome_only = false;
    let mut integer_scale = false;
    let mut fixed_size = None;
    let mut audit_gl = true;
    let mut exit_after = None;
    let mut hide_cycles = 0;
    let mut idle_window = Duration::from_secs(10);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(USAGE.into()),
            "--chrome-only" => chrome_only = true,
            "--integer-scale" => integer_scale = true,
            "--size" => fixed_size = Some(size(args.next())?),
            "--no-gl-audit" => audit_gl = false,
            "--exit-after" => {
                exit_after = Some(Duration::from_secs_f64(number(&arg, args.next())?))
            }
            "--hide-cycles" => hide_cycles = number(&arg, args.next())? as u32,
            "--idle-window" => idle_window = Duration::from_secs_f64(number(&arg, args.next())?),
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}\n\n{USAGE}")),
            _ => page = Some(arg),
        }
    }
    let url = match (chrome_only, page) {
        (true, _) => None,
        (false, Some(page)) if page.contains("://") => {
            Some(Url::parse(&page).map_err(|e| format!("{page}: {e}"))?)
        }
        (false, page) => {
            let path =
                page.unwrap_or(concat!(env!("CARGO_MANIFEST_DIR"), "/pages/page.html").into());
            let path = std::fs::canonicalize(&path).map_err(|e| format!("{path}: {e}"))?;
            Some(
                Url::from_file_path(&path)
                    .map_err(|()| format!("{}: not a file", path.display()))?,
            )
        }
    };
    Ok(Args {
        options: Options {
            url,
            integer_scale,
            audit_gl,
            fixed_size,
            idle_window,
        },
        exit_after,
        hide_cycles,
    })
}

fn size(value: Option<String>) -> Result<LogicalSize, String> {
    let parse = |v: &str| v.parse::<u32>().ok().filter(|n| *n > 0);
    value
        .as_deref()
        .and_then(|v| v.split_once('x'))
        .and_then(|(w, h)| {
            Some(LogicalSize {
                width: parse(w)?,
                height: parse(h)?,
            })
        })
        .ok_or("--size needs WxH, such as 1280x800".into())
}

fn number(name: &str, value: Option<String>) -> Result<f64, String> {
    value
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or(format!("{name} needs a number"))
}

fn main() -> ExitCode {
    elapsed();
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => {
            log!("exit: clean");
            ExitCode::SUCCESS
        }
        Err(e) => {
            log!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|e| format!("wayland: {e}"))?;
    let (globals, queue) =
        registry_queue_init::<App>(&connection).map_err(|e| format!("wayland: {e}"))?;
    let mut event_loop = EventLoop::<App>::try_new().map_err(|e| e.to_string())?;
    let handle = event_loop.handle();

    // Servo's waker pings this source; the next loop turn pumps Servo.
    let (ping, ping_source) = calloop::ping::make_ping().map_err(|e| e.to_string())?;
    handle
        .insert_source(ping_source, |_, _, app: &mut App| {
            app.stats.wake.servo = true
        })
        .map_err(|e| e.to_string())?;
    let mut app = App::new(
        connection.clone(),
        &globals,
        queue.handle(),
        args.options,
        ping,
    )?;
    handle
        .insert_source(
            WaylandSource::new(connection.clone(), queue),
            |_, queue, app: &mut App| {
                let dispatched = queue.dispatch_pending(app)?;
                if dispatched > 0 {
                    app.stats.wake.wayland = true;
                }
                Ok(dispatched)
            },
        )
        .map_err(|e| e.to_string())?;
    if let Some(after) = args.exit_after {
        handle
            .insert_source(Timer::from_duration(after), |_, _, app: &mut App| {
                app.stats.wake.timer = true;
                log!("exit: --exit-after elapsed");
                app.quit = true;
                TimeoutAction::Drop
            })
            .map_err(|e| e.to_string())?;
    }
    if args.hide_cycles > 0 {
        let mut toggles = args.hide_cycles * 2;
        handle
            .insert_source(
                Timer::from_duration(Duration::from_secs(4)),
                move |_, _, app: &mut App| {
                    app.stats.wake.timer = true;
                    app.toggle_page();
                    toggles -= 1;
                    match toggles {
                        0 => TimeoutAction::Drop,
                        _ => TimeoutAction::ToDuration(Duration::from_millis(500)),
                    }
                },
            )
            .map_err(|e| e.to_string())?;
    }

    let start = Instant::now();
    while !app.quit {
        event_loop
            .dispatch(None, &mut app)
            .map_err(|e| format!("event loop: {e}"))?;
        app.turn();
    }
    let end = Instant::now();

    log!("report:\n{}", app.stats.report(start, end));
    match app.readbacks() {
        Some((images, gl)) => log!(
            "readbacks: read_to_image {images}, GL reads {}",
            gl.map_or("not audited".into(), |n| n.to_string())
        ),
        None => log!("readbacks: no page"),
    }
    log!("shm: {}", app.shm_report());
    if let Some(error) = app.error.take() {
        // `App`'s `Drop` releases everything in the same order as `teardown`, as it does when
        // `dispatch` fails or something panics.
        return Err(error);
    }
    app.teardown()?;
    // The event loop holds the event queue and a connection handle; the connection closes
    // with the last one.
    drop(event_loop);
    drop(globals);
    drop(connection);
    Ok(())
}
