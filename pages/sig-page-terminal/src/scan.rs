//! The pre-scan of the program's output, before the `vt100` parser: the graphics strings the
//! parser would swallow (kitty's `ESC _ G … ESC \`, sixel's `ESC P … q … ESC \`) taken out of the
//! stream, and the points between two bytes where the page must look at the screen (a switch to
//! or from the alternate screen, an erase of the whole display, a reset).
//!
//! Every byte goes through one small state machine, so a sequence split across reads is found
//! whole; everything else goes to the parser unchanged and in order. A graphics command's control
//! part is at most [`MAX_CONTROL`] bytes and a DCS introducer at most [`MAX_HEADER`]; payloads are
//! handed on in runs as they come, so no string is ever buffered whole. Other APC and DCS strings
//! (which `vt100` ignores anyway) are dropped. Like the parser, an ESC inside a string ends it:
//! `ESC \` completes it, any other ESC abandons it and starts a new sequence.
//!
//! Owns: finding these sequences. Must not own: what they mean ([`crate::graphics`],
//! [`crate::sixel`]) or the screen ([`crate::term`]).

/// Most bytes of a kitty command's control part (before `;`); a longer command is dropped.
pub const MAX_CONTROL: usize = 256;
/// Most bytes of a DCS introducer (parameters and intermediates); a longer string is dropped.
pub const MAX_HEADER: usize = 64;

const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// Where the page must look between two bytes: right after the bytes given to
/// [`Sink::text`] so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// A switch to or from the alternate screen: the last `len` bytes of text are its sequence.
    Switch { len: usize },
    /// An erase of the whole display (`ED 2`) or of it and its history (`ED 3`).
    Erase(u16),
    /// A full reset (`RIS`).
    Reset,
}

/// What the scan finds, in stream order.
pub trait Sink {
    /// Bytes for the parser.
    fn text(&mut self, bytes: &[u8]);
    fn mark(&mut self, mark: Mark);
    /// A kitty graphics command starts, with its control part (`a=T,f=100,…`).
    fn kitty_begin(&mut self, control: &[u8]);
    /// Some of its payload (base64).
    fn kitty_data(&mut self, data: &[u8]);
    /// It ended: with its terminator if `complete`, else abandoned.
    fn kitty_end(&mut self, complete: bool);
    /// A sixel string starts, its introducer's parameters (P1;P2;P3) given.
    fn sixel_begin(&mut self, params: &[u16]);
    fn sixel_data(&mut self, data: &[u8]);
    fn sixel_end(&mut self, complete: bool);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Str {
    Kitty,
    Sixel,
    /// A string nobody reads.
    Ignored,
}

#[derive(Clone, Debug, Default)]
enum State {
    #[default]
    Ground,
    /// After an ESC, held back until the next byte says what it starts.
    Esc,
    Csi(Csi),
    /// After `ESC _`: the first byte says whether it is kitty's.
    ApcStart,
    /// A kitty command's control part.
    Control(Vec<u8>),
    /// After `ESC P`: the introducer.
    Dcs(Vec<u8>),
    /// A string's payload.
    Payload(Str),
    /// An ESC in a string.
    StringEsc(Str),
}

/// A CSI sequence as far as it matters here.
#[derive(Clone, Debug, Default)]
struct Csi {
    /// Its bytes so far, `ESC [` included.
    len: usize,
    /// `?` first.
    private: bool,
    /// Another prefix (`<`, `=`, `>`) or an intermediate byte.
    other: bool,
    params: [u16; 8],
    count: usize,
    current: u16,
}

impl Csi {
    fn push(&mut self) {
        if self.count < self.params.len() {
            self.params[self.count] = self.current;
            self.count += 1;
        }
        self.current = 0;
    }

    /// The mark its final byte `last` makes, if any.
    fn mark(mut self, last: u8) -> Option<Mark> {
        self.push();
        let params = &self.params[..self.count];
        match last {
            b'h' | b'l' if self.private && !self.other => params
                .iter()
                .any(|p| matches!(p, 47 | 1047 | 1049))
                .then_some(Mark::Switch { len: self.len }),
            b'J' if !self.private && !self.other => match params[0] {
                n @ (2 | 3) => Some(Mark::Erase(n)),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The scan's state between reads.
#[derive(Clone, Debug, Default)]
pub struct Scanner {
    state: State,
}

impl Scanner {
    /// Scans `bytes`, the next of the program's output, into `sink`.
    pub fn scan(&mut self, mut bytes: &[u8], sink: &mut impl Sink) {
        while !bytes.is_empty() {
            let state = std::mem::take(&mut self.state);
            let (state, used) = step(state, bytes, sink);
            self.state = state;
            bytes = &bytes[used..];
        }
    }
}

/// One step from `state` over the start of `bytes`: the new state and how many bytes it took.
fn step(state: State, bytes: &[u8], sink: &mut impl Sink) -> (State, usize) {
    let byte = bytes[0];
    match state {
        State::Ground => {
            let run = bytes.iter().position(|&b| b == ESC).unwrap_or(bytes.len());
            if run > 0 {
                sink.text(&bytes[..run]);
                (State::Ground, run)
            } else {
                (State::Esc, 1)
            }
        }
        State::Esc => match byte {
            b'_' => (State::ApcStart, 1),
            b'P' => (State::Dcs(Vec::new()), 1),
            b'[' => {
                sink.text(b"\x1b[");
                let csi = Csi {
                    len: 2,
                    ..Csi::default()
                };
                (State::Csi(csi), 1)
            }
            ESC => {
                sink.text(b"\x1b");
                (State::Esc, 1)
            }
            b'c' => {
                sink.text(b"\x1bc");
                sink.mark(Mark::Reset);
                (State::Ground, 1)
            }
            _ => {
                sink.text(&[ESC, byte]);
                (State::Ground, 1)
            }
        },
        State::Csi(mut csi) => match byte {
            ESC => (State::Esc, 1),
            CAN | SUB => {
                sink.text(&[byte]);
                (State::Ground, 1)
            }
            _ => {
                sink.text(&[byte]);
                csi.len += 1;
                match byte {
                    b'0'..=b'9' => {
                        csi.current = csi
                            .current
                            .saturating_mul(10)
                            .saturating_add(u16::from(byte - b'0'));
                    }
                    b';' | b':' => csi.push(),
                    b'?' if csi.len == 3 => csi.private = true,
                    b'<' | b'=' | b'>' | 0x20..=0x2f => csi.other = true,
                    0x40..=0x7e => {
                        if let Some(mark) = csi.mark(byte) {
                            sink.mark(mark);
                        }
                        return (State::Ground, 1);
                    }
                    _ => {}
                }
                (State::Csi(csi), 1)
            }
        },
        State::ApcStart => match byte {
            b'G' => (State::Control(Vec::new()), 1),
            ESC => (State::StringEsc(Str::Ignored), 1),
            _ => (State::Payload(Str::Ignored), 1),
        },
        State::Control(mut control) => match byte {
            b';' => {
                sink.kitty_begin(&control);
                (State::Payload(Str::Kitty), 1)
            }
            ESC => {
                sink.kitty_begin(&control);
                (State::StringEsc(Str::Kitty), 1)
            }
            _ if control.len() >= MAX_CONTROL => (State::Payload(Str::Ignored), 1),
            _ => {
                control.push(byte);
                (State::Control(control), 1)
            }
        },
        State::Dcs(mut header) => match byte {
            ESC => (State::StringEsc(Str::Ignored), 1),
            0x40..=0x7e => {
                let plain = header.iter().all(|b| b.is_ascii_digit() || *b == b';');
                if byte == b'q' && plain {
                    let params: Vec<u16> = header
                        .split(|b| *b == b';')
                        .map(|p| {
                            p.iter().fold(0u16, |n, d| {
                                n.saturating_mul(10).saturating_add(u16::from(d - b'0'))
                            })
                        })
                        .collect();
                    sink.sixel_begin(&params);
                    (State::Payload(Str::Sixel), 1)
                } else {
                    (State::Payload(Str::Ignored), 1)
                }
            }
            _ if header.len() >= MAX_HEADER => (State::Payload(Str::Ignored), 1),
            _ => {
                header.push(byte);
                (State::Dcs(header), 1)
            }
        },
        State::Payload(kind) => {
            let run = bytes.iter().position(|&b| b == ESC).unwrap_or(bytes.len());
            if run == 0 {
                return (State::StringEsc(kind), 1);
            }
            match kind {
                Str::Kitty => sink.kitty_data(&bytes[..run]),
                Str::Sixel => sink.sixel_data(&bytes[..run]),
                Str::Ignored => {}
            }
            (State::Payload(kind), run)
        }
        State::StringEsc(kind) => {
            let complete = byte == b'\\';
            match kind {
                Str::Kitty => sink.kitty_end(complete),
                Str::Sixel => sink.sixel_end(complete),
                Str::Ignored => {}
            }
            if complete {
                (State::Ground, 1)
            } else {
                // The ESC starts something else: `byte` is read again after it.
                (State::Esc, 0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a scan found, as one string per event.
    #[derive(Default)]
    struct Seen {
        text: Vec<u8>,
        events: Vec<String>,
    }

    impl Seen {
        fn flush(&mut self) {
            if !self.text.is_empty() {
                let text = String::from_utf8_lossy(&std::mem::take(&mut self.text)).into_owned();
                self.events.push(format!("text {text:?}"));
            }
        }
    }

    impl Sink for Seen {
        fn text(&mut self, bytes: &[u8]) {
            self.text.extend_from_slice(bytes);
        }
        fn mark(&mut self, mark: Mark) {
            self.flush();
            self.events.push(format!("{mark:?}"));
        }
        fn kitty_begin(&mut self, control: &[u8]) {
            self.flush();
            self.events
                .push(format!("kitty {}", String::from_utf8_lossy(control)));
        }
        fn kitty_data(&mut self, data: &[u8]) {
            self.events
                .push(format!("data {}", String::from_utf8_lossy(data)));
        }
        fn kitty_end(&mut self, complete: bool) {
            self.events.push(format!("kitty end {complete}"));
        }
        fn sixel_begin(&mut self, params: &[u16]) {
            self.flush();
            self.events.push(format!("sixel {params:?}"));
        }
        fn sixel_data(&mut self, data: &[u8]) {
            self.events
                .push(format!("sixel data {}", String::from_utf8_lossy(data)));
        }
        fn sixel_end(&mut self, complete: bool) {
            self.events.push(format!("sixel end {complete}"));
        }
    }

    fn scan(chunks: &[&[u8]]) -> Vec<String> {
        let mut scanner = Scanner::default();
        let mut seen = Seen::default();
        for chunk in chunks {
            scanner.scan(chunk, &mut seen);
        }
        seen.flush();
        // Payload runs depend on the splits; join them.
        let mut events: Vec<String> = Vec::new();
        for event in seen.events {
            match (events.last_mut(), event.strip_prefix("data ")) {
                (Some(last), Some(data)) if last.starts_with("data ") => last.push_str(data),
                _ => events.push(event),
            }
        }
        events
    }

    #[test]
    fn text_passes_untouched() {
        let text = b"hello \x1b[31mred\x1b[0m\r\n\x1b]0;title\x07\x1b7\x1b8";
        assert_eq!(
            scan(&[text]),
            [format!("text {:?}", String::from_utf8_lossy(text))]
        );
    }

    #[test]
    fn a_kitty_command_is_taken_out_whole_however_it_is_split() {
        let stream = b"a\x1b_Ga=T,f=100;iVBORw0K\x1b\\b";
        let want = [
            "text \"a\"",
            "kitty a=T,f=100",
            "data iVBORw0K",
            "kitty end true",
            "text \"b\"",
        ];
        assert_eq!(scan(&[stream]), want);
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(scan(&bytes), want);
        // Without a payload.
        assert_eq!(scan(&[b"\x1b_Ga=d\x1b\\"]), ["kitty a=d", "kitty end true"]);
    }

    #[test]
    fn an_esc_inside_a_string_abandons_it_and_starts_a_sequence() {
        assert_eq!(
            scan(&[b"\x1b_Gi=1;AAAA\x1b[2Jx"]),
            [
                "kitty i=1",
                "data AAAA",
                "kitty end false",
                "text \"\\u{1b}[2J\"",
                "Erase(2)",
                "text \"x\""
            ]
        );
    }

    #[test]
    fn other_strings_are_dropped() {
        assert_eq!(
            scan(&[b"a\x1b_xyz\x1b\\b\x1bP$qm\x1b\\c\x1bP+q544e\x1b\\d"]),
            ["text \"abcd\""]
        );
        // A control part too long to be a command.
        let mut long = b"\x1b_G".to_vec();
        long.extend(std::iter::repeat_n(b'a', MAX_CONTROL + 10));
        long.extend_from_slice(b";data\x1b\\z");
        assert_eq!(scan(&[&long]), ["text \"z\""]);
    }

    #[test]
    fn sixel_strings_stream_with_their_parameters() {
        assert_eq!(
            scan(&[b"\x1bP0;1;0q\"1;1;2;2#0~-~\x1b\\"]),
            [
                "sixel [0, 1, 0]",
                "sixel data \"1;1;2;2#0~-~",
                "sixel end true"
            ]
        );
        assert_eq!(scan(&[b"\x1bPq#1~\x1b\\"])[0], "sixel [0]");
    }

    #[test]
    fn marks_come_where_the_screen_changes() {
        assert_eq!(
            scan(&[b"a\x1b[?1049hb\x1b[?25;1049lc\x1b[3J\x1b[J\x1b[?2004h\x1bcd"]),
            [
                "text \"a\\u{1b}[?1049h\"",
                "Switch { len: 8 }",
                "text \"b\\u{1b}[?25;1049l\"",
                "Switch { len: 11 }",
                "text \"c\\u{1b}[3J\"",
                "Erase(3)",
                "text \"\\u{1b}[J\\u{1b}[?2004h\\u{1b}c\"",
                "Reset",
                "text \"d\""
            ]
        );
        // Split anywhere.
        let stream = b"x\x1b[?47h";
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(
            scan(&bytes),
            ["text \"x\\u{1b}[?47h\"", "Switch { len: 6 }"]
        );
    }
}
