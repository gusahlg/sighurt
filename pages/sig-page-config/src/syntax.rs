//! Light TOML colouring, one line at a time: comments, table headers, keys and strings.
//! A string that spans lines (`"""`) is coloured only on the line it starts on.
//!
//! Must not own: checking the text (the browser's), or drawing.

/// What a run of a line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    Comment,
    Header,
    Key,
    String,
}

/// The runs of `line`, as byte ranges with their style, in order and covering it.
pub fn spans(line: &str) -> Vec<(usize, usize, Style)> {
    let mut out: Vec<(usize, usize, Style)> = Vec::new();
    let mut push = |start: usize, end: usize, style: Style| {
        if start < end {
            match out.last_mut() {
                Some(last) if last.2 == style && last.1 == start => last.1 = end,
                _ => out.push((start, end, style)),
            }
        }
    };
    let body = line.trim_start();
    let lead = line.len() - body.len();
    push(0, lead, Style::Plain);
    if body.starts_with('[') {
        let end = body.find(']').map_or(body.len(), |i| {
            // `[[x]]` closes with two brackets.
            if body[i..].starts_with("]]") {
                i + 2
            } else {
                i + 1
            }
        });
        push(lead, lead + end, Style::Header);
        rest(line, lead + end, &mut push);
        return out;
    }
    let key_end = if body.starts_with('#') {
        0
    } else {
        body.find('=').unwrap_or(0)
    };
    push(lead, lead + key_end, Style::Key);
    rest(line, lead + key_end, &mut push);
    out
}

/// Strings and a trailing comment from `from` on.
fn rest(line: &str, from: usize, push: &mut impl FnMut(usize, usize, Style)) {
    let bytes = line.as_bytes();
    let mut i = from;
    let mut plain = from;
    while i < bytes.len() {
        match bytes[i] {
            b'#' => {
                push(plain, i, Style::Plain);
                push(i, line.len(), Style::Comment);
                return;
            }
            quote @ (b'"' | b'\'') => {
                push(plain, i, Style::Plain);
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != quote {
                    // A backslash escapes in basic strings only.
                    j += if quote == b'"' && bytes[j] == b'\\' {
                        2
                    } else {
                        1
                    };
                }
                let end = (j + 1).min(bytes.len());
                push(i, end, Style::String);
                i = end;
                plain = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    push(plain, line.len(), Style::Plain);
}

#[cfg(test)]
mod tests {
    use super::Style::*;
    use super::*;

    #[test]
    fn comments_headers_keys_and_strings() {
        assert_eq!(spans("# hi"), [(0, 4, Comment)]);
        assert_eq!(
            spans("  [ui] # x"),
            [
                (0, 2, Plain),
                (2, 6, Header),
                (6, 7, Plain),
                (7, 10, Comment)
            ]
        );
        assert_eq!(spans("[[a]]"), [(0, 5, Header)]);
        let line = r##"w = "a\"#b" # c 'd'"##;
        assert_eq!(
            spans(line),
            [
                (0, 2, Key),
                (2, 4, Plain),
                (4, 11, String),
                (11, 12, Plain),
                (12, 19, Comment)
            ]
        );
        assert_eq!(
            spans("x = 'a\\'"),
            [(0, 2, Key), (2, 4, Plain), (4, 8, String)]
        );
        assert_eq!(
            spans("x = \"open"),
            [(0, 2, Key), (2, 4, Plain), (4, 9, String)]
        );
        assert_eq!(spans(""), []);
        assert_eq!(spans("ä = 1"), [(0, 3, Key), (3, 6, Plain)]);
        // Every run is on character boundaries and the runs cover the line.
        for line in ["é\"é\\", "'é", "[é", "a=\"\\"] {
            let s = spans(line);
            assert_eq!(s.first().map(|r| r.0), Some(0));
            assert_eq!(s.last().map(|r| r.1), Some(line.len()));
            assert!(s
                .iter()
                .all(|r| line.is_char_boundary(r.0) && line.is_char_boundary(r.1)));
        }
    }
}
