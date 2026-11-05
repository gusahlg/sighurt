//! What `sig://config` does with the configuration file: reading it, checking a text against
//! this build, and replacing the file ([`PageRequest::ConfigRead`], `ConfigCheck`,
//! `ConfigWrite`). The core has already decided the request: the page is one whose grants
//! include it, and a write comes from a deliberate save in the selected tab.
//!
//! Each answer is a text whose first line says what happened; refusals are
//! [`ServiceError::Failed`] with a text of the same form. The format is between this module and
//! the page (`pages/sig-page-config`'s `reply`), which change together; it is not for users:
//!
//! - read: `file <path>`, `new <path>` (there is no file: the text is a starter), or
//!   `read-only <path>` (saves will be refused); then `target <target>`, the file a save
//!   replaces (the end of any symbolic links); then the text. Or `refused` and a message.
//! - check: `valid` and a `notice <line> <text>` line per notice, or `invalid` and a
//!   `problem <line> <text>` line per problem: what a reload of the text would report.
//!   `<line>` is the file's line it is about, 1-based, or 0 when it isn't known.
//! - write: `saved`, then a `restart <text>` line per setting that applies only when `sig`
//!   starts again (`[ui] chrome`, `[ui] renderer`, `[engines.<name>]`) and a `notice <text>`
//!   line per notice. Or `stale`, `invalid` (with problem lines) or `refused`, with a message.
//!
//! `replaces`, what the page read, names the file and says whether it existed:
//! `file <target>\n<contents>` or `missing <target>\n`, so an empty file and none differ, and
//! a read of one file can't stand for another the links lead to now. A write is refused as
//! stale unless the file is still that one and still holds that. It is refused too when the
//! text is invalid, over [`MAX_CONFIG`], or when the file or its directory can't be written
//! (a home-manager link into the read-only store).
//!
//! `sig`'s writers take turns: each holds an advisory lock (`.<name>.lock`, beside the target)
//! while it reads the file again, compares it with `replaces` and replaces it, so of two
//! `sig`s saving over the same text one is refused as stale. An editor that takes no such lock
//! (nvim in `sig://terminal`, or any other) can still write between that comparison and the
//! rename and lose its change: the guard is for `sig`'s own writers and for every change made
//! before the save, not a lock on the file. The file is replaced atomically
//! ([`persistence::replace_config`]): its mode kept (0600 for a new file), the directory left
//! as it is, created (0700) only when it is missing.
//!
//! The session takes the configuration a write returns into use at once, since the inotify
//! watch covers only the configuration's directory and misses a linked target. Which settings
//! need a restart is judged against the configuration in use (`current`): one changed by an
//! earlier save and still waiting for the restart was said then.
//!
//! Owns: the file's text on the way in and out. Must not own: who may ask (the core), parsing
//! or validating the configuration (the core's [`Config::parse`]), or taking a reloaded
//! configuration into use (the session queues it).
//!
//! [`PageRequest::ConfigRead`]: sighurt_api::services::PageRequest::ConfigRead

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{access, Access};
use sighurt_api::services::{ServiceError, ServiceReply, ServiceResult};
use sighurt_core::config::Config;
use sighurt_core::{Environment, Frontend};

use crate::persistence::{self, Paths};

/// The largest configuration `sig://config` reads or writes, in bytes.
pub const MAX_CONFIG: usize = 256 * 1024;

/// What a missing file starts as on the page: the version and where the settings are
/// described, not a copy of the defaults, which would pin every default into the file.
pub const STARTER: &str = "# sig's configuration. Every setting, with its default, is described \
                           at sig://help/config\n# (and printed by `sig --default-config`). \
                           What this file leaves out keeps its default.\nversion = 1\n";

/// Most symbolic links followed to find the file.
const MAX_LINKS: usize = 40;
/// How long a write waits for another `sig`'s write to the same file to finish.
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// The configuration file's text, for the page.
pub fn read(paths: &Paths) -> ServiceResult {
    answer(read_at(paths.config.as_deref()))
}

/// What a reload of `text` as the configuration of this build would report.
pub fn check(text: &str, env: &Environment) -> ServiceResult {
    answer(Ok(check_text(text, env)))
}

/// Replaces the file with `text` if it is still what `replaces` says, and parses it: the
/// answer for the page, and the configuration for the session to take into use now.
pub fn write(
    paths: &Paths,
    env: &Environment,
    current: &Config,
    text: &str,
    replaces: &str,
) -> (ServiceResult, Option<Config>) {
    match write_at(paths.config.as_deref(), env, current, text, replaces) {
        Ok((saved, config)) => (answer(Ok(saved)), Some(config)),
        Err(refused) => (answer(Err(refused)), None),
    }
}

fn answer(result: Result<String, String>) -> ServiceResult {
    result.map(ServiceReply::Text).map_err(ServiceError::Failed)
}

const NO_DIRECTORY: &str =
    "sig has no configuration directory ($XDG_CONFIG_HOME or $HOME/.config), so it has no \
     configuration file to edit.";

fn refused(message: &str) -> String {
    format!("refused\n{message}")
}

fn read_at(path: Option<&Path>) -> Result<String, String> {
    let Some(path) = path else {
        return Err(refused(NO_DIRECTORY));
    };
    let shown = one_line(&path.display().to_string());
    let target = resolve(path)
        .map_err(|error| refused(&format!("cannot read {}: {error}", path.display())))?;
    let named = format!("target {}", one_line(&target.display().to_string()));
    let bytes = match fs::read(&target) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(format!("new {shown}\n{named}\n{STARTER}"));
        }
        Err(error) => {
            return Err(refused(&format!(
                "cannot read {}: {error}",
                target.display()
            )))
        }
    };
    if bytes.len() > MAX_CONFIG {
        return Err(refused(&format!(
            "{} is {} KiB; sig://config edits files of at most {} KiB. Edit it with another \
             editor.",
            path.display(),
            bytes.len().div_ceil(1024),
            MAX_CONFIG / 1024
        )));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        refused(&format!(
            "{} is not UTF-8 text; sig reads only UTF-8.",
            path.display()
        ))
    })?;
    let state = if writable(&target).is_ok() {
        "file"
    } else {
        "read-only"
    };
    Ok(format!("{state} {shown}\n{named}\n{text}"))
}

fn check_text(text: &str, env: &Environment) -> String {
    if text.len() > MAX_CONFIG {
        return format!("invalid\nproblem 0 {}\n", too_large(text));
    }
    match Config::parse(Some(text), env) {
        Ok(config) => lines("valid", "notice", config.notices(), text),
        Err(error) => lines("invalid", "problem", error.problems(), text),
    }
}

/// What `replaces` says the file was: its target, and its contents (`None`: there was none).
fn snapshot(replaces: &str) -> Option<(PathBuf, Option<&[u8]>)> {
    let (head, contents) = replaces.split_once('\n')?;
    match head.split_once(' ')? {
        ("file", target) => Some((target.into(), Some(contents.as_bytes()))),
        ("missing", target) if contents.is_empty() => Some((target.into(), None)),
        _ => None,
    }
}

fn write_at(
    path: Option<&Path>,
    env: &Environment,
    current: &Config,
    text: &str,
    replaces: &str,
) -> Result<(String, Config), String> {
    let Some(path) = path else {
        return Err(refused(NO_DIRECTORY));
    };
    if text.len() > MAX_CONFIG {
        return Err(refused(&too_large(text)));
    }
    let config = Config::parse(Some(text), env)
        .map_err(|error| lines("invalid", "problem", error.problems(), text))?;
    let Some((read_target, read_contents)) = snapshot(replaces) else {
        return Err(refused("the page sent no record of what it read."));
    };
    let target = resolve(path)
        .map_err(|error| refused(&format!("cannot write {}: {error}", path.display())))?;
    let stale = |why: &str| {
        format!(
            "stale\n{} {why} since this page read it. Nothing was saved. Read the file again to \
             see what it holds now (Ctrl+Z brings your text back).",
            path.display()
        )
    };
    if target != read_target {
        return Err(stale("leads to another file"));
    }
    prepare(&target).map_err(|message| refused(&message))?;
    let _lock = lock(&target).map_err(|message| refused(&message))?;
    let (on_disk, mode) = match fs::read(&target) {
        Ok(bytes) => {
            let mode = fs::metadata(&target).map_or(0o600, |m| m.permissions().mode());
            (Some(bytes), mode)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (None, 0o600),
        Err(error) => {
            return Err(refused(&format!(
                "cannot read {}: {error}",
                target.display()
            )))
        }
    };
    if on_disk.as_deref() != read_contents {
        return Err(stale("changed (another sig, or an editor, wrote it)"));
    }
    persistence::replace_config(&target, text, mode)
        .map_err(|error| refused(&format!("cannot write {}: {error}", target.display())))?;
    let mut saved = String::from("saved\n");
    for line in restarts(&config, current, env) {
        saved.push_str(&format!("restart {line}\n"));
    }
    for line in config.notices().iter().flat_map(|n| n.lines()) {
        saved.push_str(&format!("notice {}\n", one_line(line)));
    }
    Ok((saved, config))
}

/// The settings of `config` that differ from `current` and apply only when `sig` starts
/// again, as the core says them when it takes the configuration into use.
fn restarts(config: &Config, current: &Config, env: &Environment) -> Vec<String> {
    let mut out = Vec::new();
    if config.chrome() != current.chrome() {
        out.push(format!(
            "[ui] chrome = {:?} applies when sig starts again.",
            config.chrome()
        ));
    }
    if config.renderer() != current.renderer() && env.frontend == Frontend::Gui {
        out.push(format!(
            "[ui] renderer = {:?} applies when sig starts again.",
            config.renderer()
        ));
    }
    for engine in env.engines.keys() {
        if config.engine_settings(engine) != current.engine_settings(engine) {
            out.push(format!("[engines.{engine}] applies when sig starts again."));
        }
    }
    out
}

/// Holds the lock of `sig`'s writers of `target` until dropped. Waits up to [`LOCK_WAIT`]
/// for another `sig`'s write.
fn lock(target: &Path) -> Result<fs::File, String> {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return Err("the file has no directory".into());
    };
    let path = dir.join(format!(".{}.lock", name.to_string_lossy()));
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("cannot lock {}: {error}", path.display()))?;
    let until = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "another sig is saving {}; save again in a moment.",
                    target.display()
                ))
            }
            Err(fs::TryLockError::Error(error)) => {
                return Err(format!("cannot lock {}: {error}", path.display()))
            }
        }
    }
}

fn too_large(text: &str) -> String {
    format!(
        "the text is {} KiB; a configuration file holds at most {} KiB.",
        text.len().div_ceil(1024),
        MAX_CONFIG / 1024
    )
}

/// `head`, then a `<kind> <line> <text>` line for each of `items`, located in `text`.
fn lines(head: &str, kind: &str, items: &[String], text: &str) -> String {
    let mut answer = format!("{head}\n");
    for item in items {
        let (line, message) = locate(item, text);
        answer.push_str(&format!("{kind} {line} {message}\n"));
    }
    answer
}

/// `text` on one line: line breaks become spaces.
fn one_line(text: &str) -> String {
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The line of `text` a problem (or notice) is about, 1-based or 0 when unknown, and the
/// problem on one line. A TOML syntax error names its line; it is shown without the excerpt
/// of the file the parser quotes. A setting's problem starts with its table and key
/// (`[ui] renderer: …`, or `zoom: …` for a top-level one), which are looked up in the file.
fn locate(problem: &str, text: &str) -> (usize, String) {
    if let Some(rest) = problem.strip_prefix("TOML parse error at line ") {
        let line = rest
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let message = problem
            .lines()
            .skip(1)
            .filter(|l| !is_excerpt(l))
            .collect::<Vec<_>>()
            .join(" ");
        return (line, format!("line {line}: {}", one_line(&message)));
    }
    (setting_line(problem, text).unwrap_or(0), one_line(problem))
}

/// Whether `line` is part of the excerpt a TOML error quotes: `  |`, `3 | …`, `  |   ^`.
fn is_excerpt(line: &str) -> bool {
    let rest = line
        .trim_start()
        .trim_start_matches(|c: char| c.is_ascii_digit());
    rest.trim_start().starts_with('|')
}

/// The line of the setting a problem names, if the file has it.
fn setting_line(problem: &str, text: &str) -> Option<usize> {
    let (table, rest) = match problem.strip_prefix('[') {
        Some(rest) => {
            let (table, rest) = rest.split_once(']')?;
            (Some(table.trim()), rest.trim_start())
        }
        None => (None, problem),
    };
    // `unknown key `hme``, `[session]: unknown field `save``: the key is the quoted one.
    let quoted = ["unknown key `", "unknown field `"]
        .iter()
        .find_map(|prefix| rest.split_once(prefix))
        .and_then(|(_, key)| key.split('`').next());
    let key = quoted
        .unwrap_or_else(|| rest.split([':', ' ', '=']).next().unwrap_or(""))
        .trim_matches('"');
    let mut current: Option<&str> = None;
    let mut header = None;
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[') {
            let name = name.trim_start_matches('[');
            let name = name.split(']').next().unwrap_or("").trim();
            current = Some(name);
            if table == Some(name) {
                header = Some(n + 1);
            }
            continue;
        }
        if current != table || key.is_empty() {
            continue;
        }
        let name = line
            .split('=')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"');
        if line.contains('=') && name == key {
            return Some(n + 1);
        }
    }
    header
}

/// The file `path` names: the target of a symbolic link, followed to the end, which is what
/// is replaced. A link to a file that doesn't exist yet names that file.
fn resolve(path: &Path) -> io::Result<PathBuf> {
    let mut path = path.to_owned();
    for _ in 0..MAX_LINKS {
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = fs::read_link(&path)?;
                path = match path.parent() {
                    Some(dir) => dir.join(target),
                    None => target,
                };
            }
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("too many levels of symbolic links"))
}

/// Whether `target` can be replaced: it is writable when it exists, and its directory is
/// writable. A message for the user when it can't.
fn writable(target: &Path) -> Result<(), String> {
    let read_only = |what: &Path| {
        format!(
            "{} is read-only, so sig://config can't save it. A file managed elsewhere (by \
             home-manager, a link into the read-only store) is changed where it is managed.",
            what.display()
        )
    };
    if target.exists() && access(target, Access::WRITE_OK).is_err() {
        return Err(read_only(target));
    }
    let dir = target.parent().unwrap_or(Path::new("/"));
    if dir.exists() && access(dir, Access::WRITE_OK | Access::EXEC_OK).is_err() {
        return Err(read_only(dir));
    }
    Ok(())
}

/// Makes `target` ready to be replaced: its directory created (mode 0700) when it is
/// missing, and both found writable.
fn prepare(target: &Path) -> Result<(), String> {
    let dir = target.parent().unwrap_or(Path::new("/"));
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    }
    writable(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn env() -> Environment {
        Environment {
            renderers: sighurt_core::config::RENDERERS,
            ..Environment::default()
        }
    }

    fn builtin() -> Config {
        Config::parse(None, &env()).unwrap()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Reads `path` as the page does: the answer, and the `replaces` a save sends.
    fn page_read(path: &Path) -> (String, String) {
        let answer = read_at(Some(path)).unwrap();
        let mut lines = answer.splitn(3, '\n');
        let (state, target, text) = (
            lines.next().unwrap(),
            lines.next().unwrap(),
            lines.next().unwrap(),
        );
        let target = target.strip_prefix("target ").unwrap();
        let replaces = if state.starts_with("new ") {
            format!("missing {target}\n")
        } else {
            format!("file {target}\n{text}")
        };
        (answer, replaces)
    }

    fn save(path: &Path, text: &str, replaces: &str) -> Result<String, String> {
        write_at(Some(path), &env(), &builtin(), text, replaces).map(|(saved, _)| saved)
    }

    /// Whether this process ignores permissions (root), which the read-only cases need.
    fn privileged() -> bool {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    /// The names in `dir`, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_missing_file_reads_as_a_starter_and_saves_over_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sighurt").join("config.toml");
        let (answer, replaces) = page_read(&path);
        let shown = path.display();
        assert_eq!(answer, format!("new {shown}\ntarget {shown}\n{STARTER}"));
        assert_eq!(replaces, format!("missing {shown}\n"));
        assert!(Config::parse(Some(STARTER), &env()).is_ok());
        assert!(STARTER.contains("sig://help/config"));

        // The missing directory is created, private, and the new file is 0600.
        let text = format!("{STARTER}zoom = 1.5\n");
        let (saved, config) = write_at(Some(&path), &env(), &builtin(), &text, &replaces).unwrap();
        assert_eq!(saved, "saved\n");
        assert_eq!(config.zoom(), 1.5, "the configuration to take into use");
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(
            page_read(&path).0,
            format!("file {shown}\ntarget {shown}\n{text}")
        );
        // Only the file and the writers' lock are left: no temporary file.
        assert_eq!(
            names(path.parent().unwrap()),
            [".config.toml.lock", "config.toml"]
        );
    }

    #[test]
    fn the_service_answers_and_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: Some(dir.path().join("config.toml")),
            session: None,
        };
        assert!(matches!(read(&paths), Ok(ServiceReply::Text(t)) if t.starts_with("new ")));
        assert!(
            matches!(check("zoom = 1", &env()), Ok(ServiceReply::Text(t)) if t.starts_with("invalid\n"))
        );
        let replaces = format!("missing {}\n", dir.path().join("config.toml").display());
        let (answer, config) = write(&paths, &env(), &builtin(), "version = 1\n", &replaces);
        assert_eq!(answer, Ok(ServiceReply::Text("saved\n".into())));
        assert!(config.is_some());
        let (answer, config) = write(&paths, &env(), &builtin(), "version = 1\n", &replaces);
        assert!(matches!(answer, Err(ServiceError::Failed(t)) if t.starts_with("stale\n")));
        assert!(config.is_none());

        let none = Paths {
            config: None,
            session: None,
        };
        assert!(matches!(read(&none), Err(ServiceError::Failed(t)) if t.starts_with("refused\n")));
        let (answer, _) = write(&none, &env(), &builtin(), "version = 1\n", "missing x\n");
        assert!(matches!(answer, Err(ServiceError::Failed(t)) if t.starts_with("refused\n")));
        // A malformed record of what was read is refused, not taken for anything.
        let (answer, _) = write(&paths, &env(), &builtin(), "version = 1\n", "version = 1\n");
        assert!(matches!(answer, Err(ServiceError::Failed(t)) if t.starts_with("refused\n")));
    }

    #[test]
    fn a_check_reports_the_problems_a_reload_would_with_their_lines() {
        assert_eq!(check_text("version = 1\nzoom = 2\n", &env()), "valid\n");
        let answer = check_text("version = 1\n\n[ui]\nrenderer = \"nope\"\n", &env());
        let mut lines = answer.lines();
        assert_eq!(lines.next(), Some("invalid"));
        let problem = lines.next().unwrap();
        assert!(
            problem.starts_with("problem 4 [ui] renderer: "),
            "{problem}"
        );
        assert_eq!(lines.next(), None);

        // A TOML syntax error: its line, on one line, without the parser's excerpt.
        let answer = check_text("version = 1\nzoom = = 2\n", &env());
        let problem = answer.lines().nth(1).unwrap();
        assert!(problem.starts_with("problem 2 line 2: "), "{answer}");
        assert!(
            !problem.contains('|') && !answer.contains("TOML parse error"),
            "{answer}"
        );

        // A top-level setting, an unknown key, and one the file doesn't have.
        let answer = check_text("version = 1\nzoom = 9\nbogus = 1\n", &env());
        assert!(answer.contains("problem 2 zoom: "), "{answer}");
        assert!(answer.contains("problem 3 "), "{answer}");
        let answer = check_text("zoom = 1\n", &env());
        assert!(answer.starts_with("invalid\nproblem 0 the file needs `version = 1`"));
        // Notices of a valid file.
        let answer = check_text("version = 1\n\n[chrome.nothing]\nx = 1\n", &env());
        assert!(answer.starts_with("valid\nnotice 3 "), "{answer}");
    }

    #[test]
    fn an_invalid_text_is_refused_and_the_file_stays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "version = 1\n").unwrap();
        let (_, replaces) = page_read(&path);
        let answer = save(&path, "version = 1\nzoom = 99\n", &replaces).unwrap_err();
        assert!(answer.starts_with("invalid\nproblem 2 zoom: "), "{answer}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "version = 1\n");
    }

    #[test]
    fn a_file_changed_since_it_was_read_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "version = 1\n").unwrap();
        let (_, replaces) = page_read(&path);
        fs::write(&path, "version = 1\nzoom = 2\n").unwrap();
        let answer = save(&path, "version = 1\nzoom = 3\n", &replaces).unwrap_err();
        assert!(answer.starts_with("stale\n"), "{answer}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "version = 1\nzoom = 2\n"
        );
        // So is a file that is gone.
        fs::remove_file(&path).unwrap();
        assert!(save(&path, "version = 1\n", &replaces)
            .unwrap_err()
            .starts_with("stale\n"));
        assert!(!path.exists());
    }

    #[test]
    fn a_missing_file_and_an_empty_one_differ() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (_, missing) = page_read(&path);
        fs::write(&path, "").unwrap();
        let (_, empty) = page_read(&path);
        assert_ne!(missing, empty);
        assert!(save(&path, "version = 1\n", &missing)
            .unwrap_err()
            .starts_with("stale\n"));
        fs::remove_file(&path).unwrap();
        assert!(save(&path, "version = 1\n", &empty)
            .unwrap_err()
            .starts_with("stale\n"));
        assert_eq!(save(&path, "version = 1\n", &missing).unwrap(), "saved\n");
    }

    #[test]
    fn a_read_is_bound_to_the_file_the_links_led_to() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.toml"), dir.path().join("b.toml"));
        fs::write(&a, "version = 1\n").unwrap();
        fs::write(&b, "version = 1\n").unwrap();
        let path = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&a, &path).unwrap();
        let (_, replaces) = page_read(&path);
        // The link now leads to another file holding the same text.
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&b, &path).unwrap();
        let answer = save(&path, "version = 1\nzoom = 2\n", &replaces).unwrap_err();
        assert!(
            answer.starts_with("stale\n") && answer.contains("another file"),
            "{answer}"
        );
        assert_eq!(fs::read_to_string(&b).unwrap(), "version = 1\n");
    }

    #[test]
    fn sigs_racing_to_save_over_the_same_text_take_turns_and_one_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "version = 1\n").unwrap();
        let (_, replaces) = page_read(&path);
        let writers = 8;
        let start = Arc::new(Barrier::new(writers));
        let threads: Vec<_> = (0..writers)
            .map(|n| {
                let (path, replaces, start) = (path.clone(), replaces.clone(), Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    save(&path, &format!("version = 1\nzoom = 1.{n}\n"), &replaces)
                })
            })
            .collect();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "{results:?}"
        );
        assert!(
            results
                .iter()
                .filter_map(|r| r.as_ref().err())
                .all(|e| e.starts_with("stale\n")),
            "{results:?}"
        );

        // A writer that finds the lock held waits, then compares under it: another sig that
        // wrote meanwhile makes it stale.
        let (_, replaces) = page_read(&path);
        let held = lock(&path).unwrap();
        let waiting = {
            let (path, replaces) = (path.clone(), replaces.clone());
            std::thread::spawn(move || save(&path, "version = 1\nzoom = 3\n", &replaces))
        };
        std::thread::sleep(Duration::from_millis(100));
        fs::write(&path, "version = 1\nzoom = 2\n").unwrap();
        drop(held);
        assert!(waiting.join().unwrap().unwrap_err().starts_with("stale\n"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "version = 1\nzoom = 2\n"
        );
    }

    #[test]
    fn a_link_is_kept_and_its_target_replaced_with_its_mode_and_directory() {
        let dir = tempfile::tempdir().unwrap();
        let dotfiles = dir.path().join("dotfiles");
        fs::create_dir(&dotfiles).unwrap();
        fs::set_permissions(&dotfiles, fs::Permissions::from_mode(0o755)).unwrap();
        let target = dotfiles.join("sig.toml");
        fs::write(&target, "version = 1\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let config = dir.path().join("sighurt");
        fs::create_dir(&config).unwrap();
        let path = config.join("config.toml");
        // A relative link, then a link to it.
        std::os::unix::fs::symlink("../dotfiles/sig.toml", config.join("real.toml")).unwrap();
        std::os::unix::fs::symlink("real.toml", &path).unwrap();
        let (answer, replaces) = page_read(&path);
        assert!(
            answer.starts_with(&format!("file {}\ntarget ", path.display())),
            "{answer}"
        );
        let text = "version = 1\nzoom = 2\n";
        assert_eq!(save(&path, text, &replaces).unwrap(), "saved\n");
        assert!(fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), text);
        assert_eq!(mode(&target), 0o644, "the file's mode is kept");
        assert_eq!(
            mode(&dotfiles),
            0o755,
            "the directory's mode is not changed"
        );
        assert_eq!(
            names(&dotfiles),
            [".sig.toml.lock", "sig.toml"],
            "no temporary file"
        );
        // A link whose target doesn't exist yet: the target is created.
        let dangling = config.join("dangling.toml");
        std::os::unix::fs::symlink("../dotfiles/later.toml", &dangling).unwrap();
        let (answer, replaces) = page_read(&dangling);
        assert!(answer.starts_with("new "));
        assert_eq!(save(&dangling, text, &replaces).unwrap(), "saved\n");
        assert_eq!(
            fs::read_to_string(dotfiles.join("later.toml")).unwrap(),
            text
        );
        assert_eq!(mode(&dotfiles), 0o755);
        // A loop is an error, not a hang.
        std::os::unix::fs::symlink("loop.toml", config.join("loop.toml")).unwrap();
        assert!(read_at(Some(&config.join("loop.toml")))
            .unwrap_err()
            .starts_with("refused\n"));
    }

    #[test]
    fn an_existing_files_mode_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "version = 1\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let (_, replaces) = page_read(&path);
        assert_eq!(
            save(&path, "version = 1\nzoom = 2\n", &replaces).unwrap(),
            "saved\n"
        );
        assert_eq!(mode(&path), 0o640);
    }

    #[test]
    fn a_read_only_file_or_directory_is_refused_with_a_message() {
        if privileged() {
            eprintln!("skipped: root writes read-only files");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // A store path: a read-only file in a read-only directory, linked from the config's.
        let store = dir.path().join("store");
        fs::create_dir(&store).unwrap();
        let stored = store.join("config.toml");
        fs::write(&stored, "version = 1\n").unwrap();
        fs::set_permissions(&stored, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&store, fs::Permissions::from_mode(0o555)).unwrap();
        let path = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&stored, &path).unwrap();
        let (answer, replaces) = page_read(&path);
        assert!(answer.starts_with("read-only "));
        let answer = save(&path, "version = 1\nzoom = 2\n", &replaces).unwrap_err();
        assert!(
            answer.starts_with("refused\n") && answer.contains("read-only"),
            "{answer}"
        );
        assert!(answer.contains("home-manager"), "{answer}");
        assert_eq!(fs::read_to_string(&stored).unwrap(), "version = 1\n");

        // A read-only file in a writable directory.
        let plain = dir.path().join("plain");
        fs::create_dir(&plain).unwrap();
        let file = plain.join("config.toml");
        fs::write(&file, "version = 1\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o400)).unwrap();
        let (_, replaces) = page_read(&file);
        let answer = save(&file, "version = 1\nzoom = 2\n", &replaces).unwrap_err();
        assert!(
            answer.starts_with("refused\n") && answer.contains("read-only"),
            "{answer}"
        );

        // A writable file in a read-only directory: the temporary can't be made beside it.
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        let file = locked.join("config.toml");
        fs::write(&file, "version = 1\n").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let (_, replaces) = page_read(&file);
        let answer = save(&file, "version = 1\nzoom = 2\n", &replaces).unwrap_err();
        assert!(
            answer.starts_with("refused\n") && answer.contains("read-only"),
            "{answer}"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "version = 1\n");
        assert_eq!(mode(&locked), 0o555, "the directory's mode is not changed");
        fs::set_permissions(&store, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn texts_and_files_over_the_cap_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let big = format!("version = 1\n{}", "#\n".repeat(MAX_CONFIG / 2));
        assert!(big.len() > MAX_CONFIG);
        let (_, replaces) = page_read(&path);
        let answer = save(&path, &big, &replaces).unwrap_err();
        assert!(
            answer.starts_with("refused\n") && answer.contains("256 KiB"),
            "{answer}"
        );
        assert!(!path.exists());
        assert!(check_text(&big, &env()).starts_with("invalid\nproblem 0 "));
        fs::write(&path, &big).unwrap();
        let answer = read_at(Some(&path)).unwrap_err();
        assert!(
            answer.starts_with("refused\n") && answer.contains("256 KiB"),
            "{answer}"
        );
        // At the cap exactly, it is read.
        let fits = format!("version = 1\n{}", "#".repeat(MAX_CONFIG - 12));
        assert_eq!(fits.len(), MAX_CONFIG);
        fs::write(&path, &fits).unwrap();
        assert!(read_at(Some(&path)).unwrap().starts_with("file "));
        // Not UTF-8: refused, not mangled.
        fs::write(&path, b"version = 1\n\xff\n").unwrap();
        assert!(read_at(Some(&path)).unwrap_err().starts_with("refused\n"));
    }

    #[test]
    fn the_answer_says_which_settings_need_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "version = 1\n").unwrap();
        let (_, replaces) = page_read(&path);
        let text = "version = 1\nzoom = 2\n\n[ui]\nrenderer = \"cpu\"\n\n[chrome.nothing]\nx = 1\n";
        let (saved, config) = write_at(Some(&path), &env(), &builtin(), text, &replaces).unwrap();
        let mut lines = saved.lines();
        assert_eq!(lines.next(), Some("saved"));
        assert_eq!(
            lines.next(),
            Some("restart [ui] renderer = \"cpu\" applies when sig starts again.")
        );
        assert!(
            lines.next().unwrap().starts_with("notice [chrome.nothing]"),
            "{saved}"
        );
        assert_eq!(lines.next(), None);
        assert_eq!(config.zoom(), 2.0);
        // Against a configuration that already has it, nothing waits for a restart; nor in the
        // terminal, which has no renderer.
        let (_, replaces) = page_read(&path);
        let (saved, _) = write_at(Some(&path), &env(), &config, text, &replaces).unwrap();
        assert!(!saved.contains("restart"), "{saved}");
        let tui = Environment {
            frontend: Frontend::Terminal,
            ..env()
        };
        let (_, replaces) = page_read(&path);
        let (saved, _) = write_at(Some(&path), &tui, &builtin(), text, &replaces).unwrap();
        assert!(!saved.contains("restart"), "{saved}");
    }
}
