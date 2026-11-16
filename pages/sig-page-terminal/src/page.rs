//! The page's entry points and the calls of the `sig` and `oxide` modules: the input queue into
//! [`App`], terminals and the clipboard through `sig`, and [`App::ops`] onto the canvas, drawn
//! again only when something changed (the canvas keeps what was drawn). Images go to the canvas
//! as the PNGs [`App::png`] keeps, which the host decodes.
//!
//! The face is the first installed monospace family of [`FAMILIES`], found by measuring on the
//! first frame (measuring works only in a frame); the cell is its advance wide and 1.2 times the
//! font size tall, the line box the canvas lays text in.
//!
//! Must not own: the page's logic ([`crate::app`]).

use std::cell::RefCell;

use sig_page_kit::sig::{self, Program, Status};
use sighurt_sdk::{
    canvas_clear, canvas_clip, canvas_dimensions, canvas_image, canvas_measure_text, canvas_rect,
    canvas_restore, canvas_save, canvas_text_ex, get_url, FONT_STYLE_ITALIC, FONT_STYLE_NORMAL,
    TEXT_ALIGN_LEFT,
};

use crate::app::{App, Ask, Host, Terminal};
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
/// Most clipboard requests waiting for the browser; more are dropped.
const MAX_CLIPBOARD_REQUESTS: usize = 8;

struct Page {
    app: App<sig::Pty>,
    face: Option<(&'static str, Metrics)>,
    /// `sig://config`'s question about the editor, until the browser answers.
    asking: Option<sig::Request>,
    /// Clipboard requests the browser has not answered: whether each is a paste.
    clipboard: Vec<(sig::Request, bool)>,
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
    // The page's own address (`api_get_location`, the browser's, is not an internal page's).
    let program = crate::program_for(&get_url());
    PAGE.with(|page| {
        *page.borrow_mut() = Some(Page {
            app: App::new(program),
            face: None,
            asking: None,
            clipboard: Vec::new(),
        });
    });
}

#[no_mangle]
pub extern "C" fn on_frame(dt_ms: u32) {
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
        page.app.advance_clock(dt_ms);
        page.app.set_metrics(metrics);
        for _ in 0..MAX_INPUT_PER_FRAME {
            let Some(input) = sig::input_next() else {
                break;
            };
            page.app.input(&mut Browser, &input);
        }
        if page.app.take_wants_info() {
            page.asking = sig::Request::info("editor");
        }
        if let Some(asking) = &page.asking {
            match asking.poll() {
                sig::Poll::Waiting => {}
                sig::Poll::Done(text) => {
                    page.app.editor_info(&text);
                    page.asking = None;
                }
                // The page then says the rule instead of the editor it found.
                sig::Poll::Failed(_) | sig::Poll::Gone => page.asking = None,
            }
        }
        clipboard(page);
        let (width, height) = canvas_dimensions();
        page.app.frame(metrics.fit(width, height));
        if page.app.take_dirty() {
            draw(&page.app, &page.app.ops(metrics), family);
        }
    });
}

/// Sends the app's clipboard requests, and gives it the pastes the browser answered.
fn clipboard(page: &mut Page) {
    for ask in page.app.take_asks() {
        if page.clipboard.len() >= MAX_CLIPBOARD_REQUESTS {
            break;
        }
        let request = match &ask {
            Ask::Copy {
                text,
                primary: false,
            } => sig::Request::clipboard_copy(text),
            Ask::Copy {
                text,
                primary: true,
            } => sig::Request::primary_copy(text),
            Ask::Paste { primary } => sig::Request::clipboard_paste(*primary),
        };
        if let Some(request) = request {
            page.clipboard
                .push((request, matches!(ask, Ask::Paste { .. })));
        }
    }
    let mut pasted = Vec::new();
    page.clipboard
        .retain(|(request, paste)| match request.poll() {
            sig::Poll::Waiting => true,
            sig::Poll::Done(text) => {
                if *paste {
                    pasted.push(text);
                }
                false
            }
            sig::Poll::Failed(_) | sig::Poll::Gone => false,
        });
    for text in pasted {
        page.app.pasted(&text);
    }
}

fn draw(app: &App<sig::Pty>, ops: &[Op], family: &str) {
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
            Op::Image { image, rect, clip } => {
                if let Some(png) = app.png(*image) {
                    let [x, y, w, h] = *rect;
                    let [cx, cy, cw, ch] = *clip;
                    canvas_save();
                    canvas_clip(cx, cy, cw, ch);
                    canvas_image(x, y, w, h, png);
                    canvas_restore();
                }
            }
        }
    }
}
