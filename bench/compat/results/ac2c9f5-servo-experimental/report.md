# Website compatibility 2026-10-21 (ac2c9f5-experimental)

`sig 0.7.0 (Sighurt, based on Oxide by Nikhil Ranjan)` on louise. Headless sway, GLES2, 1600×1000, scale 1, one private compositor and D-Bus per run. 1 repetition(s).

A cell is `class · load · script/engine errors · non-blank · RSS`. Load is process start until the tab stopped loading (the 3 s settle is extra). RSS is sig's peak; a WPE cell adds the sampled peak of WebKit's processes (`sig+webkit`).

| Site | Category | Servo | WPE |
|---|---|---|---|
| example | control | OK · 2.0s · 0 err · 3% · 296 MiB | — |
| github-servo | code | OK · 2.0s · 0 err · 30% · 525 MiB | — |
| gitlab | code | OK · 2.0s · 0 err · 48% · 516 MiB | — |
| chatgpt | assistant | OK · 2.0s · 0 err · 14% · 398 MiB | — |
| google-search | search | OK · 2.0s · 0 err · 3% · 420 MiB | — |
| duckduckgo | search | OK · 4.1s · 0 err · 16% · 485 MiB | — |
| youtube | video | PARTIAL · 2.0s · 6 err · 30% · 604 MiB | — |
| twitch | video | OK · 2.0s · 0 err · 56% · 561 MiB | — |
| netflix | video | OK · 2.4s · 0 err · 91% · 666 MiB | — |
| wikipedia | reference | TIMEOUT · — · 0 err · 15% · 387 MiB | — |
| mdn-grid | docs | PARTIAL · 2.0s · 5 err · 29% · 408 MiB | — |
| docsrs | docs | OK · 2.0s · 0 err · 18% · 352 MiB | — |
| crates | docs | PARTIAL · 2.0s · 1 err · 51% · 348 MiB | — |
| w3schools | tutorial | OK · 2.0s · 0 err · 64% · 412 MiB | — |
| codepen | tutorial | TIMEOUT · — · 0 err · 80% · 487 MiB | — |
| caniuse | features | PARTIAL · 3.5s · 1 err · 59% · 410 MiB | — |
| rustlang | language | OK · 2.0s · 0 err · 50% · 318 MiB | — |
| reddit-rust | social | PARTIAL · 2.0s · 33 err · 42% · 767 MiB | — |
| x | social | OK · 2.0s · 0 err · 21% · 633 MiB | — |
| linkedin | social | OK · 2.0s · 0 err · 4% · 620 MiB | — |
| instagram | social | OK · 2.7s · 0 err · 36% · 552 MiB | — |
| stackoverflow | qa | OK · 3.6s · 0 err · 48% · 492 MiB | — |
| hn | news | OK · 2.0s · 0 err · 25% · 303 MiB | — |
| bbc | news | OK · 2.0s · 0 err · 67% · 514 MiB | — |
| nytimes | news | TIMEOUT · — · 0 err · 5% · 472 MiB | — |
| guardian | news | OK · 2.0s · 0 err · 82% · 519 MiB | — |
| medium | news | PARTIAL · 2.0s · 0 err · 17% · 542 MiB | — |
| amazon | commerce | OK · 2.0s · 0 err · 82% · 548 MiB | — |
| steam | commerce | OK · 2.0s · 0 err · 89% · 633 MiB | — |
| apple | corporate | OK · 2.0s · 0 err · 31% · 507 MiB | — |
| microsoft | corporate | OK · 4.5s · 0 err · 73% · 546 MiB | — |
| cloudflare | infrastructure | OK · 2.0s · 0 err · 44% · 447 MiB | — |
| gmail | mail | OK · 2.0s · 0 err · 29% · 398 MiB | — |
| gmaps | maps | OK · 2.0s · 0 err · 4% · 356 MiB | — |
| osm | maps | OK · 2.0s · 0 err · 74% · 398 MiB | — |
| discord | chat | TIMEOUT · — · 1 err · 86% · 816 MiB | — |
| notion | productivity | TIMEOUT · — · 0 err · 3% · 513 MiB | — |
| figma | design | OK · 2.0s · 0 err · 47% · 581 MiB | — |
| spotify | media | OK · 2.0s · 0 err · 51% · 657 MiB | — |
| archive | archive | OK · 3.1s · 0 err · 36% · 373 MiB | — |

## Per engine

| Engine | OK | PARTIAL | BLANK | CRASH | TIMEOUT | ERROR | median load s | median RSS MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| servo | 29 | 6 | 0 | 0 | 5 | 0 | 2.0 | 499 |
| wpe | 0 | 0 | 0 | 0 | 0 | 0 | — | — |

## Servo failures by first error

### 4 · (TIMEOUT, no script error in the log)

wikipedia (TIMEOUT), codepen (TIMEOUT), nytimes (TIMEOUT), notion (TIMEOUT)

### 3 · Unhandled promise rejection: (new ReferenceError("IntersectionObserver is not defined", "<url> N))

mdn-grid (PARTIAL), crates (PARTIAL), caniuse (PARTIAL)

```
[2026-10-21T05:50:06Z ERROR script::script_runtime] Unhandled promise rejection: (new ReferenceError("IntersectionObserver is not defined", "https://developer.mozilla.org/static/client/index.49937252c2edb888.js", 276))
```

### 1 · (PARTIAL, no script error in the log)

medium (PARTIAL)

### 1 · Unhandled promise rejection: (new ReferenceError("requestIdleCallback is not defined", "<url> N))

discord (TIMEOUT)

```
[2026-10-21T05:56:48Z ERROR script::script_runtime] Unhandled promise rejection: (new ReferenceError("requestIdleCallback is not defined", "https://discord.com/w/assets/3a756378428a770f27e63b63edc9fd6cc02fda3a/index-consolidated.js", 87))
```

### 1 · Unhandled promise rejection: (new TypeError("k.wrapped.openCursor is not a function", "<url> N))

youtube (PARTIAL)

```
[2026-10-21T05:48:41Z ERROR script::script_runtime] Unhandled promise rejection: (new TypeError("k.wrapped.openCursor is not a function", "https://www.youtube.com/s/_/ytmainappweb/_/js/k=ytmainappweb.kevlar_base.en_US.UlHPeiH7eiw.es5.O/am=AAAAgAAACEBK/d=1/br=1/rs=AGKMywHCMh7BUkq4p1Yzkx_p63iUma_TTA/m=kevlar_base_module,kevlar_main_module,kevlar_base_sync_mod_chunk", 8210))
```

### 1 · Unhandled promise rejection: OperationError: fetchStart hasn't been implemented. {}

reddit-rust (PARTIAL)

```
[2026-10-21T05:52:13Z ERROR script::script_runtime] Unhandled promise rejection: OperationError: fetchStart hasn't been implemented. {}
```

## Harness limits

- The diagnostics JSON never includes an address or a title. Those two fields come from the session file. The run starts with session restore off and turns it on only after the screenshot, so a previous session is never restored; sig writes the snapshot because restore then applies live.
- Servo's delegate drops page console messages (they are page data and go nowhere). Counted log lines are script::script_runtime errors, other ERROR lines, panics and fatal signals, and WebKit crash or exception text that reaches stderr. A JS exception such as adoptedStyleSheets being undefined is absent unless Servo also logs or panics on it. WebKit's `ERROR: invalid option: JSC_SIGNAL_FOR_GC` lines are ignored: sig sets that variable so JavaScriptCore does not take SIGUSR1, and WebKit prints the rejection once per process.
- A WPE run still starts Servo when Servo is compiled in and enabled, because that is how sig installs its logger. sig's RSS on those runs includes Servo.
- The non-blank fraction is pixels of the page rectangle (84 px of chrome at the top, 20 px of status at the bottom; the default chrome at font size 14 and scale 1) that differ from that rectangle's dominant colour. Every pixel is sampled.
- Load time includes engine startup. The first diagnostics poll waits 2 s so SIGUSR1 cannot arrive before the handler, so a page that finished earlier is recorded at about 2 s. After that the flag is polled about every 0.3 s. CPU misses up to one 0.2 s sample of a process that exits between samples. WebKit's RSS peak is that sample's sum, not each process's high-water mark.
- Each run is one public GET. A consent wall or a login form is the page that was measured. Servo can leave the tab's loading flag set after the page is visibly complete, and that run stays TIMEOUT.
