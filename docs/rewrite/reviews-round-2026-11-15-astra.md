# Outside review of the round of 15 November 2026 (GPT-6 Astra): interrupted

Astra reviewed `config-ext` at 8eaf745 in a worktree of its own, wrote 56 probing tests against the key engine and ran them and the existing suites. Its usage limit cut it off twice before the full critique (code of the other lanes, results, design, directions) was written; what follows is the report as it stood, which covers the key engine. Six probes fail and are real defects; they are kept as regression tests (`crates/sighurt-core/tests/probes.rs`). Noted in its log and not written up: the kitty-through-tmux cleanup (`d=A` does not delete virtual placements and may delete other programs' ordinary ones) and image-id collisions between two `sig`s in one terminal.

## Verdict

1. The inherited probe compiled unchanged and found five failures in 44 tests.
2. Two failures show unfinished sequences surviving changes to the selected tab.
3. A mode-switch alias can replay on a held key in the address line.
4. Configuration validation accepts nonexistent modes in aliases and buttons.
5. The tested direct NORMAL text and paste protections passed.
6. The tested input-hold and internal-program activation protections passed.
7. These passing probes establish narrow behavior, not a general security proof.
8. Existing crate and browser tests are being verified separately.
9. Performance and portability claims require distinguishing measurements from implementation claims.
10. The key engine needs fixes before its documented guarantees can be accepted.

## Findings by severity

**High: sequences survive selected-tab changes.** The probe creates a prefix on one tab, closes it and opens a replacement at the same position and address, then finishes the old sequence on the replacement. Another probe leaves and returns before the second stroke. Both zoom the page incorrectly. Fix sequence ownership using stable tab/document identity and eager invalidation on lifecycle changes.

**Medium: aliases bypass mode-switch repeat suppression.** Bind a line key to an alias for `input.mode resize`, press it, change mode back, then send a repeat. RESIZE returns. Expand aliases for repeat eligibility and keep mode changes quiet for the entire press.

**Medium: mode validation misses aliases and buttons.** Both an unused alias and a button referencing a nonexistent mode parse successfully. Validate every statically configured command against the final mode set before accepting the configuration.

## Results critique

Five deterministic failing probes outweigh a blanket claim that all key-engine guarantees are covered. Existing passing suites do not establish the behavior missing from their assertions. Timing claims must identify workload, sample count, scheduling environment and measurement boundary; compile checks establish neither linking nor usable ports. Detailed comparison with the day's evidence follows after the first test runs.

## Thought process and design

Mode changes as commands are useful but must preserve security distinctions such as acknowledging a newly shown page. Binding replacement and one precedence rule can simplify use, but need an explanation of the winning rule. Removing a trait is reasonable for a bounded table engine; it does not make stateful extensions or native callbacks serializable. Adapter smoothing needs a policy switch and lifecycle cancellation. A platform feature should gate a dependency graph, with clear runtime refusal where no backend exists.

## Future directions

First fix sequence ownership, alias repeat classification and complete validation. Then add regression tests at lifecycle boundaries. Next measure latency through visible presentation on representative workloads and hardware. Follow with native-runner portability checks, terminal support and platform-specific rendering. Defer executable configuration modules and a generalized composable interface until byte budgets, capabilities and state lifetimes are explicit. Drop any claim that a compile check proves a working port.

## Execution log

- Read CLAUDE.md before running commands.
- Initial `export CARGO_TARGET_DIR=/data/sig-builds/targets/merge; tools/cargo test -p sighurt-core --test astra_probe` was blocked by the sandbox's denial of the systemd user bus: `Failed to connect to user scope bus via local transport: Operation not permitted`.
- Retried the identical command with approved sandbox escalation, retaining the required machine lock and memory cap: 39 passed, five failed, exit 101; no probe edits needed.
- Existing suites run separately with the inherited scratch probe temporarily outside the repository.
- No Servo build, desktop client, tmux server or user configuration access.

## Failing probes, verbatim

```rust
#[test]
fn probe_alias_mode_is_never_repeated_in_line() {
    let env = Environment { keys: modes::keys(), ..environment() };
    let mut h = Harness::with(r#"
[commands]
resize = "input.mode resize"
[keys.location]
f8 = "resize"
"#, env);
    h.open(&["https://a.test/"]);
    h.command("location.focus");
    let seat = SeatId::next();
    h.key(seat, (66, "F8"), ButtonState::Pressed, false);
    assert_eq!(h.state.keymap_indicator(), Some("RESIZE"));
    h.command("input.mode normal");
    h.key(seat, (66, "F8"), ButtonState::Pressed, true);
    assert_eq!(h.state.keymap_indicator(), None, "alias replayed input.mode");
}
```

```rust
#[test]
fn probe_alias_missing_mode_is_rejected() {
    let env = Environment { keys: modes::keys(), ..environment() };
    let parsed = Config::parse(Some(r#"version = 1
[commands]
leave = "input.mode nonexistent"
[keys.mode.normal]
escape = "leave"
"#), &env);
    assert!(parsed.is_err(), "alias to a nonexistent mode was accepted");
}
```

```rust
#[test]
fn probe_button_missing_mode_is_rejected() {
    let env = Environment { keys: modes::keys(), ..environment() };
    let parsed = Config::parse(Some(r#"version = 1
[input.buttons]
back = "input.mode nonexistent"
"#), &env);
    assert!(parsed.is_err(), "button to a nonexistent mode was accepted");
}
```

```rust
#[test]
fn probe_sequence_does_not_transfer_to_replacement_tab() {
    let mut h = Harness::start_modes(&["https://a.test/", "https://a.test/"]);
    h.command("tab.select 1");
    let first = h.shown();
    let seat = SeatId::next();
    tap(&mut h, seat, (34, "g"));
    h.command("tab.close");
    assert_ne!(h.shown(), first);
    let before = h.state.tabs().nth(h.state.active()).unwrap().zoom;
    tap(&mut h, seat, (45, "x"));
    assert_eq!(h.state.tabs().nth(h.state.active()).unwrap().zoom, before,
        "replacement tab inherited the old tab's g prefix");
}
```

```rust
#[test]
fn probe_leaving_and_returning_cancels_sequence() {
    let mut h = Harness::start_modes(&["https://a.test/", "https://b.test/"]);
    h.command("tab.select 1");
    let seat = SeatId::next();
    tap(&mut h, seat, (34, "g"));
    h.command("tab.select 2");
    h.command("tab.select 1");
    let before = h.state.tabs().nth(h.state.active()).unwrap().zoom;
    tap(&mut h, seat, (45, "x"));
    assert_eq!(h.state.tabs().nth(h.state.active()).unwrap().zoom, before,
        "a sequence survived leaving its tab and returning");
}
```

### Failure output, verbatim

```text
failures:

---- probe_button_missing_mode_is_rejected stdout ----

thread 'probe_button_missing_mode_is_rejected' (1148213) panicked at crates/sighurt-core/tests/astra_probe.rs:1666:5:
button to a nonexistent mode was accepted
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- probe_alias_missing_mode_is_rejected stdout ----

thread 'probe_alias_missing_mode_is_rejected' (1148210) panicked at crates/sighurt-core/tests/astra_probe.rs:1656:5:
alias to a nonexistent mode was accepted

---- probe_leaving_and_returning_cancels_sequence stdout ----

thread 'probe_leaving_and_returning_cancels_sequence' (1148215) panicked at crates/sighurt-core/tests/astra_probe.rs:1694:5:
assertion `left == right` failed: a sequence survived leaving its tab and returning
  left: 1.1
 right: 1.0

---- probe_alias_mode_is_never_repeated_in_line stdout ----

thread 'probe_alias_mode_is_never_repeated_in_line' (1148211) panicked at crates/sighurt-core/tests/astra_probe.rs:1644:5:
assertion `left == right` failed: alias replayed input.mode
  left: Some("RESIZE")
 right: None

---- probe_sequence_does_not_transfer_to_replacement_tab stdout ----

thread 'probe_sequence_does_not_transfer_to_replacement_tab' (1148220) panicked at crates/sighurt-core/tests/astra_probe.rs:1680:5:
assertion `left == right` failed: replacement tab inherited the old tab's g prefix
  left: 1.1
 right: 1.0


failures:
    probe_alias_missing_mode_is_rejected
    probe_alias_mode_is_never_repeated_in_line
    probe_button_missing_mode_is_rejected
    probe_leaving_and_returning_cancels_sequence
    probe_sequence_does_not_transfer_to_replacement_tab

test result: FAILED. 39 passed; 5 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

error: test failed, to rerun pass `-p sighurt-core --test astra_probe`
```


### Additional initially failing probe: probe_reload_same_url_cancels_sequence

```rust
#[test]
fn probe_reload_same_url_cancels_sequence() {
    let mut h = Harness::start_modes(&["https://a.test/"]);
    let seat = SeatId::next();
    tap(&mut h, seat, (34, "g"));
    let before = h.document(h.shown());
    h.command("page.reload");
    assert_ne!(h.document(h.shown()), before);
    tap(&mut h, seat, (45, "x"));
    assert_eq!(h.state.tabs().nth(h.state.active()).unwrap().zoom, 1.0,
        "a new document inherited its predecessor's sequence");
}
```


### Additional initially failing probe: probe_modifier_and_dead_keys_do_not_drop_prefix

```rust
#[test]
fn probe_modifier_and_dead_keys_do_not_drop_prefix() {
    let mut h = Harness::start_modes(&["https://a.test/"]);
    let seat = SeatId::next();
    tap(&mut h, seat, (34, "g"));
    tap(&mut h, seat, (42, "Shift"));
    tap(&mut h, seat, (40, "Dead"));
    tap(&mut h, seat, (45, "x"));
    assert_eq!(h.state.tabs().nth(h.state.active()).unwrap().zoom, 1.1);
}
```


### Additional initially failing probe: probe_config_reload_keeps_existing_mode_and_clears_prefix

```rust
#[test]
fn probe_config_reload_keeps_existing_mode_and_clears_prefix() {
    let env = Environment { keys: modes::keys(), ..environment() };
    let config = Config::parse(Some("version = 1"), &env).unwrap();
    let mut h = Harness::with("", env);
    h.open(&["https://a.test/"]);
    let seat = SeatId::next();
    tap(&mut h, seat, (34, "g"));
    h.apply(BrowserEvent::ConfigReloaded(Ok(Box::new(config))));
    tap(&mut h, seat, (45, "x"));
    assert_eq!(h.state.tabs().nth(h.state.active()).unwrap().zoom, 1.0);
}
```


### Expanded initial run output, verbatim

```text
failures:

---- probe_button_missing_mode_is_rejected stdout ----

thread 'probe_button_missing_mode_is_rejected' (1153431) panicked at crates/sighurt-core/tests/astra_probe.rs:1666:5:
button to a nonexistent mode was accepted
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- probe_alias_missing_mode_is_rejected stdout ----

thread 'probe_alias_missing_mode_is_rejected' (1153427) panicked at crates/sighurt-core/tests/astra_probe.rs:1656:5:
alias to a nonexistent mode was accepted

---- probe_alias_mode_is_never_repeated_in_line stdout ----

thread 'probe_alias_mode_is_never_repeated_in_line' (1153428) panicked at crates/sighurt-core/tests/astra_probe.rs:1644:5:
assertion `left == right` failed: alias replayed input.mode
  left: Some("RESIZE")
 right: None

---- probe_config_reload_keeps_existing_mode_and_clears_prefix stdout ----

thread 'probe_config_reload_keeps_existing_mode_and_clears_prefix' (1153432) panicked at crates/sighurt-core/tests/astra_probe.rs:1888:5:
assertion `left == right` failed
  left: 1.1
 right: 1.0

---- probe_modifier_and_dead_keys_do_not_drop_prefix stdout ----

thread 'probe_modifier_and_dead_keys_do_not_drop_prefix' (1153439) panicked at crates/sighurt-core/tests/astra_probe.rs:1822:5:
assertion `left == right` failed
  left: 1.0
 right: 1.1

---- probe_leaving_and_returning_cancels_sequence stdout ----

thread 'probe_leaving_and_returning_cancels_sequence' (1153436) panicked at crates/sighurt-core/tests/astra_probe.rs:1694:5:
assertion `left == right` failed: a sequence survived leaving its tab and returning
  left: 1.1
 right: 1.0

---- probe_reload_same_url_cancels_sequence stdout ----

thread 'probe_reload_same_url_cancels_sequence' (1153444) panicked at crates/sighurt-core/tests/astra_probe.rs:1803:5:
assertion `left == right` failed: a new document inherited its predecessor's sequence
  left: 1.1
 right: 1.0

---- probe_sequence_does_not_transfer_to_replacement_tab stdout ----

thread 'probe_sequence_does_not_transfer_to_replacement_tab' (1153445) panicked at crates/sighurt-core/tests/astra_probe.rs:1680:5:
assertion `left == right` failed: replacement tab inherited the old tab's g prefix
  left: 1.1
 right: 1.0


failures:
    probe_alias_missing_mode_is_rejected
    probe_alias_mode_is_never_repeated_in_line
    probe_button_missing_mode_is_rejected
    probe_config_reload_keeps_existing_mode_and_clears_prefix
    probe_leaving_and_returning_cancels_sequence
    probe_modifier_and_dead_keys_do_not_drop_prefix
    probe_reload_same_url_cancels_sequence
    probe_sequence_does_not_transfer_to_replacement_tab

test result: FAILED. 45 passed; 8 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

error: test failed, to rerun pass `-p sighurt-core --test astra_probe`
```

The dead-key probe initially used Key::Named("Dead"), but the API represents it as Key::Dead. Correcting the event makes it pass. The configuration reload probe initially supplied an identical configuration; state.rs intentionally ignores identical reloads. Changing scroll-speed makes it pass. These two initial failures are not counted as production defects.


### Corrected final probe output, verbatim

```text
failures:

---- probe_alias_missing_mode_is_rejected stdout ----

thread 'probe_alias_missing_mode_is_rejected' (1155390) panicked at crates/sighurt-core/tests/astra_probe.rs:1656:5:
alias to a nonexistent mode was accepted
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- probe_alias_mode_is_never_repeated_in_line stdout ----

thread 'probe_alias_mode_is_never_repeated_in_line' (1155391) panicked at crates/sighurt-core/tests/astra_probe.rs:1644:5:
assertion `left == right` failed: alias replayed input.mode
  left: Some("RESIZE")
 right: None

---- probe_button_missing_mode_is_rejected stdout ----

thread 'probe_button_missing_mode_is_rejected' (1155394) panicked at crates/sighurt-core/tests/astra_probe.rs:1666:5:
button to a nonexistent mode was accepted

---- probe_leaving_and_returning_cancels_sequence stdout ----

thread 'probe_leaving_and_returning_cancels_sequence' (1155399) panicked at crates/sighurt-core/tests/astra_probe.rs:1694:5:
assertion `left == right` failed: a sequence survived leaving its tab and returning
  left: 1.1
 right: 1.0

---- probe_reload_same_url_cancels_sequence stdout ----

thread 'probe_reload_same_url_cancels_sequence' (1155407) panicked at crates/sighurt-core/tests/astra_probe.rs:1803:5:
assertion `left == right` failed: a new document inherited its predecessor's sequence
  left: 1.1
 right: 1.0

---- probe_sequence_does_not_transfer_to_replacement_tab stdout ----

thread 'probe_sequence_does_not_transfer_to_replacement_tab' (1155408) panicked at crates/sighurt-core/tests/astra_probe.rs:1680:5:
assertion `left == right` failed: replacement tab inherited the old tab's g prefix
  left: 1.1
 right: 1.0


failures:
    probe_alias_missing_mode_is_rejected
    probe_alias_mode_is_never_repeated_in_line
    probe_button_missing_mode_is_rejected
    probe_leaving_and_returning_cancels_sequence
    probe_reload_same_url_cancels_sequence
    probe_sequence_does_not_transfer_to_replacement_tab

test result: FAILED. 47 passed; 6 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

error: test failed, to rerun pass `-p sighurt-core --test astra_probe`
```


### Final expanded run (56 tests), verbatim

```text
failures:

---- probe_alias_missing_mode_is_rejected stdout ----

thread 'probe_alias_missing_mode_is_rejected' (1174361) panicked at crates/sighurt-core/tests/astra_probe.rs:1657:5:
alias to a nonexistent mode was accepted
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- probe_alias_mode_is_never_repeated_in_line stdout ----

thread 'probe_alias_mode_is_never_repeated_in_line' (1174362) panicked at crates/sighurt-core/tests/astra_probe.rs:1645:5:
assertion `left == right` failed: alias replayed input.mode
  left: Some("RESIZE")
 right: None

---- probe_button_missing_mode_is_rejected stdout ----

thread 'probe_button_missing_mode_is_rejected' (1174365) panicked at crates/sighurt-core/tests/astra_probe.rs:1667:5:
button to a nonexistent mode was accepted

---- probe_leaving_and_returning_cancels_sequence stdout ----

thread 'probe_leaving_and_returning_cancels_sequence' (1174371) panicked at crates/sighurt-core/tests/astra_probe.rs:1695:5:
assertion `left == right` failed: a sequence survived leaving its tab and returning
  left: 1.1
 right: 1.0

---- probe_reload_same_url_cancels_sequence stdout ----

thread 'probe_reload_same_url_cancels_sequence' (1174381) panicked at crates/sighurt-core/tests/astra_probe.rs:1804:5:
assertion `left == right` failed: a new document inherited its predecessor's sequence
  left: 1.1
 right: 1.0

---- probe_sequence_does_not_transfer_to_replacement_tab stdout ----

thread 'probe_sequence_does_not_transfer_to_replacement_tab' (1174382) panicked at crates/sighurt-core/tests/astra_probe.rs:1681:5:
assertion `left == right` failed: replacement tab inherited the old tab's g prefix
  left: 1.1
 right: 1.0


failures:
    probe_alias_missing_mode_is_rejected
    probe_alias_mode_is_never_repeated_in_line
    probe_button_missing_mode_is_rejected
    probe_leaving_and_returning_cancels_sequence
    probe_reload_same_url_cancels_sequence
    probe_sequence_does_not_transfer_to_replacement_tab

test result: FAILED. 50 passed; 6 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

error: test failed, to rerun pass `-p sighurt-core --test astra_probe`
```

The three added probes for released modifiers, fullscreen Escape and command-shaped page titles passed. The password-field fallback probe also passed: trusted native callbacks receive its key text.


## Review B: the speed work (decisions 145 and 146), 16 November 2026

GPT-6 Astra, read-only, on `config-ext` at 94086ec. Paths are as Astra wrote them (its worktree). The triage and the fixes are in STATUS.md.

1. **P1 — The WPE observer can call freed WebKit objects.**  
   [screen.rs:124](engines/sighurt-engine-wpe/src/screen.rs:124), [runtime.rs:1219](engines/sighurt-engine-wpe/src/runtime.rs:1219), [runtime.rs:2351](engines/sighurt-engine-wpe/src/runtime.rs:2351).

   Keeping the observer alive does not keep its callback targets alive. In pinned WebKit 2.52.6, `sync` copies callback functions and raw userdata under a lock, releases the lock, then invokes them. Removing a callback does not invalidate that copied entry. The WPE monitor supplies raw `this` as userdata. [Observer implementation](https://github.com/WebKit/WebKit/blob/wpewebkit-2.52.6/Source/WebKit/WPEPlatform/wpe/WPEScreenSyncObserver.cpp), [monitor implementation](https://github.com/WebKit/WebKit/blob/wpewebkit-2.52.6/Source/WebKit/UIProcess/wpe/DisplayVBlankMonitorWPE.cpp).

   **Failing interleaving:** ticker copies a callback; owner closes the last animated view and releases its context/process pool; monitor invalidation removes the callback and `stop()` returns without waiting; ticker invokes the saved pointer after the monitor is destroyed. Shutdown likewise destroys views and runs teardown before joining.

   **Fix:** quiesce callbacks before destroying their owners, including ordinary last-view closure. Make that quiescence resumable if another context can open. Final shutdown must join before WebKit teardown. Do **not** simply join inside `stop()`: WebKit calls it while holding monitor locks, including from the ticker’s own callback path.

2. **P2 — Different tmux panes can interleave kitty uploads despite having different image IDs.**  
   [kitty.rs:592](frontends/sighurt-tui/src/kitty.rs:592), especially lines 611 and 626.

   Each chunk gets its own passthrough DCS. Only the first chunk identifies the image; continuation chunks contain `m` and `q`. Two animated panes can therefore produce `A-first, B-first, A-continuation…` in the outer terminal. An exiting pane’s delete can also interrupt another pane’s upload. Unique IDs do not make these transactions independent. The [kitty protocol explicitly requires completing one upload before sending another graphics command](https://sw.kovidgoyal.net/kitty/graphics-protocol/#remote-client).

   **Fix:** serialize complete transfers at tmux’s output boundary, or package each complete transfer into one bounded passthrough transaction. Account explicitly for tmux’s DCS size limit—potentially by tiling. A lock released when a pane’s PTY accepts bytes is insufficient.

3. **P2 — Repeated resize under backpressure can discard the tmux recovery sequence. Signal restoration also has the wrong parser state.**  
   [app.rs:2343](frontends/sighurt-tui/src/app.rs:2343), [output.rs:138](frontends/sighurt-tui/src/term/output.rs:138), [output.rs:305](frontends/sighurt-tui/src/term/output.rs:305).

   `Wire` does not understand tmux’s escaped inner APC. It can report `Ground` after the escaped inner terminator while the outer DCS remains open.

   **Failing scenario:** stop terminal reads midway through an upload, then resize twice. The first resize preserves the inner APC tail and appends the outer terminator plus upload-abort sequence. It consumes `outbound`, however, and those repair bytes are ordinary discardable output. The second resize can preserve only the inner terminator and discard the repair. Subsequent redraw bytes enter the unfinished outer DCS.

   A fatal/suspend cleanup at the false `Ground` boundary can similarly send restoration CSI sequences into that DCS.

   **Fix:** model the outer passthrough state and escape quoting in the output layer. Preserve recovery bytes across repeated cancellation, and use the same abort machinery for resize, leave and signal restoration.

4. **P2 — The 250 ms image hold measures time before transmission, not image lifetime.**  
   [app.rs:2018](frontends/sighurt-tui/src/app.rs:2018), [app.rs:2158](frontends/sighurt-tui/src/app.rs:2158), [app.rs:2273](frontends/sighurt-tui/src/app.rs:2273).

   `image_written` receives the timestamp captured before presentation, encoding and output. `on_rows` also becomes true before the upload drains.

   **Failing scenario:** an upload takes 400 ms through a slow terminal; the user switches tabs meanwhile. When the final bytes drain, the 250 ms deadline has already expired, so deletion/replacement can follow immediately. The claimed protection against Rio’s upload/delete crash has effectively disappeared.

   **Fix:** track completion of the image’s output span and start the hold there. Even that establishes only PTY-write completion: an arbitrarily stalled terminal can process upload and deletion together later. A guaranteed GPU lifetime requires terminal feedback or fixing Rio; this timer is only a mitigation.

5. **P2 — Image-ID allocation silently abandons its uniqueness guarantee on failure.**  
   [app.rs:335](frontends/sighurt-tui/src/app.rs:335).

   Missing runtime directory, an open failure, or exhausting the claim attempts returns a usable but **unclaimed** ID.

   **Failing scenario:** two same-user processes without `XDG_RUNTIME_DIR`, with PIDs congruent modulo 255, use the same image. One overwrites the other; exiting deletes the other’s picture. This is possible on one machine, independently of the documented SSH limitation.

   **Fix:** obtain a lock in a verified private fallback directory, or refuse/fall back from kitty-through-tmux when no claim is held. Do not silently return an unclaimed ID. Keeping the lock files in place is correct: unlinking them on release would introduce an inode-lock race.

6. **P2 — Kitty placeholder state survives tmux reattachment; its pixels do not.**  
   [app.rs:2063](frontends/sighurt-tui/src/app.rs:2063), [app.rs:2137](frontends/sighurt-tui/src/app.rs:2137), [app.rs:1282](frontends/sighurt-tui/src/app.rs:1282).

   **Failing scenario:** display a static page, detach, then attach from another kitty terminal with the same pane dimensions. Tmux redraws its stored placeholder cells, but the new terminal has never received the image. There need be no resize or `SIGCONT`; focus activation does not invalidate `on_screen` or the stored fingerprint. The page can remain blank until another event forces a genuine redraw.

   **Fix:** introduce an explicit client/terminal-generation invalidation and reupload path, preferably driven by tmux attach hooks or equivalent notification. Focus-triggered reupload is a useful partial fallback. Placeholder preservation alone is not image preservation.

7. **P2 — WPE refresh changes depend on a subsequent presentation.**  
   [window.rs:456](frontends/sighurt-wayland/src/window.rs:456), [runtime.rs:495](engines/sighurt-engine-wpe/src/runtime.rs:495), [runtime.rs:2283](engines/sighurt-engine-wpe/src/runtime.rs:2283).

   The host changes the lease’s rate, but the adapter synchronizes it only when opening or presenting a view.

   **Failing scenario:** a page continuously requests animation frames without changing pixels. Move its window from 60 Hz to a same-scale 120 Hz output. No presentation is needed, so the observer can retain 60 Hz indefinitely.

   **Fix:** propagate refresh changes as an engine-visible event independent of painting. Also handle WebKit’s cached nominal display rate; changing the observer period alone does not refresh the existing display-link metadata.

8. **P2, smooth-scroll enabled — e30988e’s old-pointer fix is defeated by Servo’s wheel coalescer.**  
   [input.rs:121](engines/sighurt-engine-servo/src/input.rs:121), [view.rs:363](engines/sighurt-engine-servo/src/view.rs:363), [document_event_handler.rs:262](third_party/servo-script/dom/document/document_event_handler.rs:262).

   The adapter preserves old coordinates, but Servo merges pending wheel events solely by delta mode, sums their deltas, and replaces their coordinates with the latest event’s point.

   **Failing scenario:** scroll container A, move to B and scroll before A finishes, then hide the view before the next step. `finish()` sends B’s remainder followed by A’s owed remainder in immediate succession. Servo can merge both and deliver the total at A. A busy script thread can produce the same failure during normal stepping.

   **Fix:** prevent coalescing across different wheel targets/coordinates and preserve event ordering. Drain older owed events before the current run in `finish()`. Verify with two actual scroll containers; the isolated `SmoothScroll` tests cannot establish this property.

9. **P2, smooth-scroll enabled — The monotonicity fix permits fractional overshoot and still permits reversal on `finish()`.**  
   [input.rs:199](engines/sighurt-engine-servo/src/input.rs:199), [input.rs:231](engines/sighurt-engine-servo/src/input.rs:231).

   Intermediate positions are rounded to whole pixels, then clamped against the amount already sent, without clamping against the total.

   **Failing scenario:** wheel delta `1` at 125% scale becomes `-0.625` device pixels. An intermediate step rounds to `-1`. Natural completion cannot return to `-0.625`, so it overscrolls; hiding before completion makes `finish()` emit `+0.375`, reversing direction.

   **Fix:** bound each cumulative position between zero and the requested total as well as enforcing monotonicity, or retain fractional positions throughout. The shrinking-lookahead case from the earlier review is fixed; the general “never reverse, preserve distance” claim is not.

10. **P2, measurement correctness — Native presentation latency excludes swap time and can report a false zero.**  
    [targets.rs:579](frontends/sighurt-wayland/src/targets.rs:579), [scheduler.rs:263](frontends/sighurt-wayland/src/scheduler.rs:263).

    The new diagnostic exposes a timestamp recorded when the permit is dropped, after the native swap returns.

    **Failing scenario:** the swap blocks, or the owner is descheduled between swap completion and permit destruction. The compositor may already have displayed the frame before `submitted_ns` is stored. `saturating_sub` turns that into zero latency. SHM and native measurements also start at different phases.

    **Fix:** record immediately before the operation that submits/commits, record its return separately, and report invalid timestamp ordering instead of converting it to a successful zero.

11. **P3 — A lower `input-fps` can remain effective beyond the one-second window.**  
    [app.rs:1040](frontends/sighurt-tui/src/app.rs:1040), [app.rs:2284](frontends/sighurt-tui/src/app.rs:2284).

    The timeout calculation does not include the input-window expiry, although configuration explicitly permits `input-fps < fps`.

    **Failing scenario:** `fps=60`, `input-fps=1`, last input at `t=0`, last presentation at `t=0.9`, another frame waiting. The calculated deadline is `t=1.9`; absent another wakeup, normal pacing does not resume at `t=1`.

    **Fix:** include expiry when it would accelerate pending work, or constrain input pacing to be at least the background rate.

Decision 145’s measurements support **removing the artificial output-rate bottleneck on that local demo workload**. They do not establish input-to-visible-response latency. [relay-post.py:42](/data/sig-builds/research/scripts/relay-post.py:42) associates each input with the next image’s arrival, without checking whether that image contains the input’s result. Multiple inputs can share a frame, and a frame computed before an input can arrive afterward. [relay.py:54](/data/sig-builds/research/scripts/relay.py:54) timestamps successful writes to the outer PTY, not Rio decoding, rendering or displaying the image.

The sixel numbers are almost exactly the waiting-time distribution expected from changing a 200 ms period to 33.3 ms: approximately 100/180 ms median/p90 becomes 16.7/30 ms, plus overhead. That is useful evidence of cap removal. It does not separately establish faster encoding, terminal rendering or correct responses.

Decision 146 supports **higher synthetic submission throughput**, not “input renders at once.” [analyze.py:44](/data/sig-builds/research/analyze.py:44) counts frame events; its nearest-next-frame calculation likewise does not establish causality. Turning four wheel notches per second into roughly seven animation steps each largely explains the 4→25.3 fps result. That is additional animation work, not independently a speedup. The 10→33% CPU increase matters. The measurements also combine timer changes with smoothing, whereas current HEAD defaults smoothing off.

I would measure baseline, timer-only, smoothing-only and both, including current defaults. Each input should produce a uniquely identifiable visual result, with timestamps for dispatch, DOM handling, submission and actual presentation. Report p50/p95/p99, missed responses, discarded frames, scroll distance and settling time. Include total CPU across browser, engine children, tmux, terminal and compositor; bytes/sec; and encode/drain tails. Repeat on actual 60/120 Hz displays, output changes, realistic high-entropy pages, constrained links, stopped readers, concurrent panes and prolonged resize/tab churn.

The design choices I would change are:

- **Input pacing:** [app.rs:1215](frontends/sighurt-tui/src/app.rs:1215) boosts on every key, mouse event and paste before knowing whether it affects the page. Typing in chrome can therefore accelerate an unrelated animation for a full second. I would boost pending work associated with page interaction, then decay according to outstanding work and measured output capacity.
- **Compression:** level 1 is defensible for local owner-thread latency, but the blanket justification at [kitty.rs:158](frontends/sighurt-tui/src/kitty.rs:158) is too narrow. The repository already records a document expanding from **40 KB to 619 KB** at level 1 in [HYPOTHESES.md:574](bench/HYPOTHESES.md:574). Even the newer 102 KB increase consumes the claimed 12.9 ms saving below roughly 8 MB/s. I would retain a configurable level and choose any adaptive policy from encode-plus-drain measurements across representative pages.
- **Kitty through tmux:** opt-in is appropriate while capabilities and replies are unavailable. Requiring `all` avoids one hidden-pane loss mode, but it does not solve concurrent transfers or client replacement. Those need lifecycle and transport handling before presenting this as dependable multi-pane support.
- **Servo timer and smoothing:** the 4 ms patch is a plausible targeted workaround, but it accelerates a rendering opportunity rather than guaranteeing prompt input dispatch; busy JavaScript remains blocking. I would pursue upstream input scheduling and avoid repeatedly cancelling an already suitable timer. Smoothing belongs after wheel dispatch/cancellation and scroll-target selection inside the engine. Synthesizing seven DOM wheel events changes page-observable behavior, cancellation and targeting. Keeping it off by default is the right current decision.
- **WPE screen geometry:** [runtime.rs:1078](engines/sighurt-engine-wpe/src/runtime.rs:1078) feeds view dimensions into global screen metadata. A narrow first view can make later pages select a small-screen layout on a large monitor. I would supply actual output geometry and propagate changes, or deliberately define stable virtual-screen metrics. Process-start timing and whichever view supplied dimensions should not determine the reported display.

I found no new unconditional idle tick, unbounded frame accumulation, or normal Servo GPU-to-CPU page copy in the reviewed speed paths. The terminal output cap and pending-output gate prevent an indefinitely non-reading terminal from accumulating page frames or triggering frame-deadline polling. They provide backpressure only as far as the PTY/tmux reader, however—not proof that the terminal has displayed anything.

The WebGL bookkeeping patch predates this review range. Its guards suppress the observed assertions; they do not establish correct buffer ownership or a bounded leak. The claim in [SIGHURT-PATCH.md:19](third_party/servo-script/SIGHURT-PATCH.md:19) is stronger than the demonstrated result. I would retain explicit diagnostics and a reproducer until the attachment imbalance itself is fixed.