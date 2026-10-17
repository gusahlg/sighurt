# Sighurt rewrite: status

The specification is [`sighurt-rewrite-plan.md`](../../sighurt-rewrite-plan.md); section numbers
below refer to it. The pre-rewrite browser is at git tag `gpui-baseline`.

Tick a pass criterion only with evidence, and link or name it: a test, a measurement, a run on
a named machine and compositor. A criterion that turned out not to hold goes in the decisions
log with what was decided instead.

## Where things stand

`sig` (`sighurt-browser`) is the rewrite's browser, on Wayland, with its engines compiled in and
no IPC: Servo 0.5 (the default engine for the web, presenting GL straight into its page
subsurface), WPE WebKit 2.52.6 (for the sites the configuration gives it, presenting its
DMA-BUFs) and the Wasm engine (Oxide's app ABI, drawn on the CPU), plus a demo engine for tests.
The chrome is drawn on the CPU; the core is plain Rust with no I/O. Keymaps and chromes are small
traits (extension-api.md), with the defaults built on them and one proof each: the modal keymap
and the line chrome. The old `sighurt-ipc`, `sig-wasm` and GPUI chrome are gone (at tag `gpui-baseline`).

- **Milestones.** M0, M1, M2, M3 and M5 pass. M4 passes with Stop's one Servo 0.5 limit accepted
  (decision 80). M6 is open (its daily workflows are for the user to name). M7 has its comparison
  (decision 79): the readback criterion holds; memory growth and the remaining regressions await
  decisions, and River and a second compositor family are still to be measured.
- **WPE WebKit** is a default engine of `sig` (decision 78); **the configuration and the extension
  API** are built to the design's 18 steps (decisions 67, 68 and 74–77).
- **Verified last** (the section "Final verification and documentation", at the end of the
  milestones): every end-to-end suite, unit tests and clippy on every feature set, a configuration
  showcase, and a short live run of `sig` with all three engines on River. README, DOCS, SECURITY,
  CONTRIBUTING, ROADMAP, CLAUDE.md and CI describe this state.
- **Then the wave's three reviews were applied** (section "Review fixes", decisions 82–86): links
  move a tab only when the user pressed them, a Wasm app's downloads go through the browser,
  WebKit is opened on first use and its processes end with the last WPE page, retained views are
  bounded by cost, and `shell.nix` pins the Rust toolchain; every suite was run again.

## Milestones (§23)

### M0 — Record the baseline and prove engine coexistence

Results, versions, environment and every decision: [`M0.md`](M0.md). The experiment is
`experiments/m0-coexist`; its evidence is a run on `louise-pc` on 27 September 2026, after
the independent review (`tools/remote tools/cargo run -p m0-coexist`: every check passed in
both initialization orders and both processes exited with 0; clippy `-D warnings` and rustfmt
clean). The inventory is [`compat-inventory.md`](compat-inventory.md).

Build/do:

- [x] Baseline measured: build the old browser (`sig`, `sig-servo`, `sig-wasm`) from a
  `git worktree` of `gpui-baseline`, run it on a named machine, compositor and small fixed
  workload, and record timing and memory. M7 repeats the same workloads, so write them down.
  [`bench/results/BASELINE.md`](../../bench/results/BASELINE.md):
  `gpui-baseline` revision 359cb36 in release mode (built from a `git archive` of the tag,
  not a worktree), on `louise-pc` (RTX 4060, driver 595.80) in headless sway 1.12 (GLES2,
  1280 × 800, scale 1), 16 workloads, 5 measured rounds after a warm-up, 96 runs, none failed,
  no crash; the only anomalies are the 6 `wasm-flood` runs, where the flooded browser did not
  handle ctrl+q within 10 s: startup, CPU, wakeups, PSS, frame rates, read-back and IPC
  bandwidth, input latency, and the build cost (16 min of cargo time). Set:
  `bench/results/louise/baseline/20260927-083430/`. It replaces the first set
  (`20260926-192032`, commit 58397a7), which lost keys to `wtype` (ctrl+q in 7 of 84 runs)
  and counted an unflagged GPU crash (BASELINE.md, "The first set, superseded"; decision 49).
  The workloads M7 repeats are the 16 of `bench/workloads.toml`.
- [ ] Required user-visible behavior of the baseline recorded (what the daily workflows are).
  The behavior is recorded (compat-inventory.md §1–§6); which of it the user's daily workflows
  rely on is for the user to name, and M6's pass criterion needs that list.
- [x] Wayland and driver environment recorded: River 0.4.8 with gharial 0.3.0, NVIDIA RTX 3070
  on the proprietary driver 595.99.02, Mesa 26.2.3, on `sighurt`; and the build machine
  `louise-pc` (M0.md, "Environment"). Dependency versions: M0.md, "Versions" (decisions 3–5).
- [x] Servo and Wasmtime compiled and linked into one executable: `m0-coexist`, which links
  the Wasm engine itself (`sighurt-engine-wasm`, so every native library it brings: FFmpeg,
  rodio/cpal/ALSA, Tokio, reqwest with rustls, OpenSSL through tokio-tungstenite, WebRTC,
  nokhwa/v4l, gilrs/libudev, rfd, image), from the root lockfile with no version change, and
  runs the engine's own runtime (step 3).
- [x] Both engines initialized in that program: JavaScript and Wasm executed, a guest trap,
  several views and stores, clean shutdown (M0.md, "Results", steps 3–11).
- [x] Process-global setup reviewed, each item decided and owned by `sig` (M0.md,
  "Process-global setup"; decision 26):
  - [x] Signal handlers: both orders ran with the other engine's handler on top; guest traps in
    Wasmtime and JavaScript Wasm traps in SpiderMonkey, at the same time on different threads,
    all reached the right runtime (steps 6–7).
  - [x] TLS provider: without one, `ClientConfig::builder()` panics in this graph (step 1);
    `sig` installs aws-lc-rs first, and reqwest follows it (step 2).
  - [x] Panic hook: specified, and the experiment's hook counts style-thread panics and leaves
    Servo working (step 8). A script-thread panic reaches Servo through the hook's
    `log::error!`: the panicking view gets `notify_crashed` and a view on another script
    thread goes on (step 9); that every view on the panicking thread fails is read from
    Servo's source (M0.md, open problem 9; decision 48). It never exits; Servo runs with
    `hard_fail: false`.
  - [x] Logging: the one `log` logger is Servo's `setup_logging()` (the only way script-thread
    panics reach Servo), called by `sig` on a Servo runtime built at startup; without Servo,
    `sig` installs `env_logger`.
  - [x] Profile path: `{data_dir}/sighurt/servo`, locked by the one runtime for its whole
    life; a second `sig` runs Servo without a profile (decided, not built yet). Servo writes
    the profile while dropping (step 11).
  - [x] Environment mutation and every exit path: only surfman mutates the environment
    (`LIBGL_ALWAYS_SOFTWARE`, `DRI_PRIME`; step 4 shows it); none of the old `process::exit`
    and `_exit` paths comes along (M0.md, "Exit paths").
  - [x] OpenSSL, which only the Wasm engine brings in: a guest's `wss://` connection
    initializes it with no `atexit` cleanup and no change to signal dispositions or the
    environment (step 3; M0.md, "OpenSSL").
- [x] Compatibility inventory recorded (compat-inventory.md):
  - [x] Commands: the 17 names in `sighurt_api::commands::Command::NAMES`, with their baseline
    behavior (§2).
  - [x] CLI: `-h`/`--help`, `-V`/`--version`, `--default-config` (printed the built-in
    configuration; not in the new `sig` yet), URLs and paths as arguments (§1).
  - [x] Configuration: `default_engine`, `[engines.<name>]` (`command`, `schemes`,
    `extensions`, `mime`), `[ui]` `home` and `search`, key bindings, `[ui.colors]` (§3–§4). At
    the baseline the bindings live in **`[ui.keys]`**, `[ui.keys.page]` and `[ui.keys.location]`;
    a top-level `[keys]` is rejected there (`gpui-baseline:sighurt-core/src/config.rs`, test
    `typos_are_errors`), although the old CLAUDE.md says `[keys]`. The inventory's decisions
    follow decision 28: key sequences and lenient loading are changes, not kept.
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
the limitations. Tests: `cargo test -p m1-mixed` (8 unit tests: frame pacing, the refresh
driver, key text, the report and its bounded storage); clippy with `-D warnings` is clean (both
on `louise-pc` after the review fixes). `headless-checks.sh` now checks its own log (17 PASS/FAIL
checks). The independent review's fixes are decision 50; after them R1–R4 (`louise-pc`,
RTX 4060 and llvmpipe, fractional and integer) passed 17 of 17 checks each. Only the N1–N4 and
R1–R7 logs still exist ([`m1-runs/`](m1-runs/)); the L, G, H, A and C logs were on tmpfs and
were lost in a reboot, so their numbers survive only as transcribed in M1.md.

- [x] A real HTML page and an animated/WebGL test present through the GPU path. *L1/L2: GL
  "NVIDIA GeForce RTX 3070/PCIe/SSE2" on River; `animated.html` (CSS animation + WebGL 1) at
  the 120 Hz output's pace, 2 063 frames in 20 s.*
- [x] There is no ordinary page `read_pixels`/CPU image transport path in use. *Every run that
  reached its report: `read_to_image` 0, and with the audit on, 0 `glReadPixels`/`glGetTexImage`
  among WebRender's GL calls.*
- [x] Repeated resize, scale changes, hide/show, and teardown behave correctly. *G1/G2, N1/N2
  (NVIDIA) and H1/H2 (llvmpipe): 100–200 % fractional and integer scales, a 40-size storm, 6
  hide/show cycles; L1: 5 cycles with a frame callback outstanding at each hide; every run that
  closed normally: no protocol error, context destroyed before its surface, exit 0. R1–R4 add an odd-size page
  taken to scale 2 and back and a scale change while hidden; R7 (compositor killed) shows the
  ordered release on the error path. Caveat: a scale change can make Servo lay out one
  intermediate new-size/old-ratio state (M1.md), which M4 has to handle.*
- [x] Keyboard focus routes through the window to page/chrome correctly. *G1, N1, H1: keyboard enter
  only on the root; keys go to the page or the address field by logical focus; clicks, Ctrl+L
  and Escape move it; Servo reports focus and blur. R1–R4 check it by script, including a key
  held across a focus move reaching the page as its own key-up.*
- [x] Page animation does not cause repeated CPU chrome painting. *L1: 2 chrome paints during
  2 063 page frames; G3, N3, A1 and R5a/b: 2 during about 1 000.*
- [x] A static chrome-only experiment sleeps when there is no work. *C1: 0 loop wakeups in 10 s;
  a static Servo page too (L2, H1).*
- [x] Diagnostics identify the actual GL renderer and fallback mode. *GL and EGL strings and the
  presentation path in every log; llvmpipe runs say "SOFTWARE FALLBACK … not the GPU path".*
- [x] The result runs on the actual River/gharial system and GPU driver intended for use. *L1,
  L2: River 0.4.8, gharial 0.3.0, RTX 3070, driver 595.99.02; no screenshot (grim's toplevel
  capture fails on River, see M1.md). The L1/L2 logs were lost (tmpfs); the independent review
  read them before that. Repeating them needs the user's permission for a live run.*

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

- [x] Usable address/command editing.
- [x] Reliable tab selection with dummy views.
- [x] Bounded buffer lifetime.
- [x] No rendering while idle.
- [x] Correct pointer/keyboard coordinates.
- [x] Normal close and compositor disconnect handled deliberately.

Each is ticked on the chrome's and the host's tests below and on `sig` with the demo engine in a
private headless sway ("Integration" at the end of this section); `sig` has run on River since,
without input (M4's L1, and R2 in "Final verification and documentation").
`sig` with its default features (Servo and Wasm) was built and run on this desktop on 2 October
2026 (the end of this section).

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
- [x] Shown in a real window on River. *R2 ("Final verification and documentation"): `sig` on River
  0.4.8 with gharial, three runs; the compositor presented the standard chrome's surfaces
  (`wp_presentation` feedback: the toolbar target 7, 6 and 4 frames, the status strip 1, in the
  three runs) at 1542 × 84 and 1542 × 20. No screenshot: nothing captures the user's desktop.*

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
- [x] The M3 criteria at the top: `sig` wires host, chrome and demo engine together (Integration, below).
- [x] Run on River/gharial: `sig` in M4's L1 and in R2 ("Final verification and documentation"):
  the window and its subsurfaces, every protocol of the host's list found, the three engines
  presenting, idle asleep, a clean exit on SIGTERM. Without input, a resize or a scale change
  there, since nothing may type into the user's desktop. Also untested: the integer-scale
  fallback without a viewporter, a second compositor family, cursors (shape protocol and theme)
  by eye, the primary selection. Drag-and-drop is not implemented.

Measured with `sighurt-wayland/examples/host-demo` in headless sway: `wp_presentation` reported
frames on screen 4–15 ms after submission; at 150 % the 1280×800 output gives the chrome a
1280×60 buffer for 853×40 logical pixels, and the page a checkerboard whose 16-logical-pixel
squares are 24 output pixels (grim screenshots, looked at).

Integration (`sig` = `sighurt-browser`, with `sighurt-engine-demo`). `sig` runs the owner loop
of §19 on calloop (`app.rs`): the host's Wayland source, one ping shared by engine wakers and
service threads, the signal pipe and an inotify watch on the configuration; pump deadlines and
the session's save delay are the loop's timeout, not timers. An iteration takes at most 256 host
events a pass (consecutive pointer motion over one target coalesced), hands them to the core,
carries out its effects (`effects.rs`), pumps the runtimes that were called, woke or reached a
deadline, rebuilds the chrome's snapshot only when the core said so, applies a placement only
when it changed or answers a configure, presents what the host has permits for, repeats at most
8 passes, flushes and sleeps. `registry.rs` holds the compiled engines (features `engine-demo`,
`engine-wasm`, `engine-servo`, each runtime started on first use; Servo at launch when enabled,
decision 26); `services.rs` the clipboard through the host, downloads (the baseline's
`download.rs` on threads, no file left behind), `xdg-open` for external URLs, and
`Unsupported` for file pickers and permissions; `persistence.rs` the configuration (checked with
`Colors::set`, hot reload keeping the old one on error) and the session (written atomically on a
thread, 1 s after the last change and at quit); `diagnostics.rs` the §26 counters. `sig --help`,
`--version` (with the Oxide attribution), `--default-config`, `--diagnostics FILE` (JSON at exit
and on SIGUSR1), URLs and paths as arguments.

The demo engine (`sighurt-engine-demo`) implements `EngineRuntime<Target = TargetLease,
FramePermit = FramePermit>` over SHM with the pages of §23: `demo://pattern` (static),
`demo://animation` (paced only by frame callbacks), `demo://slow` (its frame for each new layout
is 400 ms late, past the 150 ms grace), `demo://resize` (checkerboard, one-pixel border and
crosshairs at the pointer, with the page-local and buffer position as status text),
`demo://error` (the navigation fails and the page stays) and `demo://crash` (a click fails the
view; reload recovers it). Every page shows whether it has the keyboard and what was typed into it.

End to end, `sighurt-browser/tests/headless.sh` builds `sig --no-default-features --features
engine-demo` and runs four scenarios in `tools/headless-sway` (pixman, 1280×800), with a
virtual pointer and keyboard that stay plugged in (`examples/virtual-pointer.rs`, `wtype -s`).
Its checks assert on `sig`'s diagnostics JSON (the active tab, which view target is shown and
whose tab it is, what the status strip shows: "notice", "page" or "error", never the text),
on the session file, on `sig`'s log and on screenshot pixels. After the independent review
(decision 47), on this machine on 27 September 2026, all 44 checks passed
(scratchpad run `sigfix/final`), and the screenshots `main/12`, `main/14` and `config/02` were
looked at. After the second pass over that review (decision 56), all 44 passed again on 2 October
2026 (scratchpad runs `app/e2e` and `app/e2e2`; `main/12` and `config/02` looked at):

- Address/command editing: `ctrl-l`, "åäö", Escape, then `ctrl-l`, "demo://slow?åäö" and Enter
  opens it in the third tab (asserted: the tab and its shown view; the session holds
  `demo://slow?%C3%A5%C3%A4%C3%B6`); "hej åäö" typed into the page reaches it (asserted: the
  session's title "Demo: slow frame · hej åäö"). An unbound `alt-9` types nothing into the page
  (`config`). That the line shows "åäö" before Escape is screenshot-only (`06`).
- Tab selection: three demo views, tab 1 shown; `alt-2`, a click on the third tab, `ctrl-tab`
  and `alt-3` each make that tab active with its view the one shown (asserted after each).
- Buffers: during a storm of 20 output sizes, sampled by `sig` after every presentation (where
  buffers are allocated), at most 2 buffers and 1 retiring generation per target (host bound: 3
  of the current size, 2 retiring generations); 1.5 s later at most 2 and 0 retiring.
- Idle: 3 s with the pattern shown: 1 loop wakeup and 1 iteration (both the SIGUSR1 that asked
  for the report; asserted ≤ 1 and ≤ 2), 0 page frames, 0 chrome paints. The animation shown:
  124 page frames in 2 s (the headless output's rate) and 0 chrome paints; hidden behind a
  static tab, 0 frames. Page readbacks come from the engines' own counts (decision 47); the demo
  engine paints into the host's buffers and has no readback path, so for it they are 0 by
  construction, not a measurement of the Servo path.
- Coordinates: at 100 % the pointer at window (300, 300) puts the resize page's crosshairs on
  output pixel (300, 300), and at 150 % layout (400, 300) puts them on output pixel (600, 450)
  (asserted on screenshot pixels, ±1; `12` shows page (400, 216), buffer pixel (600.2, 324.3));
  every mapped buffer is its logical size × 1.5, rounded (853 → 1280). The crosshairs are drawn
  where `Metrics::to_buffer` puts the pointer, the conversion the status text reports (since
  decision 56; before, the page had its own copy of the formula). 125 % and 200 % are
  covered only by the chrome's tests. Resizing to 1000×700 unmaps the slow page for its 400 ms
  (`09`: the root's background) and maps its late frame (`10`), screenshot-only.
- Coalescing: with `sig` stopped (SIGSTOP), 30 pointer moves over the page; after SIGCONT 28
  of them were dropped for a newer one (asserted ≥ 20). Unit tests: same-target runs collapse
  to the latest, a button or key in between keeps its place, other targets and seats are not
  merged (`app::tests`).
- Recoverable errors: `demo://error` keeps the animation shown and the status strip shows the
  error; `demo://crash` fails on a click, stays shown with the error in the strip, and F5
  recovers it (all asserted).
- Notices: a fresh notice of `sig`'s own comes before the page's status text for 5 s: with the
  page's status set by typing, an invalid configuration puts "notice" in the strip, which stays
  while the page sets its status again, and the page's text returns 5 s later (asserted; broken
  on purpose, the two notice checks failed).
- Diagnostics: the renderer line (`renderer engine=demo mode=cpu`) is printed at the engine's
  first frame, and only the first tab's view prints `first-frame` at startup (the two opened in
  the background don't).
- Close: `ctrl-q`, SIGTERM and the compositor's close request (`swaymsg kill`) exit with 0 and save
  the session, which the next start restores; killing the compositor exits with 1, logs "the
  Wayland connection is gone: …", and still saves the session. Shutdown follows §20:
  the session is written, views closed, runtimes shut down, then the chrome's leases, the window
  and the host are released.
- Configuration: a new binding and a tab color apply on save; an unknown key or an unknown color
  name keeps the configuration in use (asserted by the bindings still working).

Unit tests: `tools/cargo test -p sighurt-browser -p sighurt-engine-demo --no-default-features
--features engine-demo` (25: arguments, downloads, a download whose server stalls deleted at quit
(`quitting_deletes_a_download_its_server_keeps_waiting`, which fails with the deletion removed),
atomic session writes, the configuration watch, colors, diagnostics lines, readback and buffer
accounting, motion coalescing, the status strip's order, demo URLs, the demo engine's event
coalescing); the same 22 of `sig` with `engine-demo,engine-wasm` and with its default features.
Checked on 2 October 2026: clippy `-D warnings` on all targets with no engine, `engine-demo`,
`engine-wasm`, `engine-demo,engine-wasm`, the default features (Servo and Wasm), `engine-servo`
alone and all three; `cargo fmt --check`.

Servo and Wasm in `sig` (decision 56), on this desktop (RTX 3070, driver 595.99.02) on 2 October
2026, with the working tree of that moment (the Servo track's changes to `sighurt-engine-servo`
and the api in progress): `tools/cargo build -p sighurt-browser` (its default features), clippy
as above, and `tools/cargo test -p sighurt-browser` (22 passed). In `tools/headless-sway
--renderer gles2` (sway 1.12), `sig` opened a local HTML file and https://example.com/ in Servo
and gradient-demo in the Wasm engine, one tab each, then switched through them with configured
keys: the renderer lines say `engine=servo mode=hardware gl="NVIDIA GeForce RTX 3070/PCIe/SSE2"`
and `engine=wasm mode=cpu`; 76 page frames, every frame line `readback_bytes=0` and
`page_readbacks` 0 (Servo's `read_to_image`; the GL audit was off); `first-frame` only for the
first tab (122 ms); Servo's profile written under the run's `XDG_DATA_HOME`; ctrl-q exits with 0
and the session holds the three tabs (scratchpad run `app/servo2`; an earlier run of commit
3c7f5fd with this pass's changes, `app/servo1`, gave the same). Screenshots of the three tabs
looked at. Not run: the `_exit` after a Servo that could not shut down (no page can crash
Servo's script thread in a normal build; the adapter's `crash.rs` shows the leak itself).

Not done: headless sway's gles2 renderer for the demo engine's suite (`engines.sh` uses it). Open from the
review, each needing another track: the Wasm and Servo crates exporting their capabilities as
the demo engine does (`CAPABILITIES`), so the registry's copies go; the shared third-party
versions (reqwest, rustls, log, libc, dirs, serde_json, tempfile) in the root
`[workspace.dependencies]`; one virtual-pointer helper for the headless scripts instead of three
copies; a host setter for the root's background, so a reloaded `[ui.colors] bg` reaches it.

### M4 — Convert and mount Servo

The adapter is `sighurt-engine-servo` (decisions 51–55 and 57): `ServoRuntime` implements
`EngineRuntime` with the host's `TargetLease` and `FramePermit`, one Servo per process and one
WebView per view, each view on its own `WindowRenderingContext` made from its lease. `sig`'s
registry starts it (decisions 26 and 56). `examples/servo-window.rs` puts Servo views in a window with
`sighurt-wayland` through the owner loop the tests use (`tests/common/mod.rs`, which also does
`sig`'s process setup: the TLS provider, the panic hook, Servo's logger). Evidence, all on
`louise-pc` (RTX 4060, driver 595.80) in headless sway 1.12 with the GLES2 renderer
(`SWAY_UNSUPPORTED_GPU=true`), 27 September 2026:

- **W1**: `tests/window-checks.sh` driving `servo-window`, four runs, 19 of 19 checks passed
  each time: input, text and the clipboard, four scales, tabs, hide/show, history, five window
  sizes and a 40-size storm, idle, CSS animation and WebGL, two views at once,
  https://example.com/. Screenshots looked at (typed text at 100 % and 200 %, the split views,
  the page after the storm, the web page).
- **E1**: `tests/headless.sh`, each test binary its own process: `tests/engine.rs` (navigation,
  history, queued and failed loads, hidden views, closing, the profile lock, shutdown; 2 s),
  `tests/crash.rs` (a real script-thread panic through `ServoTestUtils.panic()`: test builds
  compile Servo with `testbinding`) and `tests/unavailable_context.rs` (no EGL driver). All pass.
- **U1**: `cargo test -p sighurt-engine-servo --lib`, 22 tests: navigation attribution and
  document identity, key, pointer and input-method translation, event coalescing, the profile
  lock, and a lost backend reported to a real Servo the way Servo reports it.
- Clippy with `-D warnings` on all targets and `cargo fmt --check`: clean.

After the independent review's fixes (decision 57), on this desktop (RTX 3070, driver 595.99.02)
in headless sway 1.12 with the GLES2 renderer, 2 October 2026 (louise-pc was building WebKit):

- **E1** again, all three pass. `engine.rs` (20 s) now also runs against a local HTTP server
  (`tests/common::serve`): a navigation asked for 0.5 s after one that got a 204 commits with its
  id 5.0 s after the 204 was asked for, and the 204's is `Cancelled`; a page's own navigation to a
  page that answers after 3 s commits with an id of its own, and the browser's navigation asked
  for 1 s into that wait commits after it, with its id; two backs asked for together commit
  `one.html` and `typed.html` with their own ids, `Traverse`; a back that finds no entry by the
  time Servo gets it is `Cancelled`; the page's own `history.back()` commits with an adapter id,
  `Traverse`; a pop-up's `OpenRequested` carries its opener's document; Stop 1 s into a 3 s
  response doesn't stop it (it commits with its id), and Stop 5.5 s after a 204 gives that one up
  (`Cancelled`). `read_to_image` 0, GL reads 0; `ServoTiming` filled (37 frames, first frame
  14 ms after its navigation).
- **U1**: 27 tests, adding exact traversal attribution, a page's own traversal while the
  browser's is on its way, a forgotten request, the history effects and the coalescing queue
  (`a_flood_of_replacements_stays_small` caught a bug in it before it ran in Servo).
- **W1**: `tests/window-checks.sh`, 21 of 21 checks in the second run (in the first, the idle
  check counted the extra iteration the example now runs after an action, which a `mark` no
  longer gets). Replacing "back and forward commit": the commits
  after back, back, forward are `#two`, `#one`, `#two`, each `Traverse`; a wheel notch reaches
  the page ("scrolled"); after the storm the window's last configure (580×340) is sway's size
  for it (sway again dropped the final `resize set`, M1.md). An action's events now come at
  once (`open=` → `NavigationStarted` within 1 ms; they waited for the next wakeup). WebGL: 691
  frames, paint mean 1.6 ms, swap mean 0.2 ms; two animated views 470 frames each; idle: 1
  wakeup in 6 s; `read_to_image` 0 and GL reads 0 in every run. With
  `RUST_LOG=sighurt_engine_servo::timing=debug` every frame logged its `servo-timing` line.
  Screenshots looked at (after the storm, the split views).
- Clippy `-D warnings` on all targets and `cargo fmt --check`: clean.

At browser level (decision 58), `sig` with its default features (Servo and Wasm) on this desktop
(RTX 3070, driver 595.99.02), 2 October 2026:

- **B1**: `sighurt-browser/tests/engines.sh`, scenario `servo`, in `tools/headless-sway
  --renderer gles2` (sway 1.12). It drives `sig` with configured keys, a virtual pointer and
  `wtype` against a local web server (`tests/serve.py`, pages in `tests/pages/`), and asserts on
  `sig`'s own diagnostics (which tab and view are shown, each tab's loading and history state,
  frame, wakeup and readback counters, Servo's GL reads audited with `SIG_AUDIT_GL=1`), its
  session file (the test pages report what they received through `history.replaceState`, which
  `sig` saves), the server's request log and screenshot pixels. 59 of 59 checks passed in the
  run `final` (scratchpad), with the fixes of decision 58; screenshots looked at (the typed and
  pasted field, the WebGL page, https://example.com/, the page at scale 2, the loading page).
  `sig` is a dev build there: dependencies optimized, the workspace's own crates not.
- **B2**: scenario `cross`, the same tab across the engine boundary (M6's segments, run here
  because the task asked for it): 33 of 33 (M5).
- **L1**: one 18 s run of `sig` on the user's River 0.4.8 (gharial) desktop, a static local page,
  no input and no screenshot, its own configuration and profile: `renderer engine=servo
  mode=hardware gl="NVIDIA GeForce RTX 3070/PCIe/SSE2"`; 2 page frames at 1542×1304, 0 page
  readbacks and 0 GL reads (audited); over 10.3 s of idle, 1 wakeup and 1 iteration (the report
  that asked for the numbers), 0 pumps, 0 frames, 0 chrome paints; SIGTERM, exit 0. The host
  found the viewporter, fractional scale, single-pixel buffer, text input, clipboard and primary
  selection, cursor shape, presentation time and decorations. First frame 131 ms after
  navigating.
- After decision 58's adapter fixes: E1 (all three test binaries, `engine.rs` now also checks
  that Stop stops a document whose image never comes), U1 (28 tests) and W1 (21 of 21) pass;
  clippy `-D warnings` on all targets of the adapter and of `sig` with its default features, and
  `cargo fmt --check`, clean. `sig`'s demo suite (`tests/headless.sh`) still passes 44 of 44.

- [x] Browse a fixed set of pages. *B1:* local pages over HTTP (`input.html`, `history.html`,
  `second.html`, `count.html`, `loading.html`, `slow.html`, `animated.html` with a CSS animation
  and WebGL 1) and https://example.com/ (title "Example Domain" in the session) commit in Servo,
  rendered by "NVIDIA GeForce RTX 3070/PCIe/SSE2" (`mode=hardware`, never a software fallback);
  the WebGL page presents 119 frames in 2 s (the headless output's 60 Hz) with 0 chrome paints; a
  page that was opened in a background tab before the window had its size fills the page area
  when shown (pixel-checked; decision 58). *Adapter:* W1 also loads them on louise-pc's RTX 4060.
- [x] Open, close and switch tabs. *B1:* Ctrl+T opens a tab on the home page, a typed address
  opens in it in Servo; Ctrl+W closes it and its view (3 tabs, 3 views, the session forgets it);
  a click on the tab strip, Ctrl+Tab and Alt+N each show the tab's own view. A page animating in a
  background tab leaves `sig` asleep (1 wakeup in 3 s, the report's own; before decision 58 such
  a tab, opened hidden, woke it 137 times a second), and so does a static page with that tab
  hidden again. *Adapter:* E1 and W1 above.
- [x] Navigate back/forward, including same-document entries. *B1:* `history.html` adds `#one`
  (a fragment) and `#two` (`pushState`) itself, then `second.html`: Alt+Left returns to `#two`
  (another document), again to `#one` (the same document), Alt+Right to `#two` and to
  `second.html`; the tab's history stays one Servo segment of five entries, each saved under its
  own address (Servo 0.5 named the `#one` entry `#two` until decision 58), and the tab can go
  back, not forward. *Adapter:* E1 and W1 above.
- [x] Reload and stop, with one accepted Servo 0.5 limit (decision 80). *B1:* F5 loads
  `count.html` again (the server sees a second request) and adds no entry. Stop (F7) on
  `loading.html`, whose image the server holds for a minute, ends the tab's loading at once (it was
  loading 2 s after the commit); before decision 58 it did nothing, because
  `Window.prototype.stop` doesn't exist in Servo. *E1:* Stop gives up a load Servo dropped after a
  204. *Not met, and accepted (decision 80):* Stop 1 s into a navigation whose response takes 3 s
  doesn't stop it: the page commits when the response comes (seen in B1, where `engines.sh` logs
  it as INFO, pinned in E1; Servo 0.5 has no way to cancel it, decision 57).
- [x] Use the clipboard and text input. *B1:* "åäö" and a dead diaeresis + a, typed with `wtype`
  into a field of the page, reach it ("field åäöä"); Ctrl+A, Ctrl+C put "åäöä" on the Wayland
  clipboard (`wl-paste`), and Ctrl+V pastes `wl-copy`'s "xyz" ("field åäöäxyz"): both through the
  core's service policy and the host's clipboard. A wheel notch reaches the page. Input-method
  composition is unit-tested (U1), not run against an input method.
- [x] Resize across scales. *B1:* at scales 1.25, 1.5, 2 and back to 1 the page reports
  `dpr 1.25 1024x536`, `dpr 1.5 853x429`, `dpr 2 640x296` and `dpr 1 1280x696` (the page area at
  that ratio); the view's buffer is its logical size times the scale (1280×670, 1280×644,
  1280×592, 1280×696) at scale 150, 180, 240 and 120 over 120; and a click at page (100, 100)
  reaches the page at CSS (100, 100) at every scale. *Adapter:* W1 adds window sizes and a storm.
- [x] Close without an adapter-triggered whole-process exit. The crate has no `process::exit`,
  `_exit`, panic hook, logger or TLS provider. E1: `shutdown` closes Servo, the contexts, the EGL
  display and the leases in M1's order and returns, and the profile's lock is held until Servo has
  shut down and then released; W1: every run ends "the compositor reported no error", exit 0.
  After a page crash or a lost backend, when Servo 0.5 can't shut down, `shutdown` still returns,
  leaving Servo running (`ServoDiagnostics::leaked`, decision 54; `crash.rs`). *B1, B2, L1:* `sig`
  quits on Ctrl+Q and SIGTERM with exit 0 and no forced exit, after Servo wrote its profile
  (cookies, HSTS, auth cache: its shutdown ran to the end).

§25's engine test, "page errors, failed navigation, unavailable context, hidden tabs and runtime
failure produce distinct outcomes": a page crash is `ViewFailed` and the view loads again, with
no `ViewClosed` and the other view untouched (`crash.rs`); an address that isn't one is
`NavigationFailed(Load)`, and one that can't be reached commits Servo's error page and fails
nothing (`engine.rs`); no GL context is `ViewFailed` then `ViewClosed`, the runtime goes on
(`unavailable_context.rs`); a hidden view presents nothing (`engine.rs`); a lost backend is one
`RuntimeFailed`, after which every call fails (U1: Servo's constellation can't be killed from
outside, so the loss is reported through `ServoDelegate::notify_error`, as Servo does). No run
read page pixels back: `read_to_image` 0 and, with the GL audit on, 0 GL reads in every run of
W1, E1, B1 (346 Servo frames, every `frame` line `readback_bytes=0`) and L1. An idle static page
left the loop asleep: 1 wakeup (the mark itself) in 6 s (W1), 1 in 3 s (B1), 1 in 10 s (L1).

Measured in B1 (`frame` lines, decision 58): Servo's own paint 2.1 ms on average over 346 frames;
the first frame of each view's context and the first at a new scale are slow, 25–34 ms (5 frames
over 20 ms), and the very first 137 ms (159 ms for the whole `present`), all on the owner
thread, so the chrome waits that long once per view (L1: 83 ms on River).

Open: stopping a navigation that waits for its response (accepted as a Servo 0.5 limit, decision
80); a field identity in
`TextInputState` (decision 44), which Servo can supply; `<select>`, dialogs and HTTP
authentication (decision 16);
Servo 0.5's other limits in decisions 55 and 57 (the old layout for a frame after a resize,
overlapping navigations beyond 5 s); the `gl_handles` safety note in `sighurt-wayland`
(decision 51: one EGL display per connection); the slow first frames above.

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
- [x] `CpuTarget` for `sighurt_wayland`'s SHM `TargetLease` and `FramePermit`, in the engine
  (`sighurt-engine-wasm/src/wayland.rs`): `present` turns the permit into the host's
  `ShmFrame`, paints at the permit's metrics straight into it and submits it; a paint that
  fails abandons the buffer unsubmitted. `WasmRuntime::<TargetLease>::new(config, waker)` is
  what `sig` constructs (decision 45).
- [x] Run on a Wayland host: `sighurt-engine-wasm/examples/wasm-view.rs` opens a
  `sighurt-wayland` window with one SHM target and one Wasm view, forwards metrics and input,
  turns `FrameReady` into invalidations and permits into `present`, and wakes on a calloop
  ping. In `tools/headless-sway` (sway 1.12, pixman) on this machine, 27 September 2026, grim
  screenshots looked at: raf-demo at 100 % (1280×800) animating at the output's 60 Hz (296
  frames in 5 s, the ball moving between shots), paused by Space (`wtype -s 300 -k space`: the
  next two shots identical, "PAUSED", and no more frames submitted), and crisp at 200 %;
  gradient-demo at 200 % on a 2560×1600 output, then rescaled to 150 % while running (right
  layout, no band seams; 3 frames and 11 loop wakeups in 6 s: a static page sleeps);
  hello-sighurt at 150 %, and pointer motion reaching it in surface coordinates (its "position
  (400, 300)" and dot where the pointer was). Every run: no protocol error, 2 buffers, exit 0.
  `wtype` without `-s` loses the first press (the new virtual keyboard's race noted in M3), and
  a `wlrctl` click was lost the same way.
  Automated in `sighurt-engine-wasm/tests/headless.sh` (27 September 2026, pixman, passes):
  `tests/wayland.rs` (`#[ignore]`d outside it) presents on real SHM leases: another target's
  permit is refused unpainted, a frame whose painting fails is not submitted, and afterwards
  each lease submits a frame at its permit's buffer size; then the example runs raf-demo and
  gradient-demo each through a live resize to 1000×700 and a rescale to 125 %, and the script
  checks the counters it prints: exit 0, final buffer 1000×700, bytes matching it, at most
  three buffers (the host's bound), no retiring generation; raf-demo 352 frames in 6 s,
  gradient-demo 4 (it presents only when its layout changes). Screenshots looked at: both
  crisp and laid out at each size.
- [x] The runtime in `sig`'s registry and a run of `sig`: `sig` starts `WasmRuntime<TargetLease>`
  on first use (decision 56), and **B3** below runs it.

**B3**: `sighurt-browser/tests/engines.sh`, scenario `wasm`, `sig` with its default features in
`tools/headless-sway --renderer gles2` (sway 1.12) on this desktop (RTX 3070), 2 October 2026:
29 of 29 checks passed in the run `final` (scratchpad), screenshots looked at. The guests are
the example guests' release builds and four test guests in WAT (`tests/guests/`: an endless
`on_frame`, a host call in an endless loop, a guest busy 40 ms every frame, and the engine
tests' invalid requests), which Wasmtime compiles as it does binaries. **B2**, scenario `cross`,
takes one tab across the engine boundary: 33 of 33 in `final`. A Servo page that adds its own
`#one` and `#two`, then a Wasm app from the web server (`[routing.wasm] origins`) in the same tab,
a segment of its own: back crosses into the Servo segment at `#two`, its view kept (the server
sees no second request), back and forward move within Servo, forward crosses into the kept Wasm
view (no second request); a Servo page opened from the app is a third segment, and back twice
and forward twice cross both boundaries, each segment shown in its own engine's view; at most
three live views (the shown one and two retained). After a restart the tab has the same three
segments as records, shows the third, and back loads the app and then the Servo page at `#two`
again, each in its own engine, every saved entry kept (the entries before `#two` become a record
of their own, decision 29).

Compatibility sample (tests in `sighurt-engine-wasm/tests/`, run on this machine on 26
September 2026 with the example guests built for `wasm32-unknown-unknown`; the SDK and examples
are unchanged). `cargo test` of the crate needs those guests built first
(`tools/cargo build --target wasm32-unknown-unknown --release -p hello-sighurt -p raf-demo -p
gradient-demo -p typography-demo`); CI's test job doesn't build them yet.

| Guest | What it exercises | Evidence |
|---|---|---|
| hello-sighurt | shapes, text, 100 %/200 % scale, page zoom | `runtime::hello_sighurt_draws` (pixel probes); B3: presents in `sig`, the pointer at window (400, 384) shows as "position (400, 300)" with its dot (screenshot) |
| raf-demo | `request_animation_frame` via `on_timer`, `on_frame`, hidden-view policy | `runtime::raf_demo_animates`, `runtime::a_hidden_view_steps_once_a_second`; B3: 121 frames in 2 s, 0 chrome paints |
| gradient-demo | linear gradients, text | `golden::gradient_demo` (probes, golden image); B3: static, `sig` asleep (1 wakeup in 3 s, the report's own); B2: loaded from the web server by `[routing.wasm] origins` |
| typography-demo | `canvas_text_ex` weights/styles/alignment, `measure_text`, 150 % | `golden::typography_demo`, `golden::typography_demo_at_150_percent` |
| timer-demo | `set_interval`, `clear_timer`, hand-drawn buttons on mouse clicks | B3: Start Blink makes the light toggle (pixel-checked every 0.25 s: lit and unlit both seen), the stopwatch reads 2.2 s 2.2 s after Start (screenshot) |
| events-demo | `on_event` for built-in events (touch from the mouse, focus, resize) and `emit_event` | B3: a held button is `touch_start` drawn at the pointer (pixel-checked), released `touch_end`; Emit "ping" counts 1 per click, focus "focused", a resize "1000 x 596" (screenshots) |
| WAT guests | timers, custom and resize events, typing (dead key, Ctrl), Enter, links (hover, click, middle click), `push_state` + `Traverse`, back and forward across documents, permission prompt, KV store shared by two views, workers | `runtime::*`; B3: the looping, busy and invalid-request guests in `sig` |
| audio-player, sse-demo, fullstack-notes, index | not exercised yet | — |

Pass:

- [x] The chosen existing guests run with documented compatibility: the table above. Engine side
  through the real `EngineRuntime` with an in-memory target; in `sig` on the host's SHM targets
  (B3): hello-sighurt, gradient-demo, raf-demo, timer-demo and events-demo each load in the Wasm
  engine (`renderer engine=wasm mode=cpu`), present when their tab is shown and take the input
  above. Not exercised: audio-player, sse-demo, fullstack-notes, index (media and network, whose
  services M6 reattaches).
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
  module of up to 50 MB) and a file dialog, which waits for the user. Such a wait no longer
  holds a worker (decision 45): six guests each blocked in a fetch to a server that never
  answers leave a key press reaching another guest in milliseconds
  (`runtime::guests_waiting_on_the_network_do_not_hold_up_the_others`, which timed out with the
  hand-over removed; `pool::tests::a_waiting_job_hands_its_place_to_a_spare`,
  `pool::tests::spares_are_bounded`). Past 16 spares per pool, waits hold their threads again.
  *B3:* in `sig`, a guest looping in `on_frame` and one calling the host in an endless loop are
  each stopped within moments of loading ("on_frame halted: fuel limit exceeded" in the status
  strip, the tab marked failed, and it stays stopped), Alt+1 still switches tabs at once, and no
  engine pump of the whole run held the owner 50 ms (longest 135 µs).
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
  *B3:* the engine tests' invalid-request guest, opened in `sig`, says it lived through them
  (its `push_state` to `#alive` is in the session), its tab shows no error, and its frame shows
  the clear (0, 0, 64) outside every clip and the green fills (0, 128, 0) inside (pixel-checked).
- [x] Stale completions are discarded: `runtime::a_superseded_load_is_discarded`,
  `runtime::a_step_that_finishes_after_navigation_is_discarded`,
  `runtime::nothing_arrives_for_a_closed_view`; what they leave is dropped off the owner thread
  (`view::tests::replaced_loads_and_closed_views_stop_and_are_dropped_elsewhere`,
  `pool::tests::a_dropped_pool_drops_its_jobs_on_its_threads`).
- [x] Guest activity does not starve chrome input. *B3:* eight letters typed one by one into
  the address bar, each timed from just before `wtype` starts to `sig`'s next paint of the
  chrome strip (so `wtype`'s own start is included): over the static gradient-demo median
  11.4 ms, longest 13.4 ms; over a guest busy 40 ms every frame (presenting 20 frames a second)
  11.0 and 13.4 ms; over raf-demo at scale 2 (2560×1600, painted on the owner thread) 16.4 and
  22.3 ms; every key repainted the chrome; no engine pump held the owner 50 ms (longest 135 µs;
  longest `present` 16 ms; longest chrome paint 44 ms, a dev build's first at scale 2). The
  looping guests above leave tab switching working. Engine side:
  with six guests taking 40 ms a frame on the four worker threads, a key reached another guest
  in 2–21 ms and no `pump` took
  more than 24 µs (`runtime::input_reaches_a_guest_while_others_are_busy`); a page that draws
  the same frame every frame wakes the owner 0 times a second
  (`runtime::a_static_page_leaves_the_owner_asleep`); another guest measuring 8 MiB of text a
  frame doesn't slow painting (`runtime::measuring_text_does_not_hold_up_painting`: 1 ms);
  history reports of a guest replacing 3 MiB of history every frame took 2 % of the owner's
  time, 2 ms at most per pump (`runtime::history_is_bounded`); guests blocked on the network
  don't hold up the others (above). Painting (decision 45), measured with
  `golden::presenting_at_laptop_sizes` in a release build on this machine (12 threads, before
  the desktop embargo; the time `present` spends painting into the lent buffer, median and
  slowest of 60 frames at 1280×800 logical pixels):

  | | raf-demo 100 % | raf-demo 200 % (2560×1600) | gradient-demo 100 % | gradient-demo 200 % |
  |---|---|---|---|---|
  | one thread, shaping every frame (before) | 1.4 / 8.1 ms¹ | 3.9 / 18.7 ms¹ | 9.4 / 23 ms¹ | 20.9 / 31.9 ms¹ |
  | one thread, lines cached | 0.98 / 9.3 ms | 2.8 / 4.4 ms | 10.5 / 16.9 ms | 25.3 / 37.5 ms |
  | bands (8 here), lines cached (now) | 0.91 / 6.0 ms | 1.8 / 4.8 ms | 3.9 / 7.9 ms | 8.1 / 16.9 ms |

  ¹ Timed around `present`, so it includes the in-memory test target allocating a buffer.

  The rows above were measured before the review fixes of decision 45 (the budget decided for
  the whole frame, text blended outside the painting lock), which have not been measured under
  the same conditions. After them, the same test (typography-demo added) in a release build on
  this machine at idle priority during the embargo, the user's session competing for the cores,
  gave raf-demo 0.91 / 9.3 ms and 1.7 / 3.5 ms, gradient-demo 9.6 / 22.3 ms and 14.1 / 20.7 ms,
  typography-demo 1.0 / 8.1 ms and 2.3 / 11.7 ms (100 % and 200 %, median / slowest). These
  numbers don't compare with the table, and they don't confirm the band speedup for
  gradient-demo; that needs a run on an idle machine.

  raf-demo, which changes every frame, now costs the owner under 2 ms a frame at 2560×1600.
  gradient-demo is static: it is painted only when its layout changes (a resize or scale
  change), 8 ms at 2560×1600, and at no other time. What is left is memory bandwidth (full-page
  fills and overlapping gradient strips). Still open: the owner waits while the bands paint;
  a present that returns before painting ends needs a `Send` handle to the lent buffer from
  `sighurt-wayland` (decision 45). In the debug example under headless sway, at idle priority
  during the embargo, `present` took 6.6 ms median for raf-demo at 1280×800 (the crate
  unoptimized). Needs measuring in `sig` with the chrome.

### M6 — Complete browser behavior

- [ ] The user can complete the intended daily workflows.
- [ ] Failures identify the responsible view/engine.
- [ ] Every retained feature has a tested implementation or a clearly documented unsupported
  state.

### M7 — Measure, compare, and retire the old display path

The measurement: [`bench/results/IPC-REMOVAL.md`](../../bench/results/IPC-REMOVAL.md) (decision 79).
The baseline binaries (`gpui-baseline`, the September set's) and the rewrite at 3c510fe (default
features, release, Rust 1.98.1) ran interleaved in one session on `louise-pc` (RTX 4060, driver
595.80, headless sway 1.12 from the pinned nixpkgs, GLES2, 1280 × 800) on 2 October 2026: the 16
workloads of `bench/workloads.toml`, one warm-up and 7 measured rounds a build, 256 runs, none
failed, no rewrite run with an anomaly. Sets `bench/results/louise/{baseline,rewrite}/20261002-131052/`;
the predictions, written before the first run, are in `bench/HYPOTHESES.md` (20 of 25 held).
Every change below has p = 0.0006 (exact Mann–Whitney, 7 against 7):

- Animated Servo pages: CPU −74 % (`css-animation` 52.3 → 13.7 % of a core), −76 % (`webgl`),
  −43 % (`canvas`); wakeups 7,728 → 851/s; no chrome repaints (59 → 0 a second). The pipe alone
  (`bench/pipe-cost`, the baseline's frame size and rate) costs 6,500 wakeups/s and 10.6 % of a
  core; the readback with its comparison is the largest share of the rest.
- Idle: 191 → 0.1 wakeups/s, CPU 0.7 → 0 %. Memory: one article 375 → 184 MiB PSS, eight tabs
  after visiting all 978 → 409 MiB, a Wasm guest 242–246 → 78–82 MiB. Launch to page on screen
  0.40 → 0.15 s. Scrolling to a submitted frame 17.4 → 2.2 ms. Chrome input beside a busy guest
  33 → 17 ms; the output flood now loses no chrome input (20 of 20 lost) and quits on ctrl+q.
- Build cost: 13 min 04 s of cargo time for `sig` from an empty target against the baseline's
  16 min 04 s over two workspaces (different compilers: indicative only).
- *A threat to these figures (review, decision 84): the rewrite measured, 3c510fe, had no WPE
  WebKit. The default build after it linked libWPEWebKit and its 72 libraries into every `sig`
  (29.8 MiB PSS, 82 000 relocations at start, measured by the review). Since decision 84 the
  engine opens WebKit only when its first WPE page opens, so a session without WPE pages maps 6
  more libraries than a build without the engine (GLib's) and nothing of WebKit; measured
  against such a build in "Review fixes" (dev builds on this desktop: launch time and the
  article's memory the same within noise, the home page +0.9 MiB).*

- [x] Evidence shows the normal page readback path is eliminated. *Servo's `read_to_image`
  count (`page_readbacks`) 0 in all 64 Servo runs of the session (35,848 frames), where the
  baseline read back 6.8–202 MiB/s; with Servo's GL audit (`SIG_AUDIT_GL=1`,
  `bench/results/louise/rewrite-audit/20261002-131305/`), 0 `glReadPixels`/`glGetTexImage` in 24
  runs of the eight Servo workloads (CSS animation, WebGL, a 2D canvas, scrolling, eight tabs,
  tab churn, a resize storm across four scales). On louise-pc's NVIDIA GLES2 path; not run on
  River or another driver.*
- [ ] No major functional regression in the agreed workflows remains.
- [ ] Lifecycle tests show no unbounded resource growth. *So far: `resize-storm` keeps 15 MiB
  (baseline 111); `tab-churn` keeps 58 MiB after 10 cycles (baseline 13) and, in a 60-cycle
  exploratory run (3 times), about 69 MiB, level after 20 cycles (baseline 22–26 MiB); during two
  minutes of `css-animation` RSS rose about 1.1 MiB a minute (one run; H18 says how to settle it).*
- [ ] Remaining regressions have an explicit decision. *Open, measured and not decided:
  `tab-churn`'s retained memory (H12); Wasm animations double the compositor's CPU, which now
  uploads a whole shared-memory page per frame (H14); Servo animations at 58.2 instead of 59.2
  frames a second (H16); the flood's raster holds the owner thread 63 ms a frame, so chrome
  input waits 53 ms (H15); static guests wake the process about 250 times a second (H13).*

Not done for M7: another compositor family, River.

### WPE — Make WPE WebKit available on both machines

Short-term web compatibility through WPE WebKit, built from source; Servo stays the long-term
engine. How it is built and why, with timings, disk use and the smoke test:
[`WPE.md`](WPE.md). Files: `nix/nixpkgs.nix`, `nix/wpewebkit.nix`, `nix/build-wpewebkit.sh`,
`nix/wpe-smoke.c`, `nix/wpe-smoke.sh`, `shell.nix`. Decisions 59–61. The engine adapter is not
part of this.

- [x] nixpkgs pinned for the repository (§21): `nix/nixpkgs.nix`, revision 4975466 (the
  desktop's system revision), imported by `shell.nix`. *`rustc.drvPath` through the pin is
  `9f6ax144…-rustc-wrapper-1.98.1.drv` on both machines, the same as the desktop's `<nixpkgs>`;
  `nix-instantiate shell.nix` gave `5g4i70pv…-nix-shell.drv` on the desktop before and after the
  change, and the same on louise-pc.*
- [x] WPE WebKit 2.52.6 built from the signed release tarball on louise-pc: WPE Platform API
  (Wayland, DRM, headless) and the libwpe API, bubblewrap sandbox, GStreamer, introspection,
  MiniBrowser; no Qt, no docs. *`tools/heavy nix/build-wpewebkit.sh` on louise-pc, 2 October
  2026: 1 h 22 min, `-j6` under a 13 GiB cap (peak 12.4 GiB); the prefix's 43 store-path
  references are all in its GC-rooted runtime closure (the script's check).*
- [x] The same prefix on the desktop. *`nix/build-wpewebkit.sh --pull louise-pc`: rsync, root,
  check ok; a manifest of the prefix (190 entries: contents, modes, symlinks) hashes the same on
  both machines.*
- [x] `shell.nix` exposes the prefix when it exists (`SIGHURT_WPE_PREFIX`,
  `SIGHURT_WPE_PKG_CONFIG_PATH`, `LD_LIBRARY_PATH`, `GIO_EXTRA_MODULES`,
  `GST_PLUGIN_SYSTEM_PATH_1_0`) and is unchanged when it doesn't. *The shell derivation above;
  with the prefix present, `cargo build -v -p sighurt-wayland -p sighurt-engine-wasm` on the
  desktop reported all 549 units `Fresh`.*
- [x] A program loads pages with it on both machines. *`tools/heavy tools/headless-sway
  --renderer auto -- nix/wpe-smoke.sh`: 22 of 22 checks on the desktop (RTX 3070) and on
  louise-pc (RTX 4060). Headless and on Wayland, pages load and are painted, the compositor shows
  them, WebGL works and GStreamer plays H.264, VP9 and Opus, the web process runs under `bwrap`,
  `https://` works, and `webkit://gpu` reports the NVIDIA GPU through GBM with DMA-BUF frames.*

Open (WPE.md, "Open problems"): the DRM platform, the MiniBrowser, WebDriver and audio are
untested; test helpers still take their tools from `<nixpkgs>`; no machine compiles with Rust
1.95 any more. *Since ("Final verification and documentation"): `sighurt-browser/tests/engines.sh` takes wl-clipboard from the
pin; `sighurt-wayland/tests/headless.sh`, `sighurt-engine-servo/tests/window-checks.sh` and
`experiments/m1-mixed/live-check.sh` still use `<nixpkgs>`.*

### WPE — Prove WPE WebKit as an in-process engine (its M0 and M1)

The WPE equivalent of M0 and M1 (§23, with §9, §10, §18, §19, §27): [`WPE.md`](WPE.md), "WPE
WebKit as an in-process engine: the proof". Experiments: `experiments/wpe-engine` (the prototype
of `sighurt-engine-wpe`: `WpeRuntime` behind `EngineRuntime` on the real host, the `wpe-window`
driver and `window-checks.sh`) and `experiments/wpe-coexist` (Servo, the Wasm engine and WPE in
one process and one window, `coexist-checks.sh`). Run outputs: `experiments/wpe-*/runs/`. Tests:
`cargo test -p wpe-engine` (2 unit tests: the GLib loop's wakes and sleep, the keysym table);
clippy `-D warnings` and rustfmt clean on both crates. Decisions 62–66. **Go** for
`sighurt-engine-wpe`, which superseded the prototype (next section); `experiments/wpe-engine` and
its runs are at commit 3c510fe.

M0's criteria, for WPE:

- [x] WPE WebKit, Servo 0.5 and Wasmtime 36 (with the whole Wasm engine) link into one executable
  and work in one process. *`coexist-checks.sh` on the desktop (headless sway, GLES2): 14 of 14;
  `wpe-coexist` live on River: 12 of 12. JavaScript and Wasm traps in Servo and WebKit pages,
  Wasmtime traps before and after WebKit starts, 2 000 traps each way in both pages while two
  threads trap in Wasmtime, a trapping Wasm guest failing its view only, every engine presenting
  (Servo `read_to_image` 0, WebKit 0 copies), a clean shutdown in 12 ms with exit status 0.*
- [x] Symbol clashes checked, and the one found fixed: the executable exported its static
  HarfBuzz, FreeType, libpng, mozjpeg, SQLite and SpiderMonkey's `getenv`/`abort`, and shared
  libraries bound into them, WebKit's included. *`LD_DEBUG=bindings LD_BIND_NOW=1`: 1 307 exports
  and 1 524 bindings into the executable (220 from `libWPEWebKit-2.0.so.1`) as Cargo links it; 1
  export and 0 bindings with `-Wl,--exclude-libs,ALL` (decision 65). No library mapped twice.*
- [x] Signal and trap handlers reviewed with WebKit in the process, and the clash found fixed:
  JavaScriptCore's handler in the UI process plus Wasmtime's `longjmp` leave every signal blocked,
  and the next Wasmtime trap kills the process with `SIGILL`. *Reproduced
  (`runs/desktop-gles2-jsc-signal-handlers.txt`, a core dump); with JavaScriptCore's polling
  traps and no Wasm fault handler in Sighurt's process (decision 66), the signal mask stays intact
  and the concurrent trap checks pass.*
- [x] A crashed web process is a view failure, not a browser crash. *`SIGKILL` of WebKit's web
  process: `ViewFailed` (reason `CRASHED`) in about 0.2 s, the browser and the other engines go
  on, a reload starts a new web process and shows the page (`window-checks.sh` on both machines;
  `wpe-coexist` on the desktop and River).*
- [x] No blanket unsafe workaround: the fixes are a linker flag and two JavaScriptCore options;
  the experiments' `unsafe` is FFI and reading signal dispositions and masks.

M1's criteria, for WPE (`window-checks.sh`: 41 checks with DMA-BUF, 40 with shared memory, all
passing on the desktop and on louise-pc, in headless sway with GLES2, pixman, and GLES2 with
`--shm`; River runs live, without input or screenshots):

- [x] A real page and an animated/WebGL page present through the GPU path onto the host's page
  subsurface, as WebKit's DMA-BUFs attached as `wl_buffer`s. *0 CPU copies; 4 imports for a whole
  animation; 350 frames in 6 s at 17.1 ms median (60 Hz output) on both machines, 1 200 frames in
  10 s at 8.33 ms median on River at 120 Hz with the RTX 3070.*
- [x] No CPU copy on the normal path, and the fallback diagnosed: shared memory where DMA-BUF is
  unavailable (pixman) or forced, one copy per frame, counted. *`shm-copies`, `shm-bytes` and the
  transport in every report: 347 copies (1.35 GB) in 6 s at 1280 × 760; 1.01 GB/s on River.*
- [x] Frame callbacks and buffer release followed. *Frames paced by the host's callbacks
  (`wpe_view_buffer_rendered` at commit, so WebKit renders at most one frame ahead); releases
  hand buffers back (349 of 350 at the probe); at most 3 SHM buffers per view.*
- [x] Repeated resize, scale changes, hide/show and teardown behave correctly. *Scales 1.5, 2,
  1.25, 1 and a resize: the page sees the right `devicePixelRatio` and size, shows, and clicks land
  at the same CSS position; at most one stale frame per change, one-pixel rounding differences
  shown (WPE.md); hidden views present nothing; shutdown clean.*
- [x] Keyboard focus routes to page and chrome, with keysyms, keycodes and modifiers. *å ä ö and
  a dead-key ä typed, F2 delivered, blur and focus when the chrome takes and returns the keyboard,
  nothing typed into the page meanwhile, clicks and the wheel.*
- [x] Page animation does not cause chrome repaints. *2 chrome paints (first configure and resize)
  during every animation, on all machines and live.*
- [x] An idle page leaves the owner loop asleep. *1 loop wakeup (the probe's own) and 0 GLib
  dispatches in 5 s, every machine and transport, and on River. That counts the owner loop
  only, not WebKit's threads and processes; across `sig`'s whole process tree an idle shown WPE
  page makes 4.7 context switches a second, and a hidden one that animated about 150 (WebKit's
  own; "Review fixes").*
- [x] WebKit's GLib main loop runs inside the owner loop with no busy polling (decision 64).
  *Unit test `sources_wake_the_owner_and_an_idle_context_sleeps`; the idle figures above.*
- [x] Diagnostics identify the transport, the DRM device and the fallback. *`transport`,
  `device` (`/dev/dri/renderD128`), formats (756), copies and bytes in every report.*
- [x] Runs on River with gharial and the RTX 3070. *Three live runs (20, 15 and 9 s): a DMA-BUF
  animation then an idle page, the SHM fallback, and `wpe-coexist` (12 of 12).*

Open (WPE.md, "Limitations and open problems"): `requestAnimationFrame` at 60 Hz on faster
outputs (no `WPEScreen`); input methods, clipboard, popups, new windows, downloads, permissions,
the back-forward list and accessibility are not in the prototype (the engine crate does all but
popups and accessibility); implicit synchronization only; the host's `PresentMode::Gl` name for a
non-GL presenter; one unexplained click offset right after a page loaded; `wpe-coexist` not run
on louise-pc (Servo must be rebuilt there); `ci.yml` and CLAUDE.md's pre-commit commands must
exclude `sighurt-engine-wpe` and `wpe-coexist` (*done*, "Final verification and documentation").

### WPE — The engine crate `sighurt-engine-wpe`

The prototype as a usable engine (plan §14 by analogy, §12, §16, §17, §20, §25): [`WPE.md`](WPE.md),
"`sighurt-engine-wpe`: the engine". `WpeRuntime` implements `EngineRuntime<Target = TargetLease,
FramePermit = FramePermit>` through the WPE Platform API; `sig` wires it in separately (feature
`engine-wpe`). Decisions 69–73. Tests: `cargo test -p sighurt-engine-wpe --lib` (14 unit tests:
navigation rules and history effects, input-method signals on WebKit's own class, keysyms, the
GLib loop, cursors, coalescing, schemes); `tests/headless.sh` runs `tests/engine.rs` in headless
sway; `tests/window-checks.sh` drives `examples/wpe-window`. Clippy `-D warnings` and rustfmt
clean. All runs on this desktop (RTX 3070), headless sway 1.12, 2 October 2026.

- [x] Navigation events with ids: `NavigationStarted` for the browser's, `Committed` with a
  document and a history effect from WebKit's back-forward list by entry identity, `Failed`
  (`Load` or `Cancelled`); title, the hovered link as status text, cursor, `LoadingChanged`.
  *`navigation::tests` (6, including WebKit's dropped pending-request marker); `engine.rs`:
  fragment and `pushState` keep the document with the page's ids, back across and within
  documents, forward by two, beyond the end, reload, two navigations in a row, an unresolvable
  host, a 204, Stop; window-checks: back, back, forward, reload commit as traversals and a
  replace, the status text follows the pointer over a link.*
- [x] `HistoryChanged` from the back-forward list, traversal by any delta. *`engine.rs`: entries
  and current index after each step; beyond the list `Cancelled` without WebKit.*
- [x] Reload, stop, zoom, visibility. *`engine.rs` (reload, Stop, hidden views present nothing and
  present when shown); window-checks (hide/show, tabs, page zoom: the page sees
  `devicePixelRatio` 2 and half its size).*
- [x] Clipboard and permission prompts as `ServiceRequested`, failing closed. *`engine.rs`:
  geolocation refused (the page gets error 1) and granted (error 2: WebKit's own GeoClue provider is
  refused); window-checks: copy reaches the Wayland clipboard through `ClipboardWrite` (`wl-paste`)
  and paste comes from it through `ClipboardRead` (`wl-copy`, "field åäöäyxyz"). Camera and
  microphone are mapped but WebKit asks only when a capture device exists, and none was visible in
  the sandbox.*
- [x] New windows as `OpenRequested(NewView)` after a user's click; downloads as
  `ServiceRequested(Download)`. *`engine.rs`: a click on a `target=_blank` link asks for a tab
  without navigating; an attachment and unshowable content become download requests with URL and
  name, the navigation `Cancelled`; other schemes `OpenRequested(CurrentView)`; `window.close()`
  `CloseRequested`; file pickers as `PickFiles` for a click only.*
- [x] Text input (IME) through `WPEInputMethodContext`. *`ime::tests` (WebKit's signals in order);
  `engine.rs` with a real input method (zwp_input_method_v2 through the host): preedit shown,
  commit typed, surrounding text seen, a password field as a password with `sensitive_data` and
  `hidden_text` and without its text.*
- [x] `ViewFailed` on a web-process crash, with reload; `RuntimeFailed`; shutdown order. *`engine.rs`:
  a terminated web process fails its view only, the other view navigates and presents, a reload
  makes a new document; window-checks: `SIGKILL` of the web process, then the page shown again;
  every run exits with no protocol error. `RuntimeFailed` (a broken compositor connection on the
  presenter's queue) is implemented, not run.*
- [x] JavaScriptCore's two options before WebKit is first used; the GLib watch thread with
  `EngineWaker`. *Unchanged from the proof; `glib_loop::tests`; `wpe-coexist` now links the crate:
  `coexist-checks.sh` 14 of 14 (Servo, Wasm and WebKit traps, crash, exports).*
- [x] DMA-BUF presentation with a diagnosed, counted SHM fallback; diagnostics. *window-checks 60
  of 60 with GLES2 (DMA-BUF: 0 copies, 4 imports for 350 frames), pixman (SHM: 346 copies,
  1.35 GB, logged as the fallback) and GLES2 with `--shm` (343 copies); `renderer()` names
  transport, device and driver. Outputs in `sighurt-engine-wpe/runs/`.*
- [x] Real pages, https, WebGL, video, two views, å/ä/ö, scales 1, 1.5, 2 (and 1.25), idle wakeups,
  zero copies on DMA-BUF. *window-checks: `https://example.com/` loads and is shown, WebGL
  animates (350 frames in 6 s, 17.1 ms median), a VP9 video plays, two views animate side by side,
  `å ä ö` and a dead key typed, the page sees each scale's `devicePixelRatio` and size and clicks
  land at the same CSS position, 1 loop wakeup and 0 GLib dispatches in 5 s idle.*
- [ ] Run on louise-pc and in `sig`. *In `sig`: done, next section. On louise-pc: not yet; its
  benchmark runs held its lock, and its profile's Rust is 1.95 while the workspace needs 1.98 (the
  pinned nixpkgs' 1.98.1 works when put first on `PATH`). Since ("Review fixes"): `shell.nix`
  brings the pin's Rust (decision 86); louise-pc didn't answer over SSH in that pass.*

Open (WPE.md, "Limitations and open problems (the engine)"): `requestAnimationFrame` at 60 Hz;
navigation-id races in the instant between a browser request and WebKit starting it; an empty
paste before the real one; downloads fetched again without WebKit's cookies; `file:` media and
media without an audio server; `<select>`, context menus, dialogs, HTTP authentication,
fullscreen, pointer lock, accessibility; 4.0 loop wakeups per animated frame on DMA-BUF (from 5.8:
decision 73).

### WPE — In `sig`: the default build, per-site routing, links and settings

`sig` compiles WPE WebKit in by default and opens with it what the configuration and the user give
it; Servo stays the default engine for the web ([`WPE.md`](WPE.md), "In `sig`"). Decision 78.
Everything below ran on this desktop (RTX 3070, headless sway 1.12 with GLES2) on 2 October 2026;
scratchpad runs `wpe-int/…`.

- [x] Feature `engine-wpe` in `sig`'s `default = [...]` beside `engine-servo` and `engine-wasm`;
  `[engines] default` stays `servo`, and WPE opens what `[routing.wpe] hosts` gives it and what
  `page.open-in wpe` asks for. The built-in configuration and the README say so. *`engines.sh`
  `wpe`: a page on `localhost`, which `[routing.wpe] hosts = ["localhost"]` claims, opens in
  WebKit; `:page.open-in wpe https://example.com/` too; a page on 127.0.0.1 typed into the same tab
  opens in Servo.*
- [x] A machine without the WPE prefix gets a clear build error. *The engine's build script stops
  with `cargo::error` lines naming `nix/build-wpewebkit.sh`, `--pull louise-pc`, shell.nix and
  `--no-default-features --features engine-servo,engine-wasm`, then why (no
  `SIGHURT_WPE_PKG_CONFIG_PATH`, or pkg-config's answer): both cases built in a scratch target
  directory.*
- [x] `sig` linked with `-Wl,--exclude-libs,ALL` and an RPATH to the prefix (`build.rs`, decision
  65). *`sig` exports 1 symbol (`main`), where the build before exported 1 075; with
  `LD_DEBUG=bindings LD_BIND_NOW=1 sig --version`, no shared library binds into it but libpulse's
  look-up of `main` (837 bindings before, 0 of them from WebKit's library, which wasn't linked);
  `engines.sh` `wpe` checks both.*
- [x] `[engines.wpe] user-agent` and `storage`. *`registry::tests`:
  `the_core_learns_what_each_engine_honors` (WPE honors both, and a file setting both is valid),
  `webkits_configuration_follows_its_settings` (profile `{data_dir}/sighurt/wpe`, none when
  ephemeral; the user agent); `tests/engine.rs`: the configured agent is the header WebKit sends
  and `navigator.userAgent`; `engines.sh` `settings`: with `user-agent` and `storage =
  "ephemeral"`, `page.open-in wpe` of `ua.html` reports the agent both ways, and after quitting
  nothing of WebKit's is in the data or cache directory; `wpe`: WebKit keeps its profile
  (`localStorage`, caches) in the data directory.*
- [x] `NavigationRequested` and `AnswerNavigation` in the WPE adapter, for a link or a GET form the
  user clicked (WebKit's user-gesture flag and navigation type; WebKit's policy decision doesn't
  tell frames apart, WPE.md has the probe). *`tests/engine.rs`
  `the_page_asks_before_it_navigates`: a click on a link asks, for the link's document, and nothing
  is fetched before the answer; allowed, it commits with the page's own id (`Push`); the browser's
  back and reload don't ask; denied, nothing is fetched; a new document refuses what the old one
  held and a late answer does nothing; a GET form asks; a click's script and a POST form go ahead
  without asking; 17 links clicked in 17 frames: frames loading ask nothing, 16 are held and the
  17th goes ahead, then the 16 go ahead when allowed; closing the view refuses what it holds and
  nothing of the view follows `ViewClosed`. `engines.sh` `wpe`: from the WPE page a link to a page
  routing gives Servo opens there and back returns to the kept WPE page, a link on its own host
  stays, a click's script stays in WebKit, and a link in a Servo page to `localhost` opens in
  WebKit; the core answered WebKit within 0.07–0.23 ms (eight answers, four runs).*
- [x] `page.open-in wpe` and the capabilities, exported like Servo's. *`sig`'s registry takes
  `sighurt_engine_wpe::CAPABILITIES`; the runtime reports the same (no "as the registry says"
  warning in any run's log).*
- [x] End to end in `sig`: `engines.sh` scenario `wpe`, 59 checks: what `sig` exports and binds;
  the routing rule and `page.open-in wpe` (https://example.com/); a click at its position, å, ä, ö
  and a dead key, the clipboard both ways; idle after WebKit's scrollbar fades (the owner loop: 1
  wakeup in 3 s, no frame, no paint; the whole process tree only since "Review fixes"); a local WebGL page at the output's 60 Hz with no chrome paint and no CPU copy
  (every WPE frame `readback_bytes=0`, the renderer line `mode=hardware` on DMA-BUF); a download of
  content WebKit can't show into `[downloads] directory`; one tab across Servo, WPE and Wasm back
  and forward with nothing loaded again; links between Servo and WebKit; `SIGKILL` of WebKit's web
  processes, the tab's error, Servo going on and F5 in a new web process; a clean quit and the
  profile. *Runs `wpe-int/e4` and `wpe-int/verify/engines`: 59 of 59 each; screenshots
  `02-typed-and-pasted`, `03-webgl`, `04-example-com`, `06-from-servo-to-wpe`, `07-crashed` and
  `08-reloaded` looked at.*
- [x] Found and fixed: JavaScriptCore took `sig`'s SIGUSR1 (decision 78). *The first `engines.sh`
  run: WTF logged "Overriding existing handler for signal 10", and `sig` stopped at the first
  diagnostics request. With `JSC_SIGNAL_FOR_GC=12` set at the start of `main`, the check "WebKit's
  JavaScriptCore left sig's SIGUSR1 (diagnostics) alone" passes and every diagnostics request is
  answered.*

Tests: `cargo test -p sighurt-api` 32, `-p sighurt-core` 126, `-p sighurt-engine-wpe --lib` 14,
`-p sighurt-browser` 33 with `engine-demo,engine-wasm,keymap-modal`, 32 with `engine-wpe` alone
and 35 with its default features and `keymap-modal`; the WPE compositor test
(`sighurt-engine-wpe/tests/headless.sh`, GLES2) and `window-checks.sh` (GLES2, 60 of 60) after the
change. Clippy `-D warnings` on all targets of api, core and the WPE adapter, of `wpe-coexist`, and
of `sig` with no engine, `engine-demo`, `engine-wpe`, `engine-demo,engine-wasm,keymap-modal,
chrome-line`, `engine-servo,engine-wasm` and its default features with `keymap-modal`; rustfmt;
rustdoc `-D warnings` on the WPE adapter (its broken and private links fixed) and the api. With the
final code, both suites whole: `engines.sh` 217 of 217 (servo 63, wasm 29, cross 33, settings 17,
links 16, wpe 59; run `wpe-int/verify/engines`) and `headless.sh` 74 of 74. Eight guarantees broken
on purpose in place, one at a time under one hold of the machine's lock, each file checked byte for
byte afterwards, each failed the test meant for it (`wpe-int/breaks`): a held navigation used when
dropped, a click's script asked about, a POST form asked about, no limit of 16, a new document
keeping what the old one held, the user agent not applied, and in the registry `ephemeral` keeping
a profile and the user agent not passed on.

Open: the WPE scenario hasn't run on louise-pc or River; JavaScriptCore logs "ERROR: invalid
option: JSC_SIGNAL_FOR_GC=12" once per WebKit process; a script's navigation that a click ran stays
in WebKit and a link clicked in a frame moves the whole tab (WPE.md, "In `sig`"); `[engines.wpe]
javascript` could be honored (WebKit's `enable-javascript` setting) but nothing uses it yet
(extension-api.md §1.10); CLAUDE.md's pre-commit commands and `ci.yml` must exclude
`sighurt-engine-wpe` and `wpe-coexist` (no WPE prefix in CI), and CLAUDE.md's "Anything that builds
Servo" commands now also need the prefix (not this track's files). *Since ("Final verification and documentation"): the CI and
CLAUDE.md items are done; `sig` with WPE WebKit ran on River (R2: a page the routing gave WPE,
DMA-BUF on the RTX 3070, no copy); the `wpe` scenario still hasn't run on louise-pc.*

### Extension API — configuration and the keymap and chrome APIs (steps 1–4)

The design is [`extension-api.md`](extension-api.md); this is its part 3, "A. The APIs, with no
change in behavior", steps 1–4. Decisions 67 and 68. Everything below ran on this desktop
(RTX 3070) on 2 October 2026 with the working tree of that moment.

- [x] Step 1: `sighurt_api::{settings, keymap}`, `input::KeyboardFocus` (replacing the core's and
  the chrome's `Focus`) and `commands::CommandSet`; the `bindings` keymap through them. *`Chord`
  moved unchanged into the api, its seven tests with it; `KeyState` holds a `Box<dyn Keymap>`
  and keeps the taken-press rule for any keymap. `sighurt-api/tests/keymap.rs` implements a
  keymap with a two-key sequence from outside the crate. Core tests with a fake keymap
  (`tests/browser.rs`): `a_taken_press_takes_its_release_whatever_the_keymap_says`,
  `run_then_pass_delivers_to_the_new_focus`, `the_keymap_hears_context_changes`,
  `a_changed_indicator_redraws_the_chrome`. The existing key tests pass after the rename
  (`consumed_presses_consume_their_releases`, `a_held_key_never_turns_into_a_shortcut`,
  `losing_the_keyboard_forgets_held_keys`, `bound_keys_never_reach_the_page_by_halves`,
  `focus_follows_the_core`).*
- [x] Step 2: `[input] keymap`, `[keymap.<name>]`, the build's kinds in `Environment`,
  `Config::parse(.., &Environment)`, notices. *`config::tests`:
  `an_unknown_keymap_is_an_error_that_lists_the_builds`,
  `tables_for_what_the_build_lacks_get_a_notice` (keymaps, chromes, routing, `enabled`; the
  built-in file stays silent), `a_key_table_nobody_reads_gets_a_notice` (both ways),
  `keymap_bindings_points_to_keys`, `a_keymaps_problems_are_prefixed_and_in_the_one_list`,
  `the_selected_keymap_reads_its_own_table`, `dates_are_not_settings`; `tests/browser.rs`:
  `a_reload_that_keeps_the_keymap_configures_it_and_keeps_its_state`,
  `a_switch_replaces_the_keymap_and_a_held_keys_release_is_still_taken`,
  `configuration_notices_are_shown_at_start_and_on_reload`. Changed: every caller of
  `Config::parse`.*
- [x] Step 3: `sighurt_ui::extension`; the `standard` chrome through it; tabs by id; `sig`
  contains the snapshot's strings. *sighurt-ui's `Chrome` is `Model`, returning `ChromeAction`s
  (`UiAction` is gone); `Snapshot::keymap` and `Snapshot::contain` (every field's bound) are new;
  the core has `BrowserEvent::SelectTab` and `CloseTab`. All sighurt-ui tests pass and the eleven
  golden images are unchanged (`git status sighurt-ui/tests/golden` is clean; nothing was
  blessed). Changed: `tabs_select_close_and_open` (`SelectTab(id)`, `CloseTab(id)`). New:
  `sighurt-ui/tests/extension.rs` drives the standard chrome through `Box<dyn Chrome>` only (a
  tab click selects by id, an identical snapshot repaints nothing, a resize repaints every listed
  surface, painting the page through the trait matches golden `home-100` and is byte for byte
  the model's painting) and implements a chrome of its own (input areas, a theme, a text field);
  `state::tests::every_string_of_a_snapshot_is_contained_to_its_bound`; in the core
  `closing_a_tab_by_id_after_the_list_shifted` and `selecting_a_closed_tab_by_id_does_nothing`;
  in `sig` `chrome::tests::the_snapshot_contains_what_pages_say` (a title with U+202E and a
  newline, through a real core).*
- [x] Step 4: `sig` drives `Box<dyn Chrome>` with named surfaces and input areas; `[ui] chrome`;
  the core checks `[ui]`. *`sig`'s `chrome::ChromeTargets` is the bookkeeping, unit-tested
  without a compositor (`names_get_targets_in_the_chromes_order`,
  `a_ninth_name_is_refused_and_reported_once`, `rectangles_are_clipped_to_the_window`,
  `input_rectangles_are_clipped_to_their_surface`,
  `a_name_no_longer_listed_is_hidden_with_its_last_rectangle`); core tests
  `ui_is_checked_against_the_build`, `unknown_color_names_are_errors` (moved from
  `persistence`), `a_reload_that_changes_the_chrome_says_it_applies_at_restart`; changed
  `keys_and_ui_are_the_users_alone` (`[ui] font` valid, `fnt` an error).
  `sighurt-browser/tests/headless.sh`: 45 of 45, the roles still `chrome`, `status`, `page`, and
  the new check "no chrome target held more than 3 buffers" (2 at most over the main
  scenario's storm and scale changes; scratchpad runs `e2e1` and `e2e2`, the second with the
  final code; screenshots `main/06`, `config/02` and `config/03` looked at: the unknown color
  is now the core's error). `engines.sh` with Servo
  and Wasm: 121 of 121 (servo 59, wasm 29, cross 33; run `eng1`, screenshots `servo/04` and
  `servo/06-scale-1.5` looked at).*

Tests: `cargo test -p sighurt-api` 28 + 1, `-p sighurt-core` 45 + 52, `-p sighurt-ui` 28 + 6 + 23
+ 11 (1 ignored) + 1, `-p sighurt-browser` 27 with `engine-demo` and 27 with its default features; clippy
`-D warnings` on all targets of api, core and ui, and of `sig` with no engine, `engine-demo`,
`engine-demo,engine-wasm` and its default features; rustfmt. Fourteen guarantees broken on
purpose in a copy of the file, one at a time, each failed the test meant for it: a passed repeat
of a taken press, a `RunThenPass` press taken, the context never told, a reload always replacing
the keymap, a switch forgetting taken presses, an indicator change not redrawn, tabs by number,
the restart notice, the unused-`[keys]` notice, the missing-keymap notice, an uncontained
snapshot, hidden surfaces left out of the placement, unclipped input areas, a tab click as a
numbered command.

Open: `[ui] font` and `font-size` are checked and reach the chrome in its `Theme`, but the
standard chrome draws with its own font and sizes until step 17, and the default file doesn't
document them yet; `extension-api.md`'s status line and changelog don't record the
implementation; CLAUDE.md's and the README's command lists gain `page.open-in`, `page.send-key`
and `chrome.<action>` with steps 5, 6 and 12, which add them. `sig` with these changes has not
run on River. *Since ("Final verification and documentation"): the font is drawn (step 17) and documented, the command lists
are complete, and `sig` with the `bindings` keymap and the standard chrome ran on River (R2);
`extension-api.md` is still open.*

### Extension API — commands, the modal keymap, side buttons and scrolling (steps 5–7 and 10)

[`extension-api.md`](extension-api.md), part 3, steps 5, 6, 7 and 10, their API, core and `sig`
side; the chrome's half of steps 5 and 10 (the standard chrome's `chrome.toggle-tabs`, removing
its side-button code) is the chrome track's. Decision 74. Everything below ran on this desktop
(RTX 3070) on 2 October 2026, with the working tree of that moment (the chrome and WPE tracks'
work in progress included).

- [x] Step 5: `chrome.<action>`. `Command::Chrome(action)`: kebab-case, 1 to 32 characters, no
  argument. It is checked at load against the selected chrome's actions through `CommandSet`, in
  `[keys]`, `[input.buttons]` and any keymap's table alike; the core returns
  `Effect::ChromeCommand(action)`, and `sig` hands it to the running chrome as
  `ChromeEvent::Command` or shows a notice naming the commands it has (`sig`'s `chrome::command`).
  *api `commands::tests::chrome_commands_are_kebab_case_and_take_no_argument` (an argument, an
  empty action, upper case and `_` refused) and `a_command_set_names_what_its_chrome_lacks`; core
  `config::tests::chrome_commands_are_checked_against_the_selected_chrome` (the three tables in
  one list, and the same commands for a chrome with none) and `chrome_commands_go_to_the_chrome`
  (bound and typed; the keyboard stays where it is); `sig`
  `chrome::tests::a_chrome_gets_its_own_commands_and_a_notice_says_what_it_lacks`; `headless.sh`
  `modal`: `:chrome.nope` puts `sig`'s notice in the status strip (screenshot `modal/04`: "The
  standard chrome has no command chrome.nope; it has chrome.toggle-tabs.").*
- [x] Step 6: `page.send-key <key>`. `Command::PageSendKey(SentKey)`, with the modifiers, the W3C
  key and the evdev code: a named key (`down`, `pageup`, `enter`, `f5`, …, or a W3C key name that
  is also the code of a key that types nothing on common layouts and isn't one of a pair or on the
  keypad) or `space`, with modifiers; characters and `code:` are refused. The core presses and
  releases it in the shown view, back to back, only while the page has the keyboard; run by a key
  press, that press is the page's activation. *api `send_key_presses_named_keys` (codes, the round
  trip through `Display`, space's text, every refusal); core
  `a_sent_key_is_pressed_and_released_in_the_page_back_to_back` (two effects, nothing between),
  `a_sent_key_needs_the_page_to_have_the_keyboard` (the chrome has it; a `sig:` page shows),
  `a_key_sent_by_a_key_press_is_the_users_activation` (as a command a pop-up is blocked; from
  `alt-j` it opens a tab); `engines.sh` `servo`, on a page 20 000 pixels tall: `alt-j`, bound to
  `page.send-key end`, reaches Servo as a keydown with key and code `End` and scrolls the page to
  its bottom (19 304), and `page.send-key home` brings it back to 0.*
- [x] Step 7: `sighurt-keymap-modal` (§2.5), feature `keymap-modal`, `[input] keymap = "modal"`:
  modes, sequences of up to 4 keys, `always`, `pass` modes, a `typing` mode, `{ run, pass = true
  }`, the indicator. It depends on `sighurt-api` only (`toml` is a dev-dependency of its tests).
  *`sighurt-keymap-modal/tests/modal.rs`, 13 tests through `KIND` and `Box<dyn Keymap>` only: the
  design's table is valid (its chrome command checked against the chrome), modes and the indicator
  ("NORMAL g"), sequences, a failed sequence starting afresh, modifier and dead keys inside a
  sequence, unbound keys swallowed or passed, the typing mode, the chrome (only `always`, and only
  its keys that don't type), `RunThenPass`, repeats, `reset` and `context` ending a sequence, a
  reload keeping the mode, and every configuration problem (19, including a prefix in the same
  mode and with `always`, and sequences starting with a typing key where keys pass). `headless.sh`
  `modal`, 13 checks: `g` alone waits and `g t` shows the next tab; in normal mode `z` and `j`
  type nothing, `i` then "hej" reaches the page and Escape leaves insert mode (the session's title
  is exactly "Demo: resize · hej"); `x` closes a tab; `:` opens the command line with the colon in
  it, and `tab.select 1` typed there (with `t` bound in normal mode) runs; ctrl-q in `always`
  quits (screenshots `modal/02`–`04` looked at).*
- [x] Step 10: `[input] scroll-speed` and `[input.buttons]`, core and `sig`. `sig` sends every
  side-button event to the core as `BrowserEvent::Button`, over a view or the chrome; the built-in
  file binds `back` and `forward` to `page.back` and `page.forward`. *core
  `config::tests::input_settings_merge_key_by_key_and_are_checked`,
  `a_bound_buttons_release_stays_taken_after_a_reload_unbinds_it` (and an unbound press and its
  release reach the page), `side_buttons_over_the_chrome_run_their_commands_or_go_nowhere`,
  `scrolling_goes_scroll_speed_times_as_far` (pixels and 120ths, rounding never to 0);
  `the_mouse_back_button_goes_back` passes unchanged; `headless.sh` `modal`: the back button over
  the tab strip goes back and the forward button over the page goes forward (the tab's history
  state in the diagnostics); `engines.sh` `servo`: a wheel notch scrolled a Servo page 15 px, and
  45 px after `[input] scroll-speed = 3` was added to the file while `sig` ran.*

Tests: `cargo test -p sighurt-api` 30 + 1, `-p sighurt-core` 47 + 59, `-p sighurt-keymap-modal`
13, `-p sighurt-browser` 28 with `engine-demo,keymap-modal` and 28 with its default features and
`keymap-modal`. Clippy `-D warnings` on all targets of api, core and the modal keymap, and of
`sig` with no engine, `engine-demo`, `engine-demo,keymap-modal`,
`engine-demo,engine-wasm,keymap-modal` and its default features with `keymap-modal`; rustfmt.
`headless.sh` (built with `engine-demo,keymap-modal`): 58 of 58, three times (scratchpad runs
`e2e1`, `e2e2` with the final code, and `e2e3` with the scripts' fix below). `engines.sh` `servo`,
with Servo built here: 63 of 63, the 59 of before and the 4 above (run `eng2`); its first try ran
another track's demo build of `sig`, which a build had put in `target/debug` between this script's
build and its link, so `engines.sh` and `headless.sh` now build and link their binary under one
hold of the machine's lock. Twenty-five guarantees broken on purpose in a scratch copy of api,
core and the keymap, one at a time, each failed the test meant for it: a sent key reaching the
page while the chrome has the keyboard, a sent key not counting as activation, a taken button's
release decided by the binding of the moment, an unbound button over the chrome reaching the page,
a wheel step rounded to nothing, no scaling, a side button arriving as `Pointer` skipping the
bindings, no `ChromeCommand`, button commands parsed without the chrome's actions, no range check
on `scroll-speed`, a button's default lost when the file has `[input]`, characters, paired keys
and keypad keys accepted as sent keys, upper case in a chrome action, `CommandSet` not checking
the action; and in the keymap: no prefix check, no typing-start check, a failed sequence not
starting afresh, modifiers breaking a sequence, the last key of a sequence repeating its command,
typing keys of `always` applying in the chrome, a mode's bindings applying in the chrome, unbound
typing keys passing in normal mode, no typing mode, and a context change keeping an unfinished
sequence.

Open: CLAUDE.md's command list and the README's command table need `page.send-key <key>` and
`chrome.<action>`, and CLAUDE.md's pre-commit clippy of `sig` and `ci.yml` the `keymap-modal`
feature; `extension-api.md` doesn't record the implementation; the built-in file documents the
standard chrome's command but not yet its `[chrome.standard]` options (steps 15 and 16, the chrome
track's); the standard chrome doesn't show the keymap's indicator (the chrome track's); the modal
keymap's limits of §2.5 stand (no way out of a page's text field but a click, no link hints, no
find, no counts); `sig` with these changes has not run on River. *Since ("Final verification and documentation"): CLAUDE.md,
the README, `ci.yml` and the built-in file are done, and the standard chrome shows the indicator
(decision 75); the modal keymap ran end to end with all three engines (`engines.sh` `showcase`),
not on River.*

### Extension API — the chromes: options, the line chrome, fonts (steps 5, 8, 10 and 15–17)

The chrome's side of [`extension-api.md`](extension-api.md) part 3: step 5's
`chrome.toggle-tabs`, step 8 (`sighurt-chrome-line`, with sighurt-ui's `Canvas` split out of
`paint_cpu`), step 10's removal of the chrome's side-button code, and steps 15–17
(`[chrome.standard]`, `tabs = "left"`, `[ui] font` and `font-size`); and a bug the verification
found: the address line showed percent-encoded addresses as they were. Decision 75. Everything
below ran on this desktop (RTX 3070) on 2 October 2026 with the working tree of that moment, the
keys lane's core and `sig` changes in it. End to end: `sighurt-ui/tests/headless.sh` (new: `sig`
with `engine-demo,chrome-line` in `tools/headless-sway`, pixman, with the binaries copied out of
`target/` right after the build), 35 of 35 checks in the run `chrome-lane/run2` (scratchpad):
`line` 11, `overlay` 12, `left` 8, `font` 4; every screenshot looked at. Again 35 of 35 with the
final code (run `run3`).

- [x] Step 5, the chrome's side: `ChromeEvent::Command(action)`; the standard chrome's
  `commands` is `["toggle-tabs"]`, which hides the tabs or shows them where `tabs` puts them (at
  the top when that is `hidden`). *`sighurt-ui/tests/extension.rs::toggle_tabs_hides_and_shows_them`
  (top, left and hidden, through the trait). End to end (`left`): `ctrl-b` bound to
  `chrome.toggle-tabs` unmaps the `tabs` target and the page grows to 1280 px, and a second press
  maps it again. A first run bound it with the line chrome selected, and the file was refused:
  "the line chrome has no command chrome.toggle-tabs; it has none".*
- [x] Step 8: `sighurt-chrome-line`, `[ui] chrome = "line"`, feature `chrome-line` (two lines in
  `sig`'s manifest, two in its registry). *`cargo tree -p sighurt-chrome-line -e normal`:
  sighurt-api and sighurt-ui only. `tests/line.rs`, 7 tests through `Box<dyn Chrome>`: typing an
  address and Enter; `:tab.next`, the page getting the keyboard before the command; a bad
  command; Escape; a paste (only the latest answer, once, not after Escape; Ctrl+A Ctrl+C copies
  the decoded address); a click placing the caret where it is drawn at 100, 125, 150 and 200 %;
  its surfaces (`line`, and `page` on `sig://home`; `position = "bottom"`; 36 px at 18 px);
  repaints only for what changed; the kind's check. Golden images `line-100`, `typing-150`,
  `error-200` and `home-100` looked at. End to end (`line`): the roles are `line` (1280 × 28) and,
  on `sig://home`, `page`; `ctrl-l` and a typed `demo://resize` open it; a click on the line and
  a typed `demo://animation` open that (120 frames in 2 s, 0 chrome paints); 3 s idle: 0 chrome
  paints, 1 wakeup (the report's).* `Canvas` (with `Pen`) now holds what `CpuPainter` did to a
  buffer, and `Fonts`, `TextStyle` and `ShapedText` are public, for other chromes.
- [x] Step 10, the chrome's side: the standard chrome does nothing with the side buttons.
  *`side_buttons_are_not_the_chromes`; `toolbar_buttons` lost its side-button checks (the keys
  lane's core tests have them).*
- [x] Step 15: `[chrome.standard]` `status = "strip" | "overlay"`, `buttons`, `compact`, `tabs =
  "top" | "hidden"`, checked by the standard kind. *`layout::tests`:
  `each_option_moves_only_its_part`, and `odd_sizes_keep_everything_inside` over every
  combination of the options at 14 and 18 px; `hit_targets_stay_on_drawn_controls_at_every_scale`
  now with six option sets (compact; no buttons with the overlay; left; left and compact at
  18 px; hidden at 18 px) at four scales; `the_status_overlay_lies_over_the_page_and_takes_no_input`
  (listed only with status text, `InputArea::Rects([])`, one rectangle for every hovered link,
  the page unchanged); `the_standard_kind`; golden `compact-100`, `no-buttons-125`, `overlay-150`
  looked at. End to end (`overlay`): no status text, no status target; the pointer on the resize
  page maps the 640 × 20 overlay and the page stays 1280 × 716; under the overlay the page still
  gets the pointer (its crosshair line drawn at x 300, then 500) with 0 placements meanwhile;
  `status = "strip"` saved to the file maps the 1280 × 20 strip and the page shrinks to 696 at
  once, and back.*
- [x] Step 16: `tabs = "left"`: rows in a `tabs` surface of their own below the toolbar, the
  new-tab button the last row, scrolled a whole row at a time (3 a wheel notch; a row per row's
  height of touchpad). *`the_tab_list_scrolls_by_whole_rows` (40 tabs),
  `the_tab_list_scrolls_and_follows_the_active_tab` (only the list repaints; a switch to a tab out
  of view scrolls it into view), `the_tab_list_on_the_left` through the trait; the hit targets
  above; golden `tabs-left-125` looked at. End to end (`left`): a 220 × 736 `tabs` target beside
  the 1060 px page; a click on the third row shows tab 3.*
- [x] Step 17: `[ui] font` and `font-size`. *`heights_follow_the_font_size` (14 px: 40/44/20,
  18 px: 51/57/26, compact and 9 px too), `styles_scale_with_the_font_size`,
  `a_family_that_isnt_installed_falls_back_and_says_so` (by any case; the log's warning),
  `the_font_size_scales_the_standard_chrome` (a reload back to 14 px lays out again), the caret
  test at 14 and 18 px; golden `font-18-100` and `font-18-home-100` looked at. End to end
  (`font`): 18 px gives a 108 px chrome and a 26 px strip, the log says "the font "Nope Sans" is
  not installed; the chrome's text is in DejaVu Sans", and a saved 14 gives 84 and 20 at once.*
- [x] Addresses read decoded where that is safe (`extension::readable_url`), in the address line
  and in the status line's hovered links, in both chromes. *`extension::tests`:
  `addresses_read_decoded`, `what_could_hide_or_fake_the_address_stays_encoded`,
  `the_shown_form_opens_the_same_address` (the url crate parses each shown form back to its
  address); `the_address_line_reads_decoded` (Enter on it opens the decoded form; the page's
  accessible value stays the committed address). End to end (`font`, screenshot):
  `demo://pattern?%C3%A5%C3%A4%C3%B6%E2%80%AE#K%C3%A4llor` reads
  `demo://pattern?åäö%E2%80%AE#Källor`; in the keys lane's suite, `main/11` shows
  `demo://slow?åäö`.*
- [x] The standard chrome shows the keymap's indicator at the address field's right end (the
  keys lane's section left this open). *`an_identical_snapshot_repaints_nothing` changed: an
  indicator repaints the chrome alone; golden `compact-100` shows "NORMAL g"; in `sig` with the
  modal keymap, the keys lane's `modal/02` shows "INSERT".*

Tests: `cargo test -p sighurt-ui` 36 + 10 + 26 + 13 (1 ignored) + 1, `-p sighurt-chrome-line` 7,
`-p sighurt-browser --no-default-features --features engine-demo,chrome-line` 28; clippy `-D
warnings` on all targets of sighurt-ui, sighurt-chrome-line and that `sig`; rustfmt. The eleven
golden images from before re-render byte for byte (blessed into a copy, compared with `cmp`).
The keys lane's `sighurt-browser/tests/headless.sh` (standard chrome, `engine-demo,keymap-modal`):
58 of 58 (run `chrome-lane/sigsuite`; screenshots `main/06` and `main/11` looked at), and its
`modal` and `main` again with the final code, 42 of 42 (run `sigsuite2`, `modal/02` looked at).
Twenty
guarantees broken on purpose, one at a time while holding the machine's lock (so no other build
saw them), each failed the test meant for it: bidi controls or ASCII decoded, the raw address
shown, the overlay taking input or listed without text, repaints for unlisted surfaces,
`toggle-tabs` doing nothing or keeping hidden tabs hidden, the list's scroll repainting the
toolbar, no reveal of the active tab, one row a notch, no font-size scaling, `compact` ignored,
the side buttons back in the chrome, no log for a missing font, the address text half a pixel
off its caret rectangle; and in the line chrome a command before the keyboard moves, a stale
paste, a repaint for every snapshot, a click not placing the caret.

Open: `extension-api.md`'s changelog, CLAUDE.md's crate and module tables (the new crate;
sighurt-ui's `canvas`) and the README's commands are other tracks' files; `default-config.toml`
doesn't document `[chrome.standard]`'s options, `[ui] font` or `font-size` yet; the wheel over
the tab list is unit-tested only (the virtual pointer has no axis); the line chrome and the new
options have not run on River. *Since ("Final verification and documentation"): CLAUDE.md, the README and the built-in file are
done; the line chrome and `tabs = "left"` ran with all three engines (`engines.sh` `showcase`),
not on River.*

### Extension API — zoom, keywords, page.open-in, engine settings, files (steps 9, 11, 12, 14, 18)

[`extension-api.md`](extension-api.md), part 3, steps 9, 11, 12, 14 (Servo's and the Wasm engine's
half; WPE's is the WPE integration's: a line in the registry naming the settings it honors) and 18.
Decision 76. Everything below ran on this desktop (RTX 3070) on 2 October 2026 with the working
tree of that moment (the chrome and WPE lanes' work in progress included). End to end: a new
`settings` scenario in `sighurt-browser/tests/headless.sh` (demo engine, 16 checks) and in
`tests/engines.sh` (Servo and Wasm in headless sway with GLES2, 15 checks), all passing
(scratchpad runs `settings-lane/h1` and `settings-lane/e1`; screenshots `settings/01` of both
looked at).

- [x] Step 9: `zoom`, 0.5 to 3 (the ladder's ends), live. New tabs start at it and `page.zoom-reset` returns to it; open and restored tabs keep their own. *core
  `config::tests::zoom_is_within_the_ladders_ends`;
  `tests/browser.rs::new_tabs_start_at_the_configured_zoom_and_reset_returns_to_it` (the view opens
  at it, `SetZoom(1.25)` on reset, a reload leaves open tabs as they are, restored tabs keep 1.0,
  1.25 and 0.9 where a new one takes the new 2.0); `headless.sh` `settings`: the session's zooms are
  [1.25, 1.25], 1.5 after `:page.zoom-in`, 1.25 after `:page.zoom-reset`, and [1.25, 1.25, 0.9]
  after `zoom = 0.9` was saved while `sig` ran and a tab was opened.*
- [x] Step 11: `[keywords]`. When the first word typed is a keyword, the rest goes where its
  template has `{}`, encoded as a search is; a template without `{}` is used for the keyword
  alone. *router `keywords_come_before_paths_hosts_and_search` (17 cases: queries, a keyword
  alone, a template without `{}` with and without a query, URLs, `sig:`, paths, `~/`, host names,
  `localhost:8000`, upper case, a keyword that isn't the first word); config
  `keywords_are_names_for_web_templates` (every refusal); core `keywords_open_their_templates`
  (from `page.open`, and live after a reload); `engines.sh` `settings`: "t åäö" typed into the
  address bar opens `count.html?q=åäö` in Servo.*
- [x] Step 12: `page.open-in <engine> [address]`, and chosen views (`View::chosen`). The engine's
  name is checked for its form when the command is parsed, the build and `enabled` when it runs,
  with the tab's error as for a typed address; without an address it opens the tab's committed
  address, else the one loading, else the one that failed. *api
  `open_in_names_an_engine_and_may_take_an_address` and the round trip of
  `display_gives_the_canonical_string_back`; core
  `open_in_reopens_the_page_in_the_engine_named_and_back_returns` (a Wasm view for the Servo
  page's address that replaces it once committed, back to the kept Servo view and forward, the
  new view chosen; the page's own navigation leaves it chosen, `page.open` into the same engine
  unchooses it in the same view and `page.open-in` the same engine chooses it again without a new
  view; an address routing gives to Wasm opened in Servo; typed text through the router),
  `open_in_takes_the_address_still_loading`, `open_in_says_why_it_cant` (a `sig:` page, an engine
  not in the build with the page kept, one not enabled, a scheme the engine doesn't open);
  `headless.sh` `settings`: `:page.open-in demo demo://resize`, and `:page.open-in servo
  https://example.com/` in a demo build puts "The servo engine is not part of this build of sig."
  in the status strip with the page kept; `engines.sh` `settings`: `:page.open-in wasm` of an app
  on `http://localhost:<port>`, an origin routing gives to Servo, shows it in the Wasm engine as a
  segment after the Servo page, and back shows the Servo page.* Nothing reads `chosen` until step
  13's hand-off of page navigations.
- [x] Step 14, Servo and Wasm: `[engines.<engine>] user-agent` and `storage`, a vocabulary the core
  checks from one table (`config::ENGINE_SETTINGS`) against what the registry says each engine
  honors (`Environment::engine_settings`: Servo both, Wasm `storage`, the demo engine none).
  *config `engine_settings_are_checked_against_what_each_engine_honors` (values, an unknown key,
  a setting the engine can't honor naming what it honors, a misnamed engine, user agents with a
  line break, a tab, a letter beyond ASCII or DEL, a table for an engine the build lacks checked
  for its form with a notice); changed: `an_old_file_with_a_version_says_what_moved` (the hints
  for `[engines.wasm] command` and `extensions` instead of "[engines]: unknown field `wasm`");
  core `a_reload_that_changes_engine_settings_says_they_apply_at_restart`; `sig`
  `registry::tests`: `the_core_learns_what_each_engine_honors` (and each name is a setting the
  core reads), `servos_configuration_follows_its_settings`,
  `the_wasm_engines_directories_follow_its_storage`,
  `an_ephemeral_engines_directory_lasts_until_the_end_of_shutdown` (mode 700 under
  `$XDG_RUNTIME_DIR`, kept through `shutdown`, gone after `remove_scratch`); Servo
  `runtime::tests::user_agent_is_firefox` (the executable's agent as it is) and
  `tests/engine.rs`, whose Servo is configured with a user agent that a test page's
  `navigator.userAgent` puts in its title (`tests/headless.sh`: the three test binaries pass
  in headless sway with GLES2);
  `engines.sh` `settings`, with `[engines.servo] user-agent = "SighurtTest/1.0 (engines.sh)"`,
  `storage = "ephemeral"` and `[engines.wasm] storage = "ephemeral"`: `ua.html` reports both the
  header the server got and `navigator.userAgent` as that string; a Wasm app's key lands in
  `$XDG_RUNTIME_DIR/sighurt-wasm-*/kv` (mode 700); a user agent changed while `sig` runs gets a
  notice; after ctrl-q there is no Servo profile, no Wasm store or module cache in the run's data
  and cache directories, and the run's directory is gone; `headless.sh` `settings`:
  `[engines.demo] storage` saved while `sig` runs is refused, "[engines.demo] storage: the demo
  engine can't honor it (it honors: none)" in the log and a notice in the strip.*
- [x] Step 18: `[downloads] directory` (absolute or `~/`, live, made by the first download that
  needs it) and `[session] restore` (read at start; writing stops and resumes live). *config
  `the_download_folder_is_absolute_or_in_the_home_directory`,
  `the_session_is_restored_unless_the_file_says_not`; `sig`
  `services::tests::downloads_go_to_the_configured_folder_made_when_first_needed`,
  `a_folder_that_cant_be_made_fails_the_download_and_leaves_nothing`; `headless.sh` `settings`:
  with `restore = false` a `sig` started over a saved session of three tabs shows one tab and no
  view, and a new tab and a quit leave the file byte for byte as it was; `restore = true` saved
  while `sig` runs writes its two tabs at once.* No engine of this build downloads through the
  service yet (WPE's will), so the configured folder is unit-tested, not run end to end.

Tests: `cargo test -p sighurt-api` 31 + 1, `-p sighurt-core` 53 + 65, `-p sighurt-browser` 33 with
`engine-demo,engine-wasm,keymap-modal` and 34 with its default features and `keymap-modal`, `-p
sighurt-engine-servo --lib` 28. Clippy `-D warnings` on all targets of api and core, of `sig` with
no engine, `engine-demo`, `engine-wasm`, `engine-demo,engine-wasm,keymap-modal` and its default
features with `keymap-modal`, and of the Servo adapter; rustfmt on every file changed. With the
final code, both suites whole: `headless.sh` 74 of 74 (main 29, config 9, close 4, disconnect 3,
modal 13, settings 16; run `settings-lane/h2`) and `engines.sh` 140 of 140 (servo 63, wasm 29, cross
33, settings 15; run `settings-lane/e2`). Twenty-nine guarantees broken on purpose, one at a time,
each failed the test meant for it: 22 of the api and core in a scratch copy of the workspace with a
target directory of its own (new tabs, a reset or a restored tab ignoring `zoom`, no range check or
merge of `zoom`, keywords ignored or a template without `{}` taking a query, `localhost` or any
template accepted as a keyword, the core passing no keywords, `page.open-in` ignoring its engine or
the address loading, typing leaving a view chosen, the page's own navigation unchoosing it, engine
settings unchecked against the engine, any user agent, no notice for an engine table the build
lacks, no hint for the old `command`, no restart notice, `~/` not expanded, `[session] restore`
ignored, upper case in `page.open-in`'s engine), and 7 of `sig`'s in place under one hold of the
machine's lock, the files checked byte for byte afterwards (runtimes started without the launch
settings, the scratch directory readable by others or never deleted, an ephemeral Wasm engine
caching modules or keeping its store in the data directory, the download folder not made or
ignored).

Open: CLAUDE.md's command list and the README's command table need `page.open-in <engine> [url]`,
and `extension-api.md` doesn't record the implementation (not this lane's files); a Wasm app's own
downloads (`api_download_data`, `api_download_url`, `api_canvas_print_pdf`) still write to the
desktop's download folder themselves, not through the download service, so `[downloads]
directory` doesn't apply to them (M6 moves them to the service); `chosen` isn't kept by a record,
so a view restored from history is unchosen, and F5 on a failed `page.open-in` tries the address
again with routing; with `storage = "ephemeral"` a Wasm app's store is on disk under
`$XDG_RUNTIME_DIR` while `sig` runs, and stays there if `sig` is killed (a tmpfs, emptied at
logout); WPE's settings (`user-agent`, `storage`) are the WPE integration's; nothing has run on
River. *Since ("Final verification and documentation"): the command lists are done, and WPE's settings are (decision 78).*

### Extension API — host rules, and links that follow the routing (step 13)

[`extension-api.md`](extension-api.md), part 3, step 13: `[routing.<engine>] hosts` and the
hand-off of the navigations a page starts (`NavigationRequested`, `AnswerNavigation`), in
sighurt-api, the core and the Servo adapter, as one change. The WPE adapter's half is the WPE
integration's (below). Decision 77. Everything below ran on this desktop (RTX 3070, headless sway
1.12 with GLES2) on 2 October 2026 with the working tree of that moment; scratchpad runs
`step13/…`.

- [x] Host rules: `[routing.<engine>] hosts`, a host name or IP address, or `*.` and a host name
  (every host under it, at any depth, not the name itself), lower-cased and IDNA-normalized by the
  `url` crate as it normalizes a URL's host; a scheme, port, path, user or a `*` anywhere else is
  a problem, and a pattern belongs to one engine. Rule 6 of the router, after an origin's rule:
  an exact host beats any wildcard, a longer wildcard a shorter one. Merged field by field like
  the other routing fields. *config `host_rules_are_host_names_or_wildcards_of_one_engine`
  (normalized forms, `Bücher.de` as `xn--bcher-kva.de`, `[0:0::1]` as `[::1]`; 13 refused forms;
  `*.Example.com` and `*.example.COM` of two engines are one pattern claimed twice; the merge);
  router `host_rules_send_hosts_to_their_engines_the_most_specific_first` (19 URLs: exact versus
  wildcard versus a longer wildcard, any depth, not the bare domain, ports, IP addresses, IDNA, no
  `file:`, an origin's rule first; an explicit choice wins; a disabled engine is an error);
  `the_documented_examples_are_valid` reads the default file's `[routing.wpe]` example.*
- [x] The contract: `EngineEvent::NavigationRequested { view, document, request, url }` and
  `EngineCommand::AnswerNavigation { request, allow }` (an additive api change; their
  documentation is the contract an engine implements). The demo and Wasm engines never ask: one
  match arm each. `sig` carries the answer like any engine command.
- [x] The core's rule (`state::navigation_requested`): denied for a document its tab doesn't show,
  and for what no page may open (`sig:`, `file:` from a web page, with the notice `OpenRequested`
  gives); allowed for a chosen view; denied and opened in a new view as routing says when routing
  gives the URL to another engine that can open views now (in the build, enabled, working) and
  the user clicked or pressed a key in the page within 5 s, which that uses up; allowed
  otherwise. *`tests/browser.rs`: `a_clicked_link_to_a_host_another_engine_claims_opens_there`
  (4.9 s after a click; the new view replaces the page once it commits; back returns to the kept
  page; the click is used up; Enter in the page counts),
  `what_a_page_does_without_a_click_or_key_stays_in_its_engine` (none, one 5 s old; a link the
  page's own engine opens leaves the click for the next),
  `a_chosen_views_links_stay_in_its_engine_until_an_address_is_typed`,
  `a_link_to_a_host_no_rule_claims_returns_to_the_default_engine`,
  `only_the_page_a_tab_shows_may_navigate` (a replaced document, a page kept in the history; a
  background tab's page may), `links_routing_cant_place_stay_and_what_pages_may_not_open_is_denied`
  (a disabled engine, one the build lacks, `javascript:`, `blob:`, `data:`, a URL over 2 MiB
  stay; `sig:` and `file:` are denied with a notice; none uses the click up). The helper every one
  of these 25 requests goes through asserts exactly one `AnswerNavigation` for it; `every_event`
  gains `NavigationRequested`, so a closed or replaced view's late request gets nothing at all
  (`a_closed_tab_ignores_late_events`, `a_replaced_view_is_forgotten`).*
- [x] Servo's half: every navigation it would load itself is held (`delegate::PageNavigation`,
  which denies when dropped unanswered, where Servo's `NavigationRequest` allows) and reported
  for the current document; at most 16 per view, beyond them, or without a committed document, it
  goes ahead at once. A new document, a crash, closing the view (before its WebView goes) and
  shutdown (with notes not yet taken in) deny what is held. Servo 0.5 carries out the browser's
  reload as the page's navigation to its own address (`Location::reload`, through the
  constellation's `schedule_navigation`); the adapter lets that one go ahead without asking.
  `ServoDiagnostics::asked` counts what is held. *`tests/engine.rs`
  `the_page_asks_before_it_navigates` (the test loop answers itself): a click through the
  adapter's input on a page's link asks, for the link's document, and nothing is fetched before
  the answer; allowed, it commits with the page's own id (`Push`); a reload commits without
  asking; denied, the target is never fetched and the page stays; a new document denies what the
  old one held (`asked` 0) and a late answer does nothing; of 17 frames navigating themselves 16
  are held and the 17th goes ahead (title "landed 1"), then allowing the 16 gives "landed 17";
  closing the view while it holds one: the target is never fetched and nothing follows
  `ViewClosed`. `tests/headless.sh`: the three binaries pass (run `servo-4`).*
- [x] End to end: `engines.sh` `links` (16 checks, run `e1`): in a Servo page on
  `http://127.0.0.1:<port>`, a click on a link to `/guests/gradient-demo.wasm`, on the origin
  `[routing.wasm] origins` names, shows the app in the Wasm engine as a segment after the page
  (diagnostics: tab 1's view on `wasm`), and back shows the Servo page, not loaded again; a click
  on a link to `http://localhost:<port>/guests/hello-sighurt.wasm` with `[routing.wasm] hosts =
  ["localhost"]` opens it in Wasm too; a page that sends itself to that host 0.5 s after it loads
  stays in Servo; after `:page.open-in servo`, a click on a link to that host stays in Servo.
  Servo held each of the five navigations 0.08–0.20 ms before the core's answer
  (`servo-timing navigation-answered`): `sig` hands a pump's events to the core and carries out
  its answer in the same `pump_engines` call.
  Screenshots `links/01`–`03` looked at.*

Tests: `cargo test -p sighurt-api` 31 + 1, `-p sighurt-core` 55 + 71, `-p sighurt-browser` 33
with `engine-demo,engine-wasm,keymap-modal` and 34 with its default features and `keymap-modal`;
the Servo adapter's three compositor tests (run `servo-4`); clippy `-D warnings` on all targets
of api, core, the demo and Wasm engines, the Servo adapter and `sig` with no engine, with
`engine-demo,engine-wasm,keymap-modal` and with its default features and `keymap-modal`;
rustfmt. `headless.sh` 74 of 74 (run `h1`) and
`engines.sh` 156 of 156 (servo 63, wasm 29, cross 33, settings 15, links 16; run `e1`). Sixteen
guarantees of the core broken on purpose in a scratch copy of api and core with a target directory
of its own, one at a time, each failed the test meant for it (`core-breaks2.log`): a hand-off
without a click, a click not used up, a chosen view handed off, any document of the view asking,
no refusal of `sig:` and `file:`, an engine that can't open views taking the link, an answer
given twice, a denial without the new view, a wildcard matching its own domain, an exact host not
beating a wildcard, hosts before origins, `*` anywhere, a host claimed twice, hosts not merged, a
wildcard of an IP address, patterns not normalized. Six of the Servo adapter's, broken in place
under one hold of the machine's lock and each file checked byte for byte afterwards, each failed
`tests/engine.rs` (`servo-breaks.log`): the page navigating without asking, a denied navigation
going ahead, a held navigation allowed when dropped (Servo's own default), a new document keeping
what the old one held, no limit of 16, the browser's reload asking.

Open: **the WPE adapter's half** (the WPE integration's): its `command` needs an
`AnswerNavigation` arm to compile (until it reports any, `EngineCommand::AnswerNavigation { .. }
=> {}`); then, in `decide-policy`, a `NAVIGATION_ACTION` for the main frame that WebKit would load
itself and that the browser didn't start is held (`g_object_ref` on the decision) and reported
for the committed document, answered with `webkit_policy_decision_use` or `_ignore`, ignored when
dropped (a new document, close, shutdown), at most 16 per view. Servo 0.5 doesn't say which frame
a navigation is from, so a click on a link inside a frame that goes elsewhere moves the whole tab
(bounded by the click). A hand-off carries only the URL: a form's POST data and the referrer stay
behind. CLAUDE.md's engine contract (`OpenRequested` paragraph) and the README don't mention the
hand-off, and `extension-api.md` doesn't record the implementation (not this lane's files).
Nothing has run on River. *Since: the WPE adapter's half is done (decision 78); CLAUDE.md and the
README describe the hand-off ("Final verification and documentation").*

### Final verification and documentation

The wave's last pass: everything built and run together, a configuration showcase, a live run on
River, and the documents brought to the actual state. Decisions 80 and 81. Everything ran on this
desktop (RTX 3070, driver 595.99.02) on 2 October 2026, with the working tree after every track of
the wave; scratchpad runs `final/…`. `sig` is a dev build (dependencies optimized, Sighurt's own
crates not), built with its default features (Servo, Wasm, WPE WebKit) and `keymap-modal,
chrome-line`.

- [x] End to end, every suite whole. *`sighurt-browser/tests/headless.sh` (demo engine, pixman): 74
  of 74 (main 29, config 9, close 4, disconnect 3, modal 13, settings 16). `engines.sh` (GLES2): 250
  of 250 (servo 63, wasm 29, cross 33, settings 17, links 16, wpe 59, showcase 33).
  `sighurt-ui/tests/headless.sh`: 35 of 35 (line 11, overlay 12, left 8, font 4).
  `sighurt-wayland/tests/headless.sh`: 17 protocol tests. `sighurt-engine-wasm/tests/headless.sh`:
  the lease test, raf-demo (358 frames) and gradient-demo (4) through a resize and a rescale, 2
  buffers, no retiring generation. `sighurt-engine-servo/tests/headless.sh` (E1): `engine.rs`,
  `crash.rs` and `unavailable_context.rs` pass. `sighurt-engine-wpe/tests/headless.sh` (GLES2):
  passes. `window-checks.sh`: Servo's 21 of 21, WPE's 60 of 60. `wpe-coexist`'s
  `coexist-checks.sh`: 14 of 14. `experiments/m1-mixed/headless-checks.sh`: 15 of 17 (below).*
- [x] Unit tests. *`cargo test --workspace` without Servo, WPE and `sig` (api 31 + 1, core 55 + 71,
  wayland 48 (+ 17 ignored, run above), ui 36 + 10 + 26 + 13 + 1, the line chrome 7, the modal
  keymap 13, demo 3, Wasm 141 + 30 + 5, SDK 16, `fullstack-notes`' server 8): 515 passed, 26
  ignored (the compositor tests above, two measurements and the SDK's doc examples). `-p sighurt-engine-servo
  --lib` 28, `-p sighurt-engine-wpe --lib` 14. `-p sighurt-browser`: 31 with `engine-demo`, 32 with
  `engine-wpe`, 33 with `engine-demo,engine-wasm,keymap-modal,chrome-line`, 35 with its default
  features, 35 with all features.*
- [x] Clippy `-D warnings` on all targets. *The workspace without Servo, `sig` and the experiments
  that link Servo (WPE's adapter included); `sighurt-engine-servo`, `m0-coexist`, `m1-mixed` and
  `wpe-coexist`; `sig` with no feature, with each engine alone, with `engine-servo,engine-wasm`,
  with `engine-demo,engine-wasm,keymap-modal,chrome-line` (CI's set), with
  `engine-servo,engine-wasm,engine-demo,keymap-modal,chrome-line` (`servo.yml`'s), with its default
  features and with all features. The SDK and the example guests build for `wasm32`.*
- [ ] rustfmt. *`cargo fmt --all --check` fails on one file, `experiments/wpe-coexist/src/main.rs`
  (`use sighurt_engine_wpe::…` out of order; the WPE track's file, not changed here); every other
  file is formatted. Since ("Review fixes"): formatted, and `cargo fmt --all --check` passes.*
- [x] A configuration showcase, `engines.sh` `showcase` (33 checks, screenshots
  `showcase/01`–`08` looked at). With `[input] keymap = "modal"`, `[chrome.standard] tabs =
  "left"`, `[ui] font-size = 16`, `[routing.wpe] hosts = ["localhost"]` and a `[keywords]`
  template, and no `[keys]`: the file is valid (no notice); a page on localhost opens in WebKit and
  one on 127.0.0.1 in Servo; the tab list is a 251 × 727 surface beside a 1029 × 727 page under a
  50 px toolbar and above a 23 px strip (16 px: 220, 44 and 20 times 16 / 14); `g t` and `g T`
  switch tabs; a click on WebKit's field puts the keymap in insert mode ("INSERT" in the address
  field) and "åäö" reaches the field, Escape returns to normal mode and a `z` is swallowed; `o` and
  "t åäö" open the keyword's template in Servo; a typed localhost address opens in WPE; `:` with
  "tab.select 2" runs; `b` (`chrome.toggle-tabs`) hides and shows the list, the page taking its
  room. Saved while `sig` runs, a new binding (`n`, `tab.new`) works at once where it did nothing
  before, and a new `tab` color fills the active row (0 pixels of #7c3aed before, 8 478 after); a
  new `[ui] chrome = "line"` gets a notice and the standard chrome stays. Started again with the
  restored tabs: the line chrome alone (1280 × 32 at 16 px), the page 1280 × 768, the mode in the
  line, `g T`, a typed localhost address in WPE and `:tab.select 1` through it.
- [x] **R2**, live on the user's River 0.4.8 (gharial) desktop, RTX 3070: three runs of `sig` of
  about 7 s each, Servo's, WPE's and the Wasm engine's page in the shown tab and the other two in
  background tabs, its own configuration, profile and D-Bus session, no input, no screenshot;
  SIGTERM, exit 0 each. *Renderers: `engine=servo mode=hardware gl="NVIDIA GeForce RTX
  3070/PCIe/SSE2"`, `engine=wpe mode=hardware gl="WebKit, DMA-BUF on /dev/dri/renderD128
  (nvidia)"`, `engine=wasm mode=cpu`. Copies and readbacks: `page_readbacks` 0, `readback_bytes` 0,
  Servo's GL reads 0 (audited), WebKit's copies 0, every frame line `readback_bytes=0` (3, 2 and 1
  frames at 1542 × 1304, scale 1). Idle, 3.3 s after each page settled: 1 wakeup and 1 iteration
  (the report's own), 0 pumps, 0 frames, 0 chrome paints, in all three runs. First frame after
  navigating: Servo 394 ms, WPE 362 ms, Wasm 195 ms. The chrome presented (wp_presentation: 7, 6
  and 4 frames of the toolbar). The host found every protocol of its list (viewporter, fractional
  scale, single-pixel buffer, text input, clipboard, primary selection, cursor shape, presentation
  time, decorations).*
- [x] Documents. *README (what `sig` is, building with and without WPE, the features, the
  configuration reference, routing and links, the command table with `page.open-in`,
  `page.send-key` and `chrome.<action>`, keys and the modal keymap, the chromes, writing a keymap or
  a chrome), DOCS (the in-process Wasm engine, routing for apps, limits), SECURITY (in-process
  engines are no sandbox; what pages get; WebKit's own process sandbox), CONTRIBUTING (checks with
  and without Servo and WPE, the end-to-end scripts, adding an engine), ROADMAP, CLAUDE.md (WPE,
  keymaps and chromes, commands, the hand-off, louise-pc's Rust, the pre-commit commands), the
  built-in file's comments (the modal keymap, the line chrome, file locations), `ci.yml`,
  `servo.yml`, and `release.yml` removed (decision 81). Checked: the README's four configuration
  examples and DOCS's `[routing.wasm]` example, each started in `sig` in headless sway, are valid
  (no notice, no problem in the log); the README's keymap example, built in a crate of its own
  against `sighurt-api`, passes clippy and a test (Ctrl+N runs `tab.next`, a bad command and a
  missing key are its kind's problems); `RUST_LOG=sighurt::wasm::console=debug` shows
  hello-sighurt's console line as DOCS says.*

Found, not fixed (other tracks' files): `experiments/m1-mixed/headless-checks.sh` fails 2 of 17
(scratchpad `final/comp/m1/m1.log`): the key held across the blur goes up as "Unidentified", where
the R1–R4 runs (on louise-pc's own sway, before decision 79 pinned it) saw its own value. Not
investigated: the experiment, M1's frozen proof and not `sig`, translates a release from its own
keysym (`page.rs`). With `[engines.servo] storage = "ephemeral"`, Servo logs "Failed to initialize
ClientStorage engine … unable to open database file" in a temporary directory (`engines.sh`
`settings` and the README's example), so its client storage (IndexedDB and the like) is likely
unavailable in that mode: for the Servo track. Wasm console lines appear twice in the log.

Open: `cargo fmt` on `wpe-coexist`; `sighurt-wayland/tests/headless.sh`,
`sighurt-engine-servo/tests/window-checks.sh` and `experiments/m1-mixed/live-check.sh` still take
wl-clipboard (and lswt, grim) from `<nixpkgs>`; `extension-api.md` still calls itself a proposal
with nothing implemented; `shell.nix`'s header still describes GPUI and `sighurt-ui`'s `sig`, and
NOTICE doesn't mention WPE WebKit; `sighurt-engine-servo/README.md`'s remote build command needs
louise-pc's Rust (CLAUDE.md has the working one); nothing has run on louise-pc this pass; the
River run had no input, resize or scale change. *Since ("Review fixes"): `wpe-coexist` is
formatted, `extension-api.md` records the implementation, `shell.nix`'s header describes it and
it brings the pinned Rust, so the Servo README's remote command works as written.*

### Review fixes: the wave's three reviews applied

The correctness, security and performance reviews of this wave gave 15 findings; each was
checked against the code and the plan before it was acted on, and each fix below has its test or
measurement. Decisions 82–86. Everything ran on this desktop (RTX 3070, headless sway 1.12 from
the pin, GLES2 unless named) on 2 October 2026 with the working tree after the fixes; scratchpad
runs `runs/…`, `cache/…`, `idle/…` and `bench/…`. louise-pc didn't answer over SSH during this
pass, so nothing ran there.

- [x] **rustfmt**: `experiments/wpe-coexist/src/main.rs` formatted. *`cargo fmt --all --check`
  exits 0.*
- [x] **A tab's error on a `sig:` page is shown** (`App::status` hid it unless an engine view
  showed). *`headless.sh` `settings`: `:page.open-in demo` on `sig://home` puts "sig: pages are
  Sighurt's own and open in no engine" in the status strip (diagnostics `status == "error"`;
  screenshot `settings/02-open-in-on-home` looked at); with the old filter put back the check
  fails (`[3, null, true]`).*
- [x] **Routing rules of an engine the build lacks are inert; `[engines] default` must be in the
  build** (decision 82 (3)). *config `an_engine_the_build_lacks_opens_nothing` (the rules are
  checked, conflicts too, and left out; `default = "wpe"` is one problem naming the build's
  engines); `tests/browser.rs` `an_engine_the_build_lacks_takes_neither_typed_addresses_nor_links`
  (a command-line URL, a typed address and a clicked link on a host `[routing.wpe]` claims open
  in Servo with no error; `page.open-in wpe` says the engine isn't in the build).*
- [x] **The toolchain is pinned** (decision 86): `shell.nix` brings the pin's `rustc`, `cargo`,
  `clippy` and `rustfmt` 1.98.1, the desktop's own store paths, and the `PATH` workarounds are
  gone (CLAUDE.md; `bench/rewrite/build.sh` keeps one for revisions whose `shell.nix` predates
  it). *The shell's environment before and after differs only in `PATH`, `nativeBuildInputs`,
  `NIXPKGS_CMAKE_PREFIX_PATH` and what follows from the shell's own store path (`out`, the
  random seed in `NIX_CFLAGS_COMPILE`, the rpath in `NIX_LDFLAGS`); `cargo build -v -p
  sighurt-browser` (default features, Servo) in the new shell: 937 units `Fresh`, only the 9
  Sighurt crates whose sources changed compiled. Not run on louise-pc (unreachable).*
- [x] **extension-api.md describes what was built**: status "implemented", §1.4, §2.2, §2.5 and
  §2.6 brought in line with the code, and a changelog entry for every departure (decisions 67,
  68, 74–77 and 82). M4's stale open item (a way to hand over links, done by step 13) removed.
- [x] **A Wasm guest's downloads go through the browser** (decision 83): `api_download_data`,
  `api_download_url` and `api_canvas_print_pdf` queue `Service::Download` requests (at most 4 or
  64 MiB waiting, none from a worker) that the view hands to the core, which throttles them, and
  `sig` saves them in `[downloads] directory` under a cleaned name. The engine writes no file
  and starts no thread. *`runtime::downloads_are_the_browsers_to_make` (a guest saving to
  `/tmp/…`, `../escape.txt`, a URL ending in `..%2F.zshenv` and a PDF: four requests for the
  document with the names as given, a fifth refused, no file written); `engines.sh` `wasm`: a
  guest saving "hello" as `/tmp/sighurt-e2e-escape.txt` gets `sighurt-e2e-escape.txt` in the
  run's download folder and nothing in `/tmp`. Broken on purpose: no request handed over, no
  queue bound; each failed the test.*
- [x] **The download folder is never the home folder, and names are cleaned** (decision 83):
  `~/Downloads` when the desktop names no folder; `sanitize` drops Unicode format characters
  (bidi, zero-width, tags) and trims dots and whitespace from both ends. *`services::tests::
  names_are_never_dot_files_or_hide_characters` (`.zshenv`, `a/../.profile`, an absolute path,
  `..`, a right-to-left override, zero-width characters, a tag character, `..%2F.zshenv` from a
  URL), `downloads_go_to_the_configured_folder_made_when_first_needed` (not the home folder);
  each broken on purpose failed its test. Not done: activation for page downloads, which would
  refuse a download the user typed the address of (that load has no page activation); each
  download already shows its own status line ("Saved …").*
- [x] **WPE leaves a page that asks to stay** (decision 84). *`tests/engine.rs`: on a page whose
  `beforeunload` asks to stay, after a click in it, the browser's `Navigate` commits; with the
  handler made to fall through to WebKit's default, the test fails at that assertion.*
- [x] **A page can't cancel or replace the user's switch to another engine** (decision 82 (2)).
  *`tests/browser.rs` `a_page_cannot_cancel_or_replace_the_users_switch`: while the user's typed
  address loads in another engine, the old page's `OpenRequested(CurrentView)` (the review's case
  A) and its request for a link the user had clicked (case B, denied) close and open nothing,
  and the user's page is shown when it commits; without the guard the test fails. Left as it
  was: a page's `OpenRequested(CurrentView)` that routing can't place, with no switch pending,
  sets the tab's error, which the status strip shows over the page the tab keeps.*
- [x] **Only the link the user pressed moves the tab** (decision 82 (1)). *`tests/browser.rs`
  `only_the_link_the_user_clicked_leaves_the_engine` (a click on another link or on no link, and
  a key press, let no frame or script take the tab elsewhere); the hand-off tests now click a
  link (`click_link`), and the same address written differently (`HTTPS://X.Apps.Test:443/`)
  hands off. Broken on purpose (no URL match; a key press keeping the link; a fixed link recorded
  at every press): each failed a test. `engines.sh` `links` and `wpe` pass with real clicks over
  real links in Servo and WebKit (the pointer rests 0.3 s before the press, so the status text is
  the link's).*
- [x] **No WebKit web process outlives the last WPE page** (decision 84). *`wpe-window` over
  HTTP: before, a 99 MiB web process stayed 20 s after both views closed (WebKit's process cache;
  setting the cache model on the default context changed nothing, since the cache is sized when
  a context is made); with a context of the views' own and the document-browser model, each web
  process ended within about a second of its view; after a cross-site navigation WebKit also
  kept a 65 MiB prewarmed process, which ends with the context, now made with the first view and
  ended with the last (`cache/nav2`: none left). `engines.sh` `wpe`: after every WPE tab is
  closed, 0 web processes are left; the network process (31 MiB) stays with the runtime.*
- [x] **Retained views by cost** (decision 85): a hidden view of an engine with a process per
  view (`EngineCapabilities::process_per_view`, WPE) counts as both of the tab's retained views.
  *`tests/browser.rs` `a_view_with_a_process_of_its_own_is_kept_alone` (Servo, WPE, Servo: the
  first Servo view becomes a record, back shows WebKit's page at once and back again loads the
  record); broken on purpose, it fails. `engines.sh` `wpe`: in a tab of Servo, Wasm and WPE
  pages, a link from WebKit to a Servo page leaves 2 live views (the WPE page's and the new one),
  back shows the kept WPE page without loading it again. The review's `cross-retain` scenario
  keeps 2 live views instead of 3; by its own figures the view saved is about 28 MiB (453.7 −
  248.2 − 177.1), while the hidden web process (177 MiB) stays for an instant back.*
- [x] **WebKit isn't linked** (decision 84): the engine opens `libWPEWebKit-2.0.so.1` by path
  when its runtime first starts. *`sig` (default features) needs 21 libraries and maps 110 at
  start, where the review counted 185; a build without the engine: 18 and 104. `LD_DEBUG=
  statistics sig --version`: 27 461 relocations and 4.1–4.7 M cycles in the loader, 25 135 and
  3.7–3.8 M without the engine (the review: 81 999 and 15–16 M). `bench/run` on this desktop,
  dev builds of today's `sig` with and without the engine (both with the modal keymap and the line
  chrome), interleaved, 5 rounds after a warm-up, workloads `article`, `home` and `many-tabs`
  (`bench/results/sighurt/*/20261002-190653`): launch to page on screen 0.152 against 0.153 s
  (article) and memory 197 against 198 MiB PSS (article), 408 against 409 (eight tabs), all noise;
  `home` 74.9 against 75.8 MiB (+0.9 MiB, p = 0.008, q = 0.34: GLib's libraries). The review
  measured WebKit's linked libraries at 29.8 MiB PSS of such a session. So M7's figures for
  3c510fe, which had no WPE, stand for today's default build to about a MiB; release builds on
  louise-pc were not re-measured (it didn't answer).*
- [x] **Idle measured across the process tree** (`probe.py switches`). *`engines.sh` `wpe`: an
  idle shown WPE page, 4.7 context switches a second over `sig` and all its descendants (≤ 10
  checked; two runs); a hidden tab that animated, 145.6 and 149.4 a second, 131.2 and 121.8 of
  them one `SkiaGPUWorker` thread of its web process (≤ 300 checked, so a hidden page that
  rendered would fail). The source,
  looked for in `wpe-window`: one thread of the hidden page's web process (there WebKit's
  compositor thread) waking about 120 times a second from `poll` with a timeout, with almost no
  CPU (`perf`: about 13 µs sampled in 3 s); WebKit's own (finishing the adapter's pending frame
  at hide changed nothing), so not fixed. The WPE M1 and "In `sig`" idle claims now say they
  count the owner loop only.*
- [x] **The GLib watch thread** (decision 84): a new set wakes a thread parked on the condition
  variable by the notification alone, and the interrupt is drained only when it fired. *Animated
  WebGL page in `wpe-window`, 3 × 4 s each: the watch thread's reads 235 → 5–37 a second and the
  owner's writes 413 → 292–302 (the interrupt writes gone); context switches unchanged (watch
  thread 197–241 before, 196–236 after): an interrupted `poll` that returns at once doesn't
  block, so the review's premise (extra context switches) didn't hold; the syscalls did.
  `glib_loop::tests` passes.*

Tests: `cargo test --workspace` without Servo, WPE and `sig`: 521 passed, 26 ignored (core 56 +
75, the Wasm runtime 31); `-p sighurt-engine-servo -p sighurt-engine-wpe --lib` 28 + 14; `-p
sighurt-browser` 34 with `engine-demo,engine-wasm,keymap-modal,chrome-line`, 36 with all
features. Clippy `-D warnings` on all targets: the workspace without Servo and WPE; `sig` with
CI's set; `sighurt-engine-servo`, `sighurt-engine-wpe`, `sighurt-browser`, `m0-coexist`,
`m1-mixed` and `wpe-coexist`; `sig` with all features, none, each engine alone, `engine-servo,
engine-wasm` and `servo.yml`'s set. rustfmt. End to end: `sighurt-browser/tests/headless.sh` 75 of 75 (main 29, config 9, close 4,
disconnect 3, modal 13, settings 17; run `runs/headless`); `engines.sh` 255 of 255 (servo 63, wasm
30, cross 33, settings 17, links 16, wpe 63, showcase 33; run `runs/engines-final`, screenshots
`settings/02-open-in-on-home` of the first and `wpe/06-from-servo-to-wpe` looked at); the WPE
adapter's `tests/headless.sh` (GLES2) passes, and its `window-checks.sh` passes all 60 checks with
GLES2, with GLES2 and `--shm`, and with pixman; `wpe-coexist`'s `coexist-checks.sh` 14 of 14 (no
WebKit symbol binds into the executable, every engine presents, the traps). Fourteen guarantees
broken on purpose, one at a time, each failed the test meant for it (`breaks/`): the clicked-link
match, links recorded at key presses, the link of the press replaced by a fixed one, the switch
guard, the retention cost, inert rules, the default-engine check, the status strip's error, the
beforeunload answer, sanitize's trimming and format characters, the home-folder fallback, the
Wasm view handing over downloads and its queue bound; the redundant clearing of the link after a
hand-off was the one that failed nothing, and was removed.


### Terminal frontend (TUI.md, decisions 89–92)

The phases of [TUI.md](TUI.md) §8, each passing only with the evidence named.

- [x] **Phase 0: the risks.** Servo 0.5 renders offscreen with no display server on NVIDIA,
  WPE WebKit renders to shared memory with no compositor, and the terminal I/O prototype passes
  under a pty (TUI.md §9; `experiments/tui-servo`, `wpe-cpu`, `tty-io` and their RESULTS.md).
- [x] **Phase 1: the split.** `sig` is `sighurt-runtime`, `sighurt-gui` and a thin `main` with no
  behaviour change; the runtime owns shutdown (§20, decision 54 behind
  `Frontend::before_forced_exit`), presenting with its diagnostics, and the engine descriptors
  (b04ebf4, 8e8d576; an independent review found the diagnostics, effect order and shutdown
  order unchanged; engines.sh 271/271 on louise-pc, headless.sh 75/75, sighurt-ui's headless.sh
  35/35, clippy clean with no features and with all).
- [x] **Phase 2 (reordered, decision 97): `sig --tui` in graphics mode with Servo, Wasm and the
  demo engine** (07f7e36): `sighurt-browser/tests/tui.rs`'s two end-to-end tests of `sig --tui`
  in a pty (image sized to the page rows, address line, resize, quit and SIGTERM restoring the
  terminal, ≤ fps frames), sighurt-tui's 36 unit and 13 pty tests, and example.com through Servo
  decoded from the kitty stream on the RTX 3070 with no display server.
- [ ] **Phase 3: WPE's CPU presenter** (Servo's is done: 79be7f7), sixel; text mode and `--dump`.
- [ ] **Phase 4: semantics** in Servo and WPE; text mode and `--dump` with real pages.
- [ ] **Phase 5:** the Wasm engine in the terminal, mouse, the stdin pager, the documents.

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
45. **The Wasm engine mounts on the Wayland host itself, paints in bands, and never lets a
    waiting guest hold a worker** (amends 22 and 23). `sighurt-engine-wasm` depends on
    `sighurt-wayland` (§5 allows it) and implements `CpuTarget` for `TargetLease`, with
    `FramePermit` as the permit: `present` checks the permit's target, takes the `ShmFrame`,
    paints and submits it, so `sig` uses `WasmRuntime<TargetLease>` with no adapter of its own.
    A frame of over 256 Ki pixels is painted in up to 8 horizontal bands (at most one per core)
    at once, with `std::thread::scope`: the owner paints one band, helper threads the others,
    each straight into its rows of the lent buffer. Which ops fit the budget is decided once for
    the whole frame before the bands start (`raster::select`, a pass over the list that paints
    nothing), so every band paints its part of the same ops whatever the number of bands, and
    the bound stays `MAX_OVERDRAW` pages for the frame
    (`raster::tests::the_budget_picks_the_same_ops_for_any_number_of_bands`). A band whose
    thread can't be started is painted on the owner instead. Text is laid out, and its glyph
    images found, under the painting lock, and blended outside it, so bands blend text at the
    same time. No copy and no `unsafe`; the owner still waits for
    the slowest band, because `ShmFrame` is not `Send` (it holds the permit's `Rc`), so painting
    after `present` returns would need the host to split a `Send` pixel handle from the permit
    (a `sighurt-wayland` change, proposed, not made). Anti-aliased edge pixels may differ by
    rounding between band and whole-frame painting (`raster::tests::bands_paint_what_one_piece_does`).
    Laid-out lines of up to 1 KiB are cached for painting, at most 4096 lines and 64 Ki glyphs
    (80 bytes each, about 5 MiB; `text::tests::laid_out_lines_kept_are_bounded`), an opaque
    `Clear` is a plain fill, and a list that starts with one skips clearing the buffer first.
    Host functions that wait (`Budget::wait`) and a load's fetches run in `pool::blocking`: a
    spare thread takes the waiting job's place in its pool, at most 16 spares per pool, and a
    spare ends when the waiter is back. The compiled-module cache stays at format `v2` (the bump
    for epoch interruption was made with it); an artifact of another `Config` found there is
    refused by Wasmtime and recompiled (`engine::tests::aot_cache_entries_of_another_config_are_recompiled`).
    *Why:* M5's glue; §19 (bounded owner work, no worker starved by blocking calls) and §15;
    measured in the M5 section.
46. **`sig`'s owner loop and effects** (M3). `FrameReady` is scheduled by `sig` (it invalidates
    the view's target; the core never sees it) and a page frame is presented only on a host
    permit, so page animation reaches neither the core nor the chrome. Engine pump deadlines and
    the session's save delay are the loop's wait timeout rather than calloop timers; a runtime
    is pumped when it was called, its waker fired or its deadline passed, and steps 1–6 of §19
    repeat at most 8 times before the loop goes round without sleeping. The status strip shows
    the page's status text first, then the reason a navigation failed, then `sig`'s own latest
    notice (configuration errors, downloads, refused prompts). Permission prompts have no user
    interface yet and are answered "no" with a notice (fail closed). The demo engine is reached
    only through `[routing.demo] schemes = ["demo"]` in the configuration, not by default, and
    an Alt shortcut nobody bound types nothing into it, as in the address line. *Why:* §19's
    iteration, §26's "zero chrome repaints caused solely by page animation" and §17's fail-closed
    rule; measured in the M3 integration run.
47. **`sig` review fixes (amends 46).** (1) The status strip shows a notice of `sig`'s own
    (configuration errors, downloads, refused prompts) before the page's status text for 5 s
    after it appears, then after it; the page's text and the navigation error keep their order.
    A page could otherwise hide the browser's own messages (§17, reproduced in the review); the
    5 s are a one-shot loop deadline, not a timer. (2) Page readbacks are the engines' own
    counts (Servo's `read_to_image`, and its GL reads when auditing), taken after each frame an
    engine submits and at each report: `page_readbacks` counts them and `readback_bytes` counts
    each as a whole buffer of the frame it came with (of the page area, for one found at a
    report), so a readback anywhere in Servo reaches bench/'s readback metric; `sig` itself has
    no readback path. (3) The `renderer` line is printed at an engine's first frame, when Servo
    knows its GL renderer: `mode=hardware|software gl="…"`, or `mode=cpu`. (4) `first-frame`
    measures a navigation started while its view's tab is active, and is dropped when another
    tab becomes active, so background tabs don't report their switch time. (5) `sig` keeps the
    peak SHM buffers per target, retiring generations and total bytes, sampled after every
    presentation; the diagnostics JSON also says which tab is active, whose tab each view is and
    where the status strip's text comes from (never the text). *Why:* the independent review of
    the M3 integration. (1), (4) and (5) are asserted in `sighurt-browser/tests/headless.sh`;
    (2) has a unit test of the accounting and (3) is checked for the demo engine only: both are
    unverified for Servo until `sig` is built with it.
48. **M0 review fixes (amends 26).** (1) A script-thread panic fails **every view with a
    document on that script thread** (same-site pages in one browsing-context group), each with
    its own `notify_crashed`, not only the page that panicked: the constellation fails all
    webviews with a pipeline on the panicking event loop
    (`servo-constellation-0.5.0/constellation.rs:3022–3030`). `m0-coexist` step 9 panics a
    script thread with `ServoTestUtils.panic()` and shows that view crashed and a view on
    another script thread unaffected; that several views on one thread all fail is read from
    the source. (2) "Nothing changes the environment after threads start" is `sig`'s own rule;
    surfman, inside Servo, still sets `LIBGL_ALWAYS_SOFTWARE` and `DRI_PRIME` whenever it
    creates a GL context (M0.md, open problem 2). (3) OpenSSL, which only the Wasm engine's
    WebSocket client brings in (tokio-tungstenite with `native-tls`), needs no setup from `sig`:
    it initializes with `OPENSSL_INIT_NO_ATEXIT` and changes no signal disposition or
    environment variable (step 3, which links and runs the whole Wasm engine). (4) The
    baseline measurement is `bench/results/BASELINE.md`. *Why:* the independent review of M0.
49. **Benchmark harness review fixes; the baseline measured again.** Input comes from one
    virtual pointer and one virtual keyboard for the whole run (`bench/benchlib/seat.py`, with a
    checked-in US keymap), because `wtype` added and removed the seat's only keyboard per call
    and keys were lost. A run records how `sig` ended and scans stderr for crash lines; either is
    an anomaly, which `bench/report` lists and `bench/compare` counts (`--exclude-anomalous`
    leaves those runs out). Chrome-input latency is `input_to_pixels`, from screenshots of the
    chrome, because GPUI logs only the draws of its frame callback, so its chrome
    `input_to_screen` was timing the keyboard's coming and going. Build definitions declare
    where engines run (`engines`), and a comparison across placements leaves out the
    `*_browser`, `*_engines` and `ipc_out_*` metrics, which compare process layouts there.
    Build definitions are no longer in the harness hash; a set resolves its build directory once
    and stops if the binaries change. The rewrite's `sig` is measured through
    `bench/builds/rewrite.toml` (configuration version 1), `bench/rewrite/build.sh` (a `git
    archive` of a revision built on `louise-pc` into a directory never written again) and the
    `sig-diag` diagnostics format (`bench/benchlib/parsers.py`: `SIG_DIAG=1` lines stamped with
    CLOCK_MONOTONIC, and cumulative counters printed on SIGUSR1). Two workloads were added,
    `wasm-flood` and `wasm-workers` (§26's output limits and worker fairness), and
    `bench/build-cost` records build cost. The baseline was measured again with all of it
    (`bench/results/louise/baseline/20260927-083430/`). *Why:* the independent review of the
    benchmark track; a headless-sway probe showed the seat's keyboard capability disappearing
    between `wtype` calls, and screenshots showed every key of the new keyboard handled while
    GPUI logged none of the redraws.
50. **M1 review fixes.** In `experiments/m1-mixed`: (1) `ShmBuffers` refuses a new size, keeping
    the invalidation, while more than one generation's worth of old-size buffers await release
    (§10's bound on retiring generations; the root keeps its old buffer and buffer scale
    meanwhile); (2) the ordered release (Servo, context, `eglTerminate`, then the surfaces) also
    runs from `App`'s `Drop`, so an error or panic doesn't fall back on field-drop order (§20;
    R7); (3) key-ups synthesized on blur carry the key value of their press; (4) the root's
    opaque region follows every resize; (5) the report's storage is bounded (counters, the last
    8 192 durations, the idle window's wakeups); (6) `headless-checks.sh` judges its own log and
    adds an odd-size page at scale 2 and a scale change while hidden; (7) run logs are kept in
    `docs/rewrite/m1-runs/`, because the earlier ones on tmpfs were lost. A scale change's
    intermediate layout (Servo 0.5 sends size and device pixel ratio in two messages) is
    recorded in M1.md for M4 to decide. *Why:* the independent review of M1; everything it
    listed was checked against the code, the plan and Servo 0.5's source and held.
51. **The Servo adapter presents each view on its own GL context and paces it by permits**
    (M4). A view's `WindowRenderingContext` is made from its lease's `gl_handles` once the view
    has room, with swap interval 0 (decision 32); the WebView is made at its first navigation
    (Servo loads a WebView's first URL as it builds it; building one earlier would put
    `about:blank` in its history). `FrameReady` is reported when Servo has a frame, the view needs
    one at a new epoch or after being shown, or Servo's refresh driver waits for a frame; a permit
    means the last swap's callback has come, so `present` starts the waiting frame, spins Servo
    once and paints and swaps at the permit's buffer size (`Unchanged` when nothing is new). A
    hidden view is throttled and gives its EGL window surface back (`take_window`); shown, it
    makes a new one (`set_window`) and presents at the next permit. The EGL display is one per
    `wl_display`, shared by every view and Servo's WebGL thread, so it is terminated once, at
    shutdown, after Servo and every context, while the host still exists, not per lease as
    `TargetLease::gl_handles`'s safety note says; that note assumes one display per lease.
    *Why:* §9, §10, §14, M1's sequences; W1 and E1 in the M4 section.
52. **Servo navigation and document identity** (amends 12's attribution rule for Servo).
    Servo's history report is the truth; each report is a commit of something. It is attributed
    to the latest pending browser request that fits its shape (`Navigate`: a new entry and new
    document; `Reload`: the same entry, new document; `Traverse`: a move by exactly its delta),
    else it gets an adapter id. A new document is known from `LoadStatus::Started` (which the new
    top-level document's script thread sends before it asks to be activated), from the browser's
    `Navigate`/`Reload`, and for traversals from a document key kept per entry; no URL is
    compared. A document shown again gets a new `DocumentGeneration`. A commit that changes
    neither document nor address and wasn't asked for (an iframe's) is `HistoryChanged` only.
    Servo 0.5 drops a `WebView::load` while a load is in flight, so a navigation asked for then
    waits for that load's document (a newer one cancels it). Network errors commit Servo's error
    page (a document); only an address that isn't one fails a navigation. Stop is
    `window.stop()`. Servo 0.5's history projection names non-current entries after a
    `pushState` with the later URL; it is reported as it comes. *Why:* §16, decisions 12 and 41;
    `navigation::tests`, E1.
53. **Servo's requests go through the service API or are refused** (M4). Clipboard reads and
    writes (text), permissions (camera, microphone, geolocation; everything else Servo asks is
    denied) and file pickers are `ServiceRequested`, answered through Servo's request objects;
    at most 16 outstanding per view and only for a committed document, the rest refused on the
    spot. Servo's own clipboard (arboard) is never used. A page's navigation to a scheme Servo
    doesn't load (anything but http, https, data, blob, about, javascript), or to `file:` from a
    non-file page, is denied and handed to the browser as `OpenRequested(CurrentView)`. A new
    browsing context (`window.open`, `target=_blank`) is a hidden WebView on the page's context
    until its first navigation, which becomes `OpenRequested(NewView)` (at most 4 waiting per
    view). `<select>`, colour pickers, simple dialogs, context menus and HTTP authentication stay
    resolved closed (decision 16); Servo 0.5 has no downloads. Console messages are page data
    and are not logged. *Why:* §12, §17; W1's clipboard checks.
54. **Servo failures and shutdown** (amends 26). A page crash is `ViewFailed`; the view's next
    navigation or reload loads in a new WebView on the same context, since Servo 0.5 keeps
    sending the crashed site's documents to the dead script thread ("crash page crashed"). A
    view whose context can't be made is `ViewFailed` then `ViewClosed`. Backend loss is one
    `RuntimeFailed`. After either a page crash or backend loss Servo 0.5 never finishes shutting
    down: `Constellation::run` loops until every pipeline has exited, and the dead thread's
    never do (seen in E1: `Drop for ServoInner` spun for 10 minutes). `shutdown` then leaves
    Servo, its contexts and leases running (`ServoDiagnostics::leaked`); the profile isn't
    written, and the executable should end the process with `_exit` after saving its own state,
    since exit handlers would tear down GL state Servo still uses (the crash test's process
    segfaulted at a normal exit). The profile lock is tried once: another `sig` holding it means
    no profile. *Why:* §14, §20; `crash.rs`, M0 open problem 5.
55. **Accepted Servo 0.5 limits and API needs found by M4.** A scale change may lay out one
    intermediate state (size and device pixel ratio are two messages, M1.md); accepted, the
    settled layout follows. `NavigationCommitted` could say what a commit did to the history
    (Servo's history shape tells it; decision 41) and `TextInputState` could carry a field
    identity (Servo's input-method requests have ids; decision 44); both are API changes to
    types the core and host use, left for those tracks. Servo can't know which links the core
    routes to another engine (a Wasm app on a configured origin), so it loads them itself; that
    needs an API concept. Servo's input-method request comes only when a field is focused, so
    surrounding text goes stale, `delete_surrounding` has no Servo counterpart, and a
    composition cancelled without text leaves its preedit. A page navigation from a document
    still `Started` (no `<body>` yet) is taken for a same-document commit. *Why:* evidence in
    the M4 section; kept small per §23's "extend the API only where the second engine shows a
    missing concept".
56. **`sig` review fixes, second pass (amends 47 and 54).** The review's must-fix and should-fix
    findings had been applied in decision 47; this pass checked each against the code and the
    end-to-end suite and fixed what was left. (1) When an engine's `shutdown` had to leave it
    running (`ServoDiagnostics::leaked`, decision 54), at quit or after `RuntimeFailed`, `sig`
    ends with `_exit` once the session is written and the downloads have ended or been
    deleted, without closing the window or dropping the host: Servo's threads still use GL
    state on that connection, and the compositor cleans up after it. (2) Quitting cancels the
    downloads first (§20: stop creating work), and after the 500 ms grace deletes the file of
    one still waiting on its server, which the cancel flag can't reach in the middle of a read.
    (3) Notices are no longer logged, since they hold origins, URLs and file names (page data);
    configuration errors are logged as warnings where they are found. (4) The diagnostics'
    `view=` is `ViewId::serial()`, a new accessor on the api's ids, not a trimmed `Debug`
    string. (5) The demo engine draws its crosshairs where `Metrics::to_buffer` puts the
    pointer. (6) `servo.yml` also runs clippy on `sig` with its default features, so CI compiles
    the registry's Servo arm. (7) A failing event loop or configuration watch ends `sig` through
    the same ordered shutdown as a quit, rather than by dropping `App` field by field (host
    first). *Why:* the independent review of M3 and the Servo wiring left open in M4; §20, §26
    and CLAUDE.md's rule that diagnostics never log page data.
57. **Servo adapter review fixes (amends 52, 53 and 55; settles the field decision 41 left
    open).** Each finding of the independent M4 review was checked against Servo 0.5's sources.
    (1) **Traversals are attributed exactly.** `go_back`/`go_forward` return a `TraversalId`,
    and the constellation sends a traversal's history change and its
    `notify_traversal_complete` back to back (servo-constellation 0.5: `TraverseHistory` →
    `handle_traverse_history_msg`, then `HistoryTraversalComplete`), so the history change right
    before a traversal's completion is that traversal's. When the completion hasn't arrived with
    the change, the WebView's notes wait, in order, for Servo's next batch. A completion with no
    change before it (Servo found no entry) is `Cancelled`. History shapes now attribute only
    `Navigate` and `Reload`; the page's own `history.back()` gets an id of its own.
    (2) **A load that Servo may have dropped is waited for at most 5 s.** Servo 0.5 drops a
    `WebView::load` while the browsing context has a pending change, and a 204 or 205 response
    aborts a navigation without telling the embedder (`AbortLoadUrl`). Before this fix, every
    later navigation stayed queued until a reload. A view now waits for a top-level load
    (`view::Load`) only `LOAD_WAIT` = 5 s after the load was asked for or after its document
    reported `Started`. The load may be the browser's, or one the page started and was allowed;
    `NavigationRequest` doesn't say whether that one is an iframe's. After the wait, the queued
    navigation is sent at the pump's deadline, and the abandoned browser navigation is reported
    `Cancelled` and forgotten (a late commit of it counts as the page's own). 5 s is long enough
    for a slow server's response to keep its navigation's identity, and short enough to bound
    the wait after a 204 or an iframe's navigation. Left: if a response takes longer than 5 s and
    the browser navigates again, Servo drops the new navigation and the late commit carries its
    id; a navigation asked for within 5 s of an iframe navigating itself waits up to 5 s.
    (3) **Stop** cancels a queued navigation. It gives up a load that has shown no sign of life
    for 5 s, which is the way out of a 204. It stops the document's loading through
    `Window.prototype.stop.call(window)`, which a page can't replace by redefining
    `window.stop`. Servo 0.5 can't cancel a top-level navigation that is waiting for its response:
    no embedder message does it, and `window.stop()` only aborts the document's own fetches
    (servo-script 0.5 `Window::stop_loading`). That navigation still commits, with its id.
    (4) **`NavigationCommitted.history: Option<HistoryEffect>`** (`Push`, `Replace`,
    `Traverse`; an additive api change). Servo fills it from the history's shape. The core uses
    it for commits the page started: a `Push` discards later segments, `Replace` and `Traverse`
    don't. So the page's own back into another document, and its own reload, keep the forward
    segments, and `pushState` now discards them, since it is a new entry. The Wasm and demo
    engines report `None`, which keeps decision 41's rule for them.
    (5) A pop-up's `OpenRequested` carries the document that opened it. Pop-ups are dropped when
    that document is replaced, and none is made before a document commits.
    (6) Events and delegate notes are kept in `delegate::Coalescing`, which costs constant time per
    item and keeps the latest replaceable item in its own place. A pump spins Servo only while
    fewer than 256 events wait.
    (7) **Timing.** `ServoDiagnostics::timing` (`ServoTiming`) holds the last, longest and total
    paint (`WebView::paint`) and swap of submitted frames. It also holds the latest time from a
    `Navigate` to the first frame of its committed document. With
    `RUST_LOG=sighurt_engine_servo::timing=debug`, each is also logged as a
    `servo-timing frame view=N w= h= paint_ms= swap_ms=` or
    `servo-timing first-frame view=N after_navigate_ms=` line.
    (8) **Rejected: `NotReady` after a resize until Servo has a new frame.** `WebView::resize`
    sets Servo's `RepaintReason::Resize`, and every spin then calls `notify_new_frame_ready`
    until a paint clears it (servo-paint 0.5 `Painter::resize_rendering_context`,
    `webviews_needing_repaint`). A frame-ready after the resize therefore says nothing about the
    relayout, and Servo 0.5 has no other signal for it (M1 found the same). The first frame at a
    new size stays WebRender's current scene at that size: the old layout, for a frame or two
    (M1). It is reported `Submitted`, since the buffer is the permit's size. This is accepted, as
    the intermediate state of a scale change is in 55.
    (9) Smaller fixes. The lease is the view's last field, and a second `open_view` for an id
    that is open is refused. `shutdown` drops the leases after `eglTerminate`, as its
    documentation said. A permit for another view is an error, not a debug assertion.
    `servo-window` carries out an action at once rather than at the next wakeup. W1 checks the
    traversal commits in order, a wheel notch, and the window's size after the storm.
    `CAPABILITIES` is exported for the registry.
    Not changed: Servo still loads a page's own navigation to `data:`. An iframe's navigation to
    another scheme still becomes the tab's `OpenRequested(CurrentView)`, because Servo 0.5
    doesn't say which frame asks; the core could require a user activation for page requests to
    other schemes. *Why:* the M4 review; §16, §19, §26 and §28's question 2.
58. **Browser-level verification of `sig` with Servo and Wasm, and the integration fixes it
    found (amends 52, 54 and 57).** `sighurt-browser/tests/engines.sh` runs `sig` with its default
    features in headless sway with the GLES2 renderer (scenarios `servo`, `wasm` and `cross`; B1–B3
    in M4 and M5). Fixed in the Servo adapter, each found by it:
    (1) **Stop is `window.stop()`.** `Window` is a `[Global]` interface, so `stop` is the
    window's own property and `Window.prototype.stop` is undefined in Servo 0.5 (a probe page:
    `typeof Window.prototype.stop` is "undefined", the own property writable and configurable);
    decision 57's `Window.prototype.stop.call(window)` threw, and Stop never stopped a document's
    loading. A page that replaces `window.stop` keeps loading, as it could by starting new
    fetches. E1 now checks it.
    (2) **A hidden view is throttled at each history change.** Servo 0.5 throttles the document a
    WebView shows when asked (`Constellation::set_webview_throttled`), not the WebView: asked when
    a WebView is built, before its browsing context exists, the request is dropped ("Tried to
    SetWebViewThrottled after closure"), and every document a hidden view navigates to starts
    unthrottled. A WebGL page opened in a background tab woke `sig` 137 times a second (822
    wakeups, 0 frames, in 6 s); now 1 in 6 s.
    (3) **A hidden view is resized with the page area** (decision 13 says hidden views keep the
    page area's metrics; the adapter skipped the resize while the view had no EGL window surface).
    A view opened before the window's first configure (the host's 1024×768 default) kept its
    first layout when shown at 1280×696 if its page ran a CSS animation: Servo 0.5 didn't lay it
    out again on the resize that came with the show (a static page did). surfman's resize on
    Wayland without a bound surface only records the size, so the page is laid out at its size
    while hidden, as other browsers do with background tabs; a WebView built after its context
    is resized to the current metrics.
    (4) **History entries keep their own address.** Servo 0.5's history report names an entry made
    by `pushState`/`replaceState` after its neighbour nearer the current one (its projection has
    no address for such entries); the adapter keeps the address each entry had when it was last
    current, reshaped with the list like the document keys
    (`navigation::tests::entries_keep_the_address_they_had_when_current`, which fails without
    it). Before, the session saved `#two` for the `#one` entry, so a restored tab went back to
    the wrong address. Decision 52's "reported as it comes" no longer holds.
    In `sig`: the diagnostics JSON lists each tab's browser state (loading, back and forward, a
    record, an error; never an address or title), `SIG_AUDIT_GL=1` turns on Servo's GL read audit
    and the engine report gives `gl_reads`, a `frame` line's `paint_ms` is the engine's own
    painting (Servo's `WebView::paint`, `ServoTiming`) and `present_ms` the `present` call (the
    Servo track's ask), and the registry takes Servo's exported `CAPABILITIES`. The test helper
    `virtual-pointer` can hold the button (`press`, `release`).
    Not changed, measured and left for decisions: Stop can't stop a navigation waiting for its
    response (M4's criterion stays open); hidden Wasm guests that draw wake `sig` about once a
    second each (decision 23's hidden pace: 4 wakeups in 3 s with four in background tabs), since
    the owner hears of each new frame although a hidden view can't present it; a guest sees a
    button release as a click at the pointer's position when its next step runs, so a release
    followed within one frame by a move counts at the new position (the SDK polls position and
    clicks per frame); the first frame of each Servo context costs 25–167 ms of owner time
    (WebRender's warm-up), during which the chrome waits; the address bar shows a fragment's
    percent-encoding as it is (`#field%20%C3%A5…`). *Why:* M4's and M5's pass criteria at browser
    level (§23), §26's acceptance criteria, and §28's questions 3 (wakeups) and 5 (evidence).
59. **nixpkgs is pinned in `nix/nixpkgs.nix`, at the desktop's revision** (§21).
    `builtins.fetchTarball` of nixpkgs 4975466, named `source` so that it is the store path the
    desktop's flake input already was, with no user configuration or overlays; `shell.nix` and
    `nix/wpewebkit.nix` import it. The desktop's shell derivation stayed the same. louise-pc moved
    from its own 9ae611a (Rust 1.95) to Rust 1.98.1: its Servo is rebuilt once, and no machine
    checks the workspace's `rust-version` (1.95) any more. *Why:* both machines must build the
    same thing, and WPE WebKit's prefix refers to store paths of one revision; the desktop's
    revision kept every other build there untouched (WPE.md, "Pinned nixpkgs").
60. **WPE WebKit 2.52.6 is built from its release tarball into a plain prefix with the same
    path on both machines** (`~/.local/share/sighurt/deps/wpewebkit-2.52.6`), from a
    `nix-shell --pure` on the pinned nixpkgs (clang, Release, `-j6`), and copied with rsync. A
    `runtime` derivation listing every input is the prefix's GC root next to it, and the build
    fails if the prefix names a store path outside that closure. Flags: the WPE Platform API with
    Wayland, DRM and headless, the libwpe API, the bubblewrap sandbox with nixpkgs' WebKitGTK
    sandbox patch, GStreamer, introspection (the GIR files name the prefix's library),
    MiniBrowser; no Qt, no docs. *Why:* nixpkgs has no WPE WebKit; a Nix derivation's output
    can't move between the machines (only root is a trusted user, and changing that is the
    user's call), and building it on each costs the build twice; 2.52.6 is the pinned nixpkgs'
    WebKitGTK, so its inputs and patch apply as they are (WPE.md).
61. **WPE WebKit reaches the shell through variables of its own, never `PKG_CONFIG_PATH`**:
    `SIGHURT_WPE_PREFIX` and `SIGHURT_WPE_PKG_CONFIG_PATH`, with `LD_LIBRARY_PATH`,
    `GIO_EXTRA_MODULES` and `GST_PLUGIN_SYSTEM_PATH_1_0`, and only when the prefix is installed.
    The WPE engine's build script runs pkg-config with `SIGHURT_WPE_PKG_CONFIG_PATH` and declares
    `rerun-if-env-changed` for it. *Why:* ten crates' build scripts (`wayland-sys`,
    `yeslogic-fontconfig-sys`, `x11-dl`, …) declare `PKG_CONFIG_PATH`, so changing it, or the
    shell's `buildInputs`, rebuilds most of Servo on every machine; none declares the new
    variables (the build outputs in `target/`, rustc's env-deps, and a `Fresh` cargo run with the
    prefix present; WPE.md, "Using the prefix").
62. **WPE WebKit is embedded through the WPE Platform API** (WPE.md, "1. The embedding API"):
    Sighurt's own `WPEDisplay` and `WPEView`, GObject subclasses written with glib-rs 0.22, with
    WebKit's stock `WPEToplevel`, one per view. WebKit hands each frame to the view as a DMA-BUF
    or shared-memory `WPEBuffer`; the engine answers with `wpe_view_buffer_rendered` and
    `wpe_view_buffer_released`. The display advertises the compositor's DMA-BUF formats and main
    device and is WebKit's primary display. Not libwpe with WPEBackend-fdo. *Why:* the libwpe API
    is WebKit's legacy API (a build may leave it out since 2.51.4; the MiniBrowser left it in
    2.49.2; new embedding features go to WPEPlatform only); the buffer hand-off maps one to one
    onto the host's permits and `wl_buffer.release`, with no nested compositor and no EGL in
    Sighurt's process; it is the smaller adapter (66 C functions, two subclasses, no
    WPEBackend-fdo). Its API still changes between WebKit releases and is off by default in 2.52
    release builds: Sighurt builds WebKit itself (decision 60) and rechecks the adapter on each
    WebKit update.
63. **WPE views present on native page targets: DMA-BUF without copies, shared memory with one
    counted copy as the fallback** (WPE.md, "4. Presentation"). The presenter joins the host's
    `wl_display` with its own event queue, imports each WebKit buffer once with
    `zwp_linux_buffer_params_v1.create_immed`, and attaches, damages and commits it on the host's
    permit; frame done (`buffer_rendered`) is signalled at commit, so WebKit renders at most one
    frame ahead and the host's frame callbacks pace it. The first view picks the transport:
    DMA-BUF when the compositor has `zwp_linux_dmabuf_v1` version 4 with a main device; otherwise
    WebKit is given no DRM device, renders into shared memory, and the presenter copies each frame
    into at most 3 `wl_shm` buffers per view. A frame one pixel off the host's rounding is shown
    (WebKit floors, the host's viewport scales); a frame of an old size is dropped. Implicit
    synchronization: WebKit waits for its rendering fence before handing a frame over. *Why:*
    §3's target (no page readback, no re-upload) holds on the hardware path; §10's three signals
    stay apart (FrameReady, the host's callback, release); the fallback is visible in diagnostics
    (§26).
64. **WebKit's GLib main context runs from the owner loop through a watch thread and
    `EngineWaker`** (WPE.md, "3."). The runtime acquires the default context and splits GLib's
    iteration across pumps (check and dispatch, then prepare and query; the query's timeout is the
    pump's deadline); a thread `poll`s the queried descriptors, wakes the owner, and waits for the
    next set. Not calloop sources for GLib's descriptors. *Why:* WebKit's descriptors change every
    frame (rendering fences), and epoll registrations of descriptors GLib closes are unsafe where
    `poll` takes them afresh; the runtime needs only the `EngineWaker` every runtime gets
    (`registry.rs`); no busy polling (0 GLib dispatches and 1 loop wakeup in 5 s idle).
65. **`sig`'s executable is linked with `-Wl,--exclude-libs,ALL`**, so the C libraries
    statically linked into it stay private (proposed for `sighurt-browser`'s build script; proven
    in `wpe-coexist/build.rs`). *Why:* as Cargo links it, the executable exports its copies of
    HarfBuzz 11.2.0, FreeType 2.13.2, libpng 1.6.44, SQLite 3.51.1, mozjpeg and SpiderMonkey's
    `getenv`/`setenv`/`abort` (today's `sig`: 1 075 symbols), and shared libraries bind into them
    instead of their own versions: 1 524 bindings in `wpe-coexist`, 220 from WebKit's library and
    445 from libsqlite3's own internal calls (`LD_DEBUG=bindings`). That mixes two versions of a
    library on the same objects, in `sig` today (libass, fontconfig, HarfBuzz) and in WebKit's UI
    process. With the flag: 1 export, 0 bindings, every check passing. Servo using system
    libraries was rejected (HarfBuzz is bundled unconditionally by `servo-fonts`; a Servo
    rebuild; the other libraries would remain), and `RTLD_DEEPBIND` was not needed.
66. **JavaScriptCore in Sighurt's process stays off signals** (amends 26): before WebKit is first
    used, the WPE runtime sets JavaScriptCore's `usePollingTraps = true` and
    `useWasmFaultSignalHandler = false` (`jsc_options_set_boolean`) and fails to start if they
    don't hold. A web process that dies is `ViewFailed`, and a reload starts a new one. *Why:*
    WebKit keeps a JavaScriptCore VM in the UI process (script results are `JSCValue`s); its
    `SIGSEGV`/`SIGBUS` handler blocks every signal while it chains to the next handler, and
    Wasmtime 36 leaves its handler with `longjmp`, which keeps that mask, so the next Wasmtime trap
    is a blocked `SIGILL` that kills the process (reproduced, with a core dump). Options are per
    process: pages keep signal-based traps and fast Wasm memory in the web processes. Crash
    containment comes from WebKit's own process model (WPE.md, "Process model"), not from
    Sighurt.
67. **The configuration stays at version 1 and is checked against the build** (extension-api.md,
    decision 1, as far as its steps 1–4 go; amends 21, 28 and 41). New and optional: `[input]
    keymap` (default `bindings`, applies live), `[keymap.<name>]`, `[ui] chrome` (default
    `standard`, read when `sig` starts; a reload that changes it gets a notice saying so), `[ui]
    font` and `font-size` (checked, handed to the chrome in its theme), and `[chrome.<name>]`.
    `Config::parse(user, &Environment)` checks the whole file in one pass: each keymap's and
    chrome's table with its own check (problems prefixed `[keymap.x]` or `[chrome.x]`, in the one
    list; an unused table is still checked), and the `[ui.colors]` names of the build's chrome
    (`sig` no longer checks them itself). A table for a keymap, chrome or engine (`[routing.x]`,
    an `[engines] enabled` entry) that the build doesn't have is accepted with a notice naming
    what it has, and so is a key table no keymap reads; only the user's file gets notices, and
    the core shows them (`Effect::Notice`) at start and on each valid reload.
    `[keymap.bindings]` is an error pointing to `[keys]`. Implementations get their tables as
    `sighurt_api::settings` values, taken from the parsed TOML directly, since serde turns dates
    into strings; dates are refused. *Why:* the user's request to make more configurable, §21,
    and extension-api.md §1.1 and §1.11: every version 1 file keeps its meaning, and one file
    serves builds with and without an implementation.
68. **The keymap and chrome APIs** (extension-api.md, decision 5; amends 18 and 40). The keymap
    API is `sighurt_api::{settings, keymap}`, `input::KeyboardFocus` (replacing
    `sighurt_core::Focus` and `sighurt_ui::Focus`) and `commands::CommandSet`. The core holds a
    `Box<dyn Keymap>`, `bindings` included, and keeps the rule that a taken press takes its
    release and repeats whatever the keymap says: keymaps decide fresh presses and repeats of
    what they took, and `RunThenPass` routes the press by the focus after the command. The core
    tells the keymap the context when it changes and resets it when the window loses the
    keyboard; a changed indicator redraws the chrome; a reload configures the same keymap
    (keeping its state) or replaces it (taken presses stay taken). The chrome API is
    `sighurt_ui::extension` (`Chrome`, `ChromeEvent`, `ChromeAction`, a `ChromeLayout` of up to
    8 named surfaces with input areas, `ChromeKind`, `Theme`, `contained`); sighurt-ui's chrome
    is `Model` behind the `standard` chrome, and `UiAction` became `ChromeAction`. A tab click
    is `SelectTab(TabId)` or `CloseTab(TabId)`, which the core runs as `tab.select` or
    `tab.close` with the tab's number at that moment; a tab that is gone does nothing. `sig`
    picks the chrome from `[ui] chrome` at startup (`registry::CHROMES`; `KEYMAPS` stays empty
    until a keymap crate exists), drives it through the trait, keeps one SHM target per surface
    name (a listed one at its rectangle clipped to the window, its input area clipped to it; one
    no longer listed hidden at its last rectangle, clipped), contains every snapshot string to
    the bounds of extension-api.md §2.6 (`Snapshot::contain`), gives the host's keyboard focus to
    the chrome's text-field surface or the chrome surface at the page's rectangle, and counts
    paints and peak buffers by surface name. There is no stability promise. Left out until
    something uses them: `Value::as_f64` (nothing reads a float) and `TabSnapshot::engine`
    (neither the standard chrome nor the line chrome shows it); chrome commands
    (`ChromeEvent::Command`, `Command::Chrome`) come with step 5, and `CommandSet` already names
    a chrome's actions in its errors. *Why:* the user's request for "a tiny api that makes it
    possible for other people to program their own implementations"; §4, §8 (input regions),
    §13 and §28's first two questions: the keymap's state is the keymap's, taken presses the
    core's, surfaces the chrome's to lay out and the host's to validate, and a late tab click
    still names its tab.
69. **WPE navigations and history come from WebKit's back-forward list** (`sighurt-engine-wpe`,
    amends 12's attribution rule for WebKit). WebKit's GLib API gives navigations no ids, so the
    browser's latest request WebKit hasn't started takes the next `load-changed` STARTED, or ends
    as a same-document commit on a new list entry or, after the list moved for it, when
    `is-loading` turns false; every other load and the page's own same-document navigations
    (`uri` changing while nothing loads) get the adapter's ids. A commit's `HistoryEffect` comes
    from comparing list snapshots, taken inside WebKit's signals, by entry identity: the same
    current entry is `Replace`, an existing one `Traverse`, a new one after the old current
    `Push`. A request stays pending when WebKit merely drops its marker of a pending request (two
    requests in a row). Failed loads show no WebKit error page. *Why:* §16 ("the engine owns the
    truth of its history"; nothing guessed from URLs); the sequences WebKit 2.52 actually emits,
    recorded in probe runs (WPE.md, "What the engine adds to the prototype");
    `navigation::tests`, `engine.rs`.
70. **WPE's requests go through the service API and fail closed.** Permissions (geolocation,
    camera and microphone, screen capture), file pickers, downloads and the clipboard are
    `ServiceRequested` for the committed document, at most 16 per view; WebKit's request objects
    are refused when dropped (unanswered, or at a new document). WebKit's clipboard keeps no page
    data: a copy is `ClipboardWrite`, a paste that finds nothing asks `ClipboardRead` and, once
    answered, is run again with the text granted for one second (the page sees an empty paste
    first; WebKit's clipboard is synchronous and one per process). A download is WebKit's
    download cancelled before writing and handed to the browser by URL. New windows and other
    schemes are `OpenRequested`; HTTP authentication, context menus and fullscreen are refused;
    WebKit's own GeoClue position provider is refused (no position service exists). *Why:* §12,
    §17 and CLAUDE.md's security rules: the core decides, the executable performs, nothing
    succeeds silently; the evidence is `engine.rs` and window-checks (copy and paste through the
    Wayland clipboard).
71. **WPE input methods go through `WPEInputMethodContext`**, which the Platform API offers:
    WebKit's focus, caret, surrounding text and purpose become `TextInputChanged` (never a
    password's text), the host's updates WebKit's own signals, and key events are never
    filtered. A field is reported once WebKit has given its caret, because wlroots latches a
    text-input session's features at enable: a first report without surrounding text kept the
    text from the input method for the whole field (seen in `engine.rs` before the change). The
    host could re-enable when surrounding text first appears; that is the host's to decide.
    *Why:* §12 ("implement preedit, committed text, surrounding text ... where available");
    `ime::tests`, `engine.rs` with a real input method.
72. **One WPE runtime per process, with a locked profile and an explicit shutdown.** A second
    `WpeRuntime::new` is refused (WebKit's UI-process state and the primary display are global).
    The profile is a persistent `WebKitNetworkSession` in `WpeConfig::profile` under
    `{profile}/lock`, ephemeral when locked by another process or unset. `shutdown` closes the
    views in the proof's order, lets GLib finish, and disconnects the engine's handlers from
    WebKit objects that outlive it (the default context's geolocation manager is finalized at
    process exit, where a glib-rs closure made on another thread aborted the test process).
    `experiments/wpe-engine` is deleted; `wpe-coexist` links the crate. *Why:* §20 (explicit
    ownership, nothing left to `Drop` order); decision 26's profile rule, as for Servo.
73. **WebKit's GLib context takes up to four turns per pump while something is ready at once**
    (amends 64). WebKit often wakes its own context, or leaves a source ready, while it
    dispatches (its run loop's zero-delay work); a turn on the owner costs less than the watch
    thread waking the owner for another loop iteration. `GlibLoop::iterate` checks and
    dispatches again, at most four turns, while `prepare` says a source is ready or a queried
    descriptor is ready at once (a zero-timeout `poll`), then returns `Pending { deadline: None }`
    as before if work remains. *Why:* §19 ("if processing generates immediately runnable work,
    arrange another iteration") at the lowest cost, with the pump still bounded; measured with
    temporary counters on one animated view (DMA-BUF, desktop): loop wakeups per frame 5.1 to
    3.5, the watch thread's wakes 3.25 to 1.6, GLib dispatches (1.65) and pacing (16.7 ms
    median) unchanged; in `window-checks.sh` 5.8 to 4.0 per frame, idle still 1 wakeup in 5 s.
74. **Chrome commands, `page.send-key`, the modal keymap, the mouse's side buttons and scroll
    speed** (extension-api.md, decisions 4, 5 (its proof keymap) and 6, as far as its steps 5, 6,
    7 and 10 go; amends 27 and 28). (1) `chrome.<action>` is `Command::Chrome(action)`, the action
    kebab-case ASCII of 1 to 32 characters with no argument (`CommandError::InvalidChromeAction`
    otherwise). `Command::NAMES` gains only `page.send-key`, since a chrome's actions are its own,
    and `Command::name` is `chrome` for all of them. A configuration's are checked against the
    selected chrome through `CommandSet`; the core hands every one on as `Effect::ChromeCommand`
    without checking it, and `sig` checks it against the chrome it runs, since a command typed
    after `:` is parsed with no chrome to check against. (2) `page.send-key` holds a `SentKey`:
    the modifiers, the W3C key and the evdev code. It names a key as a keystroke of the
    configuration does, but only a named key whose W3C name is also the code of a key that types
    nothing on common layouts and is neither one of a pair nor on the keypad (`down`, `pageup`,
    `enter`, `f5`, `BrowserBack`), or `space`: what a character means depends on the layout. Its
    press types " " for space unless Control or Super is held, and nothing otherwise. The core
    sends the press and the release back to back on the seat whose keyboard it heard last, only
    while the page has the keyboard, and only a key press that runs it is the page's activation.
    (3) The side buttons are `BrowserEvent::Button { view: Option<ViewId> }`, which `sig` sends
    for every Back and Forward event wherever the pointer is; one that comes as
    `BrowserEvent::Pointer` takes the same path, so the old behavior holds by configuration and
    `the_mouse_back_button_goes_back` is unchanged. The core keeps the presses it took per seat
    and takes their releases even after the binding changes; an unbound press over the shown view
    is the page's like any button (activation, the keyboard), anywhere else it goes nowhere. The
    built-in file binds `back` and `forward` to `page.back` and `page.forward`, and sets `[input]
    keymap = "bindings"` and `scroll-speed = 1.0`, so `[input]` and `[input.buttons]` merge key by
    key and `""` unbinds a button. (4) `scroll-speed` multiplies the scrolling the core hands a
    page: pixels as they are, a wheel's 120ths rounded but never to 0; the chrome's own scrolling
    is untouched. (5) The modal keymap follows §2.5, and decides what it left open: `start` is
    required; a mode's name is 1 to 32 lower-case letters, digits and dashes; two bindings that
    apply in one mode (its own and `always`'s) may be neither the same keys nor one the start of
    the other; `always` follows the rule of `pass` modes (no sequence starting with a key that
    types), and while the chrome has the keyboard only its keys that don't type apply, so the
    address line gets every character; any change of context ends an unfinished sequence; a reload
    keeps the mode while the table still has it; text fields switch modes only when `typing` is
    set, and the chrome taking the keyboard in between changes no mode; `{ run, pass = false }` is
    a plain command. *Why:* the user's request for configurable keys and UI through a tiny API;
    §12 (a taken press or button takes its release, whatever the configuration does meanwhile),
    §21 (validated once, applied live), §26 (sequences need no timer, nothing ticks) and §28's
    first question: the mode and the unfinished sequence are the keymap's, taken keys and buttons
    and where the keyboard is the core's, a chrome's actions the chrome's, and which chrome runs
    `sig`'s.
75. **The standard chrome's options and fonts, the line chrome, and addresses shown decoded**
    (extension-api.md steps 5, 8, 10 and 15–17 on the chrome's side; amends 18 and 20). (1)
    `[chrome.standard]` takes `tabs` (`top`, `left`, `hidden`), `status` (`strip`, `overlay`),
    `buttons` and `compact`, all live; `chrome.toggle-tabs` switches between the configured tabs
    and none (none and the top for `hidden`) and stays switched across reloads. (2) Every size is
    today's times `font-size` / 14, rounded half up; compact is a 32 px strip with 28 px tabs and
    a 36 px toolbar with a 28 px address field; the text styles scale the same way, and the
    address field's text sits on whole logical pixels, as its caret rectangle does. Nothing is
    measured for the standard chrome's layout (decision 21 stands); the line chrome measures
    text within its one line. (3) The tab list on the left is 220 px wide, at most half the
    window; it shows whole rows, scrolls a row at a time (3 a wheel notch), ends with the new-tab
    button, and a switch to a tab out of view scrolls it into view. (4) The status overlay is half
    the page wide, at least 200 px and at most the page, as tall as the strip, listed only while
    there is status text, with no input region, and the page keeps its size. (5) Amends 18: the
    painter paints a surface by name (`CpuPainter::paint(model, surface, ..)`), not whatever lies
    in a rectangle, since the overlay lies over the page area, whose buffer would otherwise hold
    the status text and show it stale once the overlay goes. A chrome reports repaints only for
    the surfaces it lists. (6) Amends 20: `[ui] font` names a family, matched without regard to
    case; one that isn't installed leaves the text in the first installed family of decision
    20's list, with a warning in the log, and a new family drops the shaped text. `Canvas` and
    `Pen` (a buffer checked against its metrics, the glyph cache, ARGB8888) are split out of the
    painter, and `Fonts`, `TextStyle` and `ShapedText` are public: with `TextEditor` they are
    the helpers of the chrome API, which `sighurt-chrome-line` uses and nothing of the core, the
    host or an engine. (7) A committed address is shown decoded where that is safe
    (`readable_url`): percent-encoded UTF-8 in the path, query and fragment, but never ASCII
    (`%2F`, `%25` and `%20` stay), whitespace, control, bidi or invisible formatting characters
    (the lists Firefox and Chromium keep encoded) or padlocks, and never the scheme or host
    (`xn--` stays). The shown form, typed back, parses to the same address, upper-case hex
    aside. Both chromes show addresses and hovered links this way; the snapshot keeps the
    committed URL. (8) The side buttons are not the chrome's (decision 74). (9) The standard
    chrome shows the keymap's indicator at the address field's right end. *Why:* the user's
    request ("improve the possibilities to configure things", a tiny API others can implement),
    plan §8 (an overlay's input region covers only what it should receive), §13, §17 (page
    text and addresses shown contained, the address line not spoofable), §26 (no repaint
    without a change, nothing ticks) and §28's first question: what is shown and where is the
    chrome's, which surfaces exist the host's to validate.
76. **Zoom, search keywords, `page.open-in` and chosen views, engine settings, the download folder
    and session restore** (extension-api.md decisions 1, 3 and 6, as far as its steps 9, 11, 12,
    14 and 18 go; amends 28 and 67). (1) `zoom` (0.5 to 3, live) is the zoom of every new tab,
    a page's pop-up tabs included, and what `page.zoom-reset` returns to; open and restored tabs
    keep theirs. (2) `[keywords]` maps names of 1 to 32 lower-case letters, digits and dashes (not
    `localhost`, so never a host or a URL) to http(s) templates. When the first word typed (or
    given to `page.open`) is one, the rest goes where `{}` is, encoded as a search is; a template
    without `{}` is used only for the keyword alone, and "gh servo" is a search (the design left
    it open; this way nothing typed is dropped). Input with `://` or starting with `sig:`, `data:`
    or `about:` is never a keyword use; keywords come before paths and host names. (3)
    `page.open-in <engine> [address]` is `Command::PageOpenIn { engine, address }`; the name's form
    is checked when it is parsed (`CommandError::InvalidEngine`), the build and `enabled` when it
    runs, with the tab's error as for a typed address (a `sig:` page, an engine the build lacks,
    one not enabled, a scheme it doesn't open). Without an address it opens the tab's committed
    address, else the one loading, else the one that failed. It is a navigation of the tab with
    the user's explicit choice (decision 29: a new segment, back returns). (4) A view is chosen
    (`View::chosen`) when the user opened or last navigated it with an explicit engine; a user's
    navigation without one unchooses it, the page's own (`OpenRequested`) leaves it as it is.
    Nothing reads it before step 13; records don't keep it. (5) `[engines.<engine>]` holds a
    neutral vocabulary read from one table in the core (`ENGINE_SETTINGS`: `user-agent`, 1 to 512
    printable ASCII characters, since Servo unwraps its parse as a header; `storage`, `profile` or
    `ephemeral`), checked against what the registry declares each engine honors
    (`Environment::engine_settings`: Servo both, Wasm `storage`, the demo engine none; WPE adds
    its line). Setting what an engine can't honor is an error naming the engine and what it
    honors; a table for an engine the build lacks is checked for its form and gets the notice;
    the old format's `command`, `schemes`, `extensions` and `mime` get the hints of
    extension-api.md §1.11. Every runtime starts with the settings of the configuration `sig`
    started with (`Registry::start_at_launch`), whenever it starts, so a reload that changes them
    gets a notice that they apply at restart. (6) Servo: `user_agent` replaces the adapter's
    Firefox-like string, process-wide (the header and `navigator.userAgent`); `ephemeral` is no
    profile. Wasm: `ephemeral` puts the key-value store in a directory of the runtime's own,
    `$XDG_RUNTIME_DIR/sighurt-wasm-*` (mode 700, made when the runtime starts), and caches no
    compiled modules; `sig` deletes the directory at the end of its shutdown, after the
    downloads' grace, since a guest still finishing its last call when the runtime shut down
    could write there again. No temporary module cache, unlike the design: a compile that outlives
    shutdown would write it again, and it would record which apps ran. (7) `BrowserEvent::
    ConfigReloaded` carries `Result<Box<Config>, ConfigError>`: the configuration grew past
    clippy's bound for an enum variant. (8) `[downloads] directory` is an absolute path or `~/…`,
    expanded by the core with `Environment::home_dir`, read at each download; the download's
    thread makes the folder, and failing to fails the download with its status line and no file.
    (9) `[session] restore = false`: `sig` reads no session at start and writes none, leaving the
    file as it is; turned on while `sig` runs, it saves the tabs after the usual delay. *Why:* the
    user's request ("improve the possibilities to configure things"), plan §16 (an explicit user
    engine choice wins), §17, §20 (shutdown order, what outlives the process), §21 (validated
    once, live where possible, restart for engines) and §28's first question: each setting has one
    owner (`zoom` and keywords the core's, engine settings the engines' made by `sig`, the folder
    and the session file `sig`'s), and what an engine honors is the registry's to say.
77. **Host rules, and links that follow the routing** (extension-api.md decision 2, as far as its
    step 13 goes; amends 28, 53 and 57; settles 55's open item about links). (1)
    `[routing.<engine>] hosts` takes host names and IP addresses, and `*.name` for every host under
    a name but not the name itself, normalized as the `url` crate normalizes a URL's host (so a
    pattern means what the hosts it is matched against mean); a pattern belongs to one engine. The
    router tries them after an origin's rule and before the default engine, the most specific
    first: an exact host, then the longest wildcard. (2) An engine asks before it performs a
    navigation its page started that it would load itself (`EngineEvent::NavigationRequested`,
    with the asking document and a `RequestId`), holds it until `EngineCommand::AnswerNavigation`,
    at most 16 per view (beyond them, it goes ahead without asking), and denies what it still holds
    when the view closes, its document is replaced or gone, or the runtime shuts down; a late
    answer does nothing. (3) The core answers every one at once: denied for a document its tab
    doesn't show and for what no page may open (`sig:`, `file:` from a web page, decision 41's
    rule, checked here too so that a hand-off never opens what a page may not); allowed for a
    chosen view (decision 76); otherwise denied and opened in the tab as routing says only when
    routing gives the URL to another engine that can open views now and the user clicked or
    pressed a key in the page within 5 s, which that uses up (decision 30's activation, shared with
    pop-ups: one click, one tab or one hand-off); allowed in all other cases. Where the design said
    "another enabled engine", the core also requires the engine to be in the build and working:
    a link to a host claimed by an engine this build lacks (one file, several builds) or that
    stopped stays in its engine instead of becoming an error page. A URL longer than 2 MiB, which
    the core keeps nowhere, is allowed in place. (4) Servo: Servo's `NavigationRequest` is held in a
    wrapper that denies it when dropped, since Servo allows one dropped unanswered; nothing
    dropped by mistake goes ahead. Servo 0.5 carries out the browser's own reload as the page's
    navigation to its address (`Location::reload` → `schedule_navigation`), so the request that
    follows the adapter's `reload()` for the current address goes ahead without asking (a reload
    is never handed off). Navigations to other schemes, and to `file:` from web pages, are still
    denied and handed over as `OpenRequested` (decision 53). The time from Servo's request to the
    answer is logged (`servo-timing navigation-answered`): 0.08–0.20 ms in `sig`. (5) Accepted Servo
    0.5 limits, recorded with 55 and 57: a navigation doesn't say which frame it is from, so a
    frame's navigations are asked about too; they stay in Servo unless the user clicked or typed
    in the page within 5 s, so a click on a link inside a frame that goes elsewhere moves the
    whole tab. While a request is held, Servo's constellation drops another navigation of the same
    frame. Only the URL is handed over: a form's POST data and the referrer stay behind. *Why:*
    the user's request (more configurable: which sites open in which engine), plan §16 (routing
    without a probe; an explicit user choice wins; engines own their navigations but not the
    browser's routing), §17 (pages get nothing by themselves: an activation, used up), §28's first
    question (routing policy stays in the core; which schemes an engine loads stays in the engine)
    and second (a late answer names its request and document); extension-api.md §1.4.
78. **WPE WebKit is one of `sig`'s default engines, for the sites the configuration gives it**
    (amends 1, 26 and 65; completes 72 and 77 for WebKit). (1) `sighurt-browser`'s `default`
    features are `engine-servo`, `engine-wasm` and `engine-wpe`; `[engines] default` stays
    `servo`, so WebKit opens only what `[routing.wpe] hosts` claims and what `page.open-in wpe`
    asks for, and links between them follow decision 77. Without the WPE prefix the engine's build
    script stops the build with `cargo::error` lines saying how to install it and how to build
    `sig` without it (`--no-default-features --features engine-servo,engine-wasm`). (2) `sig`'s
    build script links every target with `-Wl,--exclude-libs,ALL` (65, now built: 1 symbol
    exported, no library binding into `sig`) and an RPATH from `DEP_WPEWEBKIT_RPATH`. (3) The
    registry's `wpe` entry honors `user-agent` (every view's `WebKitSettings`) and `storage` (a
    persistent network session in `{data_dir}/sighurt/wpe` under its lock, decision 72, or an
    ephemeral one); its renderer line is `hardware` on DMA-BUF and `fallback` on shared memory,
    whose copies are its page readbacks. (4) Amends 26: `sig` sets `JSC_SIGNAL_FOR_GC=12`
    (SIGUSR2) at the start of `main`, before any thread exists, unless the environment names a
    signal; JavaScriptCore's thread-suspension handler otherwise takes SIGUSR1, `sig`'s
    diagnostics signal, and a SIGUSR1 then suspends the thread that receives it with nobody to
    resume it (found by `engines.sh`: `sig` stopped at its first diagnostics request).
    JavaScriptCore logs that it doesn't know the variable as an option of its own, once per
    process; WTF reads it all the same. `GIO_EXTRA_MODULES` and `GST_PLUGIN_SYSTEM_PATH_1_0` stay
    `shell.nix`'s (`sig` warns when they are missing), as `LD_LIBRARY_PATH` does. (5) The WPE
    adapter asks (`NavigationRequested`) about a link or a GET form the user clicked, by WebKit's
    navigation type and user-gesture flag, not a redirect, and holds WebKit's policy decision in
    a wrapper that ignores it when dropped (WebKit uses a decision dropped unanswered), at most
    16 per view, refused at a new document, a crash, close and shutdown. Everything else goes
    ahead in WebKit without asking: WebKit's policy decision doesn't say which frame navigates,
    and a frame a click handler makes looks like a script's navigation after a click (type
    "other", a gesture; probed), so scripts' navigations stay in WebKit even after a click; a POST
    form stays, since a hand-off carries only the address. A link clicked inside a frame is asked
    about and moves the whole tab when routing gives it elsewhere, as in Servo (77). *Why:* the
    user's request ("add in another engine backend for WPE webkit ... to increase short term
    compatibility, servo is still the long term goal. Make it work"); plan §16 (routing without a
    probe; the engine owns its navigations), §17 (a page moves the tab only with the user's
    click), §18 and §21 (one build with every engine; process-wide setup is the executable's),
    §26 (no copy on the display path; measured in `engines.sh`) and §28's first question (which
    navigations are the user's is the engine's knowledge, routing the core's, the process's
    signals `sig`'s).
79. **M7's comparison is one interleaved session under the pinned compositor, and the
    benchmark builds the rewrite with the pinned Rust** (amends 49). `bench/run` takes several
    builds and runs all their workloads in one random order per round, each run holding the
    heavy-job lock alone (`--lock-runs 1`), so drift falls on every build alike and other agents'
    short jobs fit in between; each build keeps its own set. `tools/headless-sway` and the harness
    take sway, grim and the input tools from `nix/nixpkgs.nix` instead of the machine's
    `<nixpkgs>` (on louise-pc that was a different sway 1.12 build), and every set records the
    compositor's store path and the pin. Every run gets its own `XDG_STATE_HOME`: the rewrite
    saves its tabs there and restores them beside the ones it is asked to open, which in a trial
    made the `home` run measure the previous run's CSS animation; a run that ends with other tabs
    than it opened is an anomaly. `sig`'s `--diagnostics` report at exit is kept in each run
    (`exit.*` metrics). The compared runs carry no `RUST_LOG` (a debug target raises the `log`
    level for all of Servo) and no GL audit (it times every GL call); the set `rewrite-audit` has
    both. A build's parser declares what it can't measure honestly against another build
    (`INCOMPARABLE`: GPUI's key-driven draws, `sig-servo`'s navigation time), and `bench/compare`
    leaves those rows out with the reason. `bench/pipe-cost` measures the baseline's frame
    transport alone. `bench/rewrite/build.sh` puts the pinned nixpkgs' `rustc` and `cargo` first
    on `PATH`: `shell.nix` brings no Rust, and louise-pc's own is 1.95, older than the
    workspace's `rust-version` (1.98), so `tools/heavy cargo build` of the workspace fails there.
    *Why:* the same compositor, driver and toolchain for both builds, measured in the same session
    (the task's fairness requirements), and what the trial runs found; the September baseline
    measured again this way differs from its first set in nothing after the false-discovery
    correction.
80. **Stop can't stop a Servo navigation still waiting for its response: an accepted Servo 0.5
    limit** (settles what decisions 57 and 58 left open; M4's "Reload and stop"). Servo 0.5 has no
    embedder call that cancels a top-level navigation whose response hasn't come (its `WebView`
    has `load`, `reload` and traversals, no stop), and the page's `window.stop()` aborts only the
    current document's own fetches (servo-script 0.5 `Window::stop_loading`). No adapter
    workaround is correct: a load asked for while the navigation is pending is dropped by Servo
    (57 (2)), and going back or reloading after the commit would change history the user didn't
    touch and still fetch the page. So `page.stop` in Servo does what Servo can: it cancels a
    navigation the adapter still queues, gives up a load that has shown no sign of life for 5 s
    (the way out of a 204), and stops the document's loading (58 (1)); a navigation already waiting
    for its response commits when the response comes, with its id. `engines.sh` `servo` logs that
    case as INFO and E1's `engine.rs` pins it, so a Servo that can cancel shows up as a test
    change. WPE WebKit's Stop does cancel such a navigation (`sighurt-engine-wpe/tests/engine.rs`:
    "Stop ends the navigation", `Cancelled`, the document stays), so a site where it matters can
    be given to WPE. Revisit with a Servo version that can cancel a pending navigation. *Why:* §23
    ("a clearly unsupported feature is better than a claimed feature that only works by
    accident") and STATUS's rule that a criterion that doesn't hold is decided in this log.
81. **The documents and CI describe the rewrite now, and the release workflow is gone** (amends
    plan §23 M7's last step, which waits for M7 to update them; the user asked for documentation
    of the actual state this wave). README, DOCS, SECURITY, CONTRIBUTING, ROADMAP and CLAUDE.md
    describe `sig` as it is: in-process engines with no IPC (and so no sandbox claim), WPE WebKit
    beside Servo and Wasm, the configuration, the command table, and how to write a keymap or a
    chrome (the README's keymap example was compiled and tested against `sighurt-api` in a crate
    of its own). CI checks what builds without Servo and WPE WebKit: `ci.yml` leaves out
    `sighurt-engine-wpe` and `wpe-coexist` (the runners have no WPE prefix) and checks `sig` with
    the demo and Wasm engines, the modal keymap and the line chrome; `servo.yml` checks `sig` with
    every feature but `engine-wpe`. `release.yml` is deleted rather than fixed: everything it
    built is gone (`sighurt-ui`'s `sig`, `sig-wasm`), it targeted macOS (Sighurt is Wayland-only),
    its version bump reads the version from `sighurt-ui/Cargo.toml`, which now says
    `version.workspace = true`, and edits per-crate `version` lines that only the SDK still has;
    and a release of today's `sig` needs a packaging decision nobody has made: its default build needs
    the WPE prefix (not on GitHub's runners) and runs only with `shell.nix`'s libraries and an
    RPATH into `~/.local/share/sighurt/deps`. `sighurt-browser/tests/engines.sh` builds `sig` with
    its default features plus `keymap-modal` and `chrome-line`, which only its `showcase` scenario
    selects, and takes wl-clipboard from the nixpkgs pin. *Why:* the user's request ("Continue to
    optimize and test to make sure it is as good as it can be"), §23 (documents and CI that
    describe the actual architecture) and §28's fifth question: a release workflow that can't
    work is a claim without evidence.

82. **A page moves its tab only for the link the user pressed, never over the user's own
    navigation, and an engine the build lacks takes nothing** (the wave's reviews; amends 30, 67,
    76 and 77; extension-api.md's changelog records it). (1) At each press of a pointer button in
    a page the core keeps the page's status text, the hovered link (`View::link`; a key press
    clears it), and decision 77's hand-off happens only when the `NavigationRequested` URL is
    that link (as the `url` crate parses both), within decision 30's 5 s, using the activation
    up. So a frame's or script's navigation right after a click elsewhere, a link opened with the
    keyboard, and a form stay in their engine; a link clicked inside a frame still moves the
    whole tab (77 (5)). The 5 s window is kept rather than the review's 1 s: the URL match closes
    the hole (an ad or sandboxed iframe can send the tab only where the user pressed), and a slow
    press mustn't miss. (2) A switch the user started (a typed address, `page.open-in`, back or
    forward into a record) can't be cancelled or replaced by the page it leaves: while it is
    pending, the page's `OpenRequested(CurrentView)` does nothing and its hand-off is denied with
    nothing opened (`Switch::by_page`). A switch a page started may be replaced by the page or the
    user. (3) The routing rules of an engine the build lacks are checked (form and conflicts) and
    left out of `Config::routes`, so what they claim goes to the default engine, typed, restored
    or clicked alike, and the notice says the rules are unused; this replaces 77's "a link to a
    host claimed by an engine this build lacks stays in its engine". `[engines] default` in the
    user's file must name an engine of the build (a problem that lists them); the built-in
    `servo` isn't checked, so a build without Servo still starts. `page.open-in` such an engine
    still says it isn't in the build. (4) A tab's error is shown in the status strip on a `sig:`
    page too; only a tab with nothing to show draws it in the page area. *Why:* the reviews
    showed a page cancelling the user's switch to another engine's site, or replacing its
    address, any frame's script moving the tab after any click (Servo 0.5 asks about every
    frame), decision 67's "one file serves several builds" broken for routing, and errors on the
    home page invisible; plan §17 (pages get nothing by themselves; trustworthy UI), §16 (the
    user's explicit choice wins). Evidence: "Review fixes".
83. **A Wasm app's downloads are the browser's, and the download folder and names are safe**
    (amends 25 and 76; settles 76's open item and the gpui-baseline bug the security review
    found: a guest's file name went into `Path::join` uncleaned, so a guest could create any new
    file the user could, `~/.config/autostart` included, with no click). `api_download_data`,
    `api_download_url` and `api_canvas_print_pdf` queue `Service::Download` (`Data` or `Url`, the
    guest's name as the suggestion) in the document's `HostState::downloads`, at most 4 or 64 MiB
    waiting and none from a worker (each refusal is -1 to the guest and a console line); the view
    hands them to the core as `ServiceRequested` once the call has returned, with the document's
    generation and the app's origin. The core throttles them per tab (decision 15) and `sig`
    saves them as any download: `[downloads] directory`, a unique name made with `create_new`.
    The engine writes no file and starts no thread; the guest's 0 means the browser has the
    request. `sig`'s download folder is the configured one, else the desktop's, else
    `~/Downloads`, never the home folder; `sanitize` keeps the last path component, drops control
    and Unicode format characters (Cf: bidi controls, zero-width and tag characters) and trims
    dots and whitespace from both ends, as Firefox and Chromium do. Page downloads still need no
    activation: a download the user typed the address of has none, and each download shows its
    own status line. *Why:* SECURITY.md's scope (files written outside the download folder), plan
    §17 (the core decides, the executable performs) and CLAUDE.md's rule to bound what a guest
    can make the host allocate. Evidence: "Review fixes".
84. **WPE WebKit is opened when first used, its web processes end with the last WPE page, and it
    leaves a page that asks to stay** (amends 62, 64, 72 and 78). (1) The crate links nothing of
    WebKit: its build script hands the library's path to the crate, and `WpeRuntime::new` opens
    it (`ffi::load`: `dlopen`, `RTLD_NOW | RTLD_LOCAL`) and finds the 114 functions the adapter
    calls through a table; every `ffi` function keeps its name and signature and panics before
    `load`. A `sig` session that opens no WPE page maps none of WebKit or the 72 libraries only it
    needs (GStreamer, libsoup, ICU, flite, …); the RPATH to the prefix and `DEP_WPEWEBKIT_RPATH`
    are gone (the library finds its dependencies through its own RPATH). This answers the
    review's threat to M7's figures (3c510fe had no WPE); building WebKit without speech
    synthesis was the alternative, and would have removed only flite. (2) The views get a
    `WebKitWebContext` of their own, made with the first view and ended with the last, with the
    document-browser cache model set before it is made: WebKit's default context keeps a closed
    page's web process up to 30 minutes for its site (its process cache, sized when a context is
    made from the model then), and after a cross-site navigation a prewarmed spare that only the
    context's end shuts down. The network process stays with the runtime: it holds the session's
    cookies, an ephemeral session's only copy. (3) WebKit's `script-dialog` for
    `BEFORE_UNLOAD_CONFIRM` is answered "leave", as Servo 0.5's default is: WPE's own answer
    (stay) cancelled the browser's `Navigate`, `Reload` and `Traverse` inside WebKit with no
    event once the user had touched the page, so the navigation stayed pending for ever.
    `alert`, `confirm` and `prompt` keep WPE's default (closed, cancelled). (4) The GLib watch
    thread is woken by the condition variable alone while it is parked there, and drains its
    interrupt only when it fired: two syscalls less per new set; context switches unchanged.
    (5) A hidden WPE page that animated keeps one thread of its web process waking about 120 times
    a second (WebKit's own; measured, not fixed); `engines.sh` bounds the tree in that state.
    *Why:* the reviews' findings; plan §21 ("a linked optional engine need not initialize until
    first use"), §26 (memory and wakeups measured across the tree), §20 (every accepted command
    ends in events: the engine contract) and §28's third question. Evidence: "Review fixes".
85. **A tab keeps hidden views by their cost: one of an engine with a process per view is kept
    alone** (amends 29). `EngineCapabilities::process_per_view` (additive; WPE true, the others
    false) says each view is an OS process of its own. Such a hidden view counts as all of
    `RETAINED_VIEWS` (2), so it is kept only as the tab's one hidden view; beyond the bound the
    farthest segments become records, as before (the nearest page stays instant to go back to,
    the older ones reload). Servo and Wasm tabs are unchanged. Not done: evicting a hidden view
    after some time, which would free the remaining web process too but needs a clock event the
    core doesn't have (it reads no clock). *Why:* plan §27 ("hidden WebViews retain excessive
    memory: add a tested eviction policy; acknowledge reload semantics") and §16 (bounded
    retention, records reload); the review's three-views-for-one-tab case. Evidence: "Review
    fixes".
86. **`shell.nix` brings the pinned Rust** (amends 59 and 79; plan §21: pin the toolchain). The
    pin's `rustc`, `cargo`, `clippy` and `rustfmt` 1.98.1 are the shell's first, so every machine
    builds with the workspace's `rust-version` through `tools/cargo` and `tools/heavy` alone, and
    the `PATH` workarounds of CLAUDE.md's louise-pc command and `bench/rewrite/build.sh` go
    (the latter keeps one for revisions whose `shell.nix` predates this). The pin's toolchain is
    the desktop's own (the same store paths), so Servo's artifacts in `target/` stay fresh: no
    build script tracks what the shell's environment changed. *Why:* plan §21, and decision 79's
    finding that louise-pc's own Rust (1.95) can't build the workspace. Evidence: "Review fixes".
87. **Each tab names the engine its page runs in, and the chromes draw it** (amends 68, which
    left `TabSnapshot::engine` out until a chrome showed it, and 75's options). (1) It is UI
    code: `sig`'s snapshot builder (`chrome::engine`) copies the configured name of the engine
    whose view a tab shows, browser state the core already exposes (`TabSummary::content`,
    `View::engine`), into sighurt-ui's `TabSnapshot::engine`, contained to 100 characters like
    an engine row's name.
    `sig:` pages, blank tabs, records and error pages have none, and `TabSnapshot::engine_label`
    applies that rule again on the chrome's side. No engine crate, nothing of sighurt-api and no
    code of the core changed. (2) The standard chrome draws it in `text_muted` at 12 px (scaled
    with `font-size`), on the title's baseline, inside the tab right before its close button, so
    the tab's and the close button's hit targets stay as they were; the title is cut before it.
    It shows only whole, and only if it fits in half the tab's room for text, less a gap,
    whatever the title, so tabs of one width agree: in narrow tabs a cut "se…" said nothing and
    left a title of "…", and the first version, which let a short title lend the label its room,
    labelled a Servo tab titled "A" beside unlabelled long-titled ones that looked like
    sig://home's (review; `tabs_of_one_width_agree_on_their_labels`). At the address
    field's right end (before the keymap's indicator) it names the active tab's engine if it
    fits in a quarter of the field; the field's text area ends before it, and it hides while
    the line is focused. The line chrome puts it, as small and muted, at the line's end if it
    fits in a quarter of the line. `Fonts::baseline` puts text of two sizes on one baseline,
    whatever the text. (3) `[chrome.standard] engine-label` and `[chrome.line] engine-label`, true
    by default, are checked by each chrome kind and apply live. (4) The accessibility tree
    describes each labelled tab as "Engine: <name>"; the diagnostics JSON gives each tab's
    `engine`. It repaints only when a tab's engine changes, never for a frame. *Why:* the user's
    request ("for this per tab display thing make sure it is part of the UI code and not part of
    the engine code"); plan §17 (snapshot strings contained), §26 (no repaint caused by page
    animation) and §28's first and fourth questions: which engine shows a page is the core's
    state, how it is shown the chrome's, and no renderer detail reaches the core's types.
    Evidence, on this desktop (RTX 3070, headless sway, GLES2) on 2 October 2026: sighurt-ui's
    `engine_labels_stay_on_their_tabs_at_every_scale` (at 100, 125, 150 and 200 %, tabs at the
    top and on the left, compact and at 18 px, every label pixel hits its own tab and no close
    button, the close buttons' pixels hit them, clicks on a label and a close button select and
    close), `the_address_fields_engine_label_never_overlaps_its_text`,
    `the_tree_names_each_tabs_engine`, `the_engine_label_through_the_trait`,
    `a_new_engine_repaints_its_label`, `engine_labels`; six
    new golden images (`engine-tabs-100`, `-200`, `engine-compact-150`, `engine-font-18-100`,
    `engine-left-125`, `engine-narrow-150`) and the line chrome's `the_engine_label` with
    `engine-100` and `engine-typing-150`, every existing golden image unchanged; `sig`'s
    `the_snapshot_contains_what_pages_say`; and engines.sh's new `labels` scenario, 16 of 16
    (Servo, WPE through a `[routing.wpe] hosts` rule and sig://home labelled servo, wpe and
    nothing in the diagnostics and on screen; `engine-label = false` while `sig` runs; the line
    chrome's "wpe" after a restart).
88. **Sighurt is licensed AGPL-3.0-or-later; the guest SDK and the example apps stay
    Apache-2.0.** The user's request ("change the licensing to AGPL instead of the current
    Apache license … still pay respects to Oxide's licensing requirements"); the version (3 or
    later) and the SDK's exception are the user's choices. `LICENSE` is the AGPL's text,
    `LICENSE-APACHE` is Oxide's license notice (the old `LICENSE`, unchanged) followed by the
    Apache License, Version 2.0, and `NOTICE` says what came from Oxide, that Oxide's code stays
    Apache-2.0 from Oxide, and the exceptions: `sighurt-sdk` and `examples/` (`license =
    "Apache-2.0"` in each manifest), so apps built with the SDK may use any license, as with
    upstream `oxide-sdk`; `sighurt-engine-servo` is `AGPL-3.0-or-later AND MPL-2.0`; WPE WebKit's
    libraries keep their own licenses. The workspace's `license` is `AGPL-3.0-or-later`, and
    `sig --version` names the license under the Oxide attribution. *Why it may:* Apache-2.0 code
    may be included in a work under GPLv3 or AGPLv3, which keeps its notices (Apache-2.0 §4);
    MPL-2.0 allows combination with the GPL family (MPL-2.0 §3.3); and every dependency in
    `Cargo.lock` was checked with `cargo metadata` on 17 October 2026: all are under licenses
    compatible with AGPLv3 (aws-lc-sys 0.45 and ring 0.17.14 no longer carry the OpenSSL
    license; mozjpeg's IJG, ffmpeg-next's WTFPL and webpki-roots' CDLA-Permissive-2.0 are
    permissive). The only contributor outside the Sighurt contributors besides Oxide's author
    is one Oxide-era commit, under Oxide's Apache-2.0.
89. **Sighurt gets a terminal frontend; the GUI is one of two frontends** (amends 2: Sighurt was
    Wayland-only; the GUI still is). The user's request: a terminal crate, a GUI crate and a
    shared core connecting both to the engines, in one binary `sig`, with no IPC, all of
    Sighurt's own code in Rust. The design is [TUI.md](TUI.md) (Fable's design, Astra's review,
    and §8's amendments, which win). The crates: `sighurt-core` stays pure; the new
    `sighurt-runtime` (the user's name) takes the engine registry, effects, services,
    persistence and diagnostics out of `sighurt-browser`; `sighurt-gui` is the Wayland owner
    loop; `sighurt-tui` is the terminal; `sig`'s `main` parses arguments and picks a frontend:
    `--dump` prints, `--gui`/`--tui` win, else a TTY on stdout (and `TERM` not `dumb`) opens the
    terminal, else the GUI (the user's choice). *Why:* the user's request; plan §4's layering
    carried to a second frontend (TUI.md §1); decision 27 keeps the core free of I/O.
90. **The terminal shows pages with the existing engines: graphics first, then text from a
    semantic snapshot** (the user's decision; no HTML/CSS engine of Sighurt's own). Graphics go
    through the kitty and sixel protocols from CPU frames of a second, offscreen presenter per
    engine (the plan's zero-readback criterion stays the GUI's; the terminal's bound is TUI.md
    §2 and §8.6). Text comes from an optional `semantic` capability of the engine contract:
    bounded snapshots and actions tied to their `DocumentGeneration` (§3, §8.4–§8.5), produced by
    DOM extraction in Servo and WPE (Servo 0.5's AccessKit tree lacks link URLs, field values
    and actions: §8.3). *Why:* the user's decision; §8's review.
91. **The terminal's defaults: `[tui] mode = "auto"`, a small default terminal key table, and
    `--dump` as text with link footnotes** (the user's answers; the default table amends the
    rule that keys live only in the user's configuration, for the terminal only, since a fresh
    terminal session would otherwise have no way to its command line or to quit).
92. **The terminal frontend parses what the terminal sends with a small parser of its own, and
    the offscreen presenters use the GPU when there is one** (TUI.md §9). Evidence: phase 0 on
    17 October 2026. `vte`, termwiz and crossterm each miss inputs the frontend needs (APC
    replies, kitty's base-layout key and colon fields, reply classification) or own stdin
    against the calloop loop (`experiments/tty-io/RESULTS.md`, 38 tests). Servo's offscreen
    context opens surfman's Mesa surfaceless backend with the hardware adapter (on NVIDIA through
    glvnd, also with `/dev/dri` hidden; llvmpipe only where no GPU driver is present), and WPE's
    terminal presenter is its existing SHM path without a Wayland lease, bounded by acknowledging
    frames at the terminal's rate (`experiments/tui-servo`, `experiments/wpe-cpu`).
93. **The CPU frame and the semantic snapshot are part of sighurt-api** (TUI.md §10; amends 22
    and 45, which kept `CpuTarget` in the Wasm engine). One pixel format, ARGB8888, for every CPU
    target; the semantic snapshot is bounded and checked by `SemanticSnapshot::check` before a
    frontend sees it; semantic actions reach the engine only through the core
    (`BrowserEvent::Semantic`), tied to the snapshot's document, and count as the user's
    activation only when the user's input caused them. *Why:* TUI.md §8.4–§8.5 and plan §28's
    second question (a delayed action identifies its document). Evidence: sighurt-api's
    `semantic` tests (tree shape, bounds, password values) and sighurt-core's
    `a_link_activated_in_text_is_a_click_on_it`, `what_a_frontend_does_on_its_own_is_no_click`
    and `an_action_for_a_replaced_document_or_a_hidden_view_is_dropped`; clippy clean on the
    workspace and, on louise-pc, with Servo and WPE.
94. **Which engine shows a site is the user's choice, made at runtime or in the configuration;
    the browser ships no compatibility list** (the user, 17 October 2026, after chatgpt.com and
    github.com failed in Servo 0.5: "Let the user choose when the browser should use servo and
    when it should use WPE … compatibility lists are not up to the browser but rather things
    that should live in a configuration"). The mechanisms: `[engines] default` and a `--engine
    <name>` argument for the engine `sig` starts with; `[routing.<engine>] hosts` for per-site
    choices; switching a page's engine at once with `page.open-in <engine>` and a new
    `page.switch-engine` (the next web engine), bound like any command and reachable from the
    chromes; Servo's off-by-default web-platform features through `[engines.servo]` settings.
    A measured site corpus (`bench/compat/`) shows what works where and yields an *example*
    configuration in the documentation, never a default. Engines stay compiled in; switching
    opens a view in another in-process runtime (no IPC). Upgrading Servo past 0.5 (0.7.0 was
    released 5 October 2026) is tried in parallel, in a worktree of its own, and adopted only
    on measured results.
95. **`page.switch-engine` reopens the page in the next web engine, and `sig --engine` is the
    engine a run starts with** (decision 94). (1) `page.switch-engine` reopens the page the tab
    shows in the next web engine, wrapping around, in the build's descriptor order, among those
    compiled in and enabled (`[engines] enabled`). It is `page.open-in` of that engine: the same
    history, and the view is chosen the same way, so the page's links stay until an address is
    typed. A web engine opens http(s) pages. That is `Descriptor::web` in sighurt-runtime,
    copied into `Environment::web_engines`; the core does not name engines. Servo and WPE are
    web engines; Wasm and the demo engine are not. A page still loading counts, including one
    whose view is only the tab's pending switch, so the cycle continues from the engine that is
    opening it. Fewer than two enabled web engines, or a page that is not http, https or a file,
    shows a notice and leaves the page. (2) `sig --engine <name>` overrides `[engines] default`
    for the run, including every configuration reload. An engine the build lacks, or one the
    configuration does not enable, is an error at startup that names the engines available.
    `--help` documents it. The core applies the override on top of the file
    (`set_starting_engine`); it is not written into the file. (3) A click on the engine label
    at the address field's right end, or at the end of the line chrome, runs the command. The
    hit is the text that is drawn; hovering it changes nothing else, and no golden image
    changes. (4) The terminal's built-in `[tui.keys]` binds `e` to it. The windowed frontend
    still binds nothing; the default file documents the command with an example binding
    (`ctrl-alt-e`). *Why:* decision 94: which engine shows a site is the user's choice, at
    runtime and in the configuration, and the browser ships no compatibility list. Evidence:
    sighurt-core's `switch_engine_cycles_web_engines_in_descriptor_order`,
    `switch_engine_leaves_the_page_and_says_why`, `switch_engine_matches_page_open_in`,
    `a_typed_address_after_switching_engines_routes_as_after_open_in`,
    `the_starting_engine_overrides_the_default_across_a_reload`,
    `starting_engine_overrides_the_configured_default`,
    `starting_engine_names_the_engines_it_can_be`,
    `a_rejected_starting_engine_fails_the_builtin_fallback_too`,
    `the_terminal_installs_its_default_keys_and_the_window_does_not` and
    `the_built_in_table_is_the_one_documented`; sighurt-api's
    `display_gives_the_canonical_string_back` and `bad_commands_are_errors`; sighurt-runtime's
    `the_core_learns_what_each_engine_honors`; sighurt-ui's
    `a_click_on_the_address_fields_engine_label_switches_engine`; sighurt-chrome-line's
    `a_click_on_the_lines_engine_label_switches_engine`; sighurt-browser's
    `engine_is_the_engine_this_run_starts_with`.
97. **The terminal frontend shipped graphics mode first, with Servo** (the user, 17 October 2026:
    try the TUI as soon as possible, in a kitty-protocol terminal; amends TUI.md §8's phase order,
    which put a demo-engine slice and text mode first). `sig` runs in the terminal when stdout is
    one and `TERM` is set and not `dumb`, and opens the window otherwise; `--tui` and `--gui`
    override, and `--tui` without a terminal or the `frontend-tui` feature is an error (exit 2).
    The terminal writes to its fd from one place per pass, only when the previous bytes have left
    (a write source while bytes wait, no timer), so no escape sequence is ever cut. In the
    terminal, stderr (the engines' logs) goes to `$XDG_STATE_HOME/sighurt/tui.log`. The top line
    shows only the active tab, so there is no tab click yet. The end-to-end tests run the real
    `sig` from `sighurt-browser/tests/tui.rs`. *Why:* the user's request; TUI.md §8.6 and §8.8.
    Evidence: the tests named under phase 2 above.
