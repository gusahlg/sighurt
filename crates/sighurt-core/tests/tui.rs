//! The terminal's configuration: `[tui]`, its default keys, and chrome commands per frontend.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use sighurt_api::engine::{EngineCapabilities, EngineCommand};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::SurfaceRect;
use sighurt_api::ids::{DocumentGeneration, SeatId, ViewId};
use sighurt_api::input::{
    ButtonState, InputEvent, InputKind, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers,
    TextInputState, TextPurpose,
};
use sighurt_api::keymap::KeymapKind;
use sighurt_core::config::{
    Config, GraphicsProtocol, TuiConfig, TuiMode, DEFAULT_CONFIG, TERMINAL_CHROME_COMMANDS,
};
use sighurt_core::keys::Bindings;
use sighurt_core::tabs::Content;
use sighurt_core::{
    update, BrowserEvent, BrowserState, ChromeChecks, Effect, Environment, Frontend, OpenRequest,
};

const DEFAULT_KEYS: &[(&str, &str)] = &[
    ("q", "browser.quit"),
    ("ctrl-c", "browser.quit"),
    ("o", "location.focus"),
    ("ctrl-o", "location.focus"),
    (":", "location.focus"),
    ("t", "tab.new"),
    ("x", "tab.close"),
    ("J", "tab.next"),
    ("K", "tab.previous"),
    ("H", "page.back"),
    ("L", "page.forward"),
    ("r", "page.reload"),
    ("e", "page.switch-engine"),
    ("ctrl-r", "page.reload"),
    ("j", "page.send-key ArrowDown"),
    ("k", "page.send-key ArrowUp"),
    ("ctrl-d", "page.send-key PageDown"),
    ("ctrl-u", "page.send-key PageUp"),
    ("g", "page.send-key Home"),
    ("G", "page.send-key End"),
    ("+", "page.zoom-in"),
    ("-", "page.zoom-out"),
    ("=", "page.zoom-reset"),
    ("m", "chrome.toggle-mode"),
    ("ctrl-l", "chrome.refresh"),
    ("alt-1", "tab.select 1"),
    ("alt-2", "tab.select 2"),
    ("alt-3", "tab.select 3"),
    ("alt-4", "tab.select 4"),
    ("alt-5", "tab.select 5"),
    ("alt-6", "tab.select 6"),
    ("alt-7", "tab.select 7"),
    ("alt-8", "tab.select 8"),
    ("alt-9", "tab.select 9"),
];

/// A keymap whose table maps keys to commands, so `[keymap.fake]` is checked like a real one.
const FAKE: KeymapKind = KeymapKind {
    name: "fake",
    check: |table, commands| {
        table
            .iter()
            .filter_map(|(key, value)| match value.as_str() {
                Some(command) => commands
                    .parse(command)
                    .err()
                    .map(|e| format!("{key:?}: {e}")),
                None => Some(format!("{key:?}: expected a command, not {}", value.kind())),
            })
            .collect()
    },
    new: || Box::new(Bindings::default()),
};

struct FakeView {
    engine: String,
    entries: Vec<String>,
    current: usize,
    document: DocumentGeneration,
}

struct Harness {
    state: BrowserState,
    now: Instant,
    views: BTreeMap<ViewId, FakeView>,
    seat: SeatId,
}

fn environment(frontend: Frontend) -> Environment {
    let chrome = |name, commands| ChromeChecks {
        name,
        check: |_| Vec::new(),
        commands,
    };
    let servo = EngineCapabilities {
        history_traversal: true,
        page_zoom: true,
        ..EngineCapabilities::default()
    };
    Environment {
        engines: [("servo".into(), servo)].into(),
        frontend,
        keymaps: vec![FAKE],
        chromes: vec![chrome("standard", &["toggle-tabs"])],
        ..Environment::default()
    }
}

fn parse(frontend: Frontend, user: &str) -> Result<Config, sighurt_core::config::ConfigError> {
    Config::parse(
        Some(&format!("version = 1\n{user}")),
        &environment(frontend),
    )
}

fn problems(frontend: Frontend, user: &str) -> Vec<String> {
    parse(frontend, user)
        .expect_err("invalid")
        .problems()
        .to_vec()
}

fn open(input: &str) -> OpenRequest {
    OpenRequest {
        input: input.into(),
        engine: None,
    }
}

impl Harness {
    fn start(frontend: Frontend, config: &str, urls: &[&str]) -> Self {
        let env = environment(frontend);
        let config = Config::parse(Some(&format!("version = 1\n{config}")), &env).unwrap();
        let mut harness = Self {
            state: BrowserState::new(config, env),
            now: Instant::now(),
            views: BTreeMap::new(),
            seat: SeatId::next(),
        };
        harness.apply(BrowserEvent::Start {
            session: None,
            open: urls.iter().map(|url| open(url)).collect(),
        });
        harness
    }

    fn update(&mut self, event: BrowserEvent) -> Vec<Effect> {
        let effects = update(&mut self.state, event);
        for effect in &effects {
            if let Effect::OpenView { view, engine, .. } = effect {
                self.views.insert(
                    *view,
                    FakeView {
                        engine: engine.clone(),
                        entries: Vec::new(),
                        current: 0,
                        document: DocumentGeneration::next(),
                    },
                );
            }
        }
        effects
    }

    fn apply(&mut self, event: BrowserEvent) -> Vec<Effect> {
        let mut all = Vec::new();
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            let effects = self.update(event);
            for effect in &effects {
                queue.extend(self.answer(effect));
            }
            all.extend(effects);
            assert!(all.len() < 10_000, "the core and the engines never settle");
        }
        all
    }

    fn answer(&mut self, effect: &Effect) -> Vec<BrowserEvent> {
        let (view, events) = match effect {
            Effect::OpenView { view, .. } => (*view, vec![EngineEvent::ViewReady { view: *view }]),
            Effect::Engine { view, command } => (*view, self.carry_out(*view, command)),
            _ => return Vec::new(),
        };
        let engine = self.views[&view].engine.clone();
        events
            .into_iter()
            .map(|event| BrowserEvent::Engine {
                engine: engine.clone(),
                event,
                at: self.now,
            })
            .collect()
    }

    /// A navigation, the way an engine with its own history reports it.
    fn carry_out(&mut self, id: ViewId, command: &EngineCommand) -> Vec<EngineEvent> {
        let view = self.views.get_mut(&id).unwrap();
        let navigation = match command {
            EngineCommand::Navigate { navigation, url } => {
                view.entries.truncate(view.current + 1);
                view.entries.push(url.clone());
                view.current = view.entries.len() - 1;
                view.document = DocumentGeneration::next();
                *navigation
            }
            EngineCommand::Reload { navigation } => {
                view.document = DocumentGeneration::next();
                *navigation
            }
            EngineCommand::Traverse { navigation, delta } => {
                let Some(target) = view
                    .current
                    .checked_add_signed(*delta as isize)
                    .filter(|&i| i < view.entries.len())
                else {
                    return Vec::new();
                };
                view.current = target;
                view.document = DocumentGeneration::next();
                *navigation
            }
            _ => return Vec::new(),
        };
        vec![
            EngineEvent::NavigationCommitted {
                view: id,
                navigation,
                url: view.entries[view.current].clone(),
                document: view.document,
                history: None,
            },
            EngineEvent::HistoryChanged {
                view: id,
                entries: view.entries.clone(),
                current: view.current,
            },
        ]
    }

    fn shown(&self) -> ViewId {
        match self.state.tabs().nth(self.state.active()).unwrap().content {
            Content::View(view) => view,
            other => panic!("the active tab shows {other:?}"),
        }
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Effect> {
        self.update(BrowserEvent::Keyboard {
            event: InputEvent {
                seat: self.seat,
                kind: InputKind::Key(key),
            },
            at: self.now,
        })
    }

    /// Like [`Self::key`], then lets the fake engine answer, so a traversal commits.
    fn apply_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        self.apply(BrowserEvent::Keyboard {
            event: InputEvent {
                seat: self.seat,
                kind: InputKind::Key(key),
            },
            at: self.now,
        })
    }

    fn command(&mut self, text: &str) {
        self.apply(BrowserEvent::Command(text.parse().unwrap()));
    }

    fn zoom(&self) -> f64 {
        self.state.tabs().nth(self.state.active()).unwrap().zoom
    }
}

fn press(key: &str, modifiers: Modifiers) -> KeyEvent {
    KeyEvent {
        code: KeyCode(0),
        key: if key.chars().count() == 1 {
            Key::Character(key.into())
        } else {
            Key::Named(key.into())
        },
        state: ButtonState::Pressed,
        repeat: false,
        modifiers,
        consumed: Modifiers::default(),
        text: None,
    }
}

fn ctrl() -> Modifiers {
    Modifiers {
        control: true,
        ..Modifiers::default()
    }
}

fn alt() -> Modifiers {
    Modifiers {
        alt: true,
        ..Modifiers::default()
    }
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

fn page_keys(effects: &[Effect], view: ViewId) -> Vec<&KeyEvent> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Engine {
                view: id,
                command: EngineCommand::Input(event),
            } if *id == view => match &event.kind {
                InputKind::Key(key) => Some(key),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// `page.send-key` presses and releases `name`.
fn sent(keys: &[&KeyEvent], name: &str) -> bool {
    let named = |key: &KeyEvent| matches!(&key.key, Key::Named(got) if got == name);
    matches!(keys, [down, up] if down.state == ButtonState::Pressed && up.state == ButtonState::Released && named(down) && named(up))
}

fn delivered(keys: &[&KeyEvent], character: &str) -> bool {
    matches!(keys, [key]
        if key.state == ButtonState::Pressed
            && matches!(&key.key, Key::Character(got) if got == character))
}

fn has_command(effects: &[Effect], action: &str) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, Effect::ChromeCommand(got) if got == action))
}

fn reloaded(effects: &[Effect], view: ViewId) -> bool {
    effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::Engine {
                view: id,
                command: EngineCommand::Reload { .. },
            } if *id == view
        )
    })
}

fn traversed(effects: &[Effect], view: ViewId, delta: i32) -> bool {
    effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::Engine {
                view: id,
                command: EngineCommand::Traverse { delta: got, .. },
            } if *id == view && *got == delta
        )
    })
}

fn text_field(view: ViewId) -> EngineEvent {
    EngineEvent::TextInputChanged {
        view,
        state: Some(TextInputState {
            caret: SurfaceRect::default(),
            surrounding: None,
            purpose: TextPurpose::Normal,
            multiline: false,
        }),
    }
}

#[test]
fn the_built_in_table_is_the_one_documented() {
    assert_eq!(DEFAULT_KEYS.len(), 34);
    for (key, command) in DEFAULT_KEYS {
        let line = format!("\"{key}\" = \"{command}\"");
        assert!(
            DEFAULT_CONFIG.contains(&line),
            "the default file has no {line}"
        );
    }
    assert_eq!(
        TERMINAL_CHROME_COMMANDS,
        ["toggle-mode", "text-mode", "graphics-mode", "refresh"]
    );
}

#[test]
fn the_tui_table_parses_and_reports_every_invalid_value() {
    for frontend in [Frontend::Gui, Frontend::Terminal] {
        let config = Config::parse(None, &environment(frontend)).unwrap();
        assert_eq!(config.tui(), &TuiConfig::default());
        assert_eq!(config.tui().mode, TuiMode::Auto);
        assert_eq!(config.tui().graphics, GraphicsProtocol::Auto);
        assert_eq!(config.tui().fps, 10);
        assert!(config.tui().mouse && config.tui().default_keys);
        let config = parse(frontend, "[tui]\nfps = 30\n").unwrap();
        assert_eq!(config.tui().fps, 30);
        assert_eq!(config.tui().mode, TuiMode::Auto);
        assert!(config.tui().mouse);
        let config = parse(
            frontend,
            "[tui]\nmode = \"text\"\ngraphics = \"sixel\"\nmouse = false\n\
             default-keys = false\n",
        )
        .unwrap();
        assert_eq!(
            config.tui(),
            &TuiConfig {
                mode: TuiMode::Text,
                graphics: GraphicsProtocol::Sixel,
                fps: 10,
                mouse: false,
                default_keys: false,
            }
        );
    }
    let found = problems(
        Frontend::Gui,
        "[tui]\nmode = \"GUI\"\ngraphics = \"Kitty\"\nfps = 0\nmouse = \"yes\"\n\
         default-keys = 1\nkeys = \"nope\"\nextra = 1\n",
    );
    let expected = [
        "[tui] mode: expected \"auto\", \"text\" or \"graphics\", not \"GUI\"",
        "[tui] graphics: expected \"auto\", \"kitty\", \"sixel\" or \"none\", not \"Kitty\"",
        "[tui] fps: expected a whole number from 1 to 60, not 0",
        "[tui] mouse: expected true or false, not \"yes\"",
        "[tui] default-keys: expected true or false, not 1",
        "[tui] keys: expected a [tui.keys] table, not \"nope\"",
        "[tui] extra: unknown key ([tui] has mode, graphics, fps, mouse, default-keys and [tui.keys])",
    ];
    assert_eq!(found.len(), expected.len(), "{found:#?}");
    for part in expected {
        assert!(
            found.iter().any(|problem| problem == part),
            "{part:?} not in {found:#?}"
        );
    }
    assert_eq!(
        problems(Frontend::Terminal, "[tui]\nfps = 61\n"),
        ["[tui] fps: expected a whole number from 1 to 60, not 61"]
    );
    assert_eq!(
        problems(Frontend::Terminal, "[tui]\nfps = 10.0\n"),
        ["[tui] fps: expected a whole number from 1 to 60, not 10.0"]
    );
    // One bad field does not hide another, and a bad command in [tui.keys] is reported too.
    let found = problems(
        Frontend::Gui,
        "[tui]\nmode = \"nope\"\nfps = 0\n[tui.keys]\n\"q\" = \"chrome.toggle-tabs\"\n",
    );
    assert_eq!(found.len(), 3, "{found:#?}");
    assert!(found.iter().any(|p| p.contains("[tui] mode:")));
    assert!(found.iter().any(|p| p.contains("[tui] fps:")));
    assert!(found.iter().any(|p| p.contains(
        "the terminal chrome has no command chrome.toggle-tabs; it has chrome.toggle-mode, \
         chrome.text-mode, chrome.graphics-mode, chrome.refresh"
    )));
}

#[test]
fn the_terminal_gets_the_default_keys_and_the_window_does_not() {
    let mut terminal = Harness::start(Frontend::Terminal, "", &["https://a.test/"]);
    let view = terminal.shown();
    let plain = Modifiers::default();
    assert!(sent(
        &page_keys(&terminal.key(press("j", plain)), view),
        "ArrowDown"
    ));
    assert!(sent(
        &page_keys(&terminal.key(press("k", plain)), view),
        "ArrowUp"
    ));
    assert!(sent(
        &page_keys(&terminal.key(press("d", ctrl())), view),
        "PageDown"
    ));
    assert!(sent(
        &page_keys(&terminal.key(press("u", ctrl())), view),
        "PageUp"
    ));
    assert!(sent(
        &page_keys(&terminal.key(press("g", plain)), view),
        "Home"
    ));
    assert!(sent(
        &page_keys(&terminal.key(press("G", shift())), view),
        "End"
    ));
    terminal.key(press("+", plain));
    assert_eq!(terminal.zoom(), 1.1);
    terminal.key(press("=", plain));
    assert_eq!(terminal.zoom(), 1.0);
    terminal.key(press("-", plain));
    assert_eq!(terminal.zoom(), 0.9);
    assert!(reloaded(&terminal.key(press("r", plain)), view));
    assert!(reloaded(&terminal.key(press("r", ctrl())), view));
    assert!(has_command(&terminal.key(press("m", plain)), "toggle-mode"));
    assert!(has_command(&terminal.key(press("l", ctrl())), "refresh"));
    assert!(terminal
        .key(press("o", plain))
        .contains(&Effect::Focus(KeyboardFocus::Chrome)));
    terminal.update(BrowserEvent::Focus(KeyboardFocus::Page));
    assert!(terminal
        .key(press(":", plain))
        .contains(&Effect::Focus(KeyboardFocus::Chrome)));
    terminal.update(BrowserEvent::Focus(KeyboardFocus::Page));
    assert!(terminal.key(press("q", plain)).contains(&Effect::Quit));

    let mut tabs = Harness::start(
        Frontend::Terminal,
        "",
        &["https://a.test/", "https://b.test/"],
    );
    assert_eq!(tabs.state.active(), 0);
    tabs.key(press("J", shift()));
    assert_eq!(tabs.state.active(), 1);
    tabs.key(press("K", shift()));
    assert_eq!(tabs.state.active(), 0);
    tabs.key(press("2", alt()));
    assert_eq!(tabs.state.active(), 1);
    tabs.key(press("1", alt()));
    assert_eq!(tabs.state.active(), 0);
    tabs.key(press("9", alt()));
    assert_eq!(tabs.state.active(), 0);
    tabs.key(press("x", plain));
    assert_eq!(tabs.state.tabs().len(), 1, "x closes the active tab");
    tabs.key(press("t", plain));
    assert_eq!(tabs.state.tabs().len(), 2);
    assert_eq!(tabs.state.focus(), KeyboardFocus::Chrome);

    let mut history = Harness::start(Frontend::Terminal, "", &["https://a.test/"]);
    history.command("page.open https://b.test/");
    let view = history.shown();
    assert!(traversed(&history.apply_key(press("H", shift())), view, -1));
    assert!(traversed(&history.apply_key(press("L", shift())), view, 1));

    let mut window = Harness::start(Frontend::Gui, "", &["https://a.test/"]);
    let view = window.shown();
    let effects = window.key(press("j", plain));
    assert!(delivered(&page_keys(&effects, view), "j"));
    assert!(!effects.contains(&Effect::Quit));
    assert!(!window.key(press("q", plain)).contains(&Effect::Quit));
    assert!(!has_command(&window.key(press("m", plain)), "toggle-mode"));
    assert_eq!(window.zoom(), 1.0);
    window.key(press("+", plain));
    assert_eq!(window.zoom(), 1.0, "the window has no default zoom binding");
}

#[test]
fn user_keys_override_the_built_in_table_and_default_keys_false_drops_it() {
    let mut terminal = Harness::start(
        Frontend::Terminal,
        "[keys]\n\"j\" = \"tab.next\"\n\"q\" = \"page.reload\"\n",
        &["https://a.test/", "https://b.test/"],
    );
    let view = terminal.shown();
    let plain = Modifiers::default();
    assert!(reloaded(&terminal.key(press("q", plain)), view));
    assert!(!terminal.key(press("q", plain)).contains(&Effect::Quit));
    terminal.key(press("j", plain));
    assert_eq!(
        terminal.state.active(),
        1,
        "[keys] wins over the built-in j"
    );
    // A built-in binding the user did not override still fires.
    assert!(has_command(&terminal.key(press("m", plain)), "toggle-mode"));

    let mut off = Harness::start(
        Frontend::Terminal,
        "[tui]\ndefault-keys = false\n[keys]\n\"j\" = \"tab.next\"\n",
        &["https://a.test/", "https://b.test/"],
    );
    let view = off.shown();
    assert!(delivered(
        &page_keys(&off.key(press("q", plain)), view),
        "q"
    ));
    off.key(press("j", plain));
    assert_eq!(off.state.active(), 1, "the user's own [keys] stay");
    assert!(!off
        .key(press("m", plain))
        .iter()
        .any(|effect| { matches!(effect, Effect::ChromeCommand(_)) }));
    // The user's [tui.keys] stays when the built-in table is off, and still beats [keys].
    let mut own = Harness::start(
        Frontend::Terminal,
        "[tui]\ndefault-keys = false\n[tui.keys]\n\"q\" = \"browser.quit\"\n\
         [keys]\n\"q\" = \"page.reload\"\n",
        &["https://a.test/"],
    );
    assert!(own.key(press("q", plain)).contains(&Effect::Quit));
}

#[test]
fn tui_keys_win_over_keys_and_apply_only_in_the_terminal() {
    let config = "[tui.keys]\n\"j\" = \"tab.close\"\n\"ctrl-t\" = \"browser.quit\"\n\
                  [keys]\n\"j\" = \"tab.next\"\n[keys.page]\n\"ctrl-t\" = \"tab.new\"\n";
    let mut terminal = Harness::start(
        Frontend::Terminal,
        config,
        &["https://a.test/", "https://b.test/"],
    );
    let plain = Modifiers::default();
    terminal.key(press("j", plain));
    assert_eq!(terminal.state.tabs().len(), 1, "[tui.keys] beats [keys]");
    assert_eq!(terminal.state.active(), 0);
    let mut again = Harness::start(Frontend::Terminal, config, &["https://a.test/"]);
    let effects = again.key(press("t", ctrl()));
    assert!(effects.contains(&Effect::Quit));
    assert_eq!(again.state.tabs().len(), 1, "the [keys.page] binding lost");

    let mut window = Harness::start(Frontend::Gui, config, &["https://a.test/"]);
    assert_eq!(window.state.tabs().len(), 1);
    window.key(press("t", ctrl()));
    assert_eq!(
        window.state.tabs().len(),
        2,
        "the window ignores [tui.keys]"
    );
    assert!(!window.key(press("q", plain)).contains(&Effect::Quit));
}

#[test]
fn chrome_commands_are_checked_against_the_running_frontend() {
    let tables = "[keys]\n\"c\" = \"chrome.toggle-tabs\"\n\"m\" = \"chrome.toggle-mode\"\n\
                  [input.buttons]\nforward = \"chrome.toggle-mode\"\n\
                  [keymap.fake]\n\"x\" = \"chrome.toggle-mode\"\n\
                  [tui.keys]\n\"g\" = \"chrome.toggle-tabs\"\n";
    let window = problems(Frontend::Gui, tables);
    assert!(
        window.iter().any(|p| p.contains(
            "[keys] \"m\" = \"chrome.toggle-mode\": the standard chrome has no command \
             chrome.toggle-mode; it has chrome.toggle-tabs"
        )),
        "{window:#?}"
    );
    assert!(window.iter().any(|p| p.contains("[input.buttons] forward")));
    assert!(window.iter().any(|p| p.contains("[keymap.fake]")));
    assert!(
        window
            .iter()
            .any(|p| p.contains("the terminal chrome has no command chrome.toggle-tabs")),
        "{window:#?}"
    );
    assert!(
        !window.iter().any(|p| p.contains("\"c\"")),
        "the window's chrome has toggle-tabs: {window:#?}"
    );

    let terminal = problems(Frontend::Terminal, tables);
    assert!(
        terminal.iter().any(|p| p.contains(
            "[keys] \"c\" = \"chrome.toggle-tabs\": the terminal chrome has no command \
             chrome.toggle-tabs"
        )),
        "{terminal:#?}"
    );
    assert!(
        !terminal.iter().any(|p| p.contains("\"m\"")),
        "the terminal chrome has toggle-mode: {terminal:#?}"
    );
    assert!(
        terminal
            .iter()
            .any(|p| p.contains("[tui.keys]") && p.contains("chrome.toggle-tabs")),
        "{terminal:#?}"
    );
    // The same terminal command in [tui.keys] is valid in both frontends.
    assert!(parse(
        Frontend::Gui,
        "[tui.keys]\n\"m\" = \"chrome.toggle-mode\"\n\"r\" = \"chrome.refresh\"\n"
    )
    .is_ok());
    assert!(parse(
        Frontend::Terminal,
        "[keys]\n\"m\" = \"chrome.toggle-mode\"\n[input.buttons]\nback = \"chrome.text-mode\"\n"
    )
    .is_ok());

    let unused = "[input]\nkeymap = \"fake\"\n[tui.keys]\n\"m\" = \"chrome.refresh\"\n";
    assert_eq!(
        parse(Frontend::Terminal, unused).unwrap().notices(),
        ["[tui.keys] is not used: [input] keymap is \"fake\", which reads [keymap.fake]"]
    );
    assert_eq!(
        parse(Frontend::Gui, unused).unwrap().notices(),
        [] as [String; 0],
        "the window ignores [tui.keys] without saying so"
    );
}

#[test]
fn a_page_whose_fields_get_no_typing_keeps_its_character_bindings() {
    let mut terminal = Harness::start(Frontend::Terminal, "", &["https://a.test/"]);
    let view = terminal.shown();
    let plain = Modifiers::default();
    let field = |terminal: &mut Harness| {
        let at = terminal.now;
        terminal.update(BrowserEvent::Engine {
            engine: "servo".into(),
            event: text_field(view),
            at,
        })
    };
    let told = |effects: &[Effect], some: bool| {
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::PageTextInput(state) if state.is_some() == some))
    };
    assert!(told(&field(&mut terminal), true));
    assert!(delivered(&page_keys(&terminal.key(press("j", plain)), view), "j"));

    // The frontend shows the page itself (text mode): the field is the page's, not the user's.
    assert!(told(&terminal.update(BrowserEvent::PageTyping(false)), false));
    assert!(sent(&page_keys(&terminal.key(press("j", plain)), view), "ArrowDown"));
    // The page focusing a field again changes nothing.
    assert!(!told(&field(&mut terminal), true));
    assert!(sent(&page_keys(&terminal.key(press("j", plain)), view), "ArrowDown"));

    // Back to the page's own pixels: its focused field types again.
    assert!(told(&terminal.update(BrowserEvent::PageTyping(true)), true));
    assert!(delivered(&page_keys(&terminal.key(press("j", plain)), view), "j"));
    terminal.update(BrowserEvent::PageTyping(false));
    assert!(terminal.key(press("q", plain)).contains(&Effect::Quit));
}

#[test]
fn typing_suppresses_character_bindings_and_not_modifiers() {
    let mut terminal = Harness::start(
        Frontend::Terminal,
        "",
        &["https://a.test/", "https://b.test/"],
    );
    let view = terminal.shown();
    let plain = Modifiers::default();
    // The page's text field has the keyboard: j and J type, and do not scroll or change tabs.
    terminal.update(BrowserEvent::Engine {
        engine: "servo".into(),
        event: text_field(view),
        at: terminal.now,
    });
    let effects = terminal.key(press("j", plain));
    assert!(delivered(&page_keys(&effects, view), "j"));
    assert_eq!(terminal.state.active(), 0);
    let effects = terminal.key(press("J", shift()));
    assert!(delivered(&page_keys(&effects, view), "J"));
    assert_eq!(terminal.state.active(), 0);
    assert!(sent(
        &page_keys(&terminal.key(press("d", ctrl())), view),
        "PageDown"
    ));
    terminal.key(press("1", alt()));
    assert_eq!(terminal.state.active(), 0);
    terminal.key(press("2", alt()));
    assert_eq!(
        terminal.state.active(),
        1,
        "alt-2 still selects a tab while a text field is focused"
    );

    // The chrome's line is the same: a character binding waits, a modifier runs.
    let mut line = Harness::start(Frontend::Terminal, "", &["https://a.test/"]);
    line.command("location.focus");
    assert_eq!(line.state.focus(), KeyboardFocus::Chrome);
    let effects = line.key(press("j", plain));
    assert!(page_keys(&effects, line.shown()).is_empty());
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, Effect::ChromeInput(_))));
    assert!(!has_command(&effects, "toggle-mode"));
    assert!(has_command(&line.key(press("l", ctrl())), "refresh"));
    assert!(line.key(press("r", ctrl())).iter().any(|effect| {
        matches!(
            effect,
            Effect::Engine {
                command: EngineCommand::Reload { .. },
                ..
            }
        )
    }));
}
