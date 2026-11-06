//! Sighurt's internal pages (`sig://help`, `sig://config`, `sig://terminal`): Wasm guests from
//! `pages/`, built by `build.rs` and embedded here, and what each may ask for.
//!
//! [`PAGES`] is the one table both sides read: the runtime gives the core the names and grants
//! (`Environment::pages`) and the Wasm engine the modules (`WasmConfig::pages`), so they can't
//! disagree. A page whose guest this tree doesn't have (its `pages/sig-page-<name>` crate is
//! missing) has no module and is no page of the build.
//!
//! To add a page: a guest crate `pages/sig-page-<name>` (a workspace member, `cdylib`, on
//! `sighurt-sdk` and `sig-page-kit`), which `build.rs` finds by its directory, and a line in
//! [`PAGES`] with its title and grants. Two pages may share a module (`sig://config` runs the
//! terminal's, with the editor's grant): the module learns which page it is from its own
//! address, and the grants, which the core checks, are the page's.
//!
//! Must not own: what a page may do beyond its grants (the core decides each request), or
//! loading one (the engine).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use sighurt_api::services::PageGrant;

include!(concat!(env!("OUT_DIR"), "/blobs.rs"));

/// One internal page.
#[derive(Clone, Copy, Debug)]
pub struct Page {
    /// Its name: `help` for `sig://help`.
    pub name: &'static str,
    /// The title its tab shows.
    pub title: &'static str,
    /// Its module, if this build has its guest.
    pub wasm: Option<&'static [u8]>,
    /// What it may ask for (`sighurt_api::services::Service::Page`), per operation.
    pub grants: &'static [PageGrant],
}

/// Every internal page, built or not.
pub const PAGES: &[Page] = &[
    Page {
        name: "help",
        title: "sig help",
        wasm: blob!("help"),
        grants: &[PageGrant::Info],
    },
    // The terminal's module, which runs the user's editor on the configuration here: its grant
    // is the editor's, so the core lets it start nothing else.
    Page {
        name: "config",
        title: "Configuration",
        wasm: blob!("terminal"),
        grants: &[PageGrant::Editor],
    },
    Page {
        name: "terminal",
        title: "sig terminal",
        wasm: blob!("terminal"),
        grants: &[PageGrant::Shell],
    },
];

/// The pages this build has: those whose guest was built, with their modules.
pub fn built() -> impl Iterator<Item = (Page, &'static [u8])> {
    PAGES.iter().filter_map(|page| Some((*page, page.wasm?)))
}
