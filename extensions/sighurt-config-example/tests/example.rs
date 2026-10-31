//! The example configuration driven as the core drives it: through `KIND` and the `Keymap` trait,
//! with a fake `Browser`.

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::ids::TabId;
use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers};
use sighurt_api::keymap::{Browser, Chord, Frontend, KeyAction, KeyContext, Keymap, TabInfo};
use sighurt_api::settings::{Table, Value};
use sighurt_config_example::{bindings, table, Action, Mode, KIND, MAX_KEYS};

const PAGE: KeyContext = KeyContext::new(KeyboardFocus::Page, false);
const FIELD: KeyContext = KeyContext::new(KeyboardFocus::Page, true);
const CHROME: KeyContext = KeyContext::new(KeyboardFocus::Chrome, true);

const NONE: Modifiers = Modifiers {
    shift: false,
    control: false,
    alt: false,
    alt_graph: false,
    logo: false,
    caps_lock: false,
    num_lock: false,
};
const SHIFT: Modifiers = Modifiers {
    shift: true,
    ..NONE
};
const CTRL: Modifiers = Modifiers {
    control: true,
    ..NONE
};
const ALT: Modifiers = Modifiers { alt: true, ..NONE };

struct Fake {
    frontend: Frontend,
    urls: Vec<Option<&'static str>>,
    active: usize,
}

impl Fake {
    fn new(frontend: Frontend) -> Self {
        Self {
            frontend,
            urls: vec![Some("https://example.com/")],
            active: 0,
        }
    }
    fn tabs(urls: usize, active: usize) -> Self {
        Self {
            frontend: Frontend::Gui,
            urls: vec![None; urls],
            active,
        }
    }
}

impl Browser for Fake {
    fn frontend(&self) -> Frontend {
        self.frontend
    }
    fn context(&self) -> KeyContext {
        PAGE
    }
    fn tab_count(&self) -> usize {
        self.urls.len()
    }
    fn active_tab(&self) -> usize {
        self.active
    }
    fn tab(&self, index: usize) -> Option<TabInfo<'_>> {
        let url = *self.urls.get(index)?;
        Some(TabInfo::new(
            TabId::next(),
            None,
            url,
            false,
            None,
            1.0,
            false,
            false,
        ))
    }
}

fn typed(c: char) -> KeyEvent {
    let shifted = c.is_uppercase() || ":+".contains(c);
    let modifiers = if shifted { SHIFT } else { NONE };
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
        consumed: NONE,
        ..typed('x')
    }
}

fn with(c: char, modifiers: Modifiers) -> KeyEvent {
    KeyEvent {
        modifiers,
        consumed: NONE,
        text: None,
        ..typed(c)
    }
}

fn ctrl(c: char) -> KeyEvent {
    with(c, CTRL)
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

fn escape() -> KeyEvent {
    named("Escape", 1)
}

fn example() -> Box<dyn Keymap> {
    let mut keymap = (KIND.new)();
    keymap.configure(&Table::new());
    keymap
}

fn press(
    keymap: &mut Box<dyn Keymap>,
    keys: &str,
    context: KeyContext,
    browser: &Fake,
) -> Vec<KeyAction> {
    keys.chars()
        .map(|c| keymap.press(&typed(c), context, browser))
        .collect()
}

#[test]
fn the_table_is_valid_in_both_frontends() {
    for frontend in [Frontend::Gui, Frontend::Terminal] {
        let all: Vec<_> = bindings(frontend).collect();
        for (keys, action) in &all {
            let words: Vec<&str> = keys.split_whitespace().collect();
            assert!((1..=MAX_KEYS).contains(&words.len()), "{keys}");
            for word in words {
                word.parse::<Chord>()
                    .unwrap_or_else(|e| panic!("{keys:?}: {e}"));
            }
            if let Action::Run(text) | Action::RunThenPass(text) = action {
                text.parse::<Command>()
                    .unwrap_or_else(|e| panic!("{text:?}: {e}"));
            }
        }
        // No binding is the same keys as, or the start of, another in its mode.
        for (mode, tab) in [("normal", table::NORMAL), ("insert", table::INSERT)] {
            let chords = |keys: &str| -> Vec<Chord> {
                keys.split_whitespace()
                    .map(|k| k.parse().unwrap())
                    .collect()
            };
            let here: Vec<_> = tab
                .iter()
                .filter(|b| all.iter().any(|(k, _)| *k == b.keys))
                .map(|b| (b.keys, chords(b.keys)))
                .collect();
            for (i, (a_text, a)) in here.iter().enumerate() {
                for (b_text, b) in &here[i + 1..] {
                    assert!(
                        !a.starts_with(b) && !b.starts_with(a),
                        "{mode}: {a_text:?} and {b_text:?}"
                    );
                }
            }
        }
    }
    // The constructor parses every command.
    let _ = example();
    // Its table takes no settings.
    let commands = CommandSet::ANY;
    assert_eq!((KIND.check)(&Table::new(), &commands), Vec::<String>::new());
    let mut table = Table::new();
    table.insert("j".into(), Value::String("page.reload".into()));
    let problems = (KIND.check)(&table, &commands);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].starts_with(
        "[keymap.example] takes no settings; the example configuration's bindings are in its crate"
    ));
}

#[test]
fn escape_toggles_insert_and_the_indicator() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    assert_eq!(keymap.indicator(), None);
    assert_eq!(keymap.press(&escape(), PAGE, &b), KeyAction::Take);
    assert_eq!(keymap.indicator(), Some("INSERT MODE"));
    // Insert passes everything else, bound in normal or not, modifiers included.
    for key in [typed('j'), typed('q'), ctrl('o'), with('1', ALT), ctrl('c')] {
        assert_eq!(keymap.press(&key, PAGE, &b), KeyAction::Pass);
    }
    assert_eq!(keymap.press(&typed('y'), PAGE, &b), KeyAction::Pass);
    assert_eq!(keymap.press(&typed('y'), PAGE, &b), KeyAction::Pass);
    assert_eq!(keymap.press(&escape(), PAGE, &b), KeyAction::Take);
    assert_eq!(keymap.indicator(), None);
    assert_eq!(
        keymap.press(&typed('j'), PAGE, &b),
        run("page.send-key down")
    );
    // The mode is the browser's, and survives a reload.
    keymap.press(&escape(), PAGE, &b);
    keymap.configure(&Table::new());
    assert_eq!(keymap.indicator(), Some("INSERT MODE"));
    assert_eq!(table::INSERT.len(), 1);
    assert!(matches!(
        table::INSERT[0].action,
        Action::Mode(Mode::Normal)
    ));
}

#[test]
fn normal_keeps_typing_keys_for_a_field() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    for c in "jkqtxyJ:".chars() {
        assert_eq!(keymap.press(&typed(c), FIELD, &b), KeyAction::Pass, "{c}");
    }
    // Keys that don't type still fire: Ctrl+O, Alt+1, and Escape leaving for insert mode.
    assert_eq!(keymap.press(&ctrl('o'), FIELD, &b), run("location.focus"));
    assert_eq!(
        keymap.press(&with('1', ALT), FIELD, &b),
        run("tab.select 1")
    );
    assert_eq!(keymap.press(&escape(), FIELD, &b), KeyAction::Take);
    assert_eq!(keymap.indicator(), Some("INSERT MODE"));
}

#[test]
fn in_the_line_only_always_applies() {
    let b = Fake::new(Frontend::Terminal);
    let mut keymap = example();
    for c in "jqtx:rJ".chars() {
        assert_eq!(keymap.press(&typed(c), CHROME, &b), KeyAction::Pass, "{c}");
    }
    assert_eq!(keymap.press(&escape(), CHROME, &b), KeyAction::Pass);
    assert_eq!(
        keymap.press(&named("Enter", 28), CHROME, &b),
        KeyAction::Pass
    );
    assert_eq!(keymap.press(&ctrl('o'), CHROME, &b), run("location.focus"));
    assert_eq!(keymap.press(&ctrl('r'), CHROME, &b), run("page.reload"));
    assert_eq!(
        keymap.press(&with('3', ALT), CHROME, &b),
        run("tab.select 3")
    );
    assert_eq!(keymap.press(&ctrl('l'), CHROME, &b), run("chrome.refresh"));
    // Page-scope keys that don't type, too: Ctrl+C is the line's (it copies).
    assert_eq!(keymap.press(&ctrl('c'), CHROME, &b), KeyAction::Pass);
    assert_eq!(keymap.indicator(), None, "the mode is unchanged");
}

#[test]
fn terminal_bindings_exist_only_in_the_terminal() {
    let gui = Fake::new(Frontend::Gui);
    let term = Fake::new(Frontend::Terminal);
    let mut keymap = example();
    assert_eq!(keymap.press(&ctrl('c'), PAGE, &gui), KeyAction::Pass);
    assert_eq!(keymap.press(&typed('m'), PAGE, &gui), KeyAction::Pass);
    assert_eq!(keymap.press(&ctrl('l'), PAGE, &gui), KeyAction::Pass);
    assert_eq!(keymap.press(&ctrl('c'), PAGE, &term), run("browser.quit"));
    assert_eq!(
        keymap.press(&typed('m'), PAGE, &term),
        run("chrome.toggle-mode")
    );
    assert_eq!(keymap.press(&ctrl('l'), PAGE, &term), run("chrome.refresh"));
    // The shared keys are in both.
    for b in [&gui, &term] {
        assert_eq!(keymap.press(&typed('q'), PAGE, b), run("browser.quit"));
    }
    let gui_chrome = |keys: Frontend| {
        bindings(keys)
            .filter(|(_, a)| matches!(a, Action::Run(t) if t.starts_with("chrome.")))
            .count()
    };
    assert_eq!(gui_chrome(Frontend::Gui), 0);
    assert_eq!(gui_chrome(Frontend::Terminal), 2);
}

#[test]
fn the_shared_table_in_normal_mode() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    for (key, command) in [
        (typed('o'), "location.focus"),
        (ctrl('o'), "location.focus"),
        (typed('t'), "tab.new"),
        (typed('x'), "tab.close"),
        (typed('J'), "tab.next"),
        (typed('K'), "tab.previous"),
        (typed('H'), "page.back"),
        (typed('L'), "page.forward"),
        (typed('r'), "page.reload"),
        (typed('e'), "page.switch-engine"),
        (typed('k'), "page.send-key up"),
        (ctrl('d'), "page.send-key pagedown"),
        (ctrl('u'), "page.send-key pageup"),
        (typed('g'), "page.send-key home"),
        (typed('G'), "page.send-key end"),
        (typed('+'), "page.zoom-in"),
        (typed('-'), "page.zoom-out"),
        (typed('='), "page.zoom-reset"),
        (with('9', ALT), "tab.select 9"),
    ] {
        assert_eq!(keymap.press(&key, PAGE, &b), run(command), "{command}");
    }
    assert_eq!(
        keymap.press(&typed(':'), PAGE, &b),
        KeyAction::RunThenPass(Command::LocationFocus)
    );
    // Unbound keys pass; nothing is swallowed.
    assert_eq!(keymap.press(&typed('z'), PAGE, &b), KeyAction::Pass);
    assert_eq!(
        keymap.press(&named("ArrowDown", 108), PAGE, &b),
        KeyAction::Pass
    );
}

#[test]
fn y_y_copies_the_address() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    assert_eq!(keymap.press(&typed('y'), PAGE, &b), KeyAction::Take);
    assert_eq!(keymap.indicator(), Some("y"));
    assert_eq!(
        keymap.press(&typed('y'), PAGE, &b),
        KeyAction::RunAll(vec![Command::ClipboardCopy("https://example.com/".into())])
    );
    assert_eq!(keymap.indicator(), None);
    // No address: taken, nothing runs.
    let blank = Fake::tabs(1, 0);
    assert_eq!(
        press(&mut keymap, "yy", PAGE, &blank),
        [KeyAction::Take, KeyAction::RunAll(vec![])]
    );
}

#[test]
fn c_o_closes_the_others_highest_first() {
    let b = Fake::tabs(5, 1);
    let mut keymap = example();
    let [first, second] = &press(&mut keymap, "co", PAGE, &b)[..] else {
        unreachable!()
    };
    assert_eq!(*first, KeyAction::Take);
    let closes = ["tab.close 5", "tab.close 4", "tab.close 3", "tab.close 1"]
        .map(|text| text.parse::<Command>().unwrap())
        .to_vec();
    assert_eq!(*second, KeyAction::RunAll(closes));
}

#[test]
fn a_key_that_doesnt_continue_a_sequence_starts_afresh() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    // y then j: the y is dropped, and j is j.
    assert_eq!(
        press(&mut keymap, "yj", PAGE, &b),
        [KeyAction::Take, run("page.send-key down")]
    );
    assert_eq!(keymap.indicator(), None);
    // y then an unbound key: it passes, and the sequence is gone.
    assert_eq!(
        press(&mut keymap, "yz", PAGE, &b),
        [KeyAction::Take, KeyAction::Pass]
    );
    assert_eq!(keymap.press(&typed('t'), PAGE, &b), run("tab.new"));
    // y, then y y: the second y starts the sequence afresh.
    keymap.press(&typed('c'), PAGE, &b);
    assert_eq!(
        press(&mut keymap, "yy", PAGE, &b)[1],
        KeyAction::RunAll(vec![Command::ClipboardCopy("https://example.com/".into())])
    );
}

#[test]
fn modifiers_and_dead_keys_are_not_keys_of_a_sequence() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    keymap.press(&typed('y'), PAGE, &b);
    assert_eq!(keymap.press(&named("Shift", 42), PAGE, &b), KeyAction::Pass);
    let dead = KeyEvent {
        key: Key::Dead(Some('¨')),
        ..named("Dead", 27)
    };
    assert_eq!(keymap.press(&dead, PAGE, &b), KeyAction::Pass);
    assert_eq!(keymap.indicator(), Some("y"), "still waiting");
    assert!(matches!(
        keymap.press(&typed('y'), PAGE, &b),
        KeyAction::RunAll(_)
    ));
}

#[test]
fn a_held_key_repeats_a_single_key_command_only() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    let j = typed('j');
    assert_eq!(keymap.press(&j, PAGE, &b), run("page.send-key down"));
    assert_eq!(
        keymap.press(&repeat(&j), PAGE, &b),
        run("page.send-key down")
    );
    assert_eq!(
        keymap.press(&repeat(&j), PAGE, &b),
        run("page.send-key down")
    );
    // The last key of a sequence, or a mode switch: held quietly.
    let y = typed('y');
    keymap.press(&y, PAGE, &b);
    assert!(matches!(keymap.press(&y, PAGE, &b), KeyAction::RunAll(_)));
    assert_eq!(keymap.press(&repeat(&y), PAGE, &b), KeyAction::Take);
    let esc = escape();
    keymap.press(&esc, PAGE, &b);
    assert_eq!(keymap.press(&repeat(&esc), PAGE, &b), KeyAction::Take);
    keymap.press(&esc, PAGE, &b);
    // A repeat of a key pressed long ago doesn't run what another key ran since.
    keymap.press(&j, PAGE, &b);
    keymap.press(&typed('t'), PAGE, &b);
    assert_eq!(keymap.press(&repeat(&j), PAGE, &b), KeyAction::Take);
    // A modifier pressed while j repeats leaves its repeats alone.
    keymap.press(&j, PAGE, &b);
    keymap.press(&named("Shift", 42), PAGE, &b);
    assert_eq!(
        keymap.press(&repeat(&j), PAGE, &b),
        run("page.send-key down")
    );
}

#[test]
fn the_core_ends_unfinished_sequences() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    keymap.press(&typed('y'), PAGE, &b);
    keymap.reset();
    assert_eq!(keymap.indicator(), None);
    assert_eq!(keymap.press(&typed('y'), PAGE, &b), KeyAction::Take);
    keymap.context(CHROME);
    assert_eq!(keymap.indicator(), None);
    keymap.context(PAGE);
    // The sequence starts again, so one y is only its start.
    assert_eq!(keymap.press(&typed('y'), PAGE, &b), KeyAction::Take);
    assert_eq!(keymap.indicator(), Some("y"));
}
