//! Seeded random-input tests for the API's parsers: commands and keystrokes. They hold
//! three things for any text: no panic, a return within the watchdog's limit, and a round
//! trip (what parses formats to a string that parses to the same value).
//! `SIG_FUZZ_ITERATIONS`, `SIG_FUZZ_SEED` and `SIG_FUZZ_CASE` are in `tools/fuzz/mod.rs`.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::time::Duration;

use fuzz::{iterations, run, Rng};
use sighurt_api::commands::{Command, CommandSet, SentKey, TabMove, MAX_FIND};
use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, Modifiers};
use sighurt_api::keymap::Chord;

const LIMIT: Duration = Duration::from_secs(2);

const KEY_WORDS: &[&str] = &[
    "ctrl",
    "alt",
    "shift",
    "super",
    "altgr",
    "secondary",
    "-",
    "--",
    "code:",
    "KeyQ",
    "KeyA",
    "Enter",
    "enter",
    "escape",
    "esc",
    "space",
    "tab",
    "f1",
    "f5",
    "f24",
    "f25",
    "f0",
    "F5",
    "left",
    "down",
    "pageup",
    "menu",
    "BrowserBack",
    "AudioVolumeUp",
    "a",
    "A",
    "z",
    "Z",
    "1",
    "+",
    "=",
    "ä",
    "Ä",
    "ß",
    "İ",
    "ǅ",
    " ",
    "\u{a0}",
    "\t",
    "",
    "code:Digit1",
    "code:Nope",
    "ctrl-shift-t",
    "ctrl--",
    "ctrl-+",
    "Ctrl",
    "SHIFT",
];

fn keystroke(rng: &mut Rng) -> String {
    let mut text = String::new();
    for _ in 0..rng.range(0, 5) {
        text.push_str(rng.pick(KEY_WORDS));
        if rng.below(4) != 0 {
            text.push('-');
        }
    }
    if rng.one_in(3) {
        text.push_str(&rng.string(4));
    }
    text
}

fn argument(rng: &mut Rng) -> String {
    match rng.below(10) {
        0 => rng
            .pick(&[
                "0", "1", "2", "99", "-1", "+3", "1_0", "0x10", "٣", " 4 ", "1e3",
            ])
            .to_string(),
        1 => rng.next_u64().to_string(),
        2 => format!("{}{}", rng.next_u64(), rng.next_u64()),
        3 => keystroke(rng),
        4 => rng
            .pick(&[
                "servo",
                "wpe",
                "cef",
                "Servo",
                "1servo",
                "net-surf",
                "x y",
                "servo https://a.b/c d",
            ])
            .to_string(),
        5 => rng.string_from(
            &["example.com", "/", "?", "#", "%", " ", "\t", "\u{a0}", "a"],
            12,
        ),
        6 => format!("https://{}", rng.string(30)),
        7 => String::new(),
        _ => rng.string(24),
    }
}

fn command_text(rng: &mut Rng) -> String {
    let mut text = match rng.below(6) {
        0 => rng.string(30),
        1 => format!(
            "chrome.{}",
            rng.string_from(&["a", "b", "-", "1", "toggle", "Z", "é"], 12)
        ),
        _ => {
            let name = *rng.pick(&Command::NAMES);
            let separator = *rng.pick(&[" ", "  ", "\t", "\n", "\u{a0}", "\u{2003}", ""]);
            format!("{name}{separator}{}", argument(rng))
        }
    };
    if rng.one_in(3) {
        text = rng.mutate(&text);
    }
    if rng.one_in(8) {
        text = format!(
            "{}{text}{}",
            rng.pick(&[" ", "\n", "\u{a0}", "\t"]),
            rng.pick(&[" ", "\r\n", ""])
        );
    }
    text
}

#[test]
fn fuzz_commands_parse_and_round_trip() {
    run("fuzz_commands", iterations(100) * 5, LIMIT, |case| {
        let text = command_text(&mut case.rng);
        case.note(|| format!("{text:?}"));
        let Ok(command) = text.parse::<Command>() else {
            return;
        };
        let shown = command.to_string();
        case.note(|| format!("{text:?} parsed to {command:?}, shown as {shown:?}"));
        assert_eq!(
            shown.parse::<Command>().as_ref(),
            Ok(&command),
            "round trip"
        );
        assert_eq!(
            shown.parse::<Command>().unwrap().to_string(),
            shown,
            "canonical"
        );
        assert_eq!(CommandSet::ANY.parse(&text), Ok(command));
    });
}

/// An argument for the commands added with find, the tab commands and `location.focus`: text
/// around `MAX_FIND` (a multi-byte character across the limit), whitespace and control
/// characters, `tab.move`'s words and numbers at their edges.
fn new_argument(rng: &mut Rng) -> String {
    match rng.below(8) {
        0 => "x".repeat(rng.range(MAX_FIND - 2, MAX_FIND + 2)),
        1 => format!("{}é", "a".repeat(rng.range(MAX_FIND - 3, MAX_FIND))),
        2 => rng.string_from(
            &["a", " ", "\t", "\n", "\u{1b}", "\0", "\u{85}", "\u{a0}"],
            12,
        ),
        3 => rng
            .pick(&[
                "left",
                "right",
                "LEFT",
                "Left",
                "0",
                "1",
                "-1",
                "+2",
                "4294967296",
                "18446744073709551616",
                "1.5",
                "left right",
            ])
            .to_string(),
        4 => format!(":{}", rng.string(10)),
        5 => rng.next_u64().to_string(),
        _ => rng.string(30),
    }
}

/// `page.find`, `tab.move`, `tab.new`, `location.focus` and the argument-less new commands:
/// they parse as their documentation says, round trip, and `page.find` refuses exactly the
/// text over `MAX_FIND` bytes.
#[test]
fn fuzz_the_new_commands() {
    const NEW: [&str; 11] = [
        "page.find",
        "page.find-next",
        "page.find-previous",
        "page.find-stop",
        "tab.move",
        "tab.new",
        "tab.duplicate",
        "tab.reopen",
        "location.focus",
        "browser.fullscreen",
        "page.exit-fullscreen",
    ];
    run("fuzz_new_commands", iterations(100) * 2, LIMIT, |case| {
        let name = *case.rng.pick(&NEW);
        let argument = new_argument(&mut case.rng);
        let separator = *case.rng.pick(&[" ", "\t", "  "]);
        let text = format!("{name}{separator}{argument}");
        case.note(|| format!("{text:?}"));
        let parsed = text.parse::<Command>();
        let argument = argument.trim();
        if name == "page.find" {
            match &parsed {
                Ok(Command::PageFind(Some(found))) => {
                    assert!(found.len() <= MAX_FIND);
                    assert_eq!(found, argument);
                }
                Ok(Command::PageFind(None)) => assert!(argument.is_empty()),
                Ok(other) => panic!("{other:?}"),
                Err(_) => assert!(
                    argument.len() > MAX_FIND,
                    "refused {} bytes",
                    argument.len()
                ),
            }
        }
        if let Ok(Command::TabMove(to)) = &parsed {
            match to {
                TabMove::Left => assert_eq!(argument, "left"),
                TabMove::Right => assert_eq!(argument, "right"),
                TabMove::To(n) => {
                    assert_eq!(argument.parse::<std::num::NonZeroUsize>().ok(), Some(*n))
                }
            }
        }
        let Ok(command) = parsed else {
            return;
        };
        assert_eq!(command.name(), name);
        let shown = command.to_string();
        assert_eq!(
            shown.parse::<Command>().as_ref(),
            Ok(&command),
            "round trip"
        );
    });
}

#[test]
fn fuzz_command_set_with_chrome_actions() {
    run("fuzz_command_set", iterations(100), LIMIT, |case| {
        let text = command_text(&mut case.rng);
        case.note(|| format!("{text:?}"));
        let set = CommandSet::new("standard", &["toggle-tabs", "focus"]);
        let _ = set.parse(&text);
    });
}

#[test]
fn fuzz_chords_parse_and_round_trip() {
    run("fuzz_chords", iterations(100) * 5, LIMIT, |case| {
        let mut text = keystroke(&mut case.rng);
        if case.rng.one_in(3) {
            text = case.rng.mutate(&text);
        }
        case.note(|| format!("{text:?}"));
        let Ok(chord) = text.parse::<Chord>() else {
            return;
        };
        let shown = chord.to_string();
        case.note(|| format!("{text:?} parsed to {chord:?}, shown as {shown:?}"));
        assert_eq!(shown.parse::<Chord>().as_ref(), Ok(&chord), "round trip");
        // Matching any event never panics.
        let event = KeyEvent {
            code: KeyCode(case.rng.below(400) as u32),
            key: Key::Character(case.rng.string(3)),
            state: ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers {
                shift: case.rng.one_in(2),
                control: case.rng.one_in(2),
                alt: case.rng.one_in(2),
                alt_graph: case.rng.one_in(2),
                logo: case.rng.one_in(2),
                caps_lock: case.rng.one_in(2),
                num_lock: case.rng.one_in(2),
            },
            consumed: Modifiers::default(),
            text: None,
        };
        let _ = chord.matches(&event);
        let _ = chord.types();
    });
}

#[test]
fn fuzz_sent_keys_round_trip() {
    run("fuzz_sent_keys", iterations(100) * 3, LIMIT, |case| {
        let text = keystroke(&mut case.rng);
        case.note(|| format!("{text:?}"));
        let Ok(key) = text.parse::<SentKey>() else {
            return;
        };
        let shown = key.to_string();
        case.note(|| format!("{text:?} parsed to {key:?}, shown as {shown:?}"));
        assert_eq!(shown.parse::<SentKey>().as_ref(), Ok(&key), "round trip");
        let _ = key.event(ButtonState::Pressed);
        let _ = key.event(ButtonState::Released);
    });
}

/// Every command in the vocabulary, with arguments, survives a round trip.
#[test]
fn every_name_round_trips_with_arguments() {
    for name in Command::NAMES {
        for argument in [
            "",
            "1",
            "servo",
            "servo https://a.example/ b",
            "down",
            "shift-space",
            "toggle",
        ] {
            let text = format!("{name} {argument}");
            if let Ok(command) = text.parse::<Command>() {
                assert_eq!(
                    command.to_string().parse::<Command>(),
                    Ok(command),
                    "{text:?}"
                );
            }
        }
    }
}

/// Found by `fuzz_chords_parse_and_round_trip`: "İ" lowercases to two characters, so the
/// parsed keystroke formatted as "shift-i\u{307}", which no longer parsed.
#[test]
fn a_letter_whose_lower_case_is_two_characters_round_trips() {
    let chord: Chord = "İ".parse().unwrap();
    assert_eq!(chord.to_string().parse::<Chord>(), Ok(chord));
}
