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
//!   - `5` clipboard copy: the payload is text, put on the clipboard as `text/plain`. The
//!     browser's rule for any page applies: a click or key press in the page in the last 5 s.
//! - `poll(id: u32) -> i32`: `0` still waiting, `1` answered, `2` failed (its message is the
//!   reply), `-1` no such request (never made, or cancelled).
//! - `reply(id: u32, ptr: u32, cap: u32) -> i32`: copies an answered or failed request's text
//!   into the `cap` bytes at `ptr`, as much as fits, and returns its whole length; `-1` while it
//!   waits or for no such request. Reading does not consume it.
//! - `cancel(id: u32) -> i32`: forgets the request: one that waits is withdrawn. `0`, or `-1`
//!   for no such request. The document's end cancels everything.
//!
//! ## Terminals
//!
//! A terminal is a handle of the document (never an OS descriptor; the master side stays in the
//! engine, which gets it from the runtime by the session, once: [`crate::pty`]). The terminal
//! lane implements the data plane; until it does a terminal opens and then ends at once.
//!
//! - `pty_open(program: u32, cols: u32, rows: u32) -> u32`: asks for a terminal of `cols` by
//!   `rows` cells running `program` (`0` the user's shell, `1` the user's editor on the
//!   configuration file). Returns its handle (never 0), or 0 when refused at once (no grant, a
//!   terminal open already in this document). It needs a click or key press in the page, which
//!   it uses up; the browser may still refuse it, which `pty_status` then says.
//! - `pty_status(handle: u32) -> i32`: `0` opening, `1` running, `2` end of file (the program
//!   closed the terminal; its status is not known yet), `3` cancelled (refused, closed, or its
//!   document lost it), `0x100 + n` exited with status `n` (0 to 255), `0x200` ended by a
//!   signal; `-1` no such handle.
//! - `pty_read(handle: u32, ptr: u32, cap: u32) -> i32`: the program's output waiting, at most
//!   `cap` bytes; `0` when there is none now, `-1` once it has ended and nothing is left.
//!   Never blocks.
//! - `pty_write(handle: u32, ptr: u32, len: u32) -> i32`: bytes taken for the program's input,
//!   perhaps fewer than `len` while its queue is full; `-1` once it has ended. Never blocks.
//! - `pty_resize(handle: u32, cols: u32, rows: u32) -> i32`: `0`, or `-1`.
//! - `pty_close(handle: u32) -> i32`: ends the program and forgets the handle; queued input is
//!   dropped. `0`, or `-1` for no such handle.
//!
//! ## Input
//!
//! Every key press, auto-repeat, paste and input method's text the view gets, in order, also
//! goes into a queue the page reads ([`MAX_INPUT_RECORDS`] records or [`MAX_INPUT_BYTES`] bytes;
//! more is dropped until the page reads), which a new document starts empty. The polled `oxide`
//! input (`key_pressed`, `text_input`) keeps working beside it.
//!
//! - `input_next(ptr: u32, cap: u32) -> i32`: the next record's length, `0` when the queue is
//!   empty. The record is written to `ptr` and taken from the queue only if it fits in `cap`
//!   bytes; a larger one stays queued for a call with room.
//!
//! A record is an 8-byte header, then the key, then the text:
//!
//! | Bytes | Field |
//! |---|---|
//! | 0 | kind: `1` key press, `2` auto-repeat, `3` paste (a terminal's paste, kept whole: tabs, newlines and control characters as pasted), `4` an input method's text |
//! | 1 | modifiers held: bit 0 Shift, 1 Control, 2 Alt, 3 Super, 4 AltGr, 5 Caps Lock, 6 Num Lock |
//! | 2..4 | the key's length in bytes, `k` (u16) |
//! | 4..8 | the key's evdev code (u32), 0 for text |
//! | 8..8+k | the key, as W3C `KeyboardEvent.key` names it (`"a"`, `"Enter"`, `"ArrowUp"`) |
//! | 8+k.. | the text: what the press types (may be empty), or the pasted text |
//!
//! A press that types text is one record holding its key and its text, delivered once. A paste
//! or an input method's text has no key.
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
    /// Putting text on the clipboard.
    ClipboardCopy = 5,
}

impl Kind {
    /// The kind numbered `kind`.
    pub fn from_u32(kind: u32) -> Option<Self> {
        Some(match kind {
            1 => Self::Info,
            2 => Self::ConfigRead,
            3 => Self::ConfigCheck,
            4 => Self::ConfigWrite,
            5 => Self::ClipboardCopy,
            _ => return None,
        })
    }

    /// The grant a page needs for it.
    pub fn grant(self) -> PageGrant {
        match self {
            Self::Info => PageGrant::Info,
            Self::ConfigRead | Self::ConfigCheck => PageGrant::ConfigRead,
            Self::ConfigWrite => PageGrant::ConfigWrite,
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
    /// A paste: a terminal's, kept whole.
    pub const PASTE: u8 = 3;
    /// An input method's text.
    pub const COMMIT: u8 = 4;
}

/// The info kinds `request` takes, by name, for the tests and the kit.
pub fn info_names() -> impl Iterator<Item = &'static str> {
    PageInfo::ALL.into_iter().map(PageInfo::name)
}

/// The `sig` functions, which the kit imports and the tests check the blobs against.
pub const FUNCTIONS: [&str; 11] = [
    "request",
    "poll",
    "reply",
    "cancel",
    "pty_open",
    "pty_status",
    "pty_read",
    "pty_write",
    "pty_resize",
    "pty_close",
    "input_next",
];

/// `pty_status` values ([`EXITED`] and [`SIGNALLED`] carry the status).
pub mod pty_status {
    /// Waiting for the browser.
    pub const OPENING: i32 = 0;
    /// The program runs.
    pub const RUNNING: i32 = 1;
    /// The program closed the terminal; its status is not known yet.
    pub const EOF: i32 = 2;
    /// Refused, closed, or its document lost it.
    pub const CANCELLED: i32 = 3;
    /// Exited: this plus the status (0 to 255).
    pub const EXITED: i32 = 0x100;
    /// Ended by a signal.
    pub const SIGNALLED: i32 = 0x200;
}
