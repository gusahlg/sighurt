//! Policy for what pages ask the browser for: privileged services (`sighurt_api::services`),
//! new tabs and downloads.
//!
//! A request acts for one document, its view's current `DocumentGeneration`, and nothing
//! outlives that document: its pending requests are cancelled when it is replaced, a late
//! answer for it is dropped, and a permission the user grants it is granted to it alone. A
//! refusal is remembered for the tab's lifetime by the request's origin (never by view or URL),
//! so a page can't keep asking. A request without an origin (the engine can't say which frame
//! asks: Servo) is never granted from an earlier answer, and a grant to it covers it alone; its
//! refusal is remembered by the page's origin. Only the document a tab shows may ask for
//! anything.
//!
//! Pages get nothing without the user:
//!
//! - The clipboard, file pickers and handing a URL to the desktop need a click or key press in
//!   the page within the last [`ACTIVATION`] (user activation); reading the clipboard, picking
//!   files and opening a URL elsewhere use it up. A URL whose scheme makes the desktop mount a
//!   share or connect to a remote machine (`smb:`, `sftp:`, `ssh:`, `vnc:` and the like), maybe
//!   with credentials it keeps, is never handed to it.
//! - A new tab needs activation too, and uses it up, so one click opens one tab. A page never
//!   opens Sighurt's `sig:` pages, and opens `file:` URLs only if it is a `file:` page itself.
//! - Camera, microphone, location and screen capture need the user's answer to a prompt.
//! - Downloads are limited to one per [`DOWNLOAD_INTERVAL`] per tab.
//! - What only Sighurt's own pages may ask for (`Service::Page`) is granted from the core's own
//!   record of the view's page and that page's grants, never from the request's origin, which
//!   must be the page's own (`sig://<name>`) as well. A terminal (`PtyOpen`) needs the page's
//!   grant for its program (`sig://terminal` the shell, `sig://config` the editor), the page
//!   shown in the selected tab, with the keyboard, and a click or a fresh key press in it
//!   within the last [`ACTIVATION`], which it uses up with the page's activation (a missing
//!   grant is refused first, so it uses up nothing). The key press counts only if it was delivered to that page, or its own
//!   `page.send-key` was, with the page and its document shown and with the keyboard when it
//!   was pressed and until then (a command before it in the key's list that moved the keyboard
//!   to another page, even and then back, takes it away). A repeat never counts, and neither
//!   does a press or click while the document has a terminal: those are its program's. It
//!   also needs its
//!   page's setting (`[pages.terminal] enabled` for the shell, `[pages.config] enabled` for the
//!   editor), no other terminal of the document, and fewer than [`MAX_PTYS`] in all, shells
//!   and editors together. The core tracks each terminal ([`Ptys`]): opening, open, closing,
//!   closed, the allowance kept until it is closed (the runtime says the program was reaped).
//!
//! A request that fits none of this is refused. Requests are bounded per view
//! ([`MAX_PENDING_REQUESTS`]).
//!
//! Must not own: performing a service (the executable does) or the prompt (the chrome).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use sighurt_api::ids::{DocumentGeneration, PtySession, RequestId, ViewId};
use sighurt_api::services::{DownloadSource, PageGrant, Permission, PtyProgram, Service};

use crate::tabs::MAX_URL;

/// How long a click or key press in a page lets it do what pages may not do by themselves.
pub const ACTIVATION: Duration = Duration::from_secs(5);

/// How often pages in one tab may start a download.
pub const DOWNLOAD_INTERVAL: Duration = Duration::from_secs(1);

/// The most service requests one view may have waiting for an answer.
pub const MAX_PENDING_REQUESTS: usize = 16;

/// The most terminals (`PtyOpen`) running at once, in all.
pub const MAX_PTYS: usize = 8;

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
    /// An internal page's request: the view must be that page, with this grant (and for a
    /// terminal, the rest of the module documentation's rule).
    Page(PageGrant),
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
        Service::Page(request) => Requirement::Page(request.grant()),
    }
}

/// Schemes the desktop's handlers mount a file share with, browse files through or connect to
/// a remote machine with, perhaps with credentials they keep: a page never hands them over.
const NEVER_EXTERNAL: [&str; 29] = [
    "smb",
    "cifs",
    "nfs",
    "afp",
    "sftp",
    "ftp",
    "ftps",
    "fish",
    "dav",
    "davs",
    "webdav",
    "webdavs",
    "ssh",
    "telnet",
    "vnc",
    "rdp",
    "spice",
    "x11",
    "admin",
    "network",
    "computer",
    "trash",
    "recent",
    "burn",
    "archive",
    "mtp",
    "afc",
    "gphoto2",
    "google-drive",
];

/// Whether `url` is one to hand to the desktop (`mailto:`, a custom scheme) rather than one the
/// browser opens itself, that runs code, or that reaches files or machines ([`NEVER_EXTERNAL`]).
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
    url.len() <= MAX_URL
        && url::Url::parse(url).is_ok_and(|url| {
            !OURS.contains(&url.scheme()) && !NEVER_EXTERNAL.contains(&url.scheme())
        })
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

/// Where a terminal an internal page started is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PtyState {
    /// Allowed, and asked of the executable; no answer yet.
    Opening,
    /// Its program runs.
    Open(PtySession),
    /// Cancelled: the executable is ending its program, and says when it was reaped.
    Closing,
}

/// One terminal, from its request's allowance until its program was reaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pty {
    pub view: ViewId,
    /// The document it belongs to: it ends with it.
    pub document: DocumentGeneration,
    /// What runs: its page's setting keeps it going.
    pub program: PtyProgram,
    pub state: PtyState,
}

/// The terminals, by the request that started each. A terminal counts against the limits in
/// every state, so a program being ended still holds its place.
#[derive(Clone, Debug, Default)]
pub(crate) struct Ptys(BTreeMap<RequestId, Pty>);

impl Ptys {
    pub(crate) fn get(&self, request: RequestId) -> Option<&Pty> {
        self.0.get(&request)
    }

    pub(crate) fn insert(&mut self, request: RequestId, pty: Pty) {
        self.0.insert(request, pty);
    }

    pub(crate) fn remove(&mut self, request: RequestId) -> Option<Pty> {
        self.0.remove(&request)
    }

    /// Whether a terminal may start for `document` of `view`: it has none, and there are fewer
    /// than [`MAX_PTYS`].
    pub(crate) fn room(&self, view: ViewId, document: DocumentGeneration) -> bool {
        self.0.len() < MAX_PTYS && !self.runs(view, document)
    }

    /// Whether `document` of `view` has a terminal, in any state.
    pub(crate) fn runs(&self, view: ViewId, document: DocumentGeneration) -> bool {
        self.0
            .values()
            .any(|pty| pty.view == view && pty.document == document)
    }

    /// Moves `request`'s terminal to closing; whether it was not closing yet.
    pub(crate) fn close_one(&mut self, request: RequestId) -> bool {
        match self.0.get_mut(&request) {
            Some(pty) if pty.state != PtyState::Closing => {
                pty.state = PtyState::Closing;
                true
            }
            _ => false,
        }
    }

    /// Moves every terminal `end` selects that is not closing yet to closing, and returns
    /// their requests, to cancel.
    pub(crate) fn close(&mut self, mut end: impl FnMut(&Pty) -> bool) -> Vec<RequestId> {
        let mut closed = Vec::new();
        for (request, pty) in &mut self.0 {
            if pty.state != PtyState::Closing && end(pty) {
                pty.state = PtyState::Closing;
                closed.push(*request);
            }
        }
        closed
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
            "smb://server/share",
            "SFTP://me@host/",
            "ssh://host",
            "davs://host/dav",
            "vnc://host:5900",
            "rdp://host",
            "admin:///etc/shadow",
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

#[cfg(test)]
#[path = "permissions_fuzz.rs"]
mod fuzz_tests;
