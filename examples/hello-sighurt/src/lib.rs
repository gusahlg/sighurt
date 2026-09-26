//! Hello Sighurt: the guest API on one screen. Canvas drawing, mouse and
//! keyboard polling, and a text field the app draws itself, fed by
//! [`text_input`] plus key codes.

use sighurt_sdk::*;

const ACCENT: (u8, u8, u8) = (180, 140, 255);
const TEXT: (u8, u8, u8) = (225, 225, 240);
const DIM: (u8, u8, u8) = (120, 120, 140);
const GREEN: (u8, u8, u8) = (160, 220, 160);

struct State {
    clicks: u32,
    /// Offset of the arrow-key square inside its box.
    square: (f32, f32),
    name: String,
    focused: bool,
    greeting: String,
}

static mut STATE: State = State {
    clicks: 0,
    square: (40.0, 40.0),
    name: String::new(),
    focused: true,
    greeting: String::new(),
};

#[no_mangle]
pub extern "C" fn start_app() {
    log("Hello Sighurt loaded");
}

#[no_mangle]
pub extern "C" fn on_frame(dt_ms: u32) {
    let s = unsafe { &mut *core::ptr::addr_of_mut!(STATE) };
    let (width, _) = canvas_dimensions();
    let (mx, my) = mouse_position();

    canvas_clear(30, 30, 46, 255);
    canvas_rect(0.0, 0.0, width as f32, 56.0, 50, 40, 80, 255);
    text(20.0, 16.0, 24.0, ACCENT, "Hello Sighurt");

    // ── Drawing ─────────────────────────────────────────────────────
    text(20.0, 72.0, 16.0, ACCENT, "Drawing");
    canvas_rect(20.0, 100.0, 60.0, 40.0, 240, 110, 110, 255);
    canvas_rounded_rect(95.0, 100.0, 60.0, 40.0, 10.0, 110, 200, 240, 255);
    canvas_circle(195.0, 120.0, 20.0, 120, 220, 140, 255);
    canvas_arc(255.0, 120.0, 18.0, 0.0, 4.7, 240, 200, 90, 255, 4.0);
    canvas_line(295.0, 100.0, 355.0, 140.0, 200, 200, 220, 255, 2.0);
    canvas_bezier(
        370.0, 140.0, 390.0, 90.0, 420.0, 150.0, 440.0, 100.0, ACCENT.0, ACCENT.1, ACCENT.2, 255,
        3.0,
    );

    // ── Mouse ───────────────────────────────────────────────────────
    text(20.0, 160.0, 16.0, ACCENT, "Mouse: click anywhere");
    if mouse_button_clicked(0) {
        s.clicks += 1;
    }
    let button = if mouse_button_down(0) { "down" } else { "up" };
    text(
        20.0,
        184.0,
        14.0,
        TEXT,
        &format!(
            "position ({mx:.0}, {my:.0})   clicks {}   left button {button}",
            s.clicks
        ),
    );

    // ── Keyboard ────────────────────────────────────────────────────
    text(
        20.0,
        216.0,
        16.0,
        ACCENT,
        "Keyboard: arrow keys move the square, Shift is faster",
    );
    let (bx, by, bw, bh) = (20.0, 240.0, 300.0, 100.0);
    let step = if shift_held() { 0.4 } else { 0.15 } * dt_ms as f32;
    if key_down(KEY_LEFT) {
        s.square.0 -= step;
    }
    if key_down(KEY_RIGHT) {
        s.square.0 += step;
    }
    if key_down(KEY_UP) {
        s.square.1 -= step;
    }
    if key_down(KEY_DOWN) {
        s.square.1 += step;
    }
    s.square.0 = s.square.0.clamp(0.0, bw - 20.0);
    s.square.1 = s.square.1.clamp(0.0, bh - 20.0);
    canvas_rounded_rect(bx, by, bw, bh, 6.0, 40, 40, 60, 255);
    canvas_rect(
        bx + s.square.0,
        by + s.square.1,
        20.0,
        20.0,
        240,
        200,
        90,
        255,
    );

    // ── Text field ──────────────────────────────────────────────────
    text(
        20.0,
        360.0,
        16.0,
        ACCENT,
        "Text field: type, Backspace deletes, Enter submits",
    );
    let (fx, fy, fw, fh) = (20.0, 384.0, 300.0, 32.0);
    if mouse_button_clicked(0) {
        s.focused = mx >= fx && mx < fx + fw && my >= fy && my < fy + fh;
    }
    if s.focused {
        s.name
            .extend(text_input().chars().filter(|c| !c.is_control()));
        if key_pressed(KEY_BACKSPACE) {
            s.name.pop();
        }
        if key_pressed(KEY_ENTER) && !s.name.is_empty() {
            s.greeting = format!("Hello, {}!", s.name);
            s.name.clear();
        }
    }
    let border = if s.focused { ACCENT } else { DIM };
    canvas_rounded_rect(fx, fy, fw, fh, 6.0, border.0, border.1, border.2, 255);
    canvas_rounded_rect(fx + 1.5, fy + 1.5, fw - 3.0, fh - 3.0, 5.0, 22, 22, 34, 255);
    if s.name.is_empty() && !s.focused {
        text(
            fx + 10.0,
            fy + 8.0,
            14.0,
            DIM,
            "Click here and type your name",
        );
    } else {
        text(fx + 10.0, fy + 8.0, 14.0, TEXT, &s.name);
    }
    if s.focused && (time_now_ms() / 500).is_multiple_of(2) {
        let text_w = canvas_measure_text(14.0, "", 0, FONT_STYLE_NORMAL, &s.name).width;
        let caret = fx + 10.0 + text_w;
        canvas_line(
            caret,
            fy + 7.0,
            caret,
            fy + fh - 7.0,
            TEXT.0,
            TEXT.1,
            TEXT.2,
            255,
            1.5,
        );
    }
    if !s.greeting.is_empty() {
        text(20.0, 428.0, 18.0, GREEN, &s.greeting);
    }

    // Cursor follower, drawn last so it stays on top.
    canvas_circle(mx, my, 5.0, ACCENT.0, ACCENT.1, ACCENT.2, 180);
}

fn text(x: f32, y: f32, size: f32, (r, g, b): (u8, u8, u8), s: &str) {
    canvas_text(x, y, size, r, g, b, 255, s);
}
