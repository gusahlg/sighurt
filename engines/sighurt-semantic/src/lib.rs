//! The DOM extraction script and the parser that turns its JSON into a
//! [`SemanticSnapshot`](sighurt_api::semantic::SemanticSnapshot).
//!
//! Servo and WPE share this crate. An adapter evaluates [`EXTRACT`] in the page (WPE in an
//! isolated world, Servo in the main world), passes the string to [`parse`] with the document
//! generation it is extracting, and evaluates [`action_script`] for a
//! [`SemanticAction`](sighurt_api::semantic::SemanticAction). Both scripts must run in the same
//! world: that is where the node-id map lives. The script does not say which document it ran
//! in. [`parse`] stamps the generation the adapter passes.
//!
//! Owns: the extraction script, the action scripts and the bounded JSON parser.
//! Must not own: calling an engine, laying text out into cells, or browser policy.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod action;
mod extract;
mod parse;
mod world;

pub use action::action_script;
pub use extract::{EXTRACT, TIME_BUDGET_MS};
pub use parse::{max_input_bytes, parse, ParseError, NODE_JSON_OVERHEAD};
