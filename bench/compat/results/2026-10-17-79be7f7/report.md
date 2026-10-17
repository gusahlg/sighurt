# Website compatibility 2026-10-17 (79be7f7)

`sig 0.7.0 (Sighurt, based on Oxide by Nikhil Ranjan)` on louise. Headless sway, GLES2, 1600×1000, scale 1, one private compositor and D-Bus per run. 1 repetition(s).

A cell is `class · load · script/engine errors · non-blank · RSS`. Load is process start until the tab stopped loading (the 3 s settle is extra). RSS is sig's peak; a WPE cell adds the sampled peak of WebKit's processes (`sig+webkit`).

| Site | Category | Servo | WPE |
|---|---|---|---|
| example | control | OK · 2.0s · 0 err · 3% · 293 MiB | OK · 2.0s · 0 err · 3% · 142+303 MiB |
| github-servo | code | OK · 2.0s · 0 err · 30% · 513 MiB | OK · 2.0s · 0 err · 29% · 149+665 MiB |
| gitlab | code | OK · 2.0s · 0 err · 48% · 517 MiB | OK · 2.0s · 0 err · 87% · 150+561 MiB |
| chatgpt | assistant | OK · 2.0s · 0 err · 22% · 373 MiB | OK · 2.0s · 0 err · 21% · 149+415 MiB |
| google-search | search | OK · 2.0s · 0 err · 57% · 361 MiB | OK · 2.0s · 0 err · 56% · 149+465 MiB |
| duckduckgo | search | OK · 4.1s · 0 err · 20% · 501 MiB | OK · 2.4s · 0 err · 23% · 149+743 MiB |
| youtube | video | PARTIAL · 2.0s · 7 err · 30% · 619 MiB | OK · 2.7s · 0 err · 53% · 149+996 MiB |
| twitch | video | PARTIAL · 2.0s · 1 err · 55% · 618 MiB | OK · 2.0s · 0 err · 77% · 150+765 MiB |
| netflix | video | OK · 2.4s · 0 err · 91% · 665 MiB | OK · 2.4s · 0 err · 89% · 150+796 MiB |
| wikipedia | reference | TIMEOUT · — · 0 err · 15% · 386 MiB | OK · 2.0s · 0 err · 14% · 149+437 MiB |
| mdn-grid | docs | PARTIAL · 2.0s · 5 err · 26% · 403 MiB | OK · 2.0s · 0 err · 25% · 149+530 MiB |
| docsrs | docs | OK · 2.0s · 0 err · 18% · 351 MiB | OK · 2.0s · 0 err · 18% · 150+359 MiB |
| crates | docs | PARTIAL · 2.0s · 1 err · 51% · 343 MiB | OK · 2.0s · 0 err · 50% · 150+546 MiB |
| w3schools | tutorial | OK · 2.0s · 0 err · 64% · 408 MiB | OK · 2.0s · 0 err · 64% · 150+464 MiB |
| codepen | tutorial | TIMEOUT · — · 0 err · 80% · 489 MiB | OK · 2.4s · 0 err · 85% · 150+848 MiB |
| caniuse | features | PARTIAL · 3.8s · 1 err · 59% · 535 MiB | OK · 3.4s · 0 err · 59% · 150+590 MiB |
| rustlang | language | OK · 2.0s · 0 err · 50% · 319 MiB | OK · 2.0s · 0 err · 52% · 149+375 MiB |
| reddit-rust | social | TIMEOUT · — · 37 err · 48% · 843 MiB | OK · 3.8s · 0 err · 38% · 151+872 MiB |
| x | social | OK · 2.0s · 0 err · 21% · 636 MiB | OK · 2.0s · 0 err · 21% · 150+682 MiB |
| linkedin | social | PARTIAL · 2.0s · 1 err · 29% · 557 MiB | OK · 2.0s · 0 err · 46% · 150+631 MiB |
| instagram | social | OK · 2.7s · 0 err · 36% · 544 MiB | OK · 2.0s · 0 err · 35% · 150+575 MiB |
| stackoverflow | qa | OK · 3.5s · 0 err · 49% · 484 MiB | OK · 4.5s · 0 err · 47% · 150+857 MiB |
| hn | news | OK · 2.0s · 0 err · 25% · 301 MiB | OK · 2.0s · 0 err · 24% · 150+364 MiB |
| bbc | news | OK · 2.0s · 0 err · 67% · 508 MiB | OK · 2.0s · 0 err · 65% · 150+539 MiB |
| nytimes | news | TIMEOUT · — · 0 err · 5% · 466 MiB | OK · 2.0s · 0 err · 5% · 150+498 MiB |
| guardian | news | OK · 2.0s · 0 err · 82% · 515 MiB | OK · 2.0s · 0 err · 81% · 150+523 MiB |
| medium | news | PARTIAL · 2.0s · 0 err · 17% · 554 MiB | OK · 2.0s · 0 err · 17% · 150+652 MiB |
| amazon | commerce | OK · 2.0s · 0 err · 8% · 333 MiB | OK · 2.7s · 0 err · 78% · 150+677 MiB |
| steam | commerce | OK · 2.0s · 0 err · 89% · 641 MiB | OK · 2.0s · 0 err · 89% · 149+654 MiB |
| apple | corporate | OK · 2.0s · 0 err · 31% · 514 MiB | OK · 2.4s · 0 err · 32% · 149+712 MiB |
| microsoft | corporate | OK · 3.8s · 0 err · 73% · 545 MiB | OK · 2.0s · 0 err · 73% · 151+737 MiB |
| cloudflare | infrastructure | OK · 2.0s · 0 err · 44% · 448 MiB | OK · 2.0s · 0 err · 88% · 150+736 MiB |
| gmail | mail | OK · 2.0s · 0 err · 29% · 389 MiB | OK · 2.4s · 0 err · 28% · 150+417 MiB |
| gmaps | maps | OK · 2.0s · 0 err · 4% · 355 MiB | OK · 2.0s · 0 err · 4% · 150+384 MiB |
| osm | maps | OK · 2.0s · 0 err · 74% · 399 MiB | OK · 2.4s · 0 err · 74% · 149+420 MiB |
| discord | chat | TIMEOUT · — · 1 err · 86% · 800 MiB | OK · 3.1s · 0 err · 86% · 150+1217 MiB |
| notion | productivity | TIMEOUT · — · 0 err · 3% · 513 MiB | OK · 2.0s · 0 err · 23% · 149+530 MiB |
| figma | design | OK · 2.0s · 0 err · 47% · 579 MiB | OK · 2.0s · 0 err · 32% · 150+962 MiB |
| spotify | media | OK · 2.0s · 0 err · 51% · 662 MiB | OK · 2.7s · 0 err · 71% · 149+772 MiB |
| archive | archive | OK · 3.1s · 0 err · 9% · 344 MiB | OK · 2.0s · 0 err · 68% · 150+567 MiB |

## Per engine

| Engine | OK | PARTIAL | BLANK | CRASH | TIMEOUT | ERROR | median load s | median RSS MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| servo | 27 | 7 | 0 | 0 | 6 | 0 | 2.0 | 504 |
| wpe | 40 | 0 | 0 | 0 | 0 | 0 | 2.0 | 150 |

## Servo failures by first error

### 4 · (TIMEOUT, no script error in the log)

wikipedia (TIMEOUT), codepen (TIMEOUT), nytimes (TIMEOUT), notion (TIMEOUT)

### 3 · Unhandled promise rejection: (new ReferenceError("IntersectionObserver is not defined", "<url> N))

mdn-grid (PARTIAL), crates (PARTIAL), caniuse (PARTIAL)

```
[2026-10-17T12:25:13Z ERROR script::script_runtime] Unhandled promise rejection: (new ReferenceError("IntersectionObserver is not defined", "https://developer.mozilla.org/static/client/index.2bffa2cd2f8c6c78.js", 276))
```

### 1 · (PARTIAL, no script error in the log)

medium (PARTIAL)

### 1 · Unhandled promise rejection: (new ReferenceError("requestIdleCallback is not defined", "<url> N))

discord (TIMEOUT)

```
[2026-10-17T12:38:02Z ERROR script::script_runtime] Unhandled promise rejection: (new ReferenceError("requestIdleCallback is not defined", "https://discord.com/w/assets/3a756378428a770f27e63b63edc9fd6cc02fda3a/index-consolidated.js", 87))
```

### 1 · Unhandled promise rejection: (new TypeError("X.hostElement.animate is not a function", "<url> N))

youtube (PARTIAL)

```
[2026-10-17T12:23:55Z ERROR script::script_runtime] Unhandled promise rejection: (new TypeError("X.hostElement.animate is not a function", "https://www.youtube.com/s/_/ytmainappweb/_/js/k=ytmainappweb.kevlar_base.en_US.VRNYTIAj20w.es5.O/am=AAAAAAgACEBK/d=1/br=1/rs=AGKMywE2CwXnYBA7UeNL-YqRHxSyLivf2Q/m=kevlar_base_module,kevlar_main_module,kevlar_base_sync_mod_chunk", 36139))
```

### 1 · Unhandled promise rejection: (new TypeError("can't access property \"length\" of null", "<url> line N > Function", N))

twitch (PARTIAL)

```
[2026-10-17T12:24:25Z ERROR script::script_runtime] Unhandled promise rejection: (new TypeError("can't access property \"length\" of null", "https://k.twitchcdn.net/149e9513-01fa-4fb0-aad4-566afd725d1b/2d206a39-8ed7-437e-a3be-862e0f06eea3/ips.js?KP_UIDz=08ad5kDoW6CMdNyphqeMvYVZARoHgzGiR82lKCXPvX29P7gNLKTVUAxoW1ef68EsPuBaYzxNypMsw01St2cqQMVi15Oz1oSf1bkRrIvmRodMZw7bP86Z02u43ZrPBIC113R5wRqWG54xhnQPIBiGaDjShYXztb1&x-kpsdk-v=j-1.2.904&x-kpsdk-im=AALJGqzK6TirxMOZ8bc2J4_4Ct2PkTLQqcgybDZHhGKSOCe_K79WcAf
```

### 1 · Unhandled promise rejection: OperationError: fetchStart hasn't been implemented. {}

reddit-rust (TIMEOUT)

```
[2026-10-17T12:28:54Z ERROR script::script_runtime] Unhandled promise rejection: OperationError: fetchStart hasn't been implemented. {}
```

### 1 · error loading script chrome://juggler/content (Blocked as mixed content)

linkedin (PARTIAL)

```
[2026-10-17T12:30:23Z ERROR script::dom::workers::workerglobalscope] error loading script chrome://juggler/content (Blocked as mixed content)
```

## Harness limits

- The diagnostics JSON never includes an address or a title. Those two fields come from the session file. The run starts with session restore off and turns it on only after the screenshot, so a previous session is never restored; sig writes the snapshot because restore then applies live.
- Servo's delegate drops page console messages (they are page data and go nowhere). Counted log lines are script::script_runtime errors, other ERROR lines, panics and fatal signals, and WebKit crash or exception text that reaches stderr. A JS exception such as adoptedStyleSheets being undefined is absent unless Servo also logs or panics on it. WebKit's `ERROR: invalid option: JSC_SIGNAL_FOR_GC` lines are ignored: sig sets that variable so JavaScriptCore does not take SIGUSR1, and WebKit prints the rejection once per process.
- A WPE run still starts Servo when Servo is compiled in and enabled, because that is how sig installs its logger. sig's RSS on those runs includes Servo.
- The non-blank fraction is pixels of the page rectangle (84 px of chrome at the top, 20 px of status at the bottom; the default chrome at font size 14 and scale 1) that differ from that rectangle's dominant colour. Every pixel is sampled.
- Load time includes engine startup. The first diagnostics poll waits 2 s so SIGUSR1 cannot arrive before the handler, so a page that finished earlier is recorded at about 2 s. After that the flag is polled about every 0.3 s. CPU misses up to one 0.2 s sample of a process that exits between samples. WebKit's RSS peak is that sample's sum, not each process's high-water mark.
- Each run is one public GET. A consent wall or a login form is the page that was measured. Servo can leave the tab's loading flag set after the page is visibly complete, and that run stays TIMEOUT.
