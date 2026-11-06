//! The history of visited pages: what the address line completes from, in memory and in
//! `{state_dir}/sighurt/history.toml` beside the session.
//!
//! The core decides what a visit is (`Effect::Visited`: an http or https page the user opened
//! in the tab shown, see `sighurt_core::state`); this module keeps each address once, with its
//! title, when it was last visited (Unix seconds, from the clock here) and how often. It holds
//! at most [`MAX_VISITS`], and few enough bytes that the file stays under [`MAX_FILE`]; the
//! least recently visited go first. Visits older than [`RETENTION`] are dropped when the file is
//! read. [`History::complete`] answers the address line: at most [`MAX_COMPLETIONS`] addresses
//! the typed text is the start of ([`suffix`]), the most recently visited first.
//!
//! The file is TOML, version [`VERSION`], read like the session: a damaged visit is left out
//! with one notice, unknown fields are noticed, another version or a file that is not TOML is
//! refused whole, and so is a file over [`MAX_FILE`], before it is parsed. It is read and parsed
//! on a thread of its own, so a full history does not delay the first frame (H-START2): the owner
//! takes what it read when it is in ([`History::poll`], each loop pass, after the reader woke the
//! loop) or when something needs the visits (a visit, a forget, a save, `remember`), waiting
//! for it then; until then completion answers nothing, and when it comes the generation changes
//! so that the frontends ask again. The `sig` that owns
//! the session (its lock, `persistence`) owns the history: it keeps a file it can't read aside
//! as `history.toml.broken-<seconds>`, and writes the history on a thread of its own, the latest
//! snapshot only (one slot, never a queue), atomically, synced, mode 0600, at most every
//! [`SAVE_DELAY`] and only when something changed, and at quit. Another `sig` reads the file to
//! complete from, records its own visits in memory, says once that it won't save them, and
//! never writes the file.
//!
//! Forgetting (`Effect::ForgetVisits`: `history.clear`, `site.forget`) is acknowledged: the
//! visits go from memory, the save waiting is dropped, and the file is written at once, also
//! with `[history] remember` off (the file's visits are read for it); the outcome says whether
//! the file was written, failed, or is another `sig`'s. Kept-aside `.broken-*` copies are not
//! touched. With `remember` off, nothing is recorded, what was loaded and any save waiting are
//! dropped, and the file stays as it is; turning it on reads the file again. Without a session
//! path (`sig --dump`, no state directory) there is no history at all.
//!
//! Must not own: what a visit is (the core's), or how a chrome shows a completion.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sighurt_api::ids::RequestId;
use sighurt_core::ForgetOutcome;
use url::Url;

use crate::persistence::{keep_aside, remove_temporaries, write_atomically};

/// The format version of the history file.
pub const VERSION: u32 = 1;
/// The most visits kept.
pub const MAX_VISITS: usize = 5_000;
/// How long a visit is kept after the last one to its address: 90 days.
pub const RETENTION: Duration = Duration::from_secs(90 * 24 * 60 * 60);
/// The largest history file read; a larger one is refused before it is parsed.
pub const MAX_FILE: u64 = 4 * 1024 * 1024;
/// The most characters kept of a title.
pub const MAX_TITLE: usize = 200;
/// The most completions [`History::complete`] gives.
pub const MAX_COMPLETIONS: usize = 8;
/// How long a changed history waits before it is written.
pub const SAVE_DELAY: Duration = Duration::from_secs(30);
/// The longest address kept, as the core records them (`sighurt_core::state::MAX_VISIT_URL`).
const MAX_URL: usize = sighurt_core::state::MAX_VISIT_URL;
/// The bytes the visits may take by [`cost`], so that the file they make stays under
/// [`MAX_FILE`].
const MAX_BYTES: usize = MAX_FILE as usize - 64 * 1024;
/// The most unknown field names a notice lists.
const MAX_UNKNOWN_NAMED: usize = 8;
/// The fields of a saved visit.
const VISIT_FIELDS: [&str; 4] = ["url", "title", "last", "count"];

/// An address the typed text completes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The exact address, to open as it is: an http or https URL, printable ASCII, at most
    /// 8 KiB.
    pub url: String,
    /// Its title as last seen, contained (no control or bidi characters), at most
    /// [`MAX_TITLE`] characters; empty when it had none.
    pub title: String,
}

/// What the typed text `typed` completes `url` with: the rest of `url` after it, when `typed`
/// starts `url`, or `url` without its `https://` or `http://`, or without that and `www.`,
/// ignoring ASCII case. `None` when it starts none of them. An explicitly typed scheme must
/// match `url`'s.
pub fn suffix<'a>(typed: &str, url: &'a str) -> Option<&'a str> {
    let schemeless = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    let hostname = schemeless.and_then(|rest| rest.strip_prefix("www."));
    [Some(url), schemeless, hostname]
        .into_iter()
        .flatten()
        .find_map(|form| {
            let head = form.get(..typed.len())?;
            head.eq_ignore_ascii_case(typed)
                .then(|| &form[typed.len()..])
        })
}

/// One visited address.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Visit {
    title: String,
    /// When it was visited last, in seconds since 1970.
    last: u64,
    count: u32,
}

/// What a visit counts against [`MAX_BYTES`]: at least what it takes in the file, where TOML
/// escapes at most double a string.
fn cost(url: &str, visit: &Visit) -> usize {
    2 * (url.len() + visit.title.len()) + 96
}

/// Whose the history file is.
enum Owner {
    /// This `sig`'s: it writes it.
    Me(Writer),
    /// Another `sig`'s, which owns the session.
    Elsewhere,
    /// Nobody's: this `sig` could not take the session's lock.
    Nobody,
}

/// How a [`History`] may use its file.
pub enum Ownership {
    /// This `sig` holds the session's lock: a share of it, held until the history is written
    /// for the last time.
    Owner(Arc<fs::File>),
    /// Another `sig` holds it.
    Elsewhere,
    /// Nobody could take it here (a read-only state directory).
    Nobody,
}

/// The visited pages, and their file.
pub struct History {
    /// The history file; `None`: no history at all.
    path: Option<PathBuf>,
    /// `[history] remember`.
    remember: bool,
    /// Whether the file has been read into `visits`, or is being read (it is while `remember`
    /// is on).
    loaded: bool,
    /// The file being read, until [`History::poll`] or [`History::settle`] takes it.
    reading: Option<Receiver<ReadResult>>,
    /// The owner loop's wake, which the reader calls when it is done.
    wake: Arc<dyn Fn() + Send + Sync>,
    visits: HashMap<String, Visit>,
    /// Their [`cost`], all together.
    bytes: usize,
    owner: Owner,
    /// Something changed since the last snapshot the writer got.
    changed: bool,
    /// When the change should be written.
    due: Option<Instant>,
    /// Changes when visits are forgotten or dropped, so that completions shown can be asked
    /// again.
    generation: u64,
    /// Visits recorded or retitled: with `generation`, what [`History::revision`] counts.
    edits: u64,
    /// Forgets the writer has not reported, with how many visits each removed.
    forgetting: BTreeMap<RequestId, usize>,
    /// For the user, not told yet.
    notices: Vec<String>,
    /// The user was told this `sig` won't save the history, or needn't be.
    told: bool,
}

impl History {
    /// The history in `path` (none: no history), owned or not as `ownership` says, with
    /// `[history] remember` as `remember`: on, the file is read now. `wake` is the owner loop's,
    /// for the writer's reports ([`History::take_reports`]).
    pub fn open(
        path: Option<PathBuf>,
        ownership: Ownership,
        remember: bool,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let mut notices = Vec::new();
        let mut told = false;
        let owner = match (&path, ownership) {
            (Some(path), Ownership::Owner(lock)) => {
                remove_temporaries(path);
                match Writer::new(path.clone(), lock, Arc::clone(&wake)) {
                    Ok(writer) => Owner::Me(writer),
                    Err(error) => {
                        told = true;
                        notices.push(format!(
                            "The pages you visit won't be saved to {}: {error}",
                            path.display()
                        ));
                        Owner::Nobody
                    }
                }
            }
            (Some(_), Ownership::Elsewhere) => Owner::Elsewhere,
            _ => Owner::Nobody,
        };
        let mut history = Self {
            path,
            remember: false,
            loaded: false,
            reading: None,
            wake,
            visits: HashMap::new(),
            bytes: 0,
            owner,
            changed: false,
            due: None,
            generation: 0,
            edits: 0,
            forgetting: BTreeMap::new(),
            notices,
            told,
        };
        history.set_remember(remember);
        history
    }

    /// Whether `[history] remember` is on here.
    pub fn remembers(&self) -> bool {
        self.remember
    }

    /// `[history] remember` changed (a reload): on reads the file again; off drops every visit
    /// in memory and a save waiting, leaving the file as it is.
    pub fn set_remember(&mut self, on: bool) {
        if on == self.remember {
            return;
        }
        self.settle();
        self.remember = on;
        if on {
            self.load();
            if let Some(path) = self.path.as_ref().filter(|_| !self.told) {
                self.told = true;
                match self.owner {
                    Owner::Me(_) => {}
                    Owner::Elsewhere => self.notices.push(
                        "Another sig owns the history of visited pages; this one won't save \
                         the pages it visits."
                            .into(),
                    ),
                    Owner::Nobody => self.notices.push(format!(
                        "The pages you visit won't be saved to {}: sig can't lock its state \
                         files there.",
                        path.display()
                    )),
                }
            }
        } else {
            self.unload();
            if let Owner::Me(writer) = &self.owner {
                writer.discard();
            }
        }
    }

    /// Records a visit to `url` now (`commit`), or gives the visit of `url` the title `title`
    /// (not `commit`; nothing when there is none). Returns whether a visit was recorded. Does
    /// nothing with `remember` off or no history.
    pub fn visit(&mut self, url: &str, title: Option<&str>, commit: bool) -> bool {
        self.record(url, title, commit, unix_now())
    }

    /// [`History::visit`] at `now`, in seconds since 1970.
    fn record(&mut self, url: &str, title: Option<&str>, commit: bool, now: u64) -> bool {
        self.settle();
        if !self.remember || !self.loaded || url.len() > MAX_URL {
            return false;
        }
        if commit {
            let new = !self.visits.contains_key(url);
            let visit = self.visits.entry(url.to_owned()).or_insert_with(|| Visit {
                title: String::new(),
                last: now,
                count: 0,
            });
            visit.last = now;
            visit.count = visit.count.saturating_add(1);
            if new {
                self.bytes += cost(url, visit);
                self.evict();
            }
        } else {
            let Some(visit) = self.visits.get_mut(url) else {
                return false;
            };
            let title = contained(title.unwrap_or_default());
            if visit.title == title {
                return false;
            }
            let before = cost(url, visit);
            visit.title = title;
            self.bytes = self.bytes - before + cost(url, visit);
            self.evict();
        }
        self.edits += 1;
        self.changed();
        commit
    }

    /// At most [`MAX_COMPLETIONS`] visited addresses that `typed` completes ([`suffix`] gives
    /// more than nothing), the most recently visited first. None for empty text, with
    /// `remember` off, without a history, or while the file is still being read.
    pub fn complete(&self, typed: &str) -> Vec<Completion> {
        if typed.is_empty() || !self.remember || self.reading.is_some() {
            return Vec::new();
        }
        let mut found: Vec<(&String, &Visit)> = self
            .visits
            .iter()
            .filter(|(url, _)| suffix(typed, url).is_some_and(|rest| !rest.is_empty()))
            .collect();
        found.sort_by(|(a, x), (b, y)| {
            (y.last, y.count)
                .cmp(&(x.last, x.count))
                .then_with(|| a.cmp(b))
        });
        found
            .into_iter()
            .take(MAX_COMPLETIONS)
            .map(|(url, visit)| Completion {
                url: url.clone(),
                title: visit.title.clone(),
            })
            .collect()
    }

    /// At most `n` visited addresses, the most recently visited first, each once. None with
    /// `remember` off, without a history, or while the file is still being read.
    pub fn recent(&self, n: usize) -> Vec<Completion> {
        if !self.remember || self.reading.is_some() || n == 0 {
            return Vec::new();
        }
        // The `n` latest, kept sorted, without sorting every visit.
        let mut found: Vec<(&String, &Visit)> = Vec::with_capacity(n + 1);
        let later = |(a, x): &(&String, &Visit), (b, y): &(&String, &Visit)| {
            (y.last, y.count)
                .cmp(&(x.last, x.count))
                .then_with(|| a.cmp(b))
        };
        for entry in &self.visits {
            let at = found.partition_point(|kept| later(kept, &entry).is_le());
            if at < n {
                found.insert(at, entry);
                found.truncate(n);
            }
        }
        found
            .into_iter()
            .map(|(url, visit)| Completion {
                url: url.clone(),
                title: visit.title.clone(),
            })
            .collect()
    }

    /// Forgets the visits to `host` and the hosts under it, or all of them, in memory and in
    /// the file at once, dropping the save waiting. The outcome now, or `None` when the writer
    /// reports it later ([`History::take_reports`]).
    pub fn forget(&mut self, request: RequestId, host: Option<&str>) -> Option<ForgetOutcome> {
        // With `remember` off, the file still holds what was saved: read it to forget from it.
        let unload = !self.loaded && matches!(self.owner, Owner::Me(_));
        if unload {
            self.load();
        }
        self.settle();
        let before = self.visits.len();
        match host {
            None => self.visits.clear(),
            Some(host) => self.visits.retain(|url, _| !under(url, host)),
        }
        let removed = before - self.visits.len();
        self.bytes = self.visits.iter().map(|(url, v)| cost(url, v)).sum();
        self.generation += 1;
        let outcome = match &self.owner {
            Owner::Me(writer) => {
                writer.save(self.to_text(), vec![request]);
                self.forgetting.insert(request, removed);
                self.changed = false;
                self.due = None;
                None
            }
            Owner::Elsewhere => Some(ForgetOutcome::HereOnly { removed }),
            Owner::Nobody if self.path.is_some() => Some(ForgetOutcome::NotWritten {
                removed,
                error: "sig can't lock its state files".into(),
            }),
            Owner::Nobody => Some(ForgetOutcome::Done { removed }),
        };
        if unload {
            self.unload();
        }
        outcome
    }

    /// When the changed history should be written. `None` when nothing waits.
    pub fn due(&self) -> Option<Instant> {
        self.due
    }

    /// Hands the history to the writer now if it changed. Returns whether it did.
    pub fn save(&mut self) -> bool {
        self.settle();
        self.due = None;
        let Owner::Me(writer) = &self.owner else {
            return false;
        };
        if !std::mem::take(&mut self.changed) || !self.remember {
            return false;
        }
        writer.save(self.to_text(), Vec::new());
        true
    }

    /// The forgets the writer finished since the last call, as the core's answers, and a write
    /// that failed, for the user (once when writes start failing).
    pub fn take_reports(&mut self) -> (Vec<(RequestId, ForgetOutcome)>, Option<String>) {
        let Owner::Me(writer) = &self.owner else {
            return (Vec::new(), None);
        };
        let reports = writer.take();
        let forgotten = reports
            .forgotten
            .into_iter()
            .map(|(request, result)| {
                let removed = self.forgetting.remove(&request).unwrap_or(0);
                let outcome = match result {
                    Ok(()) => ForgetOutcome::Done { removed },
                    Err(error) => ForgetOutcome::NotWritten { removed, error },
                };
                (request, outcome)
            })
            .collect();
        (forgotten, reports.failure)
    }

    /// Takes what the reader read, if it is done (it woke the owner loop after handing it
    /// over), without waiting. Returns whether it took anything: the visits are in, and the
    /// generation changed.
    pub fn poll(&mut self) -> bool {
        let Some(reading) = &self.reading else {
            return false;
        };
        let read = match reading.try_recv() {
            Ok(read) => read,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("reading it failed".into()),
        };
        self.reading = None;
        self.take_read(read);
        true
    }

    /// Waits for the file being read, if it is, and takes what it holds.
    pub fn settle(&mut self) {
        let Some(reading) = self.reading.take() else {
            return;
        };
        let read = reading
            .recv()
            .unwrap_or_else(|_| Err("reading it failed".into()));
        self.take_read(read);
    }

    /// Changes when visits were forgotten or dropped, or the file was read: completions shown
    /// may be gone, or new ones there.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Changes whenever the visits do: a visit recorded or retitled, as well as everything
    /// that changes [`History::generation`]. What [`History::recent`] lists may differ.
    pub fn revision(&self) -> u64 {
        // Both only grow, so their sum does.
        self.generation.wrapping_add(self.edits)
    }

    /// What to tell the user, once.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Writes what changed and stops the writer, which lets go of its share of the lock.
    pub fn finish(mut self) {
        self.save();
        if let Owner::Me(writer) = std::mem::replace(&mut self.owner, Owner::Nobody) {
            writer.finish();
        }
    }

    fn changed(&mut self) {
        if matches!(self.owner, Owner::Me(_)) {
            self.changed = true;
            self.due.get_or_insert_with(|| Instant::now() + SAVE_DELAY);
        }
    }

    /// Drops every visit in memory and the save waiting. Nothing is being read.
    fn unload(&mut self) {
        self.visits = HashMap::new();
        self.bytes = 0;
        self.loaded = false;
        self.changed = false;
        self.due = None;
        self.generation += 1;
    }

    /// Starts reading the file on a thread of its own ([`History::settle`] takes it), or reads
    /// it here when no thread can be started.
    fn load(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.loaded = true;
        let wake = Arc::clone(&self.wake);
        let (sender, reading) = mpsc::channel();
        // The thread ends when it has handed over what it read; it is not joined.
        let reader = std::thread::Builder::new()
            .name("sighurt-history-read".into())
            .spawn(move || {
                let _ = sender.send(read_file(&path));
                wake();
            });
        match reader {
            Ok(_) => self.reading = Some(reading),
            Err(_) => {
                let read = self.path.as_deref().map_or(Ok(None), read_file);
                self.take_read(read);
            }
        }
    }

    /// Puts what was read in memory, keeping a file that can't be read aside if this `sig`
    /// owns it.
    fn take_read(&mut self, read: ReadResult) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.generation += 1;
        let read = match read {
            Ok(None) => return,
            Ok(Some(parsed)) => Ok(parsed),
            Err(error) => Err(error),
        };
        match read {
            Ok((mut visits, notices)) => {
                self.notices.extend(notices);
                // The most recently visited, as many as the bounds hold.
                visits.sort_by(|(a, x), (b, y)| {
                    (y.last, y.count)
                        .cmp(&(x.last, x.count))
                        .then_with(|| a.cmp(b))
                });
                for (url, visit) in visits {
                    let bytes = self.bytes + cost(&url, &visit);
                    if self.visits.len() == MAX_VISITS || bytes > MAX_BYTES {
                        break;
                    }
                    self.bytes = bytes;
                    self.visits.insert(url, visit);
                }
            }
            Err(error) if !matches!(self.owner, Owner::Me(_)) => {
                self.notices.push(format!(
                    "The visited pages can't be read from {}: {error}",
                    path.display()
                ));
            }
            Err(error) => self.notices.push(match keep_aside(&path) {
                Ok(kept) => format!(
                    "The visited pages can't be read from {}: {error}\nIt is kept as {}.",
                    path.display(),
                    kept.display()
                ),
                Err(failed) => format!(
                    "The visited pages can't be read from {}: {error}\nIt can't be kept aside \
                     ({failed}), and saving will replace it.",
                    path.display()
                ),
            }),
        }
    }

    /// Drops the least recently visited until the bounds hold.
    fn evict(&mut self) {
        while self.visits.len() > MAX_VISITS || self.bytes > MAX_BYTES {
            let oldest = self
                .visits
                .iter()
                .min_by(|(a, x), (b, y)| (x.last, x.count, a).cmp(&(y.last, y.count, b)))
                .map(|(url, _)| url.clone());
            let Some(url) = oldest else {
                break;
            };
            if let Some(visit) = self.visits.remove(&url) {
                self.bytes -= cost(&url, &visit).min(self.bytes);
            }
        }
    }

    /// The history as its file's text, the most recently visited first.
    fn to_text(&self) -> String {
        #[derive(Serialize)]
        struct File<'a> {
            version: u32,
            visits: Vec<Saved<'a>>,
        }
        #[derive(Serialize)]
        struct Saved<'a> {
            url: &'a str,
            title: &'a str,
            last: u64,
            count: u32,
        }
        let mut visits: Vec<Saved<'_>> = self
            .visits
            .iter()
            .map(|(url, visit)| Saved {
                url,
                title: &visit.title,
                last: visit.last,
                count: visit.count,
            })
            .collect();
        visits.sort_by(|a, b| b.last.cmp(&a.last).then_with(|| a.url.cmp(b.url)));
        toml::to_string(&File {
            version: VERSION,
            visits,
        })
        .expect("the history is plain data")
    }
}

/// Whether `url`'s host is `host` or a name under it.
fn under(url: &str, host: &str) -> bool {
    let parsed = Url::parse(url).ok();
    let Some(name) = parsed.as_ref().and_then(Url::host_str) else {
        return false;
    };
    name == host
        || name
            .strip_suffix(host)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// `title` safe to keep and show on one line: control and line-separator characters become
/// spaces, bidi controls are dropped, at most [`MAX_TITLE`] characters are kept.
fn contained(title: &str) -> String {
    title
        .chars()
        .filter(|c| !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .map(
            |c| match c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                true => ' ',
                false => c,
            },
        )
        .take(MAX_TITLE)
        .collect()
}

/// Whether `url` is an address a visit is recorded under (as the core makes them).
fn valid_url(url: &str) -> bool {
    url.len() <= MAX_URL
        && Url::parse(url).is_ok_and(|parsed| {
            matches!(parsed.scheme(), "http" | "https")
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.fragment().is_none()
                && parsed.as_str() == url
        })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The text of the file at `path`, `None` if there is none. Refused, unread, when it is over
/// [`MAX_FILE`].
fn read_bounded(path: &Path) -> Result<Option<String>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let too_big = || format!("it is over the limit of {MAX_FILE} bytes");
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE {
        return Err(too_big());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(too_big());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "it is not UTF-8".into())
}

/// The visits a history file holds, and what reading it left out, for the user.
type Parsed = (Vec<(String, Visit)>, Vec<String>);

/// What reading the history file gave: nothing (no file), its visits, or why it can't be read.
type ReadResult = Result<Option<Parsed>, String>;

/// Reads and parses the history file at `path`.
fn read_file(path: &Path) -> ReadResult {
    match read_bounded(path)? {
        None => Ok(None),
        Some(text) => parse(&text, unix_now()).map(Some),
    }
}

/// The visits in a history file's `text`, without those older than [`RETENTION`] at `now`, and
/// what reading it left out, for the user. `Err` for text that is not a history of this
/// [`VERSION`].
fn parse(text: &str, now: u64) -> Result<Parsed, String> {
    let mut file: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    match file.remove("version").as_ref().map(toml::Value::as_integer) {
        Some(Some(version)) if version == i64::from(VERSION) => {}
        Some(Some(version)) if version > i64::from(VERSION) => {
            return Err(format!(
                "it was written by a newer sig (history version {version})"
            ));
        }
        Some(Some(version)) => return Err(format!("unknown history version {version}")),
        Some(None) | None => return Err("it has no history version".into()),
    }
    let saved = match file.remove("visits") {
        None => Vec::new(),
        Some(toml::Value::Array(visits)) => visits,
        Some(_) => return Err("its list of visits is not a list".into()),
    };
    let mut unknown: BTreeSet<String> = file.keys().take(MAX_UNKNOWN_NAMED + 1).cloned().collect();
    let oldest = now.saturating_sub(RETENTION.as_secs());
    let mut visits: HashMap<String, Visit> = HashMap::new();
    let mut damaged = 0;
    for value in saved {
        let Some((url, visit)) = saved_visit(value, &mut unknown) else {
            damaged += 1;
            continue;
        };
        if visit.last < oldest {
            continue;
        }
        let newer = visits.get(&url).is_none_or(|kept| kept.last < visit.last);
        if newer {
            visits.insert(url, visit);
        }
    }
    let mut notices = Vec::new();
    if damaged > 0 {
        notices.push(match damaged {
            1 => "A visited page in the history was damaged and is left out.".into(),
            n => format!("{n} visited pages in the history were damaged and are left out."),
        });
    }
    if !unknown.is_empty() {
        let named: Vec<String> = unknown
            .iter()
            .take(MAX_UNKNOWN_NAMED)
            .map(|name| format!("{:?}", name.chars().take(40).collect::<String>()))
            .collect();
        let more = match unknown.len() > MAX_UNKNOWN_NAMED {
            true => " and others",
            false => "",
        };
        notices.push(format!(
            "The history has fields this sig doesn't know, ignored: {}{more}.",
            named.join(", ")
        ));
    }
    Ok((visits.into_iter().collect(), notices))
}

/// One saved visit, if it is usable, its unknown fields added to `unknown` (one more than a
/// notice names).
fn saved_visit(value: toml::Value, unknown: &mut BTreeSet<String>) -> Option<(String, Visit)> {
    let toml::Value::Table(mut table) = value else {
        return None;
    };
    for key in table.keys() {
        if !VISIT_FIELDS.contains(&key.as_str()) && unknown.len() <= MAX_UNKNOWN_NAMED {
            unknown.insert(key.clone());
        }
    }
    let url = match table.remove("url") {
        Some(toml::Value::String(url)) if valid_url(&url) => url,
        _ => return None,
    };
    let title = match table.remove("title") {
        None => String::new(),
        Some(toml::Value::String(title)) => contained(&title),
        Some(_) => return None,
    };
    let last = table.remove("last")?.as_integer()?;
    let count = table.remove("count")?.as_integer()?;
    let visit = Visit {
        title,
        last: u64::try_from(last).ok()?,
        count: u32::try_from(count).ok().filter(|&count| count > 0)?,
    };
    Some((url, visit))
}

/// The latest snapshot to write, and the forgets it carries.
struct Job {
    text: String,
    requests: Vec<RequestId>,
}

/// One slot: a newer snapshot replaces one not written yet, taking over its forgets.
struct Mailbox {
    job: Option<Job>,
    open: bool,
}

/// What the writer has to say.
#[derive(Default)]
struct Reports {
    /// Forgets written, or not.
    forgotten: Vec<(RequestId, Result<(), String>)>,
    /// A write failed after one that worked (or the first one).
    failure: Option<String>,
}

struct Shared {
    mailbox: Mutex<Mailbox>,
    ready: Condvar,
    reports: Mutex<Reports>,
}

/// Writes history snapshots on a thread of its own, holding a share of the session's lock
/// until the last one is written.
struct Writer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Writer {
    fn new(
        path: PathBuf,
        lock: Arc<fs::File>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox {
                job: None,
                open: true,
            }),
            ready: Condvar::new(),
            reports: Mutex::new(Reports::default()),
        });
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("sighurt-history".into())
            .spawn(move || {
                write_jobs(&path, &thread_shared, &*wake);
                drop(lock);
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Puts `text` in the slot, with the forgets it answers.
    fn save(&self, text: String, mut requests: Vec<RequestId>) {
        let mut mailbox = lock(&self.shared.mailbox);
        if let Some(old) = mailbox.job.take() {
            requests.extend(old.requests);
        }
        mailbox.job = Some(Job { text, requests });
        self.shared.ready.notify_one();
    }

    /// Drops the snapshot waiting, unless it answers a forget.
    fn discard(&self) {
        let mut mailbox = lock(&self.shared.mailbox);
        if mailbox
            .job
            .as_ref()
            .is_some_and(|job| job.requests.is_empty())
        {
            mailbox.job = None;
        }
    }

    fn take(&self) -> Reports {
        std::mem::take(&mut *lock(&self.shared.reports))
    }

    /// Writes what is in the slot and stops the thread.
    fn finish(mut self) {
        lock(&self.shared.mailbox).open = false;
        self.shared.ready.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Writes each snapshot the slot gets, until it closes and is empty.
fn write_jobs(path: &Path, shared: &Shared, wake: &dyn Fn()) {
    let mut failing = false;
    loop {
        let job = {
            let mut mailbox = lock(&shared.mailbox);
            loop {
                if let Some(job) = mailbox.job.take() {
                    break Some(job);
                }
                if !mailbox.open {
                    break None;
                }
                mailbox = shared
                    .ready
                    .wait(mailbox)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(job) = job else {
            return;
        };
        let result = write_atomically(path, &job.text).map_err(|error| error.to_string());
        let mut tell = !job.requests.is_empty();
        {
            let mut reports = lock(&shared.reports);
            for request in job.requests {
                reports.forgotten.push((request, result.clone()));
            }
            match &result {
                Ok(()) => failing = false,
                Err(error) => {
                    log::warn!("cannot save the history to {}: {error}", path.display());
                    if !failing {
                        failing = true;
                        tell = true;
                        reports.failure = Some(format!(
                            "The pages you visit can't be saved to {}: {error}",
                            path.display()
                        ));
                    }
                }
            }
        }
        if tell {
            wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const DAY: u64 = 24 * 60 * 60;
    const NOW: u64 = 1_800_000_000;

    fn quiet() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    /// A history this test owns at `dir/history.toml`, its file read.
    fn owned(dir: &Path, remember: bool) -> History {
        let lock = fs::File::create(dir.join("session.lock")).unwrap();
        opened(
            Some(dir.join("history.toml")),
            Ownership::Owner(Arc::new(lock)),
            remember,
        )
    }

    /// [`History::open`], waiting for its file to be read.
    fn opened(path: Option<PathBuf>, ownership: Ownership, remember: bool) -> History {
        let mut history = History::open(path, ownership, remember, quiet());
        history.settle();
        history
    }

    fn file(dir: &Path) -> PathBuf {
        dir.join("history.toml")
    }

    /// The forgets the writer reports, waiting for them.
    fn reports(history: &mut History, n: usize) -> Vec<(RequestId, ForgetOutcome)> {
        let mut all = Vec::new();
        for _ in 0..500 {
            all.extend(history.take_reports().0);
            if all.len() >= n {
                return all;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the writer did not report");
    }

    fn urls(completions: &[Completion]) -> Vec<&str> {
        completions.iter().map(|c| c.url.as_str()).collect()
    }

    #[test]
    fn the_suffix_skips_the_scheme_and_www_but_keeps_a_typed_scheme() {
        let url = "https://www.example.com/a";
        assert_eq!(suffix("exa", url), Some("mple.com/a"));
        assert_eq!(suffix("www.ex", url), Some("ample.com/a"));
        assert_eq!(suffix("https://www.e", url), Some("xample.com/a"));
        assert_eq!(suffix("HTTPS://WWW.E", url), Some("xample.com/a"));
        assert_eq!(suffix("Example.COM/", url), Some("a"));
        assert_eq!(suffix("http://www.e", url), None, "another scheme typed");
        assert_eq!(
            suffix("https://exa", url),
            None,
            "www. is not skipped after a scheme"
        );
        assert_eq!(suffix("ample", url), None);
        assert_eq!(suffix("example.com/a", url), Some(""));
        assert_eq!(suffix("example.com/ab", url), None);
        // Addresses are ASCII as recorded: decoded text typed does not start an encoded one.
        assert_eq!(
            suffix("example.com/räk", "https://example.com/r%C3%A4k"),
            None
        );
        assert_eq!(suffix("e", "http://example.com/"), Some("xample.com/"));
    }

    #[test]
    fn the_history_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = owned(dir.path(), true);
        assert!(history.record("https://a.example/", None, true, NOW));
        assert!(history.record("https://a.example/", None, true, NOW + 5));
        assert!(!history.record(
            "https://a.example/",
            Some("A\u{202e} \n page"),
            false,
            NOW + 6
        ));
        assert!(history.record("http://b.example/x?q=1", None, true, NOW + 1));
        assert!(!history.record("https://gone.example/", Some("no visit"), false, NOW));
        assert_eq!(history.visits.len(), 2, "a title alone records nothing");
        assert!(history.due().is_some());
        let text = history.to_text();
        history.finish();
        assert_eq!(fs::read_to_string(file(dir.path())).unwrap(), text);
        assert_eq!(
            fs::metadata(file(dir.path())).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let (visits, notices) = parse(&text, NOW + 10).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        let visits: HashMap<_, _> = visits.into_iter().collect();
        assert_eq!(
            visits["https://a.example/"],
            Visit {
                title: "A   page".into(),
                last: NOW + 5,
                count: 2
            }
        );
        assert_eq!(visits["http://b.example/x?q=1"].count, 1);
        // Read again by the next sig.
        let history = owned(dir.path(), true);
        assert_eq!(history.visits.len(), 2);
        assert_eq!(history.complete("a.ex")[0].title, "A   page");
    }

    #[test]
    fn the_least_recently_visited_go_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = owned(dir.path(), true);
        for i in 0..MAX_VISITS as u64 + 2 {
            history.record(&format!("https://e.example/{i}"), None, true, NOW + i);
        }
        assert_eq!(history.visits.len(), MAX_VISITS);
        assert!(!history.visits.contains_key("https://e.example/0"));
        assert!(!history.visits.contains_key("https://e.example/1"));
        assert!(history.visits.contains_key("https://e.example/2"));
        // Visiting the oldest again keeps it.
        history.record("https://e.example/2", None, true, NOW + 10_000);
        history.record("https://e.example/new", None, true, NOW + 10_001);
        assert!(history.visits.contains_key("https://e.example/2"));
        assert!(!history.visits.contains_key("https://e.example/3"));

        // A file with more keeps the most recent, without evicting one at a time.
        let mut text = String::from("version = 1\n");
        for i in 0..MAX_VISITS as u64 + 10 {
            text += &format!(
                "[[visits]]\nurl = \"https://f.example/{i}\"\nlast = {}\ncount = 1\n",
                NOW + i
            );
        }
        let dir = tempfile::tempdir().unwrap();
        fs::write(file(dir.path()), text).unwrap();
        let history = owned(dir.path(), true);
        assert_eq!(history.visits.len(), MAX_VISITS);
        assert!(!history.visits.contains_key("https://f.example/9"));
        assert!(history.visits.contains_key("https://f.example/10"));
        history.finish();

        // Long addresses fill the file's budget before the count.
        let mut history = owned(dir.path(), false);
        history.set_remember(true);
        history.settle();
        history.visits.clear();
        history.bytes = 0;
        let long = "x".repeat(MAX_URL - 40);
        for i in 0..400u64 {
            history.record(
                &format!("https://l.example/{i}/{long}"),
                None,
                true,
                NOW + i,
            );
        }
        assert!(history.visits.len() < 400);
        assert!(history.bytes <= MAX_BYTES);
        assert!(history.to_text().len() as u64 <= MAX_FILE);
        assert!(history
            .visits
            .contains_key(&format!("https://l.example/399/{long}")));
        assert!(!history
            .visits
            .contains_key(&format!("https://l.example/0/{long}")));
    }

    #[test]
    fn old_visits_damaged_ones_and_unknown_fields_are_left_out_at_load() {
        let text = format!(
            "version = 1\nextra = 1\n\
             [[visits]]\nurl = \"https://new.example/\"\ntitle = \"New\"\nlast = {NOW}\ncount = 2\n\
             [[visits]]\nurl = \"https://old.example/\"\ntitle = \"Old\"\nlast = {}\ncount = 9\n\
             [[visits]]\nurl = \"https://edge.example/\"\nlast = {}\ncount = 1\nshade = true\n\
             [[visits]]\nurl = \"file:///etc/passwd\"\nlast = {NOW}\ncount = 1\n\
             [[visits]]\nurl = \"https://u:p@x.example/\"\nlast = {NOW}\ncount = 1\n\
             [[visits]]\nurl = \"https://x.example/#f\"\nlast = {NOW}\ncount = 1\n\
             [[visits]]\nurl = \"https://x.example/\"\nlast = -1\ncount = 1\n\
             [[visits]]\nurl = \"https://x.example/\"\nlast = {NOW}\ncount = 0\n\
             [[visits]]\nurl = \"https://x.example/\"\ntitle = 3\nlast = {NOW}\ncount = 1\n",
            NOW - 91 * DAY,
            NOW - 89 * DAY,
        );
        let (visits, notices) = parse(&text, NOW).unwrap();
        let mut kept: Vec<_> = visits.into_iter().map(|(url, _)| url).collect();
        kept.sort();
        assert_eq!(kept, ["https://edge.example/", "https://new.example/"]);
        assert_eq!(
            notices,
            [
                "6 visited pages in the history were damaged and are left out.",
                "The history has fields this sig doesn't know, ignored: \"extra\", \"shade\"."
            ]
        );
        for (text, error) in [
            (
                "version = 2\n",
                "it was written by a newer sig (history version 2)",
            ),
            ("version = 0\n", "unknown history version 0"),
            ("visits = []\n", "it has no history version"),
            (
                "version = 1\nvisits = 3\n",
                "its list of visits is not a list",
            ),
        ] {
            assert_eq!(parse(text, NOW).unwrap_err(), error);
        }
        assert!(parse("not toml [", NOW).is_err());
    }

    #[test]
    fn a_file_over_the_limit_is_refused_unread_and_kept_aside_by_its_owner() {
        let dir = tempfile::tempdir().unwrap();
        let big = format!("version = 1\n#{}\n", "x".repeat(MAX_FILE as usize));
        fs::write(file(dir.path()), &big).unwrap();
        // Another sig's: it stays where it is.
        let mut other = opened(Some(file(dir.path())), Ownership::Elsewhere, true);
        let notices = other.take_notices();
        assert!(
            notices
                .iter()
                .any(|n| n.contains("over the limit of 4194304 bytes")),
            "{notices:?}"
        );
        assert!(file(dir.path()).exists());
        assert!(other.visits.is_empty());
        // The owner keeps it aside and starts afresh.
        let mut history = owned(dir.path(), true);
        let notices = history.take_notices();
        assert!(notices[0].contains("It is kept as"), "{notices:?}");
        assert!(!file(dir.path()).exists());
        history.finish();
        let kept = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .find(|name| name.starts_with("history.toml.broken-"))
            .unwrap();
        assert_eq!(fs::read_to_string(dir.path().join(kept)).unwrap(), big);
    }

    /// The file is read on a thread (H-START2): until it is in, completion answers nothing;
    /// the reader wakes the loop, [`History::poll`] takes it and changes the generation, and a
    /// visit that comes first waits for it, so nothing read is lost.
    #[test]
    fn the_file_is_read_on_a_thread_and_taken_when_it_is_in() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = owned(dir.path(), true);
        history.record("https://a.example/", None, true, unix_now());
        history.finish();
        let (sender, woken) = std::sync::mpsc::channel();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = sender.send(());
        });
        let mut history = History::open(Some(file(dir.path())), Ownership::Elsewhere, true, wake);
        let generation = history.generation();
        assert!(history.complete("a.").is_empty(), "nothing before it is in");
        woken
            .recv_timeout(Duration::from_secs(5))
            .expect("the reader woke the loop");
        assert!(history.poll());
        assert!(!history.poll(), "taken once");
        assert_ne!(history.generation(), generation);
        assert_eq!(urls(&history.complete("a.")), ["https://a.example/"]);
        // A visit before the file is in waits for it.
        let mut history =
            History::open(Some(file(dir.path())), Ownership::Elsewhere, true, quiet());
        assert!(history.visit("https://b.example/", None, true));
        assert_eq!(
            urls(&history.complete("")),
            Vec::<&str>::new(),
            "empty text completes nothing"
        );
        assert_eq!(
            history.complete("a.").len() + history.complete("b.").len(),
            2
        );
    }

    #[test]
    fn completions_are_the_newest_first_at_most_eight() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = owned(dir.path(), true);
        for i in 0..12u64 {
            history.record(&format!("https://www.example.com/{i}"), None, true, NOW + i);
        }
        history.record("http://example.org/", None, true, NOW + 100);
        history.record("https://other.test/", None, true, NOW + 200);
        let found = history.complete("exa");
        assert_eq!(found.len(), MAX_COMPLETIONS);
        assert_eq!(found[0].url, "http://example.org/");
        assert_eq!(found[1].url, "https://www.example.com/11");
        assert_eq!(found[7].url, "https://www.example.com/5");
        assert_eq!(
            urls(&history.complete("example.org")),
            ["http://example.org/"]
        );
        assert_eq!(
            urls(&history.complete("www.example.com/1")),
            ["https://www.example.com/11", "https://www.example.com/10"]
        );
        assert!(history.complete("http://www.ex").is_empty());
        // Nothing for the whole address, an empty line, or text that starts none.
        assert!(history.complete("https://other.test/").is_empty());
        assert!(history.complete("").is_empty());
        assert!(history.complete("ther").is_empty());
    }

    #[test]
    fn recent_visits_are_the_newest_first_each_once_and_none_with_remember_off() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = owned(dir.path(), true);
        let revision = history.revision();
        for i in 0..12u64 {
            history.record(&format!("https://a.test/{i}"), None, true, NOW + i);
        }
        assert_ne!(history.revision(), revision, "a visit changes the revision");
        // Visited again: still one entry, now the newest, with its title.
        history.record("https://a.test/3", None, true, NOW + 50);
        let revision = history.revision();
        history.record("https://a.test/3", Some("Three"), false, NOW + 50);
        assert_ne!(history.revision(), revision, "a new title changes it too");
        let recent = history.recent(8);
        assert_eq!(
            urls(&recent),
            [
                "https://a.test/3",
                "https://a.test/11",
                "https://a.test/10",
                "https://a.test/9",
                "https://a.test/8",
                "https://a.test/7",
                "https://a.test/6",
                "https://a.test/5"
            ]
        );
        assert_eq!(recent[0].title, "Three");
        assert_eq!(history.recent(2).len(), 2);
        assert!(history.recent(0).is_empty());
        assert_eq!(history.recent(100).len(), 12);
        history.set_remember(false);
        assert!(history.recent(8).is_empty());
    }

    #[test]
    fn remember_off_drops_what_was_loaded_and_on_reads_the_file_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = owned(dir.path(), true);
        history.record("https://a.example/", None, true, NOW);
        assert!(history.save());
        history.finish();

        let mut history = owned(dir.path(), true);
        history.record("https://b.example/", None, true, NOW + 1);
        history.set_remember(false);
        assert!(history.visits.is_empty());
        assert_eq!(history.due(), None, "the save waiting is dropped");
        assert!(history.complete("a").is_empty());
        assert!(!history.record("https://c.example/", None, true, NOW + 2));
        assert!(!history.save());
        history.set_remember(true);
        history.settle();
        assert_eq!(urls(&history.complete("a")), ["https://a.example/"]);
        assert!(history.complete("b").is_empty(), "never saved");
        history.finish();
        // Started with it off, the file is not read and stays as it is.
        let before = fs::read_to_string(file(dir.path())).unwrap();
        let history = owned(dir.path(), false);
        assert!(history.visits.is_empty() && !history.loaded);
        history.finish();
        assert_eq!(fs::read_to_string(file(dir.path())).unwrap(), before);
    }

    #[test]
    fn a_forget_is_written_at_once_and_supersedes_the_save_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = owned(dir.path(), true);
        history.record("https://www.example.com/", None, true, NOW);
        history.record("https://example.com/a", None, true, NOW + 1);
        history.record("https://example.org/", None, true, NOW + 2);
        history.save();
        history.record("https://late.example.com/", None, true, NOW + 3);
        assert!(history.due().is_some());
        let generation = history.generation();
        let request = RequestId::next();
        assert_eq!(history.forget(request, Some("example.com")), None);
        assert_ne!(history.generation(), generation);
        assert_eq!(history.due(), None, "nothing else waits");
        assert_eq!(
            reports(&mut history, 1),
            [(request, ForgetOutcome::Done { removed: 3 })]
        );
        let (visits, _) = parse(&fs::read_to_string(file(dir.path())).unwrap(), NOW).unwrap();
        let urls: Vec<_> = visits.into_iter().map(|(url, _)| url).collect();
        assert_eq!(urls, ["https://example.org/"]);

        // With remember off, the file's visits are read to forget them, then dropped again.
        history.set_remember(false);
        let request = RequestId::next();
        assert_eq!(history.forget(request, None), None);
        assert_eq!(
            reports(&mut history, 1),
            [(request, ForgetOutcome::Done { removed: 1 })]
        );
        assert!(history.visits.is_empty() && !history.loaded);
        history.finish();
        let (visits, _) = parse(&fs::read_to_string(file(dir.path())).unwrap(), NOW).unwrap();
        assert!(visits.is_empty());
    }

    #[test]
    fn another_sig_forgets_here_only_and_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        let mut owner = owned(dir.path(), true);
        owner.record("https://a.example/", None, true, unix_now());
        owner.finish();
        let before = fs::read_to_string(file(dir.path())).unwrap();
        let mut other = opened(Some(file(dir.path())), Ownership::Elsewhere, true);
        assert_eq!(
            other.take_notices(),
            [
                "Another sig owns the history of visited pages; this one won't save the pages it \
              visits."
            ]
        );
        assert_eq!(urls(&other.complete("a.")), ["https://a.example/"]);
        assert!(other.visit("https://b.example/", None, true));
        assert_eq!(other.due(), None);
        assert!(!other.save());
        assert_eq!(
            other.forget(RequestId::next(), None),
            Some(ForgetOutcome::HereOnly { removed: 2 })
        );
        other.finish();
        assert_eq!(fs::read_to_string(file(dir.path())).unwrap(), before);
        // Nobody's lock (a read-only state directory): forgetting can't reach the file.
        let mut stuck = opened(Some(file(dir.path())), Ownership::Nobody, true);
        assert!(stuck.take_notices()[0].contains("sig can't lock its state files"));
        assert!(matches!(
            stuck.forget(RequestId::next(), None),
            Some(ForgetOutcome::NotWritten { removed: 1, .. })
        ));
        // No path at all (sig --dump): nothing is kept, and nothing is left to forget.
        let mut none = opened(None, Ownership::Nobody, true);
        assert!(!none.visit("https://a.example/", None, true));
        assert!(none.take_notices().is_empty());
        assert_eq!(
            none.forget(RequestId::next(), None),
            Some(ForgetOutcome::Done { removed: 0 })
        );
    }

    /// A temporary file a writer killed mid-write left (`history.toml.<pid>.tmp`) holds URLs:
    /// the owner removes it when it opens the history; another `sig`, which never writes there
    /// and may be the one still writing it, leaves it.
    #[test]
    fn the_owner_removes_a_dead_writers_temporary_and_another_sig_leaves_it() {
        let dir = tempfile::tempdir().unwrap();
        let temporary = dir.path().join("history.toml.4242.tmp");
        let unrelated = dir.path().join("history.toml.x.tmp");
        fs::write(&temporary, "version = 1\n").unwrap();
        fs::write(&unrelated, "not a writer's").unwrap();
        opened(Some(file(dir.path())), Ownership::Elsewhere, true).finish();
        opened(Some(file(dir.path())), Ownership::Nobody, true).finish();
        assert!(temporary.exists(), "a sig that doesn't own it removed it");
        owned(dir.path(), true).finish();
        assert!(!temporary.exists(), "the owner left it");
        assert!(unrelated.exists(), "not a pid's temporary: kept");
    }

    #[test]
    fn a_failed_write_is_reported_once_and_a_forget_says_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = owned(dir.path(), true);
        // A directory where the file goes: renaming over it fails.
        fs::create_dir_all(file(dir.path()).join("x")).unwrap();
        history.record("https://a.example/", None, true, NOW);
        history.save();
        let request = RequestId::next();
        assert_eq!(history.forget(request, None), None);
        let mut failure = None;
        let mut forgotten = Vec::new();
        for _ in 0..500 {
            let (done, failed) = history.take_reports();
            forgotten.extend(done);
            failure = failure.or(failed);
            if !forgotten.is_empty() && failure.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(failure
            .unwrap()
            .starts_with("The pages you visit can't be saved to "));
        assert!(matches!(
            &forgotten[..],
            [(r, ForgetOutcome::NotWritten { removed: 1, .. })] if *r == request
        ));
        history.finish();
        assert!(file(dir.path()).join("x").is_dir());
    }

    /// Run with `--ignored --nocapture` to measure the writer on this machine's disk.
    #[test]
    #[ignore]
    fn measure_writing_5000_visits() {
        let dir = std::env::var_os("SIGHURT_MEASURE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = tempfile::tempdir_in(dir).unwrap();
        let mut history = owned(dir.path(), true);
        for i in 0..MAX_VISITS as u64 {
            let url = format!(
                "https://site{}.example.com/articles/{i}/a-typical-slug?ref=feed",
                i % 300
            );
            history.record(&url, None, true, NOW + i);
            history.record(
                &url,
                Some("A typical page title — Example Site"),
                false,
                NOW + i,
            );
        }
        let started = Instant::now();
        let text = history.to_text();
        let serialized = started.elapsed();
        write_atomically(&file(dir.path()), &text).unwrap();
        let written = started.elapsed();
        let started = Instant::now();
        let (visits, _) = parse(&fs::read_to_string(file(dir.path())).unwrap(), NOW).unwrap();
        let read = started.elapsed();
        assert_eq!(visits.len(), MAX_VISITS);
        println!(
            "{} visits, {} bytes: serialized in {serialized:?}, written and synced by {written:?}; \
             read and parsed in {read:?}",
            MAX_VISITS,
            text.len()
        );
        history.finish();
    }

    /// Run with `--release --ignored --nocapture` to time completion over a full history: the
    /// median of 200 queries for each prefix, from one that matches every visit to one that
    /// matches a few.
    #[test]
    #[ignore]
    fn measure_completing_from_5000_visits() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = owned(dir.path(), true);
        for i in 0..MAX_VISITS as u64 {
            let url = format!(
                "https://www.site{}.example.com/articles/{i}/a-typical-slug?ref=feed",
                i % 300
            );
            history.record(&url, None, true, NOW + i);
            history.record(
                &url,
                Some("A typical page title — Example Site"),
                false,
                NOW + i,
            );
        }
        for typed in [
            "h",
            "w",
            "site1",
            "site12.example.com/articles/1",
            "nothing",
        ] {
            let mut times: Vec<Duration> = (0..200)
                .map(|_| {
                    let started = Instant::now();
                    std::hint::black_box(history.complete(std::hint::black_box(typed)));
                    started.elapsed()
                })
                .collect();
            times.sort();
            println!(
                "complete({typed:?}): {} found, median {:?}",
                history.complete(typed).len(),
                times[100]
            );
        }
        history.finish();
    }
}
