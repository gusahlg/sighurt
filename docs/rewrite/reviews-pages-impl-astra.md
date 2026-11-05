1. **High — Auto-repeat can bypass the input hold and send keys to an unintended tab.**  
   [crates/sighurt-core/src/state.rs:2911](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:2911), [state.rs:3033](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:3033)

   `KeyOutcome::Run` unconditionally releases the hold before executing commands. Repeats of consumed bindings still produce `Run`; `page.send-key` also bypasses the new destination tracking. Hold Enter in a page-opened tab in NORMAL mode, then let that tab close itself: the example keymap repeats its cached `page.send-key enter`, clears the hold, grants activation and sends Enter to the newly exposed page. This violates A1.

   There is a related gap for keys initially delivered to chrome: they have no `pressed` entry, and `is_none_or` permits their repeats into a newly opened page—for example, holding Enter while submitting a command that opens or reopens a tab.

   **Smallest fix:** track physical press ownership before keymap dispatch, including chrome and bindings that synthesize page input. Suppress their page delivery across destination changes until release; repeats must not count as deliberate input reacquisition.

2. **High — ConfigWrite and PtyOpen are allowed while the browser window is unfocused.**  
   [crates/sighurt-core/src/state.rs:2744](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:2744)

   The authorization check uses the selected tab and `focused_view()`, but that helper checks only logical `KeyboardFocus::Page`. Window deactivation sets `window_focused = false` without changing that logical focus or clearing activation.

   Click an internal page, switch to another application, then have its queued request arrive within five seconds: ConfigWrite or enabled PtyOpen is still authorized. A5 requires the selected, focused document.

   **Smallest fix:** include `self.window_focused()` in this authorization predicate, before consuming activation.

3. **Medium — Requests and cancellations can remain queued indefinitely without a repaint.**  
   [engines/sighurt-engine-wasm/src/adapter.rs:346](/home/gusahlg/repos/sighurt/engines/sighurt-engine-wasm/src/adapter.rs:346), [view.rs:662](/home/gusahlg/repos/sighurt/engines/sighurt-engine-wasm/src/view.rs:662)

   The worker’s notification check includes navigation and downloads, but not pending `Internal` requests, PTY opens or withdrawals. Those queues reach the core only through `after_run`, following a reported step.

   An internal guest that makes a request on its second frame and keeps drawing identical content can poll forever without the core receiving it. Likewise, cancelling an already reported request or closing a PTY without changing the frame can leave cancellation unreported.

   **Smallest fix:** expose whether `Internal` has unreported work and include it in the worker’s `asks` predicate. Include withdrawals as well as new requests.

4. **Medium — A guest trap retains a taken PTY master until the view is closed or successfully reloaded.**  
   [engines/sighurt-engine-wasm/src/view.rs:646](/home/gusahlg/repos/sighurt/engines/sighurt-engine-wasm/src/view.rs:646), [crates/sighurt-core/src/state.rs:3313](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:3313)

   On a failed step, the engine drops `self.live` but retains `self.page.internal`, including `Terminal.pty` and its `OwnedFd`. The core clears the document and cancels its PTY; consequently, the eventual `PtyEnded` reply fails `is_document` and never reaches the engine to release that master.

   This can be triggered through the implemented transfer API: deliver a successful hand-over, then trap before the guest reads or closes it. Even after teardown completes, the failed view still owns the descriptor. This concerns the implemented lifecycle glue, independently of the excluded spawn/data-plane stubs.

   **Smallest fix:** explicitly revoke and clear internal document resources when a step fails, before reporting `ViewFailed`; do not depend on a subsequent service reply to release them.

5. **Medium — Invalid terminal commands close the editor and return focus to the page, contrary to A9.**  
   [frontends/sighurt-tui/src/app.rs:2449](/home/gusahlg/repos/sighurt/frontends/sighurt-tui/src/app.rs:2449)

   `submit` closes the address editor and queues page focus before parsing. Entering `:hlep` or `:help unexpected` therefore discards the editable command and returns subsequent typing to the page. This retains pre-existing TUI behavior, but does not implement A9’s explicit requirement that command errors leave the line open.

   **Smallest fix:** parse first; close the editor and transfer focus only on success. Display parse errors while preserving the editor and its text.

**Checked and found correct in the inspected paths:**

- Ordinary HTTP/file Wasm documents cannot link `sig`; downloaded embedded guests gain no privilege. Loaded modules and workers explicitly clear internal identity.
- Core routing and engine loading preserve exact per-view page identity. Non-internal `OpenRequested` and `NavigationRequested` cannot open `sig:` addresses; explicit engine selection cannot bypass this. Restore reclassifies internal addresses, and cross-page `push_state` fails origin checks.
- Page grants use the core’s recorded identity. Background-tab writes/opens are denied; successful privileged authorization consumes activation. Request IDs and document checks prevent normal replies from reaching replacement documents.
- `sig` memory accesses use checked helpers; request payloads, request counts, PTY transfers and retained input queues have bounds. No out-of-bounds host memory access was found.
- The hand-over registry moves `OwnedFd` ownership under a mutex; take removes it and forget drops an untaken master. Guests cannot supply native session IDs.
- Alias names, arguments and expansions are validated at load; recursion, built-in shadowing and expansion beyond eight commands are rejected.
- The additions preserve core/no-I/O separation, configuration-owned bindings and event-based engine interaction.

Static review at `f0def28`; no files changed and no builds or tests run.