//! The address and command line editors.
//!
//! Cursor movement, selection, deletion, paste and horizontal scrolling by grapheme cluster,
//! using the text library's cluster information; never by indexing UTF-8 bytes. Shows an input
//! method's preedit at the cursor, applies its commits and surrounding-text deletions, and
//! reports its own surrounding text and caret rectangle for the input method.
//!
//! Must not own: the clipboard (paste arrives as text from a granted clipboard read).
