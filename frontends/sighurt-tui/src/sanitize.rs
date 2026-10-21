//! One filter for every page-supplied string before it reaches the terminal.
//!
//! Owns: the removal of C0 and C1 controls, escape sequences and bidi
//! overrides. Must not own: where the string is drawn, how wide it is cut, or
//! the terminal. The chrome cuts to a width after this.

/// Strip controls, escape sequences and bidi overrides from `input`.
///
/// C0 (including tab and newline) and DEL are removed. C1 controls are
/// removed, and a C1 introducer (CSI, OSC, DCS, APC) takes its sequence with
/// it. `ESC` takes the sequence that follows: CSI through its final byte, OSC
/// and the string types through BEL or ST. Bidi embeddings, overrides, pops
/// and isolates are removed (`U+202A`–`U+202E`, `U+2066`–`U+2069`), as are the
/// directional marks `U+200E`, `U+200F` and `U+061C`.
pub fn sanitize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if is_bidi(ch) {
            continue;
        }
        match ch {
            '\u{1b}' => skip_esc(&mut chars),
            '\u{0090}' | '\u{0098}' | '\u{009e}' | '\u{009f}' => skip_until_st(&mut chars),
            '\u{009b}' => skip_csi(&mut chars),
            '\u{009d}' => skip_osc(&mut chars),
            ch if is_c0_or_c1(ch) => {}
            ch => out.push(ch),
        }
    }
    out
}

fn is_bidi(ch: char) -> bool {
    matches!(
        ch,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

fn is_c0_or_c1(ch: char) -> bool {
    matches!(ch, '\u{0000}'..='\u{001f}' | '\u{007f}' | '\u{0080}'..='\u{009f}')
}

fn skip_esc(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let Some(next) = chars.next() else {
        return;
    };
    match next {
        '[' => skip_csi(chars),
        ']' => skip_osc(chars),
        'P' | 'X' | '^' | '_' => skip_until_st(chars),
        _ => {}
    }
}

fn skip_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for _ in 0..128 {
        let Some(ch) = chars.next() else {
            return;
        };
        if ('\u{40}'..='\u{7e}').contains(&ch) {
            return;
        }
    }
}

fn skip_osc(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for _ in 0..1024 {
        let Some(ch) = chars.next() else {
            return;
        };
        if ch == '\u{7}' || ch == '\u{009c}' {
            return;
        }
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'\\') {
                chars.next();
            }
            return;
        }
    }
}

fn skip_until_st(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for _ in 0..1024 {
        let Some(ch) = chars.next() else {
            return;
        };
        if ch == '\u{009c}' || ch == '\u{7}' {
            return;
        }
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'\\') {
                chars.next();
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_controls_escapes_and_bidi() {
        assert_eq!(sanitize("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(sanitize("\u{1b}]0;title\u{7}rest"), "rest");
        assert_eq!(sanitize("\u{1b}]0;title\u{1b}\\rest"), "rest");
        assert_eq!(sanitize("\u{1b}_Gi=1;OK\u{1b}\\x"), "x");
        assert_eq!(sanitize("\u{202e}ABC\u{202c}"), "ABC");
        assert_eq!(sanitize("\u{2067}x\u{2069}"), "x");
        assert_eq!(sanitize("a\u{0000}\u{0007}\nb"), "ab");
        assert_eq!(sanitize("\u{009b}31mX"), "X");
        assert_eq!(sanitize("hi\u{1b}"), "hi");
        assert_eq!(sanitize("漢字😀"), "漢字😀");
        assert_eq!(sanitize("plain"), "plain");
    }
}
