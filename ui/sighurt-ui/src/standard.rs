//! The standard chrome, `[ui] chrome = "standard"`: sighurt-ui's [`Model`] and its drawing
//! ([`crate::paint`]) behind the chrome API ([`crate::extension`]), as any other chrome would
//! be. Every build has it, and it is the default.
//!
//! Its surfaces, bottom to top: [`PAGE`] while it draws the page area (`sig://home`, errors),
//! [`TABS`] while the tabs are on the left, [`CHROME`] (the tab strip when the tabs are at the
//! top, and the toolbar) and [`STATUS`] (the status strip, or the overlay while there is status
//! text, which takes no pointer input so the page under it keeps its hover).
//!
//! `[chrome.standard]` takes `tabs` (`"top"`, `"left"` or `"hidden"`), `status` (`"strip"` or
//! `"overlay"`), `buttons`, `compact` and `engine-label` (true or false); its one command is
//! `chrome.toggle-tabs`. `[ui] font` and `font-size` set its text.
//!
//! Must not own: anything the model doesn't; this is only the adapter.

use sighurt_api::geometry::WindowRect;
use sighurt_api::settings::{Table, Value};
use sighurt_render::Pen;

use crate::chrome::{Model, CHROME, PAGE, STATUS, TABS};
use crate::extension::{
    Chrome, ChromeAction, ChromeEvent, ChromeKind, ChromeLayout, ChromeSurface, InputArea,
    SurfaceName,
};
use crate::layout::{LayoutOptions, Status, Tabs};
use crate::{Colors, Fonts};

/// The standard chrome, for the registry.
pub const KIND: ChromeKind = ChromeKind {
    name: "standard",
    check,
    commands: &["toggle-tabs"],
    new: |fonts| Box::new(Standard::new(fonts)),
};

/// Everything wrong with `[chrome.standard]`.
fn check(settings: &Table) -> Vec<String> {
    settings
        .iter()
        .filter_map(|(key, value)| option(key, value, &mut LayoutOptions::default()).err())
        .collect()
}

/// Applies `[chrome.standard]` `key = value` to `options`, or says what is wrong with it.
fn option(key: &str, value: &Value, options: &mut LayoutOptions) -> Result<(), String> {
    let wrong = |expected: &str| {
        let given = match (value.as_str(), value.as_bool()) {
            (Some(text), _) => format!("{text:?}"),
            (_, Some(on)) => on.to_string(),
            _ => value.kind().to_owned(),
        };
        format!("{key}: expected {expected}, not {given}")
    };
    match (key, value.as_str(), value.as_bool()) {
        ("tabs", Some("top"), _) => options.tabs = Tabs::Top,
        ("tabs", Some("left"), _) => options.tabs = Tabs::Left,
        ("tabs", Some("hidden"), _) => options.tabs = Tabs::Hidden,
        ("tabs", ..) => return Err(wrong(r#""top", "left" or "hidden""#)),
        ("status", Some("strip"), _) => options.status = Status::Strip,
        ("status", Some("overlay"), _) => options.status = Status::Overlay,
        ("status", ..) => return Err(wrong(r#""strip" or "overlay""#)),
        ("buttons", _, Some(on)) => options.buttons = on,
        ("compact", _, Some(on)) => options.compact = on,
        ("engine-label", _, Some(on)) => options.engine_label = on,
        ("buttons" | "compact" | "engine-label", ..) => return Err(wrong("true or false")),
        _ => {
            return Err(format!(
                "{key}: unknown key (the standard chrome has tabs, status, buttons, compact and \
                 engine-label)"
            ))
        }
    }
    Ok(())
}

/// The standard chrome.
pub struct Standard {
    model: Model,
    layout: ChromeLayout,
}

impl Standard {
    /// The standard chrome with `fonts`, for a window of no size yet.
    pub fn new(fonts: Fonts) -> Self {
        let model = Model::new(fonts, Colors::default(), LayoutOptions::default());
        Self {
            layout: layout_of(&model),
            model,
        }
    }

    /// Where `surface` is in the window, if it is one of the standard chrome's.
    fn rect(&self, surface: SurfaceName) -> Option<WindowRect> {
        let layout = self.model.layout();
        match surface {
            CHROME => Some(layout.chrome),
            TABS => Some(layout.tab_list),
            STATUS => Some(layout.status),
            PAGE => Some(layout.page),
            _ => None,
        }
    }
}

/// The surfaces `model` shows now.
fn layout_of(model: &Model) -> ChromeLayout {
    let layout = model.layout();
    let mut surfaces = Vec::new();
    if model.draws_page() {
        surfaces.push(ChromeSurface::new(PAGE, layout.page));
    }
    if layout.options.tabs == Tabs::Left {
        surfaces.push(ChromeSurface::new(TABS, layout.tab_list));
    }
    surfaces.push(ChromeSurface::new(CHROME, layout.chrome));
    match layout.options.status {
        Status::Strip => surfaces.push(ChromeSurface::new(STATUS, layout.status)),
        Status::Overlay if model.status_line().is_some() => {
            let mut overlay = ChromeSurface::new(STATUS, layout.status);
            overlay.input = InputArea::Rects(Vec::new());
            surfaces.push(overlay);
        }
        Status::Overlay => {}
    }
    ChromeLayout::new(layout.page, surfaces)
}

impl Chrome for Standard {
    fn handle(&mut self, event: ChromeEvent) -> Vec<ChromeAction> {
        let mut actions = match event {
            ChromeEvent::Configure { theme, settings } => {
                let mut options = LayoutOptions {
                    font_size: theme.font_size,
                    ..LayoutOptions::default()
                };
                for (key, value) in &settings {
                    // The kind's check accepted the table.
                    let _ = option(key, value, &mut options);
                }
                self.model.set_colors(theme.colors);
                self.model.set_font(theme.font.as_deref());
                self.model.set_options(options)
            }
            ChromeEvent::Resize(size) => self.model.resize(size),
            ChromeEvent::Snapshot(snapshot) => self.model.set_snapshot(snapshot),
            ChromeEvent::Focus(focus) => self.model.set_focus(focus),
            ChromeEvent::LineText(text) => self.model.line_text(&text),
            ChromeEvent::WindowFocus(event) => self.model.window_focus(event),
            ChromeEvent::Pointer { surface, event } => match self.rect(surface) {
                Some(rect) => self.model.pointer(rect, &event),
                None => Vec::new(),
            },
            ChromeEvent::Key(key) => self.model.key(&key),
            ChromeEvent::TextInput(event) => self.model.text_input(&event),
            ChromeEvent::Pasted { request, text } => self.model.paste(request, &text),
            ChromeEvent::Command(action) => self.model.command(&action),
            ChromeEvent::Completions { request, matches } => {
                self.model.completions(request, &matches)
            }
        };
        self.layout = layout_of(&self.model);
        // A surface that isn't listed shows nothing; when it is listed again, what lists it
        // (a page to draw, the tabs moving, status text) damages it then.
        let listed = &self.layout.surfaces;
        let damaged = self.model.take_damage().surfaces();
        let repaints = damaged.filter(|name| listed.iter().any(|surface| surface.name == *name));
        actions.extend(repaints.map(ChromeAction::Repaint));
        actions
    }

    fn layout(&self) -> &ChromeLayout {
        &self.layout
    }

    fn draw(&mut self, surface: SurfaceName, pen: &mut Pen) {
        crate::paint::draw(pen, &mut self.model, surface);
    }
}
