The PTY split is sound **as an ownership boundary**, but it does not contain a compromised terminal guest: permission to write to a shell’s PTY is effectively permission to execute commands as the user. `Shell | EditConfig` prevents arbitrary spawn parameters; neither a shell nor an editor such as nvim is restricted to editing the configuration. The terminal guest therefore remains trusted code. The proposed documentation should say this explicitly. [Design](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:63), [security contract](/home/gusahlg/repos/sighurt/CLAUDE.md:275).

Findings below are ranked by severity. They identify design gaps, not demonstrated exploits in an implementation of these pages.

1. **High — The 500 ms guard does not establish the promised input boundary.**

   A page can close while the user is typing; typing, autorepeat, a delayed IME commit, or clipboard delivery can continue after 500 ms. Worse, dropping the beginning of a command and delivering its suffix to a shell can change its meaning. Conversely, a user who deliberately clicks the newly selected page during those 500 ms would lose legitimate input.

   The existing core tracks whether a press was consumed, but does not record the destination of passed presses. Releases and repeats are delivered according to the current focus. `page.send-key` also has a separate delivery path. A timeout added only to ordinary keyboard/text handling would leave that path open. [Proposed guard](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:42), [key tracking](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/keys.rs:298), [keyboard delivery](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:2685), [synthetic keys](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:2795).

   **Prefer explicit reacquisition of page input after an involuntary selection.** Display the neighbour, but inhibit its input until the user explicitly selects it or performs a deliberate focus action. Keep browser controls usable; neither elapsed time nor the next ordinary character should unlock the page. Include synthetic page keys, IME commits and paste, and do not grant activation for discarded input.

   Independently, associate held keys and asynchronous text/paste work with their original view/document or focus generation. On a destination change, release the old view’s keys and suppress their repeats/releases until fresh presses. This preserves the [keymap guarantees](/home/gusahlg/repos/sighurt/crates/sighurt-api/src/keymap.rs:10) without sacrificing deliberate tab switching.

   Page-opened tabs already stay in the background; preserve that simpler protection. `Keymap::reset()` is not a substitute: the example implementation clears sequences but retains INSERT mode. [Popup handling](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:2405), [reset](/home/gusahlg/repos/sighurt/extensions/sighurt-config-example/src/lib.rs:291).

2. **High — Retaining an answered request is insufficient to define the PTY lifetime.**

   The design’s claimed “existing cancellation on … quit” does not exist in the proposed form. `ServiceDone` removes the request; `browser.quit` only sets the quit flag. Shutdown closes engines directly, and service shutdown occurs after potentially blocking engine shutdown. Also, navigating to another view retains the previous view in history, so leaving `sig://terminal` does not necessarily replace its document or close it. [Design](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:127), [completion](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:680), [retained history](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:1517), [quit](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/commands.rs:229), [shutdown](/home/gusahlg/repos/sighurt/crates/sighurt-runtime/src/session.rs:958).

   Specify `Opening → Open → Closing → Closed` ownership, including cancellation during spawn, successful replies arriving after cancellation, failed delivery to the engine, guest traps, natural child exit and configuration changes. Reserve the one-per-document/eight-per-process allowance while spawning, and retain accounting through teardown.

   Decide explicitly whether a terminal survives in a **background tab** and whether it survives as a **history entry**. Those are different lifetimes. I would allow the former and close it on the latter. Begin PTY cancellation before engine shutdown can stall.

3. **High — `Arc<OwnedFd>` plus “dropped with HostState” does not guarantee closure or revocation.**

   A reader thread can retain the master while blocked on a read or waiting for queue space. Dropping the view’s reference then does nothing. Closing a descriptor from another thread is not a reliable way to interrupt a Linux blocking operation, and introduces descriptor-reuse hazards. [Proposed handle and reader](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:121), [Linux `close` semantics](https://man7.org/linux/man-pages/man2/close.2.html).

   Require nonblocking I/O, an explicit cancellation wakeup, cancellable queue waits and an orderly worker exit. Cancellation must stop writes and discard queued input immediately; merely withholding future service requests does not revoke an already delivered master.

   Keep the generic service reply from becoming an indefinitely cloneable descriptor owner. Use a one-shot native handoff or a take-once transfer object, identified by an opaque session ID. Equality should describe that identity. Every guest operation must resolve a handle belonging to its current document; the guest never receives an OS fd.

   The “runtime keeps no clone, therefore crash hangs up” explanation is incomplete: process death closes its descriptors regardless of Rust ownership. Preventing **other processes from inheriting a master** is what matters there.

4. **High — Signalling one process group cannot meet the “no orphan” acceptance criterion.**

   An interactive shell puts jobs into other process groups. Background jobs, ignored SIGHUP, `disown`, and detached sessions can survive the proposed HUP/KILL sequence. Kernel terminal hangup is not a recursive process-tree kill. The design acknowledges survivors and simultaneously requires none. [Cancellation and acceptance criteria](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:138), [survivor caveat](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:185), [session/hangup semantics](https://man7.org/linux/man-pages/man2/setsid.2.html).

   Choose an honest contract: guarantee reaping of the directly owned child and best-effort termination of terminal jobs, or introduce an appropriately managed per-terminal process scope. A cgroup can provide stronger group cleanup, but crash cleanup also needs an owner that survives the browser. [Kernel cgroup documentation](https://www.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html).

   Coordinate the waiter and escalation timer so a delayed signal cannot target a recycled PID/process-group ID. A pidfd protects the directly owned process; it does not by itself identify every descendant. Two `sig` instances need independent ownership and cancellation records, not merely independent PTYs.

5. **High — “No network policy” currently means public network access, not denial.**

   `DocNet::policy()` falls back to `NetPolicy::default()`, which permits public destinations. Skipping policy installation for internal pages would therefore leave ordinary `oxide` network APIs available to guests holding configuration or PTY access. Add an explicit deny-all capability and enforce it across every relevant networking host function. [Design](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:97), [default policy](/home/gusahlg/repos/sighurt/engines/sighurt-engine-wasm/src/net.rs:69), [fallback](/home/gusahlg/repos/sighurt/engines/sighurt-engine-wasm/src/net.rs:491).

   The proposed identity/linker/core checks otherwise form a defensible boundary, provided these details are explicit:

   - Privilege comes only from selecting embedded bytes through the trusted page registry. A downloaded copy of the same guest, a manifest, reported URL or import name grants nothing.
   - The engine fixes the **exact page identity**, not merely “internal versus web”. Help, config and terminal must not exchange grants through reload, traversal or history.
   - Child instantiation must clear all privileged state. The existing `load_module` constructor uses `..parent.clone()`, so adding privileged fields makes this particularly dangerous unless the proposed refusal and explicit clearing both land. [Constructor](/home/gusahlg/repos/sighurt/engines/sighurt-engine-wasm/src/capabilities.rs:2477).
   - Restore currently calls `open_view` directly; `navigate` is not the universal entry point today. Restore must classify again and cannot trust saved engine/identity metadata. [Restore](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:1645).
   - Define grants per operation: help gets information, config gets configuration operations, terminal gets PTY operations. Neither `info` nor URL parameters should become a command-dispatch facility.

   Require PTY launch from the selected, focused document’s deliberate start action. `asking_tab` currently means “current document of its tab”, including background tabs; existing activation checks alone do not establish visible consent. [Request eligibility](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:2508). Configuration writes should similarly require an explicit save action and remain restricted to the runtime’s configured path.

6. **High — The spawn recipe needs a precise child-only safety contract.**

   `close_range(..., CLOSE_RANGE_CLOEXEC)` is the correct direction for preserving Rust’s exec-error channel, but descriptors must also be created with CLOEXEC atomically: otherwise another concurrently spawned browser child can inherit them. Treat unavailable/failed descriptor sealing as a launch failure or use a reviewed fallback; never silently continue. [Spawn proposal](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:134), [`close_range`](https://man7.org/linux/man-pages/man2/close_range.2.html).

   Prepare argv, environment, paths and allocations before `pre_exec`. Inside it, use only appropriate low-level operations; no environment access, allocation, logging or locks. Bind all three standard descriptors to the slave, establish its controlling terminal and foreground group, and initialize its size and termios independently of the outer terminal’s raw mode. [Rust `pre_exec` requirements](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html#tymethod.pre_exec).

   Specify restoration of signal masks and ignored dispositions in the child. `exec` resets caught handlers, but preserves ignored dispositions and the signal mask. Do not reset the browser’s process-wide handlers. Ctrl-C and Ctrl-Z should normally travel through the slave’s line discipline to its foreground job; resize should update that PTY. [Signal inheritance](https://man7.org/linux/man-pages/man7/signal.7.html), [PTY behaviour](https://man7.org/linux/man-pages/man7/pty.7.html).

7. **Medium — `replaces` detects stale reads but does not prevent concurrent lost updates.**

   Two instances can both compare successfully, then overwrite each other. An external editor can write between comparison and rename. Atomic replacement prevents partial files; it does not make compare-and-replace atomic. [Claim](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:172), [current writer](/home/gusahlg/repos/sighurt/crates/sighurt-runtime/src/persistence.rs:398).

   Serialize Sighurt writers using a stable lock associated with the resolved target, then reread and compare under that lock. Distinguish missing from empty files, and bind the read snapshot to the target being replaced. Document the remaining race with non-cooperating external editors instead of claiming complete protection.

   Do not reuse the existing persistence helper unchanged for symlink targets: it tightens the destination’s parent directory to mode 0700 and uses a predictable temporary name opened with truncation. Applied to an arbitrary resolved configuration directory, that can change unrelated directory permissions. Use a dedicated writer with exclusive temporary-file creation, explicit target/symlink handling, appropriate permissions and durability. [Directory handling](/home/gusahlg/repos/sighurt/crates/sighurt-runtime/src/persistence.rs:107).

8. **Medium — The terminal’s required input provenance is already lost before Wasm receives it.**

   TUI paste becomes `TextInputEvent { commit }`; that type contains no paste marker. Wasm then funnels commits and key text through a function that removes control characters. An ordered queue added only to `sig` cannot reconstruct paste boundaries. The Wasm view also needs to report an editable input state, or the TUI’s `page_text` gate discards paste. [TUI conversion](/home/gusahlg/repos/sighurt/frontends/sighurt-tui/src/app.rs:1180), [input type](/home/gusahlg/repos/sighurt/crates/sighurt-api/src/input.rs:393), [Wasm conversion](/home/gusahlg/repos/sighurt/engines/sighurt-engine-wasm/src/view.rs:1011).

   Carry paste provenance through the native input contract, preserve ordering and avoid delivering printable key text twice. Bound the new queue by bytes and records, and clear document-specific queued input on replacement.

   “Strip all C0 controls” also strips tabs and newlines, breaking normal config-editor and nvim pastes. Define a separate terminal paste policy: preserve intended text, prevent bracketed-paste terminator/control injection, and make multiline handling explicit when bracketed paste is disabled. Bracketed paste is a terminal convention, not an authorization boundary.

   The frontend is not “without Escape”: legacy Escape can reach the guest under the proposed terminal-only binding, and the existing parser recognizes Ctrl-\\. What cannot be recovered is Escape versus Ctrl-[ and other legacy ambiguities. Keep the exit chord terminal-only as binding decision 5 requires; section E’s “everywhere” contradicts it. Never forward inner terminal output directly to the outer terminal. [Conflicting statements](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:152), [legacy parser](/home/gusahlg/repos/sighurt/frontends/sighurt-tui/src/term/parser.rs:1284).

9. **Medium — `Command::Custom` needs changes to validation and execution contracts, not just parsing and dispatch.**

   Parsing every bare word as `Custom` means `FromStr` can no longer reject unknown bare commands. `CommandSet` currently knows only built-ins and chrome actions. Extend it with the configured command namespace so misspellings remain configuration-load errors across keys, keymaps and mouse buttons. Keep unrestricted parsing only for already validated settings. [Command contract](/home/gusahlg/repos/sighurt/crates/sighurt-api/src/commands.rs:1), [`CommandSet`](/home/gusahlg/repos/sighurt/crates/sighurt-api/src/commands.rs:540), [configuration validation](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/config.rs:1065).

   “Chromes need no change” also changes observable behaviour: they currently retain an invalid command in the editor, but discard it and return focus to the page after successful parsing. With permissive `Custom` parsing, an unknown command would now return the user to a live shell before the core reports the error. Preserve that error/editing behaviour or explicitly accept the change. [Chrome submission](/home/gusahlg/repos/sighurt/ui/sighurt-ui/src/chrome.rs:887).

   Expand aliases through the same ordered executor as `RunAll`. A recursive `run_command` loop would miss the existing between-command focus settlement and `page.send-key` activation handling. Preserve input provenance: an alias invoked by a physical binding and one submitted in the chrome must retain the corresponding existing activation semantics. [Execution path](/home/gusahlg/repos/sighurt/crates/sighurt-core/src/state.rs:2703).

   Finally, “append an argument when it takes one” is underspecified. A template `"page.open"` fails today’s load-time validation; appending to `"tab.select 2"` is invalid; appending to `"tab.new sig://help"` does not mean selecting a help section. For a minimal first version, use bounded, zero-argument aliases. Otherwise define explicit parameter slots and validate the entire expansion before performing any command. Bound expansion length as well as the number of names. Preserve arguments as data—no shell evaluation, interpolation of page-controlled strings into command names, or new separator syntax.

10. **Medium — The terminal configuration and environment contracts need correction.**

    `[pages] terminal = false` cannot coexist with `[pages.terminal] command`: TOML cannot make `terminal` both a boolean and a table. Use, for example, `[pages.terminal] enabled = false`. [Conflicting settings](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:46), [program selection](/home/gusahlg/repos/sighurt/docs/rewrite/pages-2026-11-05.md:137).

    The environment denylist is useful hygiene, not isolation. Inherited credentials, agent sockets, loader variables and shell startup variables remain available to a full-rights shell. State that honestly. Construct the child environment without changing the parent’s, and handle stale `LINES`, `COLUMNS` and terminal capability overrides. Preserve necessary user/Nix environment deliberately.

    Specify argv parsing for configured commands and `$VISUAL`/`$EDITOR`: literal executable plus arguments, without implicit `sh -c`; pass the absolute config path as a separate argument. Advertise `TERM`/`COLORTERM` capabilities the emulator actually supports, backed by nvim tests.

Before implementation, I would replace the timer with explicit input reacquisition, specify the PTY lifecycle and achievable cleanup guarantee, add deny-all internal-page networking, repair paste provenance, and resolve the command/configuration contracts above.

The minimal honest guest PTY API is `open(Shell | EditConfig, size) → opaque handle`, bounded nonblocking `read`, `write` returning the accepted byte count, `resize`, explicit `close`, and status distinguishing running, EOF, cancellation and child exit. Native code owns the one-shot fd transfer, cancellation and reaping. No guest-supplied program, path, environment, PID, signal number or raw fd is needed.

Keep terminal disabled until tests cover cancellation during spawn and reply delivery, a full reader queue during close, history retention, shutdown watchdog exit, held Enter across involuntary selection, delayed paste/IME delivery, cross-page privilege attempts, and concurrent configuration writes.