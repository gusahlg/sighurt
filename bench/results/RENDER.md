# The chrome's renderers: cpu, wgpu and vulkan

Measurements for docs/rewrite/RENDER.md §12 and the `auto` decision of §14.5 ("`auto` prefers
wgpu only once it is at worst equal in chrome render time, RSS and idle owner-loop wakeups on
this desktop"; §14.13: the Vulkan backend is opt-in and never `auto`'s). **Result: neither
GPU renderer is at worst equal; `auto` stays on the CPU renderer.**

## Method

- **Machine:** tensor-ash (this desktop): Intel Core i5-12400F, NVIDIA GeForce RTX 3070, driver
  NVIDIA 595.104.02, Linux 6.18.57; wgpu 30.0.1 on Vulkan (`InstanceFlags` from a release
  build: no validation), MAILBOX, `Bgra8Unorm`, frame latency 2.
- **Build:** branch `render`, `tools/cargo build --release -p sighurt-browser
  --no-default-features --features engine-demo,render-wgpu,render-vulkan` (dependencies at
  opt-level 3 as always; our crates optimized), copied to
  `/data/sig-builds/test-out/render/sig-release3`. Vulkan through ash 0.38 on the same
  driver, MAILBOX, the driver's 3 swapchain images.
- **Script:** `bench/render/chrome.sh`, run as `tools/heavy tools/headless-sway --renderer gles2
  -- bench/render/chrome.sh SIG OUT 3`: a private sway on the GPU (gles2; wgpu's swapchains
  need a compositor that takes dmabufs and syncobjs), one output 1280×800 at scale 1, `sig`
  with the demo engine and two tabs (`demo://pattern`, `sig://home`), fresh XDG directories per
  run, `SIG_DIAG=1`. Three repetitions, the three renderers interleaved. Per run, after 3 s:
  10 s idle on the pattern tab, then 10 s of tab churn (alt-2 / alt-1 every 250 ms: the tab strip
  repaints and the chrome's page surface that draws `sig://home` is shown and hidden). CPU time
  is utime+stime of the whole process from `/proc` (100 Hz ticks); wakeups and chrome paints are
  sig's counters (the report request's own wakeup subtracted); RSS is `VmRSS`/`VmHWM` after the
  idle phase; render times are SIG_DIAG's `chrome` lines over the whole run (`render_ms` is the
  backend's `render` call, for wgpu acquire, encode, submit and present included).
- **Raw data:** [`render/20261104-chrome-runs-vulkan.txt`](render/20261104-chrome-runs-vulkan.txt)
  (the run lines of the session below); logs and diagnostics in
  `/data/sig-builds/test-out/render/bench3/` on the desktop. An earlier session with cpu and
  wgpu only ([`render/20261104-chrome-runs.txt`](render/20261104-chrome-runs.txt), `bench1/`)
  agrees with it within 0.1 % CPU, 1 MiB and 0.2 ms.

## Results (4 November 2026, medians of 3 runs)

| renderer | idle CPU | idle wakeups | idle chrome paints | churn CPU | churn wakeups | churn chrome paints | RSS | peak RSS |
|---|---|---|---|---|---|---|---|---|
| cpu | 0.00 % | 0 | 0 | 3.2 % | 131 | 60 | 15.1 MiB | 15.1 MiB |
| wgpu | 0.70 % | 0 | 0 | 3.8 % | 191 | 41 | 143.9 MiB | 143.9 MiB |
| vulkan | 0.70 % | 0 | 0 | 3.6 % | 191 | 41 | 138.7 MiB | 138.7 MiB |

Chrome render time per surface (all runs; at scale 1 the chrome surface, tab strip and
toolbar, is 1280×84, the status strip 1280×20, and the page surface drawing `sig://home`
1280×696):

| renderer | surface | renders | median | p95 | max | scene (median) |
|---|---|---|---|---|---|---|
| cpu | chrome | 123 | 0.181 ms | 0.608 ms | 0.698 ms | 0.009 ms |
| cpu | page | 60 | 1.425 ms | 4.682 ms | 5.211 ms | 0.007 ms |
| cpu | status | 3 | 0.047 ms | 0.054 ms | 0.054 ms | 0.001 ms |
| wgpu | chrome | 123 | 0.326 ms | 1.004 ms | 3.562 ms | 0.009 ms |
| wgpu | page | 3 | 10.905 ms | 12.474 ms | 12.474 ms | 0.418 ms |
| wgpu | status | 3 | 2.811 ms | 3.004 ms | 3.004 ms | 0.001 ms |
| vulkan | chrome | 123 | 0.212 ms | 0.963 ms | 3.613 ms | 0.009 ms |
| vulkan | page | 3 | 14.906 ms | 15.027 ms | 15.027 ms | 0.465 ms |
| vulkan | status | 3 | 2.837 ms | 2.941 ms | 2.941 ms | 0.002 ms |

From the diagnostics of the GPU runs:
- **wgpu:** first bind (adapter, device, surface, capability checks) 92–209 ms; a swapchain
  configure 9–10 ms; 4 configures a run; its own device resources 1.38 MB (atlas pages
  1.31 MB at 512², buffers, uniform); 19 of 62 renders a run `Unchanged`; no `Deferred`.
- **vulkan:** first bind (physical device, device, pipeline, surface) 83–84 ms; a swapchain
  9.4–10.4 ms; 4 swapchains a run, each of the driver's **3 images** (the minimum, 2, plus
  one); its own device memory 1.39–1.40 MB (the same atlas pages, buffers, staging, uniform;
  swapchain images not included, which Vulkan doesn't report: 3 × 1280×84 × 4 B = 1.3 MB for
  the chrome surface); 19 of 62 renders `Unchanged`; no `Deferred`.

## Reading

- **Render time:** both GPU renderers are slower per call (chrome surface median: cpu 0.18,
  vulkan 0.21, wgpu 0.33 ms; a surface's first frame 3–15 ms with its swapchain). It renders the page surface less often (3 against
  60 renders over the churn) because a native surface keeps its last frame across a hide
  (§15), so showing it again at the same size is `Unchanged`; the CPU renderer redraws it on
  every show since the host detaches a hidden SHM surface's buffer. That is why its chrome
  paints are fewer.
- **Memory:** +129 MiB RSS for wgpu's Vulkan device and +124 MiB for ash's (the measurement of
  §15, +117 MB, inside `sig` now): the cost is the NVIDIA driver's, not wgpu's. The chrome's
  own GPU resources are about 1.4 MB under either.
- **Idle:** no owner-loop wakeups or chrome paints for any; with either GPU renderer the
  process uses 0.6–0.7 % of a core while idle in a thread that is not the owner loop (a driver
  thread: 3–17 ticks in 10 s in a per-thread probe of wgpu), against 0 for cpu.
- **Churn wakeups:** +60 for both GPU renderers: the Vulkan WSI's Wayland events (buffer releases,
  presentation) arrive on the connection the owner loop polls, so the loop wakes for them too.
- So §14.5's criterion fails on render time, RSS and idle CPU (idle wakeups are equal):
  `auto` stays `cpu`; wgpu and vulkan are used where `[ui] renderer` names them. Between the
  two GPU renderers, ash is a little faster per frame (0.21 against 0.33 ms) and 5 MiB lighter.

## Not measured

Mesa's Vulkan WSI (another machine), a real output instead of the headless one, `sig` with
Servo running beside the chrome's device (how much of the +129 MiB the NVIDIA GL driver Servo
already loads shares), debug builds (validation on: render ~10 ms for a first frame, about the
same steady).
