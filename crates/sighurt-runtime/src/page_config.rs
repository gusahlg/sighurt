//! What `sig://config` does with the configuration file: reading it, checking a text against
//! this build, and replacing the file ([`PageRequest::ConfigRead`], `ConfigCheck`,
//! `ConfigWrite`).
//!
//! A STUB until the configuration lane implements it (docs/rewrite/pages-2026-11-05.md, G): a
//! missing file reads as `version = 1` and a pointer to `sig://help/config`; a write is refused
//! when the file no longer holds what the page read (`replaces`), when the text is invalid, over
//! 256 KiB, or its target is read-only; a symbolic link's target is replaced; the file stays
//! mode 0600; and the session reloads the configuration at once and says what that did,
//! "restart needed" included. Until then every request is unsupported.
//!
//! The core has already decided the request: the page is one whose grants include it.
//!
//! Owns: the file's text on the way in and out. Must not own: who may ask (the core) or taking
//! a reloaded configuration into use (the session queues it).

use sighurt_api::services::{ServiceError, ServiceResult};
use sighurt_core::config::Config;
use sighurt_core::state::Environment;

use crate::persistence::Paths;

/// The configuration file's text: `ServiceReply::Text`.
pub fn read(_paths: &Paths) -> ServiceResult {
    Err(ServiceError::Unsupported)
}

/// What is wrong with `text` as a configuration for this build: `ServiceReply::Text`, one
/// problem a line, empty when it is valid.
pub fn check(_text: &str, _env: &Environment) -> ServiceResult {
    Err(ServiceError::Unsupported)
}

/// Replaces the file with `text` if it still holds `replaces`, and parses it: the answer for
/// the page (`ServiceReply::Text` saying what the reload does, compared with `current`), and the
/// configuration for the session to take into use now.
pub fn write(
    _paths: &Paths,
    _env: &Environment,
    _current: &Config,
    _text: &str,
    _replaces: &str,
) -> (ServiceResult, Option<Config>) {
    (Err(ServiceError::Unsupported), None)
}
