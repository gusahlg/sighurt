# Configuration and the extension API: design

**Status:** implemented: STATUS decisions 67, 68 and 74–77, with the review fixes of decision
82. Where the code departs from this design, the changelog at the end says so, and the sections
below describe the code. First written 2 October 2026 against `rewrite` at 3c7f5fd (with the
uncommitted work then in the tree), and revised the same day after two independent reviews of
the draft; §R lists every finding, what was checked, and what became of it.
**Asked for:** "improve the possibilities to configure things. UI and keybindings should still
just have a tiny api that makes it possible for other people to program their own
implementations."
**Read with:** plan §4, §5, §7, §8, §12, §13, §16, §17 and §21, and STATUS decisions 9–11,
18–21, 27–30, 40, 41, 46, 47, 53, 55 and 57.

This document specifies three things:

- part 1: what becomes configurable, and how;
- part 2: a small Rust API that lets others write chromes and keymaps;
- part 3: the order in which to build both, with the tests that prove each step.

## Decisions at a glance

1. **The configuration stays at `version = 1`.** Everything new is optional, and no existing key
   changes meaning. Every valid file keeps working as it does today (§1.11).
2. **New settings, each with one owner:**
   - per-site engines (`[routing.<engine>] hosts`);
   - per-engine settings (`[engines.<engine>]`: `user-agent`, `storage`);
   - a default page zoom and search keywords;
   - a choice of keymap, scroll speed, and the mouse's side buttons;
   - a choice of chrome, its font and size, and its options;
   - the download folder, and whether the session is restored.
3. **Three new commands:**
   - `page.open-in <engine> [address]` reopens the page in a named engine;
   - `page.send-key <key>` presses a named key in the page, so a keymap can scroll with `j`;
   - `chrome.<action>` runs one of the chrome's own actions, such as `chrome.toggle-tabs`.
4. **Links follow routing.** Before an engine performs a navigation its page started, it asks the
   core (`NavigationRequested`). The core hands the navigation to another engine when routing
   names one and the user clicked or typed in the page within the last 5 s (§1.4).
5. **The API lives in two places:**
   - the keymap API is sighurt-api's new `settings` and `keymap` modules, plus
     `input::KeyboardFocus`;
   - the chrome API is a new module of sighurt-ui, `sighurt_ui::extension`, next to the
     snapshot, color and painting types it uses.

   Implementations are compiled in and chosen like engines: a Cargo feature, then `[input]
   keymap` (applies live) or `[ui] chrome` (applies after a restart). There is no plugin
   loading.
6. **The defaults go through the same API.** sighurt-ui's chrome is the `standard` chrome, and
   the core's binding table is the `bindings` keymap. Neither gets a back door.
7. **Two proofs.**
   - `sighurt-keymap-modal` depends on sighurt-api only.
   - `sighurt-chrome-line` depends on sighurt-api and sighurt-ui, and uses sighurt-ui's text,
     editing and painting helpers.

   They show that the traits are enough for a modal keymap with key sequences, and for a chrome
   whose surfaces differ from the standard one's, without touching the core, the host or any
   engine. They don't show that a chrome is cheap to write *without* sighurt-ui's helpers (§2.8).
8. **No stability promise yet.** The API is internal, like the rest of sighurt-api (plan §5).
   Breaking changes are listed in a changelog, and tests that implement both traits from outside
   their crates guard the API. A promise can come later, on conditions §2.10 lists.

---

# Part 1. Configuration

## 1.1 Rules every setting follows

- **One owner per setting.** The owner reads the setting and acts on it. The core *validates*
  the whole file, including the parts that belong to a chrome, a keymap or an engine: it calls
  each implementation's check function (§2.9). One load therefore reports every problem in one
  list (decision 28).
- **"Live" means the next loaded configuration applies without a restart.** A setting marked
  "restart" is accepted on reload and takes effect at the next start, and `sig` shows a notice
  saying so (plan §21: a renderer change needs a restart).
- **A file with any problem is not used.** At startup `sig` falls back to the built-in
  configuration; on a reload it keeps the configuration in use (decision 28).
- **Notices.** Some things are not errors but still need saying. `Config::notices()` returns
  them, and `sig` shows them like its other notices:
  - **An implementation this build doesn't have.** A table for one (`[chrome.x]`,
    `[keymap.x]`, `[engines.x]`, `[routing.x]`), or an `[engines] enabled` entry that names
    one, is accepted. Only the checks that need the implementation itself are skipped. Only
    the user's file gets these notices: the built-in file's `[routing.wasm]` stays silent in a
    build without Wasm. The notice names what this build has, so a misspelling shows at once:
    "[keymap.modl] is for a
    keymap this build doesn't have (it has: bindings, modal); it was not checked". One file can
    then serve builds with and without WPE. Selecting a missing implementation (`[ui] chrome`,
    `[input] keymap`) is still an error, and the error lists the build's implementations.
  - **A key table nobody reads.** For example, "[keys] is not used: [input] keymap is
    \"modal\", which reads [keymap.modal]". The reverse case, "[keymap.modal] is not used", gets
    the same notice. An unused table is still checked, so switching to it later is safe.
- **Merging over the built-in defaults** works as it does now. The following merge key by key:
  - the top-level keys;
  - `[engines]` and `[engines.*]`;
  - `[routing.*]`;
  - `[input]` and `[input.buttons]`;
  - `[downloads]` and `[session]`.

  `[keys]`, `[keywords]`, `[ui]`, `[chrome.*]` and `[keymap.*]` belong to the user alone: the
  built-in file only documents them.
- **New keys of several words are kebab-case** (`user-agent`, `font-size`), like command
  actions. The `[ui.colors]` names keep their snake_case.

## 1.2 A file that uses everything

```toml
version = 1

home = "sig://home"
search = "https://duckduckgo.com/html/?q={}"
zoom = 1.1                          # NEW: the page zoom of new tabs and of page.zoom-reset

[keywords]                          # NEW: "w rust" in the address line searches Wikipedia
w = "https://en.wikipedia.org/w/index.php?search={}"
gh = "https://github.com/"          # no {}: the keyword alone opens it

[engines]
default = "servo"
enabled = ["servo", "wasm", "wpe"]

[engines.servo]                     # NEW: settings of one engine
storage = "ephemeral"               # keep no cookies or site data on disk

[engines.wpe]
user-agent = "Mozilla/5.0 (X11; Linux x86_64) Sighurt"

[routing.wasm]
extensions = ["wasm"]

[routing.wpe]
hosts = ["*.google.com", "docs.example.com"]   # NEW: these sites open in WPE, and so do the
                                               # links you click to them (§1.4)

[input]                             # NEW
keymap = "modal"                    # which keymap reads the keyboard; "bindings" reads [keys]
scroll-speed = 1.5

[input.buttons]                     # NEW: the mouse's side buttons (these are the defaults)
back = "page.back"
forward = "page.forward"

[keymap.modal]                      # NEW: the modal keymap's own table (§2.5)
start = "normal"
typing = "insert"
pass = ["insert"]

[keymap.modal.normal]
"j" = "page.send-key down"
"g t" = "tab.next"
"T" = "chrome.toggle-tabs"
"i" = "mode insert"

[keymap.modal.insert]
"escape" = "mode normal"

[ui]
chrome = "standard"                 # NEW: which chrome draws the window (restart)
font = "Inter"                      # NEW
font-size = 14                      # NEW

[ui.colors]
bg = "#0a0a0b"

[chrome.standard]                   # NEW: the standard chrome's own table
tabs = "left"                       # "top", "left" or "hidden"
status = "overlay"                  # "strip" or "overlay"
buttons = true
compact = false

[downloads]                         # NEW
directory = "~/Downloads"

[session]                           # NEW
restore = true
```

There is no `[keys]` table, because `[input] keymap` selects the modal keymap; a `[keys]` table
here would get a notice (§1.1).

## 1.3 Reference

In this table:

- "core" is `sighurt-core`, "executable" is `sig` (`sighurt-browser`), and "chrome" and
  "keymap" are the implementations selected at run time;
- "build" means the engine registry: what each engine of the build honors.

| Setting | Values | Default | Owner | Checked by | Applies |
|---|---|---|---|---|---|
| `version` | `1` | required | core | core | — |
| `home` | absolute URL | `"sig://home"` | core | core | live |
| `search` | http(s) template with `{}` | DuckDuckGo HTML | core | core | live |
| `zoom` **new** | 0.5–3.0 (the ends of `ZOOM_STEPS`) | `1.0` | core | core | live: new tabs and `page.zoom-reset`; open tabs keep their zoom |
| `[keywords]` **new** | `name = template` | none | core | core | live |
| `[engines] default`, `enabled` | as today | `"servo"`, all | core | core | live for new views |
| `[engines.<e>] user-agent` **new** | 1–512 printable ASCII characters | the engine's own (Servo: the adapter's Firefox-like string) | engine | core (format), build (support) | restart |
| `[engines.<e>] storage` **new** | `"profile"`, `"ephemeral"` | `"profile"` | engine, with the executable's paths | same | restart |
| `[routing.<e>] schemes`, `extensions`, `origins` | as today | as today | core | core | live |
| `[routing.<e>] hosts` **new** | host patterns (§1.4) | none | core | core | live, for addresses and links |
| `[input] keymap` **new** | a keymap of the build | `"bindings"` | core | core | live |
| `[input] scroll-speed` **new** | 0.1–10 | `1.0` | core | core | live |
| `[input.buttons] back`, `forward` **new** | a command, or `""` for none | `"page.back"`, `"page.forward"` | core | core | live |
| `[keys]`, `[keys.page]`, `[keys.location]` | as today | none | the `bindings` keymap | its check | live |
| `[keymap.<k>]` **new** | the keymap's own | — | keymap `<k>` | its check | live |
| `[ui] chrome` **new** | a chrome of the build | `"standard"` | executable | core | restart |
| `[ui] font` **new** | a family name, 1–128 bytes | the first installed family of decision 20's list | chrome | core | live |
| `[ui] font-size` **new** | integer 9–32 (logical px) | `14` | chrome | core | live |
| `[ui.colors]` | as today | as today | chrome | core (names and format) | live |
| `[chrome.<c>]` **new** | the chrome's own | — | chrome `<c>` | its check | live |
| `[downloads] directory` **new** | an absolute path or `~/…` | the XDG download folder, else home | executable | core (format) | live (next download) |
| `[session] restore` **new** | bool | `true` | executable | core | read at start; writing stops or resumes live |

A font that isn't installed is not an error, because one file serves several machines: the
chrome falls back to its list and logs which family it used.

`[ui] chrome` and `[input] keymap` are checked only when the file sets them. Their defaults,
`standard` and `bindings`, exist in every build: sighurt-ui always provides the first, and the
core the second.

## 1.4 Routing: per-site engines, reopening in another engine, links

**Host rules.** `[routing.<engine>] hosts` lists the http(s) hosts that open in that engine:

- `"docs.example.com"` matches that host exactly.
- `"*.example.com"` matches every subdomain at any depth, but not `example.com` itself (list
  both to get both). A `*` is allowed only as the whole first label.
- IP addresses match exactly.
- A port, path, scheme or user name in a pattern is an error.
- Patterns are lower-cased and IDNA-normalized when they load, the way the `url` crate
  normalizes hosts.
- A pattern belongs to one engine, as a scheme or an extension does today. Among the rules that
  match, an exact host beats any wildcard, and a longer wildcard suffix beats a shorter one. So
  `[routing.servo] hosts = ["maps.google.com"]` carves an exception out of
  `[routing.wpe] hosts = ["*.google.com"]`.

The routing order of `sighurt_core::router` becomes:

1. An explicit engine choice (`page.open-in`) wins, within that engine's schemes.
2. `sig:` addresses are Sighurt's own pages.
3. Another scheme goes to the engine that lists it.
4. A `file:` URL goes to the engine that lists its extension.
5. An http(s) URL on one of an engine's `origins`, with one of its `extensions`, goes to that
   engine.
6. **New:** an http(s) URL whose host matches one of an engine's `hosts` goes to that engine.
7. Everything else goes to the default engine.

Rule 6 needs no network request (plan §16). The engine must be enabled; otherwise the tab shows
the error. The rules of an engine the build lacks are checked and left out, so what they claim
goes to the default engine, typed or clicked alike, and one file serves several builds; only
`page.open-in` such an engine says it isn't in the build.

**`page.open-in <engine> [address]`.** It opens `address`, or, without one, the active tab's
committed address, in `<engine>`. While nothing has committed yet, it uses the requested
address.

- It is a new navigation of the active tab, made with the existing explicit choice
  (`navigate(.., Some(engine))`). The new view replaces the shown one once it commits, and back
  returns to the previous engine's segment (decision 29).
- Document state, form data and the scroll position don't come along (plan §16: a reload is
  not a restoration).
- The view it opens is **chosen**: its links stay in that engine (see "Links" below).
- On a `sig:` page it reports that `sig:` pages open in no engine.
- The engine name is checked for syntax when the command is parsed. Whether the build has the
  engine is checked when the command runs.
- Example binding: `"ctrl-alt-w" = "page.open-in wpe"`.

**Links and other navigations a page starts.** Today an engine performs every navigation its
page starts, unless it can't load the scheme (decision 53), so Servo loads links that the core
would route elsewhere (decision 55). Host rules make this matter, because the usual way to reach
a site is a link on another site. So **the engine asks the core first**. These are two additions
to the engine contract:

```rust
// sighurt_api::events::EngineEvent, new variant
/// The page asks to navigate to `url`, which this engine would load itself. The engine holds
/// the navigation until `EngineCommand::AnswerNavigation` with this `request` comes. When the
/// view closes, its document goes or the runtime shuts down, it denies whatever it still holds.
/// Not replaceable: every one is answered.
NavigationRequested {
    view: ViewId,
    document: DocumentGeneration,
    request: RequestId,
    url: String,
},

// sighurt_api::engine::EngineCommand, new variant
/// The core's answer to `NavigationRequested`. `allow: true` lets the engine perform the
/// navigation; `false` drops it, and the core opens the URL elsewhere.
AnswerNavigation { request: RequestId, allow: bool },
```

The core decides in one place, in this order:

1. If the asking document is not its tab's current one, the core denies the navigation, as it
   does every request from such a document (decision 30).
2. If the view is *chosen*, the core allows it. A view is chosen when it was opened, or last
   navigated by the user, with an explicit engine (`page.open-in`, or an `OpenRequest` that
   names one). Typing an address, `page.open` and `browser.home` make the view unchosen. A
   page's own navigations leave the flag as it is. So the tab stays in the engine the user chose
   until they type an address.
3. If routing names another engine of the build that is enabled and working, and the user
   clicked a link to this very URL in the page within the last 5 s (the page's status text
   named it when the button went down; decision 30's activation), the core denies the
   navigation, uses up the activation, and navigates the tab to the URL with routing, unless the
   tab is loading an address the user asked for, which the page can't replace. The new view
   replaces this one once it commits, and back returns here (decision 29).
4. Otherwise the core allows it.

What this means for the user:

- Without an explicit choice, a link the user clicks goes where the same address, typed, would
  go: from Servo to a WPE host, and from a WPE host back to Servo (the default) for a site no
  rule claims.
- After `page.open-in`, links stay in the chosen engine.
- Everything else stays in its engine: a script's redirect, a meta refresh, an iframe loading,
  a script's navigation right after a click, a link opened with the keyboard, a form.

**Why the clicked-link rule (a Servo 0.5 limit).** Servo's `request_navigation` also covers the
navigations of nested iframes, and its `NavigationRequest` doesn't say which frame asks: its
`pipeline_id` is `pub(crate)` (servo-0.5.0/webview_delegate.rs:32–37). Without the rule, an ad
iframe navigating itself to a claimed host would move the whole tab to another engine; with the
draft's rule (any click or key press within 5 s), it still could, right after any click anywhere
in the page, and a sandboxed iframe could navigate the top level that way. Now only the address
of the link the user pressed can move the tab, once per click.

One case is left, to be recorded as a Servo 0.5 limit next to decision 55: a link *inside an
iframe* that the user clicks, to a host another engine claims, moves the whole tab, and the
iframe's page is lost. The existing hand-off of other schemes has the same ambiguity: an
iframe's navigation to `mailto:` becomes the tab's `OpenRequested(CurrentView)` (decision 57,
"Not changed"). This design doesn't change that, because `OpenRequested` is also how Wasm guests
navigate, often without a click.

**What a hand-off carries: only the URL.** A form's POST body, the referrer and the document's
state stay behind; the other engine makes a plain GET. HTTP redirects don't pass through
`request_navigation`, so they stay in the engine that started the load.

**Which engines report it:**

- **Servo** reports `NavigationRequested` for every navigation it would load itself (`loads_itself`
  in engines/sighurt-engine-servo/src/view.rs:332: http, https, data, blob, about, javascript, and
  `file:` from a `file:` page). It keeps today's hand-off of everything else as
  `OpenRequested(CurrentView)` (decision 53). The adapter already keeps the owned
  `NavigationRequest` until `pump` (delegate.rs:160, runtime.rs:676). Now it keeps it until the
  answer comes, at most 16 per view; beyond that it allows at once, which is today's behavior.
  That is a little longer than today, but within the same owner-loop iteration, since the core
  answers as soon as the event is pumped. While one is held, Servo's constellation drops another
  navigation of the same frame with a warning (servo-constellation-0.5.0/constellation.rs:
  3797–3826); today's adapter has that window too, from Servo's call to the next pump.
- **The WPE adapter** reports main-frame navigations, if WebKit's navigation policy decision
  tells frames apart, and can use WebKit's user-gesture flag. This is expected, not checked; the
  WPE track confirms it.
- **The Wasm engine** changes nothing. It already hands every guest navigation and link to the
  core as `OpenRequested` (engines/sighurt-engine-wasm/src/view.rs:531–543, 832–849), which routes like
  a typed address. The activation and chosen rules don't apply there, since these are loads the
  engine can't perform itself.
- **The demo engine** has no links.

**Kept in the Servo adapter:** which schemes Servo can load (engine knowledge, not routing
policy), and its refusal of `file:` navigations from pages that aren't `file:` pages. The core
refuses that case too (decision 41). Keeping the adapter's refusal means a bug in the core can't
make a web page load a local file in Servo.

**The alternative, considered and rejected.** The first draft had the core push a copy of its
routing rules (`HandOff`) to every runtime, at start and on every reload, and each engine would
match URLs itself. That splits routing policy between the core and the engines (§28, question
1). It also puts a URL matcher into `sighurt_api`, which has no `url` crate, and it needs pushing
again on every reload. Servo 0.5's owned `NavigationRequest`, with `allow(self)` and
`deny(self)`, makes the round trip cheap instead.

## 1.5 Engine settings

The settings are a small neutral vocabulary that the core validates:

- When the executable starts a runtime, it turns the settings into that engine's own
  configuration (`ServoConfig`, `WasmConfig`, the WPE runtime's).
- The registry declares which settings each engine honors. **Setting something an engine can't
  honor is a configuration error** naming the engine, never a silent no-op.
- Engines start once per process (Servo at launch, decision 26), so these settings apply at
  restart.

| Setting | servo (0.5) | wasm | wpe | demo |
|---|---|---|---|---|
| `user-agent` | yes: it replaces the Firefox-like string the adapter sets in `Preferences::user_agent` (engines/sighurt-engine-servo/src/runtime.rs:1330), process-wide | no: guests make their own requests | expected (WebKit's `user-agent` setting); the WPE track confirms | no |
| `storage` | yes: `"ephemeral"` builds Servo with `ServoConfig::profile = None`, which keeps nothing | yes: the kv store (`WasmConfig::kv_root`) and the compiled-module cache (`aot_cache`) go to a directory under `$XDG_RUNTIME_DIR`, removed at shutdown | expected (an ephemeral network session) | no |

An engine that needs another setting adds it to this vocabulary, and the other engines declare
it unsupported. There are no engine-specific escape hatches. `javascript` is left out (§1.10).

**Privacy** is these settings combined: `storage = "ephemeral"` for each engine, plus
`[session] restore = false`, which stops `sig` from reading or writing the session file.
Downloads still land in the download folder.

## 1.6 Search keywords

`[keywords]` maps a name to a URL template. In the address line (and in `page.open`), if the
first word of the input is a keyword, the rest becomes the query:

- `{}` is replaced by the rest, percent-encoded the way `search` encodes it, or by nothing when
  nothing follows.
- A template without `{}` opens as it is.
- An input containing `://`, or starting with `sig:`, `data:` or `about:`, is never a keyword
  use.
- Otherwise keywords come before paths, host names and search in `router::fixup`.

Checks:

- A name is 1–32 characters of `a-z`, `0-9` and `-`, and is not `localhost`. Since a name can't
  contain `.` or `:`, it never shadows a host or a URL.
- A template, with `{}` replaced, must be an http(s) URL.

## 1.7 Input

**`[input] keymap`** chooses the keymap (part 2).

- `bindings` reads `[keys]` as today. Any other keymap `k` reads `[keymap.k]`, and
  `[keymap.bindings]` is an error that points to `[keys]`.
- Switching applies live. The new keymap starts in its first mode.
- The core's record of taken presses carries over the switch, so a key held during it still has
  its release taken.

**`[input] scroll-speed`** multiplies the wheel and touchpad scrolling that the core sends to a
page:

- Pixel deltas are multiplied.
- Wheel 120ths are multiplied and rounded, and a non-zero delta never rounds to zero.
- The chrome's own scrolling (a long vertical tab list) is the chrome's business.

**`[input.buttons]`** binds the mouse's side buttons, `back` and `forward`, to commands. It uses
the same command path as keys (CLAUDE.md: "keys and buttons use the same path"). The host
reports BTN_SIDE and BTN_BACK as `back`, and BTN_EXTRA and BTN_FORWARD as `forward`
(frontends/sighurt-wayland/src/input.rs:688–698). Other buttons are left out until someone asks (§1.10).

- **Path.** The executable sends every `Back` and `Forward` button event to the core, wherever
  the pointer is, as one new event:

  ```rust
  /// A side button (`PointerButton::Back` or `Forward`) went down or up: over `view`'s
  /// target, or over the chrome (`None`).
  BrowserEvent::Button { view: Option<ViewId>, event: InputEvent, at: Instant },
  ```

  Every other pointer event goes where it goes today. Over the chrome, today's events go
  straight to the chrome (apps/sighurt-browser/src/app.rs:488–497), and `BrowserEvent::Pointer`
  needs a view. That is why this is a new event.
- **Taken buttons.** The core keeps a per-seat set of taken buttons, like `KeyState` for keys.
  - A press that `[input.buttons]` binds runs its command and is taken.
  - The release of a taken press is dropped, even if the binding changed in between.
  - An unbound press over the shown view goes to that view, followed by its release, and counts
    as user activation like any other button.
  - An unbound press over the chrome, or over a view that is no longer shown, goes nowhere.
- **The chrome loses its side-button code** (ui/sighurt-ui/src/chrome.rs:459–465).
- **The defaults** in the built-in configuration reproduce today's hard-coded behavior exactly
  (decision 27). They are a binding in the built-in file, not code. CLAUDE.md's "no default
  bindings" rule is about keys, and these two were hard-coded before.

**`page.send-key <key>`** presses and releases one named key in the page shown.

- **Which keys.** `down`, `up`, `left`, `right`, `pageup`, `pagedown`, `home`, `end`, `space`,
  `enter`, `escape`, `tab`, `f1`–`f24`, or any W3C key name that is also a W3C code name, with
  optional modifiers (`shift-space`). A character is refused ("to type text, type it"), because
  what a character means depends on the layout.
- **What it sends.** The command holds a `SentKey { modifiers, key, code }`, with the evdev code
  found through the inverse of `KeyCode::w3c_code`. The core sends the press and the release
  straight after each other, so no key can stay held. `text` is `None`, except `" "` for space.
- **Only while the page has the keyboard.** Otherwise it does nothing: the core's focus must be
  `Page`, and an engine view must be shown. A `:page.send-key end` typed in the line works,
  because the line gives the page the keyboard before its command runs
  (ui/sighurt-ui/src/chrome.rs:667–675).
- **User activation only from a key press.** When a keymap runs it from a press, the core's
  `keyboard()` has the press's time and records it as the view's activation, as it does for a
  key delivered to the page (state.rs:1512). Run from the command line or a chrome button, it
  doesn't count: `BrowserEvent::Command` carries no time, and the core reads no clock.

## 1.8 The chrome's look and options

`[ui]` holds what every chrome shares (`font`, `font-size`, `[ui.colors]`), plus `chrome`, which
picks the implementation. `[chrome.<name>]` holds what one chrome shows and how it arranges it.
The standard chrome's options:

| Key | Values | Default | What it does |
|---|---|---|---|
| `tabs` | `"top"`, `"left"`, `"hidden"` | `"top"` | `top`: the tab strip at the top, as today. `left`: a vertical list in its own surface on the left. `hidden`: no tab list (tabs are reachable by key and `:command`). |
| `status` | `"strip"`, `"overlay"` | `"strip"` | `strip`: the status strip below the page, as today. `overlay`: a surface over the bottom-left corner of the page, described below. |
| `buttons` | bool | `true` | Back, forward and reload buttons before the address line |
| `compact` | bool | `false` | A lower tab strip and toolbar: 32 and 36 px instead of 40 and 44, at 14 px |

The status overlay:

- **Size.** It is as tall as the strip and half as wide as the page, but at least 200 px and at
  most the page's width. Its size changes only when the window's does, so a new hovered URL
  repaints the same buffers.
- **When it shows.** The chrome lists it only while there is status text. The page doesn't
  resize when it appears.
- **No pointer input.** Its input area is empty, so the page under it keeps its hover and a
  hovered link's URL doesn't flicker. Plan §8 asks for this: an overlay's input region covers
  only what it should receive.

Heights follow `font-size` by arithmetic: today's heights (tab strip 40, toolbar 44, status 20;
compact 32, 36, 20) times `font-size / 14`, rounded to whole logical pixels. The text styles
scale by the same factor. Nothing is measured for layout, so decision 21 ("an explicit layout
without text measurement") stands.

The standard chrome has one command, `chrome.toggle-tabs`. It switches between the configured
`tabs` and hidden; with `tabs = "hidden"` it shows them at the top.

There is no `status = "off"`. `sig`'s own notices (configuration errors, downloads) must stay
visible somewhere (§2.6), and the overlay shows them without taking room from the page.

## 1.9 Files

- **`[downloads] directory`** is where downloads go, and `~/` means the home directory. A
  missing folder is created when the first download needs it. If it can't be created, the
  download fails with a notice and leaves no file. There is deliberately no `ask = true` yet: it
  needs a save dialog, and file pickers fail closed until M6 implements them. `ask` comes with
  them.
- **`[session] restore = false`** makes `sig` start with the home page instead of the saved
  tabs, and stop writing the session file while the setting is off.

## 1.10 Considered and left out

| Idea | Why not |
|---|---|
| `[engines.<e>] javascript` | No engine in the build can honor it. Servo 0.5 has no preference that turns script off: servo-config-0.5.0/prefs.rs has `js_disable_jit` (line 280) and nothing else. Wasm and demo can't, and WPE's support is unconfirmed. Add it with the WPE adapter if it works there. |
| Other mouse buttons (`button-<evdev code>`) | No stated need. The host already folds codes 272–278 into the named buttons, so most codes could never match. Add it when someone has a mouse that needs it. |
| A default zoom per engine | Zoom belongs to the tab and survives engine switches (decision 29). A per-engine factor would make a tab's zoom jump when it changes engine. |
| A hardware-acceleration switch | Nothing in this build could honor it: Servo presents GL by design (M1), and the Wasm engine and the chrome draw on the CPU. The WPE track can propose one, with evidence. |
| Per-site JavaScript, zoom or user agent | Servo 0.5's preferences are process-wide, and a per-site zoom would fight the tab's zoom. A per-site *engine* covers the real need ("use WPE where Servo breaks"). |
| A density scale with more steps than `compact` | One switch covers "more room for the page". A scale invites layouts nobody tests. |
| Key sequences and modes in the `bindings` keymap | Decision 28 stands for the default keymap. The modal keymap has sequences and modes, and needs no timer (§2.5). |
| Timeouts in the keymap API | The core reads no clock. Sequences need no timeouts when no binding is the start of another, which the modal keymap checks when it loads. |
| Switching the chrome live | Plan §13 prefers choosing at startup: live switching means tearing down targets and migrating state, for no architectural gain. |
| A chrome layout language, theme files, theme inheritance | Plan §13: explicit layout, no layout language. Four options and one color table are tasteful; a theme system is not. |
| Browser commands for keymap modes (`mode.insert`) | Modes are a keymap's own business and live in its table (`"i" = "mode insert"`). |
| A chrome tick (spinner, blinking caret) | §26: no permanent timer. The API has no clock. A loading indicator is drawn as a static mark. |
| Arguments to `chrome.<action>` | No proposed chrome command needs one. Checking an argument at load would need each chrome to declare its syntax, which is more API than the need. |
| Proxy, locale, HTTP cache size | Servo has preferences for them, but nobody has asked. Add them to the engine vocabulary when someone does. |

## 1.11 Version: why it stays at 1

The format version exists so that `sig` recognizes a file whose keys mean something else
(decision 28). Nothing here changes or moves an existing key, so every v1 file means exactly what
it means today. Some changes only *accept* what used to be refused: `[ui] font` was an error and
becomes valid, and a side button becomes rebindable but keeps its meaning by default. A reader of
the new keys still reads every old file the same way, so no migration is needed. Given a file
that uses new keys, an older `sig` refuses it and names each key it doesn't know, which is the
right outcome.

The old format's hints stay. `[engines.<e>]` becomes a valid table, so an old
`[engines.wasm] command = [...]` no longer fails as "[engines]: unknown field `wasm`". It fails
key by key, with messages that say where things went:

- `command`: engines are compiled into `sig` now, so there is no command to run;
- `schemes` and `extensions`: they are in `[routing.wasm]` now;
- `mime`: there is no MIME probe any more; use `[routing.wasm] extensions` and `origins`.

The existing tests that change with this part are listed under the decisions at the end.

When a future change has to alter or move an existing key, it bumps to `version = 2` and keeps
reading version 1:

- it translates a v1 file in memory;
- it reports problems in terms of the user's own file (`[routing.wasm]`, not the new name);
- it prints a notice that `sig --default-config` shows the new format.

`sig` never rewrites the user's file. No such change is proposed here.

---

# Part 2. The extension API

## 2.1 Where it lives, and why

**The keymap API is in `sighurt-api`:** two new modules, `settings` and `keymap`, plus
`input::KeyboardFocus` and `commands::CommandSet`. The core holds the keymap and calls its trait,
and plan §5 lets `sighurt-core` depend on `sighurt-api` only. What a keymap exchanges (key
events, commands, the keyboard's focus) is the neutral input and command vocabulary that §5
already puts there. A keymap crate depends on `sighurt-api` alone.

**The chrome API is in `sighurt-ui`, as the module `sighurt_ui::extension`.**

- The core never calls a chrome; the executable does.
- Every type a chrome exchanges, besides the shared input, geometry and id types, already lives
  in sighurt-ui: `Snapshot`, `TabSnapshot`, `TabContent`, `HomeInfo`, `EngineRow`, `Colors`.
  So nothing moves, and `sighurt-api` stays the neutral contract of plan §5, with no pixel
  buffers or palettes in it (§28, question 4).
- A chrome draws with `sighurt-render` (docs/rewrite/RENDER.md): a `Pen` records a scene, which
  the renderer `sig` chose renders, so a chrome names no pixel buffer, tiny-skia or cosmic-text
  type. sighurt-ui re-exports what a chrome draws and shapes with (`Pen`, `PathBuilder`, `Rgba`,
  `Fonts`, `ShapedText`, `TextStyle`, `FontSystemHandle`). Decision 10 ("sighurt-ui depends on
  sighurt-api only") becomes "on sighurt-api and sighurt-render": the render crate is the
  chrome's drawing library, as tiny-skia was.
- A chrome depends on `sighurt-api` and `sighurt-ui`. Its text, editing and drawing helpers are
  part of the chrome API (§2.8).

**What the core needs from a chrome is only checks:** its name, the check function for its
table, and its list of commands. The executable copies them into the core's `Environment`
(§2.9).

## 2.2 `sighurt_api::settings`

```rust
/// One value of a setting: TOML's types without dates (the core refuses dates).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Array(Vec<Value>),
    Table(Table),
}

/// A table of settings, by key.
pub type Table = std::collections::BTreeMap<String, Value>;

impl Value {
    pub fn as_str(&self) -> Option<&str>;
    pub fn as_bool(&self) -> Option<bool>;
    pub fn as_table(&self) -> Option<&Table>;
    pub fn kind(&self) -> &'static str;       // "a string", "a table": for messages
}
```

The core parses the file and hands each implementation its own table as these values. An
implementation never sees the rest of the file. It doesn't depend on the `toml` crate either, so
it doesn't break when that crate's version changes.

## 2.3 `sighurt_api::keymap`, and two additions elsewhere in the crate

```rust
// sighurt_api::input
/// Which part of the window has the keyboard. Replaces sighurt_core::Focus { Page, Location }
/// and sighurt_ui::Focus { Page, Chrome }.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeyboardFocus {
    /// The active tab's page, or the page the chrome draws in its place.
    #[default]
    Page,
    /// The chrome: its address and command line, or another of its widgets.
    /// `[keys.location]` keeps its name.
    Chrome,
}

// sighurt_api::commands
/// The commands a configuration may bind: every built-in command, and the `chrome.<action>`
/// commands of the chrome the configuration selects.
#[derive(Clone, Copy, Debug)]
pub struct CommandSet<'a> { /* the chrome's name and actions */ }
impl<'a> CommandSet<'a> {
    pub fn new(chrome: &'a str, actions: &'a [&'static str]) -> Self;
    /// `text` as a command, or why it can't be bound here: an unknown name, a bad argument, or
    /// a chrome action the selected chrome doesn't have ("the standard chrome has no command
    /// chrome.toggle-tab; it has chrome.toggle-tabs").
    pub fn parse(&self, text: &str) -> Result<Command, String>;
}

// sighurt_api::keymap
/// What a key press happens in.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyContext {
    pub focus: KeyboardFocus,
    /// The focused part takes typed text: the chrome's line, or a text field of the page.
    pub typing: bool,
}
impl KeyContext {
    pub fn new(focus: KeyboardFocus, typing: bool) -> Self;
}

/// What the browser does with a key press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Not the browser's: the press, its repeats and its release go to whatever has focus.
    Pass,
    /// Taken, and nothing runs: part of a sequence, or a change of mode.
    Take,
    /// Taken, and the command runs.
    Run(Command),
    /// The command runs, then the press goes to whatever has focus now, as if passed, so `:`
    /// can focus the command line and type itself into it.
    RunThenPass(Command),
}

pub trait Keymap {
    /// Applies its table (`[keys]` for `bindings`, `[keymap.<name>]` otherwise), which its
    /// kind's `check` accepted. Called before anything else, and after every reload that keeps
    /// this keymap.
    fn configure(&mut self, settings: &Table);

    /// A fresh key press, or an auto-repeat (`key.repeat`) of a press this keymap took.
    /// Releases never come here. For a repeat, `Pass` counts as `Take` and `RunThenPass` as
    /// `Run`.
    fn press(&mut self, key: &KeyEvent, context: KeyContext) -> KeyAction;

    /// The context changed between presses: the keyboard moved between page and chrome, or a
    /// text field took or lost it.
    fn context(&mut self, _context: KeyContext) {}

    /// The window lost the keyboard: drop an unfinished sequence.
    fn reset(&mut self) {}

    /// What the chrome shows about the keymap ("NORMAL", "NORMAL g"). The executable passes on
    /// at most 64 characters of one line.
    fn indicator(&self) -> Option<&str> { None }
}

/// A keymap a build can offer. Each keymap crate exports one, as `KIND`.
#[derive(Clone, Copy)]       // Debug, PartialEq and Eq compare by name
pub struct KeymapKind {
    pub name: &'static str,
    /// Everything wrong with its table, one line each; empty if the table is valid. Commands
    /// are parsed with `commands`, so chrome actions are checked too.
    pub check: fn(&Table, &CommandSet<'_>) -> Vec<String>,
    /// A new keymap, which the core configures before using it.
    pub new: fn() -> Box<dyn Keymap>,
}

/// One keystroke in the configuration syntax ("ctrl-shift-t", "alt-left", "ctrl-code:KeyQ").
/// Moved unchanged from sighurt_core::keys, with its layout-aware matching.
pub struct Chord { /* private */ }
impl FromStr for Chord { type Err = String; }
impl Display for Chord {}
impl Chord {
    pub fn matches(&self, event: &KeyEvent) -> bool;
    /// Whether it would type a character (no modifier but Shift).
    pub fn types(&self) -> bool;
}

/// Whether `key` is a modifier, lock or dead key. Sequences leave these out.
pub fn is_modifier(key: &KeyEvent) -> bool;
```

**What the core guarantees around any keymap.** A keymap can't break these:

1. **A taken press takes its release and its repeats, and a passed press passes them.** The core
   keeps today's per-seat set of taken presses (`KeyState`). The keymap decides only about fresh
   presses and about repeats of presses it took. So no keymap can leave a page or the line with
   half a keystroke (plan §12, §25).
2. **Only a fresh press starts anything.** A key held from before goes where its press went,
   without asking the keymap (today's rule, decision 41).
3. **`RunThenPass`:** the command runs first. The core then routes the press by the focus as it
   is *after* the command. The press counts as passed, so its release follows it.
4. **The context is the core's.** `typing` is true while the chrome has the keyboard, or while
   the focused page has a text field (today's `typing`). The core calls `context` when either
   changes, and `reset` when the window is deactivated.
5. **The keymap runs inside `update`.** So it does no I/O, reads no clock and starts no thread.
   It sees what the user types, passwords included. It keeps no more keys than its unfinished
   sequence, and never logs them (CLAUDE.md, diagnostics).
6. **A changed indicator redraws the chrome** (`Effect::RedrawChrome`). The indicator changes
   only on presses and context changes, so it costs nothing while idle.

## 2.4 The default keymap through it: `bindings`

`sighurt_core::keys::Bindings` keeps its parser and `find`; the parser reads commands through
the `CommandSet`. Its implementation of the trait is the whole adapter:

```rust
pub const KIND: KeymapKind = KeymapKind {
    name: "bindings",
    check: |table, commands| {
        let mut problems = Vec::new();
        Bindings::from_table(table, commands, &mut problems);
        problems
    },
    new: || Box::new(Bindings::default()),
};

impl Keymap for Bindings {
    fn configure(&mut self, settings: &Table) {
        // `check` accepted the table, so plain parsing (Command::from_str) finds nothing wrong.
        *self = Bindings::from_table(settings, &CommandSet::ANY, &mut Vec::new());
    }

    fn press(&mut self, key: &KeyEvent, context: KeyContext) -> KeyAction {
        match self.find(context.focus, context.typing, key) {
            Some(command) => KeyAction::Run(command.clone()),
            None => KeyAction::Pass,
        }
    }
}
```

(`CommandSet::ANY` stands for "parse without a chrome check", a constant of that step.)

The core's `KeyState::handle` asks the trait instead of `Bindings::find`. Today, a repeat of a
taken press runs its binding again, or is consumed when the binding no longer matches. Rule 1
gives the same result: `Run` runs again, and `Pass` on a repeat counts as `Take`. The existing
tests `consumed_presses_consume_their_releases`, `a_held_key_never_turns_into_a_shortcut` and
`losing_the_keyboard_forgets_held_keys` prove that the behavior is unchanged.

## 2.5 Proof keymap: `sighurt-keymap-modal`

A crate that depends only on `sighurt-api`, with feature `keymap-modal` and config name
`modal`. It has **no built-in bindings** (CLAUDE.md: keys live only in the configuration). Modes,
sequences and mode switches are its code; what each key does comes from the user's table. With
the table below, it gives **vim-style keys for scrolling, tabs, history and the command line**.

```toml
[input]
keymap = "modal"

[keymap.modal]
start = "normal"          # the mode it starts in
typing = "insert"         # the mode it enters when a text field of the page takes the keyboard;
                          # when the field loses it, the keymap returns to `start`
pass = ["insert"]         # modes in which unbound keys reach the page; in the others, unbound
                          # keys that would type are swallowed (arrows and ctrl-c still pass)

[keymap.modal.always]     # every mode, and the only table while the chrome has the keyboard
"ctrl-q" = "browser.quit"

[keymap.modal.normal]
"j" = "page.send-key down"
"k" = "page.send-key up"
"h" = "page.send-key left"
"l" = "page.send-key right"
"d" = "page.send-key pagedown"
"u" = "page.send-key pageup"
"g g" = "page.send-key home"
"G" = "page.send-key end"
"H" = "page.back"
"L" = "page.forward"
"J" = "tab.previous"
"K" = "tab.next"
"g t" = "tab.next"
"g T" = "tab.previous"
"g 0" = "tab.select 1"
"g $" = "tab.last"
"t" = "tab.new"
"x" = "tab.close"
"r" = "page.reload"
"o" = "location.focus"
":" = { run = "location.focus", pass = true }     # RunThenPass: opens the line with ":"
"i" = "mode insert"

[keymap.modal.insert]
"escape" = "mode normal"
```

How it behaves:

- **Modes** are the sub-tables (`always` is reserved, as are the keys `start`, `typing` and
  `pass`). A binding's value is a command, `"mode <name>"`, or
  `{ run = "<command>", pass = true }`.
- **Sequences** are 1–4 keystrokes separated by spaces, in the `bindings` syntax (`Chord`).
  - An unfinished sequence takes its keys and shows in the indicator ("NORMAL g").
  - When the next key doesn't continue it, the sequence's keys are dropped and that key starts
    afresh, as in Vim.
  - When the table loads, the keymap refuses any binding that is the start of another in the
    same mode, so no timeout is ever needed.
  - Modifier and dead keys don't count as sequence keys, so `g`, then Shift, then `t` is `g T`.
- **In a `pass` mode, a sequence can't start with a key that types** (`Chord::types`). Its
  first key would be held back from the page or text field, and lost if the sequence doesn't
  finish. For example, `"j k" = "mode normal"` in insert mode would swallow every `j` not
  followed by `k`. Such a binding is a configuration problem. A single key that types may be
  bound in a `pass` mode; it then never reaches the page in that mode, as the user asked.
- **Repeats:** a held key repeats a single-key `Run` binding, so a held `j` keeps scrolling.
  Anything else it took is held quietly.
- **While the chrome has the keyboard**, only `always`'s bindings whose first key types nothing
  apply, so the line gets every character. Enter, Escape and the editing keys belong to the
  chrome unless `always` binds them. When the chrome gives the keyboard back, the mode is what
  it was.
- **Indicator:** the mode in capitals, followed by the keys of an unfinished sequence.

**What it can't do, and why.** This is not Vimium:

| Missing | Why |
|---|---|
| Leaving a text field with `escape` | Escape leaves insert mode, but the page's field keeps the keyboard: no command blurs a field, and the engine contract has no such operation. `j` then moves the caret there. A later `page.blur` would need `EngineCommand` support. |
| Insert mode when the user clicks a second field | `typing` stays true from one field to the next, so `context` isn't called. If the user had left insert mode, the keymap stays in normal mode. This needs `TextInputState` to carry a field identity (decision 55). |
| Link hints (`f`) | They need the page's links and a way to draw over them. Neither is in the engine contract. |
| Find (`/`, `n`) | No command or engine operation finds text in a page yet: every engine declares `find_in_page: false`. |
| Copying the URL (`yy`) | No command copies the URL. A `page.copy-url` command would be a small addition. |
| Counts (`5j`, `3gt`) | `KeyAction` runs exactly one command. A later `KeyAction::RunAll(Vec<Command>)` would allow them as an addition. |
| Visual mode | Selection belongs to the page. |

No prototype of this crate exists in the tree. The first draft's prototype was in a scratch
directory and was not kept (Evidence). Step 7 writes the crate and its tests.

## 2.6 The chrome API: `sighurt_ui::extension`

```rust
use sighurt_api::commands::Command;
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{LogicalSize, SurfaceRect, WindowRect};
use sighurt_api::ids::{RequestId, TabId};
use sighurt_api::input::{
    FocusEvent, KeyEvent, KeyboardFocus, PointerEvent, TextInputEvent, TextInputState,
};
use sighurt_api::settings::Table;

use crate::{Colors, Fonts, Pen, Snapshot};

/// A chrome's own name for one of its surfaces ("toolbar", "page").
pub type SurfaceName = &'static str;
/// The most surfaces a chrome may use.
pub const MAX_SURFACES: usize = 8;

pub trait Chrome {
    /// Handles one event and returns what the executable should do, in order.
    fn handle(&mut self, event: ChromeEvent) -> Vec<ChromeAction>;
    /// Where the page goes and where the chrome's surfaces are. Read after every `handle`.
    fn layout(&self) -> &ChromeLayout;
    /// Draws `surface` with `pen`, whole: the surface's clear colour first (`Pen::clear`), then
    /// everything it shows. The pen's rectangle is the surface's (`Pen::rect`).
    fn draw(&mut self, surface: SurfaceName, pen: &mut Pen);
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum ChromeEvent {
    /// The shared look and `[chrome.<name>]`. Comes first of all, and after every valid reload.
    Configure { theme: Theme, settings: Table },
    /// The window's size; before the first drawing and on every change.
    Resize(LogicalSize),
    /// What the browser shows now, after every change the core made that a chrome could show.
    /// Never sent because a page drew a frame.
    Snapshot(Snapshot),
    /// The core moved the keyboard. `Chrome`: focus the line with its text selected. `Page`:
    /// leave the line and drop an edit (decision 40).
    Focus(KeyboardFocus),
    /// The window gained or lost the keyboard.
    WindowFocus(FocusEvent),
    /// Pointer input on a chrome surface, in its coordinates. Side buttons never come here
    /// (they are the core's, §1.7).
    Pointer { surface: SurfaceName, event: PointerEvent },
    /// A key the keymap didn't take, while the chrome has the keyboard.
    Key(KeyEvent),
    /// Input-method text, while the chrome has the keyboard.
    TextInput(TextInputEvent),
    /// The clipboard's text for `ChromeAction::Paste(request)`.
    Pasted { request: RequestId, text: String },
    /// `chrome.<action>` ran; `action` is one of the kind's `commands`.
    Command(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChromeAction {
    Command(Command),               // buttons, Enter in the line, `:commands`
    SelectTab(TabId),               // a click on a tab
    CloseTab(TabId),                // its close button
    Focus(KeyboardFocus),           // the chrome moved the keyboard itself
    Copy(String),
    Paste(RequestId),               // made with RequestId::next()
    TextInput(Option<TextField>),   // the chrome's text field, for the input method
    Cursor(Cursor),
    Repaint(SurfaceName),           // it shows something new
}

/// The text field the chrome edits, for the input method. The caret is in `surface`'s
/// coordinates.
#[non_exhaustive] #[derive(Clone, Debug, PartialEq)]
pub struct TextField { pub surface: SurfaceName, pub state: TextInputState }   // + new()

#[non_exhaustive] #[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChromeLayout {
    /// Where the shown view goes. Empty when there is no room.
    pub page: WindowRect,
    /// Bottom to top, all above the page. See the contract below for names that come and go.
    pub surfaces: Vec<ChromeSurface>,
}                                                                   // + new(page, surfaces)

#[non_exhaustive] #[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeSurface {
    pub name: SurfaceName,
    /// In the window. Empty when the layout has no room for it.
    pub rect: WindowRect,
    /// Where it takes pointer input.
    pub input: InputArea,
}                                                       // + new(name, rect): input is Whole

/// Where a chrome surface takes pointer input. A transparent pixel alone doesn't let input
/// through (plan §8).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum InputArea {
    /// All of it.
    #[default]
    Whole,
    /// These rectangles, in the surface's coordinates. None at all: no input.
    Rects(Vec<SurfaceRect>),
}

/// The look every chrome shares: `[ui]`.
#[non_exhaustive] #[derive(Clone, Debug, PartialEq)]
pub struct Theme { pub colors: Colors, pub font: Option<String>, pub font_size: u16 }

/// A chrome a build can offer. Each chrome crate exports one, as `KIND`.
#[derive(Clone, Copy)]       // Debug prints the name
pub struct ChromeKind {
    pub name: &'static str,
    /// Everything wrong with `[chrome.<name>]`, one line each; empty if the table is valid.
    pub check: fn(&Table) -> Vec<String>,
    /// The actions it takes as `chrome.<action>` commands, which take no argument.
    pub commands: &'static [&'static str],
    /// A new chrome shaping with `fonts`, the font system `sig` shares with its renderer.
    pub new: fn(Fonts) -> Box<dyn Chrome>,
}

/// Untrusted text made safe to show on one line: control characters and the line and paragraph
/// separators (U+2028, U+2029) become spaces; bidi embedding, override and isolate controls are
/// dropped; at most `max_chars` are kept.
/// (Today's `sighurt_ui::state::untrusted_line`, made public.)
pub fn contained(text: &str, max_chars: usize) -> String;

/// A committed address as an address line shows it: percent-encoded UTF-8 in its path, query
/// and fragment decoded, except what could hide or fake part of it (decision 75).
pub fn readable_url(url: &str) -> Cow<'_, str>;
```

The existing snapshot types gain one field, `Snapshot::keymap: Option<String>`, the keymap's
indicator, and one method, `Snapshot::contain`, which the executable runs before a chrome sees a
snapshot. The other snapshot and color types stay as they are, where they are. Besides `Pen`
(with `PathBuilder`), `Fonts`, `TextStyle`, `ShapedText` and `TextEditor`, a chrome may use
`state::{is_home, NO_SUCH_PAGE}`, which say what the page area shows for `sig:` addresses; the
line chrome uses all of them.

**Drawing (the painting half, amended by docs/rewrite/RENDER.md).** `Chrome::draw` records the
surface's scene with a `Pen`, in window-logical coordinates: `clear` (made opaque: chrome surfaces
are opaque), `fill`, `fill_rounded`, `bordered`, `stroke` and `shape` (with sighurt-render's own
`PathBuilder`), `text` (a `ShapedText` at a point, cut to a rectangle) and `rect` (the surface's
rectangle). It names no buffer, pixel format or rasterizer: the renderer `[ui] renderer` chose
renders the scene, with every edge and baseline snapped by the same shared functions, so the
chrome's pixels and its hit test agree under any renderer. A scene equal to the surface's last
one, at the same metrics, is not rendered again and commits nothing. The fonts are shared: `sig`
loads them once, hands the chrome a `Fonts` (`ChromeKind::new`) and the renderer the same font
system, so a chrome no longer loads fonts itself.

**The contract between the executable and a chrome:**

- **The chrome lays out; the host validates** (plan §4, §13).
  - The executable keeps one target for each surface name the chrome has used, never more than
    `MAX_SURFACES`, presenting as the renderer does (shared memory for the CPU renderer). A
    ninth name is refused, with a diagnostic, and not shown.
  - A *listed* surface is placed at its rectangle, clipped to the window. It takes pointer input
    on its `input` area, clipped to the surface, which the executable maps one to one to
    `sighurt_wayland::InputRegion` (placement.rs:79–90).
  - A listed surface with an empty rectangle has no room: it is unmapped with empty metrics, as
    for any target (decision 13).
  - A name the chrome used before but no longer lists is *hidden*: unmapped, keeping its
    metrics. Today's page-area surface works this way while an engine page shows.
  - Engine views go at `layout.page`, and the chrome's surfaces above them, in the chrome's
    order. The host still validates the whole placement and refuses a bad one whole (decision
    34).
- **Memory: the bound is the host's.**
  - Per surface: at most 3 buffers at its latest size, plus retiring ones until the compositor
    releases them (decision 39). A hidden surface keeps them, as every SHM target does today
    (`TargetState::unmap` leaves the buffers alone, targets.rs:165–172).
  - Strips are small: a 1920×44 strip at 200 % is 3840×88×4 B, about 1.4 MB a buffer.
  - The page-area surface is the big one: a 1920×1000 page at 200 % is 3840×2000×4 B, about
    30.7 MB a buffer, and 92 MB for three. A chrome that repaints rarely holds two in practice:
    one the compositor holds, one to draw into. Today's chrome already has this surface; this
    design neither adds it nor makes it larger.
  - A chrome with a second page-sized surface doubles that, and the trait's documentation says
    so.
  - Freeing the free buffers of a hidden SHM target would lower this, for engine views as well.
    That is the host track's call: requested in part 3, not assumed here.
  - A native renderer's swapchain holds 2 to 4 images of the surface's size, the same order as
    the SHM bound (RENDER.md §11).
- **The page area when no engine shows** (home, errors, blank): the chrome draws it on a surface
  of its own at `layout.page`, listed only while it is needed (decision 18).
- **Damage is the chrome's to report** (`Repaint`).
  - An identical snapshot, pointer motion within a control, and everything the chrome doesn't
    show report nothing. Nothing in the API ticks.
  - The executable sends a `Snapshot` only when the core said the chrome changed
    (`Effect::RedrawChrome`), never for a page frame (§26, decision 46).
- **Every string in the snapshot arrives contained.** The executable passes each through
  `contained` before the chrome sees it, and each field's documentation states its bound:

  | Field | Bound |
  |---|---|
  | title | 200 characters |
  | URL (the committed one) | 32 768 characters, the address editor's limit (decision 20) |
  | status | 2 000 characters |
  | `TabContent::Error` message | 4 000 characters |
  | an engine row's name / detail | 100 / 500 characters |
  | `TabSnapshot::engine` (a tab's engine) | 100 characters, an engine row's name's bound |
  | keymap indicator | 64 characters |
  | configuration path | 1 000 characters |

  The title, status, error and engine-row bounds are the ones sighurt-ui applies itself today
  (state.rs:116–125, chrome.rs:48 and 927–952), moved to where a third-party chrome can't
  forget them. The URL's is the address editor's cap (text_edit.rs:26); the indicator's and the
  path's are new. A chrome draws page text only where page text belongs, and never shows a title
  as the address (plan §17).
- **A chrome must show `status` whenever it is `Some`:** visibly, not under page content, cut to
  fit if it must. `status` carries `sig`'s own notices (configuration errors, downloads, refused
  prompts; plan §17, decisions 46 and 47), which the user must be able to see.
- **Keyboard.**
  - The core decides where the keyboard is. The chrome follows `Focus` events and reports its own
    moves (`ChromeAction::Focus`), as in decision 40.
  - When the chrome has the keyboard, the host's logical keyboard focus goes to the surface of
    its latest `TextField`, or to its first listed surface if there is none.
  - When the page has the keyboard and no engine view shows, it goes to the chrome surface whose
    rectangle is the page's.
- **Tabs by identity.** A click on a tab yields `SelectTab(id)` or `CloseTab(id)`, never a
  position, so a tab list that changed between the snapshot and the click can't select the
  wrong tab (§28, question 2). The core gains `BrowserEvent::SelectTab(TabId)` and
  `CloseTab(TabId)`, with the same rules as `tab.select` and `tab.close`. The numbered commands
  remain for keys and `:commands`.
- **Chrome commands.**
  - `chrome.<action>` parses into `Command::Chrome(action)`. The action is kebab-case ASCII,
    1–32 characters, and takes no argument (§1.10).
  - At load, the core checks every binding's chrome command against the selected chrome's
    `commands`, through `CommandSet`. An unknown one is an error.
  - When the command runs, the core returns `Effect::ChromeCommand(action)`. The executable
    delivers it as `ChromeEvent::Command`, unless the running chrome doesn't have that action;
    then it shows a notice. That can happen only for a command typed after `:`, or after
    `[ui] chrome` changed without a restart.

## 2.7 The default chrome through it: `standard`

sighurt-ui's `Chrome` struct keeps all its behavior. It is renamed `Model`, which frees the name
for the trait. A thin adapter is the `standard` chrome:

```rust
// ui/sighurt-ui/src/standard.rs (a sketch: options(), layout_of() and rect() are small helpers
// of that step, and Damage::surfaces() lists the names of the damaged surfaces)
pub const KIND: ChromeKind = ChromeKind {
    name: "standard",
    check,
    commands: &["toggle-tabs"],
    new: |fonts| Box::new(Standard::new(fonts)),
};

pub struct Standard { model: Model, layout: ChromeLayout }

impl Chrome for Standard {
    fn handle(&mut self, event: ChromeEvent) -> Vec<ChromeAction> {
        let actions = match event {
            ChromeEvent::Configure { theme, settings } => self.model.configure(&theme, options(&settings)),
            ChromeEvent::Resize(size) => self.model.resize(size),
            ChromeEvent::Snapshot(snapshot) => self.model.set_snapshot(snapshot),
            ChromeEvent::Focus(focus) => self.model.set_focus(focus),
            ChromeEvent::WindowFocus(event) => self.model.window_focus(event),
            ChromeEvent::Pointer { surface, event } => {
                let rect = self.rect(surface);
                self.model.pointer(rect, &event)
            }
            ChromeEvent::Key(key) => self.model.key(&key),
            ChromeEvent::TextInput(event) => self.model.text_input(&event),
            ChromeEvent::Pasted { request, text } => self.model.paste(request, &text),
            ChromeEvent::Command(action) => self.model.command(&action),
            _ => Vec::new(),
        };
        // Surfaces, bottom to top: "page" while the model draws the page area, "tabs" with
        // tabs = "left", then "chrome" (tab strip and toolbar) and "status" (the strip, or the
        // overlay with InputArea::Rects(vec![]) while there is status text).
        self.layout = layout_of(&self.model);
        let damage = self.model.take_damage();
        actions.into_iter().map(ChromeAction::from)
            .chain(damage.surfaces().map(ChromeAction::Repaint))
            .collect()
    }

    fn layout(&self) -> &ChromeLayout { &self.layout }

    fn draw(&mut self, surface: SurfaceName, pen: &mut Pen) {
        // paint.rs draws whatever the model shows on that surface.
        crate::paint::draw(pen, &mut self.model, surface);
    }
}
```

`UiAction` becomes `ChromeAction` one to one, with two exceptions: a tab click becomes
`SelectTab` or `CloseTab`, and `TextInput` names the `"chrome"` surface. The surface names stay
`"chrome"`, `"status"` and `"page"`, which are the roles in today's diagnostics JSON, so
`apps/sighurt-browser/tests/headless.sh` keeps working unchanged.

On the executable's side, `Surfaces { top, status, page }` becomes a small map from name to target
lease, and `paint_chrome`, `surface_of`, `surface_rect` and `placement` read the chrome's layout
through the trait.

## 2.8 Proof chrome: `sighurt-chrome-line`, and vertical tabs

**`sighurt-chrome-line`** has feature `chrome-line` and config name `line`.

- **Layout.** One line, 28 px high at a 14 px font, at the top, or at the bottom with
  `[chrome.line] position = "bottom"`. Its surfaces are `"line"` and, while it draws the page
  area, `"page"`.
- **Unfocused**, the line shows the tab's position (`[2/5]`), the keymap indicator, the
  committed address, a loading mark, and the status text, cut on the right.
- **Focused** (by a click on it, or `location.focus`), it is an editor. Enter opens what was
  typed, a line starting with `:` runs a command, and Escape gives the page the keyboard.
- **Home and error pages** are a few lines of text.
- **No buttons, no tab strip, no commands of its own.** Tabs are reached by keys and
  `:tab.select 3`.

It depends on `sighurt-api` and `sighurt-ui`, and on nothing of the core, the host or any engine.
From sighurt-ui it uses the extension module and the helpers that are part of the chrome API:

- `Fonts`, the cached font system;
- `TextEditor`, the address editor (grapheme movement, selection, paste, IME preedit, the 32 KiB
  cap);
- `Pen`, sighurt-render's scene builder (it was `Canvas` and its `Pen`, which painted into a
  `PixelBuffer` on the CPU, until the render framework, RENDER.md).

**What the proof shows, and what it doesn't.**

- It shows that the trait, the events and the actions are enough for a chrome with other
  surfaces, another layout, and its own editing and painting.
- It shows that such a chrome needs nothing of the core, the host or the engines.
- It doesn't show that a chrome is cheap to write without sighurt-ui. A chrome that brings its
  own text stack also needs its own address editor, which is the bulk of the work:
  `text_edit.rs` is 648 lines.

**Vertical tabs** are the standard chrome's `tabs = "left"`. They prove the other half of the
API: a chrome with more surfaces than today's fixed three. The list is its own `"tabs"` surface
on the left, below the toolbar. It scrolls with the wheel (its own `Pointer` scroll events), its
rows are hit-tested like today's tabs, and the toolbar stays across the top.
`chrome.toggle-tabs` hides and shows the list.

## 2.9 Selection and registration

**In the workspace,** like engines (decision 1):

```toml
# apps/sighurt-browser/Cargo.toml
[features]
default = ["engine-servo", "engine-wasm"]
chrome-line = ["dep:sighurt-chrome-line"]
keymap-modal = ["dep:sighurt-keymap-modal"]
```

```rust
// apps/sighurt-browser/src/registry.rs
pub const CHROMES: &[ChromeKind] = &[
    sighurt_ui::standard::KIND,                             // "standard": always built
    #[cfg(feature = "chrome-line")] sighurt_chrome_line::KIND,
];
pub const KEYMAPS: &[KeymapKind] = &[
    #[cfg(feature = "keymap-modal")] sighurt_keymap_modal::KIND,
];                                                          // "bindings" is the core's own
```

**What the core gets.** The executable puts the kinds into the core's `Environment`, which gains
four fields:

```rust
// sighurt_core::state
pub struct Environment {
    pub engines: BTreeMap<String, EngineCapabilities>,              // as today
    pub home_dir: Option<PathBuf>,                                  // as today
    /// The build's keymaps besides `bindings`, which the core always has.
    pub keymaps: Vec<KeymapKind>,
    /// The build's chromes, as the core checks them.
    pub chromes: Vec<ChromeChecks>,
    /// The `[ui.colors]` names: sighurt_ui::Colors::NAMES.
    pub color_names: &'static [&'static str],
    /// Which `[engines.<e>]` settings each engine of the build honors.
    pub engine_settings: BTreeMap<String, &'static [&'static str]>,
}

/// What the core needs of a chrome: ChromeKind without `new`.
#[derive(Clone, Copy)]       // Debug, PartialEq and Eq compare by name
pub struct ChromeChecks {
    pub name: &'static str,
    pub check: fn(&Table) -> Vec<String>,
    pub commands: &'static [&'static str],
}
```

- **`Config::parse(user, &Environment)`** validates the whole file in one pass, and returns the
  problems in one list. That includes each implementation's table, through its check, with the
  problems prefixed `[keymap.modal]` and the like. The core now checks `[ui.colors]` names too,
  so the executable's separate check (`persistence::colors`) goes away.
- **The core builds and holds the keymap:** `(kind.new)()`, then `configure`. On a reload it
  configures the keymap it has, or replaces it when `[input] keymap` names another. The core
  stays pure, since keymaps do no I/O.
- **The executable builds and holds the chrome** at startup. It sends `Configure` on every valid
  reload. A changed `[ui] chrome` gets the "applies at restart" notice.
- **Naming follows engines:** crate `sighurt-chrome-<name>`, feature `chrome-<name>`, config
  `<name>`; and `sighurt-keymap-<name>`, `keymap-<name>`, `<name>`. The defaults stay where they
  are (`sighurt-ui`, `sighurt-core`) and are always built, since a browser has a chrome and reads
  keys. The proofs are not in `default`; CI builds them with their features.

**Outside the workspace.** Someone who writes a chrome or keymap in a repository of their own
puts it into their build of `sig` in four steps. None of them touches the core, the engines or
the host:

1. In `apps/sighurt-browser/Cargo.toml`, add the dependency and a feature:
   ```toml
   [features]
   chrome-mine = ["dep:sighurt-chrome-mine"]
   [dependencies]
   sighurt-chrome-mine = { git = "https://example.org/sighurt-chrome-mine", optional = true }
   ```
2. Add one line to `registry.rs`: `#[cfg(feature = "chrome-mine")] sighurt_chrome_mine::KIND,`.
3. Patch the crate's dependencies on Sighurt to this workspace's paths, in the root
   `Cargo.toml`. The URL must be the one the crate's own manifest uses:
   ```toml
   [patch."https://github.com/gusahlg/oxide"]
   sighurt-api = { path = "sighurt-api" }
   sighurt-ui = { path = "sighurt-ui" }      # for a chrome
   ```
   Without the patch, the crate's `sighurt-api` is a second crate. Its types are not this
   build's types: the build fails with `expected TabId, found a different TabId`. It would also
   have its own id counters, which decision 9 forbids.
4. Build with `tools/cargo build -p sighurt-browser --features chrome-mine`, and **don't run
   `cargo update`**. Cargo adds the new crate's dependencies to `Cargo.lock`, preferring the
   versions already locked. Check that `git diff Cargo.lock` shows only additions: a changed
   version of one of Servo's dependencies means rebuilding Servo (CLAUDE.md, "Common
   pitfalls").

Step 3's mechanism was checked in a scratch workspace (Evidence). The alternative is simpler:
copy the crate into the workspace and add it to `members`; it then needs no patch.

## 2.10 Compatibility, without a promise yet

**What the API is.** It is internal, like the rest of `sighurt-api` (plan §5): rebuilt with the
workspace, with no binary interface and no loading at run time (plan §1 postpones native plugin
loading). It is not a plugin framework either (plan §4): implementations are compiled in and
listed in the registry, as engines are. There is no version constant, and no promise that an
implementation keeps building.

**How changes are kept visible:**

- **A changelog** at the end of this document, moving next to the code once it exists. Every
  change to the keymap module, the extension module, or a type they name gets an entry that
  says what an implementation must change.
- **Guard tests that implement both traits from outside their crates.** They use only what this
  part documents: `crates/sighurt-api/tests/keymap.rs` for the keymap API, and
  `ui/sighurt-ui/tests/extension.rs` for the chrome API. A change that breaks them needs a
  changelog entry.
- **The proofs live in the workspace** and are updated in the change that breaks them.

**Changes already expected** (each breaks some out-of-tree implementations):

- `TextInputState` gaining a field identity (decisions 44 and 55). It breaks every chrome that
  builds one with a struct literal; the standard chrome does (chrome.rs:820–835).
- Touch or tablet variants of `PointerEvent`. They break chromes that match it exhaustively; the
  standard chrome does.
- New commands. They break exhaustive matches on `Command`.

**A promise later.** It can come once both proofs exist and someone outside the workspace uses
the API (plan §7: finalize a contract after two implementations exercise it), and only on these
conditions:

- `TextInputState` has its field identity, or it and `SurroundingText` are `#[non_exhaustive]`
  with `new()`.
- The rule "don't match `Command`, `KeyAction` or `ChromeAction` exhaustively" is stated as a
  condition of the promise, not as a guarantee, since nothing enforces it.

## 2.11 Review against the plan

| Question (§28) | Answer |
|---|---|
| 1. One owner per field? | Mode and unfinished sequence: the keymap. Taken presses, taken buttons and the keyboard focus: the core. Routing, including where a page's navigation goes: the core. What an engine can load: the engine. Layout, editing, painting, and which of its widgets has the keyboard: the chrome. Targets, placement and input regions: the executable and the host. Each setting: §1.3. |
| 2. Can a delayed event tell what it belongs to? | Pastes carry a `RequestId`. Tab actions carry a `TabId` (new). Text fields name their surface. A navigation request carries its `RequestId` and `DocumentGeneration`, and one from a replaced document is denied. Chrome commands carry no identity: like keys, they act on the chrome's current state. |
| 3. A page-sized copy or an unconditional tick? | None. Chromes draw scenes that the renderer renders into lent buffers, and the API has no clock. A page-sized repaint happens only when the chrome's own page changes (decision 18). The status overlay has a fixed size, so a new hovered URL starts no new buffer generation. |
| 4. A renderer detail leaking into the core or the guest contract? | The chrome contract stays in sighurt-ui, and draws scenes (sighurt-render), not pixels; the core sees only `[ui] renderer`'s name. `sighurt-api` gains the neutral keymap and settings modules, plus two engine-contract items (`NavigationRequested`, `AnswerNavigation`) that carry only ids and a URL. The core sees nothing of a chrome but its checks. |
| 5. Claims backed? | Evidence lists what was checked: the code facts this design relies on, and the out-of-tree patch recipe. It also lists what wasn't: these signatures have not been compiled since this revision, nor ever on Rust 1.95; WPE; and the hand-off's latency. Every step in part 3 names its test. |

**§26.**

- No chrome repaints from page animation: no event reaches a chrome for a frame.
- No permanent timer: nothing in the API ticks.
- Bounded queues and buffers: at most 8 surfaces of at most 3 buffers each, at most 16 held
  navigation requests per view, bounded snapshot text, and at most 4 keys in a keymap's
  unfinished sequence.
- Core tests need no display: keymaps are pure.
- A chrome change rewrites no browser state.

**Security.**

- Chromes and keymaps are trusted native code compiled into `sig`. The API keeps correct code
  correct; like the in-process engines, it is not a sandbox (CLAUDE.md, Security).
- Page text reaches a chrome only after `contained`.
- A keymap sees passwords as they are typed, and must neither keep nor log them.
- A page can't issue commands, and can't send keys to the keymap.
- Through `NavigationRequested`, a page can make the core open a URL in another engine only
  within 5 s of the user's own click or key press in it, once per activation, and only where the
  user's own routing rules send that URL. Pages never open `sig:` URLs: Servo can't load them,
  so they come as `OpenRequested`, which the core refuses.

---

# Part 3. Migration plan

Each step is one reviewable change that leaves everything passing:

- `tools/cargo test` and `clippy -D warnings` on the crates it touches, and `cargo fmt`;
- where `sig` changes, `apps/sighurt-browser/tests/headless.sh`;
- where Servo changes, `sighurt-engine-servo`'s tests and clippy, on this desktop or louise-pc.

Each step also documents its keys and commands in `default-config.toml`, where
`the_default_file_documents_every_command` and `the_documented_examples_are_valid` keep that
honest, and in the README's command table. Tests that exist today and change in a step are named
as "changed".

**A. The APIs, with no change in behavior**

1. **`settings`, `keymap`, `CommandSet` and `KeyboardFocus` in `sighurt-api`; the `bindings`
   keymap through them.**
   - `Chord` moves out of the core.
   - `CommandSet` parses commands for every table; until step 5 adds chrome commands, it has no
     chrome actions to check.
   - `KeyboardFocus` replaces both `Focus` types (`Location` becomes `Chrome`; `[keys.location]`
     keeps its name). This touches the core, sighurt-ui and the executable's mapping between the
     two (effects.rs, app.rs).
   - `KeyState` asks `dyn Keymap`.
   - *Tests:*
     - Every existing key test passes after the rename alone. Changed: the `Focus::Location` uses
       in `keys.rs` and `crates/sighurt-core/tests/browser.rs`, and `focus_follows_the_core` in
       `ui/sighurt-ui/tests/interaction.rs`.
     - New core tests with a fake keymap:
       `a_taken_press_takes_its_release_whatever_the_keymap_says` (it takes the press, then
       passes the repeat: the release is still taken);
       `run_then_pass_delivers_to_the_new_focus` (`location.focus`, then the press and its
       release arrive as `ChromeInput`); `the_keymap_hears_context_changes`.
     - `crates/sighurt-api/tests/keymap.rs` implements `Keymap` from outside the crate.
2. **`[input] keymap`, the kinds in `Environment`, `Config::parse(.., &Environment)`, notices.**
   - *Tests:*
     - An unknown keymap is an error that lists the build's keymaps.
     - `[keymap.x]` for a keymap not in the build is accepted, with a notice naming the build's
       keymaps.
     - `[keys]` is unused while `modal` is selected: a notice. The reverse gets the same.
     - `[keymap.bindings]` is an error that points to `[keys]`.
     - A fake kind's problems arrive prefixed `[keymap.fake]`, in the same list as the others.
     - A reload that keeps the keymap configures it and keeps its state.
     - A switch replaces it, while a held key's release is still taken.
     - Changed: every caller of `Config::parse`, which now passes an `Environment`.
3. **`sighurt_ui::extension`; the `standard` chrome through it; tab actions by id; the
   executable contains the snapshot's strings.**
   - sighurt-ui's `Chrome` becomes `Model`, and `Standard` is added.
   - `contained` becomes public, and `Snapshot::keymap` and `TabSnapshot::engine` are added.
   - The core gains `BrowserEvent::SelectTab` and `CloseTab`.
   - *Tests:*
     - Every sighurt-ui test passes, with the eleven golden images unchanged byte for byte.
       Changed: `tabs_select_close_and_open`, which expects `SelectTab(id)` and `CloseTab(id)`
       instead of numbered commands.
     - The new `ui/sighurt-ui/tests/extension.rs` drives `Box<dyn Chrome>` only: a tab click gives
       `SelectTab(id)`; an identical snapshot gives no `Repaint`; a resize repaints every listed
       surface; painting through the trait matches a golden image.
     - In the core: `closing_a_tab_by_id_after_the_list_shifted`, and
       `selecting_a_closed_tab_by_id_does_nothing`.
     - In `sig`: a unit test that the snapshot builder contains a title holding a bidi override
       and a newline.
4. **`sig` drives `Box<dyn Chrome>` with named surfaces and input areas; `[ui] chrome`; the core
   checks `[ui]`.**
   - The name-to-lease bookkeeping becomes a pure function, `ChromeTargets::place(layout,
     window)`, so that it can be tested without a compositor: the executable only creates the
     leases it asks for.
   - *Tests:*
     - Unit tests of `ChromeTargets`: names get targets in order; a ninth name is refused with a
       diagnostic; a rectangle outside the window is clipped; an input rectangle is clipped to
       its surface; a name no longer listed is hidden with its last rectangle.
     - `headless.sh`, unchanged (roles `chrome`, `status`, `page`), plus one new check: over the
       main scenario, no chrome target's peak SHM buffers exceed 3 (the diagnostics already keep
       peaks per target, decision 47).
     - In the core: an unknown color name is a problem in the one list; `[ui] chrome = "nope"`
       is an error listing the build's chromes; a reload that changes `[ui] chrome` gives the
       restart notice.
     - Changed: `config::tests::keys_and_ui_are_the_users_alone` (`[ui] font` becomes valid, so
       it asserts that a misspelt `[ui] fnt` is an error);
       `persistence::tests::unknown_color_names_are_errors` moves to the core.

**B. Commands and proofs**

5. **`chrome.<action>`, and the standard chrome's `chrome.toggle-tabs`.**
   - *Tests:*
     - The API parses and prints `chrome.toggle-tabs`; an argument, an empty action and
       upper-case letters are refused.
     - `CommandSet` refuses an action the selected chrome lacks, naming the ones it has, and
       the core reports it at load in the one list, for `[keys]`, `[input.buttons]` and a fake
       keymap's table alike.
     - The core turns the command into `Effect::ChromeCommand`.
     - `sig` delivers it, or shows a notice for an action the running chrome lacks.
     - The standard chrome hides and shows its tabs (layout test).
6. **`page.send-key`.**
   - *Tests:*
     - The API parses and prints it (`page.send-key ArrowDown`, `shift-space`), and refuses
       characters and `code:`.
     - In the core: a press and a release reach the shown view, back to back, with the right
       evdev code.
     - Nothing happens while the chrome has the keyboard, or while no engine view shows.
     - Run by a keymap, it records activation; run as `BrowserEvent::Command`, it doesn't.
7. **`sighurt-keymap-modal`** (§2.5).
   - *Tests:*
     - The crate's own tests: modes, sequences, failed sequences, ignored modifiers, swallowing
       or passing, the typing mode, `RunThenPass`, repeats, the indicator.
     - Every configuration problem, including a prefix in the same mode, and a sequence starting
       with a typing key in a `pass` mode.
     - A new `modal` scenario in `headless.sh`, with the demo engine: `g t` makes tab 2 active
       (diagnostics); `i` then "hej" reaches the page (the session title); Escape; `x` closes
       the tab (tab count).
8. **`sighurt-chrome-line`** (§2.8), with sighurt-ui's `Canvas` split out of `paint_cpu`.
   - *Tests:*
     - Driven through the trait: typing an address and pressing Enter, `:tab.next`, Escape, a
       paste.
     - Golden images at 100, 150 and 200 %, looked at.
     - A click places the caret where it is drawn, at four scales.
     - In `headless.sh`, with `[ui] chrome = "line"`: open a demo page by typing; the
       diagnostics show the roles `line` and `page`; 3 s idle gives 0 chrome paints.
     - Building it proves that it needs nothing but `sighurt-api` and `sighurt-ui`.

**C. The settings** (each independent of the others after step 4, except where noted)

9. **`zoom`.**
   - *Tests:* a new tab starts at it; `page.zoom-reset` returns to it; a restored tab keeps its
     saved zoom; a reload doesn't change open tabs.
10. **`[input] scroll-speed` and `[input.buttons]`.** This step touches the core, sighurt-ui
    (the side-button code goes) and `app.rs` (`BrowserEvent::Button`).
    - *Tests:*
      - Pixel and wheel deltas are scaled, and a non-zero delta never becomes zero.
      - A bound button runs its command, and its release is taken, even after a reload that
        unbinds it.
      - An unbound `back` reaches the page, with its release.
      - Over the chrome, a bound button works and an unbound one goes nowhere.
      - The defaults behave exactly as today: `the_mouse_back_button_goes_back` passes
        unchanged.
      - Changed: `toolbar_buttons` in `ui/sighurt-ui/tests/interaction.rs` loses its side-button
        checks, which move to the core.
11. **`[keywords]`.**
    - *Tests:* `router::fixup` cases: a keyword with a query, a keyword alone, a template
      without `{}`; URLs, paths and hosts are never keywords; every validation error.
12. **`page.open-in`, and chosen views.**
    - *Tests:*
      - It parses and prints.
      - Reopening makes a switch view on the named engine, and back returns to the previous
        segment.
      - It takes an address argument.
      - On a `sig:` page, and for an engine not in the build or disabled, it reports why.
      - The view it opens is chosen, and typing an address makes it unchosen.
13. **`[routing.<e>] hosts` with the hand-off of page navigations, in one change** (sighurt-api,
    the core, and the Servo adapter, which goes through the Servo track's review). Host rules
    and links ship together, so a host rule never half-works.
    - *Tests:*
      - Host patterns: exact versus wildcard versus a longer wildcard; a pattern claimed twice;
        IDNA normalization; refused forms (a port, a path, `*` in the middle); an explicit
        choice beats a host rule; a disabled engine is an error.
      - The core, with fake engine events:
        - a `NavigationRequested` to a host another engine claims, within 5 s of a click in the
          view, is denied, and a switch view opens on that engine;
        - the activation is used up, so a second request is allowed;
        - one without activation is allowed;
        - one from a chosen view is allowed;
        - one to an unclaimed host, from a view a host rule put in another engine, is handed back
          to the default engine;
        - one to the same engine is allowed;
        - one from a replaced document is denied;
        - every `NavigationRequested` gets exactly one `AnswerNavigation`.
      - Servo, in `engines/sighurt-engine-servo/tests/engine.rs`:
        - a link click on a test page gives `NavigationRequested`;
        - when it is allowed, the page commits with an adapter id;
        - when it is denied, nothing commits and the page stays;
        - closing the view while it holds one denies it, and nothing commits after the close;
        - a 17th held request is allowed at once.
      - End to end, `sig` with Servo in headless sway with the virtual pointer: a local page
        links to `http://127.0.0.1:<port>/hello.wasm`. With `[routing.wasm] origins =
        ["http://127.0.0.1:<port>"]`, clicking the link opens the app in the Wasm engine (the
        diagnostics show the active tab's view on `wasm`). This is the case decision 55 left
        open.
      - `default-config.toml` documents host rules and the link rule: activation, chosen
        views, and the iframe limit.
14. **`[engines.<e>]` settings.**
    - *Tests:*
      - The core's validation; an unsupported setting is an error that names the engine.
      - The old format's hints (§1.11). Changed: `an_old_file_with_a_version_says_what_moved`,
        which expects the per-key hints instead of "[engines]: unknown field `wasm`".
      - Servo: `navigator.userAgent`, shown in a test page's title, is the configured string;
        `storage = "ephemeral"` gives `ServoDiagnostics::profile == None`.
      - Wasm: the kv store's and the module cache's directories are temporary, and gone after
        `shutdown`.
15. **`[chrome.standard]`: `status`, `buttons`, `compact`, and `tabs = "top" | "hidden"`.**
    - *Tests:*
      - Layout tests for each option, with hit targets on drawn controls at four scales.
      - Golden images for `compact`, the overlay, and no buttons.
      - In `headless.sh`: switching `status` live changes the mapped surfaces (diagnostics)
        without a restart.
      - **Hovering under the overlay:** on `demo:resize`, whose status text follows the
        pointer, with `status = "overlay"`, move the virtual pointer into the overlay's
        rectangle and hold it there for 1 s. The `status` surface is mapped, the placement
        counter rose by at most 1 after the move (the overlay doesn't flicker), and the demo
        page still reports the pointer (its status text changes with the motion).
16. **`tabs = "left"`.**
    - *Tests:* hit targets at four scales; scrolling a list of 40 tabs; a golden image; in
      `headless.sh`, a click on the third row makes tab 3 active.
17. **`[ui] font` and `font-size`.**
    - *Tests:* the heights are the arithmetic of §1.8 (14 px gives 40/44/20, 18 px gives
      51/57/26); golden images at 18 px, with the bundled test font; a missing family falls back
      and logs it.
18. **`[downloads] directory` and `[session] restore`.**
    - *Tests:* a download lands in the configured folder; `~/` expands; a folder that can't be
      created gives a notice and no file; with `restore = false`, `Start` gets no session and
      nothing is written.

**Requests to other tracks:**

- **Servo track:** step 13's adapter half (hold the `NavigationRequest`; report
  `NavigationRequested`; deny on close, on a new document and at shutdown; at most 16 held), and
  step 14's `user-agent` and `storage`.
- **WPE track:** `user-agent` and `storage`; `NavigationRequested` for main-frame navigations,
  using WebKit's user-gesture flag if it has one; and whether `javascript` can be honored.
- **Host track (optional):** free the free SHM buffers of a hidden target. That would lower
  §2.6's memory bound, for engine views as well. Its test belongs in
  `frontends/sighurt-wayland/tests/headless.rs`.
- **Orchestrator:** once step 4 lands, CLAUDE.md's crate table needs `settings` and `keymap` in
  sighurt-api's module list and `extension` in sighurt-ui's. Its command list needs
  `page.open-in`, `page.send-key` and `chrome.<action>`. The README's command table needs the
  same, and STATUS needs the decisions below once the user accepts them.

**Order and conflicts.**

- Steps 1–4 touch the core, sighurt-ui and `sig`'s `app.rs`, which other tracks are changing.
  They are best scheduled after the current integration work (Servo in `sig`, WPE) lands.
- Steps 9, 11 and 12 touch only the core and can go in at any time.
- Step 13 needs Servo builds, and its WPE half waits for the WPE adapter.

---

## Decisions this proposes (for STATUS, once the user accepts them)

1. **The configuration stays at version 1.** The additions of part 1 are optional, and §1.1's
   rules apply:
   - tables and names of implementations not in the build are accepted with a notice naming what
     the build has;
   - tables nobody reads get a notice;
   - the core checks `[ui.colors]` names.

   *Amends 28* (`[engines]` gains `[engines.<e>]` tables; `[ui]` gains `chrome`, `font` and
   `font-size`), *41* (`[ui]` holds more than colors) and *21* (`sig` no longer checks the color
   names itself; the core does, with the names from the build).
2. **Per-site engines and links.** Per-site engines are `[routing.<engine>] hosts`, exact or
   `*.` patterns, the most specific one winning. `page.open-in <engine> [address]` reopens a page
   in an engine and makes its view chosen. Engines ask before a navigation their page started
   (`NavigationRequested` and `AnswerNavigation`). The core hands a navigation to another engine
   only when routing names one, the view isn't chosen, and the user clicked or typed in the page
   within 5 s, once per activation. The Servo 0.5 limit is recorded: a user's click on a link
   inside an iframe moves the whole tab.

   *Amends 53* (Servo asks before performing a navigation it would load; its hand-off of other
   schemes is unchanged), *55* (settles its open item about links) and *57* ("Not changed": the
   iframe ambiguity now also covers host hand-offs, bounded by activation).
3. **Engine settings are a neutral vocabulary** (`user-agent`, `storage`) that the core
   validates and the registry declares support for. An unsupported setting is an error, and the
   settings apply at restart.

   *Amends 28.*
4. **The mouse's side buttons are commands in `[input.buttons]`**, with today's behavior as the
   default. They reach the core first wherever the pointer is (`BrowserEvent::Button`), and a
   taken press takes its release. The chrome no longer handles them.

   *Amends 27.*
5. **The extension API.** The keymap API is `sighurt_api::{settings, keymap}`,
   `input::KeyboardFocus` and `commands::CommandSet`. The chrome API is `sighurt_ui::extension`,
   with sighurt-ui's `Fonts`, `TextEditor` and `Canvas`.
   - Implementations are compiled in by feature and chosen by `[input] keymap` (live) and `[ui]
     chrome` (restart).
   - The `standard` chrome and the `bindings` keymap go through the API.
   - The core keeps the rule that a taken press takes its release and repeats, whatever a keymap
     does: keymaps decide only fresh presses and repeats of what they took.
   - A chrome has up to 8 named surfaces, each with an input area.
   - There is no stability promise until §2.10's conditions hold.

   *Amends 18* (three fixed chrome surfaces become named ones) and *40* (both `Focus` types
   become `KeyboardFocus`; `UiAction` becomes `ChromeAction`, with tab actions by id).
6. **New commands.**
   - `page.open-in`.
   - `page.send-key`: user activation only when a keymap runs it, and only while the page has the
     keyboard.
   - `chrome.<action>`: no argument, checked at load against the selected chrome's actions.

   *Amends no decision.* CLAUDE.md's command list and the README's table grow.

**Existing tests that change:**

- `crates/sighurt-core/src/config.rs`:
  - `keys_and_ui_are_the_users_alone`: `[ui] font` is valid now, so it asserts a misspelt key
    instead;
  - `an_old_file_with_a_version_says_what_moved`: it expects the per-key `[engines.wasm]` hints
    of §1.11;
  - every test that calls `Config::parse`, for the new `Environment` argument.
- `crates/sighurt-core/src/keys.rs` and `crates/sighurt-core/tests/browser.rs`: the `Focus::Location` rename.
- `apps/sighurt-browser/src/persistence.rs`: `unknown_color_names_are_errors` moves to the core.
- `ui/sighurt-ui/tests/interaction.rs`:
  - `focus_follows_the_core` (the rename);
  - `tabs_select_close_and_open` (tab actions by id);
  - `toolbar_buttons` (side buttons go to the core).

`typos_are_errors` still passes: `[engines.servo] command` remains an error that mentions servo.

## Evidence

**Checked for this revision** (2 October 2026, on this desktop):

- **The code facts this design relies on**, read in the tree and in the pinned sources:
  - Plan §8 requires overlay input regions, and the host has `InputRegion`
    (frontends/sighurt-wayland/src/placement.rs:79–90).
  - Unmapping keeps a target's buffers (targets.rs:165–172), which are freed only when a new
    size is acquired (shm.rs:178–200).
  - The host folds side buttons into `Back` and `Forward` (input.rs:688–698).
  - The executable sends chrome pointer events straight to the chrome
    (apps/sighurt-browser/src/app.rs:488–497), and keeps the chrome's page surface hidden rather than
    empty (app.rs:823–849).
  - The core records activation only in `keyboard()` and `pointer()` (state.rs:1512, 1544), and
    `BrowserEvent::Command` carries no time (state.rs:103).
  - The line gives the page the keyboard before it runs a `:command` (chrome.rs:667–675).
  - sighurt-ui contains the error message, the engine rows and the URL itself
    (chrome.rs:927–952), and builds `TextInputState` with a literal (chrome.rs:820–835).
  - Servo 0.5's `NavigationRequest` is owned, with `allow(self)` and `deny(self)`, and a
    `pub(crate)` `pipeline_id`; dropping it allows the navigation
    (servo-0.5.0/webview_delegate.rs:32–71). The constellation keeps one pending approval per
    pipeline (servo-constellation-0.5.0/constellation.rs:3797–3826).
  - The Servo adapter holds the request until `pump` (delegate.rs:160, runtime.rs:676–704) and
    decides what it loads in `loads_itself` (view.rs:332–338).
  - The Wasm engine hands every guest navigation and link to the core
    (engines/sighurt-engine-wasm/src/view.rs:531–543, 832–849).
  - servo-config 0.5 has `user_agent` and `js_disable_jit`, but no preference that turns script
    off (prefs.rs:280, 402).
- **The out-of-tree recipe of §2.9, step 3**, in a scratch workspace outside the repository,
  built with `tools/cargo` (`CARGO_HOME` and `CARGO_TARGET_DIR` in the scratch directory):
  - `thirdparty` depended on `sighurt-api = { git = "file:///home/gusahlg/repos/sighurt",
    branch = "rewrite" }` and returned a `Command` and a `TabId`.
  - `host` depended on `sighurt-api` by path and on `thirdparty`, and bound both values to this
    workspace's types.
  - With `[patch."file:///home/gusahlg/repos/sighurt"] sighurt-api = { path = ".../crates/sighurt-api" }`,
    `sighurt-api` compiled once (the path copy) and the program printed `tab.new TabId(1)`.
  - Without the patch, `sighurt-api` compiled twice (the path copy and the git copy at
    3c7f5fd7), and the build failed with E0308: "expected `TabId`, found `sighurt_api::ids::TabId`",
    and the same for `Command`.

  The scratch files are not kept; the description above is enough to repeat the check.

**Not verified:**

- **The API signatures.** The first draft's signatures were type-checked once, in a scratch
  crate, with rustc 1.98.1. That crate, and the 8 tests of its prototype keymap and chrome, were
  in a temporary directory and are gone. This revision changed the signatures (the chrome API
  moved, plus input areas, `CommandSet` and chrome commands), and they have not been compiled
  since. They were never checked with Rust 1.95, the workspace's `rust-version`. The guard tests
  of steps 1 and 3 do this for real.
- **Anything about WPE:** the "expected" entries of §1.5 and §1.4.
- **Servo and the user agent:** whether Servo 0.5 applies a changed `user_agent` preference
  without a restart. It is assumed not to, so the setting applies at restart.
- **The hand-off's added latency.** It is expected to be within one owner-loop iteration, and has
  not been measured.

## §R. Review findings, and what became of them

Two reviews of the first draft gave 22 findings, numbered here in the order they came. Two pairs
describe the same problem: the overlay's input (1 and 12) and the evidence (10 and 22). Each
finding was checked against the plan and the code before it was acted on.

| # | Finding | Verdict | Where |
|---|---|---|---|
| 1, 12 | A chrome surface takes input on all of it, so a status overlay flickers (plan §8) | **Accepted.** Confirmed: `InputRegion::Rects` exists in the host, and the draft used none of it. `ChromeSurface::input` was added; the standard overlay takes no input and has a fixed size; step 15 tests the hover. | §1.8, §2.6, step 15 |
| 2 | No recipe for implementations outside the workspace | **Accepted.** The recipe was written down, and its `[patch]` mechanism was checked in a scratch workspace, both with and without the patch. | §2.9, Evidence |
| 3 | The line chrome uses sighurt-ui helpers outside the promise, so decision 7 overclaims | **Accepted.** The chrome API now lives in sighurt-ui, and `Fonts`, `TextEditor` and `Canvas` are part of it. Decision 7 says what the proof shows and what it doesn't. | §2.8, decision 7 |
| 4 | The promise freezes `TextInputState`, which decision 55 plans to change | **Accepted, resolved differently.** There is no promise, so nothing forces a version bump. The expected change is listed, and either the field identity or `#[non_exhaustive]` is a condition of any later promise. The same goes for `PointerEvent` variants. | §2.10 |
| 5 | A chrome can't offer an action that can be bound to a key | **Accepted, without arguments.** `chrome.<action>` is checked at load against `ChromeKind::commands`. Arguments are left out, because no proposed command needs one and checking them would need more API (§1.10). | §2.6, §1.10, step 5 |
| 6 | "Behaves like Vimium" overclaims | **Accepted.** The claim is reworded, and a table lists the seven things it can't do and why. `KeyAction::RunAll` is noted as a later addition for counts. | §2.5 |
| 7 | Host rules half-work (links stay) and then stick (the leaving rule) | **Accepted.** Host rules and the link hand-off are one step. The leaving rule: without an explicit choice, a clicked link goes where routing sends it, so a site no rule claims returns to the default engine. | §1.4, step 13 |
| 8 | Unused key tables are silent, and a misspelt table gets only a vague notice | **Accepted.** There are notices for unused tables, and the "not in this build" notice lists the build's implementations. | §1.1, step 2 |
| 9 | Contained text is incomplete, and nothing requires a chrome to show `status` | **Accepted.** The executable contains every snapshot string, with the bounds in a table, and the contract requires showing `status`. | §2.6, step 3 |
| 10, 22 | The evidence can't be re-run, and wasn't checked with 1.95 | **Accepted** (as a reworded claim). This task may edit only this document, so the prototype can't be added as an experiment crate. Every claim about it now says "type-checked once, not kept, not with 1.95", and steps 1 and 3 create the guard tests. | Evidence, §2.5 |
| 11 | "Keeps 0 buffers" is false; the app tests need a real `Host` | **Accepted.** Confirmed in targets.rs and shm.rs. The true bound and the worst-case memory are stated, and freeing buffers is a request to the host track. The bookkeeping becomes a pure `ChromeTargets` with unit tests, and the buffer check moved to `headless.sh`. | §2.6, step 4 |
| 13 | `HandOff` splits routing between the core and the engines; there is a simpler design | **Accepted in part.** The design is now `NavigationRequested` and `AnswerNavigation`, routed in the core, and limited to Servo and WPE. **Rejected:** moving the adapter's `file:` refusal to the core. The core already refuses that case (decision 41), and keeping the adapter's refusal means a core bug can't let a web page load a local file in Servo. | §1.4 |
| 14 | Step 13's iframe test may be impossible on Servo 0.5 | **Accepted.** The outcome is decided in advance: navigations are handed over only within 5 s of a user activation, once per activation, and the remaining case is recorded as a Servo 0.5 limit. The existing scheme hand-off has the same ambiguity, and this design leaves it unchanged. | §1.4, decision 2 |
| 15 | The chrome half moves about 12 chrome types into sighurt-api | **Accepted.** The chrome API is `sighurt_ui::extension`, and nothing moves. The core gets `ChromeChecks` and `color_names` through `Environment`. | §2.1, §2.9 |
| 16 | The stability promise is premature | **Accepted.** There is no version constant and no promise. The changelog and the guard tests stay, and the conditions for a later promise are listed. | §2.10 |
| 17 | The decisions don't say what they amend; tests change; decisions recorded too early | **Accepted.** Each decision has an "Amends" line, the changed tests are listed, and the old-format hints are kept (§1.11). The old decision 6 is folded into decision 5. Nothing goes to STATUS before the user accepts. | Decisions |
| 18 | `page.send-key` activation has no time, and its focus is unspecified | **Accepted.** Activation comes only from a press a keymap ran; the command does nothing unless the page has the keyboard; `:` commands work because the line hands the keyboard over first. | §1.7, step 6 |
| 19 | `[input.buttons]` over the chrome is unspecified; its step (9 then, 10 now) isn't core-only; `button-275` can't match | **Accepted.** `BrowserEvent::Button` and the taken-button rule are specified. The step is marked as touching sighurt-ui and `app.rs`. `button-<code>` is dropped, so no code can collide with the host's folding. | §1.7, step 10 |
| 20 | `javascript`, `density` and `button-<code>` are speculative | **Accepted in part.** `javascript` and `button-<code>` are dropped. `density` became a `compact` switch whose heights are computed from `font-size`, with nothing measured, so decision 21 stands. `buttons` and `scroll-speed` are kept, because the request listed mouse buttons and scroll speed; drop them if the user doesn't want them. | §1.5, §1.8, §1.10 |
| 21 | Pass modes lose typed characters | **Accepted.** A pass mode refuses a sequence that starts with a key that types; step 7 tests it. | §2.5, step 7 |

## Changelog

- **First draft** (2 October 2026): `settings`, `keymap` and `chrome` in `sighurt-api`, and
  `EXTENSION_API = 1`. Superseded.
- **Second draft** (2 October 2026, this document):
  - the chrome API moved to `sighurt_ui::extension`;
  - `ChromeSurface::input` and `InputArea` added;
  - `ChromeKind::commands`, `ChromeEvent::Command` and `Command::Chrome` added;
  - `KeymapKind::check` takes a `CommandSet`;
  - no version constant and no promise;
  - links hand over through `NavigationRequested` (engine contract), not `HandOff`.
- **Implementation** (2 October 2026, STATUS decisions 67, 68 and 74–77, and the review fixes of
  decision 82). Where the code departs from the second draft, and what an implementation needs
  to know:
  - `Value::as_f64` was left out (nothing reads a float), and so was `TabSnapshot::engine`
    (neither chrome shows it).
  - Added to the chrome API's surface: `readable_url`, `Snapshot::contain`, `Pen`, `TextStyle`,
    `ShapedText` and `state::{is_home, NO_SUCH_PAGE}`.
  - `NavigationRequested` is narrower: an engine holds at most 16 per view and lets the page's
    navigations beyond them go ahead, and an engine that knows which navigations the user
    started may ask about those alone (WPE: a link or a GET form the user clicked).
  - The core hands a page's navigation to another engine only for the link the user pressed
    (the page's status text at the press), never while the tab loads an address the user asked
    for; a key press or a form no longer moves the tab (decision 82, amending 77).
  - The rules of an engine the build lacks are left out, so a typed address goes where a link
    does; `[engines] default` must name an engine of the build (decision 82).
  - An ephemeral Wasm engine keeps no module cache at all, not a temporary one (decision 76).
  - The modal keymap's `always` table applies its keys that type nothing while the chrome has
    the keyboard, Enter and Escape included if it binds them (decision 74 (5)).
  - Guard tests: `crates/sighurt-api/tests/keymap.rs` (a keymap with a two-key sequence, from outside
    the crate) and `ui/sighurt-ui/tests/extension.rs` (the standard chrome through the trait, and a
    chrome of its own) pass with these changes.
- **Engine labels** (2 October 2026, STATUS decision 87): `TabSnapshot::engine` is added after
  all, now that both chromes show it: the configured name of the engine a tab's page runs in,
  `None` for `sig:` pages, blank tabs and errors, contained to 100 characters like an engine
  row's name. Also added: `TabSnapshot::engine_label` (the name, for an engine page only),
  `Fonts::baseline` (to put text of two sizes on one baseline) and `LayoutOptions::engine_label`.
  A chrome that builds `TabSnapshot` values itself must name the new field.
- **The configuration extension** (31 October 2026, STATUS decision 116; design in
  `docs/rewrite/config-extension.md`): `sighurt_api::keymap` gains `Browser` (a borrowed,
  read-only view of the frontend, the key context and the tabs), `TabInfo`, `Frontend` (moved from
  the core, which re-exports it) and `KeyAction::RunAll`; `Keymap::press` takes `&dyn Browser`
  as its third argument. `sighurt-keymap-modal` is replaced by `sighurt-config-example` (config
  name `example`, always compiled in, the built-in `[input] keymap`): the `keymap-modal` feature
  and `[keymap.modal]`'s TOML modes are gone (§2.5 and the examples above describe the old
  crate), and the example's bindings are Rust. `[tui.keys]`' built-in table is gone. New command
  `clipboard.copy <text>`. The chrome API is unchanged.
- **The render framework, lane 1** (3 November 2026; design in `docs/rewrite/RENDER.md`, §14
  amending its body): the painting half of the chrome API is replaced by drawing scenes.
  `Chrome::paint(surface, &Metrics, PixelBuffer) -> Result<(), PaintError>` becomes
  `Chrome::draw(surface, &mut Pen)`; `ChromeKind::new` takes the shared `Fonts`
  (`fn(Fonts) -> Box<dyn Chrome>`), so a chrome no longer loads fonts. `Canvas`, `CpuPainter`,
  `PixelBuffer` and `PaintError` leave the chrome API (`PixelBuffer` is sighurt-render's
  backend contract now). The `Pen` (sighurt-render's, re-exported) is widened: `fill_rounded`,
  `bordered`, `stroke` and `shape` are public, `stroke` and `shape` take sighurt-render's
  `PathBuilder` (tiny-skia's method names: `move_to`, `line_to`, `quad_to`, `cubic_to`,
  `close`, `push_circle`), `text` loses its `&mut Fonts` argument, `clear` makes its color
  opaque, and `rect()` is new. `Rgba`, `Fonts`, `ShapedText`, `TextStyle` and
  `FontSystemHandle` are sighurt-render's, re-exported from sighurt-ui at their old paths;
  `ShapedText::lines` is new; `paint_cpu` is `paint` (`paint::draw`). The standard chrome's and
  the line chrome's golden images are unchanged, pixel for pixel.
- **Tabs, find, history and fullscreen: the API** (4 November 2026, STATUS decisions 126–129).
  `Command` gains `TabDuplicate`, `TabMove`
  (with `commands::TabMove`: `Left`, `Right`, `To(n)`), `TabReopen`, `PageFind(Option<String>)`
  (no text opens the chrome's find field; text over `commands::MAX_FIND`, 1024 bytes, is
  refused with `CommandError::TooLong`), `PageFindNext`,
  `PageFindPrevious`, `PageFindStop`, `PageExitFullscreen`, `HistoryClear` and
  `BrowserFullscreen`; `TabNew` and
  `LocationFocus` take an optional argument (`tab.new [address]`, `location.focus [text]`), so a
  keymap or chrome that names them writes `Command::TabNew(None)` and
  `Command::LocationFocus(None)`. `CommandError::InvalidTabMove` and `TooLong` are new.
  `keymap::TabInfo` gains `link` (the link the pointer is over: an http, https or file URL
  as opening it would load it, at most 8 KiB, per document; never the raw status text) and
  `fullscreen` (the page holds an element fullscreen), and `TabInfo::new` takes them as its
  last two arguments. The engine contract gains
  `EngineCommand::Find(FindCommand)` (each `FindCommand` names its search, and `Search` its
  document, so late ones are dropped), `EngineCommand::ExitFullscreen`,
  `EngineEvent::FindResult` (replaceable per view), `EngineEvent::FullscreenChanged` (ordered)
  and `EngineCapabilities::fullscreen`; no engine sets `find_in_page` or `fullscreen` yet, and
  every adapter refuses both commands as unsupported. The chrome API gains
  `ChromeEvent::LineText(String)`: right after `Focus(Chrome)`, `location.focus <text>` puts
  the text in the line as if typed over the selected address, the caret at its end (the core's
  `Effect::LineText`, the runtime's `Frontend::line_text`). The example configuration binds `:`
  to `location.focus :` instead of `RunThenPass(location.focus)`, `/` to `page.find`, `u` to
  `tab.reopen`, `y f` to copying `TabInfo::link`, and Escape to `page.exit-fullscreen` while
  `TabInfo::fullscreen` holds (its mode switch otherwise).
