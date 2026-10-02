//! Carrying out the core's effects: opening and closing views with their targets, engine
//! commands, visibility and focus, the chrome's own commands, the page's cursor and text field,
//! services, notices and session writes.
//!
//! Effects are carried out in the order the core returned them. What they report back (an
//! engine refusing to open a view or take a command, a service's answer) is queued as a
//! `BrowserEvent` for the core, which sees it after the current batch, never re-entrantly.
//!
//! A view opens on its engine's runtime (started on first use) with a new target of the
//! engine's presentation mode, placed at once so the engine starts with the page area's
//! metrics. Closing follows plan §20: `sig` forgets the view, so nothing more is sent to it or
//! presented for it, takes the keyboard and the input method from its target, and closes it in
//! its engine, which destroys its graphics resources and drops its lease; the host retires the
//! target at its next flush. The core cancels the view's service requests itself
//! (`Effect::CancelService`).
//!
//! Must not own: decisions; an effect is carried out as the core stated it. Permission prompts
//! have no user interface yet, so they are answered "no" (fail closed) with a notice.

use std::process::ExitCode;
use std::time::Instant;

use sighurt_api::engine::{EngineCommand, EngineError, EngineWaker};
use sighurt_api::input::InputKind;
use sighurt_core::{BrowserEvent, Effect};
use sighurt_ui::extension::ChromeEvent;

use crate::app::{App, ViewEntry};
use crate::{chrome, registry};

impl App {
    /// Carries out one effect of the core.
    pub(crate) fn apply(&mut self, effect: Effect) {
        match effect {
            Effect::OpenView {
                view,
                engine,
                options,
            } => {
                if let Err(error) = self.open_view(view, &engine, options) {
                    self.queue
                        .push_back(BrowserEvent::OpenFailed { view, error });
                }
            }
            Effect::Engine { view, command } => {
                match &command {
                    EngineCommand::SetVisibility(visible) => {
                        if let Some(entry) = self.views.get_mut(&view) {
                            entry.visible = *visible;
                        }
                    }
                    // Navigation latency is measured only where the user waits for it.
                    EngineCommand::Navigate { .. }
                    | EngineCommand::Reload { .. }
                    | EngineCommand::Traverse { .. } => {
                        let active = self.in_active_tab(view);
                        if let Some(entry) = self.views.get_mut(&view) {
                            entry.navigated = active.then(Instant::now);
                        }
                    }
                    _ => {}
                }
                self.engine_command(view, command, true);
            }
            Effect::CloseView { view } => {
                let engine = self.views.get(&view).map(|entry| entry.engine.clone());
                self.forget_view(view);
                if let Some(runtime) = engine.and_then(|e| self.registry.running(&e)) {
                    runtime.close_view(view);
                }
            }
            Effect::Focus(focus) => {
                let actions = self.chrome.handle(ChromeEvent::Focus(focus));
                self.chrome_actions(actions);
            }
            Effect::ChromeInput(event) => {
                let event = match event.kind {
                    InputKind::Key(key) => ChromeEvent::Key(key),
                    InputKind::TextInput(text) => ChromeEvent::TextInput(text),
                    // Window focus reached the chrome already, whoever has the keyboard.
                    InputKind::Focus(_) | InputKind::Pointer(_) => return,
                };
                let actions = self.chrome.handle(event);
                self.chrome_actions(actions);
            }
            Effect::ChromeCommand(action) => {
                match chrome::command(&registry::chrome(self.chrome_name), action) {
                    Ok(event) => {
                        let actions = self.chrome.handle(event);
                        self.chrome_actions(actions);
                    }
                    Err(notice) => self.show_notice(notice),
                }
            }
            Effect::PageCursor(cursor) => {
                if let Some((_, entry)) = self.shown_view() {
                    let target = entry.target;
                    self.host.set_cursor(target, cursor);
                }
            }
            Effect::PageTextInput(state) => {
                let target = match &state {
                    Some(_) => self.shown_view().map(|(_, entry)| entry.target),
                    None => self.page_text_target,
                };
                if let Some(target) = target {
                    self.host.set_text_input(self.window, target, state.clone());
                    self.page_text_target = state.map(|_| target);
                }
            }
            Effect::Service {
                request, service, ..
            } => {
                let downloads = self.core.config().downloads();
                let performed = self
                    .services
                    .perform(&mut self.host, request, service, downloads);
                if let Some(result) = performed {
                    self.queue
                        .push_back(BrowserEvent::ServiceDone { request, result });
                }
            }
            Effect::CancelService(request) => self.services.cancel(&mut self.host, request),
            Effect::Prompt {
                request,
                origin,
                permission,
                ..
            } => {
                let who = origin.unwrap_or_else(|| "A page".into());
                self.show_notice(format!(
                    "{who} asked for {permission:?}; sig can't ask you yet, so it was refused."
                ));
                self.queue.push_back(BrowserEvent::PermissionAnswered {
                    request,
                    allow: false,
                });
            }
            Effect::Notice { message, .. } => self.show_notice(message),
            Effect::RedrawChrome => self.snapshot_dirty = true,
            Effect::SaveSession => self.session_changed(),
            Effect::Quit => {
                self.save_session();
                self.quit.get_or_insert(ExitCode::SUCCESS);
            }
        }
    }

    /// Opens `view` on `engine` with a new target, placed before the engine gets it.
    fn open_view(
        &mut self,
        view: sighurt_api::ids::ViewId,
        engine: &str,
        options: sighurt_api::engine::ViewOptions,
    ) -> Result<(), EngineError> {
        let mode = match self.registry.engine(engine) {
            Some(e) => e.mode,
            None => {
                return Err(EngineError::Runtime(format!(
                    "the {engine} engine is not part of this build"
                )))
            }
        };
        let wake = self.wake.clone();
        self.registry
            .start(engine, || EngineWaker::new(move || wake.ping()))
            .map_err(EngineError::Runtime)?;
        let target = self
            .host
            .create_target(self.window, mode, Some(view))
            .map_err(|e| EngineError::View {
                view,
                message: e.to_string(),
            })?;
        self.views.insert(
            view,
            ViewEntry {
                engine: engine.to_owned(),
                target: target.id(),
                visible: options.visible,
                navigated: None,
            },
        );
        self.place();
        let runtime = self.registry.running(engine).expect("started above");
        let opened = runtime.open_view(view, options, target);
        if opened.is_err() {
            // The engine dropped the target; the next placement leaves it out.
            self.views.remove(&view);
        }
        opened
    }
}
