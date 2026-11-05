//! Fullscreen: the window's, which the user toggles (`browser.fullscreen`), and a page's, when
//! one of its elements goes fullscreen (a video, a game: `EngineEvent::FullscreenChanged`).
//!
//! Three states, each with one source:
//!
//! - **wanted**: the user's toggle. `browser.fullscreen` flips it, so two toggles before the
//!   compositor answers ask for fullscreen and then for none, never twice for it. The
//!   compositor ending fullscreen by itself resets it, and the compositor starting it adopts it.
//! - **window**: what the window is, as the frontend reports it ([`BrowserEvent::Fullscreen`](crate::state::BrowserEvent::Fullscreen)).
//!   The chrome is drawn from this one. A report is a fact; nothing assumes that a request
//!   ([`Effect::SetFullscreen`]) succeeded.
//! - **page**: the view and document holding element fullscreen, if one does.
//!
//! The window is asked to be fullscreen while the user wants it or a page holds fullscreen,
//! and only in the window ([`Frontend::Gui`]): in the terminal, `browser.fullscreen` is a
//! notice and a page's fullscreen keeps its rows. A report that differs from the latest request
//! while two or more requests are unanswered is the answer to one the core has since replaced,
//! and changes nothing; the compositor answers the latest one too.
//!
//! Engines report leaving as well as entering, on their own when the document goes, and confirm
//! `ExitFullscreen` later with a `false`, if at all: a `false` for a view that holds nothing is
//! no news. An engine without `EngineCapabilities::fullscreen` is not listened to, and never
//! told `ExitFullscreen`.
//!
//! A page's fullscreen is accepted only for the shown view's current document, while the
//! window has the keyboard, and only with the user's activation of the page within
//! [`ACTIVATION`](crate::permissions::ACTIVATION), which it uses up: the same click can't
//! enter twice. Accepted, the core shows a notice of its own ("example.com is fullscreen.
//! Escape leaves it.") and asks for the window's fullscreen; otherwise it answers
//! `EngineCommand::ExitFullscreen` at once. It ends, whatever ends it, at once and without
//! waiting for the engine: `page.exit-fullscreen` (the keymap's Escape, or the core's own
//! Escape, [`crate::state`]), another tab, a new document, the view closing or failing, the
//! window losing the keyboard, the compositor ending the window's fullscreen, the user's
//! `browser.fullscreen` (which leaves the window's fullscreen too), or the engine saying the
//! element left. The engine is told `ExitFullscreen` unless it said so itself, and the window
//! returns to what the user wants.
//!
//! Must not own: windows or surfaces (the frontend's), what the chrome hides (the chrome's), or
//! which key leaves fullscreen (the keymap's, but for the override in [`crate::state`]).

use std::time::Instant;

use sighurt_api::engine::EngineCommand;
use sighurt_api::ids::{DocumentGeneration, ViewId};
use url::Url;

use crate::permissions;
use crate::state::{BrowserState, Effect, Frontend, Fx};
use crate::tabs::clamp;

/// The window's fullscreen and the page's, as the core tracks them
/// ([`BrowserState::fullscreen`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FullscreenState {
    /// The user's toggle (`browser.fullscreen`).
    pub wanted: bool,
    /// What the frontend last reported the window to be.
    pub window: bool,
    /// The view and document whose element is fullscreen, if any: always the shown view's.
    pub page: Option<(ViewId, DocumentGeneration)>,
}

/// What the core keeps of fullscreen: [`FullscreenState`], and its requests to the window.
#[derive(Debug, Default)]
pub(crate) struct Fullscreen {
    pub(crate) state: FullscreenState,
    /// The window state last asked for with [`Effect::SetFullscreen`].
    requested: bool,
    /// Requests sent since the window last changed (saturating).
    unanswered: u8,
}

impl BrowserState {
    /// The window's fullscreen and the page's.
    pub fn fullscreen(&self) -> FullscreenState {
        self.fullscreen.state
    }

    /// The view whose element is fullscreen, if any.
    pub(crate) fn page_fullscreen(&self) -> Option<ViewId> {
        self.fullscreen.state.page.map(|(view, _)| view)
    }

    /// `browser.fullscreen`: flips what the user wants. While a page holds fullscreen, it
    /// leaves that and the window's fullscreen. A notice in the terminal.
    pub(crate) fn toggle_fullscreen(&mut self, tab: usize, fx: &mut Fx) {
        if self.frontend() == Frontend::Terminal {
            fx.push(Effect::Notice {
                tab: Some(self.tabs[tab].id),
                message: "browser.fullscreen is not available in the terminal.".into(),
            });
            return;
        }
        let state = &mut self.fullscreen.state;
        state.wanted = match state.page {
            Some(_) => false,
            None => !state.wanted,
        };
        self.end_page_fullscreen(fx);
    }

    /// `page.exit-fullscreen`: leaves the page's fullscreen, or says there is none.
    pub(crate) fn exit_page_fullscreen(&mut self, tab: usize, fx: &mut Fx) {
        let shown = self.tabs[tab].current_view();
        if shown.is_some() && self.page_fullscreen() == shown {
            self.end_page_fullscreen(fx);
        } else {
            fx.push(Effect::Notice {
                tab: Some(self.tabs[tab].id),
                message: "The page shown is not in fullscreen.".into(),
            });
        }
    }

    /// The frontend reports what the window is now. An unchanged state changes nothing.
    pub(crate) fn window_fullscreen(&mut self, now: bool, fx: &mut Fx) {
        let f = &mut self.fullscreen;
        if now == f.state.window {
            return;
        }
        f.state.window = now;
        let replaced = std::mem::take(&mut f.unanswered) >= 2;
        fx.chrome = true;
        if now == f.requested || replaced {
            return;
        }
        // The compositor's own doing: the user's, through it.
        f.state.wanted = now;
        f.requested = now;
        if !now {
            self.end_page_fullscreen(fx);
        }
    }

    /// `view`'s engine says an element of `document` entered or left fullscreen, at `at`.
    pub(crate) fn page_fullscreen_changed(
        &mut self,
        view: ViewId,
        document: DocumentGeneration,
        fullscreen: bool,
        at: Instant,
        fx: &mut Fx,
    ) {
        // An engine that does not declare element fullscreen is never told to leave it.
        let engine = self.views[&view].engine.clone();
        if !self.capabilities(&engine).fullscreen {
            return;
        }
        let v = self.views.get_mut(&view).expect("checked by the caller");
        v.fullscreen = fullscreen;
        let held = self.fullscreen.state.page;
        if !fullscreen {
            if held.is_some_and(|(holder, _)| holder == view) {
                self.end_page_fullscreen(fx);
            }
            return;
        }
        if held == Some((view, document)) {
            return;
        }
        let allowed = self.shown_view() == Some(view)
            && self.views[&view].document == Some(document)
            && self.window_focused()
            && permissions::take_activation(
                &mut self.views.get_mut(&view).expect("shown").activation,
                at,
            );
        if !allowed {
            self.views.get_mut(&view).expect("checked").fullscreen = false;
            fx.push(Effect::Engine {
                view,
                command: EngineCommand::ExitFullscreen,
            });
            return;
        }
        self.end_page_fullscreen(fx);
        self.fullscreen.state.page = Some((view, document));
        let v = &self.views[&view];
        let who = v
            .url
            .as_deref()
            .and_then(|url| Url::parse(url).ok())
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "The page".into());
        fx.push(Effect::Notice {
            tab: Some(v.tab),
            message: clamp(format!("{who} is fullscreen. Escape leaves it.")),
        });
        fx.chrome = true;
    }

    /// Ends the page's fullscreen, if one holds it: the engine is told to leave unless it said
    /// it did, or its view is closing or gone.
    pub(crate) fn end_page_fullscreen(&mut self, fx: &mut Fx) {
        let Some((view, _)) = self.fullscreen.state.page.take() else {
            return;
        };
        fx.chrome = true;
        if let Some(v) = self
            .views
            .get_mut(&view)
            .filter(|v| !v.closing && v.fullscreen)
        {
            v.fullscreen = false;
            fx.push(Effect::Engine {
                view,
                command: EngineCommand::ExitFullscreen,
            });
        }
    }

    /// After every update: a page's fullscreen ends when its view is no longer the one shown,
    /// its document was replaced or failed, or the window lost the keyboard; the window is asked
    /// to be what the user wants, or fullscreen while a page holds it.
    pub(crate) fn settle_fullscreen(&mut self, fx: &mut Fx) {
        if let Some((view, document)) = self.fullscreen.state.page {
            let holds = self.shown_view() == Some(view)
                && self.views[&view].document == Some(document)
                && self.window_focused();
            if !holds {
                self.end_page_fullscreen(fx);
            }
        }
        if self.frontend() != Frontend::Gui {
            return;
        }
        let f = &mut self.fullscreen;
        let wanted = f.state.wanted || f.state.page.is_some();
        if wanted != f.requested {
            f.requested = wanted;
            f.unanswered = f.unanswered.saturating_add(1);
            fx.push(Effect::SetFullscreen(wanted));
        }
    }
}
