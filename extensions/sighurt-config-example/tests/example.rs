//! The example configuration driven as the core drives it: through `KIND` and the `Keymap` trait,
//! with a fake `Browser`.

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::ids::TabId;
use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers};
use sighurt_api::keymap::UrlPattern;
use sighurt_api::keymap::{Browser, Chord, Frontend, KeyAction, KeyContext, Keymap, TabInfo};
use sighurt_api::settings::{Table, Value};
use sighurt_config_example::{bindings, table, tables, Action, Mode, KIND, MAX_KEYS};

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
    /// Every tab's hovered link.
    link: Option<&'static str>,
    /// Whether every tab's page holds an element fullscreen.
    fullscreen: bool,
}

impl Fake {
    fn new(frontend: Frontend) -> Self {
        Self {
            frontend,
            urls: vec![Some("https://example.com/")],
            active: 0,
            link: None,
            fullscreen: false,
        }
    }
    fn tabs(urls: usize, active: usize) -> Self {
        Self {
            frontend: Frontend::Gui,
            urls: vec![None; urls],
            active,
            link: None,
            fullscreen: false,
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
            self.link,
            self.fullscreen,
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

fn shifted(key: KeyEvent) -> KeyEvent {
    KeyEvent {
        modifiers: SHIFT,
        ..key
    }
}

/// The keys NORMAL sends the page, with the key each sends.
fn navigation() -> [(KeyEvent, &'static str); 13] {
    [
        (named("ArrowDown", 108), "page.send-key down"),
        (named("ArrowUp", 103), "page.send-key up"),
        (named("ArrowLeft", 105), "page.send-key left"),
        (named("ArrowRight", 106), "page.send-key right"),
        (named("PageDown", 109), "page.send-key pagedown"),
        (named("PageUp", 104), "page.send-key pageup"),
        (named("Home", 102), "page.send-key home"),
        (named("End", 107), "page.send-key end"),
        (named("Tab", 15), "page.send-key tab"),
        (shifted(named("Tab", 15)), "page.send-key shift-tab"),
        (named("Enter", 28), "page.send-key enter"),
        (typed(' '), "page.send-key space"),
        (with(' ', SHIFT), "page.send-key shift-space"),
    ]
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
            // Built-in commands, and `help` of the built-in configuration's `[commands]`.
            if let Action::Run(text) | Action::RunThenPass(text) = action {
                let custom = ["help".to_owned()];
                CommandSet::ANY
                    .with_custom(&custom)
                    .parse(text)
                    .unwrap_or_else(|e| panic!("{text:?}: {e}"));
            }
        }
        // No binding is the same keys as, or the start of, another in its table.
        for (mode, site, tab) in tables() {
            let mode = format!("{mode:?} {site:?}");
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
    // The constructor parses every command and every site's pattern, and no two sites have the
    // same pattern.
    let _ = example();
    let patterns: Vec<UrlPattern> = table::SITES
        .iter()
        .map(|site| site.url.parse().unwrap())
        .collect();
    for (i, pattern) in patterns.iter().enumerate() {
        assert!(!patterns[i + 1..].contains(pattern), "{pattern} twice");
    }
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
    // Insert passes every other key, bound in normal or not, the chords that apply everywhere
    // in normal included, in both frontends, in a field or not.
    let term = Fake::new(Frontend::Terminal);
    let insert = [
        typed('j'),
        typed('q'),
        typed('Z'),
        typed(' '),
        ctrl('o'),
        ctrl('r'),
        with('1', ALT),
        with('9', ALT),
        ctrl('c'),
        ctrl('l'),
        named("F11", 87),
        named("Backspace", 14),
        named("Shift", 42),
        shifted(escape()),
    ];
    for frontend in [&b, &term] {
        for context in [PAGE, FIELD] {
            for key in insert.iter().chain(navigation().iter().map(|(key, _)| key)) {
                assert_eq!(
                    keymap.press(key, context, frontend),
                    KeyAction::Pass,
                    "{key:?}"
                );
            }
        }
    }
    assert_eq!(keymap.press(&typed('y'), PAGE, &b), KeyAction::Pass);
    assert_eq!(keymap.press(&typed('y'), PAGE, &b), KeyAction::Pass);
    assert_eq!(keymap.indicator(), Some("INSERT MODE"));
    assert_eq!(keymap.press(&escape(), FIELD, &b), KeyAction::Take);
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
        Action::ExitFullscreenOr(Mode::Normal)
    ));
}

/// A browser in `frontend` whose tab shows `url`.
fn at(url: &'static str, frontend: Frontend) -> Fake {
    Fake {
        urls: vec![Some(url)],
        ..Fake::new(frontend)
    }
}

fn ctrl_escape() -> KeyEvent {
    KeyEvent {
        modifiers: CTRL,
        ..escape()
    }
}

fn f12() -> KeyEvent {
    named("F12", 88)
}

#[test]
fn on_the_terminal_pages_escape_is_the_programs_and_ctrl_escape_or_f12_leaves_insert() {
    let mut keymap = example();
    for url in ["sig://terminal", "sig://terminal/config", "sig://config"] {
        for frontend in [Frontend::Gui, Frontend::Terminal] {
            let page = at(url, frontend);
            for leave in [ctrl_escape(), f12()] {
                // Escape enters INSERT there as everywhere.
                assert_eq!(
                    keymap.press(&escape(), PAGE, &page),
                    KeyAction::Take,
                    "{url}"
                );
                assert_eq!(keymap.indicator(), Some("INSERT MODE"));
                assert!(keymap.page_takes_text());
                // Then it is the program's, with a field or not, as every other key.
                for context in [PAGE, FIELD] {
                    for key in [
                        escape(),
                        shifted(escape()),
                        typed('x'),
                        ctrl('c'),
                        ctrl('\\'),
                    ] {
                        assert_eq!(
                            keymap.press(&key, context, &page),
                            KeyAction::Pass,
                            "{key:?}"
                        );
                    }
                }
                assert_eq!(keymap.indicator(), Some("INSERT MODE"));
                // In the address line the line has every key, as on any page.
                assert_eq!(keymap.press(&leave, CHROME, &page), KeyAction::Pass);
                assert_eq!(keymap.indicator(), Some("INSERT MODE"));
                assert_eq!(
                    keymap.press(&leave, PAGE, &page),
                    KeyAction::Take,
                    "{leave:?}"
                );
                assert_eq!(keymap.indicator(), None, "{url} {frontend:?} {leave:?}");
                assert!(!keymap.page_takes_text());
                // NORMAL there is NORMAL: keys are commands, and nothing reaches the program.
                assert_eq!(
                    keymap.press(&typed('j'), PAGE, &page),
                    run("page.send-key down")
                );
                assert_eq!(keymap.press(&typed('z'), PAGE, &page), KeyAction::Take);
                assert_eq!(keymap.press(&ctrl_escape(), PAGE, &page), KeyAction::Take);
                assert_eq!(keymap.press(&f12(), PAGE, &page), KeyAction::Take);
                assert_eq!(keymap.indicator(), None);
            }
        }
    }

    // Everywhere else decision 134 stands: Escape leaves INSERT, and ctrl-escape and f12 are
    // the page's.
    for url in [
        "https://example.com/",
        "sig://help",
        "sig://terminal-x",
        "https://sig/terminal",
        "sig://configuration",
    ] {
        let page = at(url, Frontend::Gui);
        assert_eq!(keymap.press(&escape(), PAGE, &page), KeyAction::Take);
        assert_eq!(
            keymap.press(&ctrl_escape(), PAGE, &page),
            KeyAction::Pass,
            "{url}"
        );
        assert_eq!(keymap.press(&f12(), PAGE, &page), KeyAction::Pass, "{url}");
        assert_eq!(keymap.indicator(), Some("INSERT MODE"));
        assert_eq!(
            keymap.press(&escape(), PAGE, &page),
            KeyAction::Take,
            "{url}"
        );
        assert_eq!(keymap.indicator(), None);
    }
    // A tab with no address yet has the general tables.
    let blank = Fake::tabs(1, 0);
    keymap.press(&escape(), PAGE, &blank);
    assert_eq!(keymap.press(&escape(), PAGE, &blank), KeyAction::Take);
    assert_eq!(keymap.indicator(), None);
}

#[test]
fn the_site_is_the_shown_tabs_committed_address() {
    let mut keymap = example();
    // Two tabs: a web page and the terminal page. What counts is the tab shown.
    let mut b = Fake::new(Frontend::Gui);
    b.urls = vec![Some("https://example.com/"), Some("sig://terminal")];
    b.active = 1;
    keymap.press(&escape(), PAGE, &b);
    assert_eq!(keymap.press(&escape(), PAGE, &b), KeyAction::Pass);
    b.active = 0;
    assert_eq!(keymap.press(&escape(), PAGE, &b), KeyAction::Take);
    assert_eq!(keymap.indicator(), None);
    // The link under the pointer is not the address.
    let hovering = Fake {
        link: Some("sig://terminal"),
        ..at("https://example.com/", Frontend::Gui)
    };
    keymap.press(&escape(), PAGE, &hovering);
    assert_eq!(keymap.press(&escape(), PAGE, &hovering), KeyAction::Take);
    // An unfinished sequence of one page does not leave INSERT on another: there is none in
    // INSERT, and the mode is the browser's, kept across tabs.
    keymap.press(&escape(), PAGE, &at("sig://terminal", Frontend::Gui));
    assert_eq!(keymap.indicator(), Some("INSERT MODE"));
    assert_eq!(
        keymap.press(&f12(), PAGE, &at("https://example.com/", Frontend::Gui)),
        KeyAction::Pass
    );
    assert_eq!(keymap.indicator(), Some("INSERT MODE"));
}

#[test]
fn escape_leaves_a_pages_fullscreen_first() {
    let mut b = Fake::new(Frontend::Gui);
    b.fullscreen = true;
    let mut keymap = example();
    let exit = KeyAction::Run(Command::PageExitFullscreen);
    assert_eq!(keymap.press(&escape(), PAGE, &b), exit);
    assert_eq!(keymap.indicator(), None, "still NORMAL");
    b.fullscreen = false;
    assert_eq!(keymap.press(&escape(), PAGE, &b), KeyAction::Take);
    assert_eq!(keymap.indicator(), Some("INSERT MODE"));
    b.fullscreen = true;
    assert_eq!(keymap.press(&escape(), PAGE, &b), exit);
    assert_eq!(keymap.indicator(), Some("INSERT MODE"), "still INSERT");
}

#[test]
fn only_insert_gives_the_page_text() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    assert!(!keymap.page_takes_text(), "NORMAL to start with");
    keymap.press(&escape(), PAGE, &b);
    assert!(keymap.page_takes_text());
    // In the line, the mode stays, and so does what the page takes.
    keymap.press(&escape(), CHROME, &b);
    assert!(keymap.page_takes_text());
    keymap.press(&escape(), FIELD, &b);
    assert!(!keymap.page_takes_text());
    // A page's fullscreen left with Escape keeps the mode, and what the page takes.
    let mut full = Fake::new(Frontend::Gui);
    full.fullscreen = true;
    keymap.press(&escape(), PAGE, &full);
    assert!(!keymap.page_takes_text());
}

#[test]
fn normal_runs_commands_in_a_field_and_passes_nothing() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    // A text field has the keyboard: the bindings fire as they do without one.
    for (c, command) in [
        ('j', "page.send-key down"),
        ('k', "page.send-key up"),
        ('q', "browser.quit"),
        ('t', "tab.new"),
        ('x', "tab.close"),
        ('J', "tab.next"),
        (':', "location.focus :"),
        ('/', "page.find"),
    ] {
        assert_eq!(keymap.press(&typed(c), FIELD, &b), run(command), "{c}");
    }
    assert_eq!(keymap.press(&ctrl('o'), FIELD, &b), run("location.focus"));
    assert_eq!(
        keymap.press(&with('1', ALT), FIELD, &b),
        run("tab.select 1")
    );
    // Whatever would type or edit is taken, in a field or not: unbound letters, digits and
    // punctuation, Backspace and Delete, chords no binding has, modifiers and dead keys.
    let dead = KeyEvent {
        key: Key::Dead(Some('¨')),
        ..named("Dead", 27)
    };
    for context in [PAGE, FIELD] {
        for key in [
            typed('z'),
            typed('Z'),
            typed('5'),
            typed(','),
            typed('ä'),
            named("Backspace", 14),
            named("Delete", 111),
            named("Insert", 110),
            ctrl('v'),
            ctrl('a'),
            named("Shift", 42),
            named("Control", 29),
            dead.clone(),
        ] {
            assert_eq!(keymap.press(&key, context, &b), KeyAction::Take, "{key:?}");
        }
    }
    // Space and Enter would type or submit in a field: taken there, sent to the page outside.
    for key in [typed(' '), with(' ', SHIFT), named("Enter", 28)] {
        assert_eq!(keymap.press(&key, FIELD, &b), KeyAction::Take, "{key:?}");
        assert!(matches!(keymap.press(&key, PAGE, &b), KeyAction::Run(_)));
    }
    // Escape still leaves for insert mode.
    assert_eq!(keymap.indicator(), None);
    assert_eq!(keymap.press(&escape(), FIELD, &b), KeyAction::Take);
    assert_eq!(keymap.indicator(), Some("INSERT MODE"));
}

#[test]
fn normal_sends_the_page_its_navigation_keys() {
    for b in [Fake::new(Frontend::Gui), Fake::new(Frontend::Terminal)] {
        let mut keymap = example();
        for (key, command) in navigation() {
            assert_eq!(keymap.press(&key, PAGE, &b), run(command), "{key:?}");
            // Held, it repeats.
            assert_eq!(keymap.press(&repeat(&key), PAGE, &b), run(command));
        }
        // The arrows, the page keys and Tab move the caret or the focus in a field: they are
        // sent there too.
        for (key, command) in navigation().into_iter().take(10) {
            assert_eq!(keymap.press(&key, FIELD, &b), run(command), "{key:?}");
        }
        // With a modifier no binding names, a navigation key is taken.
        assert_eq!(
            keymap.press(&with_named("ArrowDown", 108, CTRL), PAGE, &b),
            KeyAction::Take
        );
    }
}

fn with_named(name: &str, code: u32, modifiers: Modifiers) -> KeyEvent {
    KeyEvent {
        modifiers,
        ..named(name, code)
    }
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
    // Page-scope keys that don't type, too: Ctrl+C is the line's (it copies), and so are the
    // keys NORMAL sends the page.
    assert_eq!(keymap.press(&ctrl('c'), CHROME, &b), KeyAction::Pass);
    for (key, _) in navigation() {
        assert_eq!(keymap.press(&key, CHROME, &b), KeyAction::Pass, "{key:?}");
    }
    // Modifiers and unbound keys are the line's too.
    assert_eq!(
        keymap.press(&named("Shift", 42), CHROME, &b),
        KeyAction::Pass
    );
    assert_eq!(keymap.press(&ctrl('v'), CHROME, &b), KeyAction::Pass);
    assert_eq!(keymap.indicator(), None, "the mode is unchanged");
}

#[test]
fn in_the_line_insert_is_normal() {
    let b = Fake::new(Frontend::Terminal);
    let mut keymap = example();
    keymap.press(&escape(), PAGE, &b);
    assert_eq!(keymap.indicator(), Some("INSERT MODE"));
    // The modes are the page's: the line keeps NORMAL's few bindings, and Escape is its own.
    for c in "jqtx:rJ ".chars() {
        assert_eq!(keymap.press(&typed(c), CHROME, &b), KeyAction::Pass, "{c}");
    }
    assert_eq!(keymap.press(&escape(), CHROME, &b), KeyAction::Pass);
    assert_eq!(keymap.press(&ctrl('o'), CHROME, &b), run("location.focus"));
    assert_eq!(keymap.press(&ctrl('r'), CHROME, &b), run("page.reload"));
    assert_eq!(keymap.press(&ctrl('l'), CHROME, &b), run("chrome.refresh"));
    assert_eq!(
        keymap.press(&with('3', ALT), CHROME, &b),
        run("tab.select 3")
    );
    assert_eq!(
        keymap.indicator(),
        Some("INSERT MODE"),
        "the mode is unchanged"
    );
    // Back in the page, insert still passes them.
    assert_eq!(keymap.press(&ctrl('o'), PAGE, &b), KeyAction::Pass);
}

#[test]
fn terminal_bindings_exist_only_in_the_terminal() {
    let gui = Fake::new(Frontend::Gui);
    let term = Fake::new(Frontend::Terminal);
    let mut keymap = example();
    // Unbound in the window, so taken there, as any key no binding has.
    assert_eq!(keymap.press(&ctrl('c'), PAGE, &gui), KeyAction::Take);
    assert_eq!(keymap.press(&typed('m'), PAGE, &gui), KeyAction::Take);
    assert_eq!(keymap.press(&ctrl('l'), PAGE, &gui), KeyAction::Take);
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
        (typed('d'), "tab.close"),
        (typed('u'), "tab.reopen"),
        (typed(':'), "location.focus :"),
        (typed('/'), "page.find"),
        (typed('n'), "page.find-next"),
        (typed('N'), "page.find-previous"),
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
    // Unbound keys are taken: nothing reaches the page.
    assert_eq!(keymap.press(&typed('z'), PAGE, &b), KeyAction::Take);
    let z = typed('z');
    assert_eq!(keymap.press(&repeat(&z), PAGE, &b), KeyAction::Take);
}

#[test]
fn question_mark_and_f1_run_the_configurations_help() {
    let b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    let help = KeyAction::Run(Command::Custom("help".into()));
    let question = KeyEvent {
        code: KeyCode(53),
        ..typed('?')
    };
    assert_eq!(keymap.press(&question, PAGE, &b), help);
    let f1 = named("F1", 59);
    assert_eq!(keymap.press(&f1, PAGE, &b), help);
    // F1 also from the address line; ? there is typing.
    assert_eq!(keymap.press(&f1, CHROME, &b), help);
    assert_eq!(keymap.press(&question, CHROME, &b), KeyAction::Pass);
}

#[test]
fn describe_lists_every_binding_of_the_frontend_by_mode() {
    let keymap = example();
    for frontend in [Frontend::Gui, Frontend::Terminal] {
        let described = keymap.describe(frontend);
        assert_eq!(described.len(), bindings(frontend).count());
        let find = |keys: &str| {
            described
                .iter()
                .find(|b| b.keys == keys)
                .unwrap_or_else(|| panic!("{keys}"))
        };
        assert_eq!(
            (find("d").mode.as_str(), find("d").action.as_str()),
            ("NORMAL", "tab.close")
        );
        assert_eq!(find("?").action, "help");
        assert_eq!(find("f1").applies, "also in the address line");
        assert_eq!(find("y y").action, "copy the page's address");
        assert!(described
            .iter()
            .any(|b| b.mode == "INSERT" && b.keys == "escape"));
        // Each site's bindings with its address, from the table.
        for site in table::SITES {
            for keys in ["ctrl-escape", "f12"] {
                let applies = format!("on {}", site.url);
                assert!(
                    described
                        .iter()
                        .any(|b| (b.mode.as_str(), b.keys.as_str(), b.action.as_str())
                            == ("INSERT", keys, "NORMAL mode")
                            && b.applies == applies),
                    "{keys} {applies}"
                );
            }
        }
        assert_eq!(find("escape").applies, "in the page");
        assert!(described.iter().any(|b| b.mode == "INSERT"
            && b.keys == "escape"
            && b.applies == "in the page, except on sig://terminal, sig://config"));
        let terminal_only = described.iter().any(|b| b.keys == "ctrl-c");
        assert_eq!(terminal_only, frontend == Frontend::Terminal);
    }
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
fn y_f_copies_the_hovered_link() {
    let mut b = Fake::new(Frontend::Gui);
    let mut keymap = example();
    // No link under the pointer: taken, nothing runs.
    assert_eq!(
        press(&mut keymap, "yf", PAGE, &b),
        [KeyAction::Take, KeyAction::RunAll(vec![])]
    );
    b.link = Some("https://example.com/a?b#c");
    assert_eq!(
        press(&mut keymap, "yf", PAGE, &b)[1],
        KeyAction::RunAll(vec![Command::ClipboardCopy(
            "https://example.com/a?b#c".into()
        )])
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
    // y then an unbound key: it is taken, and the sequence is gone.
    assert_eq!(
        press(&mut keymap, "yz", PAGE, &b),
        [KeyAction::Take, KeyAction::Take]
    );
    assert_eq!(keymap.indicator(), None);
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
    // Taken, as every key in normal mode the page would get.
    assert_eq!(keymap.press(&named("Shift", 42), PAGE, &b), KeyAction::Take);
    let dead = KeyEvent {
        key: Key::Dead(Some('¨')),
        ..named("Dead", 27)
    };
    assert_eq!(keymap.press(&dead, PAGE, &b), KeyAction::Take);
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
