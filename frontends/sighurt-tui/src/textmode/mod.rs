//! Text mode: reflow a page's semantic snapshot into terminal lines.
//!
//! Owns: the laid-out document ([`layout`]), the viewport and item focus
//! ([`TextView`]), finding text in it ([`find`]), painting those lines into a
//! [`crate::grid::Grid`], and the plain `--dump` text. Must not own: the terminal, the engine, browser
//! policy, or which keys are bound. The app maps `j`/`k`, `ctrl-d`/`ctrl-u`
//! and `g`/`G` onto [`TextView`]'s scroll operations; this module never reads
//! input.
//!
//! Strings that are drawn go through [`crate::sanitize`] before they are stored
//! in a span. Printable ASCII is copied as-is, because that function would not
//! change it. Newlines in flowing text become spaces first, so a paragraph
//! still wraps; preformatted lines are split on newlines and then sanitized. A
//! field's stored value is the snapshot's own string, so editing can commit it
//! unchanged; it is sanitized when drawn.

mod draw;
mod dump;
mod find;
mod layout;
mod types;
mod view;

#[cfg(test)]
mod tests;

pub use draw::{draw, Area};
pub use dump::{dump, dump_document};
pub use find::find;
pub use layout::layout;
pub use types::{CellRange, Item, ItemKind, Joined, Span, Style, TextDocument, TextLine};
pub use view::{Intent, TextView};
