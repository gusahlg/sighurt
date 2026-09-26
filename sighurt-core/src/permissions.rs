//! Policy for privileged service requests (`sighurt_api::services`): may this document have
//! this service?
//!
//! Decisions are keyed by view and `DocumentGeneration`, so a grant never outlives the document
//! it was made for and a late answer for a replaced document is dropped. When the document
//! changes, its outstanding requests are cancelled. Unknown or unimplemented requests fail
//! closed with a useful error. Page content cannot open a file chooser, reach the clipboard or
//! run a browser command without passing through here. A decision remembered beyond one
//! document is keyed by the request's origin, never by view or URL.
//!
//! Page-initiated downloads are throttled to one a second per tab, so a page cannot start them
//! without end.
//!
//! Must not own: performing the service (the executable does) or the prompt UI (the chrome).
