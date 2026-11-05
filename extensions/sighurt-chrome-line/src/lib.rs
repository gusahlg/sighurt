//! The line chrome, `[ui] chrome = "line"`: the whole chrome is one line, at the top of the
//! window or, with `[chrome.line] position = "bottom"`, at the bottom. It proves that the chrome
//! API (`sighurt_ui::extension`) is enough for a chrome whose surfaces, layout, editing and
//! drawing are its own: it uses sighurt-ui's helpers ([`Fonts`], [`TextEditor`], [`Pen`])
//! and nothing of the core, the host, a renderer or an engine.
//!
//! Unfocused, the line shows the active tab's place (`[2/5]`), the keymap's indicator, the
//! committed address (decoded where that is safe), a mark while the page loads, and on the right
//! the status text, cut to fit: the status always has up to half the line, so `sig`'s notices
//! stay visible however long the address. At its end, small and muted, is the engine the active
//! tab's page runs in as a button, "[servo]", if it fits whole in a third of the line
//! (`[chrome.line] engine-label = false` leaves it out). A click on it, exactly where it is
//! drawn, runs `page.switch-engine`; it brightens and is underlined under the pointer (which
//! becomes a hand), and is reversed while held. While a switch is under way it reads
//! "[servo -> wpe…]" on the selection color and clicks do nothing. `sig:` pages, blank tabs and
//! errors have none. A click on the rest of the line, or `location.focus`, makes it an editor: Enter
//! opens what was typed, a line starting with `:` runs a command, and Escape gives the page the
//! keyboard. `page.find` without text makes it the find field instead: a `/` in place of the tab's
//! place, each edit runs `page.find <text>` (one that leaves only whitespace `page.find-stop`),
//! Enter gives the page the keyboard keeping the search, Escape does so and runs
//! `page.find-stop`; the core moving the keyboard, or another tab, closes it and keeps the
//! search. What the search found ("3 of 12") shows before the status while the field is open,
//! and while the line is not being edited. There are no buttons, no tab strip
//! and no commands of its own: tabs are reached by keys and `:tab.select 3`. While the active tab
//! shows no engine's page (`sig://home`, an error), a few lines of text say what there is.
//!
//! Its surfaces, bottom to top: [`PAGE`] while it draws the page area, and [`LINE`]. While the
//! window is fullscreen, the line is listed only while it has the keyboard or there is status
//! text, over the page, which keeps the whole window. It repaints only what changed, and
//! nothing ticks: the caret is steady.
//!
//! Must not own: browser state (the core's, which arrives as snapshots), targets and placement
//! (`sig` and the host), or what a key does (the keymap's: only keys it passes reach the line).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::rc::Rc;

use sighurt_api::commands::{Command, MAX_FIND};
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{LogicalSize, SurfacePoint, SurfaceRect, WindowRect};
use sighurt_api::ids::RequestId;
use sighurt_api::input::{
    ButtonState, FocusEvent, Key, KeyEvent, KeyboardFocus, PointerButton, PointerEvent,
    TextInputState, TextPurpose,
};
use sighurt_api::settings::{Table, Value};
use sighurt_ui::extension::{
    contained, readable_url, Chrome, ChromeAction, ChromeEvent, ChromeKind, ChromeLayout,
    ChromeSurface, SurfaceName, TextField, Theme,
};
use sighurt_ui::state::{is_home, NO_SUCH_PAGE};
use sighurt_ui::text_edit::TextEditor;
use sighurt_ui::{Fonts, Pen, Rgba, ShapedText, Snapshot, TabContent, TextStyle};

/// The line.
pub const LINE: SurfaceName = "line";
/// The page area, while the line chrome draws a page there.
pub const PAGE: SurfaceName = "page";

/// The line's height at the default font size, 14 px.
const HEIGHT: f32 = 28.0;
/// Space at the line's ends and between its parts.
const GAP: f32 = 8.0;
/// Room for the loading mark, and its size.
const MARK_ROOM: f32 = 12.0;
const MARK: f32 = 6.0;
/// The caret's width.
const CARET: f32 = 2.0;
/// Characters kept of a failed command's message.
const MAX_ERROR: usize = 2000;

/// The line chrome, for the registry.
pub const KIND: ChromeKind = ChromeKind {
    name: "line",
    check,
    commands: &[],
    new: |fonts| Box::new(Line::new(fonts)),
};

/// Everything wrong with `[chrome.line]`.
fn check(settings: &Table) -> Vec<String> {
    let problem = |(key, value): (&String, &Value)| {
        let given = match value.as_str() {
            Some(text) => format!("{text:?}"),
            None => value.kind().to_owned(),
        };
        match (key.as_str(), value.as_str(), value.as_bool()) {
            ("position", Some("top" | "bottom"), _) | ("engine-label", _, Some(_)) => None,
            ("position", ..) => Some(format!(
                "position: expected \"top\" or \"bottom\", not {given}"
            )),
            ("engine-label", ..) => {
                Some(format!("engine-label: expected true or false, not {given}"))
            }
            _ => Some(format!(
                "{key}: unknown key (the line chrome has position and engine-label)"
            )),
        }
    };
    settings.iter().filter_map(problem).collect()
}

/// The line chrome.
pub struct Line {
    fonts: Fonts,
    theme: Theme,
    /// `[chrome.line] position = "bottom"`.
    bottom: bool,
    /// `[chrome.line] engine-label`: the engine's name at the line's end.
    engine_label: bool,
    window: LogicalSize,
    snapshot: Snapshot,
    layout: ChromeLayout,
    /// What is being typed, while the line has the keyboard.
    editor: TextEditor,
    focused: bool,
    /// Whether the window has the keyboard: the caret and the selection show only then.
    window_active: bool,
    /// Why the last `:command` failed, until the line changes.
    error: Option<String>,
    /// The clipboard read the line waits for: the latest Ctrl+V's, while it has the keyboard
    /// and its tab.
    paste: Option<RequestId>,
    /// While the line is the find field: the text it last searched for, empty when it ended
    /// the search.
    find: Option<String>,
    /// The field the executable was last told about, so only changes are reported.
    text_input: Option<TextInputState>,
    cursor: Cursor,
    /// The pointer is over the engine button.
    label_hover: bool,
    /// The primary button went down on the engine button and is still down.
    label_pressed: bool,
    /// The line, and the page it draws, show something new.
    line_changed: bool,
    page_changed: bool,
}

/// The line as drawn: its parts shaped, and where they are, in window coordinates.
struct View {
    /// The top of the text's line.
    top: f32,
    prefix: Rc<ShapedText>,
    /// The address's room: its left and right ends.
    field: (f32, f32),
    /// The address, or what is being typed, and where its origin is.
    text: Rc<ShapedText>,
    text_x: f32,
    caret: Option<f32>,
    selection: Option<(f32, f32)>,
    preedit: Option<(f32, f32)>,
    /// The loading mark's left edge, while the page loads.
    mark: Option<f32>,
    /// The status text, where it starts, and whether it says what went wrong.
    status: Option<(Rc<ShapedText>, f32, bool)>,
    /// What the search found ("3 of 12"), and where it starts.
    find: Option<(Rc<ShapedText>, f32)>,
    /// The engine of the active tab's page as a bracketed button, and where its text's origin
    /// is, and whether a switch is under way.
    engine: Option<(Rc<ShapedText>, f32, f32, bool)>,
}

impl Line {
    /// The line chrome with `fonts`, for a window of no size yet.
    pub fn new(fonts: Fonts) -> Self {
        let mut line = Self {
            fonts,
            theme: Theme::default(),
            bottom: false,
            engine_label: true,
            label_hover: false,
            label_pressed: false,
            window: LogicalSize::default(),
            snapshot: Snapshot::default(),
            layout: ChromeLayout::default(),
            editor: TextEditor::default(),
            focused: false,
            window_active: false,
            error: None,
            paste: None,
            find: None,
            text_input: None,
            cursor: Cursor::Default,
            line_changed: true,
            page_changed: true,
        };
        line.relayout();
        line
    }

    /// `px` logical pixels at the default font size, at the configured one.
    fn scaled(&self, px: f32) -> f32 {
        px * f32::from(self.theme.font_size) / 14.0
    }

    fn style(&self) -> TextStyle {
        TextStyle::SM.at(self.theme.font_size)
    }

    fn line_rect(&self) -> WindowRect {
        let LogicalSize { width, height } = self.window;
        let line = (self.scaled(HEIGHT).round() as u32).min(height);
        let y = if self.bottom { height - line } else { 0 };
        WindowRect {
            x: 0,
            y: y as i32,
            width,
            height: line,
        }
    }

    fn page_rect(&self) -> WindowRect {
        let line = self.line_rect();
        let y = if self.bottom { 0 } else { line.height };
        WindowRect {
            x: 0,
            y: y as i32,
            width: self.window.width,
            height: self.window.height - line.height,
        }
    }

    /// Whether the active tab shows a page the line chrome draws: `sig:` pages and errors.
    fn draws_page(&self) -> bool {
        let content = self.snapshot.active().map(|tab| &tab.content);
        matches!(content, Some(TabContent::Internal | TabContent::Error(_)))
    }

    fn relayout(&mut self) {
        let line = ChromeSurface::new(LINE, self.line_rect());
        if self.snapshot.fullscreen && !self.draws_page() {
            // Over the page, which keeps the whole window, and only while it is needed.
            let window = WindowRect {
                x: 0,
                y: 0,
                width: self.window.width,
                height: self.window.height,
            };
            let needed = self.focused || self.error.is_some() || self.snapshot.status.is_some();
            let surfaces = needed.then_some(line).into_iter().collect();
            self.layout = ChromeLayout::new(window, surfaces);
            return;
        }
        let page = self.page_rect();
        let page_surface = self.draws_page().then(|| ChromeSurface::new(PAGE, page));
        self.layout = ChromeLayout::new(page, page_surface.into_iter().chain([line]).collect());
    }

    /// Whether the latest layout lists the line.
    fn lists_line(&self) -> bool {
        self.layout
            .surfaces
            .iter()
            .any(|surface| surface.name == LINE)
    }

    fn set_snapshot(&mut self, snapshot: Snapshot) {
        if snapshot == self.snapshot {
            return;
        }
        let old = std::mem::replace(&mut self.snapshot, snapshot);
        self.line_changed |= in_line(&old) != in_line(&self.snapshot);
        self.page_changed |= in_page(&old) != in_page(&self.snapshot);
        let active = |snapshot: &Snapshot| snapshot.active().map(|tab| tab.id);
        if active(&old) != active(&self.snapshot) {
            // Another tab: what was typed for the last one goes, with its paste, and the find
            // field closes.
            self.paste = None;
            self.find = None;
            if self.focused {
                self.editor.set_text(&self.address());
                self.editor.select_all();
            }
        }
    }

    /// The active tab's committed address, as the line shows it.
    fn address(&self) -> String {
        let url = self.snapshot.active().map_or("", |tab| tab.url.as_str());
        readable_url(url).into_owned()
    }

    /// The line takes the keyboard, with the address selected, ready to type over.
    fn focus(&mut self) {
        if !self.focused {
            self.focused = true;
            self.editor.set_text(&self.address());
            self.error = None;
        }
        self.editor.select_all();
    }

    /// The line becomes the find field, holding `text` selected (`page.find` without text,
    /// right after the core focused the line).
    fn find_field(&mut self, text: &str) {
        self.find = Some(text.trim().to_owned());
        self.editor.set_text(text);
        self.editor.select_all();
        self.error = None;
    }

    /// The page takes the keyboard: what was typed goes, and the find field closes.
    fn unfocus(&mut self) {
        self.focused = false;
        self.find = None;
        self.paste = None;
        self.error = None;
        self.editor.clear_preedit();
    }

    fn pointer(&mut self, surface: SurfaceName, event: &PointerEvent) -> Vec<ChromeAction> {
        let mut actions = Vec::new();
        match (surface, event) {
            (
                LINE,
                PointerEvent::Button {
                    button: PointerButton::Left,
                    state: ButtonState::Pressed,
                    position,
                    modifiers,
                },
            ) => {
                if self.on_engine_label(*position) {
                    // Held until release. While a switch is under way the press is swallowed:
                    // it neither focuses the line nor runs the command.
                    if !self.engine_busy() {
                        self.label_pressed = true;
                        self.line_changed = true;
                    }
                } else if self.focused {
                    let x = self.line_rect().x as f32 + position.x as f32;
                    let view = self.view();
                    if view.preedit.is_none() {
                        let offset = view.text.offset_at(x - view.text_x);
                        self.editor.move_to(offset, modifiers.shift);
                    }
                } else {
                    self.focus();
                    actions.push(ChromeAction::Focus(KeyboardFocus::Chrome));
                }
            }
            (
                PAGE,
                PointerEvent::Button {
                    button: PointerButton::Left,
                    state: ButtonState::Pressed,
                    ..
                },
            ) if self.focused => {
                self.unfocus();
                actions.push(ChromeAction::Focus(KeyboardFocus::Page));
            }
            (
                LINE,
                PointerEvent::Button {
                    button: PointerButton::Left,
                    state: ButtonState::Released,
                    position,
                    ..
                },
            ) => {
                // A click: the press landed on the button and the release still does.
                let pressed = std::mem::take(&mut self.label_pressed);
                if pressed {
                    self.line_changed = true;
                    if self.on_engine_label(*position) && !self.engine_busy() {
                        actions.push(ChromeAction::Command(Command::PageSwitchEngine));
                    }
                }
            }
            (_, PointerEvent::Enter { position } | PointerEvent::Motion { position, .. }) => {
                let over = surface == LINE && self.on_engine_label(*position);
                if over != self.label_hover {
                    self.label_hover = over;
                    self.line_changed = true;
                }
                let busy = over && self.engine_busy();
                let cursor = match (surface, over, busy) {
                    (LINE, true, false) => Cursor::Pointer,
                    // Busy: clicks do nothing, so the cursor does not invite another one.
                    (LINE, true, true) => Cursor::Default,
                    (LINE, _, _) => Cursor::Text,
                    _ => Cursor::Default,
                };
                self.set_cursor(cursor, &mut actions);
            }
            (_, PointerEvent::Leave) => {
                self.line_changed |= self.label_hover || self.label_pressed;
                self.label_hover = false;
                self.label_pressed = false;
                self.set_cursor(Cursor::Default, &mut actions);
            }
            _ => {}
        }
        actions
    }

    /// Whether `position` (local to the line's surface) lies in the engine button's line box.
    /// Half-open.
    fn on_engine_label(&mut self, position: SurfacePoint) -> bool {
        let line = self.line_rect();
        let px = line.x as f32 + position.x as f32;
        let py = line.y as f32 + position.y as f32;
        let Some((x, y, width, height)) = self
            .view()
            .engine
            .map(|(shaped, x, y, _)| (x, y, shaped.width, shaped.height))
        else {
            return false;
        };
        px >= x && py >= y && px < x + width && py < y + height
    }

    /// Whether the engine button is shown and a switch is under way.
    fn engine_busy(&mut self) -> bool {
        self.view().engine.is_some_and(|(.., busy)| busy)
    }

    fn set_cursor(&mut self, cursor: Cursor, actions: &mut Vec<ChromeAction>) {
        if cursor != self.cursor {
            self.cursor = cursor;
            actions.push(ChromeAction::Cursor(cursor));
        }
    }

    /// A key press the keymap passed, while the line has the keyboard.
    fn key(&mut self, event: &KeyEvent) -> Vec<ChromeAction> {
        let mut actions = Vec::new();
        let select = event.modifiers.shift;
        // Control and Alt as modifiers, not as keys the layout used to make a character.
        let control = event.modifiers.control && !event.consumed.control;
        let alt = event.modifiers.alt && !event.consumed.alt;
        let editor = &mut self.editor;
        match &event.key {
            Key::Named(name) => match name.as_str() {
                "Escape" => {
                    let finding = self.find.is_some();
                    self.unfocus();
                    actions.push(ChromeAction::Focus(KeyboardFocus::Page));
                    if finding {
                        actions.push(ChromeAction::Command(Command::PageFindStop));
                    }
                }
                "Enter" => self.submit(&mut actions),
                "ArrowLeft" | "ArrowRight" => editor.move_grapheme(name == "ArrowRight", select),
                "Home" => editor.move_to(0, select),
                "End" => editor.move_to(editor.text().len(), select),
                "Backspace" | "Delete" => editor.delete_grapheme(name == "Delete"),
                _ => {}
            },
            _ if control => match shortcut_letter(event) {
                Some('a') => editor.select_all(),
                Some(letter @ ('c' | 'x')) => {
                    if let Some(text) = editor.selected_text() {
                        actions.push(ChromeAction::Copy(text.to_owned()));
                        if letter == 'x' {
                            editor.insert("");
                        }
                    }
                }
                Some('v') => {
                    let request = RequestId::next();
                    self.paste = Some(request);
                    actions.push(ChromeAction::Paste(request));
                }
                _ => {}
            },
            _ => {
                if let Some(text) = event.text.as_deref().filter(|_| !alt) {
                    editor.insert(text);
                }
            }
        }
        actions
    }

    /// Enter: open what was typed, or run it as a command after a `:`. The keyboard goes to the
    /// page before a command runs, so a command for the page finds it there.
    fn submit(&mut self, actions: &mut Vec<ChromeAction>) {
        // The find field: the match found is the one wanted, and the search stays.
        if self.find.is_some() {
            self.unfocus();
            actions.push(ChromeAction::Focus(KeyboardFocus::Page));
            return;
        }
        let line = self.editor.text().trim().to_owned();
        if let Some(command) = line.strip_prefix(':') {
            match command.parse::<Command>() {
                Ok(command) => {
                    self.unfocus();
                    actions.push(ChromeAction::Focus(KeyboardFocus::Page));
                    actions.push(ChromeAction::Command(command));
                }
                Err(error) => {
                    let message = format!(":{}: {error}", command.trim());
                    self.error = Some(contained(&message, MAX_ERROR));
                }
            }
        } else if line.is_empty() {
            self.unfocus();
            actions.push(ChromeAction::Focus(KeyboardFocus::Page));
        } else {
            // The core gives the page the keyboard once it opens it.
            actions.push(ChromeAction::Command(Command::PageOpen(line)));
        }
    }

    /// The line as drawn now.
    fn view(&mut self) -> View {
        let style = self.style();
        let line = self.line_rect();
        let gap = self.scaled(GAP);
        let (left, right) = (
            line.x as f32 + gap,
            (line.x + line.width as i32) as f32 - gap,
        );
        let height = f32::from(style.line_height);
        let top = line.y as f32 + ((line.height as f32 - height) / 2.0).floor();

        let tabs = &self.snapshot.tabs;
        let active = tabs.iter().position(|tab| tab.active);
        let mut prefix = active.map_or(String::new(), |i| format!("[{}/{}]", i + 1, tabs.len()));
        if let Some(indicator) = &self.snapshot.keymap {
            prefix.push_str("  ");
            prefix.push_str(indicator);
        }
        // The find field's prompt in its place.
        if self.find.is_some() {
            prefix = "/".into();
        }
        let prefix = self.fonts.line(&prefix, style);

        // The engine at the end, smaller, on the line's baseline, if it fits whole in a third
        // of the line.
        let small = TextStyle::XS.at(self.theme.font_size);
        let active = self.snapshot.active().filter(|_| self.engine_label);
        let engine = active.and_then(|tab| {
            let name = tab.engine_label()?;
            let target = tab.switching_label();
            let busy = target.is_some();
            Some(match target {
                Some(target) => (format!("[{name} -> {target}\u{2026}]"), busy),
                None => (format!("[{name}]"), busy),
            })
        });
        let engine = engine.map(|(text, busy)| (self.fonts.line(&text, small), busy));
        let engine = engine.filter(|(shaped, _)| shaped.width <= line.width as f32 / 3.0);
        let engine_top = top + self.fonts.baseline(style) - self.fonts.baseline(small);
        let engine = engine.map(|(shaped, busy)| (right - shaped.width, shaped, busy));
        // Twice the gap before it, since the status beside it is muted too.
        let right = engine.as_ref().map_or(right, |(x, ..)| x - 2.0 * gap);

        let status = match (&self.error, &self.snapshot.status) {
            (Some(error), _) => Some((error.clone(), true)),
            // A hovered link's address reads decoded, as the line's own does.
            (None, Some(status)) => Some((readable_url(status).into_owned(), false)),
            (None, None) => None,
        };
        let half = (line.width as f32 / 2.0 - gap).max(0.0);
        let status = status.map(|(text, error)| {
            let shaped = self.fonts.fitted(&text, style, half);
            let x = right - shaped.width;
            (shaped, x, error)
        });
        // What the search found, before the status, in the find field and while the line is
        // not being edited.
        let right = status.as_ref().map_or(right, |(_, x, _)| x - gap);
        let shown = self.find.is_some() || !self.focused;
        let engine_name = self.snapshot.active().and_then(|tab| tab.engine_label());
        let find = (self.snapshot.find.as_ref())
            .filter(|_| shown)
            .map(|find| find.indicator(engine_name.as_deref()))
            .filter(|text| !text.is_empty())
            .map(|text| {
                let shaped = self.fonts.fitted(&text, style, line.width as f32 / 4.0);
                (right - shaped.width, shaped)
            });

        let loading = self.snapshot.active().is_some_and(|tab| tab.loading);
        let mark_room = if loading { self.scaled(MARK_ROOM) } else { 0.0 };
        let start = match prefix.width > 0.0 {
            true => left + prefix.width + gap,
            false => left,
        };
        let end = find.as_ref().map_or(right, |(x, _)| x - gap) - mark_room;
        let field = (start, end.max(start));
        let mark = loading.then_some(field.1);

        let (text, text_x, caret, selection, preedit) = if self.focused {
            let display = self.editor.display();
            let text = self.fonts.line(&display.text, style);
            let x = field.0 - self.editor.scroll();
            let span = |range: &std::ops::Range<usize>| {
                let (a, b) = (text.x_at(range.start), text.x_at(range.end));
                (x + a.min(b), x + a.max(b))
            };
            let caret = display.caret.map(|offset| x + text.x_at(offset));
            let selection = (!display.selection.is_empty()).then(|| span(&display.selection));
            let preedit = display.preedit.as_ref().map(span);
            (text, x, caret, selection, preedit)
        } else {
            let address = self.address();
            let text = self.fonts.fitted(&address, style, field.1 - field.0);
            (text, field.0, None, None, None)
        };
        View {
            top,
            prefix,
            field,
            text,
            text_x,
            caret,
            selection,
            preedit,
            mark,
            status,
            find: find.map(|(x, shaped)| (shaped, x)),
            engine: engine.map(|(x, shaped, busy)| (shaped, x, engine_top, busy)),
        }
    }

    /// Keeps the caret in view and reports what the executable needs to know.
    fn finish(&mut self, mut actions: Vec<ChromeAction>) -> Vec<ChromeAction> {
        // The find field searches as it is edited.
        if let Some(searched) = &mut self.find {
            let text = cut(self.editor.text().trim(), MAX_FIND);
            if text != searched {
                *searched = text.to_owned();
                actions.push(ChromeAction::Command(match text.is_empty() {
                    true => Command::PageFindStop,
                    false => Command::PageFind(Some(text.to_owned())),
                }));
            }
        }
        let before = self.editor.scroll();
        if self.focused {
            let view = self.view();
            let caret = view.caret.map_or(0.0, |x| x - view.text_x);
            let room = view.field.1 - view.field.0;
            let width = view.text.width;
            self.editor.scroll_to_caret(caret, CARET, width, room);
        } else {
            self.editor.reset_scroll();
        }
        self.line_changed |= self.editor.scroll() != before;

        let text_input = self.focused.then(|| self.text_input_state());
        if text_input != self.text_input {
            self.text_input = text_input.clone();
            let field = text_input.map(|state| TextField::new(LINE, state));
            actions.push(ChromeAction::TextInput(field));
        }
        let listed = self.lists_line();
        self.relayout();
        // A line listed again after fullscreen hid it is drawn again, changed or not.
        let changed = std::mem::take(&mut self.line_changed);
        if self.lists_line() && (changed || !listed) {
            actions.push(ChromeAction::Repaint(LINE));
        }
        if std::mem::take(&mut self.page_changed) && self.draws_page() {
            actions.push(ChromeAction::Repaint(PAGE));
        }
        actions
    }

    fn text_input_state(&mut self) -> TextInputState {
        let view = self.view();
        let line = self.line_rect();
        let x = view.caret.unwrap_or(view.text_x) - line.x as f32;
        TextInputState {
            caret: SurfaceRect {
                x: x.round() as i32,
                y: view.top as i32 - line.y,
                width: CARET as u32,
                height: self.style().line_height.into(),
            },
            surrounding: Some(self.editor.surrounding()),
            purpose: match self.find {
                Some(_) => TextPurpose::Normal,
                None => TextPurpose::Url,
            },
            multiline: false,
        }
    }

    fn paint_line(&mut self, pen: &mut Pen) {
        let colors = self.theme.colors;
        pen.clear(Rgba {
            a: 255,
            ..colors.bg
        });
        let line = self.line_rect();
        let [x, y, width, height] = [
            line.x as f32,
            line.y as f32,
            line.width as f32,
            line.height as f32,
        ];
        // A line between it and the page.
        let edge = if self.bottom { y } else { y + height - 1.0 };
        pen.fill([x, edge, width, 1.0], colors.border);

        let view = self.view();
        let gap = self.scaled(GAP);
        let text_height = f32::from(self.style().line_height);
        let all = [x, y, width, height];
        pen.text(&view.prefix, (x + gap, view.top), colors.text_muted, all);
        let (from, to) = view.field;
        let field = [from, y, to - from, height];
        if self.focused {
            // The field being edited stands out from the rest of the line.
            let margin = self.scaled(MARK_ROOM - MARK);
            let back = [
                from - margin / 2.0,
                y + 2.0,
                to - from + margin,
                height - 4.0,
            ];
            pen.fill(back, colors.surface);
        }
        let clipped = |(a, b): (f32, f32)| {
            let (a, b) = (a.max(from), b.min(to));
            (a < b).then_some([a, view.top, b - a, text_height])
        };
        let active = self.focused && self.window_active;
        if let Some(selection) = view.selection.filter(|_| active).and_then(clipped) {
            pen.fill(selection, colors.selection);
        }
        let at = (view.text_x, view.top);
        pen.text(&view.text, at, colors.text, field);
        if let Some(preedit) = view.preedit.and_then(clipped) {
            let underline = view.top + text_height - self.scaled(2.0);
            pen.fill([preedit[0], underline, preedit[2], 1.0], colors.text);
        }
        let caret = view.caret.filter(|_| active && view.selection.is_none());
        if let Some(caret) = caret.filter(|&caret| caret >= from - 1.0 && caret <= to + 1.0) {
            pen.fill([caret, view.top, CARET, text_height], colors.text);
        }
        if let Some(mark) = view.mark {
            let size = self.scaled(MARK);
            let left = mark + (self.scaled(MARK_ROOM) - size) / 2.0;
            let top = y + (height - size) / 2.0;
            pen.fill([left, top, size, size], colors.text_muted);
        }
        if let Some((status, at, error)) = &view.status {
            let color = if *error {
                colors.error
            } else {
                colors.text_muted
            };
            pen.text(status, (*at, view.top), color, all);
        }
        if let Some((find, at)) = &view.find {
            pen.text(find, (*at, view.top), colors.text_muted, all);
        }
        if let Some((engine, x, top, busy)) = &view.engine {
            // Muted at rest, bright and underlined under the pointer, reversed while held, and
            // on the selection's color while a switch is under way.
            let over = self.label_hover && !busy;
            let held = over && self.label_pressed;
            let (color, back) = match (*busy, held, over) {
                (true, ..) => (colors.text, Some(colors.selection)),
                (_, true, _) => (
                    Rgba {
                        a: 255,
                        ..colors.bg
                    },
                    Some(colors.text),
                ),
                (_, _, true) => (colors.text, None),
                _ => (colors.text_muted, None),
            };
            let bounds = [*x, *top, engine.width, engine.height];
            if let Some(back) = back {
                pen.fill(bounds, back);
            }
            pen.text(engine, (*x, *top), color, all);
            if over && !held {
                let under = *top + engine.height - 1.0;
                pen.fill([*x, under, engine.width, 1.0], color);
            }
        }
    }

    /// A few lines of text about the page the active tab can't show in an engine.
    fn page_lines(&self) -> Vec<(String, Rgba)> {
        let colors = &self.theme.colors;
        let Some(tab) = self.snapshot.active() else {
            return Vec::new();
        };
        match &tab.content {
            TabContent::Internal if is_home(&tab.url) => {
                let home = &self.snapshot.home;
                let mut lines = vec![
                    ("Sighurt".to_owned(), colors.text),
                    (
                        "Click the line, or use location.focus, to type an address, a search or \
                         a command after a colon, such as :tab.new."
                            .to_owned(),
                        colors.text_muted,
                    ),
                    (
                        format!("The configuration is in {}.", home.config_path),
                        colors.text_muted,
                    ),
                ];
                for engine in &home.engines {
                    let color = match engine.available {
                        true => colors.text_muted,
                        false => colors.error,
                    };
                    lines.push((format!("{}: {}", engine.name, engine.detail), color));
                }
                lines
            }
            TabContent::Internal => vec![(NO_SUCH_PAGE.to_owned(), colors.text_muted)],
            TabContent::Error(message) => vec![
                (
                    format!("Can't show {}", readable_url(&tab.url)),
                    colors.text,
                ),
                (message.clone(), colors.text_muted),
            ],
            TabContent::Engine | TabContent::Blank => Vec::new(),
        }
    }

    fn paint_page(&mut self, pen: &mut Pen) {
        let colors = self.theme.colors;
        pen.clear(Rgba {
            a: 255,
            ..colors.bg
        });
        let page = self.page_rect();
        let [x, y, width, height] = [
            page.x as f32,
            page.y as f32,
            page.width as f32,
            page.height as f32,
        ];
        let gap = 2.0 * self.scaled(GAP);
        let style = self.style();
        let mut top = y + gap;
        for (text, color) in self.page_lines() {
            let shaped = self
                .fonts
                .wrapped(&text, style, (width - 2.0 * gap).max(1.0));
            pen.text(&shaped, (x + gap, top), color, [x, y, width, height]);
            top += shaped.height + self.scaled(GAP) / 2.0;
        }
    }
}

/// What the line shows of `snapshot`.
fn in_line(snapshot: &Snapshot) -> impl PartialEq {
    let active = snapshot.tabs.iter().position(|tab| tab.active);
    let tab = snapshot.active().map(|tab| {
        (
            tab.url.clone(),
            tab.loading,
            tab.engine_label(),
            tab.switching_label(),
        )
    });
    let count = snapshot.tabs.len();
    (
        active,
        count,
        tab,
        snapshot.keymap.clone(),
        snapshot.status.clone(),
        snapshot.find.clone(),
    )
}

/// What the page the line chrome draws shows of `snapshot`, if it draws one.
fn in_page(snapshot: &Snapshot) -> impl PartialEq {
    let tab = snapshot.active();
    let drawn =
        tab.filter(|tab| matches!(tab.content, TabContent::Internal | TabContent::Error(_)));
    drawn.map(|tab| (tab.content.clone(), tab.url.clone(), snapshot.home.clone()))
}

/// `text` cut to at most `max` bytes at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The letter of a Ctrl shortcut: the key's character, or for layouts without Latin letters
/// (Cyrillic, Greek, ...) the physical key's, so Ctrl+V pastes on every layout.
fn shortcut_letter(event: &KeyEvent) -> Option<char> {
    if let Key::Character(text) = &event.key {
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_alphabetic() {
                return Some(c.to_ascii_lowercase());
            }
        }
    }
    let code = event.code.w3c_code()?.strip_prefix("Key")?;
    code.chars().next().map(|c| c.to_ascii_lowercase())
}

impl Chrome for Line {
    fn handle(&mut self, event: ChromeEvent) -> Vec<ChromeAction> {
        let before = (
            self.editor.clone(),
            self.error.clone(),
            self.focused,
            self.find.is_some(),
        );
        let actions = match event {
            ChromeEvent::Configure { theme, settings } => {
                let position = settings.get("position").and_then(Value::as_str);
                self.bottom = position == Some("bottom");
                let label = settings.get("engine-label").and_then(Value::as_bool);
                self.engine_label = label.unwrap_or(true);
                self.fonts.set_family(theme.font.as_deref());
                self.theme = theme;
                (self.line_changed, self.page_changed) = (true, true);
                Vec::new()
            }
            ChromeEvent::Resize(size) => {
                if size != self.window {
                    self.window = size;
                    (self.line_changed, self.page_changed) = (true, true);
                }
                Vec::new()
            }
            ChromeEvent::Snapshot(snapshot) => {
                self.set_snapshot(snapshot);
                Vec::new()
            }
            ChromeEvent::Focus(KeyboardFocus::Chrome) => {
                // The find field closes and the line holds the address again; the search stays.
                if self.find.take().is_some() {
                    self.focused = false;
                }
                self.focus();
                Vec::new()
            }
            ChromeEvent::Focus(KeyboardFocus::Page) => {
                self.unfocus();
                Vec::new()
            }
            // `location.focus <text>`: typed over the address the focus selected.
            ChromeEvent::LineText(text) if self.focused => {
                self.editor.select_all();
                self.editor.insert(&text);
                Vec::new()
            }
            // `page.find` without text: the line becomes the find field.
            ChromeEvent::FindField(text) if self.focused => {
                self.find_field(&text);
                Vec::new()
            }
            ChromeEvent::WindowFocus(event) => {
                self.window_active = event == FocusEvent::Activated;
                if !self.window_active {
                    self.line_changed |= self.label_pressed;
                    self.label_pressed = false;
                }
                self.line_changed |= self.focused;
                Vec::new()
            }
            ChromeEvent::Pointer { surface, event } => self.pointer(surface, &event),
            ChromeEvent::Key(key) if self.focused && key.state == ButtonState::Pressed => {
                self.key(&key)
            }
            ChromeEvent::TextInput(event) if self.focused => {
                self.editor.input_method(&event);
                Vec::new()
            }
            ChromeEvent::Pasted { request, text } if self.paste == Some(request) => {
                self.paste = None;
                self.editor.paste(&text);
                Vec::new()
            }
            // Keys and text for a line without the keyboard, a late paste, and commands,
            // which the line chrome has none of.
            _ => Vec::new(),
        };
        let (editor, error, focused, find) = before;
        self.line_changed |= self.find.is_some() != find;
        if self.editor != editor {
            self.line_changed = true;
            if self.editor.text() != editor.text() {
                // The line changed: the error about it goes.
                self.error = None;
            }
        }
        self.line_changed |= self.error != error || self.focused != focused;
        self.finish(actions)
    }

    fn layout(&self) -> &ChromeLayout {
        &self.layout
    }

    fn draw(&mut self, surface: SurfaceName, pen: &mut Pen) {
        match surface {
            LINE => self.paint_line(pen),
            PAGE => self.paint_page(pen),
            _ => pen.clear(Rgba {
                a: 255,
                ..self.theme.colors.bg
            }),
        }
    }
}
