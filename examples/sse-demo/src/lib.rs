//! Server-Sent Events demo.
//!
//! Opens an EventSource-style stream with [`sse_open`], polls [`sse_state`],
//! and drains [`sse_recv`] each frame. The default URL is Wikimedia's public
//! recent-change firehose.
//!
//! # Build
//!
//! ```bash
//! cargo build --target wasm32-unknown-unknown --release -p sse-demo
//! ```

use sighurt_sdk::*;

const DEFAULT_URL: &str = "https://stream.wikimedia.org/v2/stream/recentchange";
const MAX_LINES: usize = 12;

const BG: (u8, u8, u8) = (24, 26, 34);
const ACCENT: (u8, u8, u8) = (40, 140, 180);
const DIM: (u8, u8, u8) = (140, 140, 165);
const BRIGHT: (u8, u8, u8) = (235, 235, 245);
const GREEN: (u8, u8, u8) = (90, 220, 130);
const ORANGE: (u8, u8, u8) = (240, 180, 60);
const RED: (u8, u8, u8) = (225, 90, 90);

struct Demo {
    handle: u32,
    lines: Vec<String>,
    event_count: u32,
}

static mut DEMO: Option<Demo> = None;

#[no_mangle]
pub extern "C" fn start_app() {
    log("SSE Demo loaded");
    unsafe {
        DEMO = Some(Demo {
            handle: 0,
            lines: Vec::new(),
            event_count: 0,
        });
    }
}

fn push_line(demo: &mut Demo, line: String) {
    demo.lines.push(line);
    if demo.lines.len() > MAX_LINES {
        let extra = demo.lines.len() - MAX_LINES;
        demo.lines.drain(..extra);
    }
}

fn state_label(state: u32) -> (&'static str, (u8, u8, u8)) {
    match state {
        SSE_CONNECTING => ("connecting", ORANGE),
        SSE_OPEN => ("open", GREEN),
        SSE_CLOSED => ("closed", DIM),
        SSE_ERROR => ("error", RED),
        _ => ("unknown", DIM),
    }
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    let (width, _) = canvas_dimensions();
    let w = width as f32;

    canvas_clear(BG.0, BG.1, BG.2, 255);
    canvas_rect(0.0, 0.0, w, 52.0, ACCENT.0, ACCENT.1, ACCENT.2, 255);
    text(20.0, 14.0, 22.0, (255, 255, 255), "Server-Sent Events");
    text(
        20.0,
        36.0,
        11.0,
        (210, 230, 240),
        "EventSource streams with automatic reconnect",
    );

    let demo = unsafe {
        match (*core::ptr::addr_of_mut!(DEMO)).as_mut() {
            Some(d) => d,
            None => return,
        }
    };

    if demo.handle != 0 {
        while let Some(ev) = sse_recv(demo.handle) {
            demo.event_count += 1;
            let preview: String = ev.data.chars().take(90).collect();
            push_line(demo, format!("{} [{}] {}", ev.name, ev.id, preview));
        }
    }

    let state = if demo.handle == 0 {
        SSE_CLOSED
    } else {
        sse_state(demo.handle)
    };
    let (label, color) = state_label(state);

    text(20.0, 70.0, 12.0, DIM, "url");
    text(70.0, 70.0, 12.0, BRIGHT, DEFAULT_URL);
    text(20.0, 92.0, 12.0, DIM, "state");
    text(70.0, 92.0, 12.0, color, label);
    text(
        160.0,
        92.0,
        12.0,
        DIM,
        &format!("events: {}", demo.event_count),
    );

    if state == SSE_ERROR {
        let err = sse_error(demo.handle);
        if !err.is_empty() {
            text(20.0, 114.0, 12.0, RED, &err);
        }
    }

    if button(20.0, 136.0, 110.0, 28.0, "Connect") {
        if demo.handle != 0 {
            sse_close(demo.handle);
            sse_remove(demo.handle);
        }
        demo.handle = sse_open(DEFAULT_URL);
        demo.event_count = 0;
        demo.lines.clear();
        if demo.handle == 0 {
            push_line(demo, "sse_open failed".into());
        }
    }
    if button(140.0, 136.0, 110.0, 28.0, "Close") && demo.handle != 0 {
        sse_close(demo.handle);
        sse_remove(demo.handle);
        demo.handle = 0;
        push_line(demo, "closed".into());
    }

    canvas_line(20.0, 180.0, w - 20.0, 180.0, 45, 45, 60, 255, 1.0);
    text(20.0, 192.0, 14.0, DIM, "EVENTS");

    let mut y = 218.0;
    for line in &demo.lines {
        text(20.0, y, 12.0, BRIGHT, line);
        y += 18.0;
    }
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
