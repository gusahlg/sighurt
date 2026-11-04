//! Seeded random-input tests of what the terminal frontend writes: the sixel and kitty encoders
//! (checked by decoding what they emit), the cell grid's diff (checked by replaying it on a
//! small terminal model), the sanitizer, and text-mode layout of random snapshots. No panic, a
//! return in time, and the output means what the input said. `tools/fuzz/mod.rs` describes the
//! environment variables.

#[path = "../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::io::Read;
use std::time::Duration;

use base64::Engine as _;
use fuzz::{iterations, run, Rng};
use sighurt_api::geometry::BufferSize;
use sighurt_api::ids::{DocumentGeneration, ViewId};
use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, Modifiers};
use sighurt_api::present::Frame;
use sighurt_api::semantic::{NodeId, NodeKind, SemanticNode, SemanticSnapshot};
use sighurt_tui::grid::{Attrs, Color, Grid};
use sighurt_tui::kitty::{self, CellRect, CHUNK_BYTES};
use sighurt_tui::sanitize::sanitize;
use sighurt_tui::sixel;
use sighurt_tui::text::TextPages;
use sighurt_tui::textmode::{self, Area, TextView};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const LIMIT: Duration = Duration::from_secs(5);

/// A frame: premultiplied B, G, R, A, rows `stride` bytes apart.
struct Image {
    width: u32,
    height: u32,
    stride: usize,
    pixels: Vec<u8>,
}

impl Image {
    fn frame(&mut self) -> Frame<'_> {
        Frame {
            pixels: &mut self.pixels,
            size: BufferSize {
                width: self.width,
                height: self.height,
            },
            stride: self.stride,
        }
    }

    /// The pixel at (x, y) as it should look over black: (r, g, b).
    fn over_black(&self, x: usize, y: usize) -> [u8; 3] {
        let at = y * self.stride + x * 4;
        let (b, g, r, a) = (
            self.pixels[at],
            self.pixels[at + 1],
            self.pixels[at + 2],
            self.pixels[at + 3],
        );
        // Premultiplied colour over black is the colour itself, no more than its alpha.
        [r.min(a), g.min(a), b.min(a)]
    }
}

fn image(rng: &mut Rng) -> Image {
    let big = rng.one_in(12);
    let width = if big {
        rng.range(100, 420)
    } else {
        rng.range(1, 70)
    } as u32;
    let height = if big {
        rng.range(60, 240)
    } else {
        rng.range(1, 50)
    } as u32;
    let stride = width as usize * 4 + if rng.one_in(3) { rng.range(1, 17) } else { 0 };
    let mut pixels = vec![0u8; stride * height as usize];
    let mode = rng.below(6);
    let base = [rng.byte(), rng.byte(), rng.byte()];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let at = y * stride + x * 4;
            let px: [u8; 4] = match mode {
                0 => [rng.byte(), rng.byte(), rng.byte(), rng.byte()],
                1 => [base[0], base[1], base[2], 255],
                2 => [
                    (x * 255 / width as usize) as u8,
                    (y * 255 / height as usize) as u8,
                    base[2],
                    255,
                ],
                3 => {
                    let on = (x / 3 + y / 5) % 2 == 0;
                    if on {
                        [255, 255, 255, 255]
                    } else {
                        [0, 0, 0, 255]
                    }
                }
                4 => {
                    // Valid premultiplied pixels.
                    let a = rng.byte();
                    let p = |c: u8| (u16::from(c) * u16::from(a) / 255) as u8;
                    [p(rng.byte()), p(rng.byte()), p(rng.byte()), a]
                }
                _ => {
                    let v = if rng.one_in(2) { 255 } else { 0 };
                    [v, rng.byte() & 0x80, 255 - v, 255]
                }
            };
            pixels[at..at + 4].copy_from_slice(&px);
        }
        // Padding bytes are garbage the encoder must not read as pixels.
        for pad in &mut pixels[(y * stride + width as usize * 4)..((y + 1) * stride)] {
            *pad = rng.byte();
        }
    }
    Image {
        width,
        height,
        stride,
        pixels,
    }
}

fn place(rng: &mut Rng) -> CellRect {
    CellRect {
        column: rng.below(200) as u16,
        row: rng.below(100) as u16,
        columns: rng.range(0, 80) as u16,
        rows: rng.range(0, 40) as u16,
    }
}

// ---- sixel ----

/// What a sixel image decoded to: the registers' colours, and the register of each pixel.
struct Sixel {
    width: usize,
    height: usize,
    palette: Vec<[u8; 3]>,
    pixels: Vec<Option<u8>>,
}

fn number(bytes: &[u8], at: &mut usize) -> usize {
    let start = *at;
    while *at < bytes.len() && bytes[*at].is_ascii_digit() {
        *at += 1;
    }
    std::str::from_utf8(&bytes[start..*at])
        .unwrap()
        .parse()
        .unwrap_or_else(|_| panic!("no number at {start}"))
}

fn expect(bytes: &[u8], at: &mut usize, text: &[u8]) {
    assert!(
        bytes[*at..].starts_with(text),
        "expected {:?} at {}, found {:?}",
        String::from_utf8_lossy(text),
        *at,
        String::from_utf8_lossy(&bytes[*at..(*at + 12).min(bytes.len())])
    );
    *at += text.len();
}

fn decode_sixel(bytes: &[u8]) -> Sixel {
    let mut at = 0;
    expect(bytes, &mut at, b"\x1b[s");
    // A cursor move: ESC [ H, or ESC [ line ; column H.
    expect(bytes, &mut at, b"\x1b[");
    while bytes[at] != b'H' {
        assert!(
            bytes[at].is_ascii_digit() || bytes[at] == b';',
            "cursor move"
        );
        at += 1;
    }
    at += 1;
    expect(bytes, &mut at, b"\x1bP0;2q\"1;1;");
    let width = number(bytes, &mut at);
    expect(bytes, &mut at, b";");
    let height = number(bytes, &mut at);
    // Every one of the 252 registers is defined, in order, before any pixel.
    let mut palette = Vec::new();
    for index in 0..252 {
        expect(bytes, &mut at, format!("#{index};2;").as_bytes());
        let r = number(bytes, &mut at);
        expect(bytes, &mut at, b";");
        let g = number(bytes, &mut at);
        expect(bytes, &mut at, b";");
        let b = number(bytes, &mut at);
        assert!(r <= 100 && g <= 100 && b <= 100);
        palette.push([
            (r * 255 / 100) as u8,
            (g * 255 / 100) as u8,
            (b * 255 / 100) as u8,
        ]);
    }
    let mut pixels = vec![None; width * height];
    let (mut x, mut band) = (0usize, 0usize);
    let mut color = None;
    while !bytes[at..].starts_with(b"\x1b\\") {
        let byte = bytes[at];
        match byte {
            b'#' => {
                at += 1;
                let n = number(bytes, &mut at);
                assert!(n < 252, "register {n}");
                assert!(bytes[at] != b';', "a register defined in the body");
                color = Some(n as u8);
            }
            b'$' => {
                x = 0;
                at += 1;
            }
            b'-' => {
                x = 0;
                band += 1;
                at += 1;
            }
            b'!' | b'?'..=b'~' => {
                let mut count = 1;
                if byte == b'!' {
                    at += 1;
                    count = number(bytes, &mut at);
                    assert!((1..=10_000).contains(&count), "repeat {count}");
                }
                let bits = bytes[at] - b'?';
                assert!(bytes[at] >= b'?' && bytes[at] <= b'~');
                at += 1;
                let color = color.expect("pixels before a colour");
                assert!(x + count <= width, "{count} columns at {x} of {width}");
                for column in x..x + count {
                    for row in 0..6 {
                        if bits & (1 << row) != 0 {
                            let y = band * 6 + row;
                            assert!(y < height, "row {y} of {height}");
                            let slot = &mut pixels[y * width + column];
                            assert!(slot.is_none(), "pixel ({column}, {y}) painted twice");
                            *slot = Some(color);
                        }
                    }
                }
                x += count;
            }
            other => panic!("byte {other:#x} in the body at {at}"),
        }
    }
    at += 2;
    assert_eq!(&bytes[at..], b"\x1b[u", "the cursor is restored");
    assert_eq!(band + 1, height / 6, "bands");
    Sixel {
        width,
        height,
        palette,
        pixels,
    }
}

#[test]
fn fuzz_sixel_round_trips_within_the_palette() {
    run("fuzz_sixel", iterations(100) / 10, LIMIT, |case| {
        let mut img = image(&mut case.rng);
        let rect = place(&mut case.rng);
        let (cw, ch) = (case.rng.range(0, 20) as u16, case.rng.range(0, 24) as u16);
        let max = if case.rng.one_in(4) {
            case.rng.below(8000)
        } else {
            usize::MAX / 2
        };
        case.note(|| {
            format!(
                "{}x{} stride {} {rect:?} cell {cw}x{ch} max {max}",
                img.width, img.height, img.stride
            )
        });
        let result = sixel::encode(&img.frame(), rect, cw, ch, max);
        let wanted_w = (img.width as usize).min(rect.columns as usize * cw as usize);
        let wanted_h = (img.height as usize).min(rect.rows as usize * ch as usize) / 6 * 6;
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(sixel::Error::Empty) => {
                assert!(
                    wanted_w == 0
                        || wanted_h == 0
                        || rect.columns == 0
                        || rect.rows == 0
                        || cw == 0
                        || ch == 0
                );
                return;
            }
            Err(sixel::Error::TooLarge { bytes, max: m }) => {
                assert_eq!(m, max);
                assert!(bytes > max);
                return;
            }
            Err(other) => panic!("{other}"),
        };
        assert!(
            bytes.len() <= max,
            "{} bytes over the cap {max}",
            bytes.len()
        );
        assert!(
            bytes
                .iter()
                .all(|&b| b == 0x1b || (0x20..0x7f).contains(&b)),
            "a control byte"
        );
        let decoded = decode_sixel(&bytes);
        assert_eq!((decoded.width, decoded.height), (wanted_w, wanted_h));
        for y in 0..decoded.height {
            for x in 0..decoded.width {
                let register = decoded.pixels[y * decoded.width + x]
                    .unwrap_or_else(|| panic!("pixel ({x}, {y}) never painted"));
                let shown = decoded.palette[register as usize];
                let wanted = img.over_black(x, y);
                for channel in 0..3 {
                    let error = (i32::from(shown[channel]) - i32::from(wanted[channel])).abs();
                    assert!(
                        error <= 56,
                        "pixel ({x}, {y}) channel {channel}: shown {shown:?} for {wanted:?}"
                    );
                }
            }
        }
    });
}

#[test]
fn sixel_and_kitty_refuse_bad_frames_without_reading_them() {
    let rect = CellRect {
        column: 0,
        row: 0,
        columns: 10,
        rows: 10,
    };
    let mut pixels = vec![0u8; 16];
    for (width, height, stride) in [
        (0, 4, 16),
        (4, 0, 16),
        (4, 1, 15),
        (4, 4, 16),
        (100_000, 100_000, 400_000),
        (u32::MAX, u32::MAX, usize::MAX),
        (4, 1, usize::MAX),
    ] {
        let frame = Frame {
            pixels: &mut pixels,
            size: BufferSize { width, height },
            stride,
        };
        assert!(
            sixel::encode(&frame, rect, 8, 16, 1 << 20).is_err(),
            "{width}x{height} stride {stride}"
        );
        assert!(
            kitty::encode(&frame, 1, rect, 1 << 20).is_err(),
            "{width}x{height} stride {stride}"
        );
    }
    let mut pixels = vec![0u8; 16];
    let frame = Frame {
        pixels: &mut pixels,
        size: BufferSize {
            width: 2,
            height: 2,
        },
        stride: 8,
    };
    assert!(
        kitty::encode(&frame, 0, rect, 1 << 20).is_err(),
        "image id 0"
    );
}

// ---- kitty ----

#[test]
fn fuzz_kitty_chunks_and_pixels() {
    run("fuzz_kitty", iterations(100) / 10, LIMIT, |case| {
        let mut img = image(&mut case.rng);
        let mut rect = place(&mut case.rng);
        rect.columns = rect.columns.max(1);
        rect.rows = rect.rows.max(1);
        let id = case.rng.range(1, u32::MAX as usize) as u32;
        let max = if case.rng.one_in(4) {
            case.rng.below(12_000)
        } else {
            usize::MAX / 2
        };
        case.note(|| {
            format!(
                "{}x{} stride {} {rect:?} id {id} max {max}",
                img.width, img.height, img.stride
            )
        });
        let bytes = match kitty::encode(&img.frame(), id, rect, max) {
            Ok(bytes) => bytes,
            Err(kitty::Error::TooLarge { bytes, max: m }) => {
                assert_eq!(m, max);
                assert!(bytes > max);
                return;
            }
            Err(other) => panic!("{other}"),
        };
        assert!(bytes.len() <= max);
        // ESC [ s, a cursor move, then the chunks, the last followed by ESC [ u.
        assert!(bytes.starts_with(b"\x1b[s\x1b["), "start");
        assert!(bytes.ends_with(b"\x1b\\\x1b[u"), "end");
        let start = bytes
            .windows(3)
            .position(|w| w == b"\x1b_G")
            .expect("a chunk");
        let mut payload = Vec::new();
        let mut chunks = 0;
        let mut at = start;
        loop {
            expect(&bytes, &mut at, b"\x1b_G");
            let semi = bytes[at..].iter().position(|&b| b == b';').unwrap() + at;
            let control = std::str::from_utf8(&bytes[at..semi]).unwrap().to_owned();
            let end = bytes[semi..]
                .windows(2)
                .position(|w| w == b"\x1b\\")
                .unwrap()
                + semi;
            let chunk = &bytes[semi + 1..end];
            assert!(chunk.len() <= CHUNK_BYTES, "chunk of {}", chunk.len());
            assert_eq!(chunk.len() % 4, 0, "base64 chunk not whole");
            assert!(chunk
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')));
            let more = control.ends_with("m=1");
            assert!(more || control.ends_with("m=0"), "{control}");
            if chunks == 0 {
                assert!(
                    control.starts_with(&format!(
                        "a=T,f=32,o=z,t=d,s={},v={},i={id},c={},r={},C=1,q=2,",
                        img.width, img.height, rect.columns, rect.rows
                    )),
                    "{control}"
                );
            } else {
                assert_eq!(control, format!("q=2,m={}", u8::from(more)));
            }
            if more {
                assert_eq!(chunk.len(), CHUNK_BYTES, "a middle chunk is full");
            }
            payload.extend_from_slice(chunk);
            chunks += 1;
            at = end + 2;
            if !more {
                break;
            }
        }
        assert_eq!(&bytes[at..], b"\x1b[u");
        let compressed = base64::engine::general_purpose::STANDARD
            .decode(&payload)
            .expect("valid base64");
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&compressed[..])
            .read_to_end(&mut raw)
            .expect("valid zlib");
        assert_eq!(raw.len(), img.width as usize * img.height as usize * 4);
        for y in 0..img.height as usize {
            for x in 0..img.width as usize {
                let at = y * img.stride + x * 4;
                let (b, g, r, a) = (
                    img.pixels[at],
                    img.pixels[at + 1],
                    img.pixels[at + 2],
                    img.pixels[at + 3],
                );
                let out = &raw[(y * img.width as usize + x) * 4..][..4];
                assert_eq!(out[3], a, "alpha at ({x}, {y})");
                if a == 255 || a == 0 {
                    if a == 255 {
                        assert_eq!(out[..3], [r, g, b], "opaque pixel ({x}, {y})");
                    }
                } else if b <= a && g <= a && r <= a {
                    // Straight colour times alpha gives back the premultiplied one.
                    for (straight, premult) in out[..3].iter().zip([r, g, b]) {
                        let back = (u32::from(*straight) * u32::from(a) + 127) / 255;
                        assert!(
                            back.abs_diff(u32::from(premult)) <= 2,
                            "pixel ({x}, {y}): {out:?} for {:?}",
                            [r, g, b, a]
                        );
                    }
                }
            }
        }
    });
}

// ---- the grid ----

#[derive(Clone, PartialEq, Debug)]
struct Shown {
    text: String,
    fg: Color,
    bg: Color,
    attrs: Attrs,
}

/// A terminal that understands what `Grid::diff` writes: cursor moves, SGR, text.
struct Terminal {
    cols: usize,
    rows: usize,
    cells: Vec<Option<Shown>>,
    x: usize,
    y: usize,
    fg: Color,
    bg: Color,
    attrs: Attrs,
}

impl Terminal {
    fn new(cols: usize, rows: usize) -> Self {
        let blank = Shown {
            text: " ".into(),
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::default(),
        };
        Self {
            cols,
            rows,
            cells: vec![Some(blank); cols * rows],
            x: 0,
            y: 0,
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::default(),
        }
    }

    fn sgr(&mut self, params: &str) {
        let numbers: Vec<u32> = if params.is_empty() {
            vec![0]
        } else {
            params.split(';').map(|p| p.parse().unwrap_or(0)).collect()
        };
        let mut i = 0;
        while i < numbers.len() {
            match numbers[i] {
                0 => {
                    self.fg = Color::Default;
                    self.bg = Color::Default;
                    self.attrs = Attrs::default();
                }
                1 => self.attrs.bold = true,
                22 => self.attrs.bold = false,
                3 => self.attrs.italic = true,
                23 => self.attrs.italic = false,
                4 => self.attrs.underline = true,
                24 => self.attrs.underline = false,
                7 => self.attrs.reverse = true,
                27 => self.attrs.reverse = false,
                39 => self.fg = Color::Default,
                49 => self.bg = Color::Default,
                code @ (38 | 48) => {
                    let color = match numbers.get(i + 1) {
                        Some(5) => {
                            i += 2;
                            Color::Indexed(numbers[i] as u8)
                        }
                        Some(2) => {
                            let c = Color::Rgb(
                                numbers[i + 2] as u8,
                                numbers[i + 3] as u8,
                                numbers[i + 4] as u8,
                            );
                            i += 4;
                            c
                        }
                        other => panic!("SGR {code} then {other:?}"),
                    };
                    if code == 38 {
                        self.fg = color
                    } else {
                        self.bg = color
                    }
                }
                other => panic!("SGR {other}"),
            }
            i += 1;
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == 0x1b {
                assert_eq!(bytes[at + 1], b'[', "only CSI is written");
                let mut end = at + 2;
                while !(0x40..=0x7e).contains(&bytes[end]) {
                    end += 1;
                }
                let params = std::str::from_utf8(&bytes[at + 2..end]).unwrap();
                match bytes[end] {
                    b'H' => {
                        let mut parts = params.split(';');
                        let line: usize = parts
                            .next()
                            .filter(|p| !p.is_empty())
                            .map_or(1, |p| p.parse().unwrap());
                        let column: usize = parts
                            .next()
                            .filter(|p| !p.is_empty())
                            .map_or(1, |p| p.parse().unwrap());
                        assert!(
                            line >= 1 && line <= self.rows && column >= 1 && column <= self.cols,
                            "move to {line};{column}"
                        );
                        (self.y, self.x) = (line - 1, column - 1);
                    }
                    b'C' => self.x += params.parse::<usize>().unwrap_or(1),
                    b'D' => {
                        let n = params.parse::<usize>().unwrap_or(1);
                        assert!(n <= self.x, "move left past the edge");
                        self.x -= n;
                    }
                    b'm' => self.sgr(params),
                    b'h' | b'l' => assert_eq!(params, "?2026"),
                    other => panic!("CSI {params:?} {}", other as char),
                }
                at = end + 1;
                continue;
            }
            let mut end = at;
            while end < bytes.len() && bytes[end] != 0x1b {
                end += 1;
            }
            let text = std::str::from_utf8(&bytes[at..end]).expect("text is UTF-8");
            assert!(
                !text.chars().any(char::is_control),
                "a control character is written: {text:?}"
            );
            for cluster in text.graphemes(true) {
                let width = cluster.width();
                assert!(width >= 1, "a zero-width cluster {cluster:?}");
                assert!(
                    self.x + width <= self.cols,
                    "writes past the edge at ({}, {})",
                    self.x,
                    self.y
                );
                let shown = Shown {
                    text: cluster.to_owned(),
                    fg: self.fg,
                    bg: self.bg,
                    attrs: self.attrs,
                };
                self.cells[self.y * self.cols + self.x] = Some(shown);
                for extra in 1..width {
                    self.cells[self.y * self.cols + self.x + extra] = None;
                }
                self.x += width;
            }
            at = end;
        }
    }
}

fn grid_text(rng: &mut Rng) -> String {
    const PIECES: &[&str] = &[
        "a",
        "b",
        " ",
        "é",
        "e\u{301}",
        "日本",
        "ｱ",
        "😀",
        "👩\u{200d}👩\u{200d}👧",
        "🇸🇪",
        "\u{301}",
        "\u{200d}",
        "\u{fe0f}",
        "x\u{fe0f}",
        "한",
        "ก\u{e33}",
        "\u{1f3fd}",
        "~",
        "[",
        "]",
    ];
    let mut text = String::new();
    for _ in 0..rng.range(0, 12) {
        text.push_str(if rng.one_in(6) { "" } else { rng.pick(PIECES) });
        if rng.one_in(8) {
            text.push_str(&rng.string(3));
        }
    }
    // Prepend characters (U+0600..U+0605, U+06DD, ...) join with whatever
    // is written in the next cell, which a terminal does and a cell grid does not model. Text
    // drawn in one piece is segmented first, so this only shows where two pieces meet; the test
    // leaves them out rather than pretend the grid decides it.
    sanitize(&text)
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{600}'
                    ..='\u{605}'
                        | '\u{6dd}'
                        | '\u{70f}'
                        | '\u{890}'
                        | '\u{891}'
                        | '\u{8e2}'
                        | '\u{110bd}'
                        | '\u{110cd}'
            )
        })
        .collect()
}

fn color(rng: &mut Rng) -> Color {
    match rng.below(4) {
        0 => Color::Default,
        1 => Color::Indexed(rng.byte()),
        2 => Color::Rgb(rng.byte(), rng.byte(), rng.byte()),
        _ => Color::Indexed(rng.byte() % 8),
    }
}

#[test]
fn fuzz_grid_diff_replays_to_the_grid() {
    run("fuzz_grid", iterations(100) / 4, LIMIT, |case| {
        let (mut cols, mut rows) = (case.rng.range(1, 30), case.rng.range(1, 10));
        let mut grid = Grid::new(cols as u16, rows as u16);
        let mut terminal = Terminal::new(cols, rows);
        for round in 0..case.rng.range(1, 8) {
            if case.rng.one_in(8) {
                // A resize: a new grid, and the terminal is cleared.
                (cols, rows) = (case.rng.range(1, 30), case.rng.range(1, 10));
                grid = Grid::new(cols as u16, rows as u16);
                terminal = Terminal::new(cols, rows);
            }
            for _ in 0..case.rng.range(0, 10) {
                let (col, row) = (
                    case.rng.below(cols + 2) as u16,
                    case.rng.below(rows + 1) as u16,
                );
                let text = grid_text(&mut case.rng);
                let attrs = Attrs {
                    bold: case.rng.one_in(3),
                    italic: case.rng.one_in(4),
                    underline: case.rng.one_in(3),
                    reverse: case.rng.one_in(4),
                };
                let (fg, bg) = (color(&mut case.rng), color(&mut case.rng));
                case.note(|| format!("round {round}: {cols}x{rows} put {text:?} at {col},{row}"));
                match case.rng.below(5) {
                    0 => grid.clear_row(row),
                    1 => {
                        // One cluster, as the call takes.
                        let cluster = text.graphemes(true).next().unwrap_or("");
                        grid.put_cluster(col, row, cluster, fg, bg, attrs);
                    }
                    _ => {
                        let end = grid.put_text(col, row, &text, fg, bg, attrs);
                        assert!(
                            end >= col && end <= (cols as u16).max(col),
                            "ended at {end} from {col} on {cols} columns"
                        );
                    }
                }
            }
            if case.rng.one_in(6) {
                grid.forget_cursor();
                terminal.x = case.rng.below(cols);
                terminal.y = case.rng.below(rows);
            }
            if case.rng.one_in(6) {
                grid.forget_pen();
                (terminal.fg, terminal.bg, terminal.attrs) = (
                    color(&mut case.rng),
                    color(&mut case.rng),
                    Attrs {
                        bold: true,
                        underline: true,
                        ..Attrs::default()
                    },
                );
            }
            let diff = grid.diff();
            terminal.write(&diff);
            for row in 0..rows {
                for col in 0..cols {
                    let cell = grid.cell(col as u16, row as u16).unwrap();
                    if cell.width == 0 {
                        continue;
                    }
                    let shown = terminal.cells[row * cols + col]
                        .as_ref()
                        .unwrap_or_else(|| {
                            panic!("cell ({col}, {row}) is half of a wide cluster on the terminal")
                        });
                    assert_eq!(
                        (shown.text.as_str(), shown.fg, shown.bg, shown.attrs),
                        (cell.grapheme.as_str(), cell.fg, cell.bg, cell.attrs),
                        "cell ({col}, {row}) after round {round}"
                    );
                }
            }
            // Nothing changed since: nothing is written.
            assert!(grid.diff().is_empty(), "a second diff is not empty");
        }
    });
}

#[test]
fn fuzz_sanitize() {
    run("fuzz_sanitize_tui", iterations(100) * 2, LIMIT, |case| {
        let pieces = [
            "\x1b[31m",
            "\x1b]52;c;aGk=\x07",
            "\x1bP1+r\x1b\\",
            "\x1b_Gx\x1b\\",
            "\u{9b}31m",
            "\u{9d}0;x\u{9c}",
            "\u{202e}",
            "\u{2066}",
            "\u{200f}",
            "\u{61c}",
            "\n",
            "\t",
            "\r",
            "\0",
            "\x7f",
            "\u{85}",
            "ok",
            "é",
            "😀",
            "\x1b",
            "\x1b[",
            "\x1b]",
            "\u{90}",
            "\u{98}",
            "\u{9e}",
        ];
        let mut text = case.rng.string_from(&pieces, 14);
        if case.rng.one_in(3) {
            text = case.rng.mutate(&text);
        }
        case.note(|| format!("{text:?}"));
        let clean = sanitize(&text);
        assert!(!clean.chars().any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{61c}')), "{clean:?}");
        assert!(clean.len() <= text.len());
        assert_eq!(sanitize(&clean), clean, "idempotent");
    });
}

// ---- text mode ----

/// Whether a line of `spans` fits `width` cells. A single grapheme cluster wider than the page
/// (a family emoji at width 1) can't be split, so it stands alone on its line.
fn fits(spans: &[sighurt_tui::textmode::Span], width: u16) -> bool {
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    text.width() <= usize::from(width) || text.graphemes(true).count() == 1
}

fn snapshot(rng: &mut Rng) -> SemanticSnapshot {
    const KINDS: &[NodeKind] = &[
        NodeKind::Container,
        NodeKind::Paragraph,
        NodeKind::Text,
        NodeKind::Link,
        NodeKind::Button,
        NodeKind::TextField,
        NodeKind::TextArea,
        NodeKind::Checkbox,
        NodeKind::Radio,
        NodeKind::Select,
        NodeKind::Image,
        NodeKind::List,
        NodeKind::ListItem,
        NodeKind::Table,
        NodeKind::Row,
        NodeKind::Cell,
        NodeKind::Pre,
        NodeKind::Code,
        NodeKind::Quote,
        NodeKind::Separator,
        NodeKind::Heading(1),
        NodeKind::Heading(6),
    ];
    let count = rng.range(1, 60);
    let mut nodes: Vec<SemanticNode> = (0..count)
        .map(|i| {
            let kind = if i == 0 {
                NodeKind::Document
            } else {
                *rng.pick(KINDS)
            };
            let words = [
                "hello",
                "wörld",
                "日本語のテキスト",
                "😀",
                "a\nb",
                "\x1b[31mred\x1b[0m",
                "x\u{202e}y",
                "\t",
                "verylongwordwithoutanyspacesatall_".repeat(5).leak(),
                "👩\u{200d}👩\u{200d}👧",
                "e\u{301}",
                "ｱｲｳ",
            ];
            let text = rng
                .one_in(2)
                .then(|| rng.string_from(&words, 12).replace("\u{0}", ""));
            let options: Vec<String> = if kind == NodeKind::Select {
                (0..rng.below(5))
                    .map(|_| rng.string_from(&words, 3))
                    .collect()
            } else {
                Vec::new()
            };
            SemanticNode {
                id: NodeId(i as u64 + 1),
                kind,
                text,
                href: (kind == NodeKind::Link && rng.one_in(2))
                    .then(|| format!("https://a.test/{}", rng.string(10))),
                value: rng.one_in(3).then(|| rng.string_from(&words, 4)),
                label: rng.one_in(3).then(|| rng.string_from(&words, 4)),
                checked: rng.one_in(3).then(|| rng.one_in(2)),
                selected: (!options.is_empty()).then_some(0),
                options,
                disabled: rng.one_in(6),
                password: kind == NodeKind::TextField && rng.one_in(4),
                children: Vec::new(),
            }
        })
        .collect();
    for node in &mut nodes {
        if node.password {
            node.value = None;
        }
    }
    // A tree: each node after the first is a child of an earlier one.
    for i in 1..count {
        let parent = rng.below(i);
        let id = nodes[i].id;
        nodes[parent].children.push(id);
    }
    SemanticSnapshot {
        document: DocumentGeneration::next(),
        root: NodeId(1),
        nodes,
        focus: rng.one_in(3).then(|| NodeId(rng.range(1, count) as u64)),
        truncated: rng.one_in(8),
    }
}

#[test]
fn fuzz_text_mode_layout_respects_the_width() {
    run("fuzz_textmode", iterations(100) / 4, LIMIT, |case| {
        let snap = snapshot(&mut case.rng);
        snap.check().expect("the generator makes valid snapshots");
        let width = case.rng.range(1, 120) as u16;
        case.note(|| format!("width {width}: {snap:?}"));
        let doc = textmode::layout(&snap, width);
        assert_eq!(doc.width, width);
        for (n, line) in doc.lines.iter().enumerate() {
            // (Below 8 columns a list's bullet and a wide emoji don't fit one line together.)
            assert!(
                width < 8 || fits(&line.spans, width),
                "line {n} is wider than {width}: {:?}",
                line.spans
            );
            for span in &line.spans {
                assert!(!span.text.chars().any(char::is_control), "{:?}", span.text);
            }
        }
        for item in &doc.items {
            for range in &item.ranges {
                assert!(
                    range.line < doc.lines.len() && range.start <= range.end,
                    "{range:?}"
                );
                // (A cluster wider than the page stands alone on a line, past its edge.)
                let line_cells: usize = doc.lines[range.line]
                    .spans
                    .iter()
                    .map(|s| s.text.width())
                    .sum();
                assert!(
                    width < 8 || usize::from(range.end) <= usize::from(width).max(line_cells),
                    "{range:?} in width {width}"
                );
            }
            if item.password {
                assert!(item.value.is_empty());
            }
        }
        // The dump has no controls (apart from the newline between lines) and ends with one.
        let dump = textmode::dump_document(&doc);
        assert!(dump.is_empty() || dump.ends_with('\n'));
        assert!(
            !dump.chars().any(|c| c.is_control() && c != '\n'),
            "{dump:?}"
        );
        // Painting it anywhere in a grid, scrolled anywhere, never panics.
        let (cols, rows) = (case.rng.range(1, 90) as u16, case.rng.range(1, 30) as u16);
        let mut grid = Grid::new(cols, rows);
        let mut view = TextView::new(width);
        for _ in 0..case.rng.range(0, 6) {
            match case.rng.below(6) {
                0 => view.scroll_lines(&doc, rows, case.rng.range(0, 40) as isize - 20),
                1 => view.scroll_pages(&doc, rows, case.rng.range(0, 6) as isize - 3),
                2 => view.focus_next(&doc, rows),
                3 => view.focus_previous(&doc, rows),
                4 => {
                    let _ = view.click(&doc, case.rng.below(120) as u16, case.rng.below(40) as u16);
                }
                _ => view.scroll_bottom(&doc, rows),
            }
            let _ = view.activate(&doc);
        }
        let area = Area {
            col: case.rng.below(usize::from(cols) + 2) as u16,
            row: case.rng.below(usize::from(rows) + 2) as u16,
            cols: case.rng.range(0, 100) as u16,
            rows: case.rng.range(0, 40) as u16,
        };
        textmode::draw(&doc, &view, &mut grid, area);
        let _ = grid.diff();
        // Laying out again at another width keeps the view usable.
        let other = textmode::layout(&snap, case.rng.range(1, 120) as u16);
        view.retarget(&doc, &other);
        textmode::draw(&other, &view, &mut grid, area);
    });
}

fn key_event(rng: &mut Rng) -> KeyEvent {
    let names = [
        "Enter",
        "Escape",
        "Tab",
        "ArrowUp",
        "ArrowDown",
        "ArrowLeft",
        "ArrowRight",
        "PageUp",
        "PageDown",
        "Home",
        "End",
        "Backspace",
        "Delete",
        "Space",
        "F5",
        "",
    ];
    let key = if rng.one_in(2) {
        Key::Named((*rng.pick(&names)).to_owned())
    } else {
        Key::Character(rng.string(2))
    };
    KeyEvent {
        code: KeyCode(rng.below(300) as u32),
        text: matches!(key, Key::Character(_))
            .then(|| rng.string(2))
            .filter(|t| !t.is_empty() && !t.chars().any(char::is_control)),
        key,
        state: if rng.one_in(4) {
            ButtonState::Released
        } else {
            ButtonState::Pressed
        },
        repeat: rng.one_in(5),
        modifiers: Modifiers {
            shift: rng.one_in(4),
            control: rng.one_in(4),
            alt: rng.one_in(4),
            ..Modifiers::default()
        },
        consumed: Modifiers::default(),
    }
}

/// The text pages, driven with random snapshots, keys, clicks, pastes and resizes: no panic, and
/// the grid they paint into stays the size it was.
#[test]
fn fuzz_text_pages_driven_by_input() {
    run("fuzz_text_pages", iterations(100) / 8, LIMIT, |case| {
        let mut pages = TextPages::new(case.rng.range(1, 100) as u16, case.rng.range(1, 30) as u16);
        let views = [ViewId::next(), ViewId::next()];
        let mut grid = Grid::new(case.rng.range(1, 100) as u16, case.rng.range(1, 32) as u16);
        for step in 0..case.rng.range(1, 30) {
            let view = *case.rng.pick(&views);
            case.note(|| format!("step {step}"));
            match case.rng.below(10) {
                0 => pages.snapshot(view, Box::new(snapshot(&mut case.rng))),
                1 => pages.set_area(case.rng.range(1, 100) as u16, case.rng.range(1, 30) as u16),
                2 | 3 => {
                    let key = key_event(&mut case.rng);
                    if pages.prompting() {
                        let _ = pages.prompt_key(&key);
                    } else {
                        let _ = pages.page_key(view, &key);
                    }
                }
                4 => {
                    let _ =
                        pages.click(view, case.rng.below(120) as u16, case.rng.below(40) as u16);
                }
                5 => pages.wheel(view, case.rng.range(0, 60) as isize - 30),
                6 => {
                    let text = case.rng.string(40);
                    let _ = pages.paste(&text);
                }
                7 => pages.forget(view),
                8 => {
                    let key = key_event(&mut case.rng);
                    let _ = pages.prompt_key(&key);
                }
                _ => {
                    let area = Area {
                        col: case.rng.below(110) as u16,
                        row: case.rng.below(34) as u16,
                        cols: case.rng.range(0, 110) as u16,
                        rows: case.rng.range(0, 34) as u16,
                    };
                    let before = (grid.cols(), grid.rows());
                    let cursor = pages.draw(view, &mut grid, area);
                    assert_eq!((grid.cols(), grid.rows()), before);
                    if let Some((col, row)) = cursor {
                        assert!(
                            col < grid.cols() && row < grid.rows(),
                            "cursor ({col}, {row}) off the grid"
                        );
                    }
                    let _ = grid.diff();
                }
            }
        }
    });
}

/// Found by `fuzz_text_mode_layout_respects_the_width`: the "(truncated)" note of a cut-off
/// snapshot was one line however narrow the page, 11 cells at width 2.
#[test]
fn the_truncation_note_wraps_with_the_page() {
    let mut rng = Rng::new(7);
    let mut snap = snapshot(&mut rng);
    snap.truncated = true;
    for width in 1..=14u16 {
        let doc = textmode::layout(&snap, width);
        for line in &doc.lines {
            assert!(fits(&line.spans, width), "width {width}: {:?}", line.spans);
        }
        let dump = textmode::dump_document(&doc);
        let letters: String = dump.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            letters.contains("(truncated)"),
            "the note is kept in width {width}"
        );
    }
}

/// Found by `fuzz_grid_diff_replays_to_the_grid`: a cluster made of combining marks and a skin
/// tone modifier ("\u{301}\u{1F3FD}") was written as a cell of its own, and the terminal joined
/// it to the cell before. It has no base, so it is not a cell.
#[test]
fn a_cluster_with_no_base_is_not_a_cell() {
    let mut grid = Grid::new(10, 1);
    let mut terminal = Terminal::new(10, 1);
    grid.put_cluster(0, 0, "a", Color::Default, Color::Default, Attrs::default());
    for orphan in [
        "\u{301}\u{1F3FD}",
        "\u{301}\u{200d}",
        "\u{1F3FD}",
        "\u{fe0f}",
        "\u{301}\u{301}",
    ] {
        assert_eq!(
            grid.put_cluster(
                1,
                0,
                orphan,
                Color::Default,
                Color::Default,
                Attrs::default()
            ),
            0,
            "{orphan:?}"
        );
    }
    assert_eq!(
        grid.put_cluster(
            2,
            0,
            "\u{301}b",
            Color::Default,
            Color::Default,
            Attrs::default()
        ),
        1,
        "marks before a base are dropped, the base kept"
    );
    terminal.write(&grid.diff());
    assert_eq!(terminal.cells[0].as_ref().unwrap().text, "a");
    assert_eq!(terminal.cells[1].as_ref().unwrap().text, " ");
    assert_eq!(terminal.cells[2].as_ref().unwrap().text, "b");
}
