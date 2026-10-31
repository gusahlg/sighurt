//! Which buffer pixels a new frame changed, so the compositor and the rasterizer can skip the rest.
//!
//! A reused SHM buffer still holds an older frame. [`region_for_age`] is the union of the
//! per-frame diffs since that buffer's contents: that is what `wl_surface.damage_buffer` must
//! name, and what the rasterizer may limit itself to. Age 0 is a new or undefined buffer, so
//! the region is the whole frame. A diff that cannot be bounded (a clip that moved, a clear
//! that changed colour, a size or scale change, two lists that do not line up) is the whole
//! frame too. An empty diff is as well: a Wayland commit with no damage is easy to read as
//! "the previous frame", and the buffer would then show pixels the guest replaced.
//!
//! Owns: the rectangles. Must not own: painting them ([`crate::raster`]) or sending them
//! ([`sighurt_wayland`]).

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use sighurt_wayland::DamageRect;

use crate::capabilities::DecodedImage;
use crate::draw::DrawOp;
use crate::text;

/// A changed region of one buffer. [`Region::Full`] is every pixel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Region {
    /// Every pixel. The buffer is new, or the diff cannot be bounded.
    Full,
    /// These rectangles, already coalesced and clipped to the buffer.
    Rects(Vec<DamageRect>),
}

/// One painted frame, as far as the next diff needs it.
pub struct Snap<'a> {
    /// The guest's ops. Empty when the view has no list.
    pub ops: &'a [DrawOp],
    /// The images those ops draw. An [`DrawOp::Image`] whose `Arc` changed is dirty even when
    /// the op compares equal.
    pub images: &'a [Arc<DecodedImage>],
    /// Buffer pixels per canvas pixel.
    pub scale: (f32, f32),
    /// The prompt, in logical pixels. It has no images.
    pub overlay: &'a [DrawOp],
    /// Buffer pixels per logical pixel.
    pub overlay_scale: (f32, f32),
    /// The buffer the frame was painted into.
    pub buffer: (u32, u32),
    /// The canvas size the ops were drawn for.
    pub canvas: (u32, u32),
}

/// How far, in buffer pixels, a shape's anti-aliased edge may fall outside its geometry.
const OUTSET: f32 = 2.0;
/// Rectangles at most this many pixels apart are one rectangle.
const COALESCE_GAP: i32 = 4;
/// More rectangles than this become their bounding box.
const MAX_RECTS: usize = 16;
/// Per-frame diffs kept. A buffer's age is at most the swapchain's three slots, plus one.
const HISTORY: usize = 4;

/// The pixels of `current` that differ from `previous`. `previous` of `None` is the whole buffer.
pub fn diff(previous: Option<&Snap<'_>>, current: &Snap<'_>) -> Region {
    let Some(previous) = previous else {
        return Region::Full;
    };
    if previous.buffer != current.buffer
        || previous.canvas != current.canvas
        || previous.scale != current.scale
        || previous.overlay_scale != current.overlay_scale
    {
        return Region::Full;
    }
    let mut rects = Vec::new();
    if !changed(
        previous.ops,
        current.ops,
        previous.images,
        current.images,
        current.scale,
        current.buffer,
        &mut rects,
    ) || !changed(
        previous.overlay,
        current.overlay,
        &[],
        &[],
        current.overlay_scale,
        current.buffer,
        &mut rects,
    ) {
        return Region::Full;
    }
    classify(rects, current.buffer)
}

/// `true` when every changed op was bounded into `rects`. `false` when the diff is the whole buffer.
fn changed(
    previous: &[DrawOp],
    current: &[DrawOp],
    previous_images: &[Arc<DecodedImage>],
    current_images: &[Arc<DecodedImage>],
    scale: (f32, f32),
    buffer: (u32, u32),
    rects: &mut Vec<DamageRect>,
) -> bool {
    if previous.len() != current.len() {
        return false;
    }
    for (previous, current) in previous.iter().zip(current) {
        if !same_clip(previous, current) {
            return false;
        }
        if matches!(previous, DrawOp::PushClip { .. } | DrawOp::PopClip) {
            continue;
        }
        if let (DrawOp::Clear { color: a }, DrawOp::Clear { color: b }) = (previous, current) {
            if a != b {
                return false;
            }
            continue;
        }
        let images = image_changed(previous, current, previous_images, current_images);
        if previous != current || images {
            push_bounds(previous, scale, buffer, rects);
            push_bounds(current, scale, buffer, rects);
        }
    }
    true
}

/// Clips line up, including their rectangles. Anything else about the two ops is ignored.
fn same_clip(previous: &DrawOp, current: &DrawOp) -> bool {
    match (previous, current) {
        (
            DrawOp::PushClip {
                x: x1,
                y: y1,
                w: w1,
                h: h1,
            },
            DrawOp::PushClip {
                x: x2,
                y: y2,
                w: w2,
                h: h2,
            },
        ) => (x1, y1, w1, h1) == (x2, y2, w2, h2),
        (DrawOp::PopClip, DrawOp::PopClip) => true,
        (DrawOp::PushClip { .. } | DrawOp::PopClip, _)
        | (_, DrawOp::PushClip { .. } | DrawOp::PopClip) => false,
        _ => true,
    }
}

fn image_changed(
    previous: &DrawOp,
    current: &DrawOp,
    previous_images: &[Arc<DecodedImage>],
    current_images: &[Arc<DecodedImage>],
) -> bool {
    let DrawOp::Image { id: a, .. } = *previous else {
        return false;
    };
    let DrawOp::Image { id: b, .. } = *current else {
        return false;
    };
    let pointer = |images: &[Arc<DecodedImage>], id: u32| images.get(id as usize).map(Arc::as_ptr);
    pointer(previous_images, a) != pointer(current_images, b)
}

fn push_bounds(op: &DrawOp, scale: (f32, f32), buffer: (u32, u32), rects: &mut Vec<DamageRect>) {
    if let Some(rect) = op_rect(op, scale, buffer) {
        rects.push(rect);
    }
}

/// `op`'s pixels in the buffer, outset for anti-aliasing and clipped to `buffer`.
fn op_rect(op: &DrawOp, (sx, sy): (f32, f32), (bw, bh): (u32, u32)) -> Option<DamageRect> {
    let bounds = match op {
        DrawOp::Rect { x, y, w, h, .. } | DrawOp::Image { x, y, w, h, .. } => {
            outset([*x * sx, *y * sy, (*x + *w) * sx, (*y + *h) * sy], OUTSET)
        }
        DrawOp::Path { points, width, .. } => {
            if points.is_empty() {
                return None;
            }
            let (mut l, mut t, mut r, mut b) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for &(x, y) in points {
                (l, t, r, b) = (l.min(x), t.min(y), r.max(x), b.max(y));
            }
            let pad = width.max(0.0) * 0.5 * sx.max(sy);
            outset(
                [l * sx - pad, t * sy - pad, r * sx + pad, b * sy + pad],
                OUTSET,
            )
        }
        DrawOp::Text { .. } => text_bounds(op, (sx, sy))?,
        DrawOp::Clear { .. } | DrawOp::PushClip { .. } | DrawOp::PopClip => return None,
    };
    clip_rect(bounds, bw, bh)
}

fn text_bounds(op: &DrawOp, (sx, sy): (f32, f32)) -> Option<[f32; 4]> {
    let DrawOp::Text {
        x,
        y,
        size,
        family,
        weight,
        italic,
        align,
        text,
        ..
    } = op
    else {
        return None;
    };
    if text.is_empty() || *size <= 0.0 || sy <= 0.0 {
        return None;
    }
    let (x, y, size, weight, italic, align) = (*x, *y, *size, *weight, *italic, *align);
    let px = size * sy;
    let [measured, ascent, descent] = text::measure(text, px, family, weight, italic);
    // A missing face measures as nothing. Over-damage rather than leave a glyph undamaged.
    let width = if measured > 0.0 {
        measured
    } else {
        px * text.len() as f32
    };
    let left = x * sx;
    let left = match align {
        1 => left - width / 2.0,
        2 => left - width,
        _ => left,
    };
    let top = y * sy;
    let ink = if measured > 0.0 {
        ascent + descent
    } else {
        px * 1.2
    };
    let glyph_top = top + (px * 1.2 - ink) / 2.0;
    Some(outset(
        [left, glyph_top, left + width, glyph_top + ink],
        OUTSET,
    ))
}

fn outset([l, t, r, b]: [f32; 4], pad: f32) -> [f32; 4] {
    [l - pad, t - pad, r + pad, b + pad]
}

fn clip_rect([l, t, r, b]: [f32; 4], bw: u32, bh: u32) -> Option<DamageRect> {
    let (bw, bh) = (bw as f32, bh as f32);
    let (l, t) = (l.floor().max(0.0), t.floor().max(0.0));
    let (r, b) = (r.ceil().min(bw), b.ceil().min(bh));
    if r <= l || b <= t {
        return None;
    }
    Some(DamageRect {
        x: l as i32,
        y: t as i32,
        width: (r - l) as i32,
        height: (b - t) as i32,
    })
}

/// The region a buffer of `age` must replace. `history` is the per-frame diffs already painted,
/// oldest first, and does not include `this`.
pub fn region_for_age(
    this: &Region,
    history: &VecDeque<Region>,
    age: u32,
    buffer: (u32, u32),
) -> Region {
    if age == 0 || matches!(this, Region::Full) {
        return Region::Full;
    }
    let need = age as usize - 1;
    if history.len() < need {
        return Region::Full;
    }
    let mut rects = match this {
        Region::Full => return Region::Full,
        Region::Rects(rects) => rects.clone(),
    };
    for older in history.iter().rev().take(need) {
        match older {
            Region::Full => return Region::Full,
            Region::Rects(older) => rects.extend(older.iter().copied()),
        }
    }
    classify(rects, buffer)
}

/// Remembers `this` and drops diffs older than a swapchain can still be holding.
pub fn remember(history: &mut VecDeque<Region>, this: Region) {
    history.push_back(this);
    while history.len() > HISTORY {
        history.pop_front();
    }
}

/// Rectangles small enough that painting them costs less than painting the buffer. `None` means
/// paint the whole buffer (the damage report may still name the rectangles).
pub fn partial_rects(region: &Region, (w, h): (u32, u32)) -> Option<&[DamageRect]> {
    let Region::Rects(rects) = region else {
        return None;
    };
    if rects.is_empty() {
        return None;
    }
    let area: u64 = rects.iter().map(|r| r.width as u64 * r.height as u64).sum();
    let buffer = w as u64 * h as u64;
    (buffer > 0 && area * 2 < buffer).then_some(rects)
}

/// What to tell the host, and the pixel count for the diagnostics line. An empty vector means
/// the whole buffer.
pub fn reported(region: &Region, (w, h): (u32, u32)) -> (u64, Vec<DamageRect>) {
    match region {
        Region::Full => (w as u64 * h as u64, Vec::new()),
        Region::Rects(rects) => {
            let pixels = rects.iter().map(|r| r.width as u64 * r.height as u64).sum();
            (pixels, rects.clone())
        }
    }
}

/// One `sig-diag` line when `SIG_DIAG=1`, in the harness's shape. `rects` of 0 is the whole buffer.
pub fn diag(pixels: u64, (w, h): (u32, u32), rects: usize, age: u32) {
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("SIG_DIAG").is_some_and(|v| v == "1")) {
        return;
    }
    static START: OnceLock<Instant> = OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f64();
    eprintln!(
        "sig-diag t={t:.3} damage engine=wasm pixels={pixels} w={w} h={h} rects={rects} age={age}"
    );
}

fn classify(rects: Vec<DamageRect>, (w, h): (u32, u32)) -> Region {
    if w == 0 || h == 0 {
        return Region::Full;
    }
    let rects = coalesce(rects);
    if rects.is_empty() {
        return Region::Full;
    }
    let area: u64 = rects.iter().map(|r| r.width as u64 * r.height as u64).sum();
    if area >= w as u64 * h as u64 {
        Region::Full
    } else {
        Region::Rects(rects)
    }
}

fn coalesce(mut rects: Vec<DamageRect>) -> Vec<DamageRect> {
    let mut changed = true;
    while changed {
        changed = false;
        let mut i = 0;
        while i < rects.len() {
            let mut j = i + 1;
            while j < rects.len() {
                if near(rects[i], rects[j], COALESCE_GAP) {
                    rects[i] = merge(rects[i], rects[j]);
                    rects.swap_remove(j);
                    changed = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }
    if rects.len() > MAX_RECTS {
        let mut bbox = rects[0];
        for rect in &rects[1..] {
            bbox = merge(bbox, *rect);
        }
        rects = vec![bbox];
    }
    rects
}

fn near(a: DamageRect, b: DamageRect, gap: i32) -> bool {
    let a = DamageRect {
        x: a.x.saturating_sub(gap),
        y: a.y.saturating_sub(gap),
        width: a.width.saturating_add(gap.saturating_mul(2)),
        height: a.height.saturating_add(gap.saturating_mul(2)),
    };
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

fn merge(a: DamageRect, b: DamageRect) -> DamageRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    DamageRect {
        x,
        y,
        width: (a.x + a.width).max(b.x + b.width) - x,
        height: (a.y + a.height).max(b.y + b.height) - y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> DrawOp {
        DrawOp::Rect {
            x,
            y,
            w,
            h,
            radius: 0.0,
            color: 0xff0000ff,
        }
    }

    fn snap<'a>(ops: &'a [DrawOp], canvas: (u32, u32)) -> Snap<'a> {
        Snap {
            ops,
            images: &[],
            scale: (1.0, 1.0),
            overlay: &[],
            overlay_scale: (1.0, 1.0),
            buffer: canvas,
            canvas,
        }
    }

    fn rects_of(region: Region) -> Vec<DamageRect> {
        match region {
            Region::Rects(rects) => rects,
            Region::Full => panic!("expected rectangles, got the whole buffer"),
        }
    }

    #[test]
    fn a_moved_rect_damages_where_it_was_and_where_it_is() {
        let clear = DrawOp::Clear { color: 0xff };
        let stay = rect(0.0, 0.0, 10.0, 10.0);
        let before = vec![clear.clone(), stay.clone(), rect(10.0, 10.0, 8.0, 8.0)];
        let after = vec![clear, stay, rect(40.0, 30.0, 8.0, 8.0)];
        let rects = rects_of(diff(
            Some(&snap(&before, (80, 60))),
            &snap(&after, (80, 60)),
        ));
        // The header that did not move contributes nothing. Both squares, outset by 2px, do.
        assert!(
            rects.iter().all(|r| r.x >= 8),
            "an unchanged op was damaged: {rects:?}"
        );
        assert!(rects.iter().any(|r| r.x <= 10 && r.y <= 10));
        assert!(rects
            .iter()
            .any(|r| r.x + r.width >= 48 && r.y + r.height >= 38));
    }

    #[test]
    fn a_changed_clear_or_clip_is_the_whole_buffer() {
        let buffer = (40, 20);
        let before = vec![DrawOp::Clear { color: 0xff }, rect(1.0, 1.0, 2.0, 2.0)];
        let after = vec![DrawOp::Clear { color: 0xffff }, rect(1.0, 1.0, 2.0, 2.0)];
        assert_eq!(
            diff(Some(&snap(&before, buffer)), &snap(&after, buffer)),
            Region::Full
        );

        let clip = |w| {
            vec![
                DrawOp::PushClip {
                    x: 0.0,
                    y: 0.0,
                    w,
                    h: 10.0,
                },
                rect(0.0, 0.0, 4.0, 4.0),
                DrawOp::PopClip,
            ]
        };
        assert_eq!(
            diff(Some(&snap(&clip(5.0), buffer)), &snap(&clip(8.0), buffer)),
            Region::Full
        );
        assert_eq!(
            diff(Some(&snap(&before, buffer)), &snap(&before[..1], buffer)),
            Region::Full,
            "lists that do not line up"
        );
    }

    #[test]
    fn nearby_rects_merge_and_age_unions_the_frames_since_the_buffer() {
        let clear = DrawOp::Clear { color: 0xff };
        let frame = |x| vec![clear.clone(), rect(x, 5.0, 6.0, 6.0)];
        let a = frame(10.0);
        let b = frame(14.0);
        let c = frame(30.0);
        let d_ab = diff(Some(&snap(&a, (80, 40))), &snap(&b, (80, 40)));
        let d_bc = diff(Some(&snap(&b, (80, 40))), &snap(&c, (80, 40)));
        // 4px apart plus the 2px outset on each side: one rectangle.
        assert_eq!(rects_of(d_ab.clone()).len(), 1, "{d_ab:?}");
        let mut history = VecDeque::new();
        remember(&mut history, d_ab);
        let aged = region_for_age(&d_bc, &history, 2, (80, 40));
        let rects = rects_of(aged);
        assert!(
            rects.iter().any(|r| r.x <= 10),
            "the buffer's old square: {rects:?}"
        );
        assert!(
            rects.iter().any(|r| r.x + r.width >= 36),
            "the new square: {rects:?}"
        );
        assert_eq!(region_for_age(&d_bc, &history, 0, (80, 40)), Region::Full);
        assert_eq!(
            region_for_age(&d_bc, &VecDeque::new(), 3, (80, 40)),
            Region::Full,
            "a buffer older than the history is repainted whole"
        );
    }
}
