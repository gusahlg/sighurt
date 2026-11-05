//! Seeded random-input tests of the history of visited pages (`sighurt_runtime::history`): its
//! file as `History::open` reads it (valid files, mutated ones, random bytes, sizes around the
//! limit), and the store driven with random visits, titles, forgets, `remember` flips and
//! completion queries. No panic, a return in time, and what completion gives is what it
//! promises: at most `MAX_COMPLETIONS` addresses, each one the typed text completes, each an
//! http or https URL of printable ASCII within the core's bound, each title contained.
//! `tools/fuzz/mod.rs` describes the environment variables.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuzz::{iterations, run, Rng};
use sighurt_api::ids::RequestId;
use sighurt_runtime::history::{
    suffix, Completion, History, Ownership, MAX_COMPLETIONS, MAX_FILE, MAX_TITLE, RETENTION,
};

const LIMIT: Duration = Duration::from_secs(5);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn quiet() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}

const HOSTS: &[&str] = &[
    "example.com",
    "www.example.com",
    "a.example.com",
    "EXAMPLE.com",
    "xn--bcher-kva.example",
    "bücher.example",
    "127.0.0.1",
    "[::1]",
    "localhost:8080",
    "user:pw@example.com",
    "",
];

/// An address as a page or a hand-edited file might hold it: mostly valid http and https
/// addresses, some with what a visit is never recorded with (userinfo, a fragment, another
/// scheme, spelling the parser would change).
fn url(rng: &mut Rng) -> String {
    let scheme = *rng.pick(&[
        "https://",
        "https://",
        "http://",
        "HTTPS://",
        "ftp://",
        "file://",
        "",
        "javascript:",
    ]);
    let host = *rng.pick(HOSTS);
    let path = rng.string_from(
        &["/", "a", "b", "-", "%20", "?q=1", "&", "é", " ", "#x", "."],
        8,
    );
    let mut url = format!("{scheme}{host}/{path}");
    if rng.one_in(10) {
        url.push_str(&rng.string(12));
    }
    if rng.one_in(40) {
        url.push_str(&"x".repeat(rng.range(8000, 8300)));
    }
    url
}

/// `url` as the core records a visit to it (`sighurt_core::state`'s `visit_url`): http or https
/// without userinfo, the fragment dropped, as the URL parser writes it, at most 8 KiB. The store
/// takes the core's word for what a visit is.
fn visit_url(url: &str) -> Option<String> {
    let mut url = url::Url::parse(url).ok()?;
    let web = matches!(url.scheme(), "http" | "https");
    if !web || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    url.set_fragment(None);
    let url = String::from(url);
    (url.len() <= 8 * 1024).then_some(url)
}

fn toml_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A history file: version 1 (mostly), visits with random fields, some damaged or old.
fn file_text(rng: &mut Rng) -> String {
    let version = *rng.pick(&["1", "1", "1", "2", "0", "\"1\"", "-1"]);
    let mut text = if rng.one_in(20) {
        String::new()
    } else {
        format!("version = {version}\n")
    };
    if rng.one_in(15) {
        text.push_str(&format!("{} = 3\n", rng.string_from(&["a", "b", "_"], 6)));
    }
    let now = now() as i64;
    for _ in 0..rng.range(0, 40) {
        text.push_str("[[visits]]\n");
        if !rng.one_in(20) {
            text.push_str(&format!("url = {}\n", toml_string(&url(rng))));
        }
        if rng.one_in(2) {
            text.push_str(&format!("title = {}\n", toml_string(&rng.string(260))));
        }
        let last = match rng.below(6) {
            0 => now - RETENTION.as_secs() as i64 - rng.below(1000) as i64,
            1 => -(rng.below(10) as i64),
            2 => i64::MAX,
            _ => now - rng.below(1_000_000) as i64,
        };
        if !rng.one_in(20) {
            text.push_str(&format!("last = {last}\n"));
        }
        let count = *rng.pick(&[1i64, 2, 7, 0, -3, u32::MAX as i64 + 1]);
        if !rng.one_in(20) {
            text.push_str(&format!("count = {count}\n"));
        }
        if rng.one_in(30) {
            text.push_str("extra = true\n");
        }
    }
    if rng.one_in(10) {
        text = rng.mutate(&text);
    }
    text
}

/// What a user types into the address line: the start of a visited address in some spelling,
/// or anything.
fn typed(rng: &mut Rng, known: &[String]) -> String {
    if known.is_empty() || rng.one_in(4) {
        return rng.string(10);
    }
    let url = rng.pick(known).clone();
    let form = match rng.below(4) {
        0 => url.as_str(),
        1 => url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(&url),
        2 => url.split_once("://").map_or(url.as_str(), |(_, rest)| {
            rest.strip_prefix("www.").unwrap_or(rest)
        }),
        _ => url.get(1..).unwrap_or(""),
    };
    let mut cut = rng.below(form.len() + 1);
    while !form.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut text: String = form[..cut].to_owned();
    if rng.one_in(3) {
        text = text
            .chars()
            .map(|c| match rng.one_in(2) {
                true => c.to_ascii_uppercase(),
                false => c,
            })
            .collect();
    }
    if rng.one_in(8) {
        text = rng.mutate(&text);
    }
    text
}

/// What completion promises, for `typed`.
fn check_completions(typed: &str, found: &[Completion]) {
    assert!(
        found.len() <= MAX_COMPLETIONS,
        "{} completions",
        found.len()
    );
    if typed.is_empty() {
        assert!(found.is_empty(), "empty text completed to {found:?}");
    }
    for completion in found {
        let url = &completion.url;
        let rest = suffix(typed, url);
        assert!(
            rest.is_some_and(|rest| !rest.is_empty()),
            "{typed:?} does not complete {url:?}"
        );
        assert!(
            url.starts_with("https://") || url.starts_with("http://"),
            "{url:?}"
        );
        assert!(url.len() <= 8 * 1024, "{} bytes", url.len());
        assert!(
            url.bytes().all(|b| b.is_ascii_graphic()),
            "not printable ASCII: {url:?}"
        );
        let title = &completion.title;
        assert!(title.chars().count() <= MAX_TITLE, "{title:?}");
        assert!(
            !title.chars().any(|c| c.is_control()
                || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
            "not contained: {title:?}"
        );
    }
}

fn owner(dir: &Path) -> Ownership {
    let lock = fs::File::create(dir.join("session.lock")).unwrap();
    Ownership::Owner(Arc::new(lock))
}

/// A history file of any content, read by its owner or by another `sig`: whatever it holds,
/// completion keeps its promises, and a file the owner can't read is kept aside whole.
#[test]
fn fuzz_history_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.toml");
    // Cases that completed to something, and cases whose file was refused: both must happen.
    let (mut completed, mut refusals) = (0, 0);
    run("fuzz_history_files", iterations(100) / 2, LIMIT, |case| {
        for entry in fs::read_dir(dir.path()).unwrap().flatten() {
            let _ = fs::remove_file(entry.path());
        }
        let bytes = match case.rng.below(12) {
            0 => case.rng.bytes(300),
            1 => {
                let text = file_text(&mut case.rng);
                case.rng.mutate_bytes(text.as_bytes())
            }
            _ => file_text(&mut case.rng).into_bytes(),
        };
        case.note(|| format!("{:?}", String::from_utf8_lossy(&bytes)));
        fs::write(&path, &bytes).unwrap();
        let owned = case.rng.one_in(2);
        let ownership = match owned {
            true => owner(dir.path()),
            false => Ownership::Elsewhere,
        };
        let mut history = History::open(Some(path.clone()), ownership, true, quiet());
        let notices = history.take_notices();
        let urls: Vec<String> = history
            .complete("h")
            .into_iter()
            .chain(history.complete("w"))
            .map(|c| c.url)
            .collect();
        for _ in 0..8 {
            let text = typed(&mut case.rng, &urls);
            case.note(|| format!("typed {text:?} after {:?}", String::from_utf8_lossy(&bytes)));
            let found = history.complete(&text);
            check_completions(&text, &found);
            completed += usize::from(!found.is_empty());
        }
        let refused = notices.iter().any(|n| n.contains("can't be read"));
        refusals += usize::from(refused);
        history.finish();
        let kept: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".broken-"))
            .collect();
        if refused && owned {
            assert_eq!(kept.len(), 1, "{notices:?}");
            assert_eq!(fs::read(kept[0].path()).unwrap(), bytes, "kept aside whole");
        } else {
            assert!(kept.is_empty(), "{notices:?}");
            if !owned {
                assert_eq!(fs::read(&path).unwrap(), bytes, "another sig changed it");
            }
        }
    });
    if std::env::var_os("SIG_FUZZ_CASE").is_none() {
        assert!(
            completed > 0 && refusals > 0,
            "{completed} completed, {refusals} refused"
        );
    }
}

/// Files around the size limit: over it, refused before parsing (no visits, a notice); at or
/// under it, read.
#[test]
fn fuzz_history_file_size_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.toml");
    let visit = format!(
        "[[visits]]\nurl = \"https://example.com/page\"\nlast = {}\ncount = 1\n",
        now()
    );
    run("fuzz_history_size", iterations(100) / 200, LIMIT, |case| {
        let size = MAX_FILE as usize + case.rng.range(0, 64) - 32;
        let mut text = format!("version = 1\n{visit}");
        // A comment pads the file to `size` without changing what it holds.
        text.push('#');
        text.push_str(&"x".repeat(size - text.len() - 1));
        text.push('\n');
        assert_eq!(text.len(), size);
        case.note(|| format!("{size} bytes"));
        fs::write(&path, &text).unwrap();
        let mut history = History::open(Some(path.clone()), Ownership::Elsewhere, true, quiet());
        let notices = history.take_notices();
        let found = history.complete("example");
        if size as u64 > MAX_FILE {
            assert!(found.is_empty());
            assert!(
                notices.iter().any(|n| n.contains("over the limit")),
                "{notices:?}"
            );
        } else {
            assert_eq!(found.len(), 1, "{notices:?}");
        }
    });
}

/// The store driven with random visits, titles, forgets, `remember` flips and queries.
#[test]
fn fuzz_history_store() {
    run("fuzz_history_store", iterations(100) / 4, LIMIT, |case| {
        let ownership = match case.rng.below(2) {
            0 => Ownership::Elsewhere,
            _ => Ownership::Nobody,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.toml");
        let mut history = History::open(Some(path), ownership, true, quiet());
        let mut urls: Vec<String> = Vec::new();
        for _ in 0..case.rng.range(1, 60) {
            match case.rng.below(10) {
                0..=3 => {
                    let Some(address) = visit_url(&url(&mut case.rng)) else {
                        continue;
                    };
                    case.note(|| format!("visit {address:?}"));
                    history.visit(&address, None, true);
                    urls.push(address);
                }
                4 | 5 if !urls.is_empty() => {
                    let address = case.rng.pick(&urls).clone();
                    let title = case.rng.string(300);
                    case.note(|| format!("title {title:?} of {address:?}"));
                    history.visit(&address, Some(&title), false);
                }
                6 => {
                    let host = case.rng.one_in(2).then(|| {
                        case.rng
                            .pick(&["example.com", "com", "a.example.com", "", "."])
                            .to_string()
                    });
                    case.note(|| format!("forget {host:?}"));
                    history.forget(RequestId::next(), host.as_deref());
                }
                7 => history.set_remember(case.rng.one_in(2)),
                _ => {
                    let text = typed(&mut case.rng, &urls);
                    case.note(|| format!("complete {text:?}"));
                    let found = history.complete(&text);
                    check_completions(&text, &found);
                    if !history.remembers() {
                        assert!(found.is_empty(), "remember is off: {found:?}");
                    }
                }
            }
        }
        history.finish();
    });
}
