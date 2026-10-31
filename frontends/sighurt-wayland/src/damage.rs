//! Which pixels of an SHM buffer a painter says it changed.
//!
//! [`CpuTarget::submit`](sighurt_api::present::CpuTarget::submit) lends a
//! [`Frame`](sighurt_api::present::Frame) and has no place to name the changed pixels. The host
//! calls [`begin_paint`] with the buffer's age around that callback. The painter reads
//! [`buffer_age`] and, when it knows the difference from what the buffer still holds,
//! [`report_damage`]. [`end_paint`] returns those rectangles. No report, or an empty one, means
//! the whole buffer, so chrome, the demo engine and the tests submit exactly as before.
//!
//! The age is how many frames ago this buffer was last submitted: 0 is a new or undefined
//! buffer, 1 still holds the previous frame, 2 the frame before that.
//!
//! Owns: the report for the paint that is in progress on this thread. Must not own: which
//! pixels actually changed, or the `damage_buffer` request (the frame's submit does that).

use std::cell::RefCell;

/// One changed rectangle, in buffer pixels, origin at the buffer's top left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageRect {
    /// Left edge, in buffer pixels.
    pub x: i32,
    /// Top edge, in buffer pixels.
    pub y: i32,
    /// Width in buffer pixels. Zero or negative is dropped at submit.
    pub width: i32,
    /// Height in buffer pixels. Zero or negative is dropped at submit.
    pub height: i32,
}

struct Paint {
    age: u32,
    reported: bool,
    rects: Vec<DamageRect>,
}

thread_local! {
    static PAINT: RefCell<Option<Paint>> = const { RefCell::new(None) };
}

/// Starts a paint on this thread. `age` is what [`buffer_age`] returns until [`end_paint`].
pub fn begin_paint(age: u32) {
    PAINT.with(|slot| {
        *slot.borrow_mut() = Some(Paint {
            age,
            reported: false,
            rects: Vec::new(),
        });
    });
}

/// The age passed to [`begin_paint`], or `None` when this thread is not inside one.
pub fn buffer_age() -> Option<u32> {
    PAINT.with(|slot| slot.borrow().as_ref().map(|paint| paint.age))
}

/// The rectangles the painter changed, in buffer pixels. Replaces any earlier report for this
/// paint. Ignored outside [`begin_paint`].
pub fn report_damage(rects: &[DamageRect]) {
    PAINT.with(|slot| {
        let mut paint = slot.borrow_mut();
        let Some(paint) = paint.as_mut() else {
            return;
        };
        paint.reported = true;
        paint.rects.clear();
        paint.rects.extend_from_slice(rects);
    });
}

/// Ends the paint and returns the reported rectangles. `None` means the painter made no report,
/// so the submit damages the whole buffer. An empty `Some` means the same.
pub fn end_paint() -> Option<Vec<DamageRect>> {
    PAINT.with(|slot| {
        let paint = slot.borrow_mut().take()?;
        paint.reported.then_some(paint.rects)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_roundtrips_and_absence_means_the_whole_buffer() {
        assert_eq!(buffer_age(), None);
        report_damage(&[DamageRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        }]);
        assert_eq!(end_paint(), None, "a report outside a paint is ignored");

        begin_paint(2);
        assert_eq!(buffer_age(), Some(2));
        assert_eq!(end_paint(), None, "no report is the whole buffer");

        begin_paint(1);
        report_damage(&[
            DamageRect {
                x: 0,
                y: 4,
                width: 10,
                height: 8,
            },
            DamageRect {
                x: 3,
                y: 1,
                width: 2,
                height: 2,
            },
        ]);
        assert_eq!(
            end_paint(),
            Some(vec![
                DamageRect {
                    x: 0,
                    y: 4,
                    width: 10,
                    height: 8,
                },
                DamageRect {
                    x: 3,
                    y: 1,
                    width: 2,
                    height: 2,
                },
            ])
        );
        assert_eq!(buffer_age(), None);
    }
}
