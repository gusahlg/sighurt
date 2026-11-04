//! Random-input tests of the names downloads are saved under: [`super::sanitize`],
//! [`super::filename_from_url`] and [`super::create_unique`]. Seeded; see `tools/fuzz/mod.rs`.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::path::Path;
use std::time::Duration;

use fuzz::{iterations, run, Rng};

use super::{create_unique, filename_from_url, is_format, sanitize, MAX_NAME};

const PIECES: &[&str] = &[
    "..",
    ".",
    "/",
    "\\",
    "../",
    "..\\",
    "%2e%2e",
    "%2F",
    "%5C",
    "%00",
    "%0a",
    "%e2%80%ae",
    "%zz",
    "%",
    "a",
    "b.txt",
    ".bashrc",
    "x.tar.gz",
    " ",
    "\t",
    "\n",
    "\u{0}",
    "\u{7f}",
    "\u{202e}",
    "\u{200b}",
    "\u{feff}",
    "\u{a0}",
    "\u{3000}",
    "é",
    "日本語",
    "😀",
    "CON",
    "-rf",
    "~",
    "$HOME",
    "`x`",
    "*",
    "?",
    ":",
    "\"",
    "<",
    ">",
    "|",
    "\u{2800}",
    "\u{3164}",
];

fn name(rng: &mut Rng) -> String {
    let mut text = String::new();
    for _ in 0..rng.range(0, 8) {
        if rng.one_in(5) {
            text.push_str(&rng.string(6));
        } else {
            text.push_str(rng.pick(PIECES));
        }
    }
    match rng.below(12) {
        0 => text.push_str(&"a".repeat(rng.range(150, 600))),
        1 => {
            text = format!(
                "{}.{}",
                "é".repeat(rng.range(50, 300)),
                "x".repeat(rng.range(0, 30))
            )
        }
        2 => text = "😀".repeat(rng.range(40, 100)),
        3 => text = format!(".{}", text),
        _ => {}
    }
    text
}

/// A sanitized name is one safe file name in the download folder.
fn check_name(clean: &str) {
    assert!(!clean.is_empty(), "empty");
    assert!(clean.len() <= MAX_NAME, "{} bytes", clean.len());
    assert!(!clean.contains(['/', '\\']), "separator");
    assert!(
        !clean.starts_with('.') && !clean.ends_with('.'),
        "dot file or dotted end"
    );
    assert_ne!(clean, "..");
    assert!(
        !clean.chars().any(|c| c.is_control() || is_format(c)),
        "control or format character"
    );
    assert!(!clean.starts_with(char::is_whitespace) && !clean.ends_with(char::is_whitespace));
    assert!(
        Path::new(clean).components().count() == 1,
        "more than one path component"
    );
    assert!(
        Path::new(clean).file_name().is_some_and(|n| n == clean),
        "not a plain file name"
    );
}

#[test]
fn fuzz_sanitize() {
    run(
        "fuzz_sanitize",
        iterations(100) * 3,
        Duration::from_secs(2),
        |case| {
            let text = name(&mut case.rng);
            case.note(|| format!("{text:?}"));
            let clean = sanitize(&text);
            check_name(&clean);
            assert_eq!(sanitize(&clean), clean, "idempotent");
        },
    );
}

#[test]
fn fuzz_filename_from_url() {
    run(
        "fuzz_filename",
        iterations(100) * 3,
        Duration::from_secs(2),
        |case| {
            let path = name(&mut case.rng);
            let encoded: String = path
                .bytes()
                .map(|b| match case.rng.below(3) {
                    0 => format!("%{b:02X}"),
                    _ => (b as char).to_string(),
                })
                .collect();
            let mut url = format!("https://example.com/{encoded}");
            if case.rng.one_in(4) {
                url = case.rng.mutate(&url);
            }
            case.note(|| format!("{url:?}"));
            check_name(&filename_from_url(&url));
        },
    );
}

#[test]
fn fuzz_create_unique_stays_in_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let folder = dir.path().join("downloads");
    std::fs::create_dir(&folder).unwrap();
    run(
        "fuzz_create_unique",
        iterations(100) / 4,
        Duration::from_secs(5),
        |case| {
            let text = name(&mut case.rng);
            case.note(|| format!("{text:?}"));
            let clean = sanitize(&text);
            for _ in 0..case.rng.range(1, 4) {
                let (path, _file) = create_unique(&folder, &clean).unwrap();
                assert_eq!(path.parent(), Some(folder.as_path()), "{path:?}");
                let leaf = path.file_name().unwrap().to_string_lossy().into_owned();
                assert!(leaf.len() <= 255, "{leaf:?}");
            }
            for entry in std::fs::read_dir(&folder).unwrap().flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        },
    );
}

/// Found by `fuzz_filename_from_url`: cutting a long name left it ending in the dot or space it
/// was cut after, though a name is never left with dots or whitespace at either end.
#[test]
fn a_cut_name_does_not_end_in_a_dot_or_a_space() {
    let tail = "b".repeat(50);
    for name in [
        format!("{}.{tail}", "a".repeat(199)),
        format!("{} {tail}", "a".repeat(199)),
        format!("{}. . {tail}", "a".repeat(197)),
    ] {
        let clean = sanitize(&name);
        check_name(&clean);
        assert!(clean.len() <= MAX_NAME);
    }
    // With an extension the name still ends in it.
    let clean = sanitize(&format!("{}.{tail}.pdf", "a".repeat(300)));
    assert!(
        clean.ends_with(".pdf") && clean.len() <= MAX_NAME,
        "{clean}"
    );
}
