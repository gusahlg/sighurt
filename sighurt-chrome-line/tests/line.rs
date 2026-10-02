//! The line chrome driven through the chrome API alone, as `sig` drives it: editing and
//! commands, the clipboard, surfaces, damage, where the caret is drawn, and golden images at
//! 100, 150 and 200 %.
//!
//! Golden images are in tests/golden/ and use sighurt-ui's bundled test font. After an intended
//! change of the look, regenerate them with `SIGHURT_BLESS=1 cargo test -p sighurt-chrome-line`
//! and look at every changed image.

use std::path::PathBuf;

use sighurt_api::commands::Command;
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{LogicalSize, Metrics, Scale, SurfacePoint, WindowRect};
use sighurt_api::ids::{LayoutEpoch, TabId};
use sighurt_api::input::{
    ButtonState, FocusEvent, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers, PointerButton,
    PointerEvent,
};
use sighurt_api::settings::{Table, Value};
use sighurt_chrome_line::{Line, KIND, LINE, PAGE};
use sighurt_ui::extension::{Chrome, ChromeAction, ChromeEvent, SurfaceName, Theme};
use sighurt_ui::{
    Colors, EngineRow, Fonts, HomeInfo, PixelBuffer, Snapshot, TabContent, TabSnapshot,
};
use tiny_skia::{IntSize, Pixmap};

/// sighurt-ui's bundled test font (sighurt-ui/tests/fonts/README.md).
const FONT: &[u8] = include_bytes!("../../sighurt-ui/tests/fonts/Cantarell-Regular.ttf");

/// 100 %, 125 %, 150 % and 200 %, as numerators over 120.
const SCALES: [u32; 4] = [120, 150, 180, 240];

fn line(settings: Table, width: u32, height: u32) -> Box<dyn Chrome> {
    let mut chrome: Box<dyn Chrome> = Box::new(Line::new(Fonts::from_data([FONT.to_vec()])));
    chrome.handle(ChromeEvent::Configure {
        theme: Theme::default(),
        settings,
    });
    chrome.handle(ChromeEvent::Resize(LogicalSize { width, height }));
    chrome.handle(ChromeEvent::WindowFocus(FocusEvent::Activated));
    chrome
}

fn tab(title: &str, url: &str, content: TabContent) -> TabSnapshot {
    TabSnapshot {
        id: TabId::next(),
        title: title.into(),
        url: url.into(),
        loading: false,
        active: false,
        content,
    }
}

/// Five tabs, the second active and loading, a hovered link's status and a keymap's indicator.
fn five_tabs() -> Snapshot {
    let mut tabs: Vec<_> = (0..5)
        .map(|i| {
            tab(
                &format!("Tab {i}"),
                "https://a.example/",
                TabContent::Engine,
            )
        })
        .collect();
    tabs[1].url = "https://sv.wikipedia.org/wiki/R%C3%A4ksm%C3%B6rg%C3%A5s#K%C3%A4llor".into();
    tabs[1].active = true;
    tabs[1].loading = true;
    Snapshot {
        tabs,
        status: Some("https://sv.wikipedia.org/wiki/Bl%C3%A5b%C3%A4r".into()),
        keymap: Some("NORMAL".into()),
        ..Snapshot::default()
    }
}

fn showing(url: &str, content: TabContent) -> Snapshot {
    let mut only = tab(url, url, content);
    only.active = true;
    let engine = |name: &str, detail: &str, available| EngineRow {
        name: name.into(),
        detail: detail.into(),
        available,
    };
    Snapshot {
        tabs: vec![only],
        home: HomeInfo {
            config_path: "/home/user/.config/sighurt/config.toml".into(),
            engines: vec![
                engine("servo", "compiled in · default", true),
                engine("demo", "not compiled into this build", false),
            ],
        },
        ..Snapshot::default()
    }
}

fn key(key: Key, text: Option<&str>, modifiers: Modifiers) -> KeyEvent {
    KeyEvent {
        code: KeyCode(30),
        key,
        state: ButtonState::Pressed,
        repeat: false,
        modifiers,
        consumed: Modifiers::default(),
        text: text.map(Into::into),
    }
}

fn named(chrome: &mut dyn Chrome, name: &str) -> Vec<ChromeAction> {
    let event = key(Key::Named(name.into()), None, Modifiers::default());
    chrome.handle(ChromeEvent::Key(event))
}

fn ctrl(chrome: &mut dyn Chrome, letter: &str) -> Vec<ChromeAction> {
    let control = Modifiers {
        control: true,
        ..Modifiers::default()
    };
    let event = key(Key::Character(letter.into()), None, control);
    chrome.handle(ChromeEvent::Key(event))
}

fn type_text(chrome: &mut dyn Chrome, text: &str) {
    for c in text.chars() {
        let c = c.to_string();
        let event = key(Key::Character(c.clone()), Some(&c), Modifiers::default());
        chrome.handle(ChromeEvent::Key(event));
    }
}

fn click(chrome: &mut dyn Chrome, surface: SurfaceName, (x, y): (f64, f64)) -> Vec<ChromeAction> {
    let mut actions = Vec::new();
    for state in [ButtonState::Pressed, ButtonState::Released] {
        let event = PointerEvent::Button {
            button: PointerButton::Left,
            state,
            position: SurfacePoint { x, y },
            modifiers: Modifiers::default(),
        };
        actions.extend(chrome.handle(ChromeEvent::Pointer { surface, event }));
    }
    actions
}

fn commands(actions: &[ChromeAction]) -> Vec<Command> {
    let command = |action: &ChromeAction| match action {
        ChromeAction::Command(command) => Some(command.clone()),
        _ => None,
    };
    actions.iter().filter_map(command).collect()
}

fn repaints(actions: &[ChromeAction]) -> Vec<SurfaceName> {
    let repaint = |action: &ChromeAction| match action {
        ChromeAction::Repaint(surface) => Some(*surface),
        _ => None,
    };
    actions.iter().filter_map(repaint).collect()
}

fn names(chrome: &dyn Chrome) -> Vec<SurfaceName> {
    chrome.layout().surfaces.iter().map(|s| s.name).collect()
}

fn rect(chrome: &dyn Chrome, surface: SurfaceName) -> WindowRect {
    let surfaces = &chrome.layout().surfaces;
    surfaces.iter().find(|s| s.name == surface).unwrap().rect
}

/// The input method's caret, from the last text-field action among `actions`.
fn caret(actions: &[ChromeAction]) -> Option<sighurt_api::geometry::SurfaceRect> {
    let field = |action: &ChromeAction| match action {
        ChromeAction::TextInput(Some(field)) => Some(field.state.caret),
        _ => None,
    };
    actions.iter().rev().find_map(field)
}

/// A painted buffer as RGBA bytes.
struct Image {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Image {
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y * self.width + x) as usize * 4;
        self.rgba[i..i + 4].try_into().unwrap()
    }
}

fn paint(chrome: &mut dyn Chrome, surface: SurfaceName, scale: u32) -> Image {
    let scale = Scale::from_fractional(scale).unwrap();
    let metrics = Metrics::new(LayoutEpoch::next(), rect(chrome, surface), scale);
    let size = metrics.buffer();
    let mut data = vec![0u8; size.width as usize * size.height as usize * 4];
    let buffer = PixelBuffer {
        data: &mut data,
        width: size.width,
        height: size.height,
        stride: size.width as usize * 4,
    };
    chrome.paint(surface, &metrics, buffer).unwrap();
    // ARGB8888 little-endian (B, G, R, A) to R, G, B, A.
    let rgba = data
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[b, g, r, a]| [r, g, b, a])
        .collect();
    Image {
        width: size.width,
        height: size.height,
        rgba,
    }
}

/// Compares `image` with tests/golden/`name`.png, or writes it there with `SIGHURT_BLESS=1`. A
/// pixel differs if a channel is off by more than 8, and at most 0.2 % may differ.
fn golden(name: &str, image: &Image) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/{name}.png"));
    let size = IntSize::from_wh(image.width, image.height).unwrap();
    let actual = Pixmap::from_vec(image.rgba.clone(), size).unwrap();
    if std::env::var_os("SIGHURT_BLESS").is_some() {
        actual.save_png(&path).unwrap();
        return;
    }
    let expected = Pixmap::load_png(&path)
        .unwrap_or_else(|error| panic!("{}: {error}; bless it", path.display()));
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.png"));
    actual.save_png(&out).unwrap();
    assert_eq!(
        (expected.width(), expected.height()),
        (image.width, image.height),
        "{name}: size changed; this run's image is {}",
        out.display()
    );
    let differing = expected
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(image.rgba.as_chunks::<4>().0)
        .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 8))
        .count();
    let allowed = (f64::from(image.width * image.height) * 0.002) as usize;
    assert!(
        differing <= allowed,
        "{name}: {differing} pixels differ; this run's image is {}",
        out.display()
    );
}

#[test]
fn the_kind_checks_its_table() {
    assert_eq!(format!("{KIND:?}"), "ChromeKind(line)");
    assert_eq!(KIND.commands, [] as [&str; 0]);
    let table = |key: &str, value| Table::from([(key.to_owned(), value)]);
    for position in ["top", "bottom"] {
        let ok = table("position", Value::String(position.into()));
        assert_eq!((KIND.check)(&ok), Vec::<String>::new());
    }
    let wrong = table("position", Value::String("left".into()));
    assert_eq!(
        (KIND.check)(&wrong),
        ["position: expected \"top\" or \"bottom\", not \"left\""]
    );
    let unknown = table("tabs", Value::Boolean(true));
    assert_eq!(
        (KIND.check)(&unknown),
        ["tabs: unknown key (the line chrome has position)"]
    );
}

/// One line at the top, or at the bottom; a page surface only while it draws the page.
#[test]
fn its_surfaces_are_a_line_and_a_page() {
    let mut chrome = line(Table::new(), 800, 600);
    chrome.handle(ChromeEvent::Snapshot(five_tabs()));
    assert_eq!(names(&*chrome), [LINE]);
    assert_eq!(
        rect(&*chrome, LINE),
        WindowRect {
            x: 0,
            y: 0,
            width: 800,
            height: 28
        }
    );
    assert_eq!(
        chrome.layout().page,
        WindowRect {
            x: 0,
            y: 28,
            width: 800,
            height: 572
        }
    );
    let actions = chrome.handle(ChromeEvent::Snapshot(showing(
        "sig://home",
        TabContent::Internal,
    )));
    assert_eq!(names(&*chrome), [PAGE, LINE]);
    assert_eq!(repaints(&actions), [LINE, PAGE]);

    let bottom = Table::from([("position".to_owned(), Value::String("bottom".into()))]);
    let mut chrome = line(bottom, 800, 600);
    chrome.handle(ChromeEvent::Snapshot(five_tabs()));
    assert_eq!(
        rect(&*chrome, LINE),
        WindowRect {
            x: 0,
            y: 572,
            width: 800,
            height: 28
        }
    );
    assert_eq!(
        chrome.layout().page,
        WindowRect {
            x: 0,
            y: 0,
            width: 800,
            height: 572
        }
    );
    // The font size makes it taller: 28 × 18 / 14.
    let mut chrome = line(Table::new(), 800, 600);
    let mut theme = Theme::default();
    theme.font_size = 18;
    chrome.handle(ChromeEvent::Configure {
        theme,
        settings: Table::new(),
    });
    assert_eq!(rect(&*chrome, LINE).height, 36);
}

/// Nothing changes, nothing repaints: an identical snapshot, a snapshot of something the line
/// doesn't show, pointer motion.
#[test]
fn it_repaints_only_what_changed() {
    let mut chrome = line(Table::new(), 800, 600);
    let snapshot = five_tabs();
    assert_eq!(
        repaints(&chrome.handle(ChromeEvent::Snapshot(snapshot.clone()))),
        [LINE]
    );
    assert!(repaints(&chrome.handle(ChromeEvent::Snapshot(snapshot.clone()))).is_empty());
    let mut retitled = snapshot.clone();
    retitled.tabs[3].title = "Another title".into();
    assert!(repaints(&chrome.handle(ChromeEvent::Snapshot(retitled.clone()))).is_empty());
    let motion = PointerEvent::Motion {
        position: SurfacePoint { x: 10.0, y: 10.0 },
        modifiers: Modifiers::default(),
    };
    let actions = chrome.handle(ChromeEvent::Pointer {
        surface: LINE,
        event: motion.clone(),
    });
    assert_eq!(actions, [ChromeAction::Cursor(Cursor::Text)]);
    let actions = chrome.handle(ChromeEvent::Pointer {
        surface: LINE,
        event: motion,
    });
    assert!(actions.is_empty());
    let mut status = retitled;
    status.status = None;
    assert_eq!(
        repaints(&chrome.handle(ChromeEvent::Snapshot(status))),
        [LINE]
    );
}

/// Typing an address and Enter opens it; a `:command` runs after the keyboard goes back to the
/// page; Escape gives it back and drops what was typed.
#[test]
fn typing_opens_addresses_and_runs_commands() {
    let mut chrome = line(Table::new(), 800, 600);
    chrome.handle(ChromeEvent::Snapshot(five_tabs()));
    // location.focus: the address is selected, ready to type over.
    let actions = chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    assert!(caret(&actions).is_some(), "{actions:?}");
    type_text(&mut *chrome, "blåbär");
    assert_eq!(
        commands(&named(&mut *chrome, "Enter")),
        [Command::PageOpen("blåbär".into())]
    );
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Page));

    // A click on the line focuses it, then a command.
    let actions = click(&mut *chrome, LINE, (400.0, 14.0));
    assert_eq!(actions[0], ChromeAction::Focus(KeyboardFocus::Chrome));
    type_text(&mut *chrome, ":tab.next");
    let actions = named(&mut *chrome, "Enter");
    assert_eq!(commands(&actions), [Command::TabNext]);
    assert!(actions.contains(&ChromeAction::TextInput(None)));
    let page = actions
        .iter()
        .position(|a| *a == ChromeAction::Focus(KeyboardFocus::Page));
    let command = actions
        .iter()
        .position(|a| matches!(a, ChromeAction::Command(_)));
    assert!(
        page < command,
        "the page has the keyboard before the command runs"
    );

    // A bad command keeps the line and says why; Escape drops it all.
    click(&mut *chrome, LINE, (400.0, 14.0));
    type_text(&mut *chrome, ":tab.explode");
    let actions = named(&mut *chrome, "Enter");
    assert!(commands(&actions).is_empty());
    assert_eq!(repaints(&actions), [LINE], "the error shows");
    let actions = named(&mut *chrome, "Escape");
    assert!(actions.contains(&ChromeAction::Focus(KeyboardFocus::Page)));
    assert!(actions.contains(&ChromeAction::TextInput(None)));
    // Keys without the keyboard type nothing.
    type_text(&mut *chrome, "x");
    assert!(commands(&named(&mut *chrome, "Enter")).is_empty());
}

/// Ctrl+V asks for the clipboard; only the latest answer is pasted, once, without line breaks,
/// and not after the line lost the keyboard.
#[test]
fn a_paste() {
    let mut chrome = line(Table::new(), 800, 600);
    chrome.handle(ChromeEvent::Snapshot(five_tabs()));
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    let request = |actions: Vec<ChromeAction>| {
        let paste = actions.iter().find_map(|action| match action {
            ChromeAction::Paste(request) => Some(*request),
            _ => None,
        });
        paste.expect("a clipboard read")
    };
    let stale = request(ctrl(&mut *chrome, "v"));
    let latest = request(ctrl(&mut *chrome, "v"));
    let pasted = |chrome: &mut dyn Chrome, request, text: &str| {
        chrome.handle(ChromeEvent::Pasted {
            request,
            text: text.into(),
        })
    };
    assert!(pasted(&mut *chrome, stale, "stale").is_empty());
    let actions = pasted(&mut *chrome, latest, "https://exempel.se/\nsylt");
    assert_eq!(repaints(&actions), [LINE]);
    assert_eq!(
        commands(&named(&mut *chrome, "Enter")),
        [Command::PageOpen("https://exempel.se/sylt".into())]
    );
    assert!(pasted(&mut *chrome, latest, "again").is_empty());
    let late = request(ctrl(&mut *chrome, "v"));
    named(&mut *chrome, "Escape");
    assert!(pasted(&mut *chrome, late, "late").is_empty());
    // Ctrl+A, Ctrl+C copies what is shown: the address, decoded.
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    ctrl(&mut *chrome, "a");
    assert_eq!(
        ctrl(&mut *chrome, "c"),
        [ChromeAction::Copy(
            "https://sv.wikipedia.org/wiki/Räksmörgås#Källor".into()
        )]
    );
}

/// A click on the focused line places the caret where it is drawn, at every scale: the caret
/// the input method is told about, and the caret painted, are at the boundary nearest the click.
#[test]
fn a_click_places_the_caret_where_it_is_drawn() {
    let text_color = Colors::default().text;
    let text_color = [text_color.r, text_color.g, text_color.b, 255];
    for scale in SCALES {
        let mut chrome = line(Table::new(), 800, 600);
        let mut snapshot = five_tabs();
        snapshot.status = None;
        chrome.handle(ChromeEvent::Snapshot(snapshot));
        chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
        type_text(&mut *chrome, "räksmörgås och blåbär");
        let start = caret(&named(&mut *chrome, "Home")).expect("the caret moved");
        let end = caret(&named(&mut *chrome, "End")).expect("the caret moved");
        // Click a third of the way into the text: the caret moves to the nearest boundary.
        let x = f64::from(start.x) + f64::from(end.x - start.x) / 3.0;
        let actions = click(&mut *chrome, LINE, (x, 14.0));
        let at = caret(&actions).expect("the caret moved");
        assert!(
            (f64::from(at.x) - x).abs() < 8.0,
            "{} for a click at {x}",
            at.x
        );
        assert!(start.x < at.x && at.x < end.x);
        let image = paint(&mut *chrome, LINE, scale);
        let ratio = f64::from(image.width) / 800.0;
        let middle = ((f64::from(at.y) + f64::from(at.height) / 2.0) * ratio) as u32;
        // The caret: a column of text-colored pixels as tall as the field's line, starting
        // where the input method is told it is.
        let left = (f64::from(at.x) * ratio).round() as u32;
        let column = (left.saturating_sub(1)..=left + 1).find(|&x| {
            let rows = (f64::from(at.y) * ratio).ceil() as u32
                ..(f64::from(at.y + at.height as i32) * ratio).floor() as u32;
            rows.clone().all(|y| image.pixel(x, y) == text_color)
        });
        assert!(
            column.is_some(),
            "at {scale}/120: no caret drawn at {left} (row {middle}: {:?})",
            image.pixel(left, middle)
        );
    }
}

/// The line unfocused (place, indicator, decoded address, loading mark, status) at 100 %,
/// typing at 150 %, a failed command at 200 % with the line at the bottom, and the home page.
#[test]
fn golden_images() {
    let mut chrome = line(Table::new(), 700, 400);
    chrome.handle(ChromeEvent::Snapshot(five_tabs()));
    golden("line-100", &paint(&mut *chrome, LINE, 120));

    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    type_text(&mut *chrome, "https://exempel.se/sök?q=blåbär");
    golden("typing-150", &paint(&mut *chrome, LINE, 180));

    let bottom = Table::from([("position".to_owned(), Value::String("bottom".into()))]);
    let mut chrome = line(bottom, 500, 400);
    chrome.handle(ChromeEvent::Snapshot(five_tabs()));
    chrome.handle(ChromeEvent::Focus(KeyboardFocus::Chrome));
    type_text(&mut *chrome, ":tab.explode");
    named(&mut *chrome, "Enter");
    golden("error-200", &paint(&mut *chrome, LINE, 240));

    let mut chrome = line(Table::new(), 700, 400);
    chrome.handle(ChromeEvent::Snapshot(showing(
        "sig://home",
        TabContent::Internal,
    )));
    golden("home-100", &paint(&mut *chrome, PAGE, 120));
}
