//! Pasted text as the program gets it (amendment A8).
//!
//! A paste is text the user did not type, from a clipboard anything may have written to, so it
//! must not be able to type keys: ESC and every other C0 control except tab, the C1 controls
//! (U+0080 to U+009F, which 8-bit terminals read as ESC sequences) and DEL are removed. That also
//! makes the bracketed-paste end marker (`ESC [ 201 ~`) impossible inside it. Line breaks become
//! CR, as the Enter key sends. When the program asked for bracketed paste (DECSET 2004) the text
//! is wrapped in `ESC [ 200 ~` and `ESC [ 201 ~`, so an editor or a shell knows it is a paste and
//! runs none of it as it arrives.
//!
//! Owns: the paste policy of the terminal page. Must not own: where the paste came from (the
//! browser's input queue marks it) or typed keys ([`crate::keys`]).

/// `text` as the bytes to give the program; `bracketed` is whether it enabled bracketed paste.
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
    }
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(b'\r');
            }
            '\n' => out.push(b'\r'),
            '\t' => out.push(b'\t'),
            c if c.is_control() => {}
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    if bracketed {
        out.extend_from_slice(b"\x1b[201~");
    }
    out
}

/// Text an input method committed, as typed text: what [`paste`] keeps, without the brackets.
pub fn commit(text: &str) -> Vec<u8> {
    paste(text, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_are_removed_but_tabs_and_lines_stay() {
        assert_eq!(paste("ls -l\n", false), b"ls -l\r");
        assert_eq!(paste("a\r\nb\rc\nd", false), b"a\rb\rc\rd");
        assert_eq!(paste("a\tb", false), b"a\tb");
        assert_eq!(paste("x\u{1b}[31my\u{7}\u{0}\u{7f}z", false), b"x[31myz");
        // C1: CSI as one code point.
        assert_eq!(paste("a\u{9b}201~b", false), b"a201~b");
        assert_eq!(paste("grüße ✓", false), "grüße ✓".as_bytes());
    }

    #[test]
    fn a_bracketed_paste_cannot_end_itself() {
        let pasted = paste("evil\u{1b}[201~rm -rf ~\n", true);
        assert_eq!(pasted, b"\x1b[200~evil[201~rm -rf ~\r\x1b[201~");
        let inner = &pasted[6..pasted.len() - 6];
        assert!(!inner.contains(&0x1b));
        assert_eq!(paste("", true), b"\x1b[200~\x1b[201~");
    }

    #[test]
    fn a_commit_is_typed_text() {
        assert_eq!(commit("日本"), "日本".as_bytes());
        assert_eq!(commit("\u{1b}x"), b"x");
    }
}
