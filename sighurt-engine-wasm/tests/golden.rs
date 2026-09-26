//! Golden images of the rasterizer: example guests painted at fixed sizes and scales.
//!
//! Each test writes what it painted to `target/tmp/golden/<name>.png`, checks a few pixels whose
//! colour doesn't depend on fonts, and compares the whole picture with `tests/golden/<name>.png`
//! when this machine resolves the default font to the family the goldens were made with
//! (`tests/golden/font`); text looks different in another font. `SIGHURT_BLESS=1` rewrites the
//! goldens from this machine.
//!
//! Needs the example guests built:
//! `tools/cargo build --target wasm32-unknown-unknown --release -p gradient-demo -p typography-demo`.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{example, is_frame_ready, Host, Shown};
use sighurt_api::engine::{EngineCommand, PresentOutcome};

/// Paints `example` in a `width` × `height` view at `scale` / 120 and returns the frame and
/// how long painting it took.
fn paint(name: &str, width: u32, height: u32, scale: u32) -> (Shown, Duration) {
    let mut host = Host::new();
    let (view, shown) = host.open(width, height, scale);
    host.navigate(view, &example(name));
    host.until(Duration::from_secs(20), is_frame_ready(view));
    // A few frames, so the guest has settled on its layout for this size.
    host.run_for(Duration::from_millis(100));
    let started = Instant::now();
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    (shown, started.elapsed())
}

fn out_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// `[b, g, r, a]` of pixel (`x`, `y`) is within `tolerance` of `0xRRGGBB`, opaque.
fn assert_near(shown: &Shown, (x, y): (u32, u32), rgb: u32, tolerance: u8, what: &str) {
    let [_, r, g, b] = rgb.to_be_bytes();
    let got = shown.pixel(x, y);
    let near = got[..3]
        .iter()
        .zip([b, g, r])
        .all(|(&got, want)| got.abs_diff(want) <= tolerance);
    assert!(
        near && got[3] == 255,
        "{what} at ({x}, {y}): {got:?}, want {rgb:06x}"
    );
}

/// Saves the frame and compares it with its golden image, if the fonts match.
fn check_golden(name: &str, shown: &Shown) {
    let out = out_dir().join(format!("{name}.png"));
    shown.save_png(&out);
    let golden = golden_dir().join(format!("{name}.png"));
    let font = sighurt_engine_wasm::text::default_family().unwrap_or_default();
    let font_file = golden_dir().join("font");
    if std::env::var_os("SIGHURT_BLESS").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::copy(&out, &golden).unwrap();
        std::fs::write(&font_file, format!("{font}\n")).unwrap();
        return;
    }
    let recorded = std::fs::read_to_string(&font_file).unwrap_or_default();
    if recorded.trim() != font || !golden.is_file() {
        eprintln!(
            "{name}: not compared with {}: the goldens use {:?}, this machine's default font \
             is {font:?}. What was painted is in {}.",
            golden.display(),
            recorded.trim(),
            out.display()
        );
        return;
    }
    let want = image::open(&golden).unwrap().into_rgba8();
    let got = image::open(&out).unwrap().into_rgba8();
    assert_eq!(want.dimensions(), got.dimensions(), "{name}: size");
    let differ = want
        .pixels()
        .zip(got.pixels())
        .filter(|(a, b)| a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 8))
        .count();
    let total = (want.width() * want.height()) as usize;
    assert!(
        differ * 1000 <= total,
        "{name}: {differ} of {total} pixels differ from {}; see {}",
        golden.display(),
        out.display()
    );
}

#[test]
fn gradient_demo() {
    let (shown, took) = paint("gradient-demo", 800, 600, 120);
    eprintln!("gradient-demo 800x600 @1: painted in {took:?}");
    // The backdrop's middle stop, in the margin left of the cards.
    assert_near(&shown, (5, 300), 0x3b2670, 12, "backdrop");
    // The first card's gradient starts in its top-left corner.
    assert_near(&shown, (24, 112), 0xff7e5f, 16, "first card");
    // The last card ("Emerald") ends in its bottom-right corner.
    assert_near(&shown, (775, 560), 0xf5fd60, 24, "last card");
    check_golden("gradient-demo", &shown);
}

#[test]
fn typography_demo() {
    let (shown, took) = paint("typography-demo", 800, 600, 120);
    eprintln!("typography-demo 800x600 @1: painted in {took:?}");
    assert_near(&shown, (5, 590), 0x181820, 0, "background");
    // The title is light text centred at the top.
    let (size, pixels) = shown.pixels();
    let light = |y0: u32, y1: u32| {
        (y0..y1)
            .flat_map(|y| (0..size.width).map(move |x| (x, y)))
            .filter(|&(x, y)| pixels[4 * (y * size.width + x) as usize + 1] > 200)
            .count()
    };
    assert!(
        light(20, 55) > 300,
        "the title: {} light pixels",
        light(20, 55)
    );
    check_golden("typography-demo", &shown);
}

#[test]
fn typography_demo_at_150_percent() {
    let (shown, took) = paint("typography-demo", 800, 600, 180);
    eprintln!("typography-demo 800x600 @1.5: painted in {took:?}");
    assert_eq!(shown.pixels().0.width, 1200);
    assert_near(&shown, (5, 890), 0x181820, 0, "background");
    check_golden("typography-demo@1.5", &shown);
}

/// How long painting takes at a common desktop size, for the record: the first frame, whose
/// glyphs aren't rasterized yet, and the same frame again.
#[test]
fn painting_a_large_page() {
    for name in ["hello-sighurt", "gradient-demo", "typography-demo"] {
        let mut host = Host::new();
        let (view, _) = host.open(1920, 1080, 120);
        host.navigate(view, &example(name));
        host.until(Duration::from_secs(20), is_frame_ready(view));
        host.run_for(Duration::from_millis(100));
        let mut times = Vec::new();
        for _ in 0..3 {
            // A new layout epoch at the same size: the same picture, painted again.
            host.command(
                view,
                EngineCommand::SetMetrics(common::metrics(1920, 1080, 120)),
            );
            let started = Instant::now();
            assert_eq!(host.present(view), PresentOutcome::Submitted);
            times.push(started.elapsed());
        }
        eprintln!("{name} 1920x1080 @1: painted in {times:?}");
        assert!(
            times.iter().all(|t| *t < Duration::from_secs(1)),
            "{name}: {times:?}"
        );
    }
}
