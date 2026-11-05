//! Files: reading and watching the configuration (`{config_dir}/sighurt/config.toml`), and
//! loading and saving the session (`{state_dir}/sighurt/session.toml`). The history of visited
//! pages beside it (`history.toml`) is [`crate::history`]'s, written with this module's
//! atomic writes under the session's lock.
//!
//! Sighurt's own directories (`{data_dir}/sighurt`, the engines' profiles under it, and
//! `{state_dir}/sighurt`) are mode 0700. An existing directory that is wider is tightened to
//! 0700; a profile is created only by an engine that keeps one; shared XDG parents are not
//! changed. Files this module writes, including
//! `session.toml` and `history.toml`, are mode 0600.
//!
//! The core parses and validates the configuration, against what the build has (its engines,
//! keymaps and chromes, and the chrome's color names: `sighurt_core::Environment`). A file with
//! any problem is not used. At startup that stops `sig` ([`startup_config`]): running with the
//! built-in configuration instead would drop what the file sets, such as an engine's ephemeral
//! storage or `[session] restore = false`. Only a missing file means the built-in
//! configuration. A reload that fails, or finds the file missing, keeps the configuration in
//! use ([`load_config`]).
//! The configuration's directory is watched with inotify, so a saved file (written in place or
//! renamed over the old one) or a deleted one is read again; nothing polls.
//!
//! Sessions are written atomically (a temporary file in the same directory, synced, then renamed
//! over the old one) by a thread of their own, which writes only the latest snapshot it has been
//! given; at quit the last one is written before `sig` goes on. A write that fails (a full disk,
//! the file size limit) leaves the file as it was; the first failure after a write that worked
//! is handed to the owner loop ([`SessionWriter::take_failure`], with a wake) to tell the user,
//! and a session that can't be written at all says so when `sig` starts ([`session_writer`]).
//! Only one `sig` writes a session
//! file: the writer holds an advisory lock (`session.lock` beside it) until its last write, and
//! the OS releases it if `sig` dies. The history's writer shares that lock
//! ([`SessionWriter::lock`]): one open lock file, held until both writers have finished, so
//! the `sig` that owns the session owns the history too. Another `sig` still restores the session, says once that it
//! won't save it, and never writes it. The owner removes temporary files a crash left behind. A
//! session file that can't be read is kept aside as `session.toml.broken-<seconds>` before
//! anything is saved over it.
//!
//! Must not own: parsing or validating the configuration's contents (the core), or deciding what
//! a reload changes.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::inotify;
use sighurt_core::config::{Config, ConfigError};
use sighurt_core::session::Session;
use sighurt_core::Environment;

/// Where `sig`'s files are.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The configuration file, if there is a configuration directory.
    pub config: Option<PathBuf>,
    /// The session file, if there is a state or data directory.
    pub session: Option<PathBuf>,
}

impl Paths {
    /// The XDG locations (`$XDG_CONFIG_HOME`, `$XDG_STATE_HOME` and their defaults).
    /// Makes Sighurt's data and state directories private before anything reads them.
    pub fn from_env() -> Self {
        let paths = Self {
            config: dirs::config_dir().map(|d| d.join("sighurt").join("config.toml")),
            session: dirs::state_dir()
                .or_else(dirs::data_dir)
                .map(|d| d.join("sighurt").join("session.toml")),
        };
        secure_owned_directories();
        paths
    }
}

/// `{data_dir}/sighurt` and `{state_dir}/sighurt`, each mode 0700, and the engine profiles under
/// the first that already exist. A profile is made (0700) by its engine's configuration when the
/// engine keeps one (`registry`'s `secure_profile`), so an ephemeral engine leaves no directory.
/// Shared XDG parents are created if they are missing and are not chmodded.
fn secure_owned_directories() {
    if let Some(data) = dirs::data_dir() {
        let root = data.join("sighurt");
        secure_quietly(&root);
        for engine in ["wpe", "ladybird", "cef", "servo"] {
            let profile = root.join(engine);
            if profile.is_dir() {
                secure_quietly(&profile);
            }
        }
    }
    if let Some(state) = dirs::state_dir() {
        secure_quietly(&state.join("sighurt"));
    }
}

fn secure_quietly(dir: &Path) {
    if let Err(error) = secure_private_dir(dir) {
        log::warn!("cannot make {} private: {error}", dir.display());
    }
}

/// Creates `dir` at mode 0700, or tightens a wider existing directory to 0700. A narrower
/// existing directory is left as it is. Parents are created with the process umask and not
/// chmodded.
pub(crate) fn secure_private_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let created = match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error),
    };
    let meta = fs::metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    let perms = meta.permissions().mode() & 0o7777;
    // 0o7077 is setuid, setgid, sticky and group/other: wider than owner rwx. Narrower stays.
    if (created || (perms & 0o7077) != 0) && perms != 0o700 {
        let mut permissions = meta.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(dir, permissions)?;
    }
    Ok(())
}

/// Why a configuration file can't be used.
pub enum LoadError {
    /// The core's parser found problems.
    Config(ConfigError),
    /// Reading the file failed; for the user.
    Other(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(error) => write!(f, "{error}"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

/// The configuration `sig` starts with, for a build with what `env` lists: the file at `path`,
/// or the built-in one when there is no file (or no configuration directory). A file that
/// can't be used is fatal: the built-in configuration in its place would drop what it sets (an
/// engine's ephemeral storage, `[session] restore = false`). `Err` names the file and every
/// problem, for the frontend to stop with before it starts anything.
pub fn startup_config(path: Option<&Path>, env: &Environment) -> Result<Config, String> {
    let text = match read_config(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "cannot read {}: {error}\n{DID_NOT_START}",
                shown(path)
            ))
        }
    };
    Config::parse(text.as_deref(), env)
        .map_err(|error| format!("{}: {error}\n{DID_NOT_START}", shown(path)))
}

/// The last line of a [`startup_config`] error. The executable exits with status 2 (a usage
/// error, as for bad arguments) when a start fails with this line, and 1 for any other failure.
pub const DID_NOT_START: &str =
    "sig did not start. Fix the file, or move it away to start with the built-in configuration.";

/// Reads and checks the configuration at `path` again, for a reload (none: the built-in one),
/// for a build with what `env` lists. A missing file is an error too, so the configuration in
/// use stays: an editor that moves the old file away before it writes the new one would
/// otherwise reset everything for a moment.
pub fn load_config(path: Option<&Path>, env: &Environment) -> Result<Config, LoadError> {
    let text = read_config(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => LoadError::Other(format!("{} is missing.", shown(path))),
        _ => LoadError::Other(format!("cannot read {}: {error}", shown(path))),
    })?;
    Config::parse(text.as_deref(), env).map_err(LoadError::Config)
}

/// The text of the file at `path`; none without a path.
fn read_config(path: Option<&Path>) -> io::Result<Option<String>> {
    path.map(fs::read_to_string).transpose()
}

/// `path` for the user.
fn shown(path: Option<&Path>) -> String {
    path.map_or_else(
        || "the built-in configuration".into(),
        |path| path.display().to_string(),
    )
}

/// Watches the configuration file's directory for the file being written, replaced or deleted.
pub struct ConfigWatch {
    fd: OwnedFd,
    name: OsString,
}

impl ConfigWatch {
    /// Watches `path`, whose directory must exist.
    pub fn new(path: &Path) -> io::Result<Self> {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::Error::other("the configuration has no directory"));
        };
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)?;
        // Not IN_CREATE: an editor that creates the file writes it afterwards, and its
        // IN_CLOSE_WRITE follows; reading at creation would report an empty or partial file.
        let flags = inotify::WatchFlags::CLOSE_WRITE
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::MOVED_FROM
            | inotify::WatchFlags::DELETE;
        inotify::add_watch(&fd, dir, flags)?;
        Ok(Self {
            fd,
            name: name.to_owned(),
        })
    }

    /// Reads every pending event; whether one was about the configuration file.
    pub fn changed(&self) -> bool {
        let mut buffer = [MaybeUninit::<u8>::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut buffer);
        let mut changed = false;
        while let Ok(event) = reader.next() {
            let name = event.file_name().map(|n| n.to_bytes());
            changed |= name == Some(self.name.as_encoded_bytes());
        }
        changed
    }
}

impl AsFd for ConfigWatch {
    /// The descriptor to wait on.
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// Session files whose lock another `sig` held when [`SessionWriter::new`] asked for it. This
/// process restores them but never writes them.
static OWNED_ELSEWHERE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// What a `sig` that doesn't own the session says about it, once.
const NOT_SAVED: &str = "Another sig owns the saved tabs; this one won't save its tabs.";

/// Reads the saved session: `Ok(None)` when there is none, `Err` when it can't be read. What
/// reading it left out is in its notices. A file that can't be read is kept aside as
/// `<file>.broken-<seconds>`, unless another `sig` owns it, so the next save doesn't replace it.
/// In a `sig` that doesn't own the session, the session says so in its notices.
pub fn load_session(path: &Path) -> Result<Option<Session>, String> {
    let elsewhere = OWNED_ELSEWHERE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|owned| owned == path);
    let read = match fs::read_to_string(path) {
        Ok(text) => Session::parse(&text).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    };
    match read {
        Ok(session) if elsewhere => {
            let mut session = session.unwrap_or_else(|| Session {
                version: sighurt_core::session::VERSION,
                active: 0,
                tabs: Vec::new(),
                notices: Vec::new(),
            });
            session.notices.push(NOT_SAVED.into());
            Ok(Some(session))
        }
        Ok(session) => Ok(session),
        Err(error) if elsewhere => Err(format!("{}: {error}", path.display())),
        Err(error) => Err(match keep_aside(path) {
            Ok(kept) => format!(
                "{}: {error}\nIt is kept as {}.",
                path.display(),
                kept.display()
            ),
            Err(failed) => format!(
                "{}: {error}\nIt can't be kept aside ({failed}), and saving will replace it.",
                path.display()
            ),
        }),
    }
}

/// Renames the session or history file at `path` to `<path>.broken-<seconds since 1970>`,
/// never over another file, and returns the new path.
pub(crate) fn keep_aside(path: &Path) -> io::Result<PathBuf> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    for n in 0..100 {
        let mut kept = path.as_os_str().to_owned();
        kept.push(format!(".broken-{seconds}"));
        if n > 0 {
            kept.push(format!("-{n}"));
        }
        let kept = PathBuf::from(kept);
        // Only the owner of the session renames it, so nothing races this check.
        if fs::symlink_metadata(&kept).is_err() {
            fs::rename(path, &kept)?;
            return Ok(kept);
        }
    }
    Err(io::Error::other("too many kept files"))
}

/// Whether another `sig` held the lock of the session at `path` when this one asked for it
/// ([`SessionWriter::new`]): that `sig` owns the session and the history beside it.
pub(crate) fn owned_elsewhere(path: &Path) -> bool {
    OWNED_ELSEWHERE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|owned| owned == path)
}

/// The history file beside the session at `path`: `history.toml`.
pub fn history_path(session: &Path) -> PathBuf {
    session.with_file_name("history.toml")
}

/// The lock file of the session at `path`: `session.lock` beside `session.toml`.
fn lock_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}.lock"))
}

/// Takes the session at `path` for this process: the lock, held as long as the file returned
/// stays open, and released by the OS when the process ends however it ends. When another
/// `sig` holds it, records that and fails.
fn lock_session(path: &Path) -> io::Result<fs::File> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("no directory"))?;
    secure_private_dir(dir)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path(path))?;
    let mut elsewhere = OWNED_ELSEWHERE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    elsewhere.retain(|owned| owned != path);
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(fs::TryLockError::WouldBlock) => {
            elsewhere.push(path.to_owned());
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "another sig owns the session, so this one won't save it",
            ))
        }
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Removes the temporary files [`write_atomically`] left beside `path` when a `sig` died
/// before renaming one: they hold URLs and titles. Called with the session's lock held, when
/// no other `sig` writes there.
pub(crate) fn remove_temporaries(path: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{}.", name.to_string_lossy());
    for entry in entries.flatten() {
        let file = entry.file_name();
        let pid = file
            .to_str()
            .and_then(|file| file.strip_prefix(&prefix)?.strip_suffix(".tmp"));
        if pid.is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())) {
            if let Err(error) = fs::remove_file(entry.path()) {
                log::warn!("cannot remove {}: {error}", entry.path().display());
            }
        }
    }
}

/// Writes `text` to `path` atomically: readers see the old file or the new one, whole.
pub fn write_atomically(path: &Path, text: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("no directory"))?;
    secure_private_dir(dir)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        let mut permissions = file.metadata()?.permissions();
        if (permissions.mode() & 0o777) != 0o600 {
            permissions.set_mode(0o600);
            file.set_permissions(permissions)?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        // The rename itself is durable once the directory is.
        fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// The writer of the session at `path` (none without a path), and the notice for the user when
/// there is none: the tabs won't be saved. A session another `sig` owns says so when it is
/// restored ([`load_session`]), so that case has no notice here. `wake` is the owner loop's,
/// for [`SessionWriter::take_failure`].
pub fn session_writer(
    path: Option<&Path>,
    wake: impl Fn() + Send + 'static,
) -> (Option<SessionWriter>, Option<String>) {
    let Some(path) = path else {
        return (None, None);
    };
    match SessionWriter::new(path.to_owned(), wake) {
        Ok(writer) => (Some(writer), None),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            log::warn!("cannot save the session: {error}");
            (None, None)
        }
        Err(error) => {
            log::warn!("cannot save the session: {error}");
            let notice = format!("The tabs won't be saved to {}: {error}", path.display());
            (None, Some(notice))
        }
    }
}

/// Writes session snapshots on a thread of its own, the latest one only. It owns the session:
/// its thread holds the session's lock until it has written the last snapshot.
pub struct SessionWriter {
    /// The session's lock, which the thread holds too. The history's writer takes a share
    /// ([`SessionWriter::lock`]).
    lock: Arc<fs::File>,
    sender: Option<Sender<String>>,
    thread: Option<JoinHandle<()>>,
    /// A failed write the owner has not taken yet.
    failure: Arc<Mutex<Option<String>>>,
}

impl SessionWriter {
    /// A writer to `path`, which takes the session's lock and removes the temporary files a
    /// crash left. Fails when another `sig` owns the session: this `sig` then restores it
    /// ([`load_session`] says it won't save it) and saves nothing. `wake` is called when a
    /// write failed after one that worked (or the first write failed).
    pub fn new(path: PathBuf, wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        let lock = Arc::new(lock_session(&path)?);
        remove_temporaries(&path);
        let (sender, receiver) = mpsc::channel::<String>();
        let failure = Arc::new(Mutex::new(None));
        let failed = Arc::clone(&failure);
        let held = Arc::clone(&lock);
        let thread = std::thread::Builder::new()
            .name("sighurt-session".into())
            .spawn(move || {
                write_latest(&path, receiver, |message| {
                    *failed.lock().unwrap_or_else(PoisonError::into_inner) = Some(message);
                    wake();
                });
                drop(held);
            })?;
        Ok(Self {
            lock,
            sender: Some(sender),
            thread: Some(thread),
            failure,
        })
    }

    /// A share of the session's lock, for the history's writer, which holds it until it has
    /// written its last snapshot. The lock is released when every share is dropped.
    pub fn lock(&self) -> Arc<fs::File> {
        Arc::clone(&self.lock)
    }

    /// The failure to report since the last call, for the user: once when writes start
    /// failing, not again until one has worked.
    pub fn take_failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Queues `text` to be written; an older snapshot not written yet is skipped.
    pub fn save(&self, text: String) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(text);
        }
    }

    /// Writes what is queued and stops the thread.
    pub fn finish(mut self) {
        self.sender = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Writes the latest snapshot `receiver` has, until it closes. `failed` hears of the first
/// failure after a write that worked.
fn write_latest(path: &Path, receiver: Receiver<String>, failed: impl Fn(String)) {
    let mut failing = false;
    while let Ok(mut text) = receiver.recv() {
        while let Ok(newer) = receiver.try_recv() {
            text = newer;
        }
        match write_atomically(path, &text) {
            Ok(()) => failing = false,
            Err(error) => {
                log::warn!("cannot save the session to {}: {error}", path.display());
                if !failing {
                    failing = true;
                    failed(format!(
                        "The tabs can't be saved to {}: {error}",
                        path.display()
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_built_in_configuration_at_startup_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let env = Environment::default();
        assert_eq!(startup_config(Some(&path), &env), Ok(Config::default()));
        assert_eq!(startup_config(None, &env), Ok(Config::default()));
        // Reloaded, it keeps the configuration in use: the file may be on its way back.
        let reloaded = load_config(Some(&path), &env);
        assert!(matches!(reloaded, Err(LoadError::Other(m)) if m.ends_with("is missing.")));
    }

    #[test]
    fn a_file_that_cant_be_used_stops_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let env = Environment::default();
        fs::write(&path, "version = 1\nbogus = 1\n").unwrap();
        let error = startup_config(Some(&path), &env).unwrap_err();
        assert!(
            error.starts_with(&format!("{}: ", path.display())),
            "{error}"
        );
        assert!(
            error.contains("bogus") && error.contains("sig did not start"),
            "{error}"
        );
        assert!(matches!(
            load_config(Some(&path), &env),
            Err(LoadError::Config(_))
        ));
        fs::write(&path, "version = 1\n").unwrap();
        assert_eq!(startup_config(Some(&path), &env), Ok(Config::default()));
    }

    #[test]
    fn the_watch_sees_the_file_written_or_replaced_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let watch = ConfigWatch::new(&path).unwrap();
        assert!(!watch.changed());
        fs::write(dir.path().join("other.toml"), "x").unwrap();
        assert!(!watch.changed());
        fs::write(&path, "version = 1\n").unwrap();
        assert!(watch.changed());
        write_atomically(&path, "version = 1\n").unwrap();
        assert!(watch.changed());
        fs::remove_file(&path).unwrap();
        assert!(watch.changed());
    }

    #[test]
    fn sessions_are_written_whole_and_the_latest_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("session.toml");
        let writer = SessionWriter::new(path.clone(), || {}).unwrap();
        let text = |n: usize| {
            Session {
                version: sighurt_core::session::VERSION,
                active: 0,
                tabs: Vec::new(),
                notices: Vec::new(),
            }
            .to_text()
                + &"#\n".repeat(n)
        };
        for n in 0..50 {
            writer.save(text(n));
        }
        writer.finish();
        assert_eq!(fs::read_to_string(&path).unwrap(), text(49));
        assert_eq!(load_session(&path).unwrap().unwrap().tabs.len(), 0);
        // Only the session and its lock are left in the directory: no temporary file.
        assert_eq!(
            names(path.parent().unwrap()),
            ["session.lock", "session.toml"]
        );
        assert_eq!(load_session(&dir.path().join("none")), Ok(None));
    }

    #[test]
    fn a_session_that_cant_be_read_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        for text in ["version = 7\n", "not toml ["] {
            fs::write(&path, text).unwrap();
            let error = load_session(&path).unwrap_err();
            assert!(error.contains("It is kept as"), "{error}");
            assert!(!path.exists());
        }
        let kept = names(dir.path());
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(kept
            .iter()
            .all(|name| name.starts_with("session.toml.broken-")));
        let contents: Vec<_> = kept
            .iter()
            .map(|name| fs::read_to_string(dir.path().join(name)).unwrap())
            .collect();
        assert!(
            contents.contains(&"version = 7\n".to_owned()),
            "{contents:?}"
        );
        // A damaged tab doesn't make the file unreadable.
        fs::write(&path, "version = 1\nactive = 0\n[[tabs]]\nzoom = \"x\"\n").unwrap();
        let session = load_session(&path).unwrap().unwrap();
        assert_eq!((session.tabs.len(), session.notices.len()), (0, 1));
        assert!(path.exists());
    }

    #[test]
    fn one_sig_owns_the_session_and_the_other_only_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        // What a sig that crashed while writing left.
        fs::write(dir.path().join("session.toml.4242.tmp"), "tabs").unwrap();
        fs::write(dir.path().join("session.toml.x.tmp"), "not ours").unwrap();
        let owner = SessionWriter::new(path.clone(), || {}).unwrap();
        assert_eq!(
            names(dir.path()),
            ["session.lock", "session.toml.x.tmp"],
            "the temporary file is gone"
        );
        owner.save(empty_session());
        // The lock is per open file, so a second writer in this process stands for another sig.
        let error = SessionWriter::new(path.clone(), || {}).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        owner.finish();
        let session = load_session(&path).unwrap().unwrap();
        assert_eq!(session.notices, [NOT_SAVED]);
        // Broken or not, the file is the owner's: it stays.
        fs::write(&path, "not toml [").unwrap();
        assert!(load_session(&path).is_err());
        assert!(path.exists());
        // The owner let go when it finished: now this one owns the session.
        SessionWriter::new(path.clone(), || {}).unwrap().finish();
        let error = load_session(&path).unwrap_err();
        assert!(error.contains("It is kept as"), "{error}");
    }

    #[test]
    fn a_failed_write_is_reported_once_until_one_works_and_the_file_stays() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        let wakes = Arc::new(AtomicUsize::new(0));
        let woken = Arc::clone(&wakes);
        let writer = SessionWriter::new(path.clone(), move || {
            woken.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        let until = |done: &dyn Fn() -> bool| {
            for _ in 0..500 {
                if done() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("timed out");
        };
        // A directory where the file goes: renaming over it fails.
        fs::create_dir_all(path.join("x")).unwrap();
        writer.save(empty_session());
        until(&|| wakes.load(Ordering::SeqCst) == 1);
        let failure = writer.take_failure().unwrap();
        assert!(
            failure.starts_with(&format!("The tabs can't be saved to {}: ", path.display())),
            "{failure}"
        );
        assert_eq!(writer.take_failure(), None, "taken");
        writer.save(empty_session());
        fs::remove_dir_all(&path).unwrap();
        writer.save(empty_session());
        until(&|| path.is_file());
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "a failure goes on unreported"
        );
        // A save still queued may write the file once more before the directory is there.
        until(&|| {
            let _ = fs::remove_file(&path);
            fs::create_dir_all(path.join("x")).is_ok()
        });
        writer.save(empty_session());
        writer.finish();
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            2,
            "failing again after a write that worked is reported"
        );
        assert!(path.join("x").is_dir(), "what was there stays");
        assert_eq!(
            names(dir.path()),
            ["session.lock", "session.toml"],
            "no temporary file"
        );
    }

    fn empty_session() -> String {
        Session {
            version: sighurt_core::session::VERSION,
            active: 0,
            tabs: Vec::new(),
            notices: Vec::new(),
        }
        .to_text()
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
    fn private_directories_are_0700_and_session_files_are_0600() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("share");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        let dir = parent.join("sighurt");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        secure_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700, "a wider directory is tightened");
        assert_eq!(mode(&parent), 0o755, "a shared parent is not chmodded");
        secure_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);

        let fresh = parent.join("fresh");
        secure_private_dir(&fresh).unwrap();
        assert_eq!(mode(&fresh), 0o700, "a new directory is created private");
        assert_eq!(mode(&parent), 0o755);

        let path = dir.join("session.toml");
        write_atomically(&path, "version = 1\n").unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&parent), 0o755);
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
}
