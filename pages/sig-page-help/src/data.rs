//! The facts the browser tells the page: one [`Slot`] per `info` request, polled until it is
//! answered. Owns the requests and their parsed answers; says nothing about how they look.

use sig_page_kit::sig::{Poll, Request};
use sig_page_kit::ui::records;

/// Where one answer is.
pub enum State {
    /// Asked, not answered.
    Loading,
    /// Answered: the records, fields split at tabs.
    Ready(Vec<Vec<String>>),
    /// Refused or failed, with why.
    Failed(String),
}

/// One `info` request and its answer.
pub struct Slot {
    request: Option<Request>,
    /// Where the answer is.
    pub state: State,
}

impl Slot {
    fn ask(name: &str) -> Self {
        match Request::info(name) {
            Some(request) => Self {
                request: Some(request),
                state: State::Loading,
            },
            None => Self {
                request: None,
                state: State::Failed("the browser refused the request".into()),
            },
        }
    }

    /// Polls an unanswered request; true when the answer changed.
    fn poll(&mut self) -> bool {
        let Some(request) = &self.request else {
            return false;
        };
        self.state = match request.poll() {
            Poll::Waiting => return false,
            Poll::Done(text) => State::Ready(records(&text, 1)),
            Poll::Failed(why) => State::Failed(why),
            Poll::Gone => State::Failed("the request was lost".into()),
        };
        self.request = None;
        true
    }

    /// The value of the `key, value` record named `key`.
    pub fn value(&self, key: &str) -> Option<&str> {
        let State::Ready(rows) = &self.state else {
            return None;
        };
        rows.iter()
            .find(|r| r[0] == key && r.len() > 1)
            .map(|r| r[1].as_str())
    }
}

/// Everything the page asks for.
pub struct Data {
    /// `about`: version, license, authors, frontend, keymap, chrome, renderer.
    pub about: Slot,
    /// `engines`.
    pub engines: Slot,
    /// `commands`.
    pub commands: Slot,
    /// `user-commands`.
    pub user_commands: Slot,
    /// `keys`.
    pub keys: Slot,
    /// `paths`.
    pub paths: Slot,
    /// `settings`.
    pub settings: Slot,
    /// Counts answers, so the layout knows to be made again.
    pub version: u32,
}

impl Data {
    /// Sends every request.
    pub fn ask() -> Self {
        Self {
            about: Slot::ask("about"),
            engines: Slot::ask("engines"),
            commands: Slot::ask("commands"),
            user_commands: Slot::ask("user-commands"),
            keys: Slot::ask("keys"),
            paths: Slot::ask("paths"),
            settings: Slot::ask("settings"),
            version: 0,
        }
    }

    /// Polls the unanswered requests.
    pub fn poll(&mut self) {
        let slots = [
            &mut self.about,
            &mut self.engines,
            &mut self.commands,
            &mut self.user_commands,
            &mut self.keys,
            &mut self.paths,
            &mut self.settings,
        ];
        let mut changed = false;
        for slot in slots {
            changed |= slot.poll();
        }
        if changed {
            self.version += 1;
        }
    }
}
