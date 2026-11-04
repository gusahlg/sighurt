//! Browser commands: everything the user can do, by name.
//!
//! Key bindings and chrome buttons name commands as strings such as `tab.new` or
//! `tab.select 3`: `<area>.<action>`, then an argument after whitespace. Users write these
//! names in their configuration, so they are a public vocabulary; keep them stable. A string
//! is parsed once, into a [`Command`], when the configuration is loaded or the chrome issues
//! it, so unknown names and bad arguments are reported then rather than ignored later.
//! Formatting a [`Command`] gives back its canonical string.
//!
//! A configuration's commands are parsed with a [`CommandSet`]: the built-in commands, and the
//! `chrome.<action>` commands of the chrome the configuration selects.

use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;

use crate::input::{ButtonState, Key, KeyCode, KeyEvent, KeyLocation, Modifiers};
use crate::keymap::{Chord, ChordKey};

/// The most bytes `page.find` searches for. A longer argument is refused when the command is
/// parsed, never cut, and the core refuses a longer one made otherwise.
pub const MAX_FIND: usize = 1024;

/// A browser command. Tab numbers count from 1, in tab-strip order.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// `tab.new [address]`: open a tab and select it, on the home page, or on `address` as
    /// `page.open` takes it (trimmed, never empty).
    TabNew(Option<String>),
    /// `tab.duplicate`: open a tab right after the selected one, at its address, and select it.
    /// Its history is not copied.
    TabDuplicate,
    /// `tab.move left|right|<n>`: move the selected tab one place (wrapping around), or to
    /// position `n`.
    TabMove(TabMove),
    /// `tab.reopen`: open the tab closed last again, at the end, and select it.
    TabReopen,
    /// `tab.close [n]`: close tab `n`, or the selected tab.
    TabClose(Option<NonZeroUsize>),
    /// `tab.next`: select the next tab, wrapping around.
    TabNext,
    /// `tab.previous`: select the previous tab, wrapping around.
    TabPrevious,
    /// `tab.select <n>`: select tab `n`.
    TabSelect(NonZeroUsize),
    /// `tab.last`: select the last tab.
    TabLast,
    /// `page.open <url>`: open a URL, or search for the text, in the selected tab. Holds the
    /// argument as given (trimmed, never empty); the core turns it into a URL.
    PageOpen(String),
    /// `page.open-in <engine> [address]`: open the address, or what the selected tab shows, in
    /// the engine named, whatever routing would choose. Whether the build has the engine is
    /// checked when the command runs.
    PageOpenIn {
        /// The engine's name: lower-case letters, digits and dashes, starting with a letter.
        engine: String,
        /// What to open, as `page.open` takes it (trimmed, never empty); `None` opens the
        /// selected tab's address again.
        address: Option<String>,
    },
    /// `page.switch-engine`: reopen the page shown in the next web engine, wrapping
    /// around. Which engines those are, and whether this page can move, is decided when
    /// the command runs.
    PageSwitchEngine,
    /// `page.reload`.
    PageReload,
    /// `page.stop`: stop loading.
    PageStop,
    /// `page.back`.
    PageBack,
    /// `page.forward`.
    PageForward,
    /// `page.zoom-in`.
    PageZoomIn,
    /// `page.zoom-out`.
    PageZoomOut,
    /// `page.zoom-reset`: back to the configured zoom (100 % unless the configuration says
    /// otherwise).
    PageZoomReset,
    /// `page.send-key <key>`: press and release a named key in the page, while the page has
    /// the keyboard: `down`, `pageup`, `shift-space`, ...
    PageSendKey(SentKey),
    /// `location.focus [text]`: focus the address line with its address selected; with
    /// `text`, the line holds `text` instead, the caret at its end (`location.focus :` opens
    /// the command line). The text is kept as given (trimmed, never empty).
    LocationFocus(Option<String>),
    /// `page.find [text]`: find `text` in the page shown, from where it is, wrapping around;
    /// without text, open the chrome's find field. The text is kept as given (trimmed, never
    /// empty), at most [`MAX_FIND`] bytes. Upper case in it makes the search case-sensitive.
    PageFind(Option<String>),
    /// `page.find-next`: the next match of the search.
    PageFindNext,
    /// `page.find-previous`: the previous match of the search.
    PageFindPrevious,
    /// `page.find-stop`: end the search and clear its highlights.
    PageFindStop,
    /// `page.exit-fullscreen`: leave the element fullscreen of the page shown.
    PageExitFullscreen,
    /// `history.clear`: forget every page visited.
    HistoryClear,
    /// `browser.fullscreen`: put the window in fullscreen, or take it out.
    BrowserFullscreen,
    /// `browser.home`: open the home page in the selected tab.
    BrowserHome,
    /// `browser.quit`.
    BrowserQuit,
    /// `site.forget [host]`: forget that site's cookies and stored data. No argument forgets
    /// the page shown. The argument is a host, as given (trimmed); wildcards are not special
    /// here, and a host that is not one is reported when the command runs.
    SiteForget(Option<String>),
    /// `clipboard.copy <text>`: put the text on the clipboard. The argument is kept as given
    /// (trimmed, never empty); a configuration makes it from what it reads of the browser.
    ClipboardCopy(String),
    /// `chrome.<action>`: one of the chrome's own commands, such as `chrome.toggle-tabs`. Holds
    /// the action. Each chrome has its own ([`CommandSet`]); they take no argument.
    Chrome(String),
}

impl Command {
    /// Every command name. The chrome's commands, `chrome.<action>`, depend on the chrome.
    pub const NAMES: [&'static str; 32] = [
        "tab.new",
        "tab.duplicate",
        "tab.move",
        "tab.reopen",
        "tab.close",
        "tab.next",
        "tab.previous",
        "tab.select",
        "tab.last",
        "page.open",
        "page.open-in",
        "page.switch-engine",
        "page.reload",
        "page.stop",
        "page.back",
        "page.forward",
        "page.zoom-in",
        "page.zoom-out",
        "page.zoom-reset",
        "page.send-key",
        "page.find",
        "page.find-next",
        "page.find-previous",
        "page.find-stop",
        "page.exit-fullscreen",
        "location.focus",
        "history.clear",
        "browser.home",
        "browser.fullscreen",
        "browser.quit",
        "site.forget",
        "clipboard.copy",
    ];

    /// The command's name, without its argument; `chrome` for every `chrome.<action>`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::TabNew(_) => "tab.new",
            Self::TabDuplicate => "tab.duplicate",
            Self::TabMove(_) => "tab.move",
            Self::TabReopen => "tab.reopen",
            Self::TabClose(_) => "tab.close",
            Self::TabNext => "tab.next",
            Self::TabPrevious => "tab.previous",
            Self::TabSelect(_) => "tab.select",
            Self::TabLast => "tab.last",
            Self::PageOpen(_) => "page.open",
            Self::PageOpenIn { .. } => "page.open-in",
            Self::PageSwitchEngine => "page.switch-engine",
            Self::PageReload => "page.reload",
            Self::PageStop => "page.stop",
            Self::PageBack => "page.back",
            Self::PageForward => "page.forward",
            Self::PageZoomIn => "page.zoom-in",
            Self::PageZoomOut => "page.zoom-out",
            Self::PageZoomReset => "page.zoom-reset",
            Self::PageSendKey(_) => "page.send-key",
            Self::PageFind(_) => "page.find",
            Self::PageFindNext => "page.find-next",
            Self::PageFindPrevious => "page.find-previous",
            Self::PageFindStop => "page.find-stop",
            Self::PageExitFullscreen => "page.exit-fullscreen",
            Self::LocationFocus(_) => "location.focus",
            Self::HistoryClear => "history.clear",
            Self::BrowserHome => "browser.home",
            Self::BrowserFullscreen => "browser.fullscreen",
            Self::BrowserQuit => "browser.quit",
            Self::SiteForget(_) => "site.forget",
            Self::ClipboardCopy(_) => "clipboard.copy",
            Self::Chrome(_) => "chrome",
        }
    }
}

impl FromStr for Command {
    type Err = CommandError;

    /// Parses `"name"` or `"name argument"`. Surrounding whitespace is ignored, as is the
    /// whitespace between name and argument; whitespace inside the argument is kept.
    fn from_str(text: &str) -> Result<Self, CommandError> {
        let text = text.trim();
        let (name, argument) = match text.split_once(char::is_whitespace) {
            Some((name, argument)) => (name, argument.trim()),
            None => (text, ""),
        };
        let tab_number = || {
            argument
                .parse()
                .map_err(|_| CommandError::InvalidTabNumber(argument.to_owned()))
        };
        let required = |command: &'static str| match argument {
            "" => Err(CommandError::MissingArgument(command)),
            argument => Ok(argument),
        };
        if let Some(action) = name.strip_prefix("chrome.") {
            return match is_action(action) && argument.is_empty() {
                true => Ok(Self::Chrome(action.to_owned())),
                false => Err(CommandError::InvalidChromeAction(text.to_owned())),
            };
        }
        let command = match name {
            "" => return Err(CommandError::Empty),
            "tab.close" if argument.is_empty() => return Ok(Self::TabClose(None)),
            "tab.close" => return Ok(Self::TabClose(Some(tab_number()?))),
            "site.forget" if argument.is_empty() => return Ok(Self::SiteForget(None)),
            "site.forget" => return Ok(Self::SiteForget(Some(argument.to_owned()))),
            "tab.new" if argument.is_empty() => return Ok(Self::TabNew(None)),
            "tab.new" => return Ok(Self::TabNew(Some(argument.to_owned()))),
            "location.focus" if argument.is_empty() => return Ok(Self::LocationFocus(None)),
            "location.focus" => return Ok(Self::LocationFocus(Some(argument.to_owned()))),
            "tab.move" => {
                return match required("tab.move")? {
                    "left" => Ok(Self::TabMove(TabMove::Left)),
                    "right" => Ok(Self::TabMove(TabMove::Right)),
                    n => n
                        .parse()
                        .map(|n| Self::TabMove(TabMove::To(n)))
                        .map_err(|_| CommandError::InvalidTabMove(n.to_owned())),
                };
            }
            "page.find" if argument.is_empty() => return Ok(Self::PageFind(None)),
            "page.find" if argument.len() > MAX_FIND => {
                return Err(CommandError::TooLong {
                    command: "page.find",
                    max: MAX_FIND,
                })
            }
            "page.find" => return Ok(Self::PageFind(Some(argument.to_owned()))),
            "tab.select" => {
                required("tab.select")?;
                return Ok(Self::TabSelect(tab_number()?));
            }
            "page.open" => return Ok(Self::PageOpen(required("page.open")?.to_owned())),
            "clipboard.copy" => {
                return Ok(Self::ClipboardCopy(required("clipboard.copy")?.to_owned()))
            }
            "page.open-in" => {
                let argument = required("page.open-in")?;
                let (engine, address) = match argument.split_once(char::is_whitespace) {
                    Some((engine, address)) => (engine, Some(address.trim().to_owned())),
                    None => (argument, None),
                };
                return match is_engine_name(engine) {
                    true => Ok(Self::PageOpenIn {
                        engine: engine.to_owned(),
                        address,
                    }),
                    false => Err(CommandError::InvalidEngine(engine.to_owned())),
                };
            }
            "page.send-key" => {
                let key = required("page.send-key")?.parse();
                return key.map(Self::PageSendKey).map_err(CommandError::InvalidKey);
            }
            "tab.duplicate" => Self::TabDuplicate,
            "tab.reopen" => Self::TabReopen,
            "tab.next" => Self::TabNext,
            "tab.previous" => Self::TabPrevious,
            "tab.last" => Self::TabLast,
            "page.reload" => Self::PageReload,
            "page.stop" => Self::PageStop,
            "page.back" => Self::PageBack,
            "page.forward" => Self::PageForward,
            "page.zoom-in" => Self::PageZoomIn,
            "page.zoom-out" => Self::PageZoomOut,
            "page.zoom-reset" => Self::PageZoomReset,
            "page.switch-engine" => Self::PageSwitchEngine,
            "page.find-next" => Self::PageFindNext,
            "page.find-previous" => Self::PageFindPrevious,
            "page.find-stop" => Self::PageFindStop,
            "page.exit-fullscreen" => Self::PageExitFullscreen,
            "history.clear" => Self::HistoryClear,
            "browser.home" => Self::BrowserHome,
            "browser.fullscreen" => Self::BrowserFullscreen,
            "browser.quit" => Self::BrowserQuit,
            unknown => return Err(CommandError::Unknown(unknown.to_owned())),
        };
        // The rest take no argument.
        match argument {
            "" => Ok(command),
            _ => Err(CommandError::UnexpectedArgument(command.name())),
        }
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())?;
        match self {
            Self::TabClose(Some(n)) | Self::TabSelect(n) => write!(f, " {n}"),
            Self::PageOpen(input)
            | Self::ClipboardCopy(input)
            | Self::PageFind(Some(input))
            | Self::TabNew(Some(input))
            | Self::LocationFocus(Some(input)) => write!(f, " {input}"),
            Self::TabMove(to) => write!(f, " {to}"),
            Self::PageOpenIn { engine, address } => match address {
                Some(address) => write!(f, " {engine} {address}"),
                None => write!(f, " {engine}"),
            },
            Self::PageSendKey(key) => write!(f, " {key}"),
            Self::SiteForget(Some(host)) => write!(f, " {host}"),
            Self::Chrome(action) => write!(f, ".{action}"),
            _ => Ok(()),
        }
    }
}

/// Where `tab.move` moves the selected tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TabMove {
    /// `left`: one place towards the start; the first tab goes to the end.
    Left,
    /// `right`: one place towards the end; the last tab goes to the start.
    Right,
    /// `<n>`: to position `n`, counting from 1; past the end is the end.
    To(NonZeroUsize),
}

impl fmt::Display for TabMove {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Left => f.write_str("left"),
            Self::Right => f.write_str("right"),
            Self::To(n) => write!(f, "{n}"),
        }
    }
}

/// Why a string is not a command.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CommandError {
    /// The string was empty or only whitespace.
    Empty,
    /// No command has this name.
    Unknown(String),
    /// The command needs an argument and got none.
    MissingArgument(&'static str),
    /// The command takes no argument but got one.
    UnexpectedArgument(&'static str),
    /// The argument should be a tab number (1 or more) but isn't.
    InvalidTabNumber(String),
    /// `tab.move`'s argument is not `left`, `right` or a tab number.
    InvalidTabMove(String),
    /// The argument is longer than the command takes (`page.find`: [`MAX_FIND`] bytes).
    TooLong {
        /// The command.
        command: &'static str,
        /// The most bytes it takes.
        max: usize,
    },
    /// `page.send-key`'s argument is not a key it can press; says why.
    InvalidKey(String),
    /// `page.open-in`'s engine is not an engine's name.
    InvalidEngine(String),
    /// A `chrome.<action>` command whose action isn't 1 to 32 lower-case letters, digits and
    /// dashes, or that has an argument: chrome commands take none.
    InvalidChromeAction(String),
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty command"),
            Self::Unknown(name) => write!(f, "unknown command {name:?}"),
            Self::MissingArgument(name) => write!(f, "{name} needs an argument"),
            Self::UnexpectedArgument(name) => write!(f, "{name} takes no argument"),
            Self::InvalidTabNumber(argument) => {
                write!(f, "{argument:?} is not a tab number (tabs count from 1)")
            }
            Self::InvalidTabMove(argument) => write!(
                f,
                "tab.move takes left, right or a tab number (from 1), not {argument:?}"
            ),
            Self::TooLong { command, max } => {
                write!(f, "{command} takes at most {max} bytes of text")
            }
            Self::InvalidKey(why) => f.write_str(why),
            Self::InvalidEngine(name) => write!(
                f,
                "{name:?} is not an engine's name (lower-case letters, digits and dashes, \
                 starting with a letter)"
            ),
            Self::InvalidChromeAction(text) => write!(
                f,
                "{text:?} is not a chrome command (chrome.<action>, in lower-case letters, \
                 digits and dashes, with no argument)"
            ),
        }
    }
}

impl std::error::Error for CommandError {}

/// Whether `action` can name a chrome's command: kebab-case ASCII, 1 to 32 characters.
fn is_action(action: &str) -> bool {
    let words = |word: &str| {
        !word.is_empty()
            && word
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    action.len() <= 32 && action.split('-').all(words)
}

/// Whether `name` can name an engine: lower-case ASCII letters, digits and dashes, starting
/// with a letter, as in the configuration.
fn is_engine_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The key `page.send-key` presses: a named key, such as `down`, `pageup`, `enter` or `f5`, or
/// `space`, with the modifiers held (`shift-space`). It is written as a keystroke of the
/// configuration (`sighurt_api::keymap::Chord`), but never a character, whose meaning depends
/// on the layout, and never a physical key (`code:`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SentKey {
    /// The modifiers it is pressed with.
    pub modifiers: Modifiers,
    /// What it means: a W3C key name ("ArrowDown", "PageUp", "F5"), or " " for space.
    pub key: Key,
    /// The physical key that means it: the one whose W3C code is the key's name, or Space.
    pub code: KeyCode,
}

/// The space bar's evdev code (`KEY_SPACE`).
const SPACE: KeyCode = KeyCode(57);

impl SentKey {
    /// The key going down or up, as the keyboard would report it. Only the press of space
    /// types something (" "), unless Control or Super is held.
    pub fn event(&self, state: ButtonState) -> KeyEvent {
        let m = self.modifiers;
        let types = state == ButtonState::Pressed && self.code == SPACE && !(m.control || m.logo);
        KeyEvent {
            code: self.code,
            key: self.key.clone(),
            state,
            repeat: false,
            modifiers: m,
            consumed: Modifiers::default(),
            text: types.then(|| " ".to_owned()),
        }
    }
}

/// Whether the key at `code` is a named key: not one that types a character on common layouts
/// (the main block's letters, digits and punctuation, Space, the ISO and JIS extra keys), and
/// not one of a pair (Shift, Control, Alt, Super) or of the keypad.
fn is_named(code: KeyCode) -> bool {
    let prints = matches!(code.0, 2..=13 | 16..=27 | 30..=41 | 43..=53 | 57 | 86 | 89 | 124);
    code.location() == KeyLocation::Standard && !prints
}

impl FromStr for SentKey {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let chord: Chord = text.parse().map_err(|e| format!("{text:?}: {e}"))?;
        let (key, code) = match chord.key {
            ChordKey::Character(c) if c == " " => (Key::Character(c), SPACE),
            ChordKey::Named(name) => {
                let code = (0..=255)
                    .map(KeyCode)
                    .find(|&code| code.w3c_code() == Some(name.as_str()) && is_named(code));
                let Some(code) = code else {
                    return Err(format!(
                        "page.send-key can't press {name:?}: it presses named keys such as \
                         down, pageup, enter, escape or f5"
                    ));
                };
                (Key::Named(name), code)
            }
            ChordKey::Character(_) => {
                return Err(format!(
                    "page.send-key presses named keys such as down or pageup, not {text:?}: \
                     to type text, type it"
                ))
            }
            ChordKey::Code(_) => {
                return Err(format!(
                    "page.send-key names a key such as down or pageup, not a key code \
                     ({text:?})"
                ))
            }
        };
        let modifiers = chord.mods.modifiers();
        Ok(Self {
            modifiers,
            key,
            code,
        })
    }
}

impl fmt::Display for SentKey {
    /// As a keystroke: `shift-space`, `ctrl-ArrowDown`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        let names = [
            (m.control, "ctrl-"),
            (m.alt, "alt-"),
            (m.shift, "shift-"),
            (m.logo, "super-"),
            (m.alt_graph, "altgr-"),
        ];
        for (_, name) in names.iter().filter(|(held, _)| *held) {
            f.write_str(name)?;
        }
        match &self.key {
            Key::Named(name) => f.write_str(name),
            _ => f.write_str("space"),
        }
    }
}

/// The commands a configuration may bind: every built-in command, and the `chrome.<action>`
/// commands of the chrome the configuration selects.
#[derive(Clone, Copy, Debug)]
pub struct CommandSet<'a> {
    /// The chrome's name, for messages.
    chrome: &'a str,
    /// The chrome's actions; `None` leaves them unchecked.
    actions: Option<&'a [&'static str]>,
}

impl<'a> CommandSet<'a> {
    /// Every command, with no chrome to check against: for reading a table again that a check
    /// with the selected chrome's set accepted.
    pub const ANY: CommandSet<'static> = CommandSet {
        chrome: "",
        actions: None,
    };

    /// The built-in commands and the chrome `chrome`'s `actions`.
    pub fn new(chrome: &'a str, actions: &'a [&'static str]) -> Self {
        Self {
            chrome,
            actions: Some(actions),
        }
    }

    /// `text` as a command, or why it can't be bound here: an unknown name, a bad argument, or
    /// a chrome action the selected chrome doesn't have ("the standard chrome has no command
    /// chrome.toggle-tab; it has chrome.toggle-tabs").
    pub fn parse(&self, text: &str) -> Result<Command, String> {
        let command = text
            .parse()
            .map_err(|error: CommandError| error.to_string())?;
        if let (Command::Chrome(action), Some(actions)) = (&command, self.actions) {
            if !actions.contains(&action.as_str()) {
                let has = match actions {
                    [] => "it has none".to_owned(),
                    _ => format!("it has chrome.{}", actions.join(", chrome.")),
                };
                return Err(format!(
                    "the {} chrome has no command {command}; {has}",
                    self.chrome
                ));
            }
        }
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Command, CommandError> {
        text.parse()
    }

    fn tab(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    #[test]
    fn every_name_parses_or_asks_for_its_argument() {
        for name in Command::NAMES {
            match parse(name) {
                Ok(command) => assert_eq!(command.name(), name),
                Err(error) => assert_eq!(error, CommandError::MissingArgument(name)),
            }
        }
    }

    #[test]
    fn arguments() {
        assert_eq!(parse("tab.close"), Ok(Command::TabClose(None)));
        assert_eq!(parse("tab.close 2"), Ok(Command::TabClose(Some(tab(2)))));
        assert_eq!(parse("tab.select 3"), Ok(Command::TabSelect(tab(3))));
        assert_eq!(
            parse(" page.open \t https://a.b "),
            Ok(Command::PageOpen("https://a.b".into()))
        );
        assert_eq!(
            parse("page.open rust  borrow checker"),
            Ok(Command::PageOpen("rust  borrow checker".into()))
        );
        assert_eq!(parse("site.forget"), Ok(Command::SiteForget(None)));
        assert_eq!(
            parse(" site.forget  example.com "),
            Ok(Command::SiteForget(Some("example.com".into())))
        );
        // A wildcard is still the command: the core says it is not a host when it runs.
        assert_eq!(
            parse("site.forget *.example.com"),
            Ok(Command::SiteForget(Some("*.example.com".into())))
        );
        assert_eq!(
            parse("clipboard.copy"),
            Err(CommandError::MissingArgument("clipboard.copy"))
        );
        assert_eq!(
            parse("clipboard.copy  a  b "),
            Ok(Command::ClipboardCopy("a  b".into()))
        );
    }

    #[test]
    fn display_gives_the_canonical_string_back() {
        for text in [
            "tab.new",
            "tab.close",
            "tab.close 4",
            "tab.select 12",
            "page.open https://example.com/a b",
            "page.open-in wpe",
            "page.open-in wasm https://example.com/app.wasm?x y",
            "page.switch-engine",
            "page.zoom-reset",
            "page.send-key ArrowDown",
            "page.send-key shift-space",
            "page.send-key ctrl-alt-F5",
            "location.focus",
            "location.focus :",
            "location.focus /",
            "tab.new demo://pattern",
            "tab.new rust borrow checker",
            "tab.duplicate",
            "tab.move left",
            "tab.move right",
            "tab.move 3",
            "tab.reopen",
            "page.find",
            "page.find Hello  world",
            "page.exit-fullscreen",
            "page.find-next",
            "page.find-previous",
            "page.find-stop",
            "history.clear",
            "browser.fullscreen",
            "browser.quit",
            "site.forget",
            "site.forget example.com",
            "clipboard.copy https://a.b/x y",
            "chrome.toggle-tabs",
        ] {
            let command = parse(text).unwrap();
            assert_eq!(command.to_string(), text);
            assert_eq!(parse(&command.to_string()), Ok(command));
        }
    }

    #[test]
    fn bad_commands_are_errors() {
        use CommandError::*;
        assert_eq!(parse(""), Err(Empty));
        assert_eq!(parse("  "), Err(Empty));
        assert_eq!(parse("tab.explode"), Err(Unknown("tab.explode".into())));
        assert_eq!(parse("Tab.New"), Err(Unknown("Tab.New".into())));
        assert_eq!(
            parse("appearance.theme dark"),
            Err(Unknown("appearance.theme".into()))
        );
        assert_eq!(parse("tab.reopen 2"), Err(UnexpectedArgument("tab.reopen")));
        assert_eq!(
            parse("history.clear all"),
            Err(UnexpectedArgument("history.clear"))
        );
        assert_eq!(
            parse("page.switch-engine wpe"),
            Err(UnexpectedArgument("page.switch-engine"))
        );
        assert_eq!(parse("tab.select"), Err(MissingArgument("tab.select")));
        assert_eq!(parse("page.open  "), Err(MissingArgument("page.open")));
        assert_eq!(parse("tab.select 0"), Err(InvalidTabNumber("0".into())));
        assert_eq!(parse("tab.close -1"), Err(InvalidTabNumber("-1".into())));
        assert_eq!(parse("tab.select two"), Err(InvalidTabNumber("two".into())));
    }

    #[test]
    fn tab_commands_take_their_arguments() {
        assert_eq!(parse("tab.new"), Ok(Command::TabNew(None)));
        assert_eq!(
            parse(" tab.new  gh rust "),
            Ok(Command::TabNew(Some("gh rust".into())))
        );
        assert_eq!(parse("tab.move left"), Ok(Command::TabMove(TabMove::Left)));
        assert_eq!(
            parse("tab.move  right "),
            Ok(Command::TabMove(TabMove::Right))
        );
        assert_eq!(
            parse("tab.move 12"),
            Ok(Command::TabMove(TabMove::To(tab(12))))
        );
        assert_eq!(
            parse("tab.move"),
            Err(CommandError::MissingArgument("tab.move"))
        );
        for bad in ["0", "-1", "up", "Left", "1 2"] {
            let error = parse(&format!("tab.move {bad}")).unwrap_err();
            assert_eq!(error, CommandError::InvalidTabMove(bad.into()));
            assert!(error.to_string().contains("left, right"), "{error}");
        }
        assert_eq!(parse("location.focus"), Ok(Command::LocationFocus(None)));
        assert_eq!(
            parse("location.focus  :"),
            Ok(Command::LocationFocus(Some(":".into())))
        );
    }

    #[test]
    fn find_text_over_the_limit_is_refused() {
        assert_eq!(parse("page.find"), Ok(Command::PageFind(None)));
        assert_eq!(
            parse(" page.find  Hello  World "),
            Ok(Command::PageFind(Some("Hello  World".into())))
        );
        let at_limit = "é".repeat(MAX_FIND / 2);
        assert_eq!(
            parse(&format!("page.find {at_limit}")),
            Ok(Command::PageFind(Some(at_limit.clone())))
        );
        let error = parse(&format!("page.find {at_limit}a")).unwrap_err();
        assert_eq!(
            error,
            CommandError::TooLong {
                command: "page.find",
                max: MAX_FIND
            }
        );
        assert_eq!(
            error.to_string(),
            "page.find takes at most 1024 bytes of text"
        );
    }

    #[test]
    fn open_in_names_an_engine_and_may_take_an_address() {
        let open_in = |engine: &str, address: Option<&str>| Command::PageOpenIn {
            engine: engine.into(),
            address: address.map(Into::into),
        };
        assert_eq!(parse("page.open-in wpe"), Ok(open_in("wpe", None)));
        assert_eq!(
            parse(" page.open-in  servo-2 \t räksmörgås  i  Göteborg "),
            Ok(open_in("servo-2", Some("räksmörgås  i  Göteborg")))
        );
        assert_eq!(
            parse("page.open-in"),
            Err(CommandError::MissingArgument("page.open-in"))
        );
        // Only the form of the name: whether the build has the engine is the core's to say.
        for bad in ["WPE", "9x", "-x", "w_p", "wpé"] {
            let error = parse(&format!("page.open-in {bad} https://a.test/")).unwrap_err();
            assert_eq!(error, CommandError::InvalidEngine(bad.into()));
            assert!(
                error.to_string().contains("not an engine's name"),
                "{error}"
            );
        }
    }

    #[test]
    fn chrome_commands_are_kebab_case_and_take_no_argument() {
        assert_eq!(
            parse("chrome.toggle-tabs"),
            Ok(Command::Chrome("toggle-tabs".into()))
        );
        assert_eq!(parse("chrome.toggle-tabs").unwrap().name(), "chrome");
        assert_eq!(parse(" chrome.a1 "), Ok(Command::Chrome("a1".into())));
        let longest = format!("chrome.{}", "a".repeat(32));
        assert!(parse(&longest).is_ok());
        for bad in [
            "chrome.",
            "chrome.Toggle-tabs",
            "chrome.toggle tabs",
            "chrome.toggle-tabs now",
            "chrome.toggle--tabs",
            "chrome.-tabs",
            "chrome.toggle_tabs",
            &format!("{longest}a"),
        ] {
            let error = parse(bad).unwrap_err();
            assert_eq!(error, CommandError::InvalidChromeAction(bad.trim().into()));
            assert!(error.to_string().contains("no argument"), "{error}");
        }
        assert_eq!(parse("chrome"), Err(CommandError::Unknown("chrome".into())));
    }

    #[test]
    fn send_key_presses_named_keys() {
        let sent = |text: &str| match parse(&format!("page.send-key {text}")) {
            Ok(Command::PageSendKey(key)) => key,
            other => panic!("{text}: {other:?}"),
        };
        // Lower-case names and W3C names are the same key, with its evdev code.
        let down = sent("down");
        assert_eq!(down, sent("ArrowDown"));
        assert_eq!(
            (down.key.clone(), down.code),
            (Key::Named("ArrowDown".into()), KeyCode(108))
        );
        assert_eq!(down.modifiers, Modifiers::default());
        for (text, code) in [
            ("pageup", 104),
            ("pagedown", 109),
            ("home", 102),
            ("end", 107),
            ("enter", 28),
            ("escape", 1),
            ("tab", 15),
            ("f1", 59),
            ("F24", 194),
            ("BrowserBack", 158),
        ] {
            assert_eq!(sent(text).code, KeyCode(code), "{text}");
        }
        // Space types a space, unless Control or Super is held; the release types nothing.
        let space = sent("shift-space");
        assert_eq!(space.key, Key::Character(" ".into()));
        assert!(space.modifiers.shift);
        let press = space.event(ButtonState::Pressed);
        assert_eq!(
            (press.code, press.text.as_deref()),
            (KeyCode(57), Some(" "))
        );
        assert_eq!(space.event(ButtonState::Released).text, None);
        assert_eq!(sent("ctrl-space").event(ButtonState::Pressed).text, None);
        let end = sent("end").event(ButtonState::Pressed);
        assert_eq!(
            (end.key, end.text, end.repeat),
            (Key::Named("End".into()), None, false)
        );

        // Characters depend on the layout, codes on the keyboard: neither is a key to send.
        for (text, why) in [
            ("j", "to type text, type it"),
            ("shift-j", "to type text, type it"),
            ("ctrl-a", "to type text, type it"),
            ("code:KeyJ", "not a key code"),
            ("code:ArrowDown", "not a key code"),
            ("KeyJ", "can't press \"KeyJ\""),
            ("Space", "can't press"),
            ("ShiftLeft", "can't press"),
            ("Numpad1", "can't press"),
            ("Shift", "can't press"),
            ("hyper-down", "unknown modifier"),
            ("down down", "not supported"),
        ] {
            let error = parse(&format!("page.send-key {text}")).unwrap_err();
            assert!(
                matches!(&error, CommandError::InvalidKey(e) if e.contains(why)),
                "{text}: {error:?}"
            );
        }
        assert_eq!(
            parse("page.send-key"),
            Err(CommandError::MissingArgument("page.send-key"))
        );
    }

    #[test]
    fn a_command_set_names_what_its_chrome_lacks() {
        let standard = CommandSet::new("standard", &["toggle-tabs"]);
        assert_eq!(
            standard.parse(" tab.close 2"),
            Ok(Command::TabClose(Some(tab(2))))
        );
        assert_eq!(
            standard.parse("chrome.toggle-tab"),
            Err(
                "the standard chrome has no command chrome.toggle-tab; it has \
                 chrome.toggle-tabs"
                    .into()
            )
        );
        let line = CommandSet::new("line", &[]);
        let error = line.parse("chrome.toggle-tabs").unwrap_err();
        assert!(error.ends_with("it has none"), "{error}");
        assert_eq!(
            line.parse("tab.nope"),
            Err("unknown command \"tab.nope\"".into())
        );
        assert_eq!(CommandSet::ANY.parse("tab.new"), Ok(Command::TabNew(None)));
        // The chrome's own command, and any with no chrome to check against; a malformed one
        // is a syntax error whichever chrome runs.
        let toggle = Command::Chrome("toggle-tabs".into());
        assert_eq!(standard.parse("chrome.toggle-tabs"), Ok(toggle));
        assert_eq!(
            CommandSet::ANY.parse("chrome.zoom"),
            Ok(Command::Chrome("zoom".into()))
        );
        assert!(standard
            .parse("chrome.Toggle")
            .unwrap_err()
            .contains("not a chrome command"));
    }
}
