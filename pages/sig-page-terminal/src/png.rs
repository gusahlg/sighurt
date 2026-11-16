//! Turns raw pixels into an uncompressed PNG a step at a time, and reads a PNG's size.
//!
//! It owns the PNG container: the chunks, their CRC-32, the zlib stream of stored (not
//! compressed) deflate blocks and its Adler-32, all written in bounded steps so that a large image
//! costs a few frames and not one. Compression would cost too much of a frame's fuel; the host
//! decodes what this makes. It must not own decoding PNGs, drawing, or deciding what sizes and
//! limits an image may have: the caller does that before and while it calls this.

/// The longest stored deflate block.
const BLOCK_MAX: usize = 65535;
/// An IDAT chunk is closed once it holds this many bytes.
const CHUNK_LIMIT: usize = 1 << 20;
/// The PNG signature.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// How the pixels of [`Encoder::new`]'s data are laid out.
pub enum Pixels {
    /// 3 bytes a pixel.
    Rgb,
    /// 4 bytes a pixel, straight (not premultiplied) alpha.
    Rgba,
    /// 1 byte a pixel, an index into `palette` (1 to 256 RGBA entries; entries with alpha below 255
    /// go into a tRNS chunk, trailing opaque ones omitted from it).
    Indexed {
        /// The colours the indices name.
        palette: Vec<[u8; 4]>,
    },
}

/// Builds a PNG from raw pixels over several calls to [`Encoder::step`].
pub struct Encoder {
    /// The raw pixels, released when the last row is copied.
    data: Vec<u8>,
    /// Bytes in a row of `data`.
    row_len: usize,
    /// Rows in the image.
    rows: usize,
    /// Rows copied so far.
    row: usize,
    /// Bytes of the zlib payload (rows with their filter bytes) not yet written.
    stream_left: usize,
    /// Bytes left in the stored block being written (0: the next byte starts a block).
    block_left: usize,
    /// Whether the zlib header has been written.
    header_done: bool,
    /// Running Adler-32 `a` and `b` of the payload.
    adler: (u32, u32),
    /// The PNG so far.
    out: Vec<u8>,
    /// Whether an IDAT chunk is open at the end of `out`.
    chunk_open: bool,
    /// Offset in `out` of the open chunk's length field.
    chunk_start: usize,
    /// Running (not inverted) CRC-32 of the open chunk, up to `crc_pos` in `out`.
    crc: u32,
    /// Offset in `out` up to which `crc` has been updated.
    crc_pos: usize,
    /// Whether the PNG has been returned.
    done: bool,
}

impl Encoder {
    /// `None` when width or height is 0, a palette is empty or longer than 256, or `data.len()` is
    /// not width × height × bytes per pixel (or memory for the result cannot be had).
    pub fn new(width: u32, height: u32, pixels: Pixels, data: Vec<u8>) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let (bpp, colour, palette) = match &pixels {
            Pixels::Rgb => (3usize, 2u8, None),
            Pixels::Rgba => (4, 6, None),
            Pixels::Indexed { palette } => {
                if palette.is_empty() || palette.len() > 256 {
                    return None;
                }
                (1, 3, Some(palette))
            }
        };
        let row_len = usize::try_from(width).ok()?.checked_mul(bpp)?;
        let rows = usize::try_from(height).ok()?;
        if row_len.checked_mul(rows)? != data.len() {
            return None;
        }
        let stream = data.len().checked_add(rows)?;
        let estimate = stream
            .checked_add((stream / BLOCK_MAX + 1).checked_mul(5)?)?
            .checked_add((stream / CHUNK_LIMIT + 1).checked_mul(12)?)?
            .checked_add(2048)?;
        let mut out = Vec::new();
        out.try_reserve_exact(estimate).ok()?;
        out.extend_from_slice(&SIGNATURE);
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, colour, 0, 0, 0]);
        write_chunk(&mut out, b"IHDR", &ihdr);
        if let Some(palette) = palette {
            let plte: Vec<u8> = palette.iter().flat_map(|c| [c[0], c[1], c[2]]).collect();
            write_chunk(&mut out, b"PLTE", &plte);
            if let Some(last) = palette.iter().rposition(|c| c[3] != 255) {
                let trns: Vec<u8> = palette[..=last].iter().map(|c| c[3]).collect();
                write_chunk(&mut out, b"tRNS", &trns);
            }
        }
        Some(Self {
            data,
            row_len,
            rows,
            row: 0,
            stream_left: stream,
            block_left: 0,
            header_done: false,
            adler: (1, 0),
            out,
            chunk_open: false,
            chunk_start: 0,
            crc: 0,
            crc_pos: 0,
            done: false,
        })
    }

    /// Does about `budget` bytes of work (at least one row per call even if the budget is smaller,
    /// so it always progresses) and returns the PNG once it is complete. After it returned `Some`,
    /// later calls return `None`.
    pub fn step(&mut self, budget: usize) -> Option<Vec<u8>> {
        if self.done {
            return None;
        }
        if !self.header_done {
            self.header_done = true;
            self.write(&[0x78, 0x01]);
        }
        let mut work = 0usize;
        while self.row < self.rows && (work == 0 || work < budget) {
            let data = std::mem::take(&mut self.data);
            let start = self.row * self.row_len;
            self.payload(&[0]);
            self.payload(&data[start..start + self.row_len]);
            self.data = data;
            self.row += 1;
            work = work.saturating_add(self.row_len + 1);
            self.sync_crc();
        }
        if self.row < self.rows {
            return None;
        }
        self.data = Vec::new();
        let (a, b) = self.adler;
        self.write(&((b << 16) | a).to_be_bytes());
        self.close_chunk();
        write_chunk(&mut self.out, b"IEND", &[]);
        self.done = true;
        Some(std::mem::take(&mut self.out))
    }

    /// Bytes of memory it holds now (input not yet released plus output so far), for the caller's
    /// accounting.
    pub fn held(&self) -> usize {
        self.data.len() + self.out.len()
    }

    /// Adds `bytes` to the zlib payload: checksums them and cuts them into stored blocks.
    fn payload(&mut self, mut bytes: &[u8]) {
        self.adler = adler_update(self.adler, bytes);
        while !bytes.is_empty() && self.stream_left > 0 {
            if self.block_left == 0 {
                let len = self.stream_left.min(BLOCK_MAX);
                let last = u8::from(self.stream_left <= BLOCK_MAX);
                let len16 = len as u16;
                let mut head = [last, 0, 0, 0, 0];
                head[1..3].copy_from_slice(&len16.to_le_bytes());
                head[3..5].copy_from_slice(&(!len16).to_le_bytes());
                self.write(&head);
                self.block_left = len;
            }
            let n = self.block_left.min(bytes.len());
            self.write(&bytes[..n]);
            self.block_left -= n;
            self.stream_left -= n;
            bytes = &bytes[n..];
        }
    }

    /// Appends to the open IDAT chunk (opening one if needed), closing it when it is full.
    fn write(&mut self, bytes: &[u8]) {
        if !self.chunk_open {
            self.chunk_start = self.out.len();
            self.out.extend_from_slice(&[0; 4]);
            self.out.extend_from_slice(b"IDAT");
            self.crc_pos = self.out.len() - 4;
            self.crc = !0;
            self.chunk_open = true;
        }
        self.out.extend_from_slice(bytes);
        if self.out.len() - self.chunk_start - 8 >= CHUNK_LIMIT {
            self.close_chunk();
        }
    }

    /// Brings the open chunk's running CRC up to the end of `out`.
    fn sync_crc(&mut self) {
        if self.chunk_open {
            self.crc = crc_update(self.crc, &self.out[self.crc_pos..]);
            self.crc_pos = self.out.len();
        }
    }

    /// Writes the open chunk's length and CRC.
    fn close_chunk(&mut self) {
        if !self.chunk_open {
            return;
        }
        self.sync_crc();
        let len = (self.out.len() - self.chunk_start - 8) as u32;
        self.out[self.chunk_start..self.chunk_start + 4].copy_from_slice(&len.to_be_bytes());
        self.out.extend_from_slice(&(!self.crc).to_be_bytes());
        self.chunk_open = false;
    }
}

/// The width and height a PNG's IHDR declares: `None` unless `png` starts with the PNG signature
/// and its first chunk is a complete IHDR (13 bytes of data) with width and height both non-zero.
pub fn dimensions(png: &[u8]) -> Option<(u32, u32)> {
    if png.get(..8)? != SIGNATURE {
        return None;
    }
    if png.get(8..12)? != 13u32.to_be_bytes() || png.get(12..16)? != b"IHDR" {
        return None;
    }
    let data = png.get(16..29)?;
    let width = u32::from_be_bytes(data[0..4].try_into().ok()?);
    let height = u32::from_be_bytes(data[4..8].try_into().ok()?);
    (width != 0 && height != 0).then_some((width, height))
}

/// Appends a whole chunk: length, type, data and CRC.
fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = !crc_update(crc_update(!0, kind), data);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// The CRC-32 slice-by-8 tables.
static CRC_TABLES: [[u32; 256]; 8] = crc_tables();

/// Computes the CRC-32 (IEEE 802.3, reflected, polynomial 0xEDB88320) tables.
const fn crc_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut n = 1;
    while n < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = t[n - 1][i];
            t[n][i] = (prev >> 8) ^ t[0][(prev & 0xFF) as usize];
            i += 1;
        }
        n += 1;
    }
    t
}

/// Feeds `bytes` to a running CRC-32 (start with `!0`, finish with `!`).
fn crc_update(mut crc: u32, bytes: &[u8]) -> u32 {
    let t = &CRC_TABLES;
    let (blocks, rest) = bytes.as_chunks::<8>();
    for b in blocks {
        let lo = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) ^ crc;
        let hi = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        crc = t[7][(lo & 0xFF) as usize]
            ^ t[6][((lo >> 8) & 0xFF) as usize]
            ^ t[5][((lo >> 16) & 0xFF) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][(hi & 0xFF) as usize]
            ^ t[2][((hi >> 8) & 0xFF) as usize]
            ^ t[1][((hi >> 16) & 0xFF) as usize]
            ^ t[0][(hi >> 24) as usize];
    }
    for &b in rest {
        crc = (crc >> 8) ^ t[0][((crc ^ u32::from(b)) & 0xFF) as usize];
    }
    crc
}

/// Feeds `bytes` to a running Adler-32 `(a, b)` (start with `(1, 0)`), reducing modulo 65521 once
/// per 5552 bytes.
fn adler_update((mut a, mut b): (u32, u32), bytes: &[u8]) -> (u32, u32) {
    for part in bytes.chunks(5552) {
        for &x in part {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes with palettes and tRNS expanded; returns the colour type and the bytes.
    fn decode(png: &[u8]) -> (png::ColorType, u32, u32, Vec<u8>) {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().expect("valid header");
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("valid image");
        buf.truncate(info.buffer_size());
        (info.color_type, info.width, info.height, buf)
    }

    fn run(enc: &mut Encoder, budget: usize) -> Vec<u8> {
        loop {
            if let Some(png) = enc.step(budget) {
                return png;
            }
        }
    }

    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + i / 13) as u8).collect()
    }

    #[test]
    fn crc_known_value() {
        assert_eq!(!crc_update(!0, b"123456789"), 0xCBF4_3926);
        assert_eq!(!crc_update(!0, b""), 0);
    }

    #[test]
    fn adler_known_value() {
        let (a, b) = adler_update((1, 0), b"Wikipedia");
        assert_eq!((b << 16) | a, 0x11E6_0398);
        let big = vec![255u8; 100_000];
        let (a, b) = adler_update((1, 0), &big);
        let (mut ea, mut eb) = (1u64, 0u64);
        for &x in &big {
            ea = (ea + u64::from(x)) % 65521;
            eb = (eb + ea) % 65521;
        }
        assert_eq!((u64::from(a), u64::from(b)), (ea, eb));
    }

    #[test]
    fn rgb_1x1() {
        let mut e = Encoder::new(1, 1, Pixels::Rgb, vec![10, 20, 30]).unwrap();
        let (ct, w, h, px) = decode(&run(&mut e, usize::MAX));
        assert_eq!((ct, w, h), (png::ColorType::Rgb, 1, 1));
        assert_eq!(px, [10, 20, 30]);
    }

    #[test]
    fn rgba_3x2() {
        let data = pattern(24);
        let mut e = Encoder::new(3, 2, Pixels::Rgba, data.clone()).unwrap();
        let (ct, w, h, px) = decode(&run(&mut e, 5));
        assert_eq!((ct, w, h), (png::ColorType::Rgba, 3, 2));
        assert_eq!(px, data);
    }

    #[test]
    fn indexed_4x3_with_transparency() {
        let palette = vec![
            [1, 2, 3, 0],
            [4, 5, 6, 128],
            [7, 8, 9, 255],
            [10, 11, 12, 255],
        ];
        let data: Vec<u8> = (0..12).map(|i| (i % 4) as u8).collect();
        let mut e = Encoder::new(
            4,
            3,
            Pixels::Indexed {
                palette: palette.clone(),
            },
            data.clone(),
        )
        .unwrap();
        let (ct, _, _, px) = decode(&run(&mut e, usize::MAX));
        assert_eq!(ct, png::ColorType::Rgba);
        let want: Vec<u8> = data.iter().flat_map(|&i| palette[i as usize]).collect();
        assert_eq!(px, want);
    }

    #[test]
    fn indexed_opaque_has_no_trns() {
        let palette = vec![[1, 2, 3, 255], [4, 5, 6, 255]];
        let mut e = Encoder::new(2, 1, Pixels::Indexed { palette }, vec![1, 0]).unwrap();
        let png = run(&mut e, usize::MAX);
        assert!(!png.windows(4).any(|w| w == b"tRNS"));
        let (ct, _, _, px) = decode(&png);
        assert_eq!(ct, png::ColorType::Rgb);
        assert_eq!(px, [4, 5, 6, 1, 2, 3]);
    }

    #[test]
    fn rgba_300x300_crosses_blocks_and_chunks() {
        let data = pattern(300 * 300 * 4);
        let mut e = Encoder::new(300, 300, Pixels::Rgba, data.clone()).unwrap();
        let png = run(&mut e, usize::MAX);
        let (_, w, h, px) = decode(&png);
        assert_eq!((w, h), (300, 300));
        assert_eq!(px, data);
    }

    #[test]
    fn many_idat_chunks() {
        let (w, h) = (1000u32, 1200u32);
        let data = pattern(w as usize * h as usize * 4);
        let mut e = Encoder::new(w, h, Pixels::Rgba, data.clone()).unwrap();
        let png = run(&mut e, 1 << 20);
        assert!(png.windows(4).filter(|x| *x == b"IDAT").count() >= 4);
        assert_eq!(decode(&png).3, data);
    }

    #[test]
    fn budgets_give_identical_output() {
        let data = pattern(300 * 300 * 3);
        let make = || Encoder::new(300, 300, Pixels::Rgb, data.clone()).unwrap();
        let whole = run(&mut make(), usize::MAX);
        for budget in [1, 7, 4096] {
            assert_eq!(run(&mut make(), budget), whole, "budget {budget}");
        }
    }

    #[test]
    fn step_after_completion_and_held() {
        let data = pattern(50 * 40 * 4);
        let mut e = Encoder::new(50, 40, Pixels::Rgba, data).unwrap();
        let start = e.held();
        assert!(start >= 8000);
        assert!(e.step(1).is_none());
        assert!(e.held() > start - 8000);
        let png = run(&mut e, 100);
        assert!(!png.is_empty());
        assert_eq!(e.held(), 0);
        assert!(e.step(usize::MAX).is_none());
        assert!(e.step(1).is_none());
    }

    #[test]
    fn new_refuses_bad_input() {
        assert!(Encoder::new(0, 1, Pixels::Rgb, vec![]).is_none());
        assert!(Encoder::new(1, 0, Pixels::Rgb, vec![]).is_none());
        assert!(Encoder::new(2, 1, Pixels::Rgb, vec![0; 5]).is_none());
        assert!(Encoder::new(2, 1, Pixels::Rgba, vec![0; 9]).is_none());
        assert!(Encoder::new(1, 1, Pixels::Indexed { palette: vec![] }, vec![0]).is_none());
        let big = Pixels::Indexed {
            palette: vec![[0; 4]; 257],
        };
        assert!(Encoder::new(1, 1, big, vec![0]).is_none());
        let full = Pixels::Indexed {
            palette: vec![[0; 4]; 256],
        };
        assert!(Encoder::new(1, 1, full, vec![0]).is_some());
        assert!(Encoder::new(u32::MAX, u32::MAX, Pixels::Rgba, vec![0; 4]).is_none());
        assert!(Encoder::new(
            u32::MAX,
            u32::MAX,
            Pixels::Indexed {
                palette: vec![[0; 4]]
            },
            vec![]
        )
        .is_none());
    }

    #[test]
    fn dimensions_reads_and_refuses() {
        let mut e = Encoder::new(5, 7, Pixels::Rgb, vec![0; 105]).unwrap();
        let png = run(&mut e, usize::MAX);
        assert_eq!(dimensions(&png), Some((5, 7)));
        let mut bad = png.clone();
        bad[1] = b'X';
        assert_eq!(dimensions(&bad), None);
        assert_eq!(dimensions(&png[..28]), None);
        assert_eq!(dimensions(&png[..29]), Some((5, 7)));
        assert_eq!(dimensions(&[]), None);
        let mut zero_w = png.clone();
        zero_w[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(dimensions(&zero_w), None);
        let mut zero_h = png.clone();
        zero_h[20..24].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(dimensions(&zero_h), None);
        let mut short_len = png;
        short_len[8..12].copy_from_slice(&12u32.to_be_bytes());
        assert_eq!(dimensions(&short_len), None);
    }
}
