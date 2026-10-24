# Sighurt

Sighurt (`sig`) is a small, configurable browser for Linux. It is one binary with two frontends:
a window on Wayland, and the terminal it is started in, where it shows pages as kitty graphics or
as reflowed text. Its content engines are compiled into `sig` and run in its process (there is
no IPC); the browser chrome around them is drawn separately, on the CPU. Three engines come with
it:

- **Servo** renders the web: HTML, CSS and JavaScript with [Servo](https://servo.org) 0.5. It is
  the default engine, and the long-term one.
- **WPE WebKit** ([wpewebkit.org](https://wpewebkit.org)) is there for compatibility in the short
  term: it opens only the sites your configuration gives it and the pages you open in it yourself.
- **The Wasm engine** runs `.wasm` apps (Rust compiled to `wasm32-unknown-unknown`) under
  Wasmtime, with no WASI. It speaks Oxide's app ABI, so Oxide apps run unchanged.

A fourth, the demo engine, exists to test the host and the chrome without a real engine.

```text
 sig (sighurt-browser): arguments, process-wide setup, the choice of frontend
  ├── sighurt-gui       the Wayland frontend: the event loop, the chrome's surfaces
  │    ├── sighurt-ui        the chrome: tabs, address and command line, drawn on the CPU
  │    └── sighurt-wayland   the window and its surfaces, input, scaling, frame scheduling
  ├── sighurt-tui       the terminal frontend: terminal I/O, the chrome as text, pages as
  │                     kitty graphics or reflowed text, `sig --dump`
  ├── sighurt-runtime   configuration and session files, the engine registry, effects, services
  ├── sighurt-core      tabs, commands, keymaps, routing, history across engines, permissions
  └── engines           servo · wpe · wasm · demo, each one a library behind sighurt-api

 In the GUI, one Wayland window: the chrome's surfaces and one page surface per engine view, which the
 desktop compositor puts together. Servo presents with OpenGL straight into its page surface,
 WPE WebKit attaches its DMA-BUFs to it, the Wasm engine paints into shared memory. No page is
 read back to the CPU to be shown (WPE copies once a frame only where the compositor has no
 DMA-BUF support, and says so). In the terminal the engines paint into CPU frames instead,
 which the frontend encodes as kitty graphics, or it shows the page's text.
```

The core is ordinary Rust (`update(state, event) -> effects`) with no I/O; engines report facts as
events and never change browser state. Keys, the chrome, the engines' routing and settings are
all configuration, and the keymap and the chrome are small traits you can implement yourself
([below](#writing-your-own-keymap-or-chrome)).

This is the `rewrite` branch. The design is
[`sighurt-rewrite-plan.md`](./sighurt-rewrite-plan.md); what has been built, tested and decided
is in [`docs/rewrite/STATUS.md`](./docs/rewrite/STATUS.md). The browser before the rewrite (GPUI
chrome, engines as separate processes) is at the git tag `gpui-baseline`.

## Build and run

You need Linux, Rust 1.98 or newer, and the
native libraries Servo, WPE WebKit and the Wasm engine link. On NixOS, `nix-shell` (see
[`shell.nix`](./shell.nix)) provides the libraries, from the nixpkgs revision pinned in
[`nix/nixpkgs.nix`](./nix/nixpkgs.nix); it brings no Rust. The first build compiles Servo, which
takes a long time and a lot of memory. The window needs a Wayland compositor (there is no X11
support); the terminal frontend needs none ([below](#in-the-terminal)).

```bash
nix-shell                                        # NixOS
cargo build -p sighurt-browser                   # sig with Servo, WPE WebKit, the Wasm engine, both frontends
target/debug/sig https://example.com/            # in a terminal: in it; otherwise: a window
target/debug/sig --help
```

```text
Usage: sig [OPTIONS] [URL|PATH ...]

Opens each URL or file in its own tab, or the home page when none is given. In a terminal
(stdout is one and TERM isn't dumb) sig runs in it, showing pages as kitty graphics; otherwise
it opens a Wayland window.

Options:
      --default-config     Print the built-in configuration and exit
      --dump URL           Print URL's page as reflowed text with numbered links on stdout and
                           exit, with no terminal or window (needs an engine that gives page
                           text: WPE WebKit); exit code 2 on a timeout
      --width N            With --dump, wrap to N columns (default 80)
      --timeout SECS       With --dump, give up after SECS seconds (default 30)
      --diagnostics FILE   Write the diagnostics to FILE as JSON at exit and on SIGUSR1
      --engine ENGINE      The engine this run starts with, overriding [engines] default
      --gui                Open a Wayland window, even from a terminal
      --tui                Run in the terminal on stdout. Its log, and anything else written
                           to stderr, goes to $XDG_STATE_HOME/sighurt/tui.log
                           (~/.local/state/sighurt/tui.log); the directory is created
  -h, --help               Print this help and exit
  -V, --version            Print the version and exit

Environment:
  SIG_DIAG=1      Print a diagnostics line for every page frame and chrome paint on stderr
  SIG_AUDIT_GL=1  Count Servo's GL reads of page pixels too (times every GL call; for tests)
  RUST_LOG        Log filter, such as RUST_LOG=debug

Signals: SIGUSR1 prints the diagnostics on stderr; SIGTERM and SIGINT quit, saving the tabs.
```

An argument that names an existing file becomes a `file:` URL; anything else is read as the
address line reads it: `example.com` opens `https://example.com/`, and text that isn't an address
is a search. `sig` restores the tabs it had when it last quit, beside the ones you give it.

### Choosing what is compiled in

Cargo features of `sighurt-browser` decide which engines, keymaps and chromes `sig` has; the
configuration decides which of them are used.

| Feature | Default | What it adds |
|---|---|---|
| `frontend-gui` | yes | the Wayland window (`--gui`) |
| `frontend-tui` | yes | the terminal frontend (`--tui`, `--dump`) |
| `engine-servo` | yes | Servo, the engine `servo` |
| `engine-wasm` | yes | the Wasm engine, `wasm` |
| `engine-wpe` | yes | WPE WebKit, `wpe` (needs the WPE prefix below) |
| `engine-demo` | no | the demo engine, `demo` |
| `keymap-modal` | no | the modal keymap, `[input] keymap = "modal"` |
| `chrome-line` | no | the line chrome, `[ui] chrome = "line"` |

```bash
cargo build -p sighurt-browser --features keymap-modal,chrome-line        # everything
cargo build -p sighurt-browser --no-default-features --features engine-servo,engine-wasm   # no WPE
cargo build -p sighurt-browser --no-default-features --features engine-demo                # no Servo
```

The `bindings` keymap and the `standard` chrome are in every build. A build without
`frontend-tui` has no terminal frontend: `--tui` and `--dump` are errors there.

### In the terminal

`sig` runs in the terminal it is started in when stdout is a terminal and `TERM` isn't `dumb`,
and opens a window otherwise; `--gui` and `--tui` override that, and `--tui` without a terminal
is an error. With no `WAYLAND_DISPLAY` and no terminal, `sig` suggests `--tui`. The terminal
shows the browser's chrome as two lines of text (the tab's place, address, engine and mode on
top; status and notices below) and the page between them in one of two modes:

- **Graphics**: the page's pixels as kitty graphics, up to `[tui] fps` frames a second (10 by
  default), in a terminal that speaks the kitty graphics protocol (kitty, Rio, ghostty,
  WezTerm), or as sixel, at most 5 a second, where the terminal has sixel but not kitty
  graphics: inside tmux 3.4 or later, foot, xterm with sixel.
- **Text**: the page's content reflowed into terminal lines, with links and controls you can
  move to and activate. It works in any terminal, and is the mode `auto` picks where the
  terminal can't show images. It needs an engine that gives the page's text: Servo and WPE
  WebKit do (and the demo engine); the Wasm engine shows a message saying so. `m` switches
  mode.

`[tui] mode = "auto"` (the default), `"text"` or `"graphics"` chooses the start; the `[tui]`
table of the [built-in configuration](crates/sighurt-core/src/default-config.toml) also has
`graphics` (`auto`, `kitty`, `sixel`, `none`), `fps`, `mouse` and `default-keys`. The mouse
clicks and scrolls the page (in text mode the wheel scrolls three lines and a click activates
the link or control under it). Logs, and anything else written to stderr, go to
`$XDG_STATE_HOME/sighurt/tui.log` (`~/.local/state/sighurt/tui.log`). The terminal is restored
on every way out.

A terminal has no key bindings of its own to start from, so it ships a small table, `[tui.keys]`,
under your `[keys]` (a binding of yours for the same keystroke wins; `default-keys = false` drops
the whole table):

| Key | Command |
|---|---|
| `q`, `ctrl-c` | `browser.quit` |
| `o`, `:`, `ctrl-o` | `location.focus` (the address line; `:` then a command runs it) |
| `t`, `x` | `tab.new`, `tab.close` |
| `J`, `K` | `tab.next`, `tab.previous` |
| `alt-1` to `alt-9` | `tab.select 1` to `9` |
| `H`, `L` | `page.back`, `page.forward` |
| `r`, `ctrl-r` | `page.reload` |
| `e` | `page.switch-engine` |
| `j`, `k` | scroll down, up (`page.send-key ArrowDown`, `ArrowUp`) |
| `ctrl-d`, `ctrl-u` | page down, up |
| `g`, `G` | top, bottom of the page |
| `+`, `-`, `=` | `page.zoom-in`, `page.zoom-out`, `page.zoom-reset` |
| `m` | `chrome.toggle-mode` (text or graphics) |
| `ctrl-l` | `chrome.refresh` (draw the page again) |

In text mode `Tab` and `Shift-Tab` move between links and controls and `Enter` activates one
(a text field opens a one-line editor, a list a chooser; `Escape` cancels). `Enter` in the
editor sets the field, and submits its form where a browser would (a form with a button, or
with that one field); otherwise activate the form's button. The terminal's other
chrome commands are `chrome.text-mode` and `chrome.graphics-mode`. Terminals that speak the kitty
keyboard protocol send every key faithfully; in others, `code:` (physical key) bindings never
match.

`sig --dump URL` prints a page as text and exits, with no terminal or window: the reflowed
page, then its links numbered. `--width N` wraps at N columns, `--timeout SECS` gives up (exit
code 2); a page that fails to load exits 1. The engine is `--engine`, else the one the routing
names (Servo by default), as in the terminal.

```bash
target/debug/sig --dump --width 100 https://example.com/ | less
```

### WPE WebKit

WPE WebKit 2.52 is built from source into a prefix of its own, which `shell.nix` finds when it
exists ([`docs/rewrite/WPE.md`](./docs/rewrite/WPE.md) has the details). Without it, building
`sig` with its default features stops with a message that says what to do:

```bash
nix/build-wpewebkit.sh                  # build it (about 1.5 hours), or
nix/build-wpewebkit.sh --pull HOST      # copy it from a machine that has it
```

Run `sig` inside `nix-shell`: it sets the library path and WebKit's `GIO_EXTRA_MODULES` (TLS for
`https:`) and `GST_PLUGIN_SYSTEM_PATH_1_0` (media); `sig` warns when the last two are missing.

### Wasm apps

```bash
cargo build --target wasm32-unknown-unknown --release -p hello-sighurt
target/debug/sig target/wasm32-unknown-unknown/release/hello_sighurt.wasm
```

Every crate in [`examples/`](./examples/) except `fullstack-notes/backend` is a Wasm app;
[`index`](./examples/index/) links to the others. A `.wasm` file opens in the Wasm engine, and so
does a `.wasm` URL on a web server you name in `[routing.wasm] origins`. [DOCS.md](./DOCS.md) is
the guide to writing them.

## Configuration

Everything is configured in one TOML file, `~/.config/sighurt/config.toml` (`$XDG_CONFIG_HOME`);
there is no settings screen. The built-in configuration,
[`crates/sighurt-core/src/default-config.toml`](crates/sighurt-core/src/default-config.toml), documents every
setting; `sig --default-config` prints it. Your file starts with `version = 1`, and every other
key is optional.

- Your file goes over the built-in one: `[routing.*]` tables merge field by field, `[engines]`,
  `[input]`, `[input.buttons]`, `[downloads]` and `[session]` key by key, and `[keywords]`,
  `[engines.*]`, `[keys]`, `[keymap.*]`, `[ui]` and `[chrome.*]` are yours alone.
- The whole file is checked at once and every problem is reported together. A file with a
  problem is not used: at startup `sig` runs with the built-in configuration, and a file saved
  while `sig` runs leaves the configuration in use as it was. The status line says so.
- A table for an engine, keymap or chrome that this build doesn't have is not a problem: `sig`
  says so and goes on, so one file serves several builds. The routing rules of an engine the
  build lacks are left out, so what they claim opens in the default engine. `[engines] default`
  must name an engine the build has.
- `sig` reads the file again whenever it is saved. The keymap and its bindings, the mouse's
  buttons, scrolling, colors, fonts and the chrome's options, the home page, search, keywords,
  zoom, routing and the download folder apply at once; pages already open stay in their engine.
  `[ui] chrome` and the engines' own settings apply when `sig` starts again.

```toml
version = 1
search = "https://duckduckgo.com/html/?q={}"
zoom = 1.25

[keywords]
w = "https://en.wikipedia.org/w/index.php?search={}"   # "w rust" searches Wikipedia

[routing.wpe]
hosts = ["*.example.com", "example.com"]               # these sites open in WPE WebKit

[engines.servo]
storage = "ephemeral"                                  # keep no cookies or site data

[keys]
"ctrl-t" = "tab.new"
"ctrl-w" = "tab.close"
"ctrl-l" = "location.focus"
"alt-left" = "page.back"
"ctrl-alt-w" = "page.open-in wpe"                      # this page, again in WPE WebKit

[ui]
font-size = 16

[ui.colors]
tab = "#7c3aed"

[chrome.standard]
tabs = "left"
```

### Reference

| Setting | Meaning | Default |
|---|---|---|
| `home` | the page of new tabs and of the start | `"sig://home"` |
| `search` | what the address line does with text that isn't an address; `{}` is the query | DuckDuckGo |
| `zoom` | the zoom of new tabs and of `page.zoom-reset`, 0.5 to 3 | `1.0` |
| `[keywords]` | `name = "template"`: the first word typed, if it is a name, picks the template; the rest goes where its `{}` is | none |
| `[engines] default` | the engine of the web, and of everything no rule below claims | `"servo"` |
| `[engines] enabled` | the engines that may open new pages | all in the build |
| `[engines.<engine>] user-agent` | the user agent the engine sends and its pages see (Servo, WPE) | the engine's own |
| `[engines.<engine>] storage` | `"profile"` keeps logins (persistent cookies), site data and caches across runs, in private (0700) directories; `"ephemeral"` keeps nothing (Servo, WPE, Wasm) | `"profile"` |
| `[routing.<engine>] schemes` | URL schemes the engine opens (not http, https, file or sig) | Servo: `data`, `about` |
| `[routing.<engine>] extensions` | file extensions it opens from `file:` URLs, and from `origins` | Wasm: `wasm` |
| `[routing.<engine>] origins` | web servers (`scheme://host[:port]`) whose files with those extensions it opens | none |
| `[routing.<engine>] hosts` | hosts it opens: `"docs.example.com"`, an IP address, or `"*.example.com"` (hosts under it, not itself) | none |
| `[input] keymap` | the keymap: `bindings` (reads `[keys]`), or one compiled in | `"bindings"` |
| `[input] scroll-speed` | how far a wheel notch or a swipe scrolls a page, 0.1 to 10 | `1.0` |
| `[input.buttons]` | `back`, `forward`: the mouse's side buttons, a command or `""` | `page.back`, `page.forward` |
| `[keys]`, `[keys.page]`, `[keys.location]` | the `bindings` keymap: everywhere, while the page has the keyboard, while the address line has it | none |
| `[keymap.<name>]` | another keymap's own table | — |
| `[ui] chrome` | the chrome that draws the window (at the next start) | `"standard"` |
| `[ui] font`, `font-size` | the chrome's font family, and its size in logical pixels (9 to 32) | an installed sans-serif, 14 |
| `[ui.colors]` | `bg`, `surface`, `hover`, `tab`, `border`, `focus`, `text`, `text_muted`, `text_dim`, `selection`, `error`: `"#rrggbb"` or `"#rrggbbaa"` | a dark theme |
| `[chrome.standard]` | `tabs` (`"top"`, `"left"`, `"hidden"`), `status` (`"strip"`, `"overlay"`), `buttons`, `compact`, `engine-label` (the engine a page runs in, in its tab and the address bar) | top, strip, true, false, true |
| `[chrome.line]` | `position` (`"top"`, `"bottom"`), `engine-label` (the page's engine at the line's end) | top, true |
| `[downloads] directory` | where downloads go: an absolute path or `~/…` | the desktop's download folder, else `~/Downloads` |
| `[session] restore` | open the tabs of the last run, and save them as they change | `true` |

The session is `~/.local/state/sighurt/session.toml`; Servo's profile is
`~/.local/share/sighurt/servo`, WPE WebKit's `~/.local/share/sighurt/wpe` and the Wasm apps' stores
`~/.local/share/sighurt/wasm`.

### Which engine opens a page

In this order:

1. `sig:` URLs are Sighurt's own pages (`sig://home`). Pages can't open them.
2. An engine that lists the URL's scheme in `schemes`.
3. A `file:` URL whose extension an engine lists in `extensions`.
4. An http(s) URL on one of an engine's `origins`, whose extension it lists.
5. An http(s) URL whose host an engine lists in `hosts`; the most specific pattern wins.
6. The default engine.

`page.open-in <engine>` overrides all of it for one page. There is no network probe: an engine
that gets content it can't show downloads it or says so.

`sig --engine <name>` is the engine this run starts with, overriding `[engines] default`: the
engine that opens http(s) and file URLs no routing rule claims. An engine the build lacks, or
one the configuration does not enable, is an error at startup that names the engines available.
The choice stays for the whole run, including when the configuration is read again. It is also
the engine `--dump` uses (without it, the one the routing names).

`page.switch-engine` reopens the page shown in the next web engine among those compiled in and
enabled, in the build's order, wrapping around. It chooses that engine the way `page.open-in`
does. With fewer than two such engines, or a page that isn't http, https or a file, it shows a
notice and the page stays. Clicking the engine's name at the right end of the address field, or
at the end of the line chrome, runs it. The terminal binds it to `e`; the GUI binds nothing
until you do.

Links follow the routing too. When you click a link to an address the routing gives another
engine, the tab opens it there and back returns to the page. Everything else a page does (a
script's redirect, a frame loading, a link opened with the keyboard, a form) stays in its engine,
even right after your click, and so do the links of a page you opened with `page.open-in`, until
you type an address. Only the address goes along. Neither Servo 0.5 nor WebKit says which frame a
link is in, so a click on a link inside a frame that goes elsewhere moves the whole tab.

### Commands

Everything the browser does is a named command. Buttons, keys and the command line (`:` and a
command, typed into the address line) all run them.

| Command | What it does |
|---|---|
| `tab.new` | a new tab on the home page |
| `tab.close [n]` | close the active tab, or tab `n` (tabs count from 1) |
| `tab.next`, `tab.previous`, `tab.last`, `tab.select <n>` | switch tabs |
| `page.open <url>` | open an address or a search in the active tab |
| `page.open-in <engine> [url]` | open the address, or the page shown, in the engine named, whatever the routing says |
| `page.switch-engine` | reopen the page shown in the next web engine |
| `page.reload`, `page.stop`, `page.back`, `page.forward` | the page's own |
| `page.zoom-in`, `page.zoom-out`, `page.zoom-reset` | page zoom |
| `page.send-key <key>` | press and release a named key in the page, as if typed: `down`, `pageup`, `end`, `space`, `enter`, `f5`, `shift-space`, … |
| `location.focus` | focus the address line with its text selected |
| `browser.home` | open the home page in the active tab |
| `browser.quit` | quit, saving the tabs |
| `chrome.<action>` | one of the chrome's own commands, no argument: the standard chrome's is `chrome.toggle-tabs`; the terminal's are `chrome.toggle-mode`, `chrome.text-mode`, `chrome.graphics-mode` and `chrome.refresh` |

### Keys

The window has no default key bindings: nothing is bound until you bind it (the terminal's small
default table is [above](#in-the-terminal)). The tab strip, the toolbar
and the address line work without any. With the `bindings` keymap:

```toml
[keys]                        # everywhere
"ctrl-t" = "tab.new"
"ctrl-tab" = "tab.next"
"ctrl-code:KeyQ" = "browser.quit"   # the key where Q is on a US keyboard

[keys.page]                   # while the page has the keyboard
"f5" = "page.reload"
"alt-j" = "page.send-key down"

[keys.location]               # while the address line has it
"alt-enter" = "page.open https://example.com"
```

A keystroke is modifiers (`ctrl`, `alt`, `shift`, `super`, `altgr`) and a key, joined by `-`. The
key is a character as your layout types it (`"ctrl-+"`, `"ctrl-å"`), a key name (`enter`,
`escape`, `tab`, `space`, `left`, `pageup`, `f1` to `f24`, or a W3C name such as `BrowserBack`),
or `code:` and a W3C key code for a physical key. AltGr is never Ctrl+Alt. While you type into the
address line or a page's text field, a keystroke that types a character types it instead of
running its command. A key the browser takes never reaches the page, release included.

The modal keymap (feature `keymap-modal`) has modes and key sequences, vim-style:

```toml
[input]
keymap = "modal"

[keymap.modal]
start = "normal"        # the mode it starts in
typing = "insert"       # the mode a text field of the page puts it in
pass = ["insert"]       # modes in which unbound keys reach the page

[keymap.modal.always]   # in every mode, and the only bindings while the address line is focused
"ctrl-q" = "browser.quit"

[keymap.modal.normal]
"j" = "page.send-key down"
"g g" = "page.send-key home"
"g t" = "tab.next"
"o" = "location.focus"
":" = { run = "location.focus", pass = true }   # opens the command line with ":"
"i" = "mode insert"

[keymap.modal.insert]
"escape" = "mode normal"
```

The chrome shows the mode and an unfinished sequence ("NORMAL g"). Sequences never wait for a
timeout: no binding may be the start of another.

### Chromes

The `standard` chrome is a tab strip, a toolbar with back, forward, reload and the address line,
and a status strip; `[chrome.standard]` moves the tabs to a list on the left or hides them, shows
the status over the page only while there is something to say, drops the buttons or makes it
compact. The line chrome (feature `chrome-line`, `[ui] chrome = "line"`) is one line at the top or
the bottom of the window: the tab's place (`[2/5]`), the keymap's mode, the address and the status;
tabs are reached by keys and commands. Both name the engine a page runs in (`servo`, `wpe`,
`wasm`), small and muted: the standard chrome in each tab and at the address bar's right end, the
line chrome at the line's end; `sig:` pages have none, and `engine-label = false` turns it off.
Every chrome shows what pages say (titles, status text, addresses) contained to one line, and
`sig`'s own notices whenever there are any.

## Writing your own keymap or chrome

A keymap and a chrome are each one small trait, and the defaults go through them too. Yours is a
crate compiled into `sig`: there is no plugin loading. The API has no stability promise yet;
[`docs/rewrite/extension-api.md`](./docs/rewrite/extension-api.md) is its design, with a changelog
of breaking changes.

**A keymap** implements `sighurt_api::keymap::Keymap` and depends on `sighurt-api` alone. The core
asks it about each fresh key press, with the context (does the page or the chrome have the
keyboard, does it take typed text), and it answers `Pass`, `Take`, `Run(command)` or
`RunThenPass(command)`. The core does the rest: a taken press takes its release and repeats, a
held key never turns into a shortcut halfway, and the keymap is told when the context changes or
the window loses the keyboard. A keymap's table, `[keymap.<name>]`, is checked by its kind when the
configuration loads, so a wrong file is refused with your messages. A keymap that runs one command
for one key:

```rust
use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::input::KeyEvent;
use sighurt_api::keymap::{Chord, KeyAction, KeyContext, Keymap, KeymapKind};
use sighurt_api::settings::{Table, Value};

/// `[keymap.one]`: `key = "ctrl-n"` runs `run = "tab.next"`; every other key passes.
#[derive(Default)]
struct One(Option<(Chord, Command)>);

fn read(settings: &Table, commands: &CommandSet<'_>) -> Result<(Chord, Command), String> {
    let text = |name| settings.get(name).and_then(Value::as_str).ok_or(format!("{name}: missing"));
    let key = text("key")?.parse().map_err(|e| format!("key: {e}"))?;
    let run = commands.parse(text("run")?).map_err(|e| format!("run: {e}"))?;
    Ok((key, run))
}

impl Keymap for One {
    fn configure(&mut self, settings: &Table) {
        self.0 = read(settings, &CommandSet::ANY).ok();
    }

    fn press(&mut self, key: &KeyEvent, _context: KeyContext) -> KeyAction {
        match &self.0 {
            Some((chord, run)) if chord.matches(key) => KeyAction::Run(run.clone()),
            _ => KeyAction::Pass,
        }
    }
}

pub const KIND: KeymapKind = KeymapKind {
    name: "one",
    check: |settings, commands| read(settings, commands).err().into_iter().collect(),
    new: || Box::<One>::default(),
};
```

**A chrome** implements `sighurt_ui::extension::Chrome`: it handles events (its configuration, the
window's size, a snapshot of the browser's state whenever something it could show changes, focus,
pointer input on its surfaces, keys the keymap passed, pasted text, its own commands), returns
actions (run a command, select or close a tab by its id, move the keyboard, copy, paste, repaint a
surface), lays out up to 8 named surfaces with their input areas and the page's rectangle, and
paints a surface into a buffer the host lends. Its kind checks `[chrome.<name>]` and names its
`chrome.<action>` commands. sighurt-ui's text, editing and painting helpers (`Fonts`, `TextEditor`,
`Canvas`) are there to use.

To add either to `sig`: a feature in `apps/sighurt-browser/Cargo.toml` and the kind in the list in
`crates/sighurt-runtime/src/registry.rs` (`KEYMAPS`) or `frontends/sighurt-gui/src/chrome.rs` (`CHROMES`):

```toml
[features]
keymap-one = ["dep:sighurt-keymap-one"]

[dependencies]
sighurt-keymap-one = { path = "../sighurt-keymap-one", optional = true }
```

```rust
pub const KEYMAPS: &[KeymapKind] = &[
    #[cfg(feature = "keymap-modal")]
    sighurt_keymap_modal::KIND,
    #[cfg(feature = "keymap-one")]
    sighurt_keymap_one::KIND,
];
```

Then `[input] keymap = "one"` with `[keymap.one] key = "ctrl-n"` and `run = "tab.next"`. The
worked examples are [`sighurt-keymap-modal`](extensions/sighurt-keymap-modal/) and
[`sighurt-chrome-line`](extensions/sighurt-chrome-line/); `crates/sighurt-api/tests/keymap.rs` and
`ui/sighurt-ui/tests/extension.rs` implement both traits from outside their crates.

## Crates

| Crate | What it is |
|---|---|
| [`sighurt-browser`](apps/sighurt-browser/) | `sig`: arguments, process-wide setup, the choice of frontend |
| [`sighurt-runtime`](crates/sighurt-runtime/) | what every frontend shares: configuration and session files, the engine registry, carrying out the core's effects, downloads, diagnostics, shutdown |
| [`sighurt-gui`](frontends/sighurt-gui/) | the Wayland frontend: the event loop, the chrome's surfaces, engines bound to Wayland targets, the clipboard |
| [`sighurt-tui`](frontends/sighurt-tui/) | the terminal frontend: terminal I/O, the cell grid, the chrome as text, pages as kitty graphics or reflowed text, `sig --dump` |
| [`sighurt-semantic`](engines/sighurt-semantic/) | the DOM extraction script and bounded parser that give text mode a page's content (used by the Servo and WPE adapters) |
| [`sighurt-core`](crates/sighurt-core/) | browser state and policy: tabs, commands, the `bindings` keymap, routing, history across engines, permissions, configuration; no I/O |
| [`sighurt-api`](crates/sighurt-api/) | the contract between them: ids, geometry, input, the engine trait and its events, services, commands, the keymap API |
| [`sighurt-wayland`](frontends/sighurt-wayland/) | the Wayland host: window, surfaces, targets, scaling, input, clipboard, shared-memory buffers, frame scheduling |
| [`sighurt-ui`](ui/sighurt-ui/) | the standard chrome, the chrome API and its helpers |
| [`sighurt-chrome-line`](extensions/sighurt-chrome-line/) | the line chrome |
| [`sighurt-keymap-modal`](extensions/sighurt-keymap-modal/) | the modal keymap |
| [`sighurt-engine-servo`](engines/sighurt-engine-servo/) | the Servo engine |
| [`sighurt-engine-wpe`](engines/sighurt-engine-wpe/) | the WPE WebKit engine |
| [`sighurt-engine-wasm`](engines/sighurt-engine-wasm/) | the Wasm engine |
| [`sighurt-engine-demo`](engines/sighurt-engine-demo/) | the demo engine |
| [`sighurt-sdk`](sdk/sighurt-sdk/) | the guest SDK for Wasm apps |
| [`examples/`](./examples/) | Wasm apps, and the native backend of `fullstack-notes` |
| [`experiments/`](./experiments/) | the proofs the rewrite was built on (Servo and Wasmtime in one process, mixed rendering, WPE beside both) |

## Writing Wasm apps

A Wasm app is a Rust `cdylib` for `wasm32-unknown-unknown` that depends on
[`sighurt-sdk`](sdk/sighurt-sdk/) by path (it is not on crates.io). It draws its whole UI on a
canvas and polls input; there are no widgets.

```rust
use sighurt_sdk::*;

#[no_mangle]
pub extern "C" fn start_app() {
    log("Hello from Sighurt!");
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    canvas_clear(30, 30, 46, 255);
    canvas_text(20.0, 30.0, 24.0, 255, 255, 255, 255, "My app");
}
```

Host functions live in the Wasm import module `"oxide"`, Oxide's app ABI, so apps built for Oxide
(including with the [`oxide-sdk`](https://crates.io/crates/oxide-sdk) crate) run unchanged unless
they use Oxide's widget kit. The guide and the API overview are in [DOCS.md](./DOCS.md).

## Security

The engines run in `sig`'s process. That is a design for correct code, not a sandbox: an engine
that is exploited, or crashes, takes the browser with it ([SECURITY.md](./SECURITY.md) says what
holds and what doesn't). What pages say (titles, status text, addresses) is bounded and shown
contained; pages can't open `sig:` pages, can't write the address line, and open a tab or move
the tab to another engine only right after a click or key press of yours. Wasm apps get no WASI
and only the host functions Sighurt registers, with camera, microphone, location and screen
capture behind a prompt. Servo and WebKit pages get their engines' web security; WebKit's pages
run in its own sandboxed web processes. To report a vulnerability, see
[SECURITY.md](./SECURITY.md).

Logins stay across runs: each engine keeps its own cookies in its profile, which only you can
read (0700 directories, 0600 files). Only persistent cookies are kept; session cookies end with
`sig`, and HTTP passwords are never written. Servo saves its cookies within 5 s of a change, so
a crash loses at most that; WebKit saves them as they change and refuses third-party cookies.
Servo 0.7 doesn't enforce SameSite yet. Per-site controls (`site.forget` and a strict mode that
keeps only the sites you name) are planned ([site-data.md](./docs/rewrite/site-data.md)).

## Development

[CONTRIBUTING.md](./CONTRIBUTING.md) covers the checks, the end-to-end tests and how to add an
engine, a command or a host function. [ROADMAP.md](./ROADMAP.md) lists what is next.

## Credits and license

Sighurt started as a fork of [Oxide](https://github.com/niklabh/oxide) by Nikhil Ranjan.

Licensed under the GNU Affero General Public License, version 3 or later (`AGPL-3.0-or-later`);
see [LICENSE](./LICENSE) and [NOTICE](./NOTICE). Oxide is Apache-2.0: its license notice and the
Apache License are in [LICENSE-APACHE](./LICENSE-APACHE), and the code that came from it remains
available under Apache-2.0 from Oxide. The guest SDK (`sighurt-sdk`) and the example apps stay
Apache-2.0, so Wasm apps built with them may use any license. `sighurt-engine-servo` is
`AGPL-3.0-or-later AND MPL-2.0`; `sig` built with Servo links Servo, which is MPL-2.0, and `sig`
built with WPE WebKit loads WPE WebKit's libraries (LGPL and BSD licenses) dynamically.
