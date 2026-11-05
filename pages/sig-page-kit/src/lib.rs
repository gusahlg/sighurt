//! The guest side of Sighurt's internal pages (`sig://help`, `sig://config`, `sig://terminal`):
//! safe wrappers of the privileged `sig` import module ([`sig`]), and what the pages share.
//!
//! The pages are Wasm guests built into `sig` (`crates/sighurt-pages`). They import the `oxide`
//! module through `sighurt-sdk`, like any app, and the `sig` module, which the Wasm engine links
//! only for them. The ABI is documented in `engines/sighurt-engine-wasm/src/pages.rs`; this
//! crate mirrors it, and must change with it.
//!
//! Must not own: anything one page alone needs.

pub mod sig;
pub mod ui;
