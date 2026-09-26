use sighurt_sdk::*;

const BG: (u8, u8, u8) = (25, 25, 40);
const ACCENT: (u8, u8, u8) = (80, 160, 220);
const DIM: (u8, u8, u8) = (140, 130, 160);
const BRIGHT: (u8, u8, u8) = (230, 220, 255);
const GREEN: (u8, u8, u8) = (80, 220, 120);
const ORANGE: (u8, u8, u8) = (240, 180, 60);
const RED: (u8, u8, u8) = (220, 80, 80);
const CYAN: (u8, u8, u8) = (80, 220, 220);

// Callback IDs for on_timer
const CB_COUNTDOWN: u32 = 1;
const CB_DELAYED_MSG: u32 = 2;
const CB_BLINK: u32 = 3;
const CB_STOPWATCH: u32 = 4;

static mut COUNTDOWN: i32 = 0;
static mut COUNTDOWN_TIMER: u32 = 0;
static mut DELAYED_MSG: &str = "";
static mut BLINK_ON: bool = false;
static mut BLINK_TIMER: u32 = 0;
static mut STOPWATCH_MS: u64 = 0;
static mut STOPWATCH_TIMER: u32 = 0;

#[no_mangle]
pub extern "C" fn start_app() {
    log("Timer Demo loaded!");
}

#[no_mangle]
pub extern "C" fn on_timer(callback_id: u32) {
    match callback_id {
        CB_COUNTDOWN => unsafe {
            COUNTDOWN -= 1;
            if COUNTDOWN <= 0 {
                COUNTDOWN = 0;
                clear_timer(COUNTDOWN_TIMER);
                COUNTDOWN_TIMER = 0;
            }
        },
        CB_DELAYED_MSG => unsafe {
            DELAYED_MSG = "Timer fired! This appeared after 3 seconds.";
        },
        CB_BLINK => unsafe {
            BLINK_ON = !BLINK_ON;
        },
        CB_STOPWATCH => unsafe {
            STOPWATCH_MS += 100;
        },
        _ => {}
    }
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    let (width, _height) = canvas_dimensions();
    let w = width as f32;

    canvas_clear(BG.0, BG.1, BG.2, 255);

    // ── Header ──────────────────────────────────────────────────────
    canvas_rect(0.0, 0.0, w, 52.0, ACCENT.0, ACCENT.1, ACCENT.2, 255);
    text(20.0, 14.0, 22.0, (255, 255, 255), "Sighurt Timer Demo");
    text(
        20.0,
        36.0,
        11.0,
        (200, 220, 255),
        "set_timeout / set_interval / clear_timer",
    );

    // ── Countdown (set_interval) ────────────────────────────────────
    text(20.0, 72.0, 14.0, DIM, "COUNTDOWN (set_interval)");

    let countdown = unsafe { COUNTDOWN };
    let running = unsafe { COUNTDOWN_TIMER } != 0;

    if button(20.0, 95.0, 140.0, 30.0, "Start from 10") && !running {
        unsafe {
            COUNTDOWN = 10;
            COUNTDOWN_TIMER = set_interval(CB_COUNTDOWN, 1000);
        }
    }

    let (count_text, count_color) = if countdown > 3 {
        (format!("{countdown}"), GREEN)
    } else if countdown > 0 {
        (format!("{countdown}"), ORANGE)
    } else if running {
        ("0".into(), RED)
    } else {
        ("--".into(), DIM)
    };

    text(200.0, 100.0, 28.0, count_color, &count_text);

    if countdown == 0 && !running {
        text(260.0, 105.0, 14.0, DIM, "Done!");
    }

    // ── Delayed Message (set_timeout) ───────────────────────────────
    canvas_line(20.0, 145.0, w - 20.0, 145.0, 40, 35, 60, 255, 1.0);
    text(20.0, 160.0, 14.0, DIM, "DELAYED MESSAGE (set_timeout)");

    if button(20.0, 183.0, 180.0, 30.0, "Fire after 3 sec") {
        unsafe { DELAYED_MSG = "Waiting..." };
        set_timeout(CB_DELAYED_MSG, 3000);
    }

    let msg = unsafe { DELAYED_MSG };
    if !msg.is_empty() {
        let color = if msg.starts_with("Waiting") {
            ORANGE
        } else {
            GREEN
        };
        text(220.0, 190.0, 14.0, color, msg);
    }

    // ── Blink (set_interval + clear_timer) ──────────────────────────
    canvas_line(20.0, 230.0, w - 20.0, 230.0, 40, 35, 60, 255, 1.0);
    text(20.0, 245.0, 14.0, DIM, "BLINK (interval + clear)");

    let blinking = unsafe { BLINK_TIMER } != 0;
    let label = if blinking {
        "Stop Blink"
    } else {
        "Start Blink"
    };
    if button(20.0, 268.0, 120.0, 30.0, label) {
        unsafe {
            if blinking {
                clear_timer(BLINK_TIMER);
                BLINK_TIMER = 0;
                BLINK_ON = false;
            } else {
                BLINK_ON = true;
                BLINK_TIMER = set_interval(CB_BLINK, 500);
            }
        }
    }

    let blink_on = unsafe { BLINK_ON };
    if blink_on {
        canvas_circle(200.0, 283.0, 14.0, CYAN.0, CYAN.1, CYAN.2, 255);
    } else {
        canvas_circle(200.0, 283.0, 14.0, 50, 50, 60, 255);
    }

    text(
        230.0,
        276.0,
        13.0,
        DIM,
        if blinking {
            "Toggling every 500ms"
        } else {
            "Idle"
        },
    );

    // ── Stopwatch (set_interval + clear_timer) ──────────────────────
    canvas_line(20.0, 315.0, w - 20.0, 315.0, 40, 35, 60, 255, 1.0);
    text(20.0, 330.0, 14.0, DIM, "STOPWATCH (100ms interval)");

    let sw_running = unsafe { STOPWATCH_TIMER } != 0;
    let sw_ms = unsafe { STOPWATCH_MS };

    if button(20.0, 353.0, 80.0, 30.0, "Start") && !sw_running {
        unsafe { STOPWATCH_TIMER = set_interval(CB_STOPWATCH, 100) };
    }
    if button(110.0, 353.0, 80.0, 30.0, "Stop") && sw_running {
        unsafe {
            clear_timer(STOPWATCH_TIMER);
            STOPWATCH_TIMER = 0;
        }
    }
    if button(200.0, 353.0, 80.0, 30.0, "Reset") && !sw_running {
        unsafe { STOPWATCH_MS = 0 };
    }

    let secs = sw_ms / 1000;
    let tenths = (sw_ms % 1000) / 100;
    text(310.0, 355.0, 28.0, BRIGHT, &format!("{secs}.{tenths}s"));

    // ── Info ─────────────────────────────────────────────────────────
    canvas_line(20.0, 405.0, w - 20.0, 405.0, 40, 35, 60, 255, 1.0);
    text(
        20.0,
        420.0,
        12.0,
        DIM,
        "Timers fire via exported on_timer(callback_id). Intervals repeat until cleared.",
    );
    text(
        20.0,
        440.0,
        12.0,
        DIM,
        "Resolution is tied to the frame rate (~16ms at 60fps).",
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
