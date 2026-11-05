//! The byte sequences the terminal sends and the startup queries.
//!
//! Owns: the fixed enter, leave and probe bytes. Must not own: the parser, the
//! grid, or what a reply means. Enter and leave are fixed slices so a signal
//! handler can write them without allocating. The kitty keyboard push asks for
//! every flag the parser understands, including associated text (bit 16).
//! Synchronized output brackets one frame; it is not left on. The startup
//! batch's kitty graphics query is omitted inside tmux: tmux's APC sets the
//! pane title, and tmux does not pass kitty graphics without passthrough.
//!
//! Mouse reporting is 1000 (presses and releases), then 1002 (button-event
//! tracking: motion while a button is held), in SGR form (1006). A terminal
//! without 1002 keeps 1000. 1002 is what a drag in graphics mode needs (the
//! pointer capture in `App::mouse`). 1003 (any motion) is not asked for: it
//! reports every cell the pointer crosses with no button held, which tmux with
//! `mouse on` passes on for the whole pane, and only graphics-mode hover (CSS
//! `:hover`, a link's address in the status line) would use it; text mode
//! ignores motion. Resetting any of 1000, 1002 or 1003 turns tracking off in
//! xterm and tmux, so [`LEAVE`] resets them all the same way. `App::adopt`
//! writes the same bytes when `[tui] mouse` changes.

/// Disambiguate, event types, alternate keys, all keys as escapes, associated text.
pub const KITTY_KEYBOARD_FLAGS: u32 = 1 | 2 | 4 | 8 | 16;

/// Alternate screen, hidden cursor, bracketed paste, kitty keyboard push,
/// SGR mouse with button-event tracking, focus events, numeric keypad
/// (DECKPNM, `ESC >`), cursor home, clear screen.
pub const ENTER: &[u8] =
    b"\x1b[?1049h\x1b[?25l\x1b[?2004h\x1b[>31u\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b>\x1b[H\x1b[2J";

/// Synchronized output off, then focus off, mouse off, kitty keyboard pop,
/// bracketed paste off, cursor shown, leave the alternate screen. The reverse
/// of [`ENTER`] apart from the clear. Numeric keypad (`ESC >` in [`ENTER`])
/// is left as it is. `CSI ? 2026 l` is first so a frame that
/// was cut off cannot leave the terminal inside a synchronized update.
pub const LEAVE: &[u8] =
    b"\x1b[?2026l\x1b[?1004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[<u\x1b[?2004l\x1b[?25h\x1b[?1049l";

/// Begin a synchronized frame (`CSI ? 2026 h`).
pub const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";

/// End a synchronized frame (`CSI ? 2026 l`).
pub const SYNC_END: &[u8] = b"\x1b[?2026l";

/// One startup batch. DA1 is last so its reply is the sentinel that the
/// terminal has finished answering. The graphics query is kitty's direct
/// 24-bit probe (`a=q`, image id 31). The batch written at startup leaves
/// that probe out inside tmux.
pub const QUERIES: &[u8] = b"\x1b[?u\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1bP+q524742\x1b\\\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[16t\x1b[14t\x1b[c";

/// [`QUERIES`] without the kitty graphics APC. tmux would take that APC as the pane title.
const QUERIES_WITHOUT_GRAPHICS: &[u8] =
    b"\x1b[?u\x1bP+q524742\x1b\\\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[16t\x1b[14t\x1b[c";

/// Startup batch for this terminal. The kitty graphics query is omitted when
/// `term` starts with `screen` or `tmux`, which tmux and screen set in their panes. `$TMUX`
/// is not the signal: a terminal started from inside tmux inherits it (kitty launched from a
/// tmux pane is kitty, and its `TERM` says so).
pub(super) fn startup_queries(term: Option<&std::ffi::OsStr>) -> &'static [u8] {
    if skip_kitty_graphics_query(term) {
        QUERIES_WITHOUT_GRAPHICS
    } else {
        QUERIES
    }
}

fn skip_kitty_graphics_query(term: Option<&std::ffi::OsStr>) -> bool {
    let Some(term) = term else {
        return false;
    };
    let bytes = term.as_encoded_bytes();
    bytes.starts_with(b"screen") || bytes.starts_with(b"tmux")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn kitty_flags_match_the_push() {
        assert_eq!(KITTY_KEYBOARD_FLAGS, 31);
        assert!(ENTER
            .windows(b"[>31u".len())
            .any(|window| window == b"[>31u"));
        assert!(LEAVE.windows(3).any(|w| w == b"[<u"));
        assert!(LEAVE.starts_with(SYNC_END));
        assert!(ENTER.windows(b"[?1004h".len()).any(|w| w == b"[?1004h"));
        assert!(LEAVE.windows(b"[?1004l".len()).any(|w| w == b"[?1004l"));
        // Mouse: presses, then drags, in SGR form; no any-motion tracking.
        let mouse_on = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";
        let mouse_off = b"\x1b[?1006l\x1b[?1002l\x1b[?1000l";
        assert!(ENTER.windows(mouse_on.len()).any(|w| w == mouse_on));
        assert!(LEAVE.windows(mouse_off.len()).any(|w| w == mouse_off));
        assert!(ENTER.windows(b"1003".len()).all(|w| w != b"1003"));
        // DECKPNM. A program that left DECKPAM on would make the numpad `ESC O`.
        assert!(ENTER.windows(2).any(|window| window == b"\x1b>"));
        assert!(LEAVE.windows(2).all(|window| window != b"\x1b>"));
        assert!(QUERIES
            .windows(b"_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA".len())
            .any(|window| { window == b"_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA" }));
        assert!(QUERIES.ends_with(b"\x1b[14t\x1b[c"));
    }

    #[test]
    fn tmux_skips_the_kitty_graphics_query() {
        let graphics = b"_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA";
        assert_eq!(startup_queries(Some(OsStr::new("xterm-kitty"))), QUERIES);
        assert_eq!(startup_queries(Some(OsStr::new("rio"))), QUERIES);
        assert_eq!(startup_queries(None), QUERIES);
        assert_eq!(startup_queries(Some(OsStr::new(""))), QUERIES);
        // The prefix match is case-sensitive.
        assert_eq!(startup_queries(Some(OsStr::new("TMUX-256color"))), QUERIES);

        let skipped = [
            Some("tmux"),
            Some("tmux-256color"),
            Some("screen"),
            Some("screen-256color"),
        ];
        for term in skipped {
            let bytes = startup_queries(term.map(OsStr::new));
            assert!(
                bytes
                    .windows(graphics.len())
                    .all(|window| window != graphics),
                "term={term:?} still queries graphics: {bytes:?}"
            );
            assert!(bytes.starts_with(b"\x1b[?u"), "{bytes:?}");
            assert!(bytes.ends_with(b"\x1b[14t\x1b[c"), "{bytes:?}");
            assert!(bytes
                .windows(b"\x1bP+q524742\x1b\\".len())
                .any(|window| { window == b"\x1bP+q524742\x1b\\" }));
            assert!(bytes
                .windows(b"\x1b]10;?\x1b\\".len())
                .any(|window| window == b"\x1b]10;?\x1b\\"));
            assert!(bytes
                .windows(b"\x1b]11;?\x1b\\".len())
                .any(|window| window == b"\x1b]11;?\x1b\\"));
            assert!(bytes
                .windows(b"\x1b[16t".len())
                .any(|window| window == b"\x1b[16t"));
        }
    }
}
