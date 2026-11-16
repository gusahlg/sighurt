//! Whether kitty graphics can reach the terminal through the tmux `sig` runs in.
//!
//! Owns: telling a tmux pane from another terminal, and asking that tmux (its own command, on
//! its own socket, `$TMUX`) for the pane's `allow-passthrough`. Must not own: what is drawn, the
//! encoders, or which protocol a layout uses (the app chooses from what this says).
//!
//! tmux does not hand the outer terminal's replies back to the pane, so nothing sent through
//! the pane can learn whether that terminal shows kitty graphics: the user says so with
//! `[tui] graphics = "kitty"`. What can be learnt is whether tmux passes the images on at all.
//! Only `all` is enough: with `on`, tmux drops what a pane passes while it is not visible (a
//! window not shown), so a page that changed meanwhile would show its old picture when the
//! window comes back.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long `tmux show` may take. tmux answers in milliseconds; a client that cannot reach its
/// server, or a tmux on `PATH` older than the server, may never answer.
const WAIT: Duration = Duration::from_secs(1);

/// The pane's `allow-passthrough`, as tmux said it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Passthrough {
    /// `all`: passed on whether or not the pane is visible.
    All,
    /// `on`: passed on only while the pane is visible.
    On,
    /// `off`, or tmux could not be asked.
    Off,
}

/// Whether `sig` runs in a tmux pane: `TERM` is tmux's (`tmux*`, or `screen*` as older tmux
/// configurations set it) and `$TMUX` names tmux's socket. A terminal started from a tmux pane
/// inherits `$TMUX` and sets its own `TERM`, so neither alone is enough; GNU screen sets
/// `screen*` and no `$TMUX`.
pub fn inside(term: Option<&OsStr>, tmux: Option<&OsStr>) -> bool {
    let term = term.map_or(&b""[..], OsStr::as_encoded_bytes);
    let tmux = tmux.is_some_and(|tmux| !tmux.is_empty());
    tmux && (term.starts_with(b"tmux") || term.starts_with(b"screen"))
}

/// Asks the tmux `sig` runs in for the pane's `allow-passthrough` (`tmux show -Apv`), waiting at
/// most [`WAIT`]. The `tmux` run is the first one in an absolute directory of `PATH`.
pub fn passthrough() -> Passthrough {
    let Some(tmux) = find_in_path("tmux", std::env::var_os("PATH").as_deref()) else {
        log::info!("no tmux in PATH to ask about allow-passthrough");
        return Passthrough::Off;
    };
    let child = Command::new(tmux)
        .args(["show", "-Apv", "allow-passthrough"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            log::info!("tmux could not be asked about allow-passthrough: {error}");
            return Passthrough::Off;
        }
    };
    let deadline = Instant::now() + WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                log::info!("tmux did not say what allow-passthrough is");
                return Passthrough::Off;
            }
        }
    }
    let output = child.wait_with_output().map(|output| output.stdout);
    let answer = output.unwrap_or_default();
    let answer = parse(&answer);
    log::info!("tmux's allow-passthrough: {answer:?}");
    answer
}

/// The value `tmux show -Apv` printed.
fn parse(output: &[u8]) -> Passthrough {
    match output.trim_ascii() {
        b"all" => Passthrough::All,
        b"on" => Passthrough::On,
        _ => Passthrough::Off,
    }
}

/// The first `name` in an absolute directory of `path` that is an executable file. A relative
/// or empty entry (the current directory) is never searched.
/// Without Unix's mode bits (Windows, which has no tmux of its own) a file is enough.
fn find_in_path(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|file| {
            Path::new(file).metadata().is_ok_and(|meta| {
                #[cfg(unix)]
                let runs = meta.permissions().mode() & 0o111 != 0;
                #[cfg(not(unix))]
                let runs = true;
                meta.is_file() && runs
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tmux_pane_needs_both_its_term_and_its_socket() {
        let some = |s| Some(OsStr::new(s));
        assert!(inside(
            some("tmux-256color"),
            some("/tmp/tmux-1000/default,1,0")
        ));
        assert!(inside(
            some("screen-256color"),
            some("/tmp/tmux-1000/default,1,0")
        ));
        // A terminal started from a tmux pane: its own TERM, tmux's TMUX.
        assert!(!inside(
            some("xterm-kitty"),
            some("/tmp/tmux-1000/default,1,0")
        ));
        // GNU screen.
        assert!(!inside(some("screen-256color"), None));
        assert!(!inside(some("tmux-256color"), some("")));
        assert!(!inside(None, some("/tmp/tmux-1000/default,1,0")));
    }

    #[test]
    fn only_what_tmux_prints_for_on_and_all_lets_images_through() {
        assert_eq!(parse(b"all\n"), Passthrough::All);
        assert_eq!(parse(b"on\n"), Passthrough::On);
        assert_eq!(parse(b"off\n"), Passthrough::Off);
        assert_eq!(parse(b""), Passthrough::Off);
        assert_eq!(
            parse(b"invalid option: allow-passthrough\n"),
            Passthrough::Off
        );
    }

    #[cfg(unix)]
    #[test]
    fn tmux_is_found_only_in_absolute_directories() {
        let dir = std::env::temp_dir().join(format!("sig-tmux-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("tmux");
        std::fs::write(&file, b"#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([Path::new("relative"), &dir]).unwrap();
        assert_eq!(find_in_path("tmux", Some(&path)), Some(file.clone()));
        assert_eq!(find_in_path("tmux", Some(OsStr::new("relative:"))), None);
        assert_eq!(find_in_path("tmux", None), None);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(find_in_path("tmux", Some(&path)), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
