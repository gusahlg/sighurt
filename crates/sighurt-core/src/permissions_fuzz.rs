//! Random-input tests of the page-facing checks of [`super`] that are private to it: the
//! external-link denylist and the download requirement. Seeded; see `tools/fuzz/mod.rs`.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::time::Duration;

use fuzz::{iterations, run, Rng};
use sighurt_api::services::{DownloadSource, Permission, Service};

use super::{is_external, requirement, Requirement, NEVER_EXTERNAL};
use crate::tabs::MAX_URL;

const OURS: [&str; 10] = [
    "http", "https", "file", "sig", "data", "about", "javascript", "blob", "ws", "wss",
];
const OTHER: [&str; 8] = ["mailto", "tel", "magnet", "xmpp", "steam", "x-custom", "web+foo", "a"];

/// The scheme the url crate's parser reads from `text`, found independently: it drops tabs and
/// newlines everywhere and leading control characters and spaces, and a scheme is a letter,
/// then letters, digits, `+`, `-` and `.`, up to the first `:`.
fn scheme_of(text: &str) -> Option<String> {
    let text: String = text.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let text = text.trim_start_matches(|c: char| c <= ' ');
    let (scheme, _) = text.split_once(':')?;
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then(|| scheme.to_ascii_lowercase())
}

fn disguise(rng: &mut Rng, scheme: &str) -> String {
    let mut text = String::new();
    for _ in 0..rng.below(3) {
        text.push(*rng.pick(&[' ', '\t', '\n', '\u{1}', '\u{1f}', '\r']));
    }
    for c in scheme.chars() {
        text.push(if rng.one_in(2) { c.to_ascii_uppercase() } else { c });
        if rng.one_in(8) {
            text.push(*rng.pick(&['\t', '\n', '\r', '\u{200b}', '\u{ad}', ' ', '\0']));
        }
    }
    text.push(':');
    text.push_str(rng.pick(&["", "//", "///", "//host/", "//u:p@host:22/x", "x", "//[::1]/", "/"]));
    text.push_str(&rng.string(8));
    text
}

#[test]
fn fuzz_external_link_denylist() {
    run("fuzz_external", iterations(100) * 3, Duration::from_secs(2), |case| {
        let scheme = match case.rng.below(3) {
            0 => *case.rng.pick(&NEVER_EXTERNAL),
            1 => *case.rng.pick(&OURS),
            _ => *case.rng.pick(&OTHER),
        };
        let mut text = disguise(&mut case.rng, scheme);
        if case.rng.one_in(4) {
            text = case.rng.mutate(&text);
        }
        case.note(|| format!("{text:?}"));
        let external = is_external(&text);
        let expected = match scheme_of(&text) {
            Some(scheme) => {
                !OURS.contains(&scheme.as_str())
                    && !NEVER_EXTERNAL.contains(&scheme.as_str())
                    && url::Url::parse(&text).is_ok()
            }
            None => false,
        };
        assert_eq!(external, expected, "scheme {:?}", scheme_of(&text));
        // Whatever the requirement, a denied scheme is never handed to the desktop.
        let required = requirement(&Service::OpenExternal { url: text.clone() });
        if scheme_of(&text).is_some_and(|s| NEVER_EXTERNAL.contains(&s.as_str()) || OURS.contains(&s.as_str())) {
            assert_eq!(required, Requirement::Refused);
        }
    });
}

#[test]
fn fuzz_download_requirement() {
    run("fuzz_download", iterations(100), Duration::from_secs(2), |case| {
        let mut url = format!("https://{}", case.rng.string(30));
        if case.rng.one_in(2) {
            url = case.rng.mutate(&url);
        }
        case.note(|| format!("{url:?}"));
        let required = requirement(&Service::Download {
            source: DownloadSource::Url(url.clone()),
            suggested_name: None,
        });
        let parses = url::Url::parse(&url).is_ok();
        assert_eq!(required == Requirement::Throttle, parses);
        assert!(matches!(
            requirement(&Service::Permission(Permission::Camera)),
            Requirement::Prompt(_)
        ));
    });
}

#[test]
fn an_overlong_external_url_is_refused() {
    let long = format!("mailto:a@b.test?{}", "x".repeat(MAX_URL));
    assert!(!is_external(&long));
}
