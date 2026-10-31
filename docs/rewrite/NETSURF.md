# NetSurf for Sighurt: feasibility, design and smoke test

NetSurf is a small browser with its own HTML, CSS 2.1 and DOM engine (libhubbub, libcss, libdom
and a layout written from scratch, in C, about 130 source files plus six libraries). This file
decides whether it can be a Sighurt engine, `sighurt-engine-netsurf`, compiled in and running in
process like Servo and WPE WebKit, drawing on the CPU into the host's SHM buffers. It records
what was verified by a smoke test (`experiments/netsurf-smoke/`), the design of the engine, a
build recipe, and a plan a worker with no context can follow. Nothing in the workspace changed:
no crate, no `Cargo.toml`, no `shell.nix` line.

## Status: implemented (steps 1 to 6, 8, 9 and 10's wiring)

`sighurt-engine-netsurf` exists and `sig` opens pages in it, in the window and in the terminal's
graphics mode. The user chose license option 1 below: in process, `engine-netsurf` in **no**
default feature set, the crate `GPL-2.0-only` with a README that says a `sig` built with it is
for personal use and must not be distributed. What is built, and the evidence:

| Step | State | Evidence |
|---|---|---|
| 1 smoke test | done | `experiments/netsurf-smoke/` |
| 2 crate, `build.rs`, link | done | `tools/cargo test -p sighurt-engine-netsurf` (`tests/lifecycle.rs`: one init per process); `sig` links |
| 3 C frontend headless | done | the same C files serve the engine tests |
| 4 `NetSurfRuntime` | done | `tests/engine.rs`: events in the contract's order, a second navigation cancels the first, `present` on an unseen epoch is `NotReady`, close and shutdown |
| 5 scale, resize, zoom, hidden | done | `tests/engine.rs`: the heading is twice as tall at scale 240, 500x400 and 800x600 frames, an empty rectangle keeps the size, a hidden view presents nothing, zoom 2 enlarges the page |
| 6 input | done | `tests/engine.rs`: link click, typing "héllo" reaches a server as UTF-8 through a form, copy, paste, End, the wheel scrolls; `src/input.rs` unit tests |
| 7 fetching: watcher, CA, cookies | partly | the descriptor watcher thread and its test (a ready fd wakes the owner once, an idle set sleeps), HTTPS CA bundle from `SSL_CERT_FILE` or `/etc/ssl/certs`, `misc.login` refuses; **not done**: certificate-error policy test against a self-signed server, a disk cache, cookie persistence, `site_data`, the `dlopen` question |
| 8 new windows, link handover | done | `tests/engine.rs`: a link click is `NavigationRequested`, deny drops it, allow follows it; `target=_blank` is `OpenRequested { NewView }`; history stays the core's (`history_traversal: false`) |
| 9 services | done except two | clipboard read (held paste key) and write (copy), downloads (`Service::Download` with the body and the proposed name), file input and external URLs are wired; the last two have no test |
| 10 `sig` end to end | done | `tests/headless.sh` in a private sway: PASS for page render, link click both ways, typing with accents, a configured plain-letter binding not firing while typing in a field (`TextInputChanged`), scale 2, an idle chrome with no repaints, and `sig --tui` in kitty drawing the page as an image |
| 11 quality passes | not started | glyph cache and fontconfig families, SVG paths and Béziers, shaping, semantics, find, JavaScript feature |

What differs from the design below:

- **Root-page scrolling is the frontend's.** NetSurf scrolls only boxes and frames inside a
  page; for the page itself every frontend keeps a scroll offset, adds it to the positions it
  gives the core (`mouse_click`, `get_features`, `scroll_at_point`), subtracts it when it
  draws (`browser_window_redraw(bw, -sx, -sy, ...)`) and scrolls itself when the core did not
  take a wheel or key. The C frontend does that (`nsh_scroll_by`, `nsh_clamp_scroll`) and
  clamps the offset to the content's extent at every resize and extent change.
- **Housekeeping.** The rule "`p == NULL` and a delay of 5 s or more" misses the image cache's
  cleaner (it passes its cache as `p`; idle wakeups were every 10 s). `nsh_sched_mark_housekeeping`
  now also records every `(fn, p)` the core scheduled while it started, and any later
  `schedule` of the same pair is housekeeping. Measured in `tests/engine.rs`: two idle seconds
  pump fewer than 8 times, and the engine's idle deadline is the once-a-minute housekeeping
  wake.
- **Linking.** `cargo:rustc-link-arg` from a library's build script reaches only that
  crate's own tests, not `sig`. `build.rs` merges the core and the twelve static libraries
  into one archive (`ar -M`: the linker rescans one archive until nothing more resolves, so
  order does not matter) and links it with `rustc-link-lib`; the shared libraries (curl,
  OpenSSL, FreeType, libpng, libjpeg, libwebp, expat, zlib) come from pkg-config the same
  way. The build tree is `$SIGHURT_NETSURF_BUILD`, else `/data/sig-builds/netsurf` when
  `/data/sig-builds` exists, else cargo's `OUT_DIR`.
- **Fonts** are the nix store paths of DejaVu, recorded by `build.rs` at build time (the
  environment overrides them), so `sig` finds them outside the nix-shell too.
- **Frames** are painted whole into the lent buffer (the host's buffers rotate, so a dirty
  rectangle would not be enough); the measured cost (1 to 5 ms at 1024x768) allows it.
- **Title order.** NetSurf reports the title just before it replaces the content; the runtime
  applies title, status, cursor, caret and loading after the ordered events of a batch, so the
  contract's order holds (`started`, `loading`, `committed`, `title`, `loaded`).
- **Cancelled navigations.** A stop that arrives before the load's own start is the previous
  load being replaced; only a stop after the start cancels a pending navigation.
- **A failed load** still shows NetSurf's own error page and commits it; there is no
  `NavigationFailed` for it yet (risk 4 stands).

## Verdict

**Feasible, and cheap to build, with one blocker that is the user's decision: the license.**

- NetSurf's core can be built with a frontend of our own (a directory in its tree that fills in
  the `netsurf_table`), and driven from a poll loop that does not tick. The smoke test did that
  and rendered real pages (example.com, Hacker News, the English Wikipedia article on Rust, a
  CSS/table/form test page) into a BGRA buffer on the CPU, headless.
- It is a *lightweight compatibility engine*, not a Servo or WebKit replacement: CSS 2.1 with
  some CSS 3, no flexbox or grid, no gradients, no `light-dark()`, ES5 JavaScript through Duktape
  (off by default), no media, no WebGL. Its value here is size (a 6 MB binary, 25 MB of memory
  for a page) and speed on text pages.
- **License: NetSurf is GPL-2.0-only** (`COPYING`: "version 2 of the License", no "or any later
  version"; every source file's header says the same). Sighurt is `AGPL-3.0-or-later` (decision
  88). GPL-2.0-only code cannot be combined with GPLv3-or-AGPLv3 code in one program that is
  distributed: GPLv2 has no clause that allows the extra terms of GPLv3/AGPLv3, and the
  copyleft of each demands the whole under its own terms. NetSurf's libraries (libcss, libdom,
  libhubbub, libparserutils, libwapcaplet, libnsgif, libnsbmp, libsvgtiny, libnsutils, libnspsl,
  libnslog, libutf8proc) are MIT and are fine; the problem is the NetSurf core (`content/`,
  `desktop/`, `utils/`, the handlers). Options, for the user to choose:
  1. Build it for yourself and never distribute a binary that contains both (`engine-netsurf`
     stays out of `default = [...]`, a release `sig` never has it). Local use of a combined
     program that is not conveyed to anyone is not restricted by either license.
  2. Run it as a separate GPL-2.0 program that talks to `sig` over a pipe, a restricted worker
     (the OS-isolation worker CLAUDE.md names as a separate decision). That is "mere
     aggregation" in GPL's terms, and it reintroduces the IPC decision 22 removed.
  3. Ask the NetSurf authors for a relicensing exception (unlikely: dozens of contributors).
  4. Drop it.
  **The user chose option 1.** The rest of this file designs it; nothing in the design prevents option 2 later, since
  the C frontend and the adapter only meet through the `EngineRuntime` trait and the owner
  loop.

## What the smoke test proved

`experiments/netsurf-smoke/` (standalone, not in the workspace): `shell.nix` (the pinned
nixpkgs), `build.sh`, `run.sh`, and `frontend/` (a NetSurf frontend named `sighurt`: one C
file, `smoke.c`, about 600 lines, GPL-2.0-only). Measured on the sighurt desktop (12 threads,
`-j4` under `tools/heavy`), 31 October 2026, gcc 15, NetSurf 3.11:

| | |
|---|---|
| Source and libraries | `netsurf-browser.src` and nixpkgs' `libcss 0.9.2`, `libdom 0.4.2`, `libhubbub 0.3.8`, `libparserutils 0.2.5`, `libwapcaplet 0.4.3`, `libnsgif 1.0.0`, `libnsbmp 0.1.7`, `libsvgtiny 0.1.8`, `libnspsl`, `libnsutils`, `libnslog`, `libutf8proc`; all cached on cache.nixos.org (static `.a`, 5 s to fetch); `nsgenbind 0.9`, `netsurf-buildsystem 1.10` |
| Clean build of the core and the frontend | 132 objects, **7.4 s wall, 22 s CPU, 118 MB peak** (JavaScript off) |
| With Duktape (`NETSURF_USE_DUKTAPE=YES`) | 357 objects, 22.7 s wall, 56 s CPU, 380 MB peak (`duktape.c` alone is 3.7 MB of object) |
| Executable | 6.3 MB without JavaScript, 12.6 MB with; 2.5 MB of code in the first. Links libcurl, OpenSSL, FreeType, libpng, libjpeg, libwebp, expat, zlib |
| Start-up (`netsurf_init`) | 1.5 ms |
| example.com, HTTPS | 57 ms from `browser_window_create` to the stop-throbber event (curl alone: 38 ms); 23 MB resident |
| news.ycombinator.com | 1.1 s (network and 25 images), 26 MB, content 1024x1323 |
| en.wikipedia.org Rust article | 2.4 s (network; 100 loop iterations), 65 MB, content 1024x28692 |
| Local CSS test page | 23 ms, 20 MB |
| Repaint, 1024x768, CPU, no cache | **1.3 to 5 ms** the first time, **0.6 to 3.8 ms** the second (text-heavy Wikipedia: 4.4 and 3.1 ms) |
| JavaScript | with Duktape built in and `enable_javascript`, a script that changes text and appends an element ran and rendered |
| Idle on a loaded page | 7 wakeups in 45 s: at 5 s, then every 10 s (see "Housekeeping timers") |

What rendered correctly: the example page (the page uses `light-dark()` for its background, which
NetSurf ignores, so the background stays white: NetSurf's CSS level, not a bug), the CSS test
page (fonts, bold, italic, monospace, borders, float, table, form controls, nested lists,
UTF-8 text with accents and quotes) and the Wikipedia page (images, layout, links). What the test
page showed NetSurf lacks: the CSS gradient and the `display: flex` row (the boxes stacked as
blocks), and SVG icons (the plotter's `path` was a stub in the test).

The test frontend is the whole contract: `misc` (`schedule`, `launch_url`), `window` (create,
destroy, invalidate, get/set scroll, get dimensions, event, set title, url, icon, status,
pointer, caret, drag_start, save_link, console_log), `download`, `clipboard`, `fetch`
(`filetype`, `get_resource_data`, `release_resource_data`, `get_resource_url`), `bitmap`,
`layout` (width, position, split), `llcache` (`null_llcache_table`, no disk cache), plus a
`plotter_table` (clip, rectangle, line, polygon, disc, arc, bitmap with repeat, text, path).
`file`, `utf8`, `search` and `search_web` tables stay NULL (the core has defaults).

### Pitfalls the test found

1. **The order of `fetch_fdset` and the deadline.** `fetch_fdset` polls the fetchers (it runs
   `curl_multi_perform`, which completes transfers, and a completed transfer schedules a
   0 ms callback) *before* it reports the descriptors. A loop that reads its next deadline
   first (as the monkey frontend's does) sleeps on a stale deadline: the first version slept 986
   ms after the last byte (example.com took 1049 ms instead of 57). The loop is: run due
   callbacks, call `fetch_fdset`, *then* read the deadline, then wait.
2. **Curl reports no descriptor while it resolves a name or connects** (`maxfd == -1` with a
   fetch in flight). NetSurf covers that itself with a 10 ms `schedule` of its fetcher poll
   (1000 ms once there are descriptors). The frontend must not treat `maxfd == -1` as
   "nothing to wait for" while the schedule has an entry.
3. **A stop-throbber event before the first start-throbber** is the window's initial state; a
   page has loaded when a stop follows a start.
4. **History thumbnails.** The core renders a bitmap of the page on every history add through
   `gui_bitmap_table.render`; a real frontend returns `NSERROR_OK` and draws nothing.
5. **`option_knockout`** in the plotter table: with `true` the core batches rectangles
   through its own knockout layer and expects the frontend to flush; `false` is simpler and
   what a full-frame target wants.
6. **CFLAGS on the make command line replace the Makefile's own** (they carry
   `-DNETSURF_BUILTIN_VERBOSE_FILTER` and the feature flags); pass them in the environment.
7. **The text metrics define the layout.** `layout.width/position/split` must be the same
   computation the `text` plotter draws with. The test uses FreeType at 90 dpi (the core's CSS
   conversion assumes `nscss_screen_dpi` = 90: `size` in the font style is in points) and the
   unhinted linear advance, summed in 26.6 so that a run does not accumulate rounding.

## NetSurf's shape

NetSurf is not a library. Its core is compiled together with one frontend, which registers its
tables with `netsurf_register()` and then calls `netsurf_init()`. Everything is process-global
(one `guit` table, one set of caches, `urldb` for cookies and history, one options struct),
single-threaded, and driven by two things: the frontend's own loop calling the scheduler, and
the fetchers' descriptors. Callbacks into the frontend happen synchronously inside whatever
core call caused them, which matches the contract's rule that delegate callbacks only queue
events. One runtime per process is the only shape; that is the same as WPE and Servo here.

The core links the static libraries and, from the system, libcurl (the only network fetcher
besides `file:`, `data:`, `about:` and `resource:`), OpenSSL (certificate checks through curl),
libpng, libjpeg(-turbo), libwebp (optional), expat or libxml2 (libdom's parser backend; the
build found expat), FreeType (our frontend). It has its own cookie jar (`urldb`), TLS settings
(`ca_bundle`, `ca_path` options), proxy options and cache (memory cache 12 MB by default; the
disk cache is the `filesystem_llcache_table`, not used so far).

## Build recipe

**Use NetSurf's own Makefile, with a frontend directory of ours dropped into its tree.** The
alternative, compiling the core sources directly with the `cc` crate, was rejected: the core is
132 files, plus generated ones (the messages split by `split-messages`, `testament.h`, and with
JavaScript the `nsgenbind`-generated Duktape bindings, 225 more objects), conditional on 15 `NETSURF_USE_*` features with
`pkg-config` probes; the Makefile already does all of it in 7 s.

1. `shell.nix` gains (like the WPE prefix, behind its own variables): `SIGHURT_NETSURF_SRC`
   (`pkgs.netsurf-browser.src`, the tarball in the store), `SIGHURT_NETSURF_PKG_CONFIG_PATH`
   (the `lib/pkgconfig` of the twelve libraries; **not** `PKG_CONFIG_PATH`, which would rebuild
   Servo, CLAUDE.md), `curl`, `libpng`, `libjpeg`, `libwebp`, `expat` as build inputs, and
   `perl` with `HTMLParser`, `xxd`, `gperf`, `nsgenbind` and `netsurf-buildsystem` as native
   inputs. The inputs of `experiments/netsurf-smoke/shell.nix` are exactly this list.
2. `build.rs` of `sighurt-engine-netsurf` unpacks the tarball into `OUT_DIR` (once), copies
   `csrc/frontend/` over `frontends/sighurt/` (plus `res/`), makes `sighurt` a valid `TARGET`
   (`sed` on `VLDTARGET` in `frontends/Makefile.hts`), and runs
   `env CFLAGS="-fcommon -Wno-error" make TARGET=sighurt` with `-j` from `NUM_JOBS`.
   `-fcommon` is required (the tarball defines globals in headers; nixpkgs does the same).
   gcc and clang warn a lot on this code; `-Wno-error` and no `-Werror`.
3. The frontend's `Makefile` lists the production sources (`S_FRONTEND`, ending in a stub
   `main.c` that only lets NetSurf's makefile finish its link); `build.rs` then archives the
   objects (`ar rcs libnscore.a`, `main` left out), merges the twelve static libraries
   (`libcss.a libdom.a libhubbub.a libparserutils.a libwapcaplet.a libnsgif.a libnsbmp.a
   libsvgtiny.a libnsutils.a libnspsl.a libnslog.a libutf8proc.a`, from
   `SIGHURT_NETSURF_PKG_CONFIG_PATH`'s `libdir`s) into one `libnsall.a` with `ar -M`, and tells
   cargo `rustc-link-lib=static=nsall` and, for the shared `curl`, `ssl`, `crypto`,
   `freetype2`, `png16`, `jpeg`, `webp`, `expat`, `z`, `rustc-link-search` and
   `rustc-link-lib=dylib` from pkg-config (not `rustc-link-arg`: that stays in the crate). (`sig` is linked with `--exclude-libs,ALL`, decision 65, which only affects
   what the archive's symbols export, not what they bind to: check once that libcurl's
   OpenSSL is the same libssl the other engines load.)
4. The feature `engine-netsurf` in `sighurt-browser`, `sighurt-runtime` and `sighurt-gui` (and
   `sighurt-tui` if text and graphics are wanted there), **not** in `default`.
5. NetSurf's resources (`default.css`, `quirks.css`, `internal.css`, `adblock.css`, `Messages`
   for `en`) are embedded with `include_bytes!` and served through `fetch.get_resource_data`
   and `messages_add_from_inline`; the smoke test reads them from a directory. The default
   CSS file is the UA stylesheet; the engine needs no files at run time.
6. Rebuild cost: the C part takes 8 s, a rebuild of Servo is not involved; with JavaScript 23 s.
   Ship JavaScript as a second feature, `engine-netsurf-js` (`NETSURF_USE_DUKTAPE=YES`,
   doubles the object size, runs ES5, disabled in the option `enable_javascript` unless
   `[engines.netsurf] javascript = true`).

## Adapter architecture

Crate `sighurt-engine-netsurf` (depends on api, wayland; like the demo and Wasm engines it draws
on the CPU into a `CpuTarget`, so it never names a GL or Wayland type beyond what they do):

```
engines/sighurt-engine-netsurf/
  build.rs            the recipe above
  csrc/frontend/      the NetSurf frontend "sighurt", GPL-2.0-only (own LICENSE note)
    Makefile Makefile.defaults Makefile.tools
    tables.c          netsurf_table, registration, option setup
    window.c          gui_window: one per view; callbacks -> an event queue
    plot.c            the plotters (software, BGRA), clip, bitmap blit
    font.c            FreeType faces, layout table (width, position, split), glyph cache
    sched.c           the scheduler, deadline, housekeeping classification
    fetch.c           fdset snapshot for the watcher thread, resources
    clipboard.c download.c bitmap.c
    ns_sighurt.h      the C API the Rust side calls (below)
  src/
    lib.rs            NAME, CAPABILITIES, docs (what it owns, what it must not)
    ffi.rs            the extern "C" declarations of ns_sighurt.h, nothing else unsafe
    runtime.rs        NetSurfRuntime: impl EngineRuntime
    view.rs           one view: its bw handle, queued events, dirty rect, viewport
    event.rs          decoding the C event queue into EngineEvent
    input.rs          InputEvent -> browser_window_mouse_click / key_press / scroll
    watcher.rs        the descriptor-watching thread and the deadline (as WPE's glib_loop)
    services.rs       clipboard, downloads, external URLs through ServiceRequest
    draw.rs           present(): C paints into the Frame
```

### The C API (ns_sighurt.h), narrow on purpose

The Rust side never sees a NetSurf type. All calls are on the owner thread.

```c
int  nsh_init(const nsh_config *cfg);          /* options, resources, CA bundle, user agent, profile dir */
void nsh_shutdown(void);
nsh_view *nsh_view_open(uint64_t view_id, int w, int h, int scale_120);
void nsh_view_close(nsh_view *);
void nsh_view_navigate(nsh_view *, const char *url, uint64_t navigation_id);
void nsh_view_reload / _stop / _back / _forward(nsh_view *, ...);
void nsh_view_resize(nsh_view *, int w, int h, int scale_120);
void nsh_view_set_scale(nsh_view *, double zoom);
void nsh_view_mouse(nsh_view *, int x, int y, unsigned state, unsigned mods);
void nsh_view_scroll(nsh_view *, int x, int y, int dx, int dy);
void nsh_view_key(nsh_view *, uint32_t ns_key);        /* NS_KEY_* or a Unicode scalar */
bool nsh_view_paint(nsh_view *, uint32_t *bgra, int w, int h, size_t stride, rect *dirty);
int  nsh_poll(nsh_poll_info *out);   /* runs due callbacks, polls fetchers, fills: fds, deadline ms */
size_t nsh_next_event(nsh_event *out);                 /* drains the queued events, one at a time */
/* clipboard / download / launch results come back as nsh_event of kind SERVICE; answers: */
void nsh_clipboard_set_shadow(const char *utf8, size_t);
void nsh_download_answer(uint64_t id, bool accept);
```

### Mapping onto `EngineRuntime`

| Contract | NetSurf |
|---|---|
| `Target`, `FramePermit` | `TargetLease` and `FramePermit` through `CpuTarget` (as the Wasm engine: `submit(permit, paint)` lends the `Frame`, the C side paints into `Frame.pixels`/`stride`: ARGB8888 premultiplied, which is what the plotters write: opaque pixels, `0xff000000 | r<<16 | g<<8 | b`) |
| `capabilities()` | `history_traversal: false` at first (see below), `page_zoom: true`, `text_input: false` (no IME), `semantics: false` (no extraction script; a later step could walk libdom), `find_in_page: false` (the core has `search`, a later step), `process_per_view: false`, `native_popups: false`, `site_data`: cookies only at first |
| `open_view` | allocates a `ViewId`-keyed `nsh_view`; `browser_window_create(BW_CREATE_HISTORY ...)` then `ViewReady`; the gui_window's `get_dimensions` answers the target's metrics in buffer pixels divided by the scale (see Scale); empty metrics: create with a 1x1 viewport and resize later |
| `command(Navigate)` | `browser_window_navigate`; `NavigationStarted` is synthesized by the adapter (it knows the id and URL) |
| `command(Reload/Stop)` | `browser_window_reload(bw, true)` / `browser_window_stop` |
| `command(Traverse)` | with history: `browser_window_history_back/forward` (see below) |
| `SetMetrics`, `SetVisibility` | `browser_window_set_dimensions`/`reformat` and the `FrameReady` suppression while hidden; a hidden view keeps its layout; no `schedule` entry is dropped (page timers keep their short deadlines; NetSurf has no background throttling: documented) |
| `SetFocus(false)` | releases nothing in the core (NetSurf holds no pressed-key state); the adapter drops held auto-repeat |
| `SetZoom` | `browser_window_set_scale(bw, zoom, true)` |
| `Input(Pointer)` | `browser_window_mouse_track` for motion, `mouse_click` with `BROWSER_MOUSE_PRESS_1/CLICK_1`, drag and modifier bits (`BROWSER_MOUSE_MOD_1` Shift, `_2` Ctrl, `_3` Alt); wheel through `browser_window_scroll_at_point`. Positions: surface-local to buffer to *CSS pixels* once, with the view's `Metrics` (`to_buffer`), then divide by the scale |
| `Input(Key)` | printable text: `browser_window_key_press(bw, unicode scalar)` per char of `text`; editing keys map to `NS_KEY_*` (arrows, Home/End are `NS_KEY_LINE_START/END`, `NS_KEY_DELETE_*`, Tab, Enter `NS_KEY_NL`, Esc, Page up/down); Ctrl+C/X/V/A map to `NS_KEY_COPY_SELECTION/CUT_SELECTION/PASTE/SELECT_ALL`. "A consumed key press also consumes its release": return the `bool` the core gives |
| `Input(TextInput)` | unsupported at first: committed text is typed as `key_press` per scalar |
| `pump(events)` | `nsh_poll`: runs due callbacks, polls the fetchers, drains the event queue into `events`, returns `Pending { deadline }` (below) |
| `present` | `Unchanged` if no dirty region and the same epoch; `NotReady` while the document is being reformatted at a new size; else `submit` the permit and have `nsh_view_paint` repaint the dirty rectangle (the first paint after a resize is the whole frame) and report `Submitted` |
| `close_view`, `shutdown` | `browser_window_destroy`; `nsh_shutdown` (`netsurf_exit`) after all views, then the fds watcher is joined |
| `site_data` | `urldb_` cookie calls for list and forget by host; `durable` once `urldb_save_cookies` ran |

### Events from the core's callbacks

The C frontend queues an `nsh_event` per callback (bounded: coalesce title, status, cursor,
loading, frame; ordered others, capped at 16 held navigation requests as in the contract). The
Rust side maps:

| NetSurf | `EngineEvent` |
|---|---|
| `window.set_title` | `TitleChanged` (replaceable) |
| `window.set_url` | the committed URL: `NavigationCommitted` for the pending navigation (a new `DocumentGeneration` per `GW_EVENT_NEW_CONTENT`); a `set_url` with no pending navigation (a click on a link, a redirect, a meta refresh) is a page-started one: the adapter allocates the `NavigationId` (contract) |
| `window.event(GW_EVENT_START_THROBBER / STOP_THROBBER)` | `LoadingChanged`; a stop before the first start is the initial state (pitfall 3) |
| `window.set_status` | `StatusTextChanged`. NetSurf's own status strings ("Fetching, Loading", "Done (0.4s)") are shown as they are; link hover text is the same callback, which the core's "status text at the press" rule relies on |
| `window.set_pointer` | `CursorChanged` (`gui_pointer_shape` onto `Cursor`: a table in `input.rs`) |
| `window.invalidate`, `GW_EVENT_UPDATE_EXTENT` | `FrameReady` (replaceable), and the dirty rectangle accumulates in the view |
| `window.place_caret`, `GW_EVENT_REMOVE_CARET` | `TextInputChanged` with a purpose from `browser_window_get_features().form_features` (text field or none) |
| `window.set_scroll/get_scroll` | internal: the view keeps the offset and tells the core; `FrameReady` |
| `window.create` with `existing != NULL` and `GW_CREATE_TAB/CLONE` (target=_blank, `window.open`) | `OpenRequested { disposition: NewView }`; the spawned bw is destroyed. The URL is not known at `create`: the core calls `browser_window_navigate` after, so the adapter keeps the new gui_window as "pending" and decides at its first `set_url` (to be verified, risk 3) |
| `misc.launch_url` (mailto:, an unsupported scheme) | `Service::OpenExternal` |
| `download.create/data/done/error` | `Service::Download { source: Data(..) }` for small bodies (capped; the page-initiated throttle is the core's), bigger ones are refused until a `Url` download exists. The core's decision, not the engine's |
| `clipboard.set` | `Service::ClipboardWrite`, fire and forget |
| `clipboard.get` (synchronous in NetSurf) | the adapter sees a paste key (Ctrl+V) first, asks `ClipboardRead`, *holds the key* until `ResolveServiceRequest`, copies the text into the C side's shadow (`nsh_clipboard_set_shadow`), then delivers `NS_KEY_PASTE`. `get` only ever reads the shadow |
| `window.file_gadget_open` | `Service::PickFiles`, answered with `browser_window_set_gadget_filename` |
| `window.console_log` | diagnostics only (never page data in logs: counts) |
| a failed load | the core replaces the page with its own error page (`browser_window__handle_fetcherror` navigates to an `about:` page); there is no failure callback. Bad certificates and logins go through the core's own pages too (`misc.login`: refuse, so the core shows its error). To emit `NavigationFailed` the C side needs a 5-line patch in `browser_window__handle_error` (recorded as risk 4), or the adapter notices the `about:` URL |
| (no source) `NavigationRequested` | NetSurf has no hook before a link navigates. The adapter does it itself at the click: on the release of button 1 it asks `browser_window_get_features(bw, x, y)`; if `.link` is set, it holds the click, emits `NavigationRequested` for that URL, and on `AnswerNavigation { allow: true }` performs the click; `false` drops it. GET forms are not covered (only links, as the contract allows) |

### History

NetSurf keeps a history *tree* per window (`browser_window_history_*`), with thumbnails. The
contract wants a flat `entries`/`current`. Version one declares `history_traversal: false`, so
the core gives each page a view (as the demo does), `HistoryChanged` is never sent, and back
and forward are the core's. A later step may keep the adapter's own list (committed URLs,
`current`) and drive `browser_window_history_back/forward` for `Traverse`, then declare it.

### The owner loop without ticking

The contract: events only through `pump`, `Pending { deadline }`, `EngineWaker`.

- `schedule(ms, fn, p)` (the only way the core asks for time) keeps a list and cancels by
  `(fn, p)`; a negative `ms` cancels. `nsh_poll` does: **run due callbacks; call
  `fetch_fdset` (it polls the fetchers and reports their descriptors); read the next deadline;
  return the fds and the deadline** (pitfall 1). `pump` returns `Pending { deadline: Some(t) }`
  for the deadline, `Pending { deadline: None }` only if it stopped with callbacks still due
  (a bounded slice: at most N callbacks, then return), and `Idle` when the list is empty and
  no fetch is in flight.
- **Descriptors.** `fetch_fdset` hands back curl's `fd_set`s. `watcher.rs` is the thread that
  `poll`s exactly that set (plus an eventfd for "the set changed") and calls the
  `EngineWaker`, then waits for the next set instead of polling a ready descriptor again, so it
  cannot spin; it never touches NetSurf. This is the shape of `glib_loop.rs` in the WPE
  engine (`Watch`: an interrupt eventfd, a thread that polls, `wake()`), and its code can be
  copied. Registering the descriptors in `sig`'s loop would need the runtime to be given the
  loop handle; not needed.
- With no descriptors but a fetch in flight (DNS, connect), NetSurf's own 10 ms poll entry
  provides the deadline: that is a bounded timer that exists only while a load is in flight
  (pitfall 2).
- **Housekeeping timers.** `hlcache_clean` (every 20 s) and the image cache (every 10 s)
  re-arm themselves forever; `llcache_persist` runs 5 s after a fetch. They are the only
  entries on an idle page (7 wakeups in 45 s, measured). The plan says "no permanent repaint
  timer in the idle chrome/demo path"; these are not repaints, but they are a permanent timer.
  The frontend classifies a `schedule` call with `p == NULL` and a delay of 5 s or more as
  *housekeeping* and keeps it out of the deadline: it runs when the loop wakes anyway, and at
  most once a minute when idle (an actual wake is armed only if the engine has live views and
  the last run was over 60 s ago). Page-originated entries (`p` = the window or content:
  meta refresh, GIF frames, the reformat, CSS fetches) always count. The alternative is a
  three-line patch in `hlcache.c`/`image_cache.c` to stop re-arming when the cache is empty;
  the classification needs no patch, and is the plan.

### Presentation and scale

Scale is a rational over 120 (CLAUDE.md). NetSurf lays out in CSS pixels with an optional
`browser_window_set_scale`: the adapter tells the core the *logical* viewport
(`buffer / scale`), keeps the core's scale at the page zoom, and paints with the plotters'
coordinates multiplied by the display scale: simplest is to render at buffer resolution by
setting the core's scale to `zoom * display_scale` and the viewport to the buffer size
(NetSurf's own approach for HiDPI: GTK does this). Text is then drawn at the buffer's pixel
size, so glyphs are crisp; hit-testing and positions use the same buffer coordinates, so
"convert once with `Metrics::to_buffer`, never apply the scale twice" holds with no further
division. To verify in step 5: `layout.width` must include the scale (it does: `size` carries
it).

The plotters draw only the dirty rectangle: `browser_window_redraw(bw, -scroll_x, -scroll_y,
&clip, &ctx)` with `ctx.priv` the `Frame`. No page-sized copy: the plotters write straight
into the lent SHM buffer. The test's repaint cost (1.3 to 5 ms for a full 1024x768 frame) is the
budget: a full repaint per frame is affordable, and animation (GIFs) only dirties its box.
A frame in the buffer swapchain may be older than the last painted one (the host's buffers are
reused): the first paint into a buffer that is not the last painted one repaints in full
(`Frame` has no age: the adapter tracks it by the permit's epoch and a "painted once" bit per
buffer if the lease exposes one, else paints in full every time, which the measurement above
allows).

Replace the test's plotters: the test's software rasteriser is enough for correctness but
draws no anti-aliased shapes, no rounded corners (CSS 3 `border-radius` is not in NetSurf
anyway), no SVG paths. For SVG (`libsvgtiny` draws through `path` and `polygon`) either
implement `path` with `tiny-skia` through a small Rust callback, or flatten Béziers in C
(about 80 lines). Glyphs: swap the per-call FreeType loads for a glyph cache keyed by face,
size and code point, and use `cosmic-text`'s font discovery for the family names instead of
the four DejaVu files; the plotters' `font_style.families` carry the CSS family list, so
`fontconfig` (already in `shell.nix`) can resolve `sans-serif`, `serif`, `monospace` and
named families to files.

## What NetSurf lacks (for the chrome and the user)

- JavaScript is Duktape (ES5.1 plus some bits), off by default (`enable_javascript` is false in
  NetSurf's defaults), no `fetch`, no `XMLHttpRequest` beyond a stub, no timers beyond
  `setTimeout`, partial DOM. Most modern sites that need script show a shell or nothing.
- CSS: no flexbox, grid, gradients, transforms, transitions, custom properties, `calc()`,
  `@media` beyond the basics, `light-dark()`, CSS variables; partial CSS 3 selectors and
  colours. Web fonts: none (system fonts only). Video, audio, canvas (partial 2D), WebGL, WebRTC,
  service workers, IndexedDB, `localStorage`: none.
- No process isolation: it is in-process C parsing hostile input (HTML, CSS, images, TLS
  through OpenSSL). CLAUDE.md's rule stands: native engines are cooperative, not a sandbox, and
  this one is memory-unsafe C that sees untrusted network data. It should be off by default,
  and not offered for sites the user does not choose.
- No text shaping (the test uses a per-code-point advance: no ligatures, no kerning, no bidi,
  no complex scripts); the framebuffer frontend has the same limit. `harfbuzz` is in `shell.nix`
  and could shape runs inside `layout.width` and the `text` plotter, at the cost of a second
  step.
- Cookies are NetSurf's own (`urldb`): per-process, saved to a file the engine names in the
  profile directory; the core's `site.forget` maps onto it.
- Security UI: certificate errors reach the user as NetSurf's own HTML pages, offering "proceed
  anyway" in the page. Sighurt's policy (fail closed, CLAUDE.md) says the adapter must refuse:
  set `misc.login` to refuse, and replace the bad-cert page action with a refusal (a small
  patch or an option; risk 5).

## Risks

1. **License (blocking for distribution).** GPL-2.0-only against AGPL-3.0-or-later, above.
   The crate directory carries its own license note, the Cargo features keep it out of the
   default build, `docs` and `sig --version` say so.
2. **Memory safety and the attack surface.** In-process C on untrusted input, with OpenSSL and
   libcurl linked in beside Servo's TLS stack; a crash or an exploit takes `sig` down. Mitigated
   only by being opt-in, bounded (`memory_cache_size`, the core's limits, a cap on content
   size), and by running the same build under the ASan option (`NETSURF_USE_SANITIZERS`) in the
   tests.
3. **New windows.** `gui_window.create` is called before the URL is known (the core creates the
   bw, then navigates): the adapter must turn a page's `target=_blank` into `OpenRequested`.
   Plan: treat a create with `existing != NULL` as pending and decide at its first `set_url`;
   verify with a page that has a `target=_blank` link and `window.open` (JavaScript build). If
   it does not work, patch `browser_window_create`'s caller (`browser_window_navigate`'s `_blank`
   branch) to call a new `guit->window->open_request(url)`.
4. **No failure callback** (above): a five-line patch to carry the error up, or detect the
   `about:` error page URL. Patches to NetSurf are GPL-2.0-only and live in the crate's
   `csrc/patches/`, applied by `build.rs`.
5. **Certificate prompts and logins** are in-page HTML flows; must fail closed.
6. **Housekeeping timers** (above): a classification rule rather than a guarantee; a test with
   `SIG_DIAG` counts idle wakeups.
7. **Descriptor watcher races.** The watcher polls a snapshot; curl's descriptors change on
   every poll. Same shape as WPE's, with its unit test (`sources_wake_the_owner_and_an_idle_context_sleeps`
   has a twin: a fetch from a local server wakes the owner, an idle runtime has no deadline
   but the housekeeping one).
8. **Text metrics and CJK/RTL.** The advance-per-code-point model gives wrong wraps for
   shaped scripts; honest limit, documented.
9. **Dependencies on the system's libcurl.** libcurl in nixpkgs links nghttp3, ngtcp2, krb5,
   libssh2: about 25 shared libraries land in `sig`'s `ldd`. Per decision 84 (WPE's library is
   opened lazily so a session without WPE maps none of it) the NetSurf core could be a
   `dlopen`ed shared library (`libsighurt-netsurf.so`) instead of a static archive: a session
   without NetSurf pages maps none of curl. Decide in step 7.
10. **Versions.** nixpkgs' NetSurf is 3.11 (2024); upstream development is slow. Pinned by the
    nixpkgs revision (`nix/nixpkgs.nix`), nothing to follow.

## Implementation plan

Each step has an acceptance test a worker with no context can run. Workers do not change the
architecture; the order is not negotiable (each step rests on the one before). Build with
`tools/cargo` and `tools/heavy`, never plain cargo.

1. **Reproduce the smoke test.** `tools/heavy nix-shell experiments/netsurf-smoke/shell.nix
   --run 'experiments/netsurf-smoke/build.sh $PWD/scratch'`, then `.../run.sh $PWD/scratch
   https://example.com/ out.png`. Accept: `out.png` shows the example paragraph, the
   program prints a load time under 200 ms, `NS_IDLE=45` prints at most 8 wakeups.
2. **Crate skeleton and build.rs** (`sighurt-engine-netsurf`, not yet in the workspace's default
   members; add to `members` and the three features, off by default). `build.rs` per
   "Build recipe" produces `libnscore.a`; `shell.nix` gets the variables. Accept:
   `SIG_WHAT="netsurf build" tools/cargo check -p sighurt-engine-netsurf` and `cargo test -p
   sighurt-engine-netsurf` (one test: `nsh_init` then `nsh_shutdown`, twice in a process:
   second init must fail cleanly, one runtime per process) pass; `shell.nix` still
   instantiates and Servo is not rebuilt (`nix-instantiate shell.nix` with the variables
   differs only by them; check that `cargo build -p sighurt-engine-servo` is up to date).
3. **The C frontend, headless.** Split `smoke.c` into the files listed, add the event queue and
   the C API; no Rust beyond `ffi.rs` and one test binary. Accept: the Rust test opens a view on
   a `file://` test page, pumps until `LoadingChanged{false}`, paints into a `Vec<u8>`, compares
   against a stored hash of the PNG for the CSS test page (copy `test.html` into `tests/`), and
   measures: `pump` returns `Idle` or `Pending` with a deadline, never a spin (count of calls
   in 2 s of idle: under 3).
4. **`NetSurfRuntime`: open, navigate, events, present.** `impl EngineRuntime` per the mapping
   table (without input, history, services). Add `NAME`, `CAPABILITIES`, the descriptor in
   `sighurt-runtime`'s `engines` (settings it honors: `javascript`, `user-agent`,
   `memory-cache-mb`), `sighurt-gui`'s `engines` start, `[routing.netsurf]`. Accept:
   `tools/cargo test -p sighurt-engine-netsurf` with a fake `CpuTarget` (an `Arc<Mutex<Vec<u8>>>`
   as the wasm engine's tests do): events in the contract's order (`ViewReady`,
   `NavigationStarted`, `LoadingChanged{true}`, `NavigationCommitted`, `TitleChanged`,
   `LoadingChanged{false}`), a stale-document check (a second navigation supersedes the first),
   `close_view` and `shutdown` leave no events, `present` on a permit of an old epoch is
   `NotReady`.
5. **Scale, resize, zoom, hidden views.** Accept: a test with metrics at scale 120, 150, 240
   renders the test page and the text is one pixel-exact size per scale (assert a known glyph
   box); `SetMetrics` with an empty rectangle keeps the last size; a hidden view presents
   nothing (`Unchanged`) and is not repainted.
6. **Input.** Mouse (hover cursor shapes, click a link navigates, drag-select text), keys
   (typing in a text field changes its value: read it back through a form `onchange`-free
   page and `browser_window_get_selection`/the next paint), editing keys, scroll. Accept: a
   test page with a link, an input and a long document; scripted `InputEvent`s navigate, type
   "héllo" (the accent proves the UTF-8 path), and scroll by a wheel step; positions are
   surface-local converted once.
7. **Fetching: descriptors, certificate policy, cookies, resources.** `watcher.rs`; `ca_bundle`
   from the system; `misc.login` refuses; bad certificates fail closed; the disk cache in the
   profile directory (the filesystem backing store, `NETSURF_FS_BACKING_STORE=YES`) or none;
   decide the `dlopen` question (risk 9). Accept: against a local HTTPS server (`openssl
   s_server` with a self-signed certificate: the load must fail and produce an error page,
   not a bypass), an HTTP server that sleeps 2 s (the owner loop sleeps: the process takes
   under 1 % CPU during the wait, and `pump` deadlines are 10 ms only while connecting),
   and the housekeeping rule (idle wakeups: at most 1 per 30 s over 90 s).
8. **New windows, link handover, history.** `OpenRequested` for `target=_blank`;
   `NavigationRequested` at link clicks; the adapter's own history list and
   `history_traversal: true` if it holds. Accept: a page with `target=_blank` and a link to a
   URL routed to another engine: tab opens, the core's routing rules apply, the held
   navigation is denied when the document is replaced.
9. **Services.** Clipboard read and write with the held paste key, downloads (a `Content-Disposition:
   attachment` file and a `.zip` link), `launch_url`, file chooser. Accept: tests with the
   services' fake host (as the Servo and WPE tests do) for each request and its cancellation
   when the view closes; no clipboard content in logs.
10. **`sig` end to end.** `sig --no-default-features --features engine-netsurf,engine-demo` in a
    private compositor: `apps/sighurt-browser/tests/engines.sh` gains a NetSurf block (page
    renders, link click, typing, resize, scale 2, a second tab). Accept: the script prints PASS
    for each check. Also `sig --dump` and the TUI's text mode are not available (no
    `semantics`): the engine says so through its capabilities.
11. **Quality passes (each separate).** Glyph cache and fontconfig family resolution; Béziers and
    SVG; HarfBuzz shaping; `semantics` from libdom (`SetSemantics`); `find_in_page` through the
    core's search; JavaScript feature. Each has its own measured acceptance (the repaint budget
    stays under 6 ms for a 1024x768 full repaint on this machine).

## Files

- The engine: `engines/sighurt-engine-netsurf/` (`build.rs`, `csrc/frontend/` the C frontend,
  `csrc/patches/`, `src/{lib,ffi,runtime,input,watcher}.rs`, `tests/engine.rs`,
  `tests/lifecycle.rs`, `tests/headless.sh`, `tests/pages/`, `README.md`, `LICENSE`).
  Run `engines/sighurt-engine-netsurf/tests/headless.sh OUT` for the end-to-end checks
  (screenshots in OUT; it keeps every sig directory under OUT, never the user's).
  `CARGO_TARGET_DIR=/data/sig-builds/targets/netsurf` keeps builds off the SSD.
- `experiments/netsurf-smoke/shell.nix`, `build.sh`, `run.sh`, `.gitignore`, `frontend/`
  (`Makefile`, `Makefile.defaults`, `Makefile.tools`, `smoke.c`): the smoke test. Run it as in
  step 1; `NS_TRACE=1` prints events, schedule calls and loop iterations, `NS_IDLE=N` probes
  idle wakeups for N seconds, `NS_JS=1` turns on JavaScript (build with
  `NS_MAKE_ARGS=NETSURF_USE_DUKTAPE=YES`).
- Everything else above is a proposal until its step's acceptance test passes. In particular
  untested so far: input, multiple windows, `window.create` for `target=_blank`, clipboard,
  downloads, the scale handling, linking from Rust, the `dlopen` shape, and behavior under
  `tools/headless-sway` (the smoke test is headless and never touches Wayland).
