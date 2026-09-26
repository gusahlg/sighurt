//! Policy for what pages ask the browser for: privileged services (`sighurt_api::services`),
//! new tabs and downloads.
//!
//! A request acts for one document, its view's current `DocumentGeneration`, and nothing
//! outlives that document: its pending requests are cancelled when it is replaced, a late
//! answer for it is dropped, and a permission the user grants it is granted to it alone. A
//! refusal is remembered for the tab's lifetime by the request's origin (never by view or URL),
//! so a page can't keep asking. Only the document a tab shows may ask for anything.
//!
//! Pages get nothing without the user:
//!
//! - The clipboard, file pickers and handing a URL to the desktop need a click or key press in
//!   the page within the last [`ACTIVATION`] (user activation); reading the clipboard, picking
//!   files and opening a URL elsewhere use it up.
//! - A new tab needs activation too, and uses it up, so one click opens one tab. A page never
//!   opens Sighurt's `sig:` pages, and opens `file:` URLs only if it is a `file:` page itself.
//! - Camera, microphone, location and screen capture need the user's answer to a prompt.
//! - Downloads are limited to one per [`DOWNLOAD_INTERVAL`] per tab.
//!
//! A request that fits none of this is refused. Requests are bounded per view
//! ([`MAX_PENDING_REQUESTS`]).
//!
//! Must not own: performing a service (the executable does) or the prompt (the chrome).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use sighurt_api::ids::{DocumentGeneration, RequestId, ViewId};
use sighurt_api::services::{DownloadSource, Permission, Service};

use crate::tabs::MAX_URL;

/// How long a click or key press in a page lets it do what pages may not do by themselves.
pub const ACTIVATION: Duration = Duration::from_secs(5);

/// How often pages in one tab may start a download.
pub const DOWNLOAD_INTERVAL: Duration = Duration::from_secs(1);

/// The most service requests one view may have waiting for an answer.
pub const MAX_PENDING_REQUESTS: usize = 16;

/// Whether the user activated the page recently enough, as of `now`.
pub(crate) fn is_active(activation: Option<Instant>, now: Instant) -> bool {
    activation.is_some_and(|at| now.saturating_duration_since(at) < ACTIVATION)
}

/// Uses up the page's activation; `true` if it had one.
pub(crate) fn take_activation(activation: &mut Option<Instant>, now: Instant) -> bool {
    is_active(activation.take(), now)
}

/// Lets a page have what it asks for at most once per [`DOWNLOAD_INTERVAL`]; `last` is when it
/// last got it.
pub(crate) fn throttle(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|last| now.saturating_duration_since(last) < DOWNLOAD_INTERVAL) {
        return false;
    }
    *last = Some(now);
    true
}

/// What a service needs before the browser performs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Requirement {
    /// User activation, used up if `consume`.
    Activation { consume: bool },
    /// The tab's download allowance.
    Throttle,
    /// The user's answer, unless the document already has it or the origin was refused.
    Prompt(Permission),
    /// Nothing: it is refused.
    Refused,
}

/// What `service` needs.
pub(crate) fn requirement(service: &Service) -> Requirement {
    match service {
        Service::ClipboardRead { .. } | Service::PickFiles { .. } => {
            Requirement::Activation { consume: true }
        }
        Service::ClipboardWrite { .. } => Requirement::Activation { consume: false },
        Service::OpenExternal { url } if is_external(url) => {
            Requirement::Activation { consume: true }
        }
        Service::OpenExternal { .. } => Requirement::Refused,
        Service::Download {
            source: DownloadSource::Url(url),
            ..
        } if url.len() > MAX_URL || url::Url::parse(url).is_err() => Requirement::Refused,
        Service::Download { .. } => Requirement::Throttle,
        Service::Permission(permission) => Requirement::Prompt(*permission),
    }
}

/// Whether `url` is one to hand to the desktop (`mailto:`, a custom scheme) rather than one the
/// browser opens itself or that runs code.
fn is_external(url: &str) -> bool {
    const OURS: [&str; 10] = [
        "http",
        "https",
        "file",
        "sig",
        "data",
        "about",
        "javascript",
        "blob",
        "ws",
        "wss",
    ];
    url.len() <= MAX_URL && url::Url::parse(url).is_ok_and(|url| !OURS.contains(&url.scheme()))
}

/// A request the browser has not answered yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pending {
    pub view: ViewId,
    /// The document it acts for.
    pub document: DocumentGeneration,
    /// Who asked, as the engine defines it.
    pub origin: Option<String>,
    /// The permission the user is being asked for, if it is a prompt.
    pub prompt: Option<Permission>,
}

/// The requests waiting for an answer, from the executable or the user.
#[derive(Clone, Debug, Default)]
pub(crate) struct Requests(BTreeMap<RequestId, Pending>);

impl Requests {
    pub(crate) fn insert(&mut self, id: RequestId, pending: Pending) {
        self.0.insert(id, pending);
    }

    pub(crate) fn get(&self, id: RequestId) -> Option<&Pending> {
        self.0.get(&id)
    }

    pub(crate) fn remove(&mut self, id: RequestId) -> Option<Pending> {
        self.0.remove(&id)
    }

    /// How many requests `view` has waiting.
    pub(crate) fn count(&self, view: ViewId) -> usize {
        self.0.values().filter(|p| p.view == view).count()
    }

    /// Removes the requests of `view` that `pick` selects, and returns their ids.
    pub(crate) fn take(
        &mut self,
        view: ViewId,
        mut pick: impl FnMut(&Pending) -> bool,
    ) -> Vec<RequestId> {
        let ids: Vec<_> = self
            .0
            .iter()
            .filter(|(_, p)| p.view == view && pick(p))
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            self.0.remove(id);
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_lasts_five_seconds_and_can_be_used_up() {
        let click = Instant::now();
        let mut activation = Some(click);
        assert!(is_active(activation, click + Duration::from_millis(4999)));
        assert!(!is_active(activation, click + ACTIVATION));
        assert!(take_activation(
            &mut activation,
            click + Duration::from_secs(1)
        ));
        assert!(!take_activation(
            &mut activation,
            click + Duration::from_secs(1)
        ));
    }

    #[test]
    fn pages_get_one_download_a_second() {
        let start = Instant::now();
        let mut last = None;
        assert!(throttle(&mut last, start));
        assert!(!throttle(&mut last, start + Duration::from_millis(500)));
        assert!(throttle(&mut last, start + Duration::from_millis(1000)));
        assert!(!throttle(&mut last, start + Duration::from_millis(1999)));
    }

    #[test]
    fn requirements() {
        let external = |url: &str| requirement(&Service::OpenExternal { url: url.into() });
        assert_eq!(
            external("mailto:a@b.test"),
            Requirement::Activation { consume: true }
        );
        for url in [
            "https://a.test/",
            "javascript:alert(1)",
            "sig://home",
            "not a url",
        ] {
            assert_eq!(external(url), Requirement::Refused, "{url}");
        }
        let download = |url: String| Service::Download {
            source: DownloadSource::Url(url),
            suggested_name: None,
        };
        assert_eq!(
            requirement(&download("https://a.test/x.zip".into())),
            Requirement::Throttle
        );
        let long = format!("https://a.test/{}", "x".repeat(MAX_URL));
        assert_eq!(requirement(&download(long)), Requirement::Refused);
        assert_eq!(
            requirement(&Service::Permission(Permission::Camera)),
            Requirement::Prompt(Permission::Camera)
        );
    }
}
