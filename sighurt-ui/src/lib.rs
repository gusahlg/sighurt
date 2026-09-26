//! The browser chrome: a tab strip, a toolbar with an address/command line and an optional
//! status strip, drawn on the CPU.
//!
//! Four concepts, kept apart:
//!
//! - a read-only [`Snapshot`] of the browser state the chrome displays (tabs, titles, committed
//!   URLs, loading, history availability, status), which the executable fills from the core;
//! - [`ChromeState`], the chrome's own interaction and editing state ([`text_edit`] has the
//!   editor);
//! - a [`Layout`] with the regions and hit targets ([`hit_test`]), in window-logical
//!   coordinates;
//! - a painter, [`CpuPainter`], that turns those into pixels for a target's metrics.
//!
//! [`Chrome`] owns the first three with the fonts ([`Fonts`]) and colors ([`Colors`]), turns
//! input into [`UiAction`]s (typed browser commands, focus moves, clipboard and input-method
//! requests) and reports which surfaces need repainting ([`Damage`]). Its [`Layout`] is also the
//! placement snapshot for the host: the chrome, page and status rectangles. [`accessibility`]
//! describes the same widgets as plain data.
//!
//! A frame, from the executable's side:
//!
//! ```no_run
//! # use sighurt_api::geometry::{LogicalSize, Metrics, Scale};
//! # use sighurt_api::ids::LayoutEpoch;
//! # use sighurt_ui::*;
//! # let (snapshot, mut memory, window) = (Snapshot::default(), vec![0u8; 0], LogicalSize::default());
//! let mut chrome = Chrome::new(Fonts::system(), Colors::default(), LayoutOptions::default());
//! let mut painter = CpuPainter::new();
//! chrome.resize(window);
//! chrome.set_snapshot(snapshot);
//! if chrome.take_damage().chrome {
//!     let metrics = Metrics::new(LayoutEpoch::next(), chrome.layout().chrome, Scale::ONE);
//!     let size = metrics.buffer();
//!     let buffer = PixelBuffer {
//!         data: &mut memory,
//!         width: size.width,
//!         height: size.height,
//!         stride: size.width as usize * 4,
//!     };
//!     painter.paint(&mut chrome, &metrics, buffer).unwrap();
//! }
//! ```
//!
//! Must not own: tabs or navigation state (the core), Wayland objects (it holds no surface and
//! paints into memory the host lends it), web layout, or guest permissions. Page titles and
//! hovered URLs are untrusted text, drawn contained. Changing the painter never changes browser
//! state types.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod accessibility;
pub mod chrome;
pub mod hit_test;
pub mod layout;
pub mod paint_cpu;
pub mod state;
pub mod text;
pub mod text_edit;

pub use chrome::{Chrome, ColorError, Colors, Damage, Focus, Rgba, UiAction};
pub use layout::{Layout, LayoutOptions};
pub use paint_cpu::{CpuPainter, PaintError, PixelBuffer};
pub use state::{ChromeState, EngineRow, HomeInfo, Snapshot, TabContent, TabSnapshot};
pub use text::Fonts;
