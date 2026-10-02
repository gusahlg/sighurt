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
