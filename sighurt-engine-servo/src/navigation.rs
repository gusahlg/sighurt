//! Navigation for Servo views: `Navigate`, `Reload`, `Stop` and `Traverse`, and the navigation
//! events Servo's callbacks produce.
//!
//! Reports `NavigationStarted`, `NavigationCommitted` and `NavigationFailed` with the browser's
//! `NavigationId`, or one allocated here for navigations the page started itself; allocates a
//! new `DocumentGeneration` when a new document commits, not for same-document navigation.
//! Servo owns its session history, including same-document entries; this reports it as
//! `HistoryChanged` with Servo's entry list and never guesses entries from URLs. Servo 0.5's
//! `WebView::load` returns no id and `request_navigation` doesn't say which frame asked, so a
//! commit is attributed to the latest pending browser navigation, as `sighurt_api::events`
//! allows.
//!
//! Must not own: cross-engine history (the core).
