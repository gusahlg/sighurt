//! Demonstrates the Sighurt event system: built-in events (`resize`, `focus`,
//! `blur`, `visibility_change`, `online`/`offline`, `touch_*`, `gamepad_*`,
//! `drop_files`) plus custom events emitted from a button click.

#![allow(static_mut_refs)]

use sighurt_sdk::*;

const BG: (u8, u8, u8) = (24, 24, 36);
const ACCENT: (u8, u8, u8) = (110, 180, 220);
const DIM: (u8, u8, u8) = (140, 130, 160);
const BRIGHT: (u8, u8, u8) = (235, 230, 250);
const GREEN: (u8, u8, u8) = (110, 220, 140);
const ORANGE: (u8, u8, u8) = (240, 180, 70);
const PINK: (u8, u8, u8) = (240, 140, 200);

const CB_RESIZE: u32 = 1;
const CB_FOCUS: u32 = 2;
const CB_BLUR: u32 = 3;
const CB_VISIBILITY: u32 = 4;
const CB_ONLINE: u32 = 5;
const CB_OFFLINE: u32 = 6;
const CB_TOUCH_START: u32 = 7;
const CB_TOUCH_MOVE: u32 = 8;
const CB_TOUCH_END: u32 = 9;
const CB_GAMEPAD_BTN: u32 = 10;
const CB_GAMEPAD_AXIS: u32 = 11;
const CB_GAMEPAD_CONNECTED: u32 = 12;
const CB_DROP_FILES: u32 = 13;
const CB_PING: u32 = 100;

static mut LAST_EVENT: String = String::new();
static mut LAST_RESIZE: (u32, u32) = (0, 0);
static mut TOUCH_POS: (f32, f32) = (0.0, 0.0);
static mut TOUCHING: bool = false;
static mut FOCUS_STATE: &str = "unknown";
static mut ONLINE_STATE: &str = "unknown";
static mut LAST_DROP: String = String::new();
static mut PING_COUNT: u32 = 0;
static mut GAMEPAD_LINE: String = String::new();

fn read_u32_le(b: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(b[offset..offset + 4].try_into().unwrap())
}

fn read_f32_le(b: &[u8], offset: usize) -> f32 {
    f32::from_bits(read_u32_le(b, offset))
}

#[no_mangle]
pub extern "C" fn start_app() {
    log("events-demo: registering listeners");
    // Fully qualify so we don't shadow the `on_event` export below.
    sighurt_sdk::on_event("resize", CB_RESIZE);
    sighurt_sdk::on_event("focus", CB_FOCUS);
    sighurt_sdk::on_event("blur", CB_BLUR);
    sighurt_sdk::on_event("visibility_change", CB_VISIBILITY);
    sighurt_sdk::on_event("online", CB_ONLINE);
    sighurt_sdk::on_event("offline", CB_OFFLINE);
    sighurt_sdk::on_event("touch_start", CB_TOUCH_START);
    sighurt_sdk::on_event("touch_move", CB_TOUCH_MOVE);
    sighurt_sdk::on_event("touch_end", CB_TOUCH_END);
    sighurt_sdk::on_event("gamepad_connected", CB_GAMEPAD_CONNECTED);
    sighurt_sdk::on_event("gamepad_button", CB_GAMEPAD_BTN);
    sighurt_sdk::on_event("gamepad_axis", CB_GAMEPAD_AXIS);
    sighurt_sdk::on_event("drop_files", CB_DROP_FILES);
    sighurt_sdk::on_event("ping", CB_PING);
}

#[no_mangle]
pub extern "C" fn on_event(callback_id: u32) {
    let etype = event_type();
    let data = event_data_into();
    unsafe {
        LAST_EVENT = format!("{etype} ({} bytes)", data.len());
    }
    match callback_id {
        CB_RESIZE if data.len() == 8 => {
            let w = read_u32_le(&data, 0);
            let h = read_u32_le(&data, 4);
            unsafe {
                LAST_RESIZE = (w, h);
            }
        }
        CB_FOCUS => unsafe {
            FOCUS_STATE = "focused";
        },
        CB_BLUR => unsafe {
            FOCUS_STATE = "blurred";
        },
        CB_VISIBILITY => {
            let s = String::from_utf8_lossy(&data).into_owned();
            log(&format!("visibility_change: {s}"));
        }
        CB_ONLINE => unsafe {
            ONLINE_STATE = "online";
        },
        CB_OFFLINE => unsafe {
            ONLINE_STATE = "offline";
        },
        CB_TOUCH_START | CB_TOUCH_MOVE if data.len() == 8 => unsafe {
            TOUCH_POS = (read_f32_le(&data, 0), read_f32_le(&data, 4));
            if callback_id == CB_TOUCH_START {
                TOUCHING = true;
            }
        },
        CB_TOUCH_END => unsafe {
            TOUCHING = false;
        },
        CB_GAMEPAD_CONNECTED => {
            let name = String::from_utf8_lossy(&data).into_owned();
            unsafe {
                GAMEPAD_LINE = format!("connected: {name}");
            }
        }
        CB_GAMEPAD_BTN if data.len() == 12 => {
            let id = read_u32_le(&data, 0);
            let code = read_u32_le(&data, 4);
            let pressed = read_u32_le(&data, 8) != 0;
            unsafe {
                GAMEPAD_LINE = format!(
                    "gamepad #{id} button code={code} {}",
                    if pressed { "DOWN" } else { "up" }
                );
            }
        }
        CB_GAMEPAD_AXIS if data.len() == 12 => {
            let id = read_u32_le(&data, 0);
            let code = read_u32_le(&data, 4);
            let v = read_f32_le(&data, 8);
            unsafe {
                GAMEPAD_LINE = format!("gamepad #{id} axis code={code} value={v:+.2}");
            }
        }
        CB_DROP_FILES => {
            let s = String::from_utf8_lossy(&data).into_owned();
            unsafe {
                LAST_DROP = s.clone();
            }
            log(&format!("drop_files: {s}"));
        }
        CB_PING => unsafe {
            PING_COUNT += 1;
        },
        _ => {}
    }
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    let (w, _h) = canvas_dimensions();
    let w = w as f32;

    canvas_clear(BG.0, BG.1, BG.2, 255);

    canvas_rect(0.0, 0.0, w, 52.0, ACCENT.0, ACCENT.1, ACCENT.2, 255);
    text(20.0, 14.0, 22.0, (255, 255, 255), "Sighurt Event System");
    text(
        20.0,
        36.0,
        11.0,
        (20, 20, 40),
        "on_event / emit_event / built-in events",
    );

    let mut y = 72.0;
    let mut row = |label: &str, value: &str, color: (u8, u8, u8)| {
        text(20.0, y, 14.0, DIM, label);
        text(220.0, y, 14.0, color, value);
        y += 28.0;
    };

    let (last, gamepad, drop) = unsafe { (&LAST_EVENT, &GAMEPAD_LINE, &LAST_DROP) };
    row("Last event delivered:", or(last, "(none yet)"), BRIGHT);

    let (rw, rh) = unsafe { LAST_RESIZE };
    let resize = if rw == 0 {
        "(no resize yet — try resizing the window)".to_string()
    } else {
        format!("{rw} x {rh}")
    };
    row("Canvas resize:", &resize, BRIGHT);

    let focus = unsafe { FOCUS_STATE };
    row(
        "Focus state:",
        focus,
        if focus == "focused" { GREEN } else { ORANGE },
    );

    let online = unsafe { ONLINE_STATE };
    row(
        "Network:",
        online,
        if online == "online" { GREEN } else { ORANGE },
    );

    let (tx, ty) = unsafe { TOUCH_POS };
    if unsafe { TOUCHING } {
        row(
            "Touch (mouse):",
            &format!("DOWN at ({tx:.0}, {ty:.0})"),
            PINK,
        );
        canvas_circle(tx, ty, 18.0, PINK.0, PINK.1, PINK.2, 200);
    } else {
        row("Touch (mouse):", "(release)", DIM);
    }

    row("Gamepad:", or(gamepad, "(no gamepad input yet)"), BRIGHT);
    row(
        "Last drop:",
        or(drop, "(drag a file onto this window)"),
        BRIGHT,
    );
    y += 4.0;

    canvas_line(20.0, y, w - 20.0, y, 50, 45, 70, 255, 1.0);
    y += 16.0;

    text(20.0, y, 14.0, DIM, "Custom event (emit_event / on_event):");
    y += 22.0;

    if button(20.0, y, 140.0, 30.0, "Emit \"ping\"") {
        emit_event("ping", b"hello");
    }
    if button(170.0, y, 140.0, 30.0, "Reset count") {
        unsafe { PING_COUNT = 0 };
    }
    let count = unsafe { PING_COUNT };
    text(
        330.0,
        y + 8.0,
        16.0,
        BRIGHT,
        &format!("ping count: {count}"),
    );
}

/// A hand-drawn button: returns `true` when it was clicked this frame.
fn button(x: f32, y: f32, w: f32, h: f32, label: &str) -> bool {
    let (mx, my) = mouse_position();
    let hover = mx >= x && mx < x + w && my >= y && my < y + h;
    let shade = if hover { 80 } else { 60 };
    canvas_rounded_rect(x, y, w, h, 6.0, shade, shade, shade + 30, 255);
    // The 14 px label's line box is 1.2 × its size tall, starting at `y`.
    canvas_text_ex(
        x + w / 2.0,
        y + (h - 14.0 * 1.2) / 2.0,
        14.0,
        235,
        235,
        245,
        255,
        "",
        0,
        FONT_STYLE_NORMAL,
        TEXT_ALIGN_CENTER,
        label,
    );
    hover && mouse_button_clicked(0)
}

fn text(x: f32, y: f32, size: f32, (r, g, b): (u8, u8, u8), s: &str) {
    canvas_text(x, y, size, r, g, b, 255, s);
}

/// `s`, or `placeholder` while `s` is empty.
fn or<'a>(s: &'a str, placeholder: &'a str) -> &'a str {
    if s.is_empty() {
        placeholder
    } else {
        s
    }
}
