//! The cost of showing a page frame in a terminal: the kitty and sixel encodes of a page-sized
//! frame, in a release build (bench/HYPOTHESES.md H-TUI*). Ignored: a timing, run by hand,
//!
//!   tools/cargo test --release -p sighurt-tui --test encode_cost -- --ignored --nocapture
//!
//! with `SIG_TUI_FRAMES` naming a directory of `NAME-WxH.bgra` frames (premultiplied B, G, R,
//! A, rows packed; bench/tui/png2bgra.py makes them from screenshots). Without it, a synthetic
//! frame of text-like stripes is used. Prints the median of 30 encodes after 3 warm-up ones, and
//! the bytes each encode writes; with `SIG_TUI_DUMP` naming a directory, writes them there too
//! (`NAME.kitty`, `NAME.sixel`), to compare two builds' output byte for byte.

use std::time::{Duration, Instant};

use sighurt_api::geometry::BufferSize;
use sighurt_api::present::Frame;
use sighurt_tui::kitty::{self, CellRect};
use sighurt_tui::sixel;

fn frames() -> Vec<(String, u32, u32, Vec<u8>)> {
    let Some(dir) = std::env::var_os("SIG_TUI_FRAMES") else {
        let (width, height) = (1280u32, 696u32);
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for (i, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i as u32 % width, i as u32 / width);
            let ink = (y % 20 < 12) && (x / 3 + y) % 7 < 3;
            let value = if ink { 40 } else { 236 };
            pixel.copy_from_slice(&[value, value, value, 255]);
        }
        return vec![("synthetic".into(), width, height, pixels)];
    };
    let mut frames = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let Some((label, size)) = name.rsplit_once('-') else {
            continue;
        };
        let (width, height) = size.split_once('x').unwrap();
        let (width, height) = (width.parse().unwrap(), height.parse().unwrap());
        frames.push((
            label.to_owned(),
            width,
            height,
            std::fs::read(&path).unwrap(),
        ));
    }
    frames.sort();
    frames
}

fn median(mut times: Vec<Duration>) -> f64 {
    times.sort_unstable();
    times[times.len() / 2].as_secs_f64() * 1e6
}

#[test]
#[ignore = "a timing, run by hand"]
fn encode_cost() {
    // A 160×35-cell page area of 8×20-pixel cells holds the 1280×696 frame.
    let place = CellRect {
        column: 0,
        row: 2,
        columns: 160,
        rows: 35,
    };
    for (name, width, height, mut pixels) in frames() {
        let stride = width as usize * 4;
        let frame = Frame {
            pixels: &mut pixels,
            size: BufferSize { width, height },
            stride,
        };
        let (mut kitty_times, mut sixel_times) = (Vec::new(), Vec::new());
        let (mut kitty_bytes, mut sixel_bytes) = (0, 0);
        for round in 0..33 {
            let start = Instant::now();
            let bytes = kitty::encode(&frame, 1, place, 64 << 20).unwrap();
            let kitty = start.elapsed();
            kitty_bytes = bytes.len();
            let start = Instant::now();
            let bytes = sixel::encode(&frame, place, 8, 20, 64 << 20).unwrap();
            let sixel = start.elapsed();
            sixel_bytes = bytes.len();
            if round >= 3 {
                kitty_times.push(kitty);
                sixel_times.push(sixel);
            }
        }
        if let Some(dir) = std::env::var_os("SIG_TUI_DUMP") {
            let dir = std::path::Path::new(&dir);
            let kitty = kitty::encode(&frame, 1, place, 64 << 20).unwrap();
            std::fs::write(dir.join(format!("{name}.kitty")), kitty).unwrap();
            let sixel = sixel::encode(&frame, place, 8, 20, 64 << 20).unwrap();
            std::fs::write(dir.join(format!("{name}.sixel")), sixel).unwrap();
        }
        println!(
            "encode_cost {name} {width}x{height}: kitty {:.0} us {kitty_bytes} bytes, sixel {:.0} us {sixel_bytes} bytes",
            median(kitty_times),
            median(sixel_times)
        );
    }
}
