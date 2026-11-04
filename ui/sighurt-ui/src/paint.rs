//! The standard chrome's drawing: what [`Model`] shows on each of its surfaces, as a scene drawn
//! with a [`Pen`], which the backend the frontend chose renders.
//!
//! Each surface is drawn whole, so a buffer that last held an older frame (or garbage) comes
//! out complete: the tab strip and the toolbar on [`CHROME`], the tab list on [`TABS`], the
//! status line on [`STATUS`] (a strip, or an overlay with a background and border of its own,
//! since it lies over the page), and a page the chrome draws itself on [`PAGE`]. Drawing by
//! surface rather than by rectangle keeps the overlay out of the page area's buffer below it.
//! Rectangles come from the layout the hit test uses, so controls stay under the pointer; icon
//! and text sizes scale with the font size as the layout does. An engine page's tab names its
//! engine in small muted text before the close button, on the title's baseline and inside the
//! tab, where a click selects the tab as anywhere else on it; the title is cut before the label,
//! which shows only whole, and only if it fits in half the tab's room for text less a gap, so
//! tabs of one width agree whatever their titles. At the address field's right end the same
//! name is a button: a rounded outline in the theme's colours, brighter under the pointer,
//! sunk while held, and naming the engine being opened while a switch is under way.
//!
//! Must not own: buffers, pixels or how a scene is rendered (the backend's), or page pixels.

use sighurt_api::geometry::WindowRect;
use sighurt_render::Pen;

use crate::chrome::{
    engine_label, label_top, Colors, EngineButton, Model, Rgba, Tone, BUTTON_ICON, CARET_WIDTH,
    CHROME, PAGE, STATUS, TABS,
};
use crate::extension::SurfaceName;
use crate::hit_test::Target;
use crate::layout::{LayoutOptions, Status, TabLayout};
use crate::state::TabSnapshot;
use crate::text::TextStyle;

fn rect_f(rect: WindowRect) -> [f32; 4] {
    [
        rect.x as f32,
        rect.y as f32,
        rect.width as f32,
        rect.height as f32,
    ]
}

fn tone(colors: &Colors, tone: Tone) -> Rgba {
    match tone {
        Tone::Text => colors.text,
        Tone::Muted => colors.text_muted,
        Tone::Dim => colors.text_dim,
        Tone::Error => colors.error,
    }
}

/// Draws all of `chrome`'s `surface` ([`CHROME`], [`TABS`], [`STATUS`] or [`PAGE`]) with `pen`.
/// A surface the chrome doesn't have gets its background. The chrome is opaque.
pub fn draw(pen: &mut Pen, chrome: &mut Model, surface: SurfaceName) {
    let colors = *chrome.colors();
    let layout = chrome.layout().clone();
    let overlay = surface == STATUS && layout.options.status == Status::Overlay;
    let background = if overlay { colors.surface } else { colors.bg };
    pen.clear(Rgba {
        a: 255,
        ..background
    });
    match surface {
        CHROME => {
            if !layout.tab_strip.size().is_empty() {
                draw_tab_strip(pen, chrome, &colors);
            }
            if !layout.toolbar.size().is_empty() {
                draw_toolbar(pen, chrome, &colors);
            }
        }
        TABS => draw_tab_list(pen, chrome, &colors),
        STATUS => draw_status(pen, chrome, &colors),
        PAGE if chrome.draws_page() => draw_page(pen, chrome, &colors),
        _ => {}
    }
}

fn draw_tab_strip(pen: &mut Pen, chrome: &mut Model, colors: &Colors) {
    let layout = chrome.layout().clone();
    let strip = rect_f(layout.tab_strip);
    pen.fill(
        [strip[0], strip[1] + strip[3] - 1.0, strip[2], 1.0],
        colors.border,
    );
    draw_tabs(pen, chrome, colors);
    let [x, y, w, h] = rect_f(layout.new_tab);
    if w > 0.0 {
        plus(pen, chrome, (x + w / 2.0, y + h / 2.0), colors);
    }
}

/// The tab list on the left: rows, then the new-tab button as the last row, and a line along
/// its right edge.
fn draw_tab_list(pen: &mut Pen, chrome: &mut Model, colors: &Colors) {
    let layout = chrome.layout().clone();
    let [x, y, w, h] = rect_f(layout.tab_list);
    pen.fill([x + w - 1.0, y, 1.0, h], colors.border);
    draw_tabs(pen, chrome, colors);
    let [x, y, _, h] = rect_f(layout.new_tab);
    if !layout.new_tab.size().is_empty() {
        plus(pen, chrome, (x + h / 2.0, y + h / 2.0), colors);
    }
}

/// Every tab with room, where the layout puts it: the strip's or the list's.
fn draw_tabs(pen: &mut Pen, chrome: &mut Model, colors: &Colors) {
    let layout = chrome.layout().clone();
    let tabs = chrome.snapshot().tabs.clone();
    for (tab, place) in tabs.iter().zip(&layout.tabs) {
        if !place.rect.size().is_empty() {
            draw_tab(pen, chrome, tab, place, colors);
        }
    }
}

fn draw_tab(
    pen: &mut Pen,
    chrome: &mut Model,
    tab: &TabSnapshot,
    place: &TabLayout,
    colors: &Colors,
) {
    let options = chrome.layout().options;
    let k = |px| options.scaled(px);
    let hover = chrome.state().hover;
    let rect = rect_f(place.rect);
    if tab.active {
        pen.fill_rounded(rect, k(6.0), colors.tab);
    }
    let mut left = rect[0] + k(12.0);
    let center_y = rect[1] + rect[3] / 2.0;
    if tab.loading {
        let color = colors.text_muted;
        pen.shape(color, |path| {
            path.push_circle(left + k(3.0), center_y, k(3.0))
        });
        left += k(12.0);
    }
    let mut right = match place.close {
        Some(close) => {
            let [x, y, w, h] = rect_f(close);
            let color = match hover == Some(Target::CloseTab(tab.id)) {
                true => colors.text,
                false => colors.text_dim,
            };
            cross(pen, options, (x + w / 2.0, y + h / 2.0), 3.5, color);
            x - k(4.0)
        }
        None => rect[0] + rect[2] - k(12.0),
    };
    // The engine's name at the right end, if it fits in half the room, whatever the title: the
    // title is cut before it.
    let style = options.text(TextStyle::SM);
    let title = tab.display_title();
    let room = (right - left).max(0.0);
    let label = engine_label(chrome.fonts(), options, tab, room / 2.0 - k(8.0));
    let label = label.map(|label| (right - label.width, label));
    if let Some((x, _)) = &label {
        right = x - k(8.0);
    }
    let text = chrome
        .fonts()
        .fitted(&title, style, (right - left).max(0.0));
    let top = center_y - f32::from(style.line_height) / 2.0;
    let color = if tab.active {
        colors.text
    } else {
        colors.text_muted
    };
    let clip = [left, rect[1], (right - left).max(0.0), rect[3]];
    pen.text(&text, (left, top), color, clip);
    if let Some((x, label)) = label {
        let at = (x, label_top(chrome.fonts(), options, style, top));
        let clip = [x, rect[1], label.width, rect[3]];
        pen.text(&label, at, colors.text_muted, clip);
    }
}

/// The new-tab button's +, centred at `(cx, cy)`.
fn plus(pen: &mut Pen, chrome: &Model, (cx, cy): (f32, f32), colors: &Colors) {
    let options = chrome.layout().options;
    let color = match chrome.state().hover == Some(Target::NewTab) {
        true => colors.text,
        false => colors.text_muted,
    };
    let arm = options.scaled(5.0);
    pen.stroke(options.scaled(1.5), color, |path| {
        path.move_to(cx - arm, cy);
        path.line_to(cx + arm, cy);
        path.move_to(cx, cy - arm);
        path.line_to(cx, cy + arm);
    });
}

/// An × of two strokes, `size` logical pixels (at the default font size) from the centre to
/// each end.
fn cross(pen: &mut Pen, options: LayoutOptions, (cx, cy): (f32, f32), size: f32, color: Rgba) {
    let size = options.scaled(size);
    pen.stroke(options.scaled(1.5), color, |path| {
        path.move_to(cx - size, cy - size);
        path.line_to(cx + size, cy + size);
        path.move_to(cx + size, cy - size);
        path.line_to(cx - size, cy + size);
    });
}

fn draw_toolbar(pen: &mut Pen, chrome: &mut Model, colors: &Colors) {
    let layout = chrome.layout().clone();
    let options = layout.options;
    let k = |px| options.scaled(px);
    let bar = rect_f(layout.toolbar);
    pen.fill([bar[0], bar[1] + bar[3] - 1.0, bar[2], 1.0], colors.border);
    let snapshot = chrome.snapshot();
    let loading = snapshot.active().is_some_and(|tab| tab.loading);
    let buttons = [
        (layout.back, Target::Back, snapshot.can_go_back),
        (layout.forward, Target::Forward, snapshot.can_go_forward),
        (layout.reload, Target::ReloadOrStop, true),
    ];
    let hover = chrome.state().hover;
    for (place, target, enabled) in buttons {
        if place.size().is_empty() {
            continue;
        }
        let [x, y, w, h] = rect_f(place);
        if enabled && hover == Some(target) {
            pen.fill_rounded([x, y, w, h], k(6.0), colors.hover);
        }
        let color = if enabled {
            colors.text_muted
        } else {
            colors.text_dim
        };
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        match target {
            // Filled triangles, as the old toolbar's ◀ and ▶.
            Target::Back => pen.shape(color, |path| {
                path.move_to(cx + k(3.5), cy - k(5.0));
                path.line_to(cx + k(3.5), cy + k(5.0));
                path.line_to(cx - k(4.5), cy);
                path.close();
            }),
            Target::Forward => pen.shape(color, |path| {
                path.move_to(cx - k(3.5), cy - k(5.0));
                path.line_to(cx - k(3.5), cy + k(5.0));
                path.line_to(cx + k(4.5), cy);
                path.close();
            }),
            _ if loading => cross(pen, options, (cx, cy), 4.0, color),
            _ => reload_icon(pen, options, (cx, cy), color),
        }
    }
    draw_address(pen, chrome, colors);
}

/// ↻: three quarters of a circle, clockwise from the right to the top, with an arrowhead.
fn reload_icon(pen: &mut Pen, options: LayoutOptions, (cx, cy): (f32, f32), color: Rgba) {
    let k = |px| options.scaled(px);
    let radius = k(5.5);
    // Control-point distance for a quarter circle.
    let c = radius * 0.552_284_8;
    pen.stroke(k(1.5), color, |path| {
        path.move_to(cx + radius, cy);
        path.cubic_to(cx + radius, cy + c, cx + c, cy + radius, cx, cy + radius);
        path.cubic_to(cx - c, cy + radius, cx - radius, cy + c, cx - radius, cy);
        path.cubic_to(cx - radius, cy - c, cx - c, cy - radius, cx, cy - radius);
    });
    pen.shape(color, |path| {
        path.move_to(cx + k(3.5), cy - radius);
        path.line_to(cx - k(0.5), cy - radius - k(3.5));
        path.line_to(cx - k(0.5), cy - radius + k(3.5));
        path.close();
    });
}

fn draw_address(pen: &mut Pen, chrome: &mut Model, colors: &Colors) {
    let field = chrome.layout().address;
    if field.size().is_empty() {
        return;
    }
    let options = chrome.layout().options;
    let state = chrome.state();
    let focused = state.focused;
    let active = focused && state.window_active;
    let border = match (&state.command_error, focused) {
        (Some(_), _) => colors.error,
        (None, true) => colors.focus,
        (None, false) => colors.border,
    };
    pen.bordered(rect_f(field), options.scaled(6.0), colors.surface, border);

    let view = chrome.address_view();
    let clip = [
        view.clip.0,
        field.y as f32,
        view.clip.1 - view.clip.0,
        field.height as f32,
    ];
    let line = f32::from(options.text(TextStyle::SM).line_height);
    let clipped = |(a, b): (f32, f32)| {
        let (a, b) = (a.max(view.clip.0), b.min(view.clip.1));
        (a < b).then_some([a, view.y, b - a, line])
    };
    if active {
        if let Some(selection) = view.selection.and_then(clipped) {
            pen.fill(selection, colors.selection);
        }
    }
    let at = (view.x, view.y);
    pen.text(&view.shaped, at, colors.text, clip);
    if let Some((indicator, x)) = &view.indicator {
        let field = rect_f(field);
        pen.text(indicator, (*x, view.y), colors.text_muted, field);
    }
    if let Some(button) = &view.engine {
        draw_engine_button(pen, chrome, button, colors);
    }
    if let Some(preedit) = view.preedit.and_then(clipped) {
        let underline = view.y + line - options.scaled(2.0);
        pen.fill([preedit[0], underline, preedit[2], 1.0], colors.text);
    }
    let caret_shown = active && view.selection.is_none();
    if let Some(caret) = view.caret.filter(|_| caret_shown) {
        if caret >= view.clip.0 - 1.0 && caret <= view.clip.1 + 1.0 {
            pen.fill([caret, view.y, CARET_WIDTH, line], colors.text);
        }
    }
}

/// The engine button: a rounded outline that fills when the pointer is over it, sinks while
/// held, and shows an accent fill and the engine being switched to while a switch is under way.
fn draw_engine_button(pen: &mut Pen, chrome: &mut Model, button: &EngineButton, colors: &Colors) {
    let options = chrome.layout().options;
    let k = |px| options.scaled(px);
    let state = chrome.state();
    let over = state.label_hover && !button.busy;
    let held = over && state.label_pressed;
    let (fill, border, text) = match (button.busy, held, over) {
        (true, _, _) => (colors.selection, colors.border, colors.text),
        (_, true, _) => (colors.bg, colors.focus, colors.text),
        (_, _, true) => (colors.hover, colors.text_dim, colors.text),
        _ => (colors.surface, colors.border, colors.text_muted),
    };
    pen.bordered(button.rect, k(5.0), fill, border);
    let [x, y, w, h] = button.rect;
    let clip = [x, y, w, h];
    let (name, name_x) = &button.name;
    pen.text(name, (*name_x, button.text_y), text, clip);
    let cy = y + h / 2.0;
    match &button.target {
        None => swap_icon(pen, options, (button.icon_x, cy), text),
        Some((target, target_x)) => {
            arrow(pen, options, (button.icon_x, cy), text);
            pen.text(target, (*target_x, button.text_y), text, clip);
        }
    }
}

/// ⇄ in a box [`BUTTON_ICON`] wide starting at `x`: a right arrow above a left one.
fn swap_icon(pen: &mut Pen, options: LayoutOptions, (x, cy): (f32, f32), color: Rgba) {
    let k = |px| options.scaled(px);
    let (w, dy, head) = (k(BUTTON_ICON), k(2.5), k(2.2));
    pen.stroke(k(1.2), color, |path| {
        for (row, right) in [(cy - dy, true), (cy + dy, false)] {
            let (from, to, back) = match right {
                true => (x, x + w, -head),
                false => (x + w, x, head),
            };
            path.move_to(from, row);
            path.line_to(to, row);
            path.move_to(to + back, row - head);
            path.line_to(to, row);
            path.line_to(to + back, row + head);
        }
    });
}

/// → in a box [`BUTTON_ICON`] wide starting at `x`.
fn arrow(pen: &mut Pen, options: LayoutOptions, (x, cy): (f32, f32), color: Rgba) {
    let k = |px| options.scaled(px);
    let (w, head) = (k(BUTTON_ICON), k(2.6));
    pen.stroke(k(1.2), color, |path| {
        path.move_to(x, cy);
        path.line_to(x + w, cy);
        path.move_to(x + w - head, cy - head);
        path.line_to(x + w, cy);
        path.line_to(x + w - head, cy + head);
    });
}

/// The status line: a strip with a line along its top, or an overlay over the page's corner
/// with a line along its top and right edges.
fn draw_status(pen: &mut Pen, chrome: &mut Model, colors: &Colors) {
    let layout = chrome.layout().clone();
    let k = |px| layout.options.scaled(px);
    let [x, y, w, h] = rect_f(layout.status);
    pen.fill([x, y, w, 1.0], colors.border);
    if layout.options.status == Status::Overlay {
        pen.fill([x + w - 1.0, y, 1.0, h], colors.border);
    }
    let Some((line, shade)) = chrome.status_line() else {
        return;
    };
    let style = layout.options.text(TextStyle::XS);
    let width = (w - k(16.0)).max(0.0);
    let text = chrome.fonts().fitted(&line, style, width);
    let top = y + (h - f32::from(style.line_height)) / 2.0;
    let clip = [x + k(8.0), y, width, h];
    pen.text(&text, (x + k(8.0), top), tone(colors, shade), clip);
}

fn draw_page(pen: &mut Pen, chrome: &mut Model, colors: &Colors) {
    let Some(page) = chrome.page_layout() else {
        return;
    };
    let options = chrome.layout().options;
    pen.bordered(
        page.card,
        options.scaled(8.0),
        colors.surface,
        colors.border,
    );
    let clip = rect_f(chrome.layout().page);
    for text in &page.texts {
        let color = tone(colors, text.tone);
        pen.text(&text.shaped, (text.x, text.y), color, clip);
    }
}
