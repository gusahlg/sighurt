# Baseline: `gpui-baseline` on louise-pc

The pre-rewrite browser measured with the harness in `bench/`, for plan §23 M0 ("run the
existing browser on a small fixed workload and retain its timing/memory observations") and as
the reference for M7 and for every hypothesis in [`../HYPOTHESES.md`](../HYPOTHESES.md).

**The baseline is the set `bench/results/louise/baseline/20260927-083430/`** (27 September
2026: 16 workloads, 1 warm-up and 5 measured rounds, 96 runs, none failed). Its summary, below
the notes, is `bench/report` output for that set.

## Notes

- **Build.** `gpui-baseline` (revision 359cb36) from a `git archive` of the tag (not a
  worktree: louise-pc has no clone), built in release mode by `bench/baseline/build.sh`:
  `sig`, `sig-wasm` and `sig-servo` (its own workspace and lockfile), with the sha256 values
  below. Machine, driver, compositor and renderer are in the summary.
- **Harness.** The set was measured from a working tree after commit a694507 (the revision
  says `+dirty`). Its harness sha256 `fafbe416…` is that of the committed harness files except
  one line of `tools/heavy` that another change edited during the set (the desktop-only check
  for a running game, `pgrep -f steamapps/common/` became `pgrep -f "steamapps/commo[n]/"`), a
  branch that never runs on louise-pc. With that line as it was, the hash of the committed
  files is the set's.
- **Build cost** (`bench/build-cost`, `results/louise/build-cost/baseline-20260927-094102.json`):
  release builds from an empty target directory under `tools/heavy` (8 jobs), rustc 1.95.0.

  | step | cargo time | crates compiled |
  |---|---|---|
  | `sig` + `sig-wasm` (`-p sighurt-ui -p sighurt-engine-wasm`) | 6 m 32 s | 766 |
  | `sig-servo` (its own workspace) | 9 m 32 s | 627 |
  | total | 16 m 4 s | |

  Lockfiles: 1026 packages in `Cargo.lock`, 836 in `sighurt-engine-servo/Cargo.lock`. Binary
  sizes: `sig` 26.2 MiB, `sig-servo` 151.4 MiB, `sig-wasm` 46.7 MiB.
- **Headline observations.** Animating Servo pages (`css-animation`, `canvas`, `webgl`) read
  back and send about 202 MiB/s of pixels over IPC at ~59 fps, and cost 44–87 % of a CPU with
  7,700–10,400 wakeups/s. The idle GPUI chrome alone wakes 191 times a second. Chrome input
  (opening, closing and switching tabs) reaches the screen in about 33 ms (`input_to_pixels`) on
  `wasm-busy`, `many-tabs` and `wasm-workers`; none of their 250 measured chrome inputs was missed. Eight article tabs
  take 597 MiB PSS. Four busy Wasm workers use four CPUs while the page's animation keeps 59.5
  window draws a second. The output flood overwhelms it: the first frame after 5.6 s, one CPU
  at 100 %, none of 20 chrome inputs changes the screen within 0.45 s, and ctrl+q is not handled
  within 10 s (every `wasm-flood` run is marked for that; the harness sent SIGTERM).
- **GPUI's draw log is incomplete** (README, "Known limits"): `ZED_MEASUREMENTS` logs only the
  draws of GPUI's frame callback, not the one it makes while dispatching a key. So
  `browser_frames`/`browser_fps` miss most key-driven redraws (typing on `home` shows 0), and
  chrome `input_to_screen` is not measurable here; `input_to_pixels` is the chrome latency.

## The first set, superseded

`bench/results/louise/baseline/20260926-192032/` (26 September, 14 workloads) was measured
before the review of the harness and is kept only for the record. It drove the keyboard with a
`wtype` process per action, so the seat had no keyboard most of the time: ctrl+q was lost in 7
of 84 runs, and its chrome `input_to_screen` values (and the 1–5 of 20 inputs per `wasm-busy`
run counted as missed) timed the redraws that the keyboard's coming and going caused, not the
keys'. Run 3-many-tabs crashed without being flagged: at 27.4 s GPUI's Vulkan backend panicked
("GPU has crashed", blade-graphics), after a 4.4 s stall of a `sig-servo` readback, and aborted
while unwinding; that run's numbers (active CPU 45.5 % against 8.6–8.9 % in the others) are in
its medians.

Against it, this set differs beyond noise (`bench/compare` of the two sets) where a keyboard
that stays on the seat changes GPUI's behavior. PSS is 5–20 MiB (3–6 %) higher on every
workload, most likely GPUI keeping a keymap loaded. On the workloads with pointer or key input
the GPU and the compositor work more (`wasm-gradient`: active CPU 1.1 % → 6.0 %, GPU 2 % →
29 %), most likely because GPUI presents again for a second after each input while its window
is active (gpui-0.2.2 `window.rs`, `last_input_timestamp`). This set is the realistic one: a
desktop always has a keyboard.

## Summary: baseline on louise (2026-09-27T08:34:30+0200)

- Build: gpui-baseline: GPUI chrome, engines as processes over sighurt-ipc, Servo frames read back (engines: processes)
  - `target/release/sig` sha256 `f31d11c48705e035…`, 26.2 MiB
  - `target/release/sig-servo` sha256 `687ef7720dbf85df…`, 151.4 MiB
  - `target/release/sig-wasm` sha256 `9099b41fde8cf1aa…`, 46.7 MiB
  - Revision 359cb3614ae3a9a5dfc73804b1abe51e0477755d, rustc 1.95.0 (59807616e 2026-04-14) (built from a source tarball)
- Machine: AMD Ryzen 5 5500 (12 CPUs, governor powersave, EPP balance_performance, boost 1), 15 GiB, NVIDIA GeForce RTX 4060, 595.80, 95.07.31.40.F2, 3
- Kernel 6.18.35, NixOS 26.11 (Zokor); compositor sway 1.12 (headless backend), renderer gles2, output 1280x800 at scale 1
- Harness a694507cca5c41c873df4e1261f1ff73f6068bd8+dirty (sha256 `fafbe4161e416756…`), workloads sha256 `b420411cfea997a9…`
- 5 measured rounds after 1 warmup; 80 measured runs, 0 failed; workload order randomized per round (seed 760265707)

Engine renderers seen: servo: hardware (NVIDIA GeForce RTX 4060/PCIe/SSE2)

### Overview

Median (IQR) over the measured runs; – where a build's diagnostics don't give the metric.

| workload | `startup.first_frame_s` | `steady.cpu_pct` | `steady.wakeups_per_s` | `steady.wakeups_per_s_browser` | `steady.wakeups_per_s_engines` | `steady.pss_mean_mib` | `steady.browser_fps` | `steady.fps_delivered` | `active.cpu_pct` | `active.fps_delivered` | `active.readback_mib_per_s` | `active.ipc_out_mib_per_s` | `active.input_to_screen_p50_ms` | `active.input_to_pixels_p50_ms` |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| home | 0.354 (0.000124) | 0.699 (0.0999) | 191 (0.207) | 191 (0.207) | 0 (0) | 214 (0.524) | 0 (0) | – | 2.10 (3.13e-05) | – | – | 0 (0) | – | – |
| article | 0.454 (0.0457) | 0.599 (4.45e-05) | 191 (0.114) | 191 (0.114) | 0 (0) | 375 (1.80) | 0 (0) | 0 (0) | 11.2 (0.101) | 9.99 (0.000334) | 33.9 (0.00113) | 33.9 (0.00113) | 17.3 (0.172) | – |
| long | 0.454 (8.99e-05) | 0.699 (0.0997) | 191 (0.19) | 191 (0.19) | 0 (0) | 415 (1.17) | 0 (0) | 0 (0) | 16.1 (2.30) | 9.98 (0.000278) | 33.9 (0.000943) | 33.9 (0.000943) | 22.2 (0.516) | – |
| css-animation | 0.404 (0.05) | 47.5 (3.20) | 7719 (11.8) | 3661 (9.45) | 4058 (0.62) | 370 (2.62) | 59.1 (0.0961) | 59.1 (0.00377) | 61.4 (1.59) | 59.3 (0.0953) | 202 (0.324) | 202 (0.349) | – | – |
| canvas | 0.454 (0.000545) | 82.7 (1.01) | 10418 (12.3) | 3672 (3.60) | 6755 (12.0) | 389 (2.20) | 59.1 (0.0991) | 59.2 (0.0994) | 86.7 (0.393) | 59.3 (0.101) | 202 (0.342) | 202 (0.346) | – | – |
| webgl | 0.454 (0.0502) | 44.4 (4.19) | 7800 (2.62) | 3672 (4.61) | 4130 (11.1) | 373 (1.15) | 59.2 (0.01) | 59.3 (0.0955) | 52.4 (2.30) | 59.4 (0.1) | 202 (0.342) | 202 (0.372) | – | – |
| wasm-hello | 0.404 (0.0502) | 1.30 (4.51e-05) | 384 (9.88) | 323 (9.87) | 59.9 (0.0978) | 245 (2.36) | 2.00 (6.93e-05) | – | 4.40 (0.000121) | – | – | 0.0211 (4.6e-07) | – | – |
| wasm-gradient | 0.354 (0.000151) | 0.899 (0.0999) | 251 (0.0917) | 191 (0.0937) | 60.0 (0.00263) | 243 (0.301) | 0 (0) | – | 5.99 (6.9e-05) | – | – | 0 (0) | – | – |
| wasm-raf | 0.353 (9.84e-05) | 5.79 (0.2) | 614 (0.678) | 555 (0.68) | 59.9 (0.00207) | 243 (0.435) | 58.5 (0.00216) | – | 5.99 (0.2) | – | – | 0.0332 (4.29e-07) | – | – |
| wasm-timer | 0.404 (0.0499) | 0.999 (0.0999) | 251 (0.1) | 191 (0.1) | 59.9 (0.00322) | 244 (2.02) | 0 (0) | – | 2.30 (4.61e-05) | – | – | 0.0137 (0.000124) | – | – |
| wasm-busy | 0.354 (3.91e-05) | 21.6 (0.0997) | 252 (0.133) | 192 (0.125) | 60.0 (0.00736) | 241 (1.16) | 0 (0) | – | 23.3 (0.199) | – | – | 0 (0) | – | 33.1 (0.0974) |
| many-tabs | 0.757 (0.0985) | 0.697 (0.0996) | 191 (1.11) | 191 (1.11) | 0 (0) | 597 (1.72) | 0 (0) | 0 (0) | 9.86 (0.198) | 1.39 (8.36e-05) | 9.48 (0.000569) | 4.74 (0.000284) | 34.1 (0.0116) | 33.8 (0.396) |
| tab-churn | 0.404 (0.05) | 0.699 (0.0998) | 191 (0.0185) | 191 (0.0185) | 0 (0) | 374 (3.07) | 0 (0) | 0 (0) | 14.0 (0.1) | 0.999 (1.17e-05) | 6.79 (7.93e-05) | 3.40 (3.97e-05) | – | – |
| resize-storm | 0.454 (0.000195) | 0.699 (8.51e-05) | 191 (0.186) | 191 (0.186) | 0 (0) | 376 (0.809) | 0 (0) | 0 (0) | 23.7 (0.2) | 14.1 (0.0997) | 48.9 (0.246) | 44.1 (0.139) | – | – |
| wasm-flood | 5.61 (0.0506) | 100 (0.1) | 223 (6.09) | 203 (6.09) | 19.6 (0.000147) | 516 (0.729) | 0.2 (1.76e-06) | – | 99.8 (0.102) | – | – | 0 (0) | 371 (37.9) | – |
| wasm-workers | 0.404 (0.05) | 401 (0.203) | 620 (3.24) | 560 (3.25) | 59.9 (0.0128) | 246 (3.30) | 59.5 (0.0127) | – | 401 (0.077) | – | – | 0.00223 (7.09e-08) | 15.8 (0.0102) | 33.4 (0.1) |

Cells below are median [Q1–Q3] over the measured runs.

### home

Chrome only: sig's home page, no engine. Types into the URL bar.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.354] | 5 |
| startup.first_frame_s | 0.354 [0.354–0.354] | 5 |
| startup.cpu_s | 0.35 [0.35–0.36] | 5 |
| steady.cpu_pct | 0.699 [0.6–0.699] | 5 |
| steady.cpu_pct_browser | 0.699 [0.6–0.699] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–191] | 5 |
| steady.wakeups_per_s_browser | 191 [191–191] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 214 [214–215] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.78 [1.74–4.20] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 4.30 [4.20–4.40] | 5 |
| active.cpu_pct | 2.10 [2.10–2.10] | 5 |
| active.cpu_pct_browser | 2.10 [2.10–2.10] | 5 |
| active.cpu_pct_engines | 0 [0–0] | 5 |
| active.wakeups_per_s | 386 [386–386] | 5 |
| active.pss_peak_mib | 217 [216–217] | 5 |
| active.browser_fps | 0 [0–0] | 5 |
| active.ipc_out_mib_per_s | 0 [0–0] | 5 |
| active.gpu_util_pct | 9.10 [9.00–11.6] | 5 |
| active.compositor_cpu_pct | 11.3 [11.2–11.3] | 5 |
| shutdown.s | 0.101 [0.101–0.101] | 5 |

### article

Static Servo page: a text-heavy article with four images. Scrolls down, then up.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.404 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.454 [0.408–0.454] | 5 |
| startup.first_frame_engine_s | 0.427 [0.398–0.436] | 5 |
| startup.cpu_s | 0.63 [0.6–0.63] | 5 |
| steady.cpu_pct | 0.599 [0.599–0.599] | 5 |
| steady.cpu_pct_browser | 0.599 [0.599–0.599] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–191] | 5 |
| steady.wakeups_per_s_browser | 191 [191–191] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 375 [374–376] | 5 |
| steady.fps_produced | 0 [0–0] | 5 |
| steady.fps_delivered | 0 [0–0] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.readback_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_pixel_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.70 [1.66–1.80] | 5 |
| steady.gpu_fb_tree_mib | 245 [245–245] | 5 |
| steady.compositor_cpu_pct | 4.19 [4.09–4.29] | 5 |
| active.cpu_pct | 11.2 [11.1–11.2] | 5 |
| active.cpu_pct_browser | 3.99 [3.99–3.99] | 5 |
| active.cpu_pct_engines | 7.09 [7.09–7.19] | 5 |
| active.wakeups_per_s | 1766 [1766–1766] | 5 |
| active.pss_peak_mib | 392 [390–392] | 5 |
| active.fps_produced | 9.99 [9.98–9.99] | 5 |
| active.fps_delivered | 9.99 [9.98–9.99] | 5 |
| active.browser_fps | 9.99 [9.98–9.99] | 5 |
| active.browser_draw_p50_ms | 0.48 [0.48–0.492] | 5 |
| active.input_to_frame_p50_ms | 7.71 [7.70–7.75] | 5 |
| active.input_to_screen_p50_ms | 17.3 [17.3–17.4] | 5 |
| active.input_to_screen_p95_ms | 25.1 [25.1–25.3] | 5 |
| active.readback_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.ipc_pixel_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.ipc_out_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.paint_p50_ms | 0.5 [0.5–0.5] | 5 |
| active.read_p50_ms | 3.80 [3.70–3.80] | 5 |
| active.send_p50_ms | 1.70 [1.70–1.70] | 5 |
| active.gpu_util_pct | 17.3 [17.1–17.3] | 5 |
| active.compositor_cpu_pct | 15.8 [15.8–15.9] | 5 |
| shutdown.s | 0.151 [0.151–0.151] | 5 |

### long

Scrolling a long text page (80 sections, ~26 000 words) with the mouse wheel.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.353–0.404] | 5 |
| startup.first_frame_s | 0.454 [0.454–0.454] | 5 |
| startup.first_frame_engine_s | 0.4 [0.397–0.436] | 5 |
| startup.cpu_s | 0.72 [0.71–0.73] | 5 |
| steady.cpu_pct | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_browser | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–191] | 5 |
| steady.wakeups_per_s_browser | 191 [191–191] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 415 [414–415] | 5 |
| steady.fps_produced | 0 [0–0] | 5 |
| steady.fps_delivered | 0 [0–0] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.readback_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_pixel_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.72 [1.68–1.72] | 5 |
| steady.gpu_fb_tree_mib | 245 [245–245] | 5 |
| steady.compositor_cpu_pct | 4.19 [4.09–4.29] | 5 |
| active.cpu_pct | 16.1 [15.8–18.1] | 5 |
| active.cpu_pct_browser | 4.09 [4.09–4.09] | 5 |
| active.cpu_pct_engines | 12.0 [11.7–14.0] | 5 |
| active.wakeups_per_s | 1768 [1767–1771] | 5 |
| active.pss_peak_mib | 430 [429–431] | 5 |
| active.fps_produced | 9.98 [9.98–9.98] | 5 |
| active.fps_delivered | 9.98 [9.98–9.98] | 5 |
| active.browser_fps | 9.98 [9.98–9.98] | 5 |
| active.browser_draw_p50_ms | 0.498 [0.498–0.499] | 5 |
| active.input_to_frame_p50_ms | 12.8 [12.7–12.8] | 5 |
| active.input_to_screen_p50_ms | 22.2 [21.8–22.3] | 5 |
| active.input_to_screen_p95_ms | 29.7 [29.7–30.2] | 5 |
| active.readback_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.ipc_pixel_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.ipc_out_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.paint_p50_ms | 0.5 [0.4–0.5] | 5 |
| active.read_p50_ms | 3.70 [3.70–3.70] | 5 |
| active.send_p50_ms | 1.70 [1.70–1.70] | 5 |
| active.gpu_util_pct | 17.0 [16.8–17.1] | 5 |
| active.compositor_cpu_pct | 15.2 [15.0–16.5] | 5 |
| shutdown.s | 0.151 [0.151–0.151] | 5 |

### css-animation

Servo: endless CSS transform, opacity, color and width animations. Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.404 [0.404–0.454] | 5 |
| startup.first_frame_engine_s | 0.398 [0.393–0.436] | 5 |
| startup.cpu_s | 1.60 [1.58–1.63] | 5 |
| steady.cpu_pct | 47.5 [46.8–50.0] | 5 |
| steady.cpu_pct_browser | 12.2 [12.0–12.3] | 5 |
| steady.cpu_pct_engines | 35.2 [34.8–37.6] | 5 |
| steady.wakeups_per_s | 7719 [7715–7727] | 5 |
| steady.wakeups_per_s_browser | 3661 [3658–3667] | 5 |
| steady.wakeups_per_s_engines | 4058 [4058–4058] | 5 |
| steady.pss_mean_mib | 370 [369–372] | 5 |
| steady.fps_produced | 59.1 [59.1–59.1] | 5 |
| steady.fps_delivered | 59.1 [59.1–59.1] | 5 |
| steady.browser_fps | 59.1 [59.0–59.1] | 5 |
| steady.readback_mib_per_s | 201 [201–201] | 5 |
| steady.ipc_pixel_mib_per_s | 201 [201–201] | 5 |
| steady.ipc_out_mib_per_s | 201 [201–201] | 5 |
| steady.gpu_util_pct | 24.9 [24.2–25.8] | 5 |
| steady.gpu_fb_tree_mib | 259 [259–259] | 5 |
| steady.compositor_cpu_pct | 14.4 [14.3–14.7] | 5 |
| active.cpu_pct | 61.4 [60.1–61.7] | 5 |
| active.cpu_pct_browser | 12.0 [11.9–12.1] | 5 |
| active.cpu_pct_engines | 49.7 [47.8–49.9] | 5 |
| active.wakeups_per_s | 7875 [7871–7881] | 5 |
| active.pss_peak_mib | 371 [371–373] | 5 |
| active.fps_produced | 59.3 [59.2–59.3] | 5 |
| active.fps_delivered | 59.3 [59.2–59.3] | 5 |
| active.browser_fps | 59.3 [59.3–59.3] | 5 |
| active.browser_draw_p50_ms | 0.501 [0.491–0.503] | 5 |
| active.readback_mib_per_s | 202 [201–202] | 5 |
| active.ipc_pixel_mib_per_s | 202 [201–202] | 5 |
| active.ipc_out_mib_per_s | 202 [201–202] | 5 |
| active.paint_p50_ms | 0.5 [0.5–0.5] | 5 |
| active.read_p50_ms | 5.50 [5.30–5.50] | 5 |
| active.send_p50_ms | 1.60 [1.60–1.60] | 5 |
| active.gpu_util_pct | 35.1 [34.8–35.2] | 5 |
| active.compositor_cpu_pct | 16.1 [15.7–16.9] | 5 |
| shutdown.s | 0.151 [0.151–0.151] | 5 |

### canvas

Servo: a 2D canvas redrawn every animation frame (300 balls). Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.404 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.454 [0.454–0.454] | 5 |
| startup.first_frame_engine_s | 0.435 [0.4–0.441] | 5 |
| startup.cpu_s | 2.86 [2.84–2.86] | 5 |
| steady.cpu_pct | 82.7 [82.6–83.6] | 5 |
| steady.cpu_pct_browser | 12.8 [12.6–12.8] | 5 |
| steady.cpu_pct_engines | 69.9 [69.8–70.8] | 5 |
| steady.wakeups_per_s | 10418 [10417–10429] | 5 |
| steady.wakeups_per_s_browser | 3672 [3671–3674] | 5 |
| steady.wakeups_per_s_engines | 6755 [6748–6760] | 5 |
| steady.pss_mean_mib | 389 [389–391] | 5 |
| steady.fps_produced | 59.2 [59.1–59.2] | 5 |
| steady.fps_delivered | 59.2 [59.1–59.2] | 5 |
| steady.browser_fps | 59.1 [59.1–59.2] | 5 |
| steady.readback_mib_per_s | 201 [201–201] | 5 |
| steady.ipc_pixel_mib_per_s | 201 [201–201] | 5 |
| steady.ipc_out_mib_per_s | 201 [201–201] | 5 |
| steady.gpu_util_pct | 22.8 [22.6–25.0] | 5 |
| steady.gpu_fb_tree_mib | 231 [231–231] | 5 |
| steady.compositor_cpu_pct | 11.3 [11.1–11.9] | 5 |
| active.cpu_pct | 86.7 [86.5–86.9] | 5 |
| active.cpu_pct_browser | 12.8 [12.7–12.8] | 5 |
| active.cpu_pct_engines | 74.0 [73.7–74.4] | 5 |
| active.wakeups_per_s | 10552 [10535–10579] | 5 |
| active.pss_peak_mib | 391 [391–393] | 5 |
| active.fps_produced | 59.3 [59.2–59.3] | 5 |
| active.fps_delivered | 59.3 [59.2–59.3] | 5 |
| active.browser_fps | 59.1 [59.1–59.3] | 5 |
| active.browser_draw_p50_ms | 0.608 [0.608–0.612] | 5 |
| active.readback_mib_per_s | 202 [201–202] | 5 |
| active.ipc_pixel_mib_per_s | 202 [201–202] | 5 |
| active.ipc_out_mib_per_s | 202 [202–202] | 5 |
| active.paint_p50_ms | 0.6 [0.6–0.6] | 5 |
| active.read_p50_ms | 3.40 [3.10–3.40] | 5 |
| active.send_p50_ms | 1.70 [1.60–1.70] | 5 |
| active.gpu_util_pct | 25.7 [25.7–27.2] | 5 |
| active.compositor_cpu_pct | 12.6 [12.6–12.6] | 5 |
| shutdown.s | 0.151 [0.151–0.151] | 5 |

### webgl

Servo: a WebGL quad rotating in 3D, redrawn every animation frame. Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.353 [0.353–0.354] | 5 |
| startup.first_frame_s | 0.454 [0.404–0.454] | 5 |
| startup.first_frame_engine_s | 0.393 [0.393–0.402] | 5 |
| startup.cpu_s | 1.50 [1.49–1.51] | 5 |
| steady.cpu_pct | 44.4 [42.9–47.1] | 5 |
| steady.cpu_pct_browser | 12.1 [11.8–12.1] | 5 |
| steady.cpu_pct_engines | 32.4 [30.9–35.4] | 5 |
| steady.wakeups_per_s | 7800 [7799–7801] | 5 |
| steady.wakeups_per_s_browser | 3672 [3669–3673] | 5 |
| steady.wakeups_per_s_engines | 4130 [4126–4137] | 5 |
| steady.pss_mean_mib | 373 [373–374] | 5 |
| steady.fps_produced | 59.3 [59.2–59.3] | 5 |
| steady.fps_delivered | 59.3 [59.2–59.3] | 5 |
| steady.browser_fps | 59.2 [59.2–59.2] | 5 |
| steady.readback_mib_per_s | 202 [201–202] | 5 |
| steady.ipc_pixel_mib_per_s | 202 [201–202] | 5 |
| steady.ipc_out_mib_per_s | 202 [202–202] | 5 |
| steady.gpu_util_pct | 25.5 [25.1–25.9] | 5 |
| steady.gpu_fb_tree_mib | 252 [252–252] | 5 |
| steady.compositor_cpu_pct | 14.5 [14.5–14.6] | 5 |
| active.cpu_pct | 52.4 [52.3–54.6] | 5 |
| active.cpu_pct_browser | 12.0 [11.9–12.1] | 5 |
| active.cpu_pct_engines | 40.4 [40.4–42.5] | 5 |
| active.wakeups_per_s | 7944 [7936–7946] | 5 |
| active.pss_peak_mib | 375 [374–377] | 5 |
| active.fps_produced | 59.4 [59.3–59.4] | 5 |
| active.fps_delivered | 59.4 [59.3–59.4] | 5 |
| active.browser_fps | 59.2 [59.1–59.2] | 5 |
| active.browser_draw_p50_ms | 0.469 [0.467–0.471] | 5 |
| active.readback_mib_per_s | 202 [202–202] | 5 |
| active.ipc_pixel_mib_per_s | 202 [202–202] | 5 |
| active.ipc_out_mib_per_s | 202 [202–202] | 5 |
| active.paint_p50_ms | 0.6 [0.6–0.6] | 5 |
| active.read_p50_ms | 5.10 [4.00–5.10] | 5 |
| active.send_p50_ms | 1.60 [1.60–1.60] | 5 |
| active.gpu_util_pct | 31.6 [31.4–31.9] | 5 |
| active.compositor_cpu_pct | 16.1 [15.7–16.5] | 5 |
| shutdown.s | 0.151 [0.151–0.151] | 5 |

### wasm-hello

Wasm guest hello-sighurt: a static screen of canvas drawing and a text field, redrawn by on_frame at 60 Hz (the old sig-wasm sends a draw list only when it changed). Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.404 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.404 [0.354–0.404] | 5 |
| startup.cpu_s | 0.43 [0.4–0.43] | 5 |
| steady.cpu_pct | 1.30 [1.30–1.30] | 5 |
| steady.cpu_pct_browser | 0.999 [0.999–0.999] | 5 |
| steady.cpu_pct_engines | 0.3 [0.3–0.3] | 5 |
| steady.wakeups_per_s | 384 [374–384] | 5 |
| steady.wakeups_per_s_browser | 323 [314–324] | 5 |
| steady.wakeups_per_s_engines | 59.9 [59.9–60.0] | 5 |
| steady.pss_mean_mib | 245 [242–245] | 5 |
| steady.browser_fps | 2.00 [2.00–2.00] | 5 |
| steady.ipc_out_mib_per_s | 0.00191 [0.00191–0.00191] | 5 |
| steady.gpu_util_pct | 4.04 [1.96–4.30] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 4.60 [4.50–4.60] | 5 |
| active.cpu_pct | 4.40 [4.40–4.40] | 5 |
| active.cpu_pct_browser | 4.00 [4.00–4.00] | 5 |
| active.cpu_pct_engines | 0.4 [0.4–0.4] | 5 |
| active.wakeups_per_s | 610 [608–610] | 5 |
| active.pss_peak_mib | 247 [247–247] | 5 |
| active.browser_fps | 21.9 [21.9–22.0] | 5 |
| active.browser_draw_p50_ms | 0.795 [0.789–0.798] | 5 |
| active.ipc_out_mib_per_s | 0.0211 [0.0211–0.0211] | 5 |
| active.gpu_util_pct | 16.4 [16.0–16.6] | 5 |
| active.compositor_cpu_pct | 16.1 [16.1–16.1] | 5 |
| shutdown.s | 0.101 [0.101–0.101] | 5 |

### wasm-gradient

Wasm guest gradient-demo: a static grid of gradients drawn with the canvas API. Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.354] | 5 |
| startup.first_frame_s | 0.354 [0.354–0.354] | 5 |
| startup.cpu_s | 0.41 [0.41–0.43] | 5 |
| steady.cpu_pct | 0.899 [0.899–0.999] | 5 |
| steady.cpu_pct_browser | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_engines | 0.2 [0.2–0.3] | 5 |
| steady.wakeups_per_s | 251 [251–252] | 5 |
| steady.wakeups_per_s_browser | 191 [191–192] | 5 |
| steady.wakeups_per_s_engines | 60.0 [60.0–60.0] | 5 |
| steady.pss_mean_mib | 243 [243–243] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.66 [1.64–1.74] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 4.20 [4.10–4.20] | 5 |
| active.cpu_pct | 5.99 [5.99–5.99] | 5 |
| active.cpu_pct_browser | 5.69 [5.69–5.69] | 5 |
| active.cpu_pct_engines | 0.3 [0.3–0.3] | 5 |
| active.wakeups_per_s | 565 [560–572] | 5 |
| active.pss_peak_mib | 245 [245–247] | 5 |
| active.browser_fps | 0 [0–0] | 5 |
| active.ipc_out_mib_per_s | 0 [0–0] | 5 |
| active.gpu_util_pct | 29.1 [29.0–36.8] | 5 |
| active.compositor_cpu_pct | 13.1 [13.0–14.4] | 5 |
| shutdown.s | 0.101 [0.101–0.101] | 5 |

### wasm-raf

Wasm guest raf-demo: a ball animated with request_animation_frame (animation). Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.353 [0.353–0.354] | 5 |
| startup.first_frame_s | 0.353 [0.353–0.354] | 5 |
| startup.cpu_s | 0.51 [0.5–0.51] | 5 |
| steady.cpu_pct | 5.79 [5.69–5.89] | 5 |
| steady.cpu_pct_browser | 5.49 [5.29–5.49] | 5 |
| steady.cpu_pct_engines | 0.3 [0.3–0.4] | 5 |
| steady.wakeups_per_s | 614 [614–615] | 5 |
| steady.wakeups_per_s_browser | 555 [554–555] | 5 |
| steady.wakeups_per_s_engines | 59.9 [59.9–59.9] | 5 |
| steady.pss_mean_mib | 243 [243–244] | 5 |
| steady.browser_fps | 58.5 [58.5–58.5] | 5 |
| steady.ipc_out_mib_per_s | 0.0332 [0.0332–0.0332] | 5 |
| steady.gpu_util_pct | 13.1 [13.0–14.0] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 15.2 [15.2–15.2] | 5 |
| active.cpu_pct | 5.99 [5.79–5.99] | 5 |
| active.cpu_pct_browser | 5.59 [5.39–5.59] | 5 |
| active.cpu_pct_engines | 0.4 [0.4–0.4] | 5 |
| active.wakeups_per_s | 694 [694–694] | 5 |
| active.pss_peak_mib | 244 [243–244] | 5 |
| active.browser_fps | 58.2 [58.2–58.3] | 5 |
| active.browser_draw_p50_ms | 0.688 [0.687–0.696] | 5 |
| active.ipc_out_mib_per_s | 0.0332 [0.0332–0.0332] | 5 |
| active.gpu_util_pct | 15.9 [15.7–18.3] | 5 |
| active.compositor_cpu_pct | 16.9 [16.8–16.9] | 5 |
| shutdown.s | 0.101 [0.101–0.101] | 5 |

### wasm-timer

Wasm guest timer-demo: idle in the steady window; in the active window its four buttons are clicked, starting set_interval timers of 1 s, 0.5 s and 0.1 s and a 3 s set_timeout.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.404 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.404 [0.354–0.404] | 5 |
| startup.cpu_s | 0.41 [0.38–0.42] | 5 |
| steady.cpu_pct | 0.999 [0.899–0.999] | 5 |
| steady.cpu_pct_browser | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_engines | 0.3 [0.3–0.3] | 5 |
| steady.wakeups_per_s | 251 [251–251] | 5 |
| steady.wakeups_per_s_browser | 191 [191–191] | 5 |
| steady.wakeups_per_s_engines | 59.9 [59.9–59.9] | 5 |
| steady.pss_mean_mib | 244 [242–244] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.80 [1.72–1.80] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 4.20 [4.10–4.30] | 5 |
| active.cpu_pct | 2.30 [2.30–2.30] | 5 |
| active.cpu_pct_browser | 2.00 [2.00–2.10] | 5 |
| active.cpu_pct_engines | 0.3 [0.3–0.3] | 5 |
| active.wakeups_per_s | 449 [448–449] | 5 |
| active.pss_peak_mib | 246 [244–246] | 5 |
| active.browser_fps | 11.1 [11.1–11.1] | 5 |
| active.browser_draw_p50_ms | 0.643 [0.643–0.645] | 5 |
| active.ipc_out_mib_per_s | 0.0137 [0.0137–0.0138] | 5 |
| active.gpu_util_pct | 7.92 [7.18–8.24] | 5 |
| active.compositor_cpu_pct | 8.99 [8.89–9.09] | 5 |
| shutdown.s | 0.101 [0.101–0.101] | 5 |

### wasm-busy

Plan §26's hostile Wasm workload: a guest (workloads/generate.py) that spends about 36M of its 50M fuel on every frame and draws one unchanging rectangle. Chrome input in the active window: a new tab and closing it again, twice a second.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.354] | 5 |
| startup.first_frame_s | 0.354 [0.354–0.354] | 5 |
| startup.cpu_s | 1.02 [1.02–1.03] | 5 |
| steady.cpu_pct | 21.6 [21.6–21.7] | 5 |
| steady.cpu_pct_browser | 0.699 [0.699–0.699] | 5 |
| steady.cpu_pct_engines | 21.0 [20.9–21.0] | 5 |
| steady.wakeups_per_s | 252 [252–252] | 5 |
| steady.wakeups_per_s_browser | 192 [192–192] | 5 |
| steady.wakeups_per_s_engines | 60.0 [60.0–60.0] | 5 |
| steady.pss_mean_mib | 241 [240–242] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.68 [1.68–1.72] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 4.20 [4.10–4.20] | 5 |
| active.cpu_pct | 23.3 [23.1–23.3] | 5 |
| active.cpu_pct_browser | 2.30 [2.30–2.30] | 5 |
| active.cpu_pct_engines | 21.0 [21.0–21.0] | 5 |
| active.wakeups_per_s | 501 [501–502] | 5 |
| active.pss_peak_mib | 244 [243–244] | 5 |
| active.browser_fps | 0 [0–0] | 5 |
| active.input_to_pixels_p50_ms | 33.1 [33.0–33.1] | 5 |
| active.input_to_pixels_p95_ms | 33.6 [33.5–33.6] | 5 |
| active.input_to_pixels_missed | 0 [0–0] | 5 |
| active.ipc_out_mib_per_s | 0 [0–0] | 5 |
| active.gpu_util_pct | 11.7 [11.4–14.0] | 5 |
| active.compositor_cpu_pct | 16.6 [16.6–16.7] | 5 |
| shutdown.s | 0.101 [0.101–0.101] | 5 |

### many-tabs

Eight tabs of the article, one visible: what hidden tabs cost. Switches tab once a second in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.454 [0.404–0.454] | 5 |
| startup.first_frame_s | 0.757 [0.706–0.804] | 5 |
| startup.first_frame_engine_s | 0.706 [0.676–0.776] | 5 |
| startup.cpu_s | 1.89 [1.88–1.90] | 5 |
| steady.cpu_pct | 0.697 [0.598–0.697] | 5 |
| steady.cpu_pct_browser | 0.697 [0.598–0.697] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–192] | 5 |
| steady.wakeups_per_s_browser | 191 [191–192] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 597 [597–599] | 5 |
| steady.fps_produced | 0 [0–0] | 5 |
| steady.fps_delivered | 0 [0–0] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.readback_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_pixel_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 2.00 [0.98–2.00] | 5 |
| steady.gpu_fb_tree_mib | 291 [291–317] | 5 |
| steady.compositor_cpu_pct | 4.58 [2.89–4.58] | 5 |
| active.cpu_pct | 9.86 [9.76–9.96] | 5 |
| active.cpu_pct_browser | 2.79 [2.69–2.79] | 5 |
| active.cpu_pct_engines | 7.17 [7.07–7.17] | 5 |
| active.wakeups_per_s | 889 [880–894] | 5 |
| active.pss_peak_mib | 970 [970–972] | 5 |
| active.fps_produced | 2.79 [2.79–2.79] | 5 |
| active.fps_delivered | 1.39 [1.39–1.39] | 5 |
| active.browser_fps | 1.69 [1.49–1.69] | 5 |
| active.browser_draw_p50_ms | 0.996 [0.932–1.05] | 5 |
| active.input_to_screen_p50_ms | 34.1 [34.1–34.1] | 5 |
| active.input_to_screen_p95_ms | 35.9 [35.8–36.1] | 5 |
| active.input_to_pixels_p50_ms | 33.8 [33.6–33.9] | 5 |
| active.input_to_pixels_p95_ms | 35.4 [34.5–35.8] | 5 |
| active.input_to_pixels_missed | 0 [0–0] | 5 |
| active.readback_mib_per_s | 9.48 [9.48–9.48] | 5 |
| active.ipc_pixel_mib_per_s | 4.74 [4.74–4.74] | 5 |
| active.ipc_out_mib_per_s | 4.74 [4.74–4.74] | 5 |
| active.paint_p50_ms | 0.2 [0.2–0.2] | 5 |
| active.read_p50_ms | 4.60 [4.40–4.60] | 5 |
| active.send_p50_ms | 2.60 [2.50–2.60] | 5 |
| active.gpu_util_pct | 11.3 [11.3–11.6] | 5 |
| active.compositor_cpu_pct | 15.5 [15.4–15.7] | 5 |
| shutdown.s | 0.353 [0.302–0.353] | 5 |

### tab-churn

Lifecycle: the article, then ten cycles of opening a new tab, loading the article in it and closing it (one every 2 s); measured again 3 s after the last close.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.404 [0.404–0.454] | 5 |
| startup.first_frame_engine_s | 0.396 [0.389–0.428] | 5 |
| startup.cpu_s | 0.6 [0.59–0.63] | 5 |
| steady.cpu_pct | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_browser | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–191] | 5 |
| steady.wakeups_per_s_browser | 191 [191–191] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 374 [373–376] | 5 |
| steady.fps_produced | 0 [0–0] | 5 |
| steady.fps_delivered | 0 [0–0] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.readback_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_pixel_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.78 [1.64–3.06] | 5 |
| steady.gpu_fb_tree_mib | 225 [225–225] | 5 |
| steady.compositor_cpu_pct | 4.19 [4.09–4.29] | 5 |
| active.cpu_pct | 14.0 [13.9–14.0] | 5 |
| active.cpu_pct_browser | 2.50 [2.45–2.50] | 5 |
| active.cpu_pct_engines | 11.5 [11.5–11.6] | 5 |
| active.wakeups_per_s | 793 [792–802] | 5 |
| active.pss_peak_mib | 483 [483–486] | 5 |
| active.fps_produced | 2.00 [2.00–2.00] | 5 |
| active.fps_delivered | 0.999 [0.999–0.999] | 5 |
| active.browser_fps | 1.10 [1.10–1.15] | 5 |
| active.browser_draw_p50_ms | 0.758 [0.753–0.774] | 5 |
| active.readback_mib_per_s | 6.79 [6.79–6.79] | 5 |
| active.ipc_pixel_mib_per_s | 3.40 [3.40–3.40] | 5 |
| active.ipc_out_mib_per_s | 3.40 [3.40–3.40] | 5 |
| active.paint_p50_ms | 0.2 [0.2–0.2] | 5 |
| active.read_p50_ms | 1.00 [1.00–1.20] | 5 |
| active.send_p50_ms | 2.60 [2.60–2.60] | 5 |
| active.gpu_util_pct | 2.32 [2.15–3.13] | 5 |
| active.compositor_cpu_pct | 3.95 [3.75–4.05] | 5 |
| after.rss_growth_mib | 12.9 [12.9–12.9] | 5 |
| after.pss_growth_mib | 12.9 [12.9–12.9] | 5 |
| shutdown.s | 0.151 [0.151–0.151] | 5 |

### resize-storm

The article while the output changes size and scale four times a second (plan §25 resize storm); restored and measured 3 s later.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.404 [0.404–0.404] | 5 |
| startup.first_frame_s | 0.454 [0.454–0.454] | 5 |
| startup.first_frame_engine_s | 0.44 [0.436–0.44] | 5 |
| startup.cpu_s | 0.64 [0.64–0.64] | 5 |
| steady.cpu_pct | 0.699 [0.699–0.699] | 5 |
| steady.cpu_pct_browser | 0.699 [0.699–0.699] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–191] | 5 |
| steady.wakeups_per_s_browser | 191 [191–191] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 376 [376–376] | 5 |
| steady.fps_produced | 0 [0–0] | 5 |
| steady.fps_delivered | 0 [0–0] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.readback_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_pixel_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.80 [1.70–1.82] | 5 |
| steady.gpu_fb_tree_mib | 153 [153–153] | 5 |
| steady.compositor_cpu_pct | 4.29 [4.29–4.39] | 5 |
| active.cpu_pct | 23.7 [23.6–23.8] | 5 |
| active.cpu_pct_browser | 5.99 [5.99–6.09] | 5 |
| active.cpu_pct_engines | 17.8 [17.7–17.8] | 5 |
| active.wakeups_per_s | 2065 [2065–2069] | 5 |
| active.pss_peak_mib | 490 [487–491] | 5 |
| active.fps_produced | 16.0 [15.8–16.0] | 5 |
| active.fps_delivered | 14.1 [14.1–14.2] | 5 |
| active.browser_fps | 14.9 [14.9–15.0] | 5 |
| active.browser_draw_p50_ms | 0.424 [0.422–0.428] | 5 |
| active.readback_mib_per_s | 48.9 [48.7–48.9] | 5 |
| active.ipc_pixel_mib_per_s | 44.1 [44.1–44.2] | 5 |
| active.ipc_out_mib_per_s | 44.1 [44.1–44.2] | 5 |
| active.paint_p50_ms | 0.2 [0.2–0.2] | 5 |
| active.read_p50_ms | 3.80 [3.80–3.90] | 5 |
| active.send_p50_ms | 1.30 [1.30–1.30] | 5 |
| active.gpu_util_pct | 27.3 [19.3–36.1] | 5 |
| active.compositor_cpu_pct | 21.8 [21.5–21.8] | 5 |
| after.rss_growth_mib | 111 [110–111] | 5 |
| after.pss_growth_mib | 110 [108–111] | 5 |
| shutdown.s | 0.151 [0.151–0.151] | 5 |

### wasm-flood

Plan §26's output limits: a guest (workloads/generate.py) that draws 250 000 moving rectangles and writes 200 console lines on every frame, more than the rewrite's Wasm engine keeps. Chrome input in the active window: a new tab and closing it again, twice a second.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.404] | 5 |
| startup.first_frame_s | 5.61 [5.56–5.61] | 5 |
| startup.cpu_s | 8.74 [8.71–8.75] | 5 |
| steady.cpu_pct | 100 [100–100] | 5 |
| steady.cpu_pct_browser | 100.0 [99.9–100.0] | 5 |
| steady.cpu_pct_engines | 0.3 [0.2–0.3] | 5 |
| steady.wakeups_per_s | 223 [217–223] | 5 |
| steady.wakeups_per_s_browser | 203 [197–204] | 5 |
| steady.wakeups_per_s_engines | 19.6 [19.6–19.6] | 5 |
| steady.pss_mean_mib | 516 [516–516] | 5 |
| steady.browser_fps | 0.2 [0.2–0.2] | 5 |
| steady.ipc_out_mib_per_s | 1.19 [1.19–1.19] | 5 |
| steady.gpu_util_pct | 0 [0–0] | 5 |
| steady.gpu_fb_tree_mib | 199 [199–199] | 5 |
| steady.compositor_cpu_pct | 0.2 [0.0999–0.2] | 5 |
| active.cpu_pct | 99.8 [99.7–99.8] | 5 |
| active.cpu_pct_browser | 99.8 [99.7–99.8] | 5 |
| active.cpu_pct_engines | 0 [0–0] | 5 |
| active.wakeups_per_s | 141 [141–141] | 5 |
| active.pss_peak_mib | 521 [520–521] | 5 |
| active.browser_fps | 0.0999 [0.0999–0.0999] | 5 |
| active.browser_draw_p50_ms | 4985 [4978–4991] | 5 |
| active.input_to_screen_p50_ms | 371 [350–388] | 5 |
| active.input_to_screen_p95_ms | 371 [350–388] | 5 |
| active.input_to_pixels_missed | 20.0 [20.0–20.0] | 5 |
| active.ipc_out_mib_per_s | 0 [0–0] | 5 |
| active.gpu_util_pct | 14.5 [14.4–14.7] | 5 |
| active.compositor_cpu_pct | 26.5 [26.5–26.5] | 5 |
| shutdown.s | 10.1 [10.0–10.1] | 5 |

### wasm-workers

Plan §26's worker fairness: a guest (workloads/generate.py) animating a square while four background workers each burn about 42M fuel per message, with eight messages queued each, so four threads stay busy. Chrome input in the active window: a new tab and closing it again, twice a second.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.404 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.404 [0.354–0.404] | 5 |
| startup.cpu_s | 13.1 [13.0–13.1] | 5 |
| steady.cpu_pct | 401 [401–401] | 5 |
| steady.cpu_pct_browser | 2.00 [2.00–2.00] | 5 |
| steady.cpu_pct_engines | 399 [399–399] | 5 |
| steady.wakeups_per_s | 620 [618–621] | 5 |
| steady.wakeups_per_s_browser | 560 [558–561] | 5 |
| steady.wakeups_per_s_engines | 59.9 [59.9–59.9] | 5 |
| steady.pss_mean_mib | 246 [243–247] | 5 |
| steady.browser_fps | 59.5 [59.5–59.5] | 5 |
| steady.ipc_out_mib_per_s | 0.00223 [0.00223–0.00223] | 5 |
| steady.gpu_util_pct | 10.9 [10.7–10.9] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 12.7 [11.9–12.7] | 5 |
| active.cpu_pct | 401 [401–401] | 5 |
| active.cpu_pct_browser | 1.90 [1.80–1.90] | 5 |
| active.cpu_pct_engines | 399 [399–399] | 5 |
| active.wakeups_per_s | 633 [633–633] | 5 |
| active.pss_peak_mib | 247 [244–247] | 5 |
| active.browser_fps | 29.5 [29.4–29.5] | 5 |
| active.browser_draw_p50_ms | 0.207 [0.205–0.207] | 5 |
| active.input_to_screen_p50_ms | 15.8 [15.8–15.8] | 5 |
| active.input_to_screen_p95_ms | 32.6 [32.5–32.7] | 5 |
| active.input_to_pixels_p50_ms | 33.4 [33.4–33.5] | 5 |
| active.input_to_pixels_p95_ms | 33.7 [33.6–33.7] | 5 |
| active.input_to_pixels_missed | 0 [0–0] | 5 |
| active.ipc_out_mib_per_s | 0.00223 [0.00223–0.00223] | 5 |
| active.gpu_util_pct | 12.4 [12.2–15.0] | 5 |
| active.compositor_cpu_pct | 16.2 [16.2–16.2] | 5 |
| shutdown.s | 0.1 [0.1–0.101] | 5 |

### Anomalies

- 0-wasm-flood: did not quit within 10 s of ctrl-q; sent SIGTERM
- 1-wasm-flood: did not quit within 10 s of ctrl-q; sent SIGTERM
- 2-wasm-flood: did not quit within 10 s of ctrl-q; sent SIGTERM
- 3-wasm-flood: did not quit within 10 s of ctrl-q; sent SIGTERM
- 4-wasm-flood: did not quit within 10 s of ctrl-q; sent SIGTERM
- 5-wasm-flood: did not quit within 10 s of ctrl-q; sent SIGTERM
