//! `sig --dump`: the page as reflowed text, like `lynx -dump`.
//!
//! Owns: turning a laid-out document into plain text with numbered link footnotes. Must not own:
//! fetching the page, deciding when it is complete or writing to stdout.

use sighurt_api::semantic::SemanticSnapshot;

use super::{layout, TextDocument};

/// The text of `snapshot` at `width` columns: links carry `[n]` after their text; a blank line and
/// `References` list `n. url` for each link that has an address. Every line is free of control
/// characters and escape sequences, and ends with a newline.
pub fn dump(snapshot: &SemanticSnapshot, width: u16) -> String {
    dump_document(&layout(snapshot, width))
}

/// [`dump`] of an already laid out document.
pub fn dump_document(doc: &TextDocument) -> String {
    let mut out = String::new();
    for line in &doc.lines {
        let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
        out.push_str(text.trim_end());
        out.push('\n');
    }
    let mut links: Vec<_> = doc
        .items
        .iter()
        .filter_map(|i| Some((i.number?, i.href.as_deref()?)))
        .collect();
    links.sort_unstable_by_key(|l| l.0);
    if !links.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("References\n\n");
        for (n, href) in links {
            out.push_str(&format!("{n}. {href}\n"));
        }
    }
    out
}
