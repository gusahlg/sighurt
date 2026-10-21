//! Text mode: reflow a page's semantic snapshot into terminal lines.
//!
//! Owns: the laid-out document ([`layout`]), the viewport and item focus
//! ([`TextView`]), painting those lines into a [`crate::grid::Grid`], and the
//! plain `--dump` text. Must not own: the terminal, the engine, browser
//! policy, or which keys are bound. The app maps `j`/`k`, `ctrl-d`/`ctrl-u`
//! and `g`/`G` onto [`TextView`]'s scroll operations; this module never reads
//! input.
//!
//! Page strings go through [`crate::sanitize`] before they are stored.
//! Printable ASCII is copied as-is, because that function would not change it.
//! Newlines in flowing text become spaces first, so a paragraph still wraps;
//! preformatted lines are split on newlines and then sanitized.

mod draw;
mod dump;
mod layout;
mod types;
mod view;

#[cfg(test)]
mod tests;

pub use draw::{draw, Area};
pub use dump::{dump, dump_document};
pub use layout::layout;
pub use types::{CellRange, Item, ItemKind, Span, Style, TextDocument, TextLine};
pub use view::{Intent, TextView};
