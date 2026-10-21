# Website compatibility 2026-10-21 (servo07-22ffede)

`sig 0.7.0 (Sighurt, based on Oxide by Nikhil Ranjan)` on louise. Headless sway, GLES2, 1600×1000, scale 1, one private compositor and D-Bus per run. 1 repetition(s).

A cell is `class · load · script/engine errors · non-blank · RSS`. Load is process start until the tab stopped loading (the 3 s settle is extra). RSS is sig's peak; a WPE cell adds the sampled peak of WebKit's processes (`sig+webkit`).

| Site | Category | Servo | WPE |
|---|---|---|---|
| example | control | OK · 2.0s · 0 err · 3% · 302 MiB | — |
| github-servo | code | OK · 2.4s · 0 err · 19% · 507 MiB | — |
| gitlab | code | OK · 2.0s · 0 err · 48% · 479 MiB | — |
| chatgpt | assistant | OK · 2.4s · 0 err · 14% · 425 MiB | — |
| google-search | search | OK · 2.0s · 0 err · 3% · 414 MiB | — |
| duckduckgo | search | OK · 2.0s · 0 err · 16% · 479 MiB | — |
| youtube | video | OK · 2.0s · 0 err · 49% · 610 MiB | — |
| twitch | video | OK · 2.0s · 0 err · 77% · 604 MiB | — |
| netflix | video | OK · 2.0s · 0 err · 91% · 640 MiB | — |
| wikipedia | reference | OK · 2.0s · 0 err · 16% · 382 MiB | — |
| mdn-grid | docs | OK · 2.0s · 0 err · 25% · 383 MiB | — |
| docsrs | docs | OK · 2.0s · 0 err · 18% · 350 MiB | — |
| crates | docs | OK · 2.0s · 0 err · 51% · 363 MiB | — |
| w3schools | tutorial | OK · 2.0s · 0 err · 65% · 391 MiB | — |
| codepen | tutorial | OK · 2.0s · 0 err · 80% · 514 MiB | — |
| caniuse | features | OK · 2.5s · 0 err · 59% · 405 MiB | — |
| rustlang | language | OK · 2.0s · 0 err · 50% · 318 MiB | — |
| reddit-rust | social | OK · 2.0s · 0 err · 49% · 443 MiB | — |
| x | social | OK · 2.0s · 0 err · 21% · 489 MiB | — |
| linkedin | social | OK · 2.0s · 0 err · 23% · 329 MiB | — |
| instagram | social | OK · 2.7s · 0 err · 36% · 505 MiB | — |
| stackoverflow | qa | BLANK · 2.0s · 0 err · 0% · 257 MiB | — |
| hn | news | OK · 2.0s · 0 err · 25% · 306 MiB | — |
| bbc | news | OK · 2.0s · 0 err · 43% · 453 MiB | — |
| nytimes | news | OK · 2.0s · 0 err · 5% · 397 MiB | — |
| guardian | news | OK · 2.0s · 0 err · 82% · 499 MiB | — |
| medium | news | OK · 2.0s · 0 err · 10% · 486 MiB | — |
| amazon | commerce | BLANK · 2.0s · 0 err · 0% · 274 MiB | — |
| steam | commerce | OK · 2.0s · 0 err · 89% · 615 MiB | — |
| apple | corporate | OK · 2.0s · 0 err · 31% · 483 MiB | — |
| microsoft | corporate | OK · 3.4s · 0 err · 73% · 524 MiB | — |
| cloudflare | infrastructure | OK · 2.0s · 0 err · 44% · 483 MiB | — |
| gmail | mail | OK · 2.0s · 0 err · 29% · 390 MiB | — |
| gmaps | maps | OK · 2.0s · 0 err · 4% · 344 MiB | — |
| osm | maps | OK · 2.0s · 0 err · 74% · 390 MiB | — |
| discord | chat | OK · 2.0s · 0 err · 87% · 753 MiB | — |
| notion | productivity | TIMEOUT · — · 0 err · 26% · 449 MiB | — |
| figma | design | OK · 2.0s · 0 err · 0% · 518 MiB | — |
| spotify | media | OK · 2.0s · 0 err · 6% · 514 MiB | — |
| archive | archive | OK · 2.4s · 0 err · 36% · 370 MiB | — |

## Per engine

| Engine | OK | PARTIAL | BLANK | CRASH | TIMEOUT | ERROR | median load s | median RSS MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| servo | 37 | 0 | 2 | 0 | 1 | 0 | 2.0 | 446 |
| wpe | 0 | 0 | 0 | 0 | 0 | 0 | — | — |

## Servo failures by first error

### 2 · (BLANK, no script error in the log)

stackoverflow (BLANK), amazon (BLANK)

### 1 · (TIMEOUT, no script error in the log)

notion (TIMEOUT)

## Harness limits

- The diagnostics JSON never includes an address or a title. Those two fields come from the session file. The run starts with session restore off and turns it on only after the screenshot, so a previous session is never restored; sig writes the snapshot because restore then applies live.
- Servo's delegate drops page console messages (they are page data and go nowhere). Counted log lines are script::script_runtime errors, other ERROR lines, panics and fatal signals, and WebKit crash or exception text that reaches stderr. A JS exception such as adoptedStyleSheets being undefined is absent unless Servo also logs or panics on it. WebKit's `ERROR: invalid option: JSC_SIGNAL_FOR_GC` lines are ignored: sig sets that variable so JavaScriptCore does not take SIGUSR1, and WebKit prints the rejection once per process.
- A WPE run still starts Servo when Servo is compiled in and enabled, because that is how sig installs its logger. sig's RSS on those runs includes Servo.
- The non-blank fraction is pixels of the page rectangle (84 px of chrome at the top, 20 px of status at the bottom; the default chrome at font size 14 and scale 1) that differ from that rectangle's dominant colour. Every pixel is sampled.
- Load time includes engine startup. The first diagnostics poll waits 2 s so SIGUSR1 cannot arrive before the handler, so a page that finished earlier is recorded at about 2 s. After that the flag is polled about every 0.3 s. CPU misses up to one 0.2 s sample of a process that exits between samples. WebKit's RSS peak is that sample's sum, not each process's high-water mark.
- Each run is one public GET. A consent wall or a login form is the page that was measured. Servo can leave the tab's loading flag set after the page is visibly complete, and that run stays TIMEOUT.
