//! `sig://config`'s file work: reading the user's configuration file for the page, checking a
//! text against this build, and writing it back, then reloading it.
//!
//! Each operation answers with a text the page reads ([`read`], [`check`], [`write`]). Its
//! first line says what happened; the format is the page's (`pages/sig-page-config`'s
//! `reply`) and is not for users:
//!
//! - read: `file <path>`, `new <path>` (there is no file: the text is a starter, and the file's
//!   contents are empty), or `read-only <path>` (saves are refused), then the text; or
//!   `refused` and a message.
//! - check: `valid` and a `notice <line> <text>` line per notice, or `invalid` and a
//!   `problem <line> <text>` line per problem. `<line>` is the file's line the problem is
//!   on, 1-based, or 0 when it isn't known.
//! - write: `saved` and a `restart <text>` line per setting that applies when `sig` starts
//!   again and a `notice <text>` line per other thing the reload said; or `stale` (the file is
//!   no longer the text the page started from), `invalid` (with problem lines as above) or
//!   `refused`, each with a message.
//!
//! A write is refused unless the file still holds `replaces` (another `sig`, or an editor in
//! `sig://terminal`, may have changed it), when the text is invalid, over [`MAX_CONFIG`], or
//! when the file or its directory can't be written (a home-manager link into the read-only
//! store). It replaces the target of a symbolic link, not the link, atomically
//! ([`persistence::replace_config`]), keeping the file's mode (0600 for a new file) and creating
//! the directory when it is missing. The reload after it is the caller's (`reload`), since the
//! inotify watch covers only the configuration's directory and misses a linked target; what
//! the reload reports comes back in the answer.
//!
//! Must not own: parsing or validating the configuration (the core's [`Config::parse`]),
//! deciding who may ask (the core's policy), or carrying a reloaded configuration into use
//! (the caller, through the core).

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{access, Access};
use sighurt_core::config::Config;
use sighurt_core::Environment;

use crate::persistence;

/// The largest configuration `sig://config` reads or writes, in bytes.
pub const MAX_CONFIG: usize = 256 * 1024;

/// What a missing file starts as on the page: the version and where the settings are
/// described, not a copy of the defaults, which would pin every default into the file.
pub const STARTER: &str = "# sig's configuration. Every setting, with its default, is described \
                           at sig://help/config\n# (and printed by `sig --default-config`). \
                           What this file leaves out keeps its default.\nversion = 1\n";

/// What a reload says when a setting changed that applies only when `sig` starts again
/// (`[ui] chrome`, `[ui] renderer`, `[engines.<name>]`): the core's notice says it.
const RESTART: &str = "when sig starts again";

/// Most symbolic links followed to find the file.
const MAX_LINKS: usize = 40;

/// The answer to a read of the configuration at `path` (none: `sig` has no configuration
/// directory).
pub fn read(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return refused(NO_DIRECTORY);
    };
    let target = match resolve(path) {
        Ok(target) => target,
        Err(error) => return refused(&format!("cannot read {}: {error}", path.display())),
    };
    let bytes = match fs::read(&target) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return format!("new {}\n{STARTER}", one_line(&path.display().to_string()));
        }
        Err(error) => return refused(&format!("cannot read {}: {error}", target.display())),
    };
    if bytes.len() > MAX_CONFIG {
        return refused(&format!(
            "{} is {} KiB; sig://config edits files of at most {} KiB. Edit it with another \
             editor.",
            path.display(),
            bytes.len().div_ceil(1024),
            MAX_CONFIG / 1024
        ));
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return refused(&format!(
            "{} is not UTF-8 text; sig reads only UTF-8.",
            path.display()
        ));
    };
    let state = if writable(&target).is_ok() {
        "file"
    } else {
        "read-only"
    };
    format!("{state} {}\n{text}", one_line(&path.display().to_string()))
}

/// The answer to a check of `text` as the configuration, for a build with what `env` lists:
/// the problems a reload of it would report, or the notices it would give.
pub fn check(text: &str, env: &Environment) -> String {
    if text.len() > MAX_CONFIG {
        return format!("invalid\nproblem 0 {}\n", too_large(text));
    }
    match Config::parse(Some(text), env) {
        Ok(config) => lines("valid", "notice", config.notices(), text),
        Err(error) => lines("invalid", "problem", error.problems(), text),
    }
}

/// The answer to writing `text` to the configuration at `path` in place of `replaces`, for a
/// build with what `env` lists. When it is written, `reload` takes the configuration into use
/// and returns what that said (the core's notices), which comes back in the answer.
pub fn write(
    path: Option<&Path>,
    text: &str,
    replaces: &str,
    env: &Environment,
    reload: impl FnOnce(Config) -> Vec<String>,
) -> String {
    let Some(path) = path else {
        return refused(NO_DIRECTORY);
    };
    if text.len() > MAX_CONFIG {
        return refused(&too_large(text));
    }
    let config = match Config::parse(Some(text), env) {
        Ok(config) => config,
        Err(error) => return lines("invalid", "problem", error.problems(), text),
    };
    let target = match resolve(path) {
        Ok(target) => target,
        Err(error) => return refused(&format!("cannot write {}: {error}", path.display())),
    };
    let (on_disk, mode) = match fs::read(&target) {
        Ok(bytes) => {
            let mode = fs::metadata(&target).map_or(0o600, |m| m.permissions().mode());
            (bytes, mode)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (Vec::new(), 0o600),
        Err(error) => return refused(&format!("cannot read {}: {error}", target.display())),
    };
    if on_disk != replaces.as_bytes() {
        return format!(
            "stale\n{} changed since this page read it (another sig, or an editor, wrote it). \
             Nothing was saved. Reload it from disk to see what it holds now; copy your changes \
             first to keep them.",
            path.display()
        );
    }
    if let Err(message) = prepare(&target) {
        return refused(&message);
    }
    if let Err(error) = persistence::replace_config(&target, text, mode) {
        return refused(&format!("cannot write {}: {error}", target.display()));
    }
    let said = reload(config);
    let mut answer = String::from("saved\n");
    for line in said.iter().flat_map(|notice| notice.lines()) {
        let kind = if line.contains(RESTART) {
            "restart"
        } else {
            "notice"
        };
        answer.push_str(&format!("{kind} {}\n", one_line(line)));
    }
    answer
}

const NO_DIRECTORY: &str =
    "sig has no configuration directory ($XDG_CONFIG_HOME or $HOME/.config), so it has no \
     configuration file to edit.";

fn refused(message: &str) -> String {
    format!("refused\n{message}")
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
    use sighurt_core::{BrowserEvent, BrowserState, Effect};

    fn env() -> Environment {
        Environment {
            renderers: sighurt_core::config::RENDERERS,
            ..Environment::default()
        }
    }

    /// A reload through the core, as `sig` does it: the notices it gives.
    fn reload_into(state: &mut BrowserState) -> impl FnOnce(Config) -> Vec<String> + '_ {
        |config| {
            sighurt_core::update(state, BrowserEvent::ConfigReloaded(Ok(Box::new(config))))
                .into_iter()
                .filter_map(|effect| match effect {
                    Effect::Notice { message, .. } => Some(message),
                    _ => None,
                })
                .collect()
        }
    }

    fn browser() -> BrowserState {
        BrowserState::new(Config::parse(None, &env()).unwrap(), env())
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn no_reload(_: Config) -> Vec<String> {
        panic!("nothing was written, so nothing reloads")
    }

    /// Whether this process ignores permissions (root), which the read-only cases need.
    fn privileged() -> bool {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    #[test]
    fn a_missing_file_reads_as_a_starter_and_saves_over_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sighurt").join("config.toml");
        let answer = read(Some(&path));
        assert_eq!(answer, format!("new {}\n{STARTER}", path.display()));
        assert!(Config::parse(Some(STARTER), &env()).is_ok());
        assert!(STARTER.contains("sig://help/config"));

        // The file's contents are empty: that is what the write replaces. The missing
        // directory is created, private, and the new file is 0600.
        let mut state = browser();
        let text = format!("{STARTER}zoom = 1.5\n");
        assert_eq!(
            write(Some(&path), &text, "", &env(), reload_into(&mut state)),
            "saved\n"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(state.config().zoom(), 1.5, "the reload applied it");
        assert_eq!(
            read(Some(&path)),
            format!("file {}\n{text}", path.display())
        );
        // No temporary file is left.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn no_configuration_directory_is_refused() {
        assert!(read(None).starts_with("refused\n"));
        assert!(write(None, "version = 1\n", "", &env(), no_reload).starts_with("refused\n"));
    }

    #[test]
    fn a_check_reports_the_problems_a_reload_would_with_their_lines() {
        assert_eq!(check("version = 1\nzoom = 2\n", &env()), "valid\n");
        let answer = check("version = 1\n\n[ui]\nrenderer = \"nope\"\n", &env());
        let mut lines = answer.lines();
        assert_eq!(lines.next(), Some("invalid"));
        let problem = lines.next().unwrap();
        assert!(
            problem.starts_with("problem 4 [ui] renderer: "),
            "{problem}"
        );
        assert_eq!(lines.next(), None);

        // A TOML syntax error: its line, on one line, without the parser's excerpt.
        let answer = check("version = 1\nzoom = = 2\n", &env());
        let problem = answer.lines().nth(1).unwrap();
        assert!(problem.starts_with("problem 2 line 2: "), "{answer}");
        assert!(
            !problem.contains('|') && !answer.contains("TOML parse error"),
            "{answer}"
        );

        // A top-level setting, an unknown key, and one the file doesn't have.
        let answer = check("version = 1\nzoom = 9\nbogus = 1\n", &env());
        assert!(answer.contains("problem 2 zoom: "), "{answer}");
        assert!(answer.contains("problem 3 "), "{answer}");
        let answer = check("zoom = 1\n", &env());
        assert!(answer.starts_with("invalid\nproblem 0 the file needs `version = 1`"));
    }

    #[test]
    fn notices_of_a_valid_file_are_reported() {
        let answer = check("version = 1\n\n[chrome.nothing]\nx = 1\n", &env());
        assert!(answer.starts_with("valid\nnotice 3 "), "{answer}");
    }

    #[test]
    fn an_invalid_text_is_refused_and_the_file_stays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "version = 1\n").unwrap();
        let answer = write(
            Some(&path),
            "version = 1\nzoom = 99\n",
            "version = 1\n",
            &env(),
            no_reload,
        );
        assert!(answer.starts_with("invalid\nproblem 2 zoom: "), "{answer}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "version = 1\n");
    }

    #[test]
    fn a_file_changed_since_it_was_read_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "version = 1\nzoom = 2\n").unwrap();
        let answer = write(
            Some(&path),
            "version = 1\nzoom = 3\n",
            "version = 1\n",
            &env(),
            no_reload,
        );
        assert!(answer.starts_with("stale\n"), "{answer}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "version = 1\nzoom = 2\n"
        );
        // A file that appeared since the page found none is stale too.
        let new = dir.path().join("new.toml");
        let answer = read(Some(&new));
        assert!(answer.starts_with("new "));
        fs::write(&new, "version = 1\n").unwrap();
        let answer = write(Some(&new), STARTER, "", &env(), no_reload);
        assert!(answer.starts_with("stale\n"), "{answer}");
        // So is a file that is gone.
        fs::remove_file(&path).unwrap();
        let answer = write(
            Some(&path),
            "version = 1\n",
            "version = 1\nzoom = 2\n",
            &env(),
            no_reload,
        );
        assert!(answer.starts_with("stale\n"), "{answer}");
        assert!(!path.exists());
    }

    #[test]
    fn a_link_is_kept_and_its_target_replaced_with_its_mode() {
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
        assert_eq!(
            read(Some(&path)),
            format!("file {}\nversion = 1\n", path.display())
        );
        let mut state = browser();
        let text = "version = 1\nzoom = 2\n";
        let answer = write(
            Some(&path),
            text,
            "version = 1\n",
            &env(),
            reload_into(&mut state),
        );
        assert_eq!(answer, "saved\n");
        assert!(fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), text);
        assert_eq!(mode(&target), 0o644, "the file's mode is kept");
        assert_eq!(mode(&dotfiles), 0o755, "the directory is not changed");
        assert_eq!(
            fs::read_dir(&dotfiles).unwrap().count(),
            1,
            "no temporary file"
        );
        // A link whose target doesn't exist yet: the target is created.
        let dangling = config.join("dangling.toml");
        std::os::unix::fs::symlink("../dotfiles/later.toml", &dangling).unwrap();
        assert!(read(Some(&dangling)).starts_with("new "));
        let answer = write(Some(&dangling), text, "", &env(), |_| Vec::new());
        assert_eq!(answer, "saved\n");
        assert_eq!(
            fs::read_to_string(dotfiles.join("later.toml")).unwrap(),
            text
        );
        // A loop is an error, not a hang.
        std::os::unix::fs::symlink("loop.toml", config.join("loop.toml")).unwrap();
        assert!(read(Some(&config.join("loop.toml"))).starts_with("refused\n"));
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
        assert!(read(Some(&path)).starts_with("read-only "));
        let answer = write(
            Some(&path),
            "version = 1\nzoom = 2\n",
            "version = 1\n",
            &env(),
            no_reload,
        );
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
        let answer = write(
            Some(&file),
            "version = 1\nzoom = 2\n",
            "version = 1\n",
            &env(),
            no_reload,
        );
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
        let answer = write(
            Some(&file),
            "version = 1\nzoom = 2\n",
            "version = 1\n",
            &env(),
            no_reload,
        );
        assert!(
            answer.starts_with("refused\n") && answer.contains("read-only"),
            "{answer}"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "version = 1\n");
        fs::set_permissions(&store, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn texts_and_files_over_the_cap_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let big = format!("version = 1\n{}", "#\n".repeat(MAX_CONFIG / 2));
        assert!(big.len() > MAX_CONFIG);
        let answer = write(Some(&path), &big, "", &env(), no_reload);
        assert!(
            answer.starts_with("refused\n") && answer.contains("256 KiB"),
            "{answer}"
        );
        assert!(!path.exists());
        assert!(check(&big, &env()).starts_with("invalid\nproblem 0 "));
        fs::write(&path, &big).unwrap();
        let answer = read(Some(&path));
        assert!(
            answer.starts_with("refused\n") && answer.contains("256 KiB"),
            "{answer}"
        );
        // At the cap exactly, it is read.
        let fits = format!("version = 1\n{}", "#".repeat(MAX_CONFIG - 12));
        assert_eq!(fits.len(), MAX_CONFIG);
        fs::write(&path, &fits).unwrap();
        assert!(read(Some(&path)).starts_with("file "));
        // Not UTF-8: refused, not mangled.
        fs::write(&path, b"version = 1\n\xff\n").unwrap();
        assert!(read(Some(&path)).starts_with("refused\n"));
    }

    #[test]
    fn the_reload_says_which_settings_need_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "version = 1\n").unwrap();
        let mut state = browser();
        let text = "version = 1\nzoom = 2\n\n[ui]\nrenderer = \"cpu\"\n";
        let answer = write(
            Some(&path),
            text,
            "version = 1\n",
            &env(),
            reload_into(&mut state),
        );
        let mut lines = answer.lines();
        assert_eq!(lines.next(), Some("saved"));
        let restart = lines.next().unwrap();
        assert!(
            restart.starts_with("restart [ui] renderer = \"cpu\" applies when sig starts again"),
            "{answer}"
        );
        assert_eq!(lines.next(), None);
        assert_eq!(state.config().zoom(), 2.0, "the rest applied");
        // A notice of the reload that is not about a restart.
        let next = format!("{text}\n[chrome.nothing]\nx = 1\n");
        let answer = write(Some(&path), &next, text, &env(), reload_into(&mut state));
        assert!(answer.starts_with("saved\n"), "{answer}");
        assert!(
            answer
                .lines()
                .any(|l| l.starts_with("notice [chrome.nothing]")),
            "{answer}"
        );
    }

    #[test]
    fn an_existing_files_mode_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "version = 1\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let answer = write(
            Some(&path),
            "version = 1\nzoom = 2\n",
            "version = 1\n",
            &env(),
            |_| Vec::new(),
        );
        assert_eq!(answer, "saved\n");
        assert_eq!(mode(&path), 0o640);
    }
}
