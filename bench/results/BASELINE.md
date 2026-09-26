# Baseline: `gpui-baseline` on louise-pc

The pre-rewrite browser measured with the harness in `bench/`, for plan §23 M0 ("run the
existing browser on a small fixed workload and retain its timing/memory observations") and as
the reference for M7 and for every hypothesis in [`../HYPOTHESES.md`](../HYPOTHESES.md).

Result set: `bench/results/louise/baseline/20260926-192032/` (6 rounds, the first a discarded warmup; 14 workloads). The summary below is `bench/report` output for that set. Headline observations: animating Servo pages move about 200 MiB/s of read-back pixels over IPC at ~59 fps and cost 46–84 % CPU with ~7,700–10,400 wakeups/s; the idle GPUI chrome alone wakes ~191 times a second.

## baseline on louise (2026-09-26T19:20:32+0200)

- Build: gpui-baseline: GPUI chrome, engines as processes over sighurt-ipc, Servo frames read back
  - `target/release/sig` sha256 `f31d11c48705e035…`, 26.2 MiB
  - `target/release/sig-servo` sha256 `687ef7720dbf85df…`, 151.4 MiB
  - `target/release/sig-wasm` sha256 `9099b41fde8cf1aa…`, 46.7 MiB
  - Revision 359cb3614ae3a9a5dfc73804b1abe51e0477755d, rustc 1.95.0 (59807616e 2026-04-14) (built from a source tarball)
- Machine: AMD Ryzen 5 5500 (12 CPUs, governor powersave, EPP balance_performance, boost 1), 15 GiB, NVIDIA GeForce RTX 4060, 595.80, 95.07.31.40.F2, 3
- Kernel 6.18.35, NixOS 26.11 (Zokor); compositor sway 1.12 (headless backend), renderer gles2, output 1280x800 at scale 1
- Harness 0944db3ea20a7439da73d7accf29897258a790f8+dirty (sha256 `90f0f1b801829dfc…`), workloads sha256 `7fb5ac806b65d4a4…`
- 5 measured rounds after 1 warmup; 70 measured runs, 0 failed; workload order randomized per round (seed 1002161634)

Engine renderers seen: servo: hardware (NVIDIA GeForce RTX 4060/PCIe/SSE2)

### Overview

Median (IQR) over the measured runs; – where a build's diagnostics don't give the metric.

| workload | `startup.first_frame_s` | `steady.cpu_pct` | `steady.wakeups_per_s` | `steady.wakeups_per_s_browser` | `steady.wakeups_per_s_engines` | `steady.pss_mean_mib` | `steady.browser_fps` | `steady.fps_delivered` | `active.cpu_pct` | `active.fps_delivered` | `active.readback_mib_per_s` | `active.ipc_out_mib_per_s` | `active.input_to_screen_p50_ms` |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| home | 0.404 (0.05) | 0.699 (0.0999) | 191 (0.00548) | 191 (0.00548) | 0 (0) | 209 (2.59) | 0 (0) | – | 2.00 (3.37e-05) | – | – | 0 (0) | – |
| article | 0.454 (0.000156) | 0.699 (0.0998) | 191 (0.193) | 191 (0.193) | 0 (0) | 356 (2.31) | 0 (0) | 0 (0) | 9.79 (0.2) | 9.98 (0.000352) | 33.9 (0.0012) | 33.9 (0.0012) | 17.0 (0.311) |
| long | 0.454 (0.0497) | 0.699 (0.0997) | 191 (0.204) | 191 (0.204) | 0 (0) | 398 (8.76) | 0 (0) | 0 (0) | 14.6 (0.0996) | 9.98 (0.00012) | 33.9 (0.000408) | 33.9 (0.000408) | 22.5 (0.573) |
| css-animation | 0.404 (4.3e-05) | 51.1 (1.09) | 7724 (10.0) | 3665 (1.63) | 4057 (4.65) | 350 (2.66) | 59.0 (0.000926) | 59.1 (0.102) | 61.7 (0.901) | 59.2 (0.0938) | 201 (0.319) | 202 (0.311) | – |
| canvas | 0.404 (0.0002) | 83.8 (1.98) | 10425 (31.2) | 3678 (3.34) | 6747 (25.6) | 371 (0.309) | 59.2 (0.288) | 59.2 (0.104) | 87.5 (0.771) | 59.3 (0.104) | 201 (0.354) | 202 (0.0444) | – |
| webgl | 0.454 (0.0499) | 45.9 (2.49) | 7788 (10.1) | 3670 (7.11) | 4123 (5.32) | 358 (2.88) | 59.1 (0.0902) | 59.2 (0.11) | 54.2 (2.90) | 59.3 (0.0964) | 202 (0.328) | 202 (0.0118) | – |
| wasm-hello | 0.354 (0.05) | 1.30 (9.63e-06) | 370 (22.1) | 310 (22.1) | 59.9 (0.00067) | 232 (2.48) | 2.00 (2.23e-05) | – | 3.30 (0.0999) | – | – | 0.0211 (0.00191) | – |
| wasm-gradient | 0.354 (0.0502) | 0.899 (0.0999) | 252 (0.101) | 192 (0.101) | 60.0 (0.000248) | 234 (3.03) | 0 (0) | – | 1.10 (0.0999) | – | – | 0 (0) | – |
| wasm-raf | 0.354 (0.0501) | 5.79 (0.2) | 613 (2.09) | 553 (2.09) | 59.9 (0.00287) | 233 (2.27) | 58.6 (0.101) | – | 5.99 (0.1) | – | – | 0.0332 (7.18e-07) | – |
| wasm-timer | 0.354 (0.0501) | 0.999 (0.0999) | 251 (0.217) | 191 (0.213) | 59.9 (8.87e-05) | 231 (2.58) | 0 (0) | – | 2.00 (0.0999) | – | – | 0.014 (0.000369) | – |
| wasm-busy | 0.354 (0.0501) | 21.6 (0.00241) | 252 (0.0825) | 192 (0.0785) | 60.0 (0.00415) | 231 (3.05) | 0 (0) | – | 22.7 (0.1) | – | – | 0 (0) | 7.54 (2.34) |
| many-tabs | 0.754 (0.000793) | 0.697 (0.0996) | 191 (0.0137) | 191 (0.0137) | 0 (0) | 580 (2.22) | 0 (0) | 0 (0) | 8.86 (0.24) | 1.39 (0.0999) | 9.43 (0.341) | 4.74 (0.0217) | 6.91 (1.40) |
| tab-churn | 0.404 (0.000117) | 0.699 (0.0999) | 191 (0.192) | 191 (0.192) | 0 (0) | 354 (9.23) | 0 (0) | 0 (0) | 13.1 (0.4) | 0.999 (1.48e-05) | 6.79 (0.0001) | 3.40 (5.02e-05) | – |
| resize-storm | 0.404 (4.28e-06) | 0.599 (2.51e-05) | 192 (0.108) | 192 (0.108) | 0 (0) | 355 (6.93) | 0 (0) | 0 (0) | 23.4 (0.000344) | 14.1 (0.299) | 48.8 (0.138) | 44.1 (0.418) | – |

Cells below are median [Q1–Q3] over the measured runs.

### home

Chrome only: sig's home page, no engine. Types into the URL bar.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.404 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.404 [0.354–0.404] | 5 |
| startup.cpu_s | 0.37 [0.33–0.37] | 5 |
| steady.cpu_pct | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_browser | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–191] | 5 |
| steady.wakeups_per_s_browser | 191 [191–191] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 209 [207–210] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.66 [1.64–1.66] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 4.10 [4.10–4.20] | 5 |
| active.cpu_pct | 2.00 [2.00–2.00] | 5 |
| active.cpu_pct_browser | 2.00 [2.00–2.00] | 5 |
| active.cpu_pct_engines | 0 [0–0] | 5 |
| active.wakeups_per_s | 360 [353–361] | 5 |
| active.pss_peak_mib | 210 [210–212] | 5 |
| active.browser_fps | 0.799 [0.799–0.999] | 5 |
| active.browser_draw_p50_ms | 0.668 [0.667–0.692] | 5 |
| active.ipc_out_mib_per_s | 0 [0–0] | 5 |
| active.gpu_util_pct | 7.46 [7.36–7.64] | 5 |
| active.compositor_cpu_pct | 9.99 [9.99–9.99] | 5 |
| shutdown.s | 0.108 [0.108–0.108] | 5 |

### article

Static Servo page: a text-heavy article with four images. Scrolls down, then up.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.404 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.454 [0.454–0.454] | 5 |
| startup.first_frame_engine_s | 0.44 [0.395–0.443] | 5 |
| startup.cpu_s | 0.61 [0.58–0.61] | 5 |
| steady.cpu_pct | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_browser | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–192] | 5 |
| steady.wakeups_per_s_browser | 191 [191–192] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 356 [354–357] | 5 |
| steady.fps_produced | 0 [0–0] | 5 |
| steady.fps_delivered | 0 [0–0] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.readback_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_pixel_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.74 [1.74–1.76] | 5 |
| steady.gpu_fb_tree_mib | 245 [245–245] | 5 |
| steady.compositor_cpu_pct | 4.39 [4.29–4.39] | 5 |
| active.cpu_pct | 9.79 [9.79–9.98] | 5 |
| active.cpu_pct_browser | 2.90 [2.90–2.90] | 5 |
| active.cpu_pct_engines | 6.89 [6.79–7.09] | 5 |
| active.wakeups_per_s | 1676 [1676–1678] | 5 |
| active.pss_peak_mib | 372 [371–373] | 5 |
| active.fps_produced | 9.98 [9.98–9.99] | 5 |
| active.fps_delivered | 9.98 [9.98–9.99] | 5 |
| active.browser_fps | 9.98 [9.98–9.99] | 5 |
| active.browser_draw_p50_ms | 0.506 [0.496–0.51] | 5 |
| active.input_to_frame_p50_ms | 7.52 [7.47–7.66] | 5 |
| active.input_to_screen_p50_ms | 17.0 [16.8–17.2] | 5 |
| active.input_to_screen_p95_ms | 25.1 [24.9–25.1] | 5 |
| active.readback_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.ipc_pixel_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.ipc_out_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.paint_p50_ms | 0.5 [0.5–0.5] | 5 |
| active.read_p50_ms | 3.70 [3.70–3.70] | 5 |
| active.send_p50_ms | 1.60 [1.60–1.70] | 5 |
| active.gpu_util_pct | 10.1 [9.96–10.1] | 5 |
| active.compositor_cpu_pct | 7.99 [7.99–8.09] | 5 |
| shutdown.s | 0.158 [0.158–0.166] | 5 |

### long

Scrolling a long text page (80 sections, ~26 000 words) with the mouse wheel.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.454 [0.404–0.454] | 5 |
| startup.first_frame_engine_s | 0.417 [0.39–0.433] | 5 |
| startup.cpu_s | 0.7 [0.69–0.72] | 5 |
| steady.cpu_pct | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_browser | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–191] | 5 |
| steady.wakeups_per_s_browser | 191 [191–191] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 398 [395–404] | 5 |
| steady.fps_produced | 0 [0–0] | 5 |
| steady.fps_delivered | 0 [0–0] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.readback_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_pixel_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.74 [1.70–1.78] | 5 |
| steady.gpu_fb_tree_mib | 245 [245–245] | 5 |
| steady.compositor_cpu_pct | 4.19 [4.19–4.29] | 5 |
| active.cpu_pct | 14.6 [14.6–14.7] | 5 |
| active.cpu_pct_browser | 3.00 [3.00–3.00] | 5 |
| active.cpu_pct_engines | 11.6 [11.6–11.7] | 5 |
| active.wakeups_per_s | 1679 [1678–1679] | 5 |
| active.pss_peak_mib | 414 [410–418] | 5 |
| active.fps_produced | 9.98 [9.98–9.98] | 5 |
| active.fps_delivered | 9.98 [9.98–9.98] | 5 |
| active.browser_fps | 9.98 [9.98–9.98] | 5 |
| active.browser_draw_p50_ms | 0.512 [0.511–0.521] | 5 |
| active.input_to_frame_p50_ms | 12.7 [12.6–12.7] | 5 |
| active.input_to_screen_p50_ms | 22.5 [22.1–22.7] | 5 |
| active.input_to_screen_p95_ms | 30.3 [30.2–30.4] | 5 |
| active.readback_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.ipc_pixel_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.ipc_out_mib_per_s | 33.9 [33.9–33.9] | 5 |
| active.paint_p50_ms | 0.5 [0.5–0.5] | 5 |
| active.read_p50_ms | 3.70 [3.70–3.70] | 5 |
| active.send_p50_ms | 1.70 [1.70–1.70] | 5 |
| active.gpu_util_pct | 9.64 [9.62–9.78] | 5 |
| active.compositor_cpu_pct | 7.29 [7.29–7.29] | 5 |
| shutdown.s | 0.158 [0.158–0.167] | 5 |

### css-animation

Servo: endless CSS transform, opacity, color and width animations. Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.354] | 5 |
| startup.first_frame_s | 0.404 [0.404–0.404] | 5 |
| startup.first_frame_engine_s | 0.391 [0.391–0.394] | 5 |
| startup.cpu_s | 1.57 [1.57–1.59] | 5 |
| steady.cpu_pct | 51.1 [50.3–51.4] | 5 |
| steady.cpu_pct_browser | 12.1 [12.0–12.1] | 5 |
| steady.cpu_pct_engines | 39.2 [38.1–39.3] | 5 |
| steady.wakeups_per_s | 7724 [7714–7724] | 5 |
| steady.wakeups_per_s_browser | 3665 [3664–3666] | 5 |
| steady.wakeups_per_s_engines | 4057 [4054–4058] | 5 |
| steady.pss_mean_mib | 350 [350–353] | 5 |
| steady.fps_produced | 59.1 [59.1–59.2] | 5 |
| steady.fps_delivered | 59.1 [59.1–59.2] | 5 |
| steady.browser_fps | 59.0 [59.0–59.0] | 5 |
| steady.readback_mib_per_s | 201 [201–201] | 5 |
| steady.ipc_pixel_mib_per_s | 201 [201–201] | 5 |
| steady.ipc_out_mib_per_s | 201 [201–201] | 5 |
| steady.gpu_util_pct | 25.4 [24.6–25.5] | 5 |
| steady.gpu_fb_tree_mib | 259 [259–259] | 5 |
| steady.compositor_cpu_pct | 14.7 [14.7–14.9] | 5 |
| active.cpu_pct | 61.7 [61.6–62.5] | 5 |
| active.cpu_pct_browser | 11.8 [11.7–11.8] | 5 |
| active.cpu_pct_engines | 49.9 [49.8–50.4] | 5 |
| active.wakeups_per_s | 7879 [7862–7889] | 5 |
| active.pss_peak_mib | 352 [352–355] | 5 |
| active.fps_produced | 59.2 [59.2–59.3] | 5 |
| active.fps_delivered | 59.2 [59.2–59.3] | 5 |
| active.browser_fps | 59.3 [59.2–59.3] | 5 |
| active.browser_draw_p50_ms | 0.498 [0.488–0.5] | 5 |
| active.readback_mib_per_s | 201 [201–202] | 5 |
| active.ipc_pixel_mib_per_s | 201 [201–202] | 5 |
| active.ipc_out_mib_per_s | 202 [202–202] | 5 |
| active.paint_p50_ms | 0.5 [0.5–0.5] | 5 |
| active.read_p50_ms | 5.50 [5.50–5.60] | 5 |
| active.send_p50_ms | 1.60 [1.60–1.60] | 5 |
| active.gpu_util_pct | 35.1 [34.9–35.7] | 5 |
| active.compositor_cpu_pct | 15.6 [15.4–16.8] | 5 |
| shutdown.s | 0.166 [0.166–0.166] | 5 |

### canvas

Servo: a 2D canvas redrawn every animation frame (300 balls). Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.354] | 5 |
| startup.first_frame_s | 0.404 [0.404–0.404] | 5 |
| startup.first_frame_engine_s | 0.395 [0.394–0.396] | 5 |
| startup.cpu_s | 2.80 [2.78–2.80] | 5 |
| steady.cpu_pct | 83.8 [82.3–84.3] | 5 |
| steady.cpu_pct_browser | 12.6 [12.6–12.7] | 5 |
| steady.cpu_pct_engines | 71.0 [69.7–71.6] | 5 |
| steady.wakeups_per_s | 10425 [10405–10436] | 5 |
| steady.wakeups_per_s_browser | 3678 [3675–3678] | 5 |
| steady.wakeups_per_s_engines | 6747 [6730–6755] | 5 |
| steady.pss_mean_mib | 371 [371–371] | 5 |
| steady.fps_produced | 59.2 [59.1–59.2] | 5 |
| steady.fps_delivered | 59.2 [59.1–59.2] | 5 |
| steady.browser_fps | 59.2 [59.0–59.3] | 5 |
| steady.readback_mib_per_s | 201 [201–201] | 5 |
| steady.ipc_pixel_mib_per_s | 201 [201–201] | 5 |
| steady.ipc_out_mib_per_s | 201 [201–201] | 5 |
| steady.gpu_util_pct | 23.7 [23.6–24.5] | 5 |
| steady.gpu_fb_tree_mib | 231 [231–231] | 5 |
| steady.compositor_cpu_pct | 11.6 [11.5–12.2] | 5 |
| active.cpu_pct | 87.5 [87.3–88.0] | 5 |
| active.cpu_pct_browser | 12.6 [12.6–12.7] | 5 |
| active.cpu_pct_engines | 74.9 [74.6–75.5] | 5 |
| active.wakeups_per_s | 10584 [10573–10586] | 5 |
| active.pss_peak_mib | 373 [372–374] | 5 |
| active.fps_produced | 59.3 [59.2–59.3] | 5 |
| active.fps_delivered | 59.3 [59.2–59.3] | 5 |
| active.browser_fps | 59.2 [59.2–59.3] | 5 |
| active.browser_draw_p50_ms | 0.607 [0.606–0.608] | 5 |
| active.readback_mib_per_s | 201 [201–202] | 5 |
| active.ipc_pixel_mib_per_s | 201 [201–202] | 5 |
| active.ipc_out_mib_per_s | 202 [202–202] | 5 |
| active.paint_p50_ms | 0.6 [0.6–0.6] | 5 |
| active.read_p50_ms | 3.40 [3.40–3.40] | 5 |
| active.send_p50_ms | 1.60 [1.60–1.60] | 5 |
| active.gpu_util_pct | 27.6 [27.2–29.6] | 5 |
| active.compositor_cpu_pct | 12.9 [12.9–12.9] | 5 |
| shutdown.s | 0.108 [0.108–0.108] | 5 |

### webgl

Servo: a WebGL quad rotating in 3D, redrawn every animation frame. Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.404 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.454 [0.404–0.454] | 5 |
| startup.first_frame_engine_s | 0.435 [0.402–0.438] | 5 |
| startup.cpu_s | 1.53 [1.49–1.53] | 5 |
| steady.cpu_pct | 45.9 [45.6–48.1] | 5 |
| steady.cpu_pct_browser | 12.1 [12.1–12.2] | 5 |
| steady.cpu_pct_engines | 34.1 [33.3–36.0] | 5 |
| steady.wakeups_per_s | 7788 [7784–7794] | 5 |
| steady.wakeups_per_s_browser | 3670 [3663–3670] | 5 |
| steady.wakeups_per_s_engines | 4123 [4121–4126] | 5 |
| steady.pss_mean_mib | 358 [356–359] | 5 |
| steady.fps_produced | 59.2 [59.2–59.3] | 5 |
| steady.fps_delivered | 59.2 [59.2–59.3] | 5 |
| steady.browser_fps | 59.1 [59.1–59.2] | 5 |
| steady.readback_mib_per_s | 201 [201–202] | 5 |
| steady.ipc_pixel_mib_per_s | 201 [201–202] | 5 |
| steady.ipc_out_mib_per_s | 202 [201–202] | 5 |
| steady.gpu_util_pct | 24.7 [24.5–24.8] | 5 |
| steady.gpu_fb_tree_mib | 252 [252–252] | 5 |
| steady.compositor_cpu_pct | 14.2 [13.9–14.4] | 5 |
| active.cpu_pct | 54.2 [53.1–56.0] | 5 |
| active.cpu_pct_browser | 11.7 [11.6–11.9] | 5 |
| active.cpu_pct_engines | 42.5 [41.1–44.1] | 5 |
| active.wakeups_per_s | 7932 [7925–7939] | 5 |
| active.pss_peak_mib | 360 [357–360] | 5 |
| active.fps_produced | 59.3 [59.2–59.3] | 5 |
| active.fps_delivered | 59.3 [59.2–59.3] | 5 |
| active.browser_fps | 59.2 [59.1–59.3] | 5 |
| active.browser_draw_p50_ms | 0.452 [0.451–0.476] | 5 |
| active.readback_mib_per_s | 202 [201–202] | 5 |
| active.ipc_pixel_mib_per_s | 202 [201–202] | 5 |
| active.ipc_out_mib_per_s | 202 [202–202] | 5 |
| active.paint_p50_ms | 0.6 [0.6–0.6] | 5 |
| active.read_p50_ms | 5.20 [5.00–5.50] | 5 |
| active.send_p50_ms | 1.60 [1.60–1.60] | 5 |
| active.gpu_util_pct | 34.4 [33.4–35.0] | 5 |
| active.compositor_cpu_pct | 16.4 [16.1–16.4] | 5 |
| shutdown.s | 0.167 [0.166–0.209] | 5 |

### wasm-hello

Wasm guest hello-sighurt: a static screen of canvas drawing and a text field, redrawn by on_frame at 60 Hz (the old sig-wasm sends a draw list only when it changed). Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.354 [0.354–0.404] | 5 |
| startup.cpu_s | 0.38 [0.37–0.41] | 5 |
| steady.cpu_pct | 1.30 [1.30–1.30] | 5 |
| steady.cpu_pct_browser | 0.999 [0.999–0.999] | 5 |
| steady.cpu_pct_engines | 0.3 [0.3–0.3] | 5 |
| steady.wakeups_per_s | 370 [358–380] | 5 |
| steady.wakeups_per_s_browser | 310 [298–320] | 5 |
| steady.wakeups_per_s_engines | 59.9 [59.9–59.9] | 5 |
| steady.pss_mean_mib | 232 [232–234] | 5 |
| steady.browser_fps | 2.00 [2.00–2.00] | 5 |
| steady.ipc_out_mib_per_s | 0.00191 [0.00191–0.00191] | 5 |
| steady.gpu_util_pct | 4.16 [4.10–4.18] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 4.70 [4.60–4.70] | 5 |
| active.cpu_pct | 3.30 [3.20–3.30] | 5 |
| active.cpu_pct_browser | 2.90 [2.80–2.90] | 5 |
| active.cpu_pct_engines | 0.4 [0.4–0.4] | 5 |
| active.wakeups_per_s | 544 [543–551] | 5 |
| active.pss_peak_mib | 234 [234–236] | 5 |
| active.browser_fps | 22.0 [20.0–22.0] | 5 |
| active.browser_draw_p50_ms | 0.787 [0.781–0.792] | 5 |
| active.ipc_out_mib_per_s | 0.0211 [0.0191–0.0211] | 5 |
| active.gpu_util_pct | 7.00 [6.84–9.74] | 5 |
| active.compositor_cpu_pct | 9.29 [8.99–9.39] | 5 |
| shutdown.s | 0.108 [0.108–0.108] | 5 |

### wasm-gradient

Wasm guest gradient-demo: a static grid of gradients drawn with the canvas API. Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.354 [0.354–0.404] | 5 |
| startup.cpu_s | 0.36 [0.35–0.4] | 5 |
| steady.cpu_pct | 0.899 [0.899–0.999] | 5 |
| steady.cpu_pct_browser | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_engines | 0.3 [0.3–0.3] | 5 |
| steady.wakeups_per_s | 252 [252–252] | 5 |
| steady.wakeups_per_s_browser | 192 [192–192] | 5 |
| steady.wakeups_per_s_engines | 60.0 [60.0–60.0] | 5 |
| steady.pss_mean_mib | 234 [232–235] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.74 [1.72–1.78] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 4.30 [4.20–4.30] | 5 |
| active.cpu_pct | 1.10 [1.10–1.20] | 5 |
| active.cpu_pct_browser | 0.899 [0.799–0.899] | 5 |
| active.cpu_pct_engines | 0.3 [0.3–0.3] | 5 |
| active.wakeups_per_s | 330 [330–330] | 5 |
| active.pss_peak_mib | 235 [232–235] | 5 |
| active.browser_fps | 0 [0–0] | 5 |
| active.ipc_out_mib_per_s | 0 [0–0] | 5 |
| active.gpu_util_pct | 2.00 [2.00–2.06] | 5 |
| active.compositor_cpu_pct | 4.70 [4.70–4.70] | 5 |
| shutdown.s | 0.108 [0.108–0.116] | 5 |

### wasm-raf

Wasm guest raf-demo: a ball animated with request_animation_frame (animation). Pointer moves in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.354 [0.354–0.404] | 5 |
| startup.cpu_s | 0.51 [0.51–0.56] | 5 |
| steady.cpu_pct | 5.79 [5.69–5.89] | 5 |
| steady.cpu_pct_browser | 5.49 [5.39–5.49] | 5 |
| steady.cpu_pct_engines | 0.3 [0.3–0.3] | 5 |
| steady.wakeups_per_s | 613 [612–614] | 5 |
| steady.wakeups_per_s_browser | 553 [552–555] | 5 |
| steady.wakeups_per_s_engines | 59.9 [59.9–59.9] | 5 |
| steady.pss_mean_mib | 233 [233–235] | 5 |
| steady.browser_fps | 58.6 [58.5–58.6] | 5 |
| steady.ipc_out_mib_per_s | 0.0332 [0.0332–0.0332] | 5 |
| steady.gpu_util_pct | 12.6 [12.4–12.7] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 15.0 [14.8–15.1] | 5 |
| active.cpu_pct | 5.99 [5.99–6.09] | 5 |
| active.cpu_pct_browser | 5.59 [5.59–5.59] | 5 |
| active.cpu_pct_engines | 0.4 [0.4–0.499] | 5 |
| active.wakeups_per_s | 694 [694–694] | 5 |
| active.pss_peak_mib | 233 [233–235] | 5 |
| active.browser_fps | 58.3 [58.2–58.3] | 5 |
| active.browser_draw_p50_ms | 0.691 [0.691–0.692] | 5 |
| active.ipc_out_mib_per_s | 0.0332 [0.0332–0.0332] | 5 |
| active.gpu_util_pct | 15.7 [15.7–15.8] | 5 |
| active.compositor_cpu_pct | 16.9 [16.8–16.9] | 5 |
| shutdown.s | 0.116 [0.116–0.116] | 5 |

### wasm-timer

Wasm guest timer-demo: idle in the steady window; in the active window its four buttons are clicked, starting set_interval timers of 1 s, 0.5 s and 0.1 s and a 3 s set_timeout.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.404] | 5 |
| startup.first_frame_s | 0.354 [0.354–0.404] | 5 |
| startup.cpu_s | 0.36 [0.36–0.39] | 5 |
| steady.cpu_pct | 0.999 [0.899–0.999] | 5 |
| steady.cpu_pct_browser | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_engines | 0.3 [0.3–0.3] | 5 |
| steady.wakeups_per_s | 251 [251–252] | 5 |
| steady.wakeups_per_s_browser | 191 [191–192] | 5 |
| steady.wakeups_per_s_engines | 59.9 [59.9–59.9] | 5 |
| steady.pss_mean_mib | 231 [231–234] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.76 [1.68–1.78] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 4.40 [4.20–4.40] | 5 |
| active.cpu_pct | 2.00 [2.00–2.10] | 5 |
| active.cpu_pct_browser | 1.80 [1.70–1.80] | 5 |
| active.cpu_pct_engines | 0.3 [0.3–0.3] | 5 |
| active.wakeups_per_s | 432 [432–432] | 5 |
| active.pss_peak_mib | 234 [233–236] | 5 |
| active.browser_fps | 11.4 [11.2–11.5] | 5 |
| active.browser_draw_p50_ms | 0.647 [0.639–0.652] | 5 |
| active.ipc_out_mib_per_s | 0.014 [0.0138–0.0141] | 5 |
| active.gpu_util_pct | 4.22 [2.96–4.36] | 5 |
| active.compositor_cpu_pct | 6.89 [4.40–6.99] | 5 |
| shutdown.s | 0.108 [0.108–0.116] | 5 |

### wasm-busy

Plan §26's hostile Wasm workload: a guest (workloads/generate.py) that spends about 36M of its 50M fuel on every frame and draws one unchanging rectangle. Chrome input in the active window: a new tab and closing it again, twice a second.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.353–0.404] | 5 |
| startup.first_frame_s | 0.354 [0.353–0.404] | 5 |
| startup.cpu_s | 1.01 [1.01–1.05] | 5 |
| steady.cpu_pct | 21.6 [21.6–21.6] | 5 |
| steady.cpu_pct_browser | 0.699 [0.699–0.699] | 5 |
| steady.cpu_pct_engines | 20.9 [20.9–21.0] | 5 |
| steady.wakeups_per_s | 252 [252–252] | 5 |
| steady.wakeups_per_s_browser | 192 [192–192] | 5 |
| steady.wakeups_per_s_engines | 60.0 [60.0–60.0] | 5 |
| steady.pss_mean_mib | 231 [230–233] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.74 [1.72–1.80] | 5 |
| steady.gpu_fb_tree_mib | 169 [169–169] | 5 |
| steady.compositor_cpu_pct | 4.30 [4.20–4.30] | 5 |
| active.cpu_pct | 22.7 [22.7–22.8] | 5 |
| active.cpu_pct_browser | 1.70 [1.70–1.70] | 5 |
| active.cpu_pct_engines | 21.0 [21.0–21.1] | 5 |
| active.wakeups_per_s | 396 [391–400] | 5 |
| active.pss_peak_mib | 232 [231–234] | 5 |
| active.browser_fps | 2.40 [2.30–2.40] | 5 |
| active.browser_draw_p50_ms | 0.549 [0.534–0.555] | 5 |
| active.input_to_screen_p50_ms | 7.54 [5.30–7.64] | 5 |
| active.input_to_screen_p95_ms | 491 [491–491] | 5 |
| active.ipc_out_mib_per_s | 0 [0–0] | 5 |
| active.gpu_util_pct | 2.04 [2.00–2.06] | 5 |
| active.compositor_cpu_pct | 5.39 [5.29–5.39] | 5 |
| shutdown.s | 0.108 [0.108–0.108] | 5 |

### many-tabs

Eight tabs of the article, one visible: what hidden tabs cost. Switches tab once a second in the active window.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.404 [0.404–0.404] | 5 |
| startup.first_frame_s | 0.754 [0.754–0.755] | 5 |
| startup.first_frame_engine_s | 0.734 [0.705–0.745] | 5 |
| startup.cpu_s | 1.91 [1.87–1.91] | 5 |
| steady.cpu_pct | 0.697 [0.598–0.697] | 5 |
| steady.cpu_pct_browser | 0.697 [0.598–0.697] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–191] | 5 |
| steady.wakeups_per_s_browser | 191 [191–191] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 580 [580–582] | 5 |
| steady.fps_produced | 0 [0–0] | 5 |
| steady.fps_delivered | 0 [0–0] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.readback_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_pixel_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 2.00 [2.00–2.06] | 5 |
| steady.gpu_fb_tree_mib | 317 [317–317] | 5 |
| steady.compositor_cpu_pct | 4.58 [4.48–4.58] | 5 |
| active.cpu_pct | 8.86 [8.63–8.87] | 5 |
| active.cpu_pct_browser | 1.69 [1.69–1.74] | 5 |
| active.cpu_pct_engines | 7.07 [6.94–7.17] | 5 |
| active.wakeups_per_s | 722 [722–723] | 5 |
| active.pss_peak_mib | 956 [953–956] | 5 |
| active.fps_produced | 2.78 [2.69–2.79] | 5 |
| active.fps_delivered | 1.39 [1.29–1.39] | 5 |
| active.browser_fps | 2.68 [2.49–2.69] | 5 |
| active.browser_draw_p50_ms | 0.703 [0.699–0.752] | 5 |
| active.input_to_screen_p50_ms | 6.91 [6.06–7.46] | 5 |
| active.input_to_screen_p95_ms | 33.3 [32.6–34.0] | 5 |
| active.readback_mib_per_s | 9.43 [9.14–9.48] | 5 |
| active.ipc_pixel_mib_per_s | 4.72 [4.40–4.74] | 5 |
| active.ipc_out_mib_per_s | 4.74 [4.72–4.74] | 5 |
| active.paint_p50_ms | 0.3 [0.2–0.3] | 5 |
| active.read_p50_ms | 4.80 [4.70–4.80] | 5 |
| active.send_p50_ms | 2.60 [2.60–2.60] | 5 |
| active.gpu_util_pct | 2.04 [2.00–8.55] | 5 |
| active.compositor_cpu_pct | 5.55 [5.48–5.78] | 5 |
| shutdown.s | 0.368 [0.36–1.12] | 5 |

### tab-churn

Lifecycle: the article, then ten cycles of opening a new tab, loading the article in it and closing it (one every 2 s); measured again 3 s after the last close.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.354] | 5 |
| startup.first_frame_s | 0.404 [0.404–0.404] | 5 |
| startup.first_frame_engine_s | 0.394 [0.39–0.395] | 5 |
| startup.cpu_s | 0.58 [0.57–0.58] | 5 |
| steady.cpu_pct | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_browser | 0.699 [0.599–0.699] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 191 [191–192] | 5 |
| steady.wakeups_per_s_browser | 191 [191–192] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 354 [353–363] | 5 |
| steady.fps_produced | 0 [0–0] | 5 |
| steady.fps_delivered | 0 [0–0] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.readback_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_pixel_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.68 [1.64–1.72] | 5 |
| steady.gpu_fb_tree_mib | 225 [225–225] | 5 |
| steady.compositor_cpu_pct | 4.19 [4.19–4.29] | 5 |
| active.cpu_pct | 13.1 [12.8–13.2] | 5 |
| active.cpu_pct_browser | 1.75 [1.70–1.75] | 5 |
| active.cpu_pct_engines | 11.4 [11.1–11.5] | 5 |
| active.wakeups_per_s | 707 [706–708] | 5 |
| active.pss_peak_mib | 467 [464–472] | 5 |
| active.fps_produced | 2.00 [2.00–2.00] | 5 |
| active.fps_delivered | 0.999 [0.999–0.999] | 5 |
| active.browser_fps | 2.85 [2.85–2.85] | 5 |
| active.browser_draw_p50_ms | 0.583 [0.576–0.583] | 5 |
| active.readback_mib_per_s | 6.79 [6.79–6.79] | 5 |
| active.ipc_pixel_mib_per_s | 3.40 [3.40–3.40] | 5 |
| active.ipc_out_mib_per_s | 3.40 [3.40–3.40] | 5 |
| active.paint_p50_ms | 0.2 [0.2–0.2] | 5 |
| active.read_p50_ms | 1.20 [1.00–1.50] | 5 |
| active.send_p50_ms | 2.50 [2.50–2.50] | 5 |
| active.gpu_util_pct | 0.6 [0.52–0.76] | 5 |
| active.compositor_cpu_pct | 2.10 [2.05–2.10] | 5 |
| after.rss_growth_mib | 11.9 [11.8–18.4] | 5 |
| after.pss_growth_mib | 11.7 [11.6–18.3] | 5 |
| shutdown.s | 0.158 [0.158–0.158] | 5 |

### resize-storm

The article while the output changes size and scale four times a second (plan §25 resize storm); restored and measured 3 s later.

| metric | median [Q1–Q3] | n |
|---|---|---|
| startup.mapped_s | 0.354 [0.354–0.354] | 5 |
| startup.first_frame_s | 0.404 [0.404–0.404] | 5 |
| startup.first_frame_engine_s | 0.391 [0.391–0.393] | 5 |
| startup.cpu_s | 0.57 [0.57–0.58] | 5 |
| steady.cpu_pct | 0.599 [0.599–0.599] | 5 |
| steady.cpu_pct_browser | 0.599 [0.599–0.599] | 5 |
| steady.cpu_pct_engines | 0 [0–0] | 5 |
| steady.wakeups_per_s | 192 [191–192] | 5 |
| steady.wakeups_per_s_browser | 192 [191–192] | 5 |
| steady.wakeups_per_s_engines | 0 [0–0] | 5 |
| steady.pss_mean_mib | 355 [354–361] | 5 |
| steady.fps_produced | 0 [0–0] | 5 |
| steady.fps_delivered | 0 [0–0] | 5 |
| steady.browser_fps | 0 [0–0] | 5 |
| steady.readback_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_pixel_mib_per_s | 0 [0–0] | 5 |
| steady.ipc_out_mib_per_s | 0 [0–0] | 5 |
| steady.gpu_util_pct | 1.70 [1.66–1.72] | 5 |
| steady.gpu_fb_tree_mib | 153 [153–153] | 5 |
| steady.compositor_cpu_pct | 4.19 [4.19–4.19] | 5 |
| active.cpu_pct | 23.4 [23.4–23.4] | 5 |
| active.cpu_pct_browser | 4.99 [4.99–5.09] | 5 |
| active.cpu_pct_engines | 18.4 [18.4–18.5] | 5 |
| active.wakeups_per_s | 1985 [1984–1994] | 5 |
| active.pss_peak_mib | 466 [464–468] | 5 |
| active.fps_produced | 16.0 [15.9–16.0] | 5 |
| active.fps_delivered | 14.1 [14.0–14.3] | 5 |
| active.browser_fps | 15.1 [15.1–15.2] | 5 |
| active.browser_draw_p50_ms | 0.419 [0.416–0.428] | 5 |
| active.readback_mib_per_s | 48.8 [48.8–48.9] | 5 |
| active.ipc_pixel_mib_per_s | 44.1 [44.0–44.4] | 5 |
| active.ipc_out_mib_per_s | 44.1 [44.0–44.4] | 5 |
| active.paint_p50_ms | 0.2 [0.2–0.2] | 5 |
| active.read_p50_ms | 3.90 [3.90–4.00] | 5 |
| active.send_p50_ms | 1.30 [1.20–1.30] | 5 |
| active.gpu_util_pct | 20.9 [3.72–25.0] | 5 |
| active.compositor_cpu_pct | 15.6 [15.5–15.6] | 5 |
| after.rss_growth_mib | 106 [99.4–107] | 5 |
| after.pss_growth_mib | 106 [102–107] | 5 |
| shutdown.s | 0.158 [0.158–0.166] | 5 |

### Anomalies

- 1-resize-storm: did not quit within 10 s of ctrl-q; sent SIGTERM
- 2-article: did not quit within 10 s of ctrl-q; sent SIGTERM
- 2-many-tabs: did not quit within 10 s of ctrl-q; sent SIGTERM
- 2-wasm-gradient: did not quit within 10 s of ctrl-q; sent SIGTERM
- 3-wasm-timer: did not quit within 10 s of ctrl-q; sent SIGTERM
- 3-webgl: did not quit within 10 s of ctrl-q; sent SIGTERM
- 5-home: did not quit within 10 s of ctrl-q; sent SIGTERM

