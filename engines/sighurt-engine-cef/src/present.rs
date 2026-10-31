//! Copying the staged frame into a lease's buffer, only as much as that buffer lacks.
//!
//! A host's swapchain reuses a few shared-memory buffers, so a buffer last written at frame N
//! needs only what changed since: the union of CEF's damage over the frames after N
//! ([`Staged::damage_since`](crate::shared::Staged::damage_since)). The target does not promise
//! that a buffer it lends again still holds what it held, so a buffer is trusted only if a few
//! probe pixels recorded when it was written are still in it (a fresh or recycled buffer fails
//! and gets the whole frame). A size or layout epoch that differs, or damage older than the
//! history, also gets the whole frame.
//!
//! Must not own: buffers, or the decision to present.

use sighurt_api::ids::LayoutEpoch;

use crate::shared::{Damage, Staged};

/// How many pixels vouch for a buffer's content.
const PROBES: usize = 32;

/// What this engine last wrote into one buffer of the lease.
#[derive(Clone, Debug)]
pub struct BufferState {
    frame: u64,
    epoch: LayoutEpoch,
    probes: Vec<[u8; 4]>,
}

fn probe_at(i: usize, width: usize, height: usize) -> (usize, usize) {
    ((i * 37 + 11) % width, (i * 53 + 7) % height)
}

fn sample(pixels: &[u8], stride: usize, width: usize, height: usize) -> Vec<[u8; 4]> {
    (0..PROBES)
        .map(|i| {
            let (x, y) = probe_at(i, width, height);
            let at = y * stride + x * 4;
            pixels
                .get(at..at + 4)
                .map_or([0; 4], |p| [p[0], p[1], p[2], p[3]])
        })
        .collect()
}

/// Copies what `frame` (a `width` by `height` buffer of `stride`-byte rows) lacks of `staged`.
/// Returns the bytes copied and what to remember about the buffer.
pub fn copy_frame(
    staged: &Staged,
    pixels: &mut [u8],
    stride: usize,
    (width, height): (usize, usize),
    epoch: LayoutEpoch,
    known: Option<&BufferState>,
) -> (usize, BufferState) {
    let (sw, sh) = (staged.width as usize, staged.height as usize);
    let trusted = known.filter(|k| {
        sw == width
            && sh == height
            && k.epoch == epoch
            && k.probes == sample(pixels, stride, width, height)
    });
    let damage = trusted.map_or(Damage::Full, |k| staged.damage_since(k.frame));
    let mut bytes = 0;
    match damage {
        Damage::None => {}
        Damage::Rect([x0, y0, x1, y1]) => {
            for y in y0..y1.min(height) {
                let (from, to) = ((y * sw + x0) * 4, (y * sw + x1.min(width)) * 4);
                let at = y * stride + x0 * 4;
                if let (Some(src), Some(dst)) = (
                    staged.pixels.get(from..to),
                    pixels.get_mut(at..at + (to - from)),
                ) {
                    dst.copy_from_slice(src);
                    bytes += to - from;
                }
            }
        }
        Damage::Full => {
            let copy = sw.min(width) * 4;
            for (y, row) in pixels.chunks_mut(stride).take(height).enumerate() {
                let len = (width * 4).min(row.len());
                let row = &mut row[..len];
                if y < sh && copy <= row.len() {
                    row[..copy].copy_from_slice(&staged.pixels[y * sw * 4..y * sw * 4 + copy]);
                    row[copy..].fill(0xFF);
                } else {
                    row.fill(0xFF);
                }
                bytes += row.len();
            }
        }
    }
    let state = BufferState {
        frame: staged.frames,
        epoch,
        probes: sample(pixels, stride, width, height),
    };
    (bytes, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capi::CefRect;

    const W: usize = 64;
    const H: usize = 48;

    fn frame(value: u8) -> Vec<u8> {
        vec![value; W * H * 4]
    }

    /// A staged page that has had its first frame and, if `dirty`, a second with that rectangle.
    fn staged(dirty: Option<CefRect>) -> Staged {
        let mut staged = Staged::default();
        staged.apply(&frame(0xAA), W as u32, H as u32, &[]);
        if let Some(rect) = dirty {
            let mut next = frame(0xAA);
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    let at = (y as usize * W + x as usize) * 4;
                    next[at..at + 4].fill(0x55);
                }
            }
            staged.apply(&next, W as u32, H as u32, &[rect]);
        }
        staged
    }

    #[test]
    fn a_buffer_that_held_the_last_frame_gets_only_the_damage() {
        let epoch = LayoutEpoch::next();
        let mut buffer = vec![0u8; W * H * 4];
        let first = staged(None);
        let (full, state) = copy_frame(&first, &mut buffer, W * 4, (W, H), epoch, None);
        assert_eq!(full, W * H * 4);
        let rect = CefRect {
            x: 10,
            y: 5,
            width: 8,
            height: 4,
        };
        let second = staged(Some(rect));
        let (bytes, _) = copy_frame(&second, &mut buffer, W * 4, (W, H), epoch, Some(&state));
        assert_eq!(bytes, 8 * 4 * 4, "only the damaged rectangle");
        assert_eq!(
            buffer, second.pixels,
            "and the buffer is the whole current frame"
        );
    }

    #[test]
    fn a_recycled_or_fresh_buffer_gets_everything() {
        let epoch = LayoutEpoch::next();
        let mut buffer = vec![0u8; W * H * 4];
        let (_, state) = copy_frame(&staged(None), &mut buffer, W * 4, (W, H), epoch, None);
        // The same state offered for a buffer that no longer holds the frame.
        let mut fresh = vec![0u8; W * H * 4];
        let rect = CefRect {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        };
        let second = staged(Some(rect));
        let (bytes, _) = copy_frame(&second, &mut fresh, W * 4, (W, H), epoch, Some(&state));
        assert_eq!(bytes, W * H * 4);
        assert_eq!(fresh, second.pixels);
    }

    #[test]
    fn a_new_layout_epoch_or_size_gets_everything() {
        let epoch = LayoutEpoch::next();
        let mut buffer = vec![0u8; W * H * 4];
        let (_, state) = copy_frame(&staged(None), &mut buffer, W * 4, (W, H), epoch, None);
        let (bytes, _) = copy_frame(
            &staged(None),
            &mut buffer,
            W * 4,
            (W, H),
            LayoutEpoch::next(),
            Some(&state),
        );
        assert_eq!(bytes, W * H * 4);
        // A buffer of another size than the staged frame is padded, not trusted.
        let mut wide = vec![0u8; (W + 8) * H * 4];
        let (bytes, _) = copy_frame(
            &staged(None),
            &mut wide,
            (W + 8) * 4,
            (W + 8, H),
            epoch,
            Some(&state),
        );
        assert_eq!(bytes, (W + 8) * H * 4);
        assert_eq!(
            wide[W * 4..W * 4 + 4],
            [0xFF; 4],
            "the extra column is opaque white"
        );
    }

    #[test]
    fn an_unchanged_frame_copies_nothing() {
        let epoch = LayoutEpoch::next();
        let mut buffer = vec![0u8; W * H * 4];
        let first = staged(None);
        let (_, state) = copy_frame(&first, &mut buffer, W * 4, (W, H), epoch, None);
        let (bytes, _) = copy_frame(&first, &mut buffer, W * 4, (W, H), epoch, Some(&state));
        assert_eq!(bytes, 0);
    }
}
