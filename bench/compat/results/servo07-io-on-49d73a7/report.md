# Website compatibility 2026-10-21 (49d73a7)

`sig 0.7.0 (Sighurt, based on Oxide by Nikhil Ranjan)` on louise. Headless sway, GLES2, 1600×1000, scale 1, one private compositor and D-Bus per run. 3 repetition(s).

A cell is `class · load · script/engine errors · non-blank · RSS`. Load is process start until the tab stopped loading (the 3 s settle is extra). RSS is sig's peak; a WPE cell adds the sampled peak of WebKit's processes (`sig+webkit`).

| Site | Category | Servo | WPE |
|---|---|---|---|
| github-servo | code | OK · 2.0s · 0 err · 30% · 510 MiB | — |
| youtube | video | OK · 2.0s · 0 err · 49% · 618 MiB | — |
| reddit-rust | social | OK · 2.0s · 0 err · 51% · 851 MiB | — |
| bbc | news | OK · 2.0s · 0 err · 67% · 490 MiB | — |
| nytimes | news | OK · 2.0s · 0 err · 5% · 393 MiB | — |
| microsoft | corporate | OK · 2.0s · 0 err · 73% · 510 MiB | — |

## Per engine

| Engine | OK | PARTIAL | BLANK | CRASH | TIMEOUT | ERROR | median load s | median RSS MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| servo | 18 | 0 | 0 | 0 | 0 | 0 | 2.0 | 510 |
| wpe | 0 | 0 | 0 | 0 | 0 | 0 | — | — |

## Servo failures by first error

No Servo run failed.
## Harness limits

- The diagnostics JSON never includes an address or a title. Those two fields come from the session file. The run starts with session restore off and turns it on only after the screenshot, so a previous session is never restored; sig writes the snapshot because restore then applies live.
- Servo's delegate drops page console messages (they are page data and go nowhere). Counted log lines are script::script_runtime errors, other ERROR lines, panics and fatal signals, and WebKit crash or exception text that reaches stderr. A JS exception such as adoptedStyleSheets being undefined is absent unless Servo also logs or panics on it. WebKit's `ERROR: invalid option: JSC_SIGNAL_FOR_GC` lines are ignored: sig sets that variable so JavaScriptCore does not take SIGUSR1, and WebKit prints the rejection once per process.
- A WPE run still starts Servo when Servo is compiled in and enabled, because that is how sig installs its logger. sig's RSS on those runs includes Servo.
- The non-blank fraction is pixels of the page rectangle (84 px of chrome at the top, 20 px of status at the bottom; the default chrome at font size 14 and scale 1) that differ from that rectangle's dominant colour. Every pixel is sampled.
- Load time includes engine startup. The first diagnostics poll waits 2 s so SIGUSR1 cannot arrive before the handler, so a page that finished earlier is recorded at about 2 s. After that the flag is polled about every 0.3 s. CPU misses up to one 0.2 s sample of a process that exits between samples. WebKit's RSS peak is that sample's sum, not each process's high-water mark.
- Each run is one public GET. A consent wall or a login form is the page that was measured. Servo can leave the tab's loading flag set after the page is visibly complete, and that run stays TIMEOUT.
