//! Painting a [`TextDocument`] into a [`Grid`].
//!
//! Owns: the mapping from a span's [`Style`] to cell colours and attributes, and clipping to the
//! page area. Must not own: layout, scrolling, the terminal or the chrome rows around the page.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::grid::{Attrs, Color, Grid};

use super::{Style, TextDocument, TextView};

/// The cells the page may use: `rows` rows from `row`, `cols` columns from `col`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    /// First column.
    pub col: u16,
    /// First row.
    pub row: u16,
    /// Width in columns.
    pub cols: u16,
    /// Height in rows.
    pub rows: u16,
}

const LINK: Color = Color::Indexed(75);
const DIM: Color = Color::Indexed(244);
const FIELD: Color = Color::Indexed(114);

fn pen(style: Style, focused: bool) -> (Color, Attrs) {
    let fg = if style.dim {
        DIM
    } else if style.link {
        LINK
    } else if style.field {
        FIELD
    } else {
        Color::Default
    };
    let attrs = Attrs {
        bold: style.bold,
        underline: style.underline,
        reverse: focused || style.focused,
        italic: false,
    };
    (fg, attrs)
}

/// Paint the visible lines into `area`. Rows past the document are blank. The focused item and
/// the current match of a search ([`TextView::found`]) are drawn in reverse video.
pub fn draw(doc: &TextDocument, view: &TextView, grid: &mut Grid, area: Area) {
    let right = area.col.saturating_add(area.cols).min(grid.cols());
    for r in 0..area.rows {
        let row = area.row + r;
        if row >= grid.rows() {
            break;
        }
        for col in area.col..right {
            grid.put_cluster(
                col,
                row,
                " ",
                Color::Default,
                Color::Default,
                Attrs::default(),
            );
        }
        let index = view.scroll + usize::from(r);
        let Some(line) = doc.lines.get(index) else {
            continue;
        };
        let found = |col: u16| {
            let col = col - area.col;
            view.found
                .iter()
                .any(|range| range.line == index && (range.start..range.end).contains(&col))
        };
        let mut col = area.col;
        'spans: for span in &line.spans {
            let focused = span.item.is_some() && span.item == view.focused_item;
            let (fg, attrs) = pen(span.style, focused);
            for g in span.text.graphemes(true) {
                let gw = u16::try_from(g.width()).unwrap_or(u16::MAX);
                if gw == 0 {
                    continue;
                }
                // A cluster that does not fit ends the row. A later span must not draw into
                // the columns this cluster could not use.
                if u32::from(col) + u32::from(gw) > u32::from(right) {
                    break 'spans;
                }
                let attrs = Attrs {
                    reverse: attrs.reverse || found(col),
                    ..attrs
                };
                grid.put_cluster(col, row, g, fg, Color::Default, attrs);
                col += gw;
            }
        }
    }
}
