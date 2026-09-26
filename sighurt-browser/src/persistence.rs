//! Files: reading and watching the configuration (`{config_dir}/sighurt/config.toml`), saving
//! and loading the session, and the engines' profile directories.
//!
//! The core validates configuration before it replaces the active one. Session snapshots are
//! written atomically.
//!
//! Must not own: parsing or validating configuration (the core).
