# sighurt-engine-servo (`sig-servo`)

The Servo content engine for Sighurt. `sig-servo` embeds [Servo](https://servo.org) 0.5 headless to render HTML, CSS and JavaScript. It runs as a separate process and speaks the [`sighurt-ipc`](../sighurt-ipc/src/lib.rs) protocol over stdin/stdout.

The shell never links Servo. It spawns one `sig-servo` process per page, sends navigation, resize and input messages, and displays the frames that come back. Servo's dependencies, global state and crashes stay out of the browser process.

## Building

This crate is its own Cargo workspace, with its own `Cargo.lock`. The root workspace excludes it. Build it from the repository root with:

```bash
cargo build --release --manifest-path sighurt-engine-servo/Cargo.toml
# -> sighurt-engine-servo/target/release/sig-servo
```

- **The first build is long** and needs a lot of disk space. Servo is a large dependency tree.
- **Debug builds optimize Servo too.** Unoptimized, Servo is slow enough that heavy sites look broken: YouTube never got past its placeholder page. So the dev profile builds every dependency at `opt-level = 3`, and only `sig-servo` itself is unoptimized. The first `cargo build` then takes about as long as a release build: 9 to 13 minutes on 12 cores, against about 4 unoptimized. Rebuilding after a change to this crate is still quick.
- **Native build dependencies**: a C/C++ toolchain, clang and libclang (for bindgen), cmake, python3, pkg-config, fontconfig, freetype, EGL/OpenGL (Mesa), xkbcommon, X11 and Wayland development libraries, and m4. On Debian/Ubuntu that is roughly:

  ```bash
  sudo apt-get install build-essential clang libclang-dev cmake python3 pkg-config m4 \
    libfontconfig1-dev libfreetype6-dev libgl1-mesa-dev libegl1-mesa-dev \
    libxkbcommon-dev libx11-dev libwayland-dev libudev-dev libdbus-1-dev
  ```

  On NixOS, build inside a nix shell that provides these packages and sets `LIBCLANG_PATH`.
- **SpiderMonkey** (`mozjs`) is downloaded as a prebuilt archive by default, so the build needs network access. Set `MOZJS_FROM_SOURCE=1` to build it from source instead, which is much slower and needs more tools.
- At runtime `sig-servo` loads `libEGL` dynamically. Mesa/libglvnd must be installed and findable, which on NixOS means on `LD_LIBRARY_PATH`.

Check it the same way:

```bash
cargo check --manifest-path sighurt-engine-servo/Cargo.toml
cargo clippy --manifest-path sighurt-engine-servo/Cargo.toml -- -D warnings
```

## How the shell finds it

The built-in config (`sig --default-config`) defines a `servo` engine with `command = ["sig-servo"]`. A bare program name like that is looked up **next to the `sig` binary** first, then on **`PATH`**. So either:

- install it onto `PATH` (`~/.cargo/bin`): `cargo install --locked --path sighurt-engine-servo`,
- copy `sighurt-engine-servo/target/release/sig-servo` into the directory that contains `sig` (`target/debug/` when you use `cargo run -p sighurt-browser`), or
- point the config at it: `[engines.servo] command = ["/path/to/sig-servo"]` in `config.toml` (see the [README](../README.md#configuration) for where that file lives).

`sig://home` shows whether the program was found. Web pages reach this engine because `default_engine = "servo"` and because the built-in config gives it HTML, XHTML, plain text, SVG and image types, their file extensions, and `data:` and `about:` URLs. See the [README](../README.md#routing) for the routing rules.

## Preferences

`sig-servo` starts Servo with its default preferences and these changes (all in `preferences()` in `src/main.rs`):

- **Web platform features that Servo 0.5 implements but leaves off**, and that common sites use: CSS grid, multi-column layout, container queries and variable fonts, `document.fonts`, `execCommand`, IndexedDB, `OffscreenCanvas`, the Permissions and Storage APIs, and WebGL 2. Grid matters most. Without it, mozilla.org and theguardian.com lose their layout.
- **`SharedWorker` off.** A page's shared worker never exits, so Servo can't shut down (x.com). The engine then lingers until its 5-second watchdog kills it, holding the profile lock, and the next page waits up to 3 s for the lock. Sites fall back as they do in browsers without `SharedWorker`, and x.com looks the same without it.
- **`layout_threads = 2`** instead of Servo's 3. With 3, parallel styling races and panics a style thread on developer.mozilla.org in about 40% of loads (14 of 36 in our runs), and the page stops rendering. With 2 it didn't happen in 30 loads. Page load times didn't change measurably.
- **A Firefox user agent**: Servo's, with its `Servo/0.5.0` token replaced by Firefox's `Gecko/20100101`, so it reads exactly like Firefox 140 on the same platform. Servo's own already claims Firefox 140, but some sites turn away anything else: web.whatsapp.com asks for "Firefox 115+" instead of showing its login.

Two features stay off because they break common sites in Servo 0.5:

- **`IntersectionObserver`.** With it on, the page's script thread spins forever: reddit.com, bbc.com, amazon.com, cloudflare.com, theguardian.com and microsoft.com stop rendering, and Servo can't shut down. Servo 0.5's layout gets stuck looking for an element's containing block when an ancestor has no box, such as a `display: contents` element. Upstream fixed this with a one-line change in [servo/servo#47693](https://github.com/servo/servo/pull/47693), which is **not** in Servo 0.6.0 either. Without the API, scripts that use it throw instead. It is the most common script error on the sites we tested (228 errors on bbc.com), and it breaks openai.com. A JavaScript stand-in injected as a user script would fix that, but Servo 0.5 panics when it runs a user script in an `<iframe sandbox>` without `allow-scripts`, which takes down the whole page (amazon.com).
- **`adoptedStyleSheets`.** With it on, layout panics (microsoft.com).

Any Servo preference can be set with `--pref NAME=VALUE`. The names are the fields of Servo's `Preferences` struct. For example, in `config.toml`:

```toml
[engines.servo]
command = ["sig-servo", "--pref", "fonts_sans_serif=Liberation Sans", "--pref", "fonts_default=Liberation Sans"]
```

**Fonts.** Servo takes the generic `serif`, `sans-serif` and `monospace` fonts from fontconfig (`fc-match sans-serif`). If fontconfig maps `sans-serif` to a monospace font, most pages show in that font. The two preferences above fix that without changing fontconfig.

## Profile

Servo keeps cookies, local storage, HSTS and HTTP authentication state in `{data_dir}/sighurt/servo` (`$XDG_DATA_HOME/sighurt/servo`, by default `~/.local/share/sighurt/servo`, on Linux), so they survive restarts. It reads the files when an engine starts, and writes cookies, HSTS and authentication state back when the engine shuts down cleanly. A lock file stops two engines from reading or writing them at the same time.

Every page is its own process with its own copy of the cookies, which has two consequences:

- Pages don't share cookies while they run. A login in one tab reaches other tabs only once that tab has closed and the other tab has been reopened.
- When several pages close, the last one to shut down writes the cookie file. Cookies that the others changed are lost.

If the directory can't be created, nothing is saved.

## Rendering path

1. Servo renders into an offscreen GL context sized to the viewport from `ToEngine::Resize`. By default this is a GPU context (`src/rendering.rs`). If one can't be created, `sig-servo` falls back to Servo's `SoftwareRenderingContext` and says so on stderr. The GPU context starts Servo's frames at most 60 times a second instead of Servo's 120, because every frame is copied to the shell. On an animating page (discord.com) that halves the frames sent (111 to 55 per second) and cuts the engine's CPU use from about 100% to 60% of a core. Scrolling is not affected.
2. When Servo reports a new frame, `sig-servo` paints the webview and reads the framebuffer back into a reused buffer.
3. If the pixels differ from the last frame sent, it sends them to the shell as `FromEngine::Frame`: tightly packed RGBA8 rows in physical pixels. Servo also reports frames for changes that aren't visible, such as a hover style that looks the same. Those frames are dropped.
4. The shell displays the frame as an image. Mouse, wheel and key input go back as `ToEngine` messages, and Servo's URL, title, load state, history, cursor, status text and console messages come forward as `FromEngine` messages.

The real stdout is reserved for the protocol. At startup `sig-servo` points file descriptor 1 at stderr, so stray prints from Servo or its dependencies cannot corrupt the message stream. Logs go to stderr, which the shell inherits.

Links that open a new browsing context (`target="_blank"`, `window.open`) are not shown by `sig-servo`. Their URL is sent to the shell as `FromEngine::Open { new_tab: true }`, and the shell routes it like any other navigation.

### GPU and software rendering

Servo 0.5 has no headless GPU context, only `SoftwareRenderingContext`. `src/rendering.rs` adds one: the same surfman setup on the adapter Servo uses for windows and WebGL, rendering into a single offscreen surface because frames are read back rather than presented. On Linux, surfman connects to Wayland, then X11, then surfaceless EGL, so no window or display server is needed. The file is adapted from Servo's `rendering_context.rs` and stays under Servo's MPL-2.0 licence.

"Software" is less certain than it sounds. surfman's software adapter only sets `LIBGL_ALWAYS_SOFTWARE`, which only Mesa reads. With the NVIDIA driver, `SoftwareRenderingContext` renders on the GPU too. To get real CPU rendering on a system with libglvnd, also point EGL at Mesa: `__EGL_VENDOR_LIBRARY_FILENAMES=/path/to/50_mesa.json`.

### Environment variables

| Variable | Effect |
|---|---|
| `SIG_SERVO_RENDERING=hardware` | Use the GPU context only. If it can't be created, the engine reports an error. |
| `SIG_SERVO_RENDERING=software` | Use Servo's `SoftwareRenderingContext` only. |
| (unset) | Try the GPU context, then fall back to software. |
| `XDG_DATA_HOME` (Linux) | Moves the profile to `$XDG_DATA_HOME/sighurt/servo` (see [Profile](#profile)). |
| `SIG_SERVO_TIMING=1` | Log to stderr: the context kind and GL renderer, and for every frame the paint, readback and send times. Also logs the time from each navigation to its first frame. |

### Measured numbers

These were measured on an RTX 3070 (NVIDIA 595 driver, Wayland) and an i5-12400F, with a 1280×800 viewport and a debug build (Servo's crates at `opt-level` 0). The driver loads each page in a fresh engine, then sends a wheel event every 16 ms for 4 s. Times are per-frame averages from `SIG_SERVO_TIMING`.

| Context (GL renderer) | Paint | Readback | Send | Scrolling servo.org | Scrolling Wikipedia |
|---|---|---|---|---|---|
| GPU (NVIDIA) | 0.9–1.3 ms | 3.0–3.4 ms | 1.4–2.2 ms | 48–50 fps | 11–13 fps |
| `SoftwareRenderingContext` (also NVIDIA, see above) | 0.9–1.4 ms | 3.2–3.3 ms | 1.5–2.1 ms | 49 fps | 11–13 fps |
| `SoftwareRenderingContext` forced onto Mesa (llvmpipe) | 12–23 ms | 1.0–1.3 ms | 1.3–1.5 ms | 41–45 fps | 12 fps |

- **The first frame is slower.** It took 3–10 ms to paint on the GPU and 234 ms on llvmpipe.
- **Time to first frame** was 0.2–0.6 s from spawning the engine, whichever context was used. On this machine, each context took 60–210 ms to create.
- **Rendering is not the bottleneck** in these runs. servo.org scrolling is limited by the 62.5 Hz input rate. Wikipedia is limited by Servo's own layout and scene building, which are very slow unoptimized.
- **Duplicate frames:** before duplicates were dropped, 2–5 frames per page were sent identical to the one before. Now none are.

Servo is much faster optimized, which debug builds now are too: Wikipedia finished loading in 1.2 s instead of 2.3 s. Readback costs more on the GPU than on llvmpipe because it waits for the GPU to finish and copies across PCIe, but paint plus readback is still 3–5× cheaper.

## Site compatibility

Tested on 2026-09-26 with a release build, a 1280×800 viewport and a small protocol driver that loads each page in a fresh engine and inspects the last frame. 30 common sites:

- **Render and work (28):** Wikipedia, GitHub, Hacker News, old.reddit.com (shows its "log in to use old Reddit" wall), reddit.com, DuckDuckGo (both), Google, YouTube, bbc.com, Stack Overflow, docs.rs, crates.io, rust-lang.org, servo.org, mozilla.org, archlinux.org, lobste.rs, amazon.com, apple.com, cloudflare.com, wiki.nixos.org, developer.mozilla.org, theguardian.com, x.com, microsoft.com, web.whatsapp.com, discord.com. First frame in 0.2–1.1 s, load complete in 0.4–4 s. Clicking, typing into forms, submitting them and going back work. Cookie banners show and can be clicked.
- **Blocked by bot detection (2):** nytimes.com shows a "confirm you are human" slider. openai.com shows a Cloudflare challenge that never completes (curl gets a 403 there too).
- **Broken by a missing web API (1):** openai.com, when it gets past Cloudflare, shows "This page couldn't load" because `IntersectionObserver` is off (see [Preferences](#preferences)).
- **Many sites log script errors** that don't stop them from rendering. Most are `IntersectionObserver is not defined` (bbc.com, cloudflare.com, discord.com, developer.mozilla.org).

Most remaining problems are interaction, not rendering: `<select>` dropdowns, dialogs, file pickers, context menus, IME and HTTP authentication are not implemented yet (see below).

## Current limitations

- **Many sites don't load or work correctly.** Servo's web platform support is still incomplete, and the embedding gaps below (dialogs, `<select>`, IME, media) add to it.
- **Every frame is copied in full through a pipe**, 4 MB at 1280×800. No shared memory is used. Per frame, the GPU readback (about 5 ms, most of it waiting for the GPU) costs more than painting or sending. With the software fallback, animations still run at Servo's 120 Hz.
- **No find-in-page.** The protocol has no find messages yet.
- **No IME composition.** Only plain key events are forwarded.
- **No `<select>` popups, context menus or dialog UI** (`alert`, `confirm`, `prompt`, file pickers, permission prompts, HTTP authentication) yet. Servo answers each one as dismissed. A `<select>` can't be opened, `alert` returns at once, `confirm` returns `false`, `prompt` returns `null`, file inputs stay empty and permission requests are denied.
- **Stop is partial.** Servo 0.5 has no stop API, so `ToEngine::Stop` runs `window.stop()` in the page and then reports loading as finished. This aborts parsing and pending fetches such as images. It can't cancel a navigation whose response hasn't arrived yet.
- **Focus is only tracked by Servo.** `ToEngine::Focus` calls `WebView::focus`/`blur`, including a focus that arrives before the page exists. In Servo 0.5 this has no effect inside the page. For example, `document.hasFocus()` stays true.
- **No media playback.** Servo is built without GStreamer, so `<video>` and `<audio>` don't play.
- **One Servo per process.** Servo can't be restarted inside a process.
- **Some pages never finish loading** as far as Servo is concerned (www.reddit.com, nytimes.com, discord.com): the page shows, but the shell's loading indicator keeps spinning.
- **Crashes are reported as errors.** When a page thread panics, Servo shows a crash page and the shell gets `FromEngine::Error`. The engine keeps running. Servo doesn't notice a panic on one of its style threads, and the page stops rendering, so `sig-servo` sends that as `FromEngine::Error` itself. A race in Servo 0.5's parallel styling did this to developer.mozilla.org in about 40% of loads, which is why `sig-servo` uses 2 layout threads (see [Preferences](#preferences)). A panic in the engine's main thread is also sent as `FromEngine::Error`, and then the process exits. So is Servo losing its backend.

## License

`Apache-2.0 AND MPL-2.0`. `src/rendering.rs` is adapted from Servo and stays under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/); the rest of the crate is Apache-2.0 like the rest of Sighurt. `sig-servo` links Servo, which is MPL-2.0. See [NOTICE](../NOTICE).
