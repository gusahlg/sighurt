//! Units of position and size, the display scale, and the metrics snapshot the host issues.
//!
//! Every position and size carries its unit in its type:
//!
//! | Types | Unit | Used for |
//! |---|---|---|
//! | [`WindowPoint`], [`WindowRect`] | window-logical: logical pixels from the window's top-left corner | Chrome layout, child placement |
//! | [`SurfacePoint`], [`SurfaceRect`] | surface-local logical: logical pixels from one surface's top-left corner | Wayland input, caret rectangles, per-target interaction |
//! | [`LogicalSize`] | logical pixels | The size of a surface or region |
//! | [`BufferSize`], [`BufferPoint`] | buffer pixels | Raster sizes, GPU targets, engines' device pixels |
//!
//! Logical rectangles are whole pixels because Wayland places and sizes surfaces in whole
//! logical pixels; pointer positions are fractional. Engine content coordinates and page zoom
//! are not here: an adapter converts from a [`Metrics`] snapshot into its engine's types, once.
//! A pointer on a page at window y = 64 arrives as surface-local y = 0 already; subtracting the
//! chrome height again, or multiplying by the scale in both the host and the adapter, is a bug.

use std::num::NonZeroU32;

use crate::ids::LayoutEpoch;

/// A size in logical pixels. Zero in either dimension means empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LogicalSize {
    /// Width in logical pixels.
    pub width: u32,
    /// Height in logical pixels.
    pub height: u32,
}

impl LogicalSize {
    /// Whether the size covers no area.
    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// A size in buffer pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BufferSize {
    /// Width in buffer pixels.
    pub width: u32,
    /// Height in buffer pixels.
    pub height: u32,
}

/// A position in window-logical coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowPoint {
    /// Logical pixels right of the window's left edge.
    pub x: f64,
    /// Logical pixels below the window's top edge.
    pub y: f64,
}

/// A rectangle in window-logical coordinates, such as a target's placement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct WindowRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in logical pixels.
    pub width: u32,
    /// Height in logical pixels.
    pub height: u32,
}

impl WindowRect {
    /// The rectangle's size.
    pub fn size(self) -> LogicalSize {
        LogicalSize {
            width: self.width,
            height: self.height,
        }
    }

    /// Whether `point` lies inside. The left and top edges are inside, the right and bottom
    /// edges belong to the neighbour, so adjacent rectangles never both claim a point.
    pub fn contains(self, point: WindowPoint) -> bool {
        let (x, y) = (f64::from(self.x), f64::from(self.y));
        point.x >= x
            && point.y >= y
            && point.x < x + f64::from(self.width)
            && point.y < y + f64::from(self.height)
    }

    /// `point` relative to this rectangle: the surface-local position on a surface placed here.
    pub fn to_surface(self, point: WindowPoint) -> SurfacePoint {
        SurfacePoint {
            x: point.x - f64::from(self.x),
            y: point.y - f64::from(self.y),
        }
    }

    /// The window position of `point` on a surface placed at this rectangle.
    pub fn to_window(self, point: SurfacePoint) -> WindowPoint {
        WindowPoint {
            x: point.x + f64::from(self.x),
            y: point.y + f64::from(self.y),
        }
    }
}

/// A position in surface-local logical coordinates, as Wayland reports pointer positions.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SurfacePoint {
    /// Logical pixels right of the surface's left edge.
    pub x: f64,
    /// Logical pixels below the surface's top edge.
    pub y: f64,
}

/// A rectangle in surface-local logical coordinates, such as a text caret.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SurfaceRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in logical pixels.
    pub width: u32,
    /// Height in logical pixels.
    pub height: u32,
}

/// A position in buffer pixels from the buffer's top-left corner.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BufferPoint {
    /// Buffer pixels right of the left edge.
    pub x: f64,
    /// Buffer pixels below the top edge.
    pub y: f64,
}

/// A display scale, kept as the fractional-scale protocol gives it: a numerator over
/// [`Scale::DENOMINATOR`] (120).
///
/// 120 is 100 %, 150 is 125 %, 180 is 150 % and 240 is 200 %. An integer scale, the fallback
/// when the compositor has no fractional-scale support, is a multiple of 120. Keeping the
/// rational value avoids accumulating float rounding.
///
/// # Rounding policy
///
/// A buffer length is `logical × numerator / 120` rounded half away from zero, computed in
/// integers so it is exact. That is what fractional-scale-v1 specifies for toplevel surfaces;
/// Sighurt uses the same rule for subsurfaces, where the protocol leaves rounding open. A
/// non-zero logical length never rounds to zero buffer pixels. Code that maps positions between
/// logical and buffer space uses the resulting dimensions (see [`Metrics::to_buffer`]), not the
/// nominal factor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scale(NonZeroU32);

impl Scale {
    /// The denominator of every scale.
    pub const DENOMINATOR: u32 = 120;

    /// 100 %.
    pub const ONE: Scale = Scale(NonZeroU32::new(Self::DENOMINATOR).unwrap());

    /// The scale `numerator / 120`, as `wp_fractional_scale_v1.preferred_scale` sends it.
    /// `None` for zero.
    pub fn from_fractional(numerator: u32) -> Option<Self> {
        NonZeroU32::new(numerator).map(Self)
    }

    /// A whole-number scale, as `wl_surface.preferred_buffer_scale` or `wl_output.scale` send
    /// it. `None` for zero or a value too large to represent.
    pub fn from_integer(factor: u32) -> Option<Self> {
        Self::from_fractional(factor.checked_mul(Self::DENOMINATOR)?)
    }

    /// The numerator over [`Scale::DENOMINATOR`].
    pub fn numerator(self) -> u32 {
        self.0.get()
    }

    /// The scale as a whole number, if it is one. Integer scales can use
    /// `wl_surface.set_buffer_scale`; the others need a viewport.
    pub fn integer(self) -> Option<u32> {
        let numerator = self.numerator();
        numerator
            .is_multiple_of(Self::DENOMINATOR)
            .then_some(numerator / Self::DENOMINATOR)
    }

    /// The scale as a float, for engine APIs that take a device pixel ratio. Don't use it to
    /// compute buffer sizes: use [`Scale::buffer_size`].
    pub fn factor(self) -> f64 {
        f64::from(self.numerator()) / f64::from(Self::DENOMINATOR)
    }

    /// The buffer length for `logical` logical pixels, following the rounding policy above.
    pub fn buffer_length(self, logical: u32) -> u32 {
        if logical == 0 {
            return 0;
        }
        let denominator = u64::from(Self::DENOMINATOR);
        let scaled =
            (u64::from(logical) * u64::from(self.numerator()) + denominator / 2) / denominator;
        u32::try_from(scaled).unwrap_or(u32::MAX).max(1)
    }

    /// The buffer size for a surface of `logical` size, following the rounding policy above.
    pub fn buffer_size(self, logical: LogicalSize) -> BufferSize {
        BufferSize {
            width: self.buffer_length(logical.width),
            height: self.buffer_length(logical.height),
        }
    }
}

/// One authoritative placement, size and scale for a target, issued by the host.
///
/// Engines and presenters render at [`Metrics::buffer`] for [`Metrics::epoch`]; a frame made
/// for another epoch is stale. The buffer size is derived from the rectangle and scale by
/// [`Scale::buffer_size`] when the snapshot is made, so the two can't disagree.
///
/// An empty rectangle means only that the layout has no room for the target right now (the
/// window is too small, say): the host unmaps it, and an engine keeps its last non-empty size,
/// allocates nothing new and presents nothing until the metrics are non-empty again. It does
/// not mean the view is hidden: visibility is [`ViewOptions::visible`] and
/// [`EngineCommand::SetVisibility`], and a hidden view, such as a background tab, keeps the
/// page area's non-empty metrics so switching tabs doesn't resize (and reflow) its page.
///
/// [`ViewOptions::visible`]: crate::engine::ViewOptions::visible
/// [`EngineCommand::SetVisibility`]: crate::engine::EngineCommand::SetVisibility
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Metrics {
    epoch: LayoutEpoch,
    rect: WindowRect,
    scale: Scale,
    buffer: BufferSize,
}

impl Metrics {
    /// The snapshot for a target placed at `rect` with `scale`, as of layout `epoch`.
    pub fn new(epoch: LayoutEpoch, rect: WindowRect, scale: Scale) -> Self {
        Self {
            epoch,
            rect,
            scale,
            buffer: scale.buffer_size(rect.size()),
        }
    }

    /// The layout this snapshot belongs to.
    pub fn epoch(&self) -> LayoutEpoch {
        self.epoch
    }

    /// Where the target sits in the window, and its logical size.
    pub fn rect(&self) -> WindowRect {
        self.rect
    }

    /// The target's scale.
    pub fn scale(&self) -> Scale {
        self.scale
    }

    /// The size buffers for the target must have.
    pub fn buffer(&self) -> BufferSize {
        self.buffer
    }

    /// Whether the rectangle is empty: the layout has no room for the target.
    pub fn is_empty(&self) -> bool {
        self.rect.size().is_empty()
    }

    /// `point` on the target's surface in buffer pixels. The ratio comes from the actual,
    /// rounded buffer size, so the surface's right and bottom edges land exactly on the
    /// buffer's. An adapter whose engine takes device-pixel input converts with this, once.
    pub fn to_buffer(&self, point: SurfacePoint) -> BufferPoint {
        let ratio = |buffer: u32, logical: u32| match logical {
            0 => 0.0,
            _ => f64::from(buffer) / f64::from(logical),
        };
        BufferPoint {
            x: point.x * ratio(self.buffer.width, self.rect.width),
            y: point.y * ratio(self.buffer.height, self.rect.height),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale(numerator: u32) -> Scale {
        Scale::from_fractional(numerator).unwrap()
    }

    #[test]
    fn buffer_lengths_round_half_away_from_zero() {
        // (numerator, logical, buffer)
        let cases = [
            (120, 1001, 1001), // 100 %, odd size
            (150, 100, 125),   // 125 %
            (150, 101, 126),   // 126.25
            (150, 102, 128),   // 127.5 rounds up
            (180, 75, 113),    // 150 %: 112.5 rounds up
            (180, 101, 152),   // 151.5 rounds up
            (180, 1279, 1919), // 1918.5 rounds up
            (240, 101, 202),   // 200 %
            (210, 1, 2),       // 175 %: 1.75
        ];
        for (numerator, logical, buffer) in cases {
            assert_eq!(
                scale(numerator).buffer_length(logical),
                buffer,
                "{logical} logical px at {numerator}/120"
            );
        }
    }

    #[test]
    fn only_empty_stays_empty() {
        assert_eq!(scale(30).buffer_length(1), 1, "0.25 would round to 0");
        assert_eq!(scale(180).buffer_length(0), 0);
        let empty = scale(180).buffer_size(LogicalSize {
            width: 0,
            height: 40,
        });
        assert_eq!(empty.width, 0);
    }

    #[test]
    fn huge_lengths_saturate() {
        assert_eq!(scale(240).buffer_length(u32::MAX), u32::MAX);
    }

    #[test]
    fn integer_scales_are_multiples_of_120() {
        assert_eq!(Scale::from_integer(2), Some(scale(240)));
        assert_eq!(Scale::from_integer(1), Some(Scale::ONE));
        assert_eq!(Scale::from_integer(0), None);
        assert_eq!(Scale::from_integer(u32::MAX), None);
        assert_eq!(Scale::from_fractional(0), None);
        assert_eq!(scale(240).integer(), Some(2));
        assert_eq!(scale(180).integer(), None);
        assert_eq!(scale(150).factor(), 1.25);
    }

    #[test]
    fn input_maps_edge_to_edge() {
        // 101 logical px at 125 % is 126 buffer px, not 126.25.
        let rect = WindowRect {
            x: 0,
            y: 64,
            width: 101,
            height: 40,
        };
        let metrics = Metrics::new(LayoutEpoch::next(), rect, scale(150));
        assert_eq!(
            metrics.buffer(),
            BufferSize {
                width: 126,
                height: 50
            }
        );
        let corner = metrics.to_buffer(SurfacePoint { x: 101.0, y: 40.0 });
        assert_eq!(corner, BufferPoint { x: 126.0, y: 50.0 });
        assert_eq!(
            metrics.to_buffer(SurfacePoint::default()),
            BufferPoint::default()
        );
    }

    #[test]
    fn empty_rect_allocates_nothing() {
        let epoch = LayoutEpoch::next();
        let rect = WindowRect {
            x: 0,
            y: 64,
            width: 800,
            height: 0,
        };
        let metrics = Metrics::new(epoch, rect, scale(180));
        assert!(metrics.is_empty());
        assert_eq!(metrics.buffer().height, 0);
    }

    #[test]
    fn chrome_page_boundary_belongs_to_one_side() {
        let chrome = WindowRect {
            x: 0,
            y: 0,
            width: 800,
            height: 64,
        };
        let page = WindowRect {
            x: 0,
            y: 64,
            width: 800,
            height: 536,
        };
        let edge = WindowPoint { x: 10.0, y: 64.0 };
        assert!(!chrome.contains(edge));
        assert!(page.contains(edge));
        let local = page.to_surface(edge);
        assert_eq!(local, SurfacePoint { x: 10.0, y: 0.0 });
        assert_eq!(page.to_window(local), edge);
    }
}
