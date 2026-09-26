# Sighurt benchmarks

A harness for measuring what the rewrite changes (plan §26, M0's baseline, M7): the same
workloads run against the pre-rewrite browser (git tag `gpui-baseline`) and against every later
build, on one controlled machine, with enough repetitions to tell a change from noise.

It is plain Python 3 (standard library only) around three tools the headless compositor
already provides: `wtype` (keyboard), `swaymsg` (outputs) and `grim` (screenshots), plus
`nvidia-smi` when there is an NVIDIA GPU. The pointer is its own small Wayland client
(`benchlib/pointer.py`), one virtual pointer for the whole run: with `wlrctl`, which makes a new
pointer device per call, the seat has no pointer between calls, so the browser drops its
`wl_pointer` and loses clicks and wheel events sent before it has bound a new one.

## Method

Every change that is supposed to make Sighurt faster or lighter goes through the same steps:

1. **Hypothesis.** Write down in [`HYPOTHESES.md`](HYPOTHESES.md), before measuring, what
   should change, on which workload and metric, in which direction, and why. For example:
   "Presenting Servo directly (M1) removes the readback: `active.readback_mib_per_s` on `long`
   falls to 0, `active.cpu_pct` falls, and `startup.first_frame_s` does not rise." The named
   metrics are the *primary* metrics.
2. **Measurement.** Run the full set (`bench/run`) for the build before and after the change,
   on the same machine, and compare them (`bench/compare`).
3. **Decision.** Keep the change only if its primary metrics improved beyond noise **and** no
   other key metric regressed beyond noise. A regression that is accepted anyway gets an entry in
   the decisions log of `docs/rewrite/STATUS.md` saying why.

Only the primary metrics are confirmatory. `bench/compare` tests every metric on every workload
(several hundred tests), so a few will come out "significant" by chance; the Benjamini–Hochberg
q-value column shows which survive the correction for that. A surprising change outside the
hypothesis is a new hypothesis: re-run the affected workloads before believing it.

### How noise is controlled

- **One heavy job at a time.** The rounds of a set run two at a time (`--lock-rounds`) inside
  one `tools/heavy` call, which holds `/tmp/gusahlg-heavy.lock` from before the first compositor
  starts until the last run of those rounds has shut down, so no build or other benchmark
  shares the machine with a run. The lock is released in between so other work can interleave;
  every run is still clean. Two rounds hold it for about 20 minutes, like a Servo build. Fewer,
  longer holds because `flock` doesn't queue its waiters: when it is released, any of them may
  get it, so with fifteen jobs waiting (26 September) one acquisition can take an hour.
- **A quiet machine.** After taking the lock, `bench/measure` waits (up to 2 minutes) until the
  whole machine uses less than 0.5 CPU and less than 5 % GPU over two seconds. What it saw is
  recorded in the run (`gate`); a run that started on a busy machine is marked as an anomaly.
- **A private compositor.** Each run gets its own headless sway (`tools/headless-sway`), with a
  1280×800 output at scale 1, on the GPU (`--renderer auto`: gles2, else pixman). The first run
  of a set makes the choice, inside the lock, and every later run uses it: a run that can't get
  that renderer fails instead of silently measuring another compositor path.
- **Fresh state, warm caches.** `sig` gets a new, empty `XDG_CONFIG_HOME` (holding only the
  build's benchmark config) and `XDG_DATA_HOME` (so Servo's profile starts empty) every run,
  and a per-build `XDG_CACHE_HOME` kept between runs (shader, font and Wasm AOT caches), which
  the warmup round fills.
- **Repetition and order.** A set is one warmup round (recorded, never compared) and five
  measured rounds (`--rounds`). Each round runs every workload once, in a new random order, so
  slow drifts (temperature, the other session's jobs between runs) spread over all workloads.
  The seed is recorded.
- **Recorded environment.** Every set records the kernel, OS, CPU model, governor, EPP and
  boost, the GPU, driver and VBIOS, the compositor and renderer, the binaries' SHA-256, the build
  revision, the harness revision, a hash of the harness files that decide the numbers (`run`,
  `measure`, the probes, pointer, parsers and metrics, the build definitions and `tools/`) and
  a hash of the workload files. Every run also records load average, free memory, swap, CPU
  clock and GPU temperature, clock and P-state before and after, and the harness's own CPU
  time. `bench/compare` prints every difference between two sets' environments, and refuses to
  be quiet about different workload files.

What is *not* controlled: the CPU governor is `powersave` with EPP `balance_performance` on
louise-pc and the harness does not change it (that needs root); the GPU is shared with the X11
desktop (idle) and with a llama.cpp RPC server that the other session runs; turbo is on. The
quiet gate and the randomized order are what protect the comparison from these.

### Statistics

For each metric and workload a set gives n values (5 by default). `bench/report` shows the
median and the interquartile range (Q1–Q3, linear interpolation, like NumPy's default).
`bench/compare` shows, for two sets A and B:

- the medians and IQRs, and the relative change of the median;
- a two-sided **Mann–Whitney U** p-value (`benchlib/stats.py`). For up to 40 pooled values it is
  exact: the permutation distribution of the rank sum given the observed ranks, ties included.
  Above that, the normal approximation with tie and continuity corrections. With 5 runs against
  5 the smallest possible p is 2/252 ≈ 0.008;
- a Benjamini–Hochberg q-value over all rows of the comparison;
- a verdict: a change counts only if p < 0.05 **and** the medians differ by more than the larger
  of the two IQRs. Otherwise it is `noise`. Metrics where lower is better are labelled
  `better`/`worse`; frame counts and rates, which are good or bad depending on the workload, are
  labelled `higher`/`lower`.

The tests in `tests/` check the exact p-values against brute-force enumeration, the quartiles
against NumPy's definition, the parsers against sig-servo's and GPUI's log lines, the window
arithmetic against a hand-computed process tree, the input-latency matching, and `compare` and
`report` on synthetic sets: `python3 -m unittest discover -s bench/tests`.

## Running it

The benchmark machine is louise-pc. `bench/remote` copies `bench/`, `tools/` and `shell.nix` to
`~/sighurt-bench/harness` there and runs a command in that copy (with this tree's git revision
as `SIG_BENCH_REV`). It is not `tools/remote`: other people's syncs don't change the harness
under a running benchmark, and it doesn't hold `tools/remote`'s lock for hours.

```sh
# The baseline binaries, once (see bench/baseline/build.sh):
git archive gpui-baseline | ssh louise-pc 'mkdir -p ~/sighurt-baseline && tar -x -C ~/sighurt-baseline'
bench/remote --detach bench/baseline/build.sh

# A full set: 14 workloads x (1 warmup + 5 rounds), about 55 minutes plus lock waits.
bench/remote --detach bench/run --build bench/builds/baseline.toml --label baseline --out ~/sighurt-bench/results
bench/remote --no-sync tail -f ~/sighurt-bench/logs/latest.log

# Bring the results here (small files under bench/results/<host>/, raw logs under
# bench/results/raw/, which git ignores):
rsync -a louise-pc:sighurt-bench/results/ bench/results/

bench/report bench/results/louise/baseline/<timestamp>
bench/compare bench/results/louise/baseline/<timestamp> bench/results/louise/<label>/<timestamp>
```

A quick look at a few workloads: `--workloads article,webgl --rounds 2 --warmup 0`. One run by
hand, inside the lock and a compositor:
`tools/heavy tools/headless-sway --renderer auto -- bench/measure --build bench/builds/baseline.toml --workload article --out /tmp/run.json --raw /tmp/raw`.

### Build cost

M7 compares build cost too. It is not a benchmark run, so it is recorded by hand in the result
notes (`results/BASELINE.md` for the baseline), always the same way: a release build from an
empty target directory on louise-pc, under `tools/heavy` (so 8 jobs and nothing else running),
timed by cargo's `Finished … in` line; the binaries' sizes (`set.json` has them for every set);
and the number of packages in the lockfiles.

## What a run does and measures

`bench/measure` starts `sig` with the workload's files as arguments (one tab each), then:

| Phase | Window | What happens |
|---|---|---|
| launch → first frame | `startup` | `grim` polls the screen every 50 ms |
| settle (3 s) | `startup` | the pointer is moved to the middle of the page; loading finishes; a screenshot (raw logs) |
| steady (10 s) | `steady` | no input: the idle window for a static page |
| active (10 s) | `active` | the workload's interaction script |
| recovery (3 s; resize storm and tab churn) | `after` = steady end → recovery end | nothing: the output is back at its first size, the extra tabs are closed |
| quit | `shutdown.s` | ctrl+q (`browser.quit`); SIGTERM after 10 s, SIGKILL after 5 more |

Metric names are `<window>.<metric>`; `total` is launch → active end. The process tree is `sig`
and all its descendants (the baseline's `sig-servo` and `sig-wasm` processes); `_browser` is
`sig` itself and `_engines` the rest.

| Metric | Meaning and source |
|---|---|
| `startup.mapped_s` | launch → the chrome first appears on screen (grim; ±50 ms) |
| `startup.first_frame_s` | launch → the page area first shows content on screen (grim; ±50 ms) |
| `startup.first_frame_engine_s` | launch → the first frame the engine delivered (its diagnostics) |
| `startup.first_frame_after_navigate_ms` | the engine's own navigate → first frame time |
| `cpu_s`, `cpu_user_s`, `cpu_sys_s`, `cpu_pct` | CPU time of the tree (`/proc/<pid>/stat`, 10 ms resolution per process; includes exited threads) and as % of one CPU |
| `cpu_pct_browser`, `cpu_pct_engines` | the same split between `sig` and its children |
| `cpu_sched_ms`, `runq_wait_ms` | ns-precise run time and time spent runnable but waiting for a CPU, summed over threads (`schedstat`) |
| `ctxsw_vol`, `ctxsw_invol`, `wakeups_per_s` (and `_browser`, `_engines`) | voluntary and involuntary context switches summed over all threads (`/proc/<pid>/task/*/status`). A voluntary switch is a sleep, so voluntary switches per second are wakeups per second: in the `steady` window of a static page, the idle wakeups |
| `rss_mean_mib`, `rss_peak_mib` | tree RSS sampled every 100 ms (`statm`) |
| `pss_mean_mib`, `pss_peak_mib`, `pss_end_mib` | tree PSS every second and at marks (`smaps_rollup`): shared pages counted once |
| `rss_growth_mib`, `pss_growth_mib` | memory at the window's end minus at its start (`after.*`: across the resize storm or the tab churn and its recovery) |
| `frames_produced`, `frames_delivered`, `fps_*` | from the build's diagnostics (`benchlib/parsers.py`). Baseline: a frame Servo painted and read back, and one it sent to `sig` (unchanged frames are not sent) |
| `browser_frames`, `browser_fps`, `browser_draw_p50_ms` (and p95) | frames the browser drew its window for, and how long each draw took. Baseline: GPUI's `ZED_MEASUREMENTS=1` lines, one per `Window::draw` + `present` (chrome and page area together; a frame that only re-presents is not counted) |
| `input_to_frame_p50_ms`, `input_to_screen_p50_ms` (and p95, `_missed`) | latency of the inputs a script marks (scrolls: page input; tab switches and opening or closing a tab: chrome input), from the input tool returning (the event is at the compositor) to the engine's next delivered frame, and to that frame reaching the compositor (baseline: the next GPUI window draw after it; an engine that presents itself: the frame). An input without a frame before the next one, or within 1 s, is counted in `_missed` (`benchlib/metrics.py` `latency`) |
| `readback_mib(_per_s)` | page pixels read back from the GPU to the CPU (w × h × 4 per produced frame) |
| `ipc_pixel_mib(_per_s)`, `ipc_pixel_mib_per_frame` | page pixels sent over IPC (w × h × 4 per delivered frame) |
| `ipc_out_mib(_per_s)`, `ipc_out_writes` | bytes and write calls of the engine processes (`/proc/<pid>/io` `wchar`, `syscw`): their protocol output (frames, draw lists) plus their stderr. The only frame-volume measure for `sig-wasm`, which has no diagnostics |
| `paint_p50_ms`, `read_p50_ms`, `send_p50_ms` (and p95) | per-frame paint, readback and IPC times from the diagnostics. Baseline: `send` is from the end of the readback to the end of the write, so it includes comparing the frame with the previous one |
| `gpu_util_pct`, `gpu_mem_used_mib`, `gpu_clock_mhz` | `nvidia-smi` every 200 ms: the whole GPU (desktop and other processes included) |
| `gpu_fb_tree_mib` | GPU memory of the tree's processes at the window's end (`nvidia-smi pmon`) |
| `compositor_cpu_pct` | the headless sway's CPU: compositing cost the browser causes |
| `shutdown.s`, `shutdown.forced` | ctrl+q → every process of the tree gone; 1 if a signal was needed |

Known limits of the measurements:

- Screen times are when the pixels reached the compositor, not a display: the output is
  headless. Polling adds up to 50 ms and some load during startup.
- A thread or process that exits inside a window counts up to its last full sample (≤ 1 s
  before). `processes_exited` says when that happened.
- Every `wtype` call creates and removes a virtual keyboard and uploads its keymap, which the
  browser has to load. It is part of every workload that presses keys and the same for every
  build.
- `nvidia-smi` GPU utilization covers the whole GPU. Per-process SM use is not reliable for
  graphics contexts, so only per-process memory is used.
- The baseline's `sig-wasm` has no timing diagnostics: its frames are not counted, only its CPU,
  memory and output volume, and the window draws `sig` makes for them.

## Workloads

Defined in `workloads.toml`; the files are in `workloads/` and need nothing from the network.

| Workload | Page | Interaction (active window) |
|---|---|---|
| `home` | sig's built-in home page, no engine | ctrl+l, types 29 characters, 5 a second, then Escape |
| `article` | text-heavy article with four images | wheel scrolls down 5 s, then up 5 s: 10 events a second of one notch each |
| `long` | 80 sections, ~26 000 words, images and tables | wheel scrolls down for 10 s: 10 events a second of three notches each |
| `css-animation` | endless CSS transform, opacity, color and width animations | pointer circles over the page |
| `canvas` | 2D canvas, 300 balls, redrawn every frame | pointer circles |
| `webgl` | WebGL quad rotating in 3D | pointer circles |
| `wasm-hello`, `wasm-gradient`, `wasm-raf` | the example guests hello-sighurt, gradient-demo, raf-demo, built from `gpui-baseline` | pointer circles |
| `wasm-timer` | the example guest timer-demo | clicks its four buttons: set_interval at 1 s, 0.5 s and 0.1 s, a 3 s set_timeout |
| `wasm-busy` | a guest that spends about 36M of its 50M fuel on every frame (§26's hostile workload) | chrome input: ctrl+t and ctrl+w alternately, twice a second |
| `many-tabs` | 8 tabs of the article, the first visible | ctrl+tab once a second |
| `tab-churn` | the article | 20 s: every 2 s a new tab (ctrl+t), the article opened in it (ctrl+o), the tab closed (ctrl+w); then 3 s recovery |
| `resize-storm` | the article | output size and scale change 4 times a second, then 3 s recovery |

`workloads/generate.py` wrote `article.html`, `long.html`, the PNGs and `wasm/busy.wasm`
(deterministically; the guest is hand-assembled, and `wasm2wat` shows it); the animation pages
are hand-written. The example guests in `workloads/wasm/` were built by
`bench/baseline/build.sh` from `gpui-baseline`. All are checked in, so later builds run exactly
the same guest code (`workloads/wasm/SHA256SUMS`).

## Extending it

- **A workload:** add the files to `workloads/` and a table to `workloads.toml`. Don't change a
  workload that a recorded set used; add a new one, or the comparison against it is void (the
  workload hash in every set shows it).
- **An interaction:** a function in `SCRIPTS` in `bench/measure`, taking the run's context and
  the window length and returning when the window is over. Use `at_rate` for fixed rates.
- **A build:** a `builds/<name>.toml` (see `builds/baseline.toml`): where `sig` is, which
  binaries to hash, extra environment, the config that binds the keys the scripts press, the
  chrome's height for the screen probe, and the diagnostic parsers. The keys: ctrl+l
  `location.focus`, ctrl+tab `tab.next`, ctrl+t `tab.new`, ctrl+w `tab.close`, ctrl+o
  `page.open {workloads}/article.html` (`{workloads}` becomes the workloads directory) and
  ctrl+q `browser.quit`; Escape in the URL bar must drop the text and give the page the focus.
  Measure release builds only: the workspace's dev profile optimizes dependencies but keeps
  debug assertions on.
- **Diagnostics of a new binary:** a parser class in `benchlib/parsers.py` that turns a stderr
  line into a `frame`, `renderer`, `first-frame` or `browser-frame` event (the kinds are
  documented at the top of that file), listed in the build's `parsers`. For the comparisons in
  `HYPOTHESES.md`, the rewrite's `sig` should print one line per page frame it presents (with
  the bytes it read back and sent, 0 on the direct path, so those metrics show 0 rather than
  "not measured") and one per chrome paint, behind an environment variable like the baseline's
  `SIG_SERVO_TIMING`.
- **A metric:** compute it in `benchlib/metrics.py` from the samples (`window` for per-window
  values), add it to `KEY_METRICS` in `benchlib/results.py` if the report should show it, and to
  `LOWER_IS_BETTER` if it has a direction. Add a test with hand-computed numbers.

## Files

| Path | What |
|---|---|
| `run` | a result set: rounds × workloads, each through `tools/heavy` and `tools/headless-sway` |
| `measure` | one run: gate, launch, probes, script, quit, metrics |
| `report`, `compare` | Markdown summary of one set; comparison of two |
| `remote` | sync the harness to the benchmark machine and run there |
| `benchlib/` | `probes.py` (/proc, nvidia-smi, grim), `pointer.py` (the virtual pointer), `parsers.py`, `metrics.py`, `stats.py`, `results.py` |
| `builds/` | build definitions and their benchmark configs |
| `baseline/build.sh` | builds `gpui-baseline` in release mode |
| `results/<host>/<label>/<timestamp>/` | `set.json` and `runs/<round>-<workload>.json` (committed) |
| `results/raw/` | per run: stderr with timestamps, samples, the sway log, and screenshots after settling and after the active window (not committed) |
| `results/BASELINE.md` | the recorded baseline |
| `HYPOTHESES.md` | what each change is expected to do, written before it is measured, and the outcome |
