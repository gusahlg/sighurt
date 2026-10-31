//! Startup capability probe.
//!
//! Owns: the answers collected from terminal replies. Must not own: the file
//! descriptor or how long the caller waits. The queries go out as one batch
//! ([`super::modes::QUERIES`]), DA1 last. The kitty graphics query in that
//! batch is omitted when `TMUX` is set or `TERM` starts with `screen` or
//! `tmux`. The probe returns when the DA1 reply has been seen and the input
//! buffer is empty, or when the deadline passes. A reply that shows up later
//! is still a reply: the same parser classifies it, so it is not delivered as
//! a key.

use std::time::Duration;

use super::parser::{Input, Reply};

/// How long [`the terminal probe`] waits when the caller does not say.
pub const PROBE_DEADLINE: Duration = Duration::from_millis(150);

/// What the terminal answered. Missing fields were not answered in time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// Primary device attributes, when a DA1 reply arrived.
    pub da1: Option<Vec<u32>>,
    /// DA1 listed sixel (parameter 4).
    pub sixel: bool,
    /// Flags from `CSI ? flags u`.
    pub kitty_keyboard: Option<u32>,
    /// `Some(true)` when the graphics reply contained `;OK`.
    pub kitty_graphics_ok: Option<bool>,
    /// XTGETTCAP entries. `found` is false for a `0 + r` reply.
    pub xtgettcap: Option<XtGetTcap>,
    /// OSC 10 colour spec.
    pub osc10: Option<String>,
    /// OSC 11 colour spec.
    pub osc11: Option<String>,
    /// Cell size in pixels, height then width (`CSI 6 ; height ; width t`).
    pub cell_pixels: Option<(u32, u32)>,
    /// Text area in pixels, height then width (`CSI 4 ; height ; width t`).
    pub text_area_pixels: Option<(u32, u32)>,
    /// Cell size from `TIOCGWINSZ` when the terminal reports a pixel size,
    /// width then height. Filled by the terminal, not by a reply.
    pub winsize_cell: Option<(u16, u16)>,
}

/// One XTGETTCAP reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XtGetTcap {
    /// The terminal reported the names (`1 + r`), rather than refusing them.
    pub found: bool,
    /// Decoded name and, when present, decoded value.
    pub entries: Vec<(String, Option<String>)>,
}

/// Whether `after` would change the page layout relative to `before`.
///
/// Graphics support is kitty's answer and sixel in DA1. The cell's pixel size
/// is the `CSI 16 t` reply. Either one is enough for the owner to relayout.
pub fn affects_layout(before: &Caps, after: &Caps) -> bool {
    before.kitty_graphics_ok != after.kitty_graphics_ok
        || before.sixel != after.sixel
        || before.cell_pixels != after.cell_pixels
}

/// Fold one input into `caps`. Returns whether it was the DA1 sentinel.
pub fn absorb(caps: &mut Caps, input: &Input) -> bool {
    let Input::Reply(reply) = input else {
        return false;
    };
    match reply {
        Reply::Da1 { params, sixel } => {
            caps.da1 = Some(params.clone());
            caps.sixel = *sixel;
            true
        }
        Reply::KittyKeyboard { flags } => {
            caps.kitty_keyboard = Some(*flags);
            false
        }
        Reply::KittyGraphics { ok, .. } => {
            caps.kitty_graphics_ok = Some(*ok);
            false
        }
        Reply::XtGetTcap { found, entries } => {
            caps.xtgettcap = Some(XtGetTcap {
                found: *found,
                entries: entries.clone(),
            });
            false
        }
        Reply::OscColor { index, spec, .. } => {
            if *index == 10 {
                caps.osc10 = Some(spec.clone());
            } else if *index == 11 {
                caps.osc11 = Some(spec.clone());
            }
            false
        }
        Reply::CellPixels { height, width } => {
            caps.cell_pixels = Some((*height, *width));
            false
        }
        Reply::TextAreaPixels { height, width } => {
            caps.text_area_pixels = Some((*height, *width));
            false
        }
        // Not a capability. During the probe it is discarded; afterwards the owner takes it.
        Reply::Clipboard { .. } => false,
    }
}
