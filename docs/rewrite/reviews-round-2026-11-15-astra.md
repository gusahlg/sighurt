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
