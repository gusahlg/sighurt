//! Sixel decoding: the bytes of a sixel string (after `ESC P P1;P2;P3 q`) into palette indices,
//! within bounds.
//!
//! It owns the sixel grammar (sixels, repeats, colour registers, raster attributes, `$` and `-`),
//! the image buffer and its bounds. Work per input byte is bounded by a constant: a repeat is at
//! most six `slice::fill` calls, and the buffer grows by doubling its width and by six-pixel
//! bands. It must not own finding the DCS in the stream, placing or drawing the image, or
//! encoding PNG.

/// A decoded sixel image.
pub struct Sixel {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA; index 0 is the background (alpha 0 when the introducer's P2 was 1), indices 1..=255
    /// are the colour registers.
    pub palette: Vec<[u8; 4]>,
    /// One index per pixel, row-major, `width * height` of them.
    pub indices: Vec<u8>,
}

/// Most parameters a command keeps (`#Pc;Pu;Px;Py;Pz`).
const MAX_PARAMS: usize = 5;

/// The VT340 default palette (libsixel's), in percent.
const DEFAULT_PALETTE: [[u32; 3]; 16] = [
    [0, 0, 0],
    [20, 20, 80],
    [80, 13, 13],
    [20, 80, 20],
    [80, 20, 80],
    [20, 80, 80],
    [80, 80, 20],
    [53, 53, 53],
    [26, 26, 26],
    [33, 33, 60],
    [60, 26, 26],
    [33, 60, 33],
    [60, 33, 60],
    [33, 60, 60],
    [60, 60, 33],
    [80, 80, 80],
];

/// A streaming sixel decoder.
pub struct Decoder {
    cap_w: u32,
    cap_h: u32,
    palette: [[u8; 4]; 256],
    /// The current colour's palette index.
    color: u8,
    x: u32,
    y: u32,
    /// Row-major, `stride` wide and `rows` tall.
    buf: Vec<u8>,
    stride: usize,
    rows: usize,
    /// Largest column and row drawn.
    width: u32,
    height: u32,
    raster_w: u32,
    raster_h: u32,
    truncated: bool,
    /// The command whose numbers are being read: `!`, `#`, `"`, or 0.
    cmd: u8,
    params: [u32; MAX_PARAMS],
    np: usize,
    cur: u32,
    have: bool,
}

/// Percent (0..=100) to a byte, rounded.
fn pct(v: u32) -> u8 {
    ((v.min(100) * 255 + 50) / 100) as u8
}

/// The palette index of colour register `n`.
fn index_of(n: u32) -> u8 {
    (n % 255 + 1) as u8
}

/// libsixel's `hls_to_rgb`: H in degrees (DEC's origin), L and S in percent; RGB in percent.
fn hls(h: u32, l: u32, s: u32) -> [u32; 3] {
    let (l, s) = (l.min(100) as f64, s.min(100) as f64);
    if s == 0.0 {
        let v = l.round() as u32;
        return [v, v, v];
    }
    let max = if l > 50.0 {
        l + s * (100.0 - l) / 100.0
    } else {
        l + s * l / 100.0
    };
    let min = l * 2.0 - max;
    let h = ((h.min(360) + 240) % 360) as f64;
    let d = max - min;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (max, min + d * (h / 60.0), min),
        1 => (min + d * ((120.0 - h) / 60.0), max, min),
        2 => (min, max, min + d * ((h - 120.0) / 60.0)),
        3 => (min, min + d * ((240.0 - h) / 60.0), max),
        4 => (min + d * ((h - 240.0) / 60.0), min, max),
        _ => (max, min, min + d * ((360.0 - h) / 60.0)),
    };
    [r.round() as u32, g.round() as u32, b.round() as u32]
}

impl Decoder {
    /// A decoder for a sixel string whose introducer had `params` (P1;P2;P3; missing ones are 0).
    /// P2 == 1: pixels no sixel sets stay transparent; otherwise they are the background colour
    /// (VT340 colour 0, black). The image never grows past `max_width` x `max_height` or
    /// `max_pixels` pixels.
    pub fn new(params: &[u16], max_width: u32, max_height: u32, max_pixels: usize) -> Self {
        let cap_w = max_width.min(u32::try_from(max_pixels).unwrap_or(u32::MAX));
        let cap_h = if cap_w == 0 {
            0
        } else {
            let by_area = max_pixels / cap_w as usize;
            max_height.min(u32::try_from(by_area).unwrap_or(u32::MAX))
        };
        let transparent = params.get(1) == Some(&1);
        let mut palette = [[0, 0, 0, 255]; 256];
        palette[0][3] = if transparent { 0 } else { 255 };
        for (n, c) in DEFAULT_PALETTE.iter().enumerate() {
            palette[index_of(n as u32) as usize] = [pct(c[0]), pct(c[1]), pct(c[2]), 255];
        }
        Decoder {
            cap_w,
            cap_h,
            palette,
            color: index_of(0),
            x: 0,
            y: 0,
            buf: Vec::new(),
            stride: 0,
            rows: 0,
            width: 0,
            height: 0,
            raster_w: 0,
            raster_h: 0,
            truncated: false,
            cmd: 0,
            params: [0; MAX_PARAMS],
            np: 0,
            cur: 0,
            have: false,
        }
    }

    /// Decodes `bytes` of the string, which may be split anywhere (even inside a number).
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match b {
                b'0'..=b'9' => {
                    if self.cmd != 0 {
                        self.cur = self
                            .cur
                            .saturating_mul(10)
                            .saturating_add((b - b'0') as u32);
                        self.have = true;
                    }
                }
                b';' => {
                    if self.cmd != 0 {
                        self.push();
                        self.cur = 0;
                        self.have = false;
                    }
                }
                0x3F..=0x7E | b'!' | b'#' | b'"' | b'$' | b'-' => self.command(b),
                _ => {}
            }
        }
    }

    /// Whether anything was cut at a bound.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The image, `None` if it has no pixels (nothing drawn and no raster size).
    pub fn finish(mut self) -> Option<Sixel> {
        // A command still reading its numbers ends with the string (a space does nothing else).
        if self.cmd != 0 {
            self.command(b' ');
        }
        let (w, h) = (
            self.width.max(self.raster_w),
            self.height.max(self.raster_h),
        );
        if w == 0 || h == 0 {
            return None;
        }
        let (wu, hu) = (w as usize, h as usize);
        if self.stride == wu && self.rows >= hu {
            self.buf.truncate(wu * hu);
        } else {
            let mut out = vec![0u8; wu * hu];
            let n = wu.min(self.stride);
            for row in 0..hu.min(self.rows) {
                out[row * wu..row * wu + n]
                    .copy_from_slice(&self.buf[row * self.stride..row * self.stride + n]);
            }
            self.buf = out;
        }
        Some(Sixel {
            width: w,
            height: h,
            palette: self.palette.to_vec(),
            indices: self.buf,
        })
    }

    fn push(&mut self) {
        if self.np < MAX_PARAMS {
            self.params[self.np] = self.cur;
            self.np += 1;
        }
    }

    /// Ends the pending command's numbers; applies it, or the repeat to the sixel `b`.
    fn command(&mut self, b: u8) {
        if self.have {
            self.push();
        }
        let (cmd, p, n) = (self.cmd, self.params, self.np);
        self.cmd = 0;
        self.params = [0; MAX_PARAMS];
        self.np = 0;
        self.cur = 0;
        self.have = false;
        let sixel = (0x3F..=0x7E).contains(&b);
        match cmd {
            b'#' if n >= 1 => self.color(&p, n),
            b'"' if n >= 4 => self.raster(p[2], p[3]),
            b'!' if sixel => {
                self.draw(b, if n >= 1 && p[0] > 0 { p[0] } else { 1 });
                return;
            }
            _ => {}
        }
        match b {
            b'!' | b'#' | b'"' => self.cmd = b,
            b'$' => self.x = 0,
            b'-' => {
                self.x = 0;
                self.y = self.y.saturating_add(6);
            }
            b' ' => {}
            _ => self.draw(b, 1),
        }
    }

    /// `#Pc` selects a register; with `;Pu;Px;Py;Pz` it also defines it.
    fn color(&mut self, p: &[u32; MAX_PARAMS], n: usize) {
        let idx = index_of(p[0]);
        self.color = idx;
        if n >= 5 {
            let rgb = match p[1] {
                2 => [p[2], p[3], p[4]],
                1 => hls(p[2], p[3], p[4]),
                _ => return,
            };
            self.palette[idx as usize] = [pct(rgb[0]), pct(rgb[1]), pct(rgb[2]), 255];
        }
    }

    /// `"Pan;Pad;Ph;Pv`: the image is at least Ph x Pv.
    fn raster(&mut self, ph: u32, pv: u32) {
        if ph > self.cap_w || pv > self.cap_h {
            self.truncated = true;
        }
        if ph > 0 && pv > 0 {
            self.raster_w = self.raster_w.max(ph.min(self.cap_w));
            self.raster_h = self.raster_h.max(pv.min(self.cap_h));
        }
    }

    /// Makes the buffer at least `w` wide and `r` rows tall (both within the caps).
    fn ensure(&mut self, w: usize, r: usize) {
        if w > self.stride {
            let ns = w.max(self.stride * 2).max(64).min(self.cap_w as usize);
            let mut nb = vec![0u8; ns * self.rows];
            for row in 0..self.rows {
                nb[row * ns..row * ns + self.stride]
                    .copy_from_slice(&self.buf[row * self.stride..(row + 1) * self.stride]);
            }
            self.buf = nb;
            self.stride = ns;
        }
        if r > self.rows {
            self.rows = r;
            self.buf.resize(self.stride * r, 0);
        }
    }

    /// Draws sixel `c` `count` times at the cursor, in the current colour.
    fn draw(&mut self, c: u8, count: u32) {
        let mask = c - 0x3F;
        if self.y >= self.cap_h {
            self.truncated = true;
            return;
        }
        let n = count.min(self.cap_w.saturating_sub(self.x));
        if n < count {
            self.truncated = true;
        }
        let x0 = self.x;
        self.x = self.x.saturating_add(count).min(self.cap_w);
        if n == 0 {
            return;
        }
        let end = x0 + n;
        self.ensure(
            end as usize,
            self.y.saturating_add(6).min(self.cap_h) as usize,
        );
        self.width = self.width.max(end);
        for bit in 0..6u32 {
            if mask >> bit & 1 == 0 {
                continue;
            }
            let row = self.y + bit;
            if row >= self.cap_h {
                self.truncated = true;
                break;
            }
            let at = row as usize * self.stride + x0 as usize;
            self.buf[at..at + n as usize].fill(self.color);
            self.height = self.height.max(row + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(s: &str) -> Sixel {
        let mut d = Decoder::new(&[], 1000, 1000, 1_000_000);
        d.feed(s.as_bytes());
        d.finish().unwrap()
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    #[test]
    fn one_band_two_colours() {
        let s = dec("#1;2;100;0;0#2;2;0;0;100#1~#2~");
        assert_eq!((s.width, s.height), (2, 6));
        assert_eq!(s.palette[2], RED);
        assert_eq!(s.palette[3], BLUE);
        assert_eq!(s.indices[0], 2);
        assert_eq!(s.indices[1], 3);
        assert_eq!(s.indices[5 * 2], 2);
    }

    #[test]
    fn repeat() {
        let s = dec("#1;2;100;0;0!5~");
        assert_eq!((s.width, s.height), (5, 6));
        assert!(s.indices.iter().all(|&i| i == 2));
        assert_eq!(dec("!0~").width, 1);
    }

    #[test]
    fn carriage_return_overdraws() {
        let s = dec("#1;2;100;0;0!3~$#2;2;0;0;100@@");
        assert_eq!((s.width, s.height), (3, 6));
        assert_eq!(&s.indices[0..3], &[3, 3, 2]);
        assert_eq!(&s.indices[3..6], &[2, 2, 2]);
    }

    #[test]
    fn two_bands() {
        let s = dec("#1;2;100;0;0~-~");
        assert_eq!((s.width, s.height), (1, 12));
        assert!(s.indices.iter().all(|&i| i == 2));
    }

    #[test]
    fn partial_band_height() {
        // '@' sets only the top pixel of the second band.
        assert_eq!(dec("~-@").height, 7);
    }

    #[test]
    fn hls_red() {
        let s = dec("#1;1;120;50;100~");
        assert_eq!(s.palette[2], RED);
        assert_eq!(dec("#1;1;0;50;100~").palette[2], BLUE);
        assert_eq!(dec("#1;1;240;50;100~").palette[2], [0, 255, 0, 255]);
    }

    #[test]
    fn raster_sets_size() {
        let s = dec("\"1;1;7;9");
        assert_eq!((s.width, s.height), (7, 9));
        assert_eq!(s.indices.len(), 63);
        assert!(s.indices.iter().all(|&i| i == 0));
    }

    #[test]
    fn transparency() {
        let mut d = Decoder::new(&[0, 1, 0], 10, 10, 100);
        d.feed(b"~");
        assert_eq!(d.finish().unwrap().palette[0][3], 0);
        let mut d = Decoder::new(&[0, 0], 10, 10, 100);
        d.feed(b"~");
        assert_eq!(d.finish().unwrap().palette[0][3], 255);
    }

    #[test]
    fn default_palette() {
        let s = dec("#1~");
        assert_eq!(s.palette[2], [51, 51, 204, 255]);
    }

    #[test]
    fn bounds() {
        let mut d = Decoder::new(&[], 100, 100, 1_000_000);
        d.feed(b"!65535~");
        assert!(d.truncated());
        let s = d.finish().unwrap();
        assert_eq!((s.width, s.height), (100, 6));

        let mut d = Decoder::new(&[], 100, 20, 1_000_000);
        d.feed(&b"~-".repeat(50));
        assert!(d.truncated());
        assert_eq!(d.finish().unwrap().height, 20);

        let mut d = Decoder::new(&[], 100, 100, 250);
        d.feed(&b"!100~-".repeat(50));
        let s = d.finish().unwrap();
        assert!(s.indices.len() <= 250);
        assert_eq!(s.indices.len(), (s.width * s.height) as usize);

        let mut d = Decoder::new(&[], 10, 10, 1_000_000);
        // The raster attributes apply when the next command starts.
        d.feed(b"\"1;1;500;500$");
        assert!(d.truncated());
        let s = d.finish().unwrap();
        assert_eq!((s.width, s.height), (10, 10));
    }

    #[test]
    fn zero_bounds_do_not_panic() {
        let mut d = Decoder::new(&[], 0, 0, 0);
        d.feed(b"#1;2;1;2;3!9~-~\"1;1;5;5");
        assert!(d.finish().is_none());
    }

    #[test]
    fn split_anywhere() {
        let src = b"\"1;1;30;12#1;2;100;0;0#2;1;120;50;100!20~#3;2;0;100;0$-!7@#1~~\n\r-?";
        let whole = {
            let mut d = Decoder::new(&[], 100, 100, 10_000);
            d.feed(src);
            d.finish().unwrap()
        };
        let mut d = Decoder::new(&[], 100, 100, 10_000);
        for b in src {
            d.feed(std::slice::from_ref(b));
        }
        let split = d.finish().unwrap();
        assert_eq!((whole.width, whole.height), (split.width, split.height));
        assert_eq!(whole.palette, split.palette);
        assert_eq!(whole.indices, split.indices);
    }

    #[test]
    fn registers_wrap() {
        let a = dec("#300;2;100;0;0~");
        let b = dec(&format!("#{};2;100;0;0~", 300 % 255));
        assert_eq!(a.indices, b.indices);
        assert_eq!(a.palette[(300 % 255 + 1) as usize], RED);
        assert_eq!(a.indices[0] as u32, 300 % 255 + 1);
    }

    #[test]
    fn saturating_numbers() {
        let mut d = Decoder::new(&[], 10, 10, 100);
        d.feed(b"!99999999999999999999999~");
        assert_eq!(d.finish().unwrap().width, 10);
    }

    #[test]
    fn empty_is_none() {
        assert!(Decoder::new(&[], 10, 10, 100).finish().is_none());
        let mut d = Decoder::new(&[], 10, 10, 100);
        d.feed(b"#1;2;1;1;1 \n");
        assert!(d.finish().is_none());
    }

    #[test]
    fn work_is_fill_based() {
        let start = std::time::Instant::now();
        let mut d = Decoder::new(&[], 4096, 6, 4096 * 6);
        for _ in 0..10_000 {
            d.feed(b"!4096~");
        }
        assert_eq!(d.finish().unwrap().width, 4096);
        assert!(start.elapsed().as_secs() < 5);
    }
}
