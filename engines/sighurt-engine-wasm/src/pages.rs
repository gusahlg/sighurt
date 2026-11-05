//! Sighurt's own pages (`sig://help`, `sig://config`, `sig://terminal`): Wasm guests compiled
//! into `sig`, and the privileged import module `"sig"` that only they get.
//!
//! A view's kind is fixed by its first load: an internal page (one name of
//! [`WasmConfig::pages`](crate::WasmConfig::pages)) or an app from the web or a file. A load of
//! the other kind, or of another page, fails ([`crate::view`]). An internal document is loaded
//! from the bytes in the table, never fetched; its origin is `sig://<name>` (so two pages share
//! no storage, permission or `push_state` scope); it has no network ([`NetPolicy::offline`]),
//! no manifest, and `api_load_module`, URL workers, the file picker and media capture refuse
//! it. Workers and loaded modules never get the `sig` module.
//!
//! The `sig` module is registered in the linker only for an internal document
//! ([`HostState::internal`] is `Some`; it defaults to `None`, so everything else fails to
//! instantiate a module that imports it), and every function checks the page's grant
//! ([`PageGrant`]) again. The core then decides each request from its own record of the view's
//! page, not from anything the engine or the guest says.
//!
//! # The `sig` ABI
//!
//! Unversioned: the guests ship inside the binary they run in. Data crosses as `(ptr, len)`
//! pairs, integers little-endian, text UTF-8. `pages/sig-page-kit` holds the guest side.
//!
//! ## Requests
//!
//! - `request(kind: u32, ptr: u32, len: u32) -> u32`: asks the browser for something; the
//!   payload is `len` bytes at `ptr`. Returns the request's id (never 0), or 0 when it is
//!   refused at once: the page lacks the grant, the payload is malformed, or the document has
//!   [`MAX_REQUESTS`] requests already (answered ones count until they are cancelled). A refusal
//!   emits nothing. The kinds ([`Kind`]):
//!   - `1` info: the payload is a [`PageInfo`] name (`about`, `engines`, `commands`,
//!     `user-commands`, `keys`, `paths`, `settings`); the reply is text in the form
//!     [`PageInfo`] documents.
//!   - `2` config read: no payload; the reply is the configuration file's text.
//!   - `3` config check: the payload is a configuration; the reply is one problem per line,
//!     empty when it is valid.
//!   - `4` config write: the payload is the byte length of `replaces` (u32), `replaces` (what
//!     the page read), then the new text; the reply says what the reload did.
//!   - `5` pty open: the payload is 5 bytes, the program (u8: 0 the shell, 1 the editor on the
//!     configuration), then columns and rows (u16 each); the reply is empty and the id names
//!     the terminal for `pty_read`, `pty_write` and `pty_resize`.
//!   - `6` clipboard copy: the payload is text, put on the clipboard as `text/plain`. The
//!     browser's rule for any page applies: a click or key press in the page in the last 5 s.
//! - `poll(id: u32) -> i32`: `0` still waiting, `1` answered, `2` failed (its message is the
//!   reply), `-1` no such request (never made, or cancelled).
//! - `reply(id: u32, ptr: u32, cap: u32) -> i32`: copies an answered or failed request's text
//!   into the `cap` bytes at `ptr`, as much as fits, and returns its whole length; `-1` while it
//!   waits or for no such request. Reading does not consume it.
//! - `cancel(id: u32) -> i32`: forgets the request: one that waits is withdrawn, a terminal's
//!   program is ended. `0`, or `-1` for no such request. The document's end cancels everything.
//!
//! ## Terminals
//!
//! For an answered pty request (lane 3 of the internal pages implements the data plane in
//! [`crate::pty`]; until it does these return `-1`):
//!
//! - `pty_read(id: u32, ptr: u32, cap: u32) -> i32`: the program's output waiting, at most
//!   `cap` bytes; `0` when there is none now, `-1` once it has ended and nothing is left.
//! - `pty_write(id: u32, ptr: u32, len: u32) -> i32`: bytes taken for the program's input,
//!   perhaps fewer than `len` while its queue is full; `-1` once it has ended.
//! - `pty_resize(id: u32, cols: u32, rows: u32) -> i32`: `0`, or `-1`.
//!
//! ## Input
//!
//! Every key press, auto-repeat and text the view gets, in order, also goes into a queue the
//! page reads ([`MAX_INPUT_RECORDS`] records or [`MAX_INPUT_BYTES`] bytes; more is dropped
//! until the page reads). The polled `oxide` input (`key_pressed`, `text_input`) keeps
//! working beside it.
//!
//! - `input_next(ptr: u32, cap: u32) -> i32`: the next record's length, `0` when the queue is
//!   empty. The record is written to `ptr` and taken from the queue only if it fits in `cap`
//!   bytes; a larger one stays queued for a call with room.
//!
//! A record is an 8-byte header, then the key, then the text:
//!
//! | Bytes | Field |
//! |---|---|
//! | 0 | kind: `1` key press, `2` auto-repeat, `3` paste: text that no key press typed (a terminal's paste, or an input method's commit, which the browser can't tell apart) |
//! | 1 | modifiers held: bit 0 Shift, 1 Control, 2 Alt, 3 Super, 4 AltGr, 5 Caps Lock, 6 Num Lock |
//! | 2..4 | the key's length in bytes, `k` (u16) |
//! | 4..8 | the key's evdev code (u32), 0 for text |
//! | 8..8+k | the key, as W3C `KeyboardEvent.key` names it (`"a"`, `"Enter"`, `"ArrowUp"`) |
//! | 8+k.. | the text: what the press types (may be empty), or the pasted text |
//!
//! A press that types text is one record holding its key and its text. A paste has no key.
//!
//! Owns: the `sig` functions, an internal document's requests and input queue. Must not own:
//! deciding a request (the core), performing it (the runtime), or what the page draws.

use sighurt_api::services::{PageGrant, PageInfo};

/// The most requests one document may hold, answered ones included until it cancels them.
pub const MAX_REQUESTS: usize = 16;
/// The most records the input queue holds.
pub const MAX_INPUT_RECORDS: usize = 512;
/// The most bytes of records the input queue holds.
pub const MAX_INPUT_BYTES: usize = 256 * 1024;
/// The most bytes of one request's payload (a configuration is at most 256 KiB, twice for a
/// write).
pub const MAX_PAYLOAD: usize = 1024 * 1024;

/// What a `request` asks for: its `kind` argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Kind {
    /// [`PageInfo`] facts.
    Info = 1,
    /// The configuration file.
    ConfigRead = 2,
    /// A check of a configuration.
    ConfigCheck = 3,
    /// Writing the configuration file.
    ConfigWrite = 4,
    /// A terminal.
    PtyOpen = 5,
    /// Putting text on the clipboard.
    ClipboardCopy = 6,
}

impl Kind {
    /// The kind numbered `kind`.
    pub fn from_u32(kind: u32) -> Option<Self> {
        Some(match kind {
            1 => Self::Info,
            2 => Self::ConfigRead,
            3 => Self::ConfigCheck,
            4 => Self::ConfigWrite,
            5 => Self::PtyOpen,
            6 => Self::ClipboardCopy,
            _ => return None,
        })
    }

    /// The grant a page needs for it.
    pub fn grant(self) -> PageGrant {
        match self {
            Self::Info => PageGrant::Info,
            Self::ConfigRead | Self::ConfigCheck => PageGrant::ConfigRead,
            Self::ConfigWrite => PageGrant::ConfigWrite,
            Self::PtyOpen => PageGrant::Pty,
            Self::ClipboardCopy => PageGrant::Clipboard,
        }
    }
}

/// Input record kinds (byte 0).
pub mod record {
    /// A key press.
    pub const PRESS: u8 = 1;
    /// An auto-repeat of a key held down.
    pub const REPEAT: u8 = 2;
    /// Text no key press typed: a terminal's paste, an input method's commit.
    pub const PASTE: u8 = 3;
}

/// The info kinds `request` takes, by name, for the tests and the kit.
pub fn info_names() -> impl Iterator<Item = &'static str> {
    PageInfo::ALL.into_iter().map(PageInfo::name)
}

/// The `sig` functions, which the kit imports and the tests check the blobs against.
pub const FUNCTIONS: [&str; 8] = [
    "request",
    "poll",
    "reply",
    "cancel",
    "pty_read",
    "pty_write",
    "pty_resize",
    "input_next",
];
