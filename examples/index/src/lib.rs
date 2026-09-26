use sighurt_sdk::*;

const BG: (u8, u8, u8) = (18, 18, 30);
const HEADER_BG: (u8, u8, u8) = (28, 28, 48);
const ACCENT: (u8, u8, u8) = (120, 90, 255);
const ACCENT_GLOW: (u8, u8, u8) = (160, 130, 255);
const TEXT_BRIGHT: (u8, u8, u8) = (240, 235, 255);
const TEXT_DIM: (u8, u8, u8) = (130, 125, 150);
const CARD_BG: (u8, u8, u8) = (32, 32, 52);
const CARD_HOVER: (u8, u8, u8) = (42, 38, 68);
const GREEN: (u8, u8, u8) = (80, 220, 140);
const BLUE: (u8, u8, u8) = (80, 160, 240);
const ORANGE: (u8, u8, u8) = (240, 170, 60);
const PURPLE: (u8, u8, u8) = (160, 90, 220);
const PINK: (u8, u8, u8) = (240, 140, 200);
const CYAN: (u8, u8, u8) = (80, 220, 220);
const YELLOW: (u8, u8, u8) = (240, 220, 80);
const DIVIDER: (u8, u8, u8) = (50, 45, 70);

/// Each card links to a sibling `.wasm` by relative URL. The engine resolves it against this
/// page's URL, so the hub works wherever the examples are built or served together.
struct Card {
    title: &'static str,
    description: &'static str,
    url: &'static str,
    color: (u8, u8, u8),
    icon_char: &'static str,
}

const CARDS: &[Card] = &[
    Card {
        title: "Hello Sighurt",
        description: "Canvas drawing, mouse, keyboard, and a hand-drawn text field.",
        url: "hello_sighurt.wasm",
        color: GREEN,
        icon_char: "H",
    },
    Card {
        title: "Typography Demo",
        description: "Font family, weight, style, alignment, and text measurement.",
        url: "typography_demo.wasm",
        color: CYAN,
        icon_char: "T",
    },
    Card {
        title: "Gradient Demo",
        description: "Linear and radial gradients in a responsive grid.",
        url: "gradient_demo.wasm",
        color: PINK,
        icon_char: "G",
    },
    Card {
        title: "Animation Frames",
        description: "A bouncing ball driven by request_animation_frame.",
        url: "raf_demo.wasm",
        color: YELLOW,
        icon_char: "R",
    },
    Card {
        title: "Timer Demo",
        description: "Countdown, delayed messages, blink intervals, and stopwatch.",
        url: "timer_demo.wasm",
        color: ORANGE,
        icon_char: "T",
    },
    Card {
        title: "Event System",
        description: "Resize, focus, online/offline, touch, gamepad, drag-drop, and custom events.",
        url: "events_demo.wasm",
        color: PURPLE,
        icon_char: "E",
    },
    Card {
        title: "Server-Sent Events",
        description: "EventSource streams with automatic reconnect and Last-Event-ID.",
        url: "sse_demo.wasm",
        color: CYAN,
        icon_char: "S",
    },
    Card {
        title: "Audio Player",
        description: "Tones and sound effects synthesised in the guest, on two channels.",
        url: "audio_player.wasm",
        color: BLUE,
        icon_char: "A",
    },
    Card {
        title: "Fullstack Notes",
        description: "Protobuf over HTTP to a Rust backend (start notes-server first).",
        url: "fullstack_notes_frontend.wasm",
        color: GREEN,
        icon_char: "N",
    },
];

#[no_mangle]
pub extern "C" fn start_app() {
    log("Sighurt examples hub loaded!");
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    let (width, _height) = canvas_dimensions();
    let w = width as f32;
    // Epoch seconds need f64: as f32 they only change every ~2 minutes.
    let t = time_now_ms() as f64 / 1000.0;

    canvas_clear(BG.0, BG.1, BG.2, 255);
    clear_hyperlinks();

    // ── Animated background particles ────────────────────────────────
    for i in 0..12 {
        let fi = i as f64;
        let px = ((t * 0.15 + fi * 1.7).sin() * 0.5 + 0.5) as f32 * w;
        let py = ((t * 0.1 + fi * 2.3).cos() * 0.5 + 0.5) as f32 * 500.0;
        let r = 2.0 + (t * 0.3 + fi).sin().abs() as f32 * 4.0;
        canvas_circle(px, py, r, ACCENT.0, ACCENT.1, ACCENT.2, 25);
    }

    // ── Header region ────────────────────────────────────────────────
    rect(0.0, 0.0, w, 120.0, HEADER_BG, 255);
    let glow_alpha = ((t * 2.0).sin() * 0.3 + 0.7) * 255.0;
    rect(0.0, 118.0, w, 2.0, ACCENT, glow_alpha as u8);

    text(32.0, 28.0, 32.0, ACCENT_GLOW, "Sighurt");
    text(
        32.0,
        66.0,
        14.0,
        TEXT_DIM,
        "Example WebAssembly apps for the WASM engine",
    );
    text(
        32.0,
        88.0,
        12.0,
        TEXT_DIM,
        "Build the examples, then run: sig file://.../target/wasm32-unknown-unknown/release/index.wasm",
    );

    // Version badge
    let badge_x = w - 120.0;
    rect(badge_x, 48.0, 88.0, 24.0, ACCENT, 60);
    text(badge_x + 12.0, 52.0, 12.0, TEXT_BRIGHT, "v0.1.0");

    // ── Section title ────────────────────────────────────────────────
    text(32.0, 142.0, 18.0, TEXT_BRIGHT, "Demo Applications");
    divider(168.0, w);

    // ── App cards ────────────────────────────────────────────────────
    let card_start_y = 185.0;
    let card_h = 90.0;
    let card_gap = 12.0;
    let card_margin = 32.0;
    let card_w = w - card_margin * 2.0;
    let text_x = card_margin + 68.0;

    let (mx, my) = mouse_position();

    for (i, card) in CARDS.iter().enumerate() {
        let cy = card_start_y + (i as f32) * (card_h + card_gap);
        let hovered =
            mx >= card_margin && mx <= card_margin + card_w && my >= cy && my <= cy + card_h;

        let bg = if hovered { CARD_HOVER } else { CARD_BG };
        rect(card_margin, cy, card_w, card_h, bg, 255);
        // Left color accent bar
        rect(card_margin, cy, 4.0, card_h, card.color, 255);

        // Icon circle
        let (c, icon_cx, icon_cy) = (card.color, card_margin + 36.0, cy + card_h / 2.0);
        canvas_circle(icon_cx, icon_cy, 18.0, c.0, c.1, c.2, 40);
        text(icon_cx - 7.0, icon_cy - 10.0, 18.0, c, card.icon_char);

        text(text_x, cy + 14.0, 16.0, TEXT_BRIGHT, card.title);
        text(text_x, cy + 36.0, 11.0, c, card.url);
        text(text_x, cy + 56.0, 12.0, TEXT_DIM, card.description);

        // Arrow indicator on hover
        if hovered {
            text(card_margin + card_w - 36.0, icon_cy - 10.0, 18.0, c, ">");
        }

        register_hyperlink(card_margin, cy, card_w, card_h, card.url);
    }

    // ── Footer ───────────────────────────────────────────────────────
    let footer_y = card_start_y + (CARDS.len() as f32) * (card_h + card_gap) + 20.0;
    divider(footer_y, w);
    text(
        32.0,
        footer_y + 16.0,
        11.0,
        TEXT_DIM,
        "Built with sighurt-sdk  |  Rust + WebAssembly  |  links open the sibling .wasm files",
    );
    text(
        32.0,
        footer_y + 36.0,
        11.0,
        TEXT_DIM,
        "Click any card to launch the demo in this browser.",
    );

    set_content_size(width, (footer_y + 70.0) as u32);
}

fn text(x: f32, y: f32, size: f32, (r, g, b): (u8, u8, u8), s: &str) {
    canvas_text(x, y, size, r, g, b, 255, s);
}

fn rect(x: f32, y: f32, w: f32, h: f32, (r, g, b): (u8, u8, u8), alpha: u8) {
    canvas_rect(x, y, w, h, r, g, b, alpha);
}

fn divider(y: f32, w: f32) {
    canvas_line(
        32.0,
        y,
        w - 32.0,
        y,
        DIVIDER.0,
        DIVIDER.1,
        DIVIDER.2,
        255,
        1.0,
    );
}
