# Sighurt's terminal frontend: design

**Status: accepted with the amendments of §8** (17 October 2026; decisions 89–91). Written by
the deep-design pass (Fable), reviewed independently (GPT-6 Astra); §8 records what the review
changed and the user's answers, and overrides §0–§6 where they differ. Phase 0's prototypes must
still kill the risks marked unverified.

The user's request: a full terminal frontend beside the Wayland GUI, split cleanly into a terminal
crate (a minimal, nice CLI/TUI tool invoked as `sig`), the GUI as its own crate, and a shared core
that does most of the logic, keeps track of keybindings and connects both frontends to the
engines. Everything is compiled into one binary `sig`, with no IPC, and all of Sighurt's own code
is Rust (Servo and WPE WebKit stay). `sig` on a TTY opens the terminal browser and without a TTY
the GUI; `--gui` / `--tui` override; `sig --dump URL` prints to stdout. Pages are shown by the
existing engines: first as graphics through the kitty and sixel protocols, then as reflowed text
from an optional semantic interface of the engine contract (accessibility tree or DOM
extraction, with actions), switchable, with text mode working in any terminal.

## 0. Three facts that shape everything

1. **Servo 0.5 renders offscreen without a display server.**
   `servo-paint-api-0.5.0/rendering_context.rs:289` `SoftwareRenderingContext::new(size)` uses
   `surfman::Connection::new()`, which on Unix (`surfman-0.13.0/src/unix.rs`,
   `multi/connection.rs:66`) tries Wayland, then X11, then `mesa_surfaceless`
   (`EGL_PLATFORM_SURFACELESS_MESA`). Frames come back with `read_to_image` (glReadPixels and a
   row flip, two copies; we can `read_pixels` straight into our buffer). Caveats: the software
   adapter sets `LIBGL_ALWAYS_SOFTWARE` process-wide (M0 open problem 2), and whether
   NVIDIA+glvnd on NixOS honours the surfaceless platform is **unverified** (phase 0a).
2. **Servo 0.5 has an AccessKit tree API** the adapter doesn't use:
   `Preferences::accessibility_enabled`, `WebView::set_accessibility_active(true)`,
   `WebViewDelegate::notify_accessibility_tree_update(webview, accesskit::TreeUpdate)`
   (accesskit 0.24, incremental updates with stable `NodeId`s). **Unverified**: whether 0.5 fills
   link URLs, input values and bounds, and whether it handles AccessKit actions. Both engines have
   `evaluate_javascript` (WPE with a `world_name`, so extraction can run in an isolated world).
3. **WPE already renders to CPU memory without a compositor device**: `Display::new(dmabuf: None,
   ..)` gives `WPEBufferSHM` frames. Only `runtime.rs` `platform()` ties this to a Wayland lease.
   **Unverified**: the web process with no `WAYLAND_DISPLAY` and no DRM node (phase 0b).

The Wasm engine is already presentation-neutral (`WasmRuntime<T: CpuTarget>`); the demo engine
uses `TargetLease` directly.

## 1. Crate structure

`sighurt-core` stays pure (decision 27, §26). The shared core is `sighurt-core` plus one new
frontend-neutral crate carved out of today's `sighurt-browser`:

| Crate | Owns | Must not own | Deps |
|---|---|---|---|
| `sighurt-api` | + `present` (the CPU frame contract, moved from wasm), `semantic` (document model, actions), `chrome` (`Snapshot`, `TabSnapshot`, `contained`, `readable_url`, moved from sighurt-ui and re-exported there) | anything third-party | none |
| `sighurt-core` | unchanged, + `BrowserEvent::Semantic` as user activation, `Environment::frontend` | I/O | api |
| **`sighurt-runtime`** (new; name pending, §8) | the engine registry and runtime lifecycle, effect execution that isn't frontend-specific, services and downloads, config and session persistence and watch, diagnostics, per-view bookkeeping, the snapshot builder, a frontend-neutral `Session<P: Platform>` driving `update()` | windows, terminals, pixels, key routing policy | api, core, engines (optional features), `sighurt-wayland` only under feature `wayland` |
| **`sighurt-gui`** (new; today's `app.rs`, chrome targets and placement, Wayland clipboard glue) | the calloop owner loop over `Host`, the Wayland `Platform` | policy, engine internals | api, core, runtime, wayland, ui |
| **`sighurt-tui`** (new) | terminal I/O, capability detection, cell grid and diff writer, terminal chrome, graphics encoders (kitty, sixel), the text renderer (semantic → cells), input mapping, dump mode | policy, engines, Wayland | api, core, runtime (never sighurt-ui or sighurt-wayland) |
| `sighurt-wayland`, `sighurt-ui`, `sighurt-chrome-line`, `sighurt-config-example` (was `sighurt-keymap-modal`; decision 116) | unchanged | | |
| `sighurt-browser` (`sig`) | `main.rs`: CLI, frontend choice, process-wide setup | | gui/tui optional |
| `sighurt-semantic` (new, small) | the DOM extraction script and the bounded JSON → `SemanticSnapshot` parser shared by the Servo and WPE adapters | engine specifics | api, serde_json |

Not grown into `sighurt-core`: the registry, effects and services do I/O and own engine
runtimes. Separate crates rather than modules: the user asked for them, and a TUI that cannot see
cosmic-text or Wayland is only enforceable at a crate boundary. `sighurt-ui`'s `Chrome` API is
not reused by the terminal (pixels, logical px); only the snapshot vocabulary moves down to api so
both kinds of chrome consume one contained snapshot.

```rust
pub trait Platform: 'static { type Target; type Permit; fn registry(&self) -> Registry<Self>; }
pub struct Session<P: Platform> { core: BrowserState, registry: Registry<P>, views: BTreeMap<ViewId, ViewEntry>, .. }
pub enum FrontendEffect { Focus(KeyboardFocus), ChromeInput(InputEvent), ChromeCommand(String),
    PageCursor(Cursor), PageTextInput(Option<TextInputState>), Notice(String), Redraw,
    FrameReady(ViewId), Semantics(ViewId, Box<SemanticSnapshot>), Show(ViewId, bool),
    OpenTarget(ViewId), CloseTarget(ViewId) }
impl<P: Platform> Session<P> {
    pub fn push(&mut self, e: BrowserEvent);
    pub fn run_core(&mut self, out: &mut Vec<FrontendEffect>);
    pub fn pump_engines(&mut self, out: &mut Vec<FrontendEffect>) -> bool;
    pub fn present(&mut self, view: ViewId, permit: P::Permit) -> Result<PresentOutcome, EngineError>;
    pub fn timeout(&self) -> Option<Duration>;
    pub fn shutdown(self) -> ExitCode;
}
```

Two `Platform`s: `Wayland` (feature `wayland`: `Target = TargetLease`, `Permit = FramePermit`)
and `Cpu<S: CpuSink>` (`Target = S`, `Permit = CpuPermit`). The GUI loop becomes a thin shell over
`Session<Wayland>`; `headless.sh` and `engines.sh` must pass unchanged after the move.

## 2. Engine presentation

Two presentation worlds behind the unchanged `EngineRuntime` associated types; the core sees
neither.

```rust
// sighurt_api::present (CpuTarget moved up from sighurt-engine-wasm, generalized)
pub enum PixelFormat { Argb8888 /* premultiplied B,G,R,A: wl_shm, WPE SHM, Wasm, demo */, Rgba8888 /* GL readback */ }
pub struct PixelFrame<'a> { pub pixels: &'a mut [u8], pub size: BufferSize, pub stride: usize, pub format: PixelFormat }
pub trait CpuTarget { type Permit; fn metrics(&self) -> Metrics; fn permit_epoch(p: &Self::Permit) -> LayoutEpoch;
    fn submit(&mut self, permit: Self::Permit, format: PixelFormat,
              paint: &mut dyn FnMut(PixelFrame<'_>) -> Result<(), String>) -> Result<(), String>; }
```

Each engine runtime becomes generic over a presenter, as Wasm already is: `ServoRuntime<P>` with
the Wayland presenter (today's, behind feature `wayland`, default on) and an offscreen one (an
`OffscreenContext` like `SoftwareRenderingContext` that uses `create_adapter()`: hardware when a
display is reachable, llvmpipe over surfaceless otherwise; `WebView::paint`, then `read_pixels`
into the `CpuTarget`'s frame as `Rgba8888`). `WpeRuntime<P>` with `present/cpu.rs` (no Wayland,
`Display::new(None, ..)`, the SHM buffer copied into the frame). `DemoRuntime<T: CpuTarget>` like
Wasm. Engine crates get a `wayland` feature gating `sighurt-wayland`.

Rejected: a nested headless compositor inside `sig` (plan §18 calls it a separate project;
wlroots is C; no GPU over SSH anyway); CPU frames as the only contract (page-sized readbacks on
the GUI, against §26).

**No compositor at all** (SSH, Linux console): Servo via surfaceless Mesa (llvmpipe), WPE via its
SHM path with the web process on surfaceless EGL, Wasm and demo trivially. Risks: NVIDIA/glvnd
surfaceless support, llvmpipe speed (expected 50–200 ms a frame at 1280×720, fine at terminal
rates), `LIBGL_ALWAYS_SOFTWARE` leaking into WebKit's web processes.

**The terminal graphics bound** (the plan's "zero readbacks" stays a GUI criterion): permits only
for the shown view, only in graphics mode, at most `[tui] fps` (default 10) and only after the
previous frame's bytes left the write buffer; frame size = page cells × cell pixel size, capped
at 8 Mpx; one readback per presented frame, counted in the diagnostics; no permits while hidden
or in text mode. Full-frame kitty transmits first (raw, zlib); banded updates via kitty animation
frames as a measured later step; sixel always full-frame, quantized, default 5 fps.

As built (decision 133): every image written counts against that rate, a frame sent again
because a tab was shown included, and a hidden tab's image is deleted or written over (by the
shown tab's image or by spaces) only once the last image has been up for its frame interval and
at least 250 ms. Rio 0.5.28 makes a Vulkan texture for each image, synchronously, and destroys it
as soon as the image is deleted or covered, without waiting for the frames in flight that draw
it. With tabs switched at key repeat (a full-page image each, blanked or deleted a moment later)
it lost its device (NVIDIA Xid 109, `ERROR_DEVICE_LOST` from its upload's queue submit) within
seconds, with sixel and with kitty graphics; a page's own frames replacing each other at
`[tui] fps` (100 ms apart for kitty, 200 ms for sixel, a minute of each) did not. A sixel for
the place and layout of the one on the page rows is written over it as it is, and not at all
when its bytes are the same: tmux frees a stored image when any cell is written over it and
redraws the whole pane, so the blank the frontend used to write before every frame showed the
rows black until the next image had arrived. Only a new layout, or rows that held text, are
blanked first (spaces, in a synchronized update that ends before the image). A tab switch
writes the shown tab's frame over the hidden tab's image when that time comes (kitty deletes
the hidden one's id in the same write); a tab with no frame for this layout gets the blank then,
and its frame when it comes. So a hidden tab's picture can stay under the shown tab's title for
up to 250 ms. tmux keeps the sixels written over each other (at most 20 in the server, the
oldest freed without a redraw) and sends them all again, oldest first, when it redraws the pane
(a window switched back to, a refresh, a resize): 25–27 sixels a redraw on the animation page.
So inside tmux (`TERM` starting with `tmux` or `screen`), once a page has gone 400 ms without a
new sixel and more than one is stacked, its picture is written again after the blank in one
write: tmux frees the stack, marks the pane for a redraw, holds that sixel back from the outer
terminal while the redraw is pending (`screen_write_set_client_cb`) and draws the lines and the
one image in that redraw. A still page then costs one sixel a redraw. A page that keeps changing
is not capped: a blank every few frames would blink each time, and spaces over a sixel Rio
shows are what loses Rio its device. In the private sway Rio died within 3 to 12 tmux window
switches whatever sig had stacked (25–27, 8 or 1), and with one still sixel that `cat` wrote and
no sig at all: Rio 0.5.28 under tmux does not live through pane redraws with a sixel shown, and
nothing the frontend sends avoids that. Rio alone, with `[tui] graphics = "sixel"` forced,
still shows an empty page for a frame now and then when a sixel replaces the one under it
(4–11 of 200 screenshots in about 4 s; with the old blank before every frame Rio lost its
device instead); a synchronized update around the image made Rio lose its device too. Rio
alone picks kitty graphics, which show no such frames.

## 3. The semantic interface (`sighurt_api::semantic`)

```rust
pub struct NodeId(pub u64); // engine-assigned, stable within one DocumentGeneration
pub struct SemanticSnapshot { pub document: DocumentGeneration, pub root: NodeId, pub nodes: Vec<SemanticNode>, pub focus: Option<NodeId>, pub truncated: bool }
pub struct SemanticNode { pub id: NodeId, pub kind: NodeKind, pub text: Option<String>, pub children: Vec<NodeId>,
    pub href: Option<String>, pub value: Option<String>, pub label: Option<String>, pub checked: Option<bool>,
    pub options: Vec<String>, pub disabled: bool, pub password: bool }
pub enum NodeKind { Document, Heading(u8), Paragraph, Text, Link, Button, TextField, TextArea, Checkbox, Radio,
    Select, Image, List, ListItem, Table, Row, Cell, Pre, Code, Quote, Separator, Landmark(Landmark), Container }
pub enum SemanticAction { Activate { node: NodeId, href: Option<String> }, Focus(NodeId), SetValue { node: NodeId, value: String },
    SetChecked { node: NodeId, checked: bool }, SelectOption { node: NodeId, index: u32 }, Submit(NodeId), ScrollIntoView(NodeId) }
// contract additions
EngineCapabilities::semantics: bool;
EngineCommand::SetSemantics(bool);                                                 // nothing is extracted unless asked
EngineCommand::Semantic { document: DocumentGeneration, action: SemanticAction };  // stale document: dropped
EngineEvent::SemanticsChanged { view, document, snapshot: Box<SemanticSnapshot> }  // replaceable: latest per view
```

Bounds, enforced in the adapters and checked again by the runtime: 20 000 nodes, 1 MiB of text,
each string contained on the TUI side; a bigger page is `truncated`. Snapshots, not deltas, at
first.

`SemanticsChanged` is page data the core never needs; the runtime hands it to the frontend like
`FrameReady` (decision 27's rule). Actions go through the core: `BrowserEvent::Semantic { view,
action, at }` counts as a click (records activation, sets `View::link` from `href` so decision
82's hand-off to another engine works in text mode) and yields `Effect::Engine { Semantic }`.

- **Demo**: a small static tree (headings, links to its other pages, a text field), so text mode
  and actions are testable without Servo.
- **WPE**: `sighurt-semantic`'s script in an isolated world (ids from a `WeakMap<Element, id>`
  in that world, reset per document); actions as script (`click()`, `focus()`, `value` plus
  input/change events, `scrollIntoView`); extraction at load end, each commit, after each action
  and on `chrome.refresh`, no polling (MutationObserver push later).
- **Servo**: the same script in the main world (page-tamperable, acceptable: it is page data,
  and actions are what the page could do itself), or AccessKit (engine-side, incremental) if
  phase 0a shows 0.5's tree carries link URLs, field values and bounds.
- **Wasm**: no semantics; graphics mode, or a placeholder in terminals without graphics.

Reflow to cells lives only in `frontends/sighurt-tui/src/textmode/`.

## 4. The terminal frontend (`sighurt-tui`)

- **I/O**: its own, on `rustix::termios` (already in the graph), `vte` (parsing what the terminal
  sends: kitty keys, SGR mouse, DA1/XTGETTCAP/OSC/APC replies) and `unicode-width`; stdin as a
  calloop fd source, SIGWINCH/SIGTSTP/SIGCONT through the existing signal pipe. Not crossterm
  (its own event model fights calloop) nor termwiz (a whole widget stack).
- **Capabilities**, one batched query at startup with a 150 ms window: DA1 (sixel), `CSI ? u`
  (kitty keyboard), kitty graphics query, XTGETTCAP, OSC 10/11 (colours, for contrast correction
  and palette quantization), cell size, `COLORTERM`, `NO_COLOR`; `[tui] graphics =
  auto|kitty|sixel|none` overrides.
- **Grid**: a cell double buffer, a damage-diffed writer with minimal cursor moves and SGR inside
  synchronized output (`CSI ? 2026`); alternate screen, bracketed paste, kitty keyboard
  push/pop, SGR mouse. The terminal is restored on quit, panic and SIGTSTP.
- **Chrome**: its own small trait `TerminalChrome { handle(ChromeEvent) -> Vec<ChromeAction>;
  rows() -> (top, bottom); draw(&mut Grid) }` over `sighurt_api::chrome::Snapshot`; the
  standard one has a top line (`[2/5] address · engine · MODE · keymap`) and a bottom line
  (status, notices, the address line, the find field and what the search found). Config `[tui] chrome`, `graphics`, `mode`, `fps`, `mouse`; `[keys]`, the
  keymaps and `[ui.colors]` apply as they are.
- **Keys → `KeyEvent`**: with the kitty keyboard protocol, `key` from the codepoint and `code`
  from the base-layout key through a US-layout → evdev table (so `code:` bindings work); in
  legacy terminals `code = KeyCode(0)` (physical bindings never match; documented), ESC prefix
  = Alt, control characters = Ctrl+letter, and a synthesized release after each press. In text
  mode the TUI consumes page-bound input itself, so one `[keys]` table serves both modes.
- **Mouse**: cells → pixels in graphics mode; the link map in text mode; the wheel scrolls.
- **Commands**: the terminal chrome's `chrome.toggle-mode`, `chrome.text-mode`,
  `chrome.graphics-mode`, `chrome.refresh`; no new core commands.
- **`--dump URL [--width N]`**: a headless session with no terminal, semantics on, wait for the
  load and a snapshot (or `--timeout`), reflow, print, non-zero exit on failure. A pager for stdin
  (`sig -`) later.

## 5. Frontend selection and the single binary

`sighurt-browser` features `frontend-gui` and `frontend-tui`, both default. A TUI-only build links
no `sighurt-wayland` and no engine Wayland presenter. `main`: `--dump` → headless; `--gui`/`--tui`
win; else stdout a TTY and `TERM` not `dumb` → TUI; else GUI (no `WAYLAND_DISPLAY` → an error
naming `--tui`).

## 6. Phases

- **Phase 0, risk killers** (parallel, in `experiments/`): 0a Servo offscreen without
  `WAYLAND_DISPLAY`/`DISPLAY` → kitty frames, timing at 1280×720, and a table of what 0.5's
  AccessKit tree fills; 0b WPE `Display::new(None)` with no compositor, SHM frames, and the
  extraction script in an isolated world; 0c terminal I/O (raw mode, queries, kitty keys, grid
  diff) tested under a pty with golden byte streams.
- **Phase 1, the split** (sequential): `sighurt-runtime`, `sighurt-gui`, `sig`'s main;
  `CpuTarget` → `sighurt_api::present`; the demo onto it; snapshot types → `sighurt_api::chrome`;
  engines' Wayland presenters behind `wayland`. Pass: every existing suite and clippy set passes
  unchanged; the runtime builds without `sighurt-wayland`.
- **Phase 2, CPU presenters** (parallel per engine): an in-memory `CpuTarget` test per engine
  receives frames at the permit's epoch and none without a permit; `engines.sh` still passes.
- **Phase 3, graphics mode**: `sighurt-tui`. Pass: `sig --tui` in a pty driven by a fake
  terminal shows the demo pattern, keys and commands work, resize keeps the image in the page
  rows, readbacks ≤ frames ≤ fps × seconds.
- **Phase 4, semantics** (parallel): the api and `sighurt-semantic`, the demo tree, the core's
  `BrowserEvent::Semantic`; WPE; Servo; text mode and `--dump`. Pass: `sig --dump` of a demo page
  at width 80; Tab/Enter follow a demo link and typed text reaches a demo field in the pty test;
  engines.sh gains a `text` scenario (Servo and WPE text, link activation, a hand-off across
  engines in text mode); a 20 000-node page is truncated, not slow.
- **Phase 5**: mode switching polish, mouse, the pager, documentation, STATUS.

## 7. Decisions this amends

1 and 78 (the feature list gains frontends); 2 (the GUI is Wayland-only, and a terminal frontend
exists); 22 and 45 (`CpuTarget` in api; Wasm's Wayland mount behind a feature); 27 (the runtime
routes `SemanticsChanged` like `FrameReady`; `BrowserEvent::Semantic` is activation); 46, 47 and
56 (the owner loop and effects move to the runtime and the GUI); 51 and 57(9) (a second, offscreen
Servo presenter); 62, 63 and 84 (a third WPE transport, CPU); 67, 68, 74 and 75 (`[tui]`, snapshot
types in api, chrome commands checked per frontend); 81 (documents); plan §26 gains the terminal
bound.

From Chawan, taken: a pager owning the terminal; capability queries; palette and contrast
reduction; a damage-diffed grid; TOML keys → named commands; dump mode; a stdin pager. Rejected:
per-page processes, a loader process, CGI protocols, mailcap and seccomp (decision 1, §16, §18;
WebKit has its own sandbox); its own HTML/CSS/JS (the user's decision); buffers as a tree (tabs
are the core's model); out-of-process image decoding (the engines decode).

## 8. Amendments after the review, and the user's answers

The user's answers (17 October 2026): the new crate is **`sighurt-runtime`**; the terminal's
default `[tui] mode` is **`auto`** (graphics where the terminal can show them, text otherwise);
the built-in configuration ships a **small terminal key table** (amending "no default bindings"
for the terminal only; users override or clear it); `--dump` prints **reflowed text with numbered
link footnotes**, like `lynx -dump`.

The independent review (GPT-6 Astra) found, and this design now says:

1. **Servo without a display is a hypothesis, not a fact.** surfman falls back to another
   backend only when creating the connection fails; the surfaceless `create_adapter()` picks
   *hardware* (it removes `LIBGL_ALWAYS_SOFTWARE` and sets `DRI_PRIME`), not llvmpipe. The
   offscreen presenter states its backend and adapter choice explicitly, and phase 0a tests no
   display sockets, an inaccessible DRM node, NVIDIA/glvnd, the renderer actually used, and its
   frame time measured (the 50–200 ms guess is struck).
2. **WPE's SHM transport does not prove WebKit starts without a compositor.** Phase 0b must show
   a web process producing changing SHM frames, navigating, resizing and shutting down with no
   display sockets and no DRM access, and coexisting with Servo (whose environment changes
   affect WebKit's later processes).
3. **Servo 0.5's AccessKit tree is not enough**: `servo-layout-0.5.0/accessibility_tree.rs`
   fills roles, text and labels but no link URLs, input values or geometry, and 0.5 has no
   action dispatch. **Both Servo and WPE use DOM extraction.** AccessKit is a later option.
4. **Semantic actions carry their document from the snapshot shown.** `BrowserEvent::Semantic
   { view, document, node, action }` takes the `DocumentGeneration` of the snapshot the user
   acted on, through the core to the adapter, which drops it if that document is gone. An
   adapter stamps an extraction with the generation at its start and drops a completion for a
   replaced document; the frontend clears a view's snapshot on commit, failure and close.
   Within one document, detached or ineligible nodes refuse the action. Only actions from user
   input count as activation (not focus or scroll), and a hand-off URL comes from the node of the
   snapshot shown, through the existing URL checks. Script `click()` is untrusted in Servo, so
   gesture-gated page behaviour may not follow; the forms tests say what does.
5. **The semantic bounds apply during extraction**, in the script (node count, depth, children
   per node, options, every string's length, total text, a time budget), before Rust parses,
   and again in the parser (unique ids, references in range, no cycles). Main-world script output
   is forged-able page data: native code assigns the document, never the script. **Password
   values never enter a snapshot or a dump.** Every string bound for the terminal (page text,
   titles, URLs, errors, dump output) has control characters and escape sequences removed, not
   just its length cut. The `sighurt-semantic` crate waits until both adapters share real code;
   the script starts in the first adapter that needs it.
6. **The graphics bound counts what happens, not what is permitted.** WPE renders buffers whether
   or not they are taken, so diagnostics count frames produced, read back, copied, encoded and
   transmitted separately, and WPE's producer is throttled (frames acknowledged only at the
   permitted rate) or the difference is reported. At most one encoded frame is pending; encoded
   bytes are bounded; writes to the terminal are nonblocking and input stays responsive over a
   slow link; a resize cancels a pending frame of the old size. Pixel origin, alpha and stride
   are specified per `PixelFormat` and validated.
7. **The terminal parser is not settled.** `vte` discards APC contents (kitty graphics replies)
   and keeps parameters as `u16`, too small for kitty key codes; termwiz's input parser stands
   alone without its widgets; crossterm's same-thread `poll`/`read` is a tradeoff, not a
   blocker. Phase 0c compares their coverage of what we receive (kitty keys, SGR mouse, DA1,
   XTGETTCAP, OSC 10/11, kitty graphics replies, bracketed paste) and picks one, or a small parser
   of our own over rustix.
8. **The terminal is restored before any forced exit**: before `App::shutdown`'s `_exit` after a
   leaked Servo, as an explicit frontend step, not a destructor. Engine and library logging never
   reaches the interactive terminal or the dump's stdout (a log file, or stderr when it isn't the
   terminal). Tests restore after an initialization failure, suspend/resume, hangup and an engine
   failure.
9. **The registry keeps its enum of `dyn EngineRuntime`s**, parameterized by the platform's
   target and permit types; `Session<P>` only where those types flow. Opening a target is a
   handshake with an error path (effects are ordered: a view's target before its commands).
   Services reach the clipboard through an explicit frontend interface.
10. **DOM extraction's limits are defined** in the script's documentation: same-origin iframes,
    open shadow roots, hidden content skipped, accessible names, selected options, ids that map
    back to nodes (a `Map<id, WeakRef<Node>>` beside the `WeakMap`), and invalidation through a
    bounded, debounced MutationObserver (or the snapshot marked stale).
11. **Keys**: kitty's unshifted, shifted and alternate keys, associated text and event types map
    deliberately onto `key`, `text` and the consumed modifiers; physical codes only when known;
    legacy Tab/Ctrl-I and Enter/Ctrl-M are ambiguous and Escape waits briefly to disambiguate.
    Input the keymap left unconsumed goes to one place: the text view in text mode, the engine in
    graphics mode, never both. Text-mode navigation and editing are named commands, bindable like
    any other.
12. **The phases change order**: after phase 0, the split (phase 1) is followed by a demo-engine
    slice all the way into the terminal (graphics and text, the demo engine's semantics) before
    Servo and WPE get CPU presenters. `--dump` defines when a document is complete (load end, then
    the first snapshot after it, or `--timeout`, default 30 s, exit 2 on timeout) and runs with an
    ephemeral profile that never writes the user's session.

The phases, as amended:

- **Phase 0** (parallel, `experiments/`): 0a Servo offscreen (§8.1, §8.3's tree check is done);
  0b WPE without a compositor, with Servo (§8.2); 0c the terminal parser choice and I/O under a
  pty (§8.7), including delayed and malformed replies, blocked output and restoration (§8.8).
- **Phase 1**: the split (`sighurt-runtime`, `sighurt-gui`, `sig`'s main, `CpuTarget` and the
  snapshot types in api), no behaviour change.
- **Phase 2**: the demo slice: the demo engine on `CpuTarget` with a semantic tree; `sighurt-tui`
  with graphics and text modes, the terminal chrome and keys, `--dump`, against the demo engine.
- **Phase 3**: Servo's and WPE's CPU presenters; graphics mode with real pages.
- **Phase 4**: DOM extraction and actions in Servo and WPE; text mode and `--dump` with real
  pages; engines.sh's `text` scenario.
- **Phase 5**: the Wasm engine in the terminal (graphics only), mouse, the stdin pager, the
  documents.

## 9. Phase 0's results (17 October 2026)

All three prototypes pass; details and numbers in each `experiments/*/RESULTS.md`.

- **0a Servo offscreen** (`experiments/tui-servo`, louise-pc over ssh, no `WAYLAND_DISPLAY` or
  `DISPLAY`, RTX 4060): surfman's Mesa surfaceless backend with the hardware adapter renders real
  pages on the NVIDIA GPU through glvnd, also with `/dev/dri` hidden. At 1280×720: paint 0.25 ms
  median, readback (glReadPixels into our buffer) 1.0 ms; 1920×1080 readback 2.0 ms. With only
  Mesa's EGL vendor visible it falls back to llvmpipe (paint 1.2 ms, first paint 1.3 s). Script
  evaluation works offscreen. `--adapter sw` does not move NVIDIA to llvmpipe and sets
  `LIBGL_ALWAYS_SOFTWARE=1` for every later child; the hardware adapter sets `DRI_PRIME=1`. The
  presenter uses the hardware adapter; WebKit's processes must not inherit either variable
  (§8.2's coexistence check is still open: phase 3).
- **0b WPE without a compositor** (`experiments/wpe-cpu`, same machine): `Display::new(None, ..)`
  without any Wayland connection delivers changing SHM frames, navigates, resizes, evaluates
  script in the isolated world `sighurt` (the page's main world can't see or forge it) and shuts
  down with its web and network processes gone; also with `/dev/dri` hidden. Copying a 640×480
  frame takes 0.08 ms. **WebKit produces no new frame until the last one is acknowledged**
  (1 produced, 0 acknowledged in 2 s), so pacing acknowledgements bounds its rendering (§8.6).
- **0c Terminal I/O** (`experiments/tty-io`): a small parser of our own (§8.7): `vte` 0.15 drops
  APC bodies and saturates parameters at `u16`; termwiz 0.23's `InputParser` does not classify
  terminal replies or parse kitty's colon fields; crossterm 0.29 owns stdin and drops kitty's
  base-layout key and associated text. The prototype (raw mode, the batched probe with a
  deadline, keys, mouse, paste, the grid and diff writer, bounded nonblocking output,
  restoration on exit, panic, SIGTSTP/SIGCONT, SIGHUP and before a forced exit) passes 38 tests,
  14 under a pty with watchdogs. Not yet run in a real terminal (foot or kitty in a private
  compositor): phase 2.

## 10. The contract as built (phase 2's start)

Where it differs from the sketches in §2–§3:

- `sighurt_api::present` has the CPU frame, moved up from the Wasm engine: `Frame { pixels,
  size, stride }` and `CpuTarget`. There is **one pixel format**, wl_shm's ARGB8888
  (premultiplied, B, G, R, A in memory, top row first): no `PixelFormat`, so no target converts;
  an engine whose renderer reads back RGBA bottom-up (Servo's GL) converts while it copies.
- `sighurt_api::semantic`: `SemanticSnapshot` (stamped with its `DocumentGeneration`), its nodes
  and `NodeKind`s, `SemanticAction` (`Activate`, `Focus`, `SetValue`, `SelectOption`, `Submit`,
  `ScrollIntoView`; a checkbox is toggled by `Activate`), the bounds (`MAX_NODES` 20 000,
  `MAX_DEPTH` 256, `MAX_STRING_BYTES` 16 KiB, `MAX_TOTAL_BYTES` 1 MiB, `MAX_OPTIONS` 1 000), and
  `SemanticSnapshot::check`, which also refuses a password field with a value.
  `SemanticAction::is_activation` says which actions only a user's input may cause.
- The engine contract: `EngineCapabilities::semantics`, `EngineCommand::SetSemantics(bool)` and
  `EngineCommand::Semantic { document, action }`, and `EngineEvent::SemanticsChanged { view,
  snapshot }`, which is replaceable. Every engine declares `semantics: false` and refuses both
  commands with `Unsupported` until it implements them.
- The core: `BrowserEvent::Semantic { view, document, action, link, at }`, dropped unless the
  view is shown and `document` is still its document; an activation records the user's
  activation, with `link` only for `Activate`, and gives the page the keyboard. The core ignores
  `SemanticsChanged`.
- The runtime: `Session::set_semantics(view, on)` (the frontend's choice; a refusal is not
  reported to the core) and `Frontend::semantics_changed(view, snapshot)`, called with snapshots
  that passed `check` (a malformed one is logged as an error). The runtime does not compare a
  snapshot's document with the core's, since the commit before it may still be queued for the
  core: a snapshot of a just-replaced document reaches the frontend, and an action on it is
  dropped by the core and the engine.

### Text mode and `--dump` as built

- **Mode.** `Tui::wants_text`: `chrome.toggle-mode`, `chrome.text-mode` and `chrome.graphics-mode`
  set a choice for the run; without one, `[tui] mode` (`auto` is text when the terminal can't show
  kitty images). A changed `[tui] mode` in the file drops the run's choice. Text mode gives the
  shown view the page rows' pixels (cells times cell pixel size; 1280x800 at scale 1 when the
  terminal reports none or the area exceeds the image cap) and takes each frame the engine offers
  at the `[tui] fps` pace, without encoding or sending it (decision 105; pages that draw or commit
  only for a view whose frames are taken gave no text at empty metrics). Switching to graphics
  shows the target's latest frame. `graphics-mode` on a terminal without images is a notice.
- **Semantics.** `App::sync_semantics` asks (`Session::set_semantics`) for exactly the shown view
  in text mode whose engine declares `semantics`, and stops asking, and drops the view's page,
  when it is hidden or the mode changes. An engine without them gets the page-rows message "No
  page text from this engine …" (the keys in it are the example configuration's, decision 116).
- **Keys.** The keymap runs first. What it leaves for the page, including `page.send-key
  ArrowDown` from the built-in `j`, reaches `Frontend::page_input` (new in `sighurt-runtime`:
  called for each `EngineCommand::Input` before the engine, `true` keeps it from the engine), and
  in text mode the shown view's keys end there: ArrowUp/Down, PageUp/Down, Home/End scroll, Tab
  and Shift-Tab move the focus, Enter activates (`text::Nav`). Text mode's navigation is therefore
  the configured commands plus those named keys; nothing is bound in code.
- **Activation.** `Action` goes to the core as `BrowserEvent::Semantic` (never to the engine);
  `EditField` opens a one-line editor over the last page row and `ChooseOption` a list over the
  bottom rows (8 at most). While one is open every key goes to it before the keymap (typing `q`
  is typing; Escape cancels, Enter commits as `SetValue` or `SelectOption`; the chooser takes the
  arrow keys, Home and End). A multi-line text area is edited as one line. A password's value is
  masked and starts empty. A new document drops an open prompt.
- **Mouse.** In text mode the wheel scrolls three lines and a left press focuses and activates
  the link or control under it; other buttons and motion are ignored. In graphics mode a button
  pressed over the page captures the pointer (`input::PointerCapture`): its motion and its release
  go to that view with positions relative to the page's top-left, negative above and left of it,
  until released or the view closes (review finding 8). The terminal is asked for modes 1000,
  1002 and 1006: motion arrives only while a button is held (1003, any motion, would flood
  through tmux and only hover would use it), and at most one motion every 16 ms reaches the
  page, the newest kept and handed on before any later event so a release never overtakes it.
- **`sig --dump URL [--width N] [--timeout SECS]`** (`sighurt_tui::dump`; no terminal, no Wayland,
  no sighurt-ui, no session file written). The engine is `--engine`, else WPE WebKit, else the
  first compiled-in engine with `semantics`, else an error. Only that engine's runtime starts
  (Servo does not, `Registry::configure`). The page is complete when the core's tab is loaded; then
  the engine is asked for a snapshot again, and the first one that arrives, once 300 ms pass
  without a newer one, or 3 s after the first since the ask on a page whose text never settles
  (Servo re-extracts every 250 ms while the DOM changes; the latest snapshot is printed then) (an extraction already running when the load ended may deliver an earlier
  state), is printed. A navigation error exits 1 with one line on stderr; the timeout (default 30
  s) exits 2. If stderr is a terminal, logging goes to the log file. The demo engine now has
  semantics (a heading, a paragraph, two links, a field, a checkbox, a select and a button), so
  all of this is tested in a pty without WPE (`apps/sighurt-browser/tests/tui.rs`).
- **Find in page** (decision 130). `page.find` without text opens the find field: the bottom
  line's editor behind a `/` prompt, holding the view's search selected. Each edit runs `page.find
  <text>` (trimmed, at most 1024 bytes), text that is empty or only whitespace `page.find-stop`;
  Enter closes the field and gives the page the keyboard, keeping the search for `page.find-next`
  and `page.find-previous` (`n` and `N` in the example keymap); Escape closes it and ends the
  search; the core moving the focus (`location.focus`, a tab change) closes it and keeps the
  search. The core's search reaches the runtime as `Effect::Find`, which offers it to
  `Frontend::find` first. In graphics mode the TUI declines and the engine searches. In text mode
  it takes every search of the shown view: `textmode::find` searches the page's logical lines, the
  text before wrapping (rows a wrap broke are joined, with the space a break dropped; a row's
  prefix, an indent, quote bar or list marker, is not page text), maps each match back to its
  rows and columns, scrolls the current match into view and draws it in reverse video; `Next` and
  `Previous` wrap; upper case in the text makes the search case-sensitive (the core's rule). It
  answers the core with `BrowserEvent::FindResult`, as an engine would, and again when a new
  snapshot of the same document changes the count; a new document ends the search (the core also
  sends `Stop`). A search made before the page's first snapshot is answered when the snapshot
  arrives. Changing the mode ends every view's search and says so once in a notice, since each
  search belonged to the other mode's finder: the shown page's through `page.find-stop` at once,
  a hidden tab's as soon as its tab is shown, its moves reaching neither finder until then.
  What the core's search found is
  right-aligned on the bottom line ("3 of 12", "12 matches", "no matches", "not supported by
  <engine>"), after the search's text (16 characters at most) when the field is closed; a
  terminal too narrow for both drops it. `--dump` is unchanged.
- **Open.** A stale page stays on screen until the new document's first snapshot (the frontend
  isn't told of commits); WPE text mode and `--dump` against real sites are checked by hand, not by
  `engines.sh` yet.

## 11. What page extraction can and can't guarantee (review, 17 October 2026)

An independent review (GPT-6 Astra) of `sighurt-semantic` (5d6e651), with the findings to fix
listed in `docs/rewrite/STATUS.md` decision 98. What the design now says:

- **In Servo, extraction and actions are unauthenticated page behaviour.** Servo 0.5 has no
  isolated script world, so `EXTRACT` and the action scripts run in the page's main world, after
  the page's own scripts. The page can replace every built-in the script uses, read the state
  slot's name (`Reflect.ownKeys`), and redirect the id map, so the snapshot may not match the DOM
  and an action may reach another node than the one the user chose. Native code guarantees only
  the shape and bounds of what it accepts (`parse`, `SemanticSnapshot::check`), never that it is
  true. That is acceptable for a page's own content (a page can always lie about itself, and an
  action can only do what the page's own script could), and it is said in the user
  documentation. Trustworthy dispatch needs engine-owned handles (Servo's accessibility tree with
  actions, if a later Servo has them) or an isolated world.
- **In WPE WebKit, the scripts run in the isolated world `sighurt`**, so the page can't see or
  replace the script's built-ins or state; the DOM it walks is still the page's, so getters on
  page objects (custom elements, shadow roots) are still page code.
- **Execution is cooperative.** The 50 ms budget is checked by the script; a getter that never
  returns can't be interrupted from JavaScript. The adapters never wait for a result on the owner
  thread, so a hung extraction costs that page, not the browser; an engine-level interrupt is a
  separate decision.
- **Passwords:** a field identified as a password never has its value read; a field whose type
  can't be read is treated as a password. A page can still copy its own secrets into text, which
  no extraction can prevent. Adapters never log action scripts (`SetValue` carries plaintext).
