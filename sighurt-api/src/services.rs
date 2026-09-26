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

use crate::ids::{DocumentGeneration, RequestId, ViewId};

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
    /// directory of a `file://` app); for Servo the origin of the committed top-level URL.
    /// `None` when the engine can't say.
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
