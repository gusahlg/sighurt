//! Sixel encoder.
//!
//! Owns: the escape sequences that show one CPU frame as a sixel image whose
//! top-left cell is a rectangle's origin. Must not own: the terminal, the
//! grid, or when a frame is allowed. The caller writes the bytes through the
//! terminal's bounded output.
//!
//! The frame is [`sighurt_api::present::Frame`]: premultiplied B, G, R, A, top
//! row first. Each pixel is composited over black and quantized to a fixed
//! 6×7×6 palette (252 registers). There is no per-frame palette search and no
//! dither. Every frame is a full image. One pass per 6-row band emits only the
//! colours present in that band, run-length encoded (`!n`, split at 10000 so a
//! repeat stays inside the limit tmux accepts). Raster attributes
//! (`"1;1;W;H`) carry the size. The height is clipped to the rectangle and
//! rounded down to a multiple of 6, so the image fits the page rows.
//!
//! The introducer is `ESC P 0 ; 2 q`. Parameter 2 is 2, so colour 0 is an
//! opaque register; `ESC P 2 q` sets parameter 1 instead, and tmux then treats
//! register 0 as transparent.
//!
//! The cursor is saved and restored around the image. tmux 3.7 leaves the
//! cursor on the row after the image and scrolls when that row is past the
//! bottom; an image no taller than the page rows does not scroll the bottom
//! chrome line. DECSDM (mode 80) and private mode 8452 are not set: xterm and
//! the VT340 disagree about which sense of DECSDM scrolls, and tmux 3.7
//! ignores both.
//!
//! A frame that would exceed `max_bytes` is refused whole.

use std::cell::RefCell;
use std::fmt;

use sighurt_api::present::Frame;

use crate::kitty::{CellRect, MAX_PIXELS};

/// Palette registers: 6 red levels, 7 green, 6 blue.
const COLORS: usize = 6 * 7 * 6;
/// tmux rejects a sixel repeat above this (`strtonum` on `!n`).
const REPEAT_MAX: usize = 10_000;
/// Mask scratch above this is not allocated; the band is scanned instead.
const SCRATCH_LIMIT: usize = 8 * 1024 * 1024;

/// Why a frame was not encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The frame, the cell rectangle, or the cell size has a zero side, or the
    /// rectangle is shorter than one sixel band.
    Empty,
    /// More than [`MAX_PIXELS`].
    TooManyPixels {
        /// Pixels in the frame.
        pixels: u64,
    },
    /// `stride` or `pixels` does not cover the rows that would be encoded.
    BadFrame(&'static str),
    /// The escape sequences would exceed the caller's cap.
    TooLarge {
        /// Bytes the encode would have produced, at least `max + 1`.
        bytes: usize,
        /// The cap the caller passed.
        max: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "sixel frame or cell rectangle is empty"),
            Self::TooManyPixels { pixels } => {
                write!(f, "sixel frame has {pixels} pixels, above {MAX_PIXELS}")
            }
            Self::BadFrame(reason) => write!(f, "sixel frame is incomplete: {reason}"),
            Self::TooLarge { bytes, max } => {
                write!(f, "sixel encode is {bytes} bytes, above the cap of {max}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Encode `frame` as one sixel image at `place`'s top-left cell.
///
/// `cell_width` and `cell_height` are the terminal cell in pixels. The image
/// is clipped to `place` and to a whole number of 6-pixel bands. `max_bytes`
/// bounds the returned vec. A frame that does not fit is refused whole.
/// Scratch buffers live in a thread-local [`Encoder`] and are reused.
pub fn encode(
    frame: &Frame<'_>,
    place: CellRect,
    cell_width: u16,
    cell_height: u16,
    max_bytes: usize,
) -> Result<Vec<u8>, Error> {
    let (width, height) = clip(frame, place, cell_width, cell_height)?;
    let scan = !masks_fit(width);
    LOCAL.with(|encoder| {
        encoder
            .borrow_mut()
            .encode(frame, place, width, height, max_bytes, scan)
    })
}

thread_local! {
    static LOCAL: RefCell<Encoder> = RefCell::new(Encoder::default());
}

/// RGB percents (0–100) for register `r * 42 + g * 6 + b`.
const PALETTE: [[u8; 3]; COLORS] = palette();

const fn level_byte(level: u32, levels: u32) -> u32 {
    level * 255 / (levels - 1)
}

const fn percent(value: u32) -> u8 {
    let percent = (value * 100 + 127) / 255;
    if percent > 100 {
        100
    } else {
        percent as u8
    }
}

const fn palette() -> [[u8; 3]; COLORS] {
    let mut out = [[0u8; 3]; COLORS];
    let mut red = 0u32;
    while red < 6 {
        let mut green = 0u32;
        while green < 7 {
            let mut blue = 0u32;
            while blue < 6 {
                let index = (red * 42 + green * 6 + blue) as usize;
                out[index] = [
                    percent(level_byte(red, 6)),
                    percent(level_byte(green, 7)),
                    percent(level_byte(blue, 6)),
                ];
                blue += 1;
            }
            green += 1;
        }
        red += 1;
    }
    out
}

fn masks_fit(width: usize) -> bool {
    width
        .checked_mul(COLORS)
        .is_some_and(|bytes| bytes <= SCRATCH_LIMIT)
}

/// The pixel rectangle that is actually encoded: clipped to `place`, and a
/// whole number of sixel bands. The full frame's pixel count is checked first,
/// before the buffer is read.
fn clip(
    frame: &Frame<'_>,
    place: CellRect,
    cell_width: u16,
    cell_height: u16,
) -> Result<(usize, usize), Error> {
    if frame.size.width == 0
        || frame.size.height == 0
        || place.columns == 0
        || place.rows == 0
        || cell_width == 0
        || cell_height == 0
    {
        return Err(Error::Empty);
    }
    let pixels = u64::from(frame.size.width) * u64::from(frame.size.height);
    if pixels > MAX_PIXELS {
        return Err(Error::TooManyPixels { pixels });
    }
    let max_width = u32::from(place.columns) * u32::from(cell_width);
    let max_height = u32::from(place.rows) * u32::from(cell_height);
    let width = frame.size.width.min(max_width);
    let height = frame.size.height.min(max_height) / 6 * 6;
    if width == 0 || height == 0 {
        return Err(Error::Empty);
    }
    let width = usize::try_from(width).map_err(|_| Error::BadFrame("width"))?;
    let height = usize::try_from(height).map_err(|_| Error::BadFrame("height"))?;
    let row_bytes = width
        .checked_mul(4)
        .ok_or(Error::BadFrame("width overflows"))?;
    if frame.stride < row_bytes {
        return Err(Error::BadFrame("stride shorter than the row"));
    }
    let need = frame
        .stride
        .checked_mul(height - 1)
        .and_then(|rows| rows.checked_add(row_bytes))
        .ok_or(Error::BadFrame("frame overflows"))?;
    if frame.pixels.len() < need {
        return Err(Error::BadFrame("pixels shorter than the encoded rows"));
    }
    Ok((width, height))
}

/// Premultiplied channel over black. Alpha 0 is black; alpha 255 keeps the
/// channel; otherwise the channel is un-premultiplied and composited.
#[inline]
fn over_black(channel: u8, alpha: u8) -> u8 {
    match alpha {
        0 => 0,
        255 => channel,
        _ => {
            let straight = (u32::from(channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha);
            let straight = straight.min(255);
            u8::try_from((straight * u32::from(alpha) + 127) / 255).unwrap_or(255)
        }
    }
}

/// Register index for a straight pixel. Levels are `(channel * n) >> 8`.
#[inline]
fn quantize(red: u8, green: u8, blue: u8) -> u8 {
    let red = (u16::from(red) * 6) >> 8;
    let green = (u16::from(green) * 7) >> 8;
    let blue = (u16::from(blue) * 6) >> 8;
    u8::try_from(red * 42 + green * 6 + blue).unwrap_or(0)
}

#[inline]
fn pixel_index(pixel: &[u8]) -> u8 {
    let alpha = pixel[3];
    quantize(
        over_black(pixel[2], alpha),
        over_black(pixel[1], alpha),
        over_black(pixel[0], alpha),
    )
}

/// Reused buffers for [`encode`]. One encoder per thread.
#[derive(Default)]
struct Encoder {
    /// Palette index of each pixel in the current 6-row band, row-major.
    indices: Vec<u8>,
    /// Six-bit mask of each colour across the band, colour-major. Empty when
    /// the band is scanned instead.
    masks: Vec<u8>,
}

impl Encoder {
    fn prepare(&mut self, width: usize, scan: bool) {
        let indices = width.saturating_mul(6);
        if self.indices.len() < indices {
            self.indices.resize(indices, 0);
        }
        if scan {
            return;
        }
        let masks = COLORS.saturating_mul(width);
        if self.masks.len() < masks {
            self.masks.resize(masks, 0);
        }
    }

    fn encode(
        &mut self,
        frame: &Frame<'_>,
        place: CellRect,
        width: usize,
        height: usize,
        max_bytes: usize,
        scan: bool,
    ) -> Result<Vec<u8>, Error> {
        self.prepare(width, scan);
        let mut out = Out::new(max_bytes);
        emit_preamble(&mut out, place, width, height);
        if out.over {
            return out.finish();
        }
        let mut defined = [false; COLORS];
        let bands = height / 6;
        for band in 0..bands {
            if band > 0 {
                out.byte(b'-');
            }
            let mut present = [false; COLORS];
            self.paint_band(frame, band, width, &mut present);
            if !scan {
                self.stamp_masks(width, &present);
            }
            let mut first = true;
            for (color, on) in present.iter().enumerate() {
                if !*on {
                    continue;
                }
                if !first {
                    out.byte(b'$');
                }
                first = false;
                let index = u8::try_from(color).unwrap_or(0);
                emit_color(&mut out, index, &mut defined);
                if scan {
                    emit_sixels_scan(&mut out, &self.indices[..width * 6], width, index);
                } else {
                    let start = color * width;
                    emit_sixels(&mut out, &self.masks[start..start + width]);
                }
            }
            if out.over {
                return out.finish();
            }
        }
        out.push(b"\x1b\\\x1b[u");
        out.finish()
    }

    fn paint_band(
        &mut self,
        frame: &Frame<'_>,
        band: usize,
        width: usize,
        present: &mut [bool; COLORS],
    ) {
        let stride = frame.stride;
        for row in 0..6 {
            let src = (band * 6 + row) * stride;
            let dst = row * width;
            for x in 0..width {
                let pixel = src + x * 4;
                let index = pixel_index(&frame.pixels[pixel..pixel + 4]);
                self.indices[dst + x] = index;
                present[usize::from(index)] = true;
            }
        }
    }

    fn stamp_masks(&mut self, width: usize, present: &[bool; COLORS]) {
        for (color, on) in present.iter().enumerate() {
            if *on {
                self.masks[color * width..(color + 1) * width].fill(0);
            }
        }
        for row in 0..6 {
            let bit = 1u8 << row;
            let src = row * width;
            for x in 0..width {
                let color = usize::from(self.indices[src + x]);
                self.masks[color * width + x] |= bit;
            }
        }
    }
}

fn emit_preamble(out: &mut Out, place: CellRect, width: usize, height: usize) {
    let column = u32::from(place.column) + 1;
    let line = u32::from(place.row) + 1;
    out.push(b"\x1b[s");
    if line == 1 && column == 1 {
        out.push(b"\x1b[H");
    } else {
        let mut buf = [0u8; 24];
        buf[0] = 0x1b;
        buf[1] = b'[';
        let mut at = 2;
        at += write_dec(&mut buf[at..], line);
        buf[at] = b';';
        at += 1;
        at += write_dec(&mut buf[at..], column);
        buf[at] = b'H';
        out.push(&buf[..=at]);
    }
    out.push(b"\x1bP0;2q\"1;1;");
    let mut buf = [0u8; 24];
    let mut at = write_dec(&mut buf, u32::try_from(width).unwrap_or(u32::MAX));
    buf[at] = b';';
    at += 1;
    at += write_dec(&mut buf[at..], u32::try_from(height).unwrap_or(u32::MAX));
    out.push(&buf[..at]);
}

fn emit_color(out: &mut Out, index: u8, defined: &mut [bool; COLORS]) {
    let mut buf = [0u8; 24];
    buf[0] = b'#';
    let mut at = 1 + write_dec(&mut buf[1..], u32::from(index));
    if !defined[usize::from(index)] {
        defined[usize::from(index)] = true;
        let [red, green, blue] = PALETTE[usize::from(index)];
        buf[at] = b';';
        buf[at + 1] = b'2';
        buf[at + 2] = b';';
        at += 3;
        at += write_dec(&mut buf[at..], u32::from(red));
        buf[at] = b';';
        at += 1;
        at += write_dec(&mut buf[at..], u32::from(green));
        buf[at] = b';';
        at += 1;
        at += write_dec(&mut buf[at..], u32::from(blue));
    }
    out.push(&buf[..at]);
}

/// Sixel bytes for one colour of a band. Trailing zero sixels are omitted.
fn emit_sixels(out: &mut Out, masks: &[u8]) {
    let end = masks
        .iter()
        .rposition(|&bits| bits != 0)
        .map_or(0, |at| at + 1);
    let mut x = 0;
    while x < end {
        let value = masks[x];
        let mut count = 1;
        while x + count < end && count < REPEAT_MAX && masks[x + count] == value {
            count += 1;
        }
        emit_run(out, value, count);
        x += count;
    }
}

fn emit_sixels_scan(out: &mut Out, indices: &[u8], width: usize, color: u8) {
    let mut end = 0;
    for x in 0..width {
        if band_bits(indices, width, color, x) != 0 {
            end = x + 1;
        }
    }
    let mut x = 0;
    while x < end {
        let value = band_bits(indices, width, color, x);
        let mut count = 1;
        while x + count < end
            && count < REPEAT_MAX
            && band_bits(indices, width, color, x + count) == value
        {
            count += 1;
        }
        emit_run(out, value, count);
        x += count;
    }
}

fn band_bits(indices: &[u8], width: usize, color: u8, x: usize) -> u8 {
    let mut bits = 0;
    for row in 0..6 {
        if indices[row * width + x] == color {
            bits |= 1 << row;
        }
    }
    bits
}

fn emit_run(out: &mut Out, value: u8, count: usize) {
    let ch = b'?' + value;
    if count < 3 {
        let pair = [ch, ch];
        out.push(&pair[..count]);
        return;
    }
    let mut buf = [0u8; 8];
    buf[0] = b'!';
    let at = 1 + write_dec(&mut buf[1..], u32::try_from(count).unwrap_or(u32::MAX));
    buf[at] = ch;
    out.push(&buf[..=at]);
}

fn write_dec(buf: &mut [u8], mut value: u32) -> usize {
    if value == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut digits = [0u8; 10];
    let mut i = 10;
    while value > 0 {
        i -= 1;
        digits[i] = b'0' + u8::try_from(value % 10).unwrap_or(0);
        value /= 10;
    }
    let len = 10 - i;
    buf[..len].copy_from_slice(&digits[i..]);
    len
}

struct Out {
    buf: Vec<u8>,
    max: usize,
    produced: usize,
    over: bool,
}

impl Out {
    fn new(max: usize) -> Self {
        Self {
            buf: Vec::with_capacity(1024.min(max)),
            max,
            produced: 0,
            over: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if self.over {
            return;
        }
        self.produced = self.produced.saturating_add(bytes.len());
        if self.buf.len().saturating_add(bytes.len()) > self.max {
            self.over = true;
            return;
        }
        self.buf.extend_from_slice(bytes);
    }

    fn byte(&mut self, byte: u8) {
        self.push(&[byte]);
    }

    fn finish(self) -> Result<Vec<u8>, Error> {
        if self.over {
            Err(Error::TooLarge {
                bytes: self.produced.max(self.max.saturating_add(1)),
                max: self.max,
            })
        } else {
            Ok(self.buf)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use sighurt_api::geometry::BufferSize;

    use super::*;

    fn owned(pixels: &mut [u8], width: u32, height: u32) -> Frame<'_> {
        Frame {
            pixels,
            size: BufferSize { width, height },
            stride: usize::try_from(width).unwrap() * 4,
        }
    }

    fn encode_pixels(
        pixels: &mut [u8],
        width: u32,
        height: u32,
        place: CellRect,
        cell_width: u16,
        cell_height: u16,
        max: usize,
    ) -> Result<Vec<u8>, Error> {
        let frame = owned(pixels, width, height);
        encode(&frame, place, cell_width, cell_height, max)
    }

    fn full(columns: u16, rows: u16) -> CellRect {
        CellRect {
            column: 0,
            row: 0,
            columns,
            rows,
        }
    }

    fn raster(bytes: &[u8]) -> (u32, u32) {
        let marker = b"\"1;1;";
        let at = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("raster attributes");
        let rest = &bytes[at + marker.len()..];
        let end = rest
            .iter()
            .position(|byte| !byte.is_ascii_digit() && *byte != b';')
            .expect("raster end");
        let spec = std::str::from_utf8(&rest[..end]).unwrap();
        let (width, height) = spec.split_once(';').unwrap();
        (width.parse().unwrap(), height.parse().unwrap())
    }

    #[test]
    fn a_known_frame_encodes_header_palette_rle_and_bands() {
        let mut red = [0u8, 0, 255, 255].repeat(4 * 6);
        let bytes = encode_pixels(&mut red, 4, 6, full(4, 1), 1, 6, 1024).unwrap();
        assert_eq!(
            bytes,
            b"\x1b[s\x1b[H\x1bP0;2q\"1;1;4;6#210;2;100;0;0!4~\x1b\\\x1b[u"
        );

        let mut tall = [0u8, 0, 255, 255].repeat(12);
        let bytes = encode_pixels(&mut tall, 1, 12, full(1, 2), 1, 6, 1024).unwrap();
        assert_eq!(
            bytes,
            b"\x1b[s\x1b[H\x1bP0;2q\"1;1;1;12#210;2;100;0;0~-#210~\x1b\\\x1b[u"
        );
        assert_eq!(bytes.iter().filter(|byte| **byte == b'-').count(), 1);

        let mut mixed = Vec::new();
        for _ in 0..6 {
            mixed.extend_from_slice(&[0, 0, 255, 255, 255, 0, 0, 255]);
        }
        let place = CellRect {
            column: 1,
            row: 2,
            columns: 2,
            rows: 1,
        };
        let bytes = encode_pixels(&mut mixed, 2, 6, place, 1, 6, 1024).unwrap();
        assert_eq!(
            bytes,
            b"\x1b[s\x1b[3;2H\x1bP0;2q\"1;1;2;6#5;2;0;0;100?~$#210;2;100;0;0~\x1b\\\x1b[u"
        );
    }

    #[test]
    fn transparent_and_premultiplied_pixels_composite_over_black() {
        // Half of opaque red, premultiplied. Un-premultiplying and forgetting
        // to composite would emit full red (register 210).
        let mut half = [0u8, 0, 128, 128].repeat(6);
        let bytes = encode_pixels(&mut half, 1, 6, full(1, 1), 1, 6, 1024).unwrap();
        assert_eq!(
            bytes,
            b"\x1b[s\x1b[H\x1bP0;2q\"1;1;1;6#126;2;60;0;0~\x1b\\\x1b[u"
        );

        // Alpha 0 is black, whatever channel was stored.
        let mut clear = [0u8, 0, 255, 0].repeat(6);
        let bytes = encode_pixels(&mut clear, 1, 6, full(1, 1), 1, 6, 1024).unwrap();
        assert_eq!(
            bytes,
            b"\x1b[s\x1b[H\x1bP0;2q\"1;1;1;6#0;2;0;0;0~\x1b\\\x1b[u"
        );
    }

    #[test]
    fn the_encoded_height_never_exceeds_the_rectangle() {
        let mut pixels = [0u8, 0, 255, 255].repeat(4 * 30);
        let place = CellRect {
            column: 0,
            row: 0,
            columns: 2,
            rows: 2,
        };
        // 2×10-pixel cells: the rectangle is 4×20, and 20 rounded down to a
        // sixel band is 18.
        let bytes = encode_pixels(&mut pixels, 4, 30, place, 2, 10, 64 * 1024).unwrap();
        let (width, height) = raster(&bytes);
        assert_eq!((width, height), (4, 18));
        assert!(height <= u32::from(place.rows) * 10);
        assert_eq!(bytes.iter().filter(|byte| **byte == b'-').count(), 2);

        let mut short = [0u8, 0, 255, 255].repeat(10 * 20);
        let bytes = encode_pixels(&mut short, 10, 20, place, 2, 10, 64 * 1024).unwrap();
        let (width, height) = raster(&bytes);
        assert_eq!((width, height), (4, 18));
        assert!(height <= 20);
    }

    #[test]
    fn a_frame_over_the_cap_is_an_error_and_not_a_partial_image() {
        let mut pixels = [0u8, 0, 255, 255].repeat(4 * 6);
        let bytes = encode_pixels(&mut pixels, 4, 6, full(4, 1), 1, 6, 1024).unwrap();
        assert!(encode_pixels(&mut pixels, 4, 6, full(4, 1), 1, 6, bytes.len()).is_ok());
        match encode_pixels(&mut pixels, 4, 6, full(4, 1), 1, 6, bytes.len() - 1) {
            Err(Error::TooLarge { bytes: got, max }) => assert!(got > max),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn a_frame_over_the_pixel_cap_is_refused_without_a_buffer() {
        let mut pixels = [];
        let frame = Frame {
            pixels: &mut pixels,
            size: BufferSize {
                width: 4000,
                height: 2001,
            },
            stride: 0,
        };
        let error = encode(&frame, full(1, 1), 1, 1, 64);
        assert_eq!(error, Err(Error::TooManyPixels { pixels: 8_004_000 }));
    }

    #[test]
    fn a_short_buffer_is_a_bad_frame_and_an_empty_rectangle_is_empty() {
        let mut pixels = [0u8; 4];
        let error = encode_pixels(&mut pixels, 2, 6, full(2, 1), 1, 6, 1024);
        assert!(matches!(error, Err(Error::BadFrame(_))));
        let mut pixels = [0u8, 0, 255, 255].repeat(6);
        let error = encode_pixels(&mut pixels, 1, 6, full(0, 1), 1, 6, 1024);
        assert_eq!(error, Err(Error::Empty));
        // One pixel is shorter than a sixel band.
        let error = encode_pixels(&mut pixels, 1, 6, full(1, 1), 1, 1, 1024);
        assert_eq!(error, Err(Error::Empty));
    }

    #[test]
    fn a_long_run_is_split_at_10000() {
        let width = 10_001u32;
        let mut pixels = [0u8, 0, 255, 255].repeat(usize::try_from(width).unwrap() * 6);
        let bytes = encode_pixels(
            &mut pixels,
            width,
            6,
            full(1, 1),
            u16::try_from(width).unwrap(),
            6,
            1024 * 1024,
        )
        .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("!10000~~"), "{text}");
    }

    #[test]
    fn scanning_a_band_matches_the_mask_path() {
        let mut pixels = Vec::new();
        for y in 0..12u32 {
            for x in 0..8u32 {
                let red = u8::try_from((x * 40 + y * 10) % 256).unwrap();
                let alpha = if x == 3 { 128 } else { 255 };
                let channel = if alpha == 255 { red } else { red / 2 };
                pixels.extend_from_slice(&[0, 0, channel, alpha]);
            }
        }
        let place = full(8, 2);
        let frame = owned(&mut pixels, 8, 12);
        let (width, height) = clip(&frame, place, 1, 6).unwrap();
        let masked = Encoder::default()
            .encode(&frame, place, width, height, 64 * 1024, false)
            .unwrap();
        let scanned = Encoder::default()
            .encode(&frame, place, width, height, 64 * 1024, true)
            .unwrap();
        assert_eq!(masked, scanned);
    }

    #[test]
    fn a_page_sized_frame_reports_its_encode_cost() {
        let width = 1876u32;
        let height = 1316u32;
        let pixels_len = usize::try_from(width).unwrap() * usize::try_from(height).unwrap();
        let mut pixels = [0u8, 0, 0, 255].repeat(pixels_len);
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = index % usize::try_from(width).unwrap();
            let y = index / usize::try_from(width).unwrap();
            let red = u8::try_from((x / 64) % 6).unwrap() * 51;
            let blue = u8::try_from((y / 64) % 6).unwrap() * 51;
            pixel[0] = blue;
            pixel[2] = red;
        }
        let started = Instant::now();
        let bytes = encode_pixels(
            &mut pixels,
            width,
            height,
            full(1876, 1316),
            1,
            1,
            16 * 1024 * 1024,
        )
        .expect("page-sized encode");
        let elapsed = started.elapsed();
        let (encoded_width, encoded_height) = raster(&bytes);
        assert_eq!((encoded_width, encoded_height), (1876, 1314));
        assert!(encoded_height <= height);
        eprintln!("sixel 1876x1316: {} bytes in {elapsed:?}", bytes.len());
        assert!(elapsed.as_secs() < 15, "encode took {elapsed:?}, above 15s");
    }
}
