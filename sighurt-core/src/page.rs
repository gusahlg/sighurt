//! Pages: engine processes and what they draw.
//!
//! Every engine is a separate process that speaks [`sighurt_ipc`]; any executable that does is
//! an engine. A [`Page`] runs one process (starting a new one if asked to load after it
//! exited) and is the GPUI view of what it sends: finished frames, or draw lists that this
//! module paints with GPUI's own primitives and fonts.
//!
//! ```text
//!   UI thread                                              engine process
//!   Page ── mpsc<ToEngine> ──> writer thread ──> stdin ──>       │
//!     ▲                                                          │
//!     └── cx.spawn task <── async channel <── reader thread <── stdout
//! ```
//!
//! The UI thread never touches the pipes. The writer thread drains a channel into the engine's
//! stdin. The reader thread decodes stdout, turns frames and images into GPUI images and checks
//! draw lists (the expensive parts) and hands everything to a task on the UI thread, which
//! updates the page. Nothing is polled: the page redraws when what it shows changes, and tells
//! the browser with [`PageEvent::Changed`] when its [`PageInfo`] does.
//!
//! Engines are untrusted, so everything they send is bounded: text and URLs by length, frames
//! by the viewport, images by count, size and total bytes, draw lists by length, and
//! coordinates by magnitude.

use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, fill, font, point, prelude::*, px, rgba, size, App, Bounds, ContentMask, Corners,
    CursorStyle, DevicePixels, EventEmitter, FocusHandle, Focusable, FontStyle, FontWeight,
    KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, NavigationDirection, PathBuilder, Pixels, Point, RenderImage, ScrollDelta,
    ScrollWheelEvent, SharedString, Size, Task, TextRun, Window,
};
use image::{Frame, RgbaImage};
use sighurt_ipc::{DrawOp, FromEngine, ToEngine, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT, VERSION};

/// Logical pixels scrolled per line by wheels that report lines. GPUI reports three lines per
/// wheel notch on Linux and Windows, so a notch scrolls 100 px, as in Chrome.
const LINE_HEIGHT: f32 = 100.0 / 3.0;
/// Longest text kept from an engine (titles, status, errors, console lines, draw list text), in
/// bytes.
const MAX_TEXT: usize = 16 * 1024;
/// Longest URL accepted from an engine, in bytes (Chrome's limit). Longer ones are ignored,
/// since a shortened URL points somewhere else.
const MAX_URL: usize = 2 * 1024 * 1024;
/// Engine messages that may wait for the UI thread before the reader thread stops reading.
/// Kept small because each one can be a whole frame.
const EVENT_QUEUE: usize = 2;
/// Engine messages the page applies in a row before it lets the UI thread do other work.
const EVENTS_PER_TURN: usize = 16;
/// How long an engine may take to exit after its stdin closes before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);
/// How long an engine may take to say Hello before it is given up on.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Most operations painted from one draw list, and most path points in it; the rest is cut.
const MAX_OPS: usize = 200_000;
const MAX_POINTS: usize = 1_000_000;
/// Most images a page may hold, and most bytes of pixels in total.
const MAX_IMAGES: usize = 4096;
const MAX_IMAGE_BYTES: usize = 512 * 1024 * 1024;
/// Longest side of an image in pixels: about what every GPU takes as one texture.
const MAX_IMAGE_SIDE: u32 = 8192;
/// Largest coordinate or size in a draw list, in logical pixels.
const MAX_COORD: f32 = 1e6;
/// Largest font size in a draw list. Glyphs are rasterized at this size.
const MAX_FONT_SIZE: f32 = 512.0;

/// What the UI shows about a page: URL bar, tab title, toolbar state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageInfo {
    pub url: String,
    pub title: Option<String>,
    pub load: LoadState,
    /// True when the engine has its own history to go back through, e.g. links followed
    /// inside the page. The browser keeps the history of navigations it made itself.
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// Transient status text such as the target of a hovered link.
    pub status: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum LoadState {
    Loading,
    #[default]
    Ready,
    Error(String),
}

/// Requests and news from a page to the browser.
#[derive(Clone, Debug, PartialEq)]
pub enum PageEvent {
    /// Open `url`. The browser picks the engine, which may not be this one.
    Open { url: String, new_tab: bool },
    /// [`Page::info`] changed.
    Changed,
    /// A navigation finished, which changed [`Page::info`] too: it has the new URL and title.
    Loaded,
}

/// A page whose content comes from an engine process.
///
/// The view takes the page's mouse and keyboard input and forwards it to the engine. Keys bound
/// in the keymap never reach it.
pub struct Page {
    /// Name of the engine, for messages.
    engine: String,
    command: Vec<String>,
    info: PageInfo,
    /// The pointer the engine asked for.
    cursor: CursorStyle,
    focus: FocusHandle,
    /// `None` once the engine has exited or failed to start.
    process: Option<Process>,
    /// Applies the engine's events to this page.
    _events: Task<()>,
    /// Messages waiting for the engine's first viewport size: the protocol wants `Resize` first.
    held: Option<Vec<ToEngine>>,
    /// Viewport last sent to the engine: physical width and height, and the scale factor.
    viewport: Option<(u32, u32, f32)>,
    /// Largest physical width and height ever sent to the engine: the most a frame may measure.
    largest: (u32, u32),
    /// Top-left corner of the page in window coordinates, from the last layout.
    origin: Point<Pixels>,
    /// The engine's latest frame or draw list.
    content: Content,
    images: Images,
    /// Images the page no longer shows. GPUI keeps painted images in its atlas until dropped.
    retired: Vec<Arc<RenderImage>>,
    /// Whether the left button went down on the page, so its release outside is forwarded too.
    dragging: bool,
    /// Focus state last sent to the engine.
    focused: bool,
    /// Kept to bring a restarted engine up to date.
    zoom: f32,
}

/// What a page shows: whatever the engine sent last.
enum Content {
    Empty,
    Frame(Arc<RenderImage>),
    Draw(Vec<DrawOp>),
}

impl Page {
    /// Starts engine `engine` by running `command` (the program followed by its arguments) and
    /// loads `url` in it. A bare program name is looked up next to the running executable first,
    /// then on `PATH`; a path is used as given.
    pub fn new(engine: String, command: Vec<String>, url: String, cx: &mut Context<Self>) -> Self {
        cx.on_release(|page, cx| {
            page.clear();
            page.drop_retired(None, cx);
        })
        .detach();

        let mut page = Self {
            engine,
            command,
            info: PageInfo {
                url,
                ..PageInfo::default()
            },
            cursor: CursorStyle::default(),
            focus: cx.focus_handle(),
            process: None,
            _events: Task::ready(()),
            held: None,
            viewport: None,
            largest: (0, 0),
            origin: Point::default(),
            content: Content::Empty,
            images: Images::default(),
            retired: Vec::new(),
            dragging: false,
            focused: false,
            zoom: 1.0,
        };
        page.start(cx);
        page
    }

    pub fn engine(&self) -> &str {
        &self.engine
    }

    pub fn info(&self) -> &PageInfo {
        &self.info
    }

    pub fn navigate(&mut self, url: &str, cx: &mut Context<Self>) {
        self.info.url = url.to_string();
        self.load(
            ToEngine::Navigate {
                url: url.to_string(),
            },
            cx,
        );
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load(ToEngine::Reload, cx);
    }

    /// Sends `msg`, which loads a page, or starts a new engine for `info.url` if the last one is
    /// gone.
    fn load(&mut self, msg: ToEngine, cx: &mut Context<Self>) {
        if self.process.is_none() {
            return self.start(cx);
        }
        self.info.clear_error();
        self.send(msg);
        cx.emit(PageEvent::Changed);
    }

    pub fn stop(&mut self) {
        self.send(ToEngine::Stop);
    }

    /// Steps back or forward through the history the engine keeps itself (see
    /// [`PageInfo::can_go_back`]).
    pub fn traverse(&mut self, forward: bool) {
        self.send(if forward {
            ToEngine::Forward
        } else {
            ToEngine::Back
        });
    }

    /// Page zoom as a multiplier; `1.0` is 100%. The engine applies it.
    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom;
        self.send(ToEngine::Zoom { factor: zoom });
    }

    /// Starts a fresh engine process and loads `info.url` in it.
    fn start(&mut self, cx: &mut Context<Self>) {
        // What an earlier process showed means nothing to the new one.
        self.clear();
        self.info.load = LoadState::Loading;
        self.viewport = None;
        let mut held = vec![ToEngine::Navigate {
            url: self.info.url.clone(),
        }];
        if self.focused {
            held.push(ToEngine::Focus { focused: true });
        }
        if self.zoom != 1.0 {
            held.push(ToEngine::Zoom { factor: self.zoom });
        }
        self.held = Some(held);

        match Process::spawn(&self.command, HELLO_TIMEOUT) {
            Ok((process, events)) => {
                self.process = Some(process);
                self._events = cx.spawn(async move |page, cx| {
                    let mut handled = 0usize;
                    while let Ok(event) = events.recv().await {
                        let exited = matches!(event, Event::Exited(_));
                        if page.update(cx, |page, cx| page.handle(event, cx)).is_err() || exited {
                            break;
                        }
                        // `recv` doesn't wait while messages keep coming, so an engine that
                        // never stops talking would keep the UI thread from input and drawing.
                        handled += 1;
                        if handled.is_multiple_of(EVENTS_PER_TURN) {
                            futures_lite::future::yield_now().await;
                        }
                    }
                });
            }
            Err(e) => {
                let message = format!("Engine \"{}\" could not be started: {e}", self.engine);
                self.info.load = LoadState::Error(message);
            }
        }
        cx.emit(PageEvent::Changed);
        cx.notify();
    }

    /// Applies what the engine said. Only what changes the page's look redraws it; what changes
    /// its [`PageInfo`] goes to the browser, which redraws the chrome.
    fn handle(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Frame(image) => {
                // A bigger frame is not meant for this page, and may be more than the GPU takes.
                let size = image.size(0);
                let (width, height) = self.largest;
                if u32::from(size.width) > width || u32::from(size.height) > height {
                    return;
                }
                self.show(Content::Frame(image));
            }
            Event::Draw(ops) => self.show(Content::Draw(ops)),
            Event::Image { id, image } => match self.images.insert(id, image) {
                Ok(replaced) => self.retired.extend(replaced),
                Err(_) => {
                    return tracing::warn!(
                        "engine \"{}\" is out of room for images; image {id} is dropped",
                        self.engine
                    )
                }
            },
            Event::Message(FromEngine::DropImage { id }) => {
                self.retired.extend(self.images.remove(id));
            }
            Event::Message(FromEngine::Cursor { name }) => {
                let cursor = css_cursor(&name);
                if cursor == self.cursor {
                    return;
                }
                self.cursor = cursor;
            }
            Event::Message(FromEngine::Console { level, message }) => {
                return tracing::debug!(engine = self.engine, level, "{}", clamp(message));
            }
            Event::Message(msg) => {
                if let Some(event) = self.info.apply(msg) {
                    cx.emit(event);
                }
                return;
            }
            Event::Exited(reason) => {
                self.process = None;
                self.info.exited(&self.engine, &reason);
                self.cursor = CursorStyle::default();
                cx.emit(PageEvent::Changed);
            }
        }
        // Runs outside any window update, so this reaches every window's atlas.
        self.drop_retired(None, cx);
        cx.notify();
    }

    /// Replaces what the page shows, retiring the frame it showed.
    fn show(&mut self, content: Content) {
        if let Content::Frame(frame) = std::mem::replace(&mut self.content, content) {
            self.retired.push(frame);
        }
    }

    /// Forgets everything the engine sent to draw.
    fn clear(&mut self) {
        self.show(Content::Empty);
        self.retired.extend(self.images.drain());
    }

    /// Evicts retired images from GPUI's atlas. `window` is the window being updated, if any:
    /// GPUI holds that one apart from the others while it is.
    fn drop_retired(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        for image in self.retired.drain(..) {
            cx.drop_image(image, window.as_deref_mut());
        }
    }

    /// Sends `msg` to the engine, or holds it until the engine has its first viewport size.
    fn send(&mut self, msg: ToEngine) {
        let Some(process) = &self.process else {
            return;
        };
        match &mut self.held {
            Some(held) => held.push(msg),
            // Fails only once the engine is gone, which the reader thread reports.
            None => _ = process.to_engine.send(msg),
        }
    }

    /// Runs on every layout. Tells the engine when its viewport changes; the first viewport also
    /// releases the messages held for it.
    fn layout(&mut self, bounds: Bounds<Pixels>, scale: f32) {
        self.origin = bounds.origin;
        let width = (f32::from(bounds.size.width) * scale).round() as u32;
        let height = (f32::from(bounds.size.height) * scale).round() as u32;
        if width == 0 || height == 0 || self.viewport == Some((width, height, scale)) {
            return;
        }
        self.viewport = Some((width, height, scale));
        self.largest = (self.largest.0.max(width), self.largest.1.max(height));
        let held = self.held.take();
        self.send(ToEngine::Resize {
            width,
            height,
            scale,
        });
        for msg in held.into_iter().flatten() {
            self.send(msg);
        }
    }

    /// Paints what the engine sent into `bounds`, the page's box.
    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        match &self.content {
            Content::Empty => {}
            Content::Frame(frame) => paint_frame(bounds, frame.clone(), window),
            Content::Draw(ops) => paint_ops(ops, &self.images, bounds, window, cx),
        }
    }

    /// Converts a window position into physical pixels relative to the page's top-left corner.
    fn page_point(&self, position: Point<Pixels>, window: &Window) -> (f32, f32) {
        let (offset, scale) = (position - self.origin, window.scale_factor());
        (f32::from(offset.x) * scale, f32::from(offset.y) * scale)
    }

    fn mouse_button(
        &mut self,
        button: MouseButton,
        down: bool,
        at: Point<Pixels>,
        window: &Window,
    ) {
        if button == MouseButton::Left {
            self.dragging = down;
        }
        let (x, y) = self.page_point(at, window);
        self.send(ToEngine::MouseButton {
            button: dom_button(button),
            down,
            x,
            y,
        });
    }

    fn key(&mut self, down: bool, keystroke: &Keystroke, repeat: bool, cx: &mut Context<Self>) {
        let Some((key, code)) = dom_key(keystroke) else {
            return;
        };
        self.send(ToEngine::Key {
            down,
            key,
            code,
            modifiers: modifier_bits(&keystroke.modifiers),
            repeat,
        });
        cx.stop_propagation();
    }
}

impl Render for Page {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drop_retired(Some(window), cx);
        // Focus and window activation changes refresh the whole window, bypassing GPUI's view
        // cache, so this sees every change while the page is shown, including focus given in the
        // same update that created the page (which focus listeners, activated only after that
        // update, would miss).
        let focused = self.focus.is_focused(window) && window.is_window_active();
        if focused != self.focused {
            self.focused = focused;
            self.send(ToEngine::Focus { focused });
        }

        let (layout, paint) = (cx.entity(), cx.entity());
        // `track_focus` also focuses the page when it is clicked.
        div()
            .track_focus(&self.focus)
            .size_full()
            .cursor(self.cursor)
            .on_key_down(cx.listener(|page, e: &KeyDownEvent, _, cx| {
                page.key(true, &e.keystroke, e.is_held, cx)
            }))
            .on_key_up(
                cx.listener(|page, e: &KeyUpEvent, _, cx| page.key(false, &e.keystroke, false, cx)),
            )
            .on_mouse_move(cx.listener(|page, e: &MouseMoveEvent, window, _| {
                let (x, y) = page.page_point(e.position, window);
                page.send(ToEngine::MouseMove { x, y });
            }))
            .on_any_mouse_down(cx.listener(|page, e: &MouseDownEvent, window, _| {
                page.mouse_button(e.button, true, e.position, window)
            }))
            // Elements offer an any-button mouse-up only in the capture phase.
            .capture_any_mouse_up(cx.listener(|page, e: &MouseUpEvent, window, _| {
                page.mouse_button(e.button, false, e.position, window)
            }))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|page, e: &MouseUpEvent, window, _| {
                    if page.dragging {
                        page.mouse_button(e.button, false, e.position, window)
                    }
                }),
            )
            .on_scroll_wheel(cx.listener(|page, e: &ScrollWheelEvent, window, _| {
                let (dx, dy) = wheel_delta(e.delta, window.scale_factor());
                let (x, y) = page.page_point(e.position, window);
                page.send(ToEngine::Wheel { dx, dy, x, y });
            }))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let scale = window.scale_factor();
                        layout.update(cx, |page, _| page.layout(bounds, scale));
                    },
                    move |bounds, (), window, cx| {
                        paint.update(cx, |page, cx| page.paint(bounds, window, cx));
                    },
                )
                .size_full(),
            )
    }
}

impl Focusable for Page {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PageEvent> for Page {}

/// Paints `frame` at the page's origin at its own size. A frame rendered for an older viewport
/// is clipped or leaves background showing, but is never stretched.
fn paint_frame(bounds: Bounds<Pixels>, frame: Arc<RenderImage>, window: &mut Window) {
    let logical = logical_size(frame.size(0), window.scale_factor());
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let bounds = Bounds::new(bounds.origin, logical);
        let _ = window.paint_image(bounds, Corners::default(), frame, 0, false);
    });
}

/// The logical size GPUI paints as exactly `pixels`. GPUI scales it back up and rounds up, so at
/// scales such as 1.2 float error would add a pixel and blur the whole frame; half a pixel of
/// slack absorbs the error.
fn logical_size(pixels: Size<DevicePixels>, scale: f32) -> Size<Pixels> {
    let logical = |n: DevicePixels| px((n.0 as f32 - 0.5) / scale);
    size(logical(pixels.width), logical(pixels.height))
}

/// Paints a draw list, as cut by [`bound_draw`], into `bounds`, the page's box.
fn paint_ops(
    ops: &[DrawOp],
    images: &Images,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    // The page's own bounds stay at the bottom.
    let mut clips = vec![bounds];
    for op in ops {
        let clip = *clips.last().unwrap_or(&bounds);
        match op {
            DrawOp::PushClip { x, y, w, h } => {
                clips.push(clip.intersect(&rect(bounds, *x, *y, *w, *h)))
            }
            DrawOp::PopClip => {
                if clips.len() > 1 {
                    clips.pop();
                }
            }
            op => window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                paint_op(op, images, bounds, window, cx)
            }),
        }
    }
}

/// Paints one operation other than a clip.
fn paint_op(op: &DrawOp, images: &Images, page: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let at = |x: f32, y: f32| page.origin + point(px(x), px(y));
    match op {
        DrawOp::Clear { color } => window.paint_quad(fill(page, rgba(*color))),
        DrawOp::Rect {
            x,
            y,
            w,
            h,
            radius,
            color,
        } => {
            let radius = radius.min(w.min(*h) / 2.0).max(0.0);
            let quad = fill(rect(page, *x, *y, *w, *h), rgba(*color));
            window.paint_quad(quad.corner_radii(px(radius)));
        }
        DrawOp::Path {
            points,
            width,
            color,
        } => {
            let (mut path, fewest) = if *width == 0.0 {
                (PathBuilder::fill(), 3)
            } else if *width > 0.0 {
                (PathBuilder::stroke(px(*width)), 2)
            } else {
                return;
            };
            if points.len() < fewest {
                return;
            }
            let mut points = points.iter().map(|&(x, y)| at(x, y));
            path.move_to(points.next().expect("there are points"));
            for point in points {
                path.line_to(point);
            }
            if *width == 0.0 {
                path.close();
            }
            // Fails for paths with more vertices than GPUI takes, which are not painted.
            if let Ok(path) = path.build() {
                window.paint_path(path, rgba(*color));
            }
        }
        DrawOp::Text {
            x,
            y,
            size,
            color,
            family,
            weight,
            italic,
            align,
            text,
        } => {
            let family: SharedString = match family.as_str() {
                "" => ".SystemUIFont".into(),
                family => family.to_string().into(),
            };
            let run = TextRun {
                len: text.len(),
                font: gpui::Font {
                    weight: FontWeight(f32::from(*weight).clamp(100.0, 900.0)),
                    style: if *italic {
                        FontStyle::Italic
                    } else {
                        FontStyle::Normal
                    },
                    ..font(family)
                },
                color: rgba(*color).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line =
                window
                    .text_system()
                    .shape_line(text.to_string().into(), px(*size), &[run], None);
            let width = f32::from(line.width);
            let x = match align {
                1 => x - width / 2.0,
                2 => x - width,
                _ => *x,
            };
            let _ = line.paint(at(x, *y), px(size * 1.2), window, cx);
        }
        DrawOp::Image { id, x, y, w, h } => {
            if let Some(image) = images.by_id.get(id) {
                let bounds = rect(page, *x, *y, *w, *h);
                let _ = window.paint_image(bounds, Corners::default(), image.clone(), 0, false);
            }
        }
        // Handled by `paint_ops`.
        DrawOp::PushClip { .. } | DrawOp::PopClip => {}
    }
}

/// A rectangle given in page coordinates, in window coordinates. Negative sizes are empty.
fn rect(page: Bounds<Pixels>, x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    let origin = page.origin + point(px(x), px(y));
    Bounds::new(origin, size(px(w.max(0.0)), px(h.max(0.0))))
}

/// Whether every coordinate and size in `op` is a number within [`MAX_COORD`], and text has
/// something to show at a font size within [`MAX_FONT_SIZE`].
fn in_range(op: &DrawOp) -> bool {
    let ok = |values: &[f32]| values.iter().all(|v| v.abs() <= MAX_COORD);
    match op {
        DrawOp::Clear { .. } | DrawOp::PopClip => true,
        DrawOp::Rect {
            x, y, w, h, radius, ..
        } => ok(&[*x, *y, *w, *h, *radius]),
        DrawOp::Path { points, width, .. } => {
            ok(&[*width]) && points.iter().all(|&(x, y)| ok(&[x, y]))
        }
        DrawOp::Text {
            x, y, size, text, ..
        } => ok(&[*x, *y]) && *size > 0.0 && *size <= MAX_FONT_SIZE && !text.is_empty(),
        DrawOp::Image { x, y, w, h, .. } | DrawOp::PushClip { x, y, w, h } => ok(&[*x, *y, *w, *h]),
    }
}

/// Cuts a draw list to [`MAX_OPS`] operations and [`MAX_POINTS`] path points, so that one list
/// cannot keep the UI thread painting for arbitrarily long, cuts text to [`MAX_TEXT`] bytes, and
/// drops the operations with a coordinate beyond [`MAX_COORD`] (NaN and infinities included) or
/// text that can't be painted. Runs on the reader thread, once per list, so painting it (again
/// whenever the window redraws) needs no checks.
fn bound_draw(ops: &mut Vec<DrawOp>) {
    let mut points = 0;
    let too_many = ops.iter().take(MAX_OPS).position(|op| {
        if let DrawOp::Path { points: p, .. } = op {
            points += p.len();
        }
        points > MAX_POINTS
    });
    ops.truncate(too_many.unwrap_or(MAX_OPS));
    ops.retain_mut(|op| {
        if let DrawOp::Text { text, .. } = op {
            *text = clamp(std::mem::take(text));
        }
        in_range(op)
    });
}

/// Images an engine uploaded for its draw lists, within [`MAX_IMAGES`] and [`MAX_IMAGE_BYTES`].
#[derive(Default)]
struct Images {
    by_id: HashMap<u32, Arc<RenderImage>>,
    /// Bytes of pixels held.
    bytes: usize,
}

impl Images {
    /// Defines or replaces image `id` and returns the image it replaces. An image that would take
    /// the page past its limits is handed back as the error, and nothing changes.
    fn insert(
        &mut self,
        id: u32,
        image: Arc<RenderImage>,
    ) -> Result<Option<Arc<RenderImage>>, Arc<RenderImage>> {
        let replaced = self.by_id.get(&id).map_or(0, |old| byte_len(old));
        let bytes = self.bytes - replaced + byte_len(&image);
        let count = self.by_id.len() + usize::from(!self.by_id.contains_key(&id));
        if count > MAX_IMAGES || bytes > MAX_IMAGE_BYTES {
            return Err(image);
        }
        self.bytes = bytes;
        Ok(self.by_id.insert(id, image))
    }

    fn remove(&mut self, id: u32) -> Option<Arc<RenderImage>> {
        let image = self.by_id.remove(&id)?;
        self.bytes -= byte_len(&image);
        Some(image)
    }

    fn drain(&mut self) -> impl Iterator<Item = Arc<RenderImage>> + '_ {
        self.bytes = 0;
        self.by_id.drain().map(|(_, image)| image)
    }
}

fn byte_len(image: &RenderImage) -> usize {
    image.as_bytes(0).map_or(0, <[u8]>::len)
}

/// How engine messages change a page's info; apart from the view, so it can be tested without a
/// window.
impl PageInfo {
    /// Applies one engine message and returns the event it raises for the browser, if any:
    /// [`PageEvent::Changed`] only when it changed something.
    fn apply(&mut self, msg: FromEngine) -> Option<PageEvent> {
        let changed = match msg {
            FromEngine::Url { url } if url.len() <= MAX_URL => set(&mut self.url, url),
            FromEngine::Title { title } => set(
                &mut self.title,
                Some(clamp(title)).filter(|t| !t.is_empty()),
            ),
            FromEngine::Loading { loading: true } => set(&mut self.load, LoadState::Loading),
            FromEngine::Loading { loading: false } if self.load == LoadState::Loading => {
                self.load = LoadState::Ready;
                return Some(PageEvent::Loaded);
            }
            FromEngine::History {
                can_back,
                can_forward,
            } => set(&mut self.can_go_back, can_back) | set(&mut self.can_go_forward, can_forward),
            FromEngine::Status { text } => set(
                &mut self.status,
                Some(clamp(text)).filter(|t| !t.is_empty()),
            ),
            FromEngine::Open { url, new_tab } if url.len() <= MAX_URL => {
                return Some(PageEvent::Open { url, new_tab })
            }
            FromEngine::Error { message } => set(&mut self.load, LoadState::Error(clamp(message))),
            // Too long (see `MAX_URL`), or a finished load that would paper over an error.
            FromEngine::Url { .. } | FromEngine::Open { .. } | FromEngine::Loading { .. } => false,
            // The page's business, or the reader thread's.
            FromEngine::Hello { .. }
            | FromEngine::Frame { .. }
            | FromEngine::Draw { .. }
            | FromEngine::Image { .. }
            | FromEngine::DropImage { .. }
            | FromEngine::Cursor { .. }
            | FromEngine::Console { .. } => false,
        };
        changed.then_some(PageEvent::Changed)
    }

    /// Forgets an error the engine reported, so the next page it loads can show.
    fn clear_error(&mut self) {
        if matches!(self.load, LoadState::Error(_)) {
            self.load = LoadState::Ready;
        }
    }

    /// Shows why the engine is gone, followed by the error it reported if that is still showing:
    /// an engine that crashes usually says why just before it exits. The history and hover state
    /// it reported went with it.
    fn exited(&mut self, engine: &str, reason: &str) {
        let message = match &self.load {
            LoadState::Error(error) => format!("Engine \"{engine}\" {reason}: {error}"),
            _ => format!("Engine \"{engine}\" {reason}."),
        };
        self.load = LoadState::Error(message);
        self.can_go_back = false;
        self.can_go_forward = false;
        self.status = None;
    }
}

/// Sets `field` to `value` and says whether that changed it.
fn set<T: PartialEq>(field: &mut T, value: T) -> bool {
    let changed = *field != value;
    *field = value;
    changed
}

/// Cuts `text` to at most [`MAX_TEXT`] bytes at a character boundary, and frees the rest.
fn clamp(mut text: String) -> String {
    if text.len() > MAX_TEXT {
        text.truncate(text.floor_char_boundary(MAX_TEXT));
        text.shrink_to_fit();
    }
    text
}

/// A running engine process. Dropping it closes the engine's stdin, which asks it to exit.
struct Process {
    to_engine: mpsc::Sender<ToEngine>,
    /// Taken by `drop`.
    child: Option<Child>,
}

/// What the reader thread passes to the page.
#[derive(Debug)]
enum Event {
    /// Any message except `Hello` and the ones below.
    Message(FromEngine),
    /// A frame, already converted for GPUI.
    Frame(Arc<RenderImage>),
    /// A draw list, cut to what the page paints.
    Draw(Vec<DrawOp>),
    /// An image for draw lists, already converted for GPUI.
    Image { id: u32, image: Arc<RenderImage> },
    /// The engine is gone or was given up on. The reason completes the sentence "Engine "name" …".
    Exited(String),
}

impl Process {
    /// Starts the engine and its I/O threads. Everything the engine says arrives on the returned
    /// channel, up to the first [`Event::Exited`]; anything after it is stale. An engine that
    /// says nothing within `hello_timeout` gets one early, and dropping the process stops it.
    fn spawn(
        command: &[String],
        hello_timeout: Duration,
    ) -> io::Result<(Self, async_channel::Receiver<Event>)> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no command configured"))?;
        let mut child = Command::new(sibling(program).unwrap_or_else(|| program.into()))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("{program}: {e}")))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");

        let (to_engine, outbox) = mpsc::channel::<ToEngine>();
        thread::spawn(move || {
            let mut stdin = BufWriter::new(stdin);
            for msg in outbox {
                if msg.write_to(&mut stdin).is_err() {
                    break;
                }
            }
            // Returning drops stdin, which closes the pipe.
        });

        let (events, receiver) = async_channel::bounded(EVENT_QUEUE);
        // Nothing is sent on `started`: the reader thread drops it once the engine has said its
        // first message or exited, which ends the wait early.
        let (started, starting) = mpsc::channel::<()>();
        let give_up = events.clone();
        thread::spawn(move || {
            if let Err(mpsc::RecvTimeoutError::Timeout) = starting.recv_timeout(hello_timeout) {
                let secs = hello_timeout.as_secs_f32();
                let reason = format!("did not start: no Hello within {secs} s");
                let _ = give_up.send_blocking(Event::Exited(reason));
            }
        });
        thread::spawn(move || {
            let reason = read_events(BufReader::new(stdout), &events, started);
            let _ = events.send_blocking(Event::Exited(reason));
        });

        let child = Some(child);
        Ok((Self { to_engine, child }, receiver))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // `to_engine` is dropped right after this, which ends the writer thread and closes the
        // engine's stdin. A well-behaved engine exits; one that doesn't is killed.
        let Some(mut child) = self.child.take() else {
            return;
        };
        thread::spawn(move || {
            let deadline = Instant::now() + EXIT_GRACE;
            while let Ok(None) = child.try_wait() {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
    }
}

/// Whether `command` names a program that can be started: a path to a file, or a bare name
/// found next to the running executable or on `PATH`.
pub fn command_found(command: &[String]) -> bool {
    let Some(program) = command.first() else {
        return false;
    };
    if program.contains(std::path::is_separator) {
        return Path::new(program).is_file();
    }
    let file = format!("{program}{}", std::env::consts::EXE_SUFFIX);
    sibling(program).is_some()
        || std::env::var_os("PATH")
            .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(&file).is_file()))
}

/// Where a bare program name is found next to the running executable, so engines shipped
/// alongside `sig` work without being on `PATH`. Anything else is left to `Command`, which
/// searches `PATH`.
fn sibling(program: &str) -> Option<PathBuf> {
    if program.contains(std::path::is_separator) {
        return None;
    }
    let file = format!("{program}{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(file)))
        .filter(|path| path.is_file())
}

/// Decodes engine output into [`Event`]s until it ends, and returns why it ended. `started` is
/// dropped once the engine has said its first message or exited.
fn read_events(
    mut stdout: impl Read,
    events: &async_channel::Sender<Event>,
    started: mpsc::Sender<()>,
) -> String {
    let hello = next_message(&mut stdout);
    drop(started);
    match hello {
        Ok(FromEngine::Hello {
            version: VERSION, ..
        }) => {}
        Ok(FromEngine::Hello { version, .. }) => {
            return format!("speaks protocol version {version}, but Sighurt speaks {VERSION}")
        }
        Ok(_) => return "broke the protocol (it did not start with Hello)".into(),
        Err(reason) => return reason,
    }
    let side = 1..=MAX_IMAGE_SIDE;
    loop {
        let event = match next_message(&mut stdout) {
            Ok(FromEngine::Frame {
                width,
                height,
                rgba,
            }) if width > 0 && height > 0 => Event::Frame(render_image(width, height, rgba)),
            Ok(FromEngine::Frame { .. }) => continue, // Empty; nothing to show.
            Ok(FromEngine::Draw { mut ops }) => {
                bound_draw(&mut ops);
                Event::Draw(ops)
            }
            Ok(FromEngine::Image {
                id,
                width,
                height,
                rgba,
            }) if side.contains(&width) && side.contains(&height) => Event::Image {
                id,
                image: render_image(width, height, rgba),
            },
            Ok(FromEngine::Image { .. }) => continue, // Empty, or too big for the GPU.
            Ok(msg) => Event::Message(msg),
            Err(reason) => return reason,
        };
        if events.send_blocking(event).is_err() {
            return "was closed".into(); // The page is gone; nobody reads this.
        }
    }
}

fn next_message(r: &mut impl Read) -> Result<FromEngine, String> {
    match FromEngine::read_from(r) {
        Ok(Some(msg)) => Ok(msg),
        Ok(None) => Err("exited".into()),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err("exited".into()),
        Err(e) => Err(format!("broke the protocol ({e})")),
    }
}

/// Converts RGBA pixels into a GPUI image. GPUI wants BGRA, so red and blue swap places in the
/// buffer that came off the pipe, without a copy.
fn render_image(width: u32, height: u32, mut rgba: Vec<u8>) -> Arc<RenderImage> {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let buffer = RgbaImage::from_raw(width, height, rgba).expect("sighurt-ipc checks image sizes");
    Arc::new(RenderImage::new([Frame::new(buffer)]))
}

/// Maps a CSS cursor keyword to the closest GPUI cursor. GPUI has no busy or move cursors;
/// those and unknown names get the arrow.
fn css_cursor(name: &str) -> CursorStyle {
    match name {
        "pointer" => CursorStyle::PointingHand,
        "text" => CursorStyle::IBeam,
        "vertical-text" => CursorStyle::IBeamCursorForVerticalLayout,
        "crosshair" => CursorStyle::Crosshair,
        "grab" => CursorStyle::OpenHand,
        "grabbing" => CursorStyle::ClosedHand,
        "e-resize" => CursorStyle::ResizeRight,
        "w-resize" => CursorStyle::ResizeLeft,
        "ew-resize" => CursorStyle::ResizeLeftRight,
        "n-resize" => CursorStyle::ResizeUp,
        "s-resize" => CursorStyle::ResizeDown,
        "ns-resize" => CursorStyle::ResizeUpDown,
        // GPUI's docs swap these two diagonals; this follows its platform code.
        "nw-resize" | "se-resize" | "nwse-resize" => CursorStyle::ResizeUpLeftDownRight,
        "ne-resize" | "sw-resize" | "nesw-resize" => CursorStyle::ResizeUpRightDownLeft,
        "col-resize" => CursorStyle::ResizeColumn,
        "row-resize" => CursorStyle::ResizeRow,
        "not-allowed" | "no-drop" => CursorStyle::OperationNotAllowed,
        "alias" => CursorStyle::DragLink,
        "copy" => CursorStyle::DragCopy,
        "context-menu" => CursorStyle::ContextualMenu,
        "none" => CursorStyle::None,
        _ => CursorStyle::Arrow,
    }
}

/// Translates a GPUI keystroke into W3C `KeyboardEvent.key` and `.code` values, or `None` for
/// keys without a sensible translation. Best effort: GPUI reports the key a layout produces,
/// not the physical key, so `code` assumes a US layout.
fn dom_key(keystroke: &Keystroke) -> Option<(String, String)> {
    let key = keystroke.key.as_str();
    if key == "space" {
        return Some((" ".into(), "Space".into()));
    }
    if let Some(name) = named_key(key) {
        return Some((name.into(), name.into()));
    }
    if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        if (1..=24).contains(&n) {
            return Some((format!("F{n}"), format!("F{n}")));
        }
    }
    // The text the key produced. Ctrl and Cmd chords produce none, so fall back to the key.
    let text = match &keystroke.key_char {
        Some(text) if !text.is_empty() => text.clone(),
        _ if key.chars().count() == 1 && keystroke.modifiers.shift => key.to_uppercase(),
        _ if key.chars().count() == 1 => key.to_string(),
        _ => return None,
    };
    Some((text, dom_code(key)))
}

/// W3C name of a GPUI key that produces no text; the name doubles as its `code`.
fn named_key(key: &str) -> Option<&'static str> {
    Some(match key {
        "enter" => "Enter",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "escape" => "Escape",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        _ => return None,
    })
}

/// The US-layout `KeyboardEvent.code` for a single-character GPUI key.
fn dom_code(key: &str) -> String {
    const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
    const PUNCTUATION: [(&str, &str); 11] = [
        ("-_", "Minus"),
        ("=+", "Equal"),
        ("[{", "BracketLeft"),
        ("]}", "BracketRight"),
        ("\\|", "Backslash"),
        (";:", "Semicolon"),
        ("'\"", "Quote"),
        (",<", "Comma"),
        (".>", "Period"),
        ("/?", "Slash"),
        ("`~", "Backquote"),
    ];
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return "Unidentified".into();
    };
    if c.is_ascii_alphabetic() {
        return format!("Key{}", c.to_ascii_uppercase());
    }
    if c.is_ascii_digit() {
        return format!("Digit{c}");
    }
    if let Some(digit) = SHIFTED_DIGITS.find(c) {
        return format!("Digit{digit}");
    }
    PUNCTUATION
        .iter()
        .find(|(chars, _)| chars.contains(c))
        .map_or("Unidentified", |&(_, code)| code)
        .into()
}

fn modifier_bits(m: &Modifiers) -> u8 {
    let bit = |held: bool, bit: u8| if held { bit } else { 0 };
    bit(m.shift, MOD_SHIFT)
        | bit(m.control, MOD_CTRL)
        | bit(m.alt, MOD_ALT)
        | bit(m.platform, MOD_META)
}

fn dom_button(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::Navigate(NavigationDirection::Back) => 3,
        MouseButton::Navigate(NavigationDirection::Forward) => 4,
    }
}

/// Converts a GPUI scroll delta into DOM wheel deltas in physical pixels. The signs flip: in
/// GPUI positive y moves the content down (scrolls up), in the DOM it scrolls down.
fn wheel_delta(delta: ScrollDelta, scale: f32) -> (f32, f32) {
    let delta = delta.pixel_delta(px(LINE_HEIGHT));
    (-f32::from(delta.x) * scale, -f32::from(delta.y) * scale)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn dom(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Option<(String, String)> {
        dom_key(&Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        })
    }

    fn pair(key: &str, code: &str) -> Option<(String, String)> {
        Some((key.into(), code.into()))
    }

    /// What an engine writes to say `msgs`.
    pub(crate) fn encode(msgs: &[FromEngine]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for msg in msgs {
            msg.write_to(&mut bytes).unwrap();
        }
        bytes
    }

    pub(crate) fn hello(version: u32) -> FromEngine {
        FromEngine::Hello {
            version,
            name: "fake".into(),
        }
    }

    #[test]
    fn translates_keys() {
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..none
        };
        let ctrl = Modifiers {
            control: true,
            ..none
        };
        assert_eq!(dom("a", Some("a"), none), pair("a", "KeyA"));
        assert_eq!(dom("a", Some("A"), shift), pair("A", "KeyA"));
        assert_eq!(dom("a", None, ctrl), pair("a", "KeyA"));
        assert_eq!(
            dom(
                "a",
                None,
                Modifiers {
                    shift: true,
                    ..ctrl
                }
            ),
            pair("A", "KeyA")
        );
        assert_eq!(dom("f", Some("f"), none), pair("f", "KeyF"));
        assert_eq!(dom("1", Some("1"), none), pair("1", "Digit1"));
        // GPUI reports shifted symbols as the key itself.
        assert_eq!(dom("!", Some("!"), none), pair("!", "Digit1"));
        assert_eq!(dom("?", Some("?"), none), pair("?", "Slash"));
        assert_eq!(dom("-", Some("-"), none), pair("-", "Minus"));
        assert_eq!(dom("é", Some("é"), none), pair("é", "Unidentified"));
        assert_eq!(dom("space", Some(" "), none), pair(" ", "Space"));
        assert_eq!(dom("enter", None, shift), pair("Enter", "Enter"));
        assert_eq!(dom("backspace", None, none), pair("Backspace", "Backspace"));
        assert_eq!(dom("left", None, none), pair("ArrowLeft", "ArrowLeft"));
        assert_eq!(dom("pagedown", None, none), pair("PageDown", "PageDown"));
        assert_eq!(dom("f5", None, none), pair("F5", "F5"));
        assert_eq!(dom("f12", None, ctrl), pair("F12", "F12"));
        assert_eq!(dom("menu", None, none), None);
    }

    #[test]
    fn maps_modifiers_and_buttons() {
        assert_eq!(modifier_bits(&Modifiers::default()), 0);
        let all = Modifiers {
            control: true,
            alt: true,
            shift: true,
            platform: true,
            function: true,
        };
        assert_eq!(
            modifier_bits(&all),
            MOD_SHIFT | MOD_CTRL | MOD_ALT | MOD_META
        );
        assert_eq!(dom_button(MouseButton::Left), 0);
        assert_eq!(dom_button(MouseButton::Middle), 1);
        assert_eq!(dom_button(MouseButton::Right), 2);
        assert_eq!(
            dom_button(MouseButton::Navigate(NavigationDirection::Back)),
            3
        );
        assert_eq!(
            dom_button(MouseButton::Navigate(NavigationDirection::Forward)),
            4
        );
    }

    #[test]
    fn converts_wheel_deltas() {
        // Wheel down: GPUI reports negative y, the DOM positive, in physical pixels.
        let (dx, dy) = wheel_delta(ScrollDelta::Lines(point(0.0, -1.0)), 2.0);
        assert_eq!((dx, dy), (0.0, 2.0 * LINE_HEIGHT));
        let (dx, dy) = wheel_delta(ScrollDelta::Pixels(point(px(10.0), px(-4.0))), 1.5);
        assert_eq!((dx, dy), (-15.0, 6.0));
    }

    #[test]
    fn maps_cursors() {
        assert_eq!(css_cursor("pointer"), CursorStyle::PointingHand);
        assert_eq!(css_cursor("text"), CursorStyle::IBeam);
        assert_eq!(
            css_cursor("nesw-resize"),
            CursorStyle::ResizeUpRightDownLeft
        );
        assert_eq!(css_cursor("none"), CursorStyle::None);
        assert_eq!(css_cursor("default"), CursorStyle::Arrow);
        assert_eq!(css_cursor("wait"), CursorStyle::Arrow);
        assert_eq!(css_cursor("made-up"), CursorStyle::Arrow);
    }

    #[test]
    fn applies_engine_messages() {
        let changed = Some(PageEvent::Changed);
        let mut info = PageInfo::default();
        assert_eq!(info.apply(FromEngine::Loading { loading: true }), changed);
        assert_eq!(info.load, LoadState::Loading);
        let url = || FromEngine::Url {
            url: "https://a.test/".into(),
        };
        assert_eq!(info.apply(url()), changed);
        // Saying the same again changes nothing, so the browser hears nothing.
        assert_eq!(info.apply(url()), None);
        info.apply(FromEngine::Title { title: "A".into() });
        assert_eq!(
            info.apply(FromEngine::Loading { loading: false }),
            Some(PageEvent::Loaded)
        );
        // Only the transition from loading counts.
        assert_eq!(info.apply(FromEngine::Loading { loading: false }), None);
        assert_eq!(info.url, "https://a.test/");
        assert_eq!(info.title.as_deref(), Some("A"));
        assert_eq!(info.load, LoadState::Ready);

        assert_eq!(info.apply(FromEngine::Title { title: "".into() }), changed);
        assert_eq!(info.title, None);
        assert_eq!(info.apply(FromEngine::Status { text: "x".into() }), changed);
        assert_eq!(info.apply(FromEngine::Status { text: "x".into() }), None);
        assert_eq!(info.status.as_deref(), Some("x"));
        info.apply(FromEngine::Status { text: "".into() });
        assert_eq!(info.status, None);
        let history = || FromEngine::History {
            can_back: true,
            can_forward: false,
        };
        assert_eq!(info.apply(history()), changed);
        assert_eq!(info.apply(history()), None);
        assert!(info.can_go_back && !info.can_go_forward);
        // The page's own business.
        let cursor = FromEngine::Cursor {
            name: "pointer".into(),
        };
        assert_eq!(info.apply(cursor), None);

        let open = FromEngine::Open {
            url: "https://b.test/".into(),
            new_tab: true,
        };
        assert_eq!(
            info.apply(open),
            Some(PageEvent::Open {
                url: "https://b.test/".into(),
                new_tab: true
            })
        );

        info.apply(FromEngine::Error {
            message: "bad".into(),
        });
        assert_eq!(info.load, LoadState::Error("bad".into()));
        // A finished load does not paper over an error.
        assert_eq!(info.apply(FromEngine::Loading { loading: false }), None);
        assert_eq!(info.load, LoadState::Error("bad".into()));
        info.clear_error();
        assert_eq!(info.load, LoadState::Ready);

        info.apply(FromEngine::Status { text: "x".into() });
        info.exited("fake", "exited");
        let gone = LoadState::Error("Engine \"fake\" exited.".into());
        assert_eq!(info.load, gone);
        assert!(!info.can_go_back && info.status.is_none());
        assert_eq!(info.url, "https://a.test/");
    }

    #[test]
    fn limits_what_engines_can_store() {
        let mut info = PageInfo::default();
        // Cut at a character boundary: "é" is two bytes and would straddle the limit.
        let title = format!("a{}", "é".repeat(MAX_TEXT));
        info.apply(FromEngine::Title { title });
        let title = info.title.take().unwrap();
        assert_eq!(title.len(), MAX_TEXT - 1);
        assert!(title.capacity() < 2 * MAX_TEXT);

        let long_url = format!("https://a.test/{}", "x".repeat(MAX_URL));
        info.apply(FromEngine::Url {
            url: "https://ok.test/".into(),
        });
        let too_long = FromEngine::Url {
            url: long_url.clone(),
        };
        assert_eq!(info.apply(too_long), None);
        assert_eq!(info.url, "https://ok.test/");
        let open = FromEngine::Open {
            url: long_url,
            new_tab: true,
        };
        assert_eq!(info.apply(open), None);
    }

    #[test]
    fn paints_frames_at_their_exact_size() {
        for scale in [1.0, 1.1, 1.2, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0] {
            for n in 1..=8192 {
                let logical = logical_size(size(DevicePixels(n), DevicePixels(1)), scale);
                // What `Window::paint_image` does with it.
                let painted = (f32::from(logical.width) * scale).ceil();
                assert_eq!(painted, n as f32, "{n} px at scale {scale}");
            }
        }
    }

    #[test]
    fn swizzles_images_to_bgra() {
        let image = render_image(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(image.as_bytes(0), Some(&[3, 2, 1, 4, 7, 6, 5, 8][..]));
        assert_eq!(u32::from(image.size(0).width), 2);
        assert_eq!(u32::from(image.size(0).height), 1);
    }

    #[test]
    fn reads_engine_output() {
        let bytes = encode(&[
            hello(VERSION),
            FromEngine::Title { title: "T".into() },
            FromEngine::Frame {
                width: 1,
                height: 1,
                rgba: vec![1, 2, 3, 4],
            },
            FromEngine::Frame {
                width: 0,
                height: 0,
                rgba: vec![],
            },
            FromEngine::Image {
                id: 7,
                width: 1,
                height: 1,
                rgba: vec![5, 6, 7, 8],
            },
            FromEngine::Image {
                id: 8,
                width: MAX_IMAGE_SIDE + 1,
                height: 1,
                rgba: vec![0; (MAX_IMAGE_SIDE as usize + 1) * 4],
            },
            FromEngine::Draw {
                ops: vec![DrawOp::PopClip; MAX_OPS + 1],
            },
        ]);
        let (tx, rx) = async_channel::unbounded();
        assert_eq!(
            read_events(bytes.as_slice(), &tx, mpsc::channel().0),
            "exited"
        );
        assert!(
            matches!(rx.try_recv(), Ok(Event::Message(FromEngine::Title { title })) if title == "T")
        );
        assert!(
            matches!(rx.try_recv(), Ok(Event::Frame(image)) if image.as_bytes(0) == Some(&[3, 2, 1, 4][..]))
        );
        // Empty frames are dropped, and so are images too big for the GPU.
        assert!(
            matches!(rx.try_recv(), Ok(Event::Image { id: 7, image }) if image.as_bytes(0) == Some(&[7, 6, 5, 8][..]))
        );
        assert!(matches!(rx.try_recv(), Ok(Event::Draw(ops)) if ops.len() == MAX_OPS));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn cuts_long_draw_lists() {
        let path = |n: usize| DrawOp::Path {
            points: vec![(0.0, 0.0); n],
            width: 1.0,
            color: 0,
        };
        let mut ops = vec![path(MAX_POINTS / 2), path(MAX_POINTS / 2), path(1), path(1)];
        bound_draw(&mut ops);
        assert_eq!(ops.len(), 2);
        let mut ops = vec![DrawOp::PopClip; 3];
        bound_draw(&mut ops);
        assert_eq!(ops.len(), 3);
        // Wild coordinates drop the operation, not the rest of the list.
        let wild = DrawOp::PushClip {
            x: f32::NAN,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
        let mut ops = vec![DrawOp::PopClip, wild, DrawOp::Clear { color: 1 }];
        bound_draw(&mut ops);
        assert_eq!(ops, [DrawOp::PopClip, DrawOp::Clear { color: 1 }]);
    }

    #[test]
    fn skips_ops_with_wild_coordinates() {
        let rect = |x: f32, w: f32| DrawOp::Rect {
            x,
            y: 0.0,
            w,
            h: 1.0,
            radius: 0.0,
            color: 0,
        };
        assert!(in_range(&rect(-5.0, 10.0)));
        assert!(in_range(&rect(0.0, -10.0)));
        assert!(!in_range(&rect(f32::NAN, 1.0)));
        assert!(!in_range(&rect(0.0, f32::INFINITY)));
        assert!(!in_range(&rect(2.0 * MAX_COORD, 1.0)));
        let path = |points| DrawOp::Path {
            points,
            width: 0.0,
            color: 0,
        };
        assert!(in_range(&path(vec![(1.0, 2.0), (3.0, 4.0)])));
        assert!(!in_range(&path(vec![(1.0, 2.0), (3.0, f32::NEG_INFINITY)])));
        assert!(in_range(&DrawOp::Clear { color: 0 }));
    }

    #[test]
    fn keeps_only_paintable_text() {
        let text = |size: f32, text: &str| DrawOp::Text {
            x: 0.0,
            y: 0.0,
            size,
            color: 0,
            family: String::new(),
            weight: 400,
            italic: false,
            align: 0,
            text: text.into(),
        };
        let long = "é".repeat(MAX_TEXT);
        let mut ops = vec![
            text(16.0, "ok"),
            text(0.0, "x"),
            text(MAX_FONT_SIZE + 1.0, "x"),
            text(f32::NAN, "x"),
            text(16.0, ""),
            text(16.0, &long),
        ];
        bound_draw(&mut ops);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0], text(16.0, "ok"));
        let DrawOp::Text { text, .. } = &ops[1] else {
            panic!("not text");
        };
        assert_eq!(text.len(), MAX_TEXT);
    }

    #[test]
    fn keeps_images_within_limits() {
        let image = || render_image(1, 2, vec![0; 8]);
        let mut images = Images::default();
        for id in 0..MAX_IMAGES as u32 {
            assert!(matches!(images.insert(id, image()), Ok(None)));
        }
        assert_eq!(images.bytes, MAX_IMAGES * 8);
        // Full: a new image is refused, but one can still be replaced.
        assert!(images.insert(u32::MAX, image()).is_err());
        let replacement = render_image(2, 2, vec![0; 16]);
        assert!(matches!(images.insert(3, replacement), Ok(Some(_))));
        assert_eq!(images.bytes, MAX_IMAGES * 8 + 8);
        assert!(images.remove(3).is_some() && images.remove(3).is_none());
        assert_eq!(images.bytes, (MAX_IMAGES - 1) * 8);
        assert_eq!(images.drain().count(), MAX_IMAGES - 1);
        assert_eq!((images.bytes, images.by_id.is_empty()), (0, true));
    }

    #[test]
    fn finds_engine_programs() {
        assert!(!command_found(&[]));
        assert!(!command_found(&["/nonexistent/sig-gone".into()]));
        assert!(!command_found(&["sighurt-no-such-engine".into()]));
        #[cfg(unix)]
        assert!(command_found(&["sh".into(), "-c".into(), "true".into()]));
    }

    #[test]
    fn rejects_engines_that_break_the_protocol() {
        let (tx, _rx) = async_channel::unbounded();
        let read = |bytes: &[u8]| read_events(bytes, &tx, mpsc::channel().0);
        assert!(read(&encode(&[hello(VERSION + 1)])).contains("protocol version"));
        assert!(read(&encode(&[FromEngine::Title { title: "x".into() }])).contains("Hello"));
        assert!(read(&[1, 0, 0, 0, 99]).starts_with("broke the protocol"));
        assert_eq!(read(&[]), "exited");
    }

    #[test]
    fn reports_engines_that_cannot_start() {
        let Err(e) = Process::spawn(&["sighurt-no-such-engine".into()], HELLO_TIMEOUT) else {
            panic!("spawned a missing program");
        };
        assert!(e.to_string().contains("sighurt-no-such-engine"));
        assert!(Process::spawn(&[], HELLO_TIMEOUT).is_err());
    }

    /// Runs a stand-in engine: `sh` records what it is sent until its stdin closes, then replays
    /// canned output.
    #[cfg(unix)]
    #[test]
    fn talks_to_an_engine_process() {
        let dir = tempfile::tempdir().unwrap();
        let (sent_path, output_path) = (dir.path().join("sent"), dir.path().join("output"));
        let output = encode(&[
            hello(VERSION),
            FromEngine::Title {
                title: "Fake".into(),
            },
            FromEngine::Frame {
                width: 1,
                height: 1,
                rgba: vec![1, 2, 3, 4],
            },
        ]);
        std::fs::write(&output_path, output).unwrap();
        let script = r#"cat > "$0" && cat "$1""#;
        let command = [
            "sh",
            "-c",
            script,
            sent_path.to_str().unwrap(),
            output_path.to_str().unwrap(),
        ]
        .map(String::from);

        let (process, events) = Process::spawn(&command, HELLO_TIMEOUT).unwrap();
        let sent = [
            ToEngine::Resize {
                width: 800,
                height: 600,
                scale: 2.0,
            },
            ToEngine::Navigate {
                url: "https://a.test/".into(),
            },
        ];
        for msg in &sent {
            process.to_engine.send(msg.clone()).unwrap();
        }
        drop(process);

        assert!(
            matches!(next_event(&events), Event::Message(FromEngine::Title { title }) if title == "Fake")
        );
        assert!(matches!(next_event(&events), Event::Frame(_)));
        assert!(matches!(next_event(&events), Event::Exited(reason) if reason == "exited"));
        // The Hello timeout was called off, so nothing follows.
        assert!(events.recv_blocking().is_err());

        let recorded = std::fs::read(&sent_path).unwrap();
        let mut reader = recorded.as_slice();
        let mut received = Vec::new();
        while let Some(msg) = ToEngine::read_from(&mut reader).unwrap() {
            received.push(msg);
        }
        assert_eq!(received, sent);
    }

    /// Runs an engine that, like `sig-servo` after a panic, says what went wrong and exits: what
    /// it said stays visible.
    #[cfg(unix)]
    #[test]
    fn shows_why_an_engine_exited() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");
        let error = FromEngine::Error {
            message: "crashed: boom".into(),
        };
        std::fs::write(&output, encode(&[hello(VERSION), error])).unwrap();
        let script = r#"cat "$0"; exit 101"#;
        let command = ["sh", "-c", script, output.to_str().unwrap()].map(String::from);

        let (_process, events) = Process::spawn(&command, HELLO_TIMEOUT).unwrap();
        // What `Page::handle` does.
        let mut info = PageInfo::default();
        loop {
            match next_event(&events) {
                Event::Message(msg) => _ = info.apply(msg),
                Event::Exited(reason) => break info.exited("servo", &reason),
                _ => {}
            }
        }
        let shown = LoadState::Error("Engine \"servo\" exited: crashed: boom".into());
        assert_eq!(info.load, shown);
    }

    /// Runs an engine that never says Hello: it is given up on, and dropping it stops it.
    #[cfg(unix)]
    #[test]
    fn gives_up_on_engines_that_do_not_say_hello() {
        let silent = ["sh", "-c", "cat > /dev/null"].map(String::from);
        let (process, events) = Process::spawn(&silent, Duration::from_millis(200)).unwrap();
        let Event::Exited(reason) = next_event(&events) else {
            panic!("the engine was not given up on");
        };
        let mut info = PageInfo::default();
        info.exited("fake", &reason);
        let shown = "Engine \"fake\" did not start: no Hello within 0.2 s.";
        assert_eq!(info.load, LoadState::Error(shown.into()));

        // What the page does next.
        drop(process);
        assert!(matches!(next_event(&events), Event::Exited(reason) if reason == "exited"));
        assert!(events.recv_blocking().is_err());
    }

    #[cfg(unix)]
    fn next_event(events: &async_channel::Receiver<Event>) -> Event {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match events.try_recv() {
                Ok(event) => return event,
                Err(async_channel::TryRecvError::Empty) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("no event from the engine: {e}"),
            }
        }
    }
}
