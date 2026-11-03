//! The chrome API: what a chrome implements so that `sig` can run it in place of the standard
//! one, and what `sig` and a chrome exchange.
//!
//! A chrome is compiled into `sig` and listed in its registry as a [`ChromeKind`]; `[ui]
//! chrome` picks one when `sig` starts. `sig` drives it through the [`Chrome`] trait: events in
//! ([`ChromeEvent`]), actions out ([`ChromeAction`]), a layout of named surfaces
//! ([`ChromeLayout`]) and drawing each surface with a [`Pen`], whose scene the backend `sig`
//! chose (`[ui] renderer`) renders. sighurt-ui's own chrome goes through the same trait
//! ([`crate::standard`]), and its text, editing and drawing helpers ([`crate::Fonts`],
//! [`crate::text_edit::TextEditor`], [`Pen`]) are there for other chromes to use.
//!
//! The contract between `sig` and a chrome:
//!
//! - **The chrome lays out; the host validates.** `sig` keeps one target for each surface name
//!   the chrome has used, at most [`MAX_SURFACES`]. A listed surface is placed at its rectangle,
//!   clipped to the window, and takes pointer input on its [`InputArea`], clipped to it; one
//!   with an empty rectangle has no room and is unmapped. A name the chrome used before but no
//!   longer lists is hidden: unmapped, keeping its last rectangle. Engine views go at
//!   [`ChromeLayout::page`], the chrome's surfaces above them in the chrome's order.
//! - **Drawing is a scene:** [`Chrome::draw`] draws a surface whole with a [`Pen`] (its clear
//!   colour first), in window-logical coordinates; it names no pixel format, buffer or
//!   rasterizer. The fonts are shared: `sig` loads them once and hands the chrome a [`Fonts`]
//!   ([`ChromeKind::new`]) and the backend the same font system. A scene equal to the surface's
//!   last one is not rendered again.
//! - **Memory is bounded by the host:** at most 3 buffers per surface at its latest size, plus
//!   ones the compositor has yet to release; a hidden surface keeps its buffers. A surface as
//!   big as the page holds about 30 MB a buffer at 1920 × 1000 and 200 %. A native backend's
//!   swapchain holds 2 to 4 images of the surface's size, the same order.
//! - **The page area when no engine shows** (`sig://home`, errors, a blank tab) is the chrome's
//!   to draw, on a surface of its own at the page's rectangle, listed only while it is needed.
//! - **Damage is the chrome's to report** ([`ChromeAction::Repaint`]): an identical snapshot,
//!   pointer motion within a control and anything the chrome doesn't show report nothing.
//!   `sig` sends a [`ChromeEvent::Snapshot`] only when the core says something the chrome could
//!   show changed, never for a page's frame. Nothing in the API ticks.
//! - **Every string in a snapshot arrives contained** ([`contained`], with the bounds each
//!   field of [`Snapshot`] states). A chrome draws page text only where page text belongs, and
//!   never shows a title as the address. [`readable_url`] decodes an address for showing it, as
//!   far as that is safe.
//! - **A chrome shows `Snapshot::status` whenever it is `Some`**, visibly, cut to fit if it
//!   must: it carries `sig`'s own notices (configuration errors, downloads, refused prompts).
//! - **The core decides where the keyboard is.** The chrome follows [`ChromeEvent::Focus`] and
//!   reports the moves it makes itself ([`ChromeAction::Focus`]). While the chrome has the
//!   keyboard, the host's keyboard focus goes to the surface of its latest [`TextField`], or its
//!   first listed surface; while the page has it and no engine view shows, to the chrome's
//!   surface at the page's rectangle.
//! - **Tabs by identity:** a click on a tab is [`ChromeAction::SelectTab`] or
//!   [`ChromeAction::CloseTab`], never a position, so a tab list that changed since the snapshot
//!   can't make it act on another tab.
//! - **Chrome commands:** a chrome offers actions ([`ChromeKind::commands`]) that keys and
//!   `:commands` run as `chrome.<action>`; `sig` hands each to the chrome as
//!   [`ChromeEvent::Command`]. They take no argument and carry no identity: like keys, they act
//!   on the chrome as it is.
//!
//! Chromes are trusted code compiled into `sig`. The API keeps correct code correct; like the
//! in-process engines, it is not a sandbox. There is no stability promise yet: see
//! docs/rewrite/extension-api.md, §2.10, and its changelog; `tests/extension.rs` uses the API
//! from outside the crate.
//!
//! Must not own: browser state (the core's), targets and placement (`sig` and the host), or
//! what a key press does (the keymap's).

use std::borrow::Cow;
use std::fmt;

use sighurt_api::commands::Command;
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{LogicalSize, SurfaceRect, WindowRect};
use sighurt_api::ids::{RequestId, TabId};
use sighurt_api::input::{
    FocusEvent, KeyEvent, KeyboardFocus, PointerEvent, TextInputEvent, TextInputState,
};
use sighurt_api::settings::Table;

use sighurt_render::Pen;

use crate::chrome::Colors;
use crate::state::Snapshot;
use crate::text::{Fonts, DEFAULT_FONT_SIZE};

/// A chrome's own name for one of its surfaces ("chrome", "page").
pub type SurfaceName = &'static str;

/// The most surfaces a chrome may use. `sig` refuses a ninth name, with a diagnostic.
pub const MAX_SURFACES: usize = 8;

/// A chrome: the window's tab list, address line and whatever else it shows, drawn as scenes on
/// the surfaces it lays out.
pub trait Chrome {
    /// Handles one event and returns what the executable should do, in order.
    fn handle(&mut self, event: ChromeEvent) -> Vec<ChromeAction>;

    /// Where the page goes and where the chrome's surfaces are. Read after every `handle`.
    fn layout(&self) -> &ChromeLayout;

    /// Draws `surface` with `pen`, whole: the surface's clear colour first ([`Pen::clear`]),
    /// then everything it shows. The pen's rectangle is the surface's, where the latest layout
    /// placed it ([`Pen::rect`]), window-logical like everything drawn.
    fn draw(&mut self, surface: SurfaceName, pen: &mut Pen);
}

/// What happens to a chrome.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum ChromeEvent {
    /// The shared look (`[ui]`) and the chrome's own `[chrome.<name>]`, which its kind's check
    /// accepted. Comes first of all, and after every valid reload of the configuration.
    Configure {
        /// `[ui]`.
        theme: Theme,
        /// `[chrome.<name>]`, empty when the configuration has none.
        settings: Table,
    },
    /// The window's size; before the first drawing and on every change.
    Resize(LogicalSize),
    /// What the browser shows now, after every change the core made that a chrome could show.
    /// Never sent because a page drew a frame.
    Snapshot(Snapshot),
    /// The core moved the keyboard. `Chrome`: focus the line with its text selected. `Page`:
    /// leave the line and drop an edit.
    Focus(KeyboardFocus),
    /// The window gained or lost the keyboard.
    WindowFocus(FocusEvent),
    /// Pointer input on one of the chrome's surfaces, in its coordinates. The mouse's side
    /// buttons never come here: they run the commands of `[input.buttons]`, wherever the
    /// pointer is.
    Pointer {
        /// The surface.
        surface: SurfaceName,
        /// The event.
        event: PointerEvent,
    },
    /// A key the keymap didn't take, while the chrome has the keyboard.
    Key(KeyEvent),
    /// Input-method text, while the chrome has the keyboard.
    TextInput(TextInputEvent),
    /// The clipboard's text for [`ChromeAction::Paste`] with `request`.
    Pasted {
        /// The request.
        request: RequestId,
        /// The text.
        text: String,
    },
    /// `chrome.<action>` ran: `action` is one of the kind's [`ChromeKind::commands`].
    Command(String),
}

/// What a chrome asks the executable to do.
#[derive(Clone, Debug, PartialEq)]
pub enum ChromeAction {
    /// Run this command: a button, Enter in the address line, a `:command`.
    Command(Command),
    /// Select this tab: a click on it.
    SelectTab(TabId),
    /// Close this tab: its close button.
    CloseTab(TabId),
    /// The chrome moved the keyboard itself: tell the core. The move is done, so it doesn't
    /// come back as [`ChromeEvent::Focus`].
    Focus(KeyboardFocus),
    /// Put this text on the clipboard.
    Copy(String),
    /// Read the clipboard and hand its text back as [`ChromeEvent::Pasted`] with this request,
    /// which the chrome made with `RequestId::next`.
    Paste(RequestId),
    /// The text field the chrome edits changed, for the input method; `None` turns it off.
    TextInput(Option<TextField>),
    /// Show this pointer over the chrome's surfaces.
    Cursor(Cursor),
    /// This surface shows something new: draw it again.
    Repaint(SurfaceName),
}

/// The text field a chrome edits, for the input method. The caret is in `surface`'s
/// coordinates.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub struct TextField {
    /// The surface the field is on.
    pub surface: SurfaceName,
    /// The field, as the input method sees it.
    pub state: TextInputState,
}

impl TextField {
    /// A field on `surface`.
    pub fn new(surface: SurfaceName, state: TextInputState) -> Self {
        Self { surface, state }
    }
}

/// Where the page and the chrome's surfaces go, in window-logical coordinates.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChromeLayout {
    /// Where the shown view goes. Empty when there is no room.
    pub page: WindowRect,
    /// The surfaces the chrome shows now, bottom to top, all above the page.
    pub surfaces: Vec<ChromeSurface>,
}

impl ChromeLayout {
    /// A layout.
    pub fn new(page: WindowRect, surfaces: Vec<ChromeSurface>) -> Self {
        Self { page, surfaces }
    }
}

/// One surface of a chrome.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeSurface {
    /// Its name.
    pub name: SurfaceName,
    /// Where it is in the window. Empty when the layout has no room for it.
    pub rect: WindowRect,
    /// Where it takes pointer input.
    pub input: InputArea,
}

impl ChromeSurface {
    /// A surface at `rect` that takes input on all of it.
    pub fn new(name: SurfaceName, rect: WindowRect) -> Self {
        Self {
            name,
            rect,
            input: InputArea::Whole,
        }
    }
}

/// Where a chrome surface takes pointer input. A transparent pixel alone doesn't let input
/// through to what is below.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum InputArea {
    /// All of it.
    #[default]
    Whole,
    /// These rectangles, in the surface's coordinates. None at all: no input.
    Rects(Vec<SurfaceRect>),
}

/// The look every chrome shares: `[ui]`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// `[ui.colors]` over the defaults.
    pub colors: Colors,
    /// `[ui] font`: a family to set the chrome's text in. It need not be installed.
    pub font: Option<String>,
    /// `[ui] font-size`, in logical pixels.
    pub font_size: u16,
}

impl Default for Theme {
    /// The default colors and size, and no font named.
    fn default() -> Self {
        Self {
            colors: Colors::default(),
            font: None,
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

/// A chrome a build can offer. Each chrome crate exports one, as `KIND`. Printed by its name.
#[derive(Clone, Copy)]
pub struct ChromeKind {
    /// Its name in `[ui] chrome`.
    pub name: &'static str,
    /// Everything wrong with `[chrome.<name>]`, one line each; empty if the table is valid.
    pub check: fn(&Table) -> Vec<String>,
    /// The actions it takes as `chrome.<action>` commands, which take no argument.
    pub commands: &'static [&'static str],
    /// A new chrome shaping its text with `fonts` (the font system `sig` shares with its
    /// renderer), which is configured before anything else.
    pub new: fn(Fonts) -> Box<dyn Chrome>,
}

impl fmt::Debug for ChromeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChromeKind({})", self.name)
    }
}

/// Untrusted text made safe to show on one line: control characters become spaces; bidi
/// embedding, override and isolate controls are dropped, so the text can't reorder what
/// follows it; at most `max_chars` characters are kept.
pub fn contained(text: &str, max_chars: usize) -> String {
    if is_contained(text, max_chars) {
        return text.to_owned();
    }
    text.chars()
        .filter(|c| !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max_chars)
        .collect()
}

/// Whether [`contained`] would return `text` unchanged: printable ASCII within `max_chars`
/// (a byte scan; text with any other character takes the slow path).
pub(crate) fn is_contained(text: &str, max_chars: usize) -> bool {
    text.len() <= max_chars && text.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// A committed address as an address line shows it: percent-encoded UTF-8 in its path, query
/// and fragment decoded, as browsers show it (`/R%C3%A4ksm%C3%B6rg%C3%A5s#K%C3%A4llor` reads
/// `/Räksmörgås#Källor`). What could hide or fake part of the address stays encoded: every
/// ASCII character (`%2F` stays `%2F`, `%20` stays `%20`), and whitespace, control, bidi and
/// invisible formatting characters and padlocks; so does anything that isn't UTF-8. The scheme
/// and host stay as they are (an internationalized host keeps its `xn--` form).
///
/// Typed back into the line, the shown form opens the same address: a URL parser encodes those
/// characters back to the same bytes (in upper-case hex, which servers treat the same).
pub fn readable_url(url: &str) -> Cow<'_, str> {
    let Some(start) = path_start(url) else {
        return Cow::Borrowed(url);
    };
    let (head, mut rest) = url.split_at(start);
    if !rest.contains('%') {
        return Cow::Borrowed(url);
    }
    let mut shown = String::with_capacity(url.len());
    shown.push_str(head);
    while let Some(percent) = rest.find('%') {
        shown.push_str(&rest[..percent]);
        rest = &rest[percent..];
        // The run of escapes from here, as bytes.
        let bytes: Vec<u8> = rest
            .as_bytes()
            .chunks(3)
            .map_while(|escape| match escape {
                [b'%', high, low] => Some(hex(*high)? << 4 | hex(*low)?),
                _ => None,
            })
            .collect();
        if bytes.is_empty() {
            shown.push('%');
            rest = &rest[1..];
            continue;
        }
        let mut i = 0;
        while i < bytes.len() {
            match decoded(&bytes[i..]) {
                Some((c, len)) if shows_safely(c) => {
                    shown.push(c);
                    i += len;
                }
                _ => {
                    shown.push_str(&rest[i * 3..i * 3 + 3]);
                    i += 1;
                }
            }
        }
        rest = &rest[bytes.len() * 3..];
    }
    shown.push_str(rest);
    Cow::Owned(shown)
}

/// Where the part of `url` after its scheme and host starts, if it starts with a scheme.
fn path_start(url: &str) -> Option<usize> {
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    let first = scheme.chars().next()?;
    let valid = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.');
    if !first.is_ascii_alphabetic() || !scheme.chars().all(valid) {
        return None;
    }
    let after = colon + 1;
    Some(match url[after..].strip_prefix("//") {
        Some(authority) => after + 2 + authority.find(['/', '?', '#']).unwrap_or(authority.len()),
        None => after,
    })
}

/// The value of an ASCII hex digit.
fn hex(digit: u8) -> Option<u8> {
    char::from(digit).to_digit(16).map(|value| value as u8)
}

/// The non-ASCII character `bytes` start with, and its length, if they start with one in UTF-8.
fn decoded(bytes: &[u8]) -> Option<(char, usize)> {
    let len = match bytes.first()? {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return None,
    };
    let c = std::str::from_utf8(bytes.get(..len)?)
        .ok()?
        .chars()
        .next()?;
    Some((c, len))
}

/// Whether `c` can be shown decoded in an address: not whitespace, a control character or one
/// that is invisible or reorders text, nor a padlock that could pass for the browser's own.
fn shows_safely(c: char) -> bool {
    let hidden = matches!(c,
        '\u{ad}' | '\u{34f}' | '\u{61c}' | '\u{115f}' | '\u{1160}' | '\u{17b4}' | '\u{17b5}'
        | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{2800}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}'
        | '\u{feff}' | '\u{ffa0}' | '\u{fff0}'..='\u{fffb}' | '\u{1bca0}'..='\u{1bca3}'
        | '\u{1d173}'..='\u{1d17a}' | '\u{1f50f}' | '\u{1f510}' | '\u{1f512}' | '\u{1f513}'
        | '\u{e0000}'..='\u{e0fff}');
    !hidden && !c.is_control() && !c.is_whitespace()
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_ascii_fast_path_agrees_with_containing() {
        for text in [
            "plain https://x.test/a%20b",
            "tab\there",
            "del\u{7f}",
            "bidi\u{202e}x",
            "ä",
            "",
        ] {
            for max in [0, 3, 100] {
                let slow: String = text
                    .chars()
                    .filter(|c| !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .take(max)
                    .collect();
                assert_eq!(contained(text, max), slow, "{text:?} {max}");
                if is_contained(text, max) {
                    assert_eq!(slow, text);
                }
            }
        }
    }
    use super::*;

    #[test]
    fn addresses_read_decoded() {
        let cases = [
            (
                "https://sv.wikipedia.org/wiki/R%C3%A4ksm%C3%B6rg%C3%A5s#K%C3%A4llor",
                "https://sv.wikipedia.org/wiki/Räksmörgås#Källor",
            ),
            ("demo://slow?%C3%A5%C3%A4%C3%B6", "demo://slow?åäö"),
            (
                "https://a.example/?q=%E6%97%A5%E6%9C%AC",
                "https://a.example/?q=日本",
            ),
            ("https://a.example/#%F0%9F%A6%80", "https://a.example/#🦀"),
            // Lower-case hex too, and a character between two that stay encoded.
            ("https://a.example/%c3%a5", "https://a.example/å"),
            (
                "https://a.example/%20%C3%A5%2F",
                "https://a.example/%20å%2F",
            ),
            // Without a host.
            ("data:text/plain,%C3%A5", "data:text/plain,å"),
            // Only the incomplete character stays encoded.
            ("https://a.example/%C3%A5%C3", "https://a.example/å%C3"),
        ];
        for (url, shown) in cases {
            assert_eq!(readable_url(url), shown, "{url}");
        }
    }

    #[test]
    fn what_could_hide_or_fake_the_address_stays_encoded() {
        for url in [
            // ASCII, whatever it means here: a space, a slash, a percent sign, a letter.
            "https://a.example/a%20b%2Fc%25d%41%3F%23",
            // Bidi controls, which could reorder what follows.
            "https://a.example/%E2%80%AEexe.txt",
            "https://a.example/%E2%81%A6x%D8%9C",
            // Whitespace that could push the rest out of view, and invisible characters.
            "https://a.example/%C2%A0%E3%80%80%E2%80%8B%C2%AD%EF%BB%BF%E2%A0%80%C2%85",
            // A padlock, which could pass for the browser's own.
            "https://a.example/%F0%9F%94%92",
            // Control characters, and what isn't UTF-8 (Latin-1, an overlong "/", a lone byte).
            "https://a.example/%C2%80%7F%E5%C0%AF%A5%C3%28",
            // Hosts stay as they are, internationalized or percent-encoded.
            "http://xn--rksmrgs-5wao1o.example/",
            "foo://h%C3%A5st/",
            // Not an address at all.
            "r%C3%A4k: no scheme",
            "%C3%A5",
        ] {
            assert_eq!(readable_url(url), url, "{url}");
        }
    }

    /// The shown form, typed back, is the same address, and nothing in it is anything
    /// `contained` would change.
    #[test]
    fn the_shown_form_opens_the_same_address() {
        for url in [
            "https://sv.wikipedia.org/wiki/R%C3%A4ksm%C3%B6rg%C3%A5s#K%C3%A4llor",
            "https://a.example/s%C3%B6k?q=bl%C3%A5b%C3%A4r%20sylt&x=%E2%80%AE#%C3%A5%20%C3%A4",
            "https://a.example/%E6%97%A5%2F%25%F0%9F%94%92%F0%9F%A6%80",
            "demo://slow?%C3%A5%C3%A4%C3%B6",
            "file:///home/anv%C3%A4ndare/r%C3%A4k.html",
            "data:text/plain,%C3%A5%E5",
        ] {
            let shown = readable_url(url);
            assert_ne!(shown, url, "something decodes in {url}");
            let parsed = url::Url::parse(&shown).unwrap();
            assert_eq!(parsed.as_str(), url, "{shown}");
            assert_eq!(contained(&shown, usize::MAX), shown);
        }
    }
}
