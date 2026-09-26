//! Browser commands: everything the user can do, by name.
//!
//! Key bindings and chrome buttons name commands as strings such as `tab.new` or
//! `tab.select 3`: `<area>.<action>`, then an argument after whitespace. Users write these
//! names in their configuration, so they are a public vocabulary; keep them stable. A string
//! is parsed once, into a [`Command`], when the configuration is loaded or the chrome issues
//! it, so unknown names and bad arguments are reported then rather than ignored later.
//! Formatting a [`Command`] gives back its canonical string.

use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;

/// A browser command. Tab numbers count from 1, in tab-strip order.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// `tab.new`: open a tab on the home page and select it.
    TabNew,
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
    /// `page.zoom-reset`: back to 100 %.
    PageZoomReset,
    /// `location.focus`: focus the address bar.
    LocationFocus,
    /// `browser.home`: open the home page in the selected tab.
    BrowserHome,
    /// `browser.quit`.
    BrowserQuit,
}

impl Command {
    /// Every command name.
    pub const NAMES: [&'static str; 17] = [
        "tab.new",
        "tab.close",
        "tab.next",
        "tab.previous",
        "tab.select",
        "tab.last",
        "page.open",
        "page.reload",
        "page.stop",
        "page.back",
        "page.forward",
        "page.zoom-in",
        "page.zoom-out",
        "page.zoom-reset",
        "location.focus",
        "browser.home",
        "browser.quit",
    ];

    /// The command's name, without its argument.
    pub fn name(&self) -> &'static str {
        match self {
            Self::TabNew => "tab.new",
            Self::TabClose(_) => "tab.close",
            Self::TabNext => "tab.next",
            Self::TabPrevious => "tab.previous",
            Self::TabSelect(_) => "tab.select",
            Self::TabLast => "tab.last",
            Self::PageOpen(_) => "page.open",
            Self::PageReload => "page.reload",
            Self::PageStop => "page.stop",
            Self::PageBack => "page.back",
            Self::PageForward => "page.forward",
            Self::PageZoomIn => "page.zoom-in",
            Self::PageZoomOut => "page.zoom-out",
            Self::PageZoomReset => "page.zoom-reset",
            Self::LocationFocus => "location.focus",
            Self::BrowserHome => "browser.home",
            Self::BrowserQuit => "browser.quit",
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
        let command = match name {
            "" => return Err(CommandError::Empty),
            "tab.close" if argument.is_empty() => return Ok(Self::TabClose(None)),
            "tab.close" => return Ok(Self::TabClose(Some(tab_number()?))),
            "tab.select" => {
                required("tab.select")?;
                return Ok(Self::TabSelect(tab_number()?));
            }
            "page.open" => return Ok(Self::PageOpen(required("page.open")?.to_owned())),
            "tab.new" => Self::TabNew,
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
            "location.focus" => Self::LocationFocus,
            "browser.home" => Self::BrowserHome,
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
            Self::PageOpen(input) => write!(f, " {input}"),
            _ => Ok(()),
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
        }
    }
}

impl std::error::Error for CommandError {}

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
    }

    #[test]
    fn display_gives_the_canonical_string_back() {
        for text in [
            "tab.new",
            "tab.close",
            "tab.close 4",
            "tab.select 12",
            "page.open https://example.com/a b",
            "page.zoom-reset",
            "location.focus",
            "browser.quit",
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
        assert_eq!(parse("tab.new now"), Err(UnexpectedArgument("tab.new")));
        assert_eq!(parse("tab.select"), Err(MissingArgument("tab.select")));
        assert_eq!(parse("page.open  "), Err(MissingArgument("page.open")));
        assert_eq!(parse("tab.select 0"), Err(InvalidTabNumber("0".into())));
        assert_eq!(parse("tab.close -1"), Err(InvalidTabNumber("-1".into())));
        assert_eq!(parse("tab.select two"), Err(InvalidTabNumber("two".into())));
    }
}
