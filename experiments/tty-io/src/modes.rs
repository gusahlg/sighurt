//! The byte sequences the terminal frontend sends.
//!
//! Enter and leave are fixed slices so a signal handler can write them without
//! allocating. The kitty keyboard push asks for every flag we know how to
//! parse, including associated text (bit 16). Synchronized output brackets one
//! frame; it is not left on.

/// Disambiguate, event types, alternate keys, all keys as escapes, associated text.
pub const KITTY_KEYBOARD_FLAGS: u32 = 1 | 2 | 4 | 8 | 16;

/// Alternate screen, hidden cursor, bracketed paste, kitty keyboard push,
/// SGR mouse, cursor home, clear screen.
pub const ENTER: &[u8] =
    b"\x1b[?1049h\x1b[?25l\x1b[?2004h\x1b[>31u\x1b[?1000h\x1b[?1006h\x1b[H\x1b[2J";

/// Mouse off, kitty keyboard pop, bracketed paste off, cursor shown, leave the
/// alternate screen. The reverse of [`ENTER`] apart from the clear.
pub const LEAVE: &[u8] = b"\x1b[?1006l\x1b[?1000l\x1b[<u\x1b[?2004l\x1b[?25h\x1b[?1049l";

/// Begin a synchronized frame (`CSI ? 2026 h`).
pub const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";

/// End a synchronized frame (`CSI ? 2026 l`).
pub const SYNC_END: &[u8] = b"\x1b[?2026l";

/// One startup batch. DA1 is last so its reply is the sentinel that the
/// terminal has finished answering.
pub const QUERIES: &[u8] = b"\x1b[?u\x1b_Ga=q\x1b\\\x1bP+q524742\x1b\\\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[16t\x1b[14t\x1b[c";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_flags_match_the_push() {
        assert_eq!(KITTY_KEYBOARD_FLAGS, 31);
        assert!(ENTER
            .windows(b"[>31u".len())
            .any(|window| window == b"[>31u"));
        assert!(LEAVE.windows(3).any(|w| w == b"[<u"));
        assert!(QUERIES.ends_with(b"\x1b[c"));
    }
}
