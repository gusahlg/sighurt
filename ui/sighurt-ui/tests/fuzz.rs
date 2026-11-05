//! Seeded random-input tests of what the chrome does to untrusted text: `Snapshot::contain`
//! (every string within its bound, on one line, with nothing that reorders or breaks it),
//! `contained`, and `readable_url` (never a panic, nothing hidden decoded, and typed back it
//! opens the same address). `tools/fuzz/mod.rs` describes the environment variables.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::time::Duration;

use fuzz::{iterations, run, Rng};
use sighurt_api::ids::TabId;
use sighurt_api::input::{DeleteSurrounding, Preedit, TextInputEvent};
use sighurt_ui::extension::{contained, readable_url};
use sighurt_ui::text_edit::{TextEditor, MAX_TEXT};
use sighurt_ui::{EngineRow, FindSnapshot, HomeInfo, Snapshot, TabContent, TabSnapshot};
use unicode_segmentation::UnicodeSegmentation;

/// Characters a one-line text must not hold: controls, the bidi embeddings, overrides and
/// isolates, and the separators that break a line.
fn offends(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}')
}

fn text(rng: &mut Rng) -> String {
    match rng.below(14) {
        0 => "x".repeat(rng.range(0, 40_000)),
        1 => "é\u{202e}\n".repeat(rng.range(0, 20_000)),
        2 => "😀".repeat(rng.range(0, 3000)),
        3..=7 => rng.string(60),
        8..=10 => rng.string_from(
            &[
                "a", "\n", "\r\n", "\t", "\u{0}", "\u{85}", "\u{2028}", "\u{2029}", "\u{202e}",
                "\u{2066}", "😀", "é", " ",
            ],
            300,
        ),
        _ => String::new(),
    }
}

fn tab(rng: &mut Rng) -> TabSnapshot {
    TabSnapshot {
        id: TabId::next(),
        title: text(rng),
        url: text(rng),
        loading: rng.one_in(2),
        active: rng.one_in(3),
        content: match rng.below(4) {
            0 => TabContent::Blank,
            1 => TabContent::Internal,
            2 => TabContent::Engine,
            _ => TabContent::Error(text(rng)),
        },
        engine: rng.one_in(2).then(|| text(rng)),
        switching_to: rng.one_in(3).then(|| text(rng)),
    }
}

#[test]
fn fuzz_snapshot_contain() {
    run(
        "fuzz_snapshot_contain",
        iterations(100) / 4,
        Duration::from_secs(2),
        |case| {
            let rng = &mut case.rng;
            let mut snapshot = Snapshot {
                tabs: (0..rng.below(4)).map(|_| tab(rng)).collect(),
                can_go_back: rng.one_in(2),
                can_go_forward: rng.one_in(2),
                status: rng.one_in(2).then(|| text(rng)),
                keymap: rng.one_in(2).then(|| text(rng)),
                fullscreen: rng.one_in(2),
                find: rng.one_in(2).then(|| FindSnapshot {
                    text: text(rng),
                    ..FindSnapshot::default()
                }),
                home: HomeInfo {
                    config_path: text(rng),
                    engines: (0..rng.below(3))
                        .map(|_| EngineRow {
                            name: text(rng),
                            detail: text(rng),
                            available: rng.one_in(2),
                        })
                        .collect(),
                },
            };
            case.note(|| format!("{} tabs", snapshot.tabs.len()));
            snapshot.contain();
            let check = |what: &str, s: &str, max: usize| {
                assert!(
                    s.chars().count() <= max,
                    "{what}: {} characters of {max}",
                    s.chars().count()
                );
                assert!(!s.chars().any(offends), "{what}: {s:?}");
            };
            for tab in &snapshot.tabs {
                check("title", &tab.title, 200);
                check("url", &tab.url, 32 * 1024);
                for engine in [&tab.engine, &tab.switching_to].into_iter().flatten() {
                    check("engine", engine, 100);
                }
                if let TabContent::Error(message) = &tab.content {
                    check("error", message, 4000);
                }
            }
            if let Some(status) = &snapshot.status {
                check("status", status, 2000);
            }
            if let Some(keymap) = &snapshot.keymap {
                check("keymap", keymap, 64);
            }
            if let Some(find) = &snapshot.find {
                check("find", &find.text, 1024);
            }
            check("config path", &snapshot.home.config_path, 1000);
            for row in &snapshot.home.engines {
                check("engine name", &row.name, 100);
                check("engine detail", &row.detail, 500);
            }
            let again = snapshot.clone();
            snapshot.contain();
            assert_eq!(snapshot, again, "containing is idempotent");
        },
    );
}

#[test]
fn fuzz_contained() {
    run(
        "fuzz_contained",
        iterations(100) * 3,
        Duration::from_secs(2),
        |case| {
            let s = text(&mut case.rng);
            let max = *case.rng.pick(&[0, 1, 2, 5, 64, 200, 2000]);
            case.note(|| format!("max {max}: {s:?}"));
            let out = contained(&s, max);
            assert!(out.chars().count() <= max);
            assert!(!out.chars().any(offends), "{out:?}");
            assert_eq!(contained(&out, max), out, "idempotent");
        },
    );
}

/// `url` with the hex digits of each `%xx` escape in upper case.
fn upper_hex_escapes(url: &str) -> String {
    let bytes = url.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            out.push('%');
            out.push(char::from(bytes[i + 1].to_ascii_uppercase()));
            out.push(char::from(bytes[i + 2].to_ascii_uppercase()));
            i += 3;
        } else {
            let c = url[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

fn url_piece(rng: &mut Rng) -> String {
    const PIECES: &[&str] = &[
        "%",
        "%%",
        "%2",
        "%2F",
        "%2f",
        "%20",
        "%00",
        "%0A",
        "%7F",
        "%41",
        "%C3%A4",
        "%c3%a4",
        "%C3",
        "%A4",
        "%E2%80%AE",
        "%E2%80%8B",
        "%EF%BB%BF",
        "%F0%9F%94%92",
        "%F0%9F%98%80",
        "%ED%A0%80",
        "%C0%80",
        "%FF",
        "%F4%90%80%80",
        "%E2%82%AC",
        "%E3%80%80",
        "%C2%A0",
        "%E2%81%A6",
        "a",
        "b",
        "/",
        "?",
        "#",
        "=",
        "&",
        ":",
        "@",
        "..",
        ".",
        "%zz",
        "%2g",
        "é",
        "日本",
        "ü",
        "\u{202e}",
        "\u{200b}",
        "\u{2028}",
        " ",
        "\t",
    ];
    let mut s = String::new();
    for _ in 0..rng.range(0, 12) {
        s.push_str(rng.pick(PIECES));
    }
    s
}

fn url_text(rng: &mut Rng) -> String {
    let scheme = *rng.pick(&[
        "https", "http", "file", "ftp", "mailto", "data", "sig", "about", "foo", "a+b-c.d",
        "HTTPS", "", "1x", "é",
    ]);
    let authority = *rng.pick(&[
        "",
        "//example.com",
        "//ex%61mple.com",
        "//user:pw@host:8080",
        "//[::1]",
        "//日本.jp",
        "//",
        "///",
    ]);
    let mut url = format!("{scheme}:{authority}/{}", url_piece(rng));
    if rng.one_in(2) {
        url.push('?');
        url.push_str(&url_piece(rng));
    }
    if rng.one_in(2) {
        url.push('#');
        url.push_str(&url_piece(rng));
    }
    if rng.one_in(8) {
        url = rng.mutate(&url);
    }
    url
}

#[test]
fn fuzz_readable_url() {
    run(
        "fuzz_readable_url",
        iterations(100) * 4,
        Duration::from_secs(2),
        |case| {
            let raw = url_text(&mut case.rng);
            case.note(|| format!("{raw:?}"));
            // Any text at all is handled.
            let shown = readable_url(&raw);
            assert!(shown.len() <= raw.len() * 3 + 3);
            // A URL as the core hands it over (parsed, so normalized) reads back as itself.
            let Ok(parsed) = url::Url::parse(&raw) else {
                return;
            };
            let committed = parsed.as_str();
            let shown = readable_url(committed);
            case.note(|| format!("{committed:?} shown as {shown:?}"));
            // What it decoded (a character the committed address did not hold as it is) is neither
            // invisible nor reordering. (A `data:` address may hold a raw space of its own.)
            for c in shown.chars().filter(|c| !committed.contains(*c)) {
                assert!(
                    !(c.is_control() || c.is_whitespace() || offends(c)),
                    "{c:?} is shown decoded in {shown:?}"
                );
                assert!(
                    !matches!(c, '\u{ad}' | '\u{34f}' | '\u{200b}'..='\u{200f}' | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{fe00}'..='\u{fe0f}' | '\u{1f510}' | '\u{1f512}' | '\u{1f513}' | '\u{e0000}'..='\u{e0fff}'),
                    "{c:?} is shown decoded in {shown:?}"
                );
            }
            let back = url::Url::parse(&shown)
                .unwrap_or_else(|e| panic!("shown form {shown:?} does not parse: {e}"));
            // (An escape it decoded comes back in upper-case hex, as `readable_url` documents.)
            assert_eq!(
                upper_hex_escapes(back.as_str()),
                upper_hex_escapes(committed),
                "typed back, a different address"
            );
            // Showing it again changes nothing.
            assert_eq!(readable_url(&shown), shown, "idempotent");
        },
    );
}

/// Found by `fuzz_contained`: U+2028 and U+2029 were kept, and a text shaper breaks a line at
/// them.
#[test]
fn line_and_paragraph_separators_are_spaces() {
    assert_eq!(contained("a\u{2028}b\u{2029}c", 10), "a b c");
}

/// Everything the address editor keeps is within its bounds and on a cluster boundary, whatever
/// is typed, pasted, deleted or composed.
fn check_editor(editor: &TextEditor) {
    let text = editor.text();
    assert!(text.len() <= MAX_TEXT, "{} bytes", text.len());
    assert!(!text.chars().any(char::is_control), "{text:?}");
    let boundaries: Vec<usize> = text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    let cursor = editor.cursor();
    let selection = editor.selection();
    assert!(
        boundaries.contains(&cursor),
        "cursor {cursor} is inside a cluster of {text:?}"
    );
    assert!(
        boundaries.contains(&selection.start) && boundaries.contains(&selection.end),
        "selection {selection:?} of {text:?}"
    );
    assert!(selection.start <= selection.end && selection.end <= text.len());
    let shown = editor.display();
    assert!(
        shown.selection.end <= shown.text.len()
            && shown.text.is_char_boundary(shown.selection.start)
            && shown.text.is_char_boundary(shown.selection.end)
    );
    if let Some(caret) = shown.caret {
        assert!(
            caret <= shown.text.len() && shown.text.is_char_boundary(caret),
            "caret {caret}"
        );
    }
    if let Some(range) = &shown.preedit {
        assert!(
            range.end <= shown.text.len()
                && shown.text.is_char_boundary(range.start)
                && shown.text.is_char_boundary(range.end)
        );
    }
    let around = editor.surrounding();
    assert!(around.text.len() <= 4000);
    assert!(around.cursor <= around.text.len() && around.text.is_char_boundary(around.cursor));
    assert!(around.anchor <= around.text.len() && around.text.is_char_boundary(around.anchor));
    assert!(
        text.contains(&around.text),
        "surrounding text is part of the text"
    );
    assert!(editor.selected_text().is_none_or(|t| text.contains(t)));
}

#[test]
fn fuzz_address_editor() {
    run(
        "fuzz_text_editor",
        iterations(100) / 4,
        Duration::from_secs(5),
        |case| {
            let mut editor = TextEditor::default();
            let pieces = [
                "a",
                "é",
                "e\u{301}",
                "日本",
                "😀",
                "👩\u{200d}👩\u{200d}👧",
                "🇸🇪",
                "\u{200d}",
                "\u{301}",
                "\n",
                "\t",
                "\r\n",
                "\0",
                "https://",
                "x y",
                "  ",
                "\u{202e}",
                "한",
                "\u{fe0f}",
            ];
            let mut generator = case.rng.clone();
            let rng = &mut generator;
            for step in 0..rng.range(1, 40) {
                let piece = if rng.one_in(8) {
                    rng.string(6)
                } else {
                    rng.string_from(&pieces, 4)
                };
                case.note(|| {
                    format!(
                        "step {step}: {piece:?} on {:?} cursor {} selection {:?}",
                        editor.text(),
                        editor.cursor(),
                        editor.selection()
                    )
                });
                match rng.below(16) {
                    0..=2 => editor.insert(&piece),
                    3 => editor.paste(&piece),
                    4 => editor.set_text(&piece),
                    5 => editor.select_all(),
                    6 => editor.move_to(rng.below(editor.text().len() + 5), rng.one_in(2)),
                    7 => editor.move_grapheme(rng.one_in(2), rng.one_in(2)),
                    8 => editor.move_word(rng.one_in(2), rng.one_in(2)),
                    9 => editor.delete_grapheme(rng.one_in(2)),
                    10 => editor.delete_word(rng.one_in(2)),
                    11 => {
                        let len = rng.below(8);
                        let text = rng.string(len);
                        let cursor = rng.one_in(2).then(|| {
                            let a = rng.below(text.len() + 4);
                            a..a + rng.below(6)
                        });
                        editor.input_method(&TextInputEvent {
                            delete_surrounding: rng.one_in(2).then(|| DeleteSurrounding {
                                before: rng.below(10) as u32,
                                after: rng.below(10) as u32,
                            }),
                            commit: rng.one_in(2).then(|| piece.clone()),
                            preedit: rng.one_in(2).then_some(Preedit { text, cursor }),
                        });
                    }
                    12 => editor.clear_preedit(),
                    13 if rng.one_in(8) => editor.insert(&"x".repeat(rng.range(0, 40_000))),
                    13 => editor.insert(&"yé".repeat(rng.range(0, 30))),
                    14 => editor.scroll_to_caret(
                        rng.below(5000) as f32,
                        2.0,
                        rng.below(6000) as f32,
                        rng.below(800) as f32,
                    ),
                    _ => editor.reset_scroll(),
                }
                check_editor(&editor);
            }
        },
    );
}
