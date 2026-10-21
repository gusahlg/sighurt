//! Kitty graphics protocol encoder.
//!
//! Owns: the escape sequences that show one CPU frame in a rectangle of cells,
//! and the bytes that delete that image. Must not own: the terminal, the grid,
//! or when a frame is allowed. The caller writes the bytes through the
//! terminal's bounded output.
//!
//! The frame is [`sighurt_api::present::Frame`]: premultiplied B, G, R, A, top
//! row first. The encoder un-premultiplies into straight RGBA (`f=32`),
//! zlib-compresses it (`o=z`) and splits the base64 into chunks of
//! [`CHUNK_BYTES`]. The first chunk places the image (`a=T`) so it scales to
//! the cell rectangle (`c`, `r`); later chunks finish the transmit (`m=1` then
//! `m=0`). `q=2` asks the terminal not to reply. The cursor is saved, moved to
//! the cell origin, and restored; `C=1` keeps the placement from moving it.
//! Transmitting again with the same image id replaces the pixels. Hide the
//! image with [`delete`] (`a=d,d=I`).

use std::cell::RefCell;
use std::fmt;
use std::io::{self, Write};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sighurt_api::present::Frame;

/// Base64 payload bytes in one APC chunk.
pub const CHUNK_BYTES: usize = 4096;

/// The most pixels one encode will accept. The terminal graphics bound.
pub const MAX_PIXELS: u64 = 8_000_000;

/// Where the image sits, in 0-based cells. The image scales to `columns` by `rows`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRect {
    /// Left column.
    pub column: u16,
    /// Top row.
    pub row: u16,
    /// Width in cells.
    pub columns: u16,
    /// Height in cells.
    pub rows: u16,
}

/// Why a frame was not encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The image id was 0. Kitty treats 0 as "no id".
    ImageId,
    /// The frame or the cell rectangle has a zero side.
    Empty,
    /// More than [`MAX_PIXELS`].
    TooManyPixels {
        /// Pixels in the frame.
        pixels: u64,
    },
    /// `stride` or `pixels` does not cover the frame.
    BadFrame(&'static str),
    /// The escape sequences would exceed the caller's cap.
    TooLarge {
        /// Bytes the encode would have produced.
        bytes: usize,
        /// The cap the caller passed.
        max: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImageId => write!(f, "kitty image id 0 is not a stored image"),
            Self::Empty => write!(f, "kitty frame or cell rectangle is empty"),
            Self::TooManyPixels { pixels } => {
                write!(f, "kitty frame has {pixels} pixels, above {MAX_PIXELS}")
            }
            Self::BadFrame(reason) => write!(f, "kitty frame is incomplete: {reason}"),
            Self::TooLarge { bytes, max } => {
                write!(f, "kitty encode is {bytes} bytes, above the cap of {max}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Encode `frame` as the escape sequences that show it at `place` under `image_id`.
///
/// `max_bytes` bounds the returned vec. A frame that does not fit is refused
/// whole, so the writer never sends a partial image. Scratch buffers live in a
/// thread-local [`Encoder`] and are reused across frames. The encode stops as
/// soon as the output would pass `max_bytes`.
pub fn encode(
    frame: &Frame<'_>,
    image_id: u32,
    place: CellRect,
    max_bytes: usize,
) -> Result<Vec<u8>, Error> {
    LOCAL.with(|encoder| {
        encoder
            .borrow_mut()
            .encode(frame, image_id, place, max_bytes)
    })
}

thread_local! {
    static LOCAL: RefCell<Encoder> = RefCell::new(Encoder::new());
}

/// RGBA rows converted per write into the compressor. Kept across frames.
const ROW_CHUNK: usize = 12 * 1024;
/// `ZlibEncoder`'s own buffer. Counted in [`Encoder::scratch_bytes`].
const ZLIB_SCRATCH: usize = 32 * 1024;

/// Reused buffers for [`encode`]. One encoder per thread is enough: `encode`
/// keeps its own and calls this.
pub struct Encoder {
    zlib: ZlibEncoder<Sink>,
    row: Vec<u8>,
    /// Set after a frame leaves the compressor ready for the next one.
    clean: bool,
}

impl Encoder {
    /// An encoder with empty scratch buffers.
    pub fn new() -> Self {
        Self {
            zlib: ZlibEncoder::new(Sink::new(), Compression::default()),
            row: Vec::with_capacity(ROW_CHUNK),
            clean: true,
        }
    }

    /// Bytes retained for the next frame, not counting a returned image.
    ///
    /// Includes the compressor's fixed buffer. It does not include the
    /// caller's pixels.
    pub fn scratch_bytes(&self) -> usize {
        self.row.capacity() + self.zlib.get_ref().scratch() + ZLIB_SCRATCH
    }

    /// [`encode`] on this encoder's buffers.
    pub fn encode(
        &mut self,
        frame: &Frame<'_>,
        image_id: u32,
        place: CellRect,
        max_bytes: usize,
    ) -> Result<Vec<u8>, Error> {
        validate(frame, image_id, place)?;
        if !self.clean {
            self.recreate();
        }
        self.clean = false;
        let width = frame.size.width;
        let height = frame.size.height;
        self.zlib
            .get_mut()
            .begin(width, height, image_id, place, max_bytes);
        if self.zlib.get_mut().over {
            let produced = self.zlib.get_mut().produced;
            self.recreate();
            return Err(Error::TooLarge {
                bytes: produced.max(max_bytes.saturating_add(1)),
                max: max_bytes,
            });
        }
        if self.write_pixels(frame).is_err() {
            let over = self.zlib.get_mut().over;
            let produced = self.zlib.get_mut().produced;
            self.recreate();
            if over {
                return Err(Error::TooLarge {
                    bytes: produced.max(max_bytes.saturating_add(1)),
                    max: max_bytes,
                });
            }
            return Err(Error::BadFrame("zlib"));
        }
        if self.zlib.try_finish().is_err() {
            let over = self.zlib.get_mut().over;
            let produced = self.zlib.get_mut().produced;
            self.recreate();
            if over {
                return Err(Error::TooLarge {
                    bytes: produced.max(max_bytes.saturating_add(1)),
                    max: max_bytes,
                });
            }
            return Err(Error::BadFrame("zlib"));
        }
        self.zlib.get_mut().finish_apc();
        if self.zlib.get_mut().over {
            let produced = self.zlib.get_mut().produced;
            self.recreate();
            return Err(Error::TooLarge {
                bytes: produced.max(max_bytes.saturating_add(1)),
                max: max_bytes,
            });
        }
        let out = std::mem::take(&mut self.zlib.get_mut().out);
        self.recycle();
        Ok(out)
    }

    fn write_pixels(&mut self, frame: &Frame<'_>) -> io::Result<()> {
        let row_bytes = usize::try_from(frame.size.width).unwrap_or(0) * 4;
        let height = usize::try_from(frame.size.height).unwrap_or(0);
        if self.row.len() != ROW_CHUNK {
            self.row.resize(ROW_CHUNK, 0);
        }
        for row in 0..height {
            let start = row * frame.stride;
            let pixels = &frame.pixels[start..start + row_bytes];
            let mut offset = 0;
            while offset < pixels.len() {
                let mut n = (pixels.len() - offset).min(self.row.len());
                n -= n % 4;
                convert(&pixels[offset..offset + n], &mut self.row[..n]);
                self.zlib.write_all(&self.row[..n])?;
                offset += n;
            }
        }
        Ok(())
    }

    /// Keep the base64 scratch and reset the compressor for the next frame.
    fn recycle(&mut self) {
        let sink = self.zlib.get_mut();
        let mut b64 = std::mem::take(&mut sink.b64);
        let mut held = std::mem::take(&mut sink.held);
        b64.clear();
        held.clear();
        if self
            .zlib
            .reset(Sink::with_scratch(b64, held, false))
            .is_err()
        {
            self.recreate();
            return;
        }
        self.clean = true;
    }

    fn recreate(&mut self) {
        let (b64, held) = {
            let sink = self.zlib.get_mut();
            let mut b64 = std::mem::take(&mut sink.b64);
            let mut held = std::mem::take(&mut sink.held);
            b64.clear();
            held.clear();
            (b64, held)
        };
        self.zlib = ZlibEncoder::new(Sink::with_scratch(b64, held, false), Compression::default());
        self.clean = true;
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

fn validate(frame: &Frame<'_>, image_id: u32, place: CellRect) -> Result<(), Error> {
    if image_id == 0 {
        return Err(Error::ImageId);
    }
    let width = frame.size.width;
    let height = frame.size.height;
    if width == 0 || height == 0 || place.columns == 0 || place.rows == 0 {
        return Err(Error::Empty);
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(Error::TooManyPixels { pixels });
    }
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or(Error::BadFrame("width overflows"))?;
    if frame.stride < row_bytes {
        return Err(Error::BadFrame("stride shorter than the row"));
    }
    let need = frame
        .stride
        .checked_mul(usize::try_from(height).unwrap_or(usize::MAX))
        .ok_or(Error::BadFrame("frame overflows"))?;
    if frame.pixels.len() < need {
        return Err(Error::BadFrame("pixels shorter than stride times height"));
    }
    Ok(())
}

/// Compressed bytes in, capped APC bytes out. The compressor writes here.
struct Sink {
    out: Vec<u8>,
    max: usize,
    produced: usize,
    over: bool,
    b64: Vec<u8>,
    held: Vec<u8>,
    has_held: bool,
    group: [u8; 3],
    group_len: usize,
    first: bool,
    width: u32,
    height: u32,
    image_id: u32,
    columns: u16,
    rows: u16,
}

impl Sink {
    fn new() -> Self {
        Self::with_scratch(Vec::new(), Vec::new(), false)
    }

    fn with_scratch(b64: Vec<u8>, held: Vec<u8>, has_held: bool) -> Self {
        Self {
            out: Vec::new(),
            max: 0,
            produced: 0,
            over: false,
            b64,
            held,
            has_held,
            group: [0; 3],
            group_len: 0,
            first: true,
            width: 0,
            height: 0,
            image_id: 0,
            columns: 0,
            rows: 0,
        }
    }

    fn scratch(&self) -> usize {
        self.out.capacity() + self.b64.capacity() + self.held.capacity()
    }

    fn begin(&mut self, width: u32, height: u32, image_id: u32, place: CellRect, max: usize) {
        self.out.clear();
        self.max = max;
        self.produced = 0;
        self.over = false;
        self.b64.clear();
        self.held.clear();
        self.has_held = false;
        self.group_len = 0;
        self.first = true;
        self.width = width;
        self.height = height;
        self.image_id = image_id;
        self.columns = place.columns;
        self.rows = place.rows;
        let cursor = cursor_to(place.column, place.row);
        if cursor.len() > max {
            self.produced = cursor.len();
            self.over = true;
            return;
        }
        self.out.extend_from_slice(&cursor);
        self.produced = self.out.len();
    }

    fn absorb(&mut self, mut data: &[u8]) {
        if self.group_len > 0 {
            let need = 3 - self.group_len;
            let take = need.min(data.len());
            self.group[self.group_len..self.group_len + take].copy_from_slice(&data[..take]);
            self.group_len += take;
            data = &data[take..];
            if self.group_len < 3 {
                return;
            }
            let group = self.group;
            self.group_len = 0;
            self.encode_group(&group);
            if self.over {
                return;
            }
        }
        while data.len() >= 3 && !self.over {
            let room_chars = CHUNK_BYTES.saturating_sub(self.b64.len());
            let room_bytes = (room_chars / 4) * 3;
            if room_bytes == 0 {
                self.seal_chunk();
                continue;
            }
            let n = if data.len() >= room_bytes {
                room_bytes
            } else {
                data.len() - data.len() % 3
            };
            if n == 0 {
                self.seal_chunk();
                continue;
            }
            let start = self.b64.len();
            self.b64.resize(start + n / 3 * 4, 0);
            let written = STANDARD
                .encode_slice(&data[..n], &mut self.b64[start..])
                .expect("base64 of a multiple of 3 bytes fits");
            self.b64.truncate(start + written);
            data = &data[n..];
            if self.b64.len() >= CHUNK_BYTES {
                self.seal_chunk();
            }
        }
        if !data.is_empty() && !self.over {
            self.group[..data.len()].copy_from_slice(data);
            self.group_len = data.len();
        }
    }

    fn encode_group(&mut self, group: &[u8; 3]) {
        if self.b64.len() + 4 > CHUNK_BYTES {
            self.seal_chunk();
            if self.over {
                return;
            }
        }
        let start = self.b64.len();
        self.b64.resize(start + 4, 0);
        let written = STANDARD
            .encode_slice(group, &mut self.b64[start..])
            .expect("one base64 group is 4 bytes");
        debug_assert_eq!(written, 4);
        if self.b64.len() >= CHUNK_BYTES {
            self.seal_chunk();
        }
    }

    fn seal_chunk(&mut self) {
        if self.b64.len() < CHUNK_BYTES || self.over {
            return;
        }
        if self.has_held {
            let chunk = std::mem::take(&mut self.held);
            self.emit(&chunk, true);
            self.held = std::mem::take(&mut self.b64);
            self.b64 = chunk;
            self.b64.clear();
            self.has_held = true;
            return;
        }
        self.held = std::mem::take(&mut self.b64);
        self.has_held = true;
    }

    fn finish_apc(&mut self) {
        if self.over {
            return;
        }
        if self.group_len > 0 {
            // Padding is 4 characters. A chunk that is already full has to be
            // sealed first: appending here used to emit 4100 payload bytes
            // when a 3072-byte group landed on the 4096 boundary and one
            // input byte was still waiting.
            if self.b64.len() + 4 > CHUNK_BYTES {
                self.seal_chunk();
            }
            if self.over {
                return;
            }
            let mut tmp = [0u8; 4];
            let written = STANDARD
                .encode_slice(&self.group[..self.group_len], &mut tmp)
                .expect("base64 tail fits in 4 bytes");
            self.b64.extend_from_slice(&tmp[..written]);
            self.group_len = 0;
        }
        let held = std::mem::take(&mut self.held);
        let tail = std::mem::take(&mut self.b64);
        let has_held = self.has_held;
        self.has_held = false;
        if has_held && tail.is_empty() {
            self.emit(&held, false);
        } else if has_held {
            self.emit(&held, true);
            if !self.over {
                self.emit(&tail, false);
            }
        } else {
            self.emit(&tail, false);
        }
        self.held = held;
        self.b64 = tail;
        self.held.clear();
        self.b64.clear();
    }

    fn emit(&mut self, b64: &[u8], more: bool) {
        if self.over {
            return;
        }
        let more_flag = if more { "1" } else { "0" };
        let control = if self.first {
            self.first = false;
            format!(
                "a=T,f=32,o=z,t=d,s={},v={},i={},c={},r={},C=1,q=2,m={more_flag}",
                self.width, self.height, self.image_id, self.columns, self.rows
            )
        } else {
            format!("q=2,m={more_flag}")
        };
        // `ESC _ G` + control + `;` + payload + `ESC \`, and `ESC [ u` on the last chunk.
        let add = 3 + control.len() + 1 + b64.len() + 2;
        let tail = if more { 0 } else { 3 };
        if self.out.len() + add + tail > self.max {
            self.produced = self.out.len() + add + tail;
            self.over = true;
            return;
        }
        self.out.extend_from_slice(b"\x1b_G");
        self.out.extend_from_slice(control.as_bytes());
        self.out.push(b';');
        self.out.extend_from_slice(b64);
        self.out.extend_from_slice(b"\x1b\\");
        if !more {
            self.out.extend_from_slice(b"\x1b[u");
        }
        self.produced = self.out.len();
    }
}

impl Write for Sink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.over {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                "kitty encode is over the cap",
            ));
        }
        self.absorb(data);
        if self.over {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                "kitty encode is over the cap",
            ));
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Delete the image `image_id` and its placements (`a=d,d=I`). `q=2` suppresses the reply.
pub fn delete(image_id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\").into_bytes()
}

/// Delete every image and placement on the screen (`a=d,d=A`), as a full redraw does before it
/// sends what is shown again.
pub const DELETE_ALL: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

/// Premultiplied BGRA in `src` becomes straight RGBA in `dst`. Lengths match
/// and are a multiple of 4. Same bytes as the old whole-frame conversion.
fn convert(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len(), dst.len());
    debug_assert_eq!(src.len() % 4, 0);
    let mut offset = 0;
    while offset < src.len() {
        let blue = src[offset];
        let green = src[offset + 1];
        let red = src[offset + 2];
        let alpha = src[offset + 3];
        dst[offset] = straight(red, alpha);
        dst[offset + 1] = straight(green, alpha);
        dst[offset + 2] = straight(blue, alpha);
        dst[offset + 3] = alpha;
        offset += 4;
    }
}

#[cfg(test)]
fn to_rgba(frame: &Frame<'_>, row_bytes: usize) -> Vec<u8> {
    let height = usize::try_from(frame.size.height).unwrap_or(0);
    let mut rgba = Vec::with_capacity(row_bytes * height);
    for row in 0..height {
        let start = row * frame.stride;
        let row_pixels = &frame.pixels[start..start + row_bytes];
        let mut offset = 0;
        while offset < row_pixels.len() {
            let blue = row_pixels[offset];
            let green = row_pixels[offset + 1];
            let red = row_pixels[offset + 2];
            let alpha = row_pixels[offset + 3];
            rgba.push(straight(red, alpha));
            rgba.push(straight(green, alpha));
            rgba.push(straight(blue, alpha));
            rgba.push(alpha);
            offset += 4;
        }
    }
    rgba
}

fn straight(channel: u8, alpha: u8) -> u8 {
    if alpha == 0 || alpha == 255 {
        return channel;
    }
    let value = (u32::from(channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha);
    u8::try_from(value.min(255)).unwrap_or(255)
}

#[cfg(test)]
fn zlib(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(bytes)
        .expect("zlib encode into a vec does not fail");
    encoder
        .finish()
        .expect("zlib finish into a vec does not fail")
}

#[cfg(test)]
fn chunks_of(b64: &str) -> Vec<&str> {
    if b64.is_empty() {
        return vec![""];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < b64.len() {
        let end = (start + CHUNK_BYTES).min(b64.len());
        chunks.push(&b64[start..end]);
        start = end;
    }
    chunks
}

fn cursor_to(column: u16, row: u16) -> Vec<u8> {
    let col = u32::from(column) + 1;
    let line = u32::from(row) + 1;
    if line == 1 && col == 1 {
        b"\x1b[s\x1b[H".to_vec()
    } else {
        format!("\x1b[s\x1b[{line};{col}H").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use flate2::read::ZlibDecoder;
    use sighurt_api::geometry::BufferSize;

    use super::*;

    fn noise(index: u32) -> u8 {
        let mut x = index.wrapping_mul(0x9E37_79B1);
        x ^= x >> 16;
        u8::try_from(x & 0xff).unwrap_or(0)
    }

    fn encode_of(width: u32, height: u32, mut pixels: Vec<u8>, stride: usize) -> Vec<u8> {
        let frame = Frame {
            pixels: &mut pixels,
            size: BufferSize { width, height },
            stride,
        };
        encode(
            &frame,
            7,
            CellRect {
                column: 2,
                row: 3,
                columns: 4,
                rows: 5,
            },
            8 * 1024 * 1024,
        )
        .expect("encode")
    }

    fn payloads(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut rest = bytes;
        while let Some(start) = rest.windows(3).position(|window| window == b"\x1b_G") {
            rest = &rest[start + 3..];
            let end = rest
                .windows(2)
                .position(|window| window == b"\x1b\\")
                .expect("APC closed");
            let message = &rest[..end];
            rest = &rest[end + 2..];
            let split = message
                .iter()
                .position(|byte| *byte == b';')
                .expect("payload");
            let control = String::from_utf8(message[..split].to_vec()).unwrap();
            let payload = STANDARD.decode(&message[split + 1..]).expect("base64");
            out.push((control, payload));
        }
        out
    }

    fn rgba_of(bytes: &[u8]) -> Vec<u8> {
        let parts = payloads(bytes);
        let mut compressed = Vec::new();
        for (_, payload) in &parts {
            compressed.extend_from_slice(payload);
        }
        let mut decoder = ZlibDecoder::new(compressed.as_slice());
        let mut rgba = Vec::new();
        decoder.read_to_end(&mut rgba).expect("zlib");
        rgba
    }

    #[test]
    fn round_trip_unpremultiplies_and_places() {
        // Half-covered red, then opaque green, with two padding bytes on the row.
        let pixels = vec![0, 0, 128, 128, 0, 255, 0, 255, 0, 0];
        let bytes = encode_of(2, 1, pixels, 10);
        assert!(bytes.starts_with(b"\x1b[s\x1b[4;3H"), "{bytes:?}");
        assert!(bytes.ends_with(b"\x1b[u"));
        let parts = payloads(&bytes);
        assert_eq!(parts.len(), 1, "a tiny frame is one chunk");
        let control = &parts[0].0;
        assert!(control.contains("a=T"), "{control}");
        assert!(control.contains("f=32"), "{control}");
        assert!(control.contains("o=z"), "{control}");
        assert!(control.contains("s=2"), "{control}");
        assert!(control.contains("v=1"), "{control}");
        assert!(control.contains("i=7"), "{control}");
        assert!(control.contains("c=4"), "{control}");
        assert!(control.contains("r=5"), "{control}");
        assert!(control.contains("C=1"), "{control}");
        assert!(control.contains("q=2"), "{control}");
        assert!(control.contains("m=0"), "{control}");
        assert!(
            !bytes.windows(4).any(|window| window == b"a=d,"),
            "{bytes:?}"
        );
        assert_eq!(rgba_of(&bytes), vec![255, 0, 0, 128, 0, 255, 0, 255]);
    }

    #[test]
    fn a_large_frame_is_chunked_and_reuses_the_id() {
        let width = 64u32;
        let height = 64u32;
        let mut pixels = Vec::new();
        for index in 0..(width * height) {
            let byte = noise(index);
            pixels.extend_from_slice(&[byte, byte.wrapping_add(3), byte.wrapping_add(7), 255]);
        }
        let bytes = encode_of(
            width,
            height,
            pixels.clone(),
            usize::try_from(width).unwrap() * 4,
        );
        let parts = payloads(&bytes);
        assert!(
            parts.len() > 1,
            "expected several chunks, got {}",
            parts.len()
        );
        assert!(parts[0].0.contains("m=1"), "{}", parts[0].0);
        assert!(
            parts.last().unwrap().0.contains("m=0"),
            "{}",
            parts.last().unwrap().0
        );
        for (index, (control, payload)) in parts.iter().enumerate() {
            assert!(control.contains("q=2"), "{control}");
            if index == 0 {
                assert!(control.contains("i=7"), "{control}");
            }
            assert!(payload.len() <= CHUNK_BYTES);
        }
        let rgba = rgba_of(&bytes);
        assert_eq!(rgba.len(), pixels.len());
        let mut expected = Vec::with_capacity(pixels.len());
        for index in 0..width * height {
            let byte = noise(index);
            expected.extend_from_slice(&[byte.wrapping_add(7), byte.wrapping_add(3), byte, 255]);
        }
        assert_eq!(rgba, expected);
        let again = encode_of(width, height, pixels, usize::try_from(width).unwrap() * 4);
        assert!(payloads(&again)[0].0.contains("i=7"));
        assert!(!again.windows(4).any(|window| window == b"a=d,"));
    }

    #[test]
    fn delete_names_the_image_and_oversize_is_refused() {
        assert_eq!(delete(7), b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
        let mut pixels = vec![0, 0, 255, 255];
        let frame = Frame {
            pixels: &mut pixels,
            size: BufferSize {
                width: 1,
                height: 1,
            },
            stride: 4,
        };
        let err = encode(
            &frame,
            7,
            CellRect {
                column: 0,
                row: 0,
                columns: 1,
                rows: 1,
            },
            8,
        )
        .expect_err("cap");
        assert!(matches!(err, Error::TooLarge { .. }), "{err}");
        assert!(matches!(
            encode(
                &frame,
                0,
                CellRect {
                    column: 0,
                    row: 0,
                    columns: 1,
                    rows: 1
                },
                1000
            ),
            Err(Error::ImageId)
        ));
    }

    /// The encoder this file used to run: whole RGBA, whole zlib, then base64.
    fn eager_encode(frame: &Frame<'_>, image_id: u32, place: CellRect) -> Vec<u8> {
        let width = frame.size.width;
        let height = frame.size.height;
        let row_bytes = usize::try_from(width).unwrap() * 4;
        let rgba = to_rgba(frame, row_bytes);
        let compressed = zlib(&rgba);
        let b64 = STANDARD.encode(compressed);
        let chunks = chunks_of(&b64);
        let mut out = cursor_to(place.column, place.row);
        for (index, chunk) in chunks.iter().enumerate() {
            let last = index + 1 == chunks.len();
            let more = if last { "0" } else { "1" };
            let control = if index == 0 {
                format!(
                    "a=T,f=32,o=z,t=d,s={width},v={height},i={image_id},c={},r={},C=1,q=2,m={more}",
                    place.columns, place.rows
                )
            } else {
                format!("q=2,m={more}")
            };
            out.extend_from_slice(b"\x1b_G");
            out.extend_from_slice(control.as_bytes());
            out.push(b';');
            out.extend_from_slice(chunk.as_bytes());
            out.extend_from_slice(b"\x1b\\");
        }
        out.extend_from_slice(b"\x1b[u");
        out
    }

    fn frame_of(pixels: &mut [u8], width: u32, height: u32) -> Frame<'_> {
        let stride = usize::try_from(width).unwrap() * 4;
        Frame {
            pixels,
            size: BufferSize { width, height },
            stride,
        }
    }

    #[test]
    fn streaming_matches_the_eager_encoder() {
        let place = CellRect {
            column: 2,
            row: 3,
            columns: 4,
            rows: 5,
        };
        let mut tiny = vec![0, 0, 128, 128, 0, 255, 0, 255, 9, 9];
        let tiny_frame = Frame {
            pixels: &mut tiny,
            size: BufferSize {
                width: 2,
                height: 1,
            },
            stride: 10,
        };
        let streamed = encode(&tiny_frame, 7, place, 8 * 1024 * 1024).expect("tiny");
        assert_eq!(streamed, eager_encode(&tiny_frame, 7, place));

        let mut wide = Vec::new();
        for index in 0..180u32 {
            let byte = noise(index);
            wide.extend_from_slice(&[byte, byte.wrapping_add(1), byte.wrapping_add(2), 200]);
        }
        let wide_frame = frame_of(&mut wide, 180, 1);
        let once = encode(&wide_frame, 3, place, 8 * 1024 * 1024).expect("wide");
        assert_eq!(once, eager_encode(&wide_frame, 3, place));
        let twice = encode(&wide_frame, 3, place, 8 * 1024 * 1024).expect("reused");
        assert_eq!(twice, once);

        let mut pixels = Vec::new();
        for index in 0..(64 * 64) {
            let byte = noise(index);
            pixels.extend_from_slice(&[byte, byte.wrapping_add(3), byte.wrapping_add(7), 255]);
        }
        let big = frame_of(&mut pixels, 64, 64);
        let streamed = encode(&big, 7, place, 8 * 1024 * 1024).expect("big");
        assert_eq!(streamed, eager_encode(&big, 7, place));
    }

    #[test]
    fn an_over_cap_frame_stops_before_the_image_is_built() {
        const WIDTH: u32 = 2000;
        const HEIGHT: u32 = 4000;
        let mut pixels = Vec::with_capacity(8_000_000 * 4);
        let mut state = 0x1234_5678u32;
        for _ in 0..8_000_000u32 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let byte = u8::try_from(state >> 24).unwrap_or(0);
            pixels.extend_from_slice(&[byte, byte.wrapping_add(1), byte.wrapping_add(2), 255]);
        }
        let frame = frame_of(&mut pixels, WIDTH, HEIGHT);
        let place = CellRect {
            column: 0,
            row: 0,
            columns: 1,
            rows: 1,
        };

        let eager_start = std::time::Instant::now();
        let rgba = to_rgba(&frame, usize::try_from(WIDTH).unwrap() * 4);
        let compressed = zlib(&rgba);
        let b64 = STANDARD.encode(&compressed);
        let eager_elapsed = eager_start.elapsed();
        let transient = rgba.capacity() + compressed.capacity() + b64.capacity();
        assert!(
            transient > 32 * 1024 * 1024,
            "eager transient {transient} in {eager_elapsed:?}"
        );

        let mut encoder = Encoder::new();
        let streamed_start = std::time::Instant::now();
        let err = encoder
            .encode(&frame, 7, place, 64 * 1024)
            .expect_err("cap");
        let streamed_elapsed = streamed_start.elapsed();
        let scratch = encoder.scratch_bytes();
        assert!(matches!(err, Error::TooLarge { .. }), "{err}");
        assert!(
            scratch < 512 * 1024,
            "scratch {scratch} after {streamed_elapsed:?}; eager kept {transient} for {eager_elapsed:?}"
        );
        assert!(
            streamed_elapsed < std::time::Duration::from_millis(500),
            "streamed {streamed_elapsed:?}, scratch {scratch}, eager {transient} in {eager_elapsed:?}"
        );
        eprintln!(
            "kitty over-cap 8MP: eager transient {transient} bytes in {eager_elapsed:?}; streamed scratch {scratch} bytes in {streamed_elapsed:?}"
        );

        let mut one = vec![0, 0, 255, 255];
        let again = encoder
            .encode(&frame_of(&mut one, 1, 1), 7, place, 1024 * 1024)
            .expect("encoder recovers");
        assert!(again.ends_with(b"\x1b[u"));
    }

    /// Whole-buffer base64, cut every [`CHUNK_BYTES`], framed like [`Sink::emit`].
    fn eager_compressed(data: &[u8]) -> Vec<u8> {
        let b64 = STANDARD.encode(data);
        let chunks = chunks_of(&b64);
        let mut out = cursor_to(0, 0);
        for (index, chunk) in chunks.iter().enumerate() {
            let last = index + 1 == chunks.len();
            let more = if last { "0" } else { "1" };
            let control = if index == 0 {
                format!("a=T,f=32,o=z,t=d,s=1,v=1,i=1,c=1,r=1,C=1,q=2,m={more}")
            } else {
                format!("q=2,m={more}")
            };
            out.extend_from_slice(b"\x1b_G");
            out.extend_from_slice(control.as_bytes());
            out.push(b';');
            out.extend_from_slice(chunk.as_bytes());
            out.extend_from_slice(b"\x1b\\");
        }
        out.extend_from_slice(b"\x1b[u");
        out
    }

    fn sink_encode(data: &[u8], splits: &[usize]) -> Vec<u8> {
        let mut sink = Sink::new();
        sink.begin(
            1,
            1,
            1,
            CellRect {
                column: 0,
                row: 0,
                columns: 1,
                rows: 1,
            },
            usize::MAX,
        );
        let mut at = 0;
        for &split in splits {
            let end = split.min(data.len());
            if end > at {
                sink.write_all(&data[at..end]).expect("fragment");
                at = end;
            }
        }
        if at < data.len() {
            sink.write_all(&data[at..]).expect("tail");
        }
        sink.finish_apc();
        assert!(!sink.over);
        sink.out
    }

    fn assert_payloads_fit(bytes: &[u8]) {
        let mut rest = bytes;
        while let Some(start) = rest.windows(3).position(|window| window == b"\x1b_G") {
            rest = &rest[start + 3..];
            let end = rest
                .windows(2)
                .position(|window| window == b"\x1b\\")
                .expect("APC closed");
            let message = &rest[..end];
            rest = &rest[end + 2..];
            let split = message
                .iter()
                .position(|byte| *byte == b';')
                .expect("payload");
            assert!(
                message.len() - split - 1 <= CHUNK_BYTES,
                "payload {} exceeds {CHUNK_BYTES}",
                message.len() - split - 1
            );
        }
    }

    #[test]
    fn fragmented_writes_match_eager_chunking() {
        let mut data = Vec::with_capacity(8192);
        for index in 0..8192u32 {
            data.push(noise(index));
        }
        let sizes = [
            0, 1, 2, 3, 4, 32, 48, 3068, 3070, 3071, 3072, 3073, 3074, 4095, 6144, 8192,
        ];
        for &size in &sizes {
            let bytes = &data[..size];
            let eager = eager_compressed(bytes);
            let mut splits: Vec<Vec<usize>> = vec![Vec::new(), vec![1], vec![2]];
            if size > 3071 {
                splits.push(vec![3071]);
                splits.push(vec![3071, 3073.min(size)]);
            }
            if size <= 48 {
                for split in 1..size {
                    splits.push(vec![split]);
                }
            } else {
                let mut split = 1;
                while split < size {
                    splits.push(vec![split]);
                    let near = split.saturating_add(3071).min(size.saturating_sub(1));
                    if near > 0 && near < size {
                        splits.push(vec![split, near]);
                    }
                    split += 64;
                }
                for edge in 3068..3076 {
                    if edge < size {
                        splits.push(vec![edge]);
                    }
                }
            }
            for split in &splits {
                let got = sink_encode(bytes, split);
                assert_payloads_fit(&got);
                assert_eq!(got, eager, "size {size} splits {split:?}");
            }
        }
    }
}
