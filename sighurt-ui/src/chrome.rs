//! The chrome's behavior: input, snapshots and commands in; `ChromeState` changes, UI actions
//! and damage out.
//!
//! [`Chrome`] ties the pieces together: the browser [`Snapshot`], the [`ChromeState`], the
//! [`Layout`], the configured [`Colors`] and the fonts. Input addressed to the chrome becomes
//! [`UiAction`]s: browser commands for the core, focus moves, clipboard requests and input-method
//! state for the host. Everything the chrome does is a command, as in the old UI: buttons and
//! the address line issue [`Command`]s for the core to run.
//!
//! The core owns where the keyboard focus is and which commands move it: the executable hands
//! each of its focus moves (`Effect::Focus`) to [`Chrome::set_focus`]. The moves the chrome makes
//! itself (a click in the address line or on a page it draws, Escape, a `:command`) come out as
//! [`UiAction::Focus`], for the executable to tell the core.
//!
//! The address line doubles as the command line: a line starting with `:` runs the command
//! after it, such as `:tab.close 2`, so every command can be reached without a key binding.
//!
//! The chrome marks what needs repainting ([`Damage`]) only after an edit, a hover change, a
//! snapshot change it shows, a configuration change or a layout change: a static chrome needs
//! no tick, and a page animating never repaints it. The caret is steady, so no timer runs.
//!
//! Must not own: key binding policy (the core resolves bindings before the chrome sees a key).

use std::fmt;
use std::rc::Rc;

use sighurt_api::commands::Command;
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{LogicalSize, SurfaceRect, WindowPoint, WindowRect};
use sighurt_api::ids::RequestId;
use sighurt_api::input::{
    ButtonState, FocusEvent, Key, KeyEvent, PointerButton, PointerEvent, TextInputEvent,
    TextInputState, TextPurpose,
};

use crate::hit_test::{hit_test, Target};
use crate::layout::{Layout, LayoutOptions};
use crate::state::{
    is_home, untrusted_line, ChromeState, HomeInfo, Snapshot, TabContent, NO_SUCH_PAGE,
};
use crate::text::{Fonts, ShapedText, TextStyle};

/// Space between the address field's border and its text.
const ADDRESS_PADDING: f32 = 12.0;
/// The caret's width in logical pixels.
pub(crate) const CARET_WIDTH: f32 = 2.0;
/// Characters of status text kept for drawing: more than the strip can show.
const MAX_STATUS: usize = 2000;

/// A color, with straight (not premultiplied) alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Opacity.
    pub a: u8,
}

impl Rgba {
    /// An opaque color from `0xrrggbb`.
    pub const fn rgb(hex: u32) -> Self {
        Self::rgb_alpha(hex << 8 | 0xff)
    }

    /// A color from `0xrrggbbaa`.
    pub const fn rgb_alpha(hex: u32) -> Self {
        let [r, g, b, a] = hex.to_be_bytes();
        Self { r, g, b, a }
    }

    /// Parses `"#rrggbb"` or `"#rrggbbaa"`.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        match digits.len() {
            6 => Some(Self::rgb(value)),
            8 => Some(Self::rgb_alpha(value)),
            _ => None,
        }
    }
}

/// The chrome's colors: Sighurt's dark look by default, each overridable from the
/// configuration's `[ui.colors]` by the names in [`Colors::NAMES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    /// Window and page background.
    pub bg: Rgba,
    /// The address field and the home page's card.
    pub surface: Rgba,
    /// Buttons under the pointer.
    pub hover: Rgba,
    /// The selected tab.
    pub tab: Rgba,
    /// Lines between areas.
    pub border: Rgba,
    /// Border of the focused address field.
    pub focus: Rgba,
    /// Text.
    pub text: Rgba,
    /// Secondary text and icons.
    pub text_muted: Rgba,
    /// Hints and disabled buttons.
    pub text_dim: Rgba,
    /// Selected text in the address field.
    pub selection: Rgba,
    /// Error messages.
    pub error: Rgba,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            bg: Rgba::rgb(0x0a0a0b),
            surface: Rgba::rgb(0x18181b),
            hover: Rgba::rgb(0x27272a),
            tab: Rgba::rgb(0x27272a),
            border: Rgba::rgb(0x27272a),
            focus: Rgba::rgb(0xd4d4d8),
            text: Rgba::rgb(0xfafafa),
            text_muted: Rgba::rgb(0xa1a1aa),
            text_dim: Rgba::rgb(0x71717a),
            selection: Rgba::rgb_alpha(0x60a5fa55),
            error: Rgba::rgb(0xdc2626),
        }
    }
}

impl Colors {
    /// The color names `[ui.colors]` accepts.
    pub const NAMES: [&'static str; 11] = [
        "bg",
        "surface",
        "hover",
        "tab",
        "border",
        "focus",
        "text",
        "text_muted",
        "text_dim",
        "selection",
        "error",
    ];

    /// Sets the color called `name` to `value` (`"#rrggbb"` or `"#rrggbbaa"`), as a
    /// `[ui.colors]` entry does. The configuration loader validates every entry this way before
    /// it replaces the active colors.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), ColorError> {
        let field = match name {
            "bg" => &mut self.bg,
            "surface" => &mut self.surface,
            "hover" => &mut self.hover,
            "tab" => &mut self.tab,
            "border" => &mut self.border,
            "focus" => &mut self.focus,
            "text" => &mut self.text,
            "text_muted" => &mut self.text_muted,
            "text_dim" => &mut self.text_dim,
            "selection" => &mut self.selection,
            "error" => &mut self.error,
            _ => return Err(ColorError::Unknown(name.to_owned())),
        };
        *field = Rgba::parse(value).ok_or_else(|| ColorError::Malformed {
            name: name.to_owned(),
            value: value.to_owned(),
        })?;
        Ok(())
    }
}

/// Why a `[ui.colors]` entry was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorError {
    /// No color has this name.
    Unknown(String),
    /// The value isn't `"#rrggbb"` or `"#rrggbbaa"`.
    Malformed {
        /// The color.
        name: String,
        /// What was given.
        value: String,
    },
}

impl fmt::Display for ColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "unknown color {name:?}"),
            Self::Malformed { name, value } => {
                write!(
                    f,
                    "{name} should be \"#rrggbb\" or \"#rrggbbaa\", not {value:?}"
                )
            }
        }
    }
}

impl std::error::Error for ColorError {}

/// Something the executable should do for the chrome.
#[derive(Clone, Debug, PartialEq)]
pub enum UiAction {
    /// Run this browser command.
    Command(Command),
    /// The chrome moved the window's logical keyboard focus: to the page, which then gets
    /// `SetFocus(true)`, or to the chrome, whose address line then gets the keys. Tell the core;
    /// the move is done, so it doesn't come back through [`Chrome::set_focus`].
    Focus(Focus),
    /// Put this text on the clipboard.
    Copy(String),
    /// Read the clipboard for this request and hand its text to [`Chrome::paste`] with it. The
    /// chrome made the id with `RequestId::next`, so it is no other request's.
    Paste(RequestId),
    /// The chrome's editor state for the input method changed; `None` turns it off. The caret
    /// is in the chrome surface's coordinates.
    TextInput(Option<TextInputState>),
    /// Show this pointer over the chrome.
    Cursor(Cursor),
}

/// Where logical keyboard focus goes: the core's `Focus::Page` and `Focus::Location`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Focus {
    /// The active page.
    Page,
    /// The chrome's address line.
    Chrome,
}

/// Which parts the chrome draws need repainting since [`Chrome::take_damage`] last ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Damage {
    /// The chrome surface: [`Layout::chrome`].
    pub chrome: bool,
    /// The status strip: [`Layout::status`].
    pub status: bool,
    /// The page area, while the chrome draws a page there ([`Chrome::draws_page`]).
    pub page: bool,
}

impl Damage {
    const ALL: Damage = Damage {
        chrome: true,
        status: true,
        page: true,
    };

    /// Whether anything needs repainting.
    pub fn any(self) -> bool {
        self.chrome || self.status || self.page
    }
}

/// A page the chrome draws in the page area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InternalPage<'a> {
    Home(&'a HomeInfo),
    Error { url: &'a str, message: &'a str },
}

/// The shade of a piece of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Text,
    Muted,
    Dim,
    Error,
}

/// One block of text on a page the chrome draws, placed in window-logical coordinates.
pub(crate) struct PageText {
    pub shaped: Rc<ShapedText>,
    pub x: f32,
    pub y: f32,
    pub tone: Tone,
    pub heading: bool,
}

/// A page the chrome draws: a card with text, centred in the page area.
pub(crate) struct PageLayout {
    /// The card: x, y, width, height.
    pub card: [f32; 4],
    pub texts: Vec<PageText>,
}

/// The address line as drawn: its shaped text and where things are, in window coordinates.
pub(crate) struct AddressView {
    pub shaped: Rc<ShapedText>,
    /// Where the text's origin (its left edge, the top of its line) is.
    pub x: f32,
    pub y: f32,
    /// The visible text area: left, right.
    pub clip: (f32, f32),
    pub caret: Option<f32>,
    pub selection: Option<(f32, f32)>,
    pub preedit: Option<(f32, f32)>,
}

/// The browser chrome: its state, layout and fonts, and what it does with input.
pub struct Chrome {
    fonts: Fonts,
    colors: Colors,
    options: LayoutOptions,
    snapshot: Snapshot,
    state: ChromeState,
    layout: Layout,
    damage: Damage,
    /// What the executable was last told, so only changes are reported.
    text_input: Option<TextInputState>,
    cursor: Cursor,
}

impl Chrome {
    /// A chrome with `fonts` and `colors`, for a window of no size yet: call
    /// [`Chrome::resize`] and [`Chrome::set_snapshot`] before painting.
    pub fn new(fonts: Fonts, colors: Colors, options: LayoutOptions) -> Self {
        Self {
            fonts,
            colors,
            options,
            snapshot: Snapshot::default(),
            state: ChromeState::default(),
            layout: Layout::new(LogicalSize::default(), &[], options),
            damage: Damage::ALL,
            text_input: None,
            cursor: Cursor::Default,
        }
    }

    /// Where everything goes: the placement snapshot for the host and the widget rectangles.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The browser state the chrome shows.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The chrome's interaction state.
    pub fn state(&self) -> &ChromeState {
        &self.state
    }

    /// The colors in use.
    pub fn colors(&self) -> &Colors {
        &self.colors
    }

    /// Whether the chrome draws the page area itself (`sig://home`, error pages), rather than
    /// an engine view showing there.
    pub fn draws_page(&self) -> bool {
        internal_page(&self.snapshot).is_some()
    }

    /// What needs repainting since the last call, which clears it.
    pub fn take_damage(&mut self) -> Damage {
        std::mem::take(&mut self.damage)
    }

    /// Lays the chrome out for a window of `window` size.
    pub fn resize(&mut self, window: LogicalSize) -> Vec<UiAction> {
        self.relayout(window);
        self.finish(Vec::new())
    }

    /// Changes what the layout includes.
    pub fn set_options(&mut self, options: LayoutOptions) -> Vec<UiAction> {
        self.options = options;
        self.relayout(self.layout.window);
        self.finish(Vec::new())
    }

    /// Changes the colors, as when the configuration is reloaded.
    pub fn set_colors(&mut self, colors: Colors) {
        if colors != self.colors {
            self.colors = colors;
            self.damage = Damage::ALL;
        }
    }

    /// Shows a new browser snapshot, as after every update of the core. Parts that show
    /// nothing different aren't repainted.
    pub fn set_snapshot(&mut self, snapshot: Snapshot) -> Vec<UiAction> {
        if snapshot != self.snapshot {
            let old = std::mem::replace(&mut self.snapshot, snapshot);
            let new = &self.snapshot;
            self.damage.chrome |= old.tabs != new.tabs
                || old.can_go_back != new.can_go_back
                || old.can_go_forward != new.can_go_forward;
            self.damage.status |= old.status != new.status;
            self.damage.page |= internal_page(&old) != internal_page(new);
            self.relayout(self.layout.window);
            // A button that just became disabled, or a closed tab, is no longer hovered.
            self.state.hover = self.state.hover.filter(|&target| self.enabled(target));
        }
        // Even an unchanged snapshot settles a line opened with Enter whose load never started.
        self.sync_address();
        self.finish(Vec::new())
    }

    /// Moves the keyboard focus where the core put it (its `Effect::Focus`), after a command or
    /// a click in the page. [`Focus::Chrome`] focuses the address line with its text selected,
    /// ready to type over. [`Focus::Page`] leaves it and drops what was typed, except a line
    /// just opened with Enter: that stays until its tab commits another address, stops loading
    /// or is switched away from. Reports no focus action, since the move is done.
    pub fn set_focus(&mut self, focus: Focus) -> Vec<UiAction> {
        match focus {
            Focus::Chrome => self.focus_address(),
            Focus::Page => self.page_takes_focus(),
        }
        self.finish(Vec::new())
    }

    /// The window gained or lost keyboard focus.
    pub fn window_focus(&mut self, event: FocusEvent) -> Vec<UiAction> {
        self.state.window_active = event == FocusEvent::Activated;
        if !self.state.window_active {
            self.state.pressed = None;
            self.state.selecting = false;
        }
        // Only the focused address line looks different: its caret and selection show while
        // the window is active.
        self.damage.chrome |= self.state.focused;
        self.finish(Vec::new())
    }

    /// Pointer input on a chrome surface placed at `surface` (the chrome, the status strip or
    /// the page area while the chrome draws it), with the event's positions local to it.
    pub fn pointer(&mut self, surface: WindowRect, event: &PointerEvent) -> Vec<UiAction> {
        let mut actions = Vec::new();
        match *event {
            PointerEvent::Enter { position } | PointerEvent::Motion { position, .. } => {
                self.motion(surface.to_window(position));
            }
            PointerEvent::Leave => self.set_hover(None),
            PointerEvent::Button {
                button,
                state,
                position,
                modifiers,
            } => {
                let point = surface.to_window(position);
                match (button, state) {
                    (PointerButton::Left, ButtonState::Pressed) => {
                        self.press(point, modifiers.shift, &mut actions)
                    }
                    (PointerButton::Left, ButtonState::Released) => {
                        self.release(point, &mut actions)
                    }
                    (PointerButton::Back, ButtonState::Pressed) if self.snapshot.can_go_back => {
                        actions.push(UiAction::Command(Command::PageBack))
                    }
                    (PointerButton::Forward, ButtonState::Pressed)
                        if self.snapshot.can_go_forward =>
                    {
                        actions.push(UiAction::Command(Command::PageForward))
                    }
                    _ => {}
                }
            }
            PointerEvent::Scroll { .. } => {}
        }
        self.finish(actions)
    }

    /// A key the core didn't bind, while the chrome has logical focus.
    pub fn key(&mut self, event: &KeyEvent) -> Vec<UiAction> {
        let mut actions = Vec::new();
        if self.state.focused && event.state == ButtonState::Pressed {
            let before = self.state.address.clone();
            self.edit_key(event, &mut actions);
            self.edited(&before);
        }
        self.finish(actions)
    }

    /// An update from the input method, while the chrome has logical focus.
    pub fn text_input(&mut self, event: &TextInputEvent) -> Vec<UiAction> {
        if self.state.focused {
            let before = self.state.address.clone();
            self.state.address.input_method(event);
            self.edited(&before);
        }
        self.finish(Vec::new())
    }

    /// The clipboard's text for the [`UiAction::Paste`] with `request`. Only the latest request
    /// is answered, once, and only while the line has kept focus and its tab since it asked: a
    /// late answer for a request the user left behind is ignored.
    pub fn paste(&mut self, request: RequestId, text: &str) -> Vec<UiAction> {
        if self.state.paste == Some(request) {
            self.state.paste = None;
            let before = self.state.address.clone();
            self.state.address.paste(text);
            self.edited(&before);
        }
        self.finish(Vec::new())
    }

    fn relayout(&mut self, window: LogicalSize) {
        let tabs: Vec<_> = self.snapshot.tabs.iter().map(|tab| tab.id).collect();
        let layout = Layout::new(window, &tabs, self.options);
        if layout != self.layout {
            let resized = layout.window != self.layout.window
                || layout.status != self.layout.status
                || layout.page != self.layout.page;
            self.layout = layout;
            self.damage.chrome = true;
            if resized {
                self.damage = Damage::ALL;
            }
        }
    }

    /// Shows the active tab's committed address, unless the user is editing it or it shows a
    /// line opened with Enter whose tab is still loading from the same committed address.
    /// Switching tabs drops both, and a pending paste.
    fn sync_address(&mut self) {
        let active = self.snapshot.active();
        let tab = active.map(|tab| tab.id);
        let url = active.map_or("", |tab| tab.url.as_str());
        let loading = active.is_some_and(|tab| tab.loading);
        let switched = tab != self.state.address_tab;
        self.state.address_tab = tab;
        if switched {
            self.state.paste = None;
        }
        let opened = self.state.opened.as_deref();
        let settled = opened.is_some_and(|from| switched || !loading || from != url);
        if settled {
            self.state.opened = None;
        }
        let editor = &self.state.address;
        let editing = editor.is_modified() || editor.preedit().is_some();
        if (switched || settled || !editing) && (editor.text() != url || editing) {
            self.state.address.set_text(url);
            self.clear_error();
            self.damage.chrome = true;
        }
    }

    /// Drops what was typed: the line shows the committed address again.
    fn discard_edit(&mut self) {
        let url = self.snapshot.active().map_or("", |tab| tab.url.as_str());
        if self.state.address.is_modified() || self.state.address.text() != url {
            self.state.address.set_text(url);
            self.damage.chrome = true;
        }
        self.state.opened = None;
        self.clear_error();
    }

    fn clear_error(&mut self) {
        if self.state.command_error.take().is_some() {
            self.damage.chrome = true;
            self.damage.status = true;
        }
    }

    /// Focuses the address line with its text selected.
    fn focus_address(&mut self) {
        self.sync_address();
        let before = self.state.address.clone();
        self.state.address.select_all();
        self.edited(&before);
        self.damage.chrome |= !self.state.focused;
        self.state.focused = true;
    }

    /// Gives the page focus, if the chrome has it, keeping the line as it is.
    fn leave(&mut self, actions: &mut Vec<UiAction>) {
        if self.state.focused {
            self.unfocus();
            actions.push(UiAction::Focus(Focus::Page));
        }
    }

    /// The page takes focus: what was typed is dropped, unless it was just opened.
    fn page_takes_focus(&mut self) {
        if self.state.opened.is_none() {
            self.discard_edit();
        }
        self.unfocus();
    }

    fn unfocus(&mut self) {
        self.state.paste = None;
        if self.state.focused {
            self.state.focused = false;
            self.state.selecting = false;
            self.state.address.clear_preedit();
            self.damage.chrome = true;
        }
    }

    /// Notes an editor change since `before`: a repaint, and a changed line is an edit again
    /// (not the line opened last) and drops a command error.
    fn edited(&mut self, before: &crate::text_edit::TextEditor) {
        if self.state.address != *before {
            self.damage.chrome = true;
            if self.state.address.text() != before.text() {
                self.state.opened = None;
                self.clear_error();
            }
        }
    }

    fn edit_key(&mut self, event: &KeyEvent, actions: &mut Vec<UiAction>) {
        let select = event.modifiers.shift;
        // Control as a modifier, not as a key the layout used to make a character.
        let word = event.modifiers.control && !event.consumed.control;
        // Alt the same way: a shortcut the core didn't bind types nothing, as in the old URL bar.
        let alt = event.modifiers.alt && !event.consumed.alt;
        let editor = &mut self.state.address;
        match &event.key {
            Key::Named(name) => match name.as_str() {
                "Escape" => {
                    self.discard_edit();
                    self.leave(actions);
                }
                "Enter" => self.submit(actions),
                "ArrowLeft" | "ArrowRight" if word => {
                    editor.move_word(name == "ArrowRight", select)
                }
                "ArrowLeft" | "ArrowRight" => editor.move_grapheme(name == "ArrowRight", select),
                "Home" => editor.move_to(0, select),
                "End" => editor.move_to(editor.text().len(), select),
                "Backspace" | "Delete" if word => editor.delete_word(name == "Delete"),
                "Backspace" | "Delete" => editor.delete_grapheme(name == "Delete"),
                _ => {}
            },
            _ if word => match shortcut_letter(event) {
                Some('a') => editor.select_all(),
                Some(letter @ ('c' | 'x')) => {
                    if let Some(text) = editor.selected_text() {
                        actions.push(UiAction::Copy(text.to_owned()));
                        if letter == 'x' {
                            editor.insert("");
                        }
                    }
                }
                Some('v') => {
                    let request = RequestId::next();
                    self.state.paste = Some(request);
                    actions.push(UiAction::Paste(request));
                }
                _ => {}
            },
            _ => {
                if let Some(text) = event.text.as_deref().filter(|_| !alt) {
                    editor.insert(text);
                }
            }
        }
    }

    /// Enter: open what was typed, or run it as a command after a `:`.
    fn submit(&mut self, actions: &mut Vec<UiAction>) {
        let line = self.state.address.text().trim();
        if let Some(command) = line.strip_prefix(':') {
            match command.parse::<Command>() {
                Ok(command) => {
                    self.discard_edit();
                    self.leave(actions);
                    actions.push(UiAction::Command(command));
                }
                Err(error) => {
                    self.state.command_error = Some(format!(":{}: {error}", command.trim()));
                    self.damage.chrome = true;
                    self.damage.status = true;
                }
            }
        } else if line.is_empty() {
            self.discard_edit();
            self.leave(actions);
        } else {
            // The core gives the page focus; the line keeps showing what is being opened.
            let url = self.snapshot.active().map_or("", |tab| tab.url.as_str());
            self.state.opened = Some(url.to_owned());
            actions.push(UiAction::Command(Command::PageOpen(line.to_owned())));
        }
    }

    fn motion(&mut self, point: WindowPoint) {
        let target = hit_test(&self.layout, point).filter(|&target| self.enabled(target));
        self.set_hover(target);
        if self.state.selecting {
            let offset = self.address_offset(point.x as f32);
            let before = self.state.address.clone();
            self.state.address.move_to(offset, true);
            self.edited(&before);
        }
    }

    fn set_hover(&mut self, target: Option<Target>) {
        if target != self.state.hover {
            self.state.hover = target;
            self.damage.chrome = true;
        }
    }

    fn press(&mut self, point: WindowPoint, extend: bool, actions: &mut Vec<UiAction>) {
        let target = hit_test(&self.layout, point).filter(|&target| self.enabled(target));
        self.state.pressed = target;
        match target {
            // The first click selects everything, ready to type over; later ones place the
            // cursor, and dragging selects.
            Some(Target::Address) if !self.state.focused => {
                self.focus_address();
                actions.push(UiAction::Focus(Focus::Chrome));
            }
            Some(Target::Address) => {
                let offset = self.address_offset(point.x as f32);
                let before = self.state.address.clone();
                self.state.address.move_to(offset, extend);
                self.state.selecting = !extend;
                self.edited(&before);
            }
            // A click on a page the chrome draws gives the page focus, as the core does for a
            // click on an engine's page.
            None if self.layout.page.contains(point) && self.state.focused => {
                self.page_takes_focus();
                actions.push(UiAction::Focus(Focus::Page));
            }
            _ => {}
        }
    }

    fn release(&mut self, point: WindowPoint, actions: &mut Vec<UiAction>) {
        self.state.selecting = false;
        let pressed = self.state.pressed.take();
        let target = hit_test(&self.layout, point).filter(|&target| self.enabled(target));
        if pressed.is_none() || pressed != target {
            return;
        }
        let number = |id| {
            let index = self.snapshot.tabs.iter().position(|tab| tab.id == id)?;
            std::num::NonZeroUsize::new(index + 1)
        };
        let command = match target {
            Some(Target::Tab(id)) => number(id).map(Command::TabSelect),
            Some(Target::CloseTab(id)) => number(id).map(|n| Command::TabClose(Some(n))),
            Some(Target::NewTab) => Some(Command::TabNew),
            Some(Target::Back) => Some(Command::PageBack),
            Some(Target::Forward) => Some(Command::PageForward),
            Some(Target::ReloadOrStop) => match self.snapshot.active().is_some_and(|t| t.loading) {
                true => Some(Command::PageStop),
                false => Some(Command::PageReload),
            },
            Some(Target::Address) | None => None,
        };
        actions.extend(command.map(UiAction::Command));
    }

    /// Whether `target` reacts now: back and forward only when there is somewhere to go, and
    /// tabs while they exist.
    fn enabled(&self, target: Target) -> bool {
        match target {
            Target::Back => self.snapshot.can_go_back,
            Target::Forward => self.snapshot.can_go_forward,
            Target::Tab(id) | Target::CloseTab(id) => self.layout.tab(id).is_some(),
            _ => true,
        }
    }

    /// The editor offset nearest to window position `x`.
    fn address_offset(&mut self, x: f32) -> usize {
        if self.state.address.preedit().is_some() {
            return self.state.address.cursor();
        }
        let shaped = self.fonts.line(self.state.address.text(), TextStyle::SM);
        let left = self.layout.address.x as f32 + ADDRESS_PADDING;
        shaped.offset_at(x - left + self.state.address.scroll())
    }

    /// Scrolls the address line to its caret and reports what the host needs to know.
    fn finish(&mut self, mut actions: Vec<UiAction>) -> Vec<UiAction> {
        let before = self.state.address.scroll();
        if self.state.focused {
            let view = self.address_view();
            let field = (view.clip.1 - view.clip.0).max(0.0);
            let caret = view.caret.map_or(0.0, |x| x - view.x);
            let width = view.shaped.width;
            self.state
                .address
                .scroll_to_caret(caret, CARET_WIDTH, width, field);
        } else {
            self.state.address.reset_scroll();
        }
        if self.state.address.scroll() != before {
            self.damage.chrome = true;
        }

        let text_input = self.state.focused.then(|| self.text_input_state());
        if text_input != self.text_input {
            self.text_input = text_input.clone();
            actions.push(UiAction::TextInput(text_input));
        }
        let cursor = match self.state.hover {
            Some(Target::Address) => Cursor::Text,
            Some(_) => Cursor::Pointer,
            None => Cursor::Default,
        };
        if cursor != self.cursor {
            self.cursor = cursor;
            actions.push(UiAction::Cursor(cursor));
        }
        actions
    }

    fn text_input_state(&mut self) -> TextInputState {
        let view = self.address_view();
        let chrome = self.layout.chrome;
        let x = view.caret.unwrap_or(view.x) - chrome.x as f32;
        TextInputState {
            caret: SurfaceRect {
                x: x.round() as i32,
                y: view.y as i32 - chrome.y,
                width: CARET_WIDTH as u32,
                height: TextStyle::SM.line_height.into(),
            },
            surrounding: Some(self.state.address.surrounding()),
            purpose: TextPurpose::Url,
            multiline: false,
        }
    }

    /// The address line as drawn now.
    pub(crate) fn address_view(&mut self) -> AddressView {
        let editor = &self.state.address;
        let display = editor.display();
        let shaped = self.fonts.line(&display.text, TextStyle::SM);
        let field = self.layout.address;
        let clip = (
            field.x as f32 + ADDRESS_PADDING,
            (field.x as f32 + field.width as f32 - ADDRESS_PADDING).max(field.x as f32),
        );
        let x = clip.0 - editor.scroll();
        let y = field.y as f32 + (field.height as f32 - f32::from(TextStyle::SM.line_height)) / 2.0;
        let span = |range: &std::ops::Range<usize>| {
            let (a, b) = (shaped.x_at(range.start), shaped.x_at(range.end));
            (x + a.min(b), x + a.max(b))
        };
        AddressView {
            caret: display.caret.map(|offset| x + shaped.x_at(offset)),
            selection: (!display.selection.is_empty()).then(|| span(&display.selection)),
            preedit: display.preedit.as_ref().map(span),
            shaped,
            x,
            y,
            clip,
        }
    }

    /// The status line and its tone: a command error, else the snapshot's status.
    pub(crate) fn status_line(&self) -> Option<(String, Tone)> {
        match (&self.state.command_error, &self.snapshot.status) {
            (Some(error), _) => Some((untrusted_line(error, MAX_STATUS), Tone::Error)),
            (None, Some(status)) => Some((untrusted_line(status, MAX_STATUS), Tone::Muted)),
            (None, None) => None,
        }
    }

    pub(crate) fn fonts(&mut self) -> &mut Fonts {
        &mut self.fonts
    }

    /// The page the chrome draws in the page area, laid out, if it draws one.
    pub(crate) fn page_layout(&mut self) -> Option<PageLayout> {
        let page = self.layout.page;
        let card_width = (page.width as f32 - 32.0).min(560.0);
        if card_width < 80.0 || page.height == 0 {
            return None;
        }
        let inner = card_width - 40.0;
        let fonts = &mut self.fonts;
        let mut texts = Vec::new();
        let mut y = 0.0;
        let mut add = |shaped: Rc<ShapedText>, x: f32, top: f32, tone, heading| {
            let height = shaped.height;
            texts.push(PageText {
                shaped,
                x,
                y: top,
                tone,
                heading,
            });
            height
        };
        match internal_page(&self.snapshot)? {
            InternalPage::Home(home) => {
                y += add(
                    fonts.line("Sighurt", TextStyle::HEADING),
                    0.0,
                    y,
                    Tone::Text,
                    true,
                );
                y += 8.0;
                let hint = format!(
                    "Type an address or a search above, or a command after a colon, such as \
                     :tab.new. Engines, search, keys and colors are set in {}.",
                    home.config_path
                );
                y += add(
                    fonts.wrapped(&hint, TextStyle::SM, inner),
                    0.0,
                    y,
                    Tone::Muted,
                    false,
                );
                y += 16.0;
                let label = fonts.line("ENGINES", TextStyle::XS.weight(600));
                y += add(label, 0.0, y, Tone::Dim, false);
                for engine in &home.engines {
                    y += 8.0;
                    let style = TextStyle::SM.weight(600);
                    let name = fonts.fitted(&untrusted_line(&engine.name, 100), style, 64.0);
                    let tone = if engine.available {
                        Tone::Muted
                    } else {
                        Tone::Error
                    };
                    let detail = untrusted_line(&engine.detail, 500);
                    let detail = fonts.wrapped(&detail, TextStyle::SM, (inner - 76.0).max(1.0));
                    let row = add(name, 0.0, y, Tone::Text, false);
                    y += row.max(add(detail, 76.0, y, tone, false));
                }
            }
            InternalPage::Error { url, message } => {
                let heading = fonts.line("Can't show this address", TextStyle::HEADING);
                y += add(heading, 0.0, y, Tone::Text, true);
                y += 8.0;
                let url = untrusted_line(url, 1000);
                y += add(
                    fonts.wrapped(&url, TextStyle::XS, inner),
                    0.0,
                    y,
                    Tone::Dim,
                    false,
                );
                y += 12.0;
                let message = untrusted_line(message, 4000);
                let message = fonts.wrapped(&message, TextStyle::SM, inner);
                y += add(message, 0.0, y, Tone::Muted, false);
            }
        }
        let card_height = y + 40.0;
        let card_x = (page.x as f32 + (page.width as f32 - card_width) / 2.0).round();
        let card_y = page.y as f32 + ((page.height as f32 - card_height) / 2.0).max(16.0).round();
        for text in &mut texts {
            text.x += card_x + 20.0;
            text.y += card_y + 20.0;
        }
        Some(PageLayout {
            card: [card_x, card_y, card_width, card_height],
            texts,
        })
    }
}

/// The page the chrome draws for the active tab, if it draws one.
pub(crate) fn internal_page(snapshot: &Snapshot) -> Option<InternalPage<'_>> {
    let tab = snapshot.active()?;
    match &tab.content {
        TabContent::Internal if is_home(&tab.url) => Some(InternalPage::Home(&snapshot.home)),
        TabContent::Internal => Some(InternalPage::Error {
            url: &tab.url,
            message: NO_SUCH_PAGE,
        }),
        TabContent::Error(message) => Some(InternalPage::Error {
            url: &tab.url,
            message,
        }),
        TabContent::Blank | TabContent::Engine => None,
    }
}

/// The letter of a Ctrl shortcut: the key's character, or for layouts without Latin letters
/// (Cyrillic, Greek, ...) the physical key's, so Ctrl+C copies on every layout.
fn shortcut_letter(event: &KeyEvent) -> Option<char> {
    if let Key::Character(text) = &event.key {
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_alphabetic() {
                return Some(c.to_ascii_lowercase());
            }
        }
    }
    let code = event.code.w3c_code()?.strip_prefix("Key")?;
    code.chars().next().map(|c| c.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_from_the_configuration() {
        let mut colors = Colors::default();
        colors.set("bg", "#000000").unwrap();
        colors.set("selection", "#ff000080").unwrap();
        assert_eq!(colors.bg, Rgba::rgb(0x000000));
        assert_eq!(
            colors.selection,
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 0x80
            }
        );
        assert_eq!(
            colors.set("nope", "#123456"),
            Err(ColorError::Unknown("nope".into()))
        );
        assert!(matches!(
            colors.set("text", "white"),
            Err(ColorError::Malformed { .. })
        ));
        // A rejected value changes nothing.
        assert_eq!(colors.text, Colors::default().text);
        for name in Colors::NAMES {
            assert_eq!(colors.set(name, "#0a0a0b"), Ok(()), "{name}");
        }
    }

    #[test]
    fn parses_hex_colors() {
        assert_eq!(Rgba::parse("#0a0a0b"), Some(Rgba::rgb(0x0a0a0b)));
        assert_eq!(Rgba::parse("#60a5fa55"), Some(Rgba::rgb_alpha(0x60a5fa55)));
        for bad in ["0a0a0b", "#fff", "#+a0a0b", "#0a0a0g", "#0a0a0b5"] {
            assert_eq!(Rgba::parse(bad), None, "{bad}");
        }
    }
}
