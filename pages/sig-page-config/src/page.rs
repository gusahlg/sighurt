//! The page: the editor over the configuration file, and its conversation with the browser
//! (read, a check while the user types, save, read again after a stale save), the layout of
//! the page's parts, and where a click lands.
//!
//! A check is sent [`CHECK_DELAY`] after the last change, one at a time; an answer about a
//! text that has changed since is dropped and the text is checked again. A save sends the
//! text with what the page last read from the file (`replaces`), so the browser refuses it
//! when the file changed meanwhile; the page then offers to read it again, which replaces the
//! text as one step undo takes back.
//!
//! Must not own: the editing itself (the editor), the answers' format (`reply`), or drawing
//! and the host's calls (the wasm entry points).

use crate::editor::{Action, Editor, Input};
use crate::host::{Host, Poll, Request};
use crate::layout::Widths;
use crate::reply::{self, Problem};

/// How long after the last change the text is checked, in milliseconds.
pub const CHECK_DELAY: u64 = 400;
/// Most rows of the panel under the editor.
pub const PANEL_ROWS: usize = 6;

/// A rectangle, in canvas pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Sizes the layout is made from, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sizes {
    /// The editor's text and line height.
    pub font: f32,
    pub line: f32,
    /// The header's, status line's and panel's text and line height.
    pub ui_font: f32,
    pub ui_line: f32,
    pub pad: f32,
}

impl Default for Sizes {
    fn default() -> Self {
        Self {
            font: 14.0,
            line: 14.0 * 1.2,
            ui_font: 13.0,
            ui_line: 13.0 * 1.2,
            pad: 12.0,
        }
    }
}

/// Where the page's parts are.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Layout {
    pub header: Rect,
    /// The "Read the file again" button, when a save was refused as stale.
    pub reload: Option<Rect>,
    /// The Save button, when the file can be saved.
    pub save: Option<Rect>,
    pub gutter: Rect,
    pub text: Rect,
    pub status: Rect,
    pub panel: Rect,
}

/// A row of the panel under the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    pub text: String,
    /// The line it is about, 1-based.
    pub line: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Info,
    Error,
    Problem,
    Notice,
    Restart,
}

/// What the page knows of the check of the current text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Validity {
    Unknown,
    Checking,
    Valid,
    Invalid(usize),
}

enum State {
    Loading,
    Ready,
    /// The file can't be edited here; why.
    Unavailable(String),
}

/// The page.
pub struct Page {
    pub editor: Editor,
    pub widths: Widths,
    pub sizes: Sizes,
    state: State,
    pub path: String,
    /// The file a save replaces: where the configuration's links lead.
    pub target: String,
    /// What the file held when the page last read or wrote it (`None`: there was none): what
    /// a save replaces.
    disk: Option<String>,
    /// The editor's revision that `disk` holds.
    saved: u64,
    pub read_only: bool,
    /// A read in flight, and whether it replaces the text (a read again) or starts the page.
    read: Option<u32>,
    /// A copy to the clipboard in flight.
    copy: Option<u32>,
    /// A save in flight: its id, and the text and revision sent.
    save: Option<(u32, String, u64)>,
    /// A check in flight and the revision it is about; when the next is due.
    check: Option<(u32, u64)>,
    check_due: Option<u64>,
    /// The revision the problems are about.
    checked: Option<u64>,
    pub problems: Vec<Problem>,
    pub notices: Vec<Problem>,
    valid: Option<bool>,
    /// The last save was refused because the file changed.
    pub stale: bool,
    /// What the last save said applies only when sig starts again, and its other notices.
    restart: Vec<String>,
    reloaded: Vec<String>,
    /// A message for the user, an error or not.
    message: Option<(String, bool)>,
    /// The canvas size the layout is for.
    size: (f32, f32),
    pub layout: Layout,
    /// The digits the gutter has room for.
    digits: usize,
    dirty: bool,
}

impl Page {
    pub fn new(widths: Widths) -> Self {
        Self {
            editor: Editor::new(""),
            widths,
            sizes: Sizes::default(),
            state: State::Loading,
            path: String::new(),
            target: String::new(),
            disk: None,
            saved: 0,
            read_only: false,
            read: None,
            copy: None,
            save: None,
            check: None,
            check_due: None,
            checked: None,
            problems: Vec::new(),
            notices: Vec::new(),
            valid: None,
            stale: false,
            restart: Vec::new(),
            reloaded: Vec::new(),
            message: None,
            size: (0.0, 0.0),
            layout: Layout::default(),
            digits: 0,
            dirty: true,
        }
    }

    /// Reads the file.
    pub fn start(&mut self, host: &mut dyn Host) {
        self.read = self.send(host, Request::ConfigRead);
    }

    /// Whether the page is showing the file (not loading or refused).
    pub fn ready(&self) -> bool {
        matches!(self.state, State::Ready)
    }

    /// Why the file can't be edited, if it can't.
    pub fn unavailable(&self) -> Option<&str> {
        match &self.state {
            State::Unavailable(why) => Some(why),
            _ => None,
        }
    }

    pub fn modified(&self) -> bool {
        self.ready() && self.editor.revision() != self.saved
    }

    pub fn saving(&self) -> bool {
        self.save.is_some()
    }

    pub fn validity(&self) -> Validity {
        if self.checked != Some(self.editor.revision()) {
            return if self.check.is_some() || self.check_due.is_some() {
                Validity::Checking
            } else {
                Validity::Unknown
            };
        }
        match self.valid {
            Some(true) => Validity::Valid,
            Some(false) => Validity::Invalid(self.problems.len()),
            None => Validity::Unknown,
        }
    }

    /// Takes whether the page must be drawn again.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Lays the page out on a canvas of `w` by `h`; marks it for drawing when that changed.
    pub fn resize(&mut self, w: f32, h: f32) {
        if self.size == (w, h) && self.layout != Layout::default() {
            return;
        }
        self.size = (w, h);
        self.relayout();
    }

    /// The rows of the panel: the message, what needs a restart, problems, notices.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some((text, error)) = &self.message {
            let kind = if *error {
                RowKind::Error
            } else {
                RowKind::Info
            };
            for line in text.lines() {
                rows.push(Row {
                    kind,
                    text: line.to_owned(),
                    line: None,
                });
            }
        }
        for text in &self.restart {
            rows.push(Row {
                kind: RowKind::Restart,
                text: text.clone(),
                line: None,
            });
        }
        for text in &self.reloaded {
            rows.push(Row {
                kind: RowKind::Notice,
                text: text.clone(),
                line: None,
            });
        }
        let fresh = self.checked == Some(self.editor.revision());
        let listed = |p: &Problem, kind| Row {
            kind,
            text: p.text.clone(),
            line: (p.line > 0).then_some(p.line),
        };
        if fresh || self.valid == Some(false) {
            rows.extend(self.problems.iter().map(|p| listed(p, RowKind::Problem)));
            rows.extend(self.notices.iter().map(|p| listed(p, RowKind::Notice)));
        }
        rows
    }

    fn relayout(&mut self) {
        let (w, h) = self.size;
        let s = self.sizes;
        let header = Rect {
            x: 0.0,
            y: 0.0,
            w,
            h: s.pad * 1.5 + s.ui_line * 2.0,
        };
        // Buttons at the header's right: Save, and Read the file again after a stale save.
        let button = |right: f32, width: f32| Rect {
            x: right - width,
            y: s.pad * 0.5,
            w: width,
            h: s.ui_line * 1.5,
        };
        let save =
            (self.ready() && !self.read_only).then(|| button(w - s.pad, 70.0_f32.min(w / 4.0)));
        let left = save.map_or(w - s.pad, |b| b.x - s.pad * 0.5);
        let reload = self.stale.then(|| button(left, 170.0_f32.min(w / 3.0)));
        let rows = self.rows().len().min(PANEL_ROWS);
        let panel_h = if rows == 0 {
            0.0
        } else {
            rows as f32 * s.ui_line + s.pad
        };
        let status_h = s.ui_line + s.pad * 0.5;
        let panel = Rect {
            x: 0.0,
            y: (h - panel_h).max(0.0),
            w,
            h: panel_h,
        };
        let status = Rect {
            x: 0.0,
            y: (panel.y - status_h).max(0.0),
            w,
            h: status_h,
        };
        let top = header.h;
        let body_h = (status.y - top).max(0.0);
        self.digits = digits(self.editor.buffer().line_count());
        // At least two digits' room, so the first lines don't move the text.
        let gutter_w = self.digits.max(2) as f32 * self.widths.x_of("0", 1) + s.pad * 1.5;
        let gutter = Rect {
            x: 0.0,
            y: top,
            w: gutter_w,
            h: body_h,
        };
        let text = Rect {
            x: gutter_w + s.pad * 0.5,
            y: top,
            w: (w - gutter_w - s.pad).max(1.0),
            h: body_h,
        };
        let rows = (body_h / s.line).floor().max(1.0) as usize;
        self.editor.set_view(rows, text.w, s.line);
        self.layout = Layout {
            header,
            reload,
            save,
            gutter,
            text,
            status,
            panel,
        };
        self.editor.follow_caret(&mut self.widths);
        self.dirty = true;
    }

    /// Handles one input at time `now` (milliseconds). Pointer inputs are in canvas pixels.
    pub fn input(&mut self, input: Input, host: &mut dyn Host, now: u64) {
        let input = match input {
            Input::Press { x, y, shift } => {
                if !self.press(x, y, host) {
                    return;
                }
                let t = self.layout.text;
                Input::Press {
                    x: x - t.x,
                    y: y - t.y,
                    shift,
                }
            }
            Input::Drag { x, y } => {
                let t = self.layout.text;
                Input::Drag {
                    x: x - t.x,
                    y: y - t.y,
                }
            }
            other => other,
        };
        if !self.ready() {
            return;
        }
        let revision = self.editor.revision();
        let action = self.editor.input(input, &mut self.widths);
        match action {
            Some(Action::Copy(text)) => {
                if let Some(old) = self.copy.take() {
                    host.cancel(old);
                }
                match host.request(Request::ClipboardWrite { text }) {
                    Ok(id) => self.copy = Some(id),
                    Err(why) => self.say(format!("Nothing was copied: {why}"), true),
                }
            }
            Some(Action::Save) => self.save_now(host),
            Some(Action::Say(text)) => self.say(text.to_owned(), false),
            None => {}
        }
        if self.editor.revision() != revision {
            self.check_due = Some(now + CHECK_DELAY);
            if digits(self.editor.buffer().line_count()) != self.digits {
                self.relayout();
            }
        }
        self.editor.follow_caret(&mut self.widths);
        self.dirty = true;
    }

    /// A press outside the text: a button or a row of the panel. Whether it is the text's.
    fn press(&mut self, x: f32, y: f32, host: &mut dyn Host) -> bool {
        if let Some(button) = self.layout.save {
            if button.contains(x, y) {
                self.save_now(host);
                return false;
            }
        }
        if let Some(button) = self.layout.reload {
            if button.contains(x, y) {
                self.read_again(host);
                return false;
            }
        }
        let panel = self.layout.panel;
        if panel.contains(x, y) {
            let row = ((y - panel.y - self.sizes.pad * 0.5) / self.sizes.ui_line).floor();
            if row >= 0.0 {
                if let Some(line) = self.rows().get(row as usize).and_then(|r| r.line) {
                    self.editor.go_to_line(line - 1);
                    self.editor.follow_caret(&mut self.widths);
                    self.dirty = true;
                }
            }
            return false;
        }
        self.layout.text.contains(x, y) || self.layout.gutter.contains(x, y)
    }

    /// Saves the text over what the page last read.
    pub fn save_now(&mut self, host: &mut dyn Host) {
        if self.save.is_some() {
            return;
        }
        if self.read_only {
            self.say(
                format!("{} is read-only: it can't be saved from here.", self.path),
                true,
            );
            return;
        }
        let text = self.editor.text();
        let request = Request::ConfigWrite {
            text: text.clone(),
            replaces: reply::Read::replaces(&self.target, self.disk.as_deref()),
        };
        if let Some(id) = self.send(host, request) {
            self.save = Some((id, text, self.editor.revision()));
            self.say("Saving…".into(), false);
        }
    }

    /// Reads the file again, replacing the text (undo takes it back).
    pub fn read_again(&mut self, host: &mut dyn Host) {
        if self.read.is_none() {
            self.read = self.send(host, Request::ConfigRead);
        }
    }

    fn send(&mut self, host: &mut dyn Host, request: Request) -> Option<u32> {
        match host.request(request) {
            Ok(id) => Some(id),
            Err(why) => {
                self.say(format!("sig refused the request: {why}"), true);
                None
            }
        }
    }

    fn say(&mut self, text: String, error: bool) {
        self.message = Some((text, error));
        self.relayout();
    }

    /// Takes the answers that came and sends the check that is due, at time `now`.
    pub fn tick(&mut self, host: &mut dyn Host, now: u64) {
        if let Some(id) = self.copy {
            if let Poll::Done(answer) = host.poll(id) {
                self.copy = None;
                if let Err(why) = answer {
                    self.say(format!("Nothing was copied: {why}"), true);
                }
            }
        }
        if let Some(id) = self.read {
            if let Poll::Done(answer) = host.poll(id) {
                self.read = None;
                self.read_done(answer, now);
            }
        }
        if let Some((id, _, _)) = &self.save {
            if let Poll::Done(answer) = host.poll(*id) {
                let (_, text, revision) = self.save.take().expect("checked");
                self.save_done(answer, text, revision);
            }
        }
        if let Some((id, revision)) = self.check {
            if let Poll::Done(answer) = host.poll(id) {
                self.check = None;
                if let (Ok(answer), true) = (answer, revision == self.editor.revision()) {
                    self.checked = Some(revision);
                    self.set_check(reply::check(&answer));
                }
                self.dirty = true;
            }
        }
        if self.check.is_none() && self.check_due.is_some_and(|due| now >= due) && self.ready() {
            self.check_due = None;
            let text = self.editor.text();
            let revision = self.editor.revision();
            self.check = self
                .send(host, Request::ConfigCheck { text })
                .map(|id| (id, revision));
            self.dirty = true;
        }
    }

    fn set_check(&mut self, check: reply::Check) {
        let (valid, problems, notices) = match check {
            reply::Check::Valid(notices) => (true, Vec::new(), notices),
            reply::Check::Invalid(problems) => (false, problems, Vec::new()),
        };
        self.valid = Some(valid);
        self.problems = problems;
        self.notices = notices;
        self.relayout();
    }

    fn read_done(&mut self, answer: Result<String, String>, now: u64) {
        let answer = match answer {
            Ok(answer) => reply::read(&answer),
            Err(why) if why.starts_with("refused\n") => reply::read(&why),
            Err(why) => reply::Read::Refused(format!("sig could not read the file: {why}")),
        };
        let first = !self.ready();
        let (path, target, text, disk, read_only) = match answer {
            reply::Read::File {
                path,
                target,
                text,
                read_only,
            } => (path, target, text.clone(), Some(text), read_only),
            reply::Read::New { path, target, text } => (path, target, text, None, false),
            reply::Read::Refused(why) => {
                if first {
                    self.state = State::Unavailable(why);
                } else {
                    self.say(why, true);
                }
                self.dirty = true;
                return;
            }
        };
        if first {
            self.editor = Editor::new(&text);
            self.state = State::Ready;
        } else if !self.editor.replace_all(&text) {
            self.say("The file is too large to edit here.".into(), true);
            return;
        }
        self.path = path;
        self.target = target;
        self.disk = disk;
        self.read_only = read_only;
        self.saved = self.editor.revision();
        self.stale = false;
        self.restart.clear();
        self.reloaded.clear();
        self.message = (!first).then(|| {
            (
                "Read the file again. Ctrl+Z brings your text back.".to_owned(),
                false,
            )
        });
        if read_only {
            self.message = Some((
                format!(
                    "{} is read-only (managed elsewhere?): it can be read here, not saved.",
                    self.path
                ),
                true,
            ));
        }
        self.check_due = Some(now);
        self.relayout();
    }

    fn save_done(&mut self, answer: Result<String, String>, text: String, revision: u64) {
        let answer = match answer {
            Ok(answer) => reply::write(&answer),
            // The browser's refusals say why in the answer's own form.
            Err(why)
                if ["stale\n", "invalid\n", "refused\n"]
                    .iter()
                    .any(|head| why.starts_with(head)) =>
            {
                reply::write(&why)
            }
            Err(why) => reply::Write::Refused(format!("sig refused the save: {why}")),
        };
        self.restart.clear();
        self.reloaded.clear();
        match answer {
            reply::Write::Saved { restart, notices } => {
                self.disk = Some(text);
                self.saved = revision;
                self.stale = false;
                self.restart = restart;
                self.reloaded = notices;
                let applied = if self.restart.is_empty() {
                    "Saved, and in use."
                } else {
                    "Saved. Some settings apply when sig starts again:"
                };
                self.message = Some((applied.into(), false));
                // What was saved is valid.
                if self.editor.revision() == revision {
                    self.checked = Some(revision);
                    self.valid = Some(true);
                    self.problems.clear();
                }
            }
            reply::Write::Stale(why) => {
                self.stale = true;
                self.message = Some((why, true));
            }
            reply::Write::Invalid(problems) => {
                self.message = Some(("Not saved: the configuration has problems.".into(), true));
                if self.editor.revision() == revision {
                    self.checked = Some(revision);
                }
                self.valid = Some(false);
                self.problems = problems;
                self.notices.clear();
            }
            reply::Write::Refused(why) => self.message = Some((format!("Not saved. {why}"), true)),
        }
        self.relayout();
    }

    /// The caret's line and column for the status line, 1-based; the column in characters.
    pub fn line_col(&self) -> (usize, usize) {
        let caret = self.editor.caret();
        let line = self.editor.buffer().line(caret.line);
        (caret.line + 1, line[..caret.col].chars().count() + 1)
    }

    /// A pointer press with no modifiers, as tests and the guest's input give it.
    pub fn click(&mut self, x: f32, y: f32, host: &mut dyn Host, now: u64) {
        self.input(Input::Press { x, y, shift: false }, host, now);
    }
}

/// The digits of the largest line number.
fn digits(lines: usize) -> usize {
    lines.max(1).ilog10() as usize + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Key, Mods};
    use crate::host::FakeHost;
    use crate::layout::testing::Cells;

    fn fake(file: Option<&str>) -> FakeHost {
        let mut host = FakeHost::default();
        host.file = file.map(str::to_owned);
        host
    }

    fn page(host: &mut FakeHost) -> Page {
        let mut page = Page::new(Widths::new(Box::new(Cells)));
        page.resize(800.0, 600.0);
        page.start(host);
        page.tick(host, 0);
        page
    }

    fn key(page: &mut Page, host: &mut FakeHost, key: Key, mods: Mods, now: u64) {
        page.input(Input::Key(key, mods), host, now);
    }

    fn type_text(page: &mut Page, host: &mut FakeHost, text: &str, now: u64) {
        page.input(Input::Text(text.into()), host, now);
    }

    #[test]
    fn it_reads_checks_after_a_pause_and_saves() {
        let mut host = fake(Some("version = 1\n"));
        let mut p = page(&mut host);
        assert!(p.ready());
        assert_eq!(p.editor.text(), "version = 1\n");
        assert!(!p.modified());
        // The text is checked when it arrives.
        p.tick(&mut host, 1);
        p.tick(&mut host, 2);
        assert_eq!(p.validity(), Validity::Valid);

        key(&mut p, &mut host, Key::End, Mods::CTRL, 10);
        type_text(&mut p, &mut host, "zoom = 2", 10);
        assert!(p.modified());
        assert_eq!(p.validity(), Validity::Checking);
        p.tick(&mut host, 100);
        assert!(!host
            .sent
            .iter()
            .any(|r| matches!(r, Request::ConfigCheck { text } if text.contains("zoom"))));
        p.tick(&mut host, 10 + CHECK_DELAY);
        p.tick(&mut host, 11 + CHECK_DELAY);
        assert_eq!(p.validity(), Validity::Valid);

        key(&mut p, &mut host, Key::Char('s'), Mods::CTRL, 500);
        assert!(p.saving());
        p.tick(&mut host, 501);
        assert!(!p.saving() && !p.modified());
        assert_eq!(host.file.as_deref(), Some("version = 1\nzoom = 2"));
        assert!(p.rows().iter().any(|r| r.text == "Saved, and in use."));
    }

    #[test]
    fn an_invalid_save_shows_its_problems_and_a_click_goes_to_the_line() {
        let mut host = fake(Some("version = 1\n"));
        let mut p = page(&mut host);
        key(&mut p, &mut host, Key::End, Mods::CTRL, 0);
        type_text(&mut p, &mut host, "oops", 0);
        key(&mut p, &mut host, Key::Char('s'), Mods::CTRL, 0);
        p.tick(&mut host, 1);
        assert_eq!(host.file.as_deref(), Some("version = 1\n"), "unchanged");
        assert_eq!(p.validity(), Validity::Invalid(1));
        let rows = p.rows();
        assert_eq!(rows[0].kind, RowKind::Error);
        assert_eq!(rows[1].line, Some(2));
        // Click the problem's row: the caret goes to its line.
        key(&mut p, &mut host, Key::Home, Mods::CTRL, 2);
        let panel = p.layout.panel;
        assert!(panel.h > 0.0);
        let y = panel.y + p.sizes.pad * 0.5 + p.sizes.ui_line * 1.5;
        p.click(10.0, y, &mut host, 3);
        assert_eq!(p.editor.caret().line, 1);
        assert_eq!(p.line_col(), (2, 1));
    }

    #[test]
    fn a_stale_save_offers_to_read_again_and_undo_brings_the_text_back() {
        let mut host = fake(Some("version = 1\n"));
        let mut p = page(&mut host);
        key(&mut p, &mut host, Key::End, Mods::CTRL, 0);
        type_text(&mut p, &mut host, "zoom = 2", 0);
        // Someone else writes the file.
        host.file = Some("version = 1\nzoom = 3\n".into());
        key(&mut p, &mut host, Key::Char('s'), Mods::CTRL, 0);
        p.tick(&mut host, 1);
        assert!(p.stale);
        assert_eq!(host.file.as_deref(), Some("version = 1\nzoom = 3\n"));
        let button = p.layout.reload.expect("the button shows");
        p.click(button.x + 1.0, button.y + 1.0, &mut host, 2);
        p.tick(&mut host, 3);
        assert!(!p.stale && p.layout.reload.is_none());
        assert_eq!(p.editor.text(), "version = 1\nzoom = 3\n");
        assert!(!p.modified());
        // A save now replaces what is there.
        key(&mut p, &mut host, Key::Char('z'), Mods::CTRL, 4);
        assert_eq!(p.editor.text(), "version = 1\nzoom = 2");
        key(&mut p, &mut host, Key::Char('s'), Mods::CTRL, 5);
        p.tick(&mut host, 6);
        assert_eq!(host.file.as_deref(), Some("version = 1\nzoom = 2"));
    }

    #[test]
    fn the_save_button_saves() {
        let mut host = fake(Some("version = 1\n"));
        let mut p = page(&mut host);
        key(&mut p, &mut host, Key::End, Mods::CTRL, 0);
        type_text(&mut p, &mut host, "# x", 0);
        let button = p.layout.save.expect("the button shows");
        p.click(button.x + 1.0, button.y + 1.0, &mut host, 1);
        p.tick(&mut host, 2);
        assert_eq!(host.file.as_deref(), Some("version = 1\n# x"));
        assert!(!p.modified());
    }

    #[test]
    fn a_missing_file_starts_from_the_starter_and_saves_over_nothing() {
        let mut host = fake(None);
        let mut p = page(&mut host);
        assert_eq!(p.editor.text(), "version = 1\n");
        assert!(!p.modified());
        key(&mut p, &mut host, Key::Char('s'), Mods::CTRL, 0);
        p.tick(&mut host, 1);
        assert_eq!(host.file.as_deref(), Some("version = 1\n"));
        let write = host
            .sent
            .iter()
            .find(|r| matches!(r, Request::ConfigWrite { .. }));
        assert!(
            matches!(write, Some(Request::ConfigWrite { replaces, .. }) if replaces.starts_with("missing "))
        );
    }

    #[test]
    fn a_check_about_an_older_text_is_dropped() {
        let mut host = fake(Some("version = 1\n"));
        let mut p = page(&mut host);
        host.hold = true;
        p.tick(&mut host, 0);
        assert_eq!(p.validity(), Validity::Checking);
        type_text(&mut p, &mut host, "x", 1);
        host.hold = false;
        p.tick(&mut host, 2);
        assert_eq!(
            p.validity(),
            Validity::Checking,
            "the answer was about the text before"
        );
        p.tick(&mut host, 1 + CHECK_DELAY);
        p.tick(&mut host, 2 + CHECK_DELAY);
        assert_eq!(
            p.validity(),
            Validity::Invalid(1),
            "xversion = 1 has no version"
        );
    }

    #[test]
    fn copy_goes_to_the_clipboard_and_a_refused_file_is_said() {
        let mut host = fake(Some("version = 1\n"));
        let mut p = page(&mut host);
        key(&mut p, &mut host, Key::End, Mods::SHIFT, 0);
        key(&mut p, &mut host, Key::Char('c'), Mods::CTRL, 0);
        assert_eq!(host.clipboard, "version = 1");

        let mut host = FakeHost::default();
        host.read_only = true;
        let mut p = page(&mut host);
        assert!(p.read_only);
        key(&mut p, &mut host, Key::Char('s'), Mods::CTRL, 0);
        assert!(!p.saving());
        assert_eq!(p.rows()[0].kind, RowKind::Error);
    }
}
