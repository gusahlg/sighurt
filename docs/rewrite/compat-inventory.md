# Compatibility inventory: what `gpui-baseline` offers

Milestone M0 (`sighurt-rewrite-plan.md` §23) asks for the command, configuration and SDK
compatibility requirements of the working browser before it is replaced. This is that list:
everything user-visible or guest-visible at git tag `gpui-baseline`, each marked

- **keep**: the rewrite must offer it, with the same meaning;
- **change**: the rewrite offers it differently, for the reason given;
- **drop**: the rewrite does not offer it, for the reason given.

A "change" or "drop" that the plan doesn't already decide names the milestone that settles it.
Sources are `git show gpui-baseline:<path>`; the paths are given per section. The retained
behavior is what M6 ("the user can complete the intended daily workflows") and M7 (no major
functional regression) are checked against.

## 1. Command line

Source: `sighurt-ui/src/main.rs`, `README.md`.

| Baseline | Decision | Why |
|---|---|---|
| `sig [URL\|PATH ...]` opens each argument in its own tab, the home page when there is none | keep | Daily workflow. |
| An argument naming an existing file or directory becomes a `file://` URL; anything else goes through URL-bar fixup (below) | keep | |
| `-h`/`--help` prints the usage; `-V`/`--version` prints `sig <version> (Sighurt, based on Oxide by Nikhil Ranjan)` | keep | The version line carries the Oxide attribution (CLAUDE.md). Both exist in the new `sig`. |
| `--default-config` prints the built-in configuration: the core's file followed by the UI's `[ui]` section | keep | The default file is the format's documentation. It prints the new format (§4 below); not in the new `sig` yet. |
| An unknown option prints the usage and exits with status 2 | keep | |
| Logging through `tracing_subscriber::fmt::init()`, filtered by `RUST_LOG` | change | One process-wide logging setup owned by `sig`; with Servo compiled in, the `log` logger must be Servo's (M0.md, "Logging"). `RUST_LOG` keeps working. |

## 2. Commands

Source: `sighurt-ui/src/commands.rs`, `sighurt-ui/src/browser.rs`, `sighurt-ui/src/lib.rs`. Every
command keeps its name, which users write in their configuration
(`sighurt_api::commands::Command::NAMES` has the same 17). Commands are `<area>.<action>`, an
argument follows whitespace, and a string is parsed once (the baseline checked names at load and
arguments at run time; `sighurt_api::commands` checks both at parse time).

| Command | Baseline behavior | Decision |
|---|---|---|
| `tab.new` | Opens a tab on the home page, selects it and focuses the URL bar | keep |
| `tab.close [n]` | Closes tab `n` (from 1) or the selected one; the last tab can't be closed; the selection moves to a neighbour | keep |
| `tab.next`, `tab.previous` | Select the next or previous tab, wrapping around | keep |
| `tab.select <n>` | Selects tab `n`; an invalid number changes nothing | keep |
| `tab.last` | Selects the last tab | keep |
| `page.open <text>` | Opens the fixed-up text in the selected tab | keep |
| `page.reload`, `page.stop` | Reload; stop loading (Servo: `window.stop()`, since Servo 0.5 has no stop API) | keep |
| `page.back`, `page.forward` | Step through the tab's history, which spans engines | keep; the history model changes (§5) |
| `page.zoom-in`, `page.zoom-out` | Step along 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300 %; the ends stay put | keep |
| `page.zoom-reset` | 100 % | keep |
| `location.focus` | Focuses the URL bar with its text selected (handled by the UI, not the core) | keep |
| `browser.home` | Opens the home page in the selected tab | keep |
| `browser.quit` | Quits | keep; now with an explicit shutdown order (plan §20) |

Commands that change what the content area shows move focus to the page, except `tab.new`
(`sighurt-ui/src/lib.rs`, `run_command`): **keep**.

## 3. Keys

Source: `sighurt-ui/src/default-ui.toml`, `sighurt-ui/src/settings.rs`, `sighurt-ui/src/commands.rs`,
`sighurt-ui/src/toolbar.rs`, `README.md`.

At the baseline, bindings live under **`[ui.keys]`**, with the contexts **`[ui.keys.page]`** and
**`[ui.keys.location]`**. A top-level `[keys]` table is a configuration error there
(`sighurt-core/src/config.rs`, test `typos_are_errors`), although the baseline's CLAUDE.md still
says `[keys]`. Configuration format version 1 (STATUS.md decision 28, M2) moves them back to the
top level: `[keys]`, `[keys.page]` and `[keys.location]`.

| Baseline | Decision | Why |
|---|---|---|
| No default bindings: nothing is bound until the user binds it; the toolbar and tab strip cover `tab.new`, `tab.close`, `tab.select`, `page.back`, `page.forward`, `page.reload`, `page.stop` and `page.open` | keep | A stated policy (CLAUDE.md). The plan's §21 example binds keys only as an illustration of syntax. |
| `keystroke = "command [argument]"`: global in `[ui.keys]`, while the page has focus in `[ui.keys.page]`, while the URL bar has focus in `[ui.keys.location]` | keep the three scopes; change the table names to `[keys]`, `[keys.page]`, `[keys.location]` | Decision 28. |
| GPUI keystroke syntax: `ctrl`, `alt`, `shift`, `cmd`, `secondary` (cmd on macOS, ctrl elsewhere) | change | GPUI is gone (decision 2). Decision 28 (M2): `mod-mod-key` with `ctrl`, `alt`, `shift`, `super`, `altgr` and `secondary` (still ctrl); `cmd` is gone. The key is a character as the layout types it, a key name, or `code:<W3C code>` for a physical key (plan §12); AltGr is never Ctrl+Alt. |
| Space-separated key sequences such as `"g g"` or `"ctrl-x ctrl-c"` | change: rejected with an error when the configuration loads | Decision 28: replaying the keys held back while a sequence is pending needs timers, which the core doesn't have. Whether a sequence (such as `"g g" = "location.focus"`) is part of a daily workflow is for the user to say with M6's workflow list (M0's open item); if one is, M6 must bring sequences back. |
| A bad binding (unknown command or context, bad keystroke) is skipped with a warning; startup continues | change: a file with any problem is rejected as a whole | Decision 28: parsing reports every problem, and any problem rejects the file; at startup `sig` runs with the built-in defaults, on a reload the configuration in use stays (plan §21). |
| The browser sees keys before the page, so a page-context binding takes that key from the page | keep, except while typing | Plan §12 ordering; a consumed press must not leave the page a stuck key (§25). Decision 28: while the address bar or a page's text field has focus, a binding that would type a character (no modifier but shift) lets it type. |
| URL bar editing, hard-wired: Escape drops the edit and focuses the page; Enter runs `page.open <text>`; `secondary-a` selects all; `secondary-c`/`x`/`v` copy, cut and paste (pasted newlines removed); Left/Right (Shift extends), Home/End, Backspace/Delete; mouse drag selects | keep, with `ctrl` for `secondary`; add IME composition | Editor behavior, not bindings. Plan §13 adds Unicode-cluster movement, horizontal scrolling and IME. |

## 4. Configuration

Source: `sighurt-core/src/config.rs`, `sighurt-core/src/default-config.toml`,
`sighurt-ui/src/default-ui.toml`, `sighurt-ui/src/colors.rs`, `README.md`.

| Baseline | Decision | Why |
|---|---|---|
| `{config_dir}/sighurt/config.toml` (`~/.config/sighurt/config.toml`), merged over the built-in defaults | keep | |
| A file that isn't valid TOML, or has an unknown key, is ignored with a warning; an error in `[ui]` only costs `[ui]` | change: any problem anywhere, `[ui]` included, rejects the whole file, reported with every other problem | Decision 28 and plan §21: at startup the built-in defaults apply; an invalid reload keeps the configuration in use. |
| Changes apply when `sig` next starts | change | Plan §21: keys and colors apply live; a renderer change needs a restart. |
| No format version | change | Plan §21 adds `version = 1`, so later format changes are deliberate. |
| `default_engine = "servo"` | change: `[engines] default = "servo"`, and `[engines] enabled` lists the engines that may open pages | Decision 28. |
| `[engines.<name>]` tables merge by name, field by field; a new name adds an engine | change: routing moves to `[routing.<engine>]`, still merged over the defaults field by field | Engines are compiled in (decision 1), so a name can only refer to a compiled engine; an unknown name is an error (decision 28). |
| `[engines.<name>] command = [...]`: the engine program, looked up next to `sig`, then on `PATH` | drop | No engine processes (decision 1). An external engine is a later, separate decision (plan §18). |
| `[engines.<name>] schemes = [...]` (Servo: `data`, `about`) | keep, as `[routing.<engine>] schemes` | Routing rule 3 of plan §16; a scheme belongs to one engine (decision 28). |
| `[engines.<name>] extensions = [...]` (Wasm: `wasm`; Servo: `html`, `htm`, `xhtml`, `svg`, `txt`, `png`, `jpg`, `jpeg`, `gif`, `webp`) | keep, as loader hints in `[routing.<engine>] extensions` | Plan §16: extensions select a loader; they don't validate content. The new defaults list only `wasm` for the Wasm engine; other `file:` URLs go to the default engine (decision 28). |
| `[engines.<name>] mime = [...]`, matched against a HEAD (then GET) probe's `Content-Type` for every http(s) URL | drop | Plan §16: ordinary http(s) goes to Servo without a probe. Decision 28 (M2) has no `mime`: a Wasm app opens over http(s) when the Wasm engine lists both the URL's origin (`[routing.wasm] origins`) and its extension, and the loader validates what it gets. |
| `[ui] home = "sig://home"`: the page at startup and in new tabs | keep, as top-level `home` | Decision 28: `[ui]` holds only the chrome's colors. |
| `[ui] search = "https://duckduckgo.com/html/?q={}"`: where URL-bar text that isn't an address goes | keep, as top-level `search` | Decision 28. |
| `[ui.colors]`: `bg`, `surface`, `hover`, `tab`, `border`, `focus`, `text`, `text_muted`, `text_dim`, `selection`, `error`, each `#rrggbb` or `#rrggbbaa` | keep the names and defaults | The CPU chrome can draw every one of them. |
| `[ui]` belongs to the UI crate, which documents and checks it | keep | Plan §5: the UI owns chrome state. |

### Routing (`sighurt-core/src/router.rs`, README "Engines and routing")

| Baseline rule | Decision |
|---|---|
| 1. `sig:` URLs belong to the UI; pages can't open them | keep |
| 2. A scheme other than http, https and file goes to the engine listing it, or can't be opened | keep |
| 3. The path's file extension is matched against `extensions` | keep |
| 4. Other `file:` URLs go to `default_engine` | keep |
| 5. Other http(s) URLs are probed (HEAD, then GET); the `Content-Type` picks the engine; a type no engine lists is downloaded; a failed probe goes to `default_engine` | change: http(s) goes to Servo directly; unsupported content becomes a download or an error through the engine's response handling (plan §16) |
| Engines are tried in name order | change: each scheme and extension belongs to one engine, so the order no longer matters (decision 28) |

### URL-bar fixup (`sighurt-ui/src/browser.rs`, `fixup`)

Keep all of it (plan §24: "preserve pure URL input normalization and useful rule tests"): input
with `://` or starting `sig:`, `data:`, `about:` is kept as typed; `~/path` and `/path` become
`file://` URLs; `localhost`, `127.0.0.1` and `[::1]` (with a port) become `http://`; a
whitespace-free input whose host contains a dot becomes `https://`; anything else is searched
with the `search` template (percent-encoded), or becomes `https://<input>` if the template has no
`{}`. The baseline's `fixes_up_input` test is the specification.

## 5. Browser behavior

Sources: `sighurt-ui/src/{lib,browser,toolbar,pages}.rs`, `sighurt-core/src/{navigable,page,download}.rs`.

| Baseline | Decision | Why |
|---|---|---|
| One window titled "Sighurt", 1024 × 720 by default, at least 600 × 400 | keep, as the xdg-toplevel title and size hints | |
| Tab strip: a button per tab with the page title, or a short title from the URL (`file:///tmp/app.wasm` → `app.wasm`); a close button on each while there are several; a new-tab button | keep | |
| Toolbar: back, forward, reload that turns into stop while loading, the URL bar | keep | |
| The URL bar shows the tab's URL until the user edits it; switching tabs drops the edit; focusing it selects everything | keep | Plan §6 also separates the edited, requested and committed URL. |
| Status bar: the page's status text (the link under the pointer) or the latest download status | keep | |
| `sig://home` (also `sig:home`, `SIG://Home/`): lists each configured engine with its command, whether it was found and which is the default | change | Engines are compiled in: list the compiled and enabled engines with their diagnostics (renderer, fallback mode; plan §26). |
| Any other `sig:` URL: "Sighurt has no such page. sig://help lists its pages." | keep | |
| Errors (an unopenable URL, an engine failure) are drawn in the content area | keep | Plan §16 separates a failed load from a runtime failure. |
| Each tab has one history across engines; back and forward go through the engine's own history when the neighbouring entry is there, otherwise reopen the entry, possibly in another engine; engine history is matched by URL | change | Plan §16: history segments per engine view; no URL-equality heuristics. |
| Page zoom per tab, kept when the tab switches engines | keep | `ViewOptions::zoom`. |
| Downloads: what no engine shows is saved to the download folder (home if there is none) as `name`, `name (1)`, ...; the name never leaves the folder; progress and result lines go to the status bar; a failed download leaves no file; a tab opened only for a download closes, or goes home if it is the last | keep | The I/O moves into an application service (plan §24). |
| Page-opened tabs only within 5 s of a click or key press in that view, once per activation; page-initiated downloads at most one a second per tab | keep | Decision 15. |
| Engine text cut to 16 KiB, URLs over 2 MiB dropped | keep | Decision 15. |
| Frames and draw lists bounded (200 000 ops, 1 000 000 points, 4096 images, 512 MiB of images, 8192 px sides, coordinates within 1e6, fonts within 512 px) | change | No draw lists cross into the browser any more. The same kind of bounds apply inside the Wasm adapter to guest drawing (plan §15). |
| The chrome repaints only when its state changes; a page frame repaints only the page (GPUI tests `frames_repaint_only_their_page`) | keep | Acceptance criterion: zero chrome repaints caused by page animation (plan §26). |
| One engine process per page: `Hello` within 10 s, a new process when a page loads after its engine exited | drop | Decision 1: engines run in `sig`. |

## 6. The Servo engine

Sources: `sighurt-engine-servo/src/{main,delegate,rendering}.rs`, `sighurt-engine-servo/README.md`.

| Baseline | Decision | Why |
|---|---|---|
| Firefox user agent (Servo's with `Servo/0.5.0` replaced by `Gecko/20100101`) | keep | web.whatsapp.com and others turn other agents away. |
| Preferences turned on: CSS grid, multi-column, container queries, variable fonts, `document.fonts`, `execCommand`, IndexedDB, `OffscreenCanvas`, Permissions and Storage APIs, WebGL 2 | keep | Common sites need them. |
| Turned off: `SharedWorker`, `IntersectionObserver`, `adoptedStyleSheets`; `layout_threads = 2` | keep | Each prevents a measured hang or panic in Servo 0.5 (README). In one process, a hang at shutdown or a style-thread panic affects every tab, so these matter more. |
| `sig-servo --pref NAME=VALUE` | change | No engine command line. Needs a configuration home, such as a table of Servo preferences, or is dropped; settled in M4. |
| Profile `{data_dir}/sighurt/servo`: cookies, local storage, HSTS list, HTTP auth cache, locked while Servo reads and writes it | keep the path and its contents; change the lock | M0.md, "Profile". |
| `target=_blank` and `window.open` become new-tab requests; the popup webview is never shown | keep | `OpenRequested { disposition: NewView }` (decision 11). |
| Title, URL, load state, back/forward availability, cursor, status text, console messages (logged at debug level) | keep | The delegate translation is kept and extended (plan §24). |
| A crashed page shows an error | keep | Per view (`ViewFailed`), separate from the runtime failing. |
| Rendering on the GPU through an offscreen surfman context, software fallback; `SIG_SERVO_RENDERING=hardware\|software` | change | M1: presentation into the page's Wayland subsurface. The fallback stays, visible in diagnostics (plan §14). |
| Every frame read back, compared with the last and sent as RGBA | drop | The path the rewrite removes (plan §3, §26). |
| `SIG_SERVO_TIMING=1`: per-frame timings on stderr | change | Diagnostics and timing instrumentation of plan §26. |
| `process::exit(1)` when the backend is lost; a 5 s `_exit` watchdog at shutdown | drop | M0.md, "Exit paths". |

## 7. The Wasm engine and the guest SDK

Sources: `sighurt-sdk/src/{lib,draw,proto}.rs` (unchanged on this branch), `sighurt-engine-wasm/src/*.rs`,
`DOCS.md`, `SECURITY.md`. Plan §15: keep the existing imports and exports, return errors instead
of false success, version guest-interface breaks separately.

### The contract

| Baseline | Decision |
|---|---|
| Import module `"oxide"` (Oxide's app ABI); apps built with Oxide's `oxide-sdk` run unchanged unless they use its widget kit (`ui_*`, not provided) | keep, permanently (CLAUDE.md) |
| Exports: `start_app()` once; optional `on_frame(dt_ms: u32)` about 60 times a second; `on_timer(callback_id)` and `on_event(callback_id)`, delivered from the frame loop and so only if `on_frame` exists | keep (plan §15: don't reinterpret old guests' expected callbacks) |
| Per frame: pending events to `on_event` first, then animation-frame requests, then expired timers to `on_timer`, then `on_frame` | keep |
| A failing `on_event`/`on_timer` is logged and the frame goes on; a failing `on_frame` or `start_app` fails the page | keep |
| Memory: the module's exported `memory`, or the host's `oxide.memory`, bounded to 4096 pages (256 MiB) | keep |
| Fuel: 500 M instructions for a store, 50 M for each callback | keep; add the epoch time budget (plan §15), which M0 shows working in the combined process |
| Compiled modules cached in `{cache_dir}/sighurt/wasm-aot/v1`; `SIGHURT_AOT_CACHE=off` or a directory | keep; bump the format directory when the Wasmtime `Config` changes (epoch interruption does) |
| Optional manifest `app.toml` next to `app.wasm`: `name`, `description`, `version`, `permissions`; a manifest denies undeclared sensitive APIs | keep |
| Camera, microphone, geolocation and screen capture need a per-origin grant, remembered for the tab's lifetime; the prompt is drawn by the engine over the page; the API returns `PERMISSION_PENDING` (-5) until decided | change: the prompt moves into trusted chrome as a service request (plan §17: page content can't be trusted UI); the pending code and per-origin memory stay |
| Session storage per origin, surviving a same-origin reload; persistent key-value store shared across pages | keep |
| No WASI; only registered host functions reach the host | keep (CLAUDE.md security rules) |
| Guests draw with `canvas_*` calls, turned into draw lists that `sig` paints with GPUI | change: the Wasm adapter paints them itself into its own target (plan §15) |

### Host functions

The baseline registers **211** functions in `"oxide"`; the SDK imports all of them but
`api_video_set_pip` (list extracted from `func_wrap` calls, including those registered in loops).
All keep their names and signatures. Where the rewrite can't honour one, it returns the
function's documented failure value and logs, never pretends success.

| Area (module) | Count | Functions | Decision |
|---|---|---|---|
| Console, time, random, crypto, encoding (`capabilities`) | 17 | `api_log`, `api_warn`, `api_error`, `api_time_now_ms`, `api_random`, `api_random_bytes`, `api_uuid_v4`, `api_hash_sha256`, `api_hash_sha512`, `api_hmac_sha256`, `api_base64_encode`, `api_base64_decode`, `api_url_encode`, `api_url_decode`, `api_url_resolve`, `api_compress`, `api_decompress` (the last two in `compression`) | keep |
| Canvas and text (`capabilities`) | 18 | `api_canvas_{clear,rect,rounded_rect,circle,arc,line,bezier,text,text_ex,measure_text,image,gradient,clip,save,restore,transform,opacity,dimensions}` | keep; painted by the adapter's renderer |
| Input and focus (`capabilities`) | 10 | `api_mouse_position`, `api_mouse_button_down`, `api_mouse_button_clicked`, `api_key_down`, `api_key_pressed`, `api_modifiers`, `api_text_input`, `api_scroll_delta`, `api_get_scroll_position`, `api_set_scroll_position` | keep; input comes from the host's normalized events (plan §12) |
| Page, navigation and history (`capabilities`) | 12 | `api_get_url`, `api_navigate`, `api_push_state`, `api_replace_state`, `api_get_state`, `api_history_back`, `api_history_forward`, `api_history_length`, `api_register_hyperlink`, `api_clear_hyperlinks`, `api_set_content_size`, `api_load_module` | keep; `api_navigate` and hyperlinks become `OpenRequested` (decision 11) |
| Timers and animation (`capabilities`) | 5 | `api_set_timeout`, `api_set_interval`, `api_clear_timer`, `api_request_animation_frame`, `api_cancel_animation_frame` | keep; closing a guest cancels them (plan §25) |
| Storage (`capabilities`) | 6 | `api_storage_{get,set,remove}`, `api_kv_store_{get,set,delete}` | keep |
| Clipboard (`capabilities`) | 2 | `api_clipboard_read`, `api_clipboard_write` | change: through the host's clipboard service under core policy (plan §12) |
| Notifications, location (`capabilities`) | 2 | `api_notify`, `api_get_location` | keep; location stays permission-gated |
| Legacy one-shot HTTP, upload (`capabilities`) | 2 | `api_fetch`, `api_upload_file` | keep |
| GPU (`capabilities`) | 10 | `api_gpu_{create_buffer,create_texture,create_shader,create_render_pipeline,create_compute_pipeline,write_buffer,draw,dispatch_compute,destroy_buffer,destroy_texture}` | keep as they are: already stubs that return 0 and log "No suitable GPU adapter" once. Plan §1 postpones GPU APIs for downloaded Wasm. |
| Streaming fetch (`fetch`) | 7 | `api_fetch_{begin,state,status,recv,error,abort,remove}` | keep |
| WebSocket (`websocket`) | 7 | `api_ws_{connect,send_text,send_binary,recv,ready_state,close,remove}` | keep |
| Server-sent events (`sse`) | 6 | `api_sse_{open,state,recv,error,close,remove}` | keep |
| Events (`events`) | 7 | `api_on_event`, `api_off_event`, `api_emit_event`, `api_event_type_len`, `api_event_type_read`, `api_event_data_len`, `api_event_data_read` | keep; built-in events: resize, focus, blur, visibility change, online/offline, touch, gamepad, file drops |
| Workers (`worker`) | 6 | `api_spawn_worker`, `api_worker_post`, `api_worker_recv`, `api_worker_post_message`, `api_worker_message_read`, `api_worker_terminate` | keep; bounded execution like the page's (plan §15) |
| Audio (`audio`) | 19 | `api_audio_{play,play_url,play_with_format,detect_format,last_url_content_type,pause,resume,stop,seek,position,duration,set_volume,get_volume,is_playing,set_loop}`, `api_audio_channel_{play,play_with_format,stop,set_volume}` | keep; rodio/cpal link and initialize next to Servo (M0.md) |
| Video and subtitles (`video`) | 21 | `api_video_{load,load_url,detect_format,last_url_content_type,play,pause,stop,seek,position,duration,render,set_volume,get_volume,set_loop,set_pip,hls_variant_count,hls_variant_url,hls_open_variant}`, `api_subtitle_{load_srt,load_vtt,clear}` | keep; FFmpeg links and initializes next to Servo (M0.md); `api_video_set_pip` stays a no-op |
| Media capture (`media_capture`) | 11 | `api_camera_{open,close,capture_frame,frame_dimensions}`, `api_microphone_{open,close,sample_rate,read_samples}`, `api_screen_capture`, `api_screen_capture_dimensions`, `api_media_pipeline_stats` | keep the ABI; screen capture is a capability to validate (plan §17: the `screenshots` crate is not a Wayland capture path) and returns its failure value until it is |
| WebRTC (`rtc`) | 19 | `api_rtc_{create_peer,close_peer,create_offer,create_answer,set_local_description,set_remote_description,add_ice_candidate,poll_ice_candidate,connection_state,create_data_channel,send,recv,poll_data_channel,add_track,poll_track,signal_connect,signal_join_room,signal_send,signal_recv}` | keep the ABI; add back after M5's sample works (plan §15: reattach and audit, don't rewrite) |
| MIDI (`midi`) | 9 | `api_midi_{input_count,output_count,input_name,output_name,open_input,open_output,send,recv,close}` | keep the ABI (on Linux the baseline reports no ports) |
| File pickers (`file_picker`) | 6 | `api_file_pick`, `api_folder_pick`, `api_folder_entries`, `api_file_read`, `api_file_read_range`, `api_file_metadata` | change: the picker is the host's file-picker service; reads stay limited to what the user picked |
| Downloads (`download`), PDF | 3 | `api_download_url`, `api_download_data`, `api_canvas_print_pdf` | change: through the host's download service and throttle |
| System (`system`) | 6 | `api_system_locale`, `api_system_timezone`, `api_system_timezone_offset`, `api_system_theme`, `api_battery_level`, `api_battery_charging` | keep |
| Widget kit (`api_ui_*`) | 0 | not registered at the baseline (a test checks that `api_ui_button` is absent) | drop (already dropped) |

The SDK's own surface (`sighurt-sdk`): the wrappers above, 83 public constants (key codes,
`PERMISSION_PENDING`, event and state codes), the `draw` facade (`Canvas`, colors, gradients)
and the `proto` protobuf encoder and decoder. All **keep**: they are guest-side code with no
host dependency beyond the imports.

## 8. The example guests: the compatibility corpus

Source: `examples/`. Every one except `fullstack-notes/backend` (a native server) is a Wasm app.
Plan §23 M5 picks a sample covering simple text and drawing, input, animation, timers and storage,
and media or network only for capabilities declared supported.

| Example | Exercises | In the M5 sample |
|---|---|---|
| `hello-sighurt` | Canvas shapes and text, mouse and keyboard polling, a text field fed by `text_input` and key codes | yes: text, drawing, input |
| `typography-demo` | `canvas_text_ex` weights, styles, alignment; `canvas_measure_text` | yes: text |
| `gradient-demo` | `canvas_gradient`, the SDK's `draw` facade, a responsive grid | yes: drawing |
| `raf-demo` | `request_animation_frame`, `cancel_animation_frame`, `on_timer`, `on_frame` | yes: animation |
| `timer-demo` | `set_timeout`, `set_interval`, `clear_timer` | yes: timers |
| `events-demo` | `on_event` with built-in events (resize, focus, blur, visibility, online/offline, touch, gamepad, file drops) and custom `emit_event` | yes: input and events |
| `index` | `register_hyperlink`, `set_content_size`, links to the other examples by relative URL | yes: navigation between guests |
| `fullstack-notes/frontend` | `fetch_get`/`post`/`delete` against its backend, `notify`, `proto` | network: only if declared supported |
| `sse-demo` | `sse_open`/`state`/`recv`/`error`/`close`/`remove` | network: only if declared supported |
| `audio-player` | Audio channels, volume, looping, WAV bytes made in the guest | media: only if declared supported |

No example uses `storage_*` or `kv_store_*`, so M5's storage case needs a small new guest (or a
test module) of its own.
