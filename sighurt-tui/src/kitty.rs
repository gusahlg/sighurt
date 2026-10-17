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

use std::fmt;
use std::io::Write;

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
/// whole, so the writer never sends a partial image.
pub fn encode(
    frame: &Frame<'_>,
    image_id: u32,
    place: CellRect,
    max_bytes: usize,
) -> Result<Vec<u8>, Error> {
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
    if out.len() > max_bytes {
        return Err(Error::TooLarge {
            bytes: out.len(),
            max: max_bytes,
        });
    }
    Ok(out)
}

/// Delete the image `image_id` and its placements (`a=d,d=I`). `q=2` suppresses the reply.
pub fn delete(image_id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\").into_bytes()
}

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

fn zlib(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(bytes)
        .expect("zlib encode into a vec does not fail");
    encoder
        .finish()
        .expect("zlib finish into a vec does not fail")
}

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
    use std::io::Read;

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
}
