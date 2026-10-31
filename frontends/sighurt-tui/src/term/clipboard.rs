//! OSC 52 clipboard bytes, and the base64 they carry.
//!
//! Owns: encoding and decoding a payload, and the byte sequences for a write and a query.
//! Must not own: a read that is waiting, the terminal, or when the bytes are sent. tmux takes
//! OSC 52 itself (`set-clipboard`); these sequences are not wrapped for it.

use sighurt_api::services::Selection;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `data` as standard base64, with padding.
pub(crate) fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let (chunks, rest) = data.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        push_group(&mut out, n, 4);
    }
    match rest {
        [a] => {
            push_group(&mut out, u32::from(*a) << 16, 2);
            out.push('=');
            out.push('=');
        }
        [a, b] => {
            let n = (u32::from(*a) << 16) | (u32::from(*b) << 8);
            push_group(&mut out, n, 3);
            out.push('=');
        }
        _ => {}
    }
    out
}

fn push_group(out: &mut String, n: u32, count: usize) {
    for index in 0..count {
        let shift = 18 - 6 * index;
        let alphabet = ((n >> shift) & 0x3f) as usize;
        out.push(ALPHABET[alphabet] as char);
    }
}

/// Standard base64, or `None` when `text` is not. Whitespace is ignored, and the padding may
/// be left off. An empty string is empty data; the caller decides what an empty OSC field means.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    let mut raw = Vec::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        raw.push(byte);
    }
    if raw.is_empty() {
        return Some(Vec::new());
    }
    let mut pad = 0usize;
    while raw.last() == Some(&b'=') {
        raw.pop();
        pad += 1;
        if pad > 2 {
            return None;
        }
    }
    if raw.contains(&b'=') || raw.len() % 4 == 1 {
        return None;
    }
    if pad > 0 && !(raw.len() + pad).is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(raw.len() / 4 * 3 + 2);
    let mut index = 0;
    while index + 4 <= raw.len() {
        let n = group(&raw[index..index + 4])?;
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
        index += 4;
    }
    match raw.len() - index {
        0 => {}
        2 => {
            let n =
                (u32::from(digit(raw[index])?) << 18) | (u32::from(digit(raw[index + 1])?) << 12);
            out.push((n >> 16) as u8);
        }
        3 => {
            let n = (u32::from(digit(raw[index])?) << 18)
                | (u32::from(digit(raw[index + 1])?) << 12)
                | (u32::from(digit(raw[index + 2])?) << 6);
            out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8]);
        }
        _ => return None,
    }
    Some(out)
}

fn group(bytes: &[u8]) -> Option<u32> {
    Some(
        (u32::from(digit(bytes[0])?) << 18)
            | (u32::from(digit(bytes[1])?) << 12)
            | (u32::from(digit(bytes[2])?) << 6)
            | u32::from(digit(bytes[3])?),
    )
}

fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Whether `mime` is a `text/*` type, ignoring a `;charset=` parameter.
pub(crate) fn is_text(mime: &str) -> bool {
    let main = mime.split(';').next().unwrap_or(mime).trim();
    main.len() >= 5 && main.as_bytes()[..5].eq_ignore_ascii_case(b"text/")
}

fn selection_byte(selection: Selection) -> u8 {
    match selection {
        Selection::Clipboard => b'c',
        Selection::Primary => b'p',
    }
}

/// Bytes of [`set`] around the base64: `ESC ] 52 ; <c|p> ;` and `ESC \`.
pub(crate) const SET_OVERHEAD: usize = 9;

/// How many bytes [`set`] returns for a payload of `data_len`, without encoding it.
pub(crate) fn set_len(data_len: usize) -> usize {
    data_len.div_ceil(3) * 4 + SET_OVERHEAD
}

/// `ESC ] 52 ; c ; <base64> ESC \` (`p` for [`Selection::Primary`]).
pub(crate) fn set(selection: Selection, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]52;");
    out.push(selection_byte(selection));
    out.push(b';');
    out.extend(encode(data).into_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// `ESC ] 52 ; c ; ? ESC \`.
pub(crate) fn query(selection: Selection) -> Vec<u8> {
    let mut out = b"\x1b]52;".to_vec();
    out.push(selection_byte(selection));
    out.extend_from_slice(b";?\x1b\\");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_and_rejects_bad_input() {
        for data in [b"".as_slice(), b"h", b"he", b"hej", &[0, 255, 10, 32]] {
            let encoded = encode(data);
            assert_eq!(decode(&encoded).as_deref(), Some(data), "{encoded}");
        }
        assert_eq!(encode(b"hej"), "aGVq");
        assert_eq!(decode("aGVq").as_deref(), Some(b"hej".as_slice()));
        assert_eq!(decode("aA").as_deref(), Some(b"h".as_slice()));
        assert_eq!(decode("aGU").as_deref(), Some(b"he".as_slice()));
        assert_eq!(decode("aG\nVq").as_deref(), Some(b"hej".as_slice()));
        assert_eq!(decode(""), Some(Vec::new()));
        assert_eq!(decode("!!!!"), None);
        assert_eq!(decode("a"), None);
        assert_eq!(decode("a==="), None);
        assert_eq!(decode("aGVq="), None);
        assert_eq!(decode("===="), None);
        assert_eq!(decode("aG=V"), None);
    }

    #[test]
    fn osc52_bytes_name_the_selection() {
        assert_eq!(set(Selection::Clipboard, b"hej"), b"\x1b]52;c;aGVq\x1b\\");
        assert_eq!(
            set_len(b"hej".len()),
            set(Selection::Clipboard, b"hej").len()
        );
        assert_eq!(set(Selection::Primary, b"hej"), b"\x1b]52;p;aGVq\x1b\\");
        assert_eq!(set(Selection::Clipboard, b""), b"\x1b]52;c;\x1b\\");
        assert_eq!(set_len(0), set(Selection::Clipboard, b"").len());
        assert_eq!(query(Selection::Clipboard), b"\x1b]52;c;?\x1b\\");
        assert_eq!(query(Selection::Primary), b"\x1b]52;p;?\x1b\\");
    }

    #[test]
    fn text_types_are_the_ones_with_a_text_prefix() {
        assert!(is_text("text/plain"));
        assert!(is_text("text/plain;charset=utf-8"));
        assert!(is_text("TEXT/HTML"));
        assert!(is_text(" text/plain "));
        assert!(!is_text("image/png"));
        assert!(!is_text("textual"));
        assert!(!is_text("text"));
        assert!(!is_text(""));
    }
}
