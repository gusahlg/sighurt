//! Editing the page on the browser's behalf: typing a paste, deleting what a cut took.
//!
//! The text of a paste belongs to the document and frame that asked for it. Both are recorded
//! when the key goes down ([`Binding`]) and checked again on CEF's UI thread immediately before
//! every insertion: commits and the checks run on that one thread, so a page that navigated
//! after asking can never receive the clipboard. A long paste goes in a thousand units at a time,
//! each a task of its own, so neither the owner nor CEF's queue holds the whole of it.
//!
//! Must not own: the clipboard (the browser's services do) or the decision to paste.

use std::sync::Arc;

use sighurt_api::ids::DocumentGeneration;

use crate::actions::send_char;
use crate::ffi::{call, post_ui, Ref};
use crate::shared::{lock, ViewShared};

/// Units of text sent per task.
const CHUNK: usize = 1024;

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

    /// Types `text` into the page as characters, if `binding` still holds at each step.
    pub fn insert_text(self: &Arc<Self>, text: &str, binding: Binding) {
        let units: Arc<Vec<u16>> = Arc::new(text.replace('\n', "\r").encode_utf16().collect());
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
            for unit in &units[from..end] {
                // SAFETY: a live host reference, on the UI thread.
                unsafe { send_char(host.as_ptr(), *unit) };
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
