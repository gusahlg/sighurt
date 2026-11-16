//! The standard chrome, `[ui] chrome = "standard"`: sighurt-ui's [`Model`] and its drawing
//! ([`crate::paint`]) behind the chrome API ([`crate::extension`]), as any other chrome would
//! be. Every build has it, and it is the default.
//!
//! Its surfaces, bottom to top: [`PAGE`] while it draws the page area (`sig://home`, errors),
//! [`TABS`] while the tabs are on the left, [`CHROME`] (the tab strip when the tabs are at the
//! top, and the toolbar) and [`STATUS`] (the status strip, or the overlay while there is status
//! text, which takes no pointer input so the page under it keeps its hover).
//!
//! While the window is fullscreen ([`Snapshot::fullscreen`](crate::Snapshot::fullscreen)) it
//! lists them only while they are needed: while it draws the page area, as always; while the
//! address or find line has the keyboard, or there is status text, over the page, which keeps
//! the whole window. Otherwise it lists none.
//!
//! `[chrome.standard]` takes `tabs` (`"top"`, `"left"` or `"hidden"`), `status` (`"strip"` or
//! `"overlay"`), `buttons`, `compact` and `engine-label` (true or false); its one command is
//! `chrome.toggle-tabs`. `[ui] font` and `font-size` set its text.
//!
//! Must not own: anything the model doesn't; this is only the adapter.

use sighurt_api::geometry::WindowRect;
use sighurt_api::settings::{Table, Value};
use sighurt_render::Pen;

use crate::chrome::{Model, PAGE};
use crate::extension::{
    Chrome, ChromeAction, ChromeEvent, ChromeKind, ChromeLayout, ChromeSurface, InputArea,
    SurfaceName,
};
use crate::layout::{LayoutOptions, Show, Status, Tabs};
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
            PAGE => Some(layout.page),
            name => layout.surface(name).map(|surface| surface.rect),
        }
    }
}

/// The surfaces `model` shows now.
fn layout_of(model: &Model) -> ChromeLayout {
    let layout = model.layout();
    let fullscreen = model.snapshot().fullscreen && !model.draws_page();
    let window = WindowRect {
        x: 0,
        y: 0,
        width: layout.window.width,
        height: layout.window.height,
    };
    if fullscreen && !model.state().focused && model.status_line().is_none() {
        return ChromeLayout::new(window, Vec::new());
    }
    let mut surfaces = Vec::new();
    if model.draws_page() {
        surfaces.push(ChromeSurface::new(PAGE, layout.page));
    }
    for surface in &layout.surfaces {
        let mut listed = ChromeSurface::new(surface.name, surface.rect);
        match surface.layer {
            // A layer shown when needed: only while it has something to say (the status line
            // while there is status), and then without pointer input.
            Some(Show::WhenNeeded) => {
                if model.status_line().is_none() {
                    continue;
                }
                listed.input = InputArea::Rects(Vec::new());
            }
            Some(Show::Always) | None => {}
        }
        surfaces.push(listed);
    }
    // In fullscreen the chrome shows over the page, which keeps its size.
    let page = if fullscreen { window } else { layout.page };
    ChromeLayout::new(page, surfaces)
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
            ChromeEvent::FindField(text) => self.model.find_field(&text),
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
        let before = std::mem::replace(&mut self.layout, layout_of(&self.model));
        // A surface that isn't listed shows nothing; when it is listed again, it is drawn
        // again: what lists it (a page to draw, the tabs moving, status text) damages it, and
        // so does listing it at all (fullscreen hides every surface, and brings them back for
        // a focused line).
        let listed = &self.layout.surfaces;
        let is_listed = |name: &SurfaceName| listed.iter().any(|surface| surface.name == *name);
        let was_listed = |name: &SurfaceName| before.surfaces.iter().any(|s| s.name == *name);
        let damaged = self.model.take_damage().surfaces();
        let revealed = listed
            .iter()
            .map(|s| s.name)
            .filter(|name| !was_listed(name));
        let mut repaints: Vec<SurfaceName> = Vec::new();
        for name in damaged.chain(revealed) {
            if is_listed(&name) && !repaints.contains(&name) {
                repaints.push(name);
            }
        }
        actions.extend(repaints.into_iter().map(ChromeAction::Repaint));
        actions
    }

    fn layout(&self) -> &ChromeLayout {
        &self.layout
    }

    fn draw(&mut self, surface: SurfaceName, pen: &mut Pen) {
        crate::paint::draw(pen, &mut self.model, surface);
    }
}

#[cfg(test)]
mod tests {
    use sighurt_api::geometry::LogicalSize;
    use sighurt_api::ids::TabId;
    use sighurt_api::input::KeyboardFocus;
    use sighurt_api::keymap::Frontend;
    use sighurt_api::ui::{Component, Kind, Node, Side};

    use super::*;
    use crate::chrome::{CHROME, STATUS, TABS};
    use crate::layout::{Arrangement, Status, Tabs};
    use crate::{Colors, Snapshot, TabContent, TabSnapshot};

    const FONT: &[u8] =
        include_bytes!("../../../render/sighurt-render/tests/fonts/Cantarell-Regular.ttf");

    fn snapshot(home: bool, status: bool, fullscreen: bool) -> Snapshot {
        let tab = |url: &str, content, active| TabSnapshot {
            id: TabId::next(),
            title: url.into(),
            url: url.into(),
            loading: false,
            active,
            content,
            engine: None,
            switching_to: None,
        };
        Snapshot {
            tabs: vec![
                tab("https://a.test/", TabContent::Engine, !home),
                tab("sig://home", TabContent::Internal, home),
            ],
            status: status.then(|| "https://a.test/link".into()),
            fullscreen,
            ..Snapshot::default()
        }
    }

    /// `layout_of` before trees, from the options.
    fn listed_before(model: &Model, options: LayoutOptions) -> ChromeLayout {
        let layout = model.layout();
        let fullscreen = model.snapshot().fullscreen && !model.draws_page();
        let window = WindowRect {
            x: 0,
            y: 0,
            width: layout.window.width,
            height: layout.window.height,
        };
        if fullscreen && !model.state().focused && model.status_line().is_none() {
            return ChromeLayout::new(window, Vec::new());
        }
        let mut surfaces = Vec::new();
        if model.draws_page() {
            surfaces.push(ChromeSurface::new(PAGE, layout.page));
        }
        if options.tabs == Tabs::Left {
            surfaces.push(ChromeSurface::new(TABS, layout.tab_list));
        }
        surfaces.push(ChromeSurface::new(CHROME, layout.chrome));
        match options.status {
            Status::Strip => surfaces.push(ChromeSurface::new(STATUS, layout.status)),
            Status::Overlay if model.status_line().is_some() => {
                let mut overlay = ChromeSurface::new(STATUS, layout.status);
                overlay.input = InputArea::Rects(Vec::new());
                surfaces.push(overlay);
            }
            Status::Overlay => {}
        }
        let page = if fullscreen { window } else { layout.page };
        ChromeLayout::new(page, surfaces)
    }

    fn model(arrangement: impl Into<Arrangement>, width: u32, height: u32) -> Model {
        let fonts = Fonts::from_data([FONT.to_vec()]);
        let mut model = Model::new(fonts, Colors::default(), arrangement);
        model.resize(LogicalSize { width, height });
        model
    }

    /// Every combination of the options, in every state that changes what is listed (the page
    /// drawn or not, status or not, fullscreen with and without the keyboard), lists the
    /// surfaces the options listed before trees, in their order.
    #[test]
    fn surfaces_are_what_the_options_listed() {
        for tabs in [Tabs::Top, Tabs::Left, Tabs::Hidden] {
            for status in [Status::Strip, Status::Overlay] {
                for (buttons, compact) in [(true, false), (false, true)] {
                    let options = LayoutOptions {
                        tabs,
                        status,
                        buttons,
                        compact,
                        ..LayoutOptions::default()
                    };
                    for (width, height) in [(800, 600), (300, 90), (0, 0)] {
                        for state in 0..16 {
                            let bit = |n: u32| state & (1 << n) != 0;
                            let mut model = model(options, width, height);
                            model.set_snapshot(snapshot(bit(0), bit(1), bit(2)));
                            if bit(3) {
                                model.set_focus(KeyboardFocus::Chrome);
                            }
                            let what = format!("{options:?} {width}×{height} {state:04b}");
                            let listed = layout_of(&model);
                            assert_eq!(listed, listed_before(&model, options), "{what}");
                        }
                    }
                }
            }
        }
    }

    /// The rules a tree can't break: at most 8 surfaces with names from the pool, each once;
    /// the status line listed whenever there is status (where there is room for it); the
    /// controls listed in fullscreen while the line has the keyboard; the page area listed
    /// while the chrome draws it.
    #[test]
    fn every_placement_keeps_the_rules() {
        let pool = [CHROME, TABS, STATUS, PAGE];
        let window = Frontend::Gui;
        let mut trees = Vec::new();
        for tabs in [Side::Top, Side::Bottom, Side::Left, Side::Right] {
            for address in [Side::Top, Side::Bottom] {
                for overlay in [false, true] {
                    let mut tree = Node::standard();
                    tree.change(window, |tree| {
                        tree.position(Kind::AddressBar, address);
                        tree.position(Kind::TabBar, tabs);
                        if overlay {
                            let status = Component::new(Kind::StatusBar);
                            let anchor = sighurt_api::ui::Anchor::BottomRight;
                            let show = sighurt_api::ui::Show::WhenNeeded;
                            tree.overlay(status, anchor, show);
                        }
                    })
                    .unwrap();
                    trees.push(tree);
                }
            }
        }
        for tree in trees {
            let arrangement = Arrangement {
                tree: tree.clone(),
                density: Default::default(),
                font_size: 14,
            };
            for state in 0..16 {
                let bit = |n: u32| state & (1 << n) != 0;
                let mut model = model(arrangement.clone(), 900, 700);
                model.set_snapshot(snapshot(bit(0), bit(1), bit(2)));
                if bit(3) {
                    model.set_focus(KeyboardFocus::Chrome);
                }
                let what = format!("{tree:?} {state:04b}");
                let listed = layout_of(&model);
                let names: Vec<_> = listed.surfaces.iter().map(|s| s.name).collect();
                assert!(names.len() <= 8, "{what}: {names:?}");
                for (i, name) in names.iter().enumerate() {
                    assert!(pool.contains(name), "{what}: {name}");
                    assert!(!names[..i].contains(name), "{what}: {name} twice");
                }
                // A strip is listed but while fullscreen hides everything; an overlay while
                // there is status.
                let hidden = bit(2) && !bit(0) && !bit(3) && !bit(1);
                let status = names.contains(&STATUS);
                let expected = match tree_overlays(&tree) {
                    true => bit(1),
                    false => !hidden,
                };
                assert_eq!(status, expected, "{what}");
                if bit(2) && bit(3) {
                    assert!(names.contains(&CHROME), "{what}: the line in fullscreen");
                }
                assert_eq!(names.contains(&PAGE), bit(0), "{what}");
                // Every surface is inside the window, and the page area too.
                let whole = WindowRect {
                    x: 0,
                    y: 0,
                    width: 900,
                    height: 700,
                };
                for surface in &listed.surfaces {
                    assert_eq!(crate::layout::intersect(surface.rect, whole), surface.rect);
                }
            }
        }
    }

    fn tree_overlays(tree: &Node) -> bool {
        tree.components().iter().any(|(_, layer)| layer.is_some())
    }
}
