# Sighurt on macOS and Windows: the terminal there, and what a port still needs

Sighurt is a Linux browser: the window is Wayland's, and WPE WebKit, Ladybird and NetSurf are
built for Linux. The user decided on 15 November 2026 that macOS and Windows usability should be
worked on, which amends decisions 2 and 89 ("Wayland only"); the research behind this file is
`reports/Sighurt Windows and macOS support.md` and its notes (in the main checkout, untracked).
It recommends porting in layers, cheapest first. The first layer, portability changes verified
on Linux, was the `port` branch ([below](#what-the-groundwork-changed)). The second, done on the
`port2` branch, is the terminal-only `sig` on macOS and on Windows: it runs on both, proven on
GitHub's hosted runners (`.github/workflows/ports.yml`). The `r3-port` branch added the
terminal pages' programs on macOS, and on Windows the Wasm engine (without video), a
configuration watch, a report request and the console restored after a native crash. A window
on either is not started, and no web engine has run on either.

## What runs where

What GitHub's `macos-latest` (Apple Silicon) and `windows-latest` (x86_64) runners show, run
by run, is in [Verified and assumed](#verified-and-assumed). Four kinds of evidence are kept
apart, each for a named feature set ([below](#evidence-by-feature-set)): *checked* is `cargo
check` for the target on this Linux machine (`tools/cross-check`: nothing linked, no C
compiled); *links* is built for the target on its runner; *tested* is tests run on the runner,
driving the real `sig` with a fake terminal on a pty (macOS) or in a ConPTY pseudoconsole
(Windows); *tried* is a real terminal emulator, which only Linux has had (Rio and kitty,
`frontends/sighurt-tui/tests/real-terminals.sh`). "Proven" in this table means tested;
"built" means it links on the runner and nothing tests it.

| | Linux | macOS | Windows |
|---|---|---|---|
| `sig --tui` (the terminal frontend) | yes | proven: `tests/tui.rs` in a pty, demo and Wasm engines | proven: `tests/console.rs` in a pseudoconsole (ConPTY), demo engine |
| `sig --dump URL` | yes | proven (`tests/tui.rs`) | proven (`tests/console.rs`) |
| Demo engine | yes | proven | proven |
| Wasm engine, sig's own pages (`sig://help`, home's links) | yes | proven by `tests/tui.rs` (FFmpeg 7 from Homebrew); `sig`'s own unit tests are run with the demo engine only | proven without video: the engine's unit tests (guests under Wasmtime), and `tests/console.rs` opens `sig://help`, which loads without an error (a report says so); video refuses (no FFmpeg, `video_none.rs`) |
| `sig://terminal`, `sig://config` programs | yes | proven: the runtime's pty tests (kqueue `EVFILT_PROC` watch, probes without `/proc`) and `tests/tui.rs`'s shell, editor and signal tests | refused, saying so (`pty_unsupported.rs`): no ConPTY yet |
| Configuration watch | inotify | kqueue (`EVFILT_VNODE`), proven by its unit test, which since run 44044117586 also checks that the read leaves nothing to wake the loop | `ReadDirectoryChangesW` on a thread, proven by its unit test and by `tests/console.rs` (a binding written while `sig` runs is used) |
| Quit and report requests | signals | signals (SIGUSR1 report; SIGTERM, SIGINT, SIGHUP quit) | console control handler: Ctrl+Break and closing the console quit; Ctrl+C is a key in the terminal and a quit in `--dump`; setting the event `Local\sighurt-report-<pid>` asks for the report (`tests/console.rs`) |
| Restoring the terminal on a crash | panic, SIGABRT, SIGBUS, SIGFPE, SIGILL, SIGSEGV | the same code; `frontends/sighurt-tui/tests/pty.rs` tests a panic, SIGABRT (sent, and chained), a SIGSEGV fault and `kill -SEGV`; SIGBUS, SIGFPE, SIGILL, a stack overflow and a recovered Wasmtime trap are not tested there | a panic, and an exception nothing handles (an unhandled-exception filter): `tests/console.rs` crashes a process in raw mode with an access violation and finds the console's modes as before; `abort` and `TerminateProcess` still leave them |
| Private files | 0700 directories, 0600 files | the same | a protected access list (the user and SYSTEM) on Sighurt's directories, given to an existing one whose list is wider and read back; files made in them inherit it (unit test on the runner); another local user, logged on and impersonated, is refused a file in one and reads one beside it (unit test, runners only) |
| External addresses | `xdg-open` | `/usr/bin/open` (built, not tested) | `ShellExecuteW`, http, https and mailto only (built, not tested) |
| The chrome's wgpu renderer against the CPU one (`render/sighurt-render-wgpu/tests/compare.rs`, offscreen; there is no window here to put it in) | Vulkan | Metal on the runner's Apple Paravirtual device: every comparison within its tolerances (run 44066514925: standard chrome worst difference 4, line chrome 1, item kinds and clips 26 at corners, random scenes 31 at corners, text 1) | DX12 on WARP (Microsoft Basic Render Driver, the runner's only adapter): within the same tolerances when it runs (run 44068580453: text 2, standard chrome 4, line chrome 0, kinds 26, random 29), but the test process once died with an access violation while four tests opened devices at once (run 44066514925), and passed the next time; not blocking there |
| The address line | paths from `/` and `~/` | the same | also `C:\…`, `C:/…` and `\\server\share\…` (the router's unit test) |
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
  quit and fails reads and writes). `main` quits cleanly for Ctrl+C (in `--dump`) and
  Ctrl+Break; a second one is left to Windows, which ends the process at once. The diagnostics
  report is asked for by setting the auto-reset event `Local\sighurt-report-<pid>`, which
  `main` makes with the token's default security (only the user and the system can set it) and
  a thread of its waits on, taking at most one request a second. An event of that name that
  already exists was made by another process (perhaps set and manual-reset, which would ask
  for a report on every wait): `main` lets it go and takes no report requests, saying so on
  stderr.
  Closing the console, logging off and shutting down quit too, and the handler does not return
  until `main` has (Windows ends the process when it returns, and allows 5 s after a close).
- **A native crash** (an exception nothing handles: an access violation, a stack overflow)
  reaches an unhandled-exception filter the session installs, on the crashing thread. A console
  write blocks while output is stalled, so a helper thread started with the filter writes what
  closes an open sequence and the leave sequence (which need the console's VT mode); the filter
  waits for it at most 200 ms, then puts the modes and code pages back, takes no lock, and hands
  the exception on either way. What
  ends the process without an exception (`abort`, Rust's `__fastfail`, `TerminateProcess`)
  still leaves the modes as they were.
- **The configuration watch** is a thread that waits on `ReadDirectoryChangesW` for the
  configuration's directory (overlapped, with a stop event) and raises a flag when a change
  names the file (written, created, deleted, renamed to or from; a full buffer or
  `ERROR_NOTIFY_ENUM_DIR`, changes lost to an overflow, counts as a change, and the watch goes
  on); the wake rings at a deadline 100 ms after the last change to the file, which changes to
  other files do not move (the session and the history are written in that directory), so an
  editor's burst of writes is read once. The stop event comes first in every wait (the lowest
  signalled handle wins), so a storm of changes cannot keep the thread from ending; only a
  real read failure ends the watch, with a warning.
- **`--dump`** writes the page from a thread of its own and waits for it with the dump's
  deadline: Windows has no non-blocking write to a pipe or a console.
- **Starting it**: a console has no `TERM`, so on Windows a console on stdout is the terminal
  unless `TERM` is `dumb`. Windows Terminal has sixel (1.22 and later) and no kitty graphics; a
  console that answers no image protocol, as ConPTY's does in the tests, gets text mode.
- **The Wasm engine** builds without FFmpeg: `ffmpeg-next` is a dependency everywhere but
  Windows, and there the decoder (`video_none.rs`) fails every open, so `api_video_load` and
  `api_video_load_url` return their failure code and the console says why. Wasmtime's C
  helpers, zstd and mozjpeg build with the runner's MSVC as they are (run 44032936386). The
  engine's Wayland dev-dependency is Linux's, so only its unit tests (`--lib`) build elsewhere.
- **The address line** takes Windows' absolute paths: a drive's (`C:\…`, `C:/…`) and a
  share's (`\\server\share\…`) become `file:` addresses; `\x` without a drive stays text.
- **Files**: sessions and history are replaced with `MoveFileExW` (replace, write-through),
  retried five times over about 150 ms on a sharing, access or lock violation (an indexer or a
  virus scanner holding the file). The session lock is its own file, as on Unix. A directory
  `sig` creates gets a protected access list granting full control to the user and to SYSTEM,
  inherited by what is made in it. That is the 0700/0600 rule as far as Windows has one: no
  other ordinary user can read the files and the profile's inherited entries are cut; unlike
  0600, SYSTEM can read them and an administrator can take ownership. A directory that already
  exists gets the list when its own is wider (not protected, letting anyone else in, or not
  inherited by files), as Unix tightens a wider one to 0700, and the list is read back after it
  is set; files already in it keep entries of their own. Download names also lose what Windows refuses (`\ / : * ? " < > |`,
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
- rustix takes a `&mut Vec` as a buffer of the Vec's *length*, not its capacity: `kevent`
  handed an empty Vec asks for no events and waits for none. The configuration watch read
  its events that way, so it never read any: after the first change in the directory (where
  macOS also keeps the session and the history, written on every tab change) the kqueue stayed
  readable and the owner loop woke at once every time, for as long as `sig` ran. Both kqueue
  users now read into the Vec's spare capacity (`rustix::buffer::spare_capacity`).
- The terminal pages' programs: the runtime's `pty.rs` is Unix's. A kqueue with the program's
  `EVFILT_PROC`/`NOTE_EXIT` replaces the pidfd, and an `EVFILT_USER` event, triggered when the
  page's session is dropped, replaces the pipe the watcher waited on: no child inherits a
  kqueue, so nothing another child of `sig` holds can keep the session from ending, as a pipe
  made without `pipe2` could. An exit before the watch is found with `waitid(WNOWAIT)`. The
  master is `/dev/ptmx` opened with `O_CLOEXEC` (`posix_openpt` takes no such flag there);
  the child marks close-on-exec every descriptor below the soft limit of open files or, above
  it, below the highest one open when the parent looked (`/dev/fd`, read before the fork: one
  opened before the limit was lowered can only be among those), in place of `close_range`. `poll` on a pty master works (the Wasm engine's pty tests). The tests' probes
  need no `/proc`: the shell's own descriptors by builtin redirection, Python for the inherited
  dispositions and mask (`ps` there has no ignored-signals column), `ps -o pgid,tpgid,stat,tt`
  for the session, `F_GETPATH` and `fstat` for this process's descriptors, `kill(pid, 0)` for a
  process being gone.
- **The "stall" of `a_megabyte_of_garbage_as_the_probes_answer_is_survived`** was no stall.
  The megabyte answering the probe outlives the probe's deadline there (the pty holds about a
  kilobyte, so the answer trickles), and what comes after the deadline is typing: some 500 000
  keystrokes, which with the default keys open hundreds of `sig://help` tabs, each a Wasm
  guest. `sig` takes such typing at about 20 KiB a second, on Linux too (typed on louise-pc,
  and in the temporary experiment of run 44040226305 on macOS), so the test's watchdog killed
  `sig` at 40 s and the fake terminal then waited 30 s on a dead process (the failed runs'
  `tui.log` is empty; the experiment's report shows `sig` a zombie). The test's keys now only
  reach the page (`[input] keymap = "bindings"`), so it tests the answer and not a thousand
  Wasm tabs; it is no longer ignored.

## What the groundwork changed

The `port` branch's changes, as they were made; three rows are superseded by the terminal on
Windows above (the signals, the terminal off Unix, `open_external` on Windows) and one by the
macOS configuration watch (and Windows' missing watch is now a notice, not only a log line).
On Linux `sig`'s default features, the frontend choice in `main`, every setting and every
command are what they were. One build recipe is not: the window is now the feature
`frontend-gui`, so a build with `--no-default-features` has to name it. The 1.0 recipe
`--no-default-features --features engine-demo` now builds a `sig` with no frontend, which says
so and exits; add `frontend-gui` (`chrome-line`, `render-wgpu` and `render-vulkan` turn it on).
The changes are features and `cfg`:

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
depends on it goes unchecked. That is a limit of this machine. Checking the Rust of the TLS
crates says nothing about their C or about linking: that they build on the systems themselves
is shown only by the runners. On `macos-latest` the terminal-only `sig` links with the demo
engine and with the Wasm engine (ring, aws-lc-sys, FFmpeg 7, Wasmtime's C helpers, zstd,
mozjpeg); on `windows-latest` it links with the demo engine (aws-lc-sys assembled with NASM)
and with the Wasm engine (Wasmtime's C helpers, zstd and mozjpeg with MSVC; no FFmpeg).

## The matrix

Each cell is `tools/cross-check <target>` on the crate (libraries only; `sig` as noted), on the
`port` branch: checked, nothing linked. "Clean" means no error and no warning from Sighurt's
code. The two Windows cells marked † describe the `port` branch: since `port2` the console
session (`term/console.rs`) is compiled for Windows, and `--tui` and `--dump` run there.

| Crate (features) | aarch64-apple-darwin | x86_64-pc-windows-msvc |
|---|---|---|
| sighurt-api, sighurt-core, sighurt-semantic, sighurt-config-example | clean | clean |
| sighurt-ui, sighurt-chrome-line, sighurt-render (also `gpu`) | clean | clean |
| sighurt-render-wgpu, sighurt-render-vulkan | clean (they compile; both bind Wayland surfaces only) | clean (same) |
| sighurt-engine-demo, sighurt-engine-cef | clean (CEF's loader refuses at run time) | clean (same) |
| sighurt-runtime (no engines) | clean | clean |
| sighurt-tui (lib; none, and `engine-demo`) | clean | clean (terminal I/O compiled out †) |
| `sig`, `--no-default-features --features frontend-tui,engine-demo` | **clean** | **clean** (`--tui` and `--dump` refuse †) |
| `sig`, `--no-default-features` (no frontend) | clean | clean |
| sighurt-engine-wasm (so `sig` with `engine-wasm`) | stopped by build scripts that compile C for the target: `ffmpeg-sys-next` (and its pkg-config), `wasmtime` and `wasmtime-internal-jit-debug` (Wasmtime's C helpers), `zstd-sys`, `mozjpeg-sys`, `objc_exception` | stopped the same way: `wasmtime`, `wasmtime-internal-jit-debug`, `wasmtime-internal-fiber`, `ittapi-sys`, `zstd-sys`, `mozjpeg-sys` (since the `r3-port` branch `ffmpeg-sys-next` is not a Windows dependency); all of them build on the runner with MSVC |
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

The terminal-only `sig` on Linux with the demo engine links no Sighurt Wayland crate
(sighurt-wayland, sighurt-gui, sighurt-ui, the renderers), checked with `cargo tree`. With the
Wasm engine it still pulls `wayland-client` in through its file picker (`rfd`'s XDG portal
backend, `ashpd`, a Linux-only dependency). With `engine-servo` it links sighurt-wayland
itself: the Servo adapter depends on it unconditionally. "Terminal-only" names the frontend,
not the dependency graph.

## Evidence by feature set

| `sig` built with `--no-default-features --features …` | Linux | macOS | Windows |
|---|---|---|---|
| `frontend-tui,engine-demo` | tested (here, CLAUDE.md's lines) | checked; links; tested (`tests/tui.rs`, `sig`'s unit tests) | checked; links; tested (`tests/console.rs`, `sig`'s unit tests) |
| `frontend-tui,engine-demo,engine-wasm` | tested (here) | not checked here (the Wasm engine's C); links; tested (`tests/tui.rs`, the Wasm engine's pty tests) | not checked here; links; tested (`tests/console.rs`, `sig`'s unit tests, the Wasm engine's unit tests), without video |
| none (no frontend) | checked | checked | checked |
| `frontend-gui,…`, `engine-servo`, `engine-wpe`, `engine-ladybird`, `engine-netsurf` | tested (CI's and CLAUDE.md's lines) | no (Linux only) | no (Linux only) |
| default features (with `engine-demo`) | tested; tried in Rio and kitty (`real-terminals.sh`) | no | no |

The crates' own tests on the runners (`cargo test -p sighurt-api -p sighurt-core -p
sighurt-runtime -p sighurt-config -p sighurt-config-example -p sighurt-semantic -p sighurt-tui
--features sighurt-tui/engine-demo`) are tested on both, `frontends/sighurt-tui/tests/pty.rs`
(the session's restoration and its faults) on macOS only: it is Unix's. Nothing on macOS or
Windows has been tried in a real terminal.

## What a port still needs

### macOS

1. **Real terminals.** The tests use a fake terminal on a pty. iTerm2, kitty, Ghostty and
   WezTerm (kitty graphics, sixel) and Terminal.app (text mode) have not been tried.
2. **A web engine in the terminal**: Servo, [below](#a-web-engine-in-the-terminal-on-macos-what-it-would-take).
   Until then text mode and `--dump` come from the demo engine (and pages from the Wasm engine
   as graphics).
3. **`/usr/bin/open`** for external addresses is not tested.
4. **Distribution**, if it is ever more than personal: signing and notarization.

### Windows

1. **Real terminals.** Only ConPTY (the console host the tests start) has run `sig`; Windows
   Terminal (sixel), the classic console window, and WezTerm have not. In ConPTY `sig://help`
   loads but is never seen: the console answers no image protocol, and sig's pages need
   graphics.
2. **Video in the Wasm engine**: FFmpeg through vcpkg (or the guests' video stays refused).
3. **ConPTY for the terminal pages**, which refuse until then: a pseudoconsole per program, the
   program in a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, inherited handles limited
   to the pseudoconsole's pipes (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), absolute program paths
   (`CreateProcess` searches the current directory), a closed choice of shells (`%ComSpec%`,
   Windows PowerShell, `pwsh`), the launch rules of the other systems (grants, activation, one
   program per document, eight at once); the Wasm engine's master becomes two pipe handles
   behind the same `PtySession` hand-over, and `ClosePseudoConsole` needs its output drained
   before Windows 11 24H2. Not started.
4. **`ShellExecuteW`** is tested only by unit tests on the runner: opening an address there
   would start a browser nobody watches.
5. **Servo** (its MSVC build and a non-Mesa offscreen context) and **CEF** (`LoadLibraryW` of
   `libcef.dll`, Windows' API hash, an `.exe` helper), as before.

### A web engine in the terminal on macOS: what it would take

No web engine has run on macOS or Windows; text mode and `--dump` there come from the demo
engine, and pages as graphics from the Wasm engine. Servo is the one candidate (WPE WebKit,
Ladybird and NetSurf are built for Linux; CEF on macOS needs an app bundle and a main-thread
loop). What it would take, each point marked *read* (in the pinned sources), *checked* (asked
of a live service) or *assumed*:

- **The context.** The adapter's terminal half opens `surfman::mesa_surfaceless` itself
  (`engines/sighurt-engine-servo/src/offscreen.rs`), which macOS has not. Servo 0.7's own
  `SoftwareRenderingContext` (servo-paint-api 0.7, `rendering_context.rs`) is
  `Connection::new()` and `create_software_adapter()`; on macOS surfman 0.14's default backend
  is CGL, and its `create_software_adapter` is `create_low_power_adapter`
  (`third_party/surfman/src/base/io_surface/connection.rs`): an IOSurface-backed context on the
  integrated GPU through Apple's deprecated OpenGL, not a software rasterizer, and it needs no
  window or display connection (*read*). Frames come back with `read_to_image`, one readback per
  frame as on Linux's terminal path (counted, decision 97). So on macOS the context is
  `SoftwareRenderingContext`, or `offscreen.rs`'s shape with CGL in place of the surfaceless
  device; nothing Mesa-like is needed (*read*). That OpenGL works headless on GitHub's
  `macos-latest` virtual machines (a paravirtualized GPU) is *assumed*.
- **The adapter's window half.** `runtime.rs`, `presentation.rs` and `target.rs` name
  sighurt-wayland types, and the crate depends on sighurt-wayland unconditionally (*read*), which
  does not build on macOS (the matrix above). The window half has to move behind a feature or a
  `cfg` before the crate compiles there: an adapter change, not a port of the window.
- **SpiderMonkey.** `mozjs_sys` 153.3.0-1 (the lockfile's) downloads a prebuilt static library
  from servo/mozjs's releases unless `MOZJS_FROM_SOURCE` is set (its `build.rs`, *read*), and that
  release has `libmozjs-aarch64-apple-darwin.tar.gz` and `libmozjs-x86_64-pc-windows-msvc.tar.gz`
  (*checked*: `gh release view mozjs-sys-v153.3.0-1 --repo servo/mozjs`, 17 November 2026). So
  the C++ engine is not compiled on the runner (*read*); the rest of Servo (about 1 000 crates)
  is.
- **The runner.** How long a cold Servo build takes on `macos-latest`'s three cores, and
  whether its target directory (Servo's dependencies at `opt-level = 3`) fits the runner's free
  disk, is not measured: an hour or more inside the job's 6-hour limit is *assumed*. A cache
  keyed on `Cargo.lock` would make later runs incremental.
- **The single riskiest assumption** is the headless OpenGL context on the runner: everything
  else is code that compiles or a download that exists. It is tested cheaply by an experiment
  binary (`experiments/tui-servo` with `SoftwareRenderingContext` on macOS) that loads one
  `data:` page and reads one frame back, built on the runner once; that has not been run yet.

### A window, on either

Out of scope here (the research's layer 4). It needs a host other than sighurt-wayland (child
NSViews or HWNDs, or one CPU-composited surface as a labelled fallback), Servo's native target
per system, a key table into evdev codes, and the renderers' non-Wayland surfaces (wgpu is
Vulkan-only by decision 125; that rule's reason is Linux's EGL). sighurt-gui stays Linux's until
then.

## Verified and assumed

The Ports workflow (`.github/workflows/ports.yml`) on GitHub's hosted runners, `port2` branch,
16 and 17 November 2026. Runs are at `https://github.com/gusahlg/sighurt/actions/runs/<id>`.

**After the outside review of the port** (GPT-6 Astra, six findings, all fixed): Ports run
44068580453, both jobs green, macOS 969 tests passed and Windows 962, none failed, with the new
tests: on macOS (and Linux) a descriptor opened above a lowered soft limit does not reach the
program; on Windows a sibling file written every 20 ms holds back neither the configuration's
wake nor dropping the watch in a storm of writes, and the crash, report-event and live-reload
tests pass with the bounded crash write and the refused foreign event. The overflow error
(`ERROR_NOTIFY_ENUM_DIR`) and a stalled console at a crash were not provoked on the runner. The
same run has the wgpu comparison (table above): blocking on macOS, not on Windows.

**The `r3-port` branch** (17 November 2026): Ports run 44044117586, both jobs green, macOS 964
tests passed and Windows 956, none failed (counted over every step; that run also had a
temporary step of experiments, since dropped). New in it:

- macOS: the runtime's pty tests (programs started, signalled, reaped and counted, their
  descriptors, dispositions, mask and session, the size and a resize, the exit status, the
  kqueue watch alone), the Wasm engine's pty tests (`--lib pty`), and `tests/tui.rs`'s
  `sig_terminal_runs_a_shell_in_the_terminal_frontend`,
  `sig_config_runs_the_editor_in_the_terminal`, `sig_config_gets_no_input_where_it_is_not_drawn`,
  `an_editors_end_reads_a_linked_configuration_with_no_more_input` and
  `signals_end_sig_and_the_shell_sig_terminal_runs`, with the Wasm engine; the configuration
  watch leaving nothing to wake the loop; the megabyte-of-garbage test not ignored. The first
  runs of the pty port (44036142883 and 44040226305) failed for the `kevent` buffer described
  above: no exit was ever seen.
- Windows: the Wasm engine built and its unit tests (183, one ignored); `sig` with it, whose
  `tests/console.rs` opens `sig://help` and finds it loaded in a report asked for through the
  report event; the configuration watch (its unit test, and in the pseudoconsole a binding
  written while `sig` runs is used); a process that crashes with an access violation in raw
  mode leaves the console's modes as before (`modes 0x1f7 0x7` both times); another local user,
  logged on and impersonated, is refused a file in a private directory and reads one beside
  it; the router's Windows paths. The first runs showed the Wasm engine's dev-dependency on
  sighurt-wayland (wayland-sys) failing there, now Linux's, and a text-painting bound broken by
  the first paint's font loading (2.3 s), now loaded before the clock starts.

**After review C's fixes** (the `fixc` branch, commit 12ad86c): Ports run 43923681446, both
jobs green, macOS 927 tests passed and Windows 757, none failed. They include the new tests:
on both, the signal flags (`a_quit_after_a_pipe_full_of_reports_is_taken` on macOS, the queue's
test on Windows) and the editor answer's `programs` record; on macOS `enter` refusing a pipe
with its flags unchanged and the image-id claim with no runtime directory; on Windows the access
list of an existing directory and of the files written in it, and the notice of the missing
configuration watch in the ConPTY test. CI run 43923681379 (ubuntu-26.04) is green too. The
runs below are the earlier ones.

**Windows (`windows-latest`), verified.** Run 43848358624 (the branch's last commit then), job
"Windows (terminal only)", both steps green: 723 tests passed, none failed (green also in runs
43824448752, 43826803295, 43830229164, 43835363647 and 43838694058). Run 43838694058's first
attempt failed one timing test whose bound is 700 ms on a shared runner
(`services::tests::quitting_deletes_a_download_its_server_keeps_waiting`); its second passed.

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

**macOS (`macos-latest`, Apple Silicon), verified.** Run 43848358624, job "macOS (terminal
only)", every step green: 879 tests passed, none failed (all green also in run 43838694058):

- `tests/tui.rs` with the demo engine (46 tests: `sig --tui` and `--dump` in a pty with a fake
  terminal: kitty and sixel frames, keys, the address line, text mode, find, the clipboard,
  SIGTERM, resizes, floods and garbage) and again with the Wasm engine (48: sig's own pages and
  home's links to them too), all passing; first all green in run 43826803295. One of the 48 is
  intermittent there and now ignored on macOS with the Wasm engine ([open](#macos)).
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
Windows Terminal resizes or passes sixel through; a sig page seen on Windows (ConPTY shows no
graphics); video in the Wasm engine on Windows (refused); the crash filter for a stack
overflow (only an access violation was tried); any web engine on either system.
