//! The extraction script.
//!
//! Owns: the JavaScript that walks a document into snapshot JSON, with the semantic bounds and
//! this process's slot name filled in. Must not own: parsing that JSON, or performing an action.

use std::sync::LazyLock;

use sighurt_api::semantic::{MAX_DEPTH, MAX_NODES, MAX_OPTIONS, MAX_STRING_BYTES, MAX_TOTAL_BYTES};

use crate::world::state_key_literal;

/// How long one walk may run before it stops and marks the snapshot truncated.
///
/// Measured in the page with `performance.now()` (or `Date.now()` when that is missing).
pub const TIME_BUDGET_MS: u64 = 50;

/// The extraction expression, ready to evaluate. Its value is a JSON string.
///
/// Dereference it (`EXTRACT.as_str()`). It is built on first use rather than stored as a
/// `&str` constant: the bounds are formatted from [`sighurt_api::semantic`] so they cannot
/// drift, and the slot name is chosen per process so a page cannot guess it.
pub static EXTRACT: LazyLock<String> = LazyLock::new(build);

fn build() -> String {
    let mut src = include_str!("extract.js").to_string();
    let pairs = [
        ("__SIGHURT_MAX_NODES__", MAX_NODES.to_string()),
        ("__SIGHURT_MAX_DEPTH__", MAX_DEPTH.to_string()),
        ("__SIGHURT_MAX_STRING_BYTES__", MAX_STRING_BYTES.to_string()),
        ("__SIGHURT_MAX_TOTAL_BYTES__", MAX_TOTAL_BYTES.to_string()),
        ("__SIGHURT_MAX_OPTIONS__", MAX_OPTIONS.to_string()),
        ("__SIGHURT_TIME_BUDGET_MS__", TIME_BUDGET_MS.to_string()),
        ("__SIGHURT_STATE_KEY__", state_key_literal()),
    ];
    for (token, value) in pairs {
        assert!(src.contains(token), "extraction script is missing {token}");
        src = src.replacen(token, &value, 1);
    }
    src
}
