# The render framework: `sighurt-render` and its backends

Status: **design, 3 November 2026** (Claude Fable 5.1; reviewed by the `deep-design` agent (Fable)
and GPT-6 Astra; **§14 holds their amendments and overrides the body where they differ**). Supersedes the painting half of the chrome API in
`extension-api.md` §2.8 ("Canvas" and "Pen" as CPU painting into `PixelBuffer`) once
implemented; the rest of that design stands.

## 1. Why

Today the chrome is drawn by one library: `sighurt_ui::canvas` (tiny-skia for shapes, swash
through cosmic-text for glyphs) into shared-memory buffers the Wayland host lends
(`PixelBuffer`). A chrome's `paint` *is* a CPU paint: the trait takes pixel memory, the chrome
names tiny-skia's `PathBuilder`, and the executable's only way to show a chrome surface is an
SHM target. Nothing else can draw the chrome: not a GPU, not a future Metal host, not a test
that wants the drawing as data.

The user wants control over rendering and a modular rendering part: a framework in which
several backends can be written and mixed, none of them visible to browser code, with a
substantial wgpu backend as the primary one, a pure Vulkan one beside it, and room for Metal.

This design separates **what is drawn** (a scene, built by a chrome in window-logical
coordinates) from **how it is rendered and presented** (a backend bound to a host target). The
chrome API loses every pixel and tiny-skia type. The browser (core, runtime, GUI loop) learns
one new name, `[ui] renderer`, and one registry, like the ones for engines, keymaps and chromes.

Non-goals: the engines' own rendering (Servo's GL, WPE's DMA-BUFs, the CPU engines' frames) is
untouched; the terminal frontend's chrome is cells and untouched; no readback path is added to
the normal display path (plan §26).

## 2. Layers and crates

```text
render/
├── sighurt-render          the scene, the pen, text shaping, glyph rasterization,
│                           the Backend trait, the CPU backend (always built),
│                           the GPU preparation shared by GPU backends (feature `gpu`)
├── sighurt-render-wgpu     Backend on wgpu 30, Vulkan only (§14.1)
└── sighurt-render-vulkan   Backend on ash (VK_KHR_wayland_surface), no wgpu
```

| Crate | Owns | Must not own | Depends on |
|---|---|---|---|
| `sighurt-render` | `scene` (items, clips, snapping), `pen` (the builder a chrome draws with), `text` (`Fonts`, `ShapedText`, `TextStyle`, moved from sighurt-ui), `glyph` (rasterizing one glyph key with swash, shared by every backend), `color` (`Rgba`, moved), `backend` (the trait, outputs, errors), `cpu` (the CPU backend: today's canvas code), `gpu` (feature: mesh building, atlas planning, the WGSL shaders) | Host targets, Wayland objects, what is drawn, browser state | sighurt-api, cosmic-text, tiny-skia |
| `sighurt-render-wgpu` | Its instance, adapter, device, queue; one `wgpu::Surface` per bound target; uploading meshes and atlas rows; presenting | Scene semantics, snapping, glyph rasterization, target lifetime | sighurt-render (`gpu`), wgpu, raw-window-handle |
| `sighurt-render-vulkan` | Its instance, physical/logical device, one swapchain per bound target, pipelines, descriptor sets, buffers and images; SPIR-V compiled from the shared WGSL by naga in `build.rs` | Same as above | sighurt-render (`gpu`), ash, raw-window-handle |
| `sighurt-ui` | As today minus painting: `canvas.rs` and the CPU half of `paint_cpu.rs` go; `paint.rs` builds scenes with a `Pen`. Re-exports `sighurt_render::{Pen, Rgba, Fonts, ShapedText, TextStyle}` for the chrome API | Any backend | sighurt-api, sighurt-render |
| `sighurt-gui` | `render.rs`: `RENDERERS`, choosing and starting the backend, binding a chrome surface's target to it (SHM lease or native handles), rendering on a permit, fallback, shutdown order | Scene or backend internals | + sighurt-render, the backend crates behind features |
| `sighurt-core` | Validates `[ui] renderer` against the build's list in `Environment`, as it does `[ui] chrome` | Anything else of rendering | — |

Rules:

- Backends never depend on `sighurt-wayland`. A native backend receives
  `raw_window_handle::{HasDisplayHandle, HasWindowHandle}` (what `TargetLease::gl_handles`
  already implements for Servo). That is what makes a Metal backend possible without a Wayland
  host: the host hands AppKit handles the same way.
- `sighurt-render` is the only crate that names tiny-skia or cosmic-text. A chrome names
  neither. (Decision 10, "sighurt-ui depends on sighurt-api only", becomes "on sighurt-api and
  sighurt-render": the render crate is the chrome's drawing library, as tiny-skia was.)
- The runtime (`sighurt-runtime`) stays frontend-neutral and knows nothing of rendering; the
  terminal frontend never depends on `sighurt-render`.
- Features on `sighurt-browser` and `sighurt-gui`: `render-wgpu`, `render-vulkan`. The CPU
  backend is in every build. `render-wgpu` is compiled into the default set (the user's choice:
  wgpu is the primary GPU backend); which backend `auto` *selects* is decided by measurement
  (§14.5); `render-vulkan` is opt-in.

## 3. The scene

A `Scene` is the drawing of one surface for one frame: a list of items in draw order, in
window-logical coordinates, with a clip rectangle per item. It is plain data: `Clone`,
`PartialEq`, no handles, no lifetimes. It can be compared to the previous frame's scene (damage),
rendered by any backend, and asserted on in tests.

```rust
pub struct Scene {
    /// What the frame starts from. Every scene clears: a buffer may hold an older frame.
    pub clear: Rgba,
    pub items: Vec<Item>,
    /// Glyph runs referenced by `Item::Text`, kept separate so items stay small.
    pub runs: Vec<GlyphRun>,
    /// Paths referenced by `Item::Path`.
    pub paths: Vec<Path>,
}

pub struct Item {
    pub kind: Kind,
    /// Cut to this logical rectangle; `None` is the surface.
    pub clip: Option<Rect>,
}

pub enum Kind {
    /// A rectangle, each edge snapped to the pixel grid.
    Rect { rect: Rect, color: Rgba },
    /// A rectangle with rounded corners, `radius` logical pixels; snapped like `Rect`.
    RoundedRect { rect: Rect, radius: f32, color: Rgba },
    /// A rounded rectangle with a one-pixel border (`border`) around `fill`.
    Bordered { rect: Rect, radius: f32, fill: Rgba, border: Rgba },
    /// A filled or stroked path (icons). Logical coordinates, unsnapped.
    Path { path: PathId, color: Rgba, stroke: Option<f32> },
    /// Positioned glyphs, one color.
    Text { run: RunId, color: Rgba },
}

pub struct GlyphRun {
    /// Font size in logical pixels, as shaped.
    pub font_size: f32,
    pub glyphs: Vec<Glyph>,
}

pub struct Glyph {
    /// cosmic-text's font and glyph ids, valid in the `Fonts` the scene was built with.
    pub font: cosmic_text::fontdb::ID,
    pub glyph: u16,
    /// Logical position of the glyph origin: `x` from the surface's left, `y` the baseline.
    pub x: f32,
    pub y: f32,
    pub flags: cosmic_text::CacheKeyFlags,
}

pub struct Path { pub commands: Vec<PathCommand> }   // MoveTo, LineTo, QuadTo, CubicTo, Close
pub struct Rect { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }
```

**Coordinates and snapping.** A scene is in the *surface's* logical coordinates (origin at the
surface's top-left; the `Pen` subtracts the target's window origin, as today's `Pen` does with
`metrics.rect()`). Every backend maps logical to buffer pixels with the *same* function,
`scene::snap(rect, &Mapping) -> PixelRect`, where `Mapping` is built from the `Metrics` (ratio
from the actual buffer size, as `Pen::new` computes it, decision: "Scale is a rational"). So a
control has the same pixels under every backend, and the hit test (logical) stays on what is
drawn. Text baselines snap to whole buffer pixels; glyphs keep subpixel x positions (cosmic-text's
`CacheKey` x bins), as today.

**Text.** Shaping stays where the chrome has it: `Fonts::line/wrapped/fitted` return
`ShapedText` (a cosmic-text `Buffer`). `Pen::text(&ShapedText, position, color, clip)` walks the
layout runs and records a `GlyphRun` with each glyph's font id, glyph id, logical position and
flags: what `LayoutGlyph::physical` needs, minus the scale, which the backend applies when it
builds the `CacheKey` at its target's scale. Rasterization is one shared function,
`glyph::rasterize(&mut Fonts, key) -> Option<GlyphImage>` (swash through `SwashCache`,
hinted, as today), used by the CPU backend directly and by the atlas of the GPU backends.

**Fonts are shared, not owned by the chrome.** `Fonts` becomes a cheap handle
(`Rc<RefCell<FontSystem>>` plus the shaping settings); the frontend creates it once
(`Fonts::system()`, one font database load instead of one per chrome and per backend), hands a
clone to the chrome (`ChromeKind::new: fn(Fonts) -> Box<dyn Chrome>`, so a chrome no longer
loads fonts itself) and a clone to the backend. Everything is on the owner thread; `Fonts` is
neither `Send` nor `Sync`, and the trait is not either. `[ui] font` still goes through
`Fonts::set_family` on `Configure`, and now reaches the backend's rasterizer by construction.

**Paths.** The chrome's icons (arrows, the reload circle, plus, close) are a handful of short
paths. The scene keeps them as path commands; the CPU backend fills or strokes them with
tiny-skia; the GPU backends rasterize each (path, stroke, scale) once on the CPU into a mask
with tiny-skia and keep it in the atlas (§6). Icons are tiny, the result is identical to the
CPU backend's, and there is no tessellator. Rounded rectangles do not take this route: they are
a signed-distance quad in the shader.

**Images** (favicons, a page thumbnail) are not in this version; the item kind is reserved
(`Kind` is `#[non_exhaustive]`), and the atlas design (§6) already holds RGBA tiles.

## 4. The backend

```rust
/// Where a scene goes: what a backend renders into.
pub enum Output<'a> {
    /// CPU memory the host lends, ARGB8888 premultiplied, `metrics.buffer()` in size.
    Pixels(PixelBuffer<'a>),
    /// The native surface the backend bound with `bind`; the backend presents it itself.
    Native,
}

/// How a backend presents, so the frontend can make the right kind of target.
pub enum Presents { Pixels, Native }

pub trait Backend {
    fn name(&self) -> &'static str;
    fn presents(&self) -> Presents;
    /// What `sig --version` and the diagnostics say: adapter, driver, API version.
    fn describe(&self) -> String;

    /// Binds `surface` to a native window the host made for it. `Pixels` backends never get
    /// this call. The handles are valid for the call; what the backend creates from them is
    /// valid until `unbind`, which the frontend promises to call before the host destroys the
    /// surface (the GL presenter's rule, `TargetLease::gl_handles`).
    unsafe fn bind(&mut self, surface: SurfaceKey, window: &dyn NativeWindow) -> Result<(), RenderError>;  // §14.2
    fn unbind(&mut self, surface: SurfaceKey);

    /// Renders `scene` for `metrics` into `output`, whole. For `Native`, also presents it
    /// (one swap, never blocking on vsync: the host paces frames).
    fn render(
        &mut self,
        surface: SurfaceKey,
        metrics: &Metrics,
        scene: &Scene,
        output: Output<'_>,
    ) -> Result<Rendered, RenderError>;

    fn stats(&self) -> BackendStats;   // frames, render time, atlas size, memory, fallbacks
}

pub trait NativeWindow: HasDisplayHandle + HasWindowHandle {}

pub struct Rendered {
    /// Buffer pixels the frame changed, when the backend knows (`Pixels` with buffer age);
    /// `None` is the whole surface.
    pub damage: Option<Vec<PixelRect>>,
}

pub enum RenderError {
    /// Wrong output kind, wrong buffer size or layout (today's `PaintError`).
    Output(String),
    /// The device is lost or the surface is gone; the frontend falls back (§10).
    Lost(String),
}
```

`SurfaceKey` is the frontend's `TargetId` wrapped (an opaque `u64` newtype in the render crate:
the render crate does not name host ids). One backend serves every surface of a window. A
backend that fails to *start* (no adapter, no Vulkan loader) is never used; one that fails to
*bind* or *render* a surface with `Lost` is replaced for that surface (§10). Backends do no
work between calls: no threads, no timers, nothing that wakes the owner (§26, "no permanent
repaint timer").

Lifetimes, in the host's order (plan §20): a backend's per-surface objects die in `unbind`,
which the frontend calls before it drops the lease; the backend itself dies after every surface
is unbound and before the host is dropped. Nothing relies on `Drop` order for a native handle.

## 5. The CPU backend (`sighurt_render::cpu`)

*The incremental painting below is cut from lane 1 (§14.6); only the `scene == previous →
Unchanged` rule ships. The text stays as the record of the idea and its conditions.*

Today's `Canvas`/`Pen` code, moved and split: `Pen` is now the scene builder (§3); the pixel
work is `cpu::CpuBackend::render`, which walks the scene with tiny-skia and the shared glyph
rasterizer, swaps R and B once at the end (or through the padded-stride scratch pixmap, as
today) and returns the damage. Its glyph cache keeps today's budget (2048 glyphs, starting over
when the scale changes).

**Incremental painting (new, measured).** `ShmFrame::age()` says how many submissions ago the
buffer held a frame. The CPU backend keeps, per surface, the last scenes' damage for the last
3 submissions (the host's buffer bound). When `age` is within that window, it paints only the
union of the changed items' snapped rectangles since the frame the buffer holds
(`Scene::changed_since(&old) -> Vec<Rect>`: items compared positionally; an inserted or removed
item damages its rectangle and everything after it that moved) and reports that damage, so the
compositor uploads only that. When `age` is 0 or unknown it paints everything. This generalizes
the "tab-row damage" hypothesis (H-UI3) to every chrome and every surface. Keep it if the
measurements in §12 show it; otherwise ship whole-surface painting and record why.

## 6. GPU preparation, shared (`sighurt_render::gpu`, feature `gpu`)

Everything a GPU backend needs that is not an API call lives here, once:

- **`Mesh`**: the scene as quads in *buffer pixels*, snapped with the shared function. One
  vertex layout for every primitive: `position: vec2<f32>` (pixels), `uv: vec2<f32>`,
  `color: vec4<f32>` (premultiplied), `kind: u32` (solid, rounded-rect SDF, mask from atlas,
  color from atlas), `params: vec4<f32>` (for SDF: the rectangle's centre and half-size and the
  radius; for atlas tiles unused). A `Bordered` item is two SDF quads. One index buffer, one
  draw call per surface per frame, triangles in scene order, with a scissor rectangle per
  clip change (runs of items with one clip are one indexed range; a clip is `vkCmdSetScissor`
  / `set_scissor_rect`). Alpha blending premultiplied over the cleared background.
- **`Atlas`**: a CPU-side planner for one texture per format (R8 for glyph masks and icon
  masks, RGBA8 for color glyphs), shelf-packed, starting at 512², growing by doubling to a
  4096² bound, then starting over (the frame that overflows renders after a reset; the same
  "starts over" rule as the glyph cache). Entries are keyed by `CacheKey` (glyphs) or by
  (path id, stroke, scale) (icons). The planner records which rows changed; the backend
  uploads only those rows each frame. The backend never rasterizes: the planner asks
  `glyph::rasterize` and tiny-skia (for icons) and hands bytes.
- **`shaders.wgsl`**: one vertex and one fragment shader, the source as `&'static str`. wgpu
  consumes it directly. `sighurt-render-vulkan`'s `build.rs` compiles it to SPIR-V with naga
  (`wgsl-in`, `spv-out`), so the two GPU backends draw with the same program; a Metal backend
  would use naga's `msl-out` the same way. A unit test in `sighurt-render` validates the WGSL
  with naga's front end so a broken shader fails `cargo test`, not the first window.
- **`Frame`** planning: `gpu::prepare(scene, metrics, &mut atlas, &mut fonts) -> Prepared`
  (mesh + atlas uploads + scissor runs); the backends only upload and draw. A test renders a
  scene through `prepare` and checks quad counts, uvs inside the atlas, and that snapping
  agrees with the CPU backend's.

## 7. The wgpu backend (`sighurt-render-wgpu`)

- wgpu 30 (`Cargo.lock` gains entries and changes none that the Linux build uses: verified
  3 November, §14.15). `Instance` with `Backends::VULKAN` only (§14.1: never GL),
  adapter by `PowerPreference::LowPower` (the chrome is light; the discrete GPU is Servo's),
  one `Device` and `Queue` per backend.
- `bind` creates a `wgpu::Surface` from the host's handles
  (`Instance::create_surface_unsafe(SurfaceTargetUnsafe::from_window(window))`; the safety
  contract is the one in §4) and configures it at `metrics.buffer()` with
  `TextureFormat::Bgra8Unorm`, alpha `Opaque`, and present mode `Mailbox` if offered,
  else `Immediate`, else `Fifo`. Never block the owner thread on vsync: the host paces with
  frame callbacks and a hidden surface's vsync never comes (the Servo rule, swap interval 0).
- `render(Native)`: reconfigure if the size changed, `get_current_texture`, one render pass
  (clear to `scene.clear`, one pipeline, scissors per run), `queue.submit`, `present`. The
  present commits the `wl_surface` with the host's pending frame-callback request, as
  `eglSwapBuffers` does for Servo; the frontend then calls `permit.finish(Submitted)`.
  `SurfaceError::Lost | Outdated` → reconfigure once and retry; still failing → `RenderError::Lost`.
- `render(Pixels)` is refused (`Output`): no readback on the display path. (A test-only
  `render_to_image` under `cfg(test)`/a `testing` feature renders to a texture and maps it,
  for the backend comparison test in §12.)
- Atlas textures are `R8Unorm` and `Rgba8Unorm` with `write_texture` per changed row.
- `trim` drops the surface's configured textures (`surface.configure` at 1×1 is not needed:
  dropping and re-creating the `Surface` on the next `render` is simpler and bounded).

## 8. The Vulkan backend (`sighurt-render-vulkan`)

Pure Vulkan on `ash` (0.38), loading the loader at run time (`ash::Entry::load`), so the crate
compiles and `sig` starts without Vulkan; the backend reports it cannot start.

- Instance with `VK_KHR_surface` and `VK_KHR_wayland_surface`; physical device with a queue
  family that supports graphics and presenting to the surface; device with `VK_KHR_swapchain`.
- Per bound surface: `vkCreateWaylandSurfaceKHR` from the raw `wl_display` and `wl_surface`;
  a swapchain at `metrics.buffer()`, `B8G8R8A8_UNORM`, opaque composite alpha, present mode
  `MAILBOX` if available else `IMMEDIATE` else `FIFO`; minimum image count +1; images and views;
  one framebuffer per image in a render pass that clears.
- One graphics pipeline (the SPIR-V from `build.rs`), dynamic viewport and scissor, one
  descriptor set with the two atlas samplers, push constants for the ortho projection; vertex
  and index buffers in host-visible memory, resized by doubling; atlas images in device
  memory updated through a staging buffer and `vkCmdCopyBufferToImage` with the changed rows.
- Synchronization: one frame in flight per surface (a fence, an image-available and a
  render-finished semaphore). `render` waits for the previous frame's fence (it is a chrome;
  frames are rare and the host already paces them), acquires with a zero timeout where the
  swapchain allows it, records, submits, presents. `ERROR_OUT_OF_DATE` or `SUBOPTIMAL` →
  recreate the swapchain once; still failing → `Lost`.
- `unbind` waits for the device to be idle for that surface's fence, destroys the swapchain
  and the `VkSurfaceKHR`; the backend's `Drop` destroys the device and instance after every
  surface is unbound (the frontend's order, §4).
- Mesa's Wayland WSI commits the `wl_surface` in `vkQueuePresentKHR`, which carries the host's
  frame-callback request, exactly as the EGL path does. Validation layers on with
  `SIG_VULKAN_VALIDATION=1` (the layer is in the nix-shell's `vulkan-validation-layers` if
  present; otherwise a notice).

## 9. Future backends and uses

- **Metal**: `sighurt-render-metal` with the same `gpu` preparation and naga `msl-out`; it
  needs a host that hands AppKit handles. Nothing in the trait is Wayland's.
- **The Wasm engine's canvas** (`draw.rs`, `raster.rs`): its draw lists map onto `Scene`
  (rects, paths, text, images). Moving it onto `sighurt-render` would let a guest's canvas
  render on the GPU and share the glyph machinery; it is a separate decision after this lands.
- **Chrome animations** and the home page's art become cheap on a GPU backend; nothing here
  ticks, so an animation is still a sequence of `Repaint`s the chrome asks for.

## 10. In `sig`: choosing, binding, presenting, falling back

- `[ui] renderer = "auto" | "cpu" | "wgpu" | "vulkan"` (default `auto`), validated by the core
  against the build's list (`Environment::renderers`, like `chromes`); a name the build lacks is
  an error at load, as for a chrome; a change needs a restart, as `[ui] chrome` does.
  `auto` tries `wgpu`, then `vulkan`, then `cpu`: the first that *starts*.
- `sighurt-gui/src/render.rs`: `RENDERERS: &[RendererKind { name, start: fn(Fonts) ->
  Result<Box<dyn Backend>, String> }]`, the CPU one always, the others behind features;
  `start(config)`. The chosen backend's `presents()` decides the `PresentMode` of every chrome
  surface's target at creation (`Pixels` → `Shm`, `Native` → `Gl`, renamed `Native` in
  sighurt-wayland and its two engine users: the name was never GL-specific).
- On a permit for a chrome surface: build the scene (`chrome.draw(surface, &mut pen)`), then
  `backend.render(surface, metrics, &scene, output)`: for `Shm`, `permit.shm_frame()` gives the
  pixels and `submit_with_damage(rendered.damage)`; for `Native`, `Output::Native` then
  `permit.finish(Submitted)` (`Skipped` on an error, as the engines do). The chrome's time and
  the backend's time are two diagnostics.
- **Fallback (mixing).** The frontend holds the chosen backend and, if that is not the CPU one,
  a CPU backend too. `Lost` on bind or render for a surface: diagnostic, `unbind`, retire that
  target, create it again as `Shm`, and render it with the CPU backend from then on; the other
  surfaces stay where they are. So a window can show one surface on wgpu and another on the
  CPU, and browser code never knows: the per-surface backend choice is a field of
  `ChromeTargets`'s bookkeeping, nothing more. A backend that never starts is reported once in
  the diagnostics and `sig --version`-style output (`SIG_DIAG=1` prints the renderer and
  adapter at start).
- Shutdown (§20 order, `App::shutdown`): unbind every chrome surface, drop the backends, then
  retire the leases, then close the window and the host. A leaked engine still blocks dropping
  the host, as today; the render backends never do (they hold no Wayland object the host did
  not give them, and are gone before the host is).

## 11. The chrome API after this

In `sighurt_ui::extension`:

```rust
pub trait Chrome {
    fn handle(&mut self, event: ChromeEvent) -> Vec<ChromeAction>;
    fn layout(&self) -> &ChromeLayout;
    /// Draws `surface` with `pen`, whole: the surface's clear colour first (`Pen::clear`),
    /// then everything it shows. The pen's rectangle is the surface's (`Pen::rect`).
    fn draw(&mut self, surface: SurfaceName, pen: &mut Pen);
}
pub struct ChromeKind { name, check, commands, new: fn(Fonts) -> Box<dyn Chrome> }
```

`Pen` keeps today's methods (`clear`, `fill`, `fill_rounded`, `bordered`, `stroke`, `shape`,
`text`), minus the `&mut Fonts` argument of `text` (the fonts are shared) and plus `rect()`;
`stroke`/`shape` take a small `PathBuilder` of the render crate, not tiny-skia's. The standard
chrome's `paint_cpu.rs` becomes `paint.rs` with the same drawing functions over the new pen;
the line chrome changes its 17 pen calls the same way. `PixelBuffer`, `PaintError`, `Canvas`
and `CpuPainter` leave the chrome API. `extension-api.md` §2.8 and the painting contract in
its "contract between the executable and a chrome" are amended to say so (the memory bound
moves to §10 here for native targets: a swapchain is 2–3 images of the surface's size, the same
order as the SHM bound).

## 12. Diagnostics, tests and measurements

- Diagnostics (`SIG_DIAG=1`, `SIGUSR1` JSON): per chrome surface the backend name, frames,
  render time; per backend `describe()`, atlas size and resets, device memory where the API
  says, fallbacks taken.
- `sighurt-render`: scene tests (snapping agrees with today's `Pen` on fixture rectangles at
  scales 1, 1.25, 1.5, 2; `changed_since` damage on tab changes), glyph rasterization, the CPU
  backend against the existing golden images (`ui/sighurt-ui/tests/golden` moves to
  `render/sighurt-render/tests`, the chrome's scene tests stay in sighurt-ui), the WGSL
  validation test, the `gpu::prepare` tests.
- Backend comparison (needs a GPU; `tools/heavy`, skipped in CI): render the standard chrome's
  fixtures with the CPU backend and with wgpu's and Vulkan's test-only readback; every pixel
  within a small tolerance (text antialiasing is identical by construction, the SDF corners are
  the only difference allowed, bounded to the corner pixels).
- End to end: `apps/sighurt-browser/tests/headless.sh` runs the demo-engine `sig` once per
  compiled renderer (`[ui] renderer = "cpu"`, `"wgpu"`, `"vulkan"`) in the private sway with
  `--renderer gles2`, checks the chrome screenshots against the CPU run and the diagnostics'
  renderer name, and a forced fallback (`SIG_RENDER_FAIL=wgpu:chrome` makes the backend report
  `Lost` on that surface) ends with that surface on the CPU and the rest where they were.
  `engines.sh` runs with the default renderer so Servo's GL, WPE's DMA-BUFs and the chrome's
  Vulkan swapchains share one process and compositor.
- Measurements, before and after, the same chrome scenes: chrome render time per surface
  (diagnostics), `sig`'s CPU during the idle chrome (must stay 0 wakeups), during tab churn,
  and memory (RSS, and the GPU backends' device memory) — recorded in `bench/results/RENDER.md`
  with the machine and method. The GPU backends are not expected to make the chrome faster;
  they are expected not to make it slower to use, and to cost a bounded, stated amount of
  memory. The incremental CPU painting (§5) must show a measured gain to stay.

Acceptance criteria that must still hold (plan §26): zero routine readbacks on the display
path (the GPU backends render into the swapchain; `Pixels` is refused), zero chrome repaints
caused by page animation (nothing here adds a repaint source), no repaint timer, bounded
buffers (the swapchain's image count is the host's bound in kind), core tests need no device.

## 13. Plan

Lanes, each in its own worktree with its own `CARGO_TARGET_DIR` under
`/data/sig-builds/targets/<worktree>` (never the shared `target/`):

1. **`render-core`** (Opus): `render/sighurt-render` with `scene`, `pen`, `text` and `color`
   moved from sighurt-ui, `glyph`, `backend`, `cpu`; sighurt-ui on it (`paint.rs`, the
   extension trait's `draw`, `ChromeKind::new(Fonts)`, re-exports); the line chrome; sighurt-gui's
   `render.rs` with the CPU backend only, `[ui] renderer` in core and config; the rename
   `PresentMode::Gl → Native`; the §5 damage with its measurement. Acceptance: every existing
   chrome test and golden image passes, `headless.sh` passes, the chrome's diagnostics show the
   backend, and `Cargo.lock` gains nothing.
2. **`render-gpu`** (Opus, after 1's API is merged): `gpu` module (mesh, atlas, shaders,
   prepare, tests) and `sighurt-render-wgpu`; the features and `auto` order; the comparison
   test; the fallback test. **`Cargo.lock` rule:** adding wgpu 30 may only *add* entries. If
   cargo wants to change the version of anything already locked (Servo's dependencies), stop
   and pick the newest wgpu that fits the lock, or pin the conflicting dependency; never run a
   broad `cargo update`.
   *Status (4 November 2026, branch `render`):* done. `gpu` (`prepare`, `Atlas`, the vertex
   layout, `shaders.wgsl`) and `sighurt-render-wgpu` (`WgpuBackend`); `render-wgpu` in `sig`'s
   default set with `auto` still the CPU backend; the fallback (`SIG_RENDER_FAIL=device|surface`)
   and the comparison tests (`render/sighurt-render-wgpu/tests/compare.rs`, headless.sh
   `renderers`) pass on this desktop; measurements in `bench/results/RENDER.md`. The lock gained
   entries only. Lane 1's API took the Astra review's amendments first (font-system provenance,
   bounded glyph admission, shared path rasterization, `RenderOutcome`, extended `BackendStats`,
   `Pen::clip`; hide unbinds only `Pixels` backends). Deviation from §15: a `Deferred` frame
   invalidates its target 16 ms later (a one-shot timer in the loop's wait timeout), not at
   once, since `NotReady` leaves the frame request uncommitted and an immediate invalidation
   would let the loop spin on a renderer that keeps putting frames off (Astra's review of
   lane 2). Measured (`bench/results/RENDER.md`): wgpu is slower per render, costs +129 MiB
   RSS and some idle CPU in a driver thread, so `auto` stays `cpu` (§14.5). Unverified:
   Mesa's WSI, a real output, `sig` with Servo beside the chrome's device.
   *Merged into config-ext* (4 November 2026, aa25a3b) with two seams fixed where the day's
   bug fixes met the framework: `Gui::fall_back` invalidates the cached keyboard focus, and,
   since an unmapped target now waits for a new frame (6c02b9c), the frontend calls the new
   `Backend::forget_frame(surface)` when a native renderer's surface is hidden, so the next
   show renders instead of answering `Unchanged` (the binding and the swapchain stay, per §15);
   a pixel renderer's surface is still unbound. Approved as a small addition to §4's trait.
3. **`render-vulkan`** (Opus or Sonnet, after 2's `gpu` module): `sighurt-render-vulkan` with
   `build.rs` on naga; the same tests.
4. **Docs and measurements** (Sonnet): `extension-api.md` amendments, CLAUDE.md's crate table
   and build commands, README's `[ui] renderer`, default-config.toml, `bench/results/RENDER.md`,
   a STATUS decision with the evidence.

Reviews: Astra on this design before lane 1 starts, and on the API after lane 1; an
independent review of each backend before it is a default.

## 14. Review amendments (3 November 2026)

Two independent critiques, the `deep-design` agent (Fable 5.1) and GPT-6 Astra, agreed on the
layering and on these corrections. They override the body above.

1. **wgpu is Vulkan-only; there is no GL path in `sig`.** wgpu-hal's gles backend would open the
   `EGLDisplay` of the `wl_display`, which is Servo's (surfman); EGL displays are deduplicated
   per native display and `eglTerminate` is not reference-counted, so either library's teardown
   would kill the other's contexts (Servo terminates at engine shutdown,
   `engines/sighurt-engine-servo/src/runtime.rs`; wgpu-hal in `Drop`). Also wgpu's EGL path
   offers only FIFO and no swap interval 0. Rule: nothing but Servo opens or terminates the
   connection's EGL display. Vulkan instances are independent; Servo's WebGPU, if ever on, is
   another instance. The GPU-selection environment is never mutated (decision 102).
2. **`bind` is `unsafe fn`, with the lease's contract** (`TargetLease::gl_handles`): the handles
   are valid for the call; what the backend creates from them lives until `unbind`, which the
   caller promises to run before the surface or connection dies. `bind` creates only the API
   surface; **`render` configures the swapchain lazily** from the permit's `metrics.buffer()`
   (chrome targets are born with empty metrics, `app.rs` `fill_placement`; a 0×0 configure is
   invalid) and reconfigures when it changes. **`trim` is gone**: hide = `unbind`, show = `bind`
   again, driven by the frontend's placement, as Servo's `release_surface`/`bind_surface` do.
   Starting a backend = instance, the first surface, an adapter compatible with it, a device:
   "started" means a usable presentation path, not an adapter found. Verify in wgpu 30's own
   sources (`~/.cargo/registry/src/*/wgpu-30.0.1`) what `SurfaceTargetUnsafe::from_window`
   takes (both handles are needed for Wayland) and what `InstanceDescriptor` needs.
3. **Presentation policy.** MAILBOX is required; a surface whose capabilities lack it is not
   bound (→ CPU, §14.4). Never FIFO: Mesa's legacy FIFO waits on its own private frame callback
   inside present, which a synchronized subsurface or an unmapped surface never gets (the
   deadlock `targets.rs`'s lease doc forbids); `wp_fifo_v1` differs but is not assumed.
   IMMEDIATE is not an alias for "non-blocking" and is not used in v1. Software adapters
   (`DeviceType::Cpu`, lavapipe) are refused by `auto` and reported like Servo's `software`;
   an explicit `renderer = "wgpu"` may use one with a notice. Acquire is bounded: wgpu-core's
   fixed surface-acquire timeout (recalled ≈1 s; read `wgpu-core/src/present.rs` before
   relying on it) is measured, and the outcome model below never loses a frame. "Never
   blocking" is restated as: no wait for vsync; bounded, measured waits (acquire, configure,
   submit, present, teardown; blocking Wayland roundtrips happen at `bind` only).
   NVIDIA's proprietary WSI (this desktop) and Mesa differ; whether `vkQueuePresentKHR` commits
   the host's pending frame request on the calling thread is a must-test on both, under
   `tools/headless-sway --renderer gles2` (the pixman default cannot composite a dmabuf
   swapchain: there only the CPU backend runs).
4. **Outcomes and fallback at backend granularity.** `render` returns
   `Rendered::{Submitted, Unchanged, Deferred}` or `RenderError::{Output, Surface(String),
   Device(String)}` (wgpu 30's `CurrentSurfaceTexture` variants map onto these: `Suboptimal`
   renders, `Timeout | Occluded` → `Deferred`, `Outdated | Lost` → reconfigure once then
   `Surface`, `Validation` and device loss → `Device`). `Deferred` keeps the scene dirty and
   arms a one-shot retry (≈16 ms, counted in the diagnostics; a bounded retry is not a repaint
   timer; MAILBOX with 3–4 images makes it rare). `Surface` → unbind and bind again once;
   then, and on `Device`: unbind every chrome surface, drop the GPU backend, switch the
   frontend's single backend to the CPU one and recreate every chrome target as `Shm` (new ids:
   re-placement, keyboard and text-input targets, cursor and input regions move with them).
   One backend alive at a time; the trait stays mixing-capable for a later per-window choice.
   Test hook: `SIG_RENDER_FAIL=device`.
5. **`auto` selects the CPU backend until measured.** `render-wgpu` is compiled by default and
   chosen with `[ui] renderer = "wgpu"`; `auto` prefers wgpu only once `bench/results/RENDER.md`
   shows it at worst equal in chrome render time, RSS and idle owner-loop wakeups on this
   desktop (CLAUDE.md: thresholds after a baseline). Drivers spawn threads of their own, so the
   criterion is owner-loop wakeups, not "no threads".
6. **§5's incremental CPU painting is cut from v1.** A correct version must clear each repair
   region and replay every intersecting item in order with conservative ink bounds (strokes,
   bearings, antialiasing), keep the last three *scenes* per surface keyed by SHM generation,
   and never swap R/B over retained pixels. A 1920×44 strip repaints in a fraction of a
   millisecond; only `scene == previous → Unchanged` (no commit) ships.
7. **Glyph placement is one shared function.** `GlyphRun { origin (logical run origin),
   line_y, glyphs: [{ font: FontRef, glyph, x, y (cosmic-text offsets), font_size, flags }] }`
   and `scene::glyph_physical(&Mapping, &GlyphRun, &Glyph) -> (i32, i32, CacheKey)`, where
   `Mapping` holds origin, actual ratio and nominal scale exactly as today's `Pen::new`
   (ratio for the run origin, nominal scale for offsets and the baseline, baseline rounded
   separately), used by the CPU backend and `gpu::prepare`. `Bordered`'s one-pixel rule
   (`scale.round().max(1)`) lives in the shared scene code. cosmic-text ids and flags are
   wrapped in newtypes (`FontRef`, `GlyphFlags`); `Kind` is exhaustive (every backend is in the
   workspace). The atlas resets when the scale changes; color glyphs are premultiplied at
   upload. Tests: combining marks, negative bearings, fallback fonts, emoji, clipped text at
   odd buffer sizes and fractional scales.
8. **Fonts: share the `FontSystem`, not the chrome's shaping cache.**
   `text::FontSystemHandle = Rc<RefCell<cosmic_text::FontSystem>>`, created once by the
   frontend; `Fonts::new(handle)` keeps the family and the shaped-line cache as the chrome's
   own; `Backend::start(handle)` gets only the rasterizer source; `glyph::rasterize(&handle,
   key)`. The `RefCell` is never held across `draw` or `render`. `ChromeKind::new:
   fn(Fonts) -> Box<dyn Chrome>` stays.
9. **No persistent icon cache in v1.** `PathId` is per scene and the chromes rebuild icon
   paths with absolute coordinates each draw, so `(path id, stroke, scale)` would never hit.
   Each frame's icons (about ten 16-px paths) are rasterized on the CPU at their actual
   transform (ratio and fractional offset) into a per-frame ring region of the atlas: identical
   to the CPU backend's pixels, microseconds of work. A content-keyed cache is a later
   measurement.
10. **Atlas: two stages and a bound.** `prepare` collects the frame's glyphs and icons,
    allocates them (growing by doubling to 2048²: R8 4 MiB + RGBA8 16 MiB per backend), then
    emits geometry, so no UV is computed before its tile is placed. Allocations carry a
    generation; a grow or reset marks every row dirty, uploads coalesced regions (not one call
    per row), and restarts the frame's placement once; a frame that still overflows is
    `Device`-scope (→ CPU) with a diagnostic. Tiles get a one-pixel gutter and nearest
    sampling at integer positions.
11. **Batches, not one draw call.** `Prepared { vertices, indices, batches: [{ clip:
    PixelRect, range }] }`, one pipeline, one scissor per batch. Primitive parameters are an
    instance struct (two `vec4`s), not a single `params: vec4`. The `gpu` module documents the
    binding layout, buffer alignment, coordinate convention (pixels, origin top-left, ortho from
    the configured size) and naga options; a test checks the Rust vertex layout against the
    WGSL's.
12. **Color contract.** Chrome surfaces are opaque in v1: `Pen::clear` forces `a = 255` and the
    backends use `Bgra8Unorm` (not sRGB) with `Opaque` composite alpha; premultiplied source-over
    in encoded space, as tiny-skia blends. The comparison tests cover colored glyphs, clipped
    text, sampling boundaries and fractional scales, with a stated tolerance per class.
13. **The Vulkan (ash) backend, lane 3, after lanes 1–2 are measured; opt-in, never default.**
    Per swapchain *image* render-finished semaphores, per-slot acquire semaphores and fences,
    a fence reset only when a submission follows, `NOT_READY`/`TIMEOUT` → `Deferred` without
    losing the repaint, one frame in flight per *backend* so the shared atlas, descriptors and
    staging memory are never referenced by two frames, noncoherent flushes and
    transfer/sampling barriers, swapchain and surface retirement after the device is idle at
    `unbind`. Memory bounds report the *actual* image count (Mesa MAILBOX provisions four).
14. **`[ui] renderer` validation.** `Environment::renderers` lists every project renderer name
    (`auto`, `cpu`, `wgpu`, `vulkan`) in both frontends and `--dump`; a GUI build that lacks the
    feature gives a notice and uses `auto`, as `[ui] font` treats an uninstalled family. An
    unknown name is an error.
15. **`Cargo.lock` evidence** (this desktop, 3 November): adding wgpu 30.0.1 (vulkan, wgsl),
    ash 0.38 and naga 30 (wgsl-in, spv-out) to a probe crate changed only the wasm-bindgen
    family (js-sys, web-sys, wasm-bindgen 0.2.117 → 0.2.130), which `cargo tree --target
    x86_64-unknown-linux-gnu` shows is not in `sig`'s Linux graph with Servo; everything else
    was added. Servo 0.7's `webgpu` feature is off (no `wgpu` entry in the lock today).
16. **Lifetimes and shutdown.** `App::shutdown` gains the step "unbind every chrome surface,
    drop the backend" between the session's shutdown and `drop(leases)`; the forced-exit path
    needs nothing. One backend per frontend, surfaces keyed by `SurfaceKey(TargetId)`, so a
    second window needs no redesign. `describe()` runs only after a start. The
    `PresentMode::Gl → Native` rename happens in lane 1 only if the diff stays mechanical.
17. **Deferred to later decisions:** native-target damage (`VK_KHR_incremental_present` for
    ash; none for wgpu), images (an immutable, versioned image resource with its own textures;
    not the glyph atlas), the Wasm engine's canvas on the scene, Metal.
18. **Lifecycle tests added to §12:** two layouts before the parent commit, hide right after a
    submission, remap, workspace occlusion, repeated resize, zero-size and restore, acquire
    timeout, device loss (`SIG_RENDER_FAIL=device`), closing one of two windows, atlas overflow
    while another surface is in flight; measured owner-loop stalls and retiring memory, not only
    render time.

## 15. Measured: wgpu 30 on Vulkan as a chrome presenter (3 November 2026)

`experiments/wgpu-subsurface` (branch `wgpu-exp`, commit d205209; Claude Opus 5.5) binds wgpu
30 (Vulkan only, validation on) to a GL-mode chrome target of the host and presents on each
permit, in `tools/headless-sway --renderer gles2` on this desktop (RTX 3070, NVIDIA 595.104.02,
sway with explicit sync). Logs and screenshots: `/data/sig-builds/test-out/wgpuexp/`.

| Question | Result |
|---|---|
| Does `queue.present` commit the host's pending frame request? | Yes, synchronously: the `WAYLAND_DEBUG` trace shows the host's `wl_surface.frame` and presentation feedback at permit issue, then inside present the syncobj acquire/release points, attach, damage and commit. 59–63 permits, callbacks and presented frames a second over 17 s; no stall. |
| Per-frame cost (debug build, validation on) | acquire 0.15–0.24 ms (0.5 max; 2.2 after a configure), submit 0.7–1.0 ms (3.4 max), present 0.27–0.5 ms (3.4 max). |
| Present modes, formats, alpha | `[Mailbox, Fifo, Immediate]`; `Bgra8Unorm` present; `[Opaque, PreMultiplied]`. wgpu asks for `desired_maximum_frame_latency + 1` images: 3 images by default (min 2, max 8). |
| Hide and show | Keeping the `Surface` across an unmap costs nothing and errs nowhere (presents on the unmapped surface 8–12 ms for 10; re-show at the same size needs no reconfigure; a new size configures in 1.7–3.6 ms). Dropping and recreating costs 20–35 ms of owner-thread time per re-show (drop waits for the device 8–12 ms, bind 1.4–5.4 ms with NVIDIA's registry roundtrips, first configure 10–17 ms). After every map NVIDIA returns `Suboptimal` on each acquire until one reconfigure (3–4 ms). |
| Acquire when every image is held | `FRAME_TIMEOUT_MS = 1000` is fixed in `wgpu-core/src/present.rs`. On a desynchronized or unmapped surface, 10 back-to-back presents never block. **In a synchronized subsurface before the root commits, the third present blocks 1 s and returns `Timeout`, each time, until the root commits** (8 in a row stalled the loop 8.2 s). |
| Drop order | Surface, then device and queue, then adapter and instance, then the lease and host: clean. A present after the lease is gone is a fatal protocol error for the whole connection (`wp_linux_drm_syncobj_surface_v1: The surface has been destroyed`), and the dispatch thread then spins printing the error. |
| Memory and start | RSS 6 MB → **123–157 MB after the device (+117 MB)**; instance 85–120 ms, device 66–117 ms, first bind 141–241 ms in all. Unmeasured: how much is shared with Servo's NVIDIA GL driver inside `sig`. |
| 0×0 configure | A validation error (`ZeroArea`); a panic without an error scope or uncaptured-error handler. Lazy configure at the first permit: 1.5–4.5 ms. |
| Pixman compositor | Refused: the adapter does not present to the surface (no `zwp_linux_dmabuf_v1`, no syncobj). lavapipe does not load in the nix-shell (needs a newer glibc), so the software-adapter rule is untested. |
| `Cargo.lock` | With `default-features = false` wgpu adds crates only (ash, gpu-allocator, naga, spirv, wgpu*); the wasm-bindgen family is unchanged (§14.15 overstated it); `bit-set`/`bit-vec` entries gain a version suffix because a second version joins. |

Rules that follow (they amend §14):

- **A native chrome target renders only on a permit, once per permit, never on its own
  clock.** The host issues the next permit for a synchronized target only after the
  transaction's root commit and the frame callback, which is what keeps acquire from
  blocking. So a `Deferred` outcome is retried by *invalidating* the target (a new permit),
  never by presenting again; nothing presents outside `render`. The host's existing pacing
  already provides this; the frontend must not add a second path.
- **`Suboptimal` after a map:** render that frame, then reconfigure once before the next.
- **Hide keeps the `Surface`** (amends §14.2): `unbind` happens on retire, fallback and
  shutdown only. Mesa is untested; the rule is reconsidered if Mesa misbehaves.
- **The RSS cost is real** (+117 MB for a device next to a Servo that already owns the GPU):
  §14.5 stands, `auto` stays on the CPU backend, and lane 2 measures the cost *inside `sig`*
  with Servo running before anything is promoted.
- **wgpu 30 API facts for lane 2:** `Instance::new(InstanceDescriptor { backends: VULKAN,
  ..new_without_display_handle() })`; the surface from
  `SurfaceTargetUnsafe::from_display_and_window(&handles, &handles)` (`from_window` leaves the
  display out and fails with `MissingDisplayHandle`); `request_adapter` returns a `Result`
  and `request_device(&desc)` takes one argument, both futures (a no-op waker `block_on`);
  `get_current_texture()` returns the `CurrentSurfaceTexture` enum; present is
  `queue.present(texture)`; `SurfaceConfiguration` has `color_space`; error scopes are
  guards whose `pop()` is a future; always install `on_uncaptured_error`; image counts are
  only visible through `wgpu::hal` (`maximum_frame_latency` = image count − 1). Debug builds
  turn validation on and load the user's implicit layers (MangoHud here).
