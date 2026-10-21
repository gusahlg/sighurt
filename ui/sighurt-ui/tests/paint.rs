//! The CPU painter: golden images of chrome states at 100 %, 125 %, 150 % and 200 %, and the
//! buffer rules (whole repaints, strides with padding, sizes).
//!
//! Golden images are in tests/golden/. They use the bundled test font, so they don't depend on
//! the machine's fonts. After an intended change of the look, regenerate them with
//! `SIGHURT_BLESS=1 cargo test -p sighurt-ui --test paint` and look at every changed image.

mod common;

use std::path::PathBuf;

use common::*;
use sighurt_api::geometry::{SurfacePoint, WindowPoint, WindowRect};
use sighurt_api::input::{
    ButtonState, FocusEvent, Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers, PointerEvent,
    Preedit, TextInputEvent,
};
use sighurt_ui::chrome::{CHROME, PAGE, STATUS, TABS};
use sighurt_ui::layout::{Status, Tabs};
use sighurt_ui::{Colors, CpuPainter, LayoutOptions, Model, PaintError, PixelBuffer, TabContent};
use tiny_skia::{IntSize, Pixmap};

fn key(key: Key, text: Option<&str>) -> KeyEvent {
    KeyEvent {
        code: KeyCode(30),
        key,
        state: ButtonState::Pressed,
        repeat: false,
        modifiers: Modifiers::default(),
        consumed: Modifiers::default(),
        text: text.map(Into::into),
    }
}

fn type_text(chrome: &mut Model, text: &str) {
    for c in text.chars() {
        let c = c.to_string();
        chrome.key(&key(Key::Character(c.clone()), Some(&c)));
    }
}

fn hover(chrome: &mut Model, rect: WindowRect) {
    let surface = chrome.layout().chrome;
    let at = WindowPoint {
        x: f64::from(rect.x) + f64::from(rect.width) / 2.0,
        y: f64::from(rect.y) + f64::from(rect.height) / 2.0,
    };
    chrome.pointer(
        surface,
        &PointerEvent::Motion {
            position: SurfacePoint { x: at.x, y: at.y },
            modifiers: Modifiers::default(),
        },
    );
}

fn focused(width: u32) -> Model {
    let mut chrome = chrome(width, 600, three_tabs());
    chrome.window_focus(FocusEvent::Activated);
    chrome.set_focus(KeyboardFocus::Chrome);
    chrome
}

/// Three tabs, the second loading, a hovered reload button and a disabled back button.
#[test]
fn golden_tabs() {
    let mut chrome = chrome(900, 600, three_tabs());
    let reload = chrome.layout().reload;
    hover(&mut chrome, reload);
    golden("tabs-100", &paint(&mut chrome, CHROME, 120));
    golden("tabs-200", &paint(&mut chrome, CHROME, 240));
}

/// The address line focused with everything selected, as after location.focus.
#[test]
fn golden_selected_address() {
    let mut chrome = focused(700);
    golden("selected-125", &paint(&mut chrome, CHROME, 150));
}

/// Typing: Swedish letters, the caret after them, then an input method's preedit.
#[test]
fn golden_typing() {
    let mut chrome = focused(700);
    type_text(&mut chrome, "räksmörgås och blåbär");
    golden("typing-150", &paint(&mut chrome, CHROME, 180));
    chrome.text_input(&TextInputEvent {
        preedit: Some(Preedit {
            text: "ö".into(),
            cursor: Some(2..2),
        }),
        ..TextInputEvent::default()
    });
    golden("preedit-150", &paint(&mut chrome, CHROME, 180));
}

/// A long address scrolled so its caret stays visible, in a narrow window.
#[test]
fn golden_scrolled_address() {
    let mut chrome = focused(420);
    type_text(
        &mut chrome,
        "https://exempel.se/en/mycket/lång/adress?med=många&delar=ö",
    );
    golden("scrolled-125", &paint(&mut chrome, CHROME, 150));
}

/// The status strip: a hovered link, and a failed command.
#[test]
fn golden_status() {
    let mut chrome = chrome(700, 300, three_tabs());
    golden("status-150", &paint(&mut chrome, STATUS, 180));
    let mut chrome = focused(700);
    type_text(&mut chrome, ":tab.explode");
    chrome.key(&key(Key::Named("Enter".into()), None));
    assert!(chrome.state().command_error.is_some());
    golden("status-error-100", &paint(&mut chrome, STATUS, 120));
    golden("command-error-100", &paint(&mut chrome, CHROME, 120));
}

/// sig://home and an error page, which the chrome draws in the page area.
#[test]
fn golden_pages() {
    let mut chrome = chrome(800, 600, showing("sig://home", TabContent::Internal));
    golden("home-100", &paint(&mut chrome, PAGE, 120));
    let message = "The server at unknown.example could not be found.".to_owned();
    let error = showing("https://unknown.example/", TabContent::Error(message));
    let mut chrome = common::chrome(640, 480, error);
    golden("error-125", &paint(&mut chrome, PAGE, 150));
}

/// `[chrome.standard]`: a compact strip and toolbar with a hovered close button and a keymap's
/// indicator in the address field, no buttons
/// with the line focused, the status overlay, and the tab list on the left with a loading and
/// a hovered tab.
#[test]
fn golden_options() {
    let default = LayoutOptions::default();
    let colors = Colors::default();
    let compact = LayoutOptions {
        compact: true,
        ..default
    };
    let mut snapshot = three_tabs();
    snapshot.keymap = Some("NORMAL g".into());
    let mut chrome = arranged(900, 600, snapshot, colors, compact);
    let close = chrome.layout().tabs[1].close.unwrap();
    hover(&mut chrome, close);
    golden("compact-100", &paint(&mut chrome, CHROME, 120));

    let bare = LayoutOptions {
        buttons: false,
        ..default
    };
    let mut chrome = arranged(700, 600, three_tabs(), colors, bare);
    chrome.window_focus(FocusEvent::Activated);
    chrome.set_focus(KeyboardFocus::Chrome);
    type_text(&mut chrome, "blåbär");
    golden("no-buttons-125", &paint(&mut chrome, CHROME, 150));

    let overlay = LayoutOptions {
        status: Status::Overlay,
        ..default
    };
    let mut chrome = arranged(700, 400, three_tabs(), colors, overlay);
    golden("overlay-150", &paint(&mut chrome, STATUS, 180));

    let left = LayoutOptions {
        tabs: Tabs::Left,
        ..default
    };
    let mut chrome = arranged(700, 400, three_tabs(), colors, left);
    let tabs = chrome.layout().tab_list;
    let second = chrome.layout().tabs[1].rect;
    let at = WindowPoint {
        x: f64::from(second.x) + 20.0,
        y: f64::from(second.y) + 10.0,
    };
    chrome.pointer(
        tabs,
        &PointerEvent::Motion {
            position: SurfacePoint {
                x: at.x - f64::from(tabs.x),
                y: at.y - f64::from(tabs.y),
            },
            modifiers: Modifiers::default(),
        },
    );
    golden("tabs-left-125", &paint(&mut chrome, TABS, 150));
}

/// `[ui] font-size = 18`: the strip, toolbar and text 18/14 as big, and the home page's text.
#[test]
fn golden_font_size_18() {
    let options = LayoutOptions {
        font_size: 18,
        ..LayoutOptions::default()
    };
    let mut chrome = arranged(900, 600, three_tabs(), Colors::default(), options);
    golden("font-18-100", &paint(&mut chrome, CHROME, 120));
    let home = showing("sig://home", TabContent::Internal);
    let mut chrome = arranged(800, 600, home, Colors::default(), options);
    golden("font-18-home-100", &paint(&mut chrome, PAGE, 120));
}

/// Engine labels: in each engine page's tab (a Servo page, a loading WPE page, none on the home
/// page's) and at the address field's right end, at the top at 100 % and 200 %, compact beside
/// a keymap's indicator at 150 %, at 18 px, in the list on the left at 125 %, and in tabs too
/// narrow for a whole label at 150 %.
#[test]
fn golden_engine_labels() {
    let default = LayoutOptions::default();
    let colors = Colors::default();
    let snapshot = || labelled(three_tabs(), &["servo", "wpe", "servo"]);
    let mut chrome = common::chrome(900, 600, snapshot());
    golden("engine-tabs-100", &paint(&mut chrome, CHROME, 120));
    golden("engine-tabs-200", &paint(&mut chrome, CHROME, 240));

    let compact = LayoutOptions {
        compact: true,
        ..default
    };
    let mut indicated = snapshot();
    indicated.keymap = Some("NORMAL".into());
    let mut chrome = arranged(700, 600, indicated, colors, compact);
    golden("engine-compact-150", &paint(&mut chrome, CHROME, 180));

    let large = LayoutOptions {
        font_size: 18,
        ..default
    };
    let mut chrome = arranged(900, 600, snapshot(), colors, large);
    golden("engine-font-18-100", &paint(&mut chrome, CHROME, 120));

    let left = LayoutOptions {
        tabs: Tabs::Left,
        ..default
    };
    let mut chrome = arranged(700, 400, snapshot(), colors, left);
    golden("engine-left-125", &paint(&mut chrome, TABS, 150));

    // Six tabs: the first two again, and a Servo page whose short title doesn't earn it a label.
    let mut many = snapshot();
    for i in 0..2 {
        let mut again = many.tabs[i].clone();
        again.id = sighurt_api::ids::TabId::next();
        again.active = false;
        many.tabs.push(again);
    }
    let mut short = tab("A", "https://a.example/", TabContent::Engine);
    short.engine = Some("servo".into());
    many.tabs.push(short);
    let mut chrome = common::chrome(600, 400, many);
    golden("engine-narrow-150", &paint(&mut chrome, CHROME, 180));
}

/// Every paint covers the whole buffer: a buffer holding an older frame or garbage comes out
/// the same as a fresh one, as alternating SHM buffers need.
#[test]
fn paints_cover_the_whole_buffer() {
    let mut chrome = chrome(640, 480, three_tabs());
    let rect = chrome.layout().chrome;
    let fresh = paint(&mut chrome, CHROME, 150);
    let metrics = metrics(rect, 150);
    let size = metrics.buffer();
    let mut data: Vec<u8> = (0..size.width * size.height * 4)
        .map(|i| (i * 7) as u8)
        .collect();
    let buffer = PixelBuffer {
        data: &mut data,
        width: size.width,
        height: size.height,
        stride: size.width as usize * 4,
    };
    CpuPainter::new()
        .paint(&mut chrome, CHROME, &metrics, buffer)
        .unwrap();
    assert!(argb_to_rgba(&data) == fresh.rgba, "garbage showed through");
}

/// Rows with padding get the same pixels, and the padding is left alone.
#[test]
fn padded_strides() {
    let mut chrome = chrome(333, 200, three_tabs());
    let rect = chrome.layout().chrome;
    for scale in SCALES {
        let packed = paint(&mut chrome, CHROME, scale);
        let metrics = metrics(rect, scale);
        let size = metrics.buffer();
        let row = size.width as usize * 4;
        let stride = row + 12;
        // The last row needs no padding after it.
        let mut data = vec![0xab; stride * (size.height as usize - 1) + row];
        let buffer = PixelBuffer {
            data: &mut data,
            width: size.width,
            height: size.height,
            stride,
        };
        let mut painter = CpuPainter::new();
        painter
            .paint(&mut chrome, CHROME, &metrics, buffer)
            .unwrap();
        for (y, line) in data.chunks(stride).enumerate() {
            let expected = &packed.rgba[y * row..(y + 1) * row];
            assert!(
                argb_to_rgba(&line[..row]) == expected,
                "row {y} at {scale}/120"
            );
            assert!(line[row..].iter().all(|&b| b == 0xab), "padding of row {y}");
        }
    }
}

#[test]
fn bad_buffers_are_refused() {
    let mut chrome = chrome(640, 480, three_tabs());
    let metrics = metrics(chrome.layout().chrome, 150);
    let size = metrics.buffer();
    let mut data = vec![0; size.width as usize * size.height as usize * 4];
    let mut painter = CpuPainter::new();
    let wrong_size = PixelBuffer {
        data: &mut data,
        width: size.width - 1,
        height: size.height,
        stride: size.width as usize * 4,
    };
    let result = painter.paint(&mut chrome, CHROME, &metrics, wrong_size);
    assert!(
        matches!(result, Err(PaintError::WrongSize { .. })),
        "{result:?}"
    );
    let short_stride = PixelBuffer {
        data: &mut data,
        width: size.width,
        height: size.height,
        stride: 4,
    };
    let result = painter.paint(&mut chrome, CHROME, &metrics, short_stride);
    assert_eq!(result, Err(PaintError::WrongLayout));
    let len = data.len();
    let short_memory = PixelBuffer {
        data: &mut data[..len - 1],
        width: size.width,
        height: size.height,
        stride: size.width as usize * 4,
    };
    let result = painter.paint(&mut chrome, CHROME, &metrics, short_memory);
    assert_eq!(result, Err(PaintError::WrongLayout));
    // An empty target paints nothing, successfully.
    let empty = self::metrics(WindowRect::default(), 150);
    let nothing = PixelBuffer {
        data: &mut [],
        width: 0,
        height: 0,
        stride: 0,
    };
    assert_eq!(painter.paint(&mut chrome, CHROME, &empty, nothing), Ok(()));
}

/// Odd window sizes paint at every scale without trouble, and the chrome is opaque.
#[test]
fn odd_sizes_paint() {
    for (width, height) in [(1, 1), (37, 39), (101, 85), (157, 104), (641, 481)] {
        for scale in SCALES {
            let mut chrome = chrome(width, height, three_tabs());
            for surface in [CHROME, STATUS, PAGE] {
                if rect_of(&chrome, surface).size().is_empty() {
                    continue;
                }
                let image = paint(&mut chrome, surface, scale);
                assert!(image
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[3] == 255));
            }
        }
    }
}

/// Edges fall on pixel boundaries at every scale: a column through the strips' borders and a
/// row across the address field's left edge hold only the exact background, border and field
/// colors, never a blend of two.
#[test]
fn edges_are_crisp_at_every_scale() {
    let colors = sighurt_ui::Colors::default();
    let exact = |color: sighurt_ui::Rgba| [color.r, color.g, color.b, 255];
    for scale in SCALES {
        for width in [640, 1001] {
            let mut chrome = chrome(width, 480, three_tabs());
            let layout = chrome.layout().clone();
            let image = paint(&mut chrome, CHROME, scale);
            let ratio = f64::from(image.width) / f64::from(width);
            let at = |logical: f64| (logical * ratio) as u32;
            // Left of the first tab and the back button: background and the two border lines.
            let column = at(2.0);
            let mut border_rows = 0;
            for y in 0..image.height {
                let pixel = image.pixel(column, y);
                assert!(
                    pixel == exact(colors.bg) || pixel == exact(colors.border),
                    "{width} at {scale}/120: pixel ({column}, {y}) is {pixel:?}"
                );
                border_rows += u32::from(pixel == exact(colors.border));
            }
            let line = (f64::from(scale) / 120.0).round().max(1.0) as u32;
            assert!(
                border_rows >= 2 && border_rows <= 2 * (line + 1),
                "{border_rows} rows"
            );
            // Across the address field's left edge, mid-height.
            let field = layout.address;
            let row = at(f64::from(field.y) + f64::from(field.height) / 2.0);
            let (from, to) = (at(f64::from(field.x) - 3.0), at(f64::from(field.x) + 8.0));
            let allowed = [
                exact(colors.bg),
                exact(colors.border),
                exact(colors.surface),
            ];
            for x in from..to {
                let pixel = image.pixel(x, row);
                assert!(
                    allowed.contains(&pixel),
                    "{width} at {scale}/120: ({x}, {row}) {pixel:?}"
                );
            }
        }
    }
}

/// Paint timings with this machine's fonts, and images to look at in the target directory:
/// `cargo test -p sighurt-ui --release --test paint -- --ignored --nocapture`. Ignored because
/// it depends on the installed fonts and says nothing when it passes.
#[test]
#[ignore]
fn measure_with_system_fonts() {
    use std::time::Instant;

    use sighurt_api::geometry::LogicalSize;
    use sighurt_ui::Fonts;

    let start = Instant::now();
    let fonts = Fonts::system();
    println!(
        "Fonts::system: {:?}, family {:?}",
        start.elapsed(),
        fonts.family()
    );
    let mut chrome = Model::new(fonts, Colors::default(), LayoutOptions::default());
    chrome.resize(LogicalSize {
        width: 1920,
        height: 1080,
    });
    let mut snapshot = three_tabs();
    snapshot.tabs[1].title = "Emoji 👩‍👩‍👧‍👦 🇸🇪 och 日本語のタイトル".into();
    chrome.set_snapshot(snapshot.clone());
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    for scale in SCALES {
        let mut painter = CpuPainter::new();
        let rect = chrome.layout().chrome;
        let metrics = metrics(rect, scale);
        let size = metrics.buffer();
        let mut data = vec![0u8; size.width as usize * size.height as usize * 4];
        let mut time = |chrome: &mut Model, data: &mut Vec<u8>| {
            let start = Instant::now();
            let buffer = PixelBuffer {
                data,
                width: size.width,
                height: size.height,
                stride: size.width as usize * 4,
            };
            painter.paint(chrome, CHROME, &metrics, buffer).unwrap();
            start.elapsed()
        };
        let cold = time(&mut chrome, &mut data);
        let warm = time(&mut chrome, &mut data);
        let (w, h) = (size.width, size.height);
        println!("chrome 1920 wide at {scale}/120 ({w}×{h}): cold {cold:?}, warm {warm:?}");
        let image = Image {
            width: size.width,
            height: size.height,
            rgba: argb_to_rgba(&data),
        };
        let pixmap = Pixmap::from_vec(image.rgba, IntSize::from_wh(w, h).unwrap()).unwrap();
        pixmap
            .save_png(out.join(format!("system-{scale}.png")))
            .unwrap();
    }
    let mut home = snapshot;
    home.tabs[0].active = false;
    home.tabs[2].active = true;
    chrome.set_snapshot(home);
    let start = Instant::now();
    let image = paint(&mut chrome, PAGE, 240);
    println!(
        "home page at 200 % ({}×{}): {:?}",
        image.width,
        image.height,
        start.elapsed()
    );
    let size = IntSize::from_wh(image.width, image.height).unwrap();
    let pixmap = Pixmap::from_vec(image.rgba, size).unwrap();
    pixmap.save_png(out.join("system-home-240.png")).unwrap();
    println!("images in {}", out.display());
}
