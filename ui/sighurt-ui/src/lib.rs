//! The browser chrome: tabs, a toolbar with an address/command line and a status line, drawn
//! as scenes, and the API other chromes implement to replace it.
//!
//! Four concepts, kept apart:
//!
//! - a read-only [`Snapshot`] of the browser state the chrome displays (tabs, titles, committed
//!   URLs, loading, history availability, status), which the executable fills from the core;
//! - [`ChromeState`], the chrome's own interaction and editing state ([`text_edit`] has the
//!   editor);
//! - a [`Layout`] with the regions and hit targets ([`hit_test`]), in window-logical
//!   coordinates;
//! - its drawing ([`paint`]), which turns those into a scene for each surface with a [`Pen`]
//!   (sighurt-render's); a backend the executable chose renders the scene.
//!
//! [`Model`] owns the first three with the fonts ([`Fonts`]) and colors ([`Colors`]), turns
//! input into [`ChromeAction`](extension::ChromeAction)s (typed browser commands, tabs by
//! identity, focus moves, clipboard and input-method requests) and reports which surfaces need
//! repainting ([`Damage`]). Its [`Layout`] is also the placement for the host: the chrome, page
//! and status rectangles. [`accessibility`] describes the same widgets as plain data.
//!
//! [`extension`] is the chrome API: the [`Chrome`](extension::Chrome) trait the executable
//! drives, with named surfaces and their input areas. [`standard`] is the model and its drawing
//! behind it, the default chrome; another chrome implements the trait the same way, with or
//! without this crate's helpers: [`Fonts`] to shape text, [`text_edit::TextEditor`] for an
//! address line and [`Pen`] to draw. A frame, from the executable's side:
//!
//! ```no_run
//! # use sighurt_api::geometry::{LogicalSize, Metrics, Scale};
//! # use sighurt_api::ids::LayoutEpoch;
//! # use sighurt_render::{Backend, CpuBackend, Output, PixelBuffer, SurfaceKey};
//! # use sighurt_ui::extension::{ChromeAction, ChromeEvent};
//! # use sighurt_ui::*;
//! # let (snapshot, mut memory, window) = (Snapshot::default(), vec![0u8; 0], LogicalSize::default());
//! let fonts = Fonts::system();
//! let mut backend = CpuBackend::new(fonts.handle());
//! let mut chrome = (standard::KIND.new)(fonts);
//! chrome.handle(ChromeEvent::Resize(window));
//! for action in chrome.handle(ChromeEvent::Snapshot(snapshot)) {
//!     let ChromeAction::Repaint(name) = action else { continue };
//!     let Some(surface) = chrome.layout().surfaces.iter().find(|s| s.name == name) else {
//!         continue;
//!     };
//!     let metrics = Metrics::new(LayoutEpoch::next(), surface.rect, Scale::ONE);
//!     let mut pen = Pen::new(metrics.rect());
//!     chrome.draw(name, &mut pen);
//!     let size = metrics.buffer();
//!     let buffer = PixelBuffer {
//!         data: &mut memory,
//!         width: size.width,
//!         height: size.height,
//!         stride: size.width as usize * 4,
//!     };
//!     let scene = pen.finish();
//!     backend.render(SurfaceKey(1), &metrics, &scene, Output::Pixels(buffer)).unwrap();
//! }
//! ```
//!
//! Must not own: tabs or navigation state (the core), Wayland objects (it holds no surface and
//! draws scenes, not pixels), how a scene is rendered (sighurt-render's backends), web layout,
//! or guest permissions. Page titles and hovered URLs are untrusted text, drawn contained.
//! Changing the renderer never changes browser state types.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod accessibility;
pub mod chrome;
pub mod extension;
pub mod hit_test;
pub mod layout;
pub mod paint;
pub mod standard;
pub mod state;
pub mod text;
pub mod text_edit;

pub use chrome::{ColorError, Colors, Damage, Model};
pub use layout::{Layout, LayoutOptions};
pub use sighurt_render::{FontSystemHandle, Fonts, PathBuilder, Pen, Rgba, ShapedText, TextStyle};
pub use state::{
    ChromeState, EngineRow, FindField, FindSnapshot, HomeInfo, HomeKey, HomeLink, Snapshot,
    TabContent, TabSnapshot,
};
