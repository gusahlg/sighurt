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
//!   - `2` config read: no payload.
//!   - `3` config check: the payload is a configuration.
//!   - `4` config write: the payload is the byte length of `replaces` (u32), `replaces` (what
//!     the page read), then the new text.
//!
//!   The replies of `2` to `4` are text in the form `sighurt_runtime::page_config`'s module
//!   documentation gives (read: `file|new|read-only <path>`, `target <resolved target>`, then
//!   the text; check: `valid` or `invalid` and their `notice`/`problem` lines; write: `saved`
//!   and its `restart`/`notice` lines, or `stale`, `invalid` or `refused` with a message).
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

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::Result;
use sighurt_api::events::EngineEvent;
use sighurt_api::ids::{DocumentGeneration, RequestId, ViewId};
use sighurt_api::input::{Key, KeyEvent, Modifiers};
use sighurt_api::services::{
    ClipboardData, PageGrant, PageInfo, PageRequest, PtyProgram, Selection, Service, ServiceReply,
    ServiceRequest, ServiceResult,
};
use wasmtime::{Caller, Linker};

use crate::capabilities::{guest_bytes, write_prefix, HostState};
use crate::pty::{self, Pty};
use crate::view::Outbox;

/// The most requests one document may hold, answered ones included until it cancels them.
pub const MAX_REQUESTS: usize = 16;
/// The most records the input queue holds.
pub const MAX_INPUT_RECORDS: usize = 512;
/// The most bytes of records the input queue holds.
pub const MAX_INPUT_BYTES: usize = 256 * 1024;
/// The most bytes of one request's payload (a configuration is at most 256 KiB, twice for a
/// write).
pub const MAX_PAYLOAD: usize = 1024 * 1024;

/// Imports of the `oxide` module an internal page may not have: everything that reaches the
/// network or the user's files and devices, loads code, or starts threads. A page importing one
/// fails to load, so its document has none of these whatever their policies say.
pub const DENIED_IMPORTS: [&str; 20] = [
    "api_fetch",
    "api_ws_",
    "api_sse_",
    "api_rtc_",
    "api_load_module",
    "api_spawn_worker",
    "api_worker_",
    "api_file_",
    "api_folder_",
    "api_upload_file",
    "api_camera_",
    "api_microphone_",
    "api_screen_capture",
    "api_audio_play_url",
    "api_video_load_url",
    "api_video_hls_",
    "api_midi_",
    "api_download_url",
    "api_get_location",
    "api_gpu_",
];

/// Whether an internal page may import `name` from `oxide`.
pub fn allowed_import(name: &str) -> bool {
    !DENIED_IMPORTS.iter().any(|denied| name.starts_with(denied))
}

/// One internal page `sig` has: its name in `sig://<name>`, the title its tab shows, its module,
/// and what it may ask for. `sighurt-pages`' table, handed to the engine through
/// [`WasmConfig::pages`](crate::WasmConfig::pages).
#[derive(Clone, Copy, Debug)]
pub struct InternalPage {
    /// Its name: `help` for `sig://help`.
    pub name: &'static str,
    /// The title its tab shows.
    pub title: &'static str,
    /// Its module.
    pub wasm: &'static [u8],
    /// What it may ask for.
    pub grants: &'static [PageGrant],
}

impl InternalPage {
    /// Whether it has `grant`.
    pub fn may(&self, grant: PageGrant) -> bool {
        self.grants.contains(&grant)
    }

    /// Whether it edits text that a paste may go to (the configuration, a terminal), so its
    /// view says it has a text field.
    pub fn edits(&self) -> bool {
        self.may(PageGrant::ConfigWrite) || self.may(PageGrant::Pty)
    }
}

/// The name of the page a `sig:` URL loads: its host, in lower case, as the core canonicalises
/// it. `None` for any other URL.
pub fn page_name(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let plain = url.username().is_empty() && url.password().is_none() && url.port().is_none();
    (url.scheme() == "sig" && plain)
        .then(|| url.host_str().map(str::to_ascii_lowercase))
        .flatten()
        .filter(|name| !name.is_empty())
}

/// An internal document's privileged state: which page it is, and what it asked for and was
/// given. Made for each document ([`crate::view::document_state`]) and never for a worker or a
/// module it loads.
pub struct Internal {
    /// The page.
    pub page: InternalPage,
    state: Mutex<PageState>,
}

#[derive(Default)]
struct PageState {
    /// The last guest id handed out, for requests and terminals alike.
    last: u32,
    requests: BTreeMap<u32, Asked>,
    terminals: BTreeMap<u32, Terminal>,
    /// Requests the guest withdrew that the browser was asked: to tell it.
    withdrawn: Vec<RequestId>,
    input: VecDeque<Vec<u8>>,
    input_bytes: usize,
}

struct Asked {
    /// What to ask the browser, until it was asked.
    service: Option<Service>,
    /// The browser's id of it, once asked.
    browser: Option<RequestId>,
    answer: Answer,
}

enum Answer {
    Waiting,
    Done(String),
    Failed(String),
}

struct Terminal {
    /// What to ask the browser, until it was asked.
    open: Option<(PtyProgram, u16, u16)>,
    browser: Option<RequestId>,
    status: i32,
    pty: Option<Pty>,
}

impl Internal {
    /// A new document of `page`.
    pub fn new(page: InternalPage) -> Self {
        Self {
            page,
            state: Mutex::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, PageState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The document's origin: `sig://<name>`.
    pub fn origin(&self) -> String {
        format!("sig://{}", self.page.name)
    }

    /// Whether the guest asked for or withdrew something the browser has not been told of yet
    /// ([`Internal::report`]): a request, a terminal to open, a request or terminal it gave up.
    /// The worker reports a step that leaves any, as it does a navigation, even when the frame
    /// stayed the same.
    pub fn unreported(&self) -> bool {
        let state = self.lock();
        !state.withdrawn.is_empty()
            || state.requests.values().any(|asked| asked.service.is_some())
            || state
                .terminals
                .values()
                .any(|terminal| terminal.open.is_some())
    }

    /// The guest ended (a trap, a limit): drops every request, every terminal with its master
    /// (which hangs the program's terminal up) and the queued input, at once, rather than when a
    /// later answer arrives, which the browser sends no more for a document that failed. The
    /// browser cancels the document's requests itself, so nothing is reported.
    pub fn revoke(&self) {
        let mut state = self.lock();
        state.requests.clear();
        state.terminals.clear();
        state.withdrawn.clear();
        state.input.clear();
        state.input_bytes = 0;
    }

    /// Tells the browser what the guest asked for and withdrew since the last call.
    pub fn report(&self, view: ViewId, document: DocumentGeneration, out: &mut Outbox) {
        let origin = self.origin();
        let mut state = self.lock();
        let mut ask = |service: Service| {
            let id = RequestId::next();
            out.push(EngineEvent::ServiceRequested(ServiceRequest {
                id,
                view,
                document,
                origin: Some(origin.clone()),
                service,
            }));
            id
        };
        let PageState {
            requests,
            terminals,
            withdrawn,
            ..
        } = &mut *state;
        for asked in requests.values_mut() {
            if let Some(service) = asked.service.take() {
                asked.browser = Some(ask(service));
            }
        }
        for terminal in terminals.values_mut() {
            if let Some((program, cols, rows)) = terminal.open.take() {
                let request = PageRequest::PtyOpen {
                    program,
                    cols,
                    rows,
                };
                terminal.browser = Some(ask(Service::Page(request)));
            }
        }
        for request in withdrawn.drain(..) {
            out.push(EngineEvent::ServiceCancelled { view, request });
        }
    }

    /// The browser answered `request`, if it is one of this document's: whether it was. A
    /// terminal's master is taken here ([`pty::take`]); one that can't be is given back
    /// ([`EngineEvent::ServiceCancelled`]).
    pub fn resolve(
        &self,
        view: ViewId,
        request: RequestId,
        result: ServiceResult,
        out: &mut Outbox,
    ) -> bool {
        let mut state = self.lock();
        if let Some(asked) = state
            .requests
            .values_mut()
            .find(|asked| asked.browser == Some(request))
        {
            asked.answer = match result {
                Ok(ServiceReply::Text(text)) => Answer::Done(text),
                Ok(_) => Answer::Done(String::new()),
                Err(error) => Answer::Failed(error.to_string()),
            };
            return true;
        }
        let Some(terminal) = state
            .terminals
            .values_mut()
            .find(|terminal| terminal.browser == Some(request))
        else {
            return false;
        };
        match result {
            Ok(ServiceReply::Pty(session)) if terminal.status == pty_status::OPENING => {
                match pty::take(session) {
                    Some(master) => {
                        terminal.pty = Some(Pty::attach(master));
                        terminal.status = pty_status::RUNNING;
                    }
                    None => {
                        terminal.status = pty_status::CANCELLED;
                        out.push(EngineEvent::ServiceCancelled { view, request });
                    }
                }
            }
            Ok(ServiceReply::PtyEnded { status }) => {
                // The terminal stays until the page has read what the program wrote last (it
                // reads its end) or closes it: the program's end comes from the runtime, and
                // may come before the page has read its output.
                terminal.status = match status {
                    Some(code) => pty_status::EXITED + (code & 0xff),
                    None => pty_status::SIGNALLED,
                };
            }
            Ok(_) => {}
            Err(_) => {
                terminal.pty = None;
                if terminal.status < pty_status::EXITED {
                    terminal.status = pty_status::CANCELLED;
                }
            }
        }
        true
    }

    /// Queues a key press or repeat the view got.
    pub fn key(&self, event: &KeyEvent) {
        let kind = match event.repeat {
            true => record::REPEAT,
            false => record::PRESS,
        };
        let key = match &event.key {
            Key::Character(text) => text.as_str(),
            Key::Named(name) => name.as_str(),
            Key::Dead(_) => "Dead",
            Key::Unidentified => "Unidentified",
        };
        let text = event.text.as_deref().unwrap_or_default();
        self.queue(kind, modifiers(event.modifiers), event.code.0, key, text);
    }

    /// Queues text no key press typed: a paste, or an input method's.
    pub fn text(&self, text: &str, paste: bool) {
        let kind = match paste {
            true => record::PASTE,
            false => record::COMMIT,
        };
        self.queue(kind, 0, 0, "", text);
    }

    fn queue(&self, kind: u8, modifiers: u8, code: u32, key: &str, text: &str) {
        let key = &key.as_bytes()[..key.len().min(usize::from(u16::MAX))];
        let mut record = Vec::with_capacity(8 + key.len() + text.len());
        record.push(kind);
        record.push(modifiers);
        record.extend_from_slice(&(key.len() as u16).to_le_bytes());
        record.extend_from_slice(&code.to_le_bytes());
        record.extend_from_slice(key);
        record.extend_from_slice(text.as_bytes());
        let mut state = self.lock();
        let full = state.input.len() >= MAX_INPUT_RECORDS
            || state.input_bytes + record.len() > MAX_INPUT_BYTES;
        if !full {
            state.input_bytes += record.len();
            state.input.push_back(record);
        }
    }

    /// A new id for a request or a terminal; 0 never.
    fn next(state: &mut PageState) -> u32 {
        state.last = state.last.wrapping_add(1).max(1);
        state.last
    }
}

/// The modifier bits of an input record.
fn modifiers(m: Modifiers) -> u8 {
    [
        m.shift,
        m.control,
        m.alt,
        m.logo,
        m.alt_graph,
        m.caps_lock,
        m.num_lock,
    ]
    .iter()
    .enumerate()
    .fold(0, |bits, (i, &held)| bits | (u8::from(held) << i))
}

/// The page's state, if this store is an internal document's.
fn internal(caller: &Caller<'_, HostState>) -> Option<Arc<Internal>> {
    caller.data().internal.clone()
}

/// What a `request` of `kind` with `payload` asks the browser, if it is well formed.
fn service(kind: Kind, payload: &[u8]) -> Option<Service> {
    let text = || String::from_utf8(payload.to_vec()).ok();
    Some(match kind {
        Kind::Info => Service::Page(PageRequest::Info(PageInfo::from_name(&text()?)?)),
        Kind::ConfigRead if payload.is_empty() => Service::Page(PageRequest::ConfigRead),
        Kind::ConfigRead => return None,
        Kind::ConfigCheck => Service::Page(PageRequest::ConfigCheck { text: text()? }),
        Kind::ConfigWrite => {
            let len = u32::from_le_bytes(payload.get(..4)?.try_into().ok()?) as usize;
            let replaces = payload.get(4..4usize.checked_add(len)?)?;
            let new = &payload[4 + len..];
            Service::Page(PageRequest::ConfigWrite {
                text: String::from_utf8(new.to_vec()).ok()?,
                replaces: String::from_utf8(replaces.to_vec()).ok()?,
            })
        }
        Kind::ClipboardCopy => Service::ClipboardWrite {
            selection: Selection::Clipboard,
            items: vec![ClipboardData {
                mime_type: "text/plain;charset=utf-8".into(),
                data: text()?.into_bytes(),
            }],
        },
    })
}

/// Registers the `sig` module. Only for an internal document: [`crate::runtime::instantiate`]
/// calls it when its state's [`HostState::internal`] is set, and every function checks again.
pub fn register_page_functions(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap(
        "sig",
        "request",
        |caller: Caller<'_, HostState>, kind: u32, ptr: u32, len: u32| -> u32 {
            let Some(page) = internal(&caller) else {
                return 0;
            };
            let Some(kind) = Kind::from_u32(kind).filter(|kind| page.page.may(kind.grant())) else {
                return 0;
            };
            if len as usize > MAX_PAYLOAD {
                return 0;
            }
            let Some(service) = guest_bytes(&caller, ptr, len).and_then(|p| service(kind, &p))
            else {
                return 0;
            };
            let mut state = page.lock();
            if state.requests.len() >= MAX_REQUESTS {
                return 0;
            }
            let id = Internal::next(&mut state);
            let asked = Asked {
                service: Some(service),
                browser: None,
                answer: Answer::Waiting,
            };
            state.requests.insert(id, asked);
            id
        },
    )?;
    linker.func_wrap(
        "sig",
        "poll",
        |caller: Caller<'_, HostState>, id: u32| -> i32 {
            let Some(page) = internal(&caller) else {
                return -1;
            };
            let state = page.lock();
            match state.requests.get(&id).map(|asked| &asked.answer) {
                Some(Answer::Waiting) => 0,
                Some(Answer::Done(_)) => 1,
                Some(Answer::Failed(_)) => 2,
                None => -1,
            }
        },
    )?;
    linker.func_wrap(
        "sig",
        "reply",
        |mut caller: Caller<'_, HostState>, id: u32, ptr: u32, cap: u32| -> i32 {
            let Some(page) = internal(&caller) else {
                return -1;
            };
            let text = match page.lock().requests.get(&id).map(|asked| &asked.answer) {
                Some(Answer::Done(text) | Answer::Failed(text)) => text.clone(),
                _ => return -1,
            };
            write_prefix(&mut caller, ptr, cap, text.as_bytes());
            i32::try_from(text.len()).unwrap_or(i32::MAX)
        },
    )?;
    linker.func_wrap(
        "sig",
        "cancel",
        |caller: Caller<'_, HostState>, id: u32| -> i32 {
            let Some(page) = internal(&caller) else {
                return -1;
            };
            let mut state = page.lock();
            let Some(asked) = state.requests.remove(&id) else {
                return -1;
            };
            if let (Some(request), Answer::Waiting) = (asked.browser, asked.answer) {
                state.withdrawn.push(request);
            }
            0
        },
    )?;
    linker.func_wrap(
        "sig",
        "pty_open",
        |caller: Caller<'_, HostState>, program: u32, cols: u32, rows: u32| -> u32 {
            let Some(page) = internal(&caller).filter(|page| page.page.may(PageGrant::Pty)) else {
                return 0;
            };
            let program = match program {
                0 => PtyProgram::Shell,
                1 => PtyProgram::EditConfig,
                _ => return 0,
            };
            let size = |n: u32| n.clamp(1, 1000) as u16;
            let mut state = page.lock();
            if !state.terminals.is_empty() {
                return 0;
            }
            let id = Internal::next(&mut state);
            let terminal = Terminal {
                open: Some((program, size(cols), size(rows))),
                browser: None,
                status: pty_status::OPENING,
                pty: None,
            };
            state.terminals.insert(id, terminal);
            id
        },
    )?;
    linker.func_wrap(
        "sig",
        "pty_status",
        |caller: Caller<'_, HostState>, handle: u32| -> i32 {
            let Some(page) = internal(&caller) else {
                return -1;
            };
            let state = page.lock();
            state.terminals.get(&handle).map_or(-1, |t| t.status)
        },
    )?;
    linker.func_wrap(
        "sig",
        "pty_read",
        |mut caller: Caller<'_, HostState>, handle: u32, ptr: u32, cap: u32| -> i32 {
            let Some(page) = internal(&caller) else {
                return -1;
            };
            let mut buf = vec![0u8; (cap as usize).min(64 * 1024)];
            let read = {
                let mut state = page.lock();
                let Some(terminal) = state.terminals.get_mut(&handle) else {
                    return -1;
                };
                match &terminal.pty {
                    Some(pty) => match pty.read(&mut buf) {
                        Ok(n) => n,
                        Err(pty::Ended) => {
                            terminal.pty = None;
                            if terminal.status == pty_status::RUNNING {
                                terminal.status = pty_status::EOF;
                            }
                            return -1;
                        }
                    },
                    None if terminal.status == pty_status::OPENING => 0,
                    None => return -1,
                }
            };
            write_prefix(&mut caller, ptr, cap, &buf[..read]).map_or(-1, |n| n as i32)
        },
    )?;
    linker.func_wrap(
        "sig",
        "pty_write",
        |caller: Caller<'_, HostState>, handle: u32, ptr: u32, len: u32| -> i32 {
            let Some(page) = internal(&caller) else {
                return -1;
            };
            let Some(bytes) = guest_bytes(&caller, ptr, len.min(64 * 1024)) else {
                return -1;
            };
            let state = page.lock();
            match state.terminals.get(&handle).and_then(|t| t.pty.as_ref()) {
                Some(pty) => pty.write(&bytes).map_or(-1, |n| n as i32),
                None => -1,
            }
        },
    )?;
    linker.func_wrap(
        "sig",
        "pty_resize",
        |caller: Caller<'_, HostState>, handle: u32, cols: u32, rows: u32| -> i32 {
            let Some(page) = internal(&caller) else {
                return -1;
            };
            let size = |n: u32| n.clamp(1, 1000) as u16;
            let state = page.lock();
            match state.terminals.get(&handle).and_then(|t| t.pty.as_ref()) {
                Some(pty) => pty.resize(size(cols), size(rows)).map_or(-1, |()| 0),
                None => -1,
            }
        },
    )?;
    linker.func_wrap(
        "sig",
        "pty_close",
        |caller: Caller<'_, HostState>, handle: u32| -> i32 {
            let Some(page) = internal(&caller) else {
                return -1;
            };
            let mut state = page.lock();
            let Some(terminal) = state.terminals.remove(&handle) else {
                return -1;
            };
            // Its queued input goes with it; the browser ends the program.
            drop(terminal.pty);
            if let Some(request) = terminal.browser {
                state.withdrawn.push(request);
            }
            0
        },
    )?;
    linker.func_wrap(
        "sig",
        "input_next",
        |mut caller: Caller<'_, HostState>, ptr: u32, cap: u32| -> i32 {
            let Some(page) = internal(&caller) else {
                return 0;
            };
            let record = {
                let mut state = page.lock();
                let Some(next) = state.input.front() else {
                    return 0;
                };
                if next.len() > cap as usize {
                    return next.len() as i32;
                }
                let record = state.input.pop_front().expect("there is one");
                state.input_bytes -= record.len();
                record
            };
            write_prefix(&mut caller, ptr, cap, &record);
            record.len() as i32
        },
    )?;
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::HostState;
    use crate::engine::WasmEngine;
    use crate::{SandboxPolicy, WasmConfig};

    /// A guest whose `start_app` asks for the commands' info, then for the clipboard, and keeps
    /// the two answers in memory at 0 and 4.
    const ASKING: &str = r#"(module
        (import "sig" "request" (func $request (param i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        (data (i32.const 16) "commands")
        (func (export "start_app")
            (i32.store (i32.const 0) (call $request (i32.const 1) (i32.const 16) (i32.const 8)))
            (i32.store (i32.const 4) (call $request (i32.const 5) (i32.const 16) (i32.const 8)))))"#;

    fn page(name: &'static str, wat: &str, grants: &'static [PageGrant]) -> InternalPage {
        let wasm: &'static [u8] = Box::leak(wat::parse_str(wat).unwrap().into_boxed_slice());
        InternalPage {
            name,
            title: "a page",
            wasm,
            grants,
        }
    }

    fn engine(pages: Vec<InternalPage>) -> WasmEngine {
        WasmEngine::new(WasmConfig {
            policy: SandboxPolicy::default(),
            kv_root: std::env::temp_dir().join("sighurt-wasm-pages-test-kv"),
            aot_cache: None,
            pages,
        })
        .unwrap()
    }

    /// A document of `url`, as a view of the engine makes one.
    fn document(engine: &WasmEngine, url: &str) -> HostState {
        let page = HostState {
            engine: Some(engine.clone()),
            ..HostState::default()
        };
        crate::view::document_state(&page, url, Default::default())
    }

    #[test]
    fn only_an_internal_document_links_the_sig_module() {
        let help = page("help", ASKING, &[PageGrant::Info]);
        let engine = engine(vec![help]);
        // The same module, as a page from the web or a file: no such import.
        let module = engine.compile_module(help.wasm).unwrap();
        for url in ["https://a.test/app.wasm", "file:///tmp/app.wasm"] {
            let state = document(&engine, url);
            assert!(state.internal.is_none());
            let error = crate::runtime::instantiate(&engine, state, &module)
                .err()
                .expect("no sig module");
            assert!(format!("{error:#}").contains("sig"), "{url}: {error:#}");
        }
        // As sig://help, from the table.
        let state = document(&engine, "sig://help");
        assert!(state.internal.is_some());
        crate::runtime::load(&engine, &state, "sig://help").unwrap();
        assert_eq!(*state.module_origin.lock().unwrap(), "sig://help");
        assert!(state.net_policy().is_offline());
    }

    #[test]
    fn a_request_without_the_grant_is_refused_and_tells_nobody() {
        let help = page("help", ASKING, &[PageGrant::Info]);
        let engine = engine(vec![help]);
        let state = document(&engine, "sig://help");
        let loaded = crate::runtime::load(&engine, &state, "sig://help").unwrap();
        drop(loaded);
        let internal = state.internal.clone().unwrap();
        // The Info request got an id, the clipboard (no grant) none.
        assert_eq!(internal.lock().requests.len(), 1);
        let (view, document) = (ViewId::next(), DocumentGeneration::next());
        let mut out = Outbox::default();
        internal.report(view, document, &mut out);
        let events = out.take();
        let [EngineEvent::ServiceRequested(request)] = events.as_slice() else {
            panic!("{events:?}");
        };
        assert_eq!(request.origin.as_deref(), Some("sig://help"));
        assert_eq!(
            request.service,
            Service::Page(PageRequest::Info(PageInfo::Commands))
        );
        // Asked once only.
        internal.report(view, document, &mut out);
        assert!(out.take().is_empty());
        // A stale answer, for a request it never made, is dropped; its own is kept.
        let stale = RequestId::next();
        assert!(!internal.resolve(view, stale, Ok(ServiceReply::Done), &mut out));
        let text = Ok(ServiceReply::Text("tab.new".into()));
        assert!(internal.resolve(view, request.id, text, &mut out));
        let state = internal.lock();
        let answer = state.requests.values().next().map(|asked| &asked.answer);
        assert!(matches!(answer, Some(Answer::Done(text)) if text == "tab.new"));
    }

    #[test]
    fn an_internal_page_may_not_import_the_network_or_code_loading() {
        let fetching = r#"(module
            (import "oxide" "api_fetch_begin" (func (param i32 i32 i32 i32 i32 i32 i32) (result i32)))
            (func (export "start_app")))"#;
        for (name, wat) in [
            ("help", fetching),
            (
                "help",
                r#"(module (import "oxide" "api_load_module" (func (param i32 i32) (result i32))) (func (export "start_app")))"#,
            ),
            (
                "help",
                r#"(module (import "oxide" "api_spawn_worker" (func (param i32 i32) (result i32))) (func (export "start_app")))"#,
            ),
            (
                "help",
                r#"(module (import "env" "x" (func)) (func (export "start_app")))"#,
            ),
        ] {
            let engine = engine(vec![page(name, wat, &[PageGrant::Info])]);
            let state = document(&engine, "sig://help");
            let error = crate::runtime::load(&engine, &state, "sig://help")
                .err()
                .unwrap();
            assert!(error.to_string().contains("may not import"), "{error:#}");
        }
        assert!(allowed_import("api_canvas_text") && allowed_import("api_push_state"));
        assert!(!allowed_import("api_ws_connect") && !allowed_import("api_rtc_create_peer"));
    }

    #[test]
    fn requests_parse_their_payloads() {
        let info = service(Kind::Info, b"keys");
        assert_eq!(info, Some(Service::Page(PageRequest::Info(PageInfo::Keys))));
        assert_eq!(service(Kind::Info, b"nope"), None);
        assert_eq!(service(Kind::ConfigRead, b"x"), None);
        let mut write = 3u32.to_le_bytes().to_vec();
        write.extend_from_slice(b"oldnew");
        assert_eq!(
            service(Kind::ConfigWrite, &write),
            Some(Service::Page(PageRequest::ConfigWrite {
                text: "new".into(),
                replaces: "old".into()
            }))
        );
        assert_eq!(service(Kind::ConfigWrite, &[9, 0, 0, 0, b'x']), None);
        assert_eq!(service(Kind::ConfigWrite, &[1, 0]), None);
        assert!(matches!(
            service(Kind::ClipboardCopy, b"hi"),
            Some(Service::ClipboardWrite { .. })
        ));
        assert_eq!(Kind::from_u32(9), None);
        assert_eq!(info_names().count(), PageInfo::ALL.len());
    }

    #[test]
    fn page_names_are_hosts_in_lower_case() {
        assert_eq!(page_name("sig://Help/keys"), Some("help".into()));
        assert_eq!(page_name("sig:help"), None);
        assert_eq!(page_name("sig://me@help"), None);
        assert_eq!(page_name("https://help/"), None);
        // Two pages never share an origin, so neither can push_state into the other.
        let origin = crate::url::app_origin_of;
        assert_eq!(origin("sig://help/keys"), "sig://help");
        assert_ne!(origin("sig://help"), origin("sig://config"));
    }

    #[test]
    fn the_input_queue_keeps_order_and_bounds() {
        let internal = Internal::new(page("help", ASKING, &[PageGrant::Info]));
        let key = KeyEvent {
            code: sighurt_api::input::KeyCode(28),
            key: Key::Named("Enter".into()),
            state: sighurt_api::input::ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            consumed: Modifiers::default(),
            text: Some("\r".into()),
        };
        internal.key(&key);
        internal.text("a\u{1b}b\n", true);
        internal.text("漢", false);
        let state = internal.lock();
        let records: Vec<&Vec<u8>> = state.input.iter().collect();
        assert_eq!(records[0][..8], [record::PRESS, 2, 5, 0, 28, 0, 0, 0]);
        assert_eq!(&records[0][8..], b"Enter\r");
        // A paste keeps its control characters; the guest decides.
        assert_eq!(records[1][0], record::PASTE);
        assert_eq!(&records[1][8..], "a\u{1b}b\n".as_bytes());
        assert_eq!(records[2][0], record::COMMIT);
        drop(state);
        for _ in 0..MAX_INPUT_RECORDS {
            internal.text("x", false);
        }
        assert_eq!(internal.lock().input.len(), MAX_INPUT_RECORDS);
    }
}
