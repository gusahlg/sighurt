# Website compatibility 2026-10-22 (846b030)

`sig 0.8.0 (Sighurt, based on Oxide by Nikhil Ranjan)` on louise. Headless sway, GLES2, 1600×1000, scale 1, one private compositor and D-Bus per run. 1 repetition(s).

A cell is `class · load · script/engine errors · non-blank · RSS`. Load is process start until the tab stopped loading (the 3 s settle is extra). RSS is sig's peak; a WPE cell adds the sampled peak of WebKit's processes (`sig+webkit`).

| Site | Category | Servo | WPE |
|---|---|---|---|
| example | control | OK · 2.0s · 0 err · 3% · 292 MiB | OK · 2.0s · 0 err · 3% · 149+308 MiB |
| github-servo | code | OK · 2.0s · 0 err · 30% · 500 MiB | OK · 2.0s · 0 err · 29% · 149+599 MiB |
| gitlab | code | OK · 2.0s · 0 err · 86% · 485 MiB | OK · 2.0s · 0 err · 87% · 149+557 MiB |
| chatgpt | assistant | OK · 2.0s · 0 err · 20% · 549 MiB | OK · 2.0s · 0 err · 20% · 149+450 MiB |
| google-search | search | OK · 2.0s · 0 err · 57% · 352 MiB | OK · 2.0s · 0 err · 56% · 150+445 MiB |
| duckduckgo | search | OK · 2.4s · 0 err · 16% · 466 MiB | OK · 2.4s · 0 err · 25% · 150+709 MiB |
| youtube | video | OK · 2.0s · 0 err · 49% · 612 MiB | OK · 2.7s · 0 err · 53% · 150+965 MiB |
| twitch | video | OK · 2.0s · 0 err · 77% · 596 MiB | OK · 2.0s · 0 err · 77% · 149+843 MiB |
| netflix | video | OK · 2.0s · 0 err · 89% · 650 MiB | OK · 2.7s · 0 err · 89% · 149+781 MiB |
| wikipedia | reference | OK · 2.0s · 0 err · 16% · 373 MiB | OK · 2.0s · 0 err · 14% · 149+432 MiB |
| mdn-grid | docs | OK · 2.0s · 0 err · 26% · 395 MiB | OK · 2.0s · 0 err · 25% · 149+526 MiB |
| docsrs | docs | OK · 2.0s · 0 err · 18% · 339 MiB | OK · 2.0s · 0 err · 18% · 150+341 MiB |
| crates | docs | OK · 2.0s · 0 err · 51% · 352 MiB | OK · 2.0s · 0 err · 50% · 149+507 MiB |
| w3schools | tutorial | OK · 2.0s · 0 err · 65% · 383 MiB | OK · 2.0s · 0 err · 64% · 150+513 MiB |
| codepen | tutorial | OK · 2.0s · 0 err · 80% · 496 MiB | OK · 2.0s · 0 err · 85% · 149+827 MiB |
| caniuse | features | OK · 2.5s · 0 err · 59% · 398 MiB | OK · 3.1s · 0 err · 59% · 150+528 MiB |
| rustlang | language | OK · 2.0s · 0 err · 50% · 307 MiB | OK · 2.0s · 0 err · 52% · 150+381 MiB |
| reddit-rust | social | OK · 2.0s · 0 err · 42% · 929 MiB | OK · 3.1s · 0 err · 35% · 149+876 MiB |
| x | social | OK · 2.0s · 0 err · 21% · 499 MiB | OK · 2.0s · 0 err · 21% · 150+676 MiB |
| linkedin | social | PARTIAL · 2.0s · 1 err · 46% · 516 MiB | OK · 2.0s · 0 err · 46% · 149+629 MiB |
| instagram | social | OK · 3.1s · 0 err · 36% · 499 MiB | OK · 2.0s · 0 err · 35% · 150+583 MiB |
| stackoverflow | qa | OK · 2.4s · 0 err · 52% · 473 MiB | OK · 4.8s · 0 err · 51% · 150+855 MiB |
| hn | news | OK · 2.0s · 0 err · 25% · 295 MiB | OK · 2.0s · 0 err · 24% · 150+354 MiB |
| bbc | news | OK · 2.0s · 0 err · 67% · 482 MiB | OK · 2.0s · 0 err · 65% · 150+538 MiB |
| nytimes | news | OK · 2.0s · 0 err · 5% · 472 MiB | OK · 2.0s · 0 err · 5% · 149+473 MiB |
| guardian | news | OK · 2.0s · 0 err · 82% · 459 MiB | OK · 2.0s · 0 err · 81% · 149+616 MiB |
| medium | news | OK · 2.0s · 0 err · 17% · 516 MiB | OK · 2.0s · 0 err · 17% · 149+649 MiB |
| amazon | commerce | OK · 2.0s · 0 err · 82% · 522 MiB | OK · 3.4s · 0 err · 78% · 149+689 MiB |
| steam | commerce | OK · 2.0s · 0 err · 89% · 604 MiB | OK · 2.0s · 0 err · 89% · 150+705 MiB |
| apple | corporate | OK · 2.0s · 0 err · 31% · 485 MiB | OK · 2.4s · 0 err · 32% · 150+725 MiB |
| microsoft | corporate | OK · 2.4s · 0 err · 73% · 509 MiB | OK · 2.0s · 0 err · 73% · 150+740 MiB |
| cloudflare | infrastructure | OK · 2.0s · 0 err · 44% · 475 MiB | OK · 2.0s · 0 err · 88% · 150+752 MiB |
| gmail | mail | OK · 2.0s · 0 err · 29% · 379 MiB | OK · 2.0s · 0 err · 28% · 149+420 MiB |
| gmaps | maps | OK · 2.0s · 0 err · 4% · 338 MiB | OK · 2.0s · 0 err · 4% · 149+385 MiB |
| osm | maps | OK · 2.0s · 0 err · 74% · 382 MiB | OK · 2.4s · 0 err · 74% · 150+420 MiB |
| discord | chat | OK · 2.0s · 0 err · 86% · 828 MiB | OK · 3.1s · 0 err · 86% · 149+1185 MiB |
| notion | productivity | OK · 2.0s · 0 err · 42% · 495 MiB | OK · 2.0s · 0 err · 23% · 149+535 MiB |
| figma | design | OK · 2.0s · 0 err · 13% · 834 MiB | OK · 2.0s · 0 err · 39% · 149+929 MiB |
| spotify | media | OK · 2.0s · 0 err · 71% · 635 MiB | OK · 2.0s · 0 err · 71% · 149+782 MiB |
| archive | archive | OK · 2.0s · 0 err · 36% · 363 MiB | OK · 2.0s · 0 err · 68% · 150+542 MiB |

## Per engine

| Engine | OK | PARTIAL | BLANK | CRASH | TIMEOUT | ERROR | median load s | median RSS MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| servo | 39 | 1 | 0 | 0 | 0 | 0 | 2.0 | 483 |
| wpe | 40 | 0 | 0 | 0 | 0 | 0 | 2.0 | 149 |

## Servo failures by first error

### 1 · error loading script chrome://juggler/content (Blocked as mixed content)

linkedin (PARTIAL)

```
[2026-10-22T05:26:23Z ERROR script::dom::workers::workerglobalscope] error loading script chrome://juggler/content (Blocked as mixed content)
```

## Harness limits

- The diagnostics JSON never includes an address or a title. Those two fields come from the session file. The run starts with session restore off and turns it on only after the screenshot, so a previous session is never restored; sig writes the snapshot because restore then applies live.
- Servo's delegate drops page console messages (they are page data and go nowhere). Counted log lines are script::script_runtime errors, other ERROR lines, panics and fatal signals, and WebKit crash or exception text that reaches stderr. A JS exception such as adoptedStyleSheets being undefined is absent unless Servo also logs or panics on it. WebKit's `ERROR: invalid option: JSC_SIGNAL_FOR_GC` lines are ignored: sig sets that variable so JavaScriptCore does not take SIGUSR1, and WebKit prints the rejection once per process.
- A WPE run still starts Servo when Servo is compiled in and enabled, because that is how sig installs its logger. sig's RSS on those runs includes Servo.
- The non-blank fraction is pixels of the page rectangle (84 px of chrome at the top, 20 px of status at the bottom; the default chrome at font size 14 and scale 1) that differ from that rectangle's dominant colour. Every pixel is sampled.
- Load time includes engine startup. The first diagnostics poll waits 2 s so SIGUSR1 cannot arrive before the handler, so a page that finished earlier is recorded at about 2 s. After that the flag is polled about every 0.3 s. CPU misses up to one 0.2 s sample of a process that exits between samples. WebKit's RSS peak is that sample's sum, not each process's high-water mark.
- Each run is one public GET. A consent wall or a login form is the page that was measured. Servo can leave the tab's loading flag set after the page is visibly complete, and that run stays TIMEOUT.
