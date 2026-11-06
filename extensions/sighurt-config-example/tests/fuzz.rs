//! Seeded random key sequences through the example configuration, in both modes and both
//! frontends, with the page's keyboard, a page field's and the chrome's, as the core calls it
//! (repeats only of a press it took), on web pages and on the sites with tables of their own.
//! What decision 134 promises holds for every sequence on every address: while the page has
//! the keyboard, NORMAL never passes a key to the page (a command that runs and then passes the
//! press, `:`, moves the keyboard to the line first), and INSERT passes every key but its leave
//! keys for that address: an unmodified Escape, or on a site with its own INSERT that table's
//! keys (`ctrl-escape` and `f12` on the terminal pages). `tools/fuzz/mod.rs` describes the
//! environment variables.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::time::Duration;

use fuzz::{iterations, run, Rng};
use sighurt_api::commands::Command;
use sighurt_api::ids::TabId;
use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers};
use sighurt_api::keymap::{Browser, Chord, Frontend, KeyAction, KeyContext, TabInfo, UrlPattern};
use sighurt_config_example::{table, Binding, KIND};

const LIMIT: Duration = Duration::from_secs(2);

struct Fake {
    frontend: Frontend,
    tabs: usize,
    active: usize,
    fullscreen: bool,
    link: Option<&'static str>,
    context: KeyContext,
    /// The shown tab's committed address (every tab's).
    url: Option<&'static str>,
}

/// Addresses on and around the sites.
const URLS: &[Option<&str>] = &[
    Some("https://example.com/"),
    Some("sig://terminal"),
    Some("sig://terminal/config"),
    Some("SIG://Terminal"),
    Some("sig://config"),
    Some("sig://terminal-x"),
    Some("https://sig/terminal"),
    Some("sig://help"),
    None,
];

/// INSERT's table on `url`: the most specific site's that has one, else the general one.
fn insert_table(url: Option<&str>) -> &'static [Binding] {
    let sites = table::SITES
        .iter()
        .filter_map(|site| Some((site.url.parse::<UrlPattern>().ok()?, site.insert?)));
    let sites: Vec<_> = sites.collect();
    url.and_then(|url| UrlPattern::most_specific(sites.iter().map(|(p, t)| (p, *t)), url))
        .unwrap_or(table::INSERT)
}

/// Whether `key` is the first keystroke of a binding of `table`.
fn leaves(table: &[Binding], key: &KeyEvent) -> bool {
    table.iter().any(|binding| {
        let first = binding.keys.split_whitespace().next().unwrap();
        first.parse::<Chord>().unwrap().matches(key)
    })
}

impl Browser for Fake {
    fn frontend(&self) -> Frontend {
        self.frontend
    }
    fn context(&self) -> KeyContext {
        self.context
    }
    fn tab_count(&self) -> usize {
        self.tabs
    }
    fn active_tab(&self) -> usize {
        self.active
    }
    fn tab(&self, index: usize) -> Option<TabInfo<'_>> {
        (index < self.tabs).then(|| {
            TabInfo::new(
                TabId::next(),
                Some("Title"),
                self.url,
                false,
                Some("servo"),
                1.0,
                true,
                false,
                self.link,
                self.fullscreen,
            )
        })
    }
}

const NAMED: &[&str] = &[
    "Escape",
    "Escape",
    "Escape",
    "Enter",
    "Tab",
    "Backspace",
    "ArrowLeft",
    "ArrowDown",
    "PageUp",
    "Home",
    "End",
    "F5",
    "F11",
    "F12",
    "F12",
    "Delete",
    "Insert",
    "Shift",
    "Control",
    "Alt",
    "AltGraph",
    "Super",
    "Compose",
    "CapsLock",
    "",
];

fn modifiers(rng: &mut Rng) -> Modifiers {
    Modifiers {
        shift: rng.one_in(4),
        control: rng.one_in(5),
        alt: rng.one_in(7),
        alt_graph: rng.one_in(12),
        logo: rng.one_in(12),
        caps_lock: rng.one_in(6),
        num_lock: rng.one_in(6),
    }
}

fn key_event(rng: &mut Rng) -> KeyEvent {
    let key = match rng.below(10) {
        0..=4 => {
            let c = *rng.pick(&[
                'a', 'j', 'k', 'o', 'O', 'g', 'G', 'y', 'f', 'd', 'u', 'x', 'r', 'n', 'N', 'q',
                'm', ':', '/', ' ', '1', '9', '+', '-', '=', '?', 'ä', 'é', 'Z',
            ]);
            Key::Character(c.into())
        }
        5..=7 => Key::Named(rng.pick(NAMED).to_string()),
        8 => Key::Dead(rng.one_in(2).then_some('¨')),
        _ => match rng.below(3) {
            0 => Key::Unidentified,
            _ => Key::Character(rng.string(2)),
        },
    };
    let modifiers = modifiers(rng);
    let text = match &key {
        Key::Character(c) if !modifiers.control && !modifiers.alt => Some(c.clone()),
        _ => None,
    };
    KeyEvent {
        code: KeyCode(rng.below(130) as u32),
        key,
        state: ButtonState::Pressed,
        repeat: false,
        consumed: match rng.one_in(2) {
            true => modifiers,
            false => Modifiers::default(),
        },
        modifiers,
        text,
    }
}

/// An unmodified Escape: no Shift, Ctrl, Alt, AltGr or Super (the locks don't count).
fn unmodified_escape(key: &KeyEvent) -> bool {
    let m = key.modifiers;
    key.key == Key::Named("Escape".into())
        && !(m.shift || m.control || m.alt || m.alt_graph || m.logo)
}

#[test]
fn fuzz_the_example_modes_under_random_keys() {
    // Presses checked in each mode with the page's keyboard: both must happen.
    let (mut normal, mut insert_checked) = (0, 0);
    run("fuzz_example_modes", iterations(100), LIMIT, |case| {
        let mut keymap = (KIND.new)();
        keymap.configure(&Default::default());
        let mut browser = Fake {
            frontend: *case.rng.pick(&[Frontend::Gui, Frontend::Terminal]),
            tabs: case.rng.range(1, 12),
            active: 0,
            fullscreen: false,
            link: None,
            context: KeyContext::new(KeyboardFocus::Page, false),
            url: *case.rng.pick(URLS),
        };
        // The last press the keymap took, which the core then sends repeats of.
        let mut taken: Option<KeyEvent> = None;
        for _ in 0..case.rng.range(1, 60) {
            let rng = &mut case.rng;
            if rng.one_in(8) {
                let focus = match rng.below(4) {
                    0 => KeyboardFocus::Chrome,
                    _ => KeyboardFocus::Page,
                };
                browser.context = KeyContext::new(focus, rng.one_in(2));
                keymap.context(browser.context);
            }
            if rng.one_in(10) {
                browser.fullscreen = !browser.fullscreen;
            }
            if rng.one_in(10) {
                browser.link = rng.one_in(2).then_some("https://example.com/link");
            }
            if rng.one_in(30) {
                keymap.reset();
            }
            if rng.one_in(12) {
                browser.url = *rng.pick(URLS);
            }
            browser.active = rng.below(browser.tabs);
            let key = match taken.as_ref().filter(|_| rng.one_in(3)) {
                Some(press) => KeyEvent {
                    repeat: true,
                    ..press.clone()
                },
                None => key_event(rng),
            };
            let insert = keymap.page_takes_text();
            let context = browser.context;
            case.note(|| {
                format!(
                    "{} {:?} {:?} fullscreen {} {key:?}",
                    if insert { "INSERT" } else { "NORMAL" },
                    browser.url,
                    context,
                    browser.fullscreen
                )
            });
            let action = keymap.press(&key, context, &browser);
            let page = context.focus == KeyboardFocus::Page;
            match (page, insert) {
                (true, false) => normal += 1,
                (true, true) if !key.repeat => insert_checked += 1,
                _ => {}
            }
            match (page, insert) {
                (true, false) => match &action {
                    KeyAction::Pass => panic!("NORMAL passed {key:?} to the page"),
                    KeyAction::RunThenPass(command) => assert!(
                        matches!(command, Command::LocationFocus(_)),
                        "NORMAL passed {key:?} to the page after {command:?}"
                    ),
                    _ => {}
                },
                (true, true) if !key.repeat && !matches!(action, KeyAction::Pass) => {
                    let table = insert_table(browser.url);
                    assert!(
                        leaves(table, &key),
                        "INSERT took {key:?} on {:?}: {action:?}",
                        browser.url
                    );
                    assert!(
                        matches!(action, KeyAction::Take)
                            || action == KeyAction::Run(Command::PageExitFullscreen),
                        "INSERT's leave key did {action:?}"
                    );
                }
                _ => {}
            }
            // Escape in the page while it holds fullscreen leaves that and keeps the mode.
            if page && !key.repeat && unmodified_escape(&key) && browser.fullscreen {
                assert_eq!(keymap.page_takes_text(), insert, "the mode changed");
            }
            taken = match action {
                KeyAction::Pass => None,
                _ => Some(KeyEvent {
                    repeat: false,
                    ..key.clone()
                }),
            };
        }
    });
    if std::env::var_os("SIG_FUZZ_CASE").is_none() {
        assert!(
            normal > 0 && insert_checked > 0,
            "{normal} NORMAL, {insert_checked} INSERT"
        );
        eprintln!("fuzz_example_modes: {normal} NORMAL presses, {insert_checked} INSERT presses");
    }
}
