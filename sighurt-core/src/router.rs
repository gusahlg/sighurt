//! Which engine opens a URL, and URL input fixup (what the user typed into a URL or a search).
//!
//! Deterministic rules, in order: an explicit engine choice by the user (if that engine is
//! enabled and allows the scheme); trusted internal `sig://` pages; configured schemes and
//! validated local app files for their engine; ordinary HTTP(S) to Servo by default; Wasm apps
//! over HTTP(S) only through an explicit app action or a narrow configured rule. Content no
//! engine can show becomes a download or a clear error, decided by the responsible engine's
//! response handling. File extensions are hints for choosing a loader, never proof of what the
//! content is.
//!
//! Must not own: network requests. There is no HEAD/GET probe here; the engine that loads a
//! page owns its networking.
