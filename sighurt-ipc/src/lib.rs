//! # sighurt-ipc — the out-of-process engine protocol
//!
//! An external engine is any executable. Sighurt spawns one process per page with piped
//! stdin/stdout, writes [`ToEngine`] messages to the engine's stdin and reads [`FromEngine`]
//! messages from its stdout. stderr is inherited, so engine logs go straight to the terminal.
//! The engine should exit when its stdin is closed.
//!
//! The format is deliberately trivial so an engine can be written in any language:
//!
//! ```text
//! message  = len:u32 tag:u8 payload        (len counts tag + payload)
//! payload  = fields in declaration order
//! u8, bool = 1 byte          u32, f32 = 4 bytes little-endian
//! str      = len:u32 utf8    bytes    = len:u32 data
//! ```
//!
//! Lifecycle: the engine writes [`FromEngine::Hello`] first. The host then sends
//! [`ToEngine::Resize`] followed by [`ToEngine::Navigate`]. Input coordinates and sizes are
//! physical pixels relative to the top-left corner of the page.
//!
//! An engine shows its page in one of two ways, and may switch at any time; the latest message
//! replaces the page's content:
//!
//! - [`FromEngine::Frame`]: finished pixels (tightly packed RGBA8 rows, top to bottom, physical
//!   pixels). For engines that rasterize themselves, such as a web engine.
//! - [`FromEngine::Draw`]: a list of [`DrawOp`]s in logical pixels (physical / scale) that the
//!   host paints with its own renderer and fonts. Images are uploaded once with
//!   [`FromEngine::Image`] and referenced by id. For engines that only need simple 2D drawing.
//!
//! Stdout belongs to the protocol: engines must keep their own logging on stderr.

use std::io::{self, Read, Write};

/// Protocol version carried by [`FromEngine::Hello`]. Bumped on incompatible changes.
pub const VERSION: u32 = 2;

/// Upper bound on a single message, so a misbehaving peer cannot make the other side allocate
/// arbitrary amounts of memory (an 8K RGBA frame is ~133 MB).
pub const MAX_MESSAGE_LEN: u32 = 256 * 1024 * 1024;

/// Modifier bits used by [`ToEngine::Key`].
pub const MOD_SHIFT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_ALT: u8 = 4;
pub const MOD_META: u8 = 8;

/// Messages from the Sighurt shell to an engine.
#[derive(Clone, Debug, PartialEq)]
pub enum ToEngine {
    Navigate {
        url: String,
    },
    Reload,
    Stop,
    Back,
    Forward,
    /// Viewport size in physical pixels and the device scale factor.
    Resize {
        width: u32,
        height: u32,
        scale: f32,
    },
    MouseMove {
        x: f32,
        y: f32,
    },
    /// `button` follows the DOM numbering: 0 left, 1 middle, 2 right, 3 back, 4 forward.
    MouseButton {
        button: u8,
        down: bool,
        x: f32,
        y: f32,
    },
    /// Scroll deltas in physical pixels at the pointer position, with DOM `WheelEvent` signs:
    /// positive `dy` scrolls down, positive `dx` scrolls right.
    Wheel {
        dx: f32,
        dy: f32,
        x: f32,
        y: f32,
    },
    /// `key` and `code` are W3C `KeyboardEvent.key` / `.code` values (e.g. "a"/"KeyA",
    /// "Enter"/"Enter"). `modifiers` is a set of `MOD_*` bits.
    Key {
        down: bool,
        key: String,
        code: String,
        modifiers: u8,
        repeat: bool,
    },
    Focus {
        focused: bool,
    },
    Zoom {
        factor: f32,
    },
}

/// Messages from an engine to the Sighurt shell.
#[derive(Clone, Debug, PartialEq)]
pub enum FromEngine {
    Hello {
        version: u32,
        name: String,
    },
    /// A rendered frame: `width * height * 4` bytes of RGBA8, in physical pixels.
    Frame {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// Draw operations, in logical pixels, that make up the whole page.
    Draw {
        ops: Vec<DrawOp>,
    },
    /// Defines (or replaces) image `id` for [`DrawOp::Image`]: `width * height * 4` bytes of
    /// RGBA8.
    Image {
        id: u32,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// Frees image `id`.
    DropImage {
        id: u32,
    },
    Url {
        url: String,
    },
    Title {
        title: String,
    },
    Loading {
        loading: bool,
    },
    History {
        can_back: bool,
        can_forward: bool,
    },
    /// A CSS cursor keyword ("default", "pointer", "text", ...).
    Cursor {
        name: String,
    },
    /// Status text such as a hovered link target; empty clears it.
    Status {
        text: String,
    },
    /// `level`: 0 log, 1 info, 2 warn, 3 error, 4 debug.
    Console {
        level: u8,
        message: String,
    },
    /// The page asks the shell to open `url` (links with a target, `window.open`, ...).
    Open {
        url: String,
        new_tab: bool,
    },
    Error {
        message: String,
    },
}

/// One step of a [`FromEngine::Draw`] list. Coordinates are logical pixels from the page's
/// top-left corner; colors are `0xRRGGBBAA`.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawOp {
    /// Fills the whole page.
    Clear {
        color: u32,
    },
    /// A filled rectangle. `radius` rounds its corners; a square with `radius = w / 2` is a
    /// circle.
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: u32,
    },
    /// A polyline stroked `width` wide, or a filled polygon when `width` is 0.
    Path {
        points: Vec<(f32, f32)>,
        width: f32,
        color: u32,
    },
    /// One line of text in the host's fonts. The line box is `size * 1.2` tall with its top at
    /// `y`; `x` is its left edge, centre or right edge for `align` 0, 1 or 2. An empty `family`
    /// means the host's default font; `weight` is CSS-style (400 normal, 700 bold).
    Text {
        x: f32,
        y: f32,
        size: f32,
        color: u32,
        family: String,
        weight: u16,
        italic: bool,
        align: u8,
        text: String,
    },
    /// Image `id` (see [`FromEngine::Image`]) scaled into the rectangle.
    Image {
        id: u32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    /// Clips the following operations to the rectangle, intersected with the current clip,
    /// until the matching [`DrawOp::PopClip`].
    PushClip {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    PopClip,
}

impl DrawOp {
    fn encode(&self, e: &mut Encoder) {
        match self {
            Self::Clear { color } => e.tag(0).u32(*color),
            Self::Rect {
                x,
                y,
                w,
                h,
                radius,
                color,
            } => e
                .tag(1)
                .f32(*x)
                .f32(*y)
                .f32(*w)
                .f32(*h)
                .f32(*radius)
                .u32(*color),
            Self::Path {
                points,
                width,
                color,
            } => {
                e.tag(2).u32(points.len() as u32);
                for (x, y) in points {
                    e.f32(*x).f32(*y);
                }
                e.f32(*width).u32(*color)
            }
            Self::Text {
                x,
                y,
                size,
                color,
                family,
                weight,
                italic,
                align,
                text,
            } => e
                .tag(3)
                .f32(*x)
                .f32(*y)
                .f32(*size)
                .u32(*color)
                .str(family)
                .u32(u32::from(*weight))
                .bool(*italic)
                .u8(*align)
                .str(text),
            Self::Image { id, x, y, w, h } => e.tag(4).u32(*id).f32(*x).f32(*y).f32(*w).f32(*h),
            Self::PushClip { x, y, w, h } => e.tag(5).f32(*x).f32(*y).f32(*w).f32(*h),
            Self::PopClip => e.tag(6),
        };
    }

    fn decode(d: &mut Decoder) -> io::Result<Self> {
        Ok(match d.u8()? {
            0 => Self::Clear { color: d.u32()? },
            1 => Self::Rect {
                x: d.f32()?,
                y: d.f32()?,
                w: d.f32()?,
                h: d.f32()?,
                radius: d.f32()?,
                color: d.u32()?,
            },
            2 => {
                let count = d.count(8)?;
                let mut points = Vec::with_capacity(count);
                for _ in 0..count {
                    points.push((d.f32()?, d.f32()?));
                }
                Self::Path {
                    points,
                    width: d.f32()?,
                    color: d.u32()?,
                }
            }
            3 => Self::Text {
                x: d.f32()?,
                y: d.f32()?,
                size: d.f32()?,
                color: d.u32()?,
                family: d.str()?,
                weight: d.u32()?.min(u32::from(u16::MAX)) as u16,
                italic: d.bool()?,
                align: d.u8()?,
                text: d.str()?,
            },
            4 => Self::Image {
                id: d.u32()?,
                x: d.f32()?,
                y: d.f32()?,
                w: d.f32()?,
                h: d.f32()?,
            },
            5 => Self::PushClip {
                x: d.f32()?,
                y: d.f32()?,
                w: d.f32()?,
                h: d.f32()?,
            },
            6 => Self::PopClip,
            tag => return Err(invalid(format!("unknown DrawOp tag {tag}"))),
        })
    }
}

impl ToEngine {
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let mut e = Encoder::default();
        match self {
            Self::Navigate { url } => e.tag(0).str(url),
            Self::Reload => e.tag(1),
            Self::Stop => e.tag(2),
            Self::Back => e.tag(3),
            Self::Forward => e.tag(4),
            Self::Resize {
                width,
                height,
                scale,
            } => e.tag(5).u32(*width).u32(*height).f32(*scale),
            Self::MouseMove { x, y } => e.tag(6).f32(*x).f32(*y),
            Self::MouseButton { button, down, x, y } => {
                e.tag(7).u8(*button).bool(*down).f32(*x).f32(*y)
            }
            Self::Wheel { dx, dy, x, y } => e.tag(8).f32(*dx).f32(*dy).f32(*x).f32(*y),
            Self::Key {
                down,
                key,
                code,
                modifiers,
                repeat,
            } => e
                .tag(9)
                .bool(*down)
                .str(key)
                .str(code)
                .u8(*modifiers)
                .bool(*repeat),
            Self::Focus { focused } => e.tag(10).bool(*focused),
            Self::Zoom { factor } => e.tag(11).f32(*factor),
        };
        e.finish(w, &[])
    }

    /// Reads one message. Returns `Ok(None)` when the stream ends cleanly between messages.
    pub fn read_from(r: &mut impl Read) -> io::Result<Option<Self>> {
        let Some((tag, len)) = read_header(r)? else {
            return Ok(None);
        };
        let mut d = Decoder::read(r, len)?;
        let msg = match tag {
            0 => Self::Navigate { url: d.str()? },
            1 => Self::Reload,
            2 => Self::Stop,
            3 => Self::Back,
            4 => Self::Forward,
            5 => Self::Resize {
                width: d.u32()?,
                height: d.u32()?,
                scale: d.f32()?,
            },
            6 => Self::MouseMove {
                x: d.f32()?,
                y: d.f32()?,
            },
            7 => Self::MouseButton {
                button: d.u8()?,
                down: d.bool()?,
                x: d.f32()?,
                y: d.f32()?,
            },
            8 => Self::Wheel {
                dx: d.f32()?,
                dy: d.f32()?,
                x: d.f32()?,
                y: d.f32()?,
            },
            9 => Self::Key {
                down: d.bool()?,
                key: d.str()?,
                code: d.str()?,
                modifiers: d.u8()?,
                repeat: d.bool()?,
            },
            10 => Self::Focus { focused: d.bool()? },
            11 => Self::Zoom { factor: d.f32()? },
            tag => return Err(invalid(format!("unknown ToEngine tag {tag}"))),
        };
        Ok(Some(msg))
    }
}

impl FromEngine {
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let mut e = Encoder::default();
        // Frame pixels are written straight from the caller's buffer instead of being copied
        // into the encoder.
        let mut tail: &[u8] = &[];
        match self {
            Self::Hello { version, name } => e.tag(0).u32(*version).str(name),
            Self::Frame {
                width,
                height,
                rgba,
            } => {
                tail = rgba;
                e.tag(1).u32(*width).u32(*height).u32(len_u32(rgba.len())?)
            }
            Self::Url { url } => e.tag(2).str(url),
            Self::Title { title } => e.tag(3).str(title),
            Self::Loading { loading } => e.tag(4).bool(*loading),
            Self::History {
                can_back,
                can_forward,
            } => e.tag(5).bool(*can_back).bool(*can_forward),
            Self::Cursor { name } => e.tag(6).str(name),
            Self::Status { text } => e.tag(7).str(text),
            Self::Console { level, message } => e.tag(8).u8(*level).str(message),
            Self::Open { url, new_tab } => e.tag(9).str(url).bool(*new_tab),
            Self::Error { message } => e.tag(10).str(message),
            Self::Draw { ops } => {
                e.tag(11).u32(len_u32(ops.len())?);
                for op in ops {
                    op.encode(&mut e);
                }
                &mut e
            }
            Self::Image {
                id,
                width,
                height,
                rgba,
            } => {
                tail = rgba;
                e.tag(12)
                    .u32(*id)
                    .u32(*width)
                    .u32(*height)
                    .u32(len_u32(rgba.len())?)
            }
            Self::DropImage { id } => e.tag(13).u32(*id),
        };
        e.finish(w, tail)
    }

    /// Reads one message. Returns `Ok(None)` when the stream ends cleanly between messages.
    pub fn read_from(r: &mut impl Read) -> io::Result<Option<Self>> {
        let Some((tag, len)) = read_header(r)? else {
            return Ok(None);
        };
        match tag {
            1 => {
                let (width, height, rgba) = read_pixels(r, len)?;
                return Ok(Some(Self::Frame {
                    width,
                    height,
                    rgba,
                }));
            }
            12 => {
                let mut d = Decoder::read(r, len.min(4))?;
                let id = d.u32()?;
                let (width, height, rgba) = read_pixels(r, len.saturating_sub(4))?;
                return Ok(Some(Self::Image {
                    id,
                    width,
                    height,
                    rgba,
                }));
            }
            _ => {}
        }
        let mut d = Decoder::read(r, len)?;
        let msg = match tag {
            0 => Self::Hello {
                version: d.u32()?,
                name: d.str()?,
            },
            2 => Self::Url { url: d.str()? },
            3 => Self::Title { title: d.str()? },
            4 => Self::Loading { loading: d.bool()? },
            5 => Self::History {
                can_back: d.bool()?,
                can_forward: d.bool()?,
            },
            6 => Self::Cursor { name: d.str()? },
            7 => Self::Status { text: d.str()? },
            8 => Self::Console {
                level: d.u8()?,
                message: d.str()?,
            },
            9 => Self::Open {
                url: d.str()?,
                new_tab: d.bool()?,
            },
            10 => Self::Error { message: d.str()? },
            11 => {
                let count = d.count(1)?;
                let mut ops = Vec::with_capacity(count);
                for _ in 0..count {
                    ops.push(DrawOp::decode(&mut d)?);
                }
                Self::Draw { ops }
            }
            13 => Self::DropImage { id: d.u32()? },
            tag => return Err(invalid(format!("unknown FromEngine tag {tag}"))),
        };
        Ok(Some(msg))
    }
}

/// Reads `width height size` and then `size` bytes of RGBA straight into their final buffer.
/// `len` is what is left of the message.
fn read_pixels(r: &mut impl Read, len: usize) -> io::Result<(u32, u32, Vec<u8>)> {
    let mut d = Decoder::read(r, len.min(12))?;
    let (width, height, size) = (d.u32()?, d.u32()?, d.u32()? as usize);
    if size as u64 != u64::from(width) * u64::from(height) * 4 || size != len - 12 {
        return Err(invalid("image size does not match its dimensions"));
    }
    // Not `vec![0; size]` and `read_exact`: zeroing the buffer first made reading a frame
    // through a pipe 5-10% slower.
    let mut rgba = Vec::with_capacity(size);
    r.take(size as u64).read_to_end(&mut rgba)?;
    if rgba.len() != size {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok((width, height, rgba))
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn len_u32(len: usize) -> io::Result<u32> {
    u32::try_from(len)
        .ok()
        .filter(|&l| l < MAX_MESSAGE_LEN)
        .ok_or_else(|| invalid("message too large"))
}

#[derive(Default)]
struct Encoder(Vec<u8>);

impl Encoder {
    fn tag(&mut self, tag: u8) -> &mut Self {
        self.u8(tag)
    }
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(v as u8)
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn f32(&mut self, v: f32) -> &mut Self {
        self.u32(v.to_bits())
    }
    fn str(&mut self, s: &str) -> &mut Self {
        // Strings are UI metadata; clamp absurd sizes rather than failing the whole message.
        let s = &s.as_bytes()[..s.len().min(1 << 20)];
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s);
        self
    }

    /// Writes the length prefix, the encoded fields and then `tail` as one message.
    fn finish(&self, w: &mut impl Write, tail: &[u8]) -> io::Result<()> {
        let len = len_u32(self.0.len() + tail.len())?;
        w.write_all(&len.to_le_bytes())?;
        w.write_all(&self.0)?;
        w.write_all(tail)?;
        w.flush()
    }
}

struct Decoder {
    buf: Vec<u8>,
    pos: usize,
}

/// Reads a message's length prefix and tag. Returns the tag and the payload length, or `None`
/// on a clean end of stream.
fn read_header(r: &mut impl Read) -> io::Result<Option<(u8, usize)>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len);
    if len == 0 || len >= MAX_MESSAGE_LEN {
        return Err(invalid(format!("invalid message length {len}")));
    }
    let mut tag = [0u8; 1];
    r.read_exact(&mut tag)?;
    Ok(Some((tag[0], len as usize - 1)))
}

impl Decoder {
    fn read(r: &mut impl Read, len: usize) -> io::Result<Self> {
        let mut buf = vec![0; len];
        r.read_exact(&mut buf)?;
        Ok(Self { buf, pos: 0 })
    }

    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.buf.len())
            .ok_or_else(|| invalid("truncated message"))?;
        let bytes = &self.buf[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> io::Result<bool> {
        Ok(self.u8()? != 0)
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn str(&mut self) -> io::Result<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| invalid("invalid utf-8"))
    }
    /// Reads an element count, rejecting counts the rest of the message cannot hold (each
    /// element takes at least `min_size` bytes), so a peer cannot trigger huge allocations.
    fn count(&mut self, min_size: usize) -> io::Result<usize> {
        let count = self.u32()? as usize;
        if count.saturating_mul(min_size) > self.buf.len() - self.pos {
            return Err(invalid("element count exceeds message size"));
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_to(msg: ToEngine) {
        let mut buf = Vec::new();
        msg.write_to(&mut buf).unwrap();
        let mut r = buf.as_slice();
        assert_eq!(ToEngine::read_from(&mut r).unwrap(), Some(msg));
        assert_eq!(ToEngine::read_from(&mut r).unwrap(), None);
    }

    fn roundtrip_from(msg: FromEngine) {
        let mut buf = Vec::new();
        msg.write_to(&mut buf).unwrap();
        let mut r = buf.as_slice();
        assert_eq!(FromEngine::read_from(&mut r).unwrap(), Some(msg));
        assert_eq!(FromEngine::read_from(&mut r).unwrap(), None);
    }

    #[test]
    fn to_engine_roundtrips() {
        roundtrip_to(ToEngine::Navigate {
            url: "https://example.com/ä".into(),
        });
        roundtrip_to(ToEngine::Reload);
        roundtrip_to(ToEngine::Stop);
        roundtrip_to(ToEngine::Back);
        roundtrip_to(ToEngine::Forward);
        roundtrip_to(ToEngine::Resize {
            width: 1920,
            height: 1080,
            scale: 1.5,
        });
        roundtrip_to(ToEngine::MouseMove { x: 1.5, y: -2.0 });
        roundtrip_to(ToEngine::MouseButton {
            button: 2,
            down: true,
            x: 3.0,
            y: 4.0,
        });
        roundtrip_to(ToEngine::Wheel {
            dx: 0.0,
            dy: -120.0,
            x: 5.0,
            y: 6.0,
        });
        roundtrip_to(ToEngine::Key {
            down: true,
            key: "Enter".into(),
            code: "Enter".into(),
            modifiers: MOD_CTRL | MOD_SHIFT,
            repeat: false,
        });
        roundtrip_to(ToEngine::Focus { focused: true });
        roundtrip_to(ToEngine::Zoom { factor: 1.25 });
    }

    #[test]
    fn from_engine_roundtrips() {
        roundtrip_from(FromEngine::Hello {
            version: VERSION,
            name: "servo".into(),
        });
        roundtrip_from(FromEngine::Frame {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
        });
        roundtrip_from(FromEngine::Frame {
            width: 0,
            height: 0,
            rgba: vec![],
        });
        roundtrip_from(FromEngine::Url {
            url: "https://example.com".into(),
        });
        roundtrip_from(FromEngine::Title {
            title: "Example Domain".into(),
        });
        roundtrip_from(FromEngine::Loading { loading: true });
        roundtrip_from(FromEngine::History {
            can_back: true,
            can_forward: false,
        });
        roundtrip_from(FromEngine::Cursor {
            name: "pointer".into(),
        });
        roundtrip_from(FromEngine::Status {
            text: String::new(),
        });
        roundtrip_from(FromEngine::Console {
            level: 3,
            message: "boom".into(),
        });
        roundtrip_from(FromEngine::Open {
            url: "https://a.b".into(),
            new_tab: true,
        });
        roundtrip_from(FromEngine::Error {
            message: "nope".into(),
        });
        roundtrip_from(FromEngine::Image {
            id: 7,
            width: 1,
            height: 2,
            rgba: vec![9; 8],
        });
        roundtrip_from(FromEngine::DropImage { id: 7 });
        roundtrip_from(FromEngine::Draw { ops: vec![] });
        roundtrip_from(FromEngine::Draw {
            ops: vec![
                DrawOp::Clear { color: 0x112233ff },
                DrawOp::Rect {
                    x: 1.0,
                    y: 2.0,
                    w: 3.0,
                    h: 4.0,
                    radius: 1.5,
                    color: 0xff0000ff,
                },
                DrawOp::Path {
                    points: vec![(0.0, 0.0), (10.0, 5.0), (3.0, 9.0)],
                    width: 0.0,
                    color: 0x00ff0080,
                },
                DrawOp::Text {
                    x: 5.0,
                    y: 6.0,
                    size: 14.0,
                    color: 0xffffffff,
                    family: "monospace".into(),
                    weight: 700,
                    italic: true,
                    align: 1,
                    text: "héllo".into(),
                },
                DrawOp::PushClip {
                    x: 0.0,
                    y: 0.0,
                    w: 50.0,
                    h: 50.0,
                },
                DrawOp::Image {
                    id: 7,
                    x: 0.0,
                    y: 0.0,
                    w: 16.0,
                    h: 16.0,
                },
                DrawOp::PopClip,
            ],
        });
    }

    #[test]
    fn rejects_bad_input() {
        // Frame whose pixel count does not match its dimensions.
        let mut buf = Vec::new();
        FromEngine::Frame {
            width: 2,
            height: 2,
            rgba: vec![0; 4],
        }
        .write_to(&mut buf)
        .unwrap();
        assert!(FromEngine::read_from(&mut buf.as_slice()).is_err());

        // Frame whose pixels end early.
        let mut r: &[u8] = &[
            21, 0, 0, 0, 1, 2, 0, 0, 0, 1, 0, 0, 0, 8, 0, 0, 0, 1, 2, 3, 4,
        ];
        assert!(FromEngine::read_from(&mut r).is_err());

        // Unknown tag.
        let mut r: &[u8] = &[1, 0, 0, 0, 200];
        assert!(ToEngine::read_from(&mut r).is_err());

        // Length prefix larger than the limit.
        let mut r: &[u8] = &[0xff, 0xff, 0xff, 0xff];
        assert!(FromEngine::read_from(&mut r).is_err());

        // Truncated string payload.
        let mut r: &[u8] = &[6, 0, 0, 0, 0, 10, 0, 0, 0, b'x'];
        assert!(ToEngine::read_from(&mut r).is_err());

        // A draw list claiming more ops than the message holds must not allocate for them.
        let mut r: &[u8] = &[5, 0, 0, 0, 11, 0xff, 0xff, 0xff, 0xff];
        assert!(FromEngine::read_from(&mut r).is_err());

        // Unknown draw op.
        let mut r: &[u8] = &[6, 0, 0, 0, 11, 1, 0, 0, 0, 99];
        assert!(FromEngine::read_from(&mut r).is_err());
    }
}
