//! The standard chrome's behavior: input, snapshots and commands in; `ChromeState` changes,
//! actions and damage out.
//!
//! [`Model`] ties the pieces together: the browser [`Snapshot`], the [`ChromeState`], the
//! [`Layout`], the configured [`Colors`] and the fonts. Input addressed to the chrome becomes
//! [`ChromeAction`]s: browser commands for the core, tabs to select or close by their identity,
//! focus moves, clipboard requests and input-method state for the host. Everything the chrome
//! does is a command, as in the old UI: buttons and the address line issue [`Command`]s for the
//! core to run. [`crate::standard`] puts the model behind the chrome API.
//!
//! The core owns where the keyboard focus is and which commands move it: the executable hands
//! each of its focus moves (`Effect::Focus`) to [`Model::set_focus`]. The moves the chrome makes
//! itself (a click in the address line or on a page it draws, Escape, a `:command`) come out as
//! [`ChromeAction::Focus`], for the executable to tell the core.
//!
//! The address line doubles as the command line: a line starting with `:` runs the command
//! after it, such as `:tab.close 2`, so every command can be reached without a key binding. Its
//! right end shows the keymap's indicator, if the keymap has one ("NORMAL"), and before it the
//! engine of the active tab's page while the line isn't focused, drawn as a button, as each
//! engine page's tab names its engine (`[chrome.standard] engine-label`). A click on that
//! button, exactly on the rectangle that is drawn, runs `page.switch-engine`. Under the pointer
//! it brightens and the cursor is a hand; while held it sinks; while a switch is under way it
//! names the engine being opened and ignores clicks.
//!
//! The find field takes the address line's place while it is open: `page.find` without text
//! opens it ([`Model::find_field`]), with a `/` before the text and the search's state
//! ("3 of 12", [`FindSnapshot::indicator`](crate::FindSnapshot::indicator)) at its right. Each
//! edit runs `page.find <text>`, and one that leaves only whitespace `page.find-stop`. Enter
//! closes it and gives the page the keyboard, keeping the search; Escape closes it and runs
//! `page.find-stop`. The core moving the keyboard, or another tab being selected, closes it
//! and keeps the search. While a search is under way and the field is closed, the line shows
//! its state before the engine button.
//!
//! The chrome marks what needs repainting ([`Damage`]) only after an edit, a hover change, a
//! snapshot change it shows, a configuration change or a layout change: a static chrome needs
//! no tick, and a page animating never repaints it. The caret is steady, so no timer runs.
//!
//! `[chrome.standard]` arranges it ([`LayoutOptions`]): its one command, `chrome.toggle-tabs`
//! ([`Model::command`]), hides the tabs or shows them again, and the tab list on the left
//! scrolls with the wheel by whole rows.
//!
//! Must not own: key binding policy (the core resolves bindings before the chrome sees a key),
//! or the mouse's side buttons (the core's, through `[input.buttons]`).

use std::fmt;
use std::rc::Rc;

use sighurt_api::commands::{Command, MAX_FIND};
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{LogicalSize, SurfaceRect, WindowPoint, WindowRect};
use sighurt_api::ids::RequestId;
use sighurt_api::input::{
    ButtonState, FocusEvent, Key, KeyEvent, KeyboardFocus, PointerButton, PointerEvent,
    ScrollDelta, TextInputEvent, TextInputState, TextPurpose,
};

use crate::extension::{contained, readable_url, ChromeAction, SurfaceName, TextField};
use crate::hit_test::{hit_test, Target};
use crate::layout::{Layout, LayoutOptions, Tabs};
use crate::state::{
    is_home, ChromeState, FindField, HomeInfo, Snapshot, TabContent, TabSnapshot,
    MAX_ENGINE_DETAIL, MAX_ENGINE_NAME, MAX_MESSAGE, MAX_STATUS, NO_SUCH_PAGE,
};
use crate::text::{Fonts, ShapedText, TextStyle};

/// The surface at the top, with the tab strip (when the tabs are at the top) and the toolbar:
/// [`Layout::chrome`].
pub const CHROME: SurfaceName = "chrome";
/// The tab list's surface, while the tabs are on the left: [`Layout::tab_list`].
pub const TABS: SurfaceName = "tabs";
/// The status line's surface, a strip or an overlay: [`Layout::status`].
pub const STATUS: SurfaceName = "status";
/// The page area's surface, while the chrome draws a page there: [`Layout::page`].
pub const PAGE: SurfaceName = "page";

/// Space between the address field's border and its text.
const ADDRESS_PADDING: f32 = 12.0;
/// What the find field shows before its text.
const FIND_PROMPT: &str = "/";
/// Rows of the tab list a wheel notch scrolls.
const ROWS_PER_NOTCH: f64 = 3.0;
/// The caret's width in logical pixels.
pub(crate) const CARET_WIDTH: f32 = 2.0;
/// The engine label's text: smaller than the title or address beside it.
pub(crate) const LABEL: TextStyle = TextStyle::XS;

/// A color, with straight (not premultiplied) alpha: sighurt-render's.
pub use sighurt_render::Rgba;

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

/// Which parts the chrome draws need repainting since [`Model::take_damage`] last ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Damage {
    /// The chrome surface: [`Layout::chrome`].
    pub chrome: bool,
    /// The tab list: [`Layout::tab_list`].
    pub tabs: bool,
    /// The status line: [`Layout::status`].
    pub status: bool,
    /// The page area, while the chrome draws a page there ([`Model::draws_page`]).
    pub page: bool,
}

impl Damage {
    const ALL: Damage = Damage {
        chrome: true,
        tabs: true,
        status: true,
        page: true,
    };

    /// Whether anything needs repainting.
    pub fn any(self) -> bool {
        self.chrome || self.tabs || self.status || self.page
    }

    /// The surfaces that need repainting: [`CHROME`], [`TABS`], [`STATUS`] and [`PAGE`].
    pub fn surfaces(self) -> impl Iterator<Item = SurfaceName> {
        let surfaces = [
            (self.chrome, CHROME),
            (self.tabs, TABS),
            (self.status, STATUS),
            (self.page, PAGE),
        ];
        surfaces
            .into_iter()
            .filter_map(|(damaged, surface)| damaged.then_some(surface))
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
    /// The keymap's indicator at the field's right end, and where it starts.
    pub indicator: Option<(Rc<ShapedText>, f32)>,
    /// The active tab's engine button before the indicator.
    pub engine: Option<EngineButton>,
    /// The find field's prompt, and where it starts, while the field is open.
    pub prompt: Option<(Rc<ShapedText>, f32)>,
    /// The search's state ("3 of 12") before the engine button, and where it starts.
    pub find: Option<(Rc<ShapedText>, f32)>,
}

/// The engine button at the address field's right end, in window coordinates: its engine's
/// name and a symbol (two arrows to switch, one while switching) and, while switching, the
/// engine it switches to. The rectangle is what is drawn and what a click hits.
pub(crate) struct EngineButton {
    /// Left, top, width, height.
    pub rect: [f32; 4],
    /// Whether a switch is under way: the button shows it and ignores clicks.
    pub busy: bool,
    /// Where the text's lines start vertically.
    pub text_y: f32,
    /// The engine the tab shows, and its origin's x.
    pub name: (Rc<ShapedText>, f32),
    /// The symbol's left edge; it is [`BUTTON_ICON`] wide.
    pub icon_x: f32,
    /// The engine being switched to, with its "…", and its origin's x.
    pub target: Option<(Rc<ShapedText>, f32)>,
}

impl EngineButton {
    /// Whether `point` (window pixels) is in the button: half-open, like the other rectangles.
    pub fn contains(&self, (px, py): (f32, f32)) -> bool {
        let [x, y, w, h] = self.rect;
        px >= x && py >= y && px < x + w && py < y + h
    }
}

/// The engine button's inner padding on the left and right.
pub(crate) const BUTTON_PAD: f32 = 7.0;
/// The space between its parts.
pub(crate) const BUTTON_GAP: f32 = 4.0;
/// The width of its symbol.
pub(crate) const BUTTON_ICON: f32 = 11.0;
/// The space above and below the button inside the address field.
const BUTTON_MARGIN: f32 = 5.0;

/// The standard chrome: its state, layout and fonts, and what it does with input.
pub struct Model {
    fonts: Fonts,
    colors: Colors,
    /// As configured; [`Model::command`] can hide or show the tabs on top of it.
    options: LayoutOptions,
    snapshot: Snapshot,
    state: ChromeState,
    layout: Layout,
    damage: Damage,
    /// What the executable was last told, so only changes are reported.
    text_input: Option<TextInputState>,
    cursor: Cursor,
}

impl Model {
    /// A chrome with `fonts` and `colors`, for a window of no size yet: call
    /// [`Model::resize`] and [`Model::set_snapshot`] before painting.
    pub fn new(fonts: Fonts, colors: Colors, options: LayoutOptions) -> Self {
        Self {
            fonts,
            colors,
            options,
            snapshot: Snapshot::default(),
            state: ChromeState::default(),
            layout: Layout::new(LogicalSize::default(), &[], options, 0),
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
    pub fn resize(&mut self, window: LogicalSize) -> Vec<ChromeAction> {
        self.relayout(window);
        self.finish(Vec::new())
    }

    /// Changes what the layout includes and how big it is, as when the configuration is
    /// reloaded. Tabs hidden or shown by `chrome.toggle-tabs` stay so.
    pub fn set_options(&mut self, options: LayoutOptions) -> Vec<ChromeAction> {
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

    /// Sets the text in `family` (`[ui] font`), or in the family the fonts were made with. A
    /// family that isn't installed falls back to that one ([`Fonts::set_family`]).
    pub fn set_font(&mut self, family: Option<&str>) {
        let before = self.fonts.family().map(str::to_owned);
        self.fonts.set_family(family);
        if self.fonts.family() != before.as_deref() {
            self.damage = Damage::ALL;
        }
    }

    /// Runs the chrome's own command `chrome.<action>`: `toggle-tabs` hides the tabs, or shows
    /// them where `[chrome.standard] tabs` puts them, at the top if that is nowhere. Other
    /// actions do nothing.
    pub fn command(&mut self, action: &str) -> Vec<ChromeAction> {
        if action == "toggle-tabs" {
            self.state.tabs_toggled = !self.state.tabs_toggled;
            self.relayout(self.layout.window);
        }
        self.finish(Vec::new())
    }

    /// Shows a new browser snapshot, as after every update of the core. Parts that show
    /// nothing different aren't repainted.
    pub fn set_snapshot(&mut self, snapshot: Snapshot) -> Vec<ChromeAction> {
        if snapshot != self.snapshot {
            let old = std::mem::replace(&mut self.snapshot, snapshot);
            let new = &self.snapshot;
            let tabs = old.tabs != new.tabs;
            // The toolbar shows where the active tab can go, whether it loads, its engine,
            // whether that engine is being switched, and the keymap's indicator.
            let active = |snapshot: &Snapshot| {
                let tab = snapshot.active();
                tab.map(|tab| {
                    (
                        tab.id,
                        tab.loading,
                        tab.engine_label(),
                        tab.switching_label(),
                    )
                })
            };
            let toolbar = old.can_go_back != new.can_go_back
                || old.can_go_forward != new.can_go_forward
                || active(&old) != active(new)
                || old.keymap != new.keymap
                || old.find != new.find;
            let at_top = self.layout.options.tabs == Tabs::Top;
            self.damage.chrome |= toolbar || tabs && at_top;
            self.damage.tabs |= tabs && !at_top;
            self.damage.status |= old.status != new.status;
            self.damage.page |= internal_page(&old) != internal_page(new);
            if old.active().map(|tab| tab.id) != new.active().map(|tab| tab.id) {
                self.reveal_active();
            }
            self.relayout(self.layout.window);
            // A button that just became disabled, or a closed tab, is no longer hovered.
            self.state.hover = self.state.hover.filter(|&target| self.enabled(target));
        }
        // Even an unchanged snapshot settles a line opened with Enter whose load never started.
        self.sync_address();
        self.finish(Vec::new())
    }

    /// Moves the keyboard focus where the core put it (its `Effect::Focus`), after a command or
    /// a click in the page. [`KeyboardFocus::Chrome`] focuses the address line with its text
    /// selected, ready to type over. [`KeyboardFocus::Page`] leaves it and drops what was
    /// typed, except a line just opened with Enter: that stays until its tab commits another
    /// address, stops loading or is switched away from. Reports no focus action, since the move
    /// is done.
    pub fn set_focus(&mut self, focus: KeyboardFocus) -> Vec<ChromeAction> {
        // The find field closes, and the search stays.
        self.close_find();
        match focus {
            KeyboardFocus::Chrome => self.focus_address(),
            KeyboardFocus::Page => self.page_takes_focus(),
        }
        self.finish(Vec::new())
    }

    /// The focused address line holds `text` instead, as if typed over its selected address,
    /// the caret at its end (`location.focus <text>`). Cut to what the editor holds. Ignored
    /// while the line doesn't have the keyboard.
    pub fn line_text(&mut self, text: &str) -> Vec<ChromeAction> {
        if self.state.focused {
            let before = self.state.address.clone();
            self.state.address.select_all();
            self.state.address.insert(text);
            self.edited(&before);
        }
        self.finish(Vec::new())
    }

    /// Opens the find field in the address line's place, holding `text` selected, ready to
    /// type over (`page.find` without text, right after the core focused the line). Ignored
    /// while the line doesn't have the keyboard. Opening it searches nothing: `text` is the
    /// search already under way, if any.
    pub fn find_field(&mut self, text: &str) -> Vec<ChromeAction> {
        if self.state.focused {
            let searched = text.trim().to_owned();
            if self.state.find.is_none() {
                let address = std::mem::take(&mut self.state.address);
                self.state.find = Some(FindField { address, searched });
            } else if let Some(field) = &mut self.state.find {
                field.searched = searched;
            }
            self.state.address.set_text(text);
            self.state.address.select_all();
            self.clear_error();
            self.damage.chrome = true;
        }
        self.finish(Vec::new())
    }

    /// The window gained or lost keyboard focus.
    pub fn window_focus(&mut self, event: FocusEvent) -> Vec<ChromeAction> {
        self.state.window_active = event == FocusEvent::Activated;
        if !self.state.window_active {
            self.state.pressed = None;
            self.state.selecting = false;
            // A held engine button sinks; losing the window cancels that, so it has to be drawn
            // again.
            self.damage.chrome |= self.state.label_pressed;
            self.state.label_pressed = false;
        }
        // Only the focused address line looks different: its caret and selection show while
        // the window is active.
        self.damage.chrome |= self.state.focused;
        self.finish(Vec::new())
    }

    /// Pointer input on a chrome surface placed at `surface` (the chrome, the tab list, the
    /// status line or the page area while the chrome draws it), with the event's positions
    /// local to it. The wheel scrolls the tab list.
    pub fn pointer(&mut self, surface: WindowRect, event: &PointerEvent) -> Vec<ChromeAction> {
        let mut actions = Vec::new();
        match *event {
            PointerEvent::Enter { position } | PointerEvent::Motion { position, .. } => {
                self.motion(surface.to_window(position));
            }
            PointerEvent::Leave => {
                self.leave_label();
                self.set_hover(None);
            }
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
                    _ => {}
                }
            }
            PointerEvent::Scroll {
                delta, position, ..
            } => {
                if self.layout.tab_list.contains(surface.to_window(position)) {
                    self.scroll_tabs(delta);
                }
            }
        }
        self.finish(actions)
    }

    /// A key the core didn't bind, while the chrome has logical focus.
    pub fn key(&mut self, event: &KeyEvent) -> Vec<ChromeAction> {
        let mut actions = Vec::new();
        if self.state.focused && event.state == ButtonState::Pressed {
            let before = self.state.address.clone();
            self.edit_key(event, &mut actions);
            self.edited(&before);
        }
        self.finish(actions)
    }

    /// An update from the input method, while the chrome has logical focus.
    pub fn text_input(&mut self, event: &TextInputEvent) -> Vec<ChromeAction> {
        if self.state.focused {
            let before = self.state.address.clone();
            self.state.address.input_method(event);
            self.edited(&before);
        }
        self.finish(Vec::new())
    }

    /// The clipboard's text for the [`ChromeAction::Paste`] with `request`. Only the latest request
    /// is answered, once, and only while the line has kept focus and its tab since it asked: a
    /// late answer for a request the user left behind is ignored.
    pub fn paste(&mut self, request: RequestId, text: &str) -> Vec<ChromeAction> {
        if self.state.paste == Some(request) {
            self.state.paste = None;
            let before = self.state.address.clone();
            self.state.address.paste(text);
            self.edited(&before);
        }
        self.finish(Vec::new())
    }

    /// The options as configured, with the tabs hidden or shown by `chrome.toggle-tabs`.
    fn shown_options(&self) -> LayoutOptions {
        let mut options = self.options;
        if self.state.tabs_toggled {
            options.tabs = match options.tabs {
                Tabs::Hidden => Tabs::Top,
                Tabs::Top | Tabs::Left => Tabs::Hidden,
            };
        }
        options
    }

    fn relayout(&mut self, window: LogicalSize) {
        let tabs: Vec<_> = self.snapshot.tabs.iter().map(|tab| tab.id).collect();
        let layout = Layout::new(window, &tabs, self.shown_options(), self.state.first_row);
        self.state.first_row = layout.first_row;
        if layout == self.layout {
            return;
        }
        let old = std::mem::replace(&mut self.layout, layout);
        let new = &self.layout;
        // Anything but the tabs moving moves every surface's contents.
        let resized = old.window != new.window
            || old.options != new.options
            || old.tab_list != new.tab_list
            || old.status != new.status
            || old.page != new.page;
        let tabs_moved = old.tabs != new.tabs || old.new_tab != new.new_tab;
        let at_top = new.options.tabs == Tabs::Top;
        self.damage.chrome |= tabs_moved && at_top;
        self.damage.tabs |= tabs_moved && !at_top;
        if resized {
            self.damage = Damage::ALL;
        }
    }

    /// Scrolls the tab list's first row so the active tab is in view, as after a switch.
    fn reveal_active(&mut self) {
        let active = self.snapshot.tabs.iter().position(|tab| tab.active);
        let (Some(active), rows) = (active, self.layout.rows) else {
            return;
        };
        let first = &mut self.state.first_row;
        if active < *first {
            *first = active;
        } else if rows > 0 && active >= *first + rows {
            *first = active + 1 - rows;
        }
    }

    /// Scrolls the tab list by `delta`, a row at a time: a wheel notch is
    /// [`ROWS_PER_NOTCH`] rows, and smooth scrolling moves a row for each row's height.
    fn scroll_tabs(&mut self, delta: ScrollDelta) {
        let pitch = f64::from(self.layout.row_pitch());
        self.state.scrolled += match delta {
            ScrollDelta::Wheel { y, .. } => f64::from(y) / 120.0 * ROWS_PER_NOTCH * pitch,
            ScrollDelta::Pixels { y, .. } => y,
        };
        let rows = (self.state.scrolled / pitch).trunc();
        self.state.scrolled -= rows * pitch;
        let before = self.layout.first_row;
        self.state.first_row = (before as f64 + rows).max(0.0) as usize;
        self.relayout(self.layout.window);
        if self.layout.first_row != before {
            // Other rows are under the pointer now; the next motion finds which.
            self.set_hover(None);
            self.leave_label();
        }
    }

    /// Shows the active tab's committed address ([`readable_url`]), unless the user is editing
    /// it or it shows a line opened with Enter whose tab is still loading from the same
    /// committed address. Switching tabs drops both, and a pending paste.
    fn sync_address(&mut self) {
        // The find field holds the search, not the address; another tab closes it.
        if self.state.find.is_some() {
            if self.snapshot.active().map(|tab| tab.id) == self.state.address_tab {
                return;
            }
            self.close_find();
        }
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
        let shown = readable_url(url);
        if (switched || settled || !editing) && (editor.text() != shown || editing) {
            self.state.address.set_text(&shown);
            self.clear_error();
            self.damage.chrome = true;
        }
    }

    /// Drops what was typed: the line shows the committed address again.
    fn discard_edit(&mut self) {
        let url = self.snapshot.active().map_or("", |tab| tab.url.as_str());
        let shown = readable_url(url);
        if self.state.address.is_modified() || self.state.address.text() != shown {
            self.state.address.set_text(&shown);
            self.damage.chrome = true;
        }
        self.state.opened = None;
        self.clear_error();
    }

    /// Closes the find field, if it is open: the address line comes back as it was. The search
    /// stays.
    fn close_find(&mut self) {
        if let Some(field) = self.state.find.take() {
            self.state.address = field.address;
            self.damage.chrome = true;
        }
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
    fn leave(&mut self, actions: &mut Vec<ChromeAction>) {
        if self.state.focused {
            self.unfocus();
            actions.push(ChromeAction::Focus(KeyboardFocus::Page));
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

    fn edit_key(&mut self, event: &KeyEvent, actions: &mut Vec<ChromeAction>) {
        let select = event.modifiers.shift;
        // Control as a modifier, not as a key the layout used to make a character.
        let word = event.modifiers.control && !event.consumed.control;
        // Alt the same way: a shortcut the core didn't bind types nothing, as in the old URL bar.
        let alt = event.modifiers.alt && !event.consumed.alt;
        let editor = &mut self.state.address;
        match &event.key {
            Key::Named(name) => match name.as_str() {
                "Escape" if self.state.find.is_some() => {
                    self.close_find();
                    self.discard_edit();
                    self.leave(actions);
                    actions.push(ChromeAction::Command(Command::PageFindStop));
                }
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
                        actions.push(ChromeAction::Copy(text.to_owned()));
                        if letter == 'x' {
                            editor.insert("");
                        }
                    }
                }
                Some('v') => {
                    let request = RequestId::next();
                    self.state.paste = Some(request);
                    actions.push(ChromeAction::Paste(request));
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
    fn submit(&mut self, actions: &mut Vec<ChromeAction>) {
        // The find field: the current match is the one wanted; the search stays.
        if self.state.find.is_some() {
            self.close_find();
            self.discard_edit();
            self.leave(actions);
            return;
        }
        let line = self.state.address.text().trim();
        if let Some(command) = line.strip_prefix(':') {
            match command.parse::<Command>() {
                Ok(command) => {
                    self.discard_edit();
                    self.leave(actions);
                    actions.push(ChromeAction::Command(command));
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
            actions.push(ChromeAction::Command(Command::PageOpen(line.to_owned())));
        }
    }

    fn motion(&mut self, point: WindowPoint) {
        let over = self.on_engine_label(point);
        if over != self.state.label_hover {
            self.state.label_hover = over;
            self.damage.chrome = true;
        }
        let target = hit_test(&self.layout, point).filter(|&target| self.enabled(target));
        self.set_hover(target.filter(|_| !over));
        if self.state.selecting {
            let offset = self.address_offset(point.x as f32);
            let before = self.state.address.clone();
            self.state.address.move_to(offset, true);
            self.edited(&before);
        }
    }

    /// The pointer is not over the engine button any more.
    fn leave_label(&mut self) {
        if std::mem::take(&mut self.state.label_hover) {
            self.damage.chrome = true;
        }
    }

    fn set_hover(&mut self, target: Option<Target>) {
        if target != self.state.hover {
            let old = std::mem::replace(&mut self.state.hover, target);
            for target in [old, target].into_iter().flatten() {
                self.damage_target(target);
            }
        }
    }

    /// Marks the surface `target` is drawn on for repainting.
    fn damage_target(&mut self, target: Target) {
        let in_list = matches!(
            target,
            Target::Tab(_) | Target::CloseTab(_) | Target::NewTab
        ) && self.layout.options.tabs == Tabs::Left;
        match in_list {
            true => self.damage.tabs = true,
            false => self.damage.chrome = true,
        }
    }

    fn press(&mut self, point: WindowPoint, extend: bool, actions: &mut Vec<ChromeAction>) {
        if self.on_engine_label(point) {
            // A press on a busy button is swallowed: it doesn't focus the address line, and its
            // release runs nothing.
            self.state.label_pressed = !self.engine_busy();
            self.state.pressed = None;
            self.damage.chrome = true;
            return;
        }
        self.state.label_pressed = false;
        let target = hit_test(&self.layout, point).filter(|&target| self.enabled(target));
        self.state.pressed = target;
        match target {
            // The first click selects everything, ready to type over; later ones place the
            // cursor, and dragging selects.
            Some(Target::Address) if !self.state.focused => {
                self.focus_address();
                actions.push(ChromeAction::Focus(KeyboardFocus::Chrome));
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
                actions.push(ChromeAction::Focus(KeyboardFocus::Page));
            }
            _ => {}
        }
    }

    fn release(&mut self, point: WindowPoint, actions: &mut Vec<ChromeAction>) {
        self.state.selecting = false;
        let on_label = std::mem::take(&mut self.state.label_pressed);
        if on_label {
            self.damage.chrome = true;
            if self.on_engine_label(point) && !self.engine_busy() {
                actions.push(ChromeAction::Command(Command::PageSwitchEngine));
            }
            return;
        }
        let pressed = self.state.pressed.take();
        let target = hit_test(&self.layout, point).filter(|&target| self.enabled(target));
        if pressed.is_none() || pressed != target {
            return;
        }
        let action = match target {
            Some(Target::Tab(id)) => ChromeAction::SelectTab(id),
            Some(Target::CloseTab(id)) => ChromeAction::CloseTab(id),
            Some(Target::NewTab) => ChromeAction::Command(Command::TabNew(None)),
            Some(Target::Back) => ChromeAction::Command(Command::PageBack),
            Some(Target::Forward) => ChromeAction::Command(Command::PageForward),
            Some(Target::ReloadOrStop) => match self.snapshot.active().is_some_and(|t| t.loading) {
                true => ChromeAction::Command(Command::PageStop),
                false => ChromeAction::Command(Command::PageReload),
            },
            Some(Target::Address) | None => return,
        };
        actions.push(action);
    }

    /// Whether `point` lies in the engine button: the rectangle that is drawn, and nothing
    /// around it. Half-open, like the chrome's other rectangles. The button is not a hover
    /// [`Target`]; see [`ChromeState::label_hover`].
    fn on_engine_label(&mut self, point: WindowPoint) -> bool {
        let point = (point.x as f32, point.y as f32);
        let button = self.address_view().engine;
        button.is_some_and(|button| button.contains(point))
    }

    /// Whether the engine button is drawn and a switch is already under way.
    fn engine_busy(&mut self) -> bool {
        self.address_view().engine.is_some_and(|button| button.busy)
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
        let view = self.address_view();
        view.shaped.offset_at(x - view.x)
    }

    /// Scrolls the address line to its caret and reports what the host needs to know.
    fn finish(&mut self, mut actions: Vec<ChromeAction>) -> Vec<ChromeAction> {
        // The find field searches as it is edited.
        if let Some(field) = &mut self.state.find {
            let text = cut(self.state.address.text().trim(), MAX_FIND);
            if text != field.searched {
                field.searched = text.to_owned();
                actions.push(ChromeAction::Command(match text.is_empty() {
                    true => Command::PageFindStop,
                    false => Command::PageFind(Some(text.to_owned())),
                }));
            }
        }
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
            let field = text_input.map(|state| TextField::new(CHROME, state));
            actions.push(ChromeAction::TextInput(field));
        }
        let idle_button = self.address_view().engine.map(|button| !button.busy);
        let on_button = self.state.label_hover && idle_button == Some(true);
        let cursor = match self.state.hover {
            _ if on_button => Cursor::Pointer,
            Some(Target::Address) => Cursor::Text,
            Some(_) => Cursor::Pointer,
            None => Cursor::Default,
        };
        if cursor != self.cursor {
            self.cursor = cursor;
            actions.push(ChromeAction::Cursor(cursor));
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
                height: self.layout.options.text(TextStyle::SM).line_height.into(),
            },
            surrounding: Some(self.state.address.surrounding()),
            purpose: match self.state.find {
                Some(_) => TextPurpose::Normal,
                None => TextPurpose::Url,
            },
            multiline: false,
        }
    }

    /// The address line as drawn now.
    pub(crate) fn address_view(&mut self) -> AddressView {
        let editor = &self.state.address;
        let display = editor.display();
        let options = self.layout.options;
        let style = options.text(TextStyle::SM);
        let shaped = self.fonts.line(&display.text, style);
        let field = self.layout.address;
        let padding = options.scaled(ADDRESS_PADDING);
        let right = field.x as f32 + field.width as f32 - padding;
        // The keymap's indicator ("NORMAL") takes up to a third of the field, on its right.
        let indicator = self
            .snapshot
            .keymap
            .as_deref()
            .filter(|text| !text.is_empty());
        let indicator = indicator.map(|text| {
            let shaped = self.fonts.fitted(text, style, field.width as f32 / 3.0);
            let x = right - shaped.width;
            (shaped, x)
        });
        let label_right = indicator.as_ref().map_or(right, |(_, x)| x - padding);
        // The engine of the active tab's page as a button, before the indicator, if it fits in a
        // third of the field, while the line isn't being edited.
        let active = self.snapshot.active().filter(|_| !self.state.focused);
        let engine = active.and_then(|tab| {
            engine_button_text(&mut self.fonts, options, tab, field.width as f32 / 3.0)
        });
        let find_right = engine
            .as_ref()
            .map_or(label_right, |button| label_right - button.width - padding);
        // The search's state before them, in the find field and while the line isn't edited.
        let finding = self.state.find.is_some();
        let find = self
            .snapshot
            .find
            .as_ref()
            .filter(|_| finding || !self.state.focused);
        let engine_name = self.snapshot.active().and_then(TabSnapshot::engine_label);
        let find = find
            .map(|find| find.indicator(engine_name.as_deref()))
            .filter(|text| !text.is_empty())
            .map(|text| {
                let shaped = self.fonts.fitted(&text, style, field.width as f32 / 3.0);
                let x = find_right - shaped.width;
                (shaped, x)
            });
        let text_right = find.as_ref().map_or(find_right, |(_, x)| x - padding);
        // The find field's prompt, where the text would start.
        let left = field.x as f32 + padding;
        let prompt = finding.then(|| (self.fonts.line(FIND_PROMPT, style), left));
        let text_left = prompt
            .as_ref()
            .map_or(left, |(shaped, x)| x + shaped.width + options.scaled(4.0));
        let clip = (text_left, text_right.max(text_left));
        let x = clip.0 - editor.scroll();
        // On a whole logical pixel, as the input method's caret rectangle is.
        let y =
            field.y as f32 + ((field.height as f32 - f32::from(style.line_height)) / 2.0).floor();
        let engine = engine.map(|text| {
            let k = |px| options.scaled(px);
            let label_line = f32::from(options.text(LABEL).line_height);
            let height = (field.height as f32 - 2.0 * k(BUTTON_MARGIN)).max(label_line + 2.0);
            let top = field.y as f32 + ((field.height as f32 - height) / 2.0).floor();
            let width = text.width.ceil();
            let left = (label_right - width).floor();
            let text_y = top + ((height - label_line) / 2.0).floor();
            let name_x = left + k(BUTTON_PAD);
            let icon_x = name_x + text.name.width + k(BUTTON_GAP);
            let target = text
                .target
                .map(|shaped| (shaped, icon_x + k(BUTTON_ICON) + k(BUTTON_GAP)));
            EngineButton {
                rect: [left, top, width, height],
                busy: target.is_some(),
                text_y,
                name: (text.name, name_x),
                icon_x,
                target,
            }
        });
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
            indicator,
            engine,
            prompt,
            find,
        }
    }

    /// The status line and its tone: a command error, else the snapshot's status (a hovered
    /// link's address reads decoded, as in the address line).
    pub(crate) fn status_line(&self) -> Option<(String, Tone)> {
        match (&self.state.command_error, &self.snapshot.status) {
            (Some(error), _) => Some((contained(error, MAX_STATUS), Tone::Error)),
            (None, Some(status)) => {
                let status = contained(&readable_url(status), MAX_STATUS);
                Some((status, Tone::Muted))
            }
            (None, None) => None,
        }
    }

    pub(crate) fn fonts(&mut self) -> &mut Fonts {
        &mut self.fonts
    }

    /// The page the chrome draws in the page area, laid out, if it draws one.
    pub(crate) fn page_layout(&mut self) -> Option<PageLayout> {
        let page = self.layout.page;
        let options = self.layout.options;
        let (k, style) = (|px| options.scaled(px), |style| options.text(style));
        let card_width = (page.width as f32 - k(32.0)).min(k(560.0));
        if card_width < k(80.0) || page.height == 0 {
            return None;
        }
        let inner = card_width - k(40.0);
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
                    fonts.line("Sighurt", style(TextStyle::HEADING)),
                    0.0,
                    y,
                    Tone::Text,
                    true,
                );
                y += k(8.0);
                let hint = format!(
                    "Type an address or a search above, or a command after a colon, such as \
                     :tab.new. Engines, search, keys and colors are set in {}.",
                    home.config_path
                );
                y += add(
                    fonts.wrapped(&hint, style(TextStyle::SM), inner),
                    0.0,
                    y,
                    Tone::Muted,
                    false,
                );
                y += k(16.0);
                let label = fonts.line("ENGINES", style(TextStyle::XS.weight(600)));
                y += add(label, 0.0, y, Tone::Dim, false);
                for engine in &home.engines {
                    y += k(8.0);
                    let name = contained(&engine.name, MAX_ENGINE_NAME);
                    let name = fonts.fitted(&name, style(TextStyle::SM.weight(600)), k(64.0));
                    let tone = if engine.available {
                        Tone::Muted
                    } else {
                        Tone::Error
                    };
                    let detail = contained(&engine.detail, MAX_ENGINE_DETAIL);
                    let column = k(76.0);
                    let width = (inner - column).max(1.0);
                    let detail = fonts.wrapped(&detail, style(TextStyle::SM), width);
                    let row = add(name, 0.0, y, Tone::Text, false);
                    y += row.max(add(detail, column, y, tone, false));
                }
            }
            InternalPage::Error { url, message } => {
                let heading = fonts.line("Can't show this address", style(TextStyle::HEADING));
                y += add(heading, 0.0, y, Tone::Text, true);
                y += k(8.0);
                let url = contained(&readable_url(url), 1000);
                y += add(
                    fonts.wrapped(&url, style(TextStyle::XS), inner),
                    0.0,
                    y,
                    Tone::Dim,
                    false,
                );
                y += k(12.0);
                let message = contained(message, MAX_MESSAGE);
                let message = fonts.wrapped(&message, style(TextStyle::SM), inner);
                y += add(message, 0.0, y, Tone::Muted, false);
            }
        }
        let card_height = y + k(40.0);
        let card_x = (page.x as f32 + (page.width as f32 - card_width) / 2.0).round();
        let top = ((page.height as f32 - card_height) / 2.0).max(k(16.0));
        let card_y = page.y as f32 + top.round();
        for text in &mut texts {
            text.x += card_x + k(20.0);
            text.y += card_y + k(20.0);
        }
        Some(PageLayout {
            card: [card_x, card_y, card_width, card_height],
            texts,
        })
    }
}

/// The label naming `tab`'s engine ([`TabSnapshot::engine_label`]), if it fits whole in
/// `width`: none when `options` leave it out or the tab names no engine. A cut engine name says
/// little, so a label that doesn't fit gives its room back to the text beside it.
pub(crate) fn engine_label(
    fonts: &mut Fonts,
    options: LayoutOptions,
    tab: &TabSnapshot,
    width: f32,
) -> Option<Rc<ShapedText>> {
    let text = tab.engine_label().filter(|_| options.engine_label)?;
    let shaped = fonts.line(&text, options.text(LABEL));
    (shaped.width <= width).then_some(shaped)
}

/// The pieces of the engine button's text, before it is placed.
pub(crate) struct ButtonText {
    name: Rc<ShapedText>,
    target: Option<Rc<ShapedText>>,
    /// The button's width: the pieces, the symbol and the padding.
    width: f32,
}

/// The engine button's text for `tab` ([`TabSnapshot::engine_label`], and while switching the
/// engine it switches to), if the button fits whole in `width`: none when `options` leave the
/// label out or the tab names no engine.
pub(crate) fn engine_button_text(
    fonts: &mut Fonts,
    options: LayoutOptions,
    tab: &TabSnapshot,
    width: f32,
) -> Option<ButtonText> {
    let text = tab.engine_label().filter(|_| options.engine_label)?;
    let style = options.text(LABEL);
    let name = fonts.line(&text, style);
    let target = tab
        .switching_label()
        .map(|target| fonts.line(&format!("{target}\u{2026}"), style));
    let k = |px| options.scaled(px);
    let mut total = 2.0 * k(BUTTON_PAD) + name.width + k(BUTTON_GAP) + k(BUTTON_ICON);
    if let Some(target) = &target {
        total += k(BUTTON_GAP) + target.width;
    }
    (total.ceil() <= width).then_some(ButtonText {
        name,
        target,
        width: total,
    })
}

/// Where a line in the label's style starts so that its baseline is that of a line in `style`
/// starting at `top`.
pub(crate) fn label_top(
    fonts: &mut Fonts,
    options: LayoutOptions,
    style: TextStyle,
    top: f32,
) -> f32 {
    top + fonts.baseline(style) - fonts.baseline(options.text(LABEL))
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
/// `text` cut to at most `max` bytes at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

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
}
