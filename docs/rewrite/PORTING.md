# Sighurt on macOS and Windows: the terminal there, and what a port still needs

Sighurt is a Linux browser: the window is Wayland's, and WPE WebKit, Ladybird and NetSurf are
built for Linux. The user decided on 15 November 2026 that macOS and Windows usability should be
worked on, which amends decisions 2 and 89 ("Wayland only"); the research behind this file is
`reports/Sighurt Windows and macOS support.md` and its notes (in the main checkout, untracked).
It recommends porting in layers, cheapest first. The first layer, portability changes verified
on Linux, was the `port` branch ([below](#what-the-groundwork-changed)). The second, done on the
`port2` branch, is the terminal-only `sig` on macOS and on Windows: it runs on both, proven on
GitHub's hosted runners (`.github/workflows/ports.yml`). A window on either is not started.

## What runs where

What GitHub's `macos-latest` (Apple Silicon) and `windows-latest` (x86_64) runners prove, run
by run, is in [Verified and assumed](#verified-and-assumed). "Proven" means a test on that
runner drives the real `sig`; "built" means it compiles and links there and nothing tests it.

| | Linux | macOS | Windows |
|---|---|---|---|
| `sig --tui` (the terminal frontend) | yes | proven: `tests/tui.rs` in a pty, demo and Wasm engines | proven: `tests/console.rs` in a pseudoconsole (ConPTY), demo engine |
| `sig --dump URL` | yes | proven (`tests/tui.rs`) | proven (`tests/console.rs`) |
| Demo engine | yes | proven | proven |
| Wasm engine, sig's own pages (`sig://help`, home's links) | yes | proven (FFmpeg 7 from Homebrew) | not built: FFmpeg and Wasmtime's C helpers need vcpkg and the MSVC SDK set up |
| `sig://terminal`, `sig://config` programs | yes | refused, saying so (`pty_unsupported.rs`) | refused, saying so |
| Configuration watch | inotify | kqueue (`EVFILT_VNODE`), proven by its unit test | none: read at startup (`ConfigWatch::new` says so) |
| Quit and report requests | signals | signals (SIGUSR1 report; SIGTERM, SIGINT, SIGHUP quit) | console control handler: Ctrl+Break and closing the console quit; Ctrl+C is a key in the terminal and a quit in `--dump`; no report |
| Restoring the terminal on a crash | panic, SIGABRT, SIGBUS, SIGFPE, SIGILL, SIGSEGV | the same, proven by `frontends/sighurt-tui/tests/pty.rs` | a panic only: a native crash leaves the console's modes as they were |
| Private files | 0700 directories, 0600 files | the same | a protected access list (the user and SYSTEM) on directories `sig` creates; files inherit it |
| External addresses | `xdg-open` | `/usr/bin/open` (built, not tested) | `ShellExecuteW`, http, https and mailto only (built, not tested) |
| The window, Servo, WPE WebKit, CEF, Ladybird, NetSurf | yes | no | no |

## The terminal on Windows

The console speaks the same bytes as a Unix terminal once its input handle has
`ENABLE_VIRTUAL_TERMINAL_INPUT` and its output `ENABLE_VIRTUAL_TERMINAL_PROCESSING` (Windows 10
1809 and later, and every Windows Terminal). So the parser, the probe, the encoders, the grid,
the chrome, text mode and the owner loop are Unix's, and only the session is new:
`frontends/sighurt-tui/src/term/console.rs`, with the Unix session's interface.

- **Raw mode** is console modes: VT input, window-size records and mouse input, the extended
  flags without quick edit (a click is `sig`'s, not the console's selection), and no line
  input, echo or processed input, so Ctrl+C is the byte 0x03 as in a raw Unix terminal. Output:
  VT processing, processed output and `DISABLE_NEWLINE_AUTO_RETURN` (Unix's `OPOST` off). Both
  code pages are UTF-8. What `enter` found is put back by `restore`, by `Drop`, by the panic hook
  on the thread that entered and by `restore_before_forced_exit`.
- **Input**: a thread (`sighurt-console-in`) waits on `CONIN$` and a stop event, reads the input
  records, and turns the characters of key-down records (one per record; a VT sequence is
  several) into UTF-8 for the owner, at most 64 KiB held, waiting while that is full. It rings a
  calloop ping for each read and for each window-size record; the owner reads the window's size
  (`GetConsoleScreenBufferInfo`'s `srWindow`) on every read, as Unix asks `TIOCGWINSZ`.
- **Output**: a thread (`sighurt-console-out`) writes one chunk of at most 64 KiB, cut at a
  character's boundary, and rings a second ping when it is out: a console write blocks while
  the terminal does not read, and the owner must not. The owner's bounded queue (`Output`, now
  written through a `Sink`) hands the writer a chunk when it is idle; a chunk handed over counts
  as written, as bytes the kernel accepted do on Unix.
- **The owner loop** is calloop on both systems. On Windows its sources are pings: the console's
  two, the runtime's `Signals` queue (`Signals::channel`, which `main`'s console control handler
  sends to and which rings a ping set with `set_wake`), the engines' and the services' wakes.
  `Terminal::insert_sources` and `frontends/sighurt-tui/src/sys.rs` hold what differs.
- **Restoring** waits up to 250 ms for the writer's chunk, writes what closes a sequence left
  open on the wire and the leave sequence, puts the modes and code pages back, and discards the
  console's unread input (Unix's `TCIFLUSH`). A console that never takes the chunk keeps that
  thread until the process ends rather than holding the restore.
- **No signals**: no job control (nothing suspends), no hang-up (a closed console comes as a
  quit and fails reads and writes), no diagnostics report. `main` quits cleanly for Ctrl+C (in
  `--dump`) and Ctrl+Break; a second one is left to Windows, which ends the process at once.
  Closing the console, logging off and shutting down quit too, and the handler does not return
  until `main` has (Windows ends the process when it returns, and allows 5 s after a close).
- **`--dump`** writes the page from a thread of its own and waits for it with the dump's
  deadline: Windows has no non-blocking write to a pipe or a console.
- **Starting it**: a console has no `TERM`, so on Windows a console on stdout is the terminal
  unless `TERM` is `dumb`. Windows Terminal has sixel (1.22 and later) and no kitty graphics; a
  console that answers no image protocol, as ConPTY's does in the tests, gets text mode.
- **Files**: sessions and history are replaced with `MoveFileExW` (replace, write-through),
  retried five times over about 150 ms on a sharing, access or lock violation (an indexer or a
  virus scanner holding the file). The session lock is its own file, as on Unix. A directory
  `sig` creates gets a protected access list granting full control to the user and to SYSTEM,
  inherited by what is made in it. That is the 0700/0600 rule as far as Windows has one: no
  other ordinary user can read the files and the profile's inherited entries are cut; unlike
  0600, SYSTEM can read them, an administrator can take ownership, and a directory that already
  exists is not changed. Download names also lose what Windows refuses (`\ / : * ? " < > |`,
  trailing dots and spaces) and device names (`CON`, `NUL`, `COM1`…) get a `_` in front.
- **External addresses** go to `ShellExecuteW` on a thread of its own, and only http, https and
  mailto: for a `file:` address or a path that call would start a program.

## The terminal on macOS: what the real machine showed

The Unix session ran on macOS as written, apart from what the first runs found:

- The terminal driver sets `PENDIN` itself whenever `ICANON` goes back on, so a restored termios
  never equalled the one found; `termios_matches` leaves `PENDIN` out there (it is state, not a
  setting).
- `F_GETFL` reports `FWASWRITTEN` (0x10000) once a descriptor has been written; the tests that
  compare status flags mask it.
- XNU gives a `SIGSEGV` that `kill` sent the fault codes, so a kill can't be told from a fault:
  on Apple every fatal signal that reaches the default action is restored and raised again,
  which ends the process with that signal either way.
- `dirs` keeps the configuration, the session, the history and the log in one directory,
  `~/Library/Application Support/sighurt`, and reads no `XDG_*` variable; `tests/tui.rs` maps
  its directories onto that one.
- A macOS pty holds about a kilobyte, so the tests' fake terminal waits on the pty with `poll`
  instead of fixed sleeps and keeps reading while it waits; with a fixed sleep the frame rate was
  the test's, not `sig`'s.
- APFS refuses some names (`EILSEQ`): download names on macOS drop noncharacters, private use
  and everything above plane 3.
- Homebrew's current FFmpeg is newer than the one `ffmpeg-next` 8.1 binds; the workflow installs
  `ffmpeg@7` and points pkg-config at it.
- The configuration watch is kqueue: the directory (`NOTE_WRITE`: entries added, removed,
  renamed) and the file while it exists, its watch made again on the current inode after a
  change; a change is a note on the file, or the file's identity (existence, device and inode,
  size, modification time) differing from the last one seen.

## What the groundwork changed

The `port` branch's changes, as they were made; three rows are superseded by the terminal on
Windows above (the signals, the terminal off Unix, `open_external` on Windows) and one by the
macOS configuration watch. Nothing changes on Linux: `sig`'s default features, the frontend
choice in `main`, every setting and every command are what they were. The changes are features
and `cfg`:

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

### macOS

0. **Open: a stall with the Wasm engine.** `tests/tui.rs`'s
   `a_megabyte_of_garbage_as_the_probes_answer_is_survived` (a megabyte of random bytes as the
   terminal's answer to the probe) failed three times in eight runs on `macos-latest`, each
   time in the pass with the Wasm engine: `sig` took none of the fake terminal's input for
   30 s (runs 43818654935, 43821547281, 43842878416). With the demo engine alone it always
   passed, and on Linux it always does. It is ignored there (`cfg_attr` names the runs) until it
   is explained; the likeliest suspects are a long start of the Wasm engine on the runner while
   the pty's kilobyte of input buffer is full, and the probe's reads under kqueue.
1. **Real terminals.** The tests use a fake terminal on a pty. iTerm2, kitty, Ghostty and
   WezTerm (kitty graphics, sixel) and Terminal.app (text mode) have not been tried.
2. **Terminal programs** (`sig://terminal`, `sig://config`): the Linux `pty.rs` with a kqueue
   `EVFILT_PROC`/`NOTE_EXIT` watch in place of the pidfd, a loop marking descriptors
   close-on-exec in the child in place of `close_range`, `pipe` plus `fcntl` in place of `pipe2`,
   and the master opened without `O_CLOEXEC` then marked; the Wasm engine's `pty.rs` needs only
   the `pipe2` change. Its tests read `/proc` (descriptors, signal masks, the controlling
   terminal), which macOS does not have, so they need other probes before they prove anything
   there; until then the pages refuse.
3. **Servo in the terminal**: its offscreen context is Mesa's surfaceless EGL (`offscreen.rs`),
   which macOS has not; surfman's CGL or Servo's `SoftwareRenderingContext` is the candidate,
   and the adapter's window half depends on sighurt-wayland. Until then text mode and `--dump`
   come from the demo engine (and pages from the Wasm engine as graphics).
4. **`/usr/bin/open`** for external addresses is not tested.
5. **Distribution**, if it is ever more than personal: signing and notarization.

### Windows

1. **Real terminals.** Only ConPTY (the console host the tests start) has run `sig`; Windows
   Terminal (sixel), the classic console window, and WezTerm have not.
2. **The Wasm engine**: FFmpeg through vcpkg, Wasmtime's C helpers, zstd and mozjpeg with the
   MSVC toolchain; its Rust is unchecked for Windows until they build. With it, sig's own pages.
3. **ConPTY for the terminal pages**: a pseudoconsole per program, the program in a job object
   with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, inherited handles limited to the pseudoconsole's
   pipes, absolute program paths (`CreateProcess` searches the current directory), a closed
   choice of shells (`%ComSpec%`, Windows PowerShell, `pwsh`); the Wasm engine's master becomes
   two pipe handles behind the same `PtySession` hand-over, and `ClosePseudoConsole` needs its
   output drained before Windows 11 24H2.
4. **A configuration watch** (`ReadDirectoryChangesW` on a thread that rings a ping).
5. **Restoring after a native crash** (an unhandled-exception filter), and a diagnostics report
   (a named event).
6. **The address line** takes `C:\…` for a search, not a path; command-line paths work.
7. **`ShellExecuteW`** and the access lists are tested only by unit tests on the runner.
8. **Servo** (its MSVC build and a non-Mesa offscreen context) and **CEF** (`LoadLibraryW` of
   `libcef.dll`, Windows' API hash, an `.exe` helper), as before.

### A window, on either

Out of scope here (the research's layer 4). It needs a host other than sighurt-wayland (child
NSViews or HWNDs, or one CPU-composited surface as a labelled fallback), Servo's native target
per system, a key table into evdev codes, and the renderers' non-Wayland surfaces (wgpu is
Vulkan-only by decision 125; that rule's reason is Linux's EGL). sighurt-gui stays Linux's until
then.

## Verified and assumed

The Ports workflow (`.github/workflows/ports.yml`) on GitHub's hosted runners, `port2` branch,
16 November 2026. Runs are at `https://github.com/gusahlg/sighurt/actions/runs/<id>`.

**Windows (`windows-latest`), verified.** Run 43838694058, job "Windows (terminal only)", both
steps green, 723 tests (and green in runs 43824448752, 43826803295, 43830229164 and
43835363647 before it). Its first attempt failed one timing test whose bound is 700 ms on a
shared runner (`services::tests::quitting_deletes_a_download_its_server_keeps_waiting`); the
second passed.

- `cargo test -p sighurt-browser --no-default-features --features frontend-tui,engine-demo --bins
  --tests`: `sig`'s 6 unit tests and `tests/console.rs`'s 3. In a ConPTY pseudoconsole of 80×24,
  `sig demo://pattern` (no `--tui`, no `TERM`) shows the page as text, `o` opens the address
  line with the address in it, typing `demo://resize` and Enter opens that page (its title on
  the top line), `q` exits with 0, and the console's modes, read by a process started in the
  same pseudoconsole, are the same before and after (`0x1f7 0x7`; run 43821547281 prints each
  screen). `sig --dump demo://pattern --width 40` into a pipe prints the page's text, and a bad
  address exits 1 with one `sig:` line.
- The crates' tests: api, config, config-example, core (its unit and integration tests),
  runtime (the access-list and replace test, the signal queue, the download names among them),
  semantic and tui (its console unit tests among them), all passing.

**macOS (`macos-latest`, Apple Silicon), verified.** Run 43838694058, job "macOS (terminal
only)", every step green, 880 tests:

- `tests/tui.rs` with the demo engine (46 tests: `sig --tui` and `--dump` in a pty with a fake
  terminal: kitty and sixel frames, keys, the address line, text mode, find, the clipboard,
  SIGTERM, resizes, floods and garbage) and again with the Wasm engine (48: sig's own pages and
  home's links to them too), all passing; first all green in run 43826803295. One of the 48 is
  intermittent there and now ignored on macOS with the Wasm engine ([open](#macos-1)).
- The crates' tests, `frontends/sighurt-tui/tests/pty.rs` (raw mode and its restoration after a
  panic, SIGABRT, a SIGSEGV fault, `kill -SEGV`, SIGTSTP and SIGCONT, a forced exit) and the
  kqueue configuration watch among them, all passing; first all green in run 43835363647.

**Linux, verified** on this machine after the changes (commands in CLAUDE.md's "Before a
commit", without Servo): `cargo fmt --all -- --check`; both clippy lines; both test lines; `cargo
test -p sighurt-browser --no-default-features --features frontend-tui,engine-demo`; `cargo check
-p sighurt-browser --no-default-features`, and with `frontend-gui,engine-demo`. `tools/cross-check`
for both targets, on `sig` (`frontend-tui,engine-demo`, with its tests) and on sighurt-tui and
sighurt-runtime with their tests: no error and no warning.

**Assumed, not run:** any real terminal emulator on either system (iTerm2, kitty, Ghostty,
WezTerm, Terminal.app; Windows Terminal, the classic console window); `/usr/bin/open` and
`ShellExecuteW` opening an address; Intel Macs; Windows on ARM; the console's behaviour when
Windows Terminal resizes or passes sixel through; that the access list keeps another user out
(the test reads the list back, it does not log in as someone else); the Wasm engine on Windows.
