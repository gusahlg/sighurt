//! Keymaps: what a key press does, as a small trait other crates can implement.
//!
//! The core holds one keymap, the one the configuration's `[input] keymap` names, and asks it
//! about each fresh key press: pass it on to whatever has the keyboard, take it, or run a
//! command. The default keymap, `bindings`, reads `[keys]`; another keymap reads its own table,
//! `[keymap.<name>]`, which its kind's check validates when the configuration loads. Keymaps are
//! compiled into `sig` and listed in its registry, as engines are; nothing is loaded at run
//! time.
//!
//! What the core guarantees around any keymap:
//!
//! 1. A taken press takes its release and its repeats, and a passed press passes them, so no
//!    keymap can leave the page or the chrome with half a keystroke. The keymap decides only
//!    about fresh presses and about repeats of presses it took.
//! 2. Only a fresh press starts anything: a key held from before goes where its press went,
//!    without asking the keymap.
//! 3. [`KeyAction::RunThenPass`] runs the command first, then routes the press by the focus as
//!    it is after the command; the press counts as passed, so its release follows it.
//! 4. The context is the core's: [`Keymap::context`] when the keyboard moves between page and
//!    chrome or a text field takes or loses it, [`Keymap::reset`] when the window loses the
//!    keyboard.
//! 5. The keymap runs inside the core's update: it does no I/O, reads no clock and starts no
//!    thread. It sees what the user types, passwords included, and keeps no more of it than an
//!    unfinished sequence and never logs it.
//! 6. A changed [`Keymap::indicator`] redraws the chrome.
//! 7. The [`Browser`] view a press comes with is read-only and lasts that one call: it is built
//!    from the state before the press is applied. It carries no clock, handle or engine type.
//! 8. A [`KeyAction::RunAll`] runs whole, in order, in the update of its press. Commands that
//!    change the tab list change the numbering the later ones see (`tab.close 3` renumbers tab
//!    4), as typing them one after another would, so a keymap closing several tabs by number
//!    closes the highest first.
//! 9. [`Keymap::page_takes_text`] is asked after every update: while it says `false`, text that
//!    is not a key press (an input method's, a terminal's paste) does not reach the page, and
//!    the host enables no input method for the page's fields.
//! 10. [`TabInfo::internal`] names the internal page a tab shows (`sig://help` is `help`) from
//!     the core's own record of the view, never from an address a page or engine reports, so a
//!     keymap may treat a page of the browser's own differently.
//!
//! [`Keymap::describe`] lists what a keymap binds, as data, for the help page.
//!
//! A keymap is the browser's configuration in Rust (docs/rewrite/config-extension.md): it reads
//! what the browser shows through [`Browser`], defines its own actions as commands computed from
//! it, and shows its state in both frontends' chromes through [`Keymap::indicator`]. Titles,
//! addresses and links are page data: a keymap may compute with them (an address to copy,
//! a tab to close) and must not put them in its indicator or log them.
//!
//! [`Chord`] is one keystroke of the configuration's syntax, for keymaps to match key events
//! with. [`UrlPattern`] is an address pattern of the configuration's syntax, for keymaps that
//! bind keys for some addresses: both of the workspace's keymaps match addresses with it, so
//! the rule is one.
//!
//! Must not own: the presses taken (the core's), where the keyboard is, or what a command does.

use std::fmt;
use std::str::FromStr;

use crate::commands::{Command, CommandSet};
use crate::ids::TabId;
use crate::input::{Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers};
use crate::settings::Table;

/// What a key press happens in.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyContext {
    /// Which part of the window has the keyboard.
    pub focus: KeyboardFocus,
    /// The focused part takes typed text: the chrome's line, or a text field of the page.
    pub typing: bool,
}

impl KeyContext {
    /// A context.
    pub const fn new(focus: KeyboardFocus, typing: bool) -> Self {
        Self { focus, typing }
    }
}

/// Which frontend runs the browser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Frontend {
    /// The Wayland window. The default, so a caller that doesn't say keeps the window's checks.
    #[default]
    Gui,
    /// The terminal.
    Terminal,
}

/// What a keymap may read of the browser while it decides one key press. Borrowed from the core
/// for that call only (guarantee 7).
pub trait Browser {
    /// The frontend that runs the browser.
    fn frontend(&self) -> Frontend;
    /// The same context [`Keymap::press`] is given.
    fn context(&self) -> KeyContext;
    /// How many tabs there are.
    fn tab_count(&self) -> usize;
    /// The index of the selected tab, counting from 0 in tab-strip order (`tab.select` counts
    /// from 1).
    fn active_tab(&self) -> usize;
    /// Tab `index`, or `None` past the end.
    fn tab(&self, index: usize) -> Option<TabInfo<'_>>;
}

/// One tab, as a keymap sees it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabInfo<'a> {
    /// The tab.
    pub id: TabId,
    /// The page's own title, if it gave one.
    pub title: Option<&'a str>,
    /// The committed address: what the tab shows, not what is loading.
    pub url: Option<&'a str>,
    /// Whether the tab is loading.
    pub loading: bool,
    /// The engine of the page it shows; `None` for `sig:` pages, blank tabs and errors.
    pub engine: Option<&'a str>,
    /// The internal page it shows, by name (`help` for `sig://help`), as the core recorded it
    /// when the view opened: never taken from an address. `None` for every other page.
    pub internal: Option<&'a str>,
    /// The page zoom, 1.0 being 100 %.
    pub zoom: f64,
    /// Whether `page.back` has somewhere to go.
    pub can_go_back: bool,
    /// Whether `page.forward` has somewhere to go.
    pub can_go_forward: bool,
    /// The link the pointer is over in the page shown: an http, https or file URL as the
    /// browser would open it, at most 8 KiB, gone when the document changes or the pointer
    /// leaves it. Page data, like the title; never anything else the page's status text says.
    pub link: Option<&'a str>,
    /// Whether the page shown holds an element fullscreen (`page.exit-fullscreen` leaves it).
    pub fullscreen: bool,
}

impl<'a> TabInfo<'a> {
    /// A tab with these fields, for the core and for tests outside the crate.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        id: TabId,
        title: Option<&'a str>,
        url: Option<&'a str>,
        loading: bool,
        engine: Option<&'a str>,
        zoom: f64,
        can_go_back: bool,
        can_go_forward: bool,
        link: Option<&'a str>,
        fullscreen: bool,
    ) -> Self {
        Self {
            id,
            title,
            url,
            loading,
            engine,
            zoom,
            can_go_back,
            can_go_forward,
            link,
            fullscreen,
            internal: None,
        }
    }

    /// This tab, showing the internal page `internal` (or none).
    pub const fn with_internal(mut self, internal: Option<&'a str>) -> Self {
        self.internal = internal;
        self
    }
}

/// One binding a keymap has, as [`Keymap::describe`] lists it for a help page.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBinding {
    /// The mode it applies in, as the keymap names its modes ("NORMAL"); empty for a keymap
    /// without modes.
    pub mode: String,
    /// Its keystrokes as the configuration writes them, separated by spaces ("y y", "ctrl-o").
    pub keys: String,
    /// What it does: the command it runs as the configuration writes it ("tab.close"), or a
    /// short description ("INSERT mode").
    pub action: String,
    /// Where it applies, in a few words ("in the page", "also in the address line"); empty
    /// for wherever the keyboard is.
    pub applies: String,
}

impl KeyBinding {
    /// A binding with these fields.
    pub fn new(
        mode: impl Into<String>,
        keys: impl Into<String>,
        action: impl Into<String>,
        applies: impl Into<String>,
    ) -> Self {
        Self {
            mode: mode.into(),
            keys: keys.into(),
            action: action.into(),
            applies: applies.into(),
        }
    }
}

/// What the browser does with a key press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Not the browser's: the press, its repeats and its release go to whatever has focus.
    Pass,
    /// Taken, and nothing runs: part of a sequence, or a change of mode.
    Take,
    /// Taken, and the command runs.
    Run(Command),
    /// The command runs, then the press goes to whatever has focus now, as if passed, so `:`
    /// can focus the command line and type itself into it.
    RunThenPass(Command),
    /// Taken, and these commands run in order in this update (guarantee 8). Empty is `Take`.
    RunAll(Vec<Command>),
}

/// A keymap: turns key presses into [`KeyAction`]s.
pub trait Keymap {
    /// Applies its table (`[keys]` for `bindings`, `[keymap.<name>]` otherwise), which its
    /// kind's check accepted. Called before anything else, and after every reload that keeps
    /// this keymap.
    fn configure(&mut self, settings: &Table);

    /// A fresh key press, or an auto-repeat (`key.repeat`) of a press this keymap took.
    /// Releases never come here. For a repeat, `Pass` counts as `Take`, `RunThenPass` as `Run`
    /// and `RunAll` as itself. `browser` is what the browser shows, before this press.
    fn press(&mut self, key: &KeyEvent, context: KeyContext, browser: &dyn Browser) -> KeyAction;

    /// The context changed between presses: the keyboard moved between page and chrome, or a
    /// text field took or lost it.
    fn context(&mut self, _context: KeyContext) {}

    /// The window lost the keyboard: drop an unfinished sequence.
    fn reset(&mut self) {}

    /// What the chrome shows about the keymap ("NORMAL", "NORMAL g"). The executable passes on
    /// at most 64 characters of one line.
    fn indicator(&self) -> Option<&str> {
        None
    }

    /// Whether the page may be given text that is not a key press: an input method's text, a
    /// terminal's paste (guarantee 9). `false` in a mode whose keys are only commands, so
    /// nothing the user types reaches the page there. Key presses are [`Self::press`]'s.
    fn page_takes_text(&self) -> bool {
        true
    }

    /// The bindings it has in `frontend`, in the order a reader should see them, for the help
    /// page. Empty by default: a keymap that can't list its bindings lists none.
    fn describe(&self, _frontend: Frontend) -> Vec<KeyBinding> {
        Vec::new()
    }
}

/// A keymap a build can offer. Each keymap crate exports one, as `KIND`. Compared and printed
/// by its name.
#[derive(Clone, Copy)]
pub struct KeymapKind {
    /// Its name in the configuration's `[input] keymap`.
    pub name: &'static str,
    /// Everything wrong with its table, one line each; empty if the table is valid. Commands
    /// are parsed with `commands`, so chrome actions are checked too.
    pub check: fn(&Table, &CommandSet<'_>) -> Vec<String>,
    /// A new keymap, which the core configures before using it.
    pub new: fn() -> Box<dyn Keymap>,
}

impl fmt::Debug for KeymapKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeymapKind({})", self.name)
    }
}

impl PartialEq for KeymapKind {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for KeymapKind {}

/// Whether `key` is a modifier, a lock, or a key that only starts or advances a composition (a
/// dead key, Compose). Sequences leave these out.
pub fn is_modifier(key: &KeyEvent) -> bool {
    const NAMES: [&str; 15] = [
        "Shift",
        "Control",
        "Alt",
        "AltGraph",
        "Meta",
        "Super",
        "Hyper",
        "Fn",
        "FnLock",
        "CapsLock",
        "NumLock",
        "ScrollLock",
        "Symbol",
        "SymbolLock",
        "ModeChange",
    ];
    match &key.key {
        Key::Dead(_) => true,
        Key::Named(name) => name == "Compose" || NAMES.contains(&name.as_str()),
        Key::Character(_) | Key::Unidentified => false,
    }
}

/// One keystroke: modifiers and a key, such as `ctrl-shift-t`, `alt-left` or `ctrl-code:KeyQ`.
///
/// A keystroke matches the key's meaning in the current layout (`KeyEvent::key`) unless it
/// names a physical key (`code:KeyQ`). Modifiers the layout used to produce a character don't
/// count, so AltGr + 2 is "@" on a Swedish layout and never a Ctrl+Alt shortcut, and `ctrl-+`
/// is Ctrl and whatever key types "+". For letters, shift counts: `ctrl-shift-t` (or `ctrl-T`)
/// is Ctrl+Shift+T whether or not Caps Lock is on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub(crate) mods: Mods,
    pub(crate) key: ChordKey,
}

/// The modifiers a binding cares about. Caps Lock and Num Lock never matter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Mods {
    ctrl: bool,
    alt: bool,
    shift: bool,
    logo: bool,
    alt_graph: bool,
}

impl Mods {
    fn held(held: Modifiers, consumed: Modifiers) -> Self {
        Self {
            ctrl: held.control && !consumed.control,
            alt: held.alt && !consumed.alt,
            shift: held.shift && !consumed.shift,
            logo: held.logo && !consumed.logo,
            alt_graph: held.alt_graph && !consumed.alt_graph,
        }
    }

    /// The modifiers held, with no lock on.
    pub(crate) fn modifiers(self) -> Modifiers {
        Modifiers {
            shift: self.shift,
            control: self.ctrl,
            alt: self.alt,
            alt_graph: self.alt_graph,
            logo: self.logo,
            ..Modifiers::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChordKey {
    /// What the key types. A letter is kept in lower case; whether it was upper case is Shift.
    Character(String),
    /// A key that types nothing, by its W3C key name ("Enter", "ArrowLeft", "F5").
    Named(String),
    /// A physical key, wherever the layout puts its meaning.
    Code(KeyCode),
}

/// Key names accepted in lower case, and the W3C key names they stand for.
const KEY_NAMES: [(&str, &str); 16] = [
    ("enter", "Enter"),
    ("escape", "Escape"),
    ("esc", "Escape"),
    ("tab", "Tab"),
    ("backspace", "Backspace"),
    ("delete", "Delete"),
    ("insert", "Insert"),
    ("home", "Home"),
    ("end", "End"),
    ("pageup", "PageUp"),
    ("pagedown", "PageDown"),
    ("left", "ArrowLeft"),
    ("right", "ArrowRight"),
    ("up", "ArrowUp"),
    ("down", "ArrowDown"),
    ("menu", "ContextMenu"),
];

/// The lower-case form of `text` if it is one letter that has upper and lower case, and whose
/// lower case is one character too ("İ" lowercases to "i" and a combining dot, which no key
/// types, so it is not a letter here and a keystroke naming it stays as written).
fn letter(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let c = chars.next()?;
    let cased = chars.next().is_none() && c.to_lowercase().ne(c.to_uppercase());
    let lower: String = c.to_lowercase().collect();
    (cased && lower.chars().count() == 1).then_some(lower)
}

impl Chord {
    /// Whether `event` is this keystroke.
    pub fn matches(&self, event: &KeyEvent) -> bool {
        let all = Mods::held(event.modifiers, Modifiers::default());
        match &self.key {
            ChordKey::Code(code) => event.code == *code && self.mods == all,
            ChordKey::Named(name) => {
                matches!(&event.key, Key::Named(key) if key == name) && self.mods == all
            }
            ChordKey::Character(character) => {
                let Key::Character(typed) = &event.key else {
                    return false;
                };
                let mut mods = Mods::held(event.modifiers, event.consumed);
                match letter(typed) {
                    Some(lower) => {
                        mods.shift = event.modifiers.shift;
                        lower == *character && self.mods == mods
                    }
                    None => typed == character && self.mods == mods,
                }
            }
        }
    }

    /// Whether the keystroke would type a character (no modifier but Shift), so it must not
    /// fire while the user types.
    pub fn types(&self) -> bool {
        let Mods {
            ctrl,
            alt,
            logo,
            alt_graph,
            ..
        } = self.mods;
        matches!(self.key, ChordKey::Character(_)) && !(ctrl || alt || logo || alt_graph)
    }
}

impl FromStr for Chord {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        if text.trim().contains(char::is_whitespace) {
            return Err("key sequences such as \"g g\" are not supported".into());
        }
        // The key comes after the last "-", and "-" is a key too: "ctrl--", "-".
        let (modifiers, key) = match text.strip_suffix("--") {
            Some(modifiers) => (modifiers, "-"),
            None if text == "-" => ("", "-"),
            None => text.rsplit_once('-').unwrap_or(("", text)),
        };
        let mut mods = Mods::default();
        for modifier in modifiers.split('-').filter(|_| !modifiers.is_empty()) {
            let held = match modifier {
                // `secondary` is how configurations for the GPUI-based sig said ctrl.
                "ctrl" | "secondary" => &mut mods.ctrl,
                "alt" => &mut mods.alt,
                "shift" => &mut mods.shift,
                "super" => &mut mods.logo,
                "altgr" => &mut mods.alt_graph,
                other => {
                    return Err(format!(
                        "unknown modifier {other:?} (use ctrl, alt, shift, super or altgr)"
                    ))
                }
            };
            *held = true;
        }
        let key = if let Some(code) = key.strip_prefix("code:") {
            let code = (0..=255)
                .map(KeyCode)
                .find(|candidate| candidate.w3c_code() == Some(code))
                .ok_or_else(|| format!("unknown key code {code:?}"))?;
            ChordKey::Code(code)
        } else if let Some(lower) = letter(key) {
            mods.shift |= key != lower;
            ChordKey::Character(lower)
        } else if key.chars().count() == 1 && !key.starts_with(char::is_control) {
            ChordKey::Character(key.to_owned())
        } else if key == "space" {
            ChordKey::Character(" ".into())
        } else if let Some((_, name)) = KEY_NAMES.iter().find(|(alias, _)| *alias == key) {
            ChordKey::Named((*name).into())
        } else if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            match n {
                1..=24 => ChordKey::Named(format!("F{n}")),
                _ => return Err(format!("there is no key {key:?}")),
            }
        } else if key.starts_with(|c: char| c.is_ascii_uppercase())
            && key.len() > 1
            && key.chars().all(|c| c.is_ascii_alphanumeric())
        {
            // A W3C key name as it is, such as "BrowserBack" or "AudioVolumeUp".
            ChordKey::Named(key.into())
        } else if key.is_empty() {
            return Err("the keystroke names no key".into());
        } else {
            return Err(format!("unknown key {key:?}"));
        };
        Ok(Self { mods, key })
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Mods {
            ctrl,
            alt,
            shift,
            logo,
            alt_graph,
        } = self.mods;
        let names = [
            (ctrl, "ctrl-"),
            (alt, "alt-"),
            (shift, "shift-"),
            (logo, "super-"),
            (alt_graph, "altgr-"),
        ];
        for (_, name) in names.iter().filter(|(held, _)| *held) {
            f.write_str(name)?;
        }
        match &self.key {
            ChordKey::Character(c) if c == " " => f.write_str("space"),
            ChordKey::Character(c) | ChordKey::Named(c) => f.write_str(c),
            ChordKey::Code(code) => write!(f, "code:{}", code.w3c_code().unwrap_or_default()),
        }
    }
}

/// The longest [`UrlPattern`], in bytes.
pub const MAX_URL_PATTERN: usize = 2048;

/// An address pattern for bindings that apply on some addresses: `scheme://host`, optionally
/// followed by a path (`sig://terminal`, `https://*.example.com/docs`). No regular expressions.
///
/// It matches an address ([`TabInfo::url`], the committed one) whose scheme and host are the
/// pattern's, ASCII case aside, and whose path starts with the pattern's path at a segment
/// boundary: `sig://terminal` matches `sig://terminal` and `sig://terminal/config`, never
/// `sig://terminal-x` or `https://sig/terminal`; `https://a.org/docs` matches `/docs` and
/// `/docs/x`, not `/docsx`. A host `*.example.com` matches the hosts under example.com at any
/// depth and not example.com itself, as `[routing.<engine>] hosts` does. The address's
/// userinfo, port, query and fragment don't count. A pattern is printable ASCII: write a
/// non-ASCII host in its `xn--` form and a path as the address has it, percent-encoded.
///
/// Of several patterns that match, [`UrlPattern::most_specific`] picks the one with the most
/// specific host (an exact host before any wildcard, a longer wildcard before a shorter one),
/// then the longest path. Compared and printed in its normal form: scheme and host in lower
/// case, a path of `/` dropped.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UrlPattern {
    scheme: String,
    /// The host, or for `*.example.com` the suffix `example.com`.
    host: String,
    wildcard: bool,
    /// Empty, or starting with "/" and longer than it.
    path: String,
}

/// An address split as [`UrlPattern`] compares it: scheme, host and path, the host in lower
/// case. `None` for an address with no `//` authority (`about:blank`, `data:`).
fn address_parts(address: &str) -> Option<(&str, String, &str)> {
    let (scheme, rest) = address.split_once(':')?;
    let rest = rest.strip_prefix("//")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, rest) = rest.split_at(end);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host.strip_prefix('[') {
        Some(inner) => &host[..inner.find(']').map_or(host.len(), |end| end + 2)],
        None => host.split(':').next().unwrap_or_default(),
    };
    let path = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    Some((scheme, host.to_ascii_lowercase(), path))
}

impl UrlPattern {
    /// Whether `address` is one of the pattern's.
    pub fn matches(&self, address: &str) -> bool {
        let Some((scheme, host, path)) = address_parts(address) else {
            return false;
        };
        let host_matches = match self.wildcard {
            true => host
                .strip_suffix(&self.host)
                .is_some_and(|under| under.len() > 1 && under.ends_with('.')),
            false => host == self.host,
        };
        let path_matches = path.strip_prefix(&self.path).is_some_and(|rest| {
            rest.is_empty() || rest.starts_with('/') || self.path.ends_with('/')
        });
        scheme.eq_ignore_ascii_case(&self.scheme) && host_matches && path_matches
    }

    /// What goes with the most specific of `patterns` that matches `address`, by the order the
    /// type describes; `None` if none does. Equal patterns are equally specific, and then the
    /// first wins.
    pub fn most_specific<'a, T>(
        patterns: impl IntoIterator<Item = (&'a UrlPattern, T)>,
        address: &str,
    ) -> Option<T> {
        let mut best: Option<((usize, usize), T)> = None;
        for (pattern, item) in patterns {
            if !pattern.matches(address) {
                continue;
            }
            let host = match pattern.wildcard {
                true => pattern.host.len(),
                false => usize::MAX,
            };
            let rank = (host, pattern.path.len());
            if best.as_ref().is_none_or(|(best, _)| rank > *best) {
                best = Some((rank, item));
            }
        }
        best.map(|(_, item)| item)
    }
}

impl FromStr for UrlPattern {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        if text.len() > MAX_URL_PATTERN {
            return Err(format!(
                "an address pattern is at most {MAX_URL_PATTERN} bytes"
            ));
        }
        if let Some(c) = text.chars().find(|c| !c.is_ascii_graphic()) {
            return Err(format!(
                "{c:?} in an address pattern (write it as the address has it: a host in its \
                 xn-- form, a path percent-encoded)"
            ));
        }
        let shape = "an address pattern is scheme://host and an optional path, such as \
                     \"sig://terminal\" or \"https://*.example.com/docs\"";
        let (scheme, rest) = text.split_once("://").ok_or_else(|| shape.to_owned())?;
        let scheme_ok = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        if !scheme_ok {
            return Err(format!("{scheme:?} is not a URL scheme; {shape}"));
        }
        let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        if host.is_empty() {
            return Err(format!("the pattern names no host; {shape}"));
        }
        // An IPv6 address is in brackets, colons and all.
        let ipv6 = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'));
        let (checked, banned) = match ipv6 {
            Some(inner) => (inner, "@?#[]"),
            None => (host, "@:?#[]"),
        };
        if let Some(c) = checked.chars().find(|c| banned.contains(*c)) {
            return Err(format!(
                "{c:?} in the host: a pattern has no user, port, query or fragment"
            ));
        }
        let (wildcard, host) = match host.strip_prefix("*.") {
            Some(suffix) if !suffix.is_empty() => (true, suffix),
            Some(_) => return Err("\"*.\" needs a domain after it".into()),
            None => (false, host),
        };
        if host.contains('*') {
            return Err(
                "\"*\" is only for the start of a host, as \"*.example.com\": hosts under it"
                    .into(),
            );
        }
        if let Some(c) = path.chars().find(|c| "?#*".contains(*c)) {
            return Err(format!(
                "{c:?} in the path: a pattern matches a path's start, with no query, fragment \
                 or wildcard"
            ));
        }
        Ok(Self {
            scheme: scheme.to_ascii_lowercase(),
            host: host.to_ascii_lowercase(),
            wildcard,
            path: match path {
                "/" => String::new(),
                path => path.to_owned(),
            },
        })
    }
}

impl fmt::Display for UrlPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let star = if self.wildcard { "*." } else { "" };
        write!(f, "{}://{star}{}{}", self.scheme, self.host, self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::ButtonState;

    fn chord(text: &str) -> Chord {
        text.parse().unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    const NONE: Modifiers = Modifiers {
        shift: false,
        control: false,
        alt: false,
        alt_graph: false,
        logo: false,
        caps_lock: false,
        num_lock: false,
    };
    const CTRL: Modifiers = Modifiers {
        control: true,
        ..NONE
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };
    const CTRL_SHIFT: Modifiers = Modifiers {
        control: true,
        shift: true,
        ..NONE
    };
    const ALT_GR: Modifiers = Modifiers {
        alt_graph: true,
        ..NONE
    };

    /// A press of the key with evdev `code` that means `key`, as xkb would report it.
    fn press(code: u32, key: Key, modifiers: Modifiers, consumed: Modifiers) -> KeyEvent {
        KeyEvent {
            code: KeyCode(code),
            key,
            state: ButtonState::Pressed,
            repeat: false,
            modifiers,
            consumed,
            text: None,
        }
    }

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    fn named(name: &str) -> Key {
        Key::Named(name.into())
    }

    const KEY_T: u32 = 20;
    const KEY_2: u32 = 3;
    const KEY_TAB: u32 = 15;
    const KEY_EQUAL: u32 = 13;
    const KEY_MINUS: u32 = 12;
    const KEY_Q: u32 = 16;
    const KEY_LEFT: u32 = 105;

    #[test]
    fn parses_keystrokes() {
        assert_eq!(chord("ctrl-t").to_string(), "ctrl-t");
        assert_eq!(chord("ctrl-T"), chord("ctrl-shift-t"));
        assert_eq!(chord("secondary-t"), chord("ctrl-t"));
        assert_eq!(chord("ctrl--").to_string(), "ctrl--");
        assert_eq!(chord("-").to_string(), "-");
        assert_eq!(chord("alt-left").to_string(), "alt-ArrowLeft");
        assert_eq!(chord("f5"), chord("F5"));
        assert_eq!(chord("space").to_string(), "space");
        assert_eq!(chord("ctrl-code:KeyQ").to_string(), "ctrl-code:KeyQ");
        assert_eq!(chord("BrowserBack").to_string(), "BrowserBack");
        assert_eq!(chord("altgr-Å").to_string(), "shift-altgr-å");
    }

    #[test]
    fn rejects_bad_keystrokes() {
        for (text, error) in [
            ("g g", "sequences"),
            ("ctrl-", "no key"),
            ("hyper-t", "unknown modifier"),
            ("ctrl--t", "unknown modifier"),
            ("ctrl-code:KeyZZ", "unknown key code"),
            ("f25", "no key"),
            ("ctrl-pgup", "unknown key"),
        ] {
            let result = text.parse::<Chord>();
            assert!(
                result.as_ref().is_err_and(|e| e.contains(error)),
                "{text:?}: {result:?}"
            );
        }
    }

    #[test]
    fn letters_match_with_their_shift_state() {
        let ctrl_t = press(KEY_T, character("t"), CTRL, NONE);
        let ctrl_shift_t = press(KEY_T, character("T"), CTRL_SHIFT, SHIFT);
        // Caps Lock makes the key "T" without Shift.
        let caps = Modifiers {
            caps_lock: true,
            ..CTRL
        };
        let ctrl_t_caps = press(KEY_T, character("T"), caps, NONE);
        assert!(chord("ctrl-t").matches(&ctrl_t));
        assert!(chord("ctrl-t").matches(&ctrl_t_caps));
        assert!(!chord("ctrl-t").matches(&ctrl_shift_t));
        assert!(chord("ctrl-shift-t").matches(&ctrl_shift_t));
        assert!(chord("ctrl-T").matches(&ctrl_shift_t));
        assert!(!chord("ctrl-shift-t").matches(&ctrl_t));
    }

    #[test]
    fn characters_match_what_the_layout_types() {
        // US: Shift + = types "+", with Shift consumed.
        let us_plus = press(KEY_EQUAL, character("+"), CTRL_SHIFT, SHIFT);
        // Swedish: + has a key of its own, left of the backspace key.
        let se_plus = press(KEY_MINUS, character("+"), CTRL, NONE);
        for event in [&us_plus, &se_plus] {
            assert!(chord("ctrl-+").matches(event));
            assert!(!chord("ctrl-=").matches(event));
        }
        // The physical key is the = key on US layouts whatever it types.
        assert!(chord("ctrl-shift-code:Equal").matches(&us_plus));
        assert!(!chord("ctrl-code:Equal").matches(&us_plus));
        assert!(!chord("ctrl-shift-code:Equal").matches(&se_plus));
    }

    #[test]
    fn altgr_is_never_ctrl_alt() {
        // Swedish AltGr + 2 types "@".
        let at = press(KEY_2, character("@"), ALT_GR, ALT_GR);
        assert!(!chord("ctrl-alt-2").matches(&at));
        assert!(!chord("ctrl-alt-@").matches(&at));
        assert!(!chord("altgr-2").matches(&at));
        assert!(chord("@").matches(&at));
        // Ctrl + Alt + 2, where the layout types nothing else, is Ctrl + Alt.
        let ctrl_alt = Modifiers { alt: true, ..CTRL };
        assert!(chord("ctrl-alt-2").matches(&press(KEY_2, character("2"), ctrl_alt, NONE)));
        // AltGr on a key that types nothing must be asked for.
        let left = press(KEY_LEFT, named("ArrowLeft"), ALT_GR, NONE);
        assert!(!chord("left").matches(&left));
        assert!(chord("altgr-left").matches(&left));
    }

    #[test]
    fn named_keys_keep_their_modifiers() {
        // xkb consumes Shift on Tab (it becomes ISO_Left_Tab), but Shift+Tab is still shifted.
        let shift_tab = press(KEY_TAB, named("Tab"), CTRL_SHIFT, SHIFT);
        assert!(chord("ctrl-shift-tab").matches(&shift_tab));
        assert!(!chord("ctrl-tab").matches(&shift_tab));
        assert!(chord("ctrl-tab").matches(&press(KEY_TAB, named("Tab"), CTRL, NONE)));
    }

    #[test]
    fn physical_keys_ignore_the_layout() {
        // On a Dvorak layout the key where Q is on QWERTY types "'".
        let event = press(KEY_Q, character("'"), CTRL, NONE);
        assert!(chord("ctrl-code:KeyQ").matches(&event));
        assert!(!chord("ctrl-q").matches(&event));
    }

    #[test]
    fn keystrokes_that_type() {
        for (text, types) in [
            ("j", true),
            ("J", true),
            ("space", true),
            ("ctrl-j", false),
            ("altgr-j", false),
            ("enter", false),
            ("code:KeyJ", false),
        ] {
            assert_eq!(chord(text).types(), types, "{text}");
        }
    }

    fn pattern(text: &str) -> UrlPattern {
        text.parse().unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    #[test]
    fn url_patterns_match_scheme_host_and_a_path_prefix() {
        let terminal = pattern("sig://terminal");
        for address in [
            "sig://terminal",
            "sig://terminal/",
            "sig://terminal/config",
            "SIG://Terminal/x?y#z",
            "sig://user@terminal:9/a",
        ] {
            assert!(terminal.matches(address), "{address}");
        }
        for address in [
            "sig://terminal-x",
            "sig://terminalx/",
            "https://sig/terminal",
            "sig://config",
            "sig:terminal",
            "about:blank",
            "",
            "terminal",
        ] {
            assert!(!terminal.matches(address), "{address}");
        }
        let docs = pattern("https://Example.ORG/docs");
        assert_eq!(docs.to_string(), "https://example.org/docs");
        assert!(docs.matches("https://example.org/docs"));
        assert!(docs.matches("https://example.org/docs/a?b"));
        assert!(docs.matches("https://example.org/docs#x"));
        assert!(!docs.matches("https://example.org/docsx"));
        assert!(
            !docs.matches("https://example.org/Docs"),
            "paths keep their case"
        );
        assert!(!docs.matches("https://example.org/"));
        assert!(!docs.matches("http://example.org/docs"));
        let slash = pattern("https://example.org/docs/");
        assert!(slash.matches("https://example.org/docs/x"));
        assert!(!slash.matches("https://example.org/docs"));
        // A path of "/" is no path.
        assert_eq!(
            pattern("https://example.org/"),
            pattern("https://EXAMPLE.org")
        );
        // IPv6 hosts keep their brackets.
        let local = pattern("http://[::1]/a");
        assert!(local.matches("http://[::1]:8080/a/b"));
        assert!(!local.matches("http://[::2]/a"));
    }

    #[test]
    fn a_wildcard_host_is_the_hosts_under_it() {
        let under = pattern("https://*.Example.com");
        assert_eq!(under.to_string(), "https://*.example.com");
        assert!(under.matches("https://a.example.com/"));
        assert!(under.matches("https://a.b.EXAMPLE.com/x"));
        assert!(!under.matches("https://example.com/"));
        assert!(!under.matches("https://.example.com/"));
        assert!(!under.matches("https://badexample.com/"));
    }

    #[test]
    fn the_most_specific_pattern_wins() {
        let patterns = [
            "https://*.example.com",
            "https://*.a.example.com",
            "https://b.a.example.com",
            "https://b.a.example.com/docs",
            "https://b.a.example.com/docs/deep",
        ]
        .map(pattern);
        let pick = |address: &str| UrlPattern::most_specific(patterns.iter().zip(0..), address);
        assert_eq!(pick("https://b.a.example.com/docs/deep/x"), Some(4));
        assert_eq!(pick("https://b.a.example.com/docs/x"), Some(3));
        assert_eq!(pick("https://b.a.example.com/"), Some(2));
        assert_eq!(pick("https://c.a.example.com/docs"), Some(1));
        assert_eq!(pick("https://c.example.com/"), Some(0));
        assert_eq!(pick("https://example.com/"), None);
        // An exact host beats a wildcard with a longer path.
        let both = [pattern("https://*.a.org/x/y/z"), pattern("https://w.a.org")];
        assert_eq!(
            UrlPattern::most_specific(both.iter().zip(0..), "https://w.a.org/x/y/z"),
            Some(1)
        );
    }

    #[test]
    fn bad_url_patterns_say_why() {
        let long = format!("https://a.org/{}", "x".repeat(MAX_URL_PATTERN));
        for (text, error) in [
            ("terminal", "scheme://host"),
            ("sig:terminal", "scheme://host"),
            ("://terminal", "not a URL scheme"),
            ("1x://a", "not a URL scheme"),
            ("sig://", "no host"),
            ("https:///docs", "no host"),
            ("https://a.org:8080", "port"),
            ("https://me@a.org", "user"),
            ("https://a.org?q", "query"),
            ("https://a.org/x?q", "query"),
            ("https://a.org/x#f", "fragment"),
            ("https://*.", "a domain after it"),
            ("https://a.*.org", "only for the start"),
            ("https://*a.org", "only for the start"),
            ("https://a.org/*", "wildcard"),
            ("https://bücher.de", "xn--"),
            ("https://a.org/a b", "' '"),
            (long.as_str(), "at most 2048 bytes"),
        ] {
            let result = text.parse::<UrlPattern>();
            assert!(
                result.as_ref().is_err_and(|e| e.contains(error)),
                "{text:?}: {result:?}"
            );
        }
    }

    #[test]
    fn modifiers_locks_and_compositions_are_not_keys_of_a_sequence() {
        let is = |key: Key| is_modifier(&press(0, key, NONE, NONE));
        for name in [
            "Shift", "Control", "AltGraph", "Meta", "CapsLock", "Compose",
        ] {
            assert!(is(named(name)), "{name}");
        }
        assert!(is(Key::Dead(Some('¨'))));
        assert!(!is(character("g")));
        assert!(!is(named("Enter")));
        assert!(!is(Key::Unidentified));
    }
}
