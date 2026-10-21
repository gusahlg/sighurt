//! The modal keymap driven as the core drives it, through `KIND` and the `Keymap` trait only.

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers};
use sighurt_api::keymap::{KeyAction, KeyContext, Keymap};
use sighurt_api::settings::{Table, Value};
use sighurt_keymap_modal::KIND;

/// extension-api.md's example table (§2.5), with a chrome command from §1.2.
const VIM: &str = r#"
start = "normal"
typing = "insert"
pass = ["insert"]

[always]
"ctrl-q" = "browser.quit"

[normal]
"j" = "page.send-key down"
"k" = "page.send-key up"
"d" = "page.send-key pagedown"
"g g" = "page.send-key home"
"G" = "page.send-key end"
"H" = "page.back"
"J" = "tab.previous"
"g t" = "tab.next"
"g T" = "tab.previous"
"g 0" = "tab.select 1"
"g $" = "tab.last"
"t" = "tab.new"
"x" = "tab.close"
"o" = "location.focus"
":" = { run = "location.focus", pass = true }
"i" = "mode insert"
"T" = "chrome.toggle-tabs"

[insert]
"escape" = "mode normal"
"#;

/// The standard chrome's commands.
const STANDARD: CommandSet<'static> = CommandSet::ANY;

fn settings(toml: &str) -> Table {
    fn convert(value: toml::Value) -> Value {
        match value {
            toml::Value::String(text) => Value::String(text),
            toml::Value::Integer(n) => Value::Integer(n),
            toml::Value::Float(n) => Value::Float(n),
            toml::Value::Boolean(b) => Value::Boolean(b),
            toml::Value::Array(values) => Value::Array(values.into_iter().map(convert).collect()),
            toml::Value::Table(table) => Value::Table(table_of(table)),
            toml::Value::Datetime(_) => unreachable!("the core refuses dates"),
        }
    }
    fn table_of(table: toml::Table) -> Table {
        table.into_iter().map(|(k, v)| (k, convert(v))).collect()
    }
    table_of(toml.parse().expect("TOML"))
}

fn problems(toml: &str) -> Vec<String> {
    let commands = CommandSet::new("standard", &["toggle-tabs"]);
    (KIND.check)(&settings(toml), &commands)
}

/// The keymap, configured with `toml`, which its check accepts.
fn modal(toml: &str) -> Box<dyn Keymap> {
    assert_eq!(problems(toml), Vec::<String>::new());
    let mut keymap = (KIND.new)();
    keymap.configure(&settings(toml));
    keymap
}

const PAGE: KeyContext = KeyContext::new(KeyboardFocus::Page, false);
const FIELD: KeyContext = KeyContext::new(KeyboardFocus::Page, true);
const CHROME: KeyContext = KeyContext::new(KeyboardFocus::Chrome, true);
const SHIFT: Modifiers = Modifiers {
    shift: true,
    control: false,
    alt: false,
    alt_graph: false,
    logo: false,
    caps_lock: false,
    num_lock: false,
};
const CTRL: Modifiers = Modifiers {
    shift: false,
    control: true,
    ..SHIFT
};

/// The press of a key that types `c` (with Shift for an upper-case letter), as xkb reports it.
fn typed(c: char) -> KeyEvent {
    let shifted = c.is_uppercase() || "$:".contains(c);
    let modifiers = if shifted { SHIFT } else { Modifiers::default() };
    KeyEvent {
        code: KeyCode(u32::from(c)),
        key: Key::Character(c.into()),
        state: ButtonState::Pressed,
        repeat: false,
        modifiers,
        consumed: modifiers,
        text: Some(c.into()),
    }
}

fn named(name: &str, code: u32) -> KeyEvent {
    KeyEvent {
        code: KeyCode(code),
        key: Key::Named(name.into()),
        text: None,
        consumed: Modifiers::default(),
        ..typed('x')
    }
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent {
        modifiers: CTRL,
        consumed: Modifiers::default(),
        text: None,
        ..typed(c)
    }
}

fn repeat(key: &KeyEvent) -> KeyEvent {
    KeyEvent {
        repeat: true,
        ..key.clone()
    }
}

fn run(text: &str) -> KeyAction {
    KeyAction::Run(text.parse().unwrap())
}

fn press(keymap: &mut Box<dyn Keymap>, keys: &str, context: KeyContext) -> Vec<KeyAction> {
    keys.chars()
        .map(|c| keymap.press(&typed(c), context))
        .collect()
}

#[test]
fn the_example_table_is_valid() {
    assert_eq!(problems(VIM), Vec::<String>::new());
    // The chrome command is checked against the chrome the configuration selects.
    let line = CommandSet::new("line", &[]);
    let problems = (KIND.check)(&settings(VIM), &line);
    assert_eq!(
        problems,
        ["normal \"T\": the line chrome has no command chrome.toggle-tabs; it has none"]
    );
}

#[test]
fn modes_switch_and_show_in_the_indicator() {
    let mut keymap = modal(VIM);
    assert_eq!(keymap.indicator(), Some("NORMAL"));
    assert_eq!(keymap.press(&typed('i'), PAGE), KeyAction::Take);
    assert_eq!(keymap.indicator(), Some("INSERT"));
    // Insert passes what it doesn't bind: j types.
    assert_eq!(keymap.press(&typed('j'), PAGE), KeyAction::Pass);
    let escape = named("Escape", 1);
    assert_eq!(keymap.press(&escape, PAGE), KeyAction::Take);
    assert_eq!(keymap.indicator(), Some("NORMAL"));
    assert_eq!(keymap.press(&typed('j'), PAGE), run("page.send-key down"));
}

#[test]
fn sequences_wait_for_their_last_key() {
    let mut keymap = modal(VIM);
    assert_eq!(keymap.press(&typed('g'), PAGE), KeyAction::Take);
    assert_eq!(keymap.indicator(), Some("NORMAL g"));
    assert_eq!(keymap.press(&typed('t'), PAGE), run("tab.next"));
    assert_eq!(keymap.indicator(), Some("NORMAL"));
    assert_eq!(
        press(&mut keymap, "gg", PAGE),
        [KeyAction::Take, run("page.send-key home")]
    );
    assert_eq!(press(&mut keymap, "g$", PAGE)[1], run("tab.last"));
    assert_eq!(press(&mut keymap, "g0", PAGE)[1], run("tab.select 1"));
}

#[test]
fn a_key_that_doesnt_continue_a_sequence_starts_afresh() {
    let mut keymap = modal(VIM);
    // g then j: the g is dropped, and j is j.
    assert_eq!(
        press(&mut keymap, "gj", PAGE),
        [KeyAction::Take, run("page.send-key down")]
    );
    assert_eq!(keymap.indicator(), Some("NORMAL"));
    // g then an unbound key that types: swallowed, as in normal mode it would be anyway.
    assert_eq!(
        press(&mut keymap, "gz", PAGE),
        [KeyAction::Take, KeyAction::Take]
    );
    assert_eq!(keymap.indicator(), Some("NORMAL"));
    // g then an arrow: the arrow passes, and the sequence is gone.
    keymap.press(&typed('g'), PAGE);
    assert_eq!(
        keymap.press(&named("ArrowDown", 108), PAGE),
        KeyAction::Pass
    );
    assert_eq!(keymap.press(&typed('t'), PAGE), run("tab.new"));
    // g, then g t: the second g starts the sequence afresh.
    assert_eq!(
        press(&mut keymap, "ggt", PAGE),
        [KeyAction::Take, run("page.send-key home"), run("tab.new")]
    );
}

#[test]
fn modifiers_and_dead_keys_are_not_keys_of_a_sequence() {
    let mut keymap = modal(VIM);
    keymap.press(&typed('g'), PAGE);
    let shift = named("Shift", 42);
    assert_eq!(keymap.press(&shift, PAGE), KeyAction::Pass);
    let dead = KeyEvent {
        key: Key::Dead(Some('¨')),
        ..named("Dead", 27)
    };
    assert_eq!(keymap.press(&dead, PAGE), KeyAction::Pass);
    assert_eq!(keymap.indicator(), Some("NORMAL g"), "still waiting");
    assert_eq!(keymap.press(&typed('T'), PAGE), run("tab.previous"));
}

#[test]
fn unbound_keys_are_swallowed_or_passed() {
    let mut keymap = modal(VIM);
    // Normal mode: what would type is swallowed; arrows, Enter and ctrl-c pass.
    assert_eq!(keymap.press(&typed('z'), PAGE), KeyAction::Take);
    assert_eq!(
        keymap.press(&named("ArrowDown", 108), PAGE),
        KeyAction::Pass
    );
    assert_eq!(keymap.press(&named("Enter", 28), PAGE), KeyAction::Pass);
    assert_eq!(keymap.press(&ctrl('c'), PAGE), KeyAction::Pass);
    // Bound everywhere: always.
    assert_eq!(keymap.press(&ctrl('q'), PAGE), run("browser.quit"));
    // Insert passes everything it doesn't bind.
    keymap.press(&typed('i'), PAGE);
    assert_eq!(keymap.press(&typed('z'), PAGE), KeyAction::Pass);
    assert_eq!(keymap.press(&ctrl('q'), PAGE), run("browser.quit"));
}

#[test]
fn a_text_field_brings_the_typing_mode_and_leaving_it_the_start() {
    let mut keymap = modal(VIM);
    keymap.context(FIELD);
    assert_eq!(keymap.indicator(), Some("INSERT"));
    assert_eq!(keymap.press(&typed('j'), FIELD), KeyAction::Pass, "j types");
    // Escape leaves insert mode; the field keeps the keyboard, and j scrolls (moves the caret).
    keymap.press(&named("Escape", 1), FIELD);
    assert_eq!(keymap.press(&typed('j'), FIELD), run("page.send-key down"));
    keymap.press(&typed('i'), FIELD);
    keymap.context(PAGE);
    assert_eq!(
        keymap.indicator(),
        Some("NORMAL"),
        "the field lost the keyboard"
    );
    // The chrome taking the keyboard in between changes no mode.
    keymap.context(FIELD);
    keymap.context(CHROME);
    keymap.context(FIELD);
    assert_eq!(keymap.indicator(), Some("INSERT"));
    // Without `typing`, text fields change nothing.
    let mut keymap = modal(&VIM.replace("typing = \"insert\"", ""));
    keymap.context(FIELD);
    assert_eq!(keymap.indicator(), Some("NORMAL"));
}

#[test]
fn while_the_chrome_has_the_keyboard_only_always_applies() {
    let always = "[always]\n\"%\" = \"page.reload\"\n\"ctrl-g z\" = { run = \"tab.new\" }\n";
    let mut keymap = modal(&VIM.replace("[always]\n", always));
    keymap.press(&typed('g'), PAGE);
    keymap.context(CHROME);
    assert_eq!(
        keymap.indicator(),
        Some("NORMAL"),
        "the context drops the sequence"
    );
    // The line takes what types: keys normal mode binds, and one always binds.
    for c in "jgtx:%".chars() {
        assert_eq!(keymap.press(&typed(c), CHROME), KeyAction::Pass, "{c}");
    }
    assert_eq!(keymap.press(&named("Escape", 1), CHROME), KeyAction::Pass);
    assert_eq!(keymap.press(&ctrl('q'), CHROME), run("browser.quit"));
    assert_eq!(keymap.press(&ctrl('g'), CHROME), KeyAction::Take);
    assert_eq!(keymap.press(&typed('z'), CHROME), run("tab.new"));
    keymap.context(PAGE);
    assert_eq!(keymap.press(&typed('j'), PAGE), run("page.send-key down"));
    assert_eq!(keymap.press(&typed('%'), PAGE), run("page.reload"));
}

#[test]
fn run_then_pass_lets_the_key_through_after_its_command() {
    let mut keymap = modal(VIM);
    let colon = keymap.press(&typed(':'), PAGE);
    assert_eq!(
        colon,
        KeyAction::RunThenPass(Command::LocationFocus),
        "the line opens and gets the colon"
    );
    let off = VIM.replace(
        "{ run = \"location.focus\", pass = true }",
        "{ run = \"location.focus\", pass = false }",
    );
    let mut keymap = modal(&off);
    assert_eq!(keymap.press(&typed(':'), PAGE), run("location.focus"));
}

#[test]
fn a_held_key_repeats_a_single_key_command_only() {
    let mut keymap = modal(VIM);
    let j = typed('j');
    assert_eq!(keymap.press(&j, PAGE), run("page.send-key down"));
    assert_eq!(keymap.press(&repeat(&j), PAGE), run("page.send-key down"));
    assert_eq!(keymap.press(&repeat(&j), PAGE), run("page.send-key down"));
    // The last key of a sequence, a mode switch, or a swallowed key: held quietly.
    keymap.press(&typed('g'), PAGE);
    let t = typed('t');
    assert_eq!(keymap.press(&t, PAGE), run("tab.next"));
    assert_eq!(keymap.press(&repeat(&t), PAGE), KeyAction::Take);
    let z = typed('z');
    keymap.press(&z, PAGE);
    assert_eq!(keymap.press(&repeat(&z), PAGE), KeyAction::Take);
    // A repeat of a key it took long ago doesn't run what another key ran since.
    keymap.press(&j, PAGE);
    keymap.press(&typed('i'), PAGE);
    assert_eq!(keymap.press(&repeat(&j), PAGE), KeyAction::Take);
    // A modifier pressed while j repeats leaves its repeats alone.
    keymap.press(&named("Escape", 1), PAGE);
    keymap.press(&j, PAGE);
    keymap.press(&named("Shift", 42), PAGE);
    assert_eq!(keymap.press(&repeat(&j), PAGE), run("page.send-key down"));
}

#[test]
fn the_core_ends_unfinished_sequences() {
    let mut keymap = modal(VIM);
    keymap.press(&typed('g'), PAGE);
    keymap.reset();
    assert_eq!(keymap.indicator(), Some("NORMAL"));
    assert_eq!(keymap.press(&typed('t'), PAGE), run("tab.new"));
    keymap.press(&typed('g'), PAGE);
    keymap.context(PAGE);
    assert_eq!(keymap.press(&typed('t'), PAGE), run("tab.new"));
}

#[test]
fn a_reload_keeps_the_mode_while_it_exists() {
    let mut keymap = modal(VIM);
    keymap.press(&typed('i'), PAGE);
    keymap.configure(&settings(VIM));
    assert_eq!(keymap.indicator(), Some("INSERT"));
    let renamed = VIM
        .replace("[insert]", "[edit]")
        .replace("\"insert\"", "\"edit\"")
        .replace("mode insert", "mode edit");
    keymap.configure(&settings(&renamed));
    assert_eq!(keymap.indicator(), Some("NORMAL"), "back to start");
}

#[test]
fn every_problem_is_reported() {
    let problems = problems(
        r#"
        typing = "insrt"
        pass = ["insert", 3]
        bogus = 1

        [always]
        "g q" = "browser.quit"
        "ctrl-w" = "tab.close"

        [normal]
        "g" = "tab.next"
        "g t" = "tab.next"
        "ctrl-T" = "tab.new"
        "ctrl-shift-t" = "tab.new"
        "ctrl-w v" = "tab.new"
        "a b c d e" = "tab.new"
        "hyper-x" = "tab.new"
        "y" = "tab.nope"
        "Y" = "chrome.zoom"
        "z" = "mode nope"
        "Z" = 1
        "q" = { run = "tab.new", passes = true }
        "Q" = { pass = true }

        [insert]
        "j k" = "mode normal"
        "ctrl-j k" = "mode normal"

        [Shout]
        "#,
    );
    let expected = [
        "start: missing",
        "typing: there is no mode \"insrt\"",
        "pass: expected modes, not a whole number",
        "bogus: expected start, typing, pass or a mode's table",
        "[keymap.modal.Shout]: a mode's name is",
        // Prefixes and duplicates, within a mode and with always.
        "normal \"g\" is the start of normal \"g t\"",
        "normal \"g\" is the start of always \"g q\"",
        "normal \"ctrl-T\" are the same keys as normal \"ctrl-shift-t\"",
        "always \"ctrl-w\" is the start of normal \"ctrl-w v\"",
        // Sequences that start with a key that types, where keys pass.
        "always \"g q\": keys pass to the page here",
        "insert \"j k\": keys pass to the page here",
        "normal \"a b c d e\": a binding is 1 to 4 keystrokes",
        "normal \"hyper-x\": unknown modifier",
        "normal \"y\": unknown command \"tab.nope\"",
        "normal \"Y\": the standard chrome has no command chrome.zoom",
        "normal \"z\": there is no mode \"nope\"",
        "normal \"Z\": expected a command, \"mode <name>\"",
        "normal \"q\": passes: unknown key",
        "normal \"Q\": run: missing",
    ];
    for part in expected {
        assert!(
            problems.iter().any(|p| p.contains(part)),
            "{part:?} not in {problems:#?}"
        );
    }
    assert_eq!(problems.len(), expected.len(), "{problems:#?}");
    // The core asks about a missing table too.
    assert_eq!(
        (KIND.check)(&Table::new(), &STANDARD),
        ["start: missing: the mode the keymap starts in"]
    );
}
