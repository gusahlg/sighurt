//! Find in a laid-out page: matches of a text in the page's logical lines, the text as it was
//! before wrapping, mapped back to the rows and columns that show them.
//!
//! Owns: joining wrapped rows into logical lines ([`super::Joined`]), the case rule, and the
//! match-to-cell mapping. Must not own: which match is current, scrolling, drawing, or the
//! search's identity (the caller's).
//!
//! A row's prefix (indent, quote bars, a list marker) is not page text and is left out. A row
//! continued after a space is joined with one space, which no row shows: a match may span it.
//! Matches do not overlap; without `case_sensitive`, letters match in either case.

use unicode_width::UnicodeWidthStr;

use super::{CellRange, Joined, TextDocument};

/// One row's share of a logical line: the row, the bytes of the line it shows, and the column
/// where they start on the row. A joining space is in no piece.
struct Piece {
    row: usize,
    start: usize,
    end: usize,
    col: usize,
}

/// A run of rows that wrapping made of one text.
#[derive(Default)]
struct Logical {
    text: String,
    pieces: Vec<Piece>,
}

/// The page's logical lines, in order.
fn logical_lines(doc: &TextDocument) -> Vec<Logical> {
    let mut lines: Vec<Logical> = Vec::new();
    for (row, line) in doc.lines.iter().enumerate() {
        let skip = usize::from(line.prefixed);
        let col = line.spans.iter().take(skip).map(|s| s.text.width()).sum();
        let text: String = line
            .spans
            .iter()
            .skip(skip)
            .map(|s| s.text.as_str())
            .collect();
        let current = match (line.joined, lines.last_mut()) {
            (Joined::No, _) | (_, None) => {
                lines.push(Logical::default());
                lines.last_mut().expect("just pushed")
            }
            (Joined::Space, Some(last)) => {
                last.text.push(' ');
                last
            }
            (Joined::Directly, Some(last)) => last,
        };
        let start = current.text.len();
        current.text.push_str(&text);
        current.pieces.push(Piece {
            row,
            start,
            end: current.text.len(),
            col,
        });
    }
    lines
}

/// `text` as compared, and for each byte of it the byte of `text` its character starts at.
fn fold(text: &str, case_sensitive: bool) -> (String, Vec<usize>) {
    let mut folded = String::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len());
    for (at, ch) in text.char_indices() {
        let before = folded.len();
        match case_sensitive {
            true => folded.push(ch),
            false => folded.extend(ch.to_lowercase()),
        }
        origin.resize(origin.len() + folded.len() - before, at);
    }
    (folded, origin)
}

/// The cells of every match of `needle` in `doc`, in reading order: each match's ranges, one
/// per row it is on. An empty needle matches nothing.
pub fn find(doc: &TextDocument, needle: &str, case_sensitive: bool) -> Vec<Vec<CellRange>> {
    let (needle, _) = fold(needle, case_sensitive);
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for line in logical_lines(doc) {
        let (folded, origin) = fold(&line.text, case_sensitive);
        for (at, part) in folded.match_indices(needle.as_str()) {
            let start = origin[at];
            let last = origin[at + part.len() - 1];
            let end = last + line.text[last..].chars().next().map_or(0, char::len_utf8);
            found.push(cells(&line, start, end));
        }
    }
    found
}

/// The cells that show bytes `start..end` of `line`.
fn cells(line: &Logical, start: usize, end: usize) -> Vec<CellRange> {
    let mut ranges = Vec::new();
    for piece in &line.pieces {
        let (from, to) = (start.max(piece.start), end.min(piece.end));
        if from >= to {
            continue;
        }
        let col = |byte: usize| piece.col + line.text[piece.start..byte].width();
        ranges.push(CellRange {
            line: piece.row,
            start: to_col(col(from)),
            end: to_col(col(to)),
        });
    }
    ranges
}

fn to_col(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}
