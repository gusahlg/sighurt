//! The CPU backend from outside the crate: scenes built with a `Pen` and rendered into a
//! wl_shm-style buffer. The chromes' golden images (sighurt-ui, sighurt-chrome-line) cover the
//! drawing of whole chromes; these cover the backend's own rules.

use sighurt_api::geometry::{Metrics, Scale, WindowRect};
use sighurt_api::ids::LayoutEpoch;
use sighurt_render::{
    Backend, CpuBackend, FontSystemHandle, Fonts, Output, Pen, PixelBuffer, Presents, RenderError,
    RenderOutcome, Rgba, Scene, SurfaceKey, TextStyle,
};

const FONT: &[u8] = include_bytes!("fonts/Cantarell-Regular.ttf");

const RECT: WindowRect = WindowRect {
    x: 10,
    y: 20,
    width: 40,
    height: 30,
};

const BG: Rgba = Rgba::rgb(0x0a0a0b);
const RED: Rgba = Rgba::rgb(0xff0000);

fn fonts() -> FontSystemHandle {
    Fonts::from_data([FONT.to_vec()]).handle()
}

fn metrics(scale: u32) -> Metrics {
    Metrics::new(
        LayoutEpoch::next(),
        RECT,
        Scale::from_fractional(scale).unwrap(),
    )
}

/// A rendered buffer as RGBA rows.
struct Image {
    width: u32,
    rgba: Vec<u8>,
}

impl Image {
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y * self.width + x) as usize * 4;
        self.rgba[i..i + 4].try_into().unwrap()
    }
}

fn render(backend: &mut CpuBackend, scene: &Scene, metrics: &Metrics) -> (RenderOutcome, Image) {
    let size = metrics.buffer();
    let mut data = vec![0u8; size.width as usize * size.height as usize * 4];
    let buffer = PixelBuffer {
        data: &mut data,
        width: size.width,
        height: size.height,
        stride: size.width as usize * 4,
    };
    let rendered = backend.render(SurfaceKey(7), metrics, scene, Output::Pixels(buffer));
    // ARGB8888 little-endian (B, G, R, A) to R, G, B, A.
    let rgba = data
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[b, g, r, a]| [r, g, b, a])
        .collect();
    let image = Image {
        width: size.width,
        rgba,
    };
    (rendered.unwrap(), image)
}

fn opaque(color: Rgba) -> [u8; 4] {
    [color.r, color.g, color.b, 255]
}

/// A rectangle covers exactly the buffer pixels whose centres it covers, at every scale, and
/// the surface is cleared opaque whatever alpha the clear color had.
#[test]
fn rectangles_cover_whole_pixels() {
    let mut backend = CpuBackend::new(fonts());
    assert_eq!(
        (backend.name(), backend.presents()),
        ("cpu", Presents::Pixels)
    );
    for scale in [120, 150, 180, 240] {
        let metrics = metrics(scale);
        let mut pen = Pen::new(metrics.rect());
        pen.clear(Rgba { a: 0, ..BG });
        pen.fill([15.0, 25.0, 10.0, 5.0], RED);
        let (rendered, image) = render(&mut backend, &pen.finish(), &metrics);
        assert_eq!(rendered, RenderOutcome::Submitted);
        let size = metrics.buffer();
        let rx = size.width as f32 / RECT.width as f32;
        let ry = size.height as f32 / RECT.height as f32;
        for y in 0..size.height {
            for x in 0..size.width {
                let (cx, cy) = ((x as f32 + 0.5) / rx, (y as f32 + 0.5) / ry);
                let inside = (5.0..15.0).contains(&cx) && (5.0..10.0).contains(&cy);
                let expected = if inside { opaque(RED) } else { opaque(BG) };
                assert_eq!(image.pixel(x, y), expected, "({x}, {y}) at {scale}/120");
            }
        }
    }
}

/// A scene shaped with another font system is refused: its font ids mean other fonts there.
#[test]
fn text_from_another_font_system_is_refused() {
    let mut backend = CpuBackend::new(fonts());
    let metrics = metrics(120);
    let shaped = Fonts::new(fonts()).line("Blåbär", TextStyle::SM);
    let mut pen = Pen::new(metrics.rect());
    pen.text(&shaped, (10.0, 20.0), RED, [10.0, 20.0, 40.0, 30.0]);
    let size = metrics.buffer();
    let mut data = vec![0u8; size.width as usize * size.height as usize * 4];
    let buffer = PixelBuffer {
        data: &mut data,
        width: size.width,
        height: size.height,
        stride: size.width as usize * 4,
    };
    let rendered = backend.render(
        SurfaceKey(3),
        &metrics,
        &pen.finish(),
        Output::Pixels(buffer),
    );
    assert!(
        matches!(rendered, Err(RenderError::Output(_))),
        "{rendered:?}"
    );
}

/// An item's clip cuts it, shapes and text alike.
#[test]
fn clips_cut_items() {
    let system = fonts();
    let mut backend = CpuBackend::new(system.clone());
    let metrics = metrics(120);
    let mut pen = Pen::new(metrics.rect());
    pen.clear(BG);
    pen.fill([10.0, 20.0, 40.0, 30.0], RED);
    let mut scene = pen.finish();
    scene.items[0].clip = Some([10.0, 20.0, 20.0, 30.0].into());
    let (_, image) = render(&mut backend, &scene, &metrics);
    assert_eq!(image.pixel(5, 5), opaque(RED));
    assert_eq!(image.pixel(25, 5), opaque(BG));

    let shaped = Fonts::new(system).line("Blåbär", TextStyle::SM);
    let mut pen = Pen::new(metrics.rect());
    pen.clear(BG);
    pen.text(
        &shaped,
        (10.0, 20.0),
        Rgba::rgb(0xffffff),
        [10.0, 20.0, 12.0, 30.0],
    );
    let (_, image) = render(&mut backend, &pen.finish(), &metrics);
    let drawn =
        |from: u32, to: u32| (from..to).any(|x| (0..30).any(|y| image.pixel(x, y) != opaque(BG)));
    assert!(drawn(0, 12), "the text shows inside its clip");
    assert!(!drawn(12, 40), "and nothing of it outside");
}

/// The same scene at the same metrics is the frame the surface shows: nothing is rendered until
/// the scene, the metrics or the surface's binding change.
#[test]
fn an_unchanged_scene_renders_nothing() {
    let mut backend = CpuBackend::new(fonts());
    let metrics = metrics(150);
    let mut pen = Pen::new(metrics.rect());
    pen.clear(BG);
    pen.bordered([12.0, 22.0, 30.0, 20.0], 6.0, BG, RED);
    let scene = pen.finish();
    assert_eq!(
        render(&mut backend, &scene, &metrics).0,
        RenderOutcome::Submitted
    );
    let (again, image) = render(&mut backend, &scene, &metrics);
    assert_eq!(again, RenderOutcome::Unchanged);
    assert!(
        image.rgba.iter().all(|&b| b == 0),
        "the buffer is untouched"
    );
    let relaid = Metrics::new(LayoutEpoch::next(), RECT, metrics.scale());
    assert_eq!(
        render(&mut backend, &scene, &relaid).0,
        RenderOutcome::Submitted
    );
    backend.unbind(SurfaceKey(7));
    assert_eq!(
        render(&mut backend, &scene, &relaid).0,
        RenderOutcome::Submitted
    );
    let mut changed = scene.clone();
    changed.clear = RED;
    assert_eq!(
        render(&mut backend, &changed, &relaid).0,
        RenderOutcome::Submitted
    );
    let stats = backend.stats();
    assert_eq!((stats.frames, stats.unchanged), (4, 1));
}

/// It renders into pixel memory only.
#[test]
fn native_output_is_refused() {
    let mut backend = CpuBackend::new(fonts());
    let metrics = metrics(120);
    let scene = Pen::new(metrics.rect()).finish();
    let rendered = backend.render(SurfaceKey(1), &metrics, &scene, Output::Native);
    assert!(
        matches!(rendered, Err(RenderError::Output(_))),
        "{rendered:?}"
    );
}
