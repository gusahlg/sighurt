//! Servo's callbacks: page state goes to the shell as [`FromEngine`] messages.

use std::cell::{Cell, RefCell};
use std::io::{BufWriter, Write};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex, PoisonError};

use servo::{
    ConsoleLogLevel, CreateNewWebViewRequest, Cursor, LoadStatus, NavigationRequest,
    RenderingContext, ServoDelegate, ServoError, WebView, WebViewDelegate,
};
use sighurt_ipc::FromEngine;
use url::Url;

/// The protocol stream to the shell. Shared with the panic hook.
pub type Output = Arc<Mutex<BufWriter<Box<dyn Write + Send>>>>;

/// Shared state for the page's `WebViewDelegate` callbacks.
pub struct Engine {
    out: Output,
    pub context: Rc<dyn RenderingContext>,
    /// Set when Servo has a new frame; taken by the render step after each spin.
    pub frame_ready: Cell<bool>,
    /// Set when the shell stopped reading. The engine then shuts down.
    pub closed: Cell<bool>,
    /// Auxiliary webviews created for `target=_blank` / `window.open`. They are never shown:
    /// their first navigation is handed to the shell as a new-tab request.
    popups: RefCell<Vec<WebView>>,
    /// This engine as a delegate for popups.
    this: Weak<Engine>,
}

impl Engine {
    pub fn new(out: Output, context: Rc<dyn RenderingContext>) -> Rc<Self> {
        Rc::new_cyclic(|this| Self {
            out,
            context,
            frame_ready: Cell::new(false),
            closed: Cell::new(false),
            popups: RefCell::default(),
            this: this.clone(),
        })
    }

    pub fn send(&self, msg: &FromEngine) {
        if self.closed.get() {
            return;
        }
        let mut out = self.out.lock().unwrap_or_else(PoisonError::into_inner);
        if msg.write_to(&mut *out).is_err() {
            // The shell is gone; nothing left to render for.
            self.closed.set(true);
        }
    }

    /// Sends `msg` about `webview` unless that is a popup, which the shell doesn't know about.
    fn send_for(&self, webview: &WebView, msg: FromEngine) {
        if !self.is_popup(webview) {
            self.send(&msg);
        }
    }

    fn is_popup(&self, webview: &WebView) -> bool {
        self.popups.borrow().contains(webview)
    }

    /// Forwards a popup's destination to the shell and drops the popup.
    fn catch_popup(&self, webview: &WebView, url: &Url) {
        if url.as_str() == "about:blank" {
            return;
        }
        self.popups.borrow_mut().retain(|popup| popup != webview);
        self.send(&FromEngine::Open {
            url: url.to_string(),
            new_tab: true,
        });
    }
}

impl ServoDelegate for Engine {
    fn notify_error(&self, error: ServoError) {
        if let ServoError::LostConnectionWithBackend = error {
            // Servo's core is gone and cannot be restarted in this process.
            self.send(&FromEngine::Error {
                message: "Servo lost the connection to its backend".into(),
            });
            std::process::exit(1);
        }
    }
}

impl WebViewDelegate for Engine {
    fn notify_new_frame_ready(&self, webview: WebView) {
        if !self.is_popup(&webview) {
            self.frame_ready.set(true);
        }
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        if self.is_popup(&webview) {
            return self.catch_popup(&webview, &url);
        }
        self.send(&FromEngine::Url { url: url.into() });
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        let title = title.unwrap_or_default();
        self.send_for(&webview, FromEngine::Title { title });
    }

    fn notify_status_text_changed(&self, webview: WebView, status: Option<String>) {
        let text = status.unwrap_or_default();
        self.send_for(&webview, FromEngine::Status { text });
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        let loading = status != LoadStatus::Complete;
        self.send_for(&webview, FromEngine::Loading { loading });
    }

    fn notify_history_changed(&self, webview: WebView, entries: Vec<Url>, current: usize) {
        let (can_back, can_forward) = (current > 0, current + 1 < entries.len());
        self.send_for(
            &webview,
            FromEngine::History {
                can_back,
                can_forward,
            },
        );
    }

    fn notify_cursor_changed(&self, webview: WebView, cursor: Cursor) {
        let name = css_cursor(cursor);
        self.send_for(&webview, FromEngine::Cursor { name });
    }

    fn notify_crashed(&self, webview: WebView, reason: String, _backtrace: Option<String>) {
        let message = format!("page crashed: {reason}");
        self.send_for(&webview, FromEngine::Error { message });
    }

    fn request_navigation(&self, webview: WebView, request: NavigationRequest) {
        if self.is_popup(&webview) {
            self.catch_popup(&webview, &request.url);
            request.deny();
        }
        // Dropping the request allows it.
    }

    fn request_create_new(&self, _parent: WebView, request: CreateNewWebViewRequest) {
        let Some(this) = self.this.upgrade() else {
            return;
        };
        let popup = request.builder(self.context.clone()).delegate(this).build();
        // Webviews start visible, and this one shares the page's rendering context.
        popup.hide();
        self.popups.borrow_mut().push(popup);
    }

    fn show_console_message(&self, _webview: WebView, level: ConsoleLogLevel, message: String) {
        let level = match level {
            ConsoleLogLevel::Log | ConsoleLogLevel::Dir => 0,
            ConsoleLogLevel::Info => 1,
            ConsoleLogLevel::Warn => 2,
            ConsoleLogLevel::Error => 3,
            ConsoleLogLevel::Debug | ConsoleLogLevel::Trace => 4,
        };
        self.send(&FromEngine::Console { level, message });
    }
}

/// Servo's cursor as a CSS keyword: `NeswResize` becomes "nesw-resize". The variant names are
/// the CSS keywords in camel case.
fn css_cursor(cursor: Cursor) -> String {
    let mut name = String::new();
    for c in format!("{cursor:?}").chars() {
        if c.is_ascii_uppercase() && !name.is_empty() {
            name.push('-');
        }
        name.push(c.to_ascii_lowercase());
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_are_css_keywords() {
        assert_eq!(css_cursor(Cursor::Default), "default");
        assert_eq!(css_cursor(Cursor::ContextMenu), "context-menu");
        assert_eq!(css_cursor(Cursor::NeswResize), "nesw-resize");
        assert_eq!(css_cursor(Cursor::ZoomIn), "zoom-in");
    }
}
