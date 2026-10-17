//! What the runtime asks a frontend to do.
//!
//! Effects the runtime cannot carry out itself — a view's target, focus, the chrome, the
//! page's cursor and text field, notices, redraws — are handed to the frontend here, one at a
//! time, in the order the core returned them. Opening a target is a handshake: the frontend
//! returns the target or an error, the runtime records the view and asks for a placement, and
//! only then does the engine receive the target. A command for a view is never sent before
//! that. The clipboard is [`crate::services::Clipboard`], which this trait requires, so page
//! services and these effects reach one object.
//!
//! Must not own: browser policy, engine runtimes, or how a frontend draws.

use std::collections::{BTreeMap, VecDeque};

use sighurt_api::events::Cursor;
use sighurt_api::ids::{TargetId, ViewId};
use sighurt_api::input::{InputEvent, KeyboardFocus, TextInputState};
use sighurt_core::BrowserEvent;

use crate::diagnostics::Diagnostics;
use crate::services::Clipboard;
use crate::view::ViewEntry;

/// The frontend the session drives.
///
/// Every method runs to completion before the next effect. `open_target` failing, or the
/// engine then refusing the view, becomes `BrowserEvent::OpenFailed` for the core; the
/// runtime does not send that view any command.
pub trait Frontend: Clipboard {
    /// The engine's `Target`. The Wayland GUI uses the host's target lease.
    type Target: 'static;

    /// Create the view's target. The engine is already started. `Err` means no target exists.
    fn open_target(
        &mut self,
        view: ViewId,
        engine: &str,
    ) -> Result<(TargetId, Self::Target), String>;

    /// The view is recorded in `views`. Place it, so the engine starts with the page area's
    /// metrics. Returns whether the placement changed.
    fn place(&mut self, views: &BTreeMap<ViewId, ViewEntry>, diag: &mut Diagnostics) -> bool;

    /// `view`'s target is being forgotten. Release the keyboard and the input method if they
    /// were on `target`. The runtime has already dropped the view.
    fn forget_target(&mut self, target: TargetId);

    /// The keyboard focus moved. Queue whatever the chrome asks for.
    fn focus(&mut self, focus: KeyboardFocus, queue: &mut VecDeque<BrowserEvent>);

    /// Keyboard input for the chrome. Pointer and window-focus input are not chrome input;
    /// ignore them.
    fn chrome_input(&mut self, event: InputEvent, queue: &mut VecDeque<BrowserEvent>);

    /// The chrome's own `chrome.<action>`. An unknown action is a notice.
    fn chrome_command(&mut self, action: String, queue: &mut VecDeque<BrowserEvent>);

    /// The pointer's shape over the page. `target` is the shown view's, if there is one.
    fn page_cursor(&mut self, target: Option<TargetId>, cursor: Cursor);

    /// The focused page's text field. `shown` is the visible view's target; `None` in `state`
    /// clears the field previously set.
    fn page_text_input(&mut self, shown: Option<TargetId>, state: Option<TextInputState>);

    /// Tell the user something. Not logged: a notice can hold page data.
    fn notice(&mut self, message: String);

    /// Something the chrome shows changed.
    fn redraw(&mut self);

    /// The core showed or hid `view`. The view entry is already updated; the next placement
    /// is what maps or unmaps the target.
    fn show(&mut self, view: ViewId, visible: bool);

    /// The engine has a new frame for the view that owns `target`.
    fn frame_ready(&mut self, target: TargetId);

    /// Called once, immediately before a forced [`crate::exit_now`] (decision 54, TUI.md
    /// §8.8). The terminal frontend restores the terminal here. Destructors do not run after
    /// this returns.
    fn before_forced_exit(&mut self);
}
