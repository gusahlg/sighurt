//! `sig://config`: the user's configuration file, seen and edited in the browser.
//!
//! A Wasm guest of sig's own Wasm engine. Its logic is plain Rust, tested on the host
//! (`tools/cargo test -p sig-page-config`): the text ([`buffer`]), the editor's commands and
//! history ([`editor`]), measuring text across a line ([`layout`]), TOML colouring
//! ([`syntax`]), the browser's answers ([`reply`]), input records ([`record`]), and the page
//! around the editor with its requests ([`page`]). The entry points and drawing (`wasm`) are
//! thin and exist only on wasm32. The browser is reached through [`host::Host`]: the
//! privileged `sig` module (`sig-page-kit`) in `sig`, or, built with the feature `fake-host`,
//! [`host::FakeHost`], an in-memory file, so the page runs as an ordinary guest
//! (`sig page.wasm`) for trying it out.
//!
//! Must not own: reading, checking or writing the file (the browser's runtime, which this
//! page only asks), or any key binding of the browser's.

pub mod buffer;
pub mod editor;
pub mod host;
pub mod layout;
pub mod page;
pub mod record;
pub mod reply;
pub mod syntax;

#[cfg(target_arch = "wasm32")]
mod wasm;
