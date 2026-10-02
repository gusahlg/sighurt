# Sighurt benchmarks

A harness for measuring what the rewrite changes (plan §26, M0's baseline, M7): the same
workloads run against the pre-rewrite browser (git tag `gpui-baseline`) and against every later
build, on one controlled machine, with enough repetitions to tell a change from noise.

It is plain Python 3 (standard library only) around two tools the headless compositor already
provides, `swaymsg` (outputs) and `grim` (screenshots), plus `nvidia-smi` when there is an NVIDIA
GPU. Input is its own small Wayland client (`benchlib/seat.py`): one virtual pointer and one
virtual keyboard (with the checked-in US keymap `benchlib/us.xkb`) for the whole run, created
before `sig` starts. `wlrctl` and `wtype` make a new device per call and remove it when they
exit, so the seat has no pointer or keyboard between calls: the browser drops its `wl_pointer`
or `wl_keyboard`, binds a new one (and loads the keymap again) when the next device appears,
and loses the events sent before it has. The first baseline set, driven by `wtype`, lost ctrl+q
in 7 of 84 runs and 1–5 of 20 chrome inputs per `wasm-busy` run that way.

## Method

Every change that is supposed to make Sighurt faster or lighter goes through the same steps:

1. **Hypothesis.** Write down in [`HYPOTHESES.md`](HYPOTHESES.md), before measuring, what
   should change, on which workload and metric, in which direction, and why. For example:
   "Presenting Servo directly (M1) removes the readback: `active.readback_mib_per_s` on `long`
   falls to 0, `active.cpu_pct` falls, and `startup.first_frame_s` does not rise." The named
   metrics are the *primary* metrics.
2. **Measurement.** Run the full set (`bench/run`) for the build before and after the change,
   on the same machine, best in one session that interleaves the two builds' runs, and compare
   them (`bench/compare`).
3. **Decision.** Keep the change only if its primary metrics improved beyond noise **and** no
   other key metric regressed beyond noise. A regression that is accepted anyway gets an entry in
   the decisions log of `docs/rewrite/STATUS.md` saying why.

Only the primary metrics are confirmatory. `bench/compare` tests every metric on every workload
(several hundred tests), so a few will come out "significant" by chance; the Benjamini–Hochberg
q-value column shows which survive the correction for that. A surprising change outside the
hypothesis is a new hypothesis: re-run the affected workloads before believing it.

### How noise is controlled

- **One heavy job at a time.** The runs go through `tools/heavy` in groups, each holding
  `/tmp/gusahlg-heavy.lock` from before its first compositor starts until its last run has shut
  down, so no build or other benchmark shares the machine with a run. The lock is released in
  between so other work can interleave; every run is still clean. By default a group is two
  rounds (`--lock-rounds`, about 20 minutes per build, like a Servo build): fewer, longer holds,
  because `flock` doesn't queue its waiters (when it is released, any of them may get it, so
  with fifteen jobs waiting on 26 September one acquisition took an hour). On a machine others
  need for short jobs, `--lock-runs 1` holds it for one run at a time.
- **A quiet machine.** After taking the lock, `bench/measure` waits (up to 2 minutes) until the
  whole machine uses less than 0.5 CPU and less than 5 % GPU over two seconds. What it saw is
  recorded in the run (`gate`); a run that started on a busy machine is marked as an anomaly.
- **A private compositor.** Each run gets its own headless sway (`tools/headless-sway`), with a
  1280×800 output at scale 1, on the GPU (`--renderer auto`: gles2, else pixman). The first run
  of a session makes the choice, inside the lock, and every later run uses it: a run that can't
  get that renderer fails instead of silently measuring another compositor path. Sway, grim and
  the other tools come from the repository's pinned nixpkgs (`nix/nixpkgs.nix`), not the
  machine's, so every machine and every build runs the same compositor build; each set records
  its store path (`compositor_store_path`) and the pin's revision (`nixpkgs`).
- **Fresh state, warm caches.** `sig` gets a new, empty `XDG_CONFIG_HOME` (holding only the
  build's benchmark config), `XDG_DATA_HOME` (so Servo's profile starts empty) and
  `XDG_STATE_HOME` (the rewrite saves its tabs there and restores them beside the ones it is
  asked to open, so without it a run would reopen the previous run's pages) every run, and a
  per-build `XDG_CACHE_HOME` kept between runs (shader, font and Wasm AOT caches), which the
  warmup round fills.
- **Repetition and order.** A set is one warmup round (recorded, never compared) and five
  measured rounds (`--rounds`). Each round runs every workload once, in a new random order, so
  slow drifts (temperature, the other session's jobs between runs) spread over all workloads.
  The seed is recorded.
- **Builds compared in one session.** Given several builds (`--build` and `--label` once each),
  `bench/run` measures them in one session: each round runs every workload of every build
  once, all in one random order, so drift during the session (temperature, other jobs between
  lock holds, the machine's state) affects every build alike, and every build runs under the
  same compositor, driver, harness and tools. Each build gets its own set; the session file
  (`results/<host>/sessions/<timestamp>.json`) has the shared order, and each run its place in
  its round (`session_index`).
- **The same binaries throughout.** `bench/run` resolves the build's directory once (a
  `rewrite-latest` link stays on the build it named when the set started) and hashes its
  binaries then and again before every run; if they change, the set stops.
- **Recorded environment.** Every set records the kernel, OS, CPU model, governor, EPP and
  boost, the GPU, driver and VBIOS, the compositor and renderer, the binaries' SHA-256, the build
  revision, the build's definition and config, the harness revision, a hash of the harness files
  that decide the numbers (`run`, `measure`, the probes, seat, keymap, parsers and metrics, and
  `tools/`; not the build definitions, so adding a build changes no hash) and a hash of the
  workload files. Every run also records load average, free memory, swap, CPU
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

`bench/compare` also lists, per set and workload, how many measured runs had anomalies (a
crash, a forced quit, a busy machine; `bench/report` lists each) and how many failed. They are
included unless `--exclude-anomalous` is given; with it, a workload may have fewer than five
runs a side. When the two builds place their engines differently (`engines` in the build
definition: `processes` for the baseline, `in-process` for the rewrite), the metrics that split
the browser process from the engine processes (`*_browser`, `*_engines`, `ipc_out_*`) are left
out, with a line saying so: in-process, `_engines` and `ipc_out_` are 0 by construction and
`_browser` holds everything, so their differences are the process layout, not work.

The tests in `tests/` check the exact p-values against brute-force enumeration, the quartiles
against NumPy's definition, the parsers against sig-servo's, GPUI's and the `sig-diag` format's
lines, the window arithmetic against a hand-computed process tree, counters, the input-latency
matching, the keyboard's wire messages, the crash patterns against the baseline's GPU crash, and
`compare` and `report` on synthetic sets: `python3 -m unittest discover -s bench/tests`.

## Running it

The benchmark machine is louise-pc. `bench/remote` copies `bench/`, `tools/` and `shell.nix` to
`~/sighurt-bench/harness` there and runs a command in that copy (with this tree's git revision
as `SIG_BENCH_REV`). It is not `tools/remote`: other people's syncs don't change the harness
under a running benchmark, and it doesn't hold `tools/remote`'s lock for hours.

```sh
# Two builds interleaved in one session (how M7's comparison was measured):
bench/remote --detach bench/run --build bench/builds/baseline.toml --label baseline \
  --build bench/builds/rewrite.toml --label rewrite --build-dir ~/sighurt-baseline \
  --build-dir ~/sighurt-bench/builds/<rev> --rounds 7 --lock-runs 1 --out ~/sighurt-bench/results

# The baseline binaries, once (see bench/baseline/build.sh):
git archive gpui-baseline | ssh louise-pc 'mkdir -p ~/sighurt-baseline && tar -x -C ~/sighurt-baseline'
bench/remote --detach bench/baseline/build.sh

# A full set: 16 workloads x (1 warmup + 5 rounds), about an hour plus lock waits.
bench/remote --detach bench/run --build bench/builds/baseline.toml --label baseline --out ~/sighurt-bench/results
bench/remote --no-sync tail -f ~/sighurt-bench/logs/latest.log

# Bring the results here (small files under bench/results/<host>/, raw logs under
# bench/results/raw/, which git ignores):
rsync -a louise-pc:sighurt-bench/results/ bench/results/

bench/report bench/results/louise/baseline/<timestamp>
bench/compare bench/results/louise/baseline/<timestamp> bench/results/louise/<label>/<timestamp>
```

The rewrite's `sig` at a revision (see `bench/rewrite/build.sh`): it builds a `git archive` of
that revision on louise-pc, with the Rust of the revision's pinned nixpkgs (`shell.nix` brings
none, and louise-pc's own is older than the workspace's `rust-version`), into
`~/sighurt-bench/builds/<rev>`, which nothing writes again, and points
`~/sighurt-bench/builds/rewrite-latest` at it, which `builds/rewrite.toml` measures:

```sh
bench/rewrite/build.sh HEAD            # detached; bench/remote --no-sync tail -f ~/sighurt-bench/logs/latest.log
bench/remote --detach bench/run --build bench/builds/rewrite.toml --label rewrite --out ~/sighurt-bench/results
# an earlier build: ... --build-dir ~/sighurt-bench/builds/<rev>
```

A quick look at a few workloads: `--workloads article,webgl --rounds 2 --warmup 0`. One run by
hand, inside the lock and a compositor:
`tools/heavy tools/headless-sway --renderer auto -- bench/measure --build bench/builds/baseline.toml --workload article --out /tmp/run.json --raw /tmp/raw`.

### The pipe alone

`bench/pipe-cost` measures what the baseline's frame transport cost by itself, without Servo,
GPUI or a readback: a writer process sends 1280×696 RGBA frames at 59.3 a second through a pipe
in sighurt-ipc's framing and a reader process reads each one whole, as `sig-servo` and `sig`
did. It prints both processes' CPU and wakeups per second and the frame's crossing time, per
run and as medians. Run it on the benchmark machine under the lock:
`bench/remote tools/heavy bench/pipe-cost`.

### Build cost

M7 compares build cost too. `bench/build-cost` measures it, always the same way: release builds
from an empty target directory on louise-pc, under `tools/heavy` (so 8 jobs and nothing else
running), timed by cargo's `Finished … in` line, and the number of packages in the lockfiles. It
writes `results/<host>/build-cost/<label>-<timestamp>.json`; the binaries' sizes are in every
set's `set.json`. The baseline's (its summary is in `results/BASELINE.md`):

```sh
bench/remote --detach bench/build-cost --label baseline --tree ~/sighurt-baseline \
  --step "sig + sig-wasm=cargo build --release -p sighurt-ui -p sighurt-engine-wasm" \
  --step "sig-servo=cargo build --release --manifest-path sighurt-engine-servo/Cargo.toml" \
  --lockfile Cargo.lock --lockfile sighurt-engine-servo/Cargo.lock
```

The rewrite's is one step, `sig=cargo build --release -p sighurt-browser`, over a `git archive`
of the revision (`~/sighurt-bench/src/<rev>` after `bench/rewrite/build.sh`), with
`--lockfile Cargo.lock`.

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
| check | `anomalies`, `exit` | how `sig` ended and what its stderr says (below) |

A run is marked with an anomaly (`anomalies` in its JSON, listed by `bench/report`, counted by
`bench/compare`) when the machine was not quiet before it, no page content appeared, `sig`
ended during the run, ctrl+q did not quit it within 10 s (`shutdown.forced`), it ended with
anything but a clean exit or the signal the harness sent (an abort while quitting, say; `exit`
records how it ended), its stderr (every process of the tree) has a crash line (`panicked at`,
`fatal runtime error`, `Segmentation fault`, `core dumped`, `GPU has crashed`, a lost Vulkan
device), a diagnostics line was malformed, or the build's report at exit is missing or shows
another number of tabs than the run opened (every script ends with the tabs it started with).

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
| `input_to_pixels_p50_ms` (and p95, `_missed`) | chrome input (tab switches, opening and closing a tab) → the chrome's pixels changed on screen: screenshots of the chrome strip (`chrome_top` high), one after another from the moment the keys were sent, until one differs from the one taken just before them. It needs nothing from the build, so it compares any two. At most one screenshot (16 ms on louise-pc) late; an input with no change within its limit (0.45 s at two inputs a second, 0.9 s at one) is `_missed` |
| `readback_mib(_per_s)` | page pixels read back from the GPU to the CPU (w × h × 4 per produced frame) |
| `ipc_pixel_mib(_per_s)`, `ipc_pixel_mib_per_frame` | page pixels sent over IPC (w × h × 4 per delivered frame) |
| `ipc_out_mib(_per_s)`, `ipc_out_writes` | bytes and write calls of the engine processes (`/proc/<pid>/io` `wchar`, `syscw`): their protocol output (frames, draw lists) plus their stderr. The only frame-volume measure for `sig-wasm`, which has no diagnostics |
| `paint_p50_ms`, `read_p50_ms`, `send_p50_ms` (and p95) | per-frame paint, readback and IPC times from the diagnostics. Baseline: `send` is from the end of the readback to the end of the write, so it includes comparing the frame with the previous one. Rewrite: `paint` is Servo's `WebView::paint` (the CPU engines' whole `present`) |
| `present_p50_ms` (and p95) | rewrite: the whole `present` call of a page frame, painting and swap included |
| `swap_p50_ms` (and p95) | Servo's `swap` of a frame into its surface, from its timing lines (`RUST_LOG=sighurt_engine_servo::timing=debug`, the audit build) |
| `gpu_util_pct`, `gpu_mem_used_mib`, `gpu_clock_mhz` | `nvidia-smi` every 200 ms: the whole GPU (desktop and other processes included) |
| `gpu_fb_tree_mib` | GPU memory of the tree's processes at the window's end (`nvidia-smi pmon`) |
| `compositor_cpu_pct` | the headless sway's CPU: compositing cost the browser causes |
| `shutdown.s`, `shutdown.forced` | ctrl+q → every process of the tree gone; 1 if a signal was needed |
| `ctr.<name>`, `ctr.<name>_per_s` | a build's own counters (`counters` diagnostics): the increase over the window and its rate. `page_frames_painted`, `page_frames_presented`, `readback_bytes` and `chrome_paints` also give `frames_produced`, `frames_delivered`, `readback_mib` and `browser_frames` (and their rates) when the build prints no per-event lines. An engine's own counts are `ctr.engine.<name>.readbacks` and, audited, `ctr.engine.<name>.gl_reads` |
| `exit.*` | from the report a build writes at exit (`diagnostics_file`): `exit.page_readbacks` and `exit.readback_mib` (the readbacks the engines counted), `exit.gl_reads` (only when audited), `exit.present_errors`, and the peaks: `exit.max_present_ms`, `exit.max_pump_ms`, `exit.max_chrome_paint_ms`, `exit.max_host_queue`, `exit.max_pump_events`, `exit.max_target_buffers`, `exit.max_retiring_generations`, `exit.max_buffer_mib` |

Known limits of the measurements:

- Screen times are when the pixels reached the compositor, not a display: the output is
  headless. Polling adds up to 50 ms and some load during startup.
- A thread or process that exits inside a window counts up to its last full sample (≤ 1 s
  before). `processes_exited` says when that happened.
- An input's time is when the harness wrote it to the compositor's socket; the compositor then
  forwards it to the browser.
- GPUI (the baseline's `ZED_MEASUREMENTS`) logs only the draws of its frame callback. A draw it
  makes while dispatching a key (gpui-0.2.2 `Window::dispatch_key_event`, when the window is
  dirty) is not logged, so the baseline's `browser_frames` miss most key-driven chrome redraws
  and its chrome `input_to_screen` is not measurable; `input_to_pixels` is the chrome latency
  to compare. The first baseline set's chrome `input_to_screen` values timed the redraws that
  `wtype`'s keyboard coming and going caused, not the keys'.
- The baseline's events are timed when the harness read their stderr line; the `sig-diag`
  format carries its own `t` (CLOCK_MONOTONIC), so those are timed when `sig` wrote them.
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
| `wasm-flood` | a guest drawing 250 000 moving rectangles and writing 200 console lines every frame (§26's output limits) | chrome input: ctrl+t and ctrl+w alternately, twice a second |
| `wasm-workers` | a guest animating a square while four workers stay busy (§26's worker fairness) | chrome input: ctrl+t and ctrl+w alternately, twice a second |

`workloads/generate.py` wrote `article.html`, `long.html`, the PNGs and `wasm/busy.wasm`,
`wasm/flood.wasm`, `wasm/workers-main.wasm` and `wasm/workers-busy.wasm` (deterministically; the
guests are hand-assembled, their WAT is in the script's comments, and `wasm2wat` shows them;
`wasm-flood` and `wasm-workers` were added after the first baseline set); the animation pages
are hand-written. The example guests in `workloads/wasm/` were built by
`bench/baseline/build.sh` from `gpui-baseline`. All are checked in, so later builds run exactly
the same guest code (`workloads/wasm/SHA256SUMS`).

## Extending it

- **A workload:** add the files to `workloads/` and a table to `workloads.toml`. Don't change a
  workload that a recorded set used; add a new one, or the comparison against it is void (the
  workload hash in every set shows it).
- **An interaction:** a function in `SCRIPTS` in `bench/measure`, taking the run's context and
  the window length and returning when the window is over. Use `at_rate` for fixed rates.
- **A build:** a `builds/<name>.toml` (see `builds/baseline.toml` and `builds/rewrite.toml`):
  where `sig` is, which binaries to hash, where the engines run (`engines`), extra environment,
  the config that binds the keys the scripts press, the chrome's height for the screen probe,
  the diagnostic parsers, for a build that prints counters on a signal `counters_signal`, and
  for one that writes its report as JSON (`sig --diagnostics FILE`) `diagnostics_file = true`:
  `bench/measure` then keeps the report written at exit in the run (`diagnostics`) and its
  `exit.*` metrics. `builds/rewrite-audit.toml` is the rewrite with Servo's GL read audit and
  its timing lines: they cost CPU (the audit times every GL call; `RUST_LOG` raises the `log`
  level for the whole process), so it checks readbacks and swap times and is not compared for
  performance.
  The keys: ctrl+l
  `location.focus`, ctrl+tab `tab.next`, ctrl+t `tab.new`, ctrl+w `tab.close`, ctrl+o
  `page.open {workloads}/article.html` (`{workloads}` becomes the workloads directory) and
  ctrl+q `browser.quit`; Escape in the URL bar must drop the text and give the page the focus.
  Measure release builds only: the workspace's dev profile optimizes dependencies but keeps
  debug assertions on.
- **Diagnostics of a new binary:** a parser class in `benchlib/parsers.py` that turns a stderr
  line into a `frame`, `renderer`, `first-frame`, `browser-frame`, `counters` or
  `engine-timing` event (the kinds are documented at the top of that file), listed in the
  build's `parsers`. What its diagnostics can't measure honestly against another build's goes
  in the class's `INCOMPARABLE` (GPUI's draw log, for one); `bench/compare` leaves those rows
  out and lists them with the reason.
- **The rewrite's diagnostics** (`sighurt-browser`'s `diagnostics` module) are read by the
  `sig-diag` parser; its docstring is the format. With `SIG_DIAG=1`, `sig` writes
  `sig-diag t=<CLOCK_MONOTONIC s> <kind> key=value …` lines to stderr. The counters are the
  core of it: cumulative integers, printed as one `counters` line when `sig` gets SIGUSR1
  (`counters_signal`; `bench/measure` sends it at every mark after the first frame) and at
  exit, so there is no timer and nothing per frame. `page_frames_painted`,
  `page_frames_presented`, `readback_bytes` (0 on the direct path, so readback shows 0 rather
  than "not measured") and `chrome_paints` (paints of chrome-drawn surfaces only, for H3) give
  the metrics the baseline has under the same names; every other counter (pumps, input events,
  effects, buffers) is reported as `ctr.<name>`. The latency metrics need an event per page
  frame and per chrome paint with its time: the `frame` and `chrome` lines, which `sig` may
  print behind a second variable to keep a write per frame out of the other measurements.
- **A metric:** compute it in `benchlib/metrics.py` from the samples (`window` for per-window
  values), add it to `KEY_METRICS` in `benchlib/results.py` if the report should show it, and to
  `LOWER_IS_BETTER` if it has a direction. Add a test with hand-computed numbers.

## Files

| Path | What |
|---|---|
| `run` | a result set: rounds × workloads, each through `tools/heavy` and `tools/headless-sway` |
| `measure` | one run: gate, launch, probes, script, quit, checks, metrics |
| `report`, `compare` | Markdown summary of one set; comparison of two |
| `build-cost` | clean release builds under the lock, timed, and lockfile package counts |
| `pipe-cost` | the baseline's frame transport alone (frames through a pipe, as `sig-servo` sent them to `sig`): CPU and wakeups of writer and reader |
| `remote` | sync the harness (`bench/`, `tools/`, `shell.nix`, `nix/`) to the benchmark machine and run there |
| `benchlib/` | `probes.py` (/proc, nvidia-smi, grim), `seat.py` and `us.xkb` (the virtual pointer and keyboard), `parsers.py`, `metrics.py`, `stats.py`, `results.py` |
| `builds/` | build definitions and their benchmark configs (`rewrite-audit.toml`: the rewrite audited, not for performance) |
| `baseline/build.sh` | builds `gpui-baseline` in release mode |
| `rewrite/build.sh` | builds a revision of the rewrite's `sig` into its own directory |
| `results/<host>/<label>/<timestamp>/` | `set.json` and `runs/<round>-<workload>.json` (committed) |
| `results/<host>/sessions/<timestamp>.json` | a session: its sets and the order its runs had (committed) |
| `results/<host>/exploratory/<date>/` | one-off runs of `bench/measure` with a workload file of their own (longer windows, more cycles), with that file and the script that ran them: evidence for a report, not sets to compare |
| `results/IPC-REMOVAL.md` | the baseline against the rewrite at 3c510fe: what removing the IPC, GPUI and the readback did |
| `results/<host>/build-cost/` | `bench/build-cost` records (committed) |
| `results/raw/` | per run: stderr with timestamps, samples, the sway log, and screenshots after settling and after the active window (not committed) |
| `results/BASELINE.md` | the recorded baseline |
| `HYPOTHESES.md` | what each change is expected to do, written before it is measured, and the outcome |
