//! Privileged services a view asks the browser for, and the answers.
//!
//! An engine never performs these itself. It reports a [`ServiceRequest`] as
//! [`EngineEvent::ServiceRequested`](crate::events::EngineEvent::ServiceRequested); the core
//! decides whether the requesting document may have it; the executable performs what was
//! allowed; the answer goes back to the engine as
//! [`EngineCommand::ResolveServiceRequest`](crate::engine::EngineCommand::ResolveServiceRequest),
//! and the adapter completes the engine's own request object with it.
//!
//! - Every request names the view and the [`DocumentGeneration`] it acts for. Once that
//!   document is replaced, its outstanding requests are answered with
//!   [`ServiceError::Cancelled`], and an answer never applies to another document. A decision
//!   the core remembers beyond one document (the Wasm engine used to remember permissions for
//!   the tab's lifetime) is keyed by the request's `origin`, never by view or URL.
//! - Either side can cancel: the engine with
//!   [`EngineEvent::ServiceCancelled`](crate::events::EngineEvent::ServiceCancelled) when the
//!   page no longer wants the result, the browser by answering [`ServiceError::Cancelled`].
//! - Whatever the browser doesn't implement, or doesn't recognise, fails closed with
//!   [`ServiceError::Unsupported`]. Nothing succeeds silently.
//!
//! For downloaded Wasm, the engine enforces the answer at its host-function boundary, so a guest
//! cannot grant itself anything. For in-process native engines the contract is cooperative: it
//! keeps honest code correct, and does not contain an exploited engine.

use std::fmt;
use std::path::PathBuf;

use crate::ids::{DocumentGeneration, PtySession, RequestId, ViewId};

/// A view asking for a privileged service.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServiceRequest {
    /// Identifies the request until it is answered or cancelled.
    pub id: RequestId,
    /// The view that asked.
    pub view: ViewId,
    /// The document that asked. An answer is only valid while it is still the view's document.
    pub document: DocumentGeneration,
    /// Who is asking, for the prompt and for remembered decisions, as the adapter's trusted
    /// loading policy defines it, never as the page claims: for the Wasm engine the app origin
    /// of the app's final URL (`app_origin_of`: scheme, host and port, or the containing
    /// directory of a `file://` app); for Servo the committed top-level URL's origin for
    /// clipboard and file requests, and `None` for permission requests, since Servo 0.7 does
    /// not say which frame asks. `None` when the engine can't say: the core then always asks
    /// and never grants from an earlier answer.
    pub origin: Option<String>,
    /// What it asks for.
    pub service: Service,
}

/// The services a view can ask for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Service {
    /// Read the clipboard. Answered with [`ServiceReply::Clipboard`] holding the first
    /// available type from `mime_types`.
    ClipboardRead {
        /// Which clipboard.
        selection: Selection,
        /// Acceptable MIME types, most preferred first.
        mime_types: Vec<String>,
        /// The most bytes the engine accepts. The host applies its own cap too; a larger
        /// transfer is abandoned with [`ServiceError::TooLarge`].
        limit: usize,
    },
    /// Replace the clipboard's contents with `items`, one per MIME type offered. Answered with
    /// [`ServiceReply::Done`], or [`ServiceError::TooLarge`] beyond the host's cap.
    ClipboardWrite {
        /// Which clipboard.
        selection: Selection,
        /// The same content in each offered type.
        items: Vec<ClipboardData>,
    },
    /// Save something to the user's download folder. Answered with [`ServiceReply::Done`]
    /// once the browser has taken the download over.
    Download {
        /// What to save.
        source: DownloadSource,
        /// A file name the page proposes. The browser sanitizes it or picks its own.
        suggested_name: Option<String>,
    },
    /// Let the user pick files or a folder. Answered with [`ServiceReply::Files`], or
    /// [`ServiceError::Cancelled`] when the user dismisses the picker.
    PickFiles {
        /// What to pick.
        mode: PickMode,
        /// Accepted types, as MIME types ("image/png", "image/*") or extensions (".png").
        /// Empty accepts anything.
        filters: Vec<String>,
    },
    /// Hand a URL that no engine should open (`mailto:`, a custom scheme) to the desktop.
    /// Answered with [`ServiceReply::Done`] once handed over.
    OpenExternal {
        /// The URL.
        url: String,
    },
    /// Use a sensitive capability. Answered with [`ServiceReply::Done`] when granted; any
    /// error means not granted.
    Permission(Permission),
    /// What only Sighurt's own pages (`sig://help`, `sig://config`, `sig://terminal`) may ask
    /// for. The core grants it from its own record of which page the view is, fixed when the
    /// view opened, and that page's [`PageGrant`]s; never from the request's `origin` or a URL
    /// the engine reports. Any other view is refused.
    Page(PageRequest),
}

/// What an internal page asks for ([`Service::Page`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PageRequest {
    /// Facts about the browser, for the help page: answered with [`ServiceReply::Text`] in the
    /// form [`PageInfo`] documents. Never the configuration's text ([`Self::ConfigRead`]).
    Info(PageInfo),
    /// The user's configuration file as it is: [`ServiceReply::Text`]. A missing file is
    /// answered with what a new one would start as.
    ConfigRead,
    /// Whether `text` is a valid configuration for this build: [`ServiceReply::Text`] with one
    /// problem per line, empty when it is valid. Nothing is written.
    ConfigCheck {
        /// The configuration to check.
        text: String,
    },
    /// Replace the user's configuration file with `text`, if it still holds `replaces` (what
    /// the page read), and reload it: [`ServiceReply::Text`] saying what the reload did. A file
    /// that changed since is refused ([`ServiceError::Failed`]), and so is an invalid `text`.
    ConfigWrite {
        /// The new file.
        text: String,
        /// What the page read, which the file must still hold.
        replaces: String,
    },
    /// Start `program` on a new pseudo-terminal of `cols` by `rows` cells: answered with
    /// [`ServiceReply::Pty`], the session, once the program runs. The browser builds the
    /// program's arguments, environment and working directory; the page only chooses from this
    /// closed set. The master side reaches the engine by the session, once, outside this type
    /// (the Wasm engine's `pty::take`); no descriptor is in a service type.
    ///
    /// The core tracks the session (opening, open, closing, closed). Cancelling the request
    /// (the engine's `ServiceCancelled`, or the core when the document ends, the page becomes
    /// a history entry, the tab closes, the setting goes off or `sig` quits) ends the program.
    /// When the program has ended and been reaped, whether by itself or cancelled, the
    /// executable answers the same request a second time with [`ServiceReply::PtyEnded`], which
    /// frees the allowance; the engine gets it too while the document lasts.
    PtyOpen {
        /// What runs.
        program: PtyProgram,
        /// The terminal's width, in cells.
        cols: u16,
        /// The terminal's height, in cells.
        rows: u16,
    },
}

impl PageRequest {
    /// The grant a page needs for this request.
    pub fn grant(&self) -> PageGrant {
        match self {
            Self::Info(_) => PageGrant::Info,
            Self::ConfigRead | Self::ConfigCheck { .. } => PageGrant::ConfigRead,
            Self::ConfigWrite { .. } => PageGrant::ConfigWrite,
            Self::PtyOpen { .. } => PageGrant::Pty,
        }
    }
}

/// Which facts [`PageRequest::Info`] asks for. Each answer is UTF-8 text, one record per line,
/// its fields separated by tabs (`\t`); a field never holds a tab or a line break (the browser
/// replaces them with spaces). Fields are listed in order; a later version may add fields at
/// the end of a record, and readers ignore what they don't know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageInfo {
    /// `key, value` records: `version` (sig's version), `license`, `authors`, `frontend`
    /// (`window` or `terminal`), `keymap` (the active keymap's name), `chrome`, `renderer`.
    About,
    /// One record per engine of this build, in descriptor order: `name, enabled, default, web,
    /// capabilities` (`yes`/`no` for the three flags; capabilities a space-separated list of
    /// what the engine supports, such as `history page-zoom find semantics`).
    Engines,
    /// One record per built-in command, in [`crate::commands::DOCS`] order: `name, argument,
    /// description` (`argument` as the command line writes it, `[url]`, `<n>`, or empty).
    Commands,
    /// One record per `[commands]` entry of the configuration: `name, commands` (the commands
    /// it runs, as the configuration writes them, separated by ` ; `).
    UserCommands,
    /// The active keymap's bindings for the running frontend
    /// ([`crate::keymap::Keymap::describe`]): `mode, keys, action, applies`.
    Keys,
    /// `name, path` records: `config` (the configuration file), `config-dir`, `state-dir`,
    /// `session`, `history`, `downloads`. A path the build has none of is left out.
    Paths,
    /// `setting, value` records: the effective values of the settings a new user asks about
    /// (`home`, `search`, `zoom`, `engines.default`, `engines.enabled`, `input.keymap`,
    /// `ui.chrome`, `ui.renderer`, `session.restore`, `history.remember`, `pages.terminal`,
    /// `tui.mode`, `tui.graphics`).
    Settings,
}

impl PageInfo {
    /// Every kind, in a fixed order.
    pub const ALL: [PageInfo; 7] = [
        Self::About,
        Self::Engines,
        Self::Commands,
        Self::UserCommands,
        Self::Keys,
        Self::Paths,
        Self::Settings,
    ];

    /// Its name in the `sig` import module's `request` (`info:<name>`).
    pub fn name(self) -> &'static str {
        match self {
            Self::About => "about",
            Self::Engines => "engines",
            Self::Commands => "commands",
            Self::UserCommands => "user-commands",
            Self::Keys => "keys",
            Self::Paths => "paths",
            Self::Settings => "settings",
        }
    }

    /// The kind named `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// What [`PageRequest::PtyOpen`] runs. The browser decides the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PtyProgram {
    /// The user's shell: `[pages.terminal] command`, else `$SHELL`, else `/bin/sh`, in the home
    /// directory.
    Shell,
    /// The user's editor (`$VISUAL`, else `$EDITOR`) on the configuration file, in its
    /// directory.
    EditConfig,
}

/// What one internal page may ask for, per operation. Each page is given a fixed set at build
/// time (the `PAGES` table of `sighurt-pages`: help has `Info`; config the configuration's;
/// terminal `Pty`); the Wasm engine checks it at every `sig` host function and the core again
/// from its record of the view's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageGrant {
    /// [`PageRequest::Info`].
    Info,
    /// [`PageRequest::ConfigRead`] and [`PageRequest::ConfigCheck`].
    ConfigRead,
    /// [`PageRequest::ConfigWrite`].
    ConfigWrite,
    /// [`PageRequest::PtyOpen`].
    Pty,
    /// [`Service::ClipboardWrite`], under its usual rule (a click or key press in the page).
    Clipboard,
}

/// Which clipboard: Wayland keeps the ordinary clipboard and the primary selection apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Selection {
    /// The clipboard of copy and paste.
    Clipboard,
    /// The primary selection: the last selected text, pasted with the middle button.
    Primary,
}

/// Clipboard content in one MIME type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClipboardData {
    /// Its type, such as "text/plain;charset=utf-8".
    pub mime_type: String,
    /// The content.
    pub data: Vec<u8>,
}

/// What a download saves.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DownloadSource {
    /// A resource the browser fetches.
    Url(String),
    /// Bytes the page produced.
    Data(Vec<u8>),
}

/// What a file picker picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PickMode {
    /// One existing file.
    OpenFile,
    /// Any number of existing files.
    OpenFiles,
    /// One folder.
    OpenFolder,
}

/// A sensitive capability that needs the user's grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Permission {
    /// The camera.
    Camera,
    /// The microphone.
    Microphone,
    /// The user's location.
    Geolocation,
    /// Capturing the screen.
    ScreenCapture,
}

/// The answer to a [`ServiceRequest`].
pub type ServiceResult = Result<ServiceReply, ServiceError>;

/// A successful answer. Which one a service gets is documented on [`Service`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ServiceReply {
    /// Done or granted, with nothing to return.
    Done,
    /// Clipboard content that was read.
    Clipboard(ClipboardData),
    /// Files or a folder the user picked. The engine decides what its content may see of them:
    /// the Wasm engine hands guests opaque handles, never paths.
    Files(Vec<PathBuf>),
    /// Text an internal page asked for ([`PageRequest`]).
    Text(String),
    /// The terminal an internal page started runs ([`PageRequest::PtyOpen`]): its session.
    Pty(PtySession),
    /// The program of an answered [`PageRequest::PtyOpen`] has ended and was reaped: its exit
    /// status, or `None` when a signal ended it. The second and last answer of that request.
    PtyEnded {
        /// The exit status, if it exited.
        status: Option<i32>,
    },
}

/// Why a service request did not succeed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ServiceError {
    /// Policy or the user said no.
    Denied,
    /// Withdrawn, dismissed, or its document was replaced.
    Cancelled,
    /// The browser doesn't implement this request.
    Unsupported,
    /// The data exceeded the size limit.
    TooLarge,
    /// Allowed, but performing it failed. The message is for the user.
    Failed(String),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied => f.write_str("denied"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Unsupported => f.write_str("not supported"),
            Self::TooLarge => f.write_str("too large"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ServiceError {}
