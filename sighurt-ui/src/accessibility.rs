//! The chrome's accessibility tree: roles, names, values and actions for its widgets, with
//! stable ids, and a container node that owns the active engine view's accessibility subtree
//! where the engine provides one.
//!
//! A bridge to the desktop (such as AccessKit's Unix adapter) comes later. Engines without an
//! accessibility tree are reported as such, never papered over.
//!
//! Must not own: engine content semantics.
