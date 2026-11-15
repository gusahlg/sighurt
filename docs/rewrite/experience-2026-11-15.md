# The Experience: one programmable configuration for keys, modes and, later, the interface

**Status:** design, 15 November 2026, against `config-ext` at 4aac14a (1.0.0). Parts 1 to 3
(the key engine, the Rust SDK, the default experience through it) are to be implemented now.
Parts 4 and 5 (the composable interface, loading a configuration as a module) are proposals:
nothing in them is decided, and each needs its own design and prototype before code.
**Asked for** (the user, 15 November 2026, "Sighurt: a fully programmable browser experience"):
one system in which the defaults and the user's changes use the same API; changing one shortcut
must not lose the rest; arbitrary modes, sequences, contextual rules, overriding and unbinding,
custom actions in Rust that read the browser, configurable handling of unmatched keys,
introspection for the help page; Rust for programming the experience and TOML for plain
settings; later a composable interface and configurations compiled apart from `sig`, perhaps
as Wasm.
**Read with:** CLAUDE.md ("Commands, keys and configuration"), `sighurt_api::keymap`'s module
documentation (the ten guarantees), decisions 99, 104, 116, 134 and 140, and
config-extension.md, which this supersedes where they disagree.

## 0. What is wrong today, precisely

There are two keymaps behind one trait, and a user picks one:

| | `example` (Rust tables) | `bindings` (TOML `[keys]`) |
|---|---|---|
| Modes, strict NORMAL/INSERT | yes | no |
| Sequences (`y y`) | yes | no |
| Actions computed from the browser | yes | no |
| Changed without rebuilding `sig` | no | yes |
| Per-address bindings | replace a mode's table | add to it, most specific table only |

Each has its own matcher, its own address rule and its own `describe`. A user who wants `x`
to do something else must either rebuild `sig` with an edited table or switch to `bindings`
and lose every mode, sequence and default. `[keys]` under `keymap = "example"` is ignored with
a notice. That is the "choose an implementation, then configure what it exposes" shape the user
wants gone.

## 1. The model

One key engine in the core. It is driven by data, **`Keys`**, and nothing else. The default
keys are a `Keys` value built with the public SDK; a user's Rust configuration changes that
value; the TOML `[keys]` tables are applied to it by the same operations. There is no second
matcher and no trait to implement.

### 1.1 The data (`sighurt_api::keymap`)

```rust
/// Everything the key engine is configured with.
pub struct Keys {
    pub modes: Vec<Mode>,   // at least one; names unique; at most 16
    pub initial: String,    // the mode a session starts in
    pub line: Vec<Binding>, // the table while the chrome has the keyboard (§1.3)
}

pub struct Mode {
    pub name: String,        // "normal": 1 to 32 of a-z, 0-9, '-'
    pub indicator: String,   // what the chrome shows in this mode; may be empty
    pub page_text: bool,     // text that is not a key press may reach the page (guarantee 9)
    pub unmatched: Fallback, // a key no binding has, while the page has the keyboard
    pub bindings: Vec<Binding>,
}

pub struct Binding {
    pub keys: KeySeq,  // 1 to 4 keystrokes ("y y"); modifier and dead keys are never members
    pub when: When,
    pub action: Action,
}

/// Conditions; all must hold. `Default` is "always".
pub struct When {
    pub frontend: Option<Frontend>, // only in the window, or only in the terminal
    pub url: Option<UrlPattern>,    // only on these addresses (the shown tab's committed one)
    pub field: Option<bool>,        // only while a page text field has / hasn't the keyboard
}

pub enum Action {
    /// The key is taken and these run in order (guarantee 8). Empty: taken, nothing runs.
    Run(Vec<Command>),
    /// These run, then the key goes to whatever has the keyboard now. Empty: the key passes.
    RunThenPass(Vec<Command>),
    /// Computed when the key is pressed, from what the browser shows before it.
    Custom(Custom),
    /// A tombstone: nothing is bound to exactly these keys here, and a less specific binding
    /// of exactly these keys does not apply either (§1.4). The key falls to `unmatched`.
    Unbound,
}

/// What a computed action comes to: plain data, never another computation.
pub struct Outcome {
    pub commands: Vec<Command>, // run in order
    pub pass: bool,             // then the key goes to whatever has the keyboard
}

/// A named function of the browser's state.
pub struct Custom {
    pub describe: String, // "copy the page's address": what the help page says
    pub run: Arc<dyn Fn(&dyn Browser) -> Outcome + Send + Sync>,
}

pub enum Fallback {
    Pass,  // the key goes to the page
    Take,  // the key is taken and does nothing
    /// Decided per key. Sees the key.
    Custom(Arc<dyn Fn(&KeyEvent, &dyn Browser) -> Outcome + Send + Sync>),
}
```

`Keys::check(&CommandSet) -> Vec<String>` validates a whole value, however it was built: the
bounds, unique mode names, `initial`, every `input.mode` naming a mode, every alias being the
configuration's, and no binding the start of another with the same `when` (§1.4).

`KeySeq` is a new type beside `Chord` (parsing "g g", `Display` giving it back). `Chord`,
`UrlPattern`, `Browser`, `TabInfo`, `KeyContext`, `Frontend` and `KeyBinding` stay as they are.

**Modes change by command.** There is no `Action::Mode`. A new command, `input.mode <name>`,
sets the mode; the core carries it out on its key engine. So a key, a mouse button
(`[input.buttons]`), the command line, a `[commands]` alias and, later, an event hook all
switch modes the same way, and an action stays "commands, and whether the key passes".
`Escape` in the default experience is a `Custom` that returns `page.exit-fullscreen` while the
page holds fullscreen and `input.mode insert` otherwise: what `Action::ExitFullscreenOr` was,
without the special variant. A mode name that no mode has is an error when the configuration
loads (in a binding, an alias or a button) and a notice, changing nothing, when it is typed or
a `Custom` returns it.

What `input.mode` is, exactly (review finding 1 and 7):

- It runs in its place among a key's commands, at once; `input.mode resize` then `input.mode
  normal` in one list ends in `normal`. It drops an unfinished sequence, also when the mode is
  the one already set. It moves no focus and closes no editor: bound in `line`, it changes the
  page's mode while the line keeps the keyboard. Typed in the command line it is submitted like
  any command. With `pass`, the key is delivered and never matched again in the new mode.
- **It does not acknowledge a page.** Today a command a key runs lifts the hold on input after
  the front tab changed without the user choosing it; a mode switch (`KeyAction::Take` today)
  does not. So `input.mode` alone never lifts that hold: the hold is lifted by a list that
  runs some other command. An empty list is inert, as now.
- It is never run again by a held key (§1.2, repeats).

**Why data and not a trait.** (a) Composition: a user's change is an edit of a value, so
"override one key" and "start from nothing" are the same mechanism. (b) Introspection:
`describe` is generated from the tables the engine really uses, TOML overrides included. (c)
The module boundary (Part 5): the tables cross it as bytes, a `Custom` as a reference the host
calls back, and its `Outcome` as bytes again. `Arc<dyn Fn>` is the native form of that
reference, not something that crosses.

**What is given up** (review finding 10): the trait let a keymap be an arbitrary state
machine. Tables, modes and stateless functions cannot count (`3 j`) or collect hint labels,
since a count must survive from one key to a binding of another. No keymap does either today.
A `Custom` closure may keep state of its own in-process, but it gets no lifecycle calls and no
indicator. A bounded stateful hook (a reducer over keys and lifecycle events) belongs with the
events of a later part, designed when something needs it.

### 1.2 What the engine does with a key (`sighurt_core::keys`)

The engine keeps: the tables, the current mode, an unfinished sequence with where it began
(the table, the shown tab and its committed address), and which single-key binding the key now
held ran (for repeats). `KeyState` keeps what it keeps today (the presses taken,
per seat) and guarantees 1 to 4 unchanged.

For a fresh press:

1. **The table** is `line` while the chrome has the keyboard, else the current mode's.
2. **The bindings that apply** are those of the table whose `when` holds, minus the *typing
   rule*: a binding whose first keystroke would type a character (`Chord::types`) does not
   apply where typed text is going, which is the chrome's line, and a page text field in a
   mode with `page_text`. (In strict NORMAL, `page_text` is false and `q` is a command even in
   a field: decision 134. In INSERT, or with no modes at all, a letter typed into a field
   types: what `bindings` always did.)
3. **Among those that the keys typed so far could be** (equal to them, or starting with them),
   the *most specific* decides (§1.4): if it is complete it runs; if it is a start, the engine
   waits for the next key and shows the keys in the indicator. No timeout, ever. If none could
   be and the sequence was longer than one key, the sequence is dropped and its last key starts
   afresh (today's rule). **A sequence belongs to where it began** (finding 5): if the table,
   the shown tab or its committed address is not what it was at the sequence's first key, the
   sequence is dropped and this key starts afresh, so a binding that did not apply then never
   inherits earlier keys. `input.mode`, a reload, `context` and `reset` drop it too.
4. **No binding, or `Unbound`:** the table's `unmatched` decides (in `line` it is always
   `Pass`: every key no binding has is the line's own).
5. **`Custom`** is called once, with the `Browser` view of the state before the press.
6. **Modifier and dead keys** never join, continue or drop a sequence. One is looked up alone
   as a single-key binding (a `[keys]` file may bind `code:` keys that are dead on a layout,
   as today, finding 15) and otherwise goes to `unmatched`.

**Delivery is one path** (finding 6). An outcome's commands run through one ordered runner;
then, if it passes, the key is delivered exactly as a key no binding took: by the focus as it
is after the commands, through the core's fullscreen-Escape guard, as the same press (never a
fresh activation in the view it lands in), with its repeats and release following it. Passing
with no commands is plain delivery. Of a sequence only the last key can pass; the earlier ones
were taken and are not replayed.

**Repeats** (finding 3) are matched again, never replayed from a cache: a repeat of a taken
press runs only if, looked up alone now, it is the same single-key binding its press ran, that
binding is a plain `Run`, and then without any `input.mode` in it. Everything else taken is
held quietly. So releasing Ctrl while A repeats stops `ctrl-a` (the `bindings` rule), and mode
switches and `Custom` actions never repeat (the example's rule).

A reload keeps the mode if the new tables have it, else `initial`. The indicator is the mode's
`indicator`, then the unfinished sequence's keys as the bindings spell them, never event text.

### 1.3 The line's table

Today "the modes are the page's": while the address line or the find field has the keyboard,
NORMAL's `Always` bindings whose first key types nothing apply in both modes. In the model
that is simply a table of its own, `Keys::line`: bindings only, no mode. The default
experience binds those keys (ctrl-o, ctrl-r, f11, f1, alt-1…9, the terminal's ctrl-l) in
`normal` and in `line` by naming both tables: `keys.tables(["normal", "line"])` is a handle
that binds in each table it names, as they are when it is called (finding 13; nothing is
inferred from a mode's properties, and a mode added later gets nothing). By the typing rule a
`line` binding whose first key types never fires; a broadcast that puts one there (`[keys]`
`j`) is not an error.

### 1.4 Overriding, unbinding, and which binding wins

**Binding is replacing** (finding 8). `bind` and `unbind` in a table, with a `when` W, first
remove from that table every binding with the same address condition as W whose keys are the
same, a start of the new keys, or started by them, and whose `field` and `frontend` conditions
are W's or narrower (W leaves the condition unstated, or states the same). Then the new binding is added (for `unbind` with no address: nothing is added,
the removal is the unbinding; with an address: the tombstone). So:

- `normal.bind("space", "tab.next")` replaces the default Space although that one is stated
  for "outside fields", and a plain `ctrl-l` replaces the default's terminal-only one: a user
  need not know the conditions a default carries.
- `normal.bind("g g", …)` removes the default `g`, and binding `g` later removes `g g`: the
  later definition wins a prefix conflict, so no binding is ever the start of another with the
  same `when` (and `Keys::check` rejects a hand-built value where one is).
- Bindings for an address are untouched by a binding without one, and the reverse: a site's
  exceptions survive a general override. `insert.bind("escape", …)` leaves the terminal pages'
  Escape alone.
- `in_window().bind("q", …)` leaves the general `q` for the terminal.

**When a key is pressed**, of the bindings that apply and that the keys could be, the most
specific decides, in this order:

1. the more specific address (`UrlPattern`'s order: an exact host before a wildcard, then the
   longer path), any address before none;
2. `field` stated before unstated; `frontend` stated before unstated;
3. the binding defined later.

A tombstone masks only bindings of exactly its keys: it is no start of anything and hides no
longer sequence (finding 4). `insert.on("sig://terminal").unbind("escape")` makes Escape fall
to INSERT's `unmatched` (pass, to nvim) on the terminal pages only.

`Keys::resolve(table, keys, context, address)` answers "what would these keys do here, and
which binding says so" for tests and for the help page.

This **replaces decision 140's rule** that a site's table replaces the mode's general table.
The evidence that is new: that rule was chosen because a site had no other way to take a
binding away; `Unbound` is that way, and replacement would make a site that adds one NORMAL key
lose every default. It also changes one thing for `[keys.url]`: every matching address table
now contributes, the most specific first, where before only the most specific table did
("a less specific match adds nothing"). A file with two overlapping patterns that relied on the
less specific one being silenced must unbind (`"x" = false`).

A site condition selects shortcuts; it grants nothing (finding 16). `UrlPattern` ignores the
port, and titles and addresses a `Custom` reads are the page's data: a configuration passes
them as a command's operand (`Command::ClipboardCopy(url)`), never parses them as commands.
What is the browser's own page is `TabInfo::internal`, the core's record.

### 1.5 The TOML key tables are the same operations

`[keys]` stays, and now works with the default keys instead of replacing them. Each entry is
one `bind` on the `Keys` value, applied after the Rust configuration, in this order (so the
later tables override the earlier):

| Table | Binds in | `when` |
|---|---|---|
| `[keys]` | every mode, and `line` | always |
| `[keys.page]` | every mode | always |
| `[keys.location]` | `line` | always |
| `[keys.mode.<name>]` (new) | that mode | always |
| `[keys.url."<pattern>"]` | every mode | that address |
| `[keys.mode.<name>.url."<pattern>"]` (new) | that mode | that address |
| `[tui.keys…]`, each of the above | the same | and `frontend = terminal` |

A value is a command in quotes, or `false` to unbind (new). Keys may be sequences (new).
Within one table, a key that is the start of another is an error (a TOML table has no order).
TOML binds commands only; modes, `Custom` actions, fallbacks and the `field` condition are
Rust's. `input.mode <name>` is a
command, so TOML can switch modes.

`[input] keymap` stays valid and chooses what the tables start from: `"example"` (the default)
is the experience's keys; `"bindings"` is no experience at all, `Keys::plain()`: one mode that
passes unmatched keys and gives the page text, binding nothing. With it `[keys]` behaves as
`bindings` did (same contexts, same typing rule), with two differences: §1.4's change, and
that `[tui.keys]` no longer wins over everything in `[keys]` (finding 2): it wins over the same
table of `[keys]`, but a `[keys.url]` binding for the address shown wins over a `[tui.keys]`
binding with no address, because an address is more specific than a frontend. One precedence
rule for every source was preferred to keeping layers; no known file depends on the old order. The names stay because users'
files have them; they now name starting points, not implementations. `[keymap.<name>]` tables
get the notice a table for an absent keymap gets today.

A user who binds a letter in `[keys]` binds it in INSERT too, where it fires when no field has
the keyboard; the built-in file says to put letters in `[keys.mode.normal]`.

### 1.6 What goes away

`Keymap`, `KeymapKind`, `KeyAction`, the `bindings` keymap's own matcher, `Example`'s matcher,
`KEYMAPS` in the registry, `Environment::keymaps`, and the per-keymap `check`. The guarantees
in `sighurt_api::keymap`'s module documentation stay, reworded for "the engine" (5: `Custom`
functions and fallbacks do no I/O, read no clock, start no thread, and log nothing; 7 and 8
unchanged; 9 is `Mode::page_text`). `Keymap::describe` becomes a function of `Keys`:
mode, keys, the commands as written (or `Custom::describe`, "pass to the page", "nothing"), and
`applies` from `when`.

## 2. The SDK: `sighurt-config`

A new crate, `sdk/sighurt-config` (Apache-2.0 like `sighurt-sdk`, so configurations may be
licensed freely), depending on `sighurt-api` only. It is what a configuration is written
against, and what the default experience is written against:

```rust
use sighurt_config::prelude::*;

pub fn configure() -> Experience {
    let mut sig = Experience::default();

    sig.keys.normal()
        .bind("e", "page.switch-engine")
        .bind("g g", "page.send-key home")
        .bind("ctrl-g", Command::PageOpen("https://github.com".into()))
        .unbind("x");
    sig.keys.mode("insert").on("https://mail.example.com").bind("ctrl-enter", send_mail);
    sig.keys.add_mode(Mode::new("resize").indicator("RESIZE").unmatched(Fallback::Take));

    sig
}
```

- `Experience` is the SDK's type: `{ keys: KeysBuilder, .. }`, `#[non_exhaustive]`, to grow
  `ui`, `commands` and `events` in later parts. `Experience::default()` is the default
  experience, built by `sighurt_config::defaults` with nothing but this API (a test holds that
  module to the prelude's names). `Experience::empty()` is `Keys::plain()`.
- A table handle (`normal()`, `mode(name)`, `line()`, `everywhere()`) has `bind`, `unbind`,
  `pass`, `take`, and the scopes `on(url)`, `in_window()`, `in_terminal()`, `in_field()`,
  `outside_fields()`, which return a narrower handle. `bind` takes the keys as text and
  anything `Into<Action>`: a `Command`, a command as text, a `Vec<Command>`, a function
  `fn(&dyn Browser) -> Outcome` wrapped by `custom("what it does", f)` (so the sketch's
  `send_mail` is `custom("send the mail", send_mail)`).
- **Mistakes are collected, not panics**, and checked at the end (finding 12). Bad keys and
  unparsable commands are noted as they are written; what needs the whole picture (a mode
  that never gets added, an alias the file's `[commands]` lacks, a prefix conflict in a
  hand-built table) is `Keys::check`'s, run by the core when the configuration loads.
  **An experience with any problem is not used at all**: `sig` says what is wrong and runs
  the default experience, since half a configuration can have unbound its way out of a mode
  whose replacement binding was the rejected one. A TOML reload with a problem keeps the
  configuration in use, as now. The default experience's own problems fail a test.
- There are no typed duplicates of commands. `Command` is the typed action; a second enum
  (`Action::SwitchEngine`) would be a second vocabulary to keep in step. Commands missing as
  primitives (the user's `ScrollToTop`) are added as commands, where TOML and the command line
  get them too.

The core does not know `Experience`. `sig` takes it apart: `keys` goes to the core in its
`Environment`; later parts go to the frontends. `sighurt-config-example` stays the crate
compiled into `sig` as *the configuration*: its `configure()` returns `Experience::default()`
with a commented example of each kind of change, and the registry calls it. Editing that crate
(or pointing the build at a copy) is how a Rust configuration is used until Part 5.

## 3. Compatibility, and what must not change

- Every binding of today's example configuration behaves as it does now in both frontends,
  the strict modes included (decision 134), `sig://terminal` and `sig://config` leaving INSERT
  on `ctrl-escape` or `f12` and giving Escape to the program (decision 140's behaviour, by
  §1.4's mechanism). The example crate's tests and `apps/sighurt-browser/tests` are the check;
  they are ported, not rewritten to fit.
- `keymap = "bindings"` with existing `[keys]` files behaves as before except §1.4.
- The fullscreen Escape exception in the core (`KeyState`) is untouched.
- The help page and `sig://home` list bindings from the engine's `describe`.
- Schema version stays 1: no key changes meaning; `[keys]` under the default keymap changes
  from "ignored with a notice" to "applied".
- One new command, `input.mode <name>` (variant, `NAMES`, parsing, `Display`, the core,
  `default-config.toml`, README).

## 4. Proposal: the interface as components (not decided)

The same move for the chrome. Today a chrome is a trait (`sighurt_ui::extension::Chrome`), the
standard one is one implementation with options, and the terminal has its own, unrelated one.
Proposed: a frontend-neutral **layout tree** in `sighurt-api` (data, like `Keys`):

```rust
sig.ui.layout(Column::new()
    .child(TabBar::new())
    .child(Row::new().child(AddressBar::new()).child(Indicator::new()))
    .child(PageView::new().fill())
    .child(StatusBar::new().when_needed()));
sig.ui.tabs.position(Side::Left);
```

Each frontend has one interpreter of the tree: `sighurt-ui` lays components out into at most
8 surfaces and draws them with the `Pen`; `sighurt-tui` lays them out in cells. The components
are the standard chrome's existing pieces, cut along the lines they already have. The `Chrome`
trait stays as the low-level API (a `Custom` component draws with a `Pen` in the window and
with cells in the terminal). `[ui] chrome` becomes a choice of starting layout, as `[input]
keymap` does in §1.5. Split views (several `PageView`s) need the core's one-shown-view model
extended and are a separate decision. Open questions that need a prototype: how a component's
size is negotiated without a general constraint solver; how one tree degrades in the terminal;
the 8-surface bound; hit testing and focus between components.

## 5. Proposal: configurations as modules (not decided)

A configuration compiled apart from `sig`, to Wasm, loaded and reloaded while it runs. What
Parts 1 to 3 do to keep that open: an experience is data but for `Custom` functions; those
are pure functions of a bounded, serializable view; nothing holds a reference into the core.
Proposed: a module exports `configure` (returns the serialized experience) and `call(index,
browser)`; a host of its own in `sighurt-runtime` (not the page engine's `"oxide"` or `"sig"`
imports: a third, versioned import module, with no network, clock, filesystem or page data
beyond `Browser`), with fuel and wall-clock limits per call, since a `Custom` runs inside a
key press; a module that traps or runs out is a notice and the key is taken. From the review
(findings 9 and 11): the two calls have different payloads (a fallback also gets the key);
the `Browser` a module sees must be a bounded or asked-for snapshot, never all 1 000 tabs'
addresses per key; references to functions have a generation, so a call into a replaced module
is refused; a module's fallback gets no keys while typed text is going to a field, since
`key` and `code` disclose it as well as `text`; and a module has no network import but can
still open pages by returning `page.open`, so it is not "without network". `sig` watches
`{config_dir}/sighurt/experience/`, builds it with the user's cargo for wasm32, validates the
result and swaps it in; a failure keeps the experience in use. Open: whether to ship the SDK's
source with `sig` or take it from a registry; what a `Fallback::Custom` in a module may see of
typed text; build latency; whether `sig` should run cargo at all or only load a module the user
built.

## 6. Order of work

1. **The engine** in `sighurt-core` and the data in `sighurt-api`, with both existing keymaps
   re-expressed on it and every existing test ported; `input.mode`; the TOML tables as
   operations (§1.5).
2. **The SDK** and the default experience through it; `sighurt-config-example` becomes the
   compiled-in configuration; the trait and the registry of keymaps go.
3. Documents: CLAUDE.md, README, DOCS, `default-config.toml`, extension-api.md's changelog,
   STATUS (the decisions this makes, and the amendment of 140).
4. Parts 4 and 5 each start with a design of their own.

## 7. Review against the plan's questions

1. *One owner per state field:* the mode, the unfinished sequence and the tables are the
   engine's, in the core; the presses taken stay `KeyState`'s. The SDK owns nothing at run time.
2. *Delayed events:* none are introduced; a `Custom` runs inside the update of its press.
3. *Copies and ticks:* none. Sequences never wait on a timer.
4. *Renderer details in the core:* none; `Frontend` is the one fact about presentation a
   binding may ask for, as today.
5. *Claims and proof:* Parts 1 to 3 are held by the ported tests; Parts 4 and 5 are proposals.
