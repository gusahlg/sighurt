# The chrome's renderers: cpu against wgpu

Measurements for docs/rewrite/RENDER.md §12 and the `auto` decision of §14.5 ("`auto` prefers
wgpu only once it is at worst equal in chrome render time, RSS and idle owner-loop wakeups on
this desktop"). **Result: wgpu is not at worst equal; `auto` stays on the CPU renderer.**

## Method

- **Machine:** tensor-ash (this desktop): Intel Core i5-12400F, NVIDIA GeForce RTX 3070, driver
  NVIDIA 595.104.02, Linux 6.18.57; wgpu 30.0.1 on Vulkan (`InstanceFlags` from a release
  build: no validation), MAILBOX, `Bgra8Unorm`, frame latency 2.
- **Build:** branch `render` (lane 2), `tools/cargo build --release -p sighurt-browser
  --no-default-features --features engine-demo,render-wgpu` (dependencies at opt-level 3 as
  always; our crates optimized), copied to `/data/sig-builds/test-out/render/sig-release`.
- **Script:** `bench/render/chrome.sh`, run as `tools/heavy tools/headless-sway --renderer gles2
  -- bench/render/chrome.sh SIG OUT 3`: a private sway on the GPU (gles2; wgpu's swapchains
  need a compositor that takes dmabufs and syncobjs), one output 1280×800 at scale 1, `sig`
  with the demo engine and two tabs (`demo://pattern`, `sig://home`), fresh XDG directories per
  run, `SIG_DIAG=1`. Three repetitions, the two renderers interleaved. Per run, after 3 s:
  10 s idle on the pattern tab, then 10 s of tab churn (alt-2 / alt-1 every 250 ms: the tab strip
  repaints and the chrome's page surface that draws `sig://home` is shown and hidden). CPU time
  is utime+stime of the whole process from `/proc` (100 Hz ticks); wakeups and chrome paints are
  sig's counters (the report request's own wakeup subtracted); RSS is `VmRSS`/`VmHWM` after the
  idle phase; render times are SIG_DIAG's `chrome` lines over the whole run (`render_ms` is the
  backend's `render` call, for wgpu acquire, encode, submit and present included).
- **Raw data:** [`render/20261104-chrome-runs.txt`](render/20261104-chrome-runs.txt) (the run
  lines); logs and diagnostics in `/data/sig-builds/test-out/render/bench1/` on the desktop.

## Results (4 November 2026, medians of 3 runs)

| renderer | idle CPU | idle wakeups | idle chrome paints | churn CPU | churn wakeups | churn chrome paints | RSS | peak RSS |
|---|---|---|---|---|---|---|---|---|
| cpu | 0.00 % | 0 | 0 | 3.3 % | 132 | 60 | 14.6 MiB | 14.8 MiB |
| wgpu | 0.60 % | 0 | 0 | 3.7 % | 192 | 41 | 143.9 MiB | 143.9 MiB |

Chrome render time per surface (all runs; at scale 1 the chrome surface, tab strip and
toolbar, is 1280×84, the status strip 1280×20, and the page surface drawing `sig://home`
1280×696):

| renderer | surface | renders | median | p95 | max | scene (median) |
|---|---|---|---|---|---|---|
| cpu | chrome | 123 | 0.187 ms | 0.601 ms | 0.697 ms | 0.009 ms |
| cpu | page | 60 | 1.423 ms | 4.422 ms | 5.076 ms | 0.009 ms |
| cpu | status | 3 | 0.046 ms | 0.057 ms | 0.057 ms | 0.001 ms |
| wgpu | chrome | 123 | 0.328 ms | 0.997 ms | 3.531 ms | 0.010 ms |
| wgpu | page | 3 | 10.689 ms | 10.823 ms | 10.823 ms | 0.415 ms |
| wgpu | status | 3 | 2.758 ms | 3.156 ms | 3.156 ms | 0.001 ms |

From the diagnostics of the wgpu runs: the first bind (instance already made: adapter, device,
surface, capability checks) 92–209 ms; a swapchain configure 9–10 ms; 4 configures a run;
the backend's own device resources 1.38 MB (atlas pages 1.31 MB at 512², buffers, uniform);
19 of 62 renders a run `Unchanged`; no `Deferred` render.

## Reading

- **Render time:** wgpu is slower per call (chrome surface median 0.33 against 0.19 ms; its first frame
  on a surface 3–11 ms with the configure). It renders the page surface less often (3 against
  60 renders over the churn) because a native surface keeps its last frame across a hide
  (§15), so showing it again at the same size is `Unchanged`; the CPU renderer redraws it on
  every show since the host detaches a hidden SHM surface's buffer. That is why its chrome
  paints are fewer.
- **Memory:** +129 MiB RSS for the Vulkan device (the measurement of §15, +117 MB, inside `sig`
  now). The chrome's own GPU resources are about 1.4 MB.
- **Idle:** no owner-loop wakeups or chrome paints for either; wgpu's process uses 0.3–1.7 % of
  a core while idle in a thread that is not the owner loop (a driver thread: 3–17 ticks in 10 s
  in a per-thread probe), against 0 for cpu.
- **Churn wakeups:** +60 for wgpu: the Vulkan WSI's Wayland events (buffer releases,
  presentation) arrive on the connection the owner loop polls, so the loop wakes for them too.
- So §14.5's criterion fails on render time, RSS and idle CPU (wakeups are equal): `auto` stays
  `cpu`, and wgpu is used where `[ui] renderer = "wgpu"` asks for it.

## Not measured

Mesa's Vulkan WSI (another machine), a real output instead of the headless one, `sig` with
Servo running beside the chrome's device (how much of the +129 MiB the NVIDIA GL driver Servo
already loads shares), debug builds (validation on: render ~10 ms for a first frame, about the
same steady), the Vulkan (ash) backend (lane 3).
