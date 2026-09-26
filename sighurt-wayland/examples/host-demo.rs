//! The Wayland host on its own: a window with a chrome strip and a page, both SHM test patterns,
//! that follows resizes and scale changes and logs its input.
//!
//! ```sh
//! cargo run -p sighurt-wayland --example host-demo
//! tools/headless-sway -- target/debug/examples/host-demo --exit-after 5
//! ```
//!
//! The page is a checkerboard of 16-logical-pixel squares drawn at buffer resolution, framed by
//! a one-buffer-pixel red border, so a screenshot shows whether buffer size, scale and position
//! are exact. The chrome lightens while hovered; clicking the chrome or the page gives it the
//! keyboard. Nothing is drawn while nothing changes.

use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use calloop::EventLoop;
use sighurt_api::geometry::{LogicalSize, Metrics, WindowRect};
use sighurt_api::ids::{TargetId, WindowId};
use sighurt_api::input::{ButtonState, InputKind, PointerEvent};
use sighurt_wayland::{
    Host, HostEvent, InputRegion, Placement, PresentMode, ShmFrame, TargetLease, TargetPlacement,
    WindowOptions,
};

const CHROME_HEIGHT: u32 = 40;

struct Demo {
    host: Host,
    window: WindowId,
    chrome: TargetLease,
    page: TargetLease,
    chrome_hovered: bool,
    frames: u32,
    running: bool,
}

impl Demo {
    fn name(&self, target: Option<TargetId>) -> &'static str {
        match target {
            Some(t) if t == self.chrome.id() => "chrome",
            Some(t) if t == self.page.id() => "page",
            Some(_) => "other",
            None => "background",
        }
    }

    /// The layout: the chrome strip on top, the page below, clamped to the window.
    fn place(&mut self, size: LogicalSize) {
        let chrome_height = CHROME_HEIGHT.min(size.height);
        let placement = Placement {
            window: self.window,
            size,
            targets: vec![
                TargetPlacement {
                    target: self.chrome.id(),
                    rect: WindowRect {
                        x: 0,
                        y: 0,
                        width: size.width,
                        height: chrome_height,
                    },
                    visible: true,
                    input: InputRegion::Whole,
                },
                TargetPlacement {
                    target: self.page.id(),
                    rect: WindowRect {
                        x: 0,
                        y: chrome_height as i32,
                        width: size.width,
                        height: size.height - chrome_height,
                    },
                    visible: true,
                    input: InputRegion::Whole,
                },
            ],
        };
        if let Err(error) = self.host.apply_placement(placement) {
            eprintln!("placement refused: {error}");
        }
    }

    fn handle(&mut self, event: HostEvent) {
        match event {
            HostEvent::WindowConfigured { size, scale, .. } => {
                println!(
                    "configured {}×{} at {}/120",
                    size.width,
                    size.height,
                    scale.numerator()
                );
                self.place(size);
            }
            HostEvent::CloseRequested { .. } => self.running = false,
            HostEvent::MetricsChanged { target, metrics } => {
                let (rect, buffer) = (metrics.rect(), metrics.buffer());
                println!(
                    "{} metrics: {}×{} at ({}, {}), buffer {}×{}, {:?}",
                    self.name(Some(target)),
                    rect.width,
                    rect.height,
                    rect.x,
                    rect.y,
                    buffer.width,
                    buffer.height,
                    metrics.epoch()
                );
            }
            HostEvent::Input { target, event, .. } => {
                println!("input to {}: {:?}", self.name(target), event.kind);
                if let InputKind::Pointer(pointer) = &event.kind {
                    let on_chrome = target == Some(self.chrome.id());
                    match pointer {
                        PointerEvent::Enter { .. } | PointerEvent::Leave if on_chrome => {
                            self.chrome_hovered = matches!(pointer, PointerEvent::Enter { .. });
                            self.host.invalidate(self.chrome.id());
                        }
                        PointerEvent::Button {
                            state: ButtonState::Pressed,
                            ..
                        } => self.host.set_keyboard_focus(self.window, target),
                        _ => {}
                    }
                }
            }
            HostEvent::ClipboardRead { request, result } => {
                println!("clipboard read {request:?}: {result:?}")
            }
            HostEvent::ConnectionLost { error } => {
                eprintln!("connection lost: {error}");
                self.running = false;
            }
        }
    }

    fn present(&mut self) {
        for permit in self.host.permits() {
            let chrome = permit.target() == self.chrome.id();
            match permit.shm_frame() {
                Ok(mut frame) => {
                    if chrome {
                        paint_chrome(&mut frame, self.chrome_hovered);
                    } else {
                        paint_page(&mut frame);
                    }
                    frame.submit();
                    self.frames += 1;
                }
                Err(error) => eprintln!("no frame: {error}"),
            }
        }
    }
}

/// An opaque ARGB8888 pixel: B, G, R, A bytes.
fn pixel(r: u8, g: u8, b: u8) -> [u8; 4] {
    [b, g, r, 0xff]
}

fn paint_chrome(frame: &mut ShmFrame, hovered: bool) {
    let size = frame.size();
    let body = if hovered {
        pixel(0x3a, 0x4a, 0x7a)
    } else {
        pixel(0x22, 0x2a, 0x44)
    };
    let line = pixel(0xe0, 0xe0, 0xe0);
    let stride = frame.stride();
    let pixels = frame.pixels();
    for (y, row) in pixels.chunks_exact_mut(stride).enumerate() {
        let color = if y + 1 == size.height as usize {
            line
        } else {
            body
        };
        for px in row.as_chunks_mut::<4>().0 {
            *px = color;
        }
    }
}

fn paint_page(frame: &mut ShmFrame) {
    let metrics: Metrics = frame.metrics();
    let size = frame.size();
    // 16 logical pixels in buffer pixels, from the actual rounded buffer size.
    let square = (16.0 * f64::from(size.width) / f64::from(metrics.rect().width.max(1))).max(1.0);
    let (light, dark, border) = (
        pixel(0xd8, 0xd8, 0xd0),
        pixel(0x98, 0x98, 0x90),
        pixel(0xff, 0x00, 0x00),
    );
    let (w, h) = (size.width as usize, size.height as usize);
    let stride = frame.stride();
    let pixels = frame.pixels();
    for (y, row) in pixels.chunks_exact_mut(stride).enumerate() {
        for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let color = if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                border
            } else if ((x as f64 / square) as u32 + (y as f64 / square) as u32).is_multiple_of(2) {
                light
            } else {
                dark
            };
            *px = color;
        }
    }
}

fn main() {
    let exit_after = std::env::args()
        .skip_while(|a| a != "--exit-after")
        .nth(1)
        .and_then(|s| s.parse::<f64>().ok());
    let mut event_loop: EventLoop<'static, Demo> = EventLoop::try_new().expect("an event loop");
    let handle = event_loop.handle();
    let mut host = match Host::new(&handle, |demo: &mut Demo| &mut demo.host) {
        Ok(host) => host,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    println!("capabilities: {:?}", host.capabilities());
    let window = WindowId::next();
    host.create_window(
        window,
        WindowOptions {
            title: "sighurt host demo".into(),
            app_id: "sighurt-host-demo".into(),
            ..WindowOptions::default()
        },
    );
    let chrome = host
        .create_target(window, PresentMode::Shm, None)
        .expect("the window exists");
    let page = host
        .create_target(window, PresentMode::Shm, None)
        .expect("the window exists");
    host.set_keyboard_focus(window, Some(page.id()));
    if let Some(seconds) = exit_after {
        let timer = Timer::from_duration(Duration::from_secs_f64(seconds));
        handle
            .insert_source(timer, |_, _, demo: &mut Demo| {
                demo.running = false;
                TimeoutAction::Drop
            })
            .expect("a timer");
    }
    let mut demo = Demo {
        host,
        window,
        chrome,
        page,
        chrome_hovered: false,
        frames: 0,
        running: true,
    };
    let signal = event_loop.get_signal();
    event_loop
        .run(None, &mut demo, |demo| {
            while let Some(event) = demo.host.next_event() {
                demo.handle(event);
            }
            demo.present();
            demo.host.flush();
            if !demo.running {
                signal.stop();
            }
        })
        .expect("the event loop");
    println!("{} frames", demo.frames);
    for target in demo.host.diagnostics().targets {
        println!(
            "{}: {:?}, {:?}",
            demo.name(Some(target.target)),
            target.stats,
            target.buffers
        );
    }
    // Presenters give their targets back before the host goes.
    let Demo {
        host, chrome, page, ..
    } = demo;
    drop((chrome, page));
    drop(host);
}
