//! What the page asks of the browser, and a browser made of memory for running the page as
//! an ordinary guest (and for tests).
//!
//! [`Host`] has the shape of the privileged `sig` module's requests: a request gets an id,
//! is polled until it has an answer, and may be cancelled. [`FakeHost`] answers at once from
//! an in-memory file, and checks a text only roughly (a `version = 1` line, balanced
//! brackets and quotes); the real check is the browser's.
//!
//! Must not own: what the answers mean (`reply`, the page).

/// A request to the browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    ConfigRead,
    ConfigCheck {
        text: String,
    },
    ConfigWrite {
        text: String,
        replaces: String,
    },
    /// Put text on the clipboard (copy and cut).
    ClipboardWrite {
        text: String,
    },
}

/// Where a request is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    Pending,
    /// The answer's text, or why there is none.
    Done(Result<String, String>),
}

/// The browser, as the page sees it.
pub trait Host {
    /// Sends `request`; its id, or why it was refused at once.
    fn request(&mut self, request: Request) -> Result<u32, String>;
    /// Where request `id` is. A finished request is forgotten once its answer is taken.
    fn poll(&mut self, id: u32) -> Poll;
    /// Forgets request `id`; its answer, if one comes, is dropped.
    fn cancel(&mut self, id: u32);
}

/// A browser that keeps the file in memory.
#[derive(Debug)]
pub struct FakeHost {
    pub path: String,
    /// The file; `None` when there is none.
    pub file: Option<String>,
    pub read_only: bool,
    /// What was last copied.
    pub clipboard: String,
    /// Requests not answered yet, with their answers; `hold` keeps them pending.
    answers: Vec<(u32, Result<String, String>)>,
    pub hold: bool,
    next: u32,
    /// Every request, for tests.
    pub sent: Vec<Request>,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            path: "(in memory) config.toml".into(),
            file: Some(DEMO.into()),
            read_only: false,
            clipboard: String::new(),
            answers: Vec::new(),
            hold: false,
            next: 1,
            sent: Vec::new(),
        }
    }
}

/// The file the page starts with when it runs as an ordinary guest.
const DEMO: &str = "# A configuration in memory: this page runs as an ordinary guest, and \
                    saving changes nothing on disk.\nversion = 1\nzoom = 1.25\n\n[ui]\n\
                    font-size = 14\n\n[keywords]\nw = \"https://en.wikipedia.org/wiki/{}\"\n";

impl FakeHost {
    fn answer(&mut self, request: &Request) -> Result<String, String> {
        Ok(match request {
            Request::ConfigRead => match &self.file {
                Some(text) if self.read_only => format!("read-only {}\n{text}", self.path),
                Some(text) => format!("file {}\n{text}", self.path),
                None => format!("new {}\nversion = 1\n", self.path),
            },
            Request::ConfigCheck { text } => rough_check(text),
            Request::ConfigWrite { text, replaces } => {
                if self.file.as_deref().unwrap_or("") != replaces {
                    "stale\nThe file changed since this page read it.".into()
                } else if self.read_only {
                    "refused\nThe file is read-only.".into()
                } else if rough_check(text).starts_with("invalid") {
                    rough_check(text)
                } else {
                    self.file = Some(text.clone());
                    "saved\n".into()
                }
            }
            Request::ClipboardWrite { text } => {
                self.clipboard = text.clone();
                String::new()
            }
        })
    }
}

impl Host for FakeHost {
    fn request(&mut self, request: Request) -> Result<u32, String> {
        let id = self.next;
        self.next += 1;
        let answer = self.answer(&request);
        self.sent.push(request);
        self.answers.push((id, answer));
        Ok(id)
    }

    fn poll(&mut self, id: u32) -> Poll {
        if self.hold {
            return Poll::Pending;
        }
        match self.answers.iter().position(|(i, _)| *i == id) {
            Some(at) => Poll::Done(self.answers.remove(at).1),
            None => Poll::Done(Err("no such request".into())),
        }
    }

    fn cancel(&mut self, id: u32) {
        self.answers.retain(|(i, _)| *i != id);
    }
}

/// A rough check, in the runtime's answer format.
fn rough_check(text: &str) -> String {
    let mut problems = Vec::new();
    if !text
        .lines()
        .any(|l| l.trim().replace(' ', "") == "version=1")
    {
        problems.push("problem 0 the file needs `version = 1` at the top".to_owned());
    }
    for (n, line) in text.lines().enumerate() {
        let code = line.split('#').next().unwrap_or("").trim();
        if code.starts_with('[') && !code.ends_with(']') {
            problems.push(format!(
                "problem {} line {}: a table header ends with ]",
                n + 1,
                n + 1
            ));
        } else if code.matches('"').count() % 2 == 1 {
            problems.push(format!(
                "problem {} line {}: a string is not closed",
                n + 1,
                n + 1
            ));
        } else if !code.is_empty() && !code.starts_with('[') && !code.contains('=') {
            problems.push(format!(
                "problem {} line {}: expected `key = value`",
                n + 1,
                n + 1
            ));
        }
    }
    if problems.is_empty() {
        "valid\n".into()
    } else {
        format!("invalid\n{}\n", problems.join("\n"))
    }
}
