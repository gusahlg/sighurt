//! The keymap API used from outside its crate, through what docs/rewrite/extension-api.md §2.2
//! and §2.3 document: a keymap with a two-key sequence, its kind and check, and the core's
//! context and reset calls, and a keymap that computes commands from the browser view. A change that breaks this test breaks keymaps outside the workspace
//! and needs an entry in that document's changelog.

use std::num::NonZeroUsize;

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::ids::TabId;
use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers};
use sighurt_api::keymap::{
    is_modifier, Browser, Chord, Frontend, KeyAction, KeyContext, Keymap, KeymapKind, TabInfo,
};
use sighurt_api::settings::{Table, Value};

/// `[keymap.pair]`: `keys = "g t"` runs `run = "tab.next"` while the page has the keyboard and
/// doesn't take typed text. The first key waits for the second, and shows in the indicator.
#[derive(Default)]
struct Pair {
    keys: Vec<Chord>,
    run: Option<Command>,
    pending: bool,
}

/// The string `settings` has for `key`, or a problem.
fn string<'a>(settings: &'a Table, key: &str, problems: &mut Vec<String>) -> Option<&'a str> {
    let value = settings.get(key);
    let text = value.and_then(Value::as_str);
    match value {
        None => problems.push(format!("{key}: missing")),
        Some(value) if text.is_none() => {
            problems.push(format!("{key}: expected a string, not {}", value.kind()))
        }
        Some(_) => {}
    }
    text
}

/// The two keystrokes and the command of `settings`, or everything wrong with them.
fn read(settings: &Table, commands: &CommandSet<'_>) -> Result<(Vec<Chord>, Command), Vec<String>> {
    let mut problems = Vec::new();
    let keys: Vec<&str> = string(settings, "keys", &mut problems)
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if keys.len() != 2 {
        problems.push("keys: expected two keystrokes".into());
    }
    let chords: Vec<Chord> = keys
        .iter()
        .filter_map(|key| {
            key.parse()
                .map_err(|e| problems.push(format!("keys: {e}")))
                .ok()
        })
        .collect();
    let run = string(settings, "run", &mut problems).and_then(|run| {
        let command = commands.parse(run);
        command.map_err(|e| problems.push(format!("run: {e}"))).ok()
    });
    match run {
        Some(run) if problems.is_empty() => Ok((chords, run)),
        _ => Err(problems),
    }
}

impl Keymap for Pair {
    fn configure(&mut self, settings: &Table) {
        let (keys, run) = read(settings, &CommandSet::ANY).expect("checked");
        *self = Self {
            keys,
            run: Some(run),
            pending: false,
        };
    }

    fn press(&mut self, key: &KeyEvent, context: KeyContext, _: &dyn Browser) -> KeyAction {
        if context.focus != KeyboardFocus::Page || context.typing || is_modifier(key) {
            return KeyAction::Pass;
        }
        let pending = std::mem::take(&mut self.pending);
        match (pending, &self.run) {
            (true, Some(run)) if self.keys[1].matches(key) => KeyAction::Run(run.clone()),
            _ if self.keys[0].matches(key) => {
                self.pending = true;
                KeyAction::Take
            }
            _ => KeyAction::Pass,
        }
    }

    fn context(&mut self, _context: KeyContext) {
        self.pending = false;
    }

    fn reset(&mut self) {
        self.pending = false;
    }

    fn indicator(&self) -> Option<&str> {
        self.pending.then_some("g")
    }
}

const KIND: KeymapKind = KeymapKind {
    name: "pair",
    check: |settings, commands| read(settings, commands).err().unwrap_or_default(),
    new: || Box::<Pair>::default(),
};

fn press(code: u32, key: Key) -> KeyEvent {
    KeyEvent {
        code: KeyCode(code),
        key,
        state: ButtonState::Pressed,
        repeat: false,
        modifiers: Modifiers::default(),
        consumed: Modifiers::default(),
        text: None,
    }
}

fn table(entries: &[(&str, Value)]) -> Table {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect()
}

#[test]
fn a_keymap_from_outside_the_crate() {
    let standard = CommandSet::new("standard", &[]);
    let string = |text: &str| Value::String(text.into());
    let bad = table(&[("keys", string("g hyper-t")), ("run", Value::Integer(3))]);
    let problems = (KIND.check)(&bad, &standard);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].contains("unknown modifier"), "{problems:?}");
    assert!(problems[1].contains("not a whole number"), "{problems:?}");
    let chrome_action = table(&[("keys", string("g t")), ("run", string("chrome.zoom"))]);
    assert!((KIND.check)(&chrome_action, &standard)[0].contains("standard chrome"));

    let settings = table(&[("keys", string("g t")), ("run", string("tab.next"))]);
    assert_eq!((KIND.check)(&settings, &standard), Vec::<String>::new());
    let mut keymap = (KIND.new)();
    keymap.configure(&settings);
    let page = KeyContext::new(KeyboardFocus::Page, false);
    let (g, t) = (
        press(34, Key::Character("g".into())),
        press(20, Key::Character("t".into())),
    );
    let shift = press(42, Key::Named("Shift".into()));
    assert_eq!(keymap.press(&g, page, &TABS), KeyAction::Take);
    assert_eq!(keymap.indicator(), Some("g"));
    assert_eq!(
        keymap.press(&shift, page, &TABS),
        KeyAction::Pass,
        "modifiers keep the sequence"
    );
    assert_eq!(
        keymap.press(&t, page, &TABS),
        KeyAction::Run(Command::TabNext)
    );
    assert_eq!(keymap.indicator(), None);
    // The core's calls end an unfinished sequence.
    keymap.press(&g, page, &TABS);
    keymap.reset();
    assert_eq!(keymap.press(&t, page, &TABS), KeyAction::Pass);
    keymap.press(&g, page, &TABS);
    keymap.context(KeyContext::new(KeyboardFocus::Chrome, true));
    assert_eq!(keymap.indicator(), None);
    let typing = KeyContext::new(KeyboardFocus::Page, true);
    assert_eq!(keymap.press(&g, typing, &TABS), KeyAction::Pass);

    // Kinds are known by their names.
    let same_name = KeymapKind {
        check: |_, _| Vec::new(),
        ..KIND
    };
    assert_eq!(KIND, same_name);
    assert_eq!(format!("{KIND:?}"), "KeymapKind(pair)");
}

/// A browser with three tabs, the second selected.
struct Tabs;

const TABS: Tabs = Tabs;

impl Browser for Tabs {
    fn frontend(&self) -> Frontend {
        Frontend::Terminal
    }

    fn context(&self) -> KeyContext {
        KeyContext::new(KeyboardFocus::Page, false)
    }

    fn tab_count(&self) -> usize {
        3
    }

    fn active_tab(&self) -> usize {
        1
    }

    fn tab(&self, index: usize) -> Option<TabInfo<'_>> {
        let url = ["https://a.example/", "https://b.example/", "sig://home"].get(index)?;
        Some(TabInfo::new(
            TabId::next(),
            None,
            Some(url),
            false,
            Some("demo"),
            1.0,
            false,
            false,
            None,
        ))
    }
}

/// `c` closes every tab but the selected one, computed from the browser view.
struct CloseOthers;

impl Keymap for CloseOthers {
    fn configure(&mut self, _: &Table) {}

    fn press(&mut self, key: &KeyEvent, _: KeyContext, browser: &dyn Browser) -> KeyAction {
        if key.key != Key::Character("c".into()) {
            return KeyAction::Pass;
        }
        let active = browser.active_tab();
        let close = (0..browser.tab_count()).rev().filter(|&i| i != active);
        KeyAction::RunAll(
            close
                .map(|i| Command::TabClose(NonZeroUsize::new(i + 1)))
                .collect(),
        )
    }
}

#[test]
fn a_keymap_computes_commands_from_the_browser() {
    let page = KeyContext::new(KeyboardFocus::Page, false);
    let action = CloseOthers.press(&press(46, Key::Character("c".into())), page, &TABS);
    let close = |n| Command::TabClose(NonZeroUsize::new(n));
    assert_eq!(action, KeyAction::RunAll(vec![close(3), close(1)]));
    assert_eq!(TABS.tab(1).unwrap().url, Some("https://b.example/"));
    assert_eq!(TABS.tab(3), None);
}
