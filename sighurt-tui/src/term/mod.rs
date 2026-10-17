//! The terminal `sig` runs in.
//!
//! Owns: raw mode, the capability probe, the input parser, bounded nonblocking
//! output, and restoration, including [`restore_before_forced_exit`]. Must not
//! own: the cell grid, the chrome, page pixels, browser policy, or what a key
//! does.
//!
//! Register [`Terminal::fd`] with the calloop loop for readability, and
//! [`Terminal::winch_fd`] as well once [`Terminal::install_handlers`] has run.
//! When either is readable, or when [`Terminal::poll_timeout`] elapses, call
//! [`Terminal::read_events`] and [`Terminal::expire_if_due`]. Keys are
//! [`sighurt_api::input::KeyEvent`]s. Legacy terminals report no evdev code
//! (`KeyCode(0)`), synthesize a release after each press, and cannot tell Tab
//! from Ctrl-I or Enter from Ctrl-M: those bytes are Tab and Enter. The enter
//! sequence asks for the kitty keyboard protocol, which keeps them apart.
//! Mouse coordinates are 0-based cells.

mod keys;
mod modes;
mod output;
mod parser;
mod probe;
mod session;

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use rustix::termios::Winsize;
use sighurt_api::input::{ButtonState, KeyEvent, Modifiers, PointerButton};

pub use modes::{ENTER, KITTY_KEYBOARD_FLAGS, LEAVE, QUERIES};
pub(crate) use modes::{SYNC_BEGIN, SYNC_END};
pub use parser::{Wheel, ESC_TIMEOUT};
pub use probe::{Caps as Capabilities, XtGetTcap, PROBE_DEADLINE};
pub use session::{open_pty, restore_before_forced_exit, termios_matches};

use parser::{Input, MouseButton};
use probe::Caps;
use session::Session;

/// Cells of the terminal, and the pixel size of one cell.
///
/// `cell_width` and `cell_height` are 0 when neither `CSI 16 t` nor
/// `TIOCGWINSZ` reported a pixel size. A probe answer wins over the ioctl.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermSize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// Pixels across one cell.
    pub cell_width: u16,
    /// Pixels down one cell.
    pub cell_height: u16,
}

/// One SGR mouse report, in 0-based cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    /// Column of the cell, 0-based.
    pub column: u16,
    /// Row of the cell, 0-based.
    pub row: u16,
    /// The button, or `None` for a wheel report.
    pub button: Option<PointerButton>,
    /// Press or release. A wheel report uses the terminal's `M`/`m` byte.
    pub state: ButtonState,
    /// Shift, Alt and Control from the button code.
    pub modifiers: Modifiers,
    /// The motion bit (32) was set.
    pub motion: bool,
    /// Set when the wheel bit (64) was set.
    pub wheel: Option<Wheel>,
}

/// What [`Terminal::read_events`] hands the owner loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    /// A key press, repeat or release.
    Key(KeyEvent),
    /// An SGR mouse report.
    Mouse(MouseEvent),
    /// Bracketed paste, lossy-decoded as UTF-8.
    Paste(String),
    /// The terminal's cell size changed.
    Resize(TermSize),
    /// `true` when the terminal gained focus (`CSI I`), `false` when it lost it.
    Focus(bool),
}

/// Raw mode on one terminal, the probe's answers, and the bytes waiting to go out.
pub struct Terminal {
    session: Session,
    caps: Caps,
    size: TermSize,
    ioctl_cell: Option<(u16, u16)>,
}

impl Terminal {
    /// Put `fd` into raw mode, non-blocking, and send [`ENTER`].
    pub fn enter(fd: OwnedFd) -> io::Result<Self> {
        Self::wrap(Session::enter(fd)?)
    }

    /// [`Self::enter`] with a pending-byte cap of `cap`.
    pub fn enter_with(fd: OwnedFd, cap: usize) -> io::Result<Self> {
        Self::wrap(Session::enter_with(fd, cap)?)
    }

    fn wrap(session: Session) -> io::Result<Self> {
        let mut terminal = Self {
            session,
            caps: Caps::default(),
            size: TermSize {
                cols: 1,
                rows: 1,
                cell_width: 0,
                cell_height: 0,
            },
            ioctl_cell: None,
        };
        terminal.note_winsize();
        Ok(terminal)
    }

    /// The terminal file descriptor. Register it for readability.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.session.fd()
    }

    /// Read end of the pipe [`Self::install_handlers`] writes on `SIGWINCH`.
    pub fn winch_fd(&self) -> BorrowedFd<'_> {
        self.session.winch_fd()
    }

    /// Duplicate of [`Self::fd`]. The duplicate shares non-blocking mode.
    pub fn try_clone_fd(&self) -> io::Result<OwnedFd> {
        self.session.try_clone_fd()
    }

    /// Answers collected so far. Missing fields were not answered.
    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// Cells, and pixels per cell. See [`TermSize`].
    pub fn size(&self) -> TermSize {
        self.size
    }

    /// Send the startup queries and collect replies until DA1 or `deadline`.
    ///
    /// A reply that arrives later stays a reply: the next [`Self::read_events`]
    /// folds it into [`Self::capabilities`] and does not report it as a key.
    pub fn probe(&mut self, deadline: Duration) -> io::Result<&Capabilities> {
        self.caps = self.session.probe(deadline)?;
        self.note_winsize();
        Ok(&self.caps)
    }

    /// Read whatever the terminal has already buffered.
    pub fn read_events(&mut self) -> io::Result<Vec<TermEvent>> {
        self.session.drain_winch();
        let inputs = self.session.read_input()?;
        let mut events = Vec::with_capacity(inputs.len());
        for input in inputs {
            if let Some(event) = self.take_input(input) {
                events.push(event);
            }
        }
        self.push_resize(&mut events);
        Ok(events)
    }

    /// Time until a partial escape sequence should be expired.
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.session.poll_timeout()
    }

    /// Expire a partial sequence whose timer has elapsed, and notice a resize.
    pub fn expire_if_due(&mut self) -> Vec<TermEvent> {
        let inputs = self.session.expire_if_due();
        let mut events = Vec::with_capacity(inputs.len());
        for input in inputs {
            if let Some(event) = self.take_input(input) {
                events.push(event);
            }
        }
        self.push_resize(&mut events);
        events
    }

    /// Queue `frame`. Does not block when the terminal stops reading. A frame
    /// that does not fit in the cap is dropped and counted by [`Self::dropped_frames`].
    pub fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.session.write_frame(frame)
    }

    /// Drop bytes not yet written. Only after a resume: `SIGCONT` already sent [`ENTER`], so
    /// the tail of a sequence cut off by the suspension would show as text.
    pub fn discard_pending(&mut self) {
        self.session.discard_pending();
    }

    /// Bytes still waiting in the output cap.
    pub fn pending_len(&self) -> usize {
        self.session.pending_len()
    }

    /// Frames discarded because the reader was not taking them.
    pub fn dropped_frames(&self) -> u64 {
        self.session.dropped_frames()
    }

    /// `SIGTSTP` restores and stops; `SIGCONT` enters again; `SIGHUP` restores
    /// and re-raises; `SIGWINCH` wakes [`Self::winch_fd`].
    pub fn install_handlers(&mut self) -> io::Result<()> {
        self.session.install_handlers()
    }

    /// Whether `SIGCONT` has re-entered since the last call.
    pub fn take_resumed(&self) -> bool {
        self.session.take_resumed()
    }

    /// Restore the termios and file flags captured at [`Self::enter`], and send
    /// [`LEAVE`]. A second call is a success and writes nothing.
    pub fn restore(&mut self) -> io::Result<()> {
        self.session.restore()
    }

    fn take_input(&mut self, input: Input) -> Option<TermEvent> {
        match input {
            Input::Reply(reply) => {
                let reply = Input::Reply(reply);
                probe::absorb(&mut self.caps, &reply);
                self.apply_cells();
                None
            }
            Input::Key(key) => Some(TermEvent::Key(keys::to_api(key))),
            Input::Mouse(mouse) => Some(TermEvent::Mouse(to_mouse(mouse))),
            Input::Paste(text) => Some(TermEvent::Paste(text)),
            Input::Focus(inside) => Some(TermEvent::Focus(inside)),
        }
    }

    fn note_winsize(&mut self) {
        let Ok(ws) = self.session.winsize() else {
            return;
        };
        self.size.cols = ws.ws_col.max(1);
        self.size.rows = ws.ws_row.max(1);
        self.ioctl_cell = cell_from_winsize(&ws);
        self.caps.winsize_cell = self.ioctl_cell;
        self.apply_cells();
    }

    fn push_resize(&mut self, out: &mut Vec<TermEvent>) {
        let Ok(ws) = self.session.winsize() else {
            return;
        };
        let cols = ws.ws_col.max(1);
        let rows = ws.ws_row.max(1);
        let changed = cols != self.size.cols || rows != self.size.rows;
        self.ioctl_cell = cell_from_winsize(&ws);
        self.caps.winsize_cell = self.ioctl_cell;
        self.size.cols = cols;
        self.size.rows = rows;
        self.apply_cells();
        if changed {
            out.push(TermEvent::Resize(self.size));
        }
    }

    fn apply_cells(&mut self) {
        if let Some((height, width)) = self.caps.cell_pixels {
            self.size.cell_width = u16::try_from(width).unwrap_or(u16::MAX);
            self.size.cell_height = u16::try_from(height).unwrap_or(u16::MAX);
        } else if let Some((width, height)) = self.ioctl_cell {
            self.size.cell_width = width;
            self.size.cell_height = height;
        }
    }
}

impl AsFd for Terminal {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd()
    }
}

fn cell_from_winsize(ws: &Winsize) -> Option<(u16, u16)> {
    if ws.ws_col == 0 || ws.ws_row == 0 || ws.ws_xpixel == 0 || ws.ws_ypixel == 0 {
        return None;
    }
    Some((ws.ws_xpixel / ws.ws_col, ws.ws_ypixel / ws.ws_row))
}

fn to_mouse(event: parser::MouseEvent) -> MouseEvent {
    let button = match event.button {
        MouseButton::Left => Some(PointerButton::Left),
        MouseButton::Middle => Some(PointerButton::Middle),
        MouseButton::Right => Some(PointerButton::Right),
        MouseButton::Back => Some(PointerButton::Back),
        MouseButton::Forward => Some(PointerButton::Forward),
        MouseButton::None => None,
    };
    let state = match event.state {
        keys::ButtonState::Pressed => ButtonState::Pressed,
        keys::ButtonState::Released => ButtonState::Released,
    };
    MouseEvent {
        column: event.column.saturating_sub(1),
        row: event.row.saturating_sub(1),
        button,
        state,
        modifiers: keys::to_api_modifiers(event.modifiers),
        motion: event.motion,
        wheel: event.wheel,
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;
    use std::thread;
    use std::time::{Duration, Instant};

    use rustix::io::Errno;
    use rustix::termios::{tcsetwinsize, Winsize};

    use super::*;

    fn write_all(fd: &impl AsFd, mut bytes: &[u8]) {
        let start = Instant::now();
        while !bytes.is_empty() {
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "pty write stalled"
            );
            match rustix::io::write(fd, bytes) {
                Ok(0) => panic!("pty write returned 0"),
                Ok(n) => bytes = &bytes[n..],
                Err(Errno::INTR) | Err(Errno::AGAIN) => thread::sleep(Duration::from_millis(1)),
                Err(err) => panic!("pty write: {err}"),
            }
        }
    }

    fn read_soon(terminal: &mut Terminal, master: &impl AsFd, bytes: &[u8]) -> Vec<TermEvent> {
        write_all(master, bytes);
        let start = Instant::now();
        let mut got = Vec::new();
        while start.elapsed() < Duration::from_secs(2) {
            got.extend(terminal.read_events().expect("read"));
            if !got.is_empty() {
                return got;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("no events for {bytes:?}");
    }

    #[test]
    fn focus_and_resize_are_events() {
        let (master, slave) = open_pty().expect("openpty");
        let initial = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        tcsetwinsize(&slave, initial).expect("initial size");
        let mut terminal = Terminal::enter(slave).expect("enter");
        assert_eq!(terminal.size().cols, 80);
        assert_eq!(terminal.size().rows, 24);

        let focus = read_soon(&mut terminal, &master, b"\x1b[I\x1b[O");
        assert_eq!(focus, vec![TermEvent::Focus(true), TermEvent::Focus(false)]);

        let fd = terminal.try_clone_fd().expect("dup");
        tcsetwinsize(
            &fd,
            Winsize {
                ws_row: 12,
                ws_col: 40,
                ws_xpixel: 400,
                ws_ypixel: 240,
            },
        )
        .expect("resize");
        let events = terminal.read_events().expect("read after resize");
        assert!(
            events.iter().any(|event| matches!(
                event,
                TermEvent::Resize(size)
                    if size.cols == 40
                        && size.rows == 12
                        && size.cell_width == 10
                        && size.cell_height == 20
            )),
            "{events:?} size {:?}",
            terminal.size()
        );
        terminal.restore().expect("restore");
    }

    #[test]
    fn forced_exit_restore_is_idempotent() {
        let (_master, slave) = open_pty().expect("openpty");
        let original = rustix::termios::tcgetattr(&slave).expect("original");
        let mut terminal = Terminal::enter(slave).expect("enter");
        let fd = terminal.try_clone_fd().expect("dup");
        restore_before_forced_exit();
        restore_before_forced_exit();
        assert!(termios_matches(
            &rustix::termios::tcgetattr(&fd).expect("restored"),
            &original
        ));
        terminal.restore().expect("second restore");
        assert!(termios_matches(
            &rustix::termios::tcgetattr(&fd).expect("still restored"),
            &original
        ));
    }
}
