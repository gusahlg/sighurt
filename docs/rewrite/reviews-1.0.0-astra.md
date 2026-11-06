1. **high — Repeated and synthetic keys can authorize a program launch without a fresh press in that page.**  
   [state.rs:3073](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:3073), [state.rs:3001](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:3001), [app.rs:159](/home/gusahlg/repos/sighurt/pages/sig-page-terminal/src/app.rs:159).

   Delivered repeats refresh activation; `key_ran` also grants activation for synthesized `page.send-key`. The guest’s `is_key("Enter", 0)` accepts repeats. Holding Enter while submitting `:wq` can therefore restart the editor after it exits. Separately, a key-bound alias containing `tab.select 2` followed by `page.send-key enter` can start an already-loaded config page from a key pressed in another tab.

   **Smallest fix:** mint launch activation only from a fresh physical press whose original tab/view/document is the destination. Preserve repeat provenance through `page.send-key`, and require a fresh press on the guest’s start screen. This also prevents synthesized repeats bypassing home’s `key.repeat` guard.

2. **medium — The terminal frontend can start and accept input into an editor it cannot display.**  
   [app.rs:1725](/home/gusahlg/repos/sighurt/frontends/sighurt-tui/src/app.rs:1725), [app.rs:2953](/home/gusahlg/repos/sighurt/frontends/sighurt-tui/src/app.rs:2953).

   The “pages need graphics” and “no text mode” messages only replace drawing. `page_input` returns `false` for Wasm because it has no semantic snapshot, allowing input through to the engine.

   Open `sig://config` in text mode or a terminal without graphics, then press Enter with page focus. The default-enabled editor starts behind the placeholder; subsequent INSERT input reaches that invisible program.

   **Smallest fix:** suppress internal-page key, synthesized-key and text delivery while displaying these placeholders, and make that state ineligible for PTY launch activation.

3. **medium — Home’s keyboard interaction bypasses the involuntary-tab input hold.**  
   [state.rs:2992](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:2992), [app.rs:2764](/home/gusahlg/repos/sighurt/frontends/sighurt-tui/src/app.rs:2764).

   The hold suppresses typing only in the `Some(view)` branch. Home has no view, so passed keys reach `ChromeInput` and its new link handler.

   A page-opened tab can close itself and expose home while the example keymap remains in INSERT. Despite the hold notice, ordinary Tab and Enter presses can select and open home links. The resulting `BrowserEvent::Command(PageOpen)` also clears the hold.

   **Smallest fix:** apply the hold to keyboard input targeting chrome-drawn pages too, while preserving access to the chrome’s address editor.

4. **medium — An editor’s completion can leave its configuration reload pending indefinitely.**  
   [session.rs:691](/home/gusahlg/repos/sighurt/crates/sighurt-runtime/src/session.rs:691), [GUI app.rs:987](/home/gusahlg/repos/sighurt/frontends/sighurt-gui/src/app.rs:987), [TUI app.rs:816](/home/gusahlg/repos/sighurt/frontends/sighurt-tui/src/app.rs:816).

   Both frontends consume `config_edited` before entering their work loop. `service_completions` sets it inside that loop without scheduling another outer iteration. After draining the completion, the loop can become idle.

   For example, edit a symlink target, leave config for home, and let cancellation finish after other work has settled. The file watch misses the target, the config guest is gone, and the reload waits for unrelated input or another wake.

   **Smallest fix:** consume the flag inside the work loop after service completions, or explicitly wake the next iteration when setting it.

5. **low — Editor fallback ignores valid relative and empty PATH entries.**  
   [pty.rs:146](/home/gusahlg/repos/sighurt/crates/sighurt-runtime/src/pty.rs:146).

   `.filter(|dir| dir.is_absolute())` silently discards relative directories and empty entries representing the current directory. With VISUAL/EDITOR unset and `PATH=.` containing an executable `nvim`, config reports “No editor found.” With additional absolute entries, it can choose a different editor instead.

   **Smallest fix:** choose an explicit resolution directory, resolve relative and empty PATH entries against it, and search the resulting absolute paths.

6. **low — Home does not refresh when asynchronously loaded history arrives.**  
   [session.rs:715](/home/gusahlg/repos/sighurt/crates/sighurt-runtime/src/session.rs:715), [GUI app.rs:1225](/home/gusahlg/repos/sighurt/frontends/sighurt-gui/src/app.rs:1225).

   `history.poll()` changes the history revision but requests no frontend redraw. Home’s revision-sensitive cache is consulted only during a later snapshot/draw.

   If home renders before the history reader finishes, “Recently visited” remains absent until another action dirties the chrome. Both frontends are affected.

   **Smallest fix:** request a frontend redraw when `history.poll()` successfully installs history.

7. **low — The editor page omits configuration read failures from its problem list.**  
   [page_info.rs:155](/home/gusahlg/repos/sighurt/crates/sighurt-runtime/src/page_info.rs:155).

   `Info(Editor)` reports only `LoadError::Config`; it discards `LoadError::Other`. An existing file containing invalid UTF-8, or one that becomes unreadable, produces no page-level problem even though the frontend rejects the reload.

   **Smallest fix:** include read failures as `problem` records, retaining the deliberate “new file” handling for a missing file.

**Checked and found correct:**

- Exact registry identity and grants prevent config from requesting Shell, terminal from requesting Editor, and help or ordinary guests from receiving `Info(Editor)`. Replies remain document-bound; removed request kinds fail closed.
- Restore and page navigation do not confer internal privileges. I found no web-page path to the terminal/config site tables through cross-origin `pushState`, redirects or data/blob/about addresses. Both default INSERT exit bindings are handled before page delivery. Specificity ties do not create ambiguity between distinct simultaneously matching patterns.
- Spawn uses argv without an implicit shell; the configuration path remains a separate `OsString` argument, including non-UTF-8 paths. Descriptor sealing, child signal setup and environment scrubbing remain intact. New directories use 0700 without chmodding existing directories.
- Invalid reloads retain the active configuration. Editor completion is distinguished from shell completion; no reload loop was found.
- Recent links originate from canonical, bounded HTTP(S) history entries without userinfo. Both frontends construct `PageOpen` directly from the exact URL. Titles are sanitized before terminal drawing; GUI containment and image-retirement/redraw paths showed no additional defect.
- The changes preserve core/no-I/O separation, configuration-owned bindings and the inspected guest request bounds.

Static review of `afa57e3..37fb81b`; no files changed and no builds or tests run.