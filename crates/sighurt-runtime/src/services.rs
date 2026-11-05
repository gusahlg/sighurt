//! Performing the privileged services the core allowed: downloads into the download folder,
//! handing URLs to the desktop, and clipboard transfers through the frontend
//! ([`Clipboard`]).
//!
//! The clipboard itself belongs to the frontend (the Wayland host, or later the terminal).
//! This module decides nothing about selections; it asks the frontend to read, write and
//! cancel, and reports the answer to the core.
//!
//! Work that takes time runs off the owner thread (downloads on threads of their own, at most
//! [`MAX_DOWNLOADS`] at once, past which a download fails with a notice; clipboard reads however
//! the frontend carries them out) and reports back with its `RequestId` through a channel that
//! wakes the loop. Downloads report progress as short status lines for the status strip, all of
//! them together at most one per [`PROGRESS_EVERY`], as the old browser did: saved as `name`,
//! `name (1)`, ... in the download folder
//! (`[downloads] directory`, else the desktop's, else `~/Downloads`; made by the download that
//! first needs it), a name that never leads out of the folder, is never a dot file, hides no
//! characters and fits a file system's 255 bytes, and no file left behind by
//! a failed or cancelled download: at quit, the file of a download still waiting on its server
//! is deleted by the owner. File pickers and permissions have no implementation yet and fail
//! closed with `ServiceError::Unsupported`, as does anything else.
//!
//! Must not own: the decision whether a service is allowed (the core).

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sighurt_api::ids::RequestId;
use sighurt_api::services::{
    ClipboardData, DownloadSource, Selection, Service, ServiceError, ServiceReply, ServiceResult,
};

/// The clipboard, as the frontend's platform provides it.
///
/// Page services reach the clipboard only through this. The frontend carries a read out
/// asynchronously and later hands the bytes back; a write is answered at once.
pub trait Clipboard {
    /// Start reading `selection` in the first of `mime_types` it offers, at most `limit` bytes.
    /// The outcome is not returned here: the frontend delivers it later, tagged with `request`.
    fn read(
        &mut self,
        request: RequestId,
        selection: Selection,
        mime_types: &[String],
        limit: usize,
    );

    /// Replace `selection` with `items`.
    fn write(
        &mut self,
        selection: Selection,
        items: Vec<ClipboardData>,
    ) -> Result<(), ServiceError>;

    /// Abandon the read for `request`. Nothing is reported for it.
    fn cancel(&mut self, request: RequestId);
}

/// How often the running downloads, all of them together, report their progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// The most downloads running at once.
pub const MAX_DOWNLOADS: usize = 8;
/// The most bytes of a download's name, before ` (n)` is added to make it unique.
const MAX_NAME: usize = 200;
/// The most clipboard bytes `sig` reads for anyone.
pub const MAX_CLIPBOARD: usize = 16 * 1024 * 1024;
/// How long a download may take, all of it.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// How long quitting waits for cancelled downloads to delete their files.
const CANCEL_GRACE: Duration = Duration::from_millis(500);

/// Something a service thread finished or reports.
pub enum Completion {
    /// A line for the status strip.
    Status(String),
    /// A thread is done.
    Finished(u64),
}

/// A running download.
struct Download {
    cancel: Arc<AtomicBool>,
    /// The file it writes, from when it is created until it is complete.
    partial: Partial,
    thread: JoinHandle<()>,
}

/// The file a download is writing, until it is complete.
type Partial = Arc<Mutex<Option<PathBuf>>>;

/// Takes the path out of `partial`.
fn take(partial: &Mutex<Option<PathBuf>>) -> Option<PathBuf> {
    partial
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}

/// The services in progress.
pub struct Services {
    sender: Sender<Completion>,
    receiver: Receiver<Completion>,
    /// Wakes the owner loop.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Clipboard reads for pages, which the host answers with `ClipboardRead`.
    clipboard_reads: HashSet<RequestId>,
    downloads: HashMap<u64, Download>,
    next: u64,
    /// When a download last reported its progress, shared by all of them.
    progress: Arc<Mutex<Option<Instant>>>,
}

impl Services {
    /// Services whose threads call `wake` when they have something to report.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver,
            wake: Arc::new(wake),
            clipboard_reads: HashSet::new(),
            downloads: HashMap::new(),
            next: 0,
            progress: Arc::default(),
        }
    }

    /// Performs `service` for `request`, a download into `downloads` if the configuration names
    /// a folder. `Some` is the answer now; `None` means it comes later, through
    /// [`Services::clipboard_read`]. Clipboard work is asked of `clipboard`.
    pub fn perform(
        &mut self,
        clipboard: &mut impl Clipboard,
        request: RequestId,
        service: Service,
        downloads: Option<&Path>,
    ) -> Option<ServiceResult> {
        match service {
            Service::ClipboardRead {
                selection,
                mime_types,
                limit,
            } => {
                self.clipboard_reads.insert(request);
                clipboard.read(request, selection, &mime_types, limit.min(MAX_CLIPBOARD));
                None
            }
            Service::ClipboardWrite { selection, items } => Some(
                clipboard
                    .write(selection, items)
                    .map(|()| ServiceReply::Done),
            ),
            Service::Download {
                source,
                suggested_name,
            } => Some(
                folder(downloads)
                    .map_err(ServiceError::Failed)
                    .and_then(|dir| self.download(source, suggested_name, dir)),
            ),
            Service::OpenExternal { url } => Some(open_external(&url)),
            Service::PickFiles { .. } | Service::Permission(_) | Service::Page(_) => {
                Some(Err(ServiceError::Unsupported))
            }
        }
    }

    /// The frontend finished a clipboard read: the answer for the page, if the read was a page's.
    pub fn clipboard_read(
        &mut self,
        request: RequestId,
        result: Result<ClipboardData, ServiceError>,
    ) -> Option<ServiceResult> {
        self.clipboard_reads
            .remove(&request)
            .then(|| result.map(ServiceReply::Clipboard))
    }

    /// Stops work on `request`; nothing is reported for it.
    pub fn cancel(&mut self, clipboard: &mut impl Clipboard, request: RequestId) {
        if self.clipboard_reads.remove(&request) {
            clipboard.cancel(request);
        }
    }

    /// What service threads reported since the last call, at most `max`.
    pub fn completions(&mut self, max: usize) -> Vec<Completion> {
        let mut done: Vec<Completion> = self.receiver.try_iter().take(max).collect();
        done.retain(|completion| match completion {
            Completion::Finished(id) => {
                if let Some(download) = self.downloads.remove(id) {
                    let _ = download.thread.join();
                }
                false
            }
            Completion::Status(_) => true,
        });
        done
    }

    /// Starts saving `source` in `dir`: the browser has taken the download over when this
    /// returns `Ok`. With [`MAX_DOWNLOADS`] running, it fails and says so.
    fn download(
        &mut self,
        source: DownloadSource,
        suggested: Option<String>,
        dir: PathBuf,
    ) -> ServiceResult {
        if self.downloads.len() >= MAX_DOWNLOADS {
            let message = format!(
                "A page's download was refused: {MAX_DOWNLOADS} downloads are running already."
            );
            self.reporter().send(Completion::Status(message.clone()));
            return Err(ServiceError::Failed(message));
        }
        let name = match (&source, suggested) {
            (_, Some(name)) => sanitize(&name),
            (DownloadSource::Url(url), None) => filename_from_url(url),
            (DownloadSource::Data(_), None) => "download".into(),
        };
        if let DownloadSource::Url(url) = &source {
            let scheme = url::Url::parse(url).map(|u| u.scheme().to_owned());
            if !matches!(scheme.as_deref(), Ok("http" | "https")) {
                return Err(ServiceError::Failed(format!(
                    "{name} can't be downloaded: only http and https downloads are supported"
                )));
            }
        }
        self.spawn_download(source, name, dir)
    }

    /// Saves `source` as `name` in `dir` on a thread of its own.
    fn spawn_download(
        &mut self,
        source: DownloadSource,
        name: String,
        dir: PathBuf,
    ) -> ServiceResult {
        let id = self.next;
        self.next += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let report = self.reporter();
        let stop = Arc::clone(&cancel);
        let partial = Partial::default();
        let file = Arc::clone(&partial);
        let thread = std::thread::Builder::new()
            .name("sighurt-download".into())
            .spawn(move || {
                let last = match save(source, &dir, &name, &stop, &file, &report) {
                    Ok(path) => format!("Saved {}", path.display()),
                    Err(e) => format!("Download of {name} failed: {e}"),
                };
                report.send(Completion::Status(last));
                report.send(Completion::Finished(id));
            })
            .map_err(|e| ServiceError::Failed(format!("cannot start the download: {e}")))?;
        self.downloads.insert(
            id,
            Download {
                cancel,
                partial,
                thread,
            },
        );
        Ok(ServiceReply::Done)
    }

    /// A reporter for a service thread.
    fn reporter(&self) -> Reporter {
        Reporter {
            sender: self.sender.clone(),
            wake: Arc::clone(&self.wake),
            progress: Arc::clone(&self.progress),
        }
    }

    /// Cancels every download: each stops at its next read and deletes its file. The first
    /// step of quitting (plan §20: stop creating work).
    pub fn cancel_downloads(&self) {
        for download in self.downloads.values() {
            download.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Cancels every download and waits a moment for them to end. The file of one still
    /// waiting on its server after that is deleted here, so none is left behind.
    pub fn shutdown(self) {
        self.cancel_downloads();
        let until = Instant::now() + CANCEL_GRACE;
        for (_, download) in self.downloads {
            while !download.thread.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            if download.thread.is_finished() {
                let _ = download.thread.join();
            } else if let Some(path) = take(&download.partial) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// How a service thread reports.
struct Reporter {
    sender: Sender<Completion>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// When a download last reported its progress, shared by all of them.
    progress: Arc<Mutex<Option<Instant>>>,
}

impl Reporter {
    fn send(&self, completion: Completion) {
        if self.sender.send(completion).is_ok() {
            (self.wake)();
        }
    }

    /// Whether a download may report its progress now: no download has in the last
    /// [`PROGRESS_EVERY`]. If so, it counts as having reported.
    fn progress_due(&self) -> bool {
        let mut last = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if last.is_some_and(|last| now.saturating_duration_since(last) < PROGRESS_EVERY) {
            return false;
        }
        *last = Some(now);
        true
    }
}

/// Hands `url` to the desktop's default handler (`xdg-open`), without waiting for it.
fn open_external(url: &str) -> ServiceResult {
    // A parsed URL starts with its scheme, so it can't be taken for an option.
    let url = url::Url::parse(url).map_err(|e| ServiceError::Failed(e.to_string()))?;
    let child = std::process::Command::new("xdg-open")
        .arg(url.as_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .map_err(|e| ServiceError::Failed(format!("cannot run xdg-open: {e}")))?;
    // Reap it when it exits, off the owner thread.
    let _ = std::thread::Builder::new()
        .name("sighurt-xdg-open".into())
        .spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });
    Ok(ServiceReply::Done)
}

/// The download folder: `configured`, else the desktop's, else `~/Downloads`; never the home
/// folder itself, where a file's name could make it a dot file something else reads.
fn folder(configured: Option<&Path>) -> Result<PathBuf, String> {
    configured
        .map(Path::to_owned)
        .or_else(dirs::download_dir)
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .ok_or_else(|| "there is no download folder: sig doesn't know the home folder".into())
}

/// Saves `source` into a new file named after `name` in `dir`, which is made if it doesn't
/// exist, and returns its path. A failed or cancelled download leaves no file behind. `partial`
/// holds the file's path while it is incomplete.
fn save(
    source: DownloadSource,
    dir: &Path,
    name: &str,
    cancel: &AtomicBool,
    partial: &Mutex<Option<PathBuf>>,
    report: &Reporter,
) -> Result<PathBuf, String> {
    let (mut body, total): (Box<dyn Read>, Option<u64>) = match source {
        DownloadSource::Data(data) => {
            let len = data.len() as u64;
            (Box::new(io::Cursor::new(data)), Some(len))
        }
        DownloadSource::Url(url) => {
            let client = reqwest::blocking::Client::builder()
                .timeout(DOWNLOAD_TIMEOUT)
                .user_agent(concat!("Sighurt/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| e.to_string())?;
            let response = client.get(url).send().map_err(|e| e.to_string())?;
            if !response.status().is_success() {
                return Err(format!("HTTP {}", response.status()));
            }
            let total = response.content_length();
            (Box::new(response), total)
        }
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let (path, file) = create_unique(dir, name).map_err(|e| e.to_string())?;
    *partial.lock().unwrap_or_else(PoisonError::into_inner) = Some(path.clone());
    let total = total.map(format_bytes);
    let copy = || -> io::Result<()> {
        let mut writer = BufWriter::new(file);
        let mut buf = vec![0; 64 * 1024];
        let (mut received, started) = (0, Instant::now());
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::other("cancelled"));
            }
            let n = body.read(&mut buf)?;
            if n == 0 {
                return writer.flush();
            }
            writer.write_all(&buf[..n])?;
            received += n as u64;
            if started.elapsed() >= PROGRESS_EVERY && report.progress_due() {
                let received = format_bytes(received);
                report.send(Completion::Status(match &total {
                    Some(total) => format!("Downloading {name}: {received} of {total}"),
                    None => format!("Downloading {name}: {received}"),
                }));
            }
        }
    };
    let copied = copy();
    take(partial);
    match copied {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(e.to_string())
        }
    }
}

/// A file name from untrusted text: its last path component, without control or invisible
/// formatting characters (Unicode's format characters: bidi controls, zero-width ones) and
/// without dots or whitespace at either end, so it is never hidden or a dot file; or
/// `"download"`. As Firefox and Chromium clean the names they save. At most [`MAX_NAME`] bytes,
/// cut at a character and keeping a short extension, so ` (n)` still fits in a file system's
/// 255.
fn sanitize(name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|&c| !c.is_control() && !is_format(c))
        .collect();
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    match last.trim_matches(|c: char| c == '.' || c.is_whitespace()) {
        "" => "download".to_string(),
        name if name.len() <= MAX_NAME => name.to_string(),
        name => {
            let ext = match name.rfind('.') {
                Some(dot) if dot > 0 && name.len() - dot <= 16 => &name[dot..],
                _ => "",
            };
            let stem = &name[..name.len() - ext.len()];
            // The cut may land after a dot or a space: those are not left at the end of the name.
            let kept = cut(stem, MAX_NAME - ext.len());
            let kept = match ext {
                "" => kept.trim_end_matches(|c: char| c == '.' || c.is_whitespace()),
                _ => kept,
            };
            format!("{kept}{ext}")
        }
    }
}

/// The longest start of `text` of at most `max` bytes that ends at a character.
fn cut(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Whether `c` is a Unicode format character (general category Cf, Unicode 16).
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}' | '\u{890}'
        | '\u{891}' | '\u{8e2}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}'
        | '\u{110bd}' | '\u{110cd}' | '\u{13430}'..='\u{1343f}' | '\u{1bca0}'..='\u{1bca3}'
        | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
}

/// A reasonable filename from a URL, falling back to `"download"`. The name never leads out of
/// the download folder, whatever the URL's escapes decode to.
fn filename_from_url(url: &str) -> String {
    let last_segment = url::Url::parse(url)
        .ok()
        .and_then(|u| Some(u.path_segments()?.next_back()?.to_string()));
    let decoded = last_segment
        .map(|name| {
            percent_encoding::percent_decode_str(&name)
                .decode_utf8_lossy()
                .into_owned()
        })
        .unwrap_or_default();
    sanitize(&decoded)
}

/// Creates `dir/name`, or `name (1)`, `name (2)`, ... if it exists: the first free one when
/// the names in use run without gaps. Creating and checking are one step, so downloads running
/// side by side never share a file. Finding the number takes a logarithmic number of looks,
/// not one for each name in use.
fn create_unique(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    let stem = Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let ext = Path::new(name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let path = |i: u32| match i {
        0 => dir.join(name),
        i => dir.join(format!("{stem} ({i}){ext}")),
    };
    let taken = |i: u32| std::fs::symlink_metadata(path(i)).is_ok();
    // The highest number known to be in use.
    let mut used = None::<u32>;
    loop {
        let free = match used {
            None => 0,
            Some(used) => {
                // Doubling steps past the names in use, then halving back to the first free one.
                let (mut low, mut step) = (used, 1u32);
                while taken(low.saturating_add(step)) {
                    low = low.saturating_add(step);
                    step = step.saturating_mul(2);
                }
                let mut high = low.saturating_add(step);
                while high - low > 1 {
                    let middle = low + (high - low) / 2;
                    match taken(middle) {
                        true => low = middle,
                        false => high = middle,
                    }
                }
                high
            }
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path(free))
        {
            Ok(file) => return Ok((path(free), file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && free < u32::MAX => {
                used = Some(free);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Formats a byte count for display: "1.2 MB", "340 KB", etc.
fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufRead;
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn filename_extraction() {
        assert_eq!(
            filename_from_url("https://github.com/robots.txt"),
            "robots.txt"
        );
        assert_eq!(
            filename_from_url("https://example.com/path/to/file.zip"),
            "file.zip"
        );
        assert_eq!(filename_from_url("https://example.com/"), "download");
        assert_eq!(filename_from_url("https://example.com"), "download");
        assert_eq!(
            filename_from_url("https://example.com/hello%20world.pdf"),
            "hello world.pdf"
        );
        // Escaped separators can't reach outside the download folder.
        assert_eq!(
            filename_from_url("https://example.com/..%2F.config%2Fautostart%2Fx.desktop"),
            "x.desktop"
        );
        assert_eq!(filename_from_url("https://example.com/a%5C..%5Cb"), "b");
        assert_eq!(filename_from_url("https://example.com/x%2F.."), "download");
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("a\nb.txt"), "ab.txt");
    }

    #[test]
    fn names_are_never_dot_files_or_hide_characters() {
        let cases = [
            (".zshenv", "zshenv"),
            ("a/../.profile", "profile"),
            ("/home/me/.ssh/authorized_keys", "authorized_keys"),
            ("..", "download"),
            (" . ", "download"),
            ("name.", "name"),
            ("  report.pdf  ", "report.pdf"),
            // A right-to-left override would show "invoice_exe.pdf" for this name.
            ("invoice_\u{202e}fdp.exe", "invoice_fdp.exe"),
            ("a\u{200b}b\u{feff}c\u{2066}d.txt", "abcd.txt"),
            ("\u{e0041}tag.txt", "tag.txt"),
            ("räksmörgås.txt", "räksmörgås.txt"),
        ];
        for (name, saved) in cases {
            assert_eq!(sanitize(name), saved, "{name:?}");
        }
        assert_eq!(filename_from_url("https://a.test/..%2F.zshenv"), "zshenv");
        assert_eq!(
            filename_from_url("https://a.test/%E2%80%AEfdp.exe"),
            "fdp.exe"
        );
    }

    #[test]
    fn names_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _) = create_unique(dir.path(), "test.txt").unwrap();
        let (second, _) = create_unique(dir.path(), "test.txt").unwrap();
        let (third, _) = create_unique(dir.path(), "test.txt").unwrap();
        assert_eq!(first, dir.path().join("test.txt"));
        assert_eq!(second, dir.path().join("test (1).txt"));
        assert_eq!(third, dir.path().join("test (2).txt"));
    }

    #[test]
    fn long_names_are_cut_to_bytes_at_a_character_keeping_the_extension() {
        let cjk = "漢".repeat(150);
        let name = sanitize(&format!("{cjk}.pdf"));
        assert!(name.len() <= MAX_NAME && name.ends_with(".pdf"), "{name}");
        assert_eq!(name, format!("{}.pdf", "漢".repeat(65)));
        assert_eq!(sanitize(&"é".repeat(150)), "é".repeat(100));
        // A long "extension" is part of the name.
        let dotted = format!("a.{}", "b".repeat(300));
        assert_eq!(sanitize(&dotted), dotted[..MAX_NAME]);
        // The numbered copies of the longest name still fit a file system's 255 bytes.
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..3 {
            let (path, _) = create_unique(dir.path(), &name).unwrap();
            assert!(path.file_name().unwrap().len() <= 255);
        }
    }

    #[test]
    fn names_skip_a_long_run_of_copies_in_few_looks() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..40 {
            create_unique(dir.path(), "a.txt").unwrap();
        }
        let (path, _) = create_unique(dir.path(), "a.txt").unwrap();
        assert_eq!(path, dir.path().join("a (40).txt"));
        // A gap the steps pass over stays a gap: any free name will do.
        std::fs::remove_file(dir.path().join("a (32).txt")).unwrap();
        let (path, _) = create_unique(dir.path(), "a.txt").unwrap();
        assert_eq!(path, dir.path().join("a (41).txt"));
        std::fs::remove_file(dir.path().join("a (1).txt")).unwrap();
        let (path, _) = create_unique(dir.path(), "a.txt").unwrap();
        assert_eq!(path, dir.path().join("a (1).txt"));
    }

    #[test]
    fn at_most_max_downloads_run_at_once() {
        // Accepts connections and never answers.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/x.bin", listener.local_addr().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let mut services = Services::new(|| {});
        let mut start =
            || services.download(DownloadSource::Url(url.clone()), None, dir.path().into());
        for _ in 0..MAX_DOWNLOADS {
            assert!(start().is_ok());
        }
        assert!(matches!(start(), Err(ServiceError::Failed(m)) if m.contains("refused")));
        let lines: Vec<String> = services
            .completions(16)
            .into_iter()
            .filter_map(|completion| match completion {
                Completion::Status(line) => Some(line),
                Completion::Finished(_) => None,
            })
            .collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        services.shutdown();
    }

    #[test]
    fn progress_is_reported_at_most_every_250_ms_by_all_downloads_together() {
        let (sender, _receiver) = mpsc::channel();
        let shared = Arc::default();
        let reporter = || Reporter {
            sender: sender.clone(),
            wake: Arc::new(|| {}),
            progress: Arc::clone(&shared),
        };
        let (a, b) = (reporter(), reporter());
        assert!(a.progress_due());
        assert!(!b.progress_due() && !a.progress_due());
        std::thread::sleep(PROGRESS_EVERY);
        assert!(b.progress_due());
        assert!(!a.progress_due());
    }

    #[test]
    fn format_bytes_display() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1 KB");
        assert_eq!(format_bytes(1_500_000), "1.4 MB");
        assert_eq!(format_bytes(2_000_000_000), "1.9 GB");
    }

    /// Serves `responses` to one connection each, on a local port, and returns its address
    /// and the request headers it received.
    fn serve(responses: &'static [&'static str]) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (requests, received) = mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                let mut reader = io::BufReader::new(&stream);
                // Up to the empty line that ends the headers.
                while reader.read_line(&mut request).unwrap() > 2 {}
                let _ = requests.send(request);
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (address, received)
    }

    /// Runs `source` to its end in `dir` and returns its last status line.
    fn finish(source: DownloadSource, dir: &Path, name: &str) -> String {
        let (sender, receiver) = mpsc::channel();
        let report = Reporter {
            sender,
            wake: Arc::new(|| {}),
            progress: Arc::default(),
        };
        let partial = Mutex::default();
        let last = match save(
            source,
            dir,
            name,
            &AtomicBool::new(false),
            &partial,
            &report,
        ) {
            Ok(path) => format!("Saved {}", path.display()),
            Err(e) => format!("Download of {name} failed: {e}"),
        };
        drop(receiver);
        last
    }

    #[test]
    fn saves_downloads_under_free_names() {
        const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
        let (address, requests) = serve(&[OK, OK]);
        let dir = tempfile::tempdir().unwrap();
        let url = format!("{address}/files/hello.txt");

        let saved = dir.path().join("hello.txt");
        let last = finish(DownloadSource::Url(url.clone()), dir.path(), "hello.txt");
        assert_eq!(last, format!("Saved {}", saved.display()));
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "hello");
        let request = requests.recv().unwrap().to_ascii_lowercase();
        assert!(request.contains("user-agent: sighurt/"), "{request}");

        let last = finish(DownloadSource::Url(url), dir.path(), "hello.txt");
        assert!(last.ends_with("hello (1).txt"), "{last}");
        let last = finish(
            DownloadSource::Data(b"data".to_vec()),
            dir.path(),
            "hello.txt",
        );
        assert!(last.ends_with("hello (2).txt"), "{last}");
    }

    #[test]
    fn failed_downloads_leave_nothing_behind() {
        const MISSING: &str =
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        // Promises more than it sends.
        const CUT: &str = "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nhalf";
        let (address, _requests) = serve(&[MISSING, CUT]);
        let dir = tempfile::tempdir().unwrap();

        let last = finish(
            DownloadSource::Url(format!("{address}/a.zip")),
            dir.path(),
            "a.zip",
        );
        assert_eq!(last, "Download of a.zip failed: HTTP 404 Not Found");
        let last = finish(
            DownloadSource::Url(format!("{address}/b.zip")),
            dir.path(),
            "b.zip",
        );
        assert!(last.starts_with("Download of b.zip failed"), "{last}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn downloads_go_to_the_configured_folder_made_when_first_needed() {
        let dir = tempfile::tempdir().unwrap();
        let configured = dir.path().join("Hämtade filer").join("sig");
        assert_eq!(folder(Some(&configured)), Ok(configured.clone()));
        // Never the home folder itself.
        let desktops = dirs::download_dir().or_else(|| Some(dirs::home_dir()?.join("Downloads")));
        assert_eq!(folder(None).ok(), desktops);
        assert_ne!(folder(None).ok(), dirs::home_dir());
        assert!(!configured.exists());
        let last = finish(
            DownloadSource::Data(b"hej".to_vec()),
            &folder(Some(&configured)).unwrap(),
            "a.txt",
        );
        let saved = configured.join("a.txt");
        assert_eq!(last, format!("Saved {}", saved.display()));
        assert_eq!(std::fs::read_to_string(saved).unwrap(), "hej");
    }

    #[test]
    fn a_folder_that_cant_be_made_fails_the_download_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a file");
        std::fs::write(&file, "").unwrap();
        let last = finish(
            DownloadSource::Data(b"hej".to_vec()),
            &file.join("sub"),
            "a.txt",
        );
        assert!(
            last.starts_with(&format!(
                "Download of a.txt failed: cannot make {}",
                file.join("sub").display()
            )),
            "{last}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read(&file).unwrap(), b"");
    }

    #[test]
    fn a_cancelled_download_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, _receiver) = mpsc::channel();
        let report = Reporter {
            sender,
            wake: Arc::new(|| {}),
            progress: Arc::default(),
        };
        let result = save(
            DownloadSource::Data(vec![0; 1024]),
            dir.path(),
            "x.bin",
            &AtomicBool::new(true),
            &Mutex::default(),
            &report,
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn quitting_deletes_a_download_its_server_keeps_waiting() {
        // Sends half of the body, then nothing, with the connection open.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/stalled.bin", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = io::BufReader::new(&stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let head = "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nhalf";
            stream.write_all(head.as_bytes()).unwrap();
            std::thread::sleep(Duration::from_secs(30));
        });
        let dir = tempfile::tempdir().unwrap();
        let mut services = Services::new(|| {});
        let started = services.spawn_download(
            DownloadSource::Url(url),
            "stalled.bin".into(),
            dir.path().into(),
        );
        assert!(started.is_ok());
        let file = dir.path().join("stalled.bin");
        let until = Instant::now() + Duration::from_secs(5);
        while !file.exists() {
            assert!(
                Instant::now() < until,
                "the download never created its file"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let quitting = Instant::now();
        services.shutdown();
        assert!(quitting.elapsed() < CANCEL_GRACE + Duration::from_millis(200));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

#[cfg(test)]
#[path = "services_fuzz.rs"]
mod fuzz_tests;
