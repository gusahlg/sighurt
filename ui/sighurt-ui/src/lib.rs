//! The browser chrome: tabs, a toolbar with an address/command line and a status line, drawn
//! on the CPU, and the API other chromes implement to replace it.
//!
//! Four concepts, kept apart:
//!
//! - a read-only [`Snapshot`] of the browser state the chrome displays (tabs, titles, committed
//!   URLs, loading, history availability, status), which the executable fills from the core;
//! - [`ChromeState`], the chrome's own interaction and editing state ([`text_edit`] has the
//!   editor);
//! - a [`Layout`] with the regions and hit targets ([`hit_test`]), in window-logical
//!   coordinates;
//! - a painter, [`CpuPainter`], that turns those into pixels for a target's metrics, drawing
//!   with a [`Canvas`].
//!
//! [`Model`] owns the first three with the fonts ([`Fonts`]) and colors ([`Colors`]), turns
//! input into [`ChromeAction`](extension::ChromeAction)s (typed browser commands, tabs by
//! identity, focus moves, clipboard and input-method requests) and reports which surfaces need
//! repainting ([`Damage`]). Its [`Layout`] is also the placement for the host: the chrome, page
//! and status rectangles. [`accessibility`] describes the same widgets as plain data.
//!
//! [`extension`] is the chrome API: the [`Chrome`](extension::Chrome) trait the executable
//! drives, with named surfaces and their input areas. [`standard`] is the model and painter
//! behind it, the default chrome; another chrome implements the trait the same way, with or
//! without this crate's helpers: [`Fonts`] to shape text, [`text_edit::TextEditor`] for an
//! address line and [`Canvas`] to draw. A frame, from the executable's side:
//!
//! ```no_run
//! # use sighurt_api::geometry::{LogicalSize, Metrics, Scale};
//! # use sighurt_api::ids::LayoutEpoch;
//! # use sighurt_ui::extension::{ChromeAction, ChromeEvent};
//! # use sighurt_ui::*;
//! # let (snapshot, mut memory, window) = (Snapshot::default(), vec![0u8; 0], LogicalSize::default());
//! let mut chrome = (standard::KIND.new)();
//! chrome.handle(ChromeEvent::Resize(window));
//! for action in chrome.handle(ChromeEvent::Snapshot(snapshot)) {
//!     let ChromeAction::Repaint(name) = action else { continue };
//!     let Some(surface) = chrome.layout().surfaces.iter().find(|s| s.name == name) else {
//!         continue;
//!     };
//!     let metrics = Metrics::new(LayoutEpoch::next(), surface.rect, Scale::ONE);
//!     let size = metrics.buffer();
//!     let buffer = PixelBuffer {
//!         data: &mut memory,
//!         width: size.width,
//!         height: size.height,
//!         stride: size.width as usize * 4,
//!     };
//!     chrome.paint(name, &metrics, buffer).unwrap();
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
pub mod canvas;
pub mod chrome;
pub mod extension;
pub mod hit_test;
pub mod layout;
pub mod paint_cpu;
pub mod standard;
pub mod state;
pub mod text;
pub mod text_edit;

pub use canvas::{Canvas, PaintError, Pen, PixelBuffer};
pub use chrome::{ColorError, Colors, Damage, Model, Rgba};
pub use layout::{Layout, LayoutOptions};
pub use paint_cpu::CpuPainter;
pub use state::{ChromeState, EngineRow, HomeInfo, Snapshot, TabContent, TabSnapshot};
pub use text::{Fonts, ShapedText, TextStyle};
