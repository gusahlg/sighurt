//! The chrome strip: a fake address bar drawn on the CPU with tiny-skia and cosmic-text.

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
use sighurt_api::geometry::BufferSize;
use tiny_skia::{Paint, Pixmap, Rect, Transform};

/// The strip's height in logical pixels.
pub const HEIGHT: u32 = 40;

const BACKGROUND: [u8; 3] = [0x2b, 0x2d, 0x31];
const FIELD: [u8; 3] = [0x1e, 0x1f, 0x22];
const OUTLINE: [u8; 3] = [0x4e, 0x50, 0x58];
const OUTLINE_FOCUSED: [u8; 3] = [0x3d, 0x8b, 0xfd];
const TEXT: [u8; 3] = [0xe8, 0xe8, 0xea];

pub struct Chrome {
    /// The address field's text. Keys routed to the chrome edit it.
    pub text: String,
    /// Whether the address field has logical focus (and the window has keyboard focus).
    pub focused: bool,
    fonts: FontSystem,
    glyphs: SwashCache,
    pixmap: Option<Pixmap>,
}

impl Chrome {
    pub fn new(text: String) -> Self {
        Self {
            text,
            focused: false,
            fonts: FontSystem::new(),
            glyphs: SwashCache::new(),
            pixmap: None,
        }
    }

    /// Paints the whole strip at `size` buffer pixels and `scale` into `out`, XRGB8888 with
    /// rows of `size.width × 4` bytes. The whole buffer is painted every time: the buffer being
    /// reused may hold an older frame (§10).
    pub fn paint(&mut self, size: BufferSize, scale: f32, out: &mut [u8]) {
        let pixmap = match &mut self.pixmap {
            Some(pixmap) if pixmap.width() == size.width && pixmap.height() == size.height => {
                pixmap
            }
            slot => slot.insert(Pixmap::new(size.width, size.height).expect("chrome size")),
        };
        let [r, g, b] = BACKGROUND;
        pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, 255));

        let px = |logical: f32| logical * scale;
        let (w, h) = (size.width as f32, size.height as f32);
        let outline = if self.focused {
            OUTLINE_FOCUSED
        } else {
            OUTLINE
        };
        fill(
            pixmap,
            px(8.0),
            px(6.0),
            w - px(16.0),
            h - px(12.0),
            outline,
        );
        fill(pixmap, px(9.0), px(7.0), w - px(18.0), h - px(14.0), FIELD);

        let metrics = Metrics::new(px(14.0), px(20.0));
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_size(&mut self.fonts, Some(w - px(40.0)), Some(px(20.0)));
        buffer.set_text(
            &mut self.fonts,
            &self.text,
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
        );
        buffer.shape_until_scroll(&mut self.fonts, false);
        let (left, top) = (px(18.0).round() as i32, px(10.0).round() as i32);
        let [r, g, b] = TEXT;
        buffer.draw(
            &mut self.fonts,
            &mut self.glyphs,
            Color::rgb(r, g, b),
            |x, y, _, _, color| blend(pixmap, left + x, top + y, color),
        );
        if self.focused {
            let end = buffer
                .layout_runs()
                .map(|run| run.line_w)
                .fold(0.0, f32::max);
            fill(
                pixmap,
                left as f32 + end + px(1.0),
                px(10.0),
                px(1.5),
                px(20.0),
                TEXT,
            );
        }

        // tiny-skia stores R G B A per pixel; XRGB8888 is a little-endian 0xXXRRGGBB word,
        // B G R X in memory. The strip is opaque, so premultiplication changes nothing.
        let pixels = pixmap.data().as_chunks::<4>().0;
        for (dst, src) in out.as_chunks_mut::<4>().0.iter_mut().zip(pixels) {
            *dst = [src[2], src[1], src[0], 0xff];
        }
    }
}

/// Fills a rectangle snapped to whole pixels, so edges stay crisp at fractional scales.
fn fill(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, [r, g, b]: [u8; 3]) {
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, 255);
    paint.anti_alias = false;
    let (x, y) = (x.round(), y.round());
    let (w, h) = (w.round().max(1.0), h.round().max(1.0));
    if let Some(rect) = Rect::from_xywh(x, y, w, h) {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
}

/// Blends one glyph coverage pixel over the opaque pixmap.
fn blend(pixmap: &mut Pixmap, x: i32, y: i32, color: Color) {
    let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
    if x < 0 || y < 0 || x >= width || y >= height {
        return;
    }
    let i = (y * width + x) as usize * 4;
    let [r, g, b, a] = color.as_rgba();
    let a = u32::from(a);
    let pixel = &mut pixmap.data_mut()[i..i + 3];
    for (channel, source) in pixel.iter_mut().zip([r, g, b]) {
        *channel = ((u32::from(source) * a + u32::from(*channel) * (255 - a)) / 255) as u8;
    }
}
