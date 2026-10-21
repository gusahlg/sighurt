//! Scale: the compositor's preferred scale for a window, and how each surface applies it.
//!
//! Prefers `wp_fractional_scale_v1` with `wp_viewporter`, keeping the scale as its numerator
//! over 120 ([`Scale`]); falls back to the integer scale (`wl_surface.preferred_buffer_scale`,
//! or the largest scale of the outputs the window is on). Without a viewporter only integer
//! scales can be shown, so the integer one is used even when a fractional one is offered.
//!
//! One scale applies to the whole window: the root's preferred scale, which every target of the
//! window follows, so the chrome and the page never render at different scales. Buffer sizes
//! follow the one rounding policy of [`Scale::buffer_size`]. With a viewporter every surface
//! gets a viewport whose destination is its logical size and keeps buffer scale 1; without one
//! it sets the integer buffer scale, for which [`Scale::buffer_size`] is exact. A scale change
//! is reported as a new window configuration, so it starts a layout transaction even when the
//! logical size is unchanged.
//!
//! Must not own: page zoom, which is an engine setting.

use sighurt_api::geometry::{LogicalSize, Scale};
use sighurt_api::ids::WindowId;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::connection::Host;

/// What the compositor said about a window's scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WindowScale {
    fractional: Option<Scale>,
    integer: Scale,
}

impl Default for WindowScale {
    fn default() -> Self {
        Self {
            fractional: None,
            integer: Scale::ONE,
        }
    }
}

impl WindowScale {
    /// `wp_fractional_scale_v1.preferred_scale`. Zero is ignored.
    pub fn set_fractional(&mut self, numerator: u32) {
        if let Some(scale) = Scale::from_fractional(numerator) {
            self.fractional = Some(scale);
        }
    }

    /// The integer scale. Zero or negative is ignored.
    pub fn set_integer(&mut self, factor: i32) {
        if let Some(scale) = u32::try_from(factor).ok().and_then(Scale::from_integer) {
            self.integer = scale;
        }
    }

    /// The scale the window renders at.
    pub fn effective(&self, viewporter: bool) -> Scale {
        match (viewporter, self.fractional) {
            (true, Some(fractional)) => fractional,
            _ => self.integer,
        }
    }
}

/// Makes `surface` show a buffer rendered at `scale` over `logical` logical pixels, as pending
/// state for its next commit.
pub(crate) fn apply(
    surface: &WlSurface,
    viewport: Option<&WpViewport>,
    scale: Scale,
    logical: LogicalSize,
) {
    match viewport {
        Some(viewport) => {
            surface.set_buffer_scale(1);
            viewport.set_destination(logical.width as i32, logical.height as i32);
        }
        None => surface.set_buffer_scale(scale.integer().unwrap_or(1) as i32),
    }
}

impl Dispatch<WpFractionalScaleV1, WindowId> for Host {
    fn event(
        host: &mut Host,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        window: &WindowId,
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            host.update_scale(*window, |s| s.set_fractional(scale));
        }
    }
}

impl Dispatch<WpFractionalScaleManagerV1, ()> for Host {
    fn event(
        _: &mut Host,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
    }
}

impl Dispatch<WpViewporter, ()> for Host {
    fn event(
        _: &mut Host,
        _: &WpViewporter,
        _: <WpViewporter as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for Host {
    fn event(
        _: &mut Host,
        _: &WpViewport,
        _: <WpViewport as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use sighurt_api::geometry::{Metrics, WindowRect};
    use sighurt_api::ids::LayoutEpoch;

    use super::*;

    #[test]
    fn fractional_needs_a_viewporter() {
        let mut scale = WindowScale::default();
        assert_eq!(scale.effective(true), Scale::ONE);
        scale.set_integer(2);
        scale.set_fractional(180);
        assert_eq!(scale.effective(true).numerator(), 180);
        // Without a viewport 150 % cannot be shown; the integer scale is used.
        assert_eq!(scale.effective(false).numerator(), 240);
    }

    #[test]
    fn nonsense_scales_are_ignored() {
        let mut scale = WindowScale::default();
        scale.set_fractional(0);
        scale.set_integer(0);
        scale.set_integer(-3);
        assert_eq!(scale.effective(true), Scale::ONE);
    }

    #[test]
    fn a_scale_change_at_the_same_logical_size_changes_the_buffer() {
        // The case the plan names: the window keeps its logical size on a new output.
        let rect = WindowRect {
            x: 0,
            y: 40,
            width: 1279,
            height: 761,
        };
        let mut scale = WindowScale::default();
        let before = Metrics::new(LayoutEpoch::next(), rect, scale.effective(true));
        scale.set_fractional(180);
        let after = Metrics::new(LayoutEpoch::next(), rect, scale.effective(true));
        assert_eq!(before.rect(), after.rect());
        assert_eq!((before.buffer().width, before.buffer().height), (1279, 761));
        // 1918.5 and 1141.5 round half away from zero.
        assert_eq!((after.buffer().width, after.buffer().height), (1919, 1142));
    }

    #[test]
    fn integer_fallback_sizes_are_exact_multiples() {
        let mut scale = WindowScale::default();
        scale.set_integer(3);
        let s = scale.effective(false);
        assert_eq!(s.integer(), Some(3));
        let buffer = s.buffer_size(LogicalSize {
            width: 333,
            height: 17,
        });
        assert_eq!((buffer.width, buffer.height), (999, 51));
    }
}
