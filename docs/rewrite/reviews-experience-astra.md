# Outside review of the Experience design (GPT-6 Astra, 15 November 2026)

Reviewed: `experience-2026-11-15.md` as first written. Every finding was taken into the design; the document cites them by number. Two were answered by an accepted change instead of the proposed fix: 2 (one precedence rule, `[tui.keys]` no longer over everything) and 10 (stateful keymaps given up for now).

1. **High — Making mode changes commands changes a security decision, not just representation.** ([Design §1.1](../../docs/rewrite/experience-2026-11-15.md:104))

   Concrete case: a page closes its tab, exposing another tab whose input the core holds; NORMAL is active and nothing is fullscreen. Press Escape, then type. Today Escape changes to INSERT through `KeyAction::Take`, so the hold remains. With `Run([input.mode insert])`, the existing command path calls `release_input()`, and subsequent typing reaches the newly exposed page. Both the [key-command path](../../crates/sighurt-core/src/state.rs:2942) and [command dispatcher](../../crates/sighurt-core/src/commands.rs:110) clear that hold.

   **Fix:** Separate “executes a command” from “acknowledges the newly shown page.” A mode-only command must preserve the hold to preserve today’s behavior. Empty command lists must also remain inert.

2. **High — URL-first specificity breaks the promised terminal-layer precedence.** ([§1.4](../../docs/rewrite/experience-2026-11-15.md:164), [compatibility claim](../../docs/rewrite/experience-2026-11-15.md:200))

   Configure common `[keys.url."https://a.test"]` with `ctrl-r = "page.reload"` and `[tui.keys]` with `ctrl-r = "chrome.refresh"`. With page focus at `https://a.test` in the terminal, today Ctrl+R refreshes the chrome: the terminal layer is searched completely before the common layer. The proposal reloads the page because an address beats a stated frontend.

   This also occurs when a common exact-host rule competes with a terminal wildcard rule. It is **additional to** the explicitly accepted change that all matching site tables contribute.

   **Fix:** Represent source-layer precedence explicitly and compare it before specificity within a layer. `frontend = terminal` is an applicability condition; it cannot substitute for a higher-priority layer.

3. **High — One unconditional replay policy cannot reproduce both existing keymaps.** ([§1.2](../../docs/rewrite/experience-2026-11-15.md:143))

   Today’s `bindings` matcher re-resolves consumed repeats. Bind Ctrl+A to `tab.new`, hold it, then release Ctrl while A continues repeating: commands stop. The proposed cached-command policy keeps opening tabs. An [existing test explicitly requires the former behavior](../../crates/sighurt-core/src/keys.rs:909).

   Conversely, the example keymap repeats single-key `Run` actions, but **never repeats mode switches, `ExitFullscreenOr`, or `Custom` actions**. Turning those into command lists makes them repeatable. Cached `input.mode insert` would not toggle on every repeat—it is an absolute assignment—but it could restore INSERT after a mouse-command mode change while Escape remains held. That violates the existing [“held quietly” behavior](../../extensions/sighurt-config-example/tests/example.rs:857).

   **Fix:** Add an explicit repeat policy: never, replay resolved commands, or re-match. Preserve each starting point’s policy. Specify triggering-key identity, invalidation by another fresh press, modifier handling, and cache clearing on reset/reload. A cache containing only “the last commands” is insufficient.

4. **High — `Unbound` participates in prefix matching, creating bindings that supposedly do not exist.** ([§1.2](../../docs/rewrite/experience-2026-11-15.md:134), [§1.4](../../docs/rewrite/experience-2026-11-15.md:169))

   In a permissive mode, with page focus outside a field, add only `unbind("g g")`. On the first `g`, that tombstone is the most specific matching prefix, so the engine waits and swallows `g`. There is no live action to complete. Conversely, define `g g`, then `unbind("g")`: the complete tombstone wins immediately and disables the longer binding, although unbinding is described as suppressing **the same keys**.

   Live prefixes have a related problem: adding the SDK sketch’s `g g` hides today’s single-key `g`; defining `g` later makes `g g` unreachable. In TOML, “defined later” additionally needs a definition: the current settings representation is a [sorted `BTreeMap`](../../crates/sighurt-api/src/settings.rs:32), which loses textual order.

   **Fix:** Resolve tombstones for each exact sequence before constructing the live prefix matcher. Tombstones must not themselves create prefixes or suppress descendants. Reject or explicitly resolve complete/prefix conflicts after composition; provide a deliberate prefix-replacement operation. Preserve source order if it has semantic significance.

5. **High — A sequence can acquire a different meaning halfway through because conditions are re-evaluated against unrelated state.** ([§1.2](../../docs/rewrite/experience-2026-11-15.md:128))

   Bind `g a` on `https://a.test` and `g enter → tab.close` on `https://b.test`. Press `g` on A, select B with the mouse, then press Enter. Page focus and field status can remain unchanged, so today’s `KeyContext` notification does not clear the prefix. The written algorithm now completes B’s binding even though that binding was ineligible when `g` was pressed. A committed URL change can produce the same result without switching tabs.

   Reload and `input.mode` changes have similar holes: only context/reset clearing is specified. This extends an existing matcher edge into the new engine’s contract.

   **Fix:** Retain the initially eligible candidate identities, and cancel pending sequences on changes of mode, selected tab/document, relevant committed URL, or configuration generation. Define cancellation and processing of the current key explicitly. Do not let newly eligible bindings inherit earlier keystrokes.

6. **High — `RunThenPass(Vec<_>)` needs a unified delivery path; leaving the current fullscreen exception “untouched” is insufficient.** ([Action definition](../../docs/rewrite/experience-2026-11-15.md:77))

   `pass("escape")` naturally becomes `RunThenPass([])`. On a fullscreen page, that must behave exactly like ordinary `Pass`. Currently the [fullscreen Escape guard handles only `KeyOutcome::Deliver`](../../crates/sighurt-core/src/state.rs:2983); `RunThenDeliver` has a separate branch. A straightforward extension of that branch to vectors bypasses the guard.

   Likewise, `RunThenPass([tab.select 2, input.mode insert])` must deliver to the final focus owner while retaining the original press’s identity. If tab 2 is `sig://terminal`, the forwarded key must not become a fresh launch activation originating there.

   **Fix:** Execute command batches through one ordered runner, then use one guarded delivery path. Normalize empty passing actions to ordinary delivery without command side effects. Preserve origin, repeat/release ownership and fullscreen handling. For sequences, specify that only the final event passes; consumed prefixes are not replayed.

7. **Medium — `input.mode` is sound as a command, but its execution and lifecycle contract is incomplete.** ([§1.1](../../docs/rewrite/experience-2026-11-15.md:104))

   A line binding running `input.mode insert` must change the underlying page mode while leaving focus in the chrome. It must not mutate `Keys::line` or close the editor. By contrast, submitting `:input.mode insert` currently follows the chrome’s normal submission behavior: [focus moves to the page before the command executes](../../ui/sighurt-ui/src/chrome.rs:988).

   Within `Run([input.mode resize, input.mode normal])`, changes must occur synchronously in order. Pending input must be cancelled even if focus does not change. With `RunThenPass([input.mode normal])`, passing remains an explicit instruction; the engine must not re-match the event against NORMAL.

   **Fix:** Specify these semantics, including same-mode assignments, pending/repeat invalidation, and final IME/paste eligibility. Validate dynamically returned mode names too: callbacks cannot have all their possible commands checked at configuration load.

8. **Medium — Generic `bind` and `unbind` do not reliably override the defaults.** ([§1.4](../../docs/rewrite/experience-2026-11-15.md:160))

   `normal.unbind("space")` loses to today’s default Space binding translated with `field = false`. Outside a field, Space still reaches the page. `normal.bind("space", "tab.next")` also loses. Similarly, a generic TOML Ctrl+L override loses to the default terminal-specific Ctrl+L binding.

   The stated rule is internally consistent, but it undermines the SDK’s central “change one shortcut” operation: users must discover and reproduce every condition attached to the original binding. “Unbind everywhere” also does not suppress more-specific site variants.

   **Fix:** Distinguish exact-definition replacement, scoped masking, and removal of all variants within a selected scope. Give the ordinary override/unbind workflow explicit semantics that handle conditional defaults. Provide an `explain(key, context)` facility showing the winning rule and why an override lost; listing `when` alone will not explain effective behavior.

9. **Medium — The typing rule is not a text-protection or password boundary.** ([§1.2](../../docs/rewrite/experience-2026-11-15.md:128), [fallback API](../../docs/rewrite/experience-2026-11-15.md:93))

   In INSERT with a password field focused, a character binding is filtered out, but `Fallback::Custom` still receives the complete `KeyEvent`, including the character and text. A counts fallback can therefore observe password digits. A sequence beginning with a non-typing chord, such as `ctrl-k a`, can also capture character continuations in an editor.

   Separately, `Chord::types()` deliberately returns false for physical `code:KeyJ` bindings. Such a binding can intercept J in a password field or line. That is [existing behavior](../../crates/sighurt-api/src/keymap.rs:870), not proof that text-producing line bindings are impossible.

   Native keymaps already receive sensitive input; the new concern is carrying that exposure into the proposed module contract.

   **Fix:** Separate text-capture permission from fallback selection. Default module fallbacks should not receive typing in sensitive contexts. Redacting only `text` is insufficient because `key` and `code` also disclose input. Keep indicators derived from configured chord spellings, never raw event text.

10. **Medium — Removing the trait loses stateful input processing that the proposed fallback cannot replace.** ([fallback’s counts/hints claim](../../docs/rewrite/experience-2026-11-15.md:96))

    For `3 j`, the fallback sees `3`, but the ordinary `j` binding bypasses it. A pure function of the current key and browser cannot remember the count, apply it to that binding, display it, or clear it on context/reset. Hint-label accumulation has the same problem. Sixteen fixed modes and four-stroke sequences do not replace arbitrary bounded interpreter state.

    Capturing a mutex or atomic in `Arc<dyn Fn>` permits state technically, but contradicts the purity claim and still supplies no lifecycle notification or dynamic indicator.

    **Fix:** Keep a bounded stateful extension point worth preserving from `Keymap`: an explicit reducer receiving input and lifecycle events, with explicit state and validated outputs. It can coexist with the common matcher. Otherwise remove the counts/hints claim and acknowledge that this capability is being dropped.

11. **Medium — The model can be lowered to Wasm later, but it is not presently a serializable data model.** ([§1.1](../../docs/rewrite/experience-2026-11-15.md:113), [§5](../../docs/rewrite/experience-2026-11-15.md:309))

    An `Arc<dyn Fn>` contains a native callable and potentially arbitrary captured state. It does not cross as bytes. A callback index works only with a module-owned registry and defined lifetime; essentially the same indirection can represent a stateful trait implementation. Also, `call(index, browser)` cannot implement `Fallback::Custom`: it lacks the key argument.

    `&dyn Browser` can remain an ergonomic façade implemented over a guest-side owned snapshot, but its borrowed strings, `usize`s and IDs require a separate wire representation.

    Snapshot cost matters: the current bounds allow [1,000 tabs](../../crates/sighurt-core/src/session.rs:28) with URLs up to [2 MiB each](../../crates/sighurt-core/src/tabs.rs:29). Serializing the whole view per key could copy roughly 2 GiB of URLs. Guest fuel does not bound host serialization.

    **Fix now:** Separate callback references from callback storage; define distinct binding/fallback invocation payloads and a nonrecursive resolved-action result. Specify generation/lifetime rules and total input/output byte budgets. Keep the native browser view zero-copy, and design bounded or demand-selected snapshots rather than serializing everything.

12. **Medium — The SDK needs a final validation phase, and partial acceptance weakens reload guarantees.** ([§2](../../docs/rewrite/experience-2026-11-15.md:249))

    `Into<Action>` is an infallible conversion, but command text is fallible and contextual. `configure()` receives neither the selected chrome’s command vocabulary nor TOML aliases. The default `help` binding cannot be fully checked there. A reference to `resize` may also precede `add_mode("resize")`, so eager mode validation rejects valid composition.

    “Use the experience without bad bindings” is unsafe for interacting changes: a configuration can successfully unbind its old escape route while its replacement mode-switch binding is rejected. Today invalid reloads retain the previous configuration.

    **Fix:** Accept an unresolved `ActionSpec` or a fallible SDK conversion, collect diagnostics, then finalize against the complete modes, aliases and frontend capabilities. Validate public `Keys` values too, not just builder calls, and swap reloads atomically. Callback results need runtime validation. Return a dedicated resolved-action type instead of accepting `Custom`/`Unbound` and silently treating them as `Take`. The sketch’s bare `send_mail` also needs the documented `custom(description, f)` wrapper.

13. **Medium — `everywhere()` conflates text permission, binding scope and construction order.** ([§1.3](../../docs/rewrite/experience-2026-11-15.md:150))

    Defaults call `everywhere()` before the example adds `resize`. Does that new mode inherit Ctrl+O and F1? Copying into current tables says no; evaluating the selector after construction says yes. Nothing specifies which. Changing a mode’s `page_text` creates the same ambiguity.

    Moreover, `page_text == false` does not mean the mode takes page keys: `unmatched = Pass` is independently allowed. A mode with `page_text = true, unmatched = Take` is excluded despite taking unmatched keys.

    There is also a line-validation contradiction: valid existing `[keys] j = "tab.next"` is broadcast into `line`, where §2 says it becomes a problem.

    **Fix:** Make target groups explicit and define when they resolve. Preserve NORMAL-plus-line defaults through a named group rather than inferring membership from text permission. Skip impossible line copies from broadcast operations; reserve errors for explicit impossible line bindings. A separate line-table type would also avoid exposing meaningless `Mode` fields there.

14. **Medium — TOML cannot express the useful intersection of mode and site.** ([§1.5](../../docs/rewrite/experience-2026-11-15.md:187))

    Suppose the user wants `j → page.find-next` only in NORMAL on `https://mail.example.com`. `[keys.url.…]` installs it in INSERT too, where it fires outside a field. `[keys.mode.normal]` installs it on every site. A generic INSERT unbind loses to the URL-specific binding.

    Rust has `mode("normal").on(url)`; the proposed TOML operations cannot express the equivalent change. This defeats an important case of changing one shortcut without rebuilding.

    **Fix:** Support composed scopes, such as `[keys.mode.normal.url."<pattern>"]`, or a binding-record form with mode, URL, frontend and field conditions. State explicitly which SDK operations TOML intentionally cannot express.

15. **Medium — Excluding modifier/dead keys also removes existing TOML bindings.** ([§1.2](../../docs/rewrite/experience-2026-11-15.md:125))

    Today a binding such as `"shift-Shift" = "page.reload"` can match a Shift press whose modifier state includes Shift. Physical bindings can also match a key that is dead on the current layout. The current `bindings` matcher does not perform the example keymap’s modifier/dead-key bypass.

    The proposal sends those events directly to fallback and forbids them in sequences. `Keys::plain()` therefore cannot preserve these existing files, independently of the documented site-table change.

    **Fix:** Preserve explicitly configured standalone bindings for these events while excluding them from sequence progression, or declare this as another intentional compatibility break.

16. **Medium — Site matching and page data need an explicit trust contract in the SDK.** ([`UrlPattern` contract](../../crates/sighurt-api/src/keymap.rs:538))

    The SDK’s `.on("https://mail.example.com")` also matches `https://mail.example.com:8443`: `UrlPattern` ignores ports. It is a shortcut-selection pattern, not an origin authorization check. Titles and URLs supplied to callbacks likewise remain page-controlled data; converting a title into command text can turn a page-chosen value into a browser action.

    These are existing limitations, not evidence that web pages can spoof internal-page identity. The trustworthy internal-page fact is [`TabInfo::internal`](../../crates/sighurt-api/src/keymap.rs:119), which must remain derived from the core’s record across any serialization boundary.

    **Fix:** Document that site conditions grant no authority; offer an origin-aware condition if privileged configurations need one. Keep page values as typed command operands rather than parsed command programs. Finally, do not describe a configuration module as unable to cause network activity merely because it lacks network imports: returning `PageOpen` can cause it through the host.