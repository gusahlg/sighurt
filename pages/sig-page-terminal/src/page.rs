//! The page's entry points and the calls of the `sig` and `oxide` modules: the input queue into
//! [`App`], terminals through `sig`, and [`App::ops`] onto the canvas, drawn again only when
//! something changed (the canvas keeps what was drawn).
//!
//! The face is the first installed monospace family of [`FAMILIES`], found by measuring on the
//! first frame (measuring works only in a frame); the cell is its advance wide and 1.2 times the
//! font size tall, the line box the canvas lays text in.
//!
//! Must not own: the page's logic ([`crate::app`]).

use std::cell::RefCell;

use sig_page_kit::sig::{self, Program, Status};
use sighurt_sdk::{
    canvas_clear, canvas_dimensions, canvas_measure_text, canvas_rect, canvas_text_ex,
    get_location, FONT_STYLE_ITALIC, FONT_STYLE_NORMAL, TEXT_ALIGN_LEFT,
};

use crate::app::{App, Host, Terminal};
use crate::draw::{Metrics, Op, BACKGROUND};

/// The font size, in canvas pixels.
const SIZE: f32 = 15.0;
/// Monospace families to look for, in order.
const FAMILIES: &[&str] = &[
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "JetBrains Mono",
    "Fira Mono",
    "Source Code Pro",
    "Hack",
    "Cascadia Mono",
    "Ubuntu Mono",
    "Menlo",
    "Consolas",
];
/// Most input records one frame takes from the queue.
const MAX_INPUT_PER_FRAME: usize = 256;

struct Page {
    app: App<sig::Pty>,
    face: Option<(&'static str, Metrics)>,
}

thread_local! {
    static PAGE: RefCell<Option<Page>> = const { RefCell::new(None) };
}

struct Browser;

impl Host for Browser {
    type Pty = sig::Pty;

    fn open(&mut self, program: Program, cols: u16, rows: u16) -> Option<sig::Pty> {
        sig::Pty::open(program, cols, rows)
    }
}

impl Terminal for sig::Pty {
    fn status(&self) -> Status {
        sig::Pty::status(self)
    }
    fn read(&self, buf: &mut [u8]) -> Option<usize> {
        sig::Pty::read(self, buf)
    }
    fn write(&self, bytes: &[u8]) -> Option<usize> {
        sig::Pty::write(self, bytes)
    }
    fn resize(&self, cols: u16, rows: u16) -> bool {
        sig::Pty::resize(self, cols, rows)
    }
}

/// The first family of [`FAMILIES`] that lays narrow and wide letters the same width, and its
/// cell; the default face, drawn a cell at a time, when none does. `None` until measuring works.
fn face() -> Option<(&'static str, Metrics)> {
    let width = |family: &str, text: &str| {
        canvas_measure_text(SIZE, family, 400, FONT_STYLE_NORMAL, text).width
    };
    let cell_h = (SIZE * 1.2).ceil();
    for family in FAMILIES {
        let wide = width(family, "MMMMMMMMMM");
        if wide > 0.0 && (wide - width(family, "iiiiiiiiii")).abs() < 0.01 {
            let metrics = Metrics {
                size: SIZE,
                cell_w: wide / 10.0,
                cell_h,
                monospace: true,
            };
            return Some((family, metrics));
        }
    }
    let wide = width("", "MMMMMMMMMM");
    (wide > 0.0).then(|| {
        let metrics = Metrics {
            size: SIZE,
            cell_w: wide / 10.0,
            cell_h,
            monospace: false,
        };
        ("", metrics)
    })
}

#[no_mangle]
pub extern "C" fn start_app() {
    let program = get_location().map_or(Program::Shell, |url| crate::program_for(&url));
    PAGE.with(|page| {
        *page.borrow_mut() = Some(Page {
            app: App::new(program),
            face: None,
        });
    });
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    PAGE.with(|page| {
        let mut page = page.borrow_mut();
        let Some(page) = page.as_mut() else {
            return;
        };
        if page.face.is_none() {
            page.face = face();
        }
        let Some((family, metrics)) = page.face else {
            return;
        };
        for _ in 0..MAX_INPUT_PER_FRAME {
            let Some(input) = sig::input_next() else {
                break;
            };
            page.app.input(&mut Browser, &input);
        }
        let (width, height) = canvas_dimensions();
        page.app.frame(metrics.fit(width, height));
        if page.app.take_dirty() {
            draw(&page.app.ops(metrics), family);
        }
    });
}

fn draw(ops: &[Op], family: &str) {
    let [r, g, b] = BACKGROUND;
    canvas_clear(r, g, b, 255);
    for op in ops {
        match op {
            Op::Fill { x, y, w, h, color } => {
                let [r, g, b] = *color;
                canvas_rect(*x, *y, *w, *h, r, g, b, 255);
            }
            Op::Text {
                x,
                y,
                text,
                color,
                bold,
                italic,
            } => {
                let [r, g, b] = *color;
                let weight = if *bold { 700 } else { 400 };
                let style = if *italic {
                    FONT_STYLE_ITALIC
                } else {
                    FONT_STYLE_NORMAL
                };
                canvas_text_ex(
                    *x,
                    *y,
                    SIZE,
                    r,
                    g,
                    b,
                    255,
                    family,
                    weight,
                    style,
                    TEXT_ALIGN_LEFT,
                    text,
                );
            }
        }
    }
}
