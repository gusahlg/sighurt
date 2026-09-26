//! Carrying out browser commands (`sighurt_api::commands::Command`) and matching key bindings
//! to them.
//!
//! A command changes core state and yields effects; `location.focus` is handed to the chrome.
//! Bindings come only from the user's configuration: there are no default bindings and no key
//! handlers in code. They match on the logical key value by default and on the physical key
//! when a binding asks for it, per input mode (page, address bar), ignoring modifiers the
//! layout consumed so AltGr characters never fire Ctrl+Alt shortcuts. A key press the browser
//! consumes also consumes its release, so the page never sees a stuck key.
//!
//! Must not own: keyboard normalization (the host) or text editing (the chrome).
