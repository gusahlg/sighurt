# The configuration extension: keys, state and the mode indicator in both frontends; copy and paste

**Status:** proposal, 31 October 2026, against `rewrite` at 1cb65d8 (0.9.0). Nothing here is
compiled; §8 lists what is unverified.
**Asked for:** copy and paste; one binding table for the terminal and the window; the terminal's
keys in the window; Escape toggling an insert mode that disables every other binding and shows
"insert mode" in the UI; a configuration layer in Rust between the browser and the user, with an
API that reads and changes browser state and defines its own bindings and actions; the keymap
crate renamed to an example configuration.
**Read with:** CLAUDE.md (the engine contract, the crate rules), extension-api.md (§2.3–§2.5 the
keymap API and the modal proof, §2.6 the chrome API), TUI.md §3–§4, and STATUS decisions 67, 68,
74, 91, 99, 104, 108 and 111.

## Decisions at a glance

1. **The user's idea is sound, with one correction of where it lives.** A frontend-neutral layer
   that receives keys, reads browser state, runs commands and tells the UI what to show already
   has a slot: the core's keymap (`sighurt_api::keymap::Keymap`), which both the window and the
   terminal run through `sighurt-core`, and whose `indicator()` both chromes already draw
   (`Snapshot::keymap` in sighurt-ui, `ChromeState::keymap_indicator` in sighurt-tui). It is
   *not* the UI code: `sighurt-ui` is the window's chrome and the terminal may not depend on it.
   So the "universal configuration" is the keymap trait, broadened, not a new kind of extension.
2. **The trait keeps its name and its TOML vocabulary.** `Keymap`, `KeymapKind`, `[input]
   keymap` and `[keymap.<name>]` stay (version 1, no key changes meaning). It gains a read-only
   view of the browser (`sighurt_api::keymap::Browser`, with `TabInfo`), the ability to run
   several commands from one key (`KeyAction::RunAll`), and knowledge of which frontend runs
   (`sighurt_api::keymap::Frontend`, moved from the core). The chrome API (`sighurt_ui::extension`)
   does not change.
3. **`sighurt-keymap-modal` becomes `sighurt-config-example`** (config name `example`), always
   compiled in like `bindings` and `standard`, and the default `[input] keymap`. Its bindings are
   Rust, in one table: the terminal's current keys, for both frontends; Escape toggling NORMAL and
   INSERT; two actions the configuration defines itself against the state API (`y y` copies the
   address, `c o` closes the other tabs). Modes and key sequences are the modal crate's code,
   kept; its TOML-driven modes go.
4. **The built-in `[tui.keys]` table and `[tui] default-keys` lose their job** to the example
   configuration: the table is deleted from the built-in file; `default-keys` is still accepted
   and does nothing (a notice if a user sets it). A user's `[keys]` and `[tui.keys]` keep their
   meaning for the `bindings` keymap, which binds nothing by default in either frontend now.
5. **Copy and paste** means: in every editor the editor's own keys (the page's: Servo's and
   WebKit's, reaching the browser's clipboard through the existing service path; the address
   line's: Ctrl+A/C/X/V, which the window's two chromes have and the terminal's line gets); the
   terminal gets a clipboard (OSC 52 writes everywhere, OSC 52 reads where the terminal answers,
   bracketed paste as today); and one new command, `clipboard.copy <text>`, so a configuration
   can copy what it reads from the state (`y y`). No `edit.*` commands and no new engine
   operation.
6. **Ctrl+C stays `browser.quit` in the terminal, but only while the page has the keyboard**;
   in the terminal's address line it copies. In the window Ctrl+C is unbound, so pages and the
   line copy with it. Quit is `q` in both.

---

# 1. What the configuration extension is

## 1.1 Why the keymap slot, and not the UI code

The requirements are: (a) receive key presses and decide what they do, in both frontends; (b)
read browser state and act on it; (c) show a mode in the UI of both frontends. Today:

- (a) is `Keymap::press`, called by `KeyState::handle` (crates/sighurt-core/src/keys.rs:309–350)
  from `BrowserState::keyboard` (state.rs:2096–2132) for every key of either frontend;
- (c) is `Keymap::indicator`, copied into the window's snapshot (frontends/sighurt-gui/src/chrome.rs:112)
  and the terminal's top line (frontends/sighurt-tui/src/app.rs:1325, chrome.rs:423–429), and
  redrawn only when it changes (state.rs:2327–2330);
- (b) is missing: `press` sees only `KeyContext { focus, typing }`.

Putting this into `sighurt-ui` would make it a window-only feature (the terminal never links
sighurt-ui, CLAUDE.md's crate table) and would split one decision (what a key does) between two
chromes. Putting it into the core's keymap slot gives both frontends the same behaviour for free
and keeps the core pure: the keymap runs inside `update`, reads no clock, does no I/O, and
returns commands the core carries out through the one command path.

## 1.2 What changes in the contract, in one paragraph

A keymap is now told, on every press, what the browser shows (`&dyn Browser`): the frontend, the
keyboard context, the tabs (id, title, address, loading, engine, zoom, back/forward) and which
is selected. It may answer with one command, several commands (`RunAll`), a taken key, a passed
key, or run-then-pass as before. Everything it returns goes through `BrowserState::run_command`
(crates/sighurt-core/src/commands.rs:66), so a configuration-defined action closes tabs, opens
addresses or copies text exactly as a chrome button or a `:command` would. Its indicator is the
only thing it shows, and both chromes already show it.

---

# 2. The API

## 2.1 `sighurt_api::keymap`: additions and one changed signature

```rust
// sighurt_api::keymap  (crates/sighurt-api/src/keymap.rs)

/// Which frontend runs the browser. Moved here from sighurt_core::state (the core re-exports
/// it as `sighurt_core::Frontend`, so nothing else changes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Frontend {
    #[default]
    Gui,
    Terminal,
}

/// What a keymap may read of the browser while it decides one key press. Borrowed from the
/// core for that call only: a keymap keeps nothing of it. Titles and addresses are page data
/// (plan §17): a keymap may compute with them (an address to open, text to copy) and must not
/// show them in its indicator or log them.
pub trait Browser {
    fn frontend(&self) -> Frontend;
    /// The same context `press` is given.
    fn context(&self) -> KeyContext;
    /// How many tabs there are.
    fn tab_count(&self) -> usize;
    /// The index of the selected tab, counting from 0 in tab-strip order (`tab.select` counts
    /// from 1).
    fn active_tab(&self) -> usize;
    /// Tab `index`, or `None` past the end.
    fn tab(&self, index: usize) -> Option<TabInfo<'_>>;
}

/// One tab, as a keymap sees it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabInfo<'a> {
    pub id: TabId,
    /// The page's own title, if it gave one (the chrome's derived title is the chrome's).
    pub title: Option<&'a str>,
    /// The committed address: what the tab shows, not what is loading.
    pub url: Option<&'a str>,
    pub loading: bool,
    /// The engine of the page it shows; `None` for `sig:` pages, blank tabs and errors.
    pub engine: Option<&'a str>,
    pub zoom: f64,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

pub enum KeyAction {
    Pass,
    Take,
    Run(Command),
    RunThenPass(Command),
    /// Taken, and these commands run in order in this update. Empty is `Take`.
    RunAll(Vec<Command>),               // NEW
}

pub trait Keymap {
    fn configure(&mut self, settings: &Table);
    /// CHANGED: the browser is passed. For a repeat, `Pass` counts as `Take`, `RunThenPass`
    /// as `Run`, and `RunAll` as itself.
    fn press(&mut self, key: &KeyEvent, context: KeyContext, browser: &dyn Browser) -> KeyAction;
    fn context(&mut self, _context: KeyContext) {}
    fn reset(&mut self) {}
    fn indicator(&self) -> Option<&str> { None }
}
// KeymapKind, Chord, is_modifier, KeyContext: unchanged.
```

Why a trait and not a struct: the core implements it over its own fields without copying a
string per tab per key press (§26: no routine copies), and sighurt-api names no core type.

Why `title` is the page's own: `TabSummary::title` is a `Cow` computed from the address when the
page has none (`display_title`, state.rs:770); a borrowed view can't hand that out, and no
keymap needs it.

## 2.2 `sighurt_api::commands`: one new command

```rust
/// `clipboard.copy <text>`: put `text` on the clipboard. The argument is kept as given
/// (trimmed, never empty).
Command::ClipboardCopy(String),
```

`NAMES` grows to 22; `name()` gives `clipboard.copy`; `Display` round-trips. A keymap makes it
from what it reads (`y y` copies the active address); typed after `:` it copies the rest of the
line. The core bounds nothing here (a keymap's text comes from a URL the core already bounded
to `MAX_URL`; the line is capped at 32 KiB); the runtime refuses more than its `MAX_CLIPBOARD`
with a notice.

## 2.3 The core's side

- **`sighurt_core::Frontend`** becomes `pub use sighurt_api::keymap::Frontend;` (lib.rs:41).
  `Environment::frontend` keeps its type.
- **The borrowed view.** `BrowserState::keyboard` builds a private `CoreBrowser<'a>` from
  disjoint fields (`&self.tabs`, `&self.views`, `self.active`, `self.focus`, `self.env.frontend`,
  the typing flag `key_context()` computes) and passes it to `self.keys.handle(..)`. Rust
  allows this only with direct field borrows, so the per-tab arithmetic of `summary` and
  `can_go` (state.rs:747–794: current view, committed URL and title, loading, engine,
  back/forward through `can_traverse` and the engine's capabilities) moves into free functions
  over `(&Tab, &BTreeMap<ViewId, View>, &Environment)` that both `summary` and `CoreBrowser::tab`
  call. No behaviour changes; `summary`'s tests stay.
- **`KeyOutcome::Run(Command)` becomes `Run(Vec<Command>)`** (keys.rs:254–263). `KeyState::handle`
  maps `Run(c)` to `Run(vec![c])`, `RunAll(cs)` to `Run(cs)` (an empty `RunAll` to `Consumed`),
  and on a repeat of a taken press `RunAll` runs again like `Run`. `keyboard()` runs the commands
  in order, calling `key_ran` for each so `page.send-key` in a sequence still counts as the
  user's activation (decision 74 (2)).
- **`Effect::ClipboardWrite(String)`**, returned by `run_command` for `ClipboardCopy`. The
  runtime carries it out (§5.2).
- **The built-in file's keymap may be absent from the build.** `KeyTables::check`
  (config.rs:1469–1538) treats a `[input] keymap` the build lacks as an error only when the
  *user's* file set it; the built-in file's value falls back to `bindings` with a log line. This
  mirrors decision 82 (3) for `[engines] default`, and it is what lets sighurt-core's tests
  (whose `Environment::default()` has no keymaps) and an assembly without the example crate
  start with the built-in file.
- **The `bindings` keymap** ignores the browser argument. Its layers (`combine`, keys.rs:56–72)
  lose the built-in layer: a user's `[tui.keys]` over `[keys]` in the terminal, `[keys]` alone
  in the window. `LAYER_BUILTIN` goes.

## 2.4 What the core guarantees, amended

Rules 1–6 of `sighurt_api::keymap`'s module doc stand. Two are added:

7. **The browser view is read-only and lasts one call.** It is built from the state before the
   press is applied and dropped after; a keymap that needs state later asks again on the next
   press. Nothing in it carries an `Instant`, a handle or an engine type.
8. **A `RunAll` runs whole, in order, in the update of its press.** Commands that change the tab
   list change the numbering the later ones see (`tab.close 3` renumbers tab 4), exactly as
   typing them one after another would. A keymap that closes several tabs by number closes the
   highest first.

Security: a keymap now sees titles and addresses of every tab, not only typed text. It is
trusted code compiled into `sig` (CLAUDE.md, Security); the rule for it is the one chromes have:
page text goes only where page text belongs. The executable still contains the indicator to 64
characters (`Snapshot::contain`, the terminal's `sanitize`), so a mistaken indicator can't break
a line, but the example never puts page text there.

---

# 3. The example configuration: `sighurt-config-example`

**Crate** `extensions/sighurt-config-example` (package `sighurt-config-example`, config name
`example`), replacing `extensions/sighurt-keymap-modal` in the workspace members. It depends on
`sighurt-api` only. It is a dependency of `sighurt-runtime` without a feature, listed first in
`KEYMAPS` (registry.rs:180–184); the `keymap-modal` features of sighurt-runtime, sighurt-gui,
sighurt-tui and sighurt-browser go. (When decision 99's step 2 moves the registrations into the
application, the example moves with them; nothing here depends on where the list is.)

**Modules:** `lib.rs` (`KIND`, the `Keymap` impl: modes, sequences, repeats, the indicator,
carried over from the modal crate), `table.rs` (the bindings), `actions.rs` (the two
configuration-defined actions).

## 3.1 The table

```rust
// extensions/sighurt-config-example/src/table.rs
pub enum Where { Both, Terminal, Window }
pub enum Mode { Normal, Insert }
/// Where a binding applies: `Page` while the page has the keyboard (the mode's own bindings);
/// `Always` in the page and, if its first key types nothing, in the address line too.
pub enum Scope { Page, Always }
pub enum Action {
    Run(&'static str),            // a command, parsed once at construction
    RunThenPass(&'static str),
    Mode(Mode),
    /// What the configuration computes from the browser: the commands to run, or none to take
    /// the key and do nothing.
    Custom(fn(&dyn Browser) -> Vec<Command>),
}
pub struct Binding { pub keys: &'static str, pub action: Action, pub scope: Scope, pub where_: Where }

pub const NORMAL: &[Binding] = &[
    // The terminal's table of 0.9.0, for both frontends.
    ("q",        Run("browser.quit"),             Page,   Both),
    ("o",        Run("location.focus"),           Page,   Both),
    (":",        RunThenPass("location.focus"),   Page,   Both),   // the line opens with ":"
    ("ctrl-o",   Run("location.focus"),           Always, Both),
    ("t",        Run("tab.new"),                  Page,   Both),
    ("x",        Run("tab.close"),                Page,   Both),
    ("J",        Run("tab.next"),                 Page,   Both),
    ("K",        Run("tab.previous"),             Page,   Both),
    ("H",        Run("page.back"),                Page,   Both),
    ("L",        Run("page.forward"),             Page,   Both),
    ("r",        Run("page.reload"),              Page,   Both),
    ("ctrl-r",   Run("page.reload"),              Always, Both),
    ("e",        Run("page.switch-engine"),       Page,   Both),
    ("j",        Run("page.send-key down"),       Page,   Both),
    ("k",        Run("page.send-key up"),         Page,   Both),
    ("ctrl-d",   Run("page.send-key pagedown"),   Page,   Both),
    ("ctrl-u",   Run("page.send-key pageup"),     Page,   Both),
    ("g",        Run("page.send-key home"),       Page,   Both),
    ("G",        Run("page.send-key end"),        Page,   Both),
    ("+",        Run("page.zoom-in"),             Page,   Both),
    ("-",        Run("page.zoom-out"),            Page,   Both),
    ("=",        Run("page.zoom-reset"),          Page,   Both),
    ("alt-1" ..= "alt-9", Run("tab.select 1..9"), Always, Both),
    // Terminal only: its chrome's commands, and Ctrl+C as the way out of a page field
    // (decision 111), but not in the address line, where it copies (§5).
    ("ctrl-c",   Run("browser.quit"),             Page,   Terminal),
    ("m",        Run("chrome.toggle-mode"),       Page,   Terminal),
    ("ctrl-l",   Run("chrome.refresh"),           Always, Terminal),
    // The mode switch.
    ("escape",   Mode(Insert),                    Page,   Both),
    // Defined by this configuration against the state API.
    ("y y",      Custom(copy_address),            Page,   Both),
    ("c o",      Custom(close_other_tabs),        Page,   Both),
];

pub const INSERT: &[Binding] = &[
    ("escape",   Mode(Normal),                    Page,   Both),
];
```

(Written as tuples here for brevity; the crate writes `Binding` literals. `alt-1`..`alt-9` are
nine entries.)

## 3.2 Behaviour

> **Amended by decision 134 (5 November 2026).** NORMAL now gives the page no key and no text,
> a field focused or not: the `typing` exemption below is gone, unbound keys are taken, and the
> arrows, PageUp/PageDown, Home/End, Tab/Shift+Tab, Space/Shift+Space and Enter are bound to
> `page.send-key` (Space and Enter taken while a page field has the keyboard). INSERT passes every
> key but an unmodified Escape, and only INSERT gives the page text (`Keymap::page_takes_text`).
> In the address line both modes apply NORMAL's `Always` bindings. The example crate's module
> doc and the README describe it as it is; the text below is the design as first proposed.

- **NORMAL.** The bindings above apply while the page has the keyboard. A binding whose first
  keystroke types a character (`Chord::types`) does not fire while `context.typing` is true (a
  page field or the line has the keyboard), so forms are typed into without changing mode; the
  others (`ctrl-`, `alt-`, Escape) fire. Unbound keys pass to the page, as `bindings` passes
  them (the terminal's behaviour today); nothing is swallowed. The indicator is `None`, or the
  keys of an unfinished sequence (`y`).
- **INSERT.** Only Escape is bound (back to NORMAL). Every other key passes to whatever has the
  keyboard, modifiers included. The indicator is `INSERT MODE`.
- **The address line.** While the chrome has the keyboard, only `Always` bindings whose first
  key types nothing apply, whatever the mode (ctrl-o, ctrl-r, alt-N, and ctrl-l in the terminal);
  Escape, Enter and the editing keys are the line's. The mode is unchanged by opening or
  closing the line.
- **Sequences** (`y y`, `c o`): as in the modal crate (at most 4 keys, no binding the start of
  another, modifier and dead keys skipped, an unfinished one dropped by a key that doesn't
  continue it, `reset` on losing the keyboard). A sequence may start with a key that types,
  because in NORMAL it never starts while the user types, and a lost `y` on a page without a
  field costs nothing; the modal crate's "pass modes refuse such sequences" rule is dropped
  with the pass modes.
- **Repeats** run a single-key `Run` again (a held `j` scrolls); anything else a repeat of a
  taken press is held quietly.
- **The mode is one for the browser**, not per tab, and starts NORMAL. A reload keeps it.
- **`[keymap.example]`** takes no keys: the check reports any key as "`[keymap.example]`
  takes no settings; the example configuration's bindings are in its crate". An empty table,
  or none, is fine.
- **Which frontend.** `Where::Terminal` bindings exist only when `browser.frontend()` is
  `Terminal`; `Where::Window` the reverse. The table is checked by the crate's tests, not at
  load: every keystroke parses, every command parses, no two bindings that apply together are
  the same keys or one the start of the other, and every `chrome.<action>` the terminal entries
  name is in sighurt-core's `TERMINAL_CHROME_COMMANDS` (that test lives in sighurt-runtime,
  which depends on both; the crate exposes `pub fn bindings(frontend: Frontend) -> impl
  Iterator<Item = (&'static str, &Action)>` for it).

## 3.3 The two actions the configuration defines

```rust
// extensions/sighurt-config-example/src/actions.rs
/// `y y`: copy the selected tab's address. Nothing to copy: the key is taken and nothing runs.
pub fn copy_address(browser: &dyn Browser) -> Vec<Command> {
    let tab = browser.tab(browser.active_tab());
    tab.and_then(|t| t.url).map(|url| vec![Command::ClipboardCopy(url.to_owned())]).unwrap_or_default()
}

/// `c o`: close every tab but the selected one, highest number first so the numbers of the
/// ones still to close don't move (guarantee 8).
pub fn close_other_tabs(browser: &dyn Browser) -> Vec<Command> {
    let active = browser.active_tab();
    (0..browser.tab_count()).rev().filter(|&i| i != active)
        .map(|i| Command::TabClose(Some(NonZeroUsize::new(i + 1).unwrap()))).collect()
}
```

They prove the two halves of the state API: reading one tab's fields, and a command sequence
computed from the tab list.

---

# 4. The unified configuration

## 4.1 What the built-in file becomes

- `[input] keymap = "example"`. The comment explains: the example configuration is Sighurt's
  default keys, the same in the window and the terminal; its table is in
  `extensions/sighurt-config-example/src/table.rs`, and the README lists it; `bindings` reads
  `[keys]` instead and binds nothing until you do.
- **The `[tui.keys]` table (default-config.toml:295–329) is deleted.** The "Keys" section's
  text ("The windowed frontend has no default key bindings… The terminal has a small table of its
  own") is rewritten: *the example configuration binds the keys below in both frontends; with
  `keymap = "bindings"` nothing is bound in either frontend until `[keys]` binds it, and a
  `[tui.keys]` you write applies in the terminal only, over `[keys]`.*
- `[tui] default-keys = true` stays in the file so `[tui]` still merges key by key (decision
  74 (3)'s reason), documented as having no effect since 0.10: "the terminal's default keys are
  the example configuration's". A user's file that sets it gets a notice saying so.
- `[keys]`, `[keys.page]`, `[keys.location]`, a user's `[tui.keys]`, `[tui.keys.page]` and
  `[tui.keys.location]`: unchanged, for `bindings`. The notices of decision 67 keep working:
  `[keys]` written while `example` is selected gets "[keys] is not used: [input] keymap is
  \"example\", which reads [keymap.example]".
- The version stays 1: no key changes meaning; one key (`default-keys`) stops having an effect,
  and says so.

Why delete the built-in table rather than keep it for `bindings`: one source of truth (the user's
"totally unified"), and `bindings` then means the same in both frontends ("nothing until you
bind it"). The alternative (keep it for `bindings`, with a test that it matches the example)
leaves two copies of 34 lines to drift. Decision 91's reason for the table (a terminal with no
way to quit) is met by the example being the default; a user who selects `bindings` in the
terminal with no bindings is told by the README and by the `default-keys` notice.

## 4.2 Is the example the default of the built `sig`?

Yes, in every build, including `--no-default-features --features engine-demo`: it is a plain
dependency of sighurt-runtime, like the core's `bindings`. Nothing is gained by a feature: a
browser needs default keys, the crate is small, and the headless and pty tests rely on `t`,
`x`, `q`. A user who writes their own configuration crate replaces one line in `KEYMAPS` (or,
after decision 99's step 2, one line in their application).

---

# 5. Copy and paste

## 5.1 What it means, per editor and frontend

| Where | Window | Terminal |
|---|---|---|
| A page's text and fields (Servo, WPE) | The page's own Ctrl+C/X/V/A reach the engine when the keymap passes them (unbound in the window; in INSERT, or in NORMAL while a field has the keyboard except Ctrl+C). The engine asks through `Service::ClipboardWrite`/`ClipboardRead` (decision 68's path, engines/sighurt-engine-servo/src/runtime.rs:1030–1053, engines/sighurt-engine-wpe/src/clipboard.rs); the GUI's `Clipboard` impl (frontends/sighurt-gui/src/app.rs:217–239) does it. **Works today.** | The same keys reach the engine the same way; the terminal's `Clipboard` impl (app.rs:1797–1810, which refuses everything) becomes OSC 52 (§5.3). Copy works wherever the terminal takes OSC 52 writes (kitty, Rio, xterm, and tmux with its default `set-clipboard external`); paste from the page's Ctrl+V works where the terminal answers OSC 52 reads (kitty asks the user; tmux doesn't answer). The terminal's own paste key (bracketed paste, app.rs:826–852) keeps pasting into a focused page field as today. |
| The address line | Ctrl+A/C/X/V in the standard chrome (ui/sighurt-ui/src/chrome.rs:800–816) and the line chrome (extensions/sighurt-chrome-line/src/lib.rs:407–423). **Works today.** | `LineEditor` has only Ctrl+A (app.rs:1735–1738). Add: Ctrl+C copies the selection, or the whole line when nothing is selected (a one-line editor); Ctrl+X the same and clears; Ctrl+V asks the terminal (OSC 52 read) and inserts the answer; the terminal's paste key still inserts (today). Ctrl+C is free here because the example binds it to quit in the page scope only. |
| The address of the page | `y y` (the example) → `clipboard.copy <url>`; `:clipboard.copy text` from the line. | The same. |

No `edit.copy`/`edit.paste` commands: copy and paste in an editor are that editor's keys
(Enter and Escape are not commands either), and a command that copies *from a page* would need
an engine operation Servo 0.7's embedder API doesn't have (WebKit has
`execute_editing_command`; Servo handles Ctrl+C inside script). The one command added is the one
a configuration needs to copy what it computes.

## 5.2 `clipboard.copy` through the runtime

`Effect::ClipboardWrite(text)` → in `Session::effect` (crates/sighurt-runtime/src/session.rs:283–
328, frontend-neutral): `frontend.write(Selection::Clipboard, vec![ClipboardData { mime_type:
"text/plain;charset=utf-8", data }])`; an error is `frontend.notice(format!("Copying failed:
{error}"))`; text over `MAX_CLIPBOARD` (services.rs:65) is refused with a notice, never cut. The
GUI's `Clipboard::write` goes to the host as today; the terminal's is §5.3.

## 5.3 The terminal's clipboard

In `frontends/sighurt-tui`:

- **Write:** `ESC ] 52 ; c ; <base64> ESC \` (Primary: `p`) appended to the control bytes
  (`Tui::control`, written by the one writer). Only `text/*` items are taken (the first); others
  return `ServiceError::Unsupported`. A hand-written base64 encoder and decoder (`term/clipboard.rs`,
  about 40 lines with tests) rather than a new dependency.
- **Read:** one at a time. `read(request, ..)` sends `ESC ] 52 ; c ; ? ESC \`, keeps
  `pending: Option<PendingRead { request, deadline: now + 1 s, for_line: bool }>`; a second read
  while one waits is answered `Failed("another clipboard read is waiting")`. The parser's
  `dispatch_osc` (term/parser.rs:1342–1357) gains index 52: `Input::Reply(Reply::Clipboard
  { data: Option<Vec<u8>> })` with the payload base64-decoded (`?` or empty is `None`). On the
  reply: `session.clipboard_done(request, Ok(ClipboardData{..}))` for a page's read, or
  `editor.insert` (capped at `MAX_PASTE`) for the line's. Past the deadline (checked where the
  loop already checks `notice_first`): `Failed("the terminal didn't answer")`. `cancel` drops a
  matching pending read. No timer is added: the deadline joins the loop's existing wake
  computation.
- **Under tmux** reads time out (tmux forwards OSC 52 writes but answers no query, as far as I
  know; verify in the user's setup); the terminal's paste key is the way to paste there, as
  today. The README's terminal section says so.

## 5.4 Default bindings that touch the clipboard

- Window: nothing bound on Ctrl+C/X/V/A by the example (pages and the line get them).
- Terminal: Ctrl+C = quit in the page scope (kept from decision 111); the line's Ctrl+C/X/V as
  above.
- Both: `y y` copies the address.

---

# 6. Migration plan

Each step leaves `tools/cargo fmt`, `clippy -D warnings` and the tests of its crates passing.
Steps 2, 3 and 4 are file-disjoint and can run in parallel once step 1 has landed; 5 needs 2
and 3; 6 needs everything. Where Servo or WPE must build (step 5's all-features check, step 6's
`engines.sh` and `real-terminals.sh`), use louise-pc per CLAUDE.md.

**1. sighurt-api** (`crates/sighurt-api/src/keymap.rs`, `commands.rs`, `tests/keymap.rs`).
`Frontend`, `Browser`, `TabInfo`, `KeyAction::RunAll`, `press` with the browser argument;
`Command::ClipboardCopy`. The guard test's `Pair` keymap takes the new argument and a fake
`Browser` with two tabs; add a case where it returns `RunAll` of two commands.
*Tests:* `every_name_parses_or_asks_for_its_argument` (22 names);
`display_gives_the_canonical_string_back` with `clipboard.copy https://a.b/x y`;
`clipboard.copy` with no text is `MissingArgument`; the guard test compiles and passes.

**2. sighurt-core** (`src/state.rs`, `keys.rs`, `commands.rs`, `config.rs`, `default-config.toml`,
`lib.rs`, `tests/browser.rs`).
`Frontend` re-exported; `CoreBrowser` and the free tab functions (§2.3); `KeyOutcome::Run(Vec)`;
`Effect::ClipboardWrite`; the built-in keymap fallback; the built-in `[tui.keys]` table gone and
`LAYER_BUILTIN` with it; the `default-keys` notice; `keymap = "example"` and the new comments.
*Tests:* new `a_keymap_reads_the_tabs_it_decides_with` (a fake keymap returns `tab.close` of
the tab whose title it read); `run_all_runs_in_order_in_one_update` (two `tab.close`, highest
first, leaves the right tab); `clipboard_copy_is_an_effect`; `the_builtin_keymap_falls_back_when
_the_build_lacks_it` (`Environment::default()` parses the built-in file to `bindings`; a user
file naming `example` in that environment is the error that lists the build's keymaps);
`default_keys_has_no_effect_and_says_so`. Changed: `the_terminal_installs_its_default_keys_and_
the_window_does_not` and `the_built_in_table_is_the_one_documented` become
`neither_frontend_has_default_bindings_keys` (both `keymap_settings` empty with `bindings`);
`a_higher_layer_wins_and_typing_does_not_hide_other_keys` loses its third layer;
`the_default_file_documents_every_command` and `the_documented_examples_are_valid` pass with the
new comments. Every existing key test passes after the signature change.

**3. The example crate** (`extensions/sighurt-config-example/**`, root `Cargo.toml` members;
`extensions/sighurt-keymap-modal` deleted).
*Tests* (`tests/example.rs`, carried over from `tests/modal.rs` where they apply):
`the_table_is_valid_in_both_frontends` (parses, no conflicts, per §3.2); `escape_toggles_insert
_and_the_indicator` (NORMAL → `None`; Escape → `Some("INSERT MODE")`; `q` passes; Escape →
`None`; `q` runs `browser.quit`); `normal_keeps_typing_keys_for_a_field` (typing=true: `j`
passes, `ctrl-d` runs); `in_the_line_only_always_applies` (focus Chrome: `ctrl-o` runs, `q`
and Escape pass); `terminal_bindings_exist_only_there` (`m` runs `chrome.toggle-mode` for
`Terminal`, passes for `Gui`; `ctrl-c` likewise); `y_y_copies_the_address` (a fake browser with
`url = Some(..)` gives `RunAll([clipboard.copy ..])`; with `None`, `Take`); `c_o_closes_the_
others_highest_first` (5 tabs, active 2 → `tab.close 5, 4, 3, 1`); the modal tests for
sequences, modifiers, repeats, reset and reload adapted.

**4. The terminal's clipboard and line keys** (`frontends/sighurt-tui/src/app.rs`,
`term/parser.rs`, `term/mod.rs`, new `term/clipboard.rs`; `chrome.rs` only if `LineEditor`
needs a `take_text`).
*Tests* (unit, in the crate): base64 round trips and rejects bad input; the parser turns
`ESC ] 52 ; c ; aGVq ESC \` into `Reply::Clipboard(Some(b"hej"))` and `?` into `None`;
`write` produces the exact OSC 52 bytes for `text/plain` and refuses `image/png`; a second
`read` while one waits fails at once; a read past its deadline is answered `Failed`; the line's
Ctrl+C with nothing selected copies the whole line, Ctrl+X clears it.

**5. Wiring** (`crates/sighurt-runtime/src/registry.rs`, `session.rs`, `Cargo.toml`;
`frontends/sighurt-gui/Cargo.toml`, `frontends/sighurt-tui/Cargo.toml`,
`apps/sighurt-browser/Cargo.toml`; `.github/workflows/ci.yml`, `servo.yml`).
`KEYMAPS = &[sighurt_config_example::KIND]`; `Effect::ClipboardWrite` carried out; the
`keymap-modal` features removed everywhere.
*Tests:* sighurt-runtime's `keymaps_follow_the_build` (the example is there);
`clipboard_write_goes_to_the_frontend_and_failures_are_notices` (the fake frontend records
the write; a failing one yields the notice); `the_examples_terminal_chrome_commands_exist`
(every `chrome.<action>` from `bindings(Terminal)` is in `TERMINAL_CHROME_COMMANDS`);
`tools/cargo check -p sighurt-browser --no-default-features` and `--features engine-demo`,
`frontend-tui,engine-demo`; on louise-pc `--all-features` clippy.

**6. End to end, and the documents** (`apps/sighurt-browser/tests/headless.sh`, `engines.sh`,
`tests/tui.rs`; `README.md`, `CLAUDE.md`, `docs/rewrite/STATUS.md`, `extension-api.md`
changelog, `TUI.md` line 402, `default-config.toml` if a comment needs the test names).
- `headless.sh`: the `modal` scenario becomes `example`, built with `engine-demo` alone: `t`
  gives 2 tabs (diagnostics); Escape then `t` leaves 2 and the indicator reads `INSERT MODE`
  (add `keymap_indicator` to the diagnostics JSON if it isn't there: it is
  `BrowserState::keymap_indicator`); Escape then `x` gives 1; a screenshot with the indicator
  visible, looked at. The `main` scenario keeps working with the example's keys where it typed
  into `[keys]` before (check its config; drop a `[keys]` table that now only earns a notice).
- `engines.sh` showcase: the example in place of `[keymap.modal]`; its steps use `J`/`K` and
  Escape instead of `g t`/`g T`; the line chrome shows `INSERT MODE`.
- `tests/tui.rs`: in `kitty_keys_open_addresses_tabs_and_quit`, Escape then `q` does not quit
  and the top line shows `INSERT MODE`; Escape then `q` quits. New
  `y_y_writes_the_address_with_osc_52`: the fake terminal receives `ESC ] 52 ; c ; <base64 of
  the demo URL>`. New `ctrl_v_in_the_line_pastes_the_terminals_answer`: the test answers the
  query with base64 text and the line shows it.
- README: the terminal's key table becomes "the default keys, in both frontends", with Escape,
  `y y`, `c o` and the per-frontend lines; the commands table gains `clipboard.copy`; the
  terminal section says how copy and paste work there.
- CLAUDE.md: the rule "Keys live only in the user's configuration: no key handlers in code, no
  default bindings" becomes "Keys live only in the configuration: the example configuration
  crate (the default, in Rust) or the `bindings` keymap's `[keys]` tables. The core, the
  frontends and the chromes bind no keys; a frontend's chrome handles only its own editing keys
  (Enter, Escape, arrows, Ctrl+A/C/X/V in the address line)." The crate table gains
  `sighurt-config-example` and loses `sighurt-keymap-modal`; the command list gains
  `clipboard.copy`; the build commands lose `keymap-modal`; "Keymaps and chromes are compiled
  in… (`keymap-modal`, `chrome-line`)" becomes `(chrome-line)`.
- `real-terminals.sh` in Rio and kitty, and a live check in the user's tmux: Escape shows
  `INSERT MODE`, `y y` lands on the clipboard, `q` quits.

---

# 7. Decisions for STATUS (once the user accepts)

**116. The configuration extension: the keymap reads the browser, runs sequences, and is the
example configuration by default** (this document; amends 68, 74 (5), 91, 99's wording and
111; CLAUDE.md's "no default bindings" rule is rewritten as §6 step 6 says).
(1) `sighurt_api::keymap` gains `Browser` (a borrowed, read-only view: the frontend, the key
context, the tabs as `TabInfo` and which is selected), `Frontend` (moved from the core, which
re-exports it) and `KeyAction::RunAll`; `Keymap::press` takes the browser. The core builds the
view from its own fields for one call and runs a `RunAll` whole, in order, in that update. The
chrome API is unchanged; the indicator is still the one thing a keymap shows, and both chromes
show it. (2) `sighurt-keymap-modal` is replaced by `sighurt-config-example` (`example`), always
compiled in and the built-in `[input] keymap`; its bindings are Rust: the terminal's 0.9.0 keys
in both frontends (`ctrl-c`, `m` and `ctrl-l` in the terminal only; `ctrl-c` only while the page
has the keyboard), Escape toggling NORMAL and INSERT (INSERT passes every key but Escape and
shows `INSERT MODE`; NORMAL shows nothing), `y y` copying the address and `c o` closing the other
tabs, both computed from the browser view. `[keymap.example]` takes no keys. The built-in file's
keymap falls back to `bindings` in a build that lacks it (as 82 (3) does for the default
engine). (3) The built-in `[tui.keys]` table is gone and `[tui] default-keys` does nothing (a
notice when set); `bindings` binds nothing in either frontend until `[keys]` does, and a user's
`[tui.keys]` still applies in the terminal over `[keys]`. Version 1 stands. (4) `clipboard.copy
<text>` is a command (`Effect::ClipboardWrite`, carried out by the runtime through the
frontend's `Clipboard`; too much text is refused with a notice). (5) The terminal has a
clipboard: OSC 52 writes, OSC 52 reads one at a time with a 1 s deadline, bracketed paste as
before; its address line copies, cuts and pastes with Ctrl+C/X/V and selects all with Ctrl+A.
Page copy and paste in both frontends are the engines' own keys through the service path of 68.
*Why:* the user's request (31 October 2026): one configuration for both frontends, the
terminal's keys in the window, Escape as an insert-mode switch shown in the UI, a Rust layer
that reads and changes browser state, copy and paste; plan §28's first question (what a key does
has one owner, in both frontends) and §26 (no copies, no ticks: the view is borrowed, the
indicator redraws on change).

---

# 8. Risks and uncertainties

- **Nothing was compiled.** The disjoint-borrow construction in `keyboard()` (§2.3) is the one
  place a worker may hit the borrow checker; the free-function refactor of `summary`/`can_go`
  is the intended fix. If it fights, an owned per-press snapshot of the *active tab only* is the
  fallback (one small clone per key press), and `tab(i)` for other tabs stays borrowed.
- **Escape never reaches a page in NORMAL** (it enters INSERT), and in INSERT it leaves. Pages
  that close dialogs with Escape need INSERT then… nothing: Escape leaves INSERT instead. The
  way to send Escape to a page is `:page.send-key escape`. If this bites, the fix is one table
  line (bind the switch to another key, or `escape escape`); the design doesn't change.
- **`q` quits the window** when no page field has the keyboard. The user asked for the
  terminal's keys; the session is restored on the next start. One table line removes it.
- **Terminal clipboard reads:** kitty prompts the user (`read-clipboard-ask`), Rio's behaviour
  is unverified, tmux answers nothing (believed, not verified): page Ctrl+V and the line's
  Ctrl+V fail after 1 s there, with the terminal's own paste key as the way. Writes are the
  common case and work through tmux's default `set-clipboard external`.
- **Page copy in the terminal needs INSERT or a focused field** in NORMAL (Ctrl+C quits in the
  page scope). This keeps decision 111's way out of a focused field; the cost is one Escape
  before copying from a page.
- **The modal keymap's TOML modes are gone.** Anyone wanting TOML-configured modes has
  `bindings` (no modes) or edits the example's table. TOML overrides of the example's table
  (`[keymap.example.normal] "j" = ""`) are a later addition: the modal crate's reader is in git
  history and would merge over the Rust table with the same conflict checks. Left out now as
  speculative.
- **`[tui] default-keys` becomes a no-op.** It is the one v1 key that stops doing something;
  the notice and the README say so. The alternative (keep the built-in table for `bindings`)
  keeps two copies of the same keys.
- **Decision 99 step 2** will move `KEYMAPS` out of sighurt-runtime; this design puts the example
  there for now (a plain dependency), which is one line to move later.
- **The diagnostics JSON may lack the indicator**; step 6 adds it if so (needed by the headless
  check).
- **Not covered, deliberately:** status segments or other UI the configuration could add beyond
  the indicator; per-tab modes; TOML overrides; `edit.*` commands; a page-copy engine operation
  (WebKit could, Servo can't); link hints, find, counts (extension-api.md §2.5's table still
  applies).

## Considered and left out

| Idea | Why not |
|---|---|
| Renaming `Keymap` to `Configuration` and `[input] keymap` to `[input] configuration` | Two names for one TOML key, or a version 2 for a cosmetic change; "configuration" also collides with `sighurt_core::config`. The doc comment of `keymap` says what the trait is now. |
| Passing `Frontend` to `KeymapKind::new` and `check` | `Browser::frontend()` at press time is enough, and the table's per-frontend validity is a test of compiled-in code, not of a user's file. |
| An owned snapshot of every tab per press | A string clone per tab per key press; the borrowed view costs nothing. |
| `edit.copy`, `edit.cut`, `edit.paste` commands | Editor keys are the editor's; a command that copies from a page needs an engine operation Servo doesn't offer. `clipboard.copy` covers what a configuration computes. |
| A `clipboard.paste`/`clipboard.open` that opens the clipboard's text as an address | A read with a reply and a second step; nobody asked. |
| Keeping the built-in `[tui.keys]` for `bindings` | Two copies to drift; `bindings` means the same in both frontends without it. |
| TOML overrides of the example's table | Speculative now; the path is known (§8). |
| A per-tab mode | The user asked for a toggle; per-tab needs `Keymap` to hear tab changes, which no other use needs. |

## Evidence

Read for this design (31 October 2026, `rewrite` at 1cb65d8): `crates/sighurt-api/src/{keymap,commands,services,input}.rs`;
`crates/sighurt-core/src/{state,keys,config,tabs,lib}.rs` and `default-config.toml`;
`crates/sighurt-runtime/src/{registry,frontend,session,services}.rs`; `ui/sighurt-ui/src/{extension,chrome,state}.rs`;
`extensions/sighurt-keymap-modal/src/lib.rs` and `tests/modal.rs`; `extensions/sighurt-chrome-line/src/lib.rs`;
`frontends/sighurt-gui/src/{chrome,app}.rs`; `frontends/sighurt-tui/src/{app,chrome}.rs`, `term/{parser,modes}.rs`;
`docs/rewrite/extension-api.md`, `crate-organization.md`, STATUS decisions 67–77, 82, 87, 89–99, 104–115;
the Cargo features of the five crates that name `keymap-modal`; `headless.sh`, `engines.sh`, `tests/tui.rs`, the README's tables.
Not verified: any compilation; OSC 52 read behaviour of Rio and tmux; whether the diagnostics JSON carries the indicator.

