//! The chrome API used from outside its crate, through what docs/rewrite/extension-api.md §2.6
//! documents: the standard chrome driven through `Box<dyn Chrome>` alone, and a chrome of this
//! test's own. A change that breaks this test breaks chromes outside the workspace and needs an
//! entry in that document's changelog.

mod common;

use common::*;
use sighurt_api::commands::Command;
use sighurt_api::geometry::{LogicalSize, SurfacePoint, SurfaceRect, WindowRect};
use sighurt_api::ids::RequestId;
use sighurt_api::input::{
    ButtonState, FocusEvent, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers, PointerButton,
    PointerEvent, ScrollDelta, TextInputState, TextPurpose,
};
use sighurt_api::settings::{Table, Value};
use sighurt_ui::chrome::CHROME;
use sighurt_ui::extension::{
    completion_suffix, contained, Chrome, ChromeAction, ChromeEvent, ChromeKind, ChromeLayout,
    ChromeSurface, Completion, InputArea, SurfaceName, TextField, Theme,
};
use sighurt_ui::standard::{self, Standard};
use sighurt_ui::{Fonts, Pen, Snapshot, TabContent};

fn configured(mut chrome: Box<dyn Chrome>, width: u32, height: u32) -> Box<dyn Chrome> {
    chrome.handle(ChromeEvent::Configure {
        theme: Theme::default(),
        settings: Table::new(),
    });
    chrome.handle(ChromeEvent::Resize(LogicalSize { width, height }));
    chrome
}

/// The standard chrome with the bundled test font, for a window of `width` × `height`.
fn standard(width: u32, height: u32) -> Box<dyn Chrome> {
    configured(Box::new(Standard::new(fonts())), width, height)
}

fn repaints(actions: &[ChromeAction]) -> Vec<SurfaceName> {
    let repaint = |action: &ChromeAction| match action {
        ChromeAction::Repaint(surface) => Some(*surface),
        _ => None,
    };
    actions.iter().filter_map(repaint).collect()
}

fn names(chrome: &dyn Chrome) -> Vec<SurfaceName> {
    let layout = chrome.layout();
    layout.surfaces.iter().map(|surface| surface.name).collect()
}

/// A press and a release of the primary button at `(x, y)` on `surface`.
fn click(chrome: &mut dyn Chrome, surface: SurfaceName, (x, y): (f64, f64)) -> Vec<ChromeAction> {
    let button = |state| PointerEvent::Button {
        button: PointerButton::Left,
        state,
        position: SurfacePoint { x, y },
        modifiers: Modifiers::default(),
    };
    let mut actions = Vec::new();
    for state in [ButtonState::Pressed, ButtonState::Released] {
        let event = button(state);
        actions.extend(chrome.handle(ChromeEvent::Pointer { surface, event }));
    }
    actions
}

/// Draws `surface` through the trait and renders it at `scale` (over 120) with the CPU backend.
fn paint_surface(chrome: &mut dyn Chrome, surface: SurfaceName, scale: u32) -> Image {
    let rect = chrome.layout().surfaces.iter().find(|s| s.name == surface);
    let metrics = metrics(rect.expect("listed").rect, scale);
    let mut pen = Pen::new(metrics.rect());
    chrome.draw(surface, &mut pen);
    render(&pen.finish(), &metrics)
}

#[test]
fn the_standard_chrome_selects_and_closes_tabs_by_identity() {
    let mut chrome = standard(800, 600);
    let snapshot = three_tabs();
    let ids: Vec<_> = snapshot.tabs.iter().map(|tab| tab.id).collect();
    chrome.handle(ChromeEvent::Snapshot(snapshot));
    assert_eq!(names(&*chrome), ["chrome", "status"]);
    // Three tabs in 800 px are 220 px wide each, 4 px apart, from x = 4; the close button is
    // the 20 px square 6 px from a tab's right edge, centred in the 40 px strip.
    let second = click(&mut *chrome, "chrome", (4.0 + 224.0 + 110.0, 20.0));
    assert_eq!(second, [ChromeAction::SelectTab(ids[1])]);
    let close_third = click(&mut *chrome, "chrome", (4.0 + 448.0 + 220.0 - 16.0, 20.0));
    assert_eq!(close_third, [ChromeAction::CloseTab(ids[2])]);
    // Another chrome's surface name means nothing to it.
    assert!(click(&mut *chrome, "line", (338.0, 20.0)).is_empty());
}

#[test]
fn an_identical_snapshot_repaints_nothing() {
    let mut chrome = standard(800, 600);
    let snapshot = three_tabs();
    let first = chrome.handle(ChromeEvent::Snapshot(snapshot.clone()));
    assert_eq!(repaints(&first), ["chrome", "status"]);
    assert!(repaints(&chrome.handle(ChromeEvent::Snapshot(snapshot.clone()))).is_empty());
    // The keymap's indicator shows in the address field, so it repaints the chrome alone.
    let snapshot = Snapshot {
        keymap: Some("NORMAL".into()),
        ..snapshot
    };
    let actions = chrome.handle(ChromeEvent::Snapshot(snapshot.clone()));
    assert_eq!(repaints(&actions), ["chrome"]);
    assert!(repaints(&chrome.handle(ChromeEvent::Snapshot(snapshot))).is_empty());
}

#[test]
fn a_resize_repaints_every_listed_surface() {
    let mut chrome = standard(800, 600);
    chrome.handle(ChromeEvent::Snapshot(showing(
        "sig://home",
        TabContent::Internal,
    )));
    assert_eq!(names(&*chrome), ["page", "chrome", "status"]);
    let size = LogicalSize {
        width: 640,
        height: 480,
    };
    let actions = chrome.handle(ChromeEvent::Resize(size));
    for name in names(&*chrome) {
        assert!(repaints(&actions).contains(&name), "{name}: {actions:?}");
    }
    let layout = chrome.layout();
    assert_eq!(layout.page, layout.surfaces[0].rect);
    assert_eq!(layout.page.y, 84, "below the tab strip and the toolbar");
}

#[test]
fn painting_through_the_trait_is_painting_the_model() {
    let home = || showing("sig://home", TabContent::Internal);
    let mut chrome = standard(800, 600);
    chrome.handle(ChromeEvent::Snapshot(home()));
    let image = paint_surface(&mut *chrome, "page", 120);
    golden("home-100", &image);
    let mut model = common::chrome(800, 600, home());
    assert!(
        paint(&mut model, "page", 120).rgba == image.rgba,
        "not the same pixels"
    );
}

fn table<const N: usize>(entries: [(&str, Value); N]) -> Table {
    entries.map(|(key, value)| (key.to_owned(), value)).into()
}

fn text(text: &str) -> Value {
    Value::String(text.into())
}

#[test]
fn the_standard_kind() {
    assert_eq!(format!("{:?}", standard::KIND), "ChromeKind(standard)");
    assert_eq!(standard::KIND.commands, ["toggle-tabs"]);
    let check = standard::KIND.check;
    assert_eq!(check(&Table::new()), Vec::<String>::new());
    let every = table([
        ("tabs", text("left")),
        ("status", text("overlay")),
        ("buttons", Value::Boolean(false)),
        ("compact", Value::Boolean(true)),
        ("engine-label", Value::Boolean(false)),
    ]);
    assert_eq!(check(&every), Vec::<String>::new());
    for tabs in ["top", "hidden"] {
        assert_eq!(check(&table([("tabs", text(tabs))])), Vec::<String>::new());
    }
    let wrong = table([
        ("tabs", text("lft")),
        ("status", Value::Boolean(true)),
        ("buttons", text("yes")),
        ("compact", Value::Integer(1)),
        ("engine-label", text("off")),
        ("colour", text("red")),
    ]);
    assert_eq!(
        check(&wrong),
        [
            "buttons: expected true or false, not \"yes\"",
            "colour: unknown key (the standard chrome has tabs, status, buttons, compact and \
             engine-label)",
            "compact: expected true or false, not a whole number",
            "engine-label: expected true or false, not \"off\"",
            "status: expected \"strip\" or \"overlay\", not true",
            "tabs: expected \"top\", \"left\" or \"hidden\", not \"lft\"",
        ]
    );
}

/// The standard chrome configured with `[chrome.standard]` `settings` and `[ui] font-size`.
fn standard_with(settings: Table, font_size: u16, width: u32, height: u32) -> Box<dyn Chrome> {
    let mut chrome: Box<dyn Chrome> = Box::new(Standard::new(fonts()));
    let mut theme = Theme::default();
    theme.font_size = font_size;
    chrome.handle(ChromeEvent::Configure { theme, settings });
    chrome.handle(ChromeEvent::Resize(LogicalSize { width, height }));
    chrome
}

fn surface(chrome: &dyn Chrome, name: SurfaceName) -> ChromeSurface {
    let surfaces = &chrome.layout().surfaces;
    let found = surfaces.iter().find(|surface| surface.name == name);
    found
        .unwrap_or_else(|| panic!("{name} isn't listed"))
        .clone()
}

/// `chrome.toggle-tabs` hides the tabs and shows them again where `[chrome.standard] tabs`
/// puts them, or at the top when that is nowhere; the page takes the room.
#[test]
fn toggle_tabs_hides_and_shows_them() {
    let toggle = || ChromeEvent::Command("toggle-tabs".into());
    let mut chrome = standard(800, 600);
    chrome.handle(ChromeEvent::Snapshot(three_tabs()));
    assert_eq!(chrome.layout().page.y, 84);
    let actions = chrome.handle(toggle());
    assert_eq!(names(&*chrome), ["chrome", "status"]);
    assert_eq!(
        surface(&*chrome, "chrome").rect.height,
        44,
        "the toolbar alone"
    );
    assert_eq!(chrome.layout().page.y, 44);
    assert!(repaints(&actions).contains(&"chrome"), "{actions:?}");
    chrome.handle(toggle());
    assert_eq!(chrome.layout().page.y, 84);
    // An action the chrome doesn't have does nothing.
    assert!(chrome
        .handle(ChromeEvent::Command("toggle-tab".into()))
        .is_empty());

    let left = table([("tabs", text("left"))]);
    let mut chrome = standard_with(left, 14, 800, 600);
    chrome.handle(ChromeEvent::Snapshot(three_tabs()));
    assert_eq!(names(&*chrome), ["tabs", "chrome", "status"]);
    assert_eq!(chrome.layout().page.x, 220);
    chrome.handle(toggle());
    assert_eq!(names(&*chrome), ["chrome", "status"]);
    assert_eq!(chrome.layout().page.x, 0);
    chrome.handle(toggle());
    assert_eq!(names(&*chrome), ["tabs", "chrome", "status"]);

    let hidden = table([("tabs", text("hidden"))]);
    let mut chrome = standard_with(hidden, 14, 800, 600);
    chrome.handle(ChromeEvent::Snapshot(three_tabs()));
    assert_eq!(chrome.layout().page.y, 44);
    chrome.handle(toggle());
    assert_eq!(chrome.layout().page.y, 84, "shown at the top");
}

/// `status = "overlay"`: the status line lies over the page's bottom-left corner, listed only
/// while there is status text, at a size that changes only with the window's, and it takes no
/// pointer input, so the page under it keeps its hover.
#[test]
fn the_status_overlay_lies_over_the_page_and_takes_no_input() {
    let overlay = table([("status", text("overlay"))]);
    let mut chrome = standard_with(overlay, 14, 800, 600);
    let mut snapshot = three_tabs();
    snapshot.status = None;
    chrome.handle(ChromeEvent::Snapshot(snapshot.clone()));
    assert_eq!(names(&*chrome), ["chrome"]);
    let page = chrome.layout().page;
    assert_eq!(
        (page.y, page.height),
        (84, 516),
        "no room taken for a strip"
    );
    snapshot.status = Some("https://sighurt.example/a".into());
    let actions = chrome.handle(ChromeEvent::Snapshot(snapshot.clone()));
    assert_eq!(repaints(&actions), ["status"]);
    let status = surface(&*chrome, "status");
    assert_eq!(
        status.rect,
        WindowRect {
            x: 0,
            y: 580,
            width: 400,
            height: 20
        }
    );
    assert_eq!(status.input, InputArea::Rects(Vec::new()));
    assert_eq!(chrome.layout().page, page);
    // Another hovered link repaints the same surface at the same size.
    snapshot.status = Some("https://sighurt.example/a/much/longer/address".into());
    assert_eq!(
        repaints(&chrome.handle(ChromeEvent::Snapshot(snapshot.clone()))),
        ["status"]
    );
    assert_eq!(surface(&*chrome, "status").rect, status.rect);
    snapshot.status = None;
    assert!(repaints(&chrome.handle(ChromeEvent::Snapshot(snapshot))).is_empty());
    assert_eq!(names(&*chrome), ["chrome"]);
}

/// `tabs = "left"`: rows in a surface of their own, hit by identity; the wheel scrolls it and
/// repaints it alone.
#[test]
fn the_tab_list_on_the_left() {
    let left = table([("tabs", text("left"))]);
    let mut chrome = standard_with(left, 14, 800, 600);
    let mut snapshot = three_tabs();
    snapshot.tabs = (0..40)
        .map(|i| {
            tab(
                &format!("Tab {i}"),
                "https://a.example/",
                TabContent::Engine,
            )
        })
        .collect();
    snapshot.tabs[0].active = true;
    let ids: Vec<_> = snapshot.tabs.iter().map(|tab| tab.id).collect();
    chrome.handle(ChromeEvent::Snapshot(snapshot));
    let list = surface(&*chrome, "tabs");
    assert_eq!(
        list.rect,
        WindowRect {
            x: 0,
            y: 44,
            width: 220,
            height: 536
        }
    );
    // Rows of 32 px every 36 px from 4 px down: the third row's middle is at 4 + 72 + 16.
    let third = click(&mut *chrome, "tabs", (100.0, 92.0));
    assert_eq!(third, [ChromeAction::SelectTab(ids[2])]);
    let wheel = PointerEvent::Scroll {
        delta: ScrollDelta::Wheel { x: 0, y: 120 },
        position: SurfacePoint { x: 100.0, y: 92.0 },
        modifiers: Modifiers::default(),
    };
    let actions = chrome.handle(ChromeEvent::Pointer {
        surface: "tabs",
        event: wheel,
    });
    assert_eq!(repaints(&actions), ["tabs"]);
    let third = click(&mut *chrome, "tabs", (100.0, 92.0));
    assert_eq!(third, [ChromeAction::SelectTab(ids[5])]);
    let image = paint_surface(&mut *chrome, "tabs", 120);
    assert_eq!((image.width, image.height), (220, 536));
}

/// `[ui] font-size` scales the standard chrome: 18 px gives a 51 px tab strip, a 57 px toolbar
/// and a 26 px status strip.
#[test]
fn the_font_size_scales_the_standard_chrome() {
    let mut chrome = standard_with(Table::new(), 18, 800, 600);
    chrome.handle(ChromeEvent::Snapshot(three_tabs()));
    assert_eq!(surface(&*chrome, "chrome").rect.height, 51 + 57);
    assert_eq!(surface(&*chrome, "status").rect.height, 26);
    // A reload back to 14 px lays it out again.
    let actions = chrome.handle(ChromeEvent::Configure {
        theme: Theme::default(),
        settings: Table::new(),
    });
    assert_eq!(surface(&*chrome, "chrome").rect.height, 84);
    assert_eq!(repaints(&actions), ["chrome", "status"]);
}

// ── A chrome of the test's own ─────────────────────────────────────────────────────────────

/// One bar, 20 px high, at the top or (`[chrome.bar] bottom = true`) the bottom, filled with the
/// theme's background, and a "page" surface while the active tab shows no engine. The bar
/// takes pointer input on its left half only; a click there focuses its line.
#[derive(Default)]
struct Bar {
    bottom: bool,
    theme: Theme,
    window: LogicalSize,
    draws_page: bool,
    layout: ChromeLayout,
}

impl Bar {
    fn lay_out(&mut self) -> Vec<ChromeAction> {
        let LogicalSize { width, height } = self.window;
        let bar_y = if self.bottom {
            height.saturating_sub(20)
        } else {
            0
        };
        let page_y = if self.bottom { 0 } else { 20.min(height) };
        let page = WindowRect {
            x: 0,
            y: page_y as i32,
            width,
            height: height.saturating_sub(20),
        };
        let bar_rect = WindowRect {
            x: 0,
            y: bar_y as i32,
            width,
            height: height.min(20),
        };
        let mut bar = ChromeSurface::new("bar", bar_rect);
        bar.input = InputArea::Rects(vec![SurfaceRect {
            x: 0,
            y: 0,
            width: width / 2,
            height: 20,
        }]);
        let page_surface = self.draws_page.then(|| ChromeSurface::new("page", page));
        self.layout = ChromeLayout::new(page, page_surface.into_iter().chain([bar]).collect());
        self.layout
            .surfaces
            .iter()
            .map(|surface| ChromeAction::Repaint(surface.name))
            .collect()
    }
}

impl Chrome for Bar {
    fn handle(&mut self, event: ChromeEvent) -> Vec<ChromeAction> {
        match event {
            ChromeEvent::Configure { theme, settings } => {
                self.theme = theme;
                self.bottom = settings.get("bottom").and_then(Value::as_bool) == Some(true);
                self.lay_out()
            }
            ChromeEvent::Resize(size) => {
                self.window = size;
                self.lay_out()
            }
            ChromeEvent::Snapshot(snapshot) => {
                let shows_engine = snapshot.active().map(|tab| &tab.content);
                self.draws_page = shows_engine != Some(&TabContent::Engine);
                self.lay_out()
            }
            ChromeEvent::Pointer {
                surface: "bar",
                event:
                    PointerEvent::Button {
                        state: ButtonState::Pressed,
                        ..
                    },
            } => {
                let state = TextInputState {
                    caret: SurfaceRect::default(),
                    surrounding: None,
                    purpose: TextPurpose::Url,
                    multiline: false,
                };
                vec![
                    ChromeAction::Focus(KeyboardFocus::Chrome),
                    ChromeAction::TextInput(Some(TextField::new("bar", state))),
                ]
            }
            _ => Vec::new(),
        }
    }

    fn layout(&self) -> &ChromeLayout {
        &self.layout
    }

    fn draw(&mut self, _surface: SurfaceName, pen: &mut Pen) {
        pen.clear(self.theme.colors.bg);
    }
}

const BAR: ChromeKind = ChromeKind {
    name: "bar",
    check: |settings| {
        let bad = |(key, value): (&String, &Value)| match (key.as_str(), value.as_bool()) {
            ("bottom", Some(_)) => None,
            ("bottom", None) => Some(format!(
                "bottom: expected true or false, not {}",
                value.kind()
            )),
            (other, _) => Some(format!("{}: unknown key", contained(other, 32))),
        };
        settings.iter().filter_map(bad).collect()
    },
    commands: &[],
    new: |_: Fonts| Box::<Bar>::default(),
};

#[test]
fn a_chrome_from_outside_the_crate() {
    let bottom = Table::from([("bottom".into(), Value::Boolean(true))]);
    assert_eq!((BAR.check)(&bottom), Vec::<String>::new());
    let bad = Table::from([
        ("bottom".into(), Value::Integer(1)),
        ("x\n".into(), Value::Boolean(true)),
    ]);
    assert_eq!(
        (BAR.check)(&bad),
        [
            "bottom: expected true or false, not a whole number",
            "x : unknown key"
        ]
    );

    let mut chrome = (BAR.new)(fonts());
    let mut theme = Theme::default();
    theme.colors.bg = sighurt_ui::Rgba::rgb(0x102030);
    chrome.handle(ChromeEvent::Configure {
        theme,
        settings: bottom,
    });
    let actions = chrome.handle(ChromeEvent::Resize(LogicalSize {
        width: 300,
        height: 200,
    }));
    assert_eq!(repaints(&actions), ["bar"]);
    let layout = chrome.layout().clone();
    assert_eq!(layout.page.height, 180);
    assert_eq!(layout.surfaces[0].rect.y, 180, "at the bottom");
    assert!(matches!(&layout.surfaces[0].input, InputArea::Rects(rects) if rects[0].width == 150));

    chrome.handle(ChromeEvent::Snapshot(showing(
        "sig://home",
        TabContent::Internal,
    )));
    assert_eq!(names(&*chrome), ["page", "bar"]);
    let image = paint_surface(&mut *chrome, "bar", 180);
    assert_eq!((image.width, image.height), (450, 30));
    assert_eq!(image.pixel(10, 10), [0x10, 0x20, 0x30, 0xff]);

    let actions = click(&mut *chrome, "bar", (10.0, 10.0));
    assert_eq!(actions[0], ChromeAction::Focus(KeyboardFocus::Chrome));
    assert!(matches!(&actions[1], ChromeAction::TextInput(Some(field)) if field.surface == "bar"));
}

/// `[chrome.standard]` with the given booleans.
fn switches<const N: usize>(entries: [(&str, bool); N]) -> Table {
    entries
        .map(|(key, on)| (key.to_owned(), Value::Boolean(on)))
        .into()
}

/// The engine label through the trait: only engine pages have one, in their tab and at the
/// address field's right end; the chrome shows the engine's name contained; and
/// `engine-label = false` turns it off at once, and on again.
#[test]
fn the_engine_label_through_the_trait() {
    let painted = |settings: Table, snapshot: Snapshot| {
        let mut chrome = standard_with(settings, 14, 800, 600);
        chrome.handle(ChromeEvent::Snapshot(snapshot));
        let surfaces = names(&*chrome);
        let images: Vec<_> = surfaces
            .iter()
            .map(|&name| paint_surface(&mut *chrome, name, 150).rgba)
            .collect();
        (surfaces, images)
    };
    let with = |engines: &[&str]| labelled(three_tabs(), engines);
    let plain = painted(Table::new(), three_tabs());
    assert_ne!(
        painted(Table::new(), with(&["servo", "wpe"])),
        plain,
        "engine pages have labels"
    );
    // A label on the home page's tab, which no engine shows, is not drawn.
    assert_eq!(
        painted(Table::new(), with(&["", "", "servo"])),
        plain,
        "sig://home has none"
    );
    for content in [
        TabContent::Internal,
        TabContent::Blank,
        TabContent::Error("No such host".into()),
    ] {
        let page = showing("https://a.example/", content.clone());
        let named = labelled(page.clone(), &["servo"]);
        assert_eq!(
            painted(Table::new(), named),
            painted(Table::new(), page),
            "{content:?}"
        );
    }

    // What `sig` sends is contained (Snapshot::contain), and the chrome contains it again.
    let mut sent = with(&["\u{202e}servo\n", "wpe"]);
    sent.contain();
    assert_eq!(sent.tabs[0].engine.as_deref(), Some("servo "));
    let clean = painted(Table::new(), with(&["servo", "wpe"]));
    assert_eq!(painted(Table::new(), sent), clean);
    assert_eq!(
        painted(Table::new(), with(&["\u{202e}servo\n", "wpe"])),
        clean
    );
    let mut long = with(&[&"x".repeat(300)]);
    long.contain();
    assert_eq!(
        long.tabs[0].engine.as_ref().map(|e| e.chars().count()),
        Some(100)
    );

    // Off and on again while the chrome runs: it repaints, and shows what it would have shown
    // configured that way from the start.
    let mut chrome = standard_with(Table::new(), 14, 800, 600);
    chrome.handle(ChromeEvent::Snapshot(with(&["servo", "wpe"])));
    let reconfigure = |chrome: &mut dyn Chrome, on: bool| {
        chrome.handle(ChromeEvent::Configure {
            theme: Theme::default(),
            settings: switches([("engine-label", on)]),
        })
    };
    let actions = reconfigure(&mut *chrome, false);
    assert!(repaints(&actions).contains(&"chrome"), "{actions:?}");
    let off = paint_surface(&mut *chrome, "chrome", 150).rgba;
    assert_eq!(off, plain.1[0], "no label at all");
    let actions = reconfigure(&mut *chrome, true);
    assert!(repaints(&actions).contains(&"chrome"), "{actions:?}");
    assert_eq!(paint_surface(&mut *chrome, "chrome", 150).rgba, clean.1[0]);
}

/// A tab whose page moves to another engine repaints what shows the label: the tab strip's
/// surface at the top, or the list on the left, and the toolbar for the active tab.
#[test]
fn a_new_engine_repaints_its_label() {
    let mut chrome = standard(800, 600);
    let before = labelled(three_tabs(), &["servo", "servo"]);
    chrome.handle(ChromeEvent::Snapshot(before.clone()));
    let mut after = before.clone();
    after.tabs[1].engine = Some("wpe".into());
    assert_eq!(
        repaints(&chrome.handle(ChromeEvent::Snapshot(after))),
        ["chrome"]
    );

    let left = Table::from([("tabs".to_owned(), text("left"))]);
    let mut chrome = standard_with(left.clone(), 14, 800, 600);
    chrome.handle(ChromeEvent::Snapshot(before.clone()));
    let mut inactive = before.clone();
    inactive.tabs[1].engine = Some("wpe".into());
    let actions = chrome.handle(ChromeEvent::Snapshot(inactive.clone()));
    assert_eq!(repaints(&actions), ["tabs"]);
    let mut active = inactive;
    active.tabs[0].engine = Some("wpe".into());
    let actions = chrome.handle(ChromeEvent::Snapshot(active));
    assert_eq!(repaints(&actions), ["chrome", "tabs"]);

    // A pending switch repaints the toolbar, where the button is, including when the tabs are
    // on the left. An unchanged snapshot — a page painting a frame — repaints nothing.
    let mut chrome = standard(800, 600);
    chrome.handle(ChromeEvent::Snapshot(before.clone()));
    let mut switching = before.clone();
    switching.tabs[0].switching_to = Some("wpe".into());
    assert_eq!(
        repaints(&chrome.handle(ChromeEvent::Snapshot(switching.clone()))),
        ["chrome"]
    );
    assert!(repaints(&chrome.handle(ChromeEvent::Snapshot(switching))).is_empty());

    let mut chrome = standard_with(left, 14, 800, 600);
    chrome.handle(ChromeEvent::Snapshot(before.clone()));
    let mut switching = before;
    switching.tabs[0].switching_to = Some("wpe".into());
    assert_eq!(
        repaints(&chrome.handle(ChromeEvent::Snapshot(switching.clone()))),
        ["chrome", "tabs"]
    );
    assert!(repaints(&chrome.handle(ChromeEvent::Snapshot(switching))).is_empty());
}

fn press(chrome: &mut dyn Chrome, key: Key, text: Option<&str>) -> Vec<ChromeAction> {
    chrome.handle(ChromeEvent::Key(KeyEvent {
        code: KeyCode(30),
        key,
        state: ButtonState::Pressed,
        repeat: false,
        modifiers: Modifiers::default(),
        consumed: Modifiers::default(),
        text: text.map(Into::into),
    }))
}

fn named(chrome: &mut dyn Chrome, name: &str) -> Vec<ChromeAction> {
    press(chrome, Key::Named(name.into()), None)
}

/// Types `text` a character at a time, and returns the completions each key asked for.
fn type_asking(chrome: &mut dyn Chrome, text: &str) -> Vec<(RequestId, String)> {
    let mut asked = Vec::new();
    for c in text.chars() {
        let c = c.to_string();
        for action in press(chrome, Key::Character(c.clone()), Some(&c)) {
            if let ChromeAction::Complete { request, prefix } = action {
                asked.push((request, prefix));
            }
        }
    }
    asked
}

fn answer(chrome: &mut dyn Chrome, request: RequestId, urls: &[&str]) -> Vec<ChromeAction> {
    let matches = urls
        .iter()
        .map(|url| Completion::new((*url).into(), "A page".into()))
        .collect();
    chrome.handle(ChromeEvent::Completions { request, matches })
}

fn opened(actions: &[ChromeAction]) -> Vec<String> {
    let open = |action: &ChromeAction| match action {
        ChromeAction::Command(Command::PageOpen(url)) => Some(url.clone()),
        _ => None,
    };
    actions.iter().filter_map(open).collect()
}

#[test]
fn the_completion_suffix_skips_the_scheme_and_www() {
    let url = "https://www.example.com/a";
    assert_eq!(completion_suffix("exa", url), Some("mple.com/a"));
    assert_eq!(completion_suffix("WWW.Ex", url), Some("ample.com/a"));
    assert_eq!(
        completion_suffix("https://www.e", url),
        Some("xample.com/a")
    );
    assert_eq!(completion_suffix("http://www.e", url), None);
    assert_eq!(completion_suffix("https://exa", url), None);
    assert_eq!(completion_suffix("ample", url), None);
}

/// The standard chrome completes what is typed through the API: it asks, shows only the latest
/// answer for the text still typed, takes the exact address with Right or Tab, and drops the
/// completion as the line changes or loses its tab or the keyboard.
#[test]
fn the_standard_chrome_completes_from_the_visited_pages() {
    const URL: &str = "https://www.example.com/page";
    let mut chrome = standard(900, 600);
    chrome.handle(ChromeEvent::Snapshot(three_tabs()));
    chrome.handle(ChromeEvent::WindowFocus(FocusEvent::Activated));
    let actions = chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    assert!(!actions
        .iter()
        .any(|a| matches!(a, ChromeAction::Complete { .. })));
    let asked = type_asking(&mut *chrome, "exa");
    let texts: Vec<&str> = asked.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(texts, ["e", "ex", "exa"]);
    let plain = paint_surface(&mut *chrome, CHROME, 120);
    assert!(
        repaints(&answer(&mut *chrome, asked[1].0, &[URL])).is_empty(),
        "stale"
    );
    assert_eq!(
        repaints(&answer(&mut *chrome, asked[2].0, &[URL])),
        [CHROME]
    );
    let shown = paint_surface(&mut *chrome, CHROME, 120);
    assert_ne!(plain.rgba, shown.rgba, "the rest shows after the caret");
    // Answered again with nothing, as after history.clear: it goes.
    assert_eq!(repaints(&answer(&mut *chrome, asked[2].0, &[])), [CHROME]);
    assert_eq!(plain.rgba, paint_surface(&mut *chrome, CHROME, 120).rgba);
    answer(&mut *chrome, asked[2].0, &[URL]);
    named(&mut *chrome, "ArrowRight");
    assert_eq!(opened(&named(&mut *chrome, "Enter")), [URL]);

    // With the caret off the end there is none: Tab takes nothing.
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    let (request, _) = type_asking(&mut *chrome, "exa").pop().unwrap();
    named(&mut *chrome, "ArrowLeft");
    answer(&mut *chrome, request, &[URL]);
    named(&mut *chrome, "Tab");
    assert_eq!(opened(&named(&mut *chrome, "Enter")), ["exa"]);

    // A command line asks nothing; an answer after the line lost the keyboard shows nothing.
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    assert!(type_asking(&mut *chrome, ":tab.n").is_empty());
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Page));
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    let (request, _) = type_asking(&mut *chrome, "exa").pop().unwrap();
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Page));
    assert!(repaints(&answer(&mut *chrome, request, &[URL])).is_empty());

    // Another tab drops it.
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    let (request, _) = type_asking(&mut *chrome, "exa").pop().unwrap();
    answer(&mut *chrome, request, &[URL]);
    let mut other = three_tabs();
    other.tabs[0].active = false;
    other.tabs[1].active = true;
    chrome.handle(ChromeEvent::Snapshot(other));
    named(&mut *chrome, "End");
    named(&mut *chrome, "Tab");
    assert_eq!(
        opened(&named(&mut *chrome, "Enter")),
        ["https://recept.example/sylt"]
    );
}

/// In fullscreen the standard chrome lists its surfaces only while they are needed, over the
/// page, which keeps the whole window; a surface listed again is drawn again.
#[test]
fn fullscreen_shows_the_chrome_only_while_it_is_needed() {
    let mut chrome = standard(800, 600);
    let window = WindowRect {
        x: 0,
        y: 0,
        width: 800,
        height: 600,
    };
    let page = Snapshot {
        status: None,
        ..showing("https://a.test/", TabContent::Engine)
    };
    chrome.handle(ChromeEvent::Snapshot(page.clone()));
    let normal = chrome.layout().page;
    assert_ne!(normal, window);
    let full = Snapshot {
        fullscreen: true,
        ..page.clone()
    };
    chrome.handle(ChromeEvent::Snapshot(full.clone()));
    assert!(names(&*chrome).is_empty());
    assert_eq!(chrome.layout().page, window);
    // The keyboard in the line brings it back, over the page.
    let actions = chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    assert_eq!(names(&*chrome), ["chrome", "status"]);
    assert_eq!(chrome.layout().page, window);
    assert_eq!(repaints(&actions), ["chrome", "status"]);
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Page));
    assert!(names(&*chrome).is_empty());
    // So does a notice, for as long as it shows.
    let notice = Snapshot {
        status: Some("a.test is fullscreen. Escape leaves it.".into()),
        ..full.clone()
    };
    let actions = chrome.handle(ChromeEvent::Snapshot(notice));
    assert_eq!(names(&*chrome), ["chrome", "status"]);
    assert!(repaints(&actions).contains(&"status"));
    chrome.handle(ChromeEvent::Snapshot(full.clone()));
    assert!(names(&*chrome).is_empty());
    // A page the chrome draws is drawn as without fullscreen.
    let home = Snapshot {
        fullscreen: true,
        ..showing("sig://home", TabContent::Internal)
    };
    chrome.handle(ChromeEvent::Snapshot(home));
    assert_eq!(names(&*chrome), ["page", "chrome", "status"]);
    assert_eq!(chrome.layout().page, normal);
    // Out of fullscreen, the layout is what it was.
    chrome.handle(ChromeEvent::Snapshot(page));
    assert_eq!(names(&*chrome), ["chrome", "status"]);
    assert_eq!(chrome.layout().page, normal);
}
