# Sighurt on macOS and Windows: the groundwork, and what a port still needs

Sighurt is a Linux browser: the window is Wayland's, and WPE WebKit, Ladybird and NetSurf are
built for Linux. The user decided on 15 November 2026 that macOS and Windows usability should be
worked on, which amends decisions 2 and 89 ("Wayland only"); the research behind this file is
`reports/Sighurt Windows and macOS support.md` and its notes (in the main checkout, untracked).
It recommends porting in layers, cheapest first: portability changes verified on Linux, then a
terminal-only `sig` on Apple Silicon macOS, then a decision about a native window. This file
records the first layer (done on the `port` branch), what was checked from this Linux machine and
how, and what each platform still needs. Nothing here has run on macOS or Windows.

## What the groundwork changed

Nothing changes on Linux: `sig`'s default features, the frontend choice in `main`, every
setting and every command are what they were. The changes are features and `cfg`:

| Change | Where | What it does |
|---|---|---|
| `frontend-gui` is a real feature | `apps/sighurt-browser/Cargo.toml`, `src/main.rs` | `sighurt-gui` (and with it sighurt-wayland, sighurt-ui and the renderers) is optional. A `sig` built with `--no-default-features --features frontend-tui,…` links no Wayland crate. `chrome-line`, `render-wgpu` and `render-vulkan` turn it on. A build without the window says so when asked for one (`--gui`, or no terminal), and `main` installs no signal handlers off Unix. Builds of the window name it: `--no-default-features --features frontend-gui,engine-demo`. |
| The damage report is the API's | `sighurt_api::present::damage` (was `sighurt-wayland`'s `damage.rs`) | The thread-local report a CPU painter makes of the pixels it changed. sighurt-wayland re-exports it, so the host and its tests are unchanged. |
| No CPU engine needs sighurt-wayland | `sighurt-engine-wasm`, `-demo`, `-netsurf` | Their only uses were the damage report and a default type parameter (`TargetLease`), which every caller already named. The Wasm engine keeps it as a dev-dependency for its compositor test and example. |
| Signals are a type, not a descriptor | `sighurt_runtime::signals` | `Signals` (`Request::Quit`, `Request::Report`) replaces the `OwnedFd` `main` handed the frontends and `--dump`. On Unix it is the same non-blocking pipe, waited on through `AsFd`; elsewhere `Signals::none` (nothing arrives, the system's default action ends the process). |
| The configuration watch says where it can't | `persistence::ConfigWatch` | inotify on Linux, as before. Elsewhere `new` fails with `Unsupported`, which the frontends already log ("not watching …"), and the file is read at startup only. |
| Terminal programs are Linux's | `sighurt-runtime`'s `pty`, the Wasm engine's `pty` | On Linux the modules are unchanged. Elsewhere a module of the same shape (`pty_unsupported.rs` in each crate) refuses `PtyOpen` with the reason and never has a master, so `sig://terminal` and `sig://config` say they can't run a program. |
| File modes only where there are modes | `persistence`, `registry` | 0700 directories and 0600 files on Unix, as before. Windows has no mode bits: files take their directory's access list. |
| Small Unix-only calls | `diagnostics::monotonic`, the forced exit's `write`, `open_external` | CLOCK_MONOTONIC on Unix, seconds since the first call elsewhere; fd 2 on both; `xdg-open` on Linux, `/usr/bin/open` on macOS, `Unsupported` on Windows. |
| The terminal on macOS | `sighurt-tui`'s `term/session.rs` | macOS has no `pipe2`, `TIOCGPTPEER`, `SYS_gettid`, `__errno_location`, `SI_TKILL` or termios `line_discipline`: each has an Apple branch (`pipe` then `fcntl`; `ptsname`; the thread's Mach port; `__error`; `SI_USER` only, `0x10001` from `<sys/signal.h>`). `main`'s signal pipe has the same `pipe2` branch. |
| The terminal off Unix | `sighurt-tui`'s `lib.rs`, `unsupported.rs` | `term`, the owner loop and `--dump` are Unix's, `cfg` at the module boundary. Elsewhere the crate keeps the rest (grid, chrome, encoders, text mode, engines), and `run` and `dump` refuse with the reason. |
| Linux-only engines stop at once elsewhere | the build scripts of `sighurt-engine-wpe`, `-ladybird`, `-netsurf` | A `cargo::error` names the feature to drop, instead of a missing prefix, `eventfd` or a C compile error. |
| CEF compiles everywhere, runs on Linux | `sighurt-engine-cef`'s `ffi::load` | Off Linux it refuses: the bindings carry Linux's API hash and the prefix is Linux's build. The adapter (OnPaint frames into a `CpuTarget`) checks on both targets. |
| `-Wl,--exclude-libs,ALL` on Linux only | `apps/sighurt-browser/build.rs` | GNU ld's flag (decision 65); Apple's linker and MSVC's reject it, and neither platform interposes symbols that way. |
| Linux-only dev-dependencies | `apps/sighurt-browser/Cargo.toml`, `tests/tui.rs` | The Ladybird engine and the window's `virtual-pointer` example's Wayland crates are Linux's dev-dependencies; `tests/tui.rs` (ptys) is Unix's. |

## How it is checked here

The pin's rustc (1.98.1, nixpkgs) has a standard library only for Linux, wasm32 and BPF, and
upstream's prebuilt `rust-std` does not load in it (nixpkgs' compiler calls itself "built from a
source tarball", so the metadata versions differ). `tools/cross-check TARGET [cargo check
arguments]` therefore builds std for the target from source with `-Zbuild-std`
(`RUSTC_BOOTSTRAP=1`, the library sources `shell.nix` exports as `SIGHURT_RUST_LIB_SRC`, which
carry their vendored dependencies). It only checks: nothing is linked, so no foreign linker is
needed, and its artifacts go to `$CARGO_TARGET_DIR/cross`. The one addition to `shell.nix` is that
variable.

There is no macOS or Windows SDK here, so C for those targets cannot be compiled. The TLS
libraries' build scripts are overridden (`links` overrides for `ring` and `aws-lc-sys`, whose Rust
bindings per target ship pregenerated): their C is not built, their Rust is checked. Any other
build script that compiles C, runs bindgen or asks pkg-config for the target fails, and what
depends on it goes unchecked. That is a limit of this machine, not of the code: on a Mac or a
Windows machine with its SDK those crates build normally (unverified).

## The matrix

Each cell is `tools/cross-check <target>` on the crate (libraries only; `sig` as noted), on the
`port` branch. "Clean" means no error and no warning from Sighurt's code.

| Crate (features) | aarch64-apple-darwin | x86_64-pc-windows-msvc |
|---|---|---|
| sighurt-api, sighurt-core, sighurt-semantic, sighurt-config-example | clean | clean |
| sighurt-ui, sighurt-chrome-line, sighurt-render (also `gpu`) | clean | clean |
| sighurt-render-wgpu, sighurt-render-vulkan | clean (they compile; both bind Wayland surfaces only) | clean (same) |
| sighurt-engine-demo, sighurt-engine-cef | clean (CEF's loader refuses at run time) | clean (same) |
| sighurt-runtime (no engines) | clean | clean |
| sighurt-tui (lib; none, and `engine-demo`) | clean | clean (terminal I/O compiled out) |
| `sig`, `--no-default-features --features frontend-tui,engine-demo` | **clean** | **clean** (`--tui` and `--dump` refuse) |
| `sig`, `--no-default-features` (no frontend) | clean | clean |
| sighurt-engine-wasm (so `sig` with `engine-wasm`) | stopped by build scripts that compile C for the target: `ffmpeg-sys-next` (and its pkg-config), `wasmtime` and `wasmtime-internal-jit-debug` (Wasmtime's C helpers), `zstd-sys`, `mozjpeg-sys`, `objc_exception` | stopped the same way: `ffmpeg-sys-next`, `wasmtime`, `wasmtime-internal-jit-debug`, `wasmtime-internal-fiber`, `ittapi-sys`, `zstd-sys`, `mozjpeg-sys` |
| sighurt-wayland (so sighurt-gui) | fails: smithay-client-toolkit uses `pipe2`, and wayland-client needs libwayland | fails: wayland-sys is Unix-only |
| sighurt-engine-ladybird, sighurt-engine-netsurf | stops at once, by design (`cargo::error`: Linux-only) | the same |
| sighurt-engine-wpe | not run; its build script has the same gate | the same |
| sighurt-engine-servo, default-feature `sig` | not attempted (Servo; see below) | not attempted |

The commands (with `CARGO_TARGET_DIR` set to this worktree's target directory):

```bash
tools/cross-check <target> --keep-going --lib -p sighurt-api -p sighurt-core -p sighurt-ui -p sighurt-render \
    -p sighurt-render-wgpu -p sighurt-render-vulkan -p sighurt-chrome-line -p sighurt-config-example \
    -p sighurt-semantic -p sighurt-engine-demo -p sighurt-engine-cef -p sighurt-runtime -p sighurt-tui
tools/cross-check <target> -p sighurt-render --features gpu
tools/cross-check <target> -p sighurt-tui --lib --features engine-demo
tools/cross-check <target> -p sighurt-browser --no-default-features --features frontend-tui,engine-demo
tools/cross-check <target> -p sighurt-browser --no-default-features
tools/cross-check <target> --keep-going -p sighurt-engine-wasm
tools/cross-check <target> -p sighurt-wayland
tools/cross-check <target> -p sighurt-engine-ladybird      # and -p sighurt-engine-netsurf
```

The terminal-only `sig` on Linux links no Sighurt Wayland crate (sighurt-wayland, sighurt-gui,
sighurt-ui, the renderers), checked with `cargo tree`. With the Wasm engine it still pulls
`wayland-client` in through its file picker (`rfd`'s XDG portal backend, `ashpd`, a Linux-only dependency); with the demo
engine alone it has none.

## What a port still needs

### macOS (Apple Silicon first)

The terminal-only `sig` with the demo engine checks clean. Before it is usable:

1. **Run it.** Nothing has run on a Mac: build `--no-default-features --features
   frontend-tui,engine-demo,engine-wasm` there, run `tests/tui.rs` and the platform-neutral
   tests (the `Ports` workflow, `.github/workflows/ports.yml`, does this on `macos-latest`, and
   has not run yet), and try iTerm2, kitty, Ghostty and WezTerm (kitty graphics, sixel) and
   Terminal.app (text mode only). The Apple branches of `term/session.rs` are unverified: the
   Mach-port thread id, `__error`, `SI_USER`'s value, and the pty opened by name.
2. **The Wasm engine's C dependencies** build only with the SDK: FFmpeg (`ffmpeg-sys-next`, from
   Homebrew through pkg-config), Wasmtime's C helpers, zstd, mozjpeg and `objc_exception` (the
   camera and file-picker stack). Its Rust is unchecked for macOS until one of them builds.
3. **The configuration watch**: kqueue (`EVFILT_VNODE` on the directory, and on the file for
   writes in place) or the `notify` crate, behind `ConfigWatch`.
4. **Terminal programs**: the Linux `pty.rs` with a kqueue `EVFILT_PROC`/`NOTE_EXIT` watch in
   place of the pidfd, a loop closing descriptors in the child (or `POSIX_SPAWN_CLOEXEC_DEFAULT`)
   in place of `close_range`, and `pipe` plus `fcntl` in place of `pipe2`; the Wasm engine's
   `pty.rs` needs only the last. `killpg` is the same.
5. **Servo in the terminal**: its offscreen context is Mesa's surfaceless EGL
   (`offscreen.rs`), which macOS has not; surfman's CGL or Servo's `SoftwareRenderingContext`
   is the candidate, and the research's first prototype (one frame read back on a Mac) decides
   how long that takes. The adapter also depends on sighurt-wayland for its window path
   (`GlHandles`, `TargetLease`), which links libwayland: that half needs a `cfg` or a feature
   before the crate builds for macOS at all. Until then text mode and `--dump` come from the
   demo engine only. Servo was not cross-checked here (its build is the machine's heaviest).
6. **Distribution**, if it is ever more than personal: signing and notarization.

### Windows

The terminal-only `sig` with the demo engine checks clean, but it runs nothing useful: `--tui`
and `--dump` refuse. Before it is usable:

1. **A console backend** for the terminal frontend: `SetConsoleMode` with VT input and output,
   resize from `WINDOW_BUFFER_SIZE_EVENT` records, a console control handler in place of the
   signals (a `Signals` that it feeds), and an owner loop that waits on console handles (calloop
   on Windows waits on sockets only). The parser, grid, chrome, sixel encoder and text mode are
   already compiled for Windows; Windows Terminal has sixel and no kitty graphics.
2. **ConPTY** for `sig://terminal`, with a kill-on-close job object, an inherited-handle allow
   list and absolute program paths; the Wasm engine's master becomes two pipe handles behind the
   same `PtySession` hand-over.
3. **Files**: owner-only access lists in place of 0600 and 0700, a durable rename (the directory
   sync is skipped there), retries on sharing violations, the session lock on its own file (it
   is already), and `ShellExecuteW` for external URLs.
4. **The Wasm engine's C dependencies** (FFmpeg through vcpkg, Wasmtime's helpers, zstd,
   mozjpeg); its Rust is unchecked for Windows until they build.
5. **Servo**: its MSVC build, and a non-Mesa offscreen context, as on macOS.
6. **CEF** is the likeliest second engine there: `LoadLibraryW` of `libcef.dll`, Windows' API
   hash, an `.exe` helper; its adapter already checks for the target.

### A window, on either

Out of scope here (the research's layer 4). It needs a host other than sighurt-wayland (child
NSViews or HWNDs, or one CPU-composited surface as a labelled fallback), Servo's native target
per system, a key table into evdev codes, and the renderers' non-Wayland surfaces (wgpu is
Vulkan-only by decision 125; that rule's reason is Linux's EGL). sighurt-gui stays Linux's until
then.

## Verified and assumed

Verified on this machine, on the `port` branch (15 November 2026): the cells marked clean above;
on Linux, `cargo fmt --all -- --check`, both pre-commit clippy lines and both test lines of
CLAUDE.md without Servo (1 328 workspace tests passed, none failed), clippy of the terminal-only
`sig` (`frontend-tui,engine-demo,engine-wasm`, all targets) and of `sig` with no frontend,
`cargo test -p sighurt-browser --no-default-features --features frontend-tui,engine-demo` (6 unit
tests and the 44 pty tests of `tests/tui.rs`), `cargo test -p sighurt-tui --features
engine-demo,engine-wasm`, `cargo check` of `sig` with no feature, with `frontend-gui,engine-demo`,
with CEF, Ladybird and the Vulkan renderer, and with NetSurf, `apps/sighurt-browser/tests/headless.sh`
in a private sway (250 PASS, 0 FAIL, SIGTERM quitting through `Signals` among them), and
`cargo check -p sighurt-browser` with its default features (Servo and WPE WebKit).

Assumed, from reading and not from a run: that any of it works on a Mac or a Windows machine;
the Apple constants and calls named above; that `/usr/bin/open` takes a URL as `xdg-open` does;
that the TLS crates and the C dependencies build with the platforms' SDKs; that the `Ports`
workflow passes (it has not run).
