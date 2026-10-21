//! Painting the demo pages into a lent CPU frame: ARGB8888, premultiplied, little-endian
//! (bytes B, G, R, A), every pixel of the buffer.
//!
//! Sizes come from the frame and the view's metrics: lengths given in logical pixels are
//! converted with the ratio of the actual, rounded buffer size to the logical size, so the
//! drawing follows every scale exactly. Rows are built once and copied, which keeps a full-page
//! paint cheap even in a debug build.
//!
//! Must not own: when to paint (the runtime) or buffer lifetime (the target).

use std::time::Duration;

use sighurt_api::geometry::{BufferPoint, Metrics, SurfacePoint};
use sighurt_api::present::Frame;

use crate::Page;

/// What one frame shows.
pub(crate) struct Scene {
    pub page: Page,
    pub failed: bool,
    pub focused: bool,
    pub pointer: Option<SurfacePoint>,
    pub elapsed: Duration,
}

/// An opaque pixel from `0xrrggbb`, as its ARGB8888 little-endian bytes.
const fn rgb(hex: u32) -> [u8; 4] {
    let [_, r, g, b] = hex.to_be_bytes();
    [b, g, r, 0xff]
}

const FOCUSED: [u8; 4] = rgb(0x22c55e);
const UNFOCUSED: [u8; 4] = rgb(0x52525b);
const BORDER: [u8; 4] = rgb(0xff0000);
const CROSSHAIR: [u8; 4] = rgb(0x2563eb);

/// The frame's pixels with their geometry.
struct Canvas<'a> {
    pixels: &'a mut [u8],
    width: usize,
    height: usize,
    stride: usize,
    /// Buffer pixels per logical pixel, horizontally and vertically.
    ratio: (f64, f64),
}

impl Canvas<'_> {
    /// Buffer pixels for `logical` logical pixels, horizontally.
    fn x(&self, logical: f64) -> usize {
        (logical * self.ratio.0).round().max(0.0) as usize
    }

    fn y(&self, logical: f64) -> usize {
        (logical * self.ratio.1).round().max(0.0) as usize
    }

    /// Fills rows `y0..y1` with copies of `row` (at least `width` pixels long).
    fn rows(&mut self, y0: usize, y1: usize, row: &[u8]) {
        let bytes = self.width * 4;
        for y in y0..y1.min(self.height) {
            let start = y * self.stride;
            self.pixels[start..start + bytes].copy_from_slice(&row[..bytes]);
        }
    }

    /// Fills the buffer-pixel rectangle `x0..x1` × `y0..y1`, clipped to the buffer.
    fn rect(&mut self, (x0, x1): (usize, usize), (y0, y1): (usize, usize), color: [u8; 4]) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let run = color.repeat(x1 - x0);
        for y in y0..y1 {
            let start = y * self.stride + x0 * 4;
            self.pixels[start..start + run.len()].copy_from_slice(&run);
        }
    }
}

/// Paints `scene` into all of `frame`. Lengths in logical pixels use `metrics`.
pub(crate) fn paint(frame: Frame<'_>, scene: &Scene, metrics: Metrics) {
    let Frame {
        pixels,
        size,
        stride,
    } = frame;
    let logical = metrics.rect();
    let ratio = |buffer: u32, logical: u32| f64::from(buffer) / f64::from(logical.max(1));
    let mut canvas = Canvas {
        pixels,
        width: size.width as usize,
        height: size.height as usize,
        stride,
        ratio: (
            ratio(size.width, logical.width),
            ratio(size.height, logical.height),
        ),
    };
    match (scene.page, scene.failed) {
        (_, true) => stripes(&mut canvas, rgb(0x450a0a), rgb(0x7f1d1d)),
        (Page::Pattern, false) => bands(&mut canvas, &PATTERN),
        (Page::Slow, false) => bands(&mut canvas, &SLOW),
        (Page::Crash, false) => stripes(&mut canvas, rgb(0x1c1917), rgb(0x292524)),
        (Page::Animation, false) => animation(&mut canvas, scene.elapsed),
        // Where the contract's conversion puts the pointer, as the status text says.
        (Page::Resize, false) => resize(&mut canvas, scene.pointer.map(|p| metrics.to_buffer(p))),
    }
    let (edge, square) = (canvas.x(8.0), canvas.x(16.0));
    let (top, bottom) = (canvas.y(8.0), canvas.y(8.0) + canvas.y(16.0));
    let focus = if scene.focused { FOCUSED } else { UNFOCUSED };
    canvas.rect((edge, edge + square), (top, bottom), focus);
}

const PATTERN: [[u8; 4]; 6] = [
    rgb(0x1e3a8a),
    rgb(0x155e75),
    rgb(0x166534),
    rgb(0x854d0e),
    rgb(0x9a3412),
    rgb(0x6b21a8),
];
const SLOW: [[u8; 4]; 3] = [rgb(0x7c2d12), rgb(0x9a3412), rgb(0xc2410c)];

/// Horizontal bands 48 logical pixels high in `colors`, with a light line every 64 logical
/// pixels across.
fn bands(canvas: &mut Canvas<'_>, colors: &[[u8; 4]]) {
    let (band, column) = (48.0 * canvas.ratio.1, 64.0 * canvas.ratio.0);
    let line = rgb(0xe4e4e7);
    let mut row = Vec::with_capacity(canvas.width * 4);
    let mut y = 0;
    while y < canvas.height {
        let index = (y as f64 / band) as usize;
        let end = (((index + 1) as f64 * band).round() as usize).clamp(y + 1, canvas.height);
        let color = colors[index % colors.len()];
        row.clear();
        for x in 0..canvas.width {
            let on_line = (x as f64 % column) < 1.0;
            row.extend_from_slice(if on_line { &line } else { &color });
        }
        canvas.rows(y, end, &row);
        y = end;
    }
}

/// Horizontal stripes: alternating 24-logical-pixel bands of `a` and `b`.
fn stripes(canvas: &mut Canvas<'_>, a: [u8; 4], b: [u8; 4]) {
    let band = 24.0 * canvas.ratio.1;
    let (row_a, row_b) = (a.repeat(canvas.width), b.repeat(canvas.width));
    for y in 0..canvas.height {
        let row = if ((y as f64 / band) as usize).is_multiple_of(2) {
            &row_a
        } else {
            &row_b
        };
        canvas.rows(y, y + 1, row);
    }
}

/// A bar sweeping across and one sweeping down, once every two seconds, whatever the frame
/// rate: the position comes from the clock, frame callbacks only pace the frames.
fn animation(canvas: &mut Canvas<'_>, elapsed: Duration) {
    let background = rgb(0x0f172a);
    let (bar, light) = (rgb(0x38bdf8), rgb(0xf472b6));
    let phase = (elapsed.as_secs_f64() / 2.0).fract();
    let width = canvas.x(48.0);
    let x = (phase * (canvas.width + width) as f64) as usize;
    let y = (phase * (canvas.height + width) as f64) as usize;
    let mut row = background.repeat(canvas.width);
    let (x0, x1) = (x.saturating_sub(width), x.min(canvas.width));
    for px in row[x0 * 4..x1 * 4].as_chunks_mut::<4>().0 {
        *px = bar;
    }
    canvas.rows(0, canvas.height, &row);
    canvas.rect((0, canvas.width), (y.saturating_sub(width), y), light);
}

/// A checkerboard of 16-logical-pixel squares, a one-buffer-pixel red border, and crosshairs
/// through the buffer pixel under the pointer.
fn resize(canvas: &mut Canvas<'_>, pointer: Option<BufferPoint>) {
    let (light, dark) = (rgb(0xd8d8d0), rgb(0x989890));
    let (square_x, square_y) = (16.0 * canvas.ratio.0, 16.0 * canvas.ratio.1);
    let row = |first: [u8; 4], second: [u8; 4]| -> Vec<u8> {
        let mut row = Vec::with_capacity(canvas.width * 4);
        for x in 0..canvas.width {
            let even = ((x as f64 / square_x) as usize).is_multiple_of(2);
            row.extend_from_slice(if even { &first } else { &second });
        }
        row
    };
    let (even, odd) = (row(light, dark), row(dark, light));
    for y in 0..canvas.height {
        let row = if ((y as f64 / square_y) as usize).is_multiple_of(2) {
            &even
        } else {
            &odd
        };
        canvas.rows(y, y + 1, row);
    }
    let (w, h) = (canvas.width, canvas.height);
    canvas.rect((0, w), (0, 1), BORDER);
    canvas.rect((0, w), (h.saturating_sub(1), h), BORDER);
    canvas.rect((0, 1), (0, h), BORDER);
    canvas.rect((w.saturating_sub(1), w), (0, h), BORDER);
    if let Some(BufferPoint { x, y }) = pointer {
        if x >= 0.0 && y >= 0.0 {
            let (x, y) = (x as usize, y as usize);
            canvas.rect((x, x + 1), (0, h), CROSSHAIR);
            canvas.rect((0, w), (y, y + 1), CROSSHAIR);
        }
    }
}
