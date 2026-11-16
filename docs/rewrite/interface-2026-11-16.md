# The interface as a layout tree: the Experience, Part 4

**Status:** design, 16 November 2026, against `config-ext` at 09ba22d. It replaces the half-page
proposal in `experience-2026-11-15.md` §4 and is to be implemented in the stages of §7.
**Asked for** (the user, 15 November 2026): the chrome as reusable components (`TabBar`,
`AddressBar`, `StatusBar`, `CommandLine`, `PageView`, `Row`, `Column`, `Overlay`); common
changes by arranging components; lower-level drawing still reachable; behaviour
frontend-neutral with separate window and terminal presentations; room for split views later.
**How it was made:** a read-only design study of the three existing chromes (the deep-design
agent), whose report is the body below; the three decisions of §6 are taken as it recommends
(Claude Fable 5.2, orchestrating): two trees over one vocabulary; the `Chrome` trait stays as
the host's seam and the choice among whole chromes goes; compound components now, with `Label`
the only custom primitive. Nothing here is built yet; pixel identity of the standard chrome is
the claim stage 1's property test must prove.

## Summary

Make the layout a small tree of **compound components** in `sighurt-api`, with **two trees per experience** (`ui.window`, `ui.terminal`) over one vocabulary. In the window, the interpreter is the existing `Model` with its `Layout` computed from the tree instead of from `LayoutOptions`. That gives pixel identity by construction for the standard chrome, and stages 0–4 fit in two days. Folding the line chrome in is a third day.

Nothing was built or run; this is from reading the code. I did not read `frontends/sighurt-gui/src/app.rs`, `ui/sighurt-ui/src/text_edit.rs`, `ui/sighurt-ui/tests/paint.rs`, `extensions/sighurt-chrome-line/tests/line.rs`, `docs/rewrite/TUI.md` or the terminal's home drawing.

## Where the half-page proposal is naive

1. **`Row(AddressBar, Indicator)` cannot reproduce today's looks.** The indicator, engine button and find state live inside the address field and negotiate: each gets at most a third of the field, the engine button shows only whole, and the text gets the rest (`Model::address_view` in `ui/sighurt-ui/src/chrome.rs`). The line chrome has its own rules (status up to half, engine a third, find a quarter), and it shapes `[2/5]  NORMAL` as one run. Generic leaf items would need a real flex negotiation and would shift glyphs by sub-pixels.
2. **`StatusBar::new().when_needed()` in a Column is a trap.** A strip that appears on link hover resizes the page view on every hover. "When needed" is only safe for overlay layers.
3. **"The `Chrome` trait stays and a `Custom` component draws with a `Pen`"** would put closures naming `sighurt-render` into the experience. `sighurt-config` depends on `sighurt-api` only, and such closures cannot cross a Wasm boundary in Part 5.
4. **One tree for both frontends does not degrade; it forks.** The window's default is tab strip, toolbar, page, status strip. The terminal's is a summary line, page, command line. One tree would need a frontend condition on every node, which is two trees interleaved.
5. **Independent component objects are the widget-toolkit trap.** Per-component state, event routing and focus is weeks of work. Today there is one editor, one focus, one hover target, and that is correct.
6. **The 8-surface bound has a hidden constraint.** `ChromeTargets::known` in `frontends/sighurt-gui/src/chrome.rs` never forgets a name, and names are `&'static str`. An interpreter that names surfaces per node would exhaust the eight on live reloads. Names must come from a fixed pool.

## 1. The data model (`sighurt_api::ui`, no third-party crates)

```rust
pub struct Interface {            // Experience.ui; core keeps it in Config after TOML ops
    pub window: Node,             // sizes in logical px at a 14 px font, scaled like LayoutOptions::px
    pub terminal: Node,           // sizes in cells
    pub density: Density,         // Normal | Compact (today's `compact`); window only
}

#[non_exhaustive]
pub enum Node {
    Column(Vec<Node>),            // children top to bottom, each the full width
    Row(Vec<Node>),               // children left to right, each the full height
    Overlay { base: Box<Node>, layers: Vec<Layer> },   // layers over base, taking no room
    Leaf(Component),
}

pub struct Layer { pub component: Component, pub anchor: Anchor, pub show: Show }
pub enum Anchor { BottomLeft, BottomRight, TopLeft, TopRight }
pub enum Show { Always, WhenNeeded }   // WhenNeeded: listed only while it has something to say,
                                       // and then takes no pointer input

#[non_exhaustive]
pub enum Component {
    TabBar(TabBar), AddressBar(AddressBar), StatusBar(StatusBar),
    CommandLine(CommandLine), Summary(Summary), PageView(PageView), Label(Label),
}

#[non_exhaustive] pub struct TabBar {
    pub size: Size,               // Auto: 40/32 px high in a Column; min(220, half) wide in a Row
    pub new_tab: bool, pub close_buttons: bool, pub engine_label: bool,
}                                 // strip or list follows the parent: Column = strip, Row = list
#[non_exhaustive] pub struct AddressBar {      // today's toolbar
    pub buttons: Vec<Button>,     // default [Back, Forward, Reload]; `buttons = false` is empty; max 8
    pub indicator: bool, pub engine_button: bool, pub find_state: bool,
}
pub enum Button { Back, Forward, Reload, NewTab }
#[non_exhaustive] pub struct StatusBar {}      // strip in a Column; corner box as an overlay layer
#[non_exhaustive] pub struct CommandLine { pub engine_label: bool }
        // one line that is the address editor and find field when focused. Window: the line
        // chrome's line. Terminal: the bottom line (editor, else notice, else status; find at right)
#[non_exhaustive] pub struct Summary {}        // terminal's top line: [2/5] title — address · engine · MODE · keymap
#[non_exhaustive] pub struct PageView { pub home: HomeStyle }   // Card (standard) | Lines (line chrome)
#[non_exhaustive] pub struct Label { pub size: Size, pub text: LabelText }
pub enum LabelText { Fixed(String), Custom(Custom) }   // keymap's Custom shape: Fn(&dyn Browser) -> Vec<Span>
pub struct Span { pub text: String, pub tone: Tone }   // Tone: Text | Muted | Dim | Error

#[non_exhaustive] pub enum Size { Auto, Fixed(u16), Fill }   // along the parent's axis
```

- **No solver.** A component's natural size is a function of kind, density, font size and orientation, never of its text. A container does one pass: `Auto` and `Fixed` children take `min(size, remaining)` in order, are laid out at full size and clipped (today's `intersect` behaviour), and the `Fill` child gets the rest. Only a `PageView`, or a container holding it, fills.
- **One special rule, taken from `Layout::new`:** a `StatusBar` strip is allocated last and only if what remains is strictly more than its height ("short windows drop the status strip first").
- **Text negotiation stays inside compounds**, as the code is today. Item lists inside a bar (`CommandLine.items`, `Summary.items`) are a later addition the `#[non_exhaustive]` structs leave room for.
- **`Interface::check() -> Vec<String>`**, per tree:
  - depth at most 4, at most 24 nodes, at most 2 overlay layers;
  - exactly one `PageView`;
  - each other kind at most once (`Label` at most 3);
  - exactly one of `AddressBar` or `CommandLine`, so something takes `location.focus`, `page.find` and command errors;
  - something that shows status (`StatusBar` or `CommandLine`), per the contract;
  - `Show::WhenNeeded` only on a layer;
  - at most 7 chrome surfaces by the rule in §2;
  - terminal tree only: a single `Column` of leaves (no `Row`, no `Overlay`), no `AddressBar`, and no `TabBar` in the first version.
  
  An experience with any problem is not used, as with keys.
- **Operations**, the analogue of `bind`/`unbind`, used by the defaults, the SDK and TOML:
  - `place(component, Side)` removes that kind wherever it is and inserts it relative to the `PageView`. Top/Bottom go in the outermost Column. Left/Right go in a Row directly around the `PageView`, made if absent.
  - `remove(Kind)`, `get_mut(Kind)`, `overlay(component, Anchor, Show)`.
- **Presets**: `Interface::standard()`, `Interface::line()`, and the terminal's default.

All three looks as trees:

```text
standard:   Column[TabBar, AddressBar, PageView, StatusBar]
tabs left:  Column[AddressBar, Row[TabBar, PageView], StatusBar]
overlay:    Column[TabBar, AddressBar, Overlay{PageView, [StatusBar @BottomLeft WhenNeeded]}]
line:       Column[CommandLine, PageView{home: Lines}]      (bottom: swap)
terminal:   Column[Summary, PageView, CommandLine]
```

Things that are impossible today become one operation: line chrome plus a tab strip, tabs on the right or at the bottom, a full-height sidebar (`Row[TabBar, Column[AddressBar, PageView, StatusBar]]`), toolbar at the bottom.

**Two trees, recommended.** Units differ and are implicit per tree. The defaults differ fundamentally. The terminal interpreter can stay rows-only. SDK sugar applies an operation to both trees where it is expressible: `sig.ui.tabs.position(Side::Left)` is `window.place(TabBar, Left)`, and Top/Bottom/Hidden apply to both once the terminal has a `TabBar`.

## 2. The interpreters

**Window (`sighurt-ui`).** Keep `Model`, `ChromeState`, `Target`, `hit_test`, `paint.rs` and `TextEditor`. Change only where `Layout` comes from.

- `Layout::from_tree(window, tabs, &Node, density, font_size, first_row)` fills the same `Layout` struct. It gains `surfaces: Vec<(SurfaceName, WindowRect, input)>`, `line: WindowRect`, `tabs_vertical`, `status_overlay`, and per-region `separator: Edge` (the edge facing the page).
- Each compound's internal rectangles are today's code cut out of `Layout::new`, relative to its region's rectangle instead of constants like `chrome.height`.
- Hit testing, focus, IME, completion and damage are unchanged, because each kind exists at most once and `Model` is the single state.
- **Surfaces.** A subtree without the `PageView` is a chrome block. A maximal run of adjacent chrome blocks among a container's children is one surface (always a rectangle).
- **Names** come from a fixed pool, by the run's principal member: `chrome` (has `AddressBar`), `line`, `tabs`, `status`, `extra-1..3`, plus `page`. The default tree gives exactly today's `chrome`, `tabs`, `status`, `page`, so diagnostics and `headless.sh` keep working.
- **Fullscreen, the page drawn by the chrome, and the input-less overlay** become generic rules of `layout_of` in `ui/sighurt-ui/src/standard.rs`.

**The trait and the registry.**

- Keep `Chrome`, `ChromeEvent`, `ChromeAction` and `ChromeLayout`. They are the host contract and the test seam. `Standard` becomes `Composed`, the one implementation.
- Remove the choice among whole chromes: `ChromeKind`, `CHROMES`, `ChromeChecks` and its `check` fn pointer, and `Environment::chromes`. That is about 60 occurrences in 15 files.
- `chrome.<action>` names become a fixed list per frontend: `toggle-tabs` in the window; the terminal's four are already a const in `crates/sighurt-core/src/config.rs`.
- `sighurt-chrome-line` is folded into `sighurt-ui` as the `CommandLine` component (`line.rs`, with its `view` and `paint_line` moved verbatim, reading `Model`'s state). The crate and the `chrome-line` feature are deleted, following decision 116's precedent.

This differs from the keymap precedent deliberately: there the trait was the only seam. Here the trait is the host boundary and stays; only "pick an implementation" goes.

**Custom drawing without `sighurt-render` in the data.**

- First version: `Label` with computed spans. It is pure, re-evaluated only on a snapshot change, works in both frontends, and crosses a module boundary as bytes like key `Custom`s.
- Later, natively: a `Component` trait in `sighurt-ui` (`size`, `handle`, `draw(&mut Pen, rect)`) and a rows trait in `sighurt-tui`, registered by name in each frontend's compile-time list and referenced as `Component::Custom { name, size }`.
- For Part 5: a serializable display list. Do not design it now.

**Terminal (`sighurt-tui`).**

- `ChromeState::draw(&self, grid, &Node) -> PageArea` walks the Column. Each non-page leaf takes one row in tree order while rows remain, and the page gets the rest. This reproduces today's behaviour at 1 and 2 rows.
- `top_line` and `bottom_line` become the `Summary` and `CommandLine` row drawers, unchanged.
- `page_area(size)` in `frontends/sighurt-tui/src/app.rs` (used near lines 802, 808, 2430 and 3326) takes the tree.
- A tree change on reload goes through the resize path.

## 3. Keeping the golden images

**Standard chrome: pixel identity is realistic.** `paint.rs` and `hit_test.rs` read only `Layout`.

1. Keep `Layout::new(options)` as a `#[cfg(test)]` oracle and add `LayoutOptions::tree()`.
2. Property test: over `every_option()`, the sizes in `odd_sizes_keep_everything_inside` and the tab counts, `from_tree(options.tree())` equals the old `Layout` field for field.
3. Replace the reads of `options.tabs == Left` and `options.status == Overlay` in `paint.rs` and `chrome.rs` with the new `Layout` facts.
4. All 33 goldens in `ui/sighurt-ui/tests/golden` stay untouched.

**Where identity does not hold:**

- **New placements** (tabs right or bottom, sidebar, toolbar at the bottom) have no old image and get new goldens. Separator lines must move to the page-facing edge; for the old placements that rule gives today's edges.
- **Line chrome drawing** is identical if the code moves verbatim. Its behaviour unifies with `Model`'s: Ctrl-word movement and deletion are gained, and so is the "line just opened stays" rule, which `Line` lacks. Tests in `line.rs` that pin the old behaviour would need a stated change.
- **Generic items inside bars** would break identity. They are excluded for that reason.

## 4. TOML

Operations applied after the Rust experience, in order; schema version stays 1.

| Setting | Becomes |
|---|---|
| `[ui] chrome = "standard"` | The experience's window tree (the default) |
| `[ui] chrome = "line"` | The line preset, replacing the experience's window tree, as `keymap = "bindings"` does |
| `[chrome.standard] tabs = top\|left\|hidden` | `place(TabBar, side)` / `remove`; `right` and `bottom` come free |
| `[chrome.standard] status = strip\|overlay` | `place(StatusBar, Bottom)` / `overlay(StatusBar, BottomLeft, WhenNeeded)` |
| `[chrome.standard] buttons`, `compact`, `engine-label` | `AddressBar.buttons`, `density`, the `engine_label` fields |
| `[chrome.line] position`, `engine-label` | `place(CommandLine, Top\|Bottom)`, its field |

- **New canonical tables:** `[ui.layout]` (`tabs`, `status`, `address`, `line`, `buttons`, `compact`, `engine-label`) and `[tui.layout]` (`summary = top|bottom|hidden`, `line = top|bottom`). `[chrome.standard]` and `[chrome.line]` stay as aliases.
- **Core's part:** it applies and validates the operations and exposes `Config::interface()`. An invalid result on reload keeps the previous configuration.
- **TOML cannot express** arbitrary trees or `Label`s; those are Rust's.
- **Unchanged:** `[ui] renderer`, `font`, `font-size`, `[ui.colors]`, and the rest of `[tui]`.
- **Live reload:** the fixed name pool makes swapping the tree live safe, so `[ui] chrome` can stop needing a restart.

## 5. Not in the first version

- Split views or more than one `PageView`.
- Item lists inside bars; fill weights.
- Rows, overlays or a `TabBar` in the terminal.
- A native `Component` registry; a display list.
- Unifying `sighurt_ui::Snapshot` with the terminal's `ChromeState`.
- Per-node colours.

**Room left for split views:** `PageView` is a `#[non_exhaustive]` struct that later gains `pane: Focused | Index(n)`; `check` enforces exactly one today; `Size::Fill` gains a weight. The core would then need:

- panes, each showing a tab, one focused;
- `KeyboardFocus::Page` naming a pane;
- visibility per pane;
- "the shown view" in fullscreen, activation, find and the key engine's context becoming "the focused pane's";
- `ChromeLayout::page` becoming a list.

## 6. Risks and owner decisions

**Risks:**

- **Toolkit creep.** Hold the line at compounds and one `Model`.
- **Line fold-in.** It changes behaviour as described in §3. I could not check which of those `line.rs` pins.
- **Terminal reload.** A tree change on reload must reach text-mode relayout, engine metrics and kitty placement through the resize path.
- **Edge assumptions in the tab list.** It uses `chrome.height` as origin and a right-edge border; new placements need the region rectangle.
- **More GPU surfaces.** Each is another swapchain; the total is bounded by the 7-run check.

**Decisions:**

1. **One tree or two.** Recommend two (`ui.window`, `ui.terminal`), one vocabulary, shared operations.
2. **Trait and registry.** Recommend keeping the `Chrome` trait as the host seam; removing `ChromeKind`, `CHROMES` and `ChromeChecks`; folding `sighurt-chrome-line` in and deleting the crate and feature; `[ui] chrome` naming a preset. Fallback if time runs out: `line` stays a legacy kind for one release.
3. **Granularity.** Recommend compound components now, with `Label` as the only custom primitive. Bar item lists and Pen-level components wait for their own design. The alternative, generic items now, costs the goldens and about a week.

## 7. Staged plan

Estimates are rough line counts from reading, not measured.

| Stage | Work | Test | Size |
|---|---|---|---|
| 0 | `crates/sighurt-api/src/ui.rs`: types, `check`, operations, presets | Unit tests: presets pass `check`; each violation is reported; `place`/`remove` round-trips; a fuzz-lite case that operations never yield an unchecked tree | ~450 + 250 test lines |
| 1 | `ui/sighurt-ui/src/layout.rs`: `from_tree`, old `new` kept as test oracle; `chrome.rs`, `paint.rs`, `standard.rs` read the new facts | The equivalence property test; all standard goldens unchanged; `tests/extension.rs` passes | ~+400/−100, 4 files |
| 2 | Generic surfaces, name pool, fullscreen, separators; new placements | Surface lists for the option combinations equal today's `layout_of`; new goldens (tabs right, tabs bottom, sidebar); `ui/sighurt-ui/tests/headless.sh` | ~250 |
| 3 | Core `config.rs`/`state.rs` (operations from TOML, `Config::interface`, chrome command lists, `ChromeChecks` out); `sighurt-runtime` `registry.rs`; `sdk/sighurt-config` (`ui` builder, defaults through it); `sighurt-gui` `chrome.rs`/`app.rs`; `default-config.toml` | `[chrome.standard] tabs = "left"` and the SDK call give the same `Interface`; an invalid tree falls back with a notice; `apps/sighurt-browser/tests/headless.sh` | ~550, 8 files |
| 4 | Terminal: `chrome.rs` draw by tree, `page_area(tree)`, `[tui.layout]` | Existing chrome and pty tests unchanged for the default tree; new ones for summary at the bottom or hidden, at 1, 2 and 24 rows; `apps/sighurt-browser/tests/tui.rs` | ~250, 2 files |
| 5 | Fold the line chrome in as `ui/sighurt-ui/src/line.rs`; delete the crate and feature; move its goldens and tests | Its 14 goldens pixel for pixel; behaviour tests ported with each change named | ~+450/−1100 |
| 6 | `Label` in both frontends; CLAUDE.md, README, DOCS, the extension-api changelog, STATUS decisions | A golden and a grid test for a label; cross-check lines clean | ~200 plus docs |

Stages 0–4 are two days and deliver the standard chrome and the terminal on trees with every old option an operation. Stages 5–6 are a third day. In total about 2,500 changed lines over about 25 files.

## Review and rulings (16 November 2026)

GPT-6 Astra reviewed the design above (`reviews-interface-astra.md`, 13 findings). The rulings
below are binding for the implementation and override the text above where they differ.

1. **Mandatory parts keep their room** (finding 1). A `Fixed` size is clamped so that the
   editor (`AddressBar` or `CommandLine`) and whatever shows status keep their natural size.
   Two exceptions that exist today are kept and stated: a window too small for the default
   tree clips as it does now (pixel identity), and the terminal's line shows the editor
   before a notice while it is being edited.
2. **Layers are passive** (finding 2). Only `StatusBar` and `Label` may be layers; never the
   `PageView`, the editor or the `TabBar`. `check` rejects the rest.
3. **A component-to-surface ownership map** (findings 3 and 8) is compiled from the tree and
   is the one source for drawing dispatch, damage, the text field's surface and local
   coordinates, and hit testing. The seven chrome names are slots, the legacy names
   (`chrome`, `tabs`, `status`, `line`) preferences; `check` fails a tree that cannot be
   assigned. A surface is cleared once; component painters draw only their regions.
4. **The line chrome is not folded in for this version** (findings 4, 5, 12, 13). Stage 5 is
   deferred: `sighurt-chrome-line`, `ChromeKind`, `CHROMES` and `[ui] chrome` stay, so
   `chrome = "line"` selects the existing crate and native chromes remain the low-level
   path. `chrome = "standard"` is the tree's interpreter. `CommandLine` in the window tree is
   rejected by `check` until the line is ported as a complete slice with a presentation
   interface (editor geometry, hit positions, the engine button while editing).
5. **Legacy tables keep their meaning** (finding 5): `[chrome.standard]` applies only while
   `chrome = "standard"`, as today; the canonical `[ui.layout]` and `[tui.layout]` apply
   after it in a fixed, documented order that does not depend on the file's key order; an
   operation that replaces the editor is atomic. The result is validated once, at the end.
   *Amended 16 November 2026*: `[chrome.standard]` applies whatever chrome is named. The
   first reading broke a reload that names another chrome: the standard chrome runs until
   the next start and lost its table's layout at once (engines.sh, "the standard chrome
   stays meanwhile"). Only the standard chrome reads the window's tree, so nothing else sees it.
6. **The grammar is what is implemented** (finding 6). `Size` is `Auto | Fixed(u16)`; only
   the branch holding the `PageView` fills. `check` rejects every component and parent
   combination no interpreter lays out (a bar inside a `Row`, a `Fixed` label in the
   terminal, and so on), each with a message naming the node. Containers measure
   recursively; fixed siblings after the filling child are reserved before it.
7. **Buttons**: `Back`, `Forward`, `Reload`, each at most once (finding 7).
8. **Pixel identity is a compatibility implementation, not a by-product** (finding 9): the
   default tree keeps the legacy surface partition, measurement order and nominal (unclipped)
   origins; the equivalence test covers the full option matrix, font sizes 12 to 24, scroll
   positions and tiny windows, and compares surface plans and events as well as rectangles.
9. **`Label`** (finding 10) is evaluated by the frontend with the `Browser` view whenever the
   core says the chrome may have changed; its output is contained and bounded (8 spans, 256
   characters), and it repaints only when the spans differ. It stays in stage 6.
10. **Identities for later** (finding 11): `PageView` carries a slot (`PageSlot::MAIN`, the
    only one), page placements are compiled as a list of one, and a `Label` has an `id`;
    operations on kinds stay for the kinds that exist once.
11. **Order** (finding 13): the grammar and the compiled geometry and ownership first; the
    standard chrome through it; then the configuration operations and reload; then the
    terminal, with one pure layout result for drawing, page metrics and input mapping and
    one complete transition path when the tree changes at an unchanged terminal size. The
    tests the review lists are acceptance tests. The estimate above is withdrawn.
