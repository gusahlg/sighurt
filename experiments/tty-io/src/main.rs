//! `tty-io` demo and the child workers the pty tests drive.
//!
//! With no arguments, and a terminal on stdin, it enters raw mode, probes, and
//! leaves on `q`, Escape or Ctrl-C. `--probe-out PATH` records the probe for a
//! real terminal. `--worker` is for the pty tests.

use std::env;
use std::io::{self, IsTerminal};
use std::os::fd::{AsFd, OwnedFd};
use std::time::Duration;

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode, PostAction};

use tty_io::keys::{ButtonState, Key};
use tty_io::parser::Input;
use tty_io::{format_caps, prepare_forced_exit, Attrs, Color, Grid, Session, PROBE_DEADLINE};

fn main() {
    if let Err(err) = dispatch() {
        eprintln!("tty-io: {err}");
        std::process::exit(1);
    }
}

fn dispatch() -> io::Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("--worker") => match args.next().as_deref() {
            Some("enter-exit") => worker_enter_exit(),
            Some("panic") => worker_panic(),
            Some("suspend") => worker_suspend(),
            Some("hangup") => worker_hangup(),
            Some("force-exit") => worker_force_exit(),
            Some("crossterm") => worker_crossterm(),
            Some(other) => Err(io::Error::other(format!("unknown worker {other}"))),
            None => Err(io::Error::other("missing worker name")),
        },
        Some("--probe-out") => {
            let Some(path) = args.next() else {
                return Err(io::Error::other("missing --probe-out path"));
            };
            probe_out(&path)
        }
        Some("--help") => {
            eprintln!(
                "tty-io [--probe-out PATH]\n  interactive when stdin is a terminal; q, Escape or Ctrl-C quits"
            );
            Ok(())
        }
        Some(other) => Err(io::Error::other(format!("unknown argument {other}"))),
        None => interactive(),
    }
}

fn stdout_fd() -> io::Result<OwnedFd> {
    std::io::stdout().as_fd().try_clone_to_owned()
}

fn stdin_fd() -> io::Result<OwnedFd> {
    std::io::stdin().as_fd().try_clone_to_owned()
}

fn worker_enter_exit() -> io::Result<()> {
    let mut session = Session::enter(stdout_fd()?)?;
    session.write_frame(b"READY")?;
    session.restore()?;
    session.write_frame(b"DONE")?;
    Ok(())
}

fn worker_panic() -> io::Result<()> {
    let mut session = Session::enter(stdout_fd()?)?;
    session.write_frame(b"READY")?;
    let _keep = session;
    panic!("tty-io worker panic");
}

fn worker_suspend() -> io::Result<()> {
    let mut session = Session::enter(stdout_fd()?)?;
    session.install_handlers()?;
    session.write_frame(b"READY")?;
    loop {
        if session.take_resumed() {
            session.write_frame(b"CONT")?;
            session.restore()?;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn worker_hangup() -> io::Result<()> {
    let mut session = Session::enter(stdout_fd()?)?;
    session.install_handlers()?;
    session.write_frame(b"READY")?;
    loop {
        std::thread::sleep(Duration::from_secs(30));
    }
}

fn worker_force_exit() -> io::Result<()> {
    let mut session = Session::enter(stdout_fd()?)?;
    session.write_frame(b"READY")?;
    // Skip Drop: this is the path before a forced `_exit`.
    prepare_forced_exit();
    let _session = session;
    unsafe { libc::_exit(0) }
}

fn worker_crossterm() -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    eprintln!("READY");
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(1500) {
        if crossterm::event::poll(Duration::from_millis(50))? {
            match crossterm::event::read() {
                Ok(event) => eprintln!("EVENT {event:?}"),
                Err(err) => {
                    eprintln!("ERROR {err}");
                    break;
                }
            }
        }
    }
    let _ = crossterm::terminal::disable_raw_mode();
    Ok(())
}

fn probe_out(path: &str) -> io::Result<()> {
    let result = (|| {
        let mut session = Session::enter(stdout_fd()?)?;
        let caps = session.probe(PROBE_DEADLINE)?;
        let mut text = format_caps(&caps);
        let term = env::var("TERM").unwrap_or_default();
        let colorterm = env::var("COLORTERM").unwrap_or_default();
        text.push_str(&format!("term={term}\ncolorterm={colorterm}\n"));
        session.restore()?;
        Ok::<String, io::Error>(text)
    })();
    match result {
        Ok(text) => std::fs::write(path, text),
        Err(err) => {
            let _ = std::fs::write(path, format!("error={err}\n"));
            Err(err)
        }
    }
}

struct App {
    session: Session,
    quit: bool,
}

fn interactive() -> io::Result<()> {
    if !io::stdin().is_terminal() {
        eprintln!("tty-io: stdin is not a terminal; try --help");
        std::process::exit(2);
    }
    let mut session = Session::enter(stdin_fd()?)?;
    session.install_handlers()?;
    let caps = session.probe(PROBE_DEADLINE)?;
    let size = session.winsize()?;
    let mut grid = Grid::new(size.ws_col.max(1), size.ws_row.max(1));
    draw(&mut grid, &format_caps(&caps));
    session.write_frame(&grid.diff())?;

    let read_fd = session.try_clone_fd()?;
    let mut eloop: EventLoop<App> = EventLoop::try_new().map_err(io::Error::other)?;
    let source = Generic::new(read_fd, Interest::READ, Mode::Level);
    eloop
        .handle()
        .insert_source(source, |readiness, _fd, app| {
            if readiness.readable || readiness.error {
                if let Ok(events) = app.session.read_input() {
                    for event in events {
                        if quits(&event) {
                            app.quit = true;
                        }
                    }
                }
            }
            Ok(PostAction::Continue)
        })
        .map_err(|err| io::Error::other(err.to_string()))?;

    let mut app = App {
        session,
        quit: false,
    };
    while !app.quit {
        let timeout = app
            .session
            .poll_timeout()
            .unwrap_or(Duration::from_millis(200));
        eloop
            .dispatch(Some(timeout), &mut app)
            .map_err(io::Error::other)?;
        for event in app.session.expire_if_due() {
            if quits(&event) {
                app.quit = true;
            }
        }
    }
    app.session.restore()?;
    Ok(())
}

fn draw(grid: &mut Grid, report: &str) {
    let plain = Attrs::default();
    grid.put_text(
        0,
        0,
        "tty-io  q, Escape or Ctrl-C quits",
        Color::Default,
        Color::Default,
        plain,
    );
    for (row, line) in report.lines().enumerate() {
        let row = u16::try_from(row + 1).unwrap_or(u16::MAX);
        if row >= grid.rows() {
            break;
        }
        grid.put_text(0, row, line, Color::Default, Color::Default, plain);
    }
}

fn quits(input: &Input) -> bool {
    let Input::Key(key) = input else {
        return false;
    };
    if key.state != ButtonState::Pressed {
        return false;
    }
    match &key.key {
        Key::Named(name) if name == "Escape" => true,
        Key::Character(text) if text == "q" || text == "Q" => true,
        Key::Character(text) if text == "c" && key.modifiers.control => true,
        _ => false,
    }
}
