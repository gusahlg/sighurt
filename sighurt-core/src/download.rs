//! Downloads: what no engine shows is saved to the user's download folder.
//!
//! Each download runs on its own thread and reports how it is going as short status lines,
//! which the browser shows in its status bar. There is no list to manage.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a running download reports its progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// The user's download folder, or their home folder if there is none.
pub fn folder() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Saves `url` into `dir` on a background thread. The returned channel carries status lines
/// ("Downloading a.zip: 1.2 MB of 3.4 MB", "Saved /home/me/Downloads/a.zip", "Download of a.zip
/// failed: HTTP 404") and closes when the download is over.
pub fn start(url: String, dir: PathBuf) -> async_channel::Receiver<String> {
    let (report, status) = async_channel::unbounded();
    std::thread::spawn(move || {
        let name = filename_from_url(&url);
        let last = match save(&url, &dir, &name, &report) {
            Ok(path) => format!("Saved {}", path.display()),
            Err(e) => format!("Download of {name} failed: {e}"),
        };
        let _ = report.send_blocking(last);
    });
    status
}

/// Downloads `url` into a new file named after `name` in `dir` and returns its path. A failed
/// download leaves no file behind.
fn save(
    url: &str,
    dir: &Path,
    name: &str,
    report: &async_channel::Sender<String>,
) -> Result<PathBuf, String> {
    let client = crate::router::http_client(Duration::from_secs(600)).map_err(|e| e.to_string())?;
    let mut response = client.get(url).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let total = response.content_length().map(format_bytes);
    let (path, file) = create_unique(dir, name).map_err(|e| e.to_string())?;

    let copy = || -> io::Result<()> {
        let mut writer = BufWriter::new(file);
        let mut buf = vec![0; 64 * 1024];
        let (mut received, mut reported) = (0, Instant::now());
        loop {
            let n = response.read(&mut buf)?;
            if n == 0 {
                return writer.flush();
            }
            writer.write_all(&buf[..n])?;
            received += n as u64;
            if reported.elapsed() >= PROGRESS_EVERY {
                reported = Instant::now();
                let received = format_bytes(received);
                let _ = report.try_send(match &total {
                    Some(total) => format!("Downloading {name}: {received} of {total}"),
                    None => format!("Downloading {name}: {received}"),
                });
            }
        }
    };
    match copy() {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(e.to_string())
        }
    }
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
    match decoded.rsplit(['/', '\\']).next() {
        Some(name) if !matches!(name, "" | "." | "..") => name.to_string(),
        _ => "download".to_string(),
    }
}

/// Creates `dir/name`, or `name (1)`, `name (2)`, ... if it exists. Creating and checking are
/// one step, so downloads running side by side never share a file.
fn create_unique(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    let stem = Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let ext = Path::new(name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 0u32.. {
        let path = match i {
            0 => dir.join(name),
            i => dir.join(format!("{stem} ({i}){ext}")),
        };
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    unreachable!("some name is free")
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
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;

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
    fn format_bytes_display() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1 KB");
        assert_eq!(format_bytes(1_500_000), "1.4 MB");
        assert_eq!(format_bytes(2_000_000_000), "1.9 GB");
    }

    /// Serves `responses` to one connection each, on a local port, and returns its address
    /// and the request headers it received.
    fn serve(responses: &'static [&'static str]) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (requests, received) = std::sync::mpsc::channel();
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

    /// Runs a download to its end and returns its last status line.
    fn finish(status: async_channel::Receiver<String>) -> String {
        let mut last = String::new();
        while let Ok(line) = status.recv_blocking() {
            last = line;
        }
        last
    }

    #[test]
    fn saves_downloads_under_free_names() {
        const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
        let (address, requests) = serve(&[OK, OK]);
        let dir = tempfile::tempdir().unwrap();
        let url = format!("{address}/files/hello.txt");

        let saved = dir.path().join("hello.txt");
        let last = finish(start(url.clone(), dir.path().to_path_buf()));
        assert_eq!(last, format!("Saved {}", saved.display()));
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "hello");
        let request = requests.recv().unwrap().to_ascii_lowercase();
        assert!(request.contains("user-agent: sighurt/"), "{request}");

        let last = finish(start(url, dir.path().to_path_buf()));
        assert!(last.ends_with("hello (1).txt"), "{last}");
    }

    #[test]
    fn failed_downloads_leave_nothing_behind() {
        const MISSING: &str =
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        // Promises more than it sends.
        const CUT: &str = "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nhalf";
        let (address, _requests) = serve(&[MISSING, CUT]);
        let dir = tempfile::tempdir().unwrap();

        let last = finish(start(format!("{address}/a.zip"), dir.path().into()));
        assert_eq!(last, "Download of a.zip failed: HTTP 404 Not Found");
        let last = finish(start(format!("{address}/b.zip"), dir.path().into()));
        assert!(last.starts_with("Download of b.zip failed"), "{last}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
