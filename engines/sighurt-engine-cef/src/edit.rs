//! Editing the page on the browser's behalf: inserting a paste, deleting what a cut took.
//!
//! The text of a paste belongs to the document and frame that asked for it. Both are recorded
//! when the key goes down ([`Binding`]) and checked again on CEF's UI thread immediately before
//! every insertion: commits and the checks run on that one thread, so a page that navigated
//! after asking can never receive the clipboard. A long paste goes in a thousand units at a time,
//! each a task of its own, so neither the owner nor CEF's queue holds the whole of it. Text goes
//! in as typed characters (Chromium ignores an input method's calls from the browser side
//! here). A line break is typed as Enter only into a multi-line field (a text area or an
//! editable element), where it is a new line; into any other field it is a space, as Chromium
//! makes it of a paste, so a multi-line paste never submits a form.
//!
//! Must not own: the clipboard (the browser's services do) or the decision to paste.

use std::sync::Arc;

use sighurt_api::ids::DocumentGeneration;

use crate::actions::send_char;
use crate::ffi::{call, post_ui, Ref};
use crate::shared::{lock, ViewShared};

/// Units of text sent per task.
const CHUNK: usize = 1024;
/// A line break, once [`line_breaks`] made every kind of them this one.
const LF: u16 = b'\n' as u16;

/// `text` with each CR LF and lone CR as LF.
fn line_breaks(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The document and frame an edit was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    document: DocumentGeneration,
    frame: Option<String>,
}

impl ViewShared {
    /// The current document and focused frame, if there is a document.
    pub fn binding(&self) -> Option<Binding> {
        let document = lock(&self.nav).document()?;
        Some(Binding {
            document,
            frame: self.focused_frame(),
        })
    }

    /// Whether `binding` is still the page's state. Called on CEF's UI thread.
    fn holds(&self, binding: &Binding) -> bool {
        !self.is_closing()
            && lock(&self.nav).document() == Some(binding.document)
            && self.focused_frame() == binding.frame
    }

    /// Types `text` into the page, if `binding` still holds at each step.
    pub fn insert_text(self: &Arc<Self>, text: &str, binding: Binding) {
        let units: Arc<Vec<u16>> = Arc::new(line_breaks(text).encode_utf16().collect());
        if !units.is_empty() {
            self.insert_from(units, 0, binding);
        }
    }

    fn insert_from(self: &Arc<Self>, units: Arc<Vec<u16>>, from: usize, binding: Binding) {
        let view = Arc::clone(self);
        post_ui(move || {
            if !view.holds(&binding) {
                return;
            }
            let Some(host) = lock(&view.slot).host.clone() else {
                return;
            };
            let mut end = (from + CHUNK).min(units.len());
            // Never split a surrogate pair across tasks.
            if end < units.len() && (0xD800..0xDC00).contains(&units[end - 1]) {
                end += 1;
            }
            // The field that has the focus now decides what a line break is.
            let multiline = lock(&view.text_input)
                .as_ref()
                .is_some_and(|state| state.multiline);
            for &unit in &units[from..end] {
                let unit = match unit {
                    LF if multiline => u16::from(b'\r'),
                    LF => u16::from(b' '),
                    unit => unit,
                };
                // SAFETY: a live host reference, on the UI thread.
                unsafe { send_char(host.as_ptr(), unit) };
            }
            if end < units.len() {
                view.insert_from(units, end, binding);
            }
        });
    }

    /// Deletes the selection (what a cut took), if `binding` still holds. Not the native cut:
    /// that would put the text on Chromium's own clipboard before the browser approved the copy.
    pub fn delete_selection(self: &Arc<Self>, binding: Binding) {
        let view = Arc::clone(self);
        post_ui(move || {
            if !view.holds(&binding) {
                return;
            }
            let Some(browser) = lock(&view.slot).browser.clone() else {
                return;
            };
            // SAFETY: a live browser reference; the frame is an owned reference.
            unsafe {
                if let Some(frame) = Ref::adopt(call!(browser.as_ptr(), get_focused_frame)) {
                    call!(frame.as_ptr(), del);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_of_line_break_is_one() {
        assert_eq!(line_breaks("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(line_breaks("\r\n\r\n"), "\n\n");
    }
}
