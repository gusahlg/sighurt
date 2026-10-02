# Removing the IPC (with GPUI and the readback), measured

What did taking the engines out of their own processes do to Sighurt's performance? This
compares the pre-rewrite browser (`gpui-baseline`: GPUI chrome, every page in its own engine
process over a pipe, Servo frames read back to the CPU) with the rewrite at 3c510fe (a CPU chrome,
Servo and the Wasm engine in `sig`'s process, Servo presenting straight into its subsurface), both
measured in one interleaved session on louise-pc. The predictions were written before the first
run ([`../HYPOTHESES.md`](../HYPOTHESES.md), "Predictions for the IPC-removal session").

## In short

On the same machine, compositor, driver and harness, 7 measured rounds a side; every difference
below has p = 0.0006, the smallest a 7-against-7 test can give:

| | baseline | rewrite | change |
|---|---|---|---|
| CSS animation, CPU (steady, % of one core) | 52.3 | 13.7 | −74 % |
| WebGL, CPU | 42.2 | 10.1 | −76 % |
| 2D canvas, CPU | 83.2 | 47.2 | −43 % |
| CSS animation, wakeups/s | 7,728 | 851 | −89 % |
| Idle (static page or chrome), wakeups/s | 191 | 0.1 | −100 % |
| Page pixels read back to the CPU, animations | 201 MiB/s | 0 | −100 % |
| Page pixels through a pipe, animations | 202 MiB/s | 0 (no pipe) | −100 % |
| Memory (PSS), one article | 375 MiB | 184 MiB | −51 % |
| Memory, eight article tabs, after visiting all | 978 MiB | 409 MiB | −58 % |
| Memory, a Wasm guest | 242–246 MiB | 78–82 MiB | −67 % |
| Launch → page on screen, article | 0.40 s | 0.15 s | −62 % |
| Scroll → frame submitted, article (p50) | 17.4 ms | 2.2 ms | −87 % |
| New tab or close beside a busy guest → chrome on screen (p50) | 33.1 ms | 16.7 ms | −49 % |

The pipe by itself, measured alone at the baseline's frame size and rate, costs **6,500
wakeups a second and 10.6 % of a core** (`bench/pipe-cost`): 84 % of the baseline's wakeups on
an animated page, and about a fifth of its CPU. The readback, the frame comparison and GPUI's
second presentation cost most of the rest. 20 of the 25 predictions held. Two were too
pessimistic (tab switches did not get slower; shutdown got faster), one missed by a hair
(`wasm-gradient`'s CPU rose by 0.1 point), and two failed in ways that point at work for the next
wave: tab churn leaves about 70 MiB more behind (it levels off), and static Wasm guests still wake
their process 250 times a second. Not predicted: Wasm frames moved a page-sized upload into the
compositor, which now spends twice the CPU on `wasm-raf`.

## Method

**Question and its limit.** The rewrite removed three things at once: the pipe that carried every
Servo frame from `sig-servo` to `sig` (sighurt-ipc over the engine's stdin and stdout), GPUI
(which swapped each frame to BGRA, uploaded it as a texture and drew the whole window), and the
readback that made those frames (`read_pixels` into CPU memory, a row flip, then a comparison with
the previous frame). No build has one without the others, so the session compares the two whole
browsers; the share of each part comes from the baseline's own timings and from
`bench/pipe-cost`, which measures the pipe alone.

**Builds.**

| | baseline | rewrite |
|---|---|---|
| Revision | `gpui-baseline` 359cb36 | 3c510fe (HEAD of `rewrite` when measured) |
| Built | 26 September 2026 by `bench/baseline/build.sh`, release | 2 October 2026 by `bench/rewrite/build.sh`: a `git archive`, its own empty `CARGO_TARGET_DIR`, `cargo build --release -p sighurt-browser` (default features: Servo and Wasm) |
| Rust | 1.95.0 (louise-pc's nixpkgs 9ae611a) | 1.98.1 (the pinned nixpkgs 4975466) |
| Binaries (sha256) | `sig` 26.2 MiB (`f31d11c4…`), `sig-servo` 151.4 MiB (`687ef772…`), `sig-wasm` 46.7 MiB (`9099b41f…`): the September baseline's | `sig` 204.5 MiB (`83bde874…`) |
| Processes | `sig` (GPUI) and one `sig-servo` or `sig-wasm` per page | `sig` |
| A Servo frame | painted, read back, flipped, compared, written to the pipe, read, swapped to BGRA, uploaded and drawn by GPUI | painted into the page's subsurface (EGL), swapped |
| Diagnostics | `SIG_SERVO_TIMING=1`, `ZED_MEASUREMENTS=1`: a line per Servo frame and per GPUI draw | `SIG_DIAG=1`: a line per page frame and chrome paint; `--diagnostics FILE` |

Both use Servo 0.5 and the release profile's defaults (`opt-level = 3`, no LTO, no debug info).

**Session.** `bench/run` with both builds: one warm-up round (not compared) and seven measured
rounds; each round ran all 16 workloads of `bench/workloads.toml` for both builds once, the 32
runs in one random order (seed 1653917054), so drift during the session fell on both builds
alike. 256 runs from 13:10 to 16:24 on 2 October 2026, none failed. Each ran in its own headless
sway, held the machine's heavy-job lock for itself (`--lock-runs 1`; other agents' builds and
tests ran between runs, never during one) and waited for the quiet-machine gate (< 0.5 busy CPUs,
≤ 5 % GPU) first. Every run got fresh configuration, data and state directories (Servo's profile;
the rewrite's saved tabs, which a trial showed it restores beside the ones it is asked to open)
and a per-build cache kept between runs, filled by the warm-up. Sets:
`louise/baseline/20261002-131052/` and `louise/rewrite/20261002-131052/`; the shared order:
`louise/sessions/20261002-131052.json`.

**Environment.** louise-pc: AMD Ryzen 5 5500 (12 CPUs, `amd-pstate-epp`, governor `powersave`, EPP
`balance_performance`, boost on), 15 GiB, NVIDIA GeForce RTX 4060 with driver 595.80, Linux
6.18.35, NixOS 26.11. Sway 1.12 from the pinned nixpkgs (`/nix/store/6816vyd7…-sway-1.12`, the
same store path the desktop has), headless backend, GLES2 renderer, one 1280 × 800 output at
scale 1 and 60 Hz. Both builds ran in the same `nix-shell` with the same harness, Python and
tools. The harness is this task's (sha256 `3d0d865f…`); after the session, one declaration was
added to `benchlib/parsers.py` (`ServoTiming.INCOMPARABLE`, below), which changes no number;
without it the files hash to the session's.

**Statistics.** `bench/compare`: medians, interquartile ranges, the relative change of the median,
a two-sided exact Mann–Whitney U test (7 against 7: the smallest possible p is 2/3432 ≈ 0.0006)
and Benjamini–Hochberg q-values over every row; a change counts only if p < 0.05 and the medians
differ by more than the larger IQR. Predictions are confirmatory; everything else is
exploratory.

**Only what both builds measure honestly is compared.** Left out, each listed by `bench/compare`:
the metrics that split the browser process from engine processes (`*_browser`, `*_engines`,
`ipc_out_*`), which compare process layouts; on the workloads whose script presses keys, the
baseline's window frames and chrome `input_to_screen` (GPUI's log misses the draws it makes while
dispatching a key); and `startup.first_frame_after_navigate_ms`, which `sig-servo` started after
its process had built Servo's renderer and `sig` starts before the view's renderer exists (9 ms
against 101 ms; the launch-to-screen times are the honest comparison). The metrics only one build
measures are listed at the end.

## The predictions

Medians over 7 runs a side, [Q1–Q3] where it matters; the full rows are in the appendix and in
`bench/compare --all` of the two sets.

| # | Prediction | Baseline → rewrite | Held |
|---|---|---|---|
| P1 | No readback on any Servo workload | `active.readback_mib_per_s` 6.8–202 → **0** on all 8 Servo workloads; `exit.page_readbacks` 0 in all 56 Servo runs; 0 GL reads in 24 audited runs (below) | yes |
| P2 | No pixel transport | `active.ipc_pixel_mib_per_s` 3.4–202 → 0 (there is no pipe) | yes, by construction |
| P3 | CPU ≥ 40 % lower on `css-animation`, `webgl` | steady 52.3 → 13.7 % (−74 %) and 42.2 → 10.1 % (−76 %); active 60.0 → 14.0 % (−77 %) and 52.7 → 10.5 % (−80 %) | yes |
| P4 | CPU ≥ 20 % lower on `canvas` | steady 83.2 → 47.2 % (−43 %); active 87.6 → 47.3 % (−46 %) | yes |
| P5 | Wakeups ≥ 40 % lower on the animations | `css-animation` 7,728 → 851/s (−89 %), `canvas` 10,436 → 3,675 (−65 %), `webgl` 7,791 → 887 (−89 %) | yes |
| P5b | The pipe alone makes 6,000–6,500 wakeups/s; the rewrite ≤ 2,000/s | `bench/pipe-cost`: 6,501/s; rewrite 851 and 887/s | yes |
| P6 | Less GPU work on the animations | `active.gpu_util_pct` 34.8 → 19.2 % (p 0.023), 30.2 → 24.2 %, 32.0 → 19.8 % (p 0.017) | yes |
| P7 | Frame rate ≥ 58/s | 59.1–59.2 → 58.2–58.3/s | yes, but 1–2 % lower beyond noise (below) |
| P8 | No chrome repaints from page animation | `steady.browser_fps` 58.5–59.5 → 0 on `css-animation`, `canvas`, `webgl`, `wasm-raf`, `wasm-workers` | yes |
| P9 | The idle browser sleeps | `steady.wakeups_per_s` 191 → 0.1 (the one SIGUSR1 per window) and CPU 0.7–0.8 → 0 % on `home`, `article`, `long`, `many-tabs`, `tab-churn`, `resize-storm` | yes |
| P10 | Static guests wake 60–200 times a second | `wasm-hello` 374 → 261/s (−30 %); `wasm-gradient` 252 → 246 and `wasm-timer` 251 → 249 (noise) | **no** |
| P11 | `wasm-raf` costs more CPU (drawn on the CPU now) | 5.89 → 6.10 % (+3 %, p 0.0012): far less than expected; the cost went to the compositor (below) | barely |
| P12 | Static guests' CPU not higher | `wasm-hello` 1.40 → 0.90 %, `wasm-timer` 1.0 → 0.6 %, `wasm-gradient` 1.0 → 1.1 % (+10 %: one 10 ms tick in 10 s) | 2 of 3 |
| P13 | PSS ≥ 25 % lower with one page, ≥ 10 % with a guest | `home` 217 → 72 MiB (−67 %); one-page Servo 371–418 → 176–225 (−46 to −53 %); Wasm 242–516 → 78–151 (−67 to −71 %) | yes |
| P14 | `many-tabs` PSS ≥ 10 % lower | steady 604 → 345 MiB (−43 %); peak after visiting all eight 978 → 409 (−58 %) | yes |
| P15 | Tab switches reach the screen later (layout transactions wait for the page) | `many-tabs` `input_to_pixels_p50_ms` 33.8 → 33.3 ms [19.7–33.4] (noise) | **no**: no regression |
| P16 | Chrome input beside busy guests not slower | `wasm-busy` 33.1 → 16.7 ms, `wasm-workers` 33.4 → 16.8 ms (p95 33.6 → 17.0, 33.7 → 17.2) | yes |
| P17 | The flood is bounded | forced quits 7 of 7 → 0; chrome inputs missed 20 of 20 → 0; first frame 5.56 → 0.30 s | yes |
| P18 | Scroll latency ≥ 3 ms lower | `input_to_screen_p50_ms` `article` 17.4 → 2.2, `long` 21.9 → 7.6 (p95 25.0 → 3.9, 29.9 → 9.3) | yes |
| P19 | Resizes stay bounded | `resize-storm` `after.pss_growth_mib` 111 → 15 | yes |
| P20 | Tab churn leaves no more memory than the baseline's 13 MiB | `tab-churn` `after.pss_growth_mib` 12.9 → 58.2 MiB | **no**: regression (it levels off, below) |
| P21 | The window appears no later | `startup.mapped_s` 0.35–0.40 → 0.05–0.15 s (`wasm-flood` 0.35 → 0.30) | yes |
| P22 | The first page frame appears no later | `startup.first_frame_s` 0.40–0.75 → 0.15–0.20 s on the Servo workloads | yes, and earlier |
| P23 | Shutdown may take longer with Servo in-process | 0.10–0.35 → 0.05–0.15 s (lower); `wasm-flood` 10.1 → 0.05 s | lower, not longer |
| P24 | Less GPU memory with one page; `many-tabs` may rise | `steady.gpu_fb_tree_mib` 153–259 → 46–92 MiB (Servo), 169–199 → 0 (`home`, Wasm: no GL context); `many-tabs` 291 → 100 | yes; no rise |

## Where the baseline's cost went

**The pipe alone** (`bench/pipe-cost`: 1280 × 696 RGBA frames, 3,563,520 bytes, in sighurt-ipc's
framing, 59.3 a second from one process to another and read whole, with nothing else running;
5 runs of 10 s under the lock, `louise/pipe-cost/default-20261002.jsonl` and
`1mib-20261002.jsonl`):

| pipe buffer | wakeups/s (writer + reader) | CPU, % of one core (writer + reader) | a frame crosses in |
|---|---|---|---|
| 64 KiB (Linux's default, the baseline's) | 6,501 [6,499–6,502] (3,266 + 3,235) | 10.6 [10.6–10.7] (5.2 + 5.4) | 1.78 ms |
| 1 MiB (`F_SETPIPE_SZ`) | 463 [462–464] (238 + 225) | 9.9 [9.9–10.2] | 1.72 ms |

A frame fills a 64 KiB pipe 54 times, and each time the writer sleeps until the reader has
emptied it: the wakeups come from the pipe's size, the CPU from the two copies into and out of the
kernel. A bigger pipe would have removed the wakeups, not the copies. It is a lower bound for the
baseline (one reused buffer, where `sig` allocated one per frame).

**Apportioned**, steady window, using the baseline's split between `sig` and `sig-servo`, the pipe
alone, and the rewrite's total as the cost of Servo's own work (an estimate: it assumes Servo does
the same work in both):

| % of one core (wakeups/s) | `css-animation` | `webgl` | `canvas` |
|---|---|---|---|
| Baseline total | 52.3 (7,728) | 42.2 (7,791) | 83.2 (10,436) |
| the pipe (writer + reader) | 10.6 (6,501) | 10.6 (6,501) | 10.6 (6,501) |
| `sig` without the pipe's reader: GPUI, BGRA swizzle, upload, draws | 6.8 (431) | 6.6 (428) | 7.3 (432) |
| `sig-servo` without the pipe's writer | 34.9 (791) | 25.0 (859) | 65.3 (3,497) |
| of which Servo's own work (the rewrite's total) | 13.7 (851) | 10.1 (887) | 47.2 (3,675) |
| leaves for the readback, row flip and comparison (`read_p50_ms` 5.0, 2.7, 3.0 ms a frame) | ≈ 21 | ≈ 15 | ≈ 18 |

So on an animated page the readback with its comparison is the largest part (about 40 % of the
baseline's CPU on `css-animation`), the pipe second (a fifth of the CPU, five sixths of the
wakeups), GPUI third (an eighth). On scrolling (`article`, 10 frames a second) the CPU fell from
11.2 to 2.9 % and the wakeups from 1,767 to 222/s; the pipe accounts for about 1,100 of those
wakeups at that rate. The compositor's own CPU fell on most workloads (`article` scrolling
15.8 → 3.8 %, `css-animation` 15.1 → 11.9 %), stayed level on `canvas` and rose on the Wasm
animations (below); the GPU's utilization on the Servo animations fell by a fifth to two fifths.

## No readback, checked at the GL level

The compared runs count readbacks as `sig` always does: Servo's `read_to_image`, which the
adapter counts (`page_readbacks`, 0 in every run). A separate set,
`louise/rewrite-audit/20261002-131305/` (`builds/rewrite-audit.toml`: the same binary with
`SIG_AUDIT_GL=1`, which counts every `glReadPixels` and `glGetTexImage` WebRender makes, and
Servo's timing lines), ran the eight Servo workloads for one warm-up and two measured rounds:
**0 GL reads and 0 `read_to_image` in all 24 runs**, with 16–1,355 Servo frames each (CSS
animation, WebGL, a 2D canvas, scrolling, eight tabs switched once a second, ten tabs opened and
closed, a resize storm across four scales). Its CPU is compared with nothing: the audit times
every GL call, and the timing lines raise the `log` level for all of Servo.

Its timing lines also give Servo's swap (`eglSwapBuffers` through surfman): 0.10–0.12 ms median on
the animations and scrolling, 1.2–1.6 ms on `tab-churn` and `resize-storm`, and 5.0–5.5 ms on
`many-tabs`, where most frames are a tab's first after being shown.

## Memory over many cycles

The session's `tab-churn` (10 cycles of opening a tab, loading the article and closing it) left
58 MiB behind against the baseline's 13 (P20). Run for 60 cycles instead (an exploratory
workload, 120 s; 3 rewrite runs, 2 baseline runs, alternating, each under the lock;
`louise/exploratory/20261002/tab-churn-long/`), the rewrite's growth was **68.3, 68.8 and 68.5 MiB** and
its RSS stopped rising after about 20 cycles (294–296 MiB from 40 s to the end); the baseline's
was 25.6 and 22.2 MiB, still rising slowly. So the rewrite keeps about 70 MiB more after closing
tabs, but a bounded amount, at a level (RSS 295 MiB) well below the baseline's (502 MiB).
`resize-storm` grew 15 MiB against the baseline's 111.

The session's 10 s windows also showed the rewrite gaining 3 MiB during `wasm-raf`'s steady window
(the baseline nothing) and 4.3 against 3.8 MiB during `css-animation`'s. Left alone for two
minutes (one exploratory run each, `louise/exploratory/20261002/animation-long/`), `wasm-raf`'s RSS in
the rewrite had 20 s means of 90.5, 92.8, 93.6, 91.6, 92.9 and 93.7 MiB: up and down, no trend. `css-animation`'s rose steadily, 217.7, 218.0, 218.3, 218.8, 219.2,
219.6 MiB, about 1.1 MiB a minute (the baseline's 475.5 to 476.3, about 0.5 MiB a minute). Two
minutes can't tell whether that levels off (H18 in `HYPOTHESES.md`).

## Surprises and regressions: hypotheses for the next wave

Exploratory: each was not predicted, so each is a hypothesis to test, not a result.

1. **Tab churn retains about 70 MiB** (above). `sig` links Servo, so Servo's jemalloc is the
   allocator for everything in the process, and every view brings its own GL context and
   WebRender instance. *Hypothesis:* the retained memory is allocator and cache retention, not
   live objects: with jemalloc's dirty pages decaying sooner (or purged after a view closes),
   `tab-churn`'s `after.pss_growth_mib` falls to the baseline's 13–26 MiB (H12).
2. **Wasm frames moved a page-sized upload into the compositor.** On `wasm-raf` the compositor's
   CPU went from 14.7 to 28.2 % and the GPU's from 12.8 to 23.0 % (`wasm-workers`: 12.8 → 27.9 %
   and 11.0 → 22.6 %), while `sig`'s own CPU barely moved (5.89 → 6.10 %; a frame takes 0.41 ms
   to paint). Each frame is a whole 1280 × 696 shared-memory buffer, which sway copies to a
   texture; GPUI drew the list into its own GPU buffer. That is §28's question 3, a page-sized
   copy per frame, now in the compositor. *Hypothesis:* damaging only the rectangles the draw list
   changed brings the compositor's CPU on `wasm-raf` to the baseline's 15 % or below (H14).
3. **The flood holds the owner thread.** `wasm-flood` paints 200,000 rectangles in 63 ms a frame
   on the owner thread (`present_p50_ms`), 14 frames a second, and the chrome waits:
   `input_to_pixels` 53 ms median and 84 ms p95, against 17 ms on the other guests (the baseline
   missed all 20 inputs). CPU 157 % against the baseline's 100 %, which one saturated thread
   pinned. *Hypothesis:* presenting a Wasm frame off the owner thread (decision 45's open item, a
   `Send` handle to the lent buffer) brings the flood's chrome latency to ≤ 20 ms (H15).
4. **Static guests still wake their process about 250 times a second** (P10). The owner loop
   sleeps (`ctr.wakeups_per_s` 0.1 on `wasm-gradient`, `wasm-timer` and `wasm-busy`); the rest
   are the engine's threads, about four wakeups per `on_frame` step at 60 steps a second, where the
   baseline's whole `sig-wasm` needed 60. `wasm-busy` rose from 252 to 300/s. *Hypothesis:* the
   epoch ticker (10 ms ticks while a guest runs) and the pool's hand-off account for the extra;
   arming a deadline per step instead brings static guests to ≤ 130/s (H13).
5. **Animations run 1–2 % slower**: 58.2 frames a second against 59.2 (steady), 57.8–57.9 against
   59.2–59.4 (active), on a 60 Hz output. The host paces Servo by frame callbacks; one or two
   intervals a second pass without a frame. *Hypothesis:* a callback that arrives before Servo's
   frame is ready costs a whole interval; timing callbacks against `FrameReady` will show it, and
   presenting when the frame becomes ready inside the interval brings the rate to ≥ 59 (H16).
6. **Showing a tab's view costs owner time.** The first show of each hidden view takes 13–17 ms
   of `present` (paint 7–11 ms, swap 5–10 ms; the audit set), later shows 1.6–2.9 ms of swap,
   because a hidden GL view loses its subsurface (decision 31) and the next swap sets the surface
   up again; `many-tabs`'
   `paint_p50_ms` went from 0.2 to 6.7 ms. It doesn't reach `input_to_pixels` (P15).
   *Hypothesis:* keeping a hidden view's surface brings the swap on show to ≤ 0.2 ms (H17).
7. **A wasted first paint at startup.** The first view paints a frame at the host's default size
   (1024 × 664) before the window's first configure, then again at 1280 × 696 (the audit set's
   logs). *Hypothesis:* opening views after the first configure saves one paint from startup.
8. **WebRender's paint is 0.05–0.11 ms longer** (`css-animation` 0.57 against 0.5 ms, `webgl`
   0.69 against 0.6; the baseline prints to 0.1 ms): painting into a window surface rather than
   an offscreen one. Small; not worth a change yet.
9. **The 2D canvas still wakes the process 3,675 times a second** (`css-animation`: 851). The
   baseline's `sig-servo` without its pipe writer had about 3,500, so this is Servo's canvas
   machinery, unchanged; it is now the largest source of wakeups left.
10. **A slow rise during a CSS animation**: about 1.1 MiB a minute over two minutes (above), twice
   the baseline's. *Hypothesis:* retention that levels off within ten minutes; a 30-minute run
   tells (H18).

## Measured by one build only

- **Only the baseline:** the readback and send time per frame (`read_p50_ms` 1.0–5.4 ms,
  `send_p50_ms` 1.3–2.5 ms), what engine processes write (`ipc_out_*`) and the browser/engine
  split, GPUI's window-draw time on workloads where the rewrite draws no chrome.
- **Only the rewrite:** the whole `present` per frame (`present_p50_ms`: 0.64–0.83 ms on Servo
  pages, 13.4 ms showing a tab, 63 ms on the flood), the Wasm engine's frames (`fps_*`, `paint_*`,
  `first_frame_engine_s` 0.04–0.05 s, the flood's 0.14 s; `sig-wasm` had no frame diagnostics),
  the owner loop's own counters (`ctr.*`: 230–340 loop wakeups a second on the Servo animations,
  0.1 idle), the engines' own readback counts and the run's peaks (`exit.*`: longest `present`
  1.2–71 ms per run, the longest a WebGL context's first frame, 67 ms, and the flood's, 71 ms;
  longest pump ≤ 5.3 ms; at most one SHM buffer per target at any sample), chrome `input_to_screen` on the chrome-input workloads, and in the
  audit set Servo's swap times and GL reads.
- **Both, not compared:** `startup.first_frame_after_navigate_ms` (different start points), and
  on the workloads that press keys the baseline's window frames and chrome `input_to_screen`.

## Checks on the measurement

- **The baseline reproduces.** The same baseline binaries measured in September (system sway,
  harness a694507) and in this session (pinned sway, this harness): of 519 comparisons, 36 are
  beyond noise by the per-row rule and none survives the false-discovery correction (smallest
  q = 0.33); the largest are a 50 ms grain in `mapped_s` and 1–2 % in PSS.
- **Anomalies:** all 7 baseline `wasm-flood` runs needed SIGTERM to quit (as in September); 2
  baseline `many-tabs` runs had a diagnostics line two `sig-servo` processes wrote into each
  other (it costs one renderer line, not a number). No rewrite run had an anomaly; every rewrite
  run ended with exactly the tabs its workload opened.
- **Screenshots** after settling and after the active window (raw logs) show each workload
  running in both builds.

## Build cost

The rewrite's `sig` from an empty target directory on louise-pc under `tools/heavy` (8 jobs,
11 G): **13 min 04 s** of cargo time, 935 crates compiled, Rust 1.98.1 (`bench/rewrite/build.sh`'s
log). The baseline (`louise/build-cost/baseline-20260927-094102.json`): 16 min 04 s over two
workspaces (6 m 32 s for `sig` and `sig-wasm`, 9 m 32 s for `sig-servo`), 1,393 crates, Rust 1.95.
One lockfile of 1,239 packages against two of 1,026 and 836. One 204.5 MiB binary against three
of 224.3 MiB together. The compilers differ, so this is indicative only.

## Threats to validity

- **Three changes at once.** The pipe, GPUI and the readback went together; the browser-level
  numbers can't apportion the gain. The shares above come from the baseline's own timings and
  split and from `bench/pipe-cost`, and assume Servo's own work is the same in both. Not every
  difference is IPC's either: the rewrite throttles hidden Wasm guests to a step a second
  (decision 23), so `wasm-busy`'s and `wasm-workers`' active-window CPU (23.3 → 11.4 %,
  401 → 223 %, while a new tab hides the guest half the time) measures that policy, not the
  transport; it opens every tab's view at startup; and its chrome is a different program.
- **Different compilers, C libraries and allocators.** The baseline was built with Rust 1.95.0
  and runs with that nixpkgs' glibc 2.42-61 and libstdc++ (its interpreter and `RUNPATH`); the
  rewrite with Rust 1.98.1 and glibc 2.42-84. The libraries both load through `LD_LIBRARY_PATH`
  (Wayland, xkbcommon, libGL, Mesa, Vulkan, fontconfig, FreeType, ALSA, …) are the same files.
  Servo's jemalloc is the allocator of `sig-servo` in the baseline and of the whole of `sig` in the
  rewrite (GPUI and `sig-wasm` used glibc's). Rebuilding the baseline with Rust 1.98.1 was not
  tried (the task measures the binaries already built); a compiler version usually moves CPU by a
  few percent, not by the factors measured here.
- **The same Servo, set up differently.** Both are Servo 0.5, but the rewrite's adapter is a new
  program: one Servo for all views, a GL context per view, frames paced by the host's frame
  callbacks, where `sig-servo` paced itself with Servo's default refresh driver; frame rates differ
  by 1–2 %.
- **Instrumentation.** Both builds write a stderr line per frame (the baseline two, `sig-servo`'s
  frame line and GPUI's draw line; the rewrite one), which the harness reads; the costs are of the
  same order. The compared rewrite runs had neither `RUST_LOG` nor the GL audit. The baseline's
  events are timed when the harness reads their line, the rewrite's carry their own CLOCK_MONOTONIC
  time, so the baseline's latencies include a pipe read (well under a millisecond on an idle
  machine). The baseline prints paint times to 0.1 ms.
- **Screen times** come from `grim` polls every 50 ms, timed at the start of the poll: `mapped_s`
  and `first_frame_s` have a 50 ms grain, and a fast browser can show up in the first poll. The
  rewrite's 0.05–0.20 s and the baseline's 0.35–0.75 s are several grains apart.
- **Submission is not display.** The output is headless: "on screen" means committed to the
  compositor (frame and chrome lines, screenshots of the compositor's buffer), not scanned out.
- **One machine, one driver, one compositor family**: louise-pc's RTX 4060 with NVIDIA 595.80 in
  headless sway 1.12 with GLES2. River, Mesa drivers, other GPUs and other compositors were not
  measured; M7's "another compositor family" is still open.
- **Machine state.** The governor and boost are not fixed, and other agents ran builds and tests
  between runs (never during one: each run held the lock and passed the quiet gate). The
  interleaved order spreads any drift over both builds; the IQRs show how much there was.
- **Workloads** are small local pages and hand-made guests on a 1280 × 800 output at scale 1, not
  real sites. The per-frame copies the rewrite removed grow with the page area (plan §3: 18.9 MiB
  a frame at 3440 × 1440), so the animation gains here are not an upper bound.
- **Memory.** PSS shares pages between the processes of a tree, so shared libraries count once in
  both. GPU memory is `nvidia-smi`'s per-process use at the window's end; GPU utilization covers
  the whole GPU (the idle X server and a llama.cpp RPC server another session runs share it).
- **The rewrite measured had no WPE WebKit.** 3c510fe predates WPE in `sig`. The default build
  that followed linked `libWPEWebKit` and its 72 libraries into every `sig` (29.8 MiB PSS and
  82 000 relocations at start in a session that opens no WPE page, by the wave's performance
  review), which these figures don't include. Since STATUS decision 84 the engine opens WebKit
  only when its first WPE page opens. Measured on the desktop (RTX 3070, headless sway, dev
  builds of the same tree with and without the engine, interleaved, 5 rounds,
  `article`/`home`/`many-tabs`): launch to page on screen and the article's and eight tabs'
  memory the same within noise, the home page +0.9 MiB PSS (GLib's libraries). So for sessions
  without WPE pages these figures describe today's default build to about a MiB; a page in WPE
  WebKit adds its web and network processes, which this report doesn't measure.

## Reproducing

```sh
bench/rewrite/build.sh 3c510fe
bench/remote --detach bench/run --build bench/builds/baseline.toml --label baseline \
  --build-dir '~/sighurt-baseline' --build bench/builds/rewrite.toml --label rewrite \
  --build-dir '~/sighurt-bench/builds/3c510fe0b051' --rounds 7 --warmup 1 --lock-runs 1 \
  --out '~/sighurt-bench/results'
bench/remote --detach bench/run --build bench/builds/rewrite-audit.toml --label rewrite-audit \
  --build-dir '~/sighurt-bench/builds/3c510fe0b051' --rounds 2 --warmup 1 --lock-runs 1 \
  --workloads article,long,css-animation,canvas,webgl,many-tabs,tab-churn,resize-storm \
  --out '~/sighurt-bench/results'
bench/remote tools/heavy bench/pipe-cost
bench/remote tools/heavy bench/pipe-cost --pipe-size 1048576
bench/compare bench/results/louise/baseline/20261002-131052 bench/results/louise/rewrite/20261002-131052
```

## Appendix: `bench/compare`, key metrics

A is the baseline, B the rewrite.

A: baseline (bench/results/louise/baseline/20261002-131052)
B: rewrite (bench/results/louise/rewrite/20261002-131052)

- engines: processes → in-process: the browser/engine split metrics (*_browser, *_engines, ipc_out_*) are left out
- A many-tabs: 2 run(s) with anomalies (included), 0 failed
- A wasm-flood: 7 run(s) with anomalies (included), 0 failed

| workload | metric | A median [IQR] | B median [IQR] | change | p | q | verdict |
|---|---|---|---|---|---|---|---|
| home | startup.mapped_s | 0.404 [0.0249] | 0.0533 [8.38e-05] | -86.8% | 0.000583 | 0.000718 | better |
| home | startup.first_frame_s | 0.404 [0.0249] | 0.0533 [8.38e-05] | -86.8% | 0.000583 | 0.000718 | better |
| home | startup.cpu_s | 0.39 [0.02] | 0.1 [0] | -74.4% | 0.000583 | 0.000718 | better |
| home | steady.cpu_pct | 0.799 [0.0999] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| home | steady.wakeups_per_s | 191 [0.161] | 0.0999 [5.92e-06] | -99.9% | 0.000583 | 0.000718 | better |
| home | steady.pss_mean_mib | 217 [2.67] | 71.6 [3.44] | -67.1% | 0.000583 | 0.000718 | better |
| home | steady.browser_fps | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| home | steady.gpu_util_pct | 1.82 [2.72] | 0.24 [2.30] | -86.8% | 0.118 | 0.134 | noise |
| home | steady.gpu_fb_tree_mib | 169 [0] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| home | steady.compositor_cpu_pct | 4.20 [0.15] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| home | active.cpu_pct | 2.10 [5.23e-05] | 0.2 [0.0999] | -90.5% | 0.000583 | 0.000718 | better |
| home | active.wakeups_per_s | 386 [0.348] | 6.20 [0.000595] | -98.4% | 0.000583 | 0.000718 | better |
| home | active.pss_peak_mib | 219 [2.65] | 71.7 [3.43] | -67.3% | 0.000583 | 0.000718 | better |
| home | active.gpu_util_pct | 9.14 [2.94] | 0.36 [2.44] | -96.1% | 0.000583 | 0.000718 | better |
| home | active.compositor_cpu_pct | 11.3 [0.0504] | 0.5 [4.8e-05] | -95.6% | 0.000583 | 0.000718 | better |
| home | shutdown.s | 0.1 [3.63e-05] | 0.0503 [3.16e-05] | -49.9% | 0.000583 | 0.000718 | better |
| article | startup.mapped_s | 0.354 [0.05] | 0.153 [2.44e-05] | -56.7% | 0.000583 | 0.000718 | better |
| article | startup.first_frame_s | 0.404 [0.05] | 0.153 [2.44e-05] | -62.0% | 0.000583 | 0.000718 | better |
| article | startup.first_frame_engine_s | 0.398 [0.0448] | 0.139 [0.00298] | -65.2% | 0.000583 | 0.000718 | better |
| article | startup.cpu_s | 0.6 [0.04] | 0.26 [0.01] | -56.7% | 0.000583 | 0.000718 | better |
| article | steady.cpu_pct | 0.699 [0.0499] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| article | steady.wakeups_per_s | 192 [0.105] | 0.0999 [2.36e-06] | -99.9% | 0.000583 | 0.000718 | better |
| article | steady.pss_mean_mib | 375 [2.19] | 184 [0.864] | -51.0% | 0.000583 | 0.000718 | better |
| article | steady.fps_produced | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| article | steady.fps_delivered | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| article | steady.browser_fps | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| article | steady.readback_mib_per_s | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| article | steady.ipc_pixel_mib_per_s | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| article | steady.gpu_util_pct | 1.68 [0.24] | 0 [0.08] | -100.0% | 0.0146 | 0.0174 | better |
| article | steady.gpu_fb_tree_mib | 245 [0] | 78.0 [0] | -68.2% | 0.000583 | 0.000718 | better |
| article | steady.compositor_cpu_pct | 4.19 [0.1] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| article | active.cpu_pct | 11.2 [0.299] | 2.90 [0.15] | -74.1% | 0.000583 | 0.000718 | better |
| article | active.wakeups_per_s | 1767 [3.51] | 222 [1.65] | -87.5% | 0.000583 | 0.000718 | better |
| article | active.pss_peak_mib | 392 [2.63] | 195 [0.951] | -50.3% | 0.000583 | 0.000718 | better |
| article | active.fps_produced | 9.99 [0.000307] | 9.99 [6.83e-05] | +0.1% | 0.000583 | 0.000718 | higher |
| article | active.fps_delivered | 9.99 [0.000307] | 9.99 [6.83e-05] | +0.1% | 0.000583 | 0.000718 | higher |
| article | active.browser_fps | 9.99 [0.000307] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| article | active.input_to_frame_p50_ms | 7.69 [0.0563] | 2.20 [0.041] | -71.4% | 0.000583 | 0.000718 | better |
| article | active.input_to_screen_p50_ms | 17.4 [0.288] | 2.20 [0.041] | -87.4% | 0.000583 | 0.000718 | better |
| article | active.input_to_screen_p95_ms | 25.0 [0.188] | 3.85 [0.0814] | -84.6% | 0.000583 | 0.000718 | better |
| article | active.readback_mib_per_s | 33.9 [0.00104] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| article | active.ipc_pixel_mib_per_s | 33.9 [0.00104] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| article | active.paint_p50_ms | 0.5 [0.05] | 0.532 [0.0105] | +6.4% | 0.000583 | 0.000718 | noise |
| article | active.gpu_util_pct | 17.5 [0.49] | 3.44 [0.95] | -80.4% | 0.000583 | 0.000718 | better |
| article | active.compositor_cpu_pct | 15.8 [0.0994] | 3.80 [0.15] | -75.9% | 0.000583 | 0.000718 | better |
| article | shutdown.s | 0.151 [8.45e-05] | 0.1 [0.0252] | -33.3% | 0.000583 | 0.000718 | better |
| long | startup.mapped_s | 0.404 [0.05] | 0.153 [0.000111] | -62.0% | 0.000583 | 0.000718 | better |
| long | startup.first_frame_s | 0.454 [0.0767] | 0.204 [0.000375] | -55.1% | 0.000583 | 0.000718 | better |
| long | startup.first_frame_engine_s | 0.436 [0.0487] | 0.139 [0.00227] | -68.1% | 0.000583 | 0.000718 | better |
| long | startup.cpu_s | 0.75 [0.04] | 0.37 [0.005] | -50.7% | 0.000583 | 0.000718 | better |
| long | steady.cpu_pct | 0.699 [2.28e-05] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| long | steady.wakeups_per_s | 191 [0.0938] | 0.0999 [3.53e-07] | -99.9% | 0.000583 | 0.000718 | better |
| long | steady.pss_mean_mib | 418 [2.46] | 225 [2.89] | -46.1% | 0.000583 | 0.000718 | better |
| long | steady.fps_produced | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| long | steady.fps_delivered | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| long | steady.browser_fps | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| long | steady.readback_mib_per_s | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| long | steady.ipc_pixel_mib_per_s | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| long | steady.gpu_util_pct | 1.74 [1.72] | 0.06 [1.29] | -96.6% | 0.0659 | 0.0755 | noise |
| long | steady.gpu_fb_tree_mib | 245 [0] | 78.0 [0] | -68.2% | 0.000583 | 0.000718 | better |
| long | steady.compositor_cpu_pct | 4.19 [0.0999] | -1.39e-16 [1.39e-16] | -100.0% | 0.000583 | 0.000718 | better |
| long | active.cpu_pct | 15.8 [1.14] | 10.3 [0.2] | -34.8% | 0.000583 | 0.000718 | better |
| long | active.wakeups_per_s | 1768 [1.77] | 228 [0.899] | -87.1% | 0.000583 | 0.000718 | better |
| long | active.pss_peak_mib | 434 [2.86] | 236 [2.99] | -45.5% | 0.000583 | 0.000718 | better |
| long | active.fps_produced | 9.98 [0.00231] | 9.99 [8.19e-05] | +0.1% | 0.000583 | 0.000718 | higher |
| long | active.fps_delivered | 9.98 [0.00231] | 9.99 [8.19e-05] | +0.1% | 0.000583 | 0.000718 | higher |
| long | active.browser_fps | 9.98 [0.00231] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| long | active.input_to_frame_p50_ms | 12.8 [0.13] | 7.55 [0.0986] | -41.2% | 0.000583 | 0.000718 | better |
| long | active.input_to_screen_p50_ms | 21.9 [0.525] | 7.55 [0.0986] | -65.6% | 0.000583 | 0.000718 | better |
| long | active.input_to_screen_p95_ms | 29.9 [0.213] | 9.28 [0.213] | -69.0% | 0.000583 | 0.000718 | better |
| long | active.readback_mib_per_s | 33.9 [0.00787] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| long | active.ipc_pixel_mib_per_s | 33.9 [0.00787] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| long | active.paint_p50_ms | 0.4 [0.05] | 0.514 [0.028] | +28.5% | 0.00408 | 0.00496 | worse |
| long | active.gpu_util_pct | 17.1 [1.60] | 3.60 [0.87] | -79.0% | 0.000583 | 0.000718 | better |
| long | active.compositor_cpu_pct | 15.2 [0.795] | 4.20 [0.000126] | -72.4% | 0.000583 | 0.000718 | better |
| long | shutdown.s | 0.151 [3.39e-05] | 0.1 [4.43e-05] | -33.3% | 0.000583 | 0.000718 | better |
| css-animation | startup.mapped_s | 0.354 [0.025] | 0.153 [4.23e-05] | -56.6% | 0.000583 | 0.000718 | better |
| css-animation | startup.first_frame_s | 0.404 [0.025] | 0.153 [4.23e-05] | -62.0% | 0.000583 | 0.000718 | better |
| css-animation | startup.first_frame_engine_s | 0.394 [0.0223] | 0.139 [0.00409] | -64.8% | 0.000583 | 0.000718 | better |
| css-animation | startup.cpu_s | 1.59 [0.03] | 0.65 [0.005] | -59.1% | 0.000583 | 0.000718 | better |
| css-animation | steady.cpu_pct | 52.3 [3.45] | 13.7 [0.0503] | -73.8% | 0.000583 | 0.000718 | better |
| css-animation | steady.wakeups_per_s | 7728 [4.10] | 851 [4.76] | -89.0% | 0.000583 | 0.000718 | better |
| css-animation | steady.pss_mean_mib | 371 [1.35] | 176 [0.722] | -52.6% | 0.000583 | 0.000718 | better |
| css-animation | steady.fps_produced | 59.2 [0.0517] | 58.2 [0.0998] | -1.8% | 0.000583 | 0.000718 | lower |
| css-animation | steady.fps_delivered | 59.2 [0.0517] | 58.2 [0.0998] | -1.8% | 0.000583 | 0.000718 | lower |
| css-animation | steady.browser_fps | 59.1 [0.0948] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| css-animation | steady.readback_mib_per_s | 201 [0.176] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| css-animation | steady.ipc_pixel_mib_per_s | 201 [0.176] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| css-animation | steady.gpu_util_pct | 25.6 [3.52] | 15.2 [0.78] | -40.6% | 0.000583 | 0.000718 | better |
| css-animation | steady.gpu_fb_tree_mib | 259 [0] | 92.0 [0] | -64.5% | 0.000583 | 0.000718 | better |
| css-animation | steady.compositor_cpu_pct | 15.1 [1.25] | 11.9 [0.35] | -21.2% | 0.000583 | 0.000718 | better |
| css-animation | active.cpu_pct | 60.0 [3.51] | 14.0 [0.0998] | -76.7% | 0.000583 | 0.000718 | better |
| css-animation | active.wakeups_per_s | 7897 [25.4] | 940 [2.34] | -88.1% | 0.000583 | 0.000718 | better |
| css-animation | active.pss_peak_mib | 372 [0.651] | 178 [0.754] | -52.3% | 0.000583 | 0.000718 | better |
| css-animation | active.fps_produced | 59.4 [0.2] | 57.8 [0.0519] | -2.6% | 0.000583 | 0.000718 | lower |
| css-animation | active.fps_delivered | 59.4 [0.2] | 57.8 [0.0519] | -2.6% | 0.000583 | 0.000718 | lower |
| css-animation | active.browser_fps | 59.2 [0.205] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| css-animation | active.readback_mib_per_s | 202 [0.681] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| css-animation | active.ipc_pixel_mib_per_s | 202 [0.681] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| css-animation | active.paint_p50_ms | 0.5 [0] | 0.571 [0.0055] | +14.2% | 0.0186 | 0.0221 | worse |
| css-animation | active.gpu_util_pct | 34.8 [1.11] | 19.2 [1.19] | -44.7% | 0.0233 | 0.0275 | better |
| css-animation | active.compositor_cpu_pct | 15.3 [0.695] | 14.2 [0.101] | -7.2% | 0.0262 | 0.0304 | better |
| css-animation | shutdown.s | 0.151 [0.0251] | 0.0504 [0.0501] | -66.6% | 0.000583 | 0.000718 | better |
| canvas | startup.mapped_s | 0.354 [0.000239] | 0.153 [0.000123] | -56.6% | 0.000583 | 0.000718 | better |
| canvas | startup.first_frame_s | 0.404 [0.000234] | 0.153 [0.000123] | -62.0% | 0.000583 | 0.000718 | better |
| canvas | startup.first_frame_engine_s | 0.388 [0.00404] | 0.139 [0.00259] | -64.3% | 0.000583 | 0.000718 | better |
| canvas | startup.cpu_s | 2.81 [0.01] | 1.73 [0.01] | -38.4% | 0.000583 | 0.000718 | better |
| canvas | steady.cpu_pct | 83.2 [0.646] | 47.2 [0.101] | -43.3% | 0.000583 | 0.000718 | better |
| canvas | steady.wakeups_per_s | 10436 [37.3] | 3675 [49.3] | -64.8% | 0.000583 | 0.000718 | better |
| canvas | steady.pss_mean_mib | 391 [1.14] | 197 [0.733] | -49.7% | 0.000583 | 0.000718 | better |
| canvas | steady.fps_produced | 59.1 [0.108] | 58.3 [0.102] | -1.3% | 0.000583 | 0.000718 | lower |
| canvas | steady.fps_delivered | 59.1 [0.108] | 58.3 [0.102] | -1.3% | 0.000583 | 0.000718 | lower |
| canvas | steady.browser_fps | 59.1 [0.0593] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| canvas | steady.readback_mib_per_s | 201 [0.366] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| canvas | steady.ipc_pixel_mib_per_s | 201 [0.366] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| canvas | steady.gpu_util_pct | 23.9 [3.23] | 19.4 [0.99] | -18.7% | 0.000583 | 0.000718 | better |
| canvas | steady.gpu_fb_tree_mib | 231 [0] | 64.0 [0] | -72.3% | 0.000583 | 0.000718 | better |
| canvas | steady.compositor_cpu_pct | 11.4 [0.25] | 11.4 [0.401] | +0.1% | 0.62 | 0.683 | noise |
| canvas | active.cpu_pct | 87.6 [0.647] | 47.3 [0.353] | -46.0% | 0.000583 | 0.000718 | better |
| canvas | active.wakeups_per_s | 10552 [21.1] | 3734 [47.6] | -64.6% | 0.000583 | 0.000718 | better |
| canvas | active.pss_peak_mib | 392 [0.854] | 199 [1.30] | -49.3% | 0.000583 | 0.000718 | better |
| canvas | active.fps_produced | 59.2 [0.0649] | 57.8 [0.0537] | -2.3% | 0.000583 | 0.000718 | lower |
| canvas | active.fps_delivered | 59.2 [0.0649] | 57.8 [0.0537] | -2.3% | 0.000583 | 0.000718 | lower |
| canvas | active.browser_fps | 59.3 [0.0847] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| canvas | active.readback_mib_per_s | 201 [0.22] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| canvas | active.ipc_pixel_mib_per_s | 201 [0.22] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| canvas | active.paint_p50_ms | 0.6 [0] | 0.639 [0.01] | +6.5% | 0.0186 | 0.0221 | worse |
| canvas | active.gpu_util_pct | 30.2 [3.31] | 24.2 [0.29] | -19.8% | 0.000583 | 0.000718 | better |
| canvas | active.compositor_cpu_pct | 12.5 [0.158] | 13.5 [0.151] | +8.1% | 0.000583 | 0.000718 | worse |
| canvas | shutdown.s | 0.101 [4.04e-05] | 0.1 [0.0252] | -0.1% | 0.128 | 0.144 | noise |
| webgl | startup.mapped_s | 0.404 [0.0251] | 0.153 [2.91e-05] | -62.0% | 0.000583 | 0.000718 | better |
| webgl | startup.first_frame_s | 0.454 [0.0251] | 0.203 [3.14e-05] | -55.2% | 0.000583 | 0.000718 | better |
| webgl | startup.first_frame_engine_s | 0.436 [0.0298] | 0.202 [0.036] | -53.7% | 0.000583 | 0.000718 | better |
| webgl | startup.cpu_s | 1.53 [0.035] | 0.61 [0.005] | -60.1% | 0.000583 | 0.000718 | better |
| webgl | steady.cpu_pct | 42.2 [2.99] | 10.1 [0.101] | -76.1% | 0.000583 | 0.000718 | better |
| webgl | steady.wakeups_per_s | 7791 [13.0] | 887 [8.79] | -88.6% | 0.000583 | 0.000718 | better |
| webgl | steady.pss_mean_mib | 378 [2.90] | 184 [0.735] | -51.2% | 0.000583 | 0.000718 | better |
| webgl | steady.fps_produced | 59.2 [0.104] | 58.2 [0.0513] | -1.6% | 0.000583 | 0.000718 | lower |
| webgl | steady.fps_delivered | 59.2 [0.104] | 58.2 [0.0513] | -1.6% | 0.000583 | 0.000718 | lower |
| webgl | steady.browser_fps | 59.2 [0.153] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| webgl | steady.readback_mib_per_s | 201 [0.354] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| webgl | steady.ipc_pixel_mib_per_s | 201 [0.354] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| webgl | steady.gpu_util_pct | 25.1 [1.39] | 16.0 [1.44] | -36.1% | 0.0262 | 0.0304 | better |
| webgl | steady.gpu_fb_tree_mib | 252 [0] | 86.0 [0] | -65.9% | 0.000583 | 0.000718 | better |
| webgl | steady.compositor_cpu_pct | 14.5 [0.302] | 11.9 [0.449] | -17.9% | 0.0262 | 0.0304 | better |
| webgl | active.cpu_pct | 52.7 [6.00] | 10.5 [0.15] | -80.1% | 0.000583 | 0.000718 | better |
| webgl | active.wakeups_per_s | 7938 [28.1] | 965 [9.03] | -87.8% | 0.000583 | 0.000718 | better |
| webgl | active.pss_peak_mib | 379 [2.67] | 186 [0.674] | -51.0% | 0.000583 | 0.000718 | better |
| webgl | active.fps_produced | 59.3 [0.192] | 57.9 [0.0528] | -2.3% | 0.0262 | 0.0304 | lower |
| webgl | active.fps_delivered | 59.3 [0.192] | 57.9 [0.0528] | -2.3% | 0.0262 | 0.0304 | lower |
| webgl | active.browser_fps | 59.3 [0.142] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| webgl | active.readback_mib_per_s | 202 [0.654] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| webgl | active.ipc_pixel_mib_per_s | 202 [0.654] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| webgl | active.paint_p50_ms | 0.6 [0] | 0.693 [0.0045] | +15.5% | 0.0111 | 0.0133 | worse |
| webgl | active.gpu_util_pct | 32.0 [2.97] | 19.8 [0.56] | -38.3% | 0.0175 | 0.0208 | better |
| webgl | active.compositor_cpu_pct | 15.7 [1.96] | 14.2 [0.25] | -9.5% | 0.128 | 0.144 | noise |
| webgl | shutdown.s | 0.151 [0.0501] | 0.1 [4e-05] | -33.4% | 0.000583 | 0.000718 | better |
| wasm-hello | startup.mapped_s | 0.354 [0.0252] | 0.103 [6.13e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-hello | startup.first_frame_s | 0.354 [0.0252] | 0.103 [6.13e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-hello | startup.cpu_s | 0.39 [0.025] | 0.14 [0] | -64.1% | 0.000583 | 0.000718 | better |
| wasm-hello | steady.cpu_pct | 1.40 [8.25e-05] | 0.899 [2.67e-05] | -35.7% | 0.000583 | 0.000718 | better |
| wasm-hello | steady.wakeups_per_s | 374 [17.9] | 261 [5.15] | -30.2% | 0.000583 | 0.000718 | better |
| wasm-hello | steady.pss_mean_mib | 243 [1.40] | 79.1 [0.244] | -67.5% | 0.000583 | 0.000718 | better |
| wasm-hello | steady.browser_fps | 2.00 [8.15e-05] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| wasm-hello | steady.gpu_util_pct | 2.64 [1.85] | 0.08 [1.58] | -97.0% | 0.0373 | 0.043 | better |
| wasm-hello | steady.gpu_fb_tree_mib | 169 [0] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-hello | steady.compositor_cpu_pct | 4.70 [0.2] | 0.899 [0.0999] | -80.8% | 0.000583 | 0.000718 | better |
| wasm-hello | active.cpu_pct | 4.30 [0.2] | 3.70 [0.15] | -13.9% | 0.000583 | 0.000718 | better |
| wasm-hello | active.wakeups_per_s | 610 [5.06] | 369 [7.19] | -39.4% | 0.000583 | 0.000718 | better |
| wasm-hello | active.pss_peak_mib | 245 [1.40] | 79.4 [0.25] | -67.6% | 0.000583 | 0.000718 | better |
| wasm-hello | active.browser_fps | 21.9 [0.35] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| wasm-hello | active.gpu_util_pct | 16.0 [1.84] | 8.00 [0.431] | -50.0% | 0.000583 | 0.000718 | better |
| wasm-hello | active.compositor_cpu_pct | 16.1 [0.0996] | 11.3 [0.949] | -29.8% | 0.000583 | 0.000718 | better |
| wasm-hello | shutdown.s | 0.101 [8.78e-06] | 0.0503 [6.05e-06] | -50.0% | 0.000583 | 0.000718 | better |
| wasm-gradient | startup.mapped_s | 0.354 [0.0251] | 0.103 [7.38e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-gradient | startup.first_frame_s | 0.354 [0.0251] | 0.103 [7.38e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-gradient | startup.cpu_s | 0.41 [0.025] | 0.16 [0.005] | -61.0% | 0.000583 | 0.000718 | better |
| wasm-gradient | steady.cpu_pct | 0.999 [0.05] | 1.10 [0.0999] | +10.0% | 0.000583 | 0.000718 | worse |
| wasm-gradient | steady.wakeups_per_s | 252 [0.111] | 246 [7.09] | -2.2% | 0.165 | 0.185 | noise |
| wasm-gradient | steady.pss_mean_mib | 244 [1.59] | 79.4 [0.511] | -67.5% | 0.000583 | 0.000718 | better |
| wasm-gradient | steady.browser_fps | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| wasm-gradient | steady.gpu_util_pct | 1.68 [3.03] | 0.0816 [2.45] | -95.1% | 0.119 | 0.135 | noise |
| wasm-gradient | steady.gpu_fb_tree_mib | 169 [0] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-gradient | steady.compositor_cpu_pct | 4.20 [0.0997] | 0 [1.39e-16] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-gradient | active.cpu_pct | 5.99 [0.2] | 1.20 [0.05] | -80.0% | 0.000583 | 0.000718 | better |
| wasm-gradient | active.wakeups_per_s | 562 [1.82] | 265 [7.65] | -52.9% | 0.000583 | 0.000718 | better |
| wasm-gradient | active.pss_peak_mib | 246 [2.53] | 79.3 [0.485] | -67.8% | 0.000583 | 0.000718 | better |
| wasm-gradient | active.browser_fps | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| wasm-gradient | active.gpu_util_pct | 26.4 [2.02] | 0.96 [2.07] | -96.4% | 0.000583 | 0.000718 | better |
| wasm-gradient | active.compositor_cpu_pct | 13.0 [0.0995] | 1.60 [0.3] | -87.7% | 0.000583 | 0.000718 | better |
| wasm-gradient | shutdown.s | 0.101 [1.71e-05] | 0.0503 [3.24e-05] | -50.0% | 0.000583 | 0.000718 | better |
| wasm-raf | startup.mapped_s | 0.354 [0.05] | 0.103 [8.2e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-raf | startup.first_frame_s | 0.354 [0.05] | 0.103 [8.2e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-raf | startup.cpu_s | 0.52 [0.03] | 0.3 [5.55e-17] | -42.3% | 0.000583 | 0.000718 | better |
| wasm-raf | steady.cpu_pct | 5.89 [0.0502] | 6.10 [0.0498] | +3.4% | 0.00117 | 0.00143 | worse |
| wasm-raf | steady.wakeups_per_s | 612 [3.04] | 549 [20.2] | -10.3% | 0.000583 | 0.000718 | better |
| wasm-raf | steady.pss_mean_mib | 245 [2.37] | 81.7 [0.282] | -66.6% | 0.000583 | 0.000718 | better |
| wasm-raf | steady.browser_fps | 58.5 [0.151] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| wasm-raf | steady.gpu_util_pct | 12.8 [0.91] | 23.0 [1.27] | +80.3% | 0.000583 | 0.000718 | worse |
| wasm-raf | steady.gpu_fb_tree_mib | 169 [0] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-raf | steady.compositor_cpu_pct | 14.7 [0.899] | 28.2 [1.30] | +91.9% | 0.000583 | 0.000718 | worse |
| wasm-raf | active.cpu_pct | 5.89 [0.2] | 6.00 [0.15] | +1.7% | 0.456 | 0.504 | noise |
| wasm-raf | active.wakeups_per_s | 694 [1.10] | 553 [8.29] | -20.4% | 0.000583 | 0.000718 | better |
| wasm-raf | active.pss_peak_mib | 245 [2.37] | 86.2 [0.273] | -64.8% | 0.000583 | 0.000718 | better |
| wasm-raf | active.browser_fps | 58.3 [0.0021] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| wasm-raf | active.gpu_util_pct | 15.7 [0.29] | 23.9 [0.88] | +52.4% | 0.000583 | 0.000718 | worse |
| wasm-raf | active.compositor_cpu_pct | 16.9 [0.0502] | 29.5 [0.149] | +74.6% | 0.000583 | 0.000718 | worse |
| wasm-raf | shutdown.s | 0.101 [3.94e-05] | 0.0503 [1.66e-05] | -50.0% | 0.000583 | 0.000718 | better |
| wasm-timer | startup.mapped_s | 0.354 [0.025] | 0.103 [9.16e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-timer | startup.first_frame_s | 0.354 [0.025] | 0.103 [9.16e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-timer | startup.cpu_s | 0.38 [0.025] | 0.13 [0] | -65.8% | 0.000583 | 0.000718 | better |
| wasm-timer | steady.cpu_pct | 0.999 [0.0998] | 0.6 [0.0999] | -40.0% | 0.000583 | 0.000718 | better |
| wasm-timer | steady.wakeups_per_s | 251 [0.843] | 249 [6.00] | -1.0% | 0.0111 | 0.0133 | noise |
| wasm-timer | steady.pss_mean_mib | 242 [0.376] | 78.7 [0.163] | -67.5% | 0.000583 | 0.000718 | better |
| wasm-timer | steady.browser_fps | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| wasm-timer | steady.gpu_util_pct | 1.72 [0.47] | 0 [1.16] | -100.0% | 0.2 | 0.224 | noise |
| wasm-timer | steady.gpu_fb_tree_mib | 169 [0] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-timer | steady.compositor_cpu_pct | 4.20 [1.25] | 0 [6.93e-17] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-timer | active.cpu_pct | 2.40 [0.05] | 2.30 [0.05] | -4.1% | 0.128 | 0.144 | noise |
| wasm-timer | active.wakeups_per_s | 449 [1.06] | 309 [6.94] | -31.3% | 0.000583 | 0.000718 | better |
| wasm-timer | active.pss_peak_mib | 244 [1.69] | 78.9 [0.207] | -67.7% | 0.000583 | 0.000718 | better |
| wasm-timer | active.browser_fps | 11.2 [0.0507] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| wasm-timer | active.gpu_util_pct | 7.88 [1.15] | 4.56 [2.48] | -42.1% | 0.0903 | 0.103 | noise |
| wasm-timer | active.compositor_cpu_pct | 8.89 [0.2] | 5.40 [0.0502] | -39.3% | 0.0262 | 0.0304 | better |
| wasm-timer | shutdown.s | 0.101 [5.18e-05] | 0.0503 [1.21e-05] | -50.0% | 0.000583 | 0.000718 | better |
| wasm-busy | startup.mapped_s | 0.354 [0.05] | 0.103 [8.55e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-busy | startup.first_frame_s | 0.354 [0.05] | 0.103 [8.55e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-busy | startup.cpu_s | 1.03 [0.04] | 0.78 [0.01] | -24.3% | 0.000583 | 0.000718 | better |
| wasm-busy | steady.cpu_pct | 21.7 [0.2] | 21.4 [0.101] | -1.4% | 0.00699 | 0.00847 | better |
| wasm-busy | steady.wakeups_per_s | 252 [0.174] | 300 [0.00681] | +19.1% | 0.000583 | 0.000718 | worse |
| wasm-busy | steady.pss_mean_mib | 242 [2.65] | 77.7 [0.417] | -67.8% | 0.000583 | 0.000718 | better |
| wasm-busy | steady.browser_fps | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| wasm-busy | steady.gpu_util_pct | 1.78 [0.4] | 0 [0.04] | -100.0% | 0.00175 | 0.00213 | better |
| wasm-busy | steady.gpu_fb_tree_mib | 169 [0] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-busy | steady.compositor_cpu_pct | 4.30 [0.15] | 0 [6.93e-17] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-busy | active.cpu_pct | 23.2 [0.1] | 11.4 [0.0499] | -50.8% | 0.000583 | 0.000718 | better |
| wasm-busy | active.wakeups_per_s | 501 [0.397] | 168 [0.394] | -66.5% | 0.000583 | 0.000718 | better |
| wasm-busy | active.pss_peak_mib | 244 [2.64] | 80.0 [0.435] | -67.2% | 0.000583 | 0.000718 | better |
| wasm-busy | active.input_to_pixels_p50_ms | 33.1 [0.0678] | 16.7 [0.182] | -49.4% | 0.000583 | 0.000718 | better |
| wasm-busy | active.input_to_pixels_p95_ms | 33.6 [0.256] | 17.0 [0.245] | -49.3% | 0.000583 | 0.000718 | better |
| wasm-busy | active.input_to_pixels_missed | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| wasm-busy | active.gpu_util_pct | 11.6 [4.56] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-busy | active.compositor_cpu_pct | 16.6 [0.0999] | 3.20 [0.05] | -80.7% | 0.000583 | 0.000718 | better |
| wasm-busy | shutdown.s | 0.101 [5.51e-05] | 0.0503 [1.45e-05] | -50.0% | 0.000583 | 0.000718 | better |
| many-tabs | startup.mapped_s | 0.404 [0.0254] | 0.153 [0.0501] | -62.0% | 0.000583 | 0.000718 | better |
| many-tabs | startup.first_frame_s | 0.754 [0.0502] | 0.203 [0.0503] | -73.0% | 0.000583 | 0.000718 | better |
| many-tabs | startup.first_frame_engine_s | 0.703 [0.0396] | 0.152 [0.00257] | -78.4% | 0.000583 | 0.000718 | better |
| many-tabs | startup.cpu_s | 1.90 [0.04] | 0.77 [0.01] | -59.5% | 0.000583 | 0.000718 | better |
| many-tabs | steady.cpu_pct | 0.698 [0.0498] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| many-tabs | steady.wakeups_per_s | 191 [0.152] | 0.0998 [4.61e-06] | -99.9% | 0.000583 | 0.000718 | better |
| many-tabs | steady.pss_mean_mib | 604 [5.37] | 345 [1.82] | -42.9% | 0.000583 | 0.000718 | better |
| many-tabs | steady.fps_produced | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| many-tabs | steady.fps_delivered | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| many-tabs | steady.browser_fps | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| many-tabs | steady.readback_mib_per_s | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| many-tabs | steady.ipc_pixel_mib_per_s | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| many-tabs | steady.gpu_util_pct | 2.04 [0.21] | 0.08 [0.12] | -96.1% | 0.0134 | 0.0161 | better |
| many-tabs | steady.gpu_fb_tree_mib | 291 [26.0] | 100 [0] | -65.6% | 0.000583 | 0.000718 | better |
| many-tabs | steady.compositor_cpu_pct | 4.58 [0.1] | -1.39e-16 [9.35e-21] | -100.0% | 0.000583 | 0.000718 | better |
| many-tabs | active.cpu_pct | 9.86 [0.197] | 1.10 [4.56e-05] | -88.9% | 0.000583 | 0.000718 | better |
| many-tabs | active.wakeups_per_s | 889 [4.16] | 18.2 [0.25] | -98.0% | 0.000583 | 0.000718 | better |
| many-tabs | active.pss_peak_mib | 978 [4.20] | 409 [2.54] | -58.2% | 0.000583 | 0.000718 | better |
| many-tabs | active.fps_produced | 2.79 [0.000113] | 0.998 [2.69e-05] | -64.2% | 0.000583 | 0.000718 | lower |
| many-tabs | active.fps_delivered | 1.39 [5.63e-05] | 0.998 [2.69e-05] | -28.4% | 0.000583 | 0.000718 | lower |
| many-tabs | active.input_to_pixels_p50_ms | 33.8 [0.117] | 33.3 [13.7] | -1.4% | 0.00117 | 0.00143 | noise |
| many-tabs | active.input_to_pixels_p95_ms | 34.7 [1.42] | 34.4 [0.445] | -0.9% | 0.165 | 0.185 | noise |
| many-tabs | active.input_to_pixels_missed | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| many-tabs | active.readback_mib_per_s | 9.48 [0.000383] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| many-tabs | active.ipc_pixel_mib_per_s | 4.74 [0.000191] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| many-tabs | active.paint_p50_ms | 0.2 [0] | 6.73 [0.171] | +3266.5% | 0.000583 | 0.000718 | worse |
| many-tabs | active.gpu_util_pct | 11.6 [2.39] | 0 [3.30] | -100.0% | 0.000583 | 0.000718 | better |
| many-tabs | active.compositor_cpu_pct | 15.5 [0.053] | 1.70 [0.0998] | -89.1% | 0.000583 | 0.000718 | better |
| many-tabs | shutdown.s | 0.352 [0.000113] | 0.151 [0.000643] | -57.1% | 0.000583 | 0.000718 | better |
| tab-churn | startup.mapped_s | 0.354 [0.05] | 0.153 [7.37e-05] | -56.7% | 0.000583 | 0.000718 | better |
| tab-churn | startup.first_frame_s | 0.454 [0.0509] | 0.153 [7.37e-05] | -66.2% | 0.000583 | 0.000718 | better |
| tab-churn | startup.first_frame_engine_s | 0.397 [0.0459] | 0.139 [0.0024] | -65.0% | 0.000583 | 0.000718 | better |
| tab-churn | startup.cpu_s | 0.6 [0.03] | 0.25 [0.005] | -58.3% | 0.000583 | 0.000718 | better |
| tab-churn | steady.cpu_pct | 0.699 [0.0998] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| tab-churn | steady.wakeups_per_s | 192 [0.0947] | 0.0999 [1.83e-06] | -99.9% | 0.000583 | 0.000718 | better |
| tab-churn | steady.pss_mean_mib | 374 [0.611] | 184 [0.789] | -50.9% | 0.000583 | 0.000718 | better |
| tab-churn | steady.fps_produced | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| tab-churn | steady.fps_delivered | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| tab-churn | steady.browser_fps | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| tab-churn | steady.readback_mib_per_s | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| tab-churn | steady.ipc_pixel_mib_per_s | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| tab-churn | steady.gpu_util_pct | 1.64 [0.12] | 0 [1.09] | -100.0% | 0.228 | 0.253 | noise |
| tab-churn | steady.gpu_fb_tree_mib | 225 [0] | 46.0 [0] | -79.6% | 0.000583 | 0.000718 | better |
| tab-churn | steady.compositor_cpu_pct | 4.19 [0.0999] | 0 [6.93e-17] | -100.0% | 0.000583 | 0.000718 | better |
| tab-churn | active.cpu_pct | 13.9 [0.35] | 4.50 [0.025] | -67.7% | 0.000583 | 0.000718 | better |
| tab-churn | active.wakeups_per_s | 797 [7.27] | 215 [2.65] | -73.0% | 0.000583 | 0.000718 | better |
| tab-churn | active.pss_peak_mib | 484 [0.42] | 246 [3.11] | -49.1% | 0.000583 | 0.000718 | better |
| tab-churn | active.fps_produced | 2.00 [3.22e-05] | 2.50 [1.09e-05] | +25.0% | 0.000583 | 0.000718 | higher |
| tab-churn | active.fps_delivered | 0.999 [1.61e-05] | 2.50 [1.09e-05] | +150.1% | 0.000583 | 0.000718 | higher |
| tab-churn | active.readback_mib_per_s | 6.79 [0.000109] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| tab-churn | active.ipc_pixel_mib_per_s | 3.40 [5.47e-05] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| tab-churn | active.paint_p50_ms | 0.2 [0] | 0.148 [0.0005] | -26.0% | 0.000583 | 0.000718 | better |
| tab-churn | active.gpu_util_pct | 2.38 [0.398] | 3.18 [3.79] | +33.6% | 0.902 | 0.991 | noise |
| tab-churn | active.compositor_cpu_pct | 3.85 [0.425] | 1.40 [0.05] | -63.6% | 0.000583 | 0.000718 | better |
| tab-churn | after.rss_growth_mib | 12.9 [2.36] | 60.1 [2.69] | +364.1% | 0.000583 | 0.000718 | worse |
| tab-churn | after.pss_growth_mib | 12.9 [3.75] | 58.2 [2.69] | +349.7% | 0.000583 | 0.000718 | worse |
| tab-churn | shutdown.s | 0.151 [2.88e-05] | 0.1 [1.64e-05] | -33.3% | 0.000583 | 0.000718 | better |
| resize-storm | startup.mapped_s | 0.354 [0.05] | 0.153 [3.83e-05] | -56.6% | 0.000583 | 0.000718 | better |
| resize-storm | startup.first_frame_s | 0.454 [0.025] | 0.153 [3.83e-05] | -66.2% | 0.000583 | 0.000718 | better |
| resize-storm | startup.first_frame_engine_s | 0.405 [0.0418] | 0.138 [0.00515] | -65.9% | 0.000583 | 0.000718 | better |
| resize-storm | startup.cpu_s | 0.6 [0.04] | 0.25 [0.01] | -58.3% | 0.000583 | 0.000718 | better |
| resize-storm | steady.cpu_pct | 0.699 [0.0499] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| resize-storm | steady.wakeups_per_s | 191 [0.0875] | 0.0999 [1.46e-06] | -99.9% | 0.000583 | 0.000718 | better |
| resize-storm | steady.pss_mean_mib | 376 [2.47] | 184 [0.916] | -51.2% | 0.000583 | 0.000718 | better |
| resize-storm | steady.fps_produced | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| resize-storm | steady.fps_delivered | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| resize-storm | steady.browser_fps | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| resize-storm | steady.readback_mib_per_s | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| resize-storm | steady.ipc_pixel_mib_per_s | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| resize-storm | steady.gpu_util_pct | 1.70 [0.04] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| resize-storm | steady.gpu_fb_tree_mib | 153 [0] | 58.0 [0] | -62.1% | 0.000583 | 0.000718 | better |
| resize-storm | steady.compositor_cpu_pct | 4.19 [0.15] | 0 [1.39e-16] | -100.0% | 0.000583 | 0.000718 | better |
| resize-storm | active.cpu_pct | 23.7 [0.25] | 8.09 [0.201] | -65.9% | 0.000583 | 0.000718 | better |
| resize-storm | active.wakeups_per_s | 2063 [13.8] | 239 [8.83] | -88.4% | 0.000583 | 0.000718 | better |
| resize-storm | active.pss_peak_mib | 489 [4.25] | 200 [1.40] | -59.1% | 0.000583 | 0.000718 | better |
| resize-storm | active.fps_produced | 15.8 [0.25] | 11.9 [0.000421] | -24.6% | 0.000583 | 0.000718 | lower |
| resize-storm | active.fps_delivered | 14.0 [0.15] | 11.9 [0.000421] | -15.0% | 0.000583 | 0.000718 | lower |
| resize-storm | active.browser_fps | 15.0 [0.102] | 7.99 [0.000182] | -46.6% | 0.000583 | 0.000718 | lower |
| resize-storm | active.browser_draw_p50_ms | 0.419 [0.00366] | 0.184 [0.08] | -56.0% | 0.000583 | 0.000718 | better |
| resize-storm | active.readback_mib_per_s | 48.6 [0.436] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| resize-storm | active.ipc_pixel_mib_per_s | 44.0 [0.236] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| resize-storm | active.paint_p50_ms | 0.2 [0] | 0.481 [0.013] | +140.5% | 0.000583 | 0.000718 | worse |
| resize-storm | active.gpu_util_pct | 11.3 [12.5] | 6.76 [6.33] | -40.2% | 0.0379 | 0.0436 | noise |
| resize-storm | active.compositor_cpu_pct | 21.8 [0.4] | 6.29 [0.198] | -71.2% | 0.000583 | 0.000718 | better |
| resize-storm | after.rss_growth_mib | 111 [2.80] | 15.0 [0.564] | -86.5% | 0.000583 | 0.000718 | better |
| resize-storm | after.pss_growth_mib | 111 [2.69] | 15.0 [0.564] | -86.5% | 0.000583 | 0.000718 | better |
| resize-storm | shutdown.s | 0.151 [1.74e-05] | 0.1 [0.0251] | -33.4% | 0.000583 | 0.000718 | better |
| wasm-flood | startup.mapped_s | 0.354 [0.0252] | 0.304 [0.0251] | -14.2% | 0.000583 | 0.000718 | better |
| wasm-flood | startup.first_frame_s | 5.56 [0.126] | 0.304 [0.0251] | -94.5% | 0.000583 | 0.000718 | better |
| wasm-flood | startup.cpu_s | 8.72 [0.13] | 5.37 [0.04] | -38.4% | 0.000583 | 0.000718 | better |
| wasm-flood | steady.cpu_pct | 100 [0.15] | 157 [0.334] | +56.6% | 0.000583 | 0.000718 | worse |
| wasm-flood | steady.wakeups_per_s | 222 [9.05] | 148 [2.14] | -33.1% | 0.000583 | 0.000718 | better |
| wasm-flood | steady.pss_mean_mib | 516 [1.01] | 151 [3.77] | -70.7% | 0.000583 | 0.000718 | better |
| wasm-flood | steady.browser_fps | 0.2 [8.78e-06] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| wasm-flood | steady.gpu_util_pct | 0.48 [0.562] | 5.24 [0.24] | +991.7% | 0.000583 | 0.000718 | worse |
| wasm-flood | steady.gpu_fb_tree_mib | 199 [0] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-flood | steady.compositor_cpu_pct | 0.2 [1.44e-05] | 6.69 [0.0996] | +3251.6% | 0.000583 | 0.000718 | worse |
| wasm-flood | active.cpu_pct | 99.7 [0.102] | 82.7 [1.50] | -17.0% | 0.000583 | 0.000718 | better |
| wasm-flood | active.wakeups_per_s | 141 [4.05] | 91.0 [1.44] | -35.2% | 0.000583 | 0.000718 | better |
| wasm-flood | active.pss_peak_mib | 522 [2.67] | 165 [3.25] | -68.3% | 0.000583 | 0.000718 | better |
| wasm-flood | active.input_to_pixels_missed | 20.0 [0] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-flood | active.gpu_util_pct | 14.4 [0.64] | 2.74 [2.81] | -81.0% | 0.000583 | 0.000718 | better |
| wasm-flood | active.compositor_cpu_pct | 26.1 [0.449] | 8.19 [0.25] | -68.6% | 0.000583 | 0.000718 | better |
| wasm-flood | shutdown.s | 10.1 [0.000872] | 0.0503 [4.94e-05] | -99.5% | 0.000583 | 0.000718 | better |
| wasm-workers | startup.mapped_s | 0.354 [6.62e-05] | 0.103 [6.59e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-workers | startup.first_frame_s | 0.354 [6.62e-05] | 0.103 [6.59e-05] | -70.8% | 0.000583 | 0.000718 | better |
| wasm-workers | startup.cpu_s | 13.0 [0.015] | 12.9 [0.01] | -0.8% | 0.000583 | 0.000718 | better |
| wasm-workers | steady.cpu_pct | 401 [0.168] | 401 [0.152] | +0.2% | 0.000583 | 0.000718 | worse |
| wasm-workers | steady.wakeups_per_s | 620 [1.88] | 521 [1.38] | -16.0% | 0.000583 | 0.000718 | better |
| wasm-workers | steady.pss_mean_mib | 245 [0.789] | 79.4 [0.326] | -67.5% | 0.000583 | 0.000718 | better |
| wasm-workers | steady.browser_fps | 59.5 [0.0431] | 0 [0] | -100.0% | 0.000583 | 0.000718 | lower |
| wasm-workers | steady.gpu_util_pct | 11.0 [0.4] | 22.6 [0.79] | +106.0% | 0.000583 | 0.000718 | worse |
| wasm-workers | steady.gpu_fb_tree_mib | 169 [0] | 0 [0] | -100.0% | 0.000583 | 0.000718 | better |
| wasm-workers | steady.compositor_cpu_pct | 12.8 [0.6] | 27.9 [0.348] | +118.1% | 0.000583 | 0.000718 | worse |
| wasm-workers | active.cpu_pct | 401 [0.158] | 223 [0.157] | -44.4% | 0.000583 | 0.000718 | better |
| wasm-workers | active.wakeups_per_s | 633 [0.383] | 291 [0.455] | -54.1% | 0.000583 | 0.000718 | better |
| wasm-workers | active.pss_peak_mib | 245 [0.748] | 81.8 [0.32] | -66.6% | 0.000583 | 0.000718 | better |
| wasm-workers | active.input_to_pixels_p50_ms | 33.4 [0.0318] | 16.8 [0.0496] | -49.7% | 0.000583 | 0.000718 | better |
| wasm-workers | active.input_to_pixels_p95_ms | 33.7 [0.2] | 17.2 [0.215] | -48.8% | 0.000583 | 0.000718 | better |
| wasm-workers | active.input_to_pixels_missed | 0 [0] | 0 [0] | – | 1 | 1 | noise |
| wasm-workers | active.gpu_util_pct | 12.0 [0.22] | 13.9 [1.65] | +15.4% | 0.302 | 0.335 | noise |
| wasm-workers | active.compositor_cpu_pct | 16.2 [0.0999] | 17.7 [0.0505] | +9.3% | 0.000583 | 0.000718 | worse |
| wasm-workers | shutdown.s | 0.101 [0.0251] | 0.0503 [2.17e-05] | -50.0% | 0.000583 | 0.000718 | better |

- home: only B measured steady.fps_produced, steady.fps_delivered, steady.readback_mib_per_s, active.fps_produced, active.fps_delivered, active.browser_draw_p50_ms, active.readback_mib_per_s, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- home: not compared (baseline: GPUI's log misses the draws it makes while dispatching a key): active.browser_fps

- article: only A measured active.browser_draw_p50_ms, active.read_p50_ms, active.send_p50_ms

- article: only B measured active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- long: only A measured active.browser_draw_p50_ms, active.read_p50_ms, active.send_p50_ms

- long: only B measured active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- css-animation: only A measured active.browser_draw_p50_ms, active.read_p50_ms, active.send_p50_ms

- css-animation: only B measured active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- canvas: only A measured active.browser_draw_p50_ms, active.read_p50_ms, active.send_p50_ms

- canvas: only B measured active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- webgl: only A measured active.browser_draw_p50_ms, active.read_p50_ms, active.send_p50_ms

- webgl: only B measured active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- wasm-hello: only A measured active.browser_draw_p50_ms

- wasm-hello: only B measured startup.first_frame_engine_s, steady.fps_produced, steady.fps_delivered, steady.readback_mib_per_s, steady.ipc_pixel_mib_per_s, active.fps_produced, active.fps_delivered, active.readback_mib_per_s, active.ipc_pixel_mib_per_s, active.paint_p50_ms, active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- wasm-gradient: only B measured startup.first_frame_engine_s, steady.fps_produced, steady.fps_delivered, steady.readback_mib_per_s, steady.ipc_pixel_mib_per_s, active.fps_produced, active.fps_delivered, active.readback_mib_per_s, active.ipc_pixel_mib_per_s, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- wasm-raf: only A measured active.browser_draw_p50_ms

- wasm-raf: only B measured startup.first_frame_engine_s, steady.fps_produced, steady.fps_delivered, steady.readback_mib_per_s, steady.ipc_pixel_mib_per_s, active.fps_produced, active.fps_delivered, active.readback_mib_per_s, active.ipc_pixel_mib_per_s, active.paint_p50_ms, active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- wasm-timer: only A measured active.browser_draw_p50_ms

- wasm-timer: only B measured startup.first_frame_engine_s, steady.fps_produced, steady.fps_delivered, steady.readback_mib_per_s, steady.ipc_pixel_mib_per_s, active.fps_produced, active.fps_delivered, active.readback_mib_per_s, active.ipc_pixel_mib_per_s, active.paint_p50_ms, active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- wasm-busy: only B measured startup.first_frame_engine_s, steady.fps_produced, steady.fps_delivered, steady.readback_mib_per_s, steady.ipc_pixel_mib_per_s, active.fps_produced, active.fps_delivered, active.browser_draw_p50_ms, active.input_to_screen_p50_ms, active.input_to_screen_p95_ms, active.readback_mib_per_s, active.ipc_pixel_mib_per_s, active.paint_p50_ms, active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- wasm-busy: not compared (baseline: GPUI's log misses the draws it makes while dispatching a key): active.browser_fps

- many-tabs: only A measured active.read_p50_ms, active.send_p50_ms

- many-tabs: only B measured active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- many-tabs: not compared (baseline: GPUI's log misses the draws it makes while dispatching a key): active.browser_fps, active.browser_draw_p50_ms, active.input_to_screen_p50_ms, active.input_to_screen_p95_ms

- tab-churn: only A measured active.read_p50_ms, active.send_p50_ms

- tab-churn: only B measured active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- tab-churn: not compared (baseline: GPUI's log misses the draws it makes while dispatching a key): active.browser_fps, active.browser_draw_p50_ms

- resize-storm: only A measured active.read_p50_ms, active.send_p50_ms

- resize-storm: only B measured active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- wasm-flood: only B measured startup.first_frame_engine_s, steady.fps_produced, steady.fps_delivered, steady.readback_mib_per_s, steady.ipc_pixel_mib_per_s, active.fps_produced, active.fps_delivered, active.input_to_pixels_p50_ms, active.input_to_pixels_p95_ms, active.readback_mib_per_s, active.ipc_pixel_mib_per_s, active.paint_p50_ms, active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- wasm-flood: not compared (baseline: GPUI's log misses the draws it makes while dispatching a key): active.browser_fps, active.browser_draw_p50_ms, active.input_to_screen_p50_ms, active.input_to_screen_p95_ms

- wasm-workers: only B measured startup.first_frame_engine_s, steady.fps_produced, steady.fps_delivered, steady.readback_mib_per_s, steady.ipc_pixel_mib_per_s, active.fps_produced, active.fps_delivered, active.readback_mib_per_s, active.ipc_pixel_mib_per_s, active.paint_p50_ms, active.present_p50_ms, exit.page_readbacks, exit.max_present_ms, exit.max_pump_ms, exit.max_target_buffers

- wasm-workers: not compared (baseline: GPUI's log misses the draws it makes while dispatching a key): active.browser_fps, active.browser_draw_p50_ms, active.input_to_screen_p50_ms, active.input_to_screen_p95_ms

367 comparisons: better 254, worse 23, higher 6, lower 32, noise 52
