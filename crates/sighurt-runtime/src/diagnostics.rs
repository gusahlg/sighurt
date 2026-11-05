//! The measurements that show whether the architecture holds, printed on demand and at exit.
//!
//! Counts loop wakeups and iterations, input events, core events and effects, engine pumps and
//! their events, frame-ready notifications, page frames presented (and not ready), chrome
//! paints per surface (and chrome frames found unchanged), placements, and the deepest queues
//! seen; counts and times the placements and chrome snapshots a frontend builds; times the
//! longest pump, present and chrome paint (and apart, the chrome's drawing of its scene and the
//! renderer's rendering of it); keeps the most SHM buffers any target, and any of the chrome's
//! targets, held, sampled after every presentation (where buffers are allocated). The host adds,
//! per target, its metrics and its permits, submissions, frame callbacks, buffer releases and
//! presentation feedback, kept distinct (submission is not display), and its buffers and bytes.
//! Each engine is listed with how it renders (for Servo, the actual GL renderer and whether it
//! is a software fallback), whether it runs, and the page readbacks it counted itself.
//!
//! Page readbacks are the engines' own counts (Servo counts every `read_to_image`; `sig` has
//! no readback path of its own), taken after each frame an engine submits and at each report:
//! `page_readbacks` is their number and `readback_bytes` their size, each counted as a whole
//! buffer of the frame it came with (or of the page area, for one found at a report).
//!
//! Output, all machine-readable:
//!
//! - with `SIG_DIAG=1`, one stderr line per page frame and chrome paint, in the format
//!   `bench/benchlib/parsers.py` (`SigDiag`) reads: `sig-diag t=<CLOCK_MONOTONIC s> <kind>
//!   key=value ...`, where a `frame` line's `paint_ms` is the engine's own painting (Servo's
//!   WebRender paint; for the CPU engines the whole `present`) and `present_ms` the `present`
//!   call; a `renderer` line for each engine at its first frame (`mode=cpu`, or
//!   `mode=hardware|software gl="<GL_RENDERER>"`); a `first-frame` line for a navigation
//!   started in the active tab, at the view's first frame after it; a `chrome` line per chrome
//!   frame, whose `draw_ms` is the whole of it, `scene_ms` the chrome's drawing and `render_ms`
//!   the renderer's; and a `chrome-renderer` line when the window starts its renderer;
//! - on SIGUSR1 and at exit (at exit only with `SIG_DIAG=1`), a `counters` line (cumulative
//!   counts, as bench/ takes them), a `peaks` line (queue depths, longest calls), one line
//!   per target and engine, and a `chrome-renderer` line with what the renderer counted;
//! - with `--diagnostics FILE`, the same as JSON in FILE, rewritten on SIGUSR1 and at exit,
//!   with which tab is active, what the status strip shows, the fullscreen (`wanted`, the
//!   user's toggle; `window`, what the window is; `page`, an element of the active page) and
//!   each tab's browser state (loading, back and forward, a record, an error, the engine its
//!   tab is labelled with), never an address or a title.
//!
//! Never logs clipboard contents, credentials, URLs or other page data.
//!
//! Must not own: decisions based on these numbers, or a frontend's own counters.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

/// CLOCK_MONOTONIC in seconds: the clock of the `t=` field, which bench/ compares with its own.
pub fn monotonic() -> f64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    now.tv_sec as f64 + now.tv_nsec as f64 / 1e9
}

/// Cumulative counts since `sig` started, never reset.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Counters {
    /// Times the loop woke up from waiting.
    pub wakeups: u64,
    /// Loop iterations, with or without waiting before them.
    pub iterations: u64,
    /// Input events from the host.
    pub input_events: u64,
    /// Pointer motion events dropped because a newer one for the same target followed.
    pub motions_coalesced: u64,
    /// Events handed to the core.
    pub core_events: u64,
    /// Effects the core returned.
    pub effects: u64,
    /// Engine pumps.
    pub pumps: u64,
    /// Events engines reported.
    pub engine_events: u64,
    /// `FrameReady` events.
    pub frame_ready: u64,
    /// Page frames an engine painted. The same as `page_frames_presented` in `sig`: an engine
    /// reports a frame only by submitting it (a frame it painted and threw away is its own).
    pub page_frames_painted: u64,
    /// Page frames an engine submitted to the compositor (not proof they were displayed).
    pub page_frames_presented: u64,
    /// Permits an engine had no frame for yet.
    pub page_not_ready: u64,
    /// Permits an engine had nothing new for.
    pub page_unchanged: u64,
    /// `present` calls that failed.
    pub present_errors: u64,
    /// Page readbacks to the CPU the engines counted.
    pub page_readbacks: u64,
    /// Their bytes, each readback counted as a whole buffer of its page.
    pub readback_bytes: u64,
    /// Chrome-drawn surfaces painted, all of them.
    pub chrome_paints: u64,
    /// The standard chrome's surface at the top ("chrome") painted.
    pub paints_chrome: u64,
    /// Its status strip ("status") painted.
    pub paints_status: u64,
    /// A page the chrome draws (`sig://home`, errors: "page") painted.
    pub paints_page: u64,
    /// Chrome frames whose scene was the surface's last: nothing rendered or committed.
    pub chrome_unchanged: u64,
    /// Chrome renders the renderer put off (no swapchain image in time): the target was
    /// invalidated for a new permit.
    pub chrome_deferred: u64,
    /// Times the chrome's renderer was replaced by the CPU one.
    pub renderer_fallbacks: u64,
    /// Placements applied.
    pub placements: u64,
    /// Placements the host refused.
    pub placement_errors: u64,
    /// Placements a frontend built to compare with the one applied last (applied or not).
    pub placement_builds: u64,
    /// Nanoseconds spent building and comparing them, all together.
    pub placement_ns: u64,
    /// Snapshots of the core built for the chrome.
    pub snapshots: u64,
    /// Nanoseconds spent building them (containing every string included), all together.
    pub snapshot_ns: u64,
    /// Configuration reloads, valid or not.
    pub config_reloads: u64,
    /// Session snapshots handed to the writer.
    pub session_saves: u64,
    /// Visits recorded in the history (`Effect::Visited` commits it kept).
    pub visits_recorded: u64,
    /// History snapshots handed to its writer, forgets included.
    pub history_saves: u64,
    /// Bytes of page frames a frontend encoded to send on. The terminal counts each kitty
    /// image it handed to the output buffer. A frontend that doesn't encode leaves this at 0.
    pub encoded_bytes: u64,
    /// Page frames a frontend dropped before they were shown: the encode didn't fit, or the
    /// reader was behind. Distinct from `page_frames_presented`, which counts submissions.
    pub frames_dropped: u64,
}

/// The largest values seen since `sig` started: queue depths and the longest calls.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Peaks {
    /// The most host events handled in one pass.
    pub max_host_queue: u64,
    /// The most events one engine pump reported.
    pub max_pump_events: u64,
    /// The longest engine pump, in microseconds.
    pub max_pump_us: u64,
    /// The longest `present`, in microseconds.
    pub max_present_us: u64,
    /// The longest chrome paint, in microseconds: its drawing and rendering together.
    pub max_chrome_paint_us: u64,
    /// The longest drawing of a chrome surface's scene, in microseconds.
    pub max_chrome_scene_us: u64,
    /// The longest rendering of a chrome surface's scene, in microseconds.
    pub max_chrome_render_us: u64,
    /// The most SHM buffers one target held, of every generation.
    pub max_target_buffers: u64,
    /// The most SHM buffers one of the chrome's targets held, of every generation.
    pub max_chrome_buffers: u64,
    /// The most superseded generations one target still held.
    pub max_retiring_generations: u64,
    /// The most bytes of SHM buffers all targets held together.
    pub max_buffer_bytes: u64,
}

/// One target, as the host and `sig` see it.
#[derive(Clone, Debug, Serialize)]
pub struct TargetReport {
    /// What it is: the name of the chrome's surface ("chrome", "status" and "page" for the
    /// standard chrome), "view", or "retiring".
    pub role: &'static str,
    /// For a view, its engine and its tab's position in the tab strip, from 1.
    pub engine: Option<String>,
    pub tab: Option<usize>,
    /// Logical size and buffer size, and the scale's numerator over 120.
    pub width: u32,
    pub height: u32,
    pub buffer_width: u32,
    pub buffer_height: u32,
    pub scale: u32,
    pub mapped: bool,
    pub shown: bool,
    pub awaiting_callback: bool,
    pub permits: u64,
    pub submissions: u64,
    pub frame_callbacks: u64,
    pub releases: u64,
    pub presented: u64,
    pub discarded: u64,
    pub late: u64,
    pub buffers: usize,
    pub buffer_bytes: usize,
    pub retiring_generations: usize,
    pub retiring_bytes: usize,
}

/// One compiled-in engine.
#[derive(Clone, Debug, Serialize)]
pub struct EngineReport {
    pub name: &'static str,
    /// How it renders and presents.
    pub renderer: &'static str,
    /// For a GL engine once it has a context: "hardware" or "software", and the GL renderer.
    pub gl: Option<String>,
    /// Page readbacks to the CPU the engine counted (Servo's `read_to_image`, and its GL reads
    /// when auditing); 0 on the display path. The CPU engines count their copies here: CEF's
    /// staged bytes, WPE's shared-memory fallback copies, Ladybird's frames copied.
    pub readbacks: u64,
    /// The `glReadPixels`/`glGetTexImage` calls among them, when the engine audits its GL calls
    /// (Servo with `SIG_AUDIT_GL=1`); `None` otherwise.
    pub gl_reads: Option<u64>,
    /// "not started", "running", "shut down" or "failed: why".
    pub state: String,
}

/// The renderer that draws the chrome (`[ui] renderer`), and what it counted. A measurement
/// the renderer can't make is `None`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RendererReport {
    /// Its name: "cpu", "wgpu", "vulkan".
    pub name: &'static str,
    /// What it runs on, as it says.
    pub describe: String,
    /// Frames it rendered.
    pub frames: u64,
    /// Renders that found the surface's last frame current.
    pub unchanged: u64,
    /// Renders it put off.
    pub deferred: u64,
    /// Its longest render, in microseconds.
    pub max_render_us: u64,
    /// Its latest render, in microseconds.
    pub last_render_us: u64,
    /// Rasterized glyphs it holds.
    pub glyphs: u64,
    /// Bytes of glyph images it holds on the CPU.
    pub glyph_bytes: u64,
    /// Times its glyph cache or atlas started over.
    pub glyph_resets: u64,
    /// Native surfaces it has bound.
    pub surfaces: u64,
    /// Its atlas pages' sides, and their bytes as textures.
    pub atlas_mask_size: Option<u32>,
    pub atlas_color_size: Option<u32>,
    pub atlas_bytes: Option<u64>,
    /// Times an atlas page grew, started over, or a frame overflowed it.
    pub atlas_grows: Option<u64>,
    pub atlas_resets: Option<u64>,
    pub atlas_overflows: Option<u64>,
    /// Swapchain images per surface, when the API says.
    pub swapchain_images: Option<u32>,
    /// Device memory of its own resources, as it can tell.
    pub device_bytes: Option<u64>,
    /// Swapchain (re)configurations.
    pub configures: u64,
    /// Its longest bind, configure and unbind, in microseconds.
    pub max_bind_us: Option<u64>,
    pub max_configure_us: Option<u64>,
    pub max_unbind_us: Option<u64>,
    /// Times the frontend replaced its renderer with the CPU one, and the last reason.
    pub fallbacks: u64,
    pub fallback: Option<String>,
    /// The renderer it replaced, and the frames that one rendered.
    pub replaced: Option<&'static str>,
    pub replaced_frames: u64,
}

/// One tab, as the core sees it: browser state only, never its address or title.
#[derive(Clone, Debug, Serialize)]
pub struct TabReport {
    /// A navigation is under way or the page is loading.
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// It shows a record, which loads again when shown.
    pub reloads: bool,
    /// Its latest navigation failed, or its view did.
    pub error: bool,
    /// The engine the chrome is told its page runs in (`TabSnapshot::engine`); none for `sig:`
    /// pages, blank tabs and errors.
    pub engine: Option<String>,
    /// The search in its page, if one is under way: what it found, never what it looks for.
    pub find: Option<FindReport>,
}

/// What a tab's search found (`TabSummary::find`), without its text.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct FindReport {
    pub found: Option<bool>,
    pub current: Option<u32>,
    pub total: Option<u32>,
    pub supported: bool,
}

impl From<&sighurt_core::tabs::Find> for FindReport {
    fn from(find: &sighurt_core::tabs::Find) -> Self {
        Self {
            found: find.found,
            current: find.current,
            total: find.total,
            supported: find.supported,
        }
    }
}

/// The window's fullscreen and the page's (`BrowserState::fullscreen`).
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct FullscreenReport {
    /// The user's toggle (`browser.fullscreen`).
    pub wanted: bool,
    /// What the frontend last reported the window to be.
    pub window: bool,
    /// An element of the active tab's page is fullscreen.
    pub page: bool,
}

impl From<sighurt_core::fullscreen::FullscreenState> for FullscreenReport {
    fn from(state: sighurt_core::fullscreen::FullscreenState) -> Self {
        Self {
            wanted: state.wanted,
            window: state.window,
            page: state.page.is_some(),
        }
    }
}

/// Everything printed at once.
pub struct Report {
    pub counters: Counters,
    pub peaks: Peaks,
    pub targets: Vec<TargetReport>,
    pub engines: Vec<EngineReport>,
    /// The chrome's renderer; `None` in the terminal, which draws no such chrome.
    pub renderer: Option<RendererReport>,
    /// The tabs, in the tab strip's order.
    pub tabs: Vec<TabReport>,
    /// The active tab's position in the tab strip, from 1.
    pub active_tab: usize,
    /// What the status strip shows: "notice", "page" or "error" (never the text itself).
    pub status: Option<&'static str>,
    /// The window's fullscreen and the page's.
    pub fullscreen: FullscreenReport,
    /// The compositor's optional protocols, as the host reports them.
    pub capabilities: String,
    /// Host events not taken yet.
    pub pending_host_events: usize,
    pub last_error: Option<String>,
}

/// Where diagnostics go.
pub struct Diagnostics {
    /// `SIG_DIAG=1`: frame and paint lines on stderr.
    lines: bool,
    /// `--diagnostics FILE`.
    file: Option<PathBuf>,
    pub counters: Counters,
    pub peaks: Peaks,
    last_error: Option<String>,
    /// The keymap's indicator at the last report (the frontend sets it; the terminal doesn't).
    pub keymap_indicator: Option<String>,
    /// Each engine's readback count already accounted for.
    readbacks_seen: BTreeMap<String, u64>,
    /// The engines whose renderer line was printed.
    renderers: BTreeSet<String>,
}

impl Diagnostics {
    pub fn new(file: Option<PathBuf>) -> Self {
        Self {
            lines: std::env::var_os("SIG_DIAG").is_some_and(|v| v == "1"),
            file,
            counters: Counters::default(),
            peaks: Peaks::default(),
            last_error: None,
            keymap_indicator: None,
            readbacks_seen: BTreeMap::new(),
            renderers: BTreeSet::new(),
        }
    }

    /// Records an error for the report, and logs it.
    pub fn error(&mut self, error: String) {
        log::warn!("{error}");
        self.last_error = Some(error);
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.clone()
    }

    /// One line on stderr. A line that can't be written (a full disk, a closed pipe, the file
    /// size limit) is lost: `eprintln!` would panic.
    fn line(&self, kind: &str, fields: std::fmt::Arguments<'_>) {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "sig-diag t={:.6} {kind} {fields}",
            monotonic()
        );
    }

    /// Whether `engine`'s renderer line was printed.
    pub fn knows_renderer(&self, engine: &str) -> bool {
        self.renderers.contains(engine)
    }

    /// How `engine` renders: `mode` is "cpu", "hardware" or "software", `gl` the GL renderer.
    /// Printed once per engine.
    pub fn renderer(&mut self, engine: &str, mode: &str, gl: Option<&str>) {
        if !self.renderers.insert(engine.to_owned()) || !self.lines {
            return;
        }
        match gl {
            Some(gl) => self.line(
                "renderer",
                format_args!("engine={engine} mode={mode} gl={gl:?}"),
            ),
            None => self.line("renderer", format_args!("engine={engine} mode={mode}")),
        }
    }

    /// `engine` has counted `total` page readbacks so far: counts the new ones, each as
    /// `bytes_each`, and returns their bytes.
    ///
    /// An engine already in the map is updated in place. Its name is copied only the first time
    /// it is seen.
    pub fn readbacks(&mut self, engine: &str, total: u64, bytes_each: u64) -> u64 {
        let new = if let Some(seen) = self.readbacks_seen.get_mut(engine) {
            let new = total.saturating_sub(*seen);
            *seen = total;
            new
        } else {
            self.readbacks_seen.insert(engine.to_owned(), total);
            total
        };
        let bytes = new.saturating_mul(bytes_each);
        self.counters.page_readbacks += new;
        self.counters.readback_bytes += bytes;
        bytes
    }

    /// A page frame an engine submitted: how long the engine painted it and how long the
    /// `present` call took (the same for the CPU engines; for Servo the paint is WebRender's,
    /// without the spin and the swap), with the bytes of the readbacks counted since the
    /// engine's previous frame.
    pub fn frame(
        &mut self,
        engine: &str,
        view: u64,
        size: (u32, u32),
        (paint, present): (Duration, Duration),
        readback_bytes: u64,
    ) {
        self.counters.page_frames_painted += 1;
        self.counters.page_frames_presented += 1;
        self.peaks.max_present_us = self.peaks.max_present_us.max(micros(present));
        if self.lines {
            self.line(
                "frame",
                format_args!(
                    "engine={engine} view={view} w={} h={} presented=1 readback_bytes={readback_bytes} paint_ms={:.3} present_ms={:.3}",
                    size.0,
                    size.1,
                    paint.as_secs_f64() * 1e3,
                    present.as_secs_f64() * 1e3
                ),
            );
        }
    }

    /// The SHM buffers the targets hold now, per target (buffers, retiring generations,
    /// whether it is the chrome's) and in bytes altogether: keeps the largest.
    pub fn buffers(
        &mut self,
        per_target: impl Iterator<Item = (usize, usize, bool)>,
        bytes: usize,
    ) {
        let p = &mut self.peaks;
        for (buffers, retiring, chrome) in per_target {
            p.max_target_buffers = p.max_target_buffers.max(buffers as u64);
            p.max_retiring_generations = p.max_retiring_generations.max(retiring as u64);
            if chrome {
                p.max_chrome_buffers = p.max_chrome_buffers.max(buffers as u64);
            }
        }
        p.max_buffer_bytes = p.max_buffer_bytes.max(bytes as u64);
    }

    /// A view's first frame after it was asked to navigate, `after` the request.
    pub fn first_frame(&self, engine: &str, after: Duration) {
        if self.lines {
            self.line(
                "first-frame",
                format_args!(
                    "engine={engine} after_navigate_ms={:.3}",
                    after.as_secs_f64() * 1e3
                ),
            );
        }
    }

    /// The chrome's renderer started: its name and what it runs on.
    pub fn chrome_renderer(&self, name: &str, describe: &str) {
        if self.lines {
            self.line(
                "chrome-renderer",
                format_args!("renderer={name} describe={describe:?}"),
            );
        }
    }

    /// A chrome-drawn surface painted: the chrome drew its scene in `scene`, the renderer
    /// rendered it in `render`.
    pub fn chrome(&mut self, surface: &'static str, scene: Duration, render: Duration) {
        let draw = scene + render;
        let c = &mut self.counters;
        c.chrome_paints += 1;
        match surface {
            "chrome" => c.paints_chrome += 1,
            "status" => c.paints_status += 1,
            "page" => c.paints_page += 1,
            _ => {}
        }
        let p = &mut self.peaks;
        p.max_chrome_paint_us = p.max_chrome_paint_us.max(micros(draw));
        p.max_chrome_scene_us = p.max_chrome_scene_us.max(micros(scene));
        p.max_chrome_render_us = p.max_chrome_render_us.max(micros(render));
        if self.lines {
            let ms = |d: Duration| d.as_secs_f64() * 1e3;
            self.line(
                "chrome",
                format_args!(
                    "surface={surface} draw_ms={:.3} scene_ms={:.3} render_ms={:.3}",
                    ms(draw),
                    ms(scene),
                    ms(render)
                ),
            );
        }
    }

    /// A frontend built a placement to compare with the last one, which took `took`.
    pub fn placement_built(&mut self, took: Duration) {
        self.counters.placement_builds += 1;
        self.counters.placement_ns = self.counters.placement_ns.saturating_add(nanos(took));
    }

    /// A frontend built a snapshot of the core for its chrome, which took `took`.
    pub fn snapshot_built(&mut self, took: Duration) {
        self.counters.snapshots += 1;
        self.counters.snapshot_ns = self.counters.snapshot_ns.saturating_add(nanos(took));
    }

    /// The renderer put off a chrome surface's frame; the target waits for a new permit.
    pub fn chrome_deferred(&mut self) {
        self.counters.chrome_deferred += 1;
    }

    /// The chrome's renderer `from` could not go on (`why`) and `to` replaced it.
    pub fn renderer_fallback(&mut self, from: &str, to: &str, why: &str) {
        self.counters.renderer_fallbacks += 1;
        log::warn!("the chrome's {from} renderer stopped ({why}); the chrome is drawn by {to}");
        if self.lines {
            self.line(
                "chrome-renderer-fallback",
                format_args!("from={from} to={to} why={why:?}"),
            );
        }
    }

    /// A chrome-drawn surface's scene was its last frame's: nothing rendered or committed.
    pub fn chrome_unchanged(&mut self, scene: Duration) {
        self.counters.chrome_unchanged += 1;
        let p = &mut self.peaks;
        p.max_chrome_scene_us = p.max_chrome_scene_us.max(micros(scene));
    }

    /// An engine pump took `took` and reported `events`.
    pub fn pump(&mut self, events: usize, took: Duration) {
        self.counters.pumps += 1;
        self.counters.engine_events += events as u64;
        let p = &mut self.peaks;
        p.max_pump_events = p.max_pump_events.max(events as u64);
        p.max_pump_us = p.max_pump_us.max(micros(took));
    }

    /// Prints `report` on stderr (on SIGUSR1; at exit with `SIG_DIAG=1`) and writes the file.
    pub fn report(&self, report: &Report, at_exit: bool) {
        if !at_exit || self.lines {
            let counters = serde_json::to_value(&report.counters).expect("plain counters");
            self.line("counters", format_args!("{}", pairs(&counters)));
            let peaks = serde_json::to_value(&report.peaks).expect("plain numbers");
            self.line("peaks", format_args!("{}", pairs(&peaks)));
            for target in &report.targets {
                let value = serde_json::to_value(target).expect("plain data");
                self.line("target", format_args!("{}", pairs(&value)));
            }
            for engine in &report.engines {
                let value = serde_json::to_value(engine).expect("plain data");
                self.line("engine", format_args!("{}", pairs(&value)));
            }
            if let Some(renderer) = &report.renderer {
                let value = serde_json::to_value(renderer).expect("plain data");
                self.line("chrome-renderer", format_args!("{}", pairs(&value)));
            }
        }
        if let Some(file) = &self.file {
            let value = json!({
                "t": monotonic(),
                "exit": at_exit,
                "counters": report.counters,
                "peaks": report.peaks,
                "targets": report.targets,
                "engines": report.engines,
                "renderer": report.renderer,
                "tabs": report.tabs,
                "active_tab": report.active_tab,
                "status": report.status,
                "fullscreen": report.fullscreen,
                "keymap_indicator": self.keymap_indicator,
                "capabilities": report.capabilities,
                "pending_host_events": report.pending_host_events,
                "last_error": report.last_error,
            });
            let text = serde_json::to_string_pretty(&value).expect("plain data");
            if let Err(error) = std::fs::write(file, text + "\n") {
                log::warn!(
                    "cannot write the diagnostics to {}: {error}",
                    file.display()
                );
            }
        }
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// An object's fields as `key=value` pairs, strings quoted when they need it, booleans 0 or 1.
fn pairs(object: &Value) -> String {
    let Value::Object(fields) = object else {
        return String::new();
    };
    let mut out = Vec::with_capacity(fields.len());
    for (key, value) in fields {
        let value = match value {
            Value::Null => continue,
            Value::Bool(b) => u8::from(*b).to_string(),
            Value::Number(n) => n.to_string(),
            Value::String(s) if !s.is_empty() && !s.contains([' ', '"', '\'', '\\']) => s.clone(),
            Value::String(s) => format!("{s:?}"),
            other => format!("{:?}", other.to_string()),
        };
        out.push(format!("{key}={value}"));
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_are_what_the_bench_parser_splits() {
        let value = json!({"a": 1, "b": true, "c": "two words", "d": null, "e": "x"});
        assert_eq!(pairs(&value), r#"a=1 b=1 c="two words" e=x"#);
    }

    #[test]
    fn readbacks_are_counted_once_each() {
        let mut diag = Diagnostics::new(None);
        assert_eq!(diag.readbacks("servo", 0, 400), 0);
        assert_eq!(diag.readbacks("servo", 2, 400), 800);
        assert_eq!(diag.readbacks("servo", 2, 400), 0);
        assert_eq!(diag.readbacks("demo", 1, 100), 100);
        assert_eq!(diag.readbacks("servo", 3, 50), 50);
        assert_eq!(diag.counters.page_readbacks, 4);
        assert_eq!(diag.counters.readback_bytes, 950);
    }

    #[test]
    fn buffer_peaks_keep_the_largest() {
        let mut diag = Diagnostics::new(None);
        diag.buffers([(2, 0, true), (3, 1, false)].into_iter(), 1000);
        diag.buffers([(1, 2, true)].into_iter(), 500);
        let p = &diag.peaks;
        assert_eq!(
            (
                p.max_target_buffers,
                p.max_chrome_buffers,
                p.max_retiring_generations,
                p.max_buffer_bytes
            ),
            (3, 2, 2, 1000)
        );
    }

    #[test]
    fn the_clock_is_monotonic_seconds() {
        let (a, b) = (monotonic(), monotonic());
        assert!(a > 0.0 && b >= a);
    }
}
