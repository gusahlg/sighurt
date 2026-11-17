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

## After the CPU backend's optimizations (4 November 2026, branch `opt2`)

H-CPU1 (painting in the buffer's byte order) and H-CPU2 (glyph rows), bench/HYPOTHESES.md.
Same machine and method as above, release builds of a89465c (`base`) and of `opt2` (`final`),
`bench/render/chrome.sh` with `CHROME_BUILDS`, both builds and all three renderers interleaved,
three runs each, in one gles2 sway; the workload now has a third tab (`demo://resize`) and a
10 s hover phase (pointer motion every 10 ms: a status-line change per motion). Logs:
`/data/sig-builds/test-out/opt2/chrome-final/`.

| renderer | build | churn CPU | hover CPU | RSS | chrome render | page render | status render |
|---|---|---|---|---|---|---|---|
| cpu | base | 3.7 % | 11.4 % | 15.1 MiB | 0.197 ms | 1.987 ms | 0.208 ms |
| cpu | final | 3.4 % | 11.0 % | 15.3 MiB | 0.168 ms | 1.488 ms | 0.160 ms |
| wgpu | base | 4.0 % | 14.7 % | 142.6 MiB | 0.358 ms | 0.593 ms | 0.727 ms |
| wgpu | final | 3.7 % | 14.8 % | 142.7 MiB | 0.352 ms | 0.323 ms | 0.727 ms |
| vulkan | base | 3.9 % | 12.6 % | 138.5 MiB | 0.561 ms | 2.769 ms | 0.392 ms |
| vulkan | final | 3.9 % | 12.4 % | 139.2 MiB | 0.784 ms | 2.550 ms | 0.390 ms |

The GPU renderers' code did not change; their rows show this session's run-to-run spread
(Vulkan's chrome median moved 0.56 → 0.78 ms with the same code). The CPU renderer's per-surface
medians fell 15–25 %, and it is now the fastest of the three on every surface but the
page surface under wgpu (whose native surface keeps its frame across a hide, so most of its
page renders find nothing new to draw). Idle: 0 wakeups and 0 chrome paints for every build.

## Round of 17 November 2026: where the memory goes, beside Servo (branch `r3-render`)

**Machine:** louise-pc: NVIDIA GeForce RTX 4060, driver NVIDIA 595.80, NixOS (13 Vulkan driver
manifests installed: NVIDIA's and 12 of Mesa's), Vulkan loader 1.4.357. Not the desktop of the
measurements above (the machine rules of 17 November keep GPU work off it), so compare rows
only within a table.

**The backends' cost probes** (`render/sighurt-render-{vulkan,wgpu}/tests/cost.rs`, `cargo test
--test cost -- --ignored`, debug test binaries, headless device, the standard chrome's 1280×84
surface rendered 101 times; resident memory, `VmRSS`):

| stage | vulkan, all drivers | wgpu, all drivers | vulkan, NVIDIA's only | wgpu, NVIDIA's only | vulkan, lavapipe only |
|---|---|---|---|---|---|
| loader and instance | 125.6 MiB (181 ms) | 127.2 MiB (194 ms) | 103.0 MiB (116 ms) | 104.4 MiB (116 ms) | 41.7 MiB (14 ms) |
| device | 167.1 MiB (324 ms) | 170.3 MiB (333 ms) | 129.4 MiB (198 ms) | 132.9 MiB (202 ms) | 62.2 MiB (18 ms) |
| first frame | 168.4 MiB | 237.9 MiB¹ | 130.9 MiB | 200.9 MiB¹ | 83.4 MiB |
| threads after the device | 7 | 7 | 7 | 7 | 28 |

¹ wgpu's default memory hints, which the headless test device used then (fixed in e13cdd9: it
now has the backend's `MemoryHints::MemoryUsage`): 80 MB of `/dev/nvidiactl` mappings, against
14 MB for ash. With the backend's hints (final run, all drivers): wgpu 174.7 MiB after its first
frame against vulkan's 165.8 MiB; on lavapipe 88.8 against 82.4 MiB.

- **The instance is most of it.** `vkCreateInstance` loads every installed driver: NVIDIA's
  (libnvidia-gpucomp 24–40 MB resident, eglcore 12, glcore 9, rtcore 9, glvkspirv 3–7) and
  Mesa's (libLLVM 28 MB, gallium 4, the lvp and dzn drivers 4), about 23 MiB more than
  NVIDIA's alone. The device adds 27–43 MiB (driver heaps and `/dev/nvidiactl` mappings). The
  backends' own resources are about 1.4 MB of device memory (the atlas pages at 512², the
  buffers). Nothing of this can be chosen by the application without writing the environment
  (`VK_DRIVER_FILES`; decision 102) or reimplementing the loader's driver discovery
  (`VK_LUNARG_direct_driver_loading`), so neither was done.
- **Beside Servo the cost halves**, because Servo's NVIDIA EGL has loaded most of the same
  libraries already. `bench/render/servo.sh` (debug sig with Servo, the demo engine, wgpu and
  vulkan; gles2 sway; a page animating with CSS and requestAnimationFrame; 3 interleaved runs):

| renderer | animate CPU | animate wakeups | animate chrome paints | churn CPU | churn wakeups | churn chrome paints | RSS |
|---|---|---|---|---|---|---|---|
| cpu | 9.0 % | 2471 | 0 | 8.8 % | 1022 | 52 | 295.9 MiB |
| wgpu | 10.5 % | 2470 | 0 | 7.2 % | 1049 | 52 | 367.3 MiB (+71.4) |
| vulkan | 9.0 % | 2442 | 0 | 8.8 % | 1218 | 52 | 362.6 MiB (+66.7) |

  Of vulkan's +66 MiB (smaps, run 1): NVIDIA's libraries +49 MiB (glcore, rtcore and
  glvkspirv are Vulkan's and not loaded for EGL; gpucomp and eglcore gain pages), the heap +7,
  the debug binary's code +6, Mesa's drivers +4. CPU and wakeups are within run-to-run spread
  (Servo's 60 Hz dominates them). Every run: no renderer failure, fallback or panic, a clean
  quit; with `SIG_VULKAN_VALIDATION=1` no validation message from either GPU renderer.

**The chrome in `sig`, before and after the round** (`bench/render/chrome.sh` with
`CHROME_BUILDS`, release builds of b1da7e3 (`base`) and of this branch (`new`, 4141ee9: present
fences from a pool), the demo engine, 1280×800 at scale 1, gles2 sway, louise-pc, 3
interleaved runs; idle is 10 s on a static page, churn 10 s of tab switches, hover 10 s of
pointer motion):

| renderer | build | idle CPU | idle wakeups | churn CPU | hover CPU | RSS | chrome render median | status render median |
|---|---|---|---|---|---|---|---|---|
| cpu | base | 0.00 % | 0 | 2.3 % | 4.5 % | 15.6 MiB | 0.176 ms | 0.048 ms |
| cpu | new | 0.00 % | 0 | 2.2 % | 6.2 % | 16.2 MiB | 0.197 ms | 0.057 ms |
| wgpu | base | 0.20 % | 0 | 1.9 % | 5.9 % | 176.9 MiB | 0.460 ms | 0.311 ms |
| wgpu | new | 0.20 % | 0 | 1.9 % | 7.5 % | 177.6 MiB | 0.433 ms | 0.338 ms |
| vulkan | base | 0.10 % | 0 | 1.9 % | 5.2 % | 172.9 MiB | 0.279 ms | 0.180 ms |
| vulkan | new | 0.30 % | 0 | 2.8 % | 8.8 % | 173.6 MiB | 0.885 ms | 0.283 ms |

- The idle CPU of the GPU renderers is NVIDIA's `[vkps] Update` thread (0–2 ticks in 10 s,
  per-thread `/proc` probe); the owner loop wakes 0 times for every renderer.
- The Vulkan renderer's chrome render time is bimodal *per run*: a run's median is either
  about 0.27 ms or 0.8–1.4 ms (base: 0.282, 0.267, 0.850 here and 0.267, 0.285, 0.825 in an
  earlier session; new: 1.208, 1.371, 0.263, and 0.806, 0.821, 1.418 before the fence pool).
  The new build was in the slow mode in 5 of 6 runs, base in 2 of 6, which suggested the
  present fences; an A/B says they are not the cause: the final build against the same build
  with present fences off (`fence`/`nofence`, `CHROME_RENDERERS=vulkan`, 6 interleaved runs
  each, same machine an hour later): chrome render median 0.744 against 0.835 ms (per run
  0.29–1.43 against 0.79–1.15), status 0.213 against 0.185 ms, idle 0.20 against 0.25 %,
  RSS 173.7 against 173.8 MiB. The mode follows the machine (other lanes' jobs share it), not
  the build; the fast mode, 0.26–0.29 ms, is what the backend costs. The CPU renderer's hover row (4.5 against 6.2 %) is
  within this session's spread on a machine three other lanes share; its code changed only in
  the glyph cache's restart, which a session this short never reaches.
