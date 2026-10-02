# WPE WebKit for Sighurt: the build

WPE WebKit is the engine for short-term web compatibility; Servo stays the long-term engine.
nixpkgs has libwpe and wpebackend-fdo but no WPE WebKit, so it is built from its release
tarball. This file says how and why, what it costs, how a program finds it, and how it was
checked. The engine adapter is separate work; what it needs from this build is under "Using the
prefix".

```sh
# Build on louise-pc (1 h 22 min), as a user service that outlives the ssh session:
tools/remote true    # sync the tree
ssh louise-pc 'systemd-run --user --unit=sighurt-wpewebkit-build --collect \
  --working-directory=$HOME/repos/sighurt-rewrite \
  --setenv=SIG_MEM=13G --setenv=SIG_JOBS=6 "--setenv=SIG_WHAT=wpewebkit build" \
  bash -lc "exec tools/heavy nix/build-wpewebkit.sh"'
ssh louise-pc journalctl --user -f    # its output (under tools/heavy's scope, not the unit)
# Copy it to this machine; fetch and root the store paths it uses; check it:
nix/build-wpewebkit.sh --pull louise-pc
# Smoke test, in a private compositor:
tools/heavy tools/headless-sway --renderer auto -- nix/wpe-smoke.sh
```

A build that stops (killed, out of memory, a fix to a flag) resumes where it was when the script
runs again: the source and build tree stay in `~/.cache/sighurt/wpewebkit-2.52.6`.

## Pinned nixpkgs

`nix/nixpkgs.nix` pins nixpkgs (plan §21) to revision
`4975466d324710c576dc11ad614684e6bd8cad8e` (narHash
`sha256-xJ+X4hBtOcAFGBOe5nAMyMUeF9foJBmIOu3NjBqBycU=`), with no user configuration or overlays.
`shell.nix` and `nix/wpewebkit.nix` import it, so every build uses it. Test helpers still take
their tools (sway, grim, wl-clipboard, lswt) from the machine's `<nixpkgs>`: `tools/headless-sway`,
`sighurt-wayland/tests/headless.sh`, `sighurt-browser/tests/engines.sh`,
`sighurt-engine-servo/tests/window-checks.sh`, `bench/run`, `experiments/m1-mixed/live-check.sh`
(open problem 1).

That revision is the `sighurt` desktop's system flake input of 2 October 2026 (Rust 1.98.1,
WebKitGTK 2.52.6). `fetchTarball` names the tarball `source` like a flake input, so the pin is
the very store path the desktop already had (`/nix/store/398fqjqkp383m7pyla9nxpi1is5vzywh-source`)
and nothing was downloaded or rebuilt there. Checked on both machines:

| | `sighurt` (desktop) | `louise-pc` |
|---|---|---|
| `rustc.drvPath` through the pin | `9f6ax144…-rustc-wrapper-1.98.1.drv` | the same |
| `rustc.drvPath` through `<nixpkgs>` | the same | `fjx6b96c…-rustc-wrapper-1.95.0.drv` (9ae611a) |
| `nix-instantiate shell.nix` without the WPE prefix | `5g4i70pv…-nix-shell.drv` before and after the change | the same |

The shell derivation records every input and variable of the shell, so the desktop's development
environment stayed bit-identical. louise-pc moved from its own nixpkgs (9ae611a, Rust 1.95) to the
pinned one: its Servo build in `~/repos/sighurt-rewrite/target` is redone once with Rust 1.98.1,
and no machine compiles with the workspace's `rust-version` (1.95) any more (open problem 2).

To move the pin: change `rev`, put `nix-prefetch-url --unpack <url>` in `sha256`, and expect
Servo to rebuild on both machines and WPE WebKit to need a rebuild (its prefix refers to store
paths of this revision).

## Version and source

WPE WebKit **2.52.6**, the WebKit release of the pinned nixpkgs' `webkitgtk_6_0`: the same
WebKit, so nixpkgs' inputs and its sandbox patch for WebKitGTK apply as they are.

- `https://wpewebkit.org/releases/wpewebkit-2.52.6.tar.xz`, 62.5 MiB, released 18 August 2026.
- sha256 `b2bafef2751625b7fdf530f230ff0f542ff0eeba3590c3a989d931b2a55c858e`, as in
  `wpewebkit-2.52.6.tar.xz.sums`; `nix/wpewebkit.nix` fetches it with `fetchurl` and that hash.
- The `.asc` signature is good, by Adrián Pérez de Castro's DSA key
  `5AA3 BC33 4FD7 E336 9E7C 77B2 91C5 59DB E4C9 123B`, one of the two release keys listed on
  wpewebkit.org/release/verify/. The copy of the key on that page says it expired on 14 April
  2026, before the signature (19 August 2026); the copy on keyserver.ubuntu.com carries the
  extended expiry (16 March 2027), and with it gpg reports a good signature. Checked once on 2
  October 2026 in a scratch keyring; the build checks the sha256.

## Why a plain prefix, not a Nix package

The obvious build is a Nix derivation (nixpkgs' `webkitgtk_6_0` with `-DPORT=WPE`). Its output
would be a store path only the building machine has: `nix copy` to the other machine needs a
trusted user or a signing key the other machine trusts, only root is a trusted user on both, and
changing that is the user's decision. Building the derivation on each machine costs the build
twice.

So WPE WebKit is built once, from a `nix-shell --pure` on the pinned nixpkgs, into a plain
directory with **the same absolute path on both machines**:

```
~/.local/share/sighurt/deps/wpewebkit-2.52.6               the prefix (170 MB)
~/.local/share/sighurt/deps/wpewebkit-2.52.6.nix-runtime   GC root of the store paths it uses
~/.cache/sighurt/wpewebkit-2.52.6/{src,build}              source and build tree (louise-pc only)
```

Every absolute path in the prefix is either inside the prefix (its own RPATH, and the paths
WebKit compiles in: `libexec/wpe-webkit-2.0/WPEWebProcess`, `WPENetworkProcess` and
`WPEGPUProcess`, the injected bundle, WPEPlatform's module directory, the sandbox's binds of
`lib`, `libexec` and `share`) or a store path of the pinned nixpkgs (the RPATHs, `bwrap`,
`xdg-dbus-proxy`), which every machine fetches from cache.nixos.org. So a plain `rsync -a` of the
prefix is a working copy (`--pull` does it into a temporary directory and swaps it in), as long
as the home directory is the same path; it is `/home/gusahlg` on both machines. After the pull,
a manifest of the prefix (mode and sha256 of every file, every symlink and its target, every
directory: 190 entries) hashed to the same value on both machines.

A plain directory has no references Nix can see, so a garbage collection would delete the store
paths it uses. `nix/wpewebkit.nix` has a `runtime` derivation, a text file listing the library,
binary and development outputs of every input, linked or loaded at run time (including GCC's
libstdc++ and the GStreamer plugins): 380 store paths, 0.98 GiB. `nix/build-wpewebkit.sh` builds
it with `nix-build -o <prefix>.nix-runtime`, which registers it as a GC root and, on a machine
that only pulled the prefix, fetches it all. It then greps every `/nix/store/…` path out of the
prefix (43 distinct paths) and fails if one is outside that root's closure. Paths whose hash is
all `e` are skipped: nixpkgs' compiler wrapper rewrites store paths in `__FILE__` to that
placeholder (libstdc++ assertions, GStreamer macros), and no such path exists. The runtime root
is the same store path on both machines (`x4lrwcdk…-wpewebkit-2.52.6-runtime`).

## Build environment and flags

`nix/wpewebkit.nix`, attribute `shell`: `mkShell` on `clangStdenv` (Clang 21.1.8; nixpkgs builds
WebKitGTK with clang, as WebKit recommends), entered with `--pure`.

- **Inputs** are nixpkgs' `webkitgtk_6_0` inputs without the GTK port's own (GTK, X11, cairo,
  libGLU, enchant, libsecret, geoclue) and the ones the WPE port doesn't look for (gnutls, nettle,
  p11-kit, libintl, libpthread-stubs), plus `libwpe` 1.16.3 (the libwpe API), `libwpe-fdo` 1.16.1 (wpebackend-fdo, for the MiniBrowser's
  legacy backends), `libinput`, udev and `libdrm`/`libgbm` (the DRM platform, GBM buffers),
  `wayland` and `wayland-protocols` (the Wayland platform), and GStreamer 1.28.6 core, base and
  bad. `woff2` is in the list but unused: FreeType 2.14 has WOFF2, so the configure step turns
  `USE_WOFF2` off.
- **`NIX_NO_SELF_RPATH`**, so the linker wrapper doesn't add the shell's own (nonexistent)
  output to every RPATH. The wrapper does add the store directory of every library linked, which
  is what makes the prefix work without `LD_LIBRARY_PATH`. WebKit links with
  `--disable-new-dtags`, so these are `DT_RPATH`: `<prefix>/lib` first, then the store paths (42
  entries for `libWPEWebKit-2.0.so.1`).
- **`NIX_GOBJECT_INTROSPECTION_DEFAULT_FALLBACK_LIBPATH=<prefix>`**: nixpkgs' g-ir-scanner writes
  the library into the `.gir` and `.typelib` files as an absolute path under this directory, which
  the setup hook sets to the shell's nonexistent output. The first build had
  `shared-library="/nix/store/373x…-nix-shell/lib/libWPEWebKit-2.0.so.1"`; the store-path check
  caught it, and the GIR files were regenerated with the variable set.
- **The sandbox patch**: `sandboxPatch` is nixpkgs' `fix-bubblewrap-paths.patch` for WebKitGTK
  with its variables filled in (it applies to WPE 2.52.6 at an offset of 142 lines). Without it
  the sandboxed web process sees neither `/nix/store`, `/run/current-system` nor
  `/run/opengl-driver`.
- No shebangs are patched: the build's scripts use `/usr/bin/env` or `/bin/sh`, which NixOS has.
- Configuring prints "Package 'libpcre2-8', required by 'glib-2.0', not found" (and the same for
  `xcb` through `x11`) many times. These are private requirements nixpkgs doesn't propagate;
  every package WebKit looks for is found, as in nixpkgs' own WebKitGTK build.

CMake (`nix/build-wpewebkit.sh`):

| Flag | Why |
|---|---|
| `-DPORT=WPE -DCMAKE_BUILD_TYPE=Release` | no LTO (WebKit's default), no debug info |
| `-DCMAKE_INSTALL_PREFIX=<prefix>`, `LIBDIR=lib`, `LIBEXECDIR=libexec`, `-DCMAKE_INSTALL_RPATH=<prefix>/lib` | the prefix layout; installed programs find `libWPEWebKit-2.0.so.1` |
| `-DENABLE_WPE_PLATFORM=ON` with `_WAYLAND`, `_DRM`, `_HEADLESS` | the WPE Platform API (WPEPlatform, built into `libWPEWebKit-2.0`); off by default outside developer builds |
| `-DENABLE_WPE_LEGACY_API=ON` | keeps the libwpe API (`WebKitWebViewBackend`) |
| `-DENABLE_WPE_QT_API=OFF -DENABLE_DOCUMENTATION=OFF` | no Qt, no docs |
| `-DENABLE_INTROSPECTION=ON` | `.gir` and `.typelib` files for WPEWebKit, WPEPlatform (and its Wayland, DRM and headless parts), WPEJavaScriptCore and WPEWebProcessExtension: exact signatures for the adapter's bindings |
| `-DENABLE_MINIBROWSER=ON` | `libexec/wpe-webkit-2.0/MiniBrowser`, for trying pages by hand |
| `-DENABLE_BUBBLEWRAP_SANDBOX=ON`, `-DBWRAP_EXECUTABLE`, `-DDBUS_PROXY_EXECUTABLE` | the web process runs in bubblewrap; both programs are compiled in by store path |
| `WEBKIT_NINJA_LINK_MAX=1` (environment, read at configure time) | one link at a time |

Everything else is the WPE port's default (`OptionsWPE.cmake`). The configure summary: Skia,
GStreamer (video, Web Audio; MPEG-TS, GL; not WebRTC, which is experimental), JPEG XL, AVIF,
LCMS, libhyphen, Flite speech synthesis, ATK/AT-SPI accessibility, journald logging,
libbacktrace, gamepads (libmanette), WebDriver (`bin/WPEWebDriver`), PDF.js, XSLT.

## Cost

On louise-pc (12 cores, 15 GB), 2 October 2026, under `tools/heavy` with a 13 GiB memory cap:

| | |
|---|---|
| Entering both shells, fetching the tarball, unpacking, patching | 30 s (09:05:50–09:06:20; louise-pc had most of the build environment) |
| Configure and generate | 56 s (46.9 s + 8.9 s) |
| Build, 9 438 ninja steps, `-j6` | 1 h 20 min 56 s (09:07:16–10:28:12) |
| Install, root, check | 4 s |
| Whole run | 1 h 22 min 26 s wall clock, 8 h 6 min of CPU |
| Peak memory (the scope's cgroup, page cache included) | 12.4 GiB; anonymous memory was 7.2 GB in a WebCore-heavy sample |
| Source tree / build tree / prefix | 483 MB / 888 MB / 170 MB |
| Runtime closure | 380 store paths, 0.98 GiB (all already present on the desktop) |
| Pull to the desktop (rsync, root, check) | 4 s |

The jobs were held at 6 because WebKit's unified sources take 1.5–2 GB each: during WebCore the
cgroup peak rose from 6 GB to 12.4 GiB, so 8 jobs could have reached the cap. Re-running the
script after the GIR fix took 36 s, 13 s of it ninja's 28 steps.

## Using the prefix

`shell.nix` looks for `<prefix>/lib/pkgconfig/wpe-webkit-2.0.pc`. Without it the shell is exactly
what it was. With it, the shell adds (`nix/wpewebkit.nix`, attribute `env`):

| Variable | Value | For |
|---|---|---|
| `SIGHURT_WPE_PREFIX` | the prefix | finding it |
| `SIGHURT_WPE_PKG_CONFIG_PATH` | `<prefix>/lib/pkgconfig` and the pkg-config directories of glib, libsoup 3 and libwpe with what they propagate | compiling against `wpe-webkit-2.0`, `wpe-platform-2.0`, `wpe-platform-{wayland,headless,drm}-2.0`, `wpe-web-process-extension-2.0` |
| `LD_LIBRARY_PATH` | `:<prefix>/lib` appended | running a program linked to `libWPEWebKit-2.0.so.1` (or the program carries an RPATH) |
| `GIO_EXTRA_MODULES` | glib-networking's GIO modules | TLS: libsoup in the network process needs a GIO TLS backend |
| `GST_PLUGIN_SYSTEM_PATH_1_0` | GStreamer core, base, good, bad and libav plugins | media; the sandbox binds these directories into the web process |

**Why not `PKG_CONFIG_PATH`.** Cargo reruns a build script when a variable it declared with
`rerun-if-env-changed` changes, and rebuilds everything that depends on that crate. In this
workspace's `target/debug`, 16 build-script runs of 10 crates declare `PKG_CONFIG_PATH` (through
the `pkg-config` crate): `wayland-sys`, `smithay-client-toolkit`, `yeslogic-fontconfig-sys`,
`x11-dl`, `libz-sys`, `libudev-sys`, `alsa-sys`, `ffmpeg-sys-next`, `openssl-sys` and
`aws-lc-sys`. Changing it, or adding a package to the shell's `buildInputs` (whose setup hooks
extend it), would rebuild most of Servo and the Wasm engine on every machine. None of the
variables in the table is declared by a build script in `target/debug` or `target/release`, and
none is in an `# env-dep:` line of rustc's dependency files. Checked with cargo itself on the
desktop: `cargo build -v -p sighurt-wayland -p sighurt-engine-wasm` (which builds six of the ten:
`wayland-sys`, `smithay-client-toolkit`, `alsa-sys`, `ffmpeg-sys-next`, `libudev-sys` and
`openssl-sys`; the other four declare the same variables) reported 549 units `Fresh` both with
the four variables exported by hand and then in the real shell once the prefix was there. The WPE engine's build script should run
pkg-config with `PKG_CONFIG_PATH=$SIGHURT_WPE_PKG_CONFIG_PATH` and declare
`cargo:rerun-if-env-changed=SIGHURT_WPE_PKG_CONFIG_PATH`; nothing else sees it.

Once the prefix exists, the shell's derivation changes, and with it `out`, `NIX_LDFLAGS`'s
self-RPATH and `-frandom-seed` in `NIX_CFLAGS_COMPILE`; no build script tracks those either (the
`Fresh` run above was in that shell).

**What WebKit finds by itself.** Its helper processes, the injected bundle and WPEPlatform's
module directory are compiled in as absolute paths under the prefix; `bwrap` and `xdg-dbus-proxy`
as store paths. No variable is needed for them. The GL driver comes from `/run/opengl-driver`
through libglvnd and Mesa's GBM, as for every Nix program on NixOS.

**Sandbox** (`BubblewrapLauncher.cpp` with the patch). The web process runs in bubblewrap with
its own network, PID and UTS namespaces, a private `/tmp`, a seccomp filter, and read-only binds
of `/etc`, `/nix/store`, `/run/current-system`, `/run/opengl-driver/{lib,share}` and the
prefix's `lib`, `libexec/wpe-webkit-2.0` and `share`; device binds of `/dev/dri`,
`/dev/nvidiactl`, `/dev/nvidia0`, `/dev/udmabuf` and V4L devices; the Wayland socket, the
PulseAudio socket, fonts and GStreamer's plugin directories; the session and accessibility buses
only through xdg-dbus-proxy. The network process is not sandboxed (WebKit's choice). This is
WebKit's sandbox for its own web process; it says nothing about Sighurt's in-process engines
(CLAUDE.md, "Security").

## Smoke test

`nix/wpe-smoke.c` loads one page with the WPE Platform API (a `WebKitWebView` on a `WPEDisplay`:
headless, or the default display, which is Wayland under a compositor) and prints one JSON
report: what the page says about itself (title, WebGL renderer, whether GStreamer can play H.264,
VP9 and Opus; on `webkit://gpu`, the GL renderer, the platform and how frames reach the UI
process) and what `webkit_web_view_get_snapshot` shows (size, centre pixel). `nix/wpe-smoke.sh`
compiles it with `cc -Wall -Werror` and only `SIGHURT_WPE_PKG_CONFIG_PATH`, runs it on a private
D-Bus session without service activation (so nothing reaches the user's session and no portal is
started), and checks 22 things: headless, a `data:` page loads, has its title, is painted red,
has WebGL in the web process, can play the three media types, and runs its web process under
`bwrap`; `https://example.com` loads (TLS through glib-networking) with its title; `webkit://gpu`
names a GL renderer. Under a compositor, the same for Wayland, plus that the compositor shows the
page (its centre pixel, read from `grim`, matches the snapshot's), with screenshots.

Results, `tools/heavy tools/headless-sway --renderer auto -- nix/wpe-smoke.sh` (sway with its
GLES2 renderer, 1280 × 800, scale 1), 2 October 2026:

| | `sighurt` (desktop) | `louise-pc` |
|---|---|---|
| Checks | 22 of 22 PASS | 22 of 22 PASS |
| `webkit://gpu` GL renderer | NVIDIA GeForce RTX 3070/PCIe/SSE2 | NVIDIA GeForce RTX 4060/PCIe/SSE2 |
| Platform, frames to the UI process | GBM; DMABuf (hardware and shared-memory buffers supported) | the same |
| Media | H.264, VP9, Opus: "probably" | the same |
| Compositor's centre pixel, red page / example.com / `webkit://gpu` | `#ff0000` / `#eeeeee` / `#f6f6f4`, as in the snapshots | the same |

Warnings in the logs, none of them failures: the web process finds no accessibility bus (the
private session bus has none), and on Wayland the WPE platform finds no cursor theme in the
private compositor's environment. WebGL's `UNMASKED_RENDERER_WEBGL` reads "Apple GPU": WebKit
masks it against fingerprinting, which is why the test reads the renderer from `webkit://gpu`
(that one is the UI process's GL context; the web process renders on the same GBM device). The
two machines render example.com in different fonts: WebKit uses each machine's `/etc/fonts` and
installed fonts, which differ.

## Open problems

1. The test helpers listed under "Pinned nixpkgs" take sway, grim and the rest from the
   machine's `<nixpkgs>` (on louise-pc a different revision than the pin). They aren't part of
   this work; importing `nix/nixpkgs.nix` would pin them too.
2. No machine compiles with Rust 1.95 any more: louise-pc did through its own nixpkgs, and
   CLAUDE.md says it does. The workspace's `rust-version` (1.95) goes untested until a 1.95
   toolchain is pinned for that check or `rust-version` is raised.
3. Untested: the DRM platform (it needs a free DRM master: a VT without a compositor), the
   MiniBrowser, WebDriver, audio output, and a run on the user's River session.
4. The prefix depends on the home directory being the same path on both machines. A machine with
   another user name needs its own build (or a prefix outside `$HOME`, chosen before building).

# WPE WebKit as an in-process engine: the proof

The WPE equivalent of milestones M0 and M1 (plan §23, with §9, §10, §14 by analogy, §18, §19 and
§27): can WPE WebKit be an in-process Sighurt engine, presenting on a host-owned subsurface,
beside Servo and Wasmtime, inside `sig`'s owner loop? Two experiments answer it, both on the real
host (`sighurt-wayland`) and the real engine contract (`EngineRuntime`):

- **`experiments/wpe-engine`**, the prototype of `sighurt-engine-wpe`: `WpeRuntime` implements
  `EngineRuntime` with the host's target leases and frame permits; `wpe-window` is a window with
  a CPU chrome strip and WPE views, like M1's experiment; `window-checks.sh` drives it in a
  private compositor (41 checks with DMA-BUF, 40 with shared memory).
- **`experiments/wpe-coexist`**: Servo 0.5 (through `sighurt-engine-servo`), the Wasm engine
  (`sighurt-engine-wasm`, Wasmtime 36 and every native library it links) and the WPE prototype in
  one executable, mounted on one host and one owner loop as `sig` mounts its engines, side by side
  in one window; `coexist-checks.sh` adds what the executable exports and what binds to it.

```sh
tools/cargo build -p wpe-engine
tools/cargo build -p sighurt-browser --no-default-features --example virtual-pointer
tools/heavy tools/headless-sway --renderer gles2  -- experiments/wpe-engine/window-checks.sh OUT
tools/heavy tools/headless-sway --renderer pixman -- experiments/wpe-engine/window-checks.sh OUT
tools/heavy tools/headless-sway --renderer gles2  -- experiments/wpe-engine/window-checks.sh OUT --shm
tools/cargo build -p wpe-coexist     # links Servo: build it where Servo is built
tools/heavy tools/headless-sway --renderer gles2  -- experiments/wpe-coexist/coexist-checks.sh OUT
```

**Result: go**, on every question, with two fixes that `sig` needs anyway (below: the executable
must not export the C libraries linked into it, and JavaScriptCore in `sig`'s process must stay
off signals). Run outputs are in `experiments/wpe-engine/runs/` and `experiments/wpe-coexist/runs/`.

| Question | Answer | Evidence |
|---|---|---|
| 1. Embedding API | **WPE Platform API**, with Sighurt's own `WPEDisplay` and `WPEView` (GObject subclasses in Rust, glib-rs) | All checks below; 66 C functions, 2 subclasses, no libwpe loader, no WPEBackend-fdo, no EGL in Sighurt's process |
| 2. Coexistence with Servo 0.5 and Wasmtime 36 | **Go**, with `--exclude-libs ALL` on the executable and JavaScriptCore's signal handlers kept out of `sig`'s process; a crashed web process is a view failure | `wpe-coexist`: 14 of 14 on the desktop in headless sway, 12 of 12 live on River; the clashes reproduced and fixed |
| 3. GLib in the calloop owner loop | **Go**: GLib's iteration split into owner-side halves, a watch thread that only `poll`s and wakes the owner through `EngineWaker` | Unit test; 1 loop wakeup (the probe's own) and 0 GLib dispatches in 5 s idle, on every machine |
| 4. Presentation on a host-owned subsurface | **Go**: WebKit's DMA-BUFs attached as `wl_buffer`s with no CPU copy; shared memory with one copy where DMA-BUF is unavailable, counted in diagnostics | 0 copies, 4 imports for a whole animation; 60 Hz and 120 Hz pacing; releases follow frames |
| 5. Input | **Go**: pointer, wheel, keys (keysym, hardware keycode, modifiers; å ä ö and a dead key), focus | `window-checks.sh` input checks on both machines |

## The machines

| | Desktop `sighurt` | `louise-pc` |
|---|---|---|
| GPU, driver | NVIDIA RTX 3070, 595.99.02 | NVIDIA RTX 4060, 595.80 |
| Private compositor | sway 1.12, GLES2 or pixman, one 1280 × 800 output at 60 Hz | the same |
| Live compositor | River 0.4.8 with gharial, 1542 × 1408 window, 120 Hz, scale 1 | — |
| WPE WebKit | 2.52.6 from the prefix above (Release) | the same prefix |
| Experiments | debug builds, dependencies at `opt-level = 3` | the same |

`wpe-coexist` ran on the desktop only (headless, and live on River): it links Servo, which
`louise-pc` must rebuild once with the pinned Rust 1.98.1 (decision 59), and that build would hold
its lock for a long time.

## 1. The embedding API

WebKit 2.52 offers two ways to embed WPE:

- **The WPE Platform API** (`wpe-platform-2.0`, built into `libWPEWebKit-2.0`): the embedder
  subclasses `WPEDisplay` and `WPEView`. WebKit's UI process creates a `WPEView` per web view
  through the display, and hands each finished frame to `WPEViewClass.render_buffer` as a
  `WPEBuffer`: a `WPEBufferDMABuf` (planes' fds, offsets, strides, fourcc, modifier) or a
  `WPEBufferSHM` (bytes, stride, ARGB8888). The embedder answers with `wpe_view_buffer_rendered`
  (WebKit may render the next frame) and `wpe_view_buffer_released` (it may reuse the buffer). The
  display advertises the DMA-BUF formats and the DRM device (`get_preferred_buffer_formats`,
  `get_drm_device`), which reach the web process. Input goes in as `WPEEvent`s.
- **libwpe with WPEBackend-fdo's exportable backends**: a libwpe backend loaded with
  `wpe_loader_init`, a nested Wayland compositor (libwayland-server) inside the UI process that
  the web process presents to, and frames exported as EGL images
  (`wpe_fdo_initialize_for_egl_display`, which needs an `EGLDisplay` in Sighurt's process), an
  EGLStream, a DMA-BUF pool or SHM buffers; the DMA-BUF and SHM entry points are under
  `wpe-fdo-1.0/wpe/unstable/` in WPEBackend-fdo 1.16.1. Input goes through libwpe's own structs.

**Chosen: the WPE Platform API.**

- *Supported long term.* In 2.52 the libwpe API is the "legacy API": since 2.51.4 a build can
  leave it out (`ENABLE_WPE_LEGACY_API`); the MiniBrowser has defaulted to WPEPlatform since
  2.49.2; every new embedding feature of the last releases (clipboard, gamepads, screens,
  rendering fences, toplevels, buffer reconfiguration) went into WPEPlatform (WebKit's `NEWS`).
  Caveat: `ENABLE_WPE_PLATFORM` is still off by default outside developer builds in 2.52
  (`OptionsWPE.cmake`), and the API grows between releases (`WPEMonitor` became `WPEScreen` in
  2.47; toplevel creation came in 2.51.90). Sighurt builds WebKit itself (decision 60), so the
  default doesn't matter, but each WebKit update needs the adapter rechecked against the headers.
- *Robust.* The buffer hand-off is exactly the host's model: WebKit's frames arrive as plain
  buffers on the owner thread, `buffer_rendered` and `buffer_released` map one to one onto the
  host's frame permits and `wl_buffer.release`, and WebKit renders in the formats Sighurt read
  from the compositor (`zwp_linux_dmabuf_v1` feedback), so the compositor can always import them.
  There is no nested compositor and no EGL context in Sighurt's process.
- *Smallest.* The prototype calls 66 C functions (`ffi.rs`), subclasses two GObject types and uses
  WebKit's stock `WPEToplevel`; WPEBackend-fdo is not needed at all.

The two subclasses (`platform.rs`) are written with glib-rs: `WpeDisplay` and `WpeView` wrap
WebKit's abstract classes (`glib::wrapper!` with the C class structs mirrored in `ffi.rs`), and
`IsSubclassable` fills their virtual methods with trampolines into Rust traits; the subclasses'
callbacks only queue. `glib` 0.22 (with `glib-sys`, `gobject-sys`, `gio-sys`, `glib-macros`,
`system-deps`, `cfg-expr`, `version-compare`) is the only addition to the lockfile; nothing else
changed in it. The build script finds WebKit only through `SIGHURT_WPE_PKG_CONFIG_PATH`
(decision 61) and gives the binaries an RPATH to the prefix.

## 2. Coexistence with Servo and Wasmtime

### Process model

WebKit's UI-process half runs in Sighurt's process, on the owner thread; pages run in WebKit's
own processes, which is WebKit's architecture and not Sighurt IPC: `WPEWebProcess` (in
bubblewrap: its own network, PID and UTS namespaces and a seccomp filter), `WPENetworkProcess`
(not sandboxed, WebKit's choice) and `xdg-dbus-proxy`. So the in-process risks of §18 are smaller
for WebKit than for Servo: page JavaScript, layout and rendering crash or hang in the web process.
What runs in `sig` is WebKit's UI-process code, GLib, and the presenter.

**Crash containment.** A web process that dies becomes `ViewFailed` (WebKit's
`web-process-terminated`, reason `CRASHED`), within 0.2 s of the kill; the browser and the other
engines go on (Servo ran its next page), and a reload starts a new web process that loads and
shows the page (`window-checks.sh` on both machines; `wpe-coexist` on the desktop and on River).
The test kills with `SIGKILL`: a `SIGSEGV` sent with `kill(2)` doesn't kill
WebKit's web process, because JavaScriptCore's handler there returns from signals that are not
faults. A crash of the network process was not tested.

### Symbols: the executable must not export its C libraries

Servo 0.5 and the Wasm engine link C libraries statically: HarfBuzz 11.2.0 (`harfbuzz-sys`
bundled, always), FreeType 2.13.2 with libpng 1.6.44 (Servo's default `bundled_freetype`),
SQLite 3.51.1 (`rusqlite` bundled, through `servo-storage`), mozjpeg (the Wasm engine's camera
support, through `nokhwa`), and SpiderMonkey's `getenv`/`setenv`/`abort` wrappers. The linker
exports every such symbol that a shared library in the link's closure defines or references: today's `target/debug/sig` exports 1 075 symbols (507
`hb_*`, 240 `png_*`, 153 `FT_*`, `jpeg_*`, zlib, `setenv`, `abort`, …). At run time the
executable comes first in the lookup scope, so those shared libraries bind to the executable's
copies, of other versions than they were built against: WebKit was built with HarfBuzz 13.2.1,
FreeType 2.14.3, libpng 1.6.58, SQLite 3.53.3 and libjpeg-turbo 3.1.4.

Measured with the dynamic linker itself (`LD_DEBUG=bindings LD_BIND_NOW=1`, the executable
loading and exiting at once; `coexist-checks.sh`):

| `wpe-coexist` linked | Exported symbols | Bindings from shared libraries into the executable |
|---|---|---|
| as Cargo does by default | 1 307 (507 `hb_*`, 240 `png_*`, 153 `FT_*`, 72 `jpeg_*`, …) | **1 524**: libsqlite3 445, libharfbuzz 236, **libWPEWebKit 220** (47 `FT_*`, 43 `hb_*`, 56 `png_*`, 25 `jpeg_*`, 47 `sqlite3_*`, `abort`, `getenv`), libpng 106, libfreetype 105, libjpeg 70, libass 66, fontconfig 23, … |
| with `-Wl,--exclude-libs,ALL` | **1** | **0** |

So with the default link, libsqlite3's and libharfbuzz's own internal calls land in other
versions' code, and WebKit's UI process (its favicon database and website-data bookkeeping use
SQLite, `WebKitImage` encodes PNG with libpng) runs two versions of each library on the same
objects. Nothing failed in the
runs, which exercise none of those paths in the UI process; that is luck, not compatibility. The
same applies to `sig` today without WebKit: FFmpeg's libass and libavfilter, fontconfig and the
system HarfBuzz already bind into Servo's copies.

**Fix: link the executable with `-Wl,--exclude-libs,ALL`** (`wpe-coexist/build.rs`): symbols of
static archives become hidden, the executable's own code still uses its own copies (bound at link
time), and every shared library binds to its own dependencies. Making Servo use system
libraries instead was rejected: `harfbuzz-sys` is bundled unconditionally by `servo-fonts`, the
change would rebuild Servo, and mozjpeg, SQLite and the SpiderMonkey wrappers would remain.
`dlopen` with `RTLD_DEEPBIND` was not needed. With the flag, all of `wpe-coexist`'s functional
checks pass; it is equally valid without WebKit, so `sig` should take it now (decision 65).

No shared library is mapped twice under one name (210 mapped, checked in `/proc/self/maps`): the
shell and WebKit come from the same pinned nixpkgs, and the two SONAMEs whose paths differ between
`sig`'s and WebKit's closures (libmpg123, libsystemd) are the same versions, loaded once.

### Signals: JavaScriptCore in Sighurt's process breaks Wasmtime's traps

WebKit keeps a JavaScriptCore VM in the UI process too, to hold what scripts return
(`webkit_web_view_evaluate_javascript` gives a `JSCValue`). Once that VM exists, JavaScriptCore
installs its `SIGSEGV`/`SIGBUS` handler (`VMTraps::SignalSender`, and the Wasm fault handler) with
`sa_mask` blocking every signal and without `SA_NODEFER` (WTF's `Signals.cpp`); it chains faults
that are not its own to the previous handler. Wasmtime 36 leaves its handler on Linux with
`wasmtime_longjmp` from inside the handler (`signals.rs`), which keeps the thread's signal mask as
the handler found it. So after one Wasmtime out-of-bounds trap under JavaScriptCore's handler, the
thread blocks every signal, and its next trap, a `ud2` raising `SIGILL`, is a blocked synchronous
signal: the kernel kills the process.

Reproduced in `wpe-coexist` with `WPE_COEXIST_JSC_SIGNALS=1`
(`runs/desktop-gles2-jsc-signal-handlers.txt`): after the script result came back, `SIGSEGV` and
`SIGBUS` belonged to `libWPEWebKit-2.0.so.1`, and the next Wasmtime trap pair killed the process
with `SIGILL` (a core dump). Without the UI-process script (`NO_JSC` run during development)
everything passed. SpiderMonkey does not have this problem: its handlers use `SA_NODEFER` and an
empty mask (M0 ran Wasmtime under it in both orders).

**Fix: JavaScriptCore in Sighurt's process stays off signals** (`keep_jsc_off_signals` in
`runtime.rs`, decision 66): before WebKit is first used, the runtime sets JavaScriptCore's options
`usePollingTraps = true` and `useWasmFaultSignalHandler = false` through `jsc_options_set_boolean`
and reads them back. Options are per process, so the web processes, where pages run JavaScript and
Wasm, keep their signal-based traps and fast memory. With the fix, `SIGSEGV` after the script
result is SpiderMonkey's (the executable), Wasmtime's trap leaves the mask as it was, and 2 000
traps each way in both pages ran while a thread of ours and the owner thread trapped in Wasmtime
(9 580 rounds, no wrong trap).

Dispositions through the run (`wpe-coexist/runs/desktop-gles2.txt`, each handler named by the
object it is in): at start, Rust's runtime has its stack-overflow `SIGSEGV`/`SIGBUS` handler;
Servo's runtime changes nothing; the Wasm engine's Wasmtime replaces `SIGSEGV`, `SIGILL` and
`SIGFPE`; WebKit's first view adds a `SIGUSR1` handler in `libWPEWebKit-2.0.so.1` (WTF's thread
suspension) and nothing else; SpiderMonkey's Wasm handlers come on top of Wasmtime's for `SIGSEGV`
and `SIGILL` once Servo's page runs Wasm; with the fix, nothing of WebKit's after that. No other
engine uses `SIGUSR1`.

### The run

`coexist-checks.sh` on the desktop (headless sway, GLES2) and `wpe-coexist` live on River:

| Check | Desktop | River |
|---|---|---|
| Wasmtime guests trap before WebKit starts (100 out-of-bounds, 100 `unreachable`) | PASS | PASS |
| Servo and WebKit run JavaScript and trap 100 times each way in their own Wasm | PASS (`js=42 oob=100/100 unreachable=100/100`) | PASS |
| Every engine presents: Servo (GL, `read_to_image` 0), WebKit (DMA-BUF, 0 copies), Wasm (SHM) | PASS | PASS |
| WebKit returns a script's value into this process (`"42 true"`) | PASS | PASS |
| A Wasmtime trap leaves the thread's signal mask as it was | PASS | PASS |
| Both pages trap 2 000 times each way while Wasmtime traps on two threads | PASS | PASS |
| A trapping Wasm guest fails its view only | PASS | PASS |
| A killed web process fails the WPE view; Servo goes on; a reload brings it back | PASS | PASS |
| No shared library mapped twice; process exits with 0; shutdown in 12 ms, Servo shut down | PASS | PASS |
| No symbol of WebKit's library binds into the executable | PASS | — |

The screenshot of the desktop run shows the three views: Servo's and WebKit's pages with
"servo åäö" and "wpe åäö" in their fonts, and the Wasm guest's green.

## 3. WebKit's GLib main loop inside the owner loop

WebKit's UI-process side runs on the default `GMainContext` of the thread that first uses it: its
IPC with the web and network processes, its timers and the rendering fences it waits for are GLib
sources. `GlibLoop` (`glib_loop.rs`) acquires that context on the owner thread and runs GLib's
iteration in two halves inside `pump`: `check` and `dispatch` what is ready (after a zero-timeout
`poll` of the descriptors, so readiness is fresh), then `prepare` and `query` the next wait, whose
timeout becomes the pump's deadline (`PumpStatus::Pending { deadline }`, or `Idle`). The wait is a
small thread that `poll`s exactly the descriptors GLib asked for and wakes the owner through the
engine's `EngineWaker`; it never touches GLib, and after waking the owner it waits for the next
set rather than polling a ready set again, so it cannot spin. Sources attached from other threads
wake GLib's own wakeup descriptor, which is in every set.

Registering GLib's descriptors as calloop sources directly was the alternative. It was not taken:
WebKit's descriptors come and go every frame (the rendering fence of each frame is a source), and
an epoll registration of a descriptor that GLib's owner already closed fails or, if the number was
reused, watches the wrong file; `poll` takes the descriptors afresh each time. It would also need
the runtime to be given `sig`'s loop handle, where now it needs only the `EngineWaker` every
runtime gets (`registry.rs`'s `fn(EngineWaker)`).

Evidence: the unit test `glib_loop::tests::sources_wake_the_owner_and_an_idle_context_sleeps`
(no deadline and no wake with nothing attached; a source attached from another thread wakes the
owner and is dispatched; a timeout is a deadline, not a wake). With a static page, the window's
loop woke once in 5 s (the probe's own `mark`) and GLib dispatched nothing, on both machines, both
transports, and live on River. During a 60 Hz animation the loop iterates about 6 times per frame
and GLib dispatches about 2 times per frame (WebKit's frame message, the rendering fence; plus the
host's frame callback and buffer release); measured, not yet optimized.

## 4. Presentation on a host-owned subsurface

**The path.** The host leases each WPE view a native target (`PresentMode::Gl`, whose contract is
that the presenter, not the host, attaches and commits the leaf; the name says GL, the presenter
here attaches DMA-BUFs). The presenter (`present.rs`) joins the host's `wl_display` with a second
event queue of its own (`Backend::from_foreign_display`), wraps the target's `wl_surface`, and binds
`zwp_linux_dmabuf_v1` version 4 and `wl_shm`. The default DMA-BUF feedback gives the compositor's
main device (its render node is found through sysfs) and the format table; the display hands both
to WebKit, which makes it the primary display, so WebKit's web process renders with GBM on that
device in a format and modifier the compositor imports (756 format and modifier pairs on both
NVIDIA machines). `render_buffer` only keeps the frame and reports `FrameReady`. On the host's
permit, `present` turns the WebKit buffer into a `wl_buffer` with
`zwp_linux_buffer_params_v1.create_immed`, once per WebKit buffer (cached in the buffer's user data
and destroyed with it), attaches it, damages WebKit's damage rectangles, and commits; the commit
carries the host's pending viewport, scale and frame callback request. Then it calls
`wpe_view_buffer_rendered`, so WebKit renders at most one frame ahead of what the host presents and
the host's frame callbacks pace it. When the compositor releases a buffer, the next pump or present
hands it back with `wpe_view_buffer_released`.

**Copies.** None on the DMA-BUF path: a whole animation used 4 imports (WebKit's swap chain) for
351 frames, with 349 releases by the probe. WebKit waits for each frame's rendering fence in the
UI process before handing it over (`use_explicit_sync` is false), so the compositor gets finished
buffers with implicit synchronization.

**Shared-memory fallback.** Without `zwp_linux_dmabuf_v1` version 4 and a DRM device (sway's pixman
renderer), or with `WpeConfig::force_shm`, the display reports no DRM device, so WebKit's web
process renders and reads the frame back into shared memory itself (`glReadPixels`, WebKit's own
copy). The presenter copies each frame once into one of at most 3 `wl_shm` buffers of its own per
view, reused only after release, and gives WebKit's buffer back at once. Diagnostics count the
copies and bytes and name the transport, so the fallback can't pass for the hardware path: 3.9 MB
per 1280 × 760 frame, 233 MB/s at 60 Hz; 8.4 MB per frame and 1.01 GB/s at 120 Hz on River.

**Geometry.** Metrics become `wpe_toplevel_scale_changed`, `wpe_toplevel_resized` and
`wpe_view_resized`; WebKit renders at the new size, a frame of the old size is dropped as stale
(at most one per change). WebKit floors `logical × scale` (853 × 1.5 → 1279) where the host rounds
half away from zero (1280); since the host's viewport scales the buffer to the logical size, a
frame one pixel off in a dimension is shown and counted (`rounded_frames`: 64 during the scale
test). Scales 1.5, 2, 1.25 and 1 and a resize to 1000 × 700 each showed the page, its 100 CSS px
corner box covering the screen pixels 90 CSS px scaled from the page's corner, and clicks landed at
the same CSS position. Hiding unmaps the view
(`wpe_view_unmap`; WebKit stops rendering), a hidden view presented nothing while its tab was
hidden, and showing maps it with its last frame.

**Pacing.** A CSS animation and a WebGL canvas (`pages/animated.html`):

| | Transport | Frames | Interval median / p95 / max | Copies |
|---|---|---|---|---|
| Desktop, headless GLES2, 6 s | DMA-BUF | 350 | 17.13 / 17.57 / 19.33 ms | 0 |
| Desktop, headless pixman | SHM | 346 | 17.35 / 17.76 / 18.45 ms | 347 (1.35 GB since start) |
| Desktop, headless GLES2, forced SHM | SHM | 342 | 17.46 / 18.13 / 19.62 ms | 343 |
| louise-pc, headless GLES2 | DMA-BUF | 351 | 17.09 / 17.42 / 19.62 ms | 0 |
| louise-pc, headless pixman | SHM | 361 | 16.67 / 16.78 / 16.90 ms | 362 |
| louise-pc, headless GLES2, forced SHM | SHM | 351 | 17.07 / 17.23 / 18.39 ms | 352 |
| River live, 10 s, 120 Hz output | DMA-BUF | 1 200 | 8.33 / 9.07 / 12.42 ms | 0 |
| River live, 10 s, forced SHM | SHM | 1 200 | 8.33 / 8.67 / 11.86 ms | 1 200 (10.1 GB) |

The chrome strip was painted twice (first configure and resize) during every animation, never
because the page animated. On River, WebKit's compositor followed the 120 Hz frame callbacks while
the page's `requestAnimationFrame` ran at 60 Hz: WebKit's vblank timer assumes 60 Hz because the
display provides no `WPEScreen` with the output's refresh rate (limitation 1 below).

**Memory**, proportional set size of the process and WebKit's processes, two views (the
animation shown): 250 MiB on the desktop (web process 193, network process 24, Sighurt's process
30, bubblewrap 2, D-Bus proxy 2), 261 MiB on louise-pc, 258 MiB on River. One static page:
173 MiB (web process 110). The SHM path costs Sighurt's process about 11 MiB more (its buffers).

## 5. Input

`input.rs` turns the host's normalized input into `WPEEvent`s. Positions are surface-local logical
pixels, which a `WPEView` takes as they are (WebKit applies the scale). Buttons map to WPE's
numbering, the view tracks held buttons (WebKit reads them from every pointer event's modifiers) and
computes click counts with `wpe_view_compute_press_count`; wheel detents and smooth deltas go in
with WebKit's sign. Keys go in as WebKit wants them: the hardware keycode (evdev + 8) and an X
keysym, rebuilt from the host's translated key, since the host already ran xkbcommon with dead keys
and compose: a character's keysym (`å` is `aring`; the press that completes `¨` + `a` carries
`adiaeresis`), `dead_*` for a dead key, a table for named keys (left and right modifiers by
keycode). Modifiers map to WPE's; a modifier key's own event includes its modifier, as WPE's own
seat does it. Focus is `wpe_view_focus_in`/`focus_out`; losing focus first sends key-ups for every
key the view holds; window deactivation clears the toplevel's active state.

Checked by `window-checks.sh` on both machines: a click lands at its surface-local position (and
at the same CSS position at every scale); å ä ö and a dead-key ä are typed into a field; a named
key (F2) arrives; the page blurs when the chrome takes the keyboard, gets nothing typed meanwhile,
and gets keys again after focus returns; the wheel scrolls. Keys typed with `wtype` have made-up
keycodes (wtype uploads its own keymap), so their `code` says nothing about a real keyboard. The
unit test `input::tests::swedish_letters_and_a_dead_key_have_their_keysyms` covers the keysym
table.

## Limitations and open problems

1. **`requestAnimationFrame` runs at 60 Hz on faster outputs**: the display has no `WPEScreen`, so
   WebKit's vblank timer uses 60 Hz. A screen with the output's refresh rate (and a
   `WPEScreenSyncObserver` fed by the host's frame callbacks) fixes it.
2. **Not implemented in the prototype**: input methods (`WPEInputMethodContext` ↔ the host's
   text-input), the clipboard (`WPEClipboard` ↔ the host's clipboard services), popups and context
   menus, `window.open` and navigation policy (`OpenRequested`), downloads and permission requests
   (`ServiceRequested`), the back-forward list (`HistoryChanged`, history effects), cursor images
   (only cursor names), pointer lock, touch, accessibility. Capabilities report only history
   traversal and page zoom.
3. **Implicit synchronization only**: WebKit waits for each rendering fence in the UI process.
   Explicit synchronization (`linux-drm-syncobj-v1` in the host, `use_explicit_sync`) would save a
   wakeup per frame. The loop's 5–6 iterations per frame are measured, not optimized.
4. **The host's names**: the presenter uses `PresentMode::Gl` targets and `TargetLease::gl_handles`
   for a non-GL native presenter. A `Native` mode, or a documented accessor for the display and
   surface, would say what it is (a `sighurt-wayland` change, not made here).
5. **Releases are seen when the engine is pumped or presents**: the host reads the socket, and the
   presenter's queue is dispatched at its next entry. WebKit drives the next frame, so nothing
   waits on it; an idle page holds its last buffer until then.
6. **A click 0.2 s after a page loaded** arrived 40 px off (at its window position) once, in a
   check run that clicked right after load; with a 1 s wait it never did. The cause is not known;
   it needs looking at in the host, with the first frame's map.
7. **The WPE Platform API changes between WebKit releases** and is not built by default in 2.52:
   the adapter has to be checked against each new WebKit's headers.
8. **Not run**: `wpe-coexist` on louise-pc (Servo must be rebuilt there first), a crash of the
   network process, fractional scales live on River, more than one window.
9. **`ci.yml` and CLAUDE.md's pre-commit commands** must exclude `wpe-engine` and `wpe-coexist`
   (they need the WPE prefix; `wpe-coexist` builds Servo), as they exclude `m0-coexist` and
   `m1-mixed`.

## A plan for `sighurt-engine-wpe`

The prototype's modules carry over almost as they are; what changes is what `sig` adds around
them.

1. **Crate** `sighurt-engine-wpe` (api, wayland; `glib` 0.22; build script as `wpe-engine`'s):
   `ffi`, `platform` (the subclasses), `glib_loop`, `present`, `input`, `runtime`, plus
   `navigation` (WebKit's load events and back-forward list → navigation events, history effects
   and `HistoryChanged`) and `view` (per-view state). Feature `engine-wpe` on `sighurt-browser`, an
   entry in `registry.rs` (`start: fn(EngineWaker)`, mode `Gl` until the host has a `Native` mode,
   renderer "WebKit, DMA-BUF" or "SHM fallback").
2. **`sig`'s link**: `-Wl,--exclude-libs,ALL` for the `sig` binary (`cargo:rustc-link-arg-bins`
   in `sighurt-browser`'s build script), with or without WPE (decision 65).
3. **Process setup** stays in the runtime: JavaScriptCore's options before WebKit is first used
   (decision 66); one WPE runtime per process (WebKit's UI-process state is global, and the
   display is the primary one).
4. **Services**: WebKit's `decide-policy` (new windows → `OpenRequested`, downloads →
   `ServiceRequested`), `permission-request` → the permission service, a `WPEClipboard` subclass on
   the host's clipboard, a `WPEInputMethodContext` subclass on the host's text input, cursor
   bytes, a `WPEScreen` per output with scale and refresh rate.
5. **Profile**: a persistent `WebKitNetworkSession` under `{data_dir}/sighurt/wpe` (the prototype
   uses an ephemeral one), WebKit's sandbox on.
6. **Tests**: `window-checks.sh`'s scenarios move into `sighurt-browser/tests/engines.sh` as a
   `wpe` scenario; the GLib and keysym unit tests move with their modules.
7. **Later**: explicit synchronization, the 60 Hz `requestAnimationFrame` (limitation 1), fewer
   wakeups per frame.
