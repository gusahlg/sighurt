# Outside review of the interface design (GPT-6 Astra, 16 November 2026)

Reviewed: `interface-2026-11-16.md` as first written (58f96d0). The rulings on each finding are in that document, "Review and rulings".

1. **P1 — `check()` proves that required components exist, not that they remain usable.**  
   [interface-2026-11-16.md:81](../../docs/rewrite/interface-2026-11-16.md:81)

   This passes the listed checks:

   ```text
   Column[TabBar{size: Fixed(600)}, AddressBar, PageView, StatusBar]
   ```

   At 800×600 with the default font, the tabs consume the entire height. The editor and status have no visible area. `location.focus` changes logical focus to an invisible editor. Fullscreen does not rescue it: the existing fullscreen adapter changes the **engine page rectangle**, while retaining the chrome’s ordinary geometry ([standard.rs:111](../../ui/sighurt-ui/src/standard.rs:111)). Revealing those surfaces still reveals no editor or notice.

   There is also an existing exception the proposal glosses over: terminal `Column[Summary, PageView, CommandLine]` hides notices while editing, because [chrome.rs:540](../../frontends/sighurt-tui/src/chrome.rs:540) returns the editor before considering notices. At one row, the current implementation displays only the summary.

   **Fix:** define runtime priority for mandatory UI, not just structural checks. Reserve usable editor/status space before optional components, or provide a fallback presentation when ordinary placement starves them. Explicitly decide the tiny-window and terminal-editing exceptions; unchanged legacy behavior and an unconditional “status always visible” guarantee are incompatible.

2. **P1 — Layers admit trees the host cannot represent, and interactive overlays break existing hit testing.**  
   [interface-2026-11-16.md:48](../../docs/rewrite/interface-2026-11-16.md:48)

   This passes the listed checks:

   ```text
   Overlay {
     base: Column[AddressBar, StatusBar],
     layers: [PageView @BottomLeft Always]
   }
   ```

   The page is a layer above the chrome base, but the host contract puts engine views **below all chrome surfaces** ([extension.rs:14](../../ui/sighurt-ui/src/extension.rs:14)). Its allocation is also undefined: the only `PageView` is outside normal flow.

   A second accepted tree exposes the input problem:

   ```text
   Overlay {
     base: Column[TabBar, PageView, StatusBar],
     layers: [AddressBar @TopLeft Always]
   }
   ```

   With ordinary toolbar geometry, its address field overlaps the top tabs. A click at `(300, 20)` can hit a covered tab instead of the visible editor: [hit_test.rs:37](../../ui/sighurt-ui/src/hit_test.rs:37) checks every tab before the address field, while [chrome.rs:650](../../ui/sighurt-ui/src/chrome.rs:650) discards the receiving surface’s identity after translating coordinates.

   Changing that layer to `WhenNeeded` introduces another accepted failure: the specification explicitly removes its pointer input.

   **Fix:** initially restrict layers to explicitly supported passive components, forbid `PageView` in layers, and require the editor in normal flow. Supporting interactive layers requires hit testing by receiving surface and stacking order. Component uniqueness does not solve this.

3. **P1 — Surface grouping requires new paint, damage and text-field ownership. These cannot remain unchanged.**  
   [interface-2026-11-16.md:116](../../docs/rewrite/interface-2026-11-16.md:116)

   Consider:

   ```text
   Column[AddressBar, PageView, TabBar, StatusBar]
   ```

   The bottom tabs and status form one surface. Whichever component supplies its name, the existing code cannot correctly service both: [paint.rs:73](../../ui/sighurt-ui/src/paint.rs:73) dispatches drawing by fixed surface name, and [chrome.rs:248](../../ui/sighurt-ui/src/chrome.rs:248) converts damage directly to those names. A status-only change requests `status`; if the combined surface is `tabs`, the adapter filters that repaint out.

   The line preset exposes the focus failure directly:

   ```text
   Column[CommandLine, PageView]
   ```

   [chrome.rs:1226](../../ui/sighurt-ui/src/chrome.rs:1226) reports `TextField::new(CHROME, …)`, and its caret coordinates are relative to `layout.chrome`. The actual surface is `line`. On a fresh start, IME has no matching target; after switching from standard chrome, the GUI can prefer the retained, hidden `chrome` target ([app.rs:582](../../frontends/sighurt-gui/src/app.rs:582), [app.rs:768](../../frontends/sighurt-gui/src/app.rs:768)).

   **Fix:** compile a component-to-surface ownership map. Record damage by component, translate it through that map, and draw the assigned components for each surface. Derive text-field surface and local coordinates from the same map; include surface identity in text-field change detection. Keep one interaction model if desired, but remove its hard-coded surface ownership.

4. **P1 — Folding the line chrome in “verbatim” loses both drawing and interaction behavior.**  
   [interface-2026-11-16.md:130](../../docs/rewrite/interface-2026-11-16.md:130)

   For the advertised combination:

   ```text
   Column[TabBar, CommandLine, PageView]
   ```

   tabs and line share a surface. Calling the existing `paint_line` after painting the tabs erases them: it calls `Pen::clear` ([lib.rs:711](../../extensions/sighurt-chrome-line/src/lib.rs:711)), which explicitly discards previous drawing ([pen.rs:53](../../render/sighurt-render/src/pen.rs:53)). Its geometry helper also assumes a line at the window’s top or bottom, not below another component.

   More seriously, `Model::finish` computes scrolling and IME geometry using `address_view`, and pointer placement uses that same presentation. The line’s prefix, status reservation and engine button produce different text coordinates. Moving its painter without moving those calculations gives a caret and hit positions that disagree with the pixels.

   There is a pinned behavioral difference beyond the two listed in the proposal: the line’s engine button remains clickable **while editing** ([line.rs:921](../../extensions/sighurt-chrome-line/tests/line.rs:921)); the standard model suppresses its engine button while focused ([chrome.rs:1289](../../ui/sighurt-ui/src/chrome.rs:1289)).

   **Fix:** extract a presentation interface that supplies editor geometry, hit positions and engine-button behavior to the shared interaction state. Clear once per surface; component painters only draw their regions. Port the line as an early complete slice, with its interaction tests, before deleting its implementation.

5. **P1 — Applying legacy TOML tables as unconditional operations breaks valid configurations.**  
   [interface-2026-11-16.md:164](../../docs/rewrite/interface-2026-11-16.md:164)

   A valid existing configuration can contain:

   ```toml
   [ui]
   chrome = "standard"

   [chrome.standard]
   tabs = "left"

   [chrome.line]
   position = "bottom"
   ```

   Today only the selected chrome receives its settings ([chrome.rs:65](../../frontends/sighurt-gui/src/chrome.rs:65)). Applying the proposed aliases literally inserts a `CommandLine` into the standard tree without removing `AddressBar`; validation then rejects a previously valid file.

   “In order” also needs an explicit precedence definition. These are tables, not an operation log; the shared settings representation is a `BTreeMap` ([settings.rs:33](../../crates/sighurt-api/src/settings.rs:33)). Canonical and legacy tables can specify conflicting placements or engine-label values.

   **Fix:** preserve selected-preset semantics for legacy tables, then apply canonical overrides in a documented order independent of TOML key order. Make editor replacement an explicit atomic operation. Test files containing both legacy tables, alias/canonical conflicts, and repeated reloads.

6. **P2 — The accepted grammar is broader than the sizing and presentation rules.**  
   [interface-2026-11-16.md:59](../../docs/rewrite/interface-2026-11-16.md:59)

   These satisfy the enumerated checks but have contradictory or missing interpretation:

   - `Column[Label{size: Fill}, AddressBar, PageView, StatusBar]`: two fill candidates, despite the rule that only the page-containing branch fills.
   - `Row[AddressBar, PageView, StatusBar]`: no natural width or vertical presentation is specified for either bar.
   - Terminal `Column[Label{size: Fixed(3)}, PageView, CommandLine]`: `Size` promises three cells along the parent axis, while the terminal algorithm assigns every non-page leaf exactly one row.

   Chrome-only nested containers also need intrinsic sizes. “A component’s natural size” does not define the natural size of a `Row` inside a `Column`. The allocation algorithm must reserve fixed siblings after a fill child; an ordinary forward allocation would consume the space before reaching a bottom `CommandLine`.

   **Fix:** specify supported component/parent combinations, recursive container measurement, and allocation order. Reject unsupported combinations. Put sizing on child placements, or remove unsupported `Size` variants. Currently `Fill` is exposed on `TabBar` and `Label`, while neither `PageView` nor containers have a size field—the opposite of where future fill weights are needed.

7. **P2 — The proposed toolbar already exceeds the representational capacity of the retained `Layout` and `Target`.**  
   [interface-2026-11-16.md:64](../../docs/rewrite/interface-2026-11-16.md:64)

   This accepted tree needs multiple instances of the same control:

   ```text
   Column[
     TabBar{new_tab: true},
     AddressBar{buttons: [Back, Back, NewTab]},
     PageView,
     StatusBar
   ]
   ```

   `Layout` has one `back` rectangle and one `new_tab` rectangle ([layout.rs:145](../../ui/sighurt-ui/src/layout.rs:145)). The toolbar painter only knows Back, Forward and Reload ([paint.rs:226](../../ui/sighurt-ui/src/paint.rs:226)). Even after adding rectangles, hover keyed solely by `Target::Back` cannot distinguish the two Back buttons.

   **Fix:** use a small list of placed controls with instance identity, action and owning component. Alternatively, explicitly restrict the first version’s button vocabulary and reject duplicates—but the document must stop promising an arbitrary eight-button list with unchanged layout and targets.

8. **P2 — Eight names are sufficient, but the proposed role-based naming rule is not a complete allocator.**  
   [interface-2026-11-16.md:121](../../docs/rewrite/interface-2026-11-16.md:121)

   Let `L1`, `L2`, `L3` be small fixed-size labels:

   ```text
   Column[
     L1,
     Row[L2, Column[Summary, PageView, AddressBar, StatusBar], L3],
     TabBar
   ]
   ```

   This has depth four and six chrome runs. Four runs contain only `L1`, `L2`, `L3`, or `Summary`; the other two contain tabs and address/status. Under the stated role-based assignment, four runs need anonymous names, but only `extra-1..3` exist. There are unused named slots, but borrowing them is unspecified.

   **Fix:** treat all seven chrome names as allocatable slots, with legacy names as preferences, and use the ownership map from finding 3. Check the resulting assignment for uniqueness and count, including layers and the chrome-drawn page.

   The fixed eight-name pool itself correctly prevents lifetime exhaustion across reloads. There is no honest ninth-name counterexample if that pool is enforced. The gap is assigning accepted trees to it, not the arithmetic of seven plus one.

9. **P2 — Standard pixel identity is achievable, but the proposed proof is insufficient and “by construction” is false.**  
   [interface-2026-11-16.md:149](../../docs/rewrite/interface-2026-11-16.md:149)

   Three concrete traps matter:

   - **Clipped geometry retains nominal origins.** At height 39, the old tab strip is 39 high, but `toolbar.y` remains 40. At height 50, toolbar contents are positioned using the full 44-pixel toolbar and then intersected; the address starts at `y=46` and has four visible pixels. Advancing by clipped sizes or centering inside the allocated ten-pixel toolbar changes the result ([layout.rs:193](../../ui/sighurt-ui/src/layout.rs:193), [layout.rs:306](../../ui/sighurt-ui/src/layout.rs:306)). Empty-rectangle origins also mean literal field equality is stronger than visual equality.
   - **Surface partition affects rasterization.** At font size 18, the standard top surface is 108 logical pixels high. At 125%, that maps to 135 pixels. Splitting its 51- and 57-pixel regions into separate surfaces gives different rounded buffer ratios. Glyph origins and paths use those ratios ([scene.rs:387](../../render/sighurt-render/src/scene.rs:387), [scene.rs:484](../../render/sighurt-render/src/scene.rs:484)). Equal widget rectangles alone do not preserve pixels.
   - **`every_option()` is not every option.** It tests only `(buttons=true, compact=false)` and `(buttons=false, compact=true)`, always enables engine labels, and uses only font sizes 14 and 18 ([layout.rs:415](../../ui/sighurt-ui/src/layout.rs:415)). The cited odd-size loop always uses `first_row=0`. For example, at font size 12, independently rounded tab height plus gap is `27+3=30`; rounding their combined nominal pitch gives 31.

   Also, `paint.rs` does not read only `Layout`: it calls `address_view`, `status_line` and `page_layout`. Independent component flags must reach those computations.

   **Fix:** preserve legacy surface partition, measurement order, full-versus-clipped geometry and presentation calculations. Compare legacy geometry, surface plans and event behavior, then retain the goldens. Expand the option matrix, font sizes and scroll positions. No generic flex solver is necessary, but this is a deliberate compatibility implementation.

10. **P2 — Computed labels have neither a sufficient invalidation input nor a defined module boundary.**  
    [interface-2026-11-16.md:136](../../docs/rewrite/interface-2026-11-16.md:136)

    A useful label displaying `Browser::tab(active).zoom` is already problematic: `Browser` exposes zoom ([keymap.rs:127](../../crates/sighurt-api/src/keymap.rs:127)), but the GUI snapshot does not ([state.rs:23](../../ui/sighurt-ui/src/state.rs:23)). A zoom command invalidates chrome, yet the resulting existing snapshot can be identical. Re-evaluation only when that snapshot changes leaves the label stale. Focus/typing-dependent labels expose the same mismatch.

    “Crosses a module boundary as bytes like key Customs” skips the actual mechanism. Current key Customs contain an `Arc<dyn Fn>` ([keys.rs:286](../../crates/sighurt-api/src/keymap/keys.rs:286)); that object is not the wire representation. Node-count limits also do not bound a label’s string size, returned span count or evaluation cost.

    **Fix:** establish a label evaluator with explicit input/invalidation semantics, invoked where the necessary browser state exists; pass evaluated spans to each presentation. Bound and sanitize output. Use a typed callback reference with module generation for the future wire form, with native closures behind the evaluator today. Version the wire format; `#[non_exhaustive]` does not provide serialization compatibility.

11. **P2 — The split-view escape plan preserves the wrong identities.**  
    [interface-2026-11-16.md:190](../../docs/rewrite/interface-2026-11-16.md:190)

    `Focused | Index(n)` is not enough. In a future tree:

    ```text
    Row[PageView{pane: Focused}, PageView{pane: Index(1)}]
    ```

    focusing pane 1 binds both regions to the same pane. One native view target cannot simply occupy both rectangles. Removing or reordering panes also changes what an index identifies. The existing tab contract deliberately avoids this class of mistake by using `TabId`.

    Kind-based operations cause trouble even before splits: with three labels, `get_mut(Label)` is ambiguous and `place(Label, …)` cannot identify which label to move. Later, per-pane address bars require both component identity and a command target. Seven chrome surfaces plus a single reserved `page` name also leaves no account of two independently drawn home/error panes.

    **Fix:** introduce stable component IDs and page-slot identities now, with one default page slot. Compile page placements as an identified collection, even if its length is currently one. Keep kind-based SDK methods as convenience operations on default roles. Budget surfaces from actual page placements rather than a permanently baked-in “seven plus one.” Full pane state can still wait.

12. **P2 — Section 6’s decisions need one substantive change: preserve a usable low-level extension path.**  
    [interface-2026-11-16.md:210](../../docs/rewrite/interface-2026-11-16.md:210)

    My decisions would be:

    - **Two trees: yes.** Keep shared concepts, but validate frontend-specific capabilities explicitly. The terminal’s restricted grammar should be a checked contract.
    - **Keep `Chrome`, remove user-facing whole-chrome selection: yes, after the line migration is proven.** Retain a small native factory/injection hook. A public trait with no construction route is not a usable customization seam.
    - **Compound components: yes. `Label` as the entire customization escape hatch: no.** A configuration wanting a custom graphic cannot express it after the registry is removed and native components are deferred. That fails the stated requirement that lower-level drawing remain reachable ([interface-2026-11-16.md:7](../../docs/rewrite/interface-2026-11-16.md:7)).

    **Fix:** keep native drawing available through the frontend hook, or implement a minimal named native-component reference backed by a frontend registry. Neither requires embedding renderer closures in `sighurt-api`, designing a Wasm display list now, or introducing independently stateful widgets.

13. **P2 — The staged plan removes dependencies before their replacements exist and substantially understates the risky work.**  
    [interface-2026-11-16.md:220](../../docs/rewrite/interface-2026-11-16.md:220)

    Stage 3 removes chrome registration and introduces the line preset; the window `CommandLine` implementation does not arrive until stage 5. Stage 0 accepts labels before either interpreter supports them. Stage 1’s compatibility claim also depends on the surface ownership work deferred to stage 2. These need explicit temporary restrictions or a different order.

    **Fix the order:** first establish the accepted grammar and compiled geometry/ownership representation; then prove standard and line presentations through it; then enable configuration operations and reload; finally remove legacy implementations. Keep each intermediate release accepting only implemented components.

    The missing acceptance tests are substantive:

    - Every counterexample above, plus visibility, input reachability and unique surface ownership for accepted trees.
    - Status-only and tab-only updates after components share a surface; unchanged snapshots must still produce no repaint.
    - Focused standard↔line reload with selection, preedit, pending completion/paste, and correct IME target migration.
    - Fullscreen entry, focus reveal, notice reveal and hide, without changing the engine’s fullscreen viewport.
    - Repeated layout swaps through the host’s lifetime name cache, including previously hidden surfaces.
    - Terminal layout changes at **unchanged terminal size**, including moving the page origin without changing its dimensions. Exercise text reflow, home links, cursor placement, kitty/sixel output and queued output during reload.

    The terminal’s actual resize path discards queued output before relayout and redraw ([app.rs:1265](../../frontends/sighurt-tui/src/app.rs:1265)); ordinary settings adoption merely calls `relayout` ([app.rs:2242](../../frontends/sighurt-tui/src/app.rs:2242)). Reusing the latter is not equivalent. Use one pure terminal layout result for drawing, page metrics and input mapping, plus one complete layout-transition path.

    Finally, “operations never yield an unchecked tree” is the wrong property: `remove(StatusBar)` necessarily makes an address-bar tree invalid, and replacing editors can require an invalid intermediate state. Test transactional application and final validation instead.

    Stages 1–2 are a layout, rendering-dispatch and ownership refactor; stage 4 is viewport lifecycle work; stage 5 is an editor-presentation integration. The two-day/three-day estimate is not supported by the actual dependencies. Re-estimate after one complete standard/line migration slice passes the compatibility checks.