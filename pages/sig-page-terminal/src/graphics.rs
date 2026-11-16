//! Images a program shows in the terminal: the kitty graphics protocol and sixel, as far as
//! they can be done well here, with every byte the program sends treated as untrusted.
//!
//! What is understood (kitty's <https://sw.kovidgoyal.net/kitty/graphics-protocol/>):
//! transmission directly in the escape code (`t=d`, chunked with `m=1`) of 24-bit RGB (`f=24`),
//! 32-bit RGBA (`f=32`) or PNG (`f=100`) data; transmit (`a=t`), transmit and show (`a=T`), show
//! one already sent (`a=p`), the query (`a=q`) and deletes (`a=d` by all, id, number, cursor,
//! cell, column, row, z-index and id range, lower case keeping the image and upper case freeing
//! it). A placement takes its size in cells (`c`, `r`) or from the image, a part of the image
//! (`x`, `y`, `w`, `h`), an offset in its first cell (`X`, `Y`), a z-index (`z`: below the text
//! when negative) and an id (`p`); it moves the cursor past itself unless `C=1`. Answers follow
//! the protocol (`i`, `I` and `p` say which; `q` quiets them). Refused with an error: files,
//! temporary files and shared memory (`t=f`, `t=t`, `t=s`; the page reads no file), compressed
//! data (`o=z`), animation (`a=f`, `a=a`, `a=c`) and Unicode placeholders (`U=1`). A sixel
//! string (`DCS q`) becomes an image placed at the cursor, the cursor then at the start of the
//! line below it.
//!
//! Where images live: each is kept as a PNG, since the canvas takes encoded images and decodes
//! them in the host, the same decoder (and limits) every Wasm app's images go through. A PNG the
//! program sent is kept as it came, after its header was read for its size; raw pixels and
//! sixel's palette indices are made into an uncompressed PNG ([`crate::png`]) a slice per frame,
//! so a large image never takes more than a frame's share of fuel. Bounds: [`MAX_SIDE`] pixels a
//! side and [`MAX_PIXELS`] an image, [`MAX_IMAGE_BYTES`] of data each, [`MAX_STORED`] bytes and
//! [`MAX_IMAGES`] images in all, the least recently used going first when more arrive (as kitty
//! does), and [`MAX_PLACEMENTS`] placements, the oldest going first.
//!
//! Where placements are: at a line of the screen's content, so they scroll with the text into
//! the history and leave with it ([`Graphics::scrolled`]); the alternate screen's are its own
//! and go when it is left. Erasing the whole display removes those on it.
//!
//! Owns: images, placements and the protocol's answers. Must not own: finding the commands in
//! the stream ([`crate::scan`]), the cursor and the screen ([`crate::term`]), or drawing
//! ([`crate::draw`]).

use crate::png::{self, Encoder, Pixels};
use crate::sixel;

/// Longest side of an image, in pixels.
pub const MAX_SIDE: u32 = 4096;
/// Most pixels of one image (what the canvas's decoder takes).
pub const MAX_PIXELS: u64 = 4096 * 4096;
/// Most bytes of one image's data, as sent (PNG) or as pixels (raw).
pub const MAX_IMAGE_BYTES: usize = 24 << 20;
/// Most bytes of all images together.
pub const MAX_STORED: usize = 64 << 20;
/// Most images kept.
pub const MAX_IMAGES: usize = 256;
/// Most placements kept.
pub const MAX_PLACEMENTS: usize = 1024;
/// Bytes of encoding work [`Graphics::step`] does at most per call.
pub const STEP_BYTES: usize = 2 << 20;

/// The screen as a graphics command finds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Context {
    /// The cursor: row and column.
    pub cursor: (u16, u16),
    /// Rows and columns.
    pub size: (u16, u16),
    pub alt: bool,
    /// Lines the main screen has scrolled into its history: its first row's line.
    pub scrolled: i64,
    /// A cell's width and height, in canvas pixels.
    pub cell: (f32, f32),
}

impl Context {
    /// The line of `row` on this screen.
    fn line(&self, row: u16) -> i64 {
        i64::from(row) + if self.alt { 0 } else { self.scrolled }
    }
}

/// What the terminal does after a command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The answer to send the program.
    pub reply: Vec<u8>,
    /// Move the cursor past an image placed at it.
    pub advance: Option<Advance>,
}

/// How the cursor moves past an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advance {
    /// Kitty's: down to the image's last row and right past its last column.
    Kitty { cols: u16, rows: u16 },
    /// Sixel's: to the start of the line below the image.
    Below { rows: u16 },
}

/// An image placed on the screen.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    /// The image ([`Graphics::png`]).
    pub image: u64,
    /// The program's id for it (0 none).
    id: u32,
    /// On the alternate screen.
    pub alt: bool,
    /// The line of its first row ([`Context::line`]) and its column.
    pub line: i64,
    pub col: u16,
    /// Cells it covers.
    pub cols: u16,
    pub rows: u16,
    /// The part of the image shown, in its pixels: x, y, width, height.
    pub source: [u32; 4],
    /// Where in its first cell it starts, in canvas pixels.
    pub offset: (f32, f32),
    /// Drawn at this size in canvas pixels (`None`: the source's own size).
    pub size: Option<(f32, f32)>,
    /// Below the text when negative.
    pub z: i32,
}

enum Data {
    /// Being made into a PNG.
    Building(Box<Encoder>),
    Png(Vec<u8>),
}

struct Image {
    key: u64,
    /// The program's id (0 none) and number (0 none).
    id: u32,
    number: u32,
    width: u32,
    height: u32,
    data: Data,
    /// When it was last sent or placed, for eviction.
    used: u64,
}

impl Image {
    fn bytes(&self) -> usize {
        match &self.data {
            Data::Building(encoder) => encoder.held(),
            Data::Png(png) => png.len(),
        }
    }
}

/// A kitty command's keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Command {
    action: u8,
    quiet: u8,
    format: u32,
    medium: u8,
    compression: u8,
    width: u32,
    height: u32,
    id: u32,
    number: u32,
    placement: u32,
    more: bool,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    offset_x: u32,
    offset_y: u32,
    cols: u32,
    rows: u32,
    no_move: bool,
    z: i32,
    delete: u8,
    unicode: bool,
    /// Keys other than `m` and `q` were given: not a continuation.
    keyed: bool,
}

impl Command {
    fn parse(control: &[u8]) -> Self {
        let mut cmd = Command {
            action: b't',
            format: 32,
            medium: b'd',
            delete: b'a',
            ..Command::default()
        };
        for pair in control.split(|b| *b == b',') {
            let Some((&key, value)) = pair.split_first() else {
                continue;
            };
            let Some(value) = value.strip_prefix(b"=") else {
                continue;
            };
            let char_value = value.first().copied().unwrap_or(0);
            let number = parse_number(value);
            let unsigned = number.clamp(0, i64::from(u32::MAX)) as u32;
            if !matches!(key, b'm' | b'q') {
                cmd.keyed = true;
            }
            match key {
                b'a' => cmd.action = char_value,
                b'q' => cmd.quiet = unsigned.min(2) as u8,
                b'f' => cmd.format = unsigned,
                b't' => cmd.medium = char_value,
                b'o' => cmd.compression = char_value,
                b's' => cmd.width = unsigned,
                b'v' => cmd.height = unsigned,
                b'i' => cmd.id = unsigned,
                b'I' => cmd.number = unsigned,
                b'p' => cmd.placement = unsigned,
                b'm' => cmd.more = unsigned == 1,
                b'x' => cmd.x = unsigned,
                b'y' => cmd.y = unsigned,
                b'w' => cmd.w = unsigned,
                b'h' => cmd.h = unsigned,
                b'X' => cmd.offset_x = unsigned,
                b'Y' => cmd.offset_y = unsigned,
                b'c' => cmd.cols = unsigned,
                b'r' => cmd.rows = unsigned,
                b'C' => cmd.no_move = unsigned == 1,
                b'z' => cmd.z = number.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                b'd' => cmd.delete = char_value,
                b'U' => cmd.unicode = unsigned == 1,
                _ => {}
            }
        }
        cmd
    }
}

/// A decimal integer, saturating; 0 for anything else.
fn parse_number(value: &[u8]) -> i64 {
    let (negative, digits) = match value.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, value),
    };
    let mut n: i64 = 0;
    for &d in digits {
        if !d.is_ascii_digit() {
            return 0;
        }
        n = n.saturating_mul(10).saturating_add(i64::from(d - b'0'));
    }
    if negative {
        -n
    } else {
        n
    }
}

/// Decodes base64 as it streams in, into a bounded buffer.
#[derive(Default)]
struct Base64 {
    bits: u32,
    count: u8,
}

impl Base64 {
    /// Decodes `text` onto `out`, up to `cap` bytes in all; `Err` names what went wrong.
    fn decode(&mut self, text: &[u8], out: &mut Vec<u8>, cap: usize) -> Result<(), &'static str> {
        for &c in text {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => {
                    // Padding ends the quartet: its whole bytes are kept.
                    self.finish(out, cap)?;
                    continue;
                }
                b' ' | b'\n' | b'\r' | b'\t' => continue,
                _ => return Err("EINVAL:the payload is not base64"),
            };
            self.bits = (self.bits << 6) | u32::from(v);
            self.count += 1;
            if self.count == 4 {
                if out.len() + 3 > cap {
                    return Err("EFBIG:the image is too large");
                }
                out.extend_from_slice(&self.bits.to_be_bytes()[1..]);
                self.bits = 0;
                self.count = 0;
            }
        }
        Ok(())
    }

    /// Ends the stream: the bytes of a last, unpadded quartet.
    fn finish(&mut self, out: &mut Vec<u8>, cap: usize) -> Result<(), &'static str> {
        let bytes = match self.count {
            2 => vec![(self.bits >> 4) as u8],
            3 => {
                let v = self.bits >> 2;
                vec![(v >> 8) as u8, v as u8]
            }
            _ => Vec::new(),
        };
        self.bits = 0;
        self.count = 0;
        if out.len() + bytes.len() > cap {
            return Err("EFBIG:the image is too large");
        }
        out.extend_from_slice(&bytes);
        Ok(())
    }
}

/// A kitty transmission under way, perhaps over several chunks.
struct Transfer {
    cmd: Command,
    data: Vec<u8>,
    cap: usize,
    base64: Base64,
    error: Option<&'static str>,
}

/// The images and placements of one terminal.
#[derive(Default)]
pub struct Graphics {
    images: Vec<Image>,
    placements: Vec<Placement>,
    /// The kitty command being read: its keys, and its data if it transmits.
    current: Option<Transfer>,
    /// A chunked transmission waiting for its next chunk.
    pending: Option<Transfer>,
    sixel: Option<Box<sixel::Decoder>>,
    /// The last key given an image, and the last use.
    keys: u64,
    clock: u64,
    /// Ids given to images sent with a number and no id.
    next_id: u32,
}

impl Graphics {
    /// Whether there is anything to draw or being made.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty() && self.placements.is_empty()
    }

    /// The PNG of image `key`, once it is ready.
    pub fn png(&self, key: u64) -> Option<&[u8]> {
        match &self.images.iter().find(|i| i.key == key)?.data {
            Data::Png(png) => Some(png),
            Data::Building(_) => None,
        }
    }

    /// The image `key`'s size in pixels.
    pub fn size(&self, key: u64) -> Option<(u32, u32)> {
        let image = self.images.iter().find(|i| i.key == key)?;
        Some((image.width, image.height))
    }

    /// The placements, oldest first.
    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    /// Bytes of image data held.
    pub fn stored(&self) -> usize {
        self.images.iter().map(Image::bytes).sum()
    }

    /// Works on the PNG of the oldest image being built, about [`STEP_BYTES`] of it; whether
    /// it became ready to draw.
    pub fn step(&mut self) -> bool {
        let building = self
            .images
            .iter_mut()
            .find(|i| matches!(i.data, Data::Building(_)));
        let Some(image) = building else {
            return false;
        };
        let Data::Building(encoder) = &mut image.data else {
            return false;
        };
        match encoder.step(STEP_BYTES) {
            Some(png) => {
                image.data = Data::Png(png);
                true
            }
            None => false,
        }
    }

    /// Whether an image is still being built.
    pub fn building(&self) -> bool {
        self.images
            .iter()
            .any(|i| matches!(i.data, Data::Building(_)))
    }

    /// A kitty command starts with `control`.
    pub fn kitty_begin(&mut self, control: &[u8]) {
        let cmd = Command::parse(control);
        // Chunks after the first carry only `m` (and `q`).
        if !cmd.keyed {
            if let Some(mut transfer) = self.pending.take() {
                transfer.cmd.more = cmd.more;
                if cmd.quiet > 0 {
                    transfer.cmd.quiet = cmd.quiet;
                }
                self.current = Some(transfer);
                return;
            }
        }
        self.pending = None;
        let transmits = matches!(cmd.action, b't' | b'T' | b'q');
        let error = if !transmits {
            None
        } else if cmd.medium != b'd' {
            Some("EINVAL:only direct transmission (t=d): the page reads no files")
        } else if cmd.compression != 0 {
            Some("EINVAL:compressed data (o=z) is not supported")
        } else if !matches!(cmd.format, 24 | 32 | 100) {
            Some("EINVAL:unsupported format")
        } else if cmd.format != 100 && (cmd.width == 0 || cmd.height == 0) {
            Some("EINVAL:s and v are needed for raw pixels")
        } else if cmd.width > MAX_SIDE || cmd.height > MAX_SIDE {
            Some("EFBIG:the image is too large")
        } else {
            None
        };
        let cap = match cmd.format {
            24 | 32 => (cmd.width as usize)
                .saturating_mul(cmd.height as usize)
                .saturating_mul(cmd.format as usize / 8),
            _ => MAX_IMAGE_BYTES,
        };
        let error = error.or((cap > MAX_IMAGE_BYTES).then_some("EFBIG:the image is too large"));
        self.current = Some(Transfer {
            cmd,
            data: Vec::new(),
            cap: cap.min(MAX_IMAGE_BYTES),
            base64: Base64::default(),
            error,
        });
    }

    /// Some of the current kitty command's payload.
    pub fn kitty_data(&mut self, data: &[u8]) {
        if let Some(t) = &mut self.current {
            let transmits = matches!(t.cmd.action, b't' | b'T' | b'q');
            if transmits && t.error.is_none() {
                if let Err(error) = t.base64.decode(data, &mut t.data, t.cap) {
                    t.error = Some(error);
                    t.data = Vec::new();
                }
            }
        }
    }

    /// The current kitty command ended; `complete` unless something cut it off.
    pub fn kitty_end(&mut self, complete: bool, ctx: &Context) -> Outcome {
        let Some(mut t) = self.current.take() else {
            return Outcome::default();
        };
        if !complete {
            return Outcome::default();
        }
        if t.cmd.more && matches!(t.cmd.action, b't' | b'T' | b'q') {
            self.pending = Some(t);
            return Outcome::default();
        }
        if t.error.is_none() {
            if let Err(error) = t.base64.finish(&mut t.data, t.cap) {
                t.error = Some(error);
            }
        }
        let cmd = t.cmd.clone();
        let result = match cmd.action {
            b'q' => self.check(t).map(|_| None),
            b't' | b'T' => self.transmit(t, ctx),
            b'p' => self.put(&cmd, ctx),
            b'd' => {
                self.delete(&cmd, ctx);
                return Outcome::default();
            }
            _ => Err("EINVAL:unsupported action"),
        };
        let mut outcome = Outcome::default();
        let (id, message) = match result {
            Ok(Some((id, advance))) => {
                outcome.advance = advance;
                (id, "OK")
            }
            Ok(None) => (cmd.id, "OK"),
            Err(message) => (cmd.id, message),
        };
        let quiet = if message == "OK" { 1 } else { 2 };
        if (cmd.id != 0 || cmd.number != 0) && cmd.quiet < quiet {
            outcome.reply = reply(id, cmd.number, cmd.placement, message);
        }
        outcome
    }

    /// Whether a transmission's data makes an image: its size, and its pixels' encoder for raw
    /// data.
    fn check(&self, t: Transfer) -> Result<(u32, u32, Data), &'static str> {
        if let Some(error) = t.error {
            return Err(error);
        }
        let cmd = &t.cmd;
        match cmd.format {
            100 => {
                let (w, h) = png::dimensions(&t.data).ok_or("EINVAL:not a PNG")?;
                if w > MAX_SIDE || h > MAX_SIDE || u64::from(w) * u64::from(h) > MAX_PIXELS {
                    return Err("EFBIG:the image is too large");
                }
                Ok((w, h, Data::Png(t.data)))
            }
            format => {
                if t.data.len() != t.cap {
                    return Err("ENODATA:the data is not the size s and v say");
                }
                let pixels = if format == 24 {
                    Pixels::Rgb
                } else {
                    Pixels::Rgba
                };
                let encoder = Encoder::new(cmd.width, cmd.height, pixels, t.data)
                    .ok_or("EINVAL:the data does not make an image")?;
                Ok((cmd.width, cmd.height, Data::Building(Box::new(encoder))))
            }
        }
    }

    /// Stores a transmission; places it for `a=T`. The image's id, and how the cursor moves.
    fn transmit(
        &mut self,
        t: Transfer,
        ctx: &Context,
    ) -> Result<Option<(u32, Option<Advance>)>, &'static str> {
        if t.cmd.unicode {
            return Err("EINVAL:Unicode placeholders are not supported");
        }
        let cmd = t.cmd.clone();
        let (width, height, data) = self.check(t)?;
        let mut id = cmd.id;
        if id == 0 && cmd.number != 0 {
            id = self.free_id();
        }
        let key = self.store(id, cmd.number, width, height, data);
        let advance = match cmd.action {
            b'T' => self.place(key, &cmd, ctx),
            _ => None,
        };
        Ok(Some((id, advance)))
    }

    /// `a=p`: places an image sent before.
    fn put(
        &mut self,
        cmd: &Command,
        ctx: &Context,
    ) -> Result<Option<(u32, Option<Advance>)>, &'static str> {
        if cmd.unicode {
            return Err("EINVAL:Unicode placeholders are not supported");
        }
        let image = match (cmd.id, cmd.number) {
            (0, 0) => None,
            (0, number) => self.images.iter().rev().find(|i| i.number == number),
            (id, _) => self.images.iter().find(|i| i.id == id),
        };
        let Some(image) = image else {
            return Err("ENOENT:no such image");
        };
        let (key, id) = (image.key, image.id);
        Ok(Some((id, self.place(key, cmd, ctx))))
    }

    /// An id no image has.
    fn free_id(&mut self) -> u32 {
        loop {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            let id = u32::MAX - self.next_id;
            if !self.images.iter().any(|i| i.id == id) {
                return id;
            }
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Keeps an image, replacing the data of one with the same id; its key.
    fn store(&mut self, id: u32, number: u32, width: u32, height: u32, data: Data) -> u64 {
        let used = self.tick();
        if id != 0 {
            if let Some(image) = self.images.iter_mut().find(|i| i.id == id) {
                image.number = number;
                (image.width, image.height, image.data, image.used) = (width, height, data, used);
                let key = image.key;
                self.evict(Some(key));
                return key;
            }
        }
        self.keys += 1;
        let key = self.keys;
        self.images.push(Image {
            key,
            id,
            number,
            width,
            height,
            data,
            used,
        });
        self.evict(Some(key));
        key
    }

    /// Frees the least recently used images, those not placed first, until the bounds hold;
    /// `keep` never goes.
    fn evict(&mut self, keep: Option<u64>) {
        while self.images.len() > MAX_IMAGES || self.stored() > MAX_STORED {
            let placed = |key: u64| self.placements.iter().any(|p| p.image == key);
            let victim = self
                .images
                .iter()
                .filter(|i| Some(i.key) != keep)
                .min_by_key(|i| (placed(i.key), i.used))
                .map(|i| i.key);
            let Some(victim) = victim else {
                break;
            };
            self.forget(victim);
        }
    }

    fn forget(&mut self, key: u64) {
        self.images.retain(|i| i.key != key);
        self.placements.retain(|p| p.image != key);
    }

    /// Places image `key` at the cursor as `cmd` says; how the cursor moves.
    fn place(&mut self, key: u64, cmd: &Command, ctx: &Context) -> Option<Advance> {
        let image = self.images.iter_mut().find(|i| i.key == key)?;
        self.clock += 1;
        image.used = self.clock;
        let (iw, ih) = (image.width, image.height);
        let x = cmd.x.min(iw.saturating_sub(1));
        let y = cmd.y.min(ih.saturating_sub(1));
        let w = if cmd.w == 0 {
            iw - x
        } else {
            cmd.w.min(iw - x)
        };
        let h = if cmd.h == 0 {
            ih - y
        } else {
            cmd.h.min(ih - y)
        };
        let (cw, ch) = (ctx.cell.0.max(1.0), ctx.cell.1.max(1.0));
        let offset = (
            (cmd.offset_x as f32).min(cw - 1.0),
            (cmd.offset_y as f32).min(ch - 1.0),
        );
        let cells = |n: f32| n.ceil().clamp(1.0, 1000.0) as u16;
        let (cols, rows, size) = match (cmd.cols, cmd.rows) {
            (0, 0) => (
                cells((w as f32 + offset.0) / cw),
                cells((h as f32 + offset.1) / ch),
                None,
            ),
            (c, r) => {
                let (c, r) = match (c, r) {
                    (c, 0) => (c as f32, c as f32 * cw * h as f32 / w as f32 / ch),
                    (0, r) => (r as f32 * ch * w as f32 / h as f32 / cw, r as f32),
                    (c, r) => (c as f32, r as f32),
                };
                let (cols, rows) = (cells(c), cells(r));
                let size = (
                    f32::from(cols) * cw - offset.0,
                    f32::from(rows) * ch - offset.1,
                );
                (cols, rows, Some(size))
            }
        };
        let placement = Placement {
            image: key,
            id: cmd.placement,
            alt: ctx.alt,
            line: ctx.line(ctx.cursor.0),
            col: ctx.cursor.1,
            cols,
            rows,
            source: [x, y, w, h],
            offset,
            size,
            z: cmd.z,
        };
        if cmd.placement != 0 {
            let same = |p: &Placement| p.image == key && p.id == cmd.placement;
            self.placements.retain(|p| !same(p));
        }
        self.placements.push(placement);
        if self.placements.len() > MAX_PLACEMENTS {
            self.placements.remove(0);
        }
        (!cmd.no_move).then_some(Advance::Kitty { cols, rows })
    }

    /// `a=d`.
    fn delete(&mut self, cmd: &Command, ctx: &Context) {
        let (rows, _) = ctx.size;
        let top = ctx.line(0);
        let on_screen = |p: &Placement| {
            p.alt == ctx.alt && p.line < top + i64::from(rows) && p.line + i64::from(p.rows) > top
        };
        let covers = |p: &Placement, row: u16, col: u16| {
            let line = ctx.line(row);
            on_screen(p)
                && (p.line..p.line + i64::from(p.rows)).contains(&line)
                && (p.col..p.col.saturating_add(p.cols)).contains(&col)
        };
        let key_of = |images: &[Image], f: &dyn Fn(&Image) -> bool| {
            images
                .iter()
                .filter(|i| f(i))
                .map(|i| i.key)
                .collect::<Vec<_>>()
        };
        let cell = |n: u32| n.saturating_sub(1).min(u32::from(u16::MAX)) as u16;
        let what = cmd.delete;
        let (row, col) = ctx.cursor;
        let targets: Vec<u64> = match what.to_ascii_lowercase() {
            b'i' => key_of(&self.images, &|i| cmd.id != 0 && i.id == cmd.id),
            b'n' => self
                .images
                .iter()
                .rev()
                .find(|i| cmd.number != 0 && i.number == cmd.number)
                .map(|i| vec![i.key])
                .unwrap_or_default(),
            b'r' => key_of(&self.images, &|i| {
                i.id != 0 && (cmd.x..=cmd.y).contains(&i.id)
            }),
            _ => Vec::new(),
        };
        let before: Vec<u64> = self.placements.iter().map(|p| p.image).collect();
        self.placements.retain(|p| {
            let gone = match what.to_ascii_lowercase() {
                b'a' => on_screen(p),
                b'i' | b'n' => {
                    targets.contains(&p.image) && (cmd.placement == 0 || p.id == cmd.placement)
                }
                b'r' => targets.contains(&p.image),
                b'c' => covers(p, row, col),
                b'p' => covers(p, cell(cmd.y), cell(cmd.x)),
                b'x' => {
                    on_screen(p) && (p.col..p.col.saturating_add(p.cols)).contains(&cell(cmd.x))
                }
                b'y' => {
                    let line = ctx.line(cell(cmd.y));
                    on_screen(p) && (p.line..p.line + i64::from(p.rows)).contains(&line)
                }
                b'z' => on_screen(p) && p.z == cmd.z,
                _ => false,
            };
            !gone
        });
        // Upper case frees the images left with no placement.
        if what.is_ascii_uppercase() {
            let mut affected = before;
            affected.extend(targets);
            for key in affected {
                if !self.placements.iter().any(|p| p.image == key) {
                    self.images.retain(|i| i.key != key);
                }
            }
        }
    }

    /// A sixel string starts.
    pub fn sixel_begin(&mut self, params: &[u16]) {
        let max = (MAX_PIXELS as usize).min(MAX_IMAGE_BYTES);
        self.sixel = Some(Box::new(sixel::Decoder::new(
            params, MAX_SIDE, MAX_SIDE, max,
        )));
    }

    pub fn sixel_data(&mut self, data: &[u8]) {
        if let Some(decoder) = &mut self.sixel {
            decoder.feed(data);
        }
    }

    /// The sixel string ended: its image is placed at the cursor.
    pub fn sixel_end(&mut self, complete: bool, ctx: &Context) -> Outcome {
        let Some(decoder) = self.sixel.take() else {
            return Outcome::default();
        };
        let Some(image) = decoder.finish().filter(|_| complete) else {
            return Outcome::default();
        };
        let (width, height) = (image.width, image.height);
        let pixels = Pixels::Indexed {
            palette: image.palette,
        };
        let Some(encoder) = Encoder::new(width, height, pixels, image.indices) else {
            return Outcome::default();
        };
        let key = self.store(0, 0, width, height, Data::Building(Box::new(encoder)));
        let cmd = Command {
            no_move: true,
            ..Command::default()
        };
        self.place(key, &cmd, ctx);
        let rows = self.placements.last().map_or(1, |p| p.rows);
        Outcome {
            reply: Vec::new(),
            advance: Some(Advance::Below { rows }),
        }
    }

    /// The main screen scrolled to `scrolled`, with `history` lines kept above it: placements
    /// wholly above the history go.
    pub fn scrolled(&mut self, scrolled: i64, history: usize) {
        let first = scrolled - history as i64;
        self.placements
            .retain(|p| p.alt || p.line + i64::from(p.rows) > first);
        self.drop_unplaced_anonymous();
    }

    /// The display was erased (`ED 2`: the placements on the screen go), or its history (`ED 3`:
    /// those in the history go).
    pub fn erased(&mut self, ctx: &Context, history: bool) {
        let top = ctx.line(0);
        let bottom = top + i64::from(ctx.size.0);
        self.placements.retain(|p| {
            let end = p.line + i64::from(p.rows);
            let gone = match history {
                false => p.line < bottom && end > top,
                true => p.line < top,
            };
            p.alt != ctx.alt || !gone
        });
        self.drop_unplaced_anonymous();
    }

    /// The alternate screen was left: its placements go.
    pub fn left_alternate(&mut self) {
        self.placements.retain(|p| !p.alt);
        self.drop_unplaced_anonymous();
    }

    /// A reset: everything goes.
    pub fn reset(&mut self) {
        *self = Self {
            keys: self.keys,
            ..Self::default()
        };
    }

    /// Images without an id can never be placed again: they go with their last placement.
    fn drop_unplaced_anonymous(&mut self) {
        let placements = &self.placements;
        self.images
            .retain(|i| i.id != 0 || placements.iter().any(|p| p.image == i.key));
    }
}

/// The protocol's answer: `ESC _ G i=…[,I=…][,p=…];message ESC \`.
fn reply(id: u32, number: u32, placement: u32, message: &str) -> Vec<u8> {
    let mut keys = format!("i={id}");
    if number != 0 {
        keys.push_str(&format!(",I={number}"));
    }
    if placement != 0 {
        keys.push_str(&format!(",p={placement}"));
    }
    format!("\x1b_G{keys};{message}\x1b\\").into_bytes()
}
