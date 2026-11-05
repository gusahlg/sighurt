//! The guest's entry points: input from the engine into the page, and the page drawn with the
//! canvas API, only when something changed (the canvas keeps what was drawn).
//!
//! Input is polled from the SDK each frame (keys, typed text, the pointer, the wheel), which
//! loses the order within a frame and tells no paste from typing; Ctrl+V pastes what the page
//! itself copied. The `sig` module's ordered input queue replaces this when it exists.
//!
//! Must not own: editing or the page's logic, which are tested on the host.

use std::cell::RefCell;

use sighurt_sdk::*;

use crate::editor::{Input, Key, Mods};
use crate::host::FakeHost;
use crate::layout::{Measure, Widths};
use crate::page::{Page, RowKind, Validity, PANEL_ROWS};
use crate::syntax::{self, Style};

/// Monospace families tried in order; the first installed one is used.
const MONOSPACE: &[&str] = &[
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "JetBrains Mono",
    "Fira Mono",
    "Source Code Pro",
    "Hack",
    "Ubuntu Mono",
    "Cascadia Mono",
];

const HINT: &str = "With the example keymap, press Escape for INSERT mode to type here. Ctrl+S \
                    saves. Every setting is described at sig://help/config.";

type Rgb = (u8, u8, u8);
const BG: Rgb = (29, 31, 36);
const BAR: Rgb = (38, 41, 48);
const TEXT: Rgb = (221, 223, 228);
const DIM: Rgb = (128, 133, 145);
const LINE_BG: Rgb = (37, 40, 47);
const SELECTION: Rgb = (56, 78, 120);
const CARET: Rgb = (240, 240, 245);
const ERROR: Rgb = (240, 112, 112);
const NOTICE: Rgb = (214, 196, 120);
const OK: Rgb = (140, 200, 150);

fn style_color(style: Style) -> Rgb {
    match style {
        Style::Plain => TEXT,
        Style::Comment => (120, 150, 120),
        Style::Header => (232, 182, 96),
        Style::Key => (132, 182, 240),
        Style::String => (206, 152, 112),
    }
}

struct HostMeasure {
    family: &'static str,
    size: f32,
}

impl Measure for HostMeasure {
    fn advance(&mut self, cluster: &str) -> f32 {
        canvas_measure_text(self.size, self.family, 400, FONT_STYLE_NORMAL, cluster).width
    }
}

struct App {
    page: Page,
    host: FakeHost,
    mono: &'static str,
    size: (u32, u32),
    pointer: (f32, f32),
    down: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

#[no_mangle]
pub extern "C" fn start_app() {
    log("sig://config: running over an in-memory file (no sig module yet)");
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    APP.with(|app| {
        let mut app = app.borrow_mut();
        let app = app.get_or_insert_with(App::new);
        app.frame();
    });
}

/// The first monospace family installed: one whose narrow and wide letters measure alike.
fn monospace(size: f32) -> &'static str {
    let width =
        |family, text| canvas_measure_text(size, family, 400, FONT_STYLE_NORMAL, text).width;
    MONOSPACE
        .iter()
        .copied()
        .find(|family| {
            let narrow = width(family, "iiiiiiiiii");
            narrow > 0.0 && (narrow - width(family, "MMMMMMMMMM")).abs() < 0.5
        })
        .unwrap_or("")
}

impl App {
    fn new() -> Self {
        let size = crate::page::Sizes::default().font;
        let mono = monospace(size);
        let mut page = Page::new(Widths::new(Box::new(HostMeasure { family: mono, size })));
        let mut host = FakeHost::default();
        page.start(&mut host);
        Self {
            page,
            host,
            mono,
            size: (0, 0),
            pointer: (0.0, 0.0),
            down: false,
        }
    }

    fn frame(&mut self) {
        let now = time_now_ms();
        let size = canvas_dimensions();
        if size != self.size {
            self.size = size;
            self.page.resize(size.0 as f32, size.1 as f32);
        }
        for input in self.inputs() {
            self.page.input(input, &mut self.host, now);
        }
        self.page.tick(&mut self.host, now);
        if self.page.take_dirty() {
            draw(&mut self.page, self.mono);
        }
    }

    /// This frame's input, polled.
    fn inputs(&mut self) -> Vec<Input> {
        let mut out = Vec::new();
        let bits = modifiers();
        let mods = Mods {
            shift: bits & 1 != 0,
            ctrl: bits & 2 != 0,
            alt: bits & 4 != 0,
        };
        let pointer = mouse_position();
        let down = mouse_button_down(0);
        if down && !self.down {
            out.push(Input::Press {
                x: pointer.0,
                y: pointer.1,
                shift: mods.shift,
            });
        } else if down && pointer != self.pointer {
            out.push(Input::Drag {
                x: pointer.0,
                y: pointer.1,
            });
        }
        (self.down, self.pointer) = (down, pointer);
        let (sx, sy) = scroll_delta();
        if sx != 0.0 || sy != 0.0 {
            // The SDK's delta is positive scrolling up or left.
            out.push(Input::Wheel { dx: -sx, dy: -sy });
        }
        for &(code, key) in KEYS {
            if key_pressed(code) {
                out.push(Input::Key(key, mods));
            }
        }
        if mods.ctrl {
            for (i, code) in (KEY_A..=KEY_Z).enumerate() {
                if key_pressed(code) {
                    let c = char::from(b'a' + i as u8);
                    if c == 'v' {
                        out.push(Input::Paste(self.host.clipboard.clone()));
                    } else {
                        out.push(Input::Key(Key::Char(c), mods));
                    }
                }
            }
        } else {
            let text = text_input();
            if !text.is_empty() {
                out.push(Input::Text(text));
            }
        }
        out
    }
}

const KEYS: &[(u32, Key)] = &[
    (KEY_ENTER, Key::Enter),
    (KEY_ESCAPE, Key::Escape),
    (KEY_TAB, Key::Tab),
    (KEY_BACKSPACE, Key::Backspace),
    (KEY_DELETE, Key::Delete),
    (KEY_LEFT, Key::Left),
    (KEY_RIGHT, Key::Right),
    (KEY_UP, Key::Up),
    (KEY_DOWN, Key::Down),
    (KEY_HOME, Key::Home),
    (KEY_END, Key::End),
    (KEY_PAGE_UP, Key::PageUp),
    (KEY_PAGE_DOWN, Key::PageDown),
];

fn rect(x: f32, y: f32, w: f32, h: f32, (r, g, b): Rgb) {
    canvas_rect(x, y, w, h, r, g, b, 255);
}

// The SDK's text call with the color as one value: as many arguments as it has.
#[allow(clippy::too_many_arguments)]
fn text(x: f32, y: f32, size: f32, (r, g, b): Rgb, family: &str, weight: u32, align: u32, s: &str) {
    canvas_text_ex(
        x,
        y,
        size,
        r,
        g,
        b,
        255,
        family,
        weight,
        FONT_STYLE_NORMAL,
        align,
        s,
    );
}

fn draw(page: &mut Page, mono: &str) {
    let s = page.sizes;
    let l = page.layout;
    canvas_clear(BG.0, BG.1, BG.2, 255);

    let h = l.header;
    rect(h.x, h.y, h.w, h.h, BAR);
    text(
        s.pad,
        s.pad * 0.5,
        s.ui_font,
        TEXT,
        "",
        700,
        TEXT_ALIGN_LEFT,
        "sig://config: your configuration",
    );
    text(
        s.pad,
        s.pad * 0.5 + s.ui_line,
        s.ui_font,
        DIM,
        "",
        400,
        TEXT_ALIGN_LEFT,
        HINT,
    );
    if let Some(b) = l.reload {
        canvas_rounded_rect(b.x, b.y, b.w, b.h, 4.0, 70, 90, 140, 255);
        let y = b.y + (b.h - s.ui_line) / 2.0;
        text(
            b.x + b.w / 2.0,
            y,
            s.ui_font,
            TEXT,
            "",
            600,
            TEXT_ALIGN_CENTER,
            "Read the file again",
        );
    }

    let t = l.text;
    if let Some(why) = page.unavailable() {
        for (i, line) in why.lines().enumerate() {
            text(
                t.x,
                t.y + s.pad + i as f32 * s.ui_line,
                s.ui_font,
                ERROR,
                "",
                400,
                TEXT_ALIGN_LEFT,
                line,
            );
        }
        return;
    }
    if !page.ready() {
        text(
            t.x,
            t.y + s.pad,
            s.ui_font,
            DIM,
            "",
            400,
            TEXT_ALIGN_LEFT,
            "Reading the configuration…",
        );
        return;
    }
    draw_text(page, mono);
    draw_status(page);
    draw_panel(page);
}

fn draw_text(page: &mut Page, mono: &str) {
    let s = page.sizes;
    let (g, t) = (page.layout.gutter, page.layout.text);
    let ed = &page.editor;
    let (top, scroll_x, caret) = (ed.top, ed.scroll_x, ed.caret());
    let selection = ed.selection();
    let rows = (t.h / s.line).ceil() as usize;
    let marked: Vec<usize> = page
        .problems
        .iter()
        .map(|p| p.line)
        .filter(|&n| n > 0)
        .collect();
    let widths = &mut page.widths;
    let buf = page.editor.buffer();
    let space = widths.space();
    for row in 0..rows {
        let n = top + row;
        if n >= buf.line_count() {
            break;
        }
        let line = buf.line(n);
        let y = t.y + row as f32 * s.line;
        if n == caret.line {
            rect(0.0, y, g.w + t.w + s.pad, s.line, LINE_BG);
        }
        if marked.contains(&(n + 1)) {
            rect(0.0, y, 3.0, s.line, ERROR);
        }
        let number = (n + 1).to_string();
        let color = if n == caret.line { TEXT } else { DIM };
        text(
            g.w - s.pad * 0.75,
            y,
            s.font,
            color,
            mono,
            400,
            TEXT_ALIGN_RIGHT,
            &number,
        );

        canvas_save();
        canvas_clip(t.x, t.y, t.w, t.h);
        if let Some((start, end)) = selection.filter(|(a, b)| a.line <= n && n <= b.line) {
            let x0 = if n == start.line {
                widths.x_of(line, start.col)
            } else {
                0.0
            };
            let mut x1 = if n == end.line {
                widths.x_of(line, end.col)
            } else {
                widths.x_of(line, line.len())
            };
            if n < end.line {
                x1 += space;
            }
            rect(t.x + x0 - scroll_x, y, x1 - x0, s.line, SELECTION);
        }
        let (from, to, at) = widths.visible(line, scroll_x, scroll_x + t.w);
        let mut x = at;
        for (a, b, style) in syntax::spans(line) {
            let (a, b) = (a.max(from), b.min(to));
            if a >= b {
                continue;
            }
            // Tabs are drawn as the room they take.
            for piece in line[a..b].split_inclusive('\t') {
                let shown = piece.trim_end_matches('\t');
                if !shown.is_empty() {
                    text(
                        t.x + x - scroll_x,
                        y,
                        s.font,
                        style_color(style),
                        mono,
                        400,
                        TEXT_ALIGN_LEFT,
                        shown,
                    );
                }
                x = widths.advance(x, piece);
            }
        }
        if n == caret.line {
            let cx = widths.x_of(line, caret.col);
            rect(t.x + cx - scroll_x, y + 1.0, 2.0, s.line - 2.0, CARET);
        }
        canvas_restore();
    }
}

fn draw_status(page: &mut Page) {
    let s = page.sizes;
    let b = page.layout.status;
    rect(b.x, b.y, b.w, b.h, BAR);
    let y = b.y + s.pad * 0.25;
    let mut left = page.path.clone();
    if page.read_only {
        left.push_str("  [read-only]");
    }
    if page.modified() {
        left.push_str("  [modified]");
    }
    if page.saving() {
        left.push_str("  saving…");
    }
    text(s.pad, y, s.ui_font, TEXT, "", 400, TEXT_ALIGN_LEFT, &left);
    let (validity, color) = match page.validity() {
        Validity::Unknown => (String::new(), DIM),
        Validity::Checking => ("checking…".to_owned(), DIM),
        Validity::Valid => ("valid".to_owned(), OK),
        Validity::Invalid(1) => ("1 problem".to_owned(), ERROR),
        Validity::Invalid(n) => (format!("{n} problems"), ERROR),
    };
    let right = b.w - s.pad;
    text(
        right,
        y,
        s.ui_font,
        color,
        "",
        600,
        TEXT_ALIGN_RIGHT,
        &validity,
    );
    let (line, col) = page.line_col();
    let at = format!("Ln {line}, Col {col}");
    text(
        right - 110.0,
        y,
        s.ui_font,
        DIM,
        "",
        400,
        TEXT_ALIGN_RIGHT,
        &at,
    );
}

fn draw_panel(page: &mut Page) {
    let s = page.sizes;
    let p = page.layout.panel;
    if p.h <= 0.0 {
        return;
    }
    let rows = page.rows();
    let shown = rows.len().min(PANEL_ROWS);
    for (i, row) in rows.iter().take(shown).enumerate() {
        let y = p.y + s.pad * 0.5 + i as f32 * s.ui_line;
        if i + 1 == shown && rows.len() > shown {
            let more = format!("… and {} more", rows.len() - shown + 1);
            text(s.pad, y, s.ui_font, DIM, "", 400, TEXT_ALIGN_LEFT, &more);
            break;
        }
        let color = match row.kind {
            RowKind::Info => OK,
            RowKind::Error | RowKind::Problem => ERROR,
            RowKind::Notice | RowKind::Restart => NOTICE,
        };
        let shown = match row.line {
            Some(n) if !row.text.starts_with("line ") => format!("line {n}: {}", row.text),
            _ => row.text.clone(),
        };
        text(s.pad, y, s.ui_font, color, "", 400, TEXT_ALIGN_LEFT, &shown);
    }
}
