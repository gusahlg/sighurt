//! The key data used from outside its crate, through what docs/rewrite/extension-api.md and
//! docs/rewrite/experience-2026-11-15.md §1 document: keys with two modes, a sequence, a site's
//! exception and an action computed from the browser view, built with the operations a
//! configuration uses, checked, looked up and described. A change that breaks this test breaks
//! configurations outside the workspace and needs an entry in extension-api.md's changelog.

use std::num::NonZeroUsize;

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::ids::TabId;
use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers};
use sighurt_api::keymap::{
    is_modifier, Action, Binding, Browser, Custom, Fallback, Found, Frontend, KeyContext, KeyTable,
    Keys, Mode, Outcome, Situation, TabInfo, When,
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

fn character(c: &str) -> KeyEvent {
    press(34, Key::Character(c.into()))
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
            false,
        ))
    }
}

/// `c`: close every tab but the selected one, computed from the browser view.
fn close_others(browser: &dyn Browser) -> Outcome {
    let active = browser.active_tab();
    let close = (0..browser.tab_count()).rev().filter(|&i| i != active);
    Outcome::run(
        close
            .map(|i| Command::TabClose(NonZeroUsize::new(i + 1)))
            .collect(),
    )
}

fn bind(keys: &str, when: When, action: Action) -> Binding {
    Binding {
        keys: keys.parse().unwrap(),
        when,
        action,
    }
}

/// `normal` (takes what it doesn't bind, gives the page no text) and `insert`.
fn keys() -> Keys {
    let mut keys = Keys {
        modes: vec![
            Mode::new("normal")
                .page_text(false)
                .unmatched(Fallback::Take),
            Mode::new("insert").indicator("INSERT"),
        ],
        initial: "normal".into(),
        line: Vec::new(),
    };
    let run = |text: &str| Action::Run(vec![text.parse().unwrap()]);
    let normal = KeyTable::Mode("normal");
    let insert = KeyTable::Mode("insert");
    let always = When::default();
    keys.bind(normal, bind("g t", always.clone(), run("tab.next")))
        .unwrap();
    keys.bind(
        normal,
        bind("escape", always.clone(), run("input.mode insert")),
    )
    .unwrap();
    let custom = Action::Custom(Custom::new("close the other tabs", close_others));
    keys.bind(normal, bind("c", always.clone(), custom))
        .unwrap();
    keys.bind(insert, bind("escape", always, run("input.mode normal")))
        .unwrap();
    let home = When {
        url: Some("sig://home".parse().unwrap()),
        ..When::default()
    };
    keys.unbind(insert, "escape".parse().unwrap(), home)
        .unwrap();
    keys
}

#[test]
fn keys_built_from_outside_check_resolve_and_describe() {
    let keys = keys();
    assert!(keys
        .check(&CommandSet::new("standard", &[]), Frontend::Gui)
        .is_empty());
    let page = Situation {
        frontend: Frontend::Terminal,
        context: KeyContext::new(KeyboardFocus::Page, false),
        address: Some("https://b.example/"),
    };
    let normal = KeyTable::Mode("normal");
    // The first key of a sequence waits; the second completes it.
    assert!(matches!(
        keys.resolve(normal, &[character("g")], &page),
        Found::Start(_)
    ));
    let Found::Complete(binding) = keys.resolve(normal, &[character("g"), character("t")], &page)
    else {
        panic!("g t is a binding");
    };
    assert_eq!(binding.action, Action::Run(vec![Command::TabNext]));
    // A site's tombstone: Escape is nothing in insert on sig://home.
    let escape = press(1, Key::Named("Escape".into()));
    let home = Situation {
        address: Some("sig://home"),
        ..page
    };
    let insert = KeyTable::Mode("insert");
    assert!(matches!(
        keys.resolve(insert, std::slice::from_ref(&escape), &home),
        Found::Unbound(_)
    ));
    assert!(matches!(
        keys.resolve(insert, std::slice::from_ref(&escape), &page),
        Found::Complete(_)
    ));
    // A custom action computes its commands from the view: the tab numbers, highest first.
    let Found::Complete(Binding {
        action: Action::Custom(custom),
        ..
    }) = keys.resolve(normal, &[character("c")], &page)
    else {
        panic!("c is a custom action");
    };
    let close = |n| Command::TabClose(NonZeroUsize::new(n));
    assert_eq!((custom.run)(&TABS), Outcome::run(vec![close(3), close(1)]));
    assert_eq!(TABS.tab(1).unwrap().url, Some("https://b.example/"));
    assert_eq!(TABS.tab(3), None);
    // Described for the help page.
    let described = keys.describe(Frontend::Terminal);
    let rows: Vec<(&str, &str, &str)> = described
        .iter()
        .map(|b| (b.mode.as_str(), b.keys.as_str(), b.action.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("NORMAL", "g t", "tab.next"),
            ("NORMAL", "escape", "INSERT mode"),
            ("NORMAL", "c", "close the other tabs"),
            ("INSERT", "escape", "NORMAL mode"),
        ]
    );
    assert_eq!(described[3].applies, "in the page, except on sig://home");
    // Modifiers and compositions are never keys of a sequence.
    assert!(is_modifier(&press(42, Key::Named("Shift".into()))));
    assert!(!is_modifier(&character("g")));
}
