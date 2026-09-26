//! Performing the privileged services the core allowed: clipboard transfers through the host,
//! downloads into the download folder, file pickers, and handing URLs to the desktop.
//!
//! Work runs asynchronously and can be cancelled; results go back to the core with their
//! `RequestId`. Anything not implemented fails closed with `ServiceError::Unsupported`.
//!
//! Must not own: the decision whether a service is allowed (the core).
