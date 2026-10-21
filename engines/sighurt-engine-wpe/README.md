# sighurt-engine-wpe

The WPE WebKit engine for Sighurt: web pages with WebKit, for compatibility while Servo matures
(Servo stays the long-term engine). A library that `sig` compiles in (feature `engine-wpe`, one of
`sighurt-browser`'s default features; the engine `wpe` of its configuration) and runs in its
process: WebKit's UI-process half runs on `sig`'s owner
thread, pages run in WebKit's own web and network processes. See
[`docs/rewrite/WPE.md`](../../docs/rewrite/WPE.md) for the design, the evidence and the limits, and
`docs/rewrite/STATUS.md` ("WPE") for where it stands.

It needs WPE WebKit 2.52 from `nix/build-wpewebkit.sh`; the build script finds it only through
`SIGHURT_WPE_PKG_CONFIG_PATH`, which `shell.nix` sets when the prefix exists (`tools/cargo` runs
in that shell), and fails with a message naming the script otherwise. The library is not linked:
`WpeRuntime::new` opens it (`ffi::load`) by the path the build script found, so a program with the
engine compiled in loads WebKit only when it starts the runtime.

## Using it

`WpeRuntime::new(WpeConfig { profile, user_agent, .. }, waker)` once per process (a second is
refused), on the owner thread, before WebKit is used: it sets JavaScriptCore's options that keep it off
signals in this process (decision 66) and takes GLib's default main context, whose watch thread
wakes the owner through the `EngineWaker` (decision 64). The runtime implements `EngineRuntime`
with `sighurt_wayland::TargetLease` by default (a `PresentMode::Gl` target per view: the
presenter, not the host, attaches and commits) and `FramePermit`. `WpeRuntime<Offscreen<C>>`
presents into any `CpuTarget` with no Wayland connection: WebKit's shared-memory frames
(`WPE_PIXEL_FORMAT_ARGB8888`, premultiplied, top-down) are copied into the lent frame, counted
in `diagnostics().present.shm_copies`, and acknowledged only when `present` holds a permit, so
the frontend's frame rate bounds WebKit. `SetMetrics` resizes the view; an empty metrics keeps
the last size. `CAPABILITIES` is what it supports; `diagnostics()` names the transport, the DRM
device and driver, frames, copies and present times, and `Diagnostics::renderer()` says it in
one line. `libgl_always_software` and `dri_prime` record those variables as the first view opens,
just before WebKit starts the processes that inherit them.
`tests/common/mod.rs` is an owner loop that does all of this; `examples/wpe-window.rs` is a window
built on it:

```sh
tools/cargo build -p sighurt-engine-wpe --example wpe-window
tools/heavy tools/headless-sway --renderer gles2 -- target/debug/examples/wpe-window --exit-after 10 PAGE...
```

At run time WebKit's network process needs a GIO TLS module for `https:` (`GIO_EXTRA_MODULES`),
and media need GStreamer's plugins and an audio server; `shell.nix` sets the first two.

JavaScriptCore suspends threads with SIGUSR1 unless `JSC_SIGNAL_FOR_GC` names another signal, and
its handler replaces the process's and suspends whichever thread receives that signal, for good
when nobody resumes it. An executable that uses SIGUSR1 sets the variable before any thread
starts: `sig` sets 12, SIGUSR2 (decision 78). JavaScriptCore then logs "ERROR: invalid option:
JSC_SIGNAL_FOR_GC=12" once per process, since it reads every `JSC_` variable as an option of its
own too; WebKit's thread code takes the signal from it all the same.

## How it maps onto WebKit 2.52

- **Embedding**: the WPE Platform API, with Sighurt's own `WPEDisplay` and `WPEView` (GObject
  subclasses in Rust) and WebKit's stock `WPEToplevel`; also the display's clipboard and each
  view's input-method context.
- **Presentation**: WebKit's DMA-BUFs attached to the view's subsurface as `wl_buffer`s, no CPU
  copy; where the compositor has no DMA-BUF (or with `force_shm`), shared memory with one copy per
  frame, logged and counted. Frames are paced by the host's permits and frame callbacks. A CPU
  target (`Offscreen`) never connects to Wayland: the same shared-memory frames are copied into
  the lent buffer and released when presented, so WebKit waits while permits are withheld.
- **Navigation and history**: WebKit's back-forward list is the truth; commits get their
  `HistoryEffect` from it by entry identity. The browser's latest request takes the next load
  WebKit starts; the page's own loads and same-document navigations get ids of their own
  (`src/navigation.rs` has the rules and their limits).
- **The page's navigations**: a link or a GET form the user clicked (WebKit's user-gesture flag
  and navigation type) is held as WebKit's policy decision and asked about with
  `NavigationRequested`; the answer uses or ignores it, and dropping it ignores it. Scripts'
  navigations, frames loading, POST forms and the browser's own loads go ahead without asking.
  WebKit doesn't say which frame navigates, so a link clicked in a frame is asked about too.
- **Requests**: permissions (geolocation, camera, microphone, screen capture), file pickers,
  downloads and the clipboard are service requests; unanswered or refused, they fail closed.
  New windows and other schemes go to the browser as `OpenRequested`; `window.close()` as
  `CloseRequested`. HTTP authentication, context menus, `<select>` popups, fullscreen and
  WebKit's own geolocation provider are refused.
- **Input**: keys with keysyms and hardware keycodes, pointer and wheel, focus, and input methods
  through the host's text input (preedit, commit, surrounding text, the caret; passwords without
  their text).
- **Failures**: a web process that dies is `ViewFailed`, and a reload starts a new one; a broken
  compositor connection is `RuntimeFailed`.

## Tests

- `tools/cargo test -p sighurt-engine-wpe --lib`: navigation rules, input, the input-method
  signals on WebKit's own class, the GLib loop, without a compositor.
- `engines/sighurt-engine-wpe/tests/headless.sh [pixman]`: the compositor test (`tests/engine.rs`, plan
  §25's engine tests) in a private headless sway on a D-Bus session of its own.
- `engines/sighurt-engine-wpe/tests/offscreen.sh`: the CPU target (`tests/offscreen.rs`) with no display,
  on a private D-Bus session. `tests/tui-wpe.sh [OUT]` runs `sig --tui --engine wpe
  https://example.com/` in a pty, decodes the last kitty image to `OUT/wpe/page.png`, and checks
  that WebKit's processes inherit neither `LIBGL_ALWAYS_SOFTWARE` nor `DRI_PRIME`, also with a
  Servo tab in the same process, and that none outlives `sig`.
- `tests/window-checks.sh`: `wpe-window` driven with input, the clipboard, scales, tabs, two views,
  a video, history, a crash and an https page.

## License

AGPL-3.0-or-later, like the rest of Sighurt. WPE WebKit is loaded dynamically and keeps its own licenses
(LGPL-2.1 and BSD).
