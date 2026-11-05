# Hypotheses

Every change that is meant to make Sighurt faster or lighter gets an entry here **before** its
build is measured (README, "Method"): what should change, on which workloads and metrics, in which
direction and why, and which other metrics must not get worse. After the measurement, the entry
gets the two result sets it compared and the outcome: *supported*, *not supported* (the primary
metrics did not move beyond noise, or moved the wrong way) or *rejected with a regression*
(something else got worse beyond noise; the decision then goes into the decisions log of
`docs/rewrite/STATUS.md`).

Primary metrics are confirmatory; anything else `bench/compare` finds is a new hypothesis to
re-test, not a result. "Beyond noise" is `bench/compare`'s verdict: p < 0.05 and the medians
differ by more than the larger IQR.

The baseline numbers each hypothesis starts from are in [`results/BASELINE.md`](results/BASELINE.md).

**Amended on 27 September 2026, before any build of the rewrite was measured** (the review of
the harness): `ipc_out_mib_per_s` measures what engine *processes* write, so for the in-process
rewrite it is 0 by construction and says nothing about copies; `bench/compare` leaves it and the
`_browser`/`_engines` split out when the builds place engines differently. H2 and H8 now name
the metrics that do measure it. The chrome-input latencies of H5 and H9 are now
`input_to_pixels` (screenshots of the chrome), since GPUI's draw log misses the redraws a key
causes (README, "Known limits"). H10 and H11 are new, for the two workloads added then.

## Open

### H-UI1: Hidden chrome surfaces hold no SHM buffers

- **Seen:** after `sig://home`, then an engine page, the chrome's `page` surface is unmapped but
  keeps its free buffers (a page-sized buffer each).
- **Change:** `Swapchain::trim`: when a target is unmapped and every buffer is free, destroy
  them (on unmap, and on the release that makes the last one free).
- **Prediction:** the diagnostics' `buffers.bytes` of the unmapped chrome `page` target goes from
  at least one page-sized buffer to 0; total chrome `buffer_bytes` falls by that much.
- **Guards:** time from `tab.new` to home shown (one allocation), `paints_*` counts, the
  swapchain unit tests.
- **Finding while measuring:** trimming free buffers alone changed nothing (3 runs each, identical
  to the baseline): sway keeps the last submitted buffer of an unmapped surface attached, so the
  hidden surface holds one buffer in `Submitted`, never a free one. The change therefore also
  attaches a null buffer and commits when an SHM target is unmapped because it is not wanted
  (`want_shown` false), so the compositor releases it and the release trims.
- **Outcome:** supported. `sig` (demo, 1280x800, scale 1), `sig://home` then `demo://pattern`
  and back, `ui1.sh` in tools/headless-sway, three runs per build, every run identical (no
  spread): after switching to the engine page total `buffer_bytes` 8,089,600 to 4,526,080 and
  hidden-target bytes 3,563,520 to 0; after returning to home 11,653,120 to 4,526,080.
  `chrome_paints` unchanged (6, 7, 9).

### H-UI2: The first chrome frame is the first configure's size

- **Seen:** on `home`, startup paints the chrome twice. The triage (31 October 2026) measured
  6 chrome paints; the hypothesis list says every surface has `permits=2`, `presented=1`,
  `discarded=1` (three surfaces: page, chrome, status).
- **Change:** do not paint, and do not create chrome targets, before a configure names a size.
  A configure that leaves the size to the client (both axes zero) is answered with a root
  commit only. The first configure that names a size is the first placement and the first
  paint.
- **Prediction:** `chrome_paints` on `home` falls from 6 to 3, and the sum of `discarded`
  over chrome targets falls from 3 to 0. Each chrome target keeps `presented=1`.
- **Guards:** the window is 1280×800 (the compositor's size, not the 1024×768 fallback);
  `paints_page`, `paints_chrome` and `paints_status` are each 1; after the first second,
  further chrome paints stay 0; `placement_errors` stays 0.
- **Workload:** `sig` with the demo engine, `sig://home`, 1280×800 at scale 1, in
  `tools/headless-sway` (pixman). Diagnostics: `SIG_DIAG=1` chrome lines and `kill -USR1`
  after the session file exists. Three runs. Written before the change.
- **Baseline:** three runs, every count identical. `chrome_paints` 6,
  `paints_page` / `paints_chrome` / `paints_status` each 2, `discarded` 3, `presented` 3,
  `permits` 6, `placements` 2, `placement_errors` 0. Final surfaces page 1280×696, chrome
  1280×84, status 1280×20. `max_chrome_paint_us` 34612, 36931, 34842. A protocol trace (not a
  repetition) showed the first `xdg_toplevel.configure` at 0×0, the client setting window
  geometry to 1024×768, then a configure at 1280×800.
- **After:** three runs, every count identical. `chrome_paints` 3, each paint counter 1,
  `discarded` 0, `presented` 3, `permits` 3, `placements` 1, `placement_errors` 0, same final
  size, one paint batch. `max_chrome_paint_us` 35918, 36716, 38796 (medians 34842 → 36716,
  inside the larger IQR, so not a move).
- **Outcome:** supported. A sizeless configure commits the root at the client size and does not
  report `WindowConfigured`. The first configure that names a size is the first placement and
  the first paint. If none names a size within 20 ms, the client default is reported.

### H-OWN1: The owner frame path allocates nothing of its own

- **Seen:** every presented frame on the owner thread builds a host diagnostics report
  (`Host::diagnostics`: a `Vec` of every target), copies the engine name in
  `Diagnostics::readbacks` (`BTreeMap::entry` takes an owned key), and allocates fresh `Vec`s in
  `Registry::due`, `Session::pump` (the event buffer), `Gui::placement` and
  `ChromeTargets::place`. `Host::permits` also allocates its id list and the permit `Vec`.
- **Change:** read buffer peaks through a counter accessor that does not build the report; look
  up an engine already in the readbacks map without copying its name; reuse the `Vec`s for due
  engines, pump events, permits, placement and the chrome's placing.
- **Prediction:** over presented frames 41–160 of `demo://animation` (120 frames, after startup),
  heap allocations per frame fall, and process CPU time per frame falls. The floor is the demo
  engine, which this change does not touch: each animation frame allocates a page-width
  background row and a page-width run for the sweeping bar (`Canvas::rect`), and `coalesce`
  allocates a set and a keep-list (`engines/sighurt-engine-demo`). The total does not reach 0.
  The owner loop's own allocations in the list above go to 0, which is several allocations and
  one diagnostics `Vec` per frame today.
- **Guards:** the window still presents (120 frames in the sample); `chrome_paints` stays at the
  startup count (an animation does not repaint the chrome); `present_errors` stays 0;
  `placement_errors` stays 0.
- **Workload:** `sig` with the demo engine, `demo://animation`, 1280×800 at scale 1, in
  `tools/headless-sway` (pixman). A counting global allocator (installed only for this
  measurement) records allocations, allocated bytes and `CLOCK_PROCESS_CPUTIME_ID` from the end
  of presented frame 40 to the end of frame 160. `RUST_LOG=error`, `SIG_DIAG` unset, so the
  sample is not the log. Three runs. Written before the change.
- **Baseline:** three runs of the measurement build (counting allocator installed, this change not
  in it). `sig-alloc` over frames 41–160: allocs 4844, 4844, 4843; bytes 1,898,691, 1,898,691,
  1,893,571; `cpu_ns` 104,460,485, 110,896,492, 98,843,583. That is 40.37 allocations and about
  15.8 KiB per frame. Exit diagnostics: `chrome_paints` 2, `present_errors` 0,
  `placement_errors` 0, `placements` 1, `page_frames_presented` 169, 168, 169.
- **After:** three runs, allocations and bytes identical. allocs 1719, 1719, 1719; bytes
  1,320,430, 1,320,430, 1,320,430; `cpu_ns` 94,017,133, 84,854,974, 104,767,885. That is 14.33
  allocations and 11.0 KiB per frame. Exit diagnostics: `chrome_paints` 2, `present_errors` 0,
  `placement_errors` 0, `placements` 1, `page_frames_presented` 161, 168, 168.
- **Outcome:** supported. Medians 4844 → 1719 allocations and 1,898,691 → 1,320,430 bytes over
  the 120-frame window (26.0 fewer allocations and 4.8 KiB less per frame). The baseline
  allocation counts differ by 1, so the drop is outside the noise. CPU medians 104,460,485 →
  94,017,133 ns; the drop sits inside the larger run-to-run span, so CPU is not a move and not
  a regression. The counting allocator was removed after the measurement. What remains per
  frame is outside the lists above (the demo painter's row and bar, `coalesce`, and the
  Wayland client).

### H-SNAP1: A snapshot costs no more than a copy for ordinary strings

- **Seen:** every core change rebuilds the snapshot, and `contain` rebuilt each string char by
  char (50 tabs of 32 Ki-character addresses: 2.0 ms per snapshot plus compare).
- **Change:** a byte scan decides whether a string is already contained (printable ASCII within
  its bound); only others are rebuilt.
- **Prediction:** the per-snapshot time (test `snapshot_cost_with_many_long_tabs`, release)
  falls by at least half.
- **Outcome:** supported: median 2038 us to 763-806 us (3 runs), kept (written together with its
  baseline, not before it).

### Owner-loop review (3 November 2026): method

Five hypotheses from an independent review of the GUI owner loop (`frontends/sighurt-gui/src/app.rs`,
`chrome.rs`, `crates/sighurt-runtime/src/session.rs`), written on 3 November 2026 after a
probe (one 5 s run of each workload, flat `perf record` profiles of the main thread) and before
the measured runs. Every number is from the debug `sig` the build commands make
(`--no-default-features --features engine-demo,engine-wasm,chrome-line`: dependencies at
opt-level 3, the workspace's own crates unoptimized), so owner-loop code costs more here than in
a release build; the ratios between workloads and the direction are what carry over.

- **Workloads**, 1280×800 at scale 1, in `tools/headless-sway` (pixman), isolated XDG
  directories, `RUST_LOG=error`, `SIG_DIAG` unset: `raf` (the Wasm raf-demo alone); `anim50`
  (49 `demo://pattern/N/<16 000-character path>` tabs and `demo://animation/<same>` last,
  selected with `tab.last`, so its view is the last in the session's map); `idle50` (50 such
  pattern tabs, nothing moving); `hover50` (`demo://resize/<long>` first and active, 49 pattern
  tabs; the pointer moves over the page every 10 ms, and each motion is a status-text change, as
  hovering links is); `chrome50` (50 pattern tabs, the pointer moves along the tab strip every
  10 ms).
- **Method:** start `sig`, wait 4 s (6 s for `anim50`), start the pointer if any, wait 2 s, then
  10 s of `perf stat -e task-clock,instructions:u` on the main thread (`-t`, the owner thread) and
  on the process, diagnostics (`--diagnostics`, SIGUSR1) and an allocation count
  (an `LD_PRELOAD` counter of malloc, calloc, realloc and the aligned family, per process and on
  the main thread, read with SIGUSR2; heaptrack and valgrind are not installed) at both ends.
  Three runs per build, the builds interleaved. The diagnostics gained four counters for this:
  `placement_builds` and `placement_ns` (placements built and compared, applied or not) and
  `snapshots` and `snapshot_ns` (snapshots built for the chrome).
- **Builds compared:** `base` is HEAD plus only those four counters; `after` adds H-OWN2,
  H-SNAP2 and H-OWN4. Both were built one after the other from the same tree (other agents were
  editing engines meanwhile), so they differ only in `app.rs` and `chrome.rs`. H-OWN3 was
  measured afterwards the same way, `session.rs` as at HEAD (`nomap`) against the map (`map`).
  Results and logs: `/data/sig-builds/test-out/opt-gui-bench/` (`ab1` all workloads, `ab2`–`ab4`
  `anim50`, `map1` H-OWN3, `ab3/prof-after-*` the profiles after). On `anim50`, `tab.last` sent
  by a freshly plugged-in wtype keyboard was sometimes missed by both builds alike (`sig` had not
  bound the new device yet); such runs show no animation and are left out, and the later runs
  keep a keyboard plugged in, as headless.sh does. Seven `base` and six `after` runs counted.

### H-OWN2: The placement is built only when what feeds it changed

- **Seen:** every pass of every iteration builds the whole placement (every view, the retiring
  targets and the chrome's surfaces) and compares it with the one applied last: about 4 builds
  per animation frame (raf 1269 builds for 305 frames), 10 µs each with one view and 25 µs with
  50 (`placement_ns`), about 7 % of the owner thread's CPU on `anim50`. Keyboard focus is also
  recomputed from the chrome's layout every pass.
- **Change:** the GUI keeps a dirty flag, set when a view is opened, shown or hidden, or
  forgotten, when the window is configured, when the chrome handles any event (its layout is
  read after every `handle`) and while a retiring target is listed (its lease can drop without an
  event). A placement is built only then; the comparison with the last applied placement stays as
  the safety net. A refused placement leaves the flag set, so it is retried as before. Keyboard
  focus is recomputed only when the core's focus, the shown view, the chrome's text field or the
  chrome's layout changed.
- **Prediction:** on `raf` and `anim50`, `placement_builds` per presented frame falls from about
  4 to under 0.1, `placement_ns` by at least 90 %, and the owner thread's instructions per
  presented frame by at least 3 % on `anim50` (by roughly the placement's share).
- **Guards:** `placements` and `placement_errors` unchanged, no chrome paint during the
  animations, frames presented per second not lower, headless.sh 78/78 (its resize, scale,
  tab-switch and focus checks exercise every path that sets the flag).
- **Outcome:** supported. `placement_builds` per 10 s: `raf` 2482, 2458, 2462 → 0, 0, 0;
  `anim50` about 2330 → 0 (`placement_ns` 63.7 ms → 0); `chrome50` 2633–2668 → 806–811 (once
  per chrome event; `placement_ns` 21.2 → 10.1 ms median); `idle50` 1 → 0. Owner-thread
  instructions per presented frame on `anim50`: median 1.2200 M (7 runs, 1.2163–1.2218) →
  1.0706 M (6 runs, 1.0662–1.0719), −12.2 %, more than the placement's measured share (the focus
  sync and the walks inside the placement went with it); task-clock per frame 1.537 → 1.431 ms
  median, inside the spread (1.435–1.604 against 1.183–1.521). `raf`: 7.548 → 7.522 M
  instructions per frame (−0.35 %, outside both spreads of 0.17 % and 0.13 %); task-clock 1679
  → 1576 ms median, inside the spread. On `chrome50` the instructions moved −0.7 %, inside the
  spread (the chrome's own painting dominates). Guards: `placements` and `placement_errors`
  0 → 0 in every window, chrome paints 0 during `raf` and `anim50`, frames presented the same
  (`raf` 605–607, `anim50` 579–592 per 10 s), main-thread allocations the same (`raf` 264.3–264.7 k,
  `anim50` 29.5–29.7 k), headless.sh 78/78. Kept.

### H-OWN3: Lookups by target are not worth a map

- **Seen:** `Gui::surface_of` (at most 8 leases) and `Session::view_of` (every view) are linear
  scans, on every permit and every pointer event; `record_buffers` scans the leases for each
  target after every presenting pass.
- **Prediction (to decide whether to change anything):** with 50 views and the animated view
  last in the map, these scans together cost under 2 % of the owner thread's samples on `anim50`
  and `hover50`. If so, a map kept in step with the views costs more code (and a second place to
  keep right) than it saves, and nothing changes. Above 2 %, a `TargetId → ViewId` map in the
  session.
- **Measured:** above 2 %, so the prediction does not hold. After H-OWN2, the flat profile of
  the owner thread on `anim50` (animated view last; about 505 samples in each of the two PMU
  groups perf reports) puts the walks of the session's view map (`view_of` per permit and
  `shown_view` per pass) at 5.9 % and 2.3 % (about 4 %), `record_buffers` and `for_each_buffer`
  at about 2 % (mostly the host's per-target buffer statistics, which this change does not
  touch), and `surface_of` at 0 %. On `hover50` (active view first) the walks are 0.8 %.
- **Change (by the rule above):** `Session::view_of` reads a `TargetId → ViewId` map kept in step
  with the views where a view is recorded (`open_view`) and forgotten (`forget_view`), the only
  two places; `shown_view` and `record_buffers` stay as they are.
- **Outcome:** supported, small. `anim50` instructions per frame 1.0717 M median (1.0663–1.0856)
  → 1.0497 M (1.0472–1.0521), −2.1 %, every run with the map below every run without; task-clock
  inside the spread. `hover50` 44.79 → 44.63 M per frame (−0.4 %, inside the spread). In a
  release build the walk of 50 entries is well under a microsecond a frame; kept because it is
  a few lines, tested (`a_refused_open_forgets_the_frontends_target`,
  `quitting_writes_the_session_once_and_the_exit_report_sees_every_view`) and removes the one
  per-event scan that grows with the tab count.

### H-SNAP2: A snapshot reuses the contained strings of unchanged tabs

- **Seen:** on `hover50` the owner thread is saturated (100 % of a CPU): each status change
  rebuilds the snapshot, and building it, mostly the containment scan of 50 unchanged 16 000-byte
  addresses (`is_contained` in `Snapshot::contain`), takes 10.4 ms (`snapshot_ns`: 4.25 s of the
  5 s window, 409 snapshots). H-SNAP1 made the scan cheaper; it is still linear in the bytes of
  every tab.
- **Change:** the GUI keeps, per tab, the core's strings it last contained and the contained
  tab. A tab whose title, address, engine, switching engine and error are byte-equal to last time
  (compared with `memcmp`, not scanned per character) reuses its contained tab; only the others
  go through `Snapshot::contain` (a snapshot of just them, so the bounds stay the UI crate's).
  `HomeInfo` is left as it is unless the profile after the change shows it.
- **Prediction:** on `hover50` `snapshot_ns` per snapshot falls by at least 80 %, the owner
  thread is no longer saturated (task-clock under 80 % of the window) and the page and status
  repaint more often (status paints per second up). Allocations per snapshot do not rise.
- **Guards:** the chrome sees the same snapshot (a unit test compares the reused snapshot with a
  freshly contained one through title, address and error changes, and with strings that need
  containing); `the_snapshot_contains_what_pages_say`; headless.sh 78/78.
- **Outcome:** supported. `hover50`, three runs each: `snapshot_ns` per snapshot 10.21, 10.35,
  10.67 ms → 0.354, 0.356, 0.368 ms (−96.6 % median). The owner thread is no longer saturated:
  task-clock 10 002 ms of the 10 s window in every base run → 4581, 4666, 4674 ms; instructions
  137.3, 135.8, 132.4 G → 26.88, 26.80, 26.73 G (−80 %). Status and page repaints per 10 s
  358, 354, 330 → 601, 601, 598. Main-thread allocations per snapshot 3586 → 3738 (+4 %): the
  snapshot itself copies the same strings as before (one copy per tab string, from the cache
  instead of the core), and the rise is the repaints that now follow (0.83 paints per snapshot
  against 0.44; solving for both gives about 3400 allocations per snapshot and 390 per
  status-and-page repaint, so not a rise in the snapshot's own). After the change `home_info`
  is 0.08 % of the owner thread's samples on `hover50`, so `HomeInfo` is not cached: an
  invalidation key (engine states, configuration) would cost more than the dozen small
  allocations it saves. Guards: `a_reused_tab_is_what_containing_it_again_gives` (the reused
  snapshot equals a freshly contained one after start, commits with a 40 000-character
  address cut to its bound, a bidi-and-newline title set and reset, a failed first navigation
  shown as an error tab, and a closed tab that moves the rest; and it counts the tabs
  contained each step: 4, 0, 2, 0, 1, 1, 1, 1, 0, 0), `the_snapshot_contains_what_pages_say`,
  headless.sh 78/78. The cache holds a second copy only of tabs whose strings containing
  changed. Kept. Still linear in the tabs: a byte comparison and a copy of each tab's strings
  per snapshot (`memcmp`/`memcpy`, fast in any build), and the chrome compares the whole
  snapshot again.

### H-OWN4: Draining host events reuses its list

- **Seen:** `drain_host_events` makes a new `Vec` each pass, which allocates whenever there is an
  event (pointer motion: about once per pass on `hover50` and `chrome50`).
- **Change:** a list kept in the app and reused, as the permits are.
- **Prediction:** main-thread allocations per input event fall by about one on `chrome50`
  (where nothing else here changes allocations). Wasted work removed; kept unless it costs
  something.
- **Outcome:** not resolvable, kept. `chrome50` main-thread allocations per input event 377.8
  median (373.7–379.5) → 373.9 (370.3–377.4): the expected one allocation per pass with events
  (about 810 of about 305 000 in a window) is inside the run-to-run spread, which the chrome's
  painting sets. The change is three lines with no cost, like the permit list's.

### H-OWN5: An engine command is not cloned for a refusal that rarely comes

- **Seen:** `Session::engine_command` clones every command so that a refusal can carry it back
  to the core (`BrowserEvent::CommandFailed`).
- **Prediction (to decide whether to change anything):** the clone is free for pointer input
  (no heap data: a pointer event is plain values) and two small strings for a key, and the large
  payloads (a URL, a service result) come once per navigation or request; so on `hover50` it
  is not in the profile. The core reads the command of a refusal (a refused navigation fails
  that navigation), and `EngineRuntime::command` takes it by value, so it cannot be cloned only on
  the error path without changing the API. If the profile agrees, nothing changes.
- **Outcome:** as predicted; nothing changed. On `hover50` after H-SNAP2 the clone and
  `engine_command` are 0.03 % of the owner thread's samples. `EngineCommand::Input` of a pointer
  event clones no heap data.

Candidates from the IPC-removal session (`results/IPC-REMOVAL.md`, "Surprises and regressions"),
each seen there but not predicted, so each still to be tested:

### H12: Tab churn's retained memory is allocator and cache retention

- **Seen:** `tab-churn` leaves 58 MiB after 10 cycles (baseline 13) and about 69 MiB after 60,
  levelling off after about 20 (an exploratory 60-cycle run, 3 times).
- **Change:** let jemalloc (Servo's, the allocator of all of `sig`) return dirty pages sooner, or
  purge after a view closes.
- **Prediction:** `after.pss_growth_mib` on `tab-churn` falls to the baseline's 13–26 MiB.
- **Guards:** `active.cpu_pct` and `input_to_pixels` on `tab-churn` and `many-tabs`.

### H13: Static Wasm guests sleep between steps

- **Seen:** `wasm-gradient`, `wasm-timer` and `wasm-busy` wake the process about 250–300 times a
  second while the owner loop sleeps (0.1/s): about four engine-thread wakeups per `on_frame` step,
  where the baseline's whole `sig-wasm` needed one.
- **Change:** no epoch ticker between steps (a deadline per step instead of 10 ms ticks while a
  guest runs), and a direct hand-off to the pool.
- **Prediction:** `steady.wakeups_per_s` ≤ 130 on `wasm-gradient`, `wasm-timer` and `wasm-hello`.
- **Guards:** `wasm-busy` and `wasm-flood` keep their time limits (no anomaly), `steady.cpu_pct`.

### H14: Wasm frames damage only what changed

- **Seen:** on `wasm-raf` the compositor's CPU doubled (14.7 → 28.2 %) and the GPU's utilization
  rose 12.8 → 23.0 %: every frame is a whole 1280 × 696 shared-memory buffer for sway to upload,
  a page-sized copy per frame (§28, question 3), now in the compositor.
- **Change:** damage only the rectangles the draw list changed (`wl_surface.damage_buffer`).
- **Prediction:** `steady.compositor_cpu_pct` on `wasm-raf` ≤ 15 % and `steady.gpu_util_pct`
  ≤ 13 %.
- **Guards:** `wasm-raf` `steady.cpu_pct`, frames presented.
- **Done as H19** (below): the Wasm engine damages the rectangles its draw list changed
  (`engines/sighurt-engine-wasm/src/damage.rs`), measured on this desktop on 31 October 2026.
  The louise-pc bench numbers of this entry were not repeated.

### H15: A flooding guest doesn't hold the owner thread

- **Seen:** `wasm-flood`'s raster takes 63 ms a frame on the owner thread, and chrome input waits
  for it: `input_to_pixels` 53 ms p50, 84 ms p95.
- **Change:** present Wasm frames off the owner thread (decision 45's open item: a `Send` handle to
  the lent buffer).
- **Prediction:** `wasm-flood` `active.input_to_pixels_p50_ms` ≤ 20 ms.
- **Guards:** `wasm-flood` PSS and CPU, no anomaly.

### H16: Frame pacing loses no frame

- **Seen:** Servo animations present 58.2 frames a second at a 60 Hz output (the baseline 59.2).
- **Change:** present when Servo's frame becomes ready within the interval, rather than waiting for
  the next callback (to be confirmed first by timing callbacks against `FrameReady`).
- **Prediction:** `steady.fps_delivered` ≥ 59 on `css-animation`, `canvas`, `webgl`.
- **Guards:** `steady.cpu_pct`, `steady.wakeups_per_s`.

### H17: Showing a tab keeps its surface

- **Seen:** a hidden Servo view loses its subsurface (decision 31); its first show costs 13–17 ms
  of `present`, later ones 1.6–2.9 ms of swap, against 0.1 ms in steady animation.
- **Change:** unmap a hidden view's surface instead of destroying it.
- **Prediction:** `many-tabs` `active.present_p50_ms` ≤ 2 ms.
- **Guards:** `many-tabs` PSS and GPU memory, hidden views present nothing.

### H18: A running animation doesn't grow memory

- **Seen:** left alone for two minutes, the rewrite's RSS on `css-animation` rose steadily by about
  1.1 MiB a minute (217.7 → 219.6 MiB; the baseline about 0.5 MiB a minute), one exploratory run
  each; `wasm-raf`'s moved up and down within 3 MiB.
- **Test first:** 30 minutes of `css-animation` and `webgl` (a workload of its own), RSS and PSS
  sampled as usual: does it level off?
- **Prediction (if it is retention, not a leak):** it levels off within 10 minutes, under 20 MiB
  above the first minute.

### H19: Wasm frames damage only the rects the draw list changed (H14, this machine)

Written on 31 October 2026, before any code change and before this session's measurement.
H14's numbers (sway 14.7 % → 28.2 % on `wasm-raf`) were taken on louise-pc with the full bench.
This session repeats the question on this machine, with `sig` built
`--no-default-features --features engine-wasm`, raf-demo, 30 s, three repetitions, medians and
spread. The method is the harness's: sway and `sig` CPU are `(utime + stime) / wall`.

- **Metric:** sway's CPU % over 30 s of raf-demo, and the damaged buffer pixels per submitted
  frame (whole buffer today: `w * h` from `SIG_DIAG=1` frame lines).
- **Direction:** down.
- **Expected size:** sway's CPU falls by at least 30 % relative to this machine's whole-surface
  baseline, toward the IPC-removal baseline's 14.7 % (a full upload of a ~1280×696 frame at 60 Hz
  is what doubled it). Damaged pixels per frame fall under 25 % of the buffer: raf-demo's list
  changes the ball, its velocity line and the position text, not the clear. Raster and the copy
  into the SHM buffer cover only that region when the buffer still holds a frame the unchanged
  pixels match (its age); otherwise the whole buffer is painted.
- **Guards:** `sig`'s CPU over the same 30 s does not rise by more than the baseline's spread,
  and presented frames stay at the output rate (at least 50/s). A wrong damage rect is a
  corrupted frame, so the partial paint must match a full paint in a unit test and headless.sh
  must still pass.

Measured on 31 October 2026, before the damage change, debug `sig`
(`--no-default-features --features engine-wasm`), raf-demo, headless sway gles2, 1280×800,
30 s × 3. The page buffer is 1280×696 (890880 px) and every frame damages all of it
(`damage_frac` 1.0, from `w * h` on the `SIG_DIAG=1` frame lines; there is no damage line yet).

| run | sway CPU % | sig CPU % | frames | fps |
| --- | --- | --- | --- | --- |
| 1 | 22.83 | 64.77 | 1737 | 57.90 |
| 2 | 22.73 | 58.23 | 1738 | 57.93 |
| 3 | 22.73 | 68.20 | 1737 | 57.90 |

Sway median 22.73 % (spread 0.10). sig median 64.77 % (spread 9.97); this is a debug build, so
the sig number is not the release figure from louise-pc. Frames stay at the output rate.
A kept result needs sway's median down by more than that 0.10 spread (the prediction is at
least 30 % relative, to about 15.9 % or below), damaged pixels under 25 % of the buffer, sig's
median not above 64.77 + 9.97, and fps still at least 50.

**Outcome** (same binary kind, guest, renderer and window, measured after the change on
31 October 2026, three runs): **Supported.**

| run | sway CPU % | sig CPU % | frames | fps | damage px | damage frac |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 9.37 | 12.93 | 1783 | 59.43 | 6742 | 0.0076 |
| 2 | 11.30 | 18.53 | 1790 | 59.67 | 6742 | 0.0076 |
| 3 | 12.07 | 18.90 | 1785 | 59.50 | 6742 | 0.0076 |

Sway median 11.30 % (spread 2.70), down 11.43 points, 50 % relative to 22.73 %, past the 30 %
prediction and the 0.10 baseline spread. Every run's worst sway (12.07 %) is still under the
baseline. Damaged pixels are the median of the `SIG_DIAG=1` `damage` lines in the 30 s window:
6742 of 890880, 0.76 % of the buffer. In run 2, 1921 of 1922 damage lines had buffer age 1 and
1894 named two rectangles; two lines were a new buffer (age 0, the whole frame). sig median
18.53 % (spread 5.97), down from 64.77 %, inside the guard. fps median 59.50, still at the
output rate. Headless screenshots of raf-demo show one ball and intact text, and gradient-demo
still presents only when its layout changes (4 submissions). Kept.

### H20: `api_download_url` does not start a thread per call

Written on 31 October 2026, before any code change. `docs/rewrite/optimization-hypotheses.md`
says a guest calling `api_download_url` N times per `on_frame` starts N OS threads at
`download.rs:113`. The file at that line now queues the request (at most 4, at most 64 MiB of
guest bytes) and the view hands each one to the browser as `Service::Download`, which the core
throttles to one a second per tab and `sighurt-runtime` performs on a thread of its own. Measure
before deciding the line is already gone.

- **Metric:** OS threads and VmRSS of the process after a guest calls `api_download_url` N times
  in one turn, for N = 1 and N = 32.
- **Direction:** both flat in N.
- **Expected size:** the engine itself starts 0 download threads (the requests are queued). In
  the browser, at most one new download thread per second per tab, so thread count and RSS at
  N = 32 stay within a thread or two and 10 MiB of N = 1. If the baseline already does this,
  there is nothing to change.
- **Guards:** a download the browser accepts is still performed; a fifth queued request still
  fails; a worker still cannot download.

Measured on 31 October 2026 with `a_burst_of_download_urls_starts_no_thread`, before any change
to `download.rs`. A guest calling `api_download_url` 32 times in `start_app` produced 4
`Service::Download` requests (the queue cap; the 5th and the rest fail in the guest, which
`downloads_are_the_browsers_to_make` already locks). The process went from 22 threads to 22,
none of them named `sighurt-download`, and VmRSS from 50060 KiB to 50640 KiB (+580 KiB). The
engine starts no download thread, and thread count and RSS do not grow with N past the queue of
4. Nothing to change. The burst test stays, so a later path that spawns a thread per call fails
it.

### Optimization session (4 November 2026): method

Written on 4 November 2026 on tensor-ash (Intel Core i5-12400F, RTX 3070, NVIDIA 595.104.02,
Linux 6.18.57), branch `opt2` off a89465c. Every `sig` measured is a **release** build
(`tools/cargo build --release -p sighurt-browser --no-default-features --features
engine-demo,render-wgpu,render-vulkan`), the build before a change and the build after it copied
side by side and run interleaved in one private compositor (`tools/heavy tools/headless-sway
[--renderer gles2] --`), 1280×800 at scale 1, every run with fresh XDG directories. `perf` cannot
open events in this session's sandbox, so where a profile would have said where time goes,
temporary timing lines (never committed) did.

**A finding before any hypothesis:** H-UI2's `max_chrome_paint_us` of about 35 ms is a debug
build's. In a release build the whole startup is about 7 ms (`bench/startup/startup.sh`, probe
of 3 runs: exec to the first chrome surface 5.0 ms, to `sig://home`'s page surface 7.0 ms; the
first paint of that 1280×696 surface 2.3 ms, of which 0.4 ms is the scene and the cold shaping);
the debug build takes 57 ms to the same page surface, 38 ms of it in that one render
(`/data/sig-builds/test-out/opt2/start-probe-home`). A timeline of temporary probes (three runs,
`start-probe2`) splits the release startup into: exec to `main` 1.1 ms (the exec, page faults
and Rust's start; the dynamic loader is 0.05 ms by `LD_DEBUG=statistics`), the Wayland
connection 0.2 ms, the configuration 0.2 ms, **the font scan 2.3 ms** (`FontSystem::new`: fontdb
reading the metadata of 125 faces; 2.4–3.1 ms in a release test of the scan alone), the
renderer, the chrome and the window 0.05 ms, then 0.4 ms waiting for the first configure, and
the first paints.

### H-START1: The font scan overlaps the connection and the first configure

- **Change:** the GUI starts the font scan on a thread of its own as `run` begins, before it
  connects to the compositor, and creates the window (whose first commit asks the compositor
  for a configure) before it waits for the scan. The connection, the configuration, the engine
  registry and the configure's round trip then happen while the fonts are read, instead of
  after. Nothing else changes: the same `FontSystem`, made by the same call, is handed to the
  chrome and the renderer.
- **Prediction:** on `demo://pattern` and on `sig://home`, the time from exec to the first
  chrome line falls by 0.5 to 0.9 ms (the connection, configuration and configure wait measured
  above), and the time to the page surface of `sig://home` by as much. Ten runs per build,
  interleaved, a change counts when the medians differ by more than the larger IQR.
- **Guards:** the same paints (`chrome_paints` 3 on `sig://home`, 2 on `demo://pattern`), the
  chrome screenshots of headless.sh unchanged (78/78), no thread left running after startup (the
  scan's thread ends when the scan does).
- **Outcome:** not supported; reverted. `startup.sh`, ten runs a build, interleaved
  (`/data/sig-builds/test-out/opt2/start1-pattern`, `start1-home`), medians (min–max):
  `demo://pattern` exec to the first chrome surface 5.00 (4.70–6.40) → 5.10 (4.30–6.70) ms, to
  the page's first frame 6.00 (5.40–6.50) → 6.20 (5.50–6.50) ms; `sig://home` first chrome
  surface 4.85 (4.40–7.00) → 5.80 (4.50–7.20) ms, its page surface 6.85 (6.70–7.10) →
  7.15 (6.50–7.50) ms. Nothing moved beyond the spread, and the medians moved the wrong way. The
  configure that follows the window's first commit is sizeless (H-UI2), so the round trip that
  matters starts only when the loop answers it, after the fonts; and the scan on a second thread
  ran no faster than on the first. What is left of startup is the scan itself (2.3 ms of about
  7), which only a cache of the fonts' metadata kept between runs would remove: not done, an
  open item at that size.

### H-CPU1: The CPU backend paints in the buffer's byte order

- **Seen:** the CPU backend paints with tiny-skia in R, G, B, A byte order, then walks the whole
  buffer again to swap red and blue (`swap_red_blue`), because wl_shm's ARGB8888 is B, G, R, A
  in memory. On `sig://home` that is a second pass over 3.5 MB (1280×696) per frame. The
  release measurement `frame_cost` (ui/sighurt-ui/tests/paint.rs, bundled font, 200 frames
  after 20 warm-up frames, each a new layout so none is unchanged) gives render medians:
  chrome surface 1280×84 108 µs, status 1280×20 16 µs, home page 1280×696 426 µs; at 200 %
  311, 51 and 1730 µs. The standard chrome's scenes have no clipped shapes (only clipped text),
  so the whole-buffer `Mask` of `clip_mask` is never made for them and is not a candidate.
- **Change:** hand tiny-skia and the text blender every color with red and blue exchanged
  (the clear color, shapes, paths, glyph colors, and the bytes of color glyphs), so what they
  paint is already B, G, R, A, and drop the swap pass (a padded stride still copies row by
  row, without the swap). Source-over with premultiplied alpha treats the three color channels
  alike, so exchanging two of them before painting is the same as exchanging them after: the
  same bytes.
- **Prediction:** `frame_cost` render medians fall, most on the large surfaces: home page by at
  least 15 % at 100 % and 200 %, the chrome surface by at least 5 %. Five runs of the test per
  build, interleaved; a change counts when the medians of the five medians differ by more than
  the larger spread.
- **Guards:** every golden image byte for byte (`tools/cargo test -p sighurt-ui --test paint`,
  `-p sighurt-chrome-line`), the CPU backend's unit tests, `sighurt-render --all-features`
  (the GPU preparation's reference comparison with the CPU backend); in `sig`, `chrome.sh` cpu
  render medians do not rise.
- **Outcome:** supported, kept. `frame_cost`, five runs per build interleaved
  (`/data/sig-builds/test-out/opt2/framecost-cpu1.txt`), median of the run medians (min–max)
  in µs, before → after: home page 1280×696 427.0 (426.0–428.6) → 180.9 (180.0–197.1), −58 %;
  at 200 % 1768.7 (1735.4–1798.7) → 687.1 (686.7–697.2), −61 %; chrome surface 1280×84 102.9
  (98.7–103.2) → 73.8 (70.3–77.2), −28 %; at 200 % 313.7 → 202.6, −35 %; status 1280×20 15.4 →
  9.0, at 200 % 50.8 → 25.1. The swap pass was most of the large surfaces' render. Every golden
  image is byte for byte the same: regenerating all of them with `SIGHURT_BLESS=1` (sighurt-ui
  `paint`, sighurt-chrome-line) left `git status` clean; `sighurt-render --all-features` (the
  GPU preparation against the CPU backend) passes. In `sig` (`chrome.sh`, cpu renderer, five
  runs per build interleaved in one gles2 sway, `chrome-cpu1/`): render medians chrome 0.200 →
  0.166 ms, page 1.780 → 1.180 ms, status 0.205 → 0.167 ms; churn CPU 3.4 → 3.1 %, hover CPU
  11.3 → 11.0 %, RSS 15.1 → 14.9 MiB (those three inside the spread). `sig`'s page render
  (1.2 ms) stays well above the test's (0.18 ms): each show of `sig://home` paints into a new
  shared-memory buffer (H-UI1 frees a hidden surface's), whose first writes fault its pages in.
- **Seen while measuring (not a change):** on `chrome.sh`'s new hover phase the status
  surface's `scene_ms` is about 0.5 ms a frame, almost all of it shaping the new status line
  (`Fonts::fitted`, 0.53 ms in a probe inside `sig`). The same 75-character line shapes in
  71 µs in a tight loop and in 421 µs when the loop sleeps 10 ms between lines (DejaVu Sans;
  Cantarell 37 → 193 µs): it is cold caches and clocks after the idle gap, not the shaper's own
  work, and a page's status text changes once per link hovered, not per motion as on
  `demo://resize`. Left alone.

### H-TUI1: The sixel encoder quantizes runs of one color once and scans only where a color is

- **Seen:** the terminal's page frames go out as sixel in the user's own setup (tmux in Rio).
  `encode_cost` (frontends/sighurt-tui/tests/encode_cost.rs, release, 30 encodes after 3
  warm-up ones, a 160×35-cell area of 8×20-pixel cells) on two 1280×696 frames cut from
  screenshots (`bench/tui/png2bgra.py`: WPE's example.com, a text page; the Wasm gradient
  showcase, many colors): sixel 2.5–2.8 ms and 3.0–3.3 ms a frame. Per pixel the encoder
  composites over black and quantizes (`pixel_index`) even inside long runs of one color; per
  band it clears and scans the whole width once for every color the band has, though most
  colors sit in a few columns.
- **Change:** in each row, a pixel equal (as a 32-bit word) to the one before takes its index
  without quantizing again; each band records, per color, the first and last column it has,
  and clears, stamps and run-length encodes only between them, writing the columns before the
  first as the one run of empty sixels the full scan would have written. The output is
  unchanged: the same registers, runs and bytes.
- **Prediction:** sixel encode time falls by at least 30 % on both frames.
- **Guards:** the output of both frames and the synthetic one byte for byte the same as before
  (`SIG_TUI_DUMP`, `cmp`); sixel.rs's unit tests (the masked path against the scanning path, the
  registers, the runs); `apps/sighurt-browser/tests/tui.rs`; kitty unchanged.
- **Outcome:** supported, kept. Five runs of each build's `encode_cost`, interleaved, under
  `tools/heavy` (`/data/sig-builds/test-out/opt2/encodecost-tui1.txt`), sixel medians (min–max):
  example.com 2527 (2519–2562) → 1206 (1204–1255) µs, −52 %; gradients 3146 (3120–3192) →
  1232 (1229–1340) µs, −61 %; synthetic 2303 (2281–2380) → 1133 (1091–1142) µs, −51 %. The
  output of all three frames is byte for byte the same (`cmp` of `SIG_TUI_DUMP`'s files), and so
  is kitty's, whose time did not move (11.7 → 11.5 ms on example.com, inside its spread).
  sixel.rs's unit tests pass, the masked and the scanning paths still agree.
- **Not changed, kitty:** its encode is 11–12 ms a page frame, nearly all zlib at level 6;
  b6f1a28 measured level 1 (a document frame 40 KB → 619 KB on the wire) and kept 6. A faster
  deflate backend for flate2 (zlib-ng, zlib-rs) would change flate2's features for the whole
  graph Servo shares, so it is an open item, not done here.

### H-SHM1: A new SHM buffer is faulted in with one call

Written after its measurement, which was a probe: recorded because it was rejected.

- **Seen:** in `sig`, the CPU renderer's render of `sig://home`'s page surface takes 1.16 ms
  against 0.18 ms in `frame_cost`; the surface is shown afresh on every tab switch (H-UI1 frees
  a hidden surface's buffers), so its first frame paints into a new 3.5 MB buffer whose pages
  fault in one by one as they are written.
- **Change tried:** `madvise(MADV_POPULATE_WRITE)` on a new buffer's mapping (memmap2's
  `advise`, the version smithay-client-toolkit's pool already uses), so the faults happen in
  one call before painting.
- **Measured:** `chrome.sh`, cpu, three runs a build interleaved, with a temporary probe line
  in both builds timing `shm_frame` (the buffer) and the render (`/data/sig-builds/test-out/
  opt2/shmprobe`): page surface, medians, buffer + render 0.015 + 1.139 = 1.156 ms → 0.858 +
  0.291 = 1.589 ms. The render fell by the faults, but populating took longer than the faults
  it replaced.
- **Outcome:** rejected; not kept.

### H-MEM1: Tab churn with the demo and Wasm engines levels off

- **Question:** do the frontend's own caches and buffers grow with tabs opened and closed? By
  construction they are bounded: the shaped-text cache (`Fonts`, 256 lines and 128 KiB of text,
  then it starts over), the glyph cache (2048 glyphs, 8 MiB), the snapshot cache (one entry per
  open tab, rebuilt each snapshot), the SHM buffers (three per surface, two retiring
  generations, freed with their target).
- **Workload:** `bench/memory/churn.sh`, release `sig` with the demo and Wasm engines, 20
  cycles of: new tab, `demo://animation`, new tab, raf-demo (Wasm), 1 s, close both. Samples
  after cycles 0, 1, 5, 10 and 20; three runs.
- **Prediction:** after cycle 5, VmRSS grows by less than 2 MiB per further 5 cycles (allocator
  retention aside, H12), the SHM `buffer_bytes` the targets hold after a cycle are the same
  every cycle (the one remaining tab's), the open targets are the same, and the glyph cache stays
  under its bound. If something grows linearly, it is a leak to fix.
- **Outcome:** supported; nothing to fix. Three runs (`/data/sig-builds/test-out/opt2/mem1`,
  raf-demo presenting 2202 frames a run): VmRSS after cycles 0, 1, 5, 10, 20 (MiB) 43.8, 70.2,
  71.3, 71.6, 71.7; 43.1, 69.6, 70.3, 71.0, 66.3; 42.9, 69.1, 69.9, 70.4, 70.6. The step at the
  first cycle is the Wasm engine starting (Wasmtime, its threads) and the first `sig://home`;
  from cycle 5 to 20 RSS moves −4.0 to +0.7 MiB. After every cycle the targets hold the same
  4,526,080 bytes of SHM buffers in 4 targets (the remaining tab's view and the chrome's three
  surfaces, the hidden page surface holding none), and the glyph cache the same 138 glyphs,
  8.5 KB. The shaped-text, glyph and snapshot caches are bounded by construction (above), so a
  longer run cannot grow them either.

### H-CPU2: Glyphs are blended a row slice at a time

- **Seen:** after H-CPU1, text is about a quarter of the CPU backend's render on the large
  surface (a probe rendering `frame_cost`'s scenes with only some kinds of item: home page,
  nothing but the clear 91 µs, the text alone 138 µs, everything 187 µs; chrome surface 7, 22,
  73 µs). Each glyph pixel recomputes its source and target index and slices the buffer with
  bounds checks before blending.
- **Change:** per glyph row, slice the coverage (or the color glyph's bytes) and the target
  row once and blend along them; the blend itself is unchanged.
- **Prediction:** `frame_cost` render medians fall by at least 10 % on the home page and 5 % on
  the chrome surface; five runs a build, interleaved.
- **Guards:** golden images byte for byte (`SIGHURT_BLESS=1` leaves `git status` clean), the
  backend's unit tests, `sighurt-render --all-features`.
- **Outcome:** supported in part, below the predicted size; kept. `frame_cost`, five runs a
  build interleaved (`/data/sig-builds/test-out/opt2/framecost-cpu2.txt`), render medians
  (min–max) in µs: at 200 %, beyond the spread: home 694.6 (691.1–734.9) → 647.9
  (634.2–660.9), −6.7 %; chrome 202.6 (194.2–206.0) → 175.6 (165.8–183.4), −13 %; status
  25.2 → 21.6. At 100 %, inside the spread: home 180.1 (179.4–189.6) → 174.1 (172.5–181.9),
  −3.3 %; chrome 73.9 (70.6–77.6) → 69.2 (68.2–71.2), −6 %. Kept because it moved beyond
  noise where glyphs are larger and regressed nothing; the golden images are byte for byte
  the same.

### Optimization session (4 November 2026): measured and left alone

- **Text mode's layout** (`frontends/sighurt-tui/src/textmode/layout.rs`): the existing test
  `a_truncated_document_says_so_and_a_big_one_is_fast`, release, lays out a 20 000-node
  snapshot with 4999 links at 80 columns in 6.8, 7.2 and 14.7 ms (three runs). It runs once per
  snapshot or resize, not per frame; nothing changed.
- **The owner loop** after decision 124 was not profiled again: `perf` cannot open events in
  this session's sandbox. On `chrome.sh`'s hover phase (a status change and a `demo://resize`
  repaint every 10 ms) `sig` uses 11 % of a core, which is the demo page's full repaint per
  motion and the status line's shaping (H-CPU1's note), not the loop.
- **The GPU renderers** (wgpu, Vulkan) were not changed: their per-frame cost is the driver's
  acquire, submit and present (bench/results/RENDER.md), `auto` does not choose them, and the
  CPU renderer is now the faster one per frame by more than before.

### Optimization session (5 November 2026): method

Written on 5 November 2026 on tensor-ash (the same machine as the 4 November session), branch
`opt3` off 63d3e4c, before any change below was measured. The method is the 4 November one:
release builds of `sig` (`--no-default-features --features
engine-demo,engine-wasm,frontend-tui,render-wgpu`), the build before a change and after it run
interleaved in one private compositor, 1280×800 at scale 1, fresh XDG directories each run;
temporary timing lines where `perf` would have said where time goes (it cannot open events in
the sandbox), never committed.

### H-START2: The history file is read after the first frame, not before it

- **Seen:** since decision 131 the runtime reads and parses `history.toml` in `Session::new`
  (`History::open` → `set_remember(true)` → `load`), which the GUI calls after it creates the
  window and before its loop runs, so before the first configure is answered; the terminal
  does the same before its first frame. A full history (5 000 visits, 807 KB) took 37–47 ms
  to read and parse in a debug build (decision 131); a release build should take several
  milliseconds, against a whole release startup of about 7 ms (H-START1).
- **Change:** `History` reads and parses the file on a thread of its own, started when the
  history is opened; the owner takes the result when it is ready (each loop pass asks, without
  waiting) or when something needs the visits (a visit, a forget, a save, `remember`), waiting
  then if it must. Completion answers nothing until the file is in, and the frontends ask again
  when it arrives (the history's generation changes). What is read and kept is unchanged.
- **Prediction:** with a 5 000-visit history in the state directory, the time from exec to the
  first chrome surface and to the page's first frame (`bench/startup/startup.sh`,
  `demo://pattern`) falls by at least 3 ms; without a history file it does not move beyond
  the spread. Ten runs a build, interleaved; a change counts when the medians differ by more
  than the larger of the two builds' spreads (min–max).
- **Guards:** the runtime's history tests (the store, the writer, forgets with `remember` off,
  another `sig`'s history), the history fuzz tests, headless.sh `history` (the ghost text from
  a seeded file), the terminal's `the_address_line_completes_from_the_history`; no thread left
  after startup (the reader ends when it has read).
- **Outcome:** supported, kept. `startup.sh`, ten runs a build interleaved, headless sway
  (pixman), with `HISTORY_FILE` a 5 000-visit file of 807 KB
  (`/data/sig-builds/test-out/opt3/start2-hist`), medians (min–max) in ms: exec → first chrome
  surface 35.35 (34.30–36.00) → 19.95 (19.40–21.10), −15.4 ms; → the page's first frame
  35.75 (35.30–36.30) → 20.90 (20.30–21.80), −14.9 ms. The spreads do not overlap. Without a
  history file (`start2-nohist`): first chrome 20.75 (19.00–21.90) → 19.90 (18.80–22.60), first
  page 20.65 (20.10–21.40) → 20.70 (20.10–24.40), inside the spread. A release build takes
  about 15 ms to read and parse the full history (more than the 3 ms predicted), so it was
  three quarters of the whole startup. The guards pass (the runtime's 50 tests with a new
  one for the read on a thread, the fuzz tests, and the end-to-end suites listed in the
  session's report).
- **Seen while measuring (not a change):** in this harness exec → `main` takes about 15 ms of
  the 20 (temporary probes, three runs, `start-probe`), where H-START1 found 1.1 ms. It is the
  dynamic loader's search, not sig: this build links the Wasm engine's FFmpeg and its codec
  libraries (105 shared objects), and inside `nix-shell` `LD_LIBRARY_PATH` has 21 directories,
  so loading them makes 2 564 failed `openat` calls (`strace`, `--version`). Outside the shell
  the binary does not start at all (another glibc), so where `sig` is started from decides
  this cost; loading FFmpeg only when a guest first uses media would remove most of it. Left
  as an open item.

## Predictions for the IPC-removal session (gpui-baseline against 3c510fe)

Written on 2 October 2026, before the session's first run. The session measures the baseline
binaries (`~/sighurt-baseline`, `gpui-baseline` 359cb36) and the rewrite at 3c510fe (default
features: Servo and Wasm in `sig`'s process) interleaved, in one random order per round, in the
same headless sway from the pinned nixpkgs, on louise-pc. It cannot separate the three changes:
the rewrite has no pipe, no GPUI and no readback, all at once. The baseline's own timings
(`read_p50_ms`, `send_p50_ms`, the `_browser`/`_engines` split) are what apportion its costs
between them.

What the baseline pays per animated Servo frame (1280 × 696, about 59 a second; BASELINE.md):
paint 0.5–0.6 ms, readback and row flip 3.4–5.5 ms, comparison with the previous frame and the
3.5 MB pipe write 1.6–1.7 ms, then `sig` reads it, swaps it to BGRA (`render_image` in
`gpui-baseline:sighurt-core/src/page.rs`), and GPUI uploads it and draws it as a texture (12 % of a CPU and 3,660 wakeups a second in `sig` on `css-animation`).

| # | Hypothesis | Workloads | Primary metric | Prediction |
|---|---|---|---|---|
| P1 | H1: no readback | every Servo workload | `steady`/`active.readback_mib_per_s`, `exit.page_readbacks` | 0 in every rewrite run (baseline: 201 MiB/s on the animations, 34 on scrolling) |
| P2 | H2: no pixel transport | every workload | `active.ipc_pixel_mib_per_s` | 0 (by construction: no pipe); not a finding, a check that the rewrite reports it |
| P3 | H2: the copies' CPU is gone | `css-animation`, `webgl` | `steady.cpu_pct`, `active.cpu_pct` | at least 40 % lower |
| P4 | H2 | `canvas` | the same | at least 20 % lower: Servo draws the 2D canvas on the CPU either way |
| P5 | H2: GPUI's and the pipe's threads are gone | `css-animation`, `canvas`, `webgl` | `steady.wakeups_per_s` | at least 40 % lower (baseline 7,700–10,400/s, of which `sig` 3,660/s) |
| P5b | The pipe alone: a frame is 3,563,520 bytes and a pipe holds 64 KiB, so each frame puts the writer (`sig-servo`) and the reader (`sig`) to sleep about 55 times each | `css-animation`, `webgl` | `steady.wakeups_per_s`; `bench/pipe-cost` | the transport alone, measured by `bench/pipe-cost` at 1280 × 696 and 59 frames a second, makes about 6,000–6,500 of the baseline's ~7,700 wakeups a second; the rewrite's are at most 2,000/s |
| P6 | H2: no readback or second upload on the GPU | `css-animation`, `canvas`, `webgl` | `active.gpu_util_pct` | lower beyond noise |
| P7 | H2 guard | `css-animation`, `canvas`, `webgl` | `steady.fps_delivered` | no lower than 58/s (the output's 60 Hz paces both) |
| P8 | H3: animation doesn't repaint the chrome | `css-animation`, `canvas`, `webgl`, `wasm-raf`, `wasm-workers` | `steady.browser_fps` | 0 (baseline: 58–59, GPUI redraws the window per page frame) |
| P9 | H4: the idle browser sleeps | `home`, `article`, `long`, `many-tabs`, `tab-churn`, `resize-storm` | `steady.wakeups_per_s`, `steady.cpu_pct` | at most 2/s (baseline 191/s, GPUI's) and at most 0.1 % |
| P10 | H4, for static guests | `wasm-hello`, `wasm-gradient`, `wasm-timer` | `steady.wakeups_per_s` | lower, but not near 0: their `on_frame` still runs 60 times a second on a guest thread (decision 23); 60–200/s (baseline 251–384) |
| P11 | H8: CPU drawing of Wasm guests | `wasm-raf` | `steady.cpu_pct` | **higher** (H8 not supported): its every frame is rasterized on the CPU (decision 45), where GPUI drew the list on the GPU |
| P12 | H8 | `wasm-hello`, `wasm-gradient`, `wasm-timer` | `steady.cpu_pct` | no higher beyond noise: unchanged lists aren't painted again |
| P13 | Memory: one process, no GPUI | every one-page workload | `steady.pss_mean_mib` | lower: at least 25 % on `home` and the one-page Servo workloads, at least 10 % on the Wasm ones. GPUI with its Vulkan device (214 MiB PSS alone on `home`) and the second copy of a runtime per page are gone. Against it: `sig` starts Servo at launch even for `home` and Wasm pages (decision 26), and each Servo view has its own GL context (decision 51) |
| P14 | H5: hidden tabs | `many-tabs` | `steady.pss_mean_mib`, `active.pss_peak_mib` | lower, by at least 10 %; least certain of the memory predictions, since eight GL contexts and eight laid-out documents (decision 58) stand against seven processes that never painted |
| P15 | H5 guard | `many-tabs` | `active.input_to_pixels_p50_ms` | **higher** (H5's guard fails): a tab switch shows a view whose first frame on its surface the layout transaction waits for, up to 150 ms (decision 35), and each context's first frame costs WebRender's warm-up, 25–167 ms (decision 58); GPUI drew the tab strip without waiting for the page |
| P16 | H9–H11: chrome input beside busy guests | `wasm-busy`, `wasm-workers` | `active.input_to_pixels_p50_ms` | no higher beyond noise (baseline 33 ms): guests run on their own threads, the owner presents the latest list |
| P17 | H10: output limits | `wasm-flood` | `shutdown.forced`, `active.input_to_pixels_missed`, `startup.first_frame_s` | no forced quit (baseline: all), at most 2 of 20 inputs missed (baseline: 20), first frame under 1 s (baseline 5.6 s) |
| P18 | Page input latency | `article`, `long` | `active.input_to_screen_p50_ms` | at least 3 ms lower (baseline 17.3 and 22.2 ms): no readback (3.7 ms), no send (1.7 ms), no second presenter |
| P19 | H6: resizes stay bounded | `resize-storm` | `after.pss_growth_mib` | no higher than the baseline's 110 MiB |
| P20 | H6 guard: churn | `tab-churn` | `after.pss_growth_mib` | no higher than the baseline's 13 MiB, although every cycle makes and destroys a GL context |
| P21 | H7: startup | every workload | `startup.mapped_s` | lower or no change: no Vulkan device, a CPU chrome; but Servo is built before the window (decision 26), on the same thread |
| P22 | H7 | every Servo workload | `startup.first_frame_s` | no higher beyond noise; may move either way: the baseline started Servo in its own process in parallel with GPUI, the rewrite builds it, then the window, then the view, on one thread, and saves the readback and the second presentation of the first frame |
| P23 | Shutdown | the Servo workloads | `shutdown.s` | may rise: Servo now shuts down in `sig` in order and writes its profile before `sig` exits (decision 54), where the baseline's engine processes ended on their own; lower on `wasm-flood` (the baseline needed SIGTERM) |
| P24 | GPU memory | one-page workloads | `steady.gpu_fb_tree_mib` | lower: one GL context, no Vulkan device; on `many-tabs` it may rise (eight contexts) |

The rewrite's CPU, wakeups and memory are measured without `RUST_LOG`: enabling the Servo
timing target raises the `log` crate's level for the whole process (benchlib/parsers.py,
`ServoLogTiming`). Its GL audit (`SIG_AUDIT_GL=1`, which times every GL call) and the timing lines
run in a separate set, which checks P1 at the GL level and is not compared for performance.

## Tested

### The IPC-removal session's predictions

`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, measured interleaved on 2 October 2026; the analysis is
[`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md). 20 of the 25 predictions held. Not held: P10
(static guests stay at about 250 wakeups/s, H13), P12 for `wasm-gradient` (+0.1 point of CPU), P15
(tab switches did not get slower: 33.8 → 33.3 ms), P20 (tab churn leaves 58 MiB, a regression,
H12) and P23's possible rise (shutdown got faster). P11 held only in direction (+3 %).

### H1: Direct presentation removes Servo's readback (M1, M4)

- **Change:** Servo renders into a host-owned page subsurface and presents it (plan §9, §14),
  instead of `read_pixels` into a CPU array per frame.
- **Prediction:** `readback_mib_per_s` falls to 0 in the steady and active windows of every
  Servo workload (`article`, `long`, `css-animation`, `canvas`, `webgl`, `many-tabs`,
  `resize-storm`). This is §26's first hard criterion, so the new build must report its frames
  with `readback_bytes = 0` (not omit them).
- **Guards:** `startup.first_frame_s` and `active.fps_delivered` on the animation workloads.
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Supported.** 201 MiB/s → 0 on the animations, 6.8–49 MiB/s → 0 on the rest; 0 `read_to_image` in all 56 measured Servo runs and 0 GL reads in 24 audited ones. Guards: first frame 0.40–0.75 → 0.15–0.20 s; frames 59.2 → 58.2/s (1–2 % lower beyond noise).

### H2: No pixel transport, no second upload (M1, M4)

- **Change:** the same, seen from the browser: no frame crosses a pipe and GPUI no longer
  uploads it as a texture and draws it.
- **Prediction:** `ipc_pixel_mib_per_s` falls to 0 on every workload (the new build reports
  its frames with `ipc_pixel_bytes = 0`; there is no pipe), and `active.cpu_pct` and
  `steady.cpu_pct` fall on the animation workloads (`css-animation`, `canvas`, `webgl`), by at
  least the baseline's CPU share of reading, comparing and sending frames (`read_p50_ms` +
  `send_p50_ms` per produced frame, times the frame rate).
- **Guards:** `active.input_to_screen_p50_ms` on `long` and `article`; `steady.fps_produced`
  on the animations must not drop.
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Supported.** No pipe (`ipc_pixel_mib_per_s` 0); CPU −74 % (`css-animation`), −43 % (`canvas`), −76 % (`webgl`), more than the baseline's read and send share. Guards: scroll latency 17.4 → 2.2 ms (`article`) and 21.9 → 7.6 ms (`long`); `steady.fps_produced` 59.1–59.2 → 58.2–58.3, lower by 1–2 % beyond noise: a small regression, H16.

### H3: Page animation no longer repaints the chrome (M1, M3)

- **Change:** chrome and page are separate surfaces (§8, §13); a page frame is committed on the
  page's subsurface only.
- **Prediction:** on the animation workloads, the chrome's frame rate in the steady window
  (`steady.browser_fps`, which the new build must report for chrome paints only) falls from the
  baseline's, where GPUI redraws the whole window for every page frame, to 0 (§26: zero chrome
  repaints caused solely by page animation).
- **Guards:** `steady.cpu_pct`, `steady.wakeups_per_s`.
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Supported.** `steady.browser_fps` 58.5–59.5 → 0 on every animation, Servo and Wasm. Guards: CPU and wakeups fell.

### H4: An idle browser sleeps (M3)

- **Change:** no permanent repaint timer in the chrome (§10, §26).
- **Prediction:** on `home`, `article` and `wasm-gradient`, `steady.browser_fps` is 0 and
  `steady.wakeups_per_s` and `steady.cpu_pct` are not higher than the baseline's; on `home`
  they fall, because the baseline's GPUI loop and engine processes are gone.
- **Guards:** `startup.mapped_s`.
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Supported.** `steady.browser_fps` 0, wakeups 191 → 0.1/s and CPU 0.7–0.8 → 0 % on `home` and `article`. On `wasm-gradient` wakeups 252 → 246/s (noise) and CPU 1.0 → 1.1 % (beyond noise, one 10 ms tick): not lower, H13. Guard: `mapped_s` 0.35–0.40 → 0.05–0.15 s.

### H5: One Servo runtime is lighter than one process per tab (M4)

- **Change:** one Servo runtime with a WebView per tab (§14, §18) instead of a `sig-servo`
  process each.
- **Prediction:** on `many-tabs`, `steady.pss_mean_mib` falls, by roughly seven times the
  per-process overhead the baseline's `article` shows beyond its page (one Servo, SpiderMonkey
  runtime, font and GL context each).
- **Risk:** hidden WebViews may keep GPU memory (§27): watch `steady.gpu_fb_tree_mib` and
  `steady.gpu_mem_used_mib`.
- **Guards:** `active.input_to_pixels_p50_ms` (tab switches) on `many-tabs`.
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Supported.** `many-tabs` PSS 604 → 345 MiB, 978 → 409 MiB after visiting all eight tabs. Risk: GPU memory 291 → 100 MiB. Guard: tab switches 33.8 → 33.3 ms (noise).

### H6: Resizes stay bounded (M1, M3)

- **Change:** layout epochs, target recreation and SHM buffer rules (§10).
- **Prediction:** on `resize-storm`, `after.pss_growth_mib` and `after.rss_growth_mib` are no
  higher than the baseline's, and repeated sets show no trend with the round number.
- **Guards:** `active.cpu_pct` on `resize-storm`.
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Supported.** `resize-storm` growth 111 → 15 MiB (PSS), 111 → 15 MiB (RSS); only one set, so no trend over rounds yet. Guard: CPU 23.7 → 8.1 %.

### H7: Startup does not get slower (M4, M5)

- **Change:** engines initialize in `sig` instead of in a spawned process per page.
- **Prediction:** `startup.first_frame_s` does not rise beyond noise on any workload, and falls
  on `many-tabs`, where the baseline starts eight Servo processes at once.
- **Guards:** `startup.pss_peak_mib`.
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Supported**, and faster: `first_frame_s` 0.40–0.75 → 0.15–0.20 s on every Servo workload, 0.35 → 0.10 s on the Wasm ones (`wasm-flood` 5.56 → 0.30 s); `many-tabs` 0.75 → 0.20 s. Guard: `startup.pss_peak_mib` 217–604 → 72–345 MiB.

### H8: The Wasm guests keep their cost (M5)

- **Change:** the Wasm engine draws its guests on the CPU into its own surface (§15) instead of
  sending draw lists for GPUI to paint.
- **Prediction:** no change beyond noise in `steady.cpu_pct` and `active.cpu_pct` on
  `wasm-hello`, `wasm-gradient`, `wasm-raf` and `wasm-timer` (whole tree: the baseline's
  `sig-wasm` process is in it). This is a guard hypothesis (§27: CPU Wasm rendering may be too
  slow), not an expected gain.
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Not supported, in both directions and by little**: `wasm-hello` 1.40 → 0.90 % and `wasm-timer` 1.0 → 0.6 % (lower), `wasm-gradient` 1.0 → 1.1 % and `wasm-raf` 5.89 → 6.10 % (higher), all beyond noise; in the active window `wasm-hello` 4.3 → 3.7 % and `wasm-gradient` 6.0 → 1.2 % (lower), the other two noise. The CPU drawing itself is cheap (0.41 ms a `wasm-raf` frame); its cost went to the compositor, H14.

### H9: A busy guest in the same process doesn't slow the chrome (M5)

- **Change:** the Wasm engine runs in `sig`'s process (§15, §18) instead of its own.
- **Prediction:** on `wasm-busy`, `active.input_to_pixels_p50_ms` and `_p95_ms` (opening and
  closing a tab while the guest burns most of its fuel every frame) are not higher than the
  baseline's beyond noise, where the guest has its own process. This is M5's "guest activity
  does not starve chrome input", measured.
- **Guards:** `steady.cpu_pct` on `wasm-busy` (the guest's own cost should not change much).
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Supported.** `wasm-busy` `input_to_pixels` 33.1 → 16.7 ms (p95 33.6 → 17.0). Guard: `steady.cpu_pct` 21.7 → 21.4 % (beyond noise by the rule), `steady.wakeups_per_s` 252 → 300 (H13).

### H10: The host bounds what a flooding guest makes it keep (M5)

- **Change:** the Wasm engine's limits on draw commands (200 000 a frame), raster work and
  console lines (§15, §25, decision 43), with the guest in `sig`'s process.
- **Prediction:** on `wasm-flood`, `active.pss_growth_mib` and `active.pss_peak_mib` are not
  higher than the baseline's beyond noise, and neither is `active.input_to_pixels_p50_ms`
  (chrome input while the guest floods).
- **Guards:** no run of `wasm-flood` has an anomaly (a crash or a forced quit).
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Supported.** `wasm-flood` `active.pss_peak_mib` 522 → 165 MiB, growth noise; every chrome input now changes the screen (20 of 20 missed before) at 53 ms p50, which has no baseline value to compare. Guard: no anomaly in 7 runs (the baseline needed SIGTERM in all 7). The owner thread waits 63 ms a frame for the flood's raster, H15.

### H11: Busy background workers don't starve the page or the chrome (M5)

- **Change:** Wasm workers become threads of `sig`'s process (§15, §18) instead of threads of
  the page's `sig-wasm` process.
- **Prediction:** on `wasm-workers`, `active.input_to_pixels_p50_ms` and `_p95_ms` are not
  higher than the baseline's beyond noise.
- **Guards:** `steady.cpu_pct` on `wasm-workers` (four busy workers cost about four CPUs either
  way). The page's animation rate is a secondary observation, compared by hand: the baseline
  shows it only as GPUI window draws (`steady.browser_fps`), the rewrite as its own presented
  frames (`steady.fps_delivered`).
- **Outcome** (`results/louise/baseline/20261002-131052/` against `results/louise/rewrite/20261002-131052/`, [`results/IPC-REMOVAL.md`](results/IPC-REMOVAL.md)): **Supported.** `wasm-workers` `input_to_pixels` 33.4 → 16.8 ms (p95 33.7 → 17.2). Guard: 401 % CPU both. The page's animation: 59.5 window draws a second in the baseline, 59.6 presented frames in the rewrite.
