//! Navigation coordination and cross-engine history.
//!
//! Each navigation moves through requested, in progress, committed, and completed, failed or
//! cancelled, keyed by its `NavigationId`; the current usable document stays until the
//! replacement commits. The URL the user is editing, the requested URL and the committed URL
//! are three separate fields, so an unfinished navigation never changes the address bar's
//! committed identity. A failed load is not a failed engine. A commit, or a newer navigation
//! starting, supersedes the view's earlier pending navigations: they are dropped, not left
//! showing a requested URL forever.
//!
//! History is a list of segments, one per engine view with its retained history, or a degraded
//! restore record after eviction (restored by reloading its URL, with the loss of document
//! state stated). Back and forward traverse the active engine's own history first and cross a
//! segment boundary only at its end. Internal `sig://` pages are segments of their own.
//!
//! Must not own: the engines' session history, which they report through `HistoryChanged`.
//! Never reconstruct it from URL comparisons.
