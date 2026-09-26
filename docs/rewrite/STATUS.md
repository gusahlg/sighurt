# Sighurt rewrite: status

The specification is [`sighurt-rewrite-plan.md`](../../sighurt-rewrite-plan.md); section numbers
below refer to it. The pre-rewrite browser is at git tag `gpui-baseline`.

Tick a pass criterion only with evidence, and link or name it: a test, a measurement, a run on
a named machine and compositor. A criterion that turned out not to hold goes in the decisions
log with what was decided instead.

## Where things stand

- **Foundation.** The workspace has the crate layout of §5. `sighurt-api` v0 exists: ids, units
  and scale, input, the engine contract, events, services and commands, with unit tests for id
  allocation, scale rounding, the engine waker and command parsing. `sighurt-core`,
  `sighurt-wayland`, `sighurt-ui`, `sighurt-engine-servo`, `sighurt-engine-demo` and
  `sighurt-browser` are skeletons: module files whose documentation states their
  responsibility. `sig` prints its version and help and otherwise says the rewrite is in
  progress. `sighurt-engine-wasm` and `sighurt-ipc` are unchanged, still the old `sig-wasm`
  process. The API went through a plan-conformance and implementability review; what changed
  is in decisions 9 and 11–17.

## Milestones (§23)

### M0 — Record the baseline and prove engine coexistence

Results, versions, environment and every decision: [`M0.md`](M0.md). The experiment is
`experiments/m0-coexist`; its evidence is a run on `louise-pc` on 26 September 2026
(`tools/remote tools/cargo run -p m0-coexist`: every check passed in both initialization
orders and both processes exited with 0). The inventory is [`compat-inventory.md`](compat-inventory.md).

Build/do:

- [ ] Baseline measured: build the old browser (`sig`, `sig-servo`, `sig-wasm`) from a
  `git worktree` of `gpui-baseline`, run it on a named machine, compositor and small fixed
  workload, and record timing and memory. M7 repeats the same workloads, so write them down.
  *Not done yet* (M0.md, open problem 9).
- [ ] Required user-visible behavior of the baseline recorded (what the daily workflows are).
  The behavior is recorded (compat-inventory.md §1–§6); which of it the user's daily workflows
  rely on is for the user to name, and M6's pass criterion needs that list.
- [x] Wayland and driver environment recorded: River 0.4.8 with gharial 0.3.0, NVIDIA RTX 3070
  on the proprietary driver 595.99.02, Mesa 26.2.3, on `sighurt`; and the build machine
  `louise-pc` (M0.md, "Environment"). Dependency versions: M0.md, "Versions" (decisions 3–5).
- [x] Servo and Wasmtime compiled and linked into one executable: `m0-coexist`, with the Wasm
  engine's FFmpeg, rodio/cpal/ALSA, Tokio and reqwest (rustls) dependencies too, from the root
  lockfile with no version change.
- [x] Both engines initialized in that program: JavaScript and Wasm executed, a guest trap,
  several views and stores, clean shutdown (M0.md, "Results", steps 3–10).
- [x] Process-global setup reviewed, each item decided and owned by `sig` (M0.md,
  "Process-global setup"; decision 26):
  - [x] Signal handlers: both orders ran with the other engine's handler on top; guest traps in
    Wasmtime and JavaScript Wasm traps in SpiderMonkey, at the same time on different threads,
    all reached the right runtime (steps 6–7).
  - [x] TLS provider: without one, `ClientConfig::builder()` panics in this graph (step 1);
    `sig` installs aws-lc-rs first, and reqwest follows it (step 2).
  - [x] Panic hook: specified, and the experiment's hook counts style-thread panics and leaves
    Servo working (step 8). It never exits; Servo runs with `hard_fail: false`.
  - [x] Logging: the one `log` logger is Servo's `setup_logging()` (the only way script-thread
    panics reach Servo), called by `sig` on a Servo runtime built at startup; without Servo,
    `sig` installs `env_logger`.
  - [x] Profile path: `{data_dir}/sighurt/servo`, locked by the one runtime for its whole
    life; a second `sig` runs Servo without a profile. Servo writes it while dropping (step 10).
  - [x] Environment mutation and every exit path: only surfman mutates the environment
    (`LIBGL_ALWAYS_SOFTWARE`, `DRI_PRIME`; step 4 shows it); none of the old `process::exit`
    and `_exit` paths comes along (M0.md, "Exit paths").
- [x] Compatibility inventory recorded (compat-inventory.md):
  - [x] Commands: the 17 names in `sighurt_api::commands::Command::NAMES`, with their baseline
    behavior (§2).
  - [x] CLI: `-h`/`--help`, `-V`/`--version`, `--default-config` (printed the built-in
    configuration; not in the new `sig` yet), URLs and paths as arguments (§1).
  - [x] Configuration: `default_engine`, `[engines.<name>]` (`command`, `schemes`,
    `extensions`, `mime`), `[ui]` `home` and `search`, key bindings, `[ui.colors]` (§3–§4). At
    the baseline the bindings live in **`[ui.keys]`**, `[ui.keys.page]` and `[ui.keys.location]`;
    a top-level `[keys]` is rejected there (`gpui-baseline:sighurt-core/src/config.rs`, test
    `typos_are_errors`), although the old CLAUDE.md says `[keys]`.
  - [x] SDK: the `"oxide"` import module, the guest exports (`start_app`, `on_frame`,
    `on_timer`, `on_event`), all 211 registered host functions by area, and the example guests
    as the compatibility corpus (§7–§8).

Pass:

- [x] A minimal combined program links and runs correctly: `m0-coexist` on `louise-pc`, both
  orders (M0.md, "Results"). No process boundary is needed for coexistence (decision 26).
- [x] No blanket unsafe workaround was needed to force compatibility: the experiment's only
  `unsafe` reads signal dispositions for the record.

### M1 — Prove mixed rendering in one Wayland window

The experiment is `experiments/m1-mixed`; [`M1.md`](M1.md) has the runs (L1/L2 on River with
the RTX 3070; G1–G3 in headless sway on the same GPU, N1–N3 on `louise-pc`'s RTX 4060, both with
sway's GLES2 renderer; H1/H2/A1/C1/N4 with llvmpipe), the measurements, the exact sequences and
the limitations. Tests: `cargo test -p m1-mixed` (6 unit tests: frame pacing, the refresh
driver, key text, the report); clippy with `-D warnings` is clean.

- [x] A real HTML page and an animated/WebGL test present through the GPU path. *L1/L2: GL
  "NVIDIA GeForce RTX 3070/PCIe/SSE2" on River; `animated.html` (CSS animation + WebGL 1) at
  the 120 Hz output's pace, 2 063 frames in 20 s.*
- [x] There is no ordinary page `read_pixels`/CPU image transport path in use. *Every run:
  `read_to_image` 0 and 0 `glReadPixels`/`glGetTexImage` among WebRender's audited GL calls.*
- [x] Repeated resize, scale changes, hide/show, and teardown behave correctly. *G1/G2, N1/N2
  (NVIDIA) and H1/H2 (llvmpipe): 100–200 % fractional and integer scales, a 40-size storm, 6
  hide/show cycles; L1: 5 cycles with a frame callback outstanding at each hide; every run: no
  protocol error, context destroyed before its surface, exit 0.*
- [x] Keyboard focus routes through the window to page/chrome correctly. *G1, N1, H1: keyboard enter
  only on the root; keys go to the page or the address field by logical focus; clicks, Ctrl+L
  and Escape move it; Servo reports focus and blur.*
- [x] Page animation does not cause repeated CPU chrome painting. *L1: 2 chrome paints during
  2 063 page frames; G3, N3 and A1: 2 during about 1 000.*
- [x] A static chrome-only experiment sleeps when there is no work. *C1: 0 loop wakeups in 10 s;
  a static Servo page too (L2, H1).*
- [x] Diagnostics identify the actual GL renderer and fallback mode. *GL and EGL strings and the
  presentation path in every log; llvmpipe runs say "SOFTWARE FALLBACK … not the GPU path".*
- [x] The result runs on the actual River/gharial system and GPU driver intended for use. *L1,
  L2: River 0.4.8, gharial 0.3.0, RTX 3070, driver 595.99.02; no screenshot (grim's toplevel
  capture fails on River, see M1.md).*

### M2 — Extract the core and neutral contracts

Core half (`sighurt-core`, decisions 27–30 and 41): `update(&mut BrowserState, BrowserEvent) ->
Vec<Effect>` over tabs with stable ids, their views and the §20 view states; navigation
coordination (the shown page stays until its replacement is ready and has committed); history
segments with bounded retained views and records that reload honestly; routing without a
network probe; the 17 commands with the baseline's semantics; key bindings; service,
permission, pop-up and download policy with the baseline's page-data bounds; the session
snapshot; configuration format version 1, whose default file (`config::DEFAULT_CONFIG`) is
`sig --default-config`'s text. Not wired into `sig` yet. Tests: 42 unit tests in
`sighurt-core/src/*` and 41 in `sighurt-core/tests/browser.rs`, which drives the core as `sig`
will, events in and effects out, with a fake engine. The §25 core tests are
`a_closed_tab_ignores_late_events`, `an_engine_switch_keeps_the_tab_and_rejects_the_old_view`,
`same_document_history_is_the_engines_not_url_guesses`,
`back_and_forward_cross_retained_segments`, `an_invalid_reload_keeps_the_configuration_in_use`,
`permissions_belong_to_one_document` and `an_answer_for_a_replaced_document_is_dropped`. Each
guarantee was also broken on purpose (the late-event filter, closing views, the scope of a
runtime failure, per-document permission answers, the forward limit, consumed releases, stale
documents, keeping the page until commit, pop-up activation, pruning on commit, typing mode,
the retained-view bound, a failed reload): each of the 13 breakages failed the test meant for it.
After the review (decision 41), 15 more breakages in a scratch copy each failed the test meant
for it: forgetting superseded requests (`quick_traversals_are_known_when_they_commit`), pruning
on same-document page commits, keeping a cancelled switch view, a stopped record reloading
itself, back not leaving a loading record, a repeat starting a binding, accepting unknown `[ui]`
keys, checking the version late, dropping unknown top-level keys, ignoring each of the history
traversal and page zoom capabilities, one view per page for engines without history,
`OpenFailed(Runtime)` failing only the view, an unchecked session and web pages opening `file:`
URLs.

Open for integration: a page's own cross-document traversal or reload in a segment with later
segments still discards them (decision 41: it needs the engine to say what a commit did, which
waits for the Servo adapter); the Wasm engine's history starts afresh with each document, so
after `page.open` of another app in the same Wasm view, back skips the previous app; a download
that an engine finds while loading a page that never commits can't be asked for, since service
requests act for a committed document.

- [x] Core tests run without a display or either engine: `cargo test -p sighurt-core` with
  `WAYLAND_DISPLAY`, `DISPLAY` and `XDG_RUNTIME_DIR` unset, 83 tests passed (this machine,
  26 September 2026, after the review); the only engine is the tests' fake.
- [x] UI/platform types are absent from the core's dependency graph: `cargo tree -p
  sighurt-core -e normal` lists 43 crates, sighurt-api, serde, toml, url and their parsing
  dependencies (idna, icu_*, winnow, …): nothing of GPUI, Wayland, Servo, Wasmtime or graphics.
- [x] Late events for closed/replaced views are rejected: `a_closed_tab_ignores_late_events`
  and `a_replaced_view_is_forgotten` send every `EngineEvent` variant for a closed and an
  evicted view, and a late `ServiceDone` and `PermissionAnswered`: no effect, tabs unchanged.
  Events from the wrong engine (`a_replaced_view_is_forgotten`), the failure of a superseded
  navigation (`stale_and_superseded_navigations_are_ignored`) and requests of replaced documents
  (`an_answer_for_a_replaced_document_is_dropped`) are dropped too; a superseded traversal the
  engine still commits counts as the traversal it was (`quick_traversals_are_known_when_they_commit`).
- [x] Configuration errors are handled without replacing valid state:
  `an_invalid_reload_keeps_the_configuration_in_use` (the error names the problem, the old
  bindings keep working, a valid reload applies at once); `config::tests::every_problem_is_reported`,
  `config::tests::an_old_file_with_a_version_says_what_moved` (the old `[ui] home`, `search` and
  `[ui.keys]` are errors that say where they went, not silently dropped).

### M3 — Build the actual Wayland host and CPU chrome

- [ ] Usable address/command editing.
- [ ] Reliable tab selection with dummy views.
- [ ] Bounded buffer lifetime.
- [ ] No rendering while idle.
- [ ] Correct pointer/keyboard coordinates.
- [ ] Normal close and compositor disconnect handled deliberately.

Chrome half (`sighurt-ui`, tested without a compositor; the criteria above also need the host and
`sig`). Tests are in `sighurt-ui/src/*` and `sighurt-ui/tests/{interaction,paint}.rs`:

- [x] Address/command line editing by grapheme cluster and Unicode word (å ä ö, combining marks,
  ZWJ emoji, dead keys, AltGr, Ctrl shortcuts on non-Latin layouts), selection, paste without
  line breaks, input-method preedit/commit, horizontal scrolling, and `:command` lines
  (`text_edit::tests`, `swedish_typing_with_dead_keys_and_altgr`,
  `selection_copy_cut_and_paste`, `input_method_preedit_and_commit`,
  `typed_commands_run_after_a_colon`, golden `typing-150`, `preedit-150`, `scrolled-125`). A
  clipboard answer is pasted only for the latest Ctrl+V, once, while the line has kept focus and
  its tab (`a_late_clipboard_answer_is_ignored`); an Alt shortcut the core didn't bind types
  nothing, as in the old bar (`alt_shortcuts_type_nothing`).
- [x] The old URL-bar semantics: focus selects all, Escape discards and returns focus to the
  page, Enter opens and the line keeps showing what was opened until its tab commits, stops
  loading or is switched away from, switching tabs drops typed text, and the committed URL is
  never replaced by what is typed (`escape_discards_and_enter_opens`,
  `an_opened_line_stays_until_its_load_ends`, `the_committed_address_is_not_the_typed_one`,
  `a_new_tab_focuses_its_address_selected`, `clicking_the_address_selects_then_places_the_cursor`).
  Focus moves after commands are the core's, which the chrome follows (`Chrome::set_focus`,
  `focus_follows_the_core`); tab titles are the core's, contained (`state::tests::titles`,
  `page_titles_are_contained`). Decision 40.
- [x] Pointer on tabs and buttons issues the right commands; a press and release on different
  targets, or a release alone, is no click; drags that leave the chrome end cleanly
  (`tabs_select_close_and_open`, `toolbar_buttons`, `dragging_out_of_the_chrome_selects_and_never_sticks`).
- [x] Coordinates: hit targets stay on the drawn controls at 100, 125, 150 and 200 % at odd
  widths (`hit_targets_stay_on_drawn_controls_at_every_scale`: every pixel drawn in a control's
  color hits it, and every pixel that hits it lies within that drawing, give or take the address
  field's border); surface-local positions are converted
  once (`surface_positions_are_converted_once`); the input method's caret rectangle is where the
  caret is painted at all four scales (`the_input_method_caret_is_where_the_caret_is_drawn`).
- [x] No chrome repaint without a visible change: identical snapshots, pointer motion within a
  control and anything the chrome doesn't show damage nothing, and a change damages only its own
  surface (`idle_snapshots_repaint_nothing`, `hover_sets_the_cursor_and_repaints_only_the_chrome`).
  The caret is steady, so the chrome has no timer at all.
- [x] Painting: whole-buffer repaints into `ARGB8888` (premultiplied, little-endian), padded
  strides, refused wrong sizes (`paints_cover_the_whole_buffer`, `padded_strides`,
  `bad_buffers_are_refused`, `paint_cpu::tests::rgba_becomes_argb8888_little_endian`); crisp
  edges at all four scales (`edges_are_crisp_at_every_scale`); eleven golden images in
  `sighurt-ui/tests/golden/`, each looked at; layout and painting at odd sizes
  (`layout::tests::odd_sizes_keep_everything_inside`, `odd_sizes_paint`).
- [x] Accessibility tree as plain data, matching the hit targets
  (`accessibility_matches_the_hit_targets`).
- [ ] Shown in a real window on River (needs `sighurt-wayland` and `sig`).

Measured on this machine (release build, `measure_with_system_fonts`, DejaVu Sans chosen from the
installed fonts): `Fonts::system()` 2.8 ms; the 1920-wide chrome repaints in 0.27 ms at 100 %
(1920×84) and 0.48 ms at 200 % (3840×168) with warm glyph caches, 2–14 ms cold; the home page at
200 % (3840×1952, repainted only when it changes) 23 ms.

Host half (`sighurt-wayland`). Unit tests are in `sighurt-wayland/src/*` (48, no compositor
needed); protocol tests in `sighurt-wayland/tests/headless.rs` (17) run against a private
headless sway 1.12 (pixman renderer) through `sighurt-wayland/tests/headless.sh`, which drives it
with wtype, wlrctl, swaymsg, grim and wl-clipboard, plus a virtual pointer and an input method
written in the test. The protocol tests are `#[ignore]`d, so a plain `cargo test` lists them as
ignored, not passed, and they refuse to start outside the script's compositor. After the review
(decision 44), `headless.sh` passed three runs in a row on this machine (16 tests, then the one
that stops the compositor), and each of the review's three fixes, broken on purpose, failed the
test written for it:

- [x] Bounded buffer lifetime: SHM buffers are free, drawing or submitted, reused only after
  `wl_buffer.release`; at most 3 per size and 2 retiring sizes, beyond which a new size waits
  (`shm::tests::a_delayed_release_never_lends_a_submitted_buffer`,
  `a_resize_storm_stays_bounded`, `without_releases_a_new_size_waits_instead_of_allocating`).
  Against sway, a storm of 30 output modes held at most 2 buffers and 1 retiring generation per
  target (`a_resize_storm_keeps_buffers_bounded`). The bound is a count; there is no separate
  byte limit, so retiring bytes are at most six buffers of recent sizes. A lent buffer belongs to
  its `ShmFrame`, so a painter holds no borrow of the host: the tests' painter reads its lease's
  metrics and the host's diagnostics while it paints every SHM frame.
- [x] No rendering while idle, host side: 0 loop wakeups and 0 permits in 2 s idle
  (`an_idle_window_sleeps`); invalidations coalesce, a frame callback alone never makes a frame,
  a hidden target never presents, and unmapping forgets a callback that may never come
  (`scheduler::tests`).
- [x] Layout transactions (§10): a second placement while a transaction is open gets both new
  frames in the same loop iteration (8.7–15 ms, painting included) and the root commits with
  them, with no target late (`a_second_placement_joins_the_layout_without_waiting_for_the_grace_period`,
  `scheduler::tests::a_new_layout_presents_without_the_callback_of_a_cached_commit`). A page with
  no frame when the 150 ms grace ends is unmapped and the root's background shows; the frame it
  submits later for the same layout maps it at the next flush, with no further frame
  (`a_frame_after_the_grace_period_still_maps_its_page`, asserted on grim screenshots).
- [x] Pointer coordinates: surface-local to the target under the pointer (window y 40 is the
  page's row 0); a drag stays with the target that got the press, in its own coordinates, until
  the release, then leave and enter (`pointer_positions_are_local_to_the_surface_under_the_pointer`,
  `a_drag_from_the_page_into_the_chrome_stays_with_the_page`,
  `input::tests::a_drag_keeps_its_recipient_across_surfaces`,
  `losing_the_pointer_mid_drag_releases_the_buttons`).
- [x] Keyboard: the real Swedish layout through xkbcommon (å ä ö, AltGr+2 is "@" with AltGr
  consumed and never Ctrl+Alt, ¨ then a composes ä, Ctrl+a is a shortcut without text,
  `input::tests`); through sway with wtype: "åäö", `dead_diaeresis` + a, AltGr
  (`swedish_letters_and_a_dead_key_arrive_as_text`, `altgr_is_not_control_and_alt`); repeat from
  `repeat_info` on calloop timers (`a_held_key_repeats_at_the_compositors_rate`,
  `input::tests::repeat_*`). Keys go to the logical focus target the application sets.
- [x] Input methods (text-input-v3) against a real input method: a password field reaches it with
  purpose `password` and the `sensitive_data` and `hidden_text` hints; preedit and commit arrive
  as `TextInputEvent`s; it turns off with the field
  (`the_input_method_sees_passwords_as_passwords_and_its_text_arrives`); the caret is translated
  once into the focused surface (`text_input::tests::the_caret_is_translated_once_into_the_focused_surface`).
  A password field after a text field of the same page is a new session: `enable` again, and the
  input method no longer has the first field's text
  (`a_new_field_on_the_same_target_is_a_new_input_method_session`). Two fields of one target
  that look alike (same purpose and multi-line flag, both with surrounding text) stay one session
  unless `None` comes between them.
- [x] Clipboard both ways (wl-copy, wl-paste) with Swedish UTF-8 text, asynchronous pipes in the
  loop, and a read over its limit refused rather than truncated (`the_clipboard_goes_both_ways`).
- [x] Scale: fractional 150 % and 225 % with `wp_viewporter`, buffers exactly
  `Scale::buffer_size`, a new epoch and new buffers when only the scale changes
  (`resizes_and_scale_changes_reach_the_buffers`, `scale::tests`); screenshots exact to the pixel
  at 100 % (`chrome_and_page_are_placed_exactly`).
- [x] GL lease: an EGL presenter (Mesa, llvmpipe) on a `PresentMode::Gl` lease maps, resizes,
  hides (the root's background shows), shows with a bootstrapped frame, and tears down in the
  documented order with no protocol error
  (`a_gl_presenter_owns_its_leaf_from_first_frame_to_teardown`). The host never commits that
  leaf: in that test run with `WAYLAND_DEBUG=client`, all 4 `attach` and 4 `commit` requests on
  the page surface came from Mesa's EGL surface queue; the host sent it only pending state (frame
  callbacks, viewport, buffer scale, presentation feedback), its two subsurface roles and, after
  the presenter's teardown, `destroy`.
- [x] Close and disconnect, host side: a compositor close request arrives as `CloseRequested`
  and closes nothing; `close_window` while leases live is clean
  (`a_close_request_goes_to_the_browser`); losing the compositor is reported once as
  `ConnectionLost` with the protocol error when there is one, and the loop keeps running
  (`losing_the_compositor_is_reported_once`).
- [ ] The M3 criteria at the top: need `sig` to wire host, chrome and demo engine together.
- [ ] Run on River/gharial (not done: the desktop is in use). Also untested: the integer-scale
  fallback without a viewporter, a second compositor family, cursors (shape protocol and theme)
  by eye, the primary selection. Drag-and-drop is not implemented.

Measured with `sighurt-wayland/examples/host-demo` in headless sway: `wp_presentation` reported
frames on screen 4–15 ms after submission; at 150 % the 1280×800 output gives the chrome a
1280×60 buffer for 853×40 logical pixels, and the page a checkerboard whose 16-logical-pixel
squares are 24 output pixels (grim screenshots, looked at).

### M4 — Convert and mount Servo

- [ ] Browse a fixed set of pages.
- [ ] Open, close and switch tabs.
- [ ] Navigate back/forward, including same-document entries.
- [ ] Reload and stop.
- [ ] Use the clipboard and text input.
- [ ] Resize across scales.
- [ ] Close without an adapter-triggered whole-process exit.

### M5 — Mount Wasm while preserving the SDK

Build/do:

- [x] `sighurt-engine-wasm` is a library (decision 22): `WasmRuntime<T: CpuTarget>` implements
  `EngineRuntime`; the `sig-wasm` process (stdin/stdout loop, stdout dup, exiting panic hook,
  `process::exit`) and `sighurt-ipc` are deleted. One Wasmtime engine per runtime, one store
  per document; guest callbacks on bounded worker pools, each guest keeping its own cadence on
  the worker side (decisions 23 and 42).
- [x] Page logic ported from the old `page.rs` into `view.rs`: navigation, reload, stop, the
  view's session history across documents (`push_state`, `Traverse` within a document and to
  earlier and later documents, which load again), the permission prompt, links, hover status
  and cursor, zoom, focus, visibility; page facts become `EngineEvent`s. Keys map from the
  api's `Key`/`KeyCode`, typed text comes from the key event's `text` or an IME commit.
- [x] CPU rasterizer (`raster.rs`, tiny-skia + cosmic-text) paints the display list into a
  lent wl_shm ARGB8888 buffer; the baseline's draw-list bounds are kept (`draw::bound`), and a
  frame's work is bounded (decision 43).
- [x] `screenshots` behind the `screen-capture` feature, off by default: `api_screen_capture`
  reports a failed capture (-3) without prompting.
- [ ] The executable's glue: `CpuTarget` for `sighurt_wayland`'s SHM target lease and frame
  permit, the runtime in `sig`'s registry, and a run under `tools/headless-sway`.

Compatibility sample (tests in `sighurt-engine-wasm/tests/`, run on this machine on 26
September 2026 with the example guests built for `wasm32-unknown-unknown`; the SDK and examples
are unchanged). `cargo test` of the crate needs those guests built first
(`tools/cargo build --target wasm32-unknown-unknown --release -p hello-sighurt -p raf-demo -p
gradient-demo -p typography-demo`); CI's test job doesn't build them yet.

| Guest | What it exercises | Evidence |
|---|---|---|
| hello-sighurt | shapes, text, 100 %/200 % scale, page zoom | `runtime::hello_sighurt_draws` (pixel probes) |
| raf-demo | `request_animation_frame` via `on_timer`, `on_frame`, hidden-view policy | `runtime::raf_demo_animates`, `runtime::a_hidden_view_steps_once_a_second` |
| gradient-demo | linear gradients, text | `golden::gradient_demo` (probes, golden image) |
| typography-demo | `canvas_text_ex` weights/styles/alignment, `measure_text`, 150 % | `golden::typography_demo`, `golden::typography_demo_at_150_percent` |
| WAT guests | timers, custom and resize events, typing (dead key, Ctrl), Enter, links (hover, click, middle click), `push_state` + `Traverse`, back and forward across documents, permission prompt, KV store shared by two views, workers | `runtime::*` |
| timer-demo, events-demo, audio-player, sse-demo, fullstack-notes, index | not exercised yet | — |

Pass:

- [ ] The chosen existing guests run with documented compatibility. Engine side: the table
  above, through the real `EngineRuntime` with an in-memory target. Not yet in `sig` on a
  Wayland target (the glue above).
- [x] Looping guests hit execution limits (decision 42), each stopped in bounded time with the
  owner's longest pump under 50 ms: an endless `on_frame` by fuel
  (`runtime::a_looping_guest_hits_the_fuel_limit_without_blocking_the_owner`, 27 ms); a host
  call per iteration with unlimited fuel by the 200 ms time limit
  (`runtime::a_guest_calling_the_host_in_a_loop_hits_the_time_limit`, 203 ms); a loop around
  `api_fetch`, whose waiting isn't charged to the time limit, by the waiting limit
  (`runtime::a_guest_that_keeps_the_host_waiting_is_stopped`: 300 ms allowed, stopped after
  317 ms); a loop in a module's `start` function by the budget of `start_app`
  (`runtime::a_start_function_has_the_budget_of_start_app`); a flood of 4096 timers spread over
  steps (`runtime::a_flood_of_timers_is_spread_over_steps`); loads, steps and workers of a
  closed, replaced, stopped or terminated document at the next epoch tick
  (`runtime::stopping_or_closing_a_load_stops_its_guest`,
  `runtime::workers_are_bounded_and_terminating_stops_them`,
  `engine::tests::cancelling_stops_a_running_call`); unit tests in `engine::tests`. What no limit
  interrupts: a host call in progress (a synchronous fetch, up to its 30 s timeout; compiling a
  module of up to 50 MB) and a file dialog, which waits for the user.
- [x] Invalid drawing/resource requests fail safely:
  `runtime::invalid_drawing_and_resource_requests_fail_safely` (NaN/infinite geometry,
  pointers outside guest memory, a broken image, 240 000 draw commands and 120 000 links in one
  frame: the view lives, the frame is painted in budget),
  `runtime::the_permission_prompt_cannot_be_hidden_or_answered_unseen`,
  `runtime::history_is_bounded`, `runtime::a_page_cannot_show_another_origins_url`,
  `runtime::workers_are_bounded_and_terminating_stops_them`, `raster::tests::a_frame_does_bounded_work`,
  `raster::tests::a_frame_of_text_does_bounded_work`, `draw::tests::bound_*`, and the unit tests
  of the capabilities, navigation, worker, fetch, WebSocket, SSE and RTC modules. Bounded:
  draw commands and their bytes, images (count, 128 MiB of pixels a frame, side), the decode
  cache (64 MiB a frame), text (drawn and measured), font family names, gradient stops, clip
  depth, raster work per frame, console, timers, animation frames, hyperlinks (4 MiB of URLs),
  session storage, a view's whole history (1000 entries, 4 MiB), event listeners and payloads,
  worker threads and their queues (16 MiB each way), `api_fetch` and `api_audio_play_url`
  bodies, streaming fetch, WebSocket and SSE (handles, queued bytes with backpressure, message
  size), RTC (peers, channels, queued messages). Not audited: video (`api_video_load_url` blocks
  for its HTTP timeouts and reads HLS playlists without a size limit) and MIDI message sizes.
- [x] Stale completions are discarded: `runtime::a_superseded_load_is_discarded`,
  `runtime::a_step_that_finishes_after_navigation_is_discarded`,
  `runtime::nothing_arrives_for_a_closed_view`; what they leave is dropped off the owner thread
  (`view::tests::replaced_loads_and_closed_views_stop_and_are_dropped_elsewhere`,
  `pool::tests::a_dropped_pool_drops_its_jobs_on_its_threads`).
- [ ] Guest activity does not starve chrome input. Engine side: with six guests taking 40 ms a
  frame on the four worker threads, a key reached another guest in 2–21 ms and no `pump` took
  more than 24 µs (`runtime::input_reaches_a_guest_while_others_are_busy`); a page that draws
  the same frame every frame wakes the owner 0 times a second
  (`runtime::a_static_page_leaves_the_owner_asleep`); another guest measuring 8 MiB of text a
  frame doesn't slow painting (`runtime::measuring_text_does_not_hold_up_painting`: 1 ms);
  history reports of a guest replacing 3 MiB of history every frame took 2 % of the owner's
  time, 2 ms at most per pump (`runtime::history_is_bounded`). Open: `present` rasterizes on
  the owner thread. Measured on this machine (debug build of the crate, optimized
  dependencies, idle priority while the desktop was in use): the example guests 7–105 ms per
  frame at 800×600 to 1920×1080 over three runs (`golden::*`, whose three painted images were
  looked at after the rasterizer changes), and a hostile frame that
  uses up the whole budget 292 ms at 1920×1080 (`raster::tests::text_costs_what_shaping_it_does`).
  Needs measuring in `sig`, and probably painting off the owner thread.

### M6 — Complete browser behavior

- [ ] The user can complete the intended daily workflows.
- [ ] Failures identify the responsible view/engine.
- [ ] Every retained feature has a tested implementation or a clearly documented unsupported
  state.

### M7 — Measure, compare, and retire the old display path

- [ ] Evidence shows the normal page readback path is eliminated.
- [ ] No major functional regression in the agreed workflows remains.
- [ ] Lifecycle tests show no unbounded resource growth.
- [ ] Remaining regressions have an explicit decision.

## Decisions log

Newest last. Each entry says what was decided and why.

1. **No IPC: engines are compiled into `sig` and run in its process** (§18's initial decision).
   Cargo features on `sighurt-browser` select them: `engine-servo`, `engine-wasm`,
   `engine-demo`. The `default = [...]` line of `sighurt-browser/Cargo.toml` is the one small
   place that decides which engines a default build compiles and links. *Why:* it removes the
   control-transport machinery and makes native presentation into a host-owned subsurface
   possible; §18 records the cost (shared address space and fate, no sandbox claim).
2. **GPUI is gone. The chrome is drawn on the CPU (cosmic-text + tiny-skia) into Wayland SHM
   subsurfaces, and Sighurt is Wayland-only.** *Why:* §1 and §13: a small chrome needs no GPU
   renderer of its own, and the page no longer passes through a UI toolkit's renderer.
3. **Servo stays pinned at 0.5, with surfman 0.13.** *Why:* its build is cached in `target/`, and
   `servo-paint-api` 0.5.0 already has `WindowRenderingContext::new(DisplayHandle,
   WindowHandle, size)` and `new_with_refresh_driver`, which is the presentation path M1 needs
   (§14: pin one version and code against its sources, not the 0.6 documentation).
4. **One dependency graph.** Without GPUI, Servo 0.5, Wasmtime 36 and the Wayland/text stack
   (wayland-client 0.31 with the `system` backend, wayland-protocols 0.32,
   smithay-client-toolkit 0.20, calloop 0.14, cosmic-text 0.14, tiny-skia 0.11,
   raw-window-handle 0.6) resolve together. surfman and wayland-client 0.31 share wayland-sys
   0.31, so there is one libwayland. Only the Wasm engine's `screenshots` dependency pulls in
   wayland 0.30. The shared versions are pinned in the root `[workspace.dependencies]`. So far
   only Servo and Wasmtime are in `Cargo.lock`; the Wayland and text crates enter it when
   `sighurt-wayland` and `sighurt-ui` first depend on them.
5. **The root `Cargo.lock` was seeded from the Servo engine's lockfile**, and Cargo resolved the
   rest. *Why:* Servo and all of its dependencies keep exactly the versions already compiled
   (checked: every registry entry of the seed, including `mozjs_sys`, `servo` and `surfman`, is
   unchanged). Servo and the Wasm engine resolve together without `links` conflicts; they have
   not been compiled or run together yet (that is M0). The Wasm engine's dependencies were
   re-resolved in the process (for example Wasmtime 36.0.8 became 36.0.16); it has not been
   rebuilt since.
6. **Dependencies are optimized in dev builds** (`[profile.dev.package."*"] opt-level = 3`,
   `debug = false`). *Why:* Servo is unusable unoptimized, linking Servo with debug info is slow,
   and these are the settings the cached Servo artifacts were built with.
7. **`rust-version = "1.95"` and resolver 3.** *Why:* louise-pc, where Servo builds, has Rust
   1.95; resolver 3 makes Cargo choose dependency versions that support it.
8. **The pre-rewrite code is kept at git tag `gpui-baseline`**, not in the tree. The old GPUI
   crates were deleted; `sighurt-engine-servo` became a library in the root workspace.
   `sighurt-ipc` and the `sig-wasm` process stay until the Wasm engine is converted.
9. **Ids are opaque, never reused, and each kind has one process-wide counter.** `ViewId::next()`
   (and the same for every kind in `sighurt_api::ids`) is the only way to make an id, so a
   `NavigationId` an adapter allocates for a navigation its page started itself can never equal
   one the core allocated, and the same holds for `DocumentGeneration` and `RequestId`.
   *Why:* §6 requires that a late event never matches a newer object. The first version had
   shareable `IdSource`s, where uniqueness depended on the executable creating exactly one per
   kind; a second `IdSource::new()` (or a `Default` in a runtime's state) restarted at 1 and
   collided silently. A static counter makes it structural and needs no plumbing.
10. **`sighurt-ui` depends on `sighurt-api` only**, not on `sighurt-wayland` (§5 allows both).
    *Why:* it paints into buffer memory the host lends it, so it needs no Wayland types and its
    layout, editing and painting stay testable without a compositor.
11. **`EngineCommand` has no `Close`**: closing is `EngineRuntime::close_view`, which the §7
    trait already has. **Pages ask to open URLs with `OpenRequested { view, document, url,
    disposition }`**, where the disposition is `CurrentView` or `NewView`. This is the old
    protocol's `FromEngine::Open { url, new_tab }`, which had both: the old core routed
    `new_tab: false` like any load, possibly switching engines, and refused `sig:` targets
    (`gpui-baseline:sighurt-core/src/navigable.rs`). The Wasm engine needs it for guest
    `navigate` calls and link clicks, and Servo for navigations it shouldn't perform itself.
    A new view gets no `window.opener` link, as before; adding one is an API extension for when
    M4 shows it is needed.
12. **Engine contract rules** (`sighurt_api::engine`, "Event delivery"): events reach the owner
    only through `pump`, and the owner pumps every runtime it called in that iteration; events
    for one view keep their order; a pump does a bounded slice of work and keeps only the
    latest replaceable event (title, status, cursor, loading, history, text input, frame ready)
    per view; a view accepts commands as soon as `open_view` returns `Ok`, and `ViewReady`
    means it can present; `ViewFailed` leaves a reloadable view, `ViewFailed` + `ViewClosed` a
    finished one; a commit supersedes earlier pending navigations; `EngineRuntime::shutdown`
    closes everything in order before the host is released. *Why:* §7, §16, §19 and §20
    require these, and every adapter and the app loop must agree on them; Servo 0.5 shuts down
    only in `Drop for ServoInner`, and `WebView::load` returns no navigation id.
13. **An empty metrics rectangle means "no room", not "hidden"**: visibility is only
    `ViewOptions::visible` / `SetVisibility`, and a hidden view keeps the page area's metrics.
    `Metrics::is_hidden` became `is_empty`. *Why:* Servo 0.5's `WindowRenderingContext::new`
    fails for a 0 size, so a background tab with empty metrics couldn't open, and resizing a
    live WebView on every tab switch would reflow its page.
14. **Input carries what engines need without per-adapter tables**: `key` has W3C semantics
    (the press that completes a dead key is `Character("ä")`), `text` never holds control
    characters and is `None` while Control or Logo is held; `KeyCode::w3c_code()` and
    `location()` give the W3C code ("KeyA") once, for key bindings, Servo (`Code`, `Location`)
    and the Wasm engine; pointer motion, buttons and scrolling carry the seat's modifiers;
    `SetFocus(false)` makes the engine release held keys; `TextInputState` has a
    `TextPurpose` and `multiline`, and password fields get the `sensitive_data` and
    `hidden_text` hints. `HistoryChanged` carries Servo's entry list and the current index, and
    `ServiceRequest` the requesting origin as the adapter defines it. *Why:* §12 and §16; the
    old Wasm engine remembered permissions per origin (`app_origin_of`).
15. **Page-data bounds and throttles are kept from the baseline**, now in the core: text cut to
    16 KiB, URLs over 2 MiB dropped, a page-opened tab only within 5 s of user activation in
    that view, page-initiated downloads one a second per tab
    (`gpui-baseline:sighurt-core/src/{page,navigable}.rs`). *Why:* §17 and §26.
16. **Deferred to M4: `<select>` menus, colour pickers, `alert`/`confirm`/`prompt` and HTTP
    authentication.** The API has no requests for them, `native_popups` stays `false`, and until
    then Servo resolves them on drop (a `<select>` keeps its selection, `alert` is acknowledged,
    `confirm` and `prompt` are cancelled, no credentials). The old `sig-servo` didn't handle
    them either. *Why:* M4 says to extend the API only where the second real engine shows a
    missing concept; `ServiceRequested`/`ServiceCancelled` (Servo's `hide_embedder_control`)
    are the intended shape.
17. **CI on this branch never builds Servo in the main job.** `ci.yml` runs the "before a
    commit" commands from CLAUDE.md (everything but `sighurt-engine-servo` and
    `sighurt-browser`'s default features); `servo.yml` runs clippy on `sighurt-engine-servo` in
    the root workspace when the API, the host, Servo's crate or the lockfile change. *Why:*
    §24; the old jobs would have compiled Servo on every push without its build dependencies.

18. **The chrome paints any part of the window it draws, with one painter**: given a target's
    `Metrics`, `CpuPainter` paints whatever the chrome shows in that rectangle. That is the
    top chrome surface (tab strip and toolbar), a separate status strip surface at the bottom
    (`LayoutOptions::status_strip`, on by default as in the old UI; the host needs a second
    chrome subsurface for it, or the executable turns it off), and the page area while the active
    tab shows a `sig:` page or an error, which the chrome draws as the old UI did.
    `Chrome::take_damage` says which of the three changed; the page-sized buffer of an internal
    page is repainted only when it changes. *Why:* §8 allows a status strip "only if the UI
    needs it", and the old UI had one for hovered links and downloads; internal pages have no
    engine to draw them (§16).
19. **The address line is also the command line**: a line starting with `:` runs the command
    after it (`:tab.close 2`), so every command is reachable without a key binding, and
    a bad command is shown in the status strip. The executable hands every command it runs,
    however issued, to `Chrome::command_run`, which applies the old root view's focus rules
    (`location.focus` and `tab.new` focus the line with its text selected; tab switches, opens,
    back, forward and home drop the edit and give the page focus). *Why:* M3 asks for
    address/command editing; the 17 commands have no palette of their own, and the old UI moved
    focus in one place for all commands.
20. **Chrome text rules**: the address editor holds at most 32 KiB (longer addresses, such as
    big `data:` URLs, are cut as other browsers' address bars cut them: shaping megabytes per
    keystroke would stall the chrome and cost hundreds of megabytes); untrusted titles and
    status text lose control and bidi-override characters and are cut before shaping; the
    caret is steady, so the chrome never needs a timer; the chrome's font is the first of
    Inter, Cantarell, Noto Sans, DejaVu Sans, Liberation Sans, Open Sans and FreeSans that is
    installed, with cosmic-text's fallback for the rest (emoji, CJK). Tests and golden images
    use a bundled Cantarell subset (OFL 1.1, `sighurt-ui/tests/fonts/README.md`), so they don't
    depend on the machine's fonts. *Why:* §13's bounded caches and §17's contained page text;
    there is no font setting yet.
21. **`sighurt-ui` reads colors through `Colors::set(name, "#rrggbb[aa]")`**, with the old
    `[ui.colors]` names and defaults (`Colors::NAMES`), and has no TOML dependency: the
    configuration loader validates each entry with it before swapping the configuration in.
    Tabs share the strip equally between 64 and 220 px (the old UI sized them by title between
    140 and 220 and let the rest overflow); tabs that don't fit are reachable by keyboard only.
    Rectangle edges snap to the first pixel whose centre lies inside, so a control covers exactly
    the pixels whose centres hit it. *Why:* one config parser (the core's); an explicit layout
    without text measurement; hit targets that stay on drawn controls at fractional scales.
22. **The Wasm engine is a library, generic over its presentation target.** `sig-wasm` and
    `sighurt-ipc` are deleted. `WasmRuntime<T: CpuTarget>` implements `EngineRuntime` with
    `Target = T` and `FramePermit = T::Permit`; `CpuTarget` (initial metrics, the permit's
    epoch, "lend a buffer, paint, submit") is what the executable implements for
    `sighurt_wayland`'s SHM lease and permit. `present` paints the latest display list into the
    lent buffer, on the owner thread; the display list (`DrawOp`) is private to the engine. The
    executable passes the directories (`WasmConfig::kv_root`, `aot_cache`); the engine no longer
    reads `dirs::` or environment variables itself (`kv::default_root` and `default_aot_cache`
    remain for `sig` to call). *Why:* §7, §15; `sighurt-wayland` has no lease or SHM presenter
    yet, and the trait lets the tests drive the real `EngineRuntime` with an in-memory target.
23. **Wasm guests run on two fixed pools, never on the owner thread.** Loads (fetch, compile,
    `start_app`) on 2 loader threads, steps (`on_event`, `on_timer`, `on_frame`) on
    min(cores − 1, 4) guest threads; each guest has at most one job queued or running, so its
    calls stay in order and busy guests take turns. Results come back over a channel tagged
    with view and navigation or document generation, wake the owner through `EngineWaker`, and
    `pump` applies at most 64 per call. Limits: fuel as before (500M for `start_app`, 50M per
    callback) plus wall-clock limits by epoch interruption (10 s for `start_app`, 1 s per
    callback, 10 ms ticks; the ticker thread sleeps while no guest runs); a step delivers events
    and timers for at most 50 ms or 1000 callbacks and leaves the rest for the next step.
    Blocking host functions (`api_fetch`, `api_load_module`, file dialogs, `api_audio_play_url`)
    are bounded by their own timeouts and renew the guest's time limit when they return, so
    synchronous-fetch guests keep working. A hidden view steps once a second. *Why:* §15, §19,
    §25; a flood of individually bounded callbacks needs a budget of its own.
24. **Wasm navigation semantics follow the api contract**, not the old process: a failed load
    (including a trap in `start_app`) is `NavigationFailed` and the previous document stays (the
    old engine replaced it); a trap in `on_frame` is `ViewFailed` and the view can be reloaded;
    `push_state`, `replace_state` and the guest's own history moves are same-document
    `NavigationCommitted` + `HistoryChanged` with adapter-allocated ids, and are not reported
    while a cross-document load is pending, so they can't supersede it. Guest console lines go
    to `tracing` at debug level (target `sighurt::wasm::console`). *Why:* §16 and the contract
    rules of decision 12.
25. **Wasm services are unchanged for now**: the permission prompt is still drawn by the
    engine over the page (as the task for M5 asked), clipboard access is refused, downloads are
    written by the engine and file pickers are `rfd` dialogs on a worker thread. Moving them to
    `sighurt_api::services` is M6's service bridge. Screen capture is behind the
    `screen-capture` feature, off by default, because `screenshots` opens its own Wayland 0.30
    connection (§17). *Why:* keeps M5 to mounting the engine; each of these is listed as open
    in M5.
26. **Servo and Wasmtime share `sig`'s process, and `sig` owns every process-wide setting**
    (M0). `m0-coexist` ran both engines together in either order with no workaround, so the
    engines stay in-process (decision 1). `sig`'s `main`, before any engine: (1) installs the
    panic hook (default hook; return if `servo::should_panic_hook_suppress_termination()`;
    `log::error!` the panic so Servo can turn a script-thread panic into `notify_crashed`; a
    `StyleThread*` panic is an engine-level diagnostic; never exits); (2) installs rustls's
    aws-lc-rs provider; (3) with Servo compiled in and enabled, builds the Servo runtime at
    startup (with `hard_fail: false`) and calls its `setup_logging()`, the process's only `log`
    logger (without Servo, `env_logger`); a `tracing` subscriber, if any, must not install
    `LogTracer`. The runtime holds the profile lock from before it builds Servo until Servo has
    dropped; a second `sig` runs Servo without a profile. Nothing installs trap-signal handlers
    that don't chain, calls `Engine::unload_process_handlers`, declares a global allocator
    (Servo's jemalloc is it) or changes the environment after threads start. No engine code
    calls `process::exit` or `_exit`; backend loss is `RuntimeFailed`, and since Servo
    initializes once per process, web pages then need a restart of `sig`. *Why:* §14 "Required
    library conversion", §18, §20; evidence and sources in `docs/rewrite/M0.md`.
27. **The core is one function, and everything that changes browser state goes through it.**
    `sighurt_core::update(&mut BrowserState, BrowserEvent) -> Vec<Effect>`; events that need
    the time carry it (`at: Instant`), the core reads no clock. `sig` hands it every event of
    every engine pump (it drops those for views it closed or replaced, from the wrong engine,
    and requests of replaced documents; `FrameReady` is `sig`'s to schedule from), every
    keyboard event (the core matches bindings in the focused part's context and routes the rest
    to the shown view, or to the chrome as `Effect::ChromeInput`) and every pointer event on a
    page target (user activation; the mouse's back and forward buttons stay browser commands).
    The core owns whether the page or the address bar has the keyboard (`Effect::Focus` tells
    the chrome, `BrowserEvent::Focus` tells the core when the chrome moves it; the rules are the
    ones `Chrome::command_run` applies), and after every update reconciles which view is
    visible, focused and at the tab's zoom, and the page's cursor and text field for the host
    (`Effect::PageCursor`, `Effect::PageTextInput`). `Effect::SaveSession` carries no data:
    `sig` saves `BrowserState::session()` when it likes, and at once before `Effect::Quit`.
    *Why:* §6, §12's ownership sequence, §19; one place rejects late events.
28. **Configuration format version 1** (`sighurt-core/src/default-config.toml`): `version = 1`
    is required, so a file in the old format gets a message saying so; `home`, `search`;
    `[engines] default` and `enabled` (names only: engines are compiled in, so no `command`);
    `[routing.<engine>] schemes, extensions, origins`, merged over the defaults field by field,
    each scheme and extension belonging to one engine; key bindings at the top level again
    (`[keys]`, `[keys.page]`, `[keys.location]`; the baseline had `[ui.keys]`); `[ui]` is the
    chrome's, the core checks only the `#rrggbb[aa]` format of `[ui.colors]`, and `sig` checks
    the names with `Colors::set` (decision 21) before it sends `ConfigReloaded(Ok)`. Parsing
    reports every problem, and any problem rejects the file. There is no MIME probe: http(s)
    goes to the default engine unless an engine lists both the URL's origin (`origins`) and its
    extension, which is how a Wasm app now opens from a web server; `file:` URLs still route by
    extension. Keystrokes are `mod-mod-key` (ctrl, alt, shift, super, altgr; `secondary` for
    ctrl as before) with a character (matched as the layout types it; shift belongs to the
    character, except for letters), a key name or W3C key name, or `code:<W3C code>` for a
    physical key. Key sequences ("g g") are rejected with a clear error: replaying the held
    keys needs timers. The binding "mode" is typing: while the address bar or a page's text
    field has focus, a binding that would type a character (no modifier but shift) lets it type.
    *Why:* §12, §16, §21; the old format's process fields mean nothing now.
29. **Cross-engine history** (§16): a tab's history is a list of segments, and an internal
    page is a segment of its own. Inside a segment the engine's `HistoryChanged` is the truth and
    `Traverse` deltas move through it; a segment left by a new navigation keeps a limit, so its
    engine's later entries are no longer reachable forward. A tab keeps at most 2 live views
    besides the shown one (the farthest become records), 50 segments, and 50 entries or 4 MiB of
    URLs per segment. Opening a record reloads its current URL in a new view
    (`TabSummary::reloads` says so until then) and splits its other entries into records around
    it. A new view opens hidden and replaces the shown one once it is `ViewReady` and has
    committed. A commit the page started counts as a new navigation (the api carries no
    traversal flag), so a page's own `history.back()` in a segment followed by others discards
    them; an api flag would fix it if M4 or M5 need it. *Why:* §16, §20, §25.
30. **What pages may ask for** (§17): only the document a tab shows may ask; a retained or
    still-loading view gets `Denied`, a replaced document `Cancelled`. The clipboard, file
    pickers and external URLs need a click or key press in the page within 5 s; reading the
    clipboard, picking files, external URLs and new tabs use it up, so one click opens one tab.
    External URLs only for schemes the browser neither opens nor runs. Downloads once a second
    per tab. Permissions prompt (`Effect::Prompt`); an answer holds for that document only, and
    for its other pending requests of the same permission; a refusal is also remembered by
    origin for the tab. At most 16 pending requests per view; everything else is refused. A page
    may close only a tab a page opened that has shown nothing else. *Why:* §17, decision 15, the
    old core's `take_activation`.
31. **A GL page view is hidden by destroying its `wl_subsurface`, and shown with a new one and a
    frame presented at once** (M1). The host never attaches a null buffer to a GL page surface:
    its buffer state is the driver's (§9). Destroying the role object unmaps the surface at once,
    and `wl_subcompositor.get_subsurface` on the same surface later is allowed, since the surface
    keeps its subsurface role. On show the adapter presents immediately rather than waiting for
    the frame callback that was in flight at the hide, which may never come. *Why:* §9, §10;
    measured in M1.md, "Hide and show".
32. **GL pages present with swap interval 0, paced by the host's frame callbacks** (M1). The
    adapter sets `eglSwapInterval(0)` (surfman has no API for it), so `eglSwapBuffers` never
    blocks on a callback the compositor may withhold from a hidden or invisible surface. Before
    each swap the host requests `wl_surface.frame` on the page surface (the swap's commit carries
    it) and keeps one page frame in flight; Servo's `RefreshDriver` is driven by those callbacks,
    starting a frame at once when none is in flight. *Why:* §10 and §19: no fixed tick, no wait
    forever; M1 measured the page at the output's 120 Hz on River and no idle wakeups.
33. **A GL page is torn down Servo first, then the rendering context, then its EGL display, then
    the Wayland objects** (M1). Dropping Servo releases its clones of the context; the context
    (EGL window surface, `wl_egl_window`, GL context) must then be the last reference, or the
    page surface is leaked rather than destroyed under the driver; `eglTerminate` follows,
    because surfman never terminates a display it didn't open, and a display still initialized
    at exit is terminated by the driver's exit handler (Mesa's `_eglFiniDisplay`), after the
    connection it uses is gone. *Why:* §9, §20; M1.md, "Teardown".
34. **The Wayland host's API is pull-based and takes complete layouts.** The executable asks
    `Host::permits()` each iteration for permits of targets that are shown, invalidated, not
    waiting for a frame callback and (SHM) have a free buffer; a placement lists every live
    target of a window, bottom to top, for the size of the latest `WindowConfigured` (only the
    latest per window stays queued), and a stale or incomplete one is refused whole; the
    iteration ends with `Host::flush`, which commits finished layout transactions. *Why:* one
    path for the chrome, the demo engine and Servo, no callbacks into the application, and no
    half-applied layouts.
35. **Layout transactions use synchronized subsurfaces and a 150 ms grace period**
    (`sighurt_wayland::RESIZE_GRACE`). Targets a placement resizes or maps switch to synchronized
    commits; the root commits once each has submitted a frame for its new epoch, or when the
    grace ends, unmapping those without one until their next frame; then the leaves run
    desynchronized. A GL target gets its (synchronized) role before its first permit, so its first
    swap waits for the root commit. Hiding and showing follow decision 31. *Why:* §10's resize
    sequence and slow-page fallback; M1 measured whole transactions at 1.4–19 ms.
36. **One scale per window**: the root's preferred scale (fractional with a viewporter, else
    the integer one) applies to every target of the window. With a viewporter every surface keeps
    buffer scale 1 and sets its viewport destination to its logical size. *Why:* the chrome and
    the page must never render at different scales.
37. **The host translates keys with xkbcommon itself**, not through smithay-client-toolkit's
    keyboard, which reports no consumed modifiers or AltGr and applies xkb's Control
    transformation (Ctrl+A would type "\x01"). Dead keys and Compose use the locale's compose
    table, or en_US.UTF-8's when the locale has none; a key that breaks a composition is reported
    but types nothing, as xkbcommon recommends; composition steps don't auto-repeat; a compositor
    that repeats keys itself (`wl_keyboard` 10) turns client-side repeat off. smithay-client-toolkit
    is used for the registry, seats, pointer frames, cursors, xdg-shell, the SHM pool and the data
    devices. *Why:* §12 and decision 14.
38. **A frame callback withheld from an invisible window is waited for; one for a target the
    host unmapped never is.** Unmapping forgets the outstanding callback and bootstraps the next
    frame; while the compositor withholds callbacks (another workspace), nothing presents, and
    nothing needs to. *Why:* §10; a timeout would only draw what nobody sees. GL presenters must
    not throttle on their own (decision 32).
39. **SHM buffers: one pool per buffer, ARGB8888 premultiplied little-endian (B, G, R, A bytes),
    at most 3 per size and 2 sizes retiring**; a new size beyond that waits for a release, which
    always comes because every retiring buffer but the newest has been superseded. *Why:* §10;
    a pool per buffer frees each buffer's memory as soon as it is released.
40. **The core alone decides where the keyboard focus goes after a command; the chrome follows,
    and shows the core's tab titles.** `Chrome::command_run` (decision 19) is gone: the
    executable hands each `Effect::Focus` to `Chrome::set_focus` (`Location` → `Focus::Chrome`
    focuses the line with its text selected; `Page` → `Focus::Page` leaves it and drops an edit),
    and forwards the moves the chrome makes itself (`UiAction::Focus`: a click in the line or on a
    page the chrome draws, Escape, a `:command`) as `BrowserEvent::Focus`. `TabSnapshot::title`
    is `TabSummary::title`; the chrome only contains it and names its own pages "Home" and
    "Can't open page". After Enter the line keeps what was typed until the tab commits another
    address, stops loading or is switched away from, rather than showing the core's `requested`
    URL, since a navigation a page starts could put an address it never commits over its own
    content. Clipboard reads carry a `RequestId` the chrome makes (`UiAction::Paste(id)`,
    `Chrome::paste(id, text)`), and only the latest, while the line keeps focus and tab, is
    pasted. An engine page clicked while the line has an edit drops it (the core's
    `Effect::Focus` contract); the old UI kept it. *Why:* §6 and §28's first two questions: one
    owner for each rule, and a late answer can tell which request it belongs to.
41. **Review fixes to the core (amends 28 and 29).** A commit the page started itself discards
    the tab's later segments only if it brings a new document: same-document commits (a
    fragment, `pushState`, `replaceState`, the page's own traversal within the document) never
    do, so `pushState` leaves them reachable. The core remembers its own requests that a newer
    one superseded (the last 8 per view), since an engine carries out every traversal it is
    sent, so their commits count as the traversals or reloads they were. What is left: a page's
    own traversal to another document, or its own reload, discards later segments. The fix is
    for `NavigationCommitted` to say what the commit did to the engine's history (a new entry,
    a replacement, a traversal); it waits for the Servo adapter, since M4 extends the API only
    where the second engine shows a missing concept, and Servo is the first with such page
    commits (the Wasm engine's are all same-document) and needs it for its commit attribution
    too. `Environment::engines` maps each engine to its `EngineCapabilities`: an engine without
    history traversal is never sent `Traverse` and gets a view per page, so back reaches each
    page as a segment; one without page zoom is never sent `SetZoom`, and
    `TabSummary::can_zoom` tells the chrome (the tab keeps its zoom). A switch view whose only
    navigation is cancelled is closed; a tab showing a record whose loading stops (`page.stop`,
    or the engine) says "Loading stopped." until reloaded instead of loading again by itself,
    and back and forward leave such a record. Only a fresh key press starts a binding, so a key
    held before Ctrl went down keeps repeating into the page and its release reaches it.
    `[ui]` holds only `colors`: the old `[ui] home`, `search` and `[ui.keys]` are errors saying
    where they went; the version is checked first, and each top-level part of the file is read
    on its own, so one misspelt key doesn't hide the other parts' problems. A damaged session
    in `Start` is refused with a notice; `OpenFailed` with `EngineError::Runtime` fails the
    whole runtime, like `RuntimeFailed`; a page may ask to open a `file:` URL only from a
    `file:` page. *Why:* the review of the core track against §7, §12, §16, §17, §21 and §25.
42. **Wasm guest limits and scheduling (amends 23).** A call's time limit counts only the time
    the guest runs: host functions that wait say so (`Budget::wait`), for the network and other
    modules against `SandboxPolicy::max_wait` (30 s a call), for the user (file dialogs) not at
    all. The limits are checked by the store's epoch callback on every 10 ms tick, which also
    stops the call as soon as its document is closed, replaced or stopped, or its worker
    terminated (a worker's and a loaded module's budgets are children of their page's). A
    module's `start` function runs with the budget of `start_app`; no guest code runs outside
    a budgeted call. A live guest keeps its own cadence: its step job queues the next step on
    the guest pool (a pool of timed jobs), and the owner hears of a step only when it changed
    something the browser shows (a frame, the history, the prompt, a navigation request, a
    failure). What a closed or replaced document leaves is dropped by a thread of its own, and
    the subsystems' Tokio runtimes don't wait for their tasks when dropped. Worker threads stay
    threads of their own, at most 16 alive per guest and 32 per runtime, terminated ones
    counted until they end. *Why:* the review of the Wasm track: renewing the time limit after
    every blocking call let a guest loop on them forever, start functions and stopped loads
    held loader threads, dropping a document could wait on DNS on the owner thread, and every
    visible guest woke the owner twice a frame; §15, §19, §20 and §26.
43. **What the Wasm engine keeps and paints is bounded as a whole** (amends 22). The rasterizer
    skips ops and glyphs outside the clip, charges text for its shaping (150 pixels of work a
    byte, measured against fills), paints no text over 2048 buffer pixels, gives the overlay (the
    permission prompt) a budget of its own, and does at most 16 pages of work a frame (the
    examples use 1.1–5.7). Images are converted for painting where the list is made, on the
    worker. The prompt answers only once a frame showed all of it. Guest measurement and
    painting use separate font systems, so a guest measuring can't hold up the owner. A view's
    session history spans its documents (back and forward load earlier and later documents
    again, restoring their entries) and holds at most 1000 entries and 4 MiB; `push_state` may
    not show another origin's URL. *Why:* the review of the Wasm track found the prompt could
    be hidden and still answered, a guest's text could stall painting for seconds, history
    copies could take half the owner's time, and back across documents was lost; §15, §16,
    §17 and §25.
44. **Wayland host review fixes (amends 35 and 38).** A target's first frame for a new layout
    epoch is bootstrapped, like the first after mapping: its frame for the previous epoch may sit
    in the compositor's cache of a synchronized subsurface, whose callback comes only after the
    root commit that waits for the new frame, so a second placement during an open transaction
    stalled until the grace period ended. A frame for the current epoch that arrives after the
    grace period maps its target at the next `Host::flush`, in a transaction that commits at
    once, so the slow-resize fallback shows the background only until that frame, as §10 says;
    before, the target stayed unmapped until another frame. Targets joining an open transaction
    don't extend its grace period, so a window resized without pause still commits every 150 ms.
    `ShmFrame` owns its buffer while it is lent, so painting holds no borrow of the host's state.
    text-input-v3 is enabled again when the field changes within one target (another purpose or
    multi-line flag, or its surrounding text is gone), as the protocol requires; two fields that
    look alike need `None` between them until `TextInputState` has a field identity, an api
    change left for M4 (Servo's input-method controls have ids). *Why:* the review of the host
    track against §10 and §12; each of the three behavior fixes, broken on purpose, failed the
    headless test written for it.
