# Terminal I/O prototype (phase 0c)

Recommendation: a small parser of our own, fed from a non-blocking fd the owner loop already has. `vte` 0.15, `termwiz` 0.23's `InputParser`, and `crossterm` 0.29 were compared on the bytes a terminal sends back. The prototype in this crate is that parser plus raw mode, the startup probe, a cell grid, and bounded writes.

Cells marked **M** were executed by `tools/cargo test -p tty-io`. Cells marked **S** are from that crate's source and were not re-executed here. `vte` has no key or reply type: its `Perform` callbacks are the whole API, and the comparison performer implements that trait.

## Coverage

| Input | vte 0.15 | termwiz 0.23 `InputParser` | crossterm 0.29 | ours |
|---|---|---|---|---|
| Kitty `CSI u`, Unicode key (`CSI 97 u` → "a") | Raw CSI. **M** for the same CSI path (DA1, cell size). Params are `u16`. | One `Key(Char('a'))`. **M** | Public `Char('a')`. **M** | One press, evdev from the base layout when present. **M** |
| Shifted and base-layout alternates (`97:65:97`) | Raw CSI; a codepoint above `u16::MAX` saturates (128512 → 65535). **M** | Not one shifted key. **M** | Public event uses the shifted character and drops Shift. No field for the base-layout key. **M** for `Char('A')`; the missing field is **S** (`parse_csi_u_encoded_key_code` never stores the third subparameter). | Key "A", text "A", Shift consumed, evdev `KEY_A` (30) from the base layout. **M** |
| Modifiers | Raw CSI parameters. **M** (DA1 params). | Modifier bitmask on plain keys. Colon event-type fields are not modifiers. **M** | Kitty modifier bits, including release. **M** | Kitty bits, including the consumed subset. **M** |
| Press / repeat / release | Not an event type. | `CSI 97;1:3 u` is Alt+`[` plus the characters `97;1:3u`, not a release. **M** | `Release` is reported. **M** | Press, repeat, and release stay distinct. No synthetic release. **M** |
| Associated text | Not separated from parameters. | Not applied (the colon form is not one key). **M** | Not applied: `CSI 97;1;66 u` does not produce `Char('B')`. **M**. The text subparameters are ignored. **S** | Text is `KeyEvent.text`. Control or logo suppresses it. **M** |
| Legacy arrows | Raw CSI. | `CSI A` is Up. **M** | `KeyCode::Up` and the other arrows. **S** | `ArrowUp`, evdev 0, plus a synthetic release. **M** |
| Legacy F-keys | Raw CSI or SS3. | `CSI 15 ~` and `SS3 P` are non-empty key events. Exact code not asserted. **M** | `F(n)` from `~` and from SS3 `P`–`S`. **S** | `F5` from `CSI 15 ~`, `F1` from `SS3 P`, synthetic release. **M** |
| ESC-prefixed Alt | Would be an escape plus a print. | `ESC a` is a non-empty key event. **M** | Alt on the following key. **S** | Alt+"a", no text, synthetic release. **M** |
| C0 controls | `execute`. | `0x01` is a non-empty key event. **M** | `0x01`–`0x1a` are Ctrl+letter. **S** | Ctrl+"a", no text, synthetic release. **M** |
| Tab / Ctrl-I | Both are byte `0x09` on a legacy terminal. | Byte `0x09` is a key. **M**. Kitty's two sequences were not asserted. | Byte `0x09` is Tab, so Ctrl-I is the same key. **S** | Kitty `CSI 9 u` is Tab (evdev 15). Kitty `CSI 105;5 u` is Ctrl+i (evdev 0). Legacy `0x09` is ambiguous Tab plus a release. **M** |
| Enter / Ctrl-M | Both are byte `0x0d`. | Byte `0x0d` is a key. **M** | Byte `0x0d` is Enter. **S** | Legacy `0x0d` is ambiguous Enter plus a release. **M** |
| Lone Escape | A short delay is the caller's problem. | With `parse_as_vec(..., false)`, a lone ESC is a key immediately. **M** | Esc when the parser is told no more input is waiting. **S** | Held until `expire` (25 ms). A partial CSI uses a 1 s timer and is not turned into keys. **M** |
| SGR mouse (`CSI < … M`, 1006) | Raw CSI. No mouse callback. | Button and cell. **M** (`<0;3;4M` is left at 3,4). | `parse_csi_sgr_mouse`. **S** | Press, release, and wheel. **M** |
| Bracketed paste (`200~` / `201~`) | Two CSIs plus printed body. No paste callback. | One `Paste`. **M** | `parse_csi_bracketed_paste` (default feature). **S** | One paste, including bytes that look like keys. **M** |
| DA1 (`CSI ? 62 ; 4 c`, sixel = 4) | Raw CSI, intermediate `?`, param 4. **M** | Unstructured keys. **M** | Parsed as an internal `PrimaryDeviceAttributes` and not returned from `event::read`. **M** (the public stream has neither `PrimaryDevice` nor `sixel`). | `Reply::Da1 { sixel: true }`. **M** |
| Kitty keyboard query (`CSI ? flags u`) | Raw CSI. | Unstructured keys. **M** | Internal `KeyboardEnhancementFlags`, not a public event. **S** | `Reply::KittyKeyboard`. **M** |
| Kitty graphics (`APC _G i=…;OK ST`) | Payload dropped: no `OK`, no `G`, no `put`. **M** | Unstructured keys. **M** | No APC arm in `parse_event`. **S** | `Reply::KittyGraphics { ok: true }`. **M** |
| XTGETTCAP (`DCS 1 + r … ST`) | `hook` / `put` / `unhook`, payload kept. **M** | Unstructured keys. **M** | No DCS arm. **S** | `Reply::XtGetTcap`, hex names decoded (`RGB` = `rgb`). **M** |
| OSC 10 / 11, BEL and ST | BEL: one `osc_dispatch`, bell set. **M**. ST: payload kept, and the `\` of ST is also an `Esc` (a spurious key if escapes become keys). **M** | Both forms are unstructured keys. **M** | No OSC arm. **S** | `Reply::OscColor` for 10 and 11, `bell` set only for BEL. **M** |
| Cell / pixel size (`CSI 16 t` → `CSI 6 ; h ; w t`, `CSI 14 t` → `CSI 4 ; h ; w t`) | Raw CSI params, `u16`. **M** for both reply forms. | Unstructured keys. **M** | Final byte `t` is not a size reply. **S** | `CellPixels` and `TextAreaPixels`. **M** |
| Calloop / app-owned fd | Caller feeds bytes (`advance`). Does not read the fd. **M** | Caller feeds bytes (`parse_as_vec`). Does not read the fd. **M** | `event::poll` / `event::read` own stdin. The pty test is a child whose stdin is the slave. **M** | `InputSource` reads a non-blocking fd the app registered. Partial CSI arms a 1 s timer, not the 25 ms Escape timer. **M** |

## Why ours

`vte` is the right shape (push bytes in, get callbacks out) and keeps OSC and DCS, which is what TUI.md §4 assumed. It drops APC contents, so a kitty graphics reply never arrives, and it stores CSI parameters as `u16`, so a kitty key code or an emoji saturates at 65535. An ST-terminated OSC also leaks a trailing escape.

`termwiz::input::InputParser` does not pull in the widget crate, and it does not own the fd, so it can sit under calloop. It understands a plain `CSI u` key, SGR mouse, and bracketed paste. It does not understand kitty's colon fields (shifted key, base layout, event type, associated text): a release is typed as characters. It has no reply type, so DA1, the keyboard query, graphics, XTGETTCAP, OSC colours, and window reports come back as keys. A probe cannot tell a late DA1 from a keypress.

`crossterm` 0.29 parses more of the kitty keyboard protocol than `termwiz` (Unicode scalar, shifted key, press/repeat/release) and it does classify DA1 and the keyboard-query reply internally. Those replies are filtered out of `event::read`, and graphics, XTGETTCAP, OSC, and cell size are not replies at all. Associated text and the base-layout key are dropped, so there is no evdev code for `code:` bindings. The blocking objection is the one in TUI.md §4 and §8.7: `poll`/`read` own stdin. The browser's owner loop is calloop and already reads the fd when it is readable.

The parser here is a few hundred lines with no extra crate. Parameters are `u32`. APC, DCS, and OSC bodies are kept. Replies stay replies if they arrive after the probe deadline, which is the §8.8 requirement. `InputSource` is a calloop source over an fd the caller owns; the source never puts the terminal into raw mode and never reads a cooked fd's blocking `read`.

Licenses of the crates linked for the comparison and the prototype: `vte` MIT, `termwiz` MIT, `crossterm` MIT, `unicode-width` Unicode-3.0, `unicode-segmentation` Unicode-3.0, `rustix` Apache-2.0 OR MIT, `libc` MIT OR Apache-2.0, `calloop` MIT. All of those are AGPL-3.0-compatible. The prototype does not use `crossterm`; the demo links it only so `--worker crossterm` can run.

## What the prototype does

`Session::enter` saves termios and the file status flags, sets raw mode and `O_NONBLOCK`, and writes one enter sequence: alternate screen (1049), cursor hide, bracketed paste, kitty keyboard push with flags `1|2|4|8|16` (`CSI > 31 u`), SGR mouse (1000 and 1006). `restore` writes the matching leave sequence, puts the saved termios and flags back, and is a no-op the second time. `Drop` restores too. `prepare_forced_exit` is the same restore for the moment before `_exit`.

The panic hook restores only when the panicking thread is the one that called `enter`. A helper thread's panic used to clear `O_NONBLOCK` and cooked-mode the pty under the owner, and the next `read` blocked. `set_hook` is not called while unwinding; Rust aborts the process if it is. Reads call `poll` with the time still left on the deadline and put `O_NONBLOCK` back before `read`.

`SIGTSTP` restores, sets the default action, unblocks the signal, then `raise`s. The process stops inside `raise`. `SIGCONT` re-enters raw mode and the enter sequence; only then is the `SIGTSTP` handler installed again. Installing it earlier delivers the pending stop back to the handler, and the process never stops. `SIGHUP` restores, unblocks, and re-raises so the default action terminates the process.

The probe writes one batch, DA1 last (`CSI ? u`, graphics query, XTGETTCAP `RGB`, OSC 10 and 11, `CSI 16 t`, `CSI 14 t`, `CSI c`). It returns when DA1 has been seen or the deadline passes (default 150 ms). Missing, late, and malformed replies are tolerated. A late DA1 is still a `Reply` on the next read.

The grid stores grapheme clusters and `unicode-width` widths, default / 256 / 24-bit colours, and bold, italic, underline, reverse. `diff` emits only the cells that changed, inside synchronized output (`CSI ? 2026`), with a cursor move and an SGR change when the pen differs. A wide character cut by the right edge becomes a space. A wide character's continuation is cleared when something overwrites it.

`Output` keeps at most `cap` bytes. A frame that does not fit is dropped and counted. Writes use non-blocking `write` and return on `EAGAIN`.

Keys match the shape of `sighurt_api::input::KeyEvent` (`code`, `key`, `state`, `repeat`, `modifiers`, `consumed`, `text`). This crate does not depend on `sighurt-api`. `code` is the evdev code from the kitty base-layout key through a US QWERTY table, or 0 when that alternate is absent. Legacy presses have code 0 and a synthetic release immediately after. `ambiguous` is extra: legacy Tab/Ctrl-I, Enter/Ctrl-M, and Backspace/Ctrl-H share a byte.

The `tty-io` binary with no arguments probes a real terminal and leaves on `q`, Escape, or Ctrl-C. `--probe-out PATH` writes the probe. `--worker` is the child the pty tests drive.

## Tests

Final run, default (parallel) test threads, exit 0:

```text
tools/cargo fmt -p tty-io && tools/cargo clippy -p tty-io --all-targets -- -D warnings && tools/cargo test -p tty-io
```

| Target | Result |
|---|---|
| `tty-io` lib | 18 passed |
| `tty-io` bin | 0 tests |
| `tests/coverage.rs` | 6 passed |
| `tests/pty.rs` | 14 passed, 0.66 s |
| doc-tests | 0 tests |

Clippy was clean (`-D warnings` on all targets). After that run, `ps` showed no `tty-io --worker` and no `target/debug/tty-io`.

Pty coverage, all measured in that run: golden bytes for enter, one changed cell (`CSI ? 2026 h`, cup, bold 24-bit SGR, `A`, `CSI ? 2026 l`), a wide hiragana, and a second `restore` that writes nothing and puts termios and flags back; probe answers that are immediate, late (still a reply, not a key), absent (returns at the deadline), and malformed (a later `q` is a press and a synthetic release); a stalled master does not block the writer and the pending buffer stays within the cap; termios restored after a normal child exit, a panic in the child (panic text on stderr, not on the pty), `SIGTSTP`/`SIGCONT`, `SIGHUP`, and `prepare_forced_exit` before `_exit`; crossterm's public events as in the table; calloop reading the app-owned fd; a helper-thread panic leaves raw mode and `O_NONBLOCK` in place and the probe still returns inside its deadline.

Every pty test arms an 8 s watchdog. On expiry the watchdog `SIGKILL`s children that test spawned, and the test thread fails on its next poll. `PtyChild`'s own waits kill the child at 2 s. Reply helpers do not panic: a panic on that thread used to run the session hook.

## Not done

`foot` was not run. The optional check is `tools/headless-sway -- foot -e target/debug/tty-io` (or `--probe-out`) inside the private compositor. It was skipped so the required test and clippy runs, and this note, could finish.
