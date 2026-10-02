//! Files: reading and watching the configuration (`{config_dir}/sighurt/config.toml`), and
//! loading and saving the session (`{state_dir}/sighurt/session.toml`).
//!
//! The core parses and validates the configuration; `sig` adds the one check the core can't
//! make, the `[ui.colors]` names, with `sighurt_ui::Colors::set`, before it tells the core a
//! reload succeeded. A file with any problem is not used: at startup `sig` runs with the
//! built-in configuration and says why, and a reload that fails keeps the configuration in use.
//! The configuration's directory is watched with inotify, so a saved file (written in place or
//! renamed over the old one) or a deleted one is read again; nothing polls.
//!
//! Sessions are written atomically (a temporary file in the same directory, synced, then renamed
//! over the old one) by a thread of their own, which writes only the latest snapshot it has been
//! given; at quit the last one is written before `sig` goes on.
//!
//! Must not own: parsing or validating the configuration's contents (the core), or deciding what
//! a reload changes.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use rustix::fs::inotify;
use sighurt_core::config::{Config, ConfigError};
use sighurt_core::session::Session;
use sighurt_ui::Colors;

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
    pub fn from_env() -> Self {
        Self {
            config: dirs::config_dir().map(|d| d.join("sighurt").join("config.toml")),
            session: dirs::state_dir()
                .or_else(dirs::data_dir)
                .map(|d| d.join("sighurt").join("session.toml")),
        }
    }
}

/// A configuration that passed every check, with the chrome's colors from it.
pub struct Loaded {
    pub config: Config,
    pub colors: Colors,
}

/// Why a configuration file can't be used.
pub enum LoadError {
    /// The core's parser found problems.
    Config(ConfigError),
    /// Reading the file, or `[ui.colors]`, failed; for the user.
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

/// Reads and checks the configuration at `path` (none: the built-in one).
pub fn load_config(path: Option<&Path>) -> Result<Loaded, LoadError> {
    let text = match path.map(fs::read_to_string) {
        None => None,
        Some(Ok(text)) => Some(text),
        Some(Err(error)) if error.kind() == io::ErrorKind::NotFound => None,
        Some(Err(error)) => {
            let path = path.expect("read from a path").display();
            return Err(LoadError::Other(format!("cannot read {path}: {error}")));
        }
    };
    let config = Config::parse(text.as_deref()).map_err(LoadError::Config)?;
    let colors = colors(&config).map_err(LoadError::Other)?;
    Ok(Loaded { config, colors })
}

/// The chrome's colors from `config`'s `[ui.colors]`, or every problem with them. The core has
/// checked that each value is a color; only the chrome knows the names.
pub fn colors(config: &Config) -> Result<Colors, String> {
    let mut colors = Colors::default();
    let Some(table) = config.ui().get("colors").and_then(|v| v.as_table()) else {
        return Ok(colors);
    };
    let problems: Vec<String> = table
        .iter()
        .filter_map(|(name, value)| {
            let value = value.as_str().unwrap_or_default();
            colors
                .set(name, value)
                .err()
                .map(|e| format!("- [ui.colors] {e}"))
        })
        .collect();
    match problems.is_empty() {
        true => Ok(colors),
        false => Err(format!(
            "invalid configuration\n{}\nknown colors: {}",
            problems.join("\n"),
            Colors::NAMES.join(", ")
        )),
    }
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

/// Reads the saved session: `Ok(None)` when there is none, `Err` when it can't be read.
pub fn load_session(path: &Path) -> Result<Option<Session>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Session::parse(&text)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Writes `text` to `path` atomically: readers see the old file or the new one, whole.
pub fn write_atomically(path: &Path, text: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("no directory"))?;
    fs::create_dir_all(dir)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
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

/// Writes session snapshots on a thread of its own, the latest one only.
pub struct SessionWriter {
    sender: Option<Sender<String>>,
    thread: Option<JoinHandle<()>>,
}

impl SessionWriter {
    /// A writer to `path`.
    pub fn new(path: PathBuf) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel::<String>();
        let thread = std::thread::Builder::new()
            .name("sighurt-session".into())
            .spawn(move || write_latest(&path, receiver))?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
        })
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

fn write_latest(path: &Path, receiver: Receiver<String>) {
    while let Ok(mut text) = receiver.recv() {
        while let Ok(newer) = receiver.try_recv() {
            text = newer;
        }
        if let Err(error) = write_atomically(path, &text) {
            log::warn!("cannot save the session to {}: {error}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_built_in_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_config(Some(&dir.path().join("config.toml")))
            .ok()
            .unwrap();
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.colors, Colors::default());
    }

    #[test]
    fn unknown_color_names_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "version = 1\n[ui.colors]\ntext = \"#ff0000\"\nbackground = \"#000000\"\n",
        )
        .unwrap();
        let error = load_config(Some(&path)).err().unwrap().to_string();
        assert!(error.contains("unknown color \"background\""), "{error}");
        fs::write(&path, "version = 1\n[ui.colors]\ntext = \"#ff0000\"\n").unwrap();
        let loaded = load_config(Some(&path)).ok().unwrap();
        assert_eq!(
            loaded.colors.text,
            sighurt_ui::Rgba::parse("#ff0000").unwrap()
        );
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
        let writer = SessionWriter::new(path.clone()).unwrap();
        let text = |n: usize| {
            Session {
                version: sighurt_core::session::VERSION,
                active: 0,
                tabs: Vec::new(),
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
        // Only the session is left in the directory: no temporary file.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        fs::write(&path, "version = 7\n").unwrap();
        assert!(load_session(&path).is_err());
        assert_eq!(load_session(&dir.path().join("none")), Ok(None));
    }
}
