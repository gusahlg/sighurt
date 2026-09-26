//! The tab strip, and the toolbar with its URL bar.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    canvas, div, font, point, px, Action, App, Bounds, ClickEvent, ClipboardItem, Context, Div,
    Entity, FocusHandle, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Rgba, ShapedLine, SharedString, Stateful, TextRun, Window,
};
use sighurt_core::browser::Browser;
use sighurt_core::commands::RunCommand;

use crate::{rendered, Colors};

/// Text of the URL bar with its caret and selection.
#[derive(Default)]
struct UrlInput {
    text: String,
    /// Caret byte offset in `text`.
    cursor: usize,
    /// Selection anchor; the selection runs between it and `cursor`.
    anchor: usize,
    /// True while the mouse button is held to drag-select.
    selecting: bool,
    /// The user changed the text, so it no longer follows the page's URL.
    edited: bool,
    /// Where the text was laid out, for mouse hit-testing.
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl UrlInput {
    /// Replaces the text, keeping it all selected if it was.
    fn set_text(&mut self, text: String) {
        let all = !self.text.is_empty() && self.selection() == (0..self.text.len());
        self.cursor = text.len();
        self.anchor = if all { 0 } else { self.cursor };
        self.text = text;
    }

    fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    fn selection(&self) -> Range<usize> {
        self.cursor.min(self.anchor)..self.cursor.max(self.anchor)
    }

    fn selected_text(&self) -> Option<String> {
        let range = self.selection();
        (!range.is_empty()).then(|| self.text[range].to_string())
    }

    /// Moves the caret to `offset`, extending the selection if `select`.
    fn move_to(&mut self, offset: usize, select: bool) {
        self.cursor = offset.min(self.text.len());
        if !select {
            self.anchor = self.cursor;
        }
    }

    /// The character boundary after the caret, or before it.
    fn boundary(&self, forward: bool) -> usize {
        let (before, after) = self.text.split_at(self.cursor);
        if forward {
            self.cursor + after.chars().next().map_or(0, char::len_utf8)
        } else {
            before.char_indices().next_back().map_or(0, |(i, _)| i)
        }
    }

    /// Moves the caret a character right or left, extending the selection if `select`; without
    /// `select`, a selection collapses to its end or start instead.
    fn step(&mut self, forward: bool, select: bool) {
        let range = self.selection();
        let to = match (forward, select || range.is_empty()) {
            (true, false) => range.end,
            (false, false) => range.start,
            (forward, true) => self.boundary(forward),
        };
        self.move_to(to, select);
    }

    /// Replaces the selection (or inserts at the caret) with `text`.
    fn insert(&mut self, text: &str) {
        let range = self.selection();
        self.text.replace_range(range.clone(), text);
        self.move_to(range.start + text.len(), false);
        self.edited = true;
    }

    fn delete(&mut self, forward: bool) {
        if self.selection().is_empty() {
            self.anchor = self.boundary(forward);
        }
        self.insert("");
    }
}

/// Shapes the URL bar's text the way it is painted.
fn shape(text: &str, color: Rgba, window: &Window) -> ShapedLine {
    let run = TextRun {
        len: text.len(),
        font: font(".SystemUIFont"),
        color: color.into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(SharedString::from(text.to_string()), px(14.0), &[run], None)
}

/// Text a key press types into the URL bar.
fn typed_text(keystroke: &Keystroke) -> Option<String> {
    let m = &keystroke.modifiers;
    if m.control || m.platform || m.function || m.alt {
        return None;
    }
    let text = keystroke
        .key_char
        .clone()
        .or_else(|| keystroke.clone().with_simulated_ime().key_char)?;
    // Enter and Tab arrive as "\n" and "\t"; the URL bar is single-line.
    (!text.chars().any(char::is_control)).then_some(text)
}

/// A click handler that runs `command`. Like a key binding, it dispatches [`RunCommand`], which
/// the root view handles.
fn run_on_click(command: impl Into<SharedString>) -> impl Fn(&ClickEvent, &mut Window, &mut App) {
    let command = RunCommand(command.into());
    move |_, window, cx| window.dispatch_action(command.boxed_clone(), cx)
}

/// A square toolbar button showing `icon`.
fn icon_button(
    id: &'static str,
    icon: &'static str,
    enabled: bool,
    colors: &Colors,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(28.0))
        .h(px(28.0))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_sm()
        .text_color(if enabled {
            colors.text_muted
        } else {
            colors.text_dim
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(colors.hover))
        })
        .child(icon)
}

/// One button per tab, each with a close button while there are several, and one for a new tab.
pub(crate) fn tab_strip(browser: &Browser, colors: &Colors) -> Div {
    rendered("tabs");
    let count = browser.tabs().len();
    div()
        .size_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_1()
        .border_b_1()
        .border_color(colors.border)
        .children(browser.tabs().iter().enumerate().map(|(i, tab)| {
            let active = i == browser.active();
            let close = run_on_click(format!("tab.close {}", i + 1));
            div()
                .id(("sig-tab", i))
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .min_w(px(140.0))
                .max_w(px(220.0))
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .text_sm()
                .when(active, |d| d.bg(colors.tab))
                .text_color(if active {
                    colors.text
                } else {
                    colors.text_muted
                })
                .child(div().flex_1().min_w_0().truncate().child(tab.title()))
                .on_click(run_on_click(format!("tab.select {}", i + 1)))
                .when(count > 1, |d| {
                    d.child(
                        div()
                            .id(("sig-tab-close", i))
                            .flex_shrink_0()
                            .cursor_pointer()
                            .text_color(colors.text_dim)
                            .hover(|style| style.text_color(colors.text))
                            .child("×")
                            .on_click(move |event, window, cx| {
                                // The tab behind the button would select itself too.
                                cx.stop_propagation();
                                close(event, window, cx);
                            }),
                    )
                })
        }))
        .child(
            div()
                .id("sig-new-tab")
                .debug_selector(|| "sig-new-tab".into())
                .ml_1()
                .px_2()
                .cursor_pointer()
                .text_color(colors.text_muted)
                .hover(|style| style.text_color(colors.text))
                .child("+")
                .on_click(run_on_click("tab.new")),
        )
}

/// Back, forward, reload or stop, and the URL bar.
pub(crate) struct Toolbar {
    browser: Entity<Browser>,
    colors: Colors,
    /// The URL bar's.
    focus: FocusHandle,
    /// The content area's, where Escape sends focus.
    content_focus: FocusHandle,
    url: UrlInput,
    /// The tab the URL bar shows. Switching tabs drops what was typed.
    url_tab: u64,
}

impl Toolbar {
    pub fn new(
        browser: Entity<Browser>,
        colors: Colors,
        content_focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&browser, |_, _, cx| cx.notify()).detach();
        Self {
            url_tab: browser.read(cx).active_tab().id(),
            browser,
            colors,
            focus: cx.focus_handle(),
            content_focus,
            url: UrlInput::default(),
        }
    }

    /// Focuses the URL bar with its text selected, ready to type over.
    pub fn focus_location(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_url(cx);
        self.url.select_all();
        window.focus(&self.focus);
        cx.notify();
    }

    /// Drops what was typed into the URL bar, which shows the page's URL again.
    pub fn discard_edit(&mut self, cx: &mut Context<Self>) {
        self.url.edited = false;
        cx.notify();
    }

    /// Shows the active tab's URL in the URL bar unless the user has typed something else.
    fn sync_url(&mut self, cx: &App) {
        let tab = self.browser.read(cx).active_tab();
        if tab.id() != self.url_tab {
            self.url_tab = tab.id();
            self.url.edited = false;
        }
        if !self.url.edited && self.url.text != tab.url() {
            self.url.set_text(tab.url().to_string());
        }
    }

    fn render_url_bar(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let colors = self.colors;
        let focused = self.focus.is_focused(window);
        let text = self.url.text.clone();
        let selection = self.url.selection();
        let caret = self.url.cursor;
        let bounds = self.url.bounds.clone();

        div()
            .id("sig-url-bar")
            .key_context("location")
            .track_focus(&self.focus)
            .flex_1()
            .flex()
            .flex_row()
            .items_center()
            .h(px(32.0))
            .px_3()
            .rounded_md()
            .bg(colors.surface)
            .border_1()
            .border_color(if focused { colors.focus } else { colors.border })
            .overflow_hidden()
            .on_key_down(cx.listener(Self::url_bar_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::url_bar_mouse_down))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.url.selecting = false),
            )
            .on_mouse_move(cx.listener(Self::url_bar_mouse_move))
            .child(
                canvas(
                    move |layout, window, _| {
                        bounds.set(layout);
                        (!text.is_empty()).then(|| shape(&text, colors.text, window))
                    },
                    move |bounds, line: Option<ShapedLine>, window, cx| {
                        let x = |index| line.as_ref().map_or(px(0.0), |l| l.x_for_index(index));
                        let span = |start: Pixels, end: Pixels| {
                            Bounds::from_corners(
                                point(bounds.origin.x + start, bounds.origin.y),
                                point(bounds.origin.x + end, bounds.origin.y + bounds.size.height),
                            )
                        };
                        if focused && !selection.is_empty() {
                            let area = span(x(selection.start), x(selection.end));
                            window.paint_quad(gpui::fill(area, colors.selection));
                        }
                        if let Some(line) = &line {
                            let _ = line.paint(bounds.origin, bounds.size.height, window, cx);
                        }
                        // A steady caret: blinking would need a timer running while focused.
                        if focused && selection.is_empty() {
                            let at = x(caret);
                            window.paint_quad(gpui::fill(span(at, at + px(2.0)), colors.text));
                        }
                    },
                )
                .flex_1()
                .h(px(16.0)),
            )
    }

    fn url_bar_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        let select = keystroke.modifiers.shift;
        let input = &mut self.url;
        match key {
            "escape" => {
                input.edited = false;
                window.focus(&self.content_focus);
            }
            "enter" => {
                let command = format!("page.open {}", input.text);
                window.dispatch_action(Box::new(RunCommand(command.into())), cx);
            }
            "a" if keystroke.modifiers.secondary() => input.select_all(),
            "c" | "x" if keystroke.modifiers.secondary() => {
                if let Some(text) = input.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    if key == "x" {
                        input.insert("");
                    }
                }
            }
            "v" if keystroke.modifiers.secondary() => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    input.insert(text.trim().replace(['\n', '\r'], "").as_str());
                }
            }
            "left" => input.step(false, select),
            "right" => input.step(true, select),
            "home" => input.move_to(0, select),
            "end" => input.move_to(input.text.len(), select),
            "backspace" => input.delete(false),
            "delete" => input.delete(true),
            _ => match typed_text(keystroke) {
                Some(text) => input.insert(&text),
                // Not ours: let key bindings and the rest of the window have it.
                None => return,
            },
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn url_bar_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus.is_focused(window) {
            // The first click selects everything, ready to type over.
            self.url.select_all();
        } else {
            let index = self.input_index(event.position.x, window);
            self.url.move_to(index, event.modifiers.shift);
            self.url.selecting = !event.modifiers.shift;
        }
        cx.notify();
    }

    fn url_bar_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The button may have been released outside the URL bar.
        self.url.selecting &= event.dragging();
        if self.url.selecting {
            let index = self.input_index(event.position.x, window);
            self.url.move_to(index, true);
            cx.notify();
        }
    }

    /// The URL bar text offset closest to window position `x`.
    fn input_index(&self, x: Pixels, window: &Window) -> usize {
        let input = &self.url;
        if input.text.is_empty() {
            return 0;
        }
        shape(&input.text, self.colors.text, window)
            .closest_index_for_x(x - input.bounds.get().origin.x)
    }
}

impl Render for Toolbar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        rendered("toolbar");
        self.sync_url(cx);
        let colors = &self.colors;
        let tab = self.browser.read(cx).active_tab();
        let reload = if tab.loading() {
            icon_button("sig-stop", "✕", true, colors).on_click(run_on_click("page.stop"))
        } else {
            icon_button("sig-reload", "↻", true, colors).on_click(run_on_click("page.reload"))
        };
        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(colors.border)
            .child(
                icon_button("sig-back", "◀", tab.can_go_back(), colors)
                    .on_click(run_on_click("page.back")),
            )
            .child(
                icon_button("sig-forward", "▶", tab.can_go_forward(), colors)
                    .on_click(run_on_click("page.forward")),
            )
            .child(reload)
            .child(self.render_url_bar(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> UrlInput {
        let mut input = UrlInput::default();
        input.set_text(text.to_string());
        input
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut url = input("https://example.com");
        url.select_all();
        url.insert("sighurt");
        assert_eq!(url.text, "sighurt");
        assert!(url.edited);
        assert_eq!(url.cursor, 7);
    }

    #[test]
    fn caret_moves_over_whole_characters() {
        let mut url = input("aé");
        url.step(false, false);
        assert_eq!(url.cursor, 1);
        url.step(true, true);
        assert_eq!(url.selected_text().as_deref(), Some("é"));
        url.delete(false);
        assert_eq!(url.text, "a");
        url.delete(false);
        assert_eq!(url.text, "");
        url.delete(false);
        assert_eq!(url.text, "");
    }

    #[test]
    fn a_new_url_stays_selected() {
        // Focusing the URL bar selects it all; a redirect meanwhile must not undo that.
        let mut url = input("https://a.test");
        url.select_all();
        url.set_text("https://a.test/".into());
        assert_eq!(url.selected_text().as_deref(), Some("https://a.test/"));
        url.step(false, false);
        url.set_text("https://b.test/".into());
        assert_eq!((url.cursor, url.anchor), (15, 15));
    }

    #[test]
    fn arrows_collapse_a_selection() {
        let mut url = input("abcd");
        url.select_all();
        url.step(false, false);
        assert_eq!((url.cursor, url.anchor), (0, 0));
        url.move_to(4, true);
        url.step(true, false);
        assert_eq!((url.cursor, url.anchor), (4, 4));
        // Stepping with nothing selected moves by a character, and stops at the ends.
        url.step(true, false);
        assert_eq!(url.cursor, 4);
        url.step(false, true);
        assert_eq!(url.selected_text().as_deref(), Some("d"));
    }

    #[test]
    fn typed_text_ignores_chords_and_control_characters() {
        let key = |s: &str| Keystroke::parse(s).unwrap();
        assert_eq!(typed_text(&key("a")).as_deref(), Some("a"));
        assert_eq!(typed_text(&key("ctrl-a")), None);
        assert_eq!(typed_text(&key("enter")), None);
        assert_eq!(typed_text(&key("tab")), None);
    }
}
