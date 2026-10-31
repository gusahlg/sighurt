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
