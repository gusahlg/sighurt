//! The `sig` import module: requests to the browser, terminals, and the ordered input queue.
//! Mirrors `engines/sighurt-engine-wasm/src/pages.rs`, which documents the ABI.

/// What a request asks for (`request`'s `kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Kind {
    /// Facts about the browser: the payload names them (`about`, `engines`, `commands`,
    /// `user-commands`, `keys`, `paths`, `settings`).
    Info = 1,
    /// The configuration file's text.
    ConfigRead = 2,
    /// The problems of a configuration, one per line.
    ConfigCheck = 3,
    /// Replacing the configuration file.
    ConfigWrite = 4,
    /// A terminal running a program.
    PtyOpen = 5,
    /// Putting text on the clipboard.
    ClipboardCopy = 6,
}

/// What [`PtyOpen`](Kind::PtyOpen) runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Program {
    /// The user's shell.
    Shell = 0,
    /// The user's editor on the configuration file.
    EditConfig = 1,
}

#[link(wasm_import_module = "sig")]
extern "C" {
    #[link_name = "request"]
    fn _request(kind: u32, ptr: u32, len: u32) -> u32;
    #[link_name = "poll"]
    fn _poll(id: u32) -> i32;
    #[link_name = "reply"]
    fn _reply(id: u32, ptr: u32, cap: u32) -> i32;
    #[link_name = "cancel"]
    fn _cancel(id: u32) -> i32;
    #[link_name = "pty_read"]
    fn _pty_read(id: u32, ptr: u32, cap: u32) -> i32;
    #[link_name = "pty_write"]
    fn _pty_write(id: u32, ptr: u32, len: u32) -> i32;
    #[link_name = "pty_resize"]
    fn _pty_resize(id: u32, cols: u32, rows: u32) -> i32;
    #[link_name = "input_next"]
    fn _input_next(ptr: u32, cap: u32) -> i32;
}

/// A request the browser has not been told to forget. Dropping it cancels it.
#[derive(Debug, PartialEq, Eq)]
pub struct Request(u32);

/// Where a request is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    /// Not answered yet.
    Waiting,
    /// Answered, with this text (empty for a terminal).
    Done(String),
    /// Refused or failed, with why.
    Failed(String),
    /// The browser knows no such request.
    Gone,
}

impl Request {
    /// Asks for `kind` with `payload`; `None` when the browser refuses at once (the page lacks
    /// the grant, too many requests are held, a malformed payload).
    pub fn new(kind: Kind, payload: &[u8]) -> Option<Self> {
        let id = unsafe { _request(kind as u32, payload.as_ptr() as u32, payload.len() as u32) };
        (id != 0).then_some(Self(id))
    }

    /// Facts named `what` (`"commands"`, `"keys"`, ...).
    pub fn info(what: &str) -> Option<Self> {
        Self::new(Kind::Info, what.as_bytes())
    }

    /// The configuration file.
    pub fn config_read() -> Option<Self> {
        Self::new(Kind::ConfigRead, &[])
    }

    /// The problems of `text` as a configuration.
    pub fn config_check(text: &str) -> Option<Self> {
        Self::new(Kind::ConfigCheck, text.as_bytes())
    }

    /// Replacing the configuration file with `text`, if it still holds `replaces`.
    pub fn config_write(text: &str, replaces: &str) -> Option<Self> {
        let mut payload = Vec::with_capacity(4 + replaces.len() + text.len());
        payload.extend_from_slice(&(replaces.len() as u32).to_le_bytes());
        payload.extend_from_slice(replaces.as_bytes());
        payload.extend_from_slice(text.as_bytes());
        Self::new(Kind::ConfigWrite, &payload)
    }

    /// A terminal of `cols` by `rows` cells running `program`.
    pub fn pty_open(program: Program, cols: u16, rows: u16) -> Option<Self> {
        let mut payload = [0u8; 5];
        payload[0] = program as u8;
        payload[1..3].copy_from_slice(&cols.to_le_bytes());
        payload[3..5].copy_from_slice(&rows.to_le_bytes());
        Self::new(Kind::PtyOpen, &payload)
    }

    /// Putting `text` on the clipboard. Needs a click or key press in the page in the last 5 s.
    pub fn clipboard_copy(text: &str) -> Option<Self> {
        Self::new(Kind::ClipboardCopy, text.as_bytes())
    }

    /// Its id, which names a terminal it opened.
    pub fn id(&self) -> u32 {
        self.0
    }

    /// Where it is, with the answer's text once there is one.
    pub fn poll(&self) -> Poll {
        match unsafe { _poll(self.0) } {
            0 => Poll::Waiting,
            1 => Poll::Done(self.text()),
            2 => Poll::Failed(self.text()),
            _ => Poll::Gone,
        }
    }

    fn text(&self) -> String {
        let mut buf = vec![0u8; 4096];
        loop {
            let len = unsafe { _reply(self.0, buf.as_mut_ptr() as u32, buf.len() as u32) };
            let Ok(len) = usize::try_from(len) else {
                return String::new();
            };
            if len <= buf.len() {
                buf.truncate(len);
                return String::from_utf8_lossy(&buf).into_owned();
            }
            buf.resize(len, 0);
        }
    }

    /// The terminal this answered request opened.
    pub fn pty(&self) -> Pty<'_> {
        Pty(self)
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        unsafe { _cancel(self.0) };
    }
}

/// A terminal an answered [`Request::pty_open`] opened; it ends when the request is dropped.
#[derive(Debug)]
pub struct Pty<'a>(&'a Request);

impl Pty<'_> {
    /// The program's output waiting, into `buf`: how many bytes, `Some(0)` for none now, `None`
    /// once it has ended.
    pub fn read(&self, buf: &mut [u8]) -> Option<usize> {
        let n = unsafe { _pty_read(self.0 .0, buf.as_mut_ptr() as u32, buf.len() as u32) };
        usize::try_from(n).ok()
    }

    /// Gives the program `bytes` as input: how many it took, `None` once it has ended.
    pub fn write(&self, bytes: &[u8]) -> Option<usize> {
        let n = unsafe { _pty_write(self.0 .0, bytes.as_ptr() as u32, bytes.len() as u32) };
        usize::try_from(n).ok()
    }

    /// Tells the program the terminal is `cols` by `rows` cells.
    pub fn resize(&self, cols: u16, rows: u16) -> bool {
        unsafe { _pty_resize(self.0 .0, u32::from(cols), u32::from(rows)) == 0 }
    }
}

/// What an input record is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// A key press.
    Press,
    /// An auto-repeat of a key held down.
    Repeat,
    /// Text no key press typed: a terminal's paste, or an input method's commit.
    Paste,
}

/// Modifier bits of an input record.
pub mod modifiers {
    /// Shift.
    pub const SHIFT: u8 = 1;
    /// Control.
    pub const CONTROL: u8 = 2;
    /// Alt.
    pub const ALT: u8 = 4;
    /// Super.
    pub const SUPER: u8 = 8;
    /// AltGr.
    pub const ALT_GRAPH: u8 = 16;
    /// Caps Lock is on.
    pub const CAPS_LOCK: u8 = 32;
    /// Num Lock is on.
    pub const NUM_LOCK: u8 = 64;
}

/// One record of the input queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    /// What it is.
    pub kind: InputKind,
    /// The [`modifiers`] held.
    pub modifiers: u8,
    /// The key's evdev code; 0 for a paste.
    pub code: u32,
    /// The key as W3C `KeyboardEvent.key` names it (`"a"`, `"Enter"`, `"ArrowUp"`); empty for a
    /// paste.
    pub key: String,
    /// What the press typed, or the pasted text.
    pub text: String,
}

impl Input {
    /// Whether every modifier in `mask` is held.
    pub fn has(&self, mask: u8) -> bool {
        self.modifiers & mask == mask
    }

    /// Whether it is a press or a repeat of the key named `key`, with no modifier but those in
    /// `allowed` (the locks never count).
    pub fn is_key(&self, key: &str, allowed: u8) -> bool {
        let locks = modifiers::CAPS_LOCK | modifiers::NUM_LOCK;
        self.kind != InputKind::Paste && self.key == key && self.modifiers & !(allowed | locks) == 0
    }

    /// Reads a record as `input_next` wrote it.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let header = bytes.get(..8)?;
        let kind = match header[0] {
            1 => InputKind::Press,
            2 => InputKind::Repeat,
            3 => InputKind::Paste,
            _ => return None,
        };
        let key_len = usize::from(u16::from_le_bytes([header[2], header[3]]));
        let code = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let key = bytes.get(8..8 + key_len)?;
        let text = &bytes[8 + key_len..];
        Some(Self {
            kind,
            modifiers: header[1],
            code,
            key: String::from_utf8_lossy(key).into_owned(),
            text: String::from_utf8_lossy(text).into_owned(),
        })
    }
}

/// The next record of the input queue, if any.
pub fn input_next() -> Option<Input> {
    let mut buf = vec![0u8; 256];
    loop {
        let len = unsafe { _input_next(buf.as_mut_ptr() as u32, buf.len() as u32) };
        let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
        if len <= buf.len() {
            return Input::parse(&buf[..len]);
        }
        buf.resize(len, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_parse() {
        let mut record = vec![1, modifiers::CONTROL, 5, 0, 30, 0, 0, 0];
        record.extend_from_slice(b"Enter");
        record.extend_from_slice(b"\r");
        let input = Input::parse(&record).unwrap();
        assert_eq!(input.kind, InputKind::Press);
        assert_eq!(
            (input.code, input.key.as_str(), input.text.as_str()),
            (30, "Enter", "\r")
        );
        assert!(input.has(modifiers::CONTROL) && !input.has(modifiers::SHIFT));
        assert!(input.is_key("Enter", modifiers::CONTROL));
        assert!(!input.is_key("Enter", 0));
        let paste = Input::parse(&[3, 0, 0, 0, 0, 0, 0, 0, b'h', b'i']).unwrap();
        assert_eq!((paste.kind, paste.text.as_str()), (InputKind::Paste, "hi"));
        assert!(Input::parse(&[9, 0, 0, 0, 0, 0, 0, 0]).is_none());
        assert!(Input::parse(&[1, 0, 9, 0, 0, 0, 0, 0, b'a']).is_none());
    }
}
