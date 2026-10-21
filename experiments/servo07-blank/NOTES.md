# Why Servo 0.7 blanks Stack Overflow and Amazon

Measured 2026-10-21 on louise-pc. Servo 0.7 binary:
`~/repos/sighurt-servo07/target/debug/sig` (built 17 Oct). Servo 0.5 comparison
binary: `~/repos/sighurt-rewrite/target/debug/sig`. Both print `sig 0.7.0`; the
rewrite tree pins `servo = "0.5"`.

```
SIG_REMOTE_DEST=repos/sighurt-servo07 SIG_WHAT="servo07 blank probe" \
  tools/remote tools/heavy python3 experiments/servo07-blank/probe.py
```

Exit 0, 217.68 s. Per-case logs and screenshots stayed on louise under
`experiments/servo07-blank/out/` and are not committed. Each `sig` ran inside
`tools/headless-sway` (gles2, 1600×1000) on `sighurt-engine-wpe/tests/private-bus`,
with throwaway XDG dirs. Page metrics crop 84 px of chrome and 20 px of status,
the same crop as `bench/compat/run`.

## The user-agent string does not decide the page

`firefox_user_agent` in `sighurt-engine-servo/src/runtime.rs` rewrites Servo's
`Servo/<version>` token to `Gecko/20100101`. A local echo server recorded the
defaults:

- 0.5: `Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0`
- 0.7: `Mozilla/5.0 (X11; Linux x86_64; rv:153.0) Gecko/20100101 Firefox/153.0`

`[engines.servo] user-agent` then put 140 on 0.7 and 153 on 0.5. Outcome follows
the engine:

| case | title | nonblank | distinct | load |
|---|---|---:|---:|---:|
| so-07 (Firefox/153) | Just a moment… | 0 | 1 | 2.005 s |
| so-07-ua140 | Just a moment… | 0 | 1 | 15.322 s |
| so-05 (Firefox/140) | Newest 'rust' Questions - Stack Overflow | 0.4827 | 8687 | 3.405 s |
| so-05-ua153 | same real title | 0.4827 | 8687 | 3.404 s |
| amz-07, amz-07-ua140 | empty | 0 | 1 | ~2.00 s |
| amz-05, amz-05-ua153 | Amazon.com. Spend less. Smile more. | 0.8135 | 93294 | 2.003 s |

so-07-ua140's load time is the reload loop holding the loading flag. Header
debug ran for the two defaults (`so-07`, `amz-07`) and the echo probes. The swap
cases set the config key; info-level `net::http_loader` does not print the
header, so the swapped on-the-wire value was not dumped. Page outcome still
tracks the engine.

## Both sites serve a challenge that 0.7 does not finish

A local page (background `#c20`, `document.title = "probe-ran"`, `console.error`,
an uncaught `ReferenceError`, an unhandled rejection) paints on both engines
(dominant RGB 204,34,0, title `probe-ran`). Script runs. The live pages are white
because the challenge never yields a document.

Stack Overflow, both engines and both user agents: HTTP/2 403,
`cf-mitigated: challenge`, `set-cookie: __cf_bm`, then
`/cdn-cgi/challenge-platform/h/b/orchestrate/precursor_interstitial/v1` as
JavaScript, 200.

- 0.5, default and Firefox/153: POST `/cdn-cgi/challenge-platform/h/b/fo/…` 200,
  GET `…/scripts/precursor/…/main.js` 200, another `/fo/` POST 200, POST the page
  URL 302, GET the page 200, then the real assets.
- 0.7, default and Firefox/140: precursor 200 only. The document reloads to a new
  403 on about a 2 s / 4 s / 8 s backoff. The retry sends
  `cookie: __cf_bm=…; cf_chl_rc_ni=1` and sends no `Sec-CH-UA*`. Title stays
  "Just a moment…". The response's `critical-ch` asked for client hints; 0.5
  solves in the same second as the precursor load, with no hint-retry visible
  in the info log. 0.5's request headers were not captured at debug.

The first 0.7 document request sent `sec-fetch-site: cross-site`. Servo leaves
user-typed navigations off `none` in both 0.5 and 0.7, so that header is shared.

Amazon, both engines and both user agents: HTTP 202, then `challenge.js` (200)
from `https://1c5c1ecf7303.535706ac.eu-west-1.token.awswaf.com/…/challenge.js`.

- 0.5: GET `inputs?client=browser` 200, POST `mp_verify` 200, GET amazon.com 200,
  storefront. No `/report`.
- 0.7: POST `/report` 200 and GET `inputs?client=browser` 200, then stop. No
  `mp_verify`, empty title, one white colour. `/report` is the collector's
  failure telemetry.

`challenge.js` is one obfuscated 687056-byte line. A readable-string scan named
no error. The string table mentions `requestIdleCallback` once. That API is
absent from servo-script 0.5 and 0.7, and the 17 Oct 0.5 compat log already shows
`requestIdleCallback is not defined` on Discord while 0.5 still completes this
WAF flow. That gap does not separate the two engines.

rustls 0.23.45 and hyper 1.11.1 match in both `Cargo.lock` files. `mozjs` does
not: 0.5 is 0.21.6, 0.7 is 0.26.4. `create_http_client` enables HTTP/1 and HTTP/2
with `http1_title_case_headers(true)` in both servo-net versions. Cipher and
ALPN lists in `create_tls_config` were not diffed.

No `error loading script` line appeared, so this run is not the blob-worker
`InvalidFileID` path (servo/servo#35649, closed as a duplicate of #34320).
#34320 (Cloudflare Turnstile still open; last comment 2026-10-07) describes a
related hang. It does not name this precursor / `mp_verify` split. The thrown
exception was not observed, because console output is dropped (next section).

## Missing script errors are an adapter bug

`Servo::setup_logging` is the same shape in 0.5 and 0.7: env_logger from
`RUST_LOG`, default filter error, plus Servo's `FromEmbedderLogger`. The adapter
calls it from `runtime.rs`. Targets were not renamed away, and the logger was
not replaced.

0.5 `servo-script-0.5.0/script_runtime.rs` around line 683 does
`log::error!("Unhandled promise rejection: {}", …)`. The crate's lib name is
`script` and the file sits at the crate root, so the target is
`script::script_runtime`. That is what `bench/compat/run` counts.

0.7 moved the file to `servo-script-0.7.0/runtime/script_runtime.rs`. Around
lines 514–521 it builds the same message and calls `Console::internal_error`.
That sends `EmbedderMsg::ShowConsoleApiMessage` (`dom/console.rs`,
`send_to_embedder` / `internal_error`). Servo's defaults
`WebViewDelegate::show_console_message` (`webview_delegate.rs` ~1074) and
`ServoDelegate::show_console_message` (`servo_delegate.rs` ~46) are empty.
`sighurt-engine-servo/src/delegate.rs` implements neither. Its module note says
console messages are page data and go nowhere.

The local probe confirms it. 0.5 logs one line:
`ERROR script::script_runtime` … `rejection-probe`. 0.7 logs the GET/200 lines
and nothing about the rejection, the uncaught exception, or `console.error`.
`report_an_error` already sent `ShowConsoleApiMessage` in both versions, so
ordinary exceptions and `console.error` were already invisible in 0.5. Only
unhandled rejections used `log::error!`, and 0.7 moved those onto the console
message. Worker script-load failures still use `log::error!` in both versions
(`script::dom::workers::workerglobalscope` in 0.7). None of those lines fired
on these pages.

`bench/compat/run` hardcodes
`RUST_LOG=error,script::script_runtime=info,…`. On 0.7 that directive matches
nothing: the module path would be `script::runtime::script_runtime`, and the
call is no longer the `log` crate.

### Fix, not applied

Product code stays unchanged. The adapter change is to implement
`WebViewDelegate::show_console_message` and
`ServoDelegate::show_console_message`, and `log::error!` for
`ConsoleLogLevel::Error` (Warn if useful) with the message truncated to about
2 KiB. Leave `console.log` out. An `ERROR` line is enough for the harness error
regex. Keeping the old `script::script_runtime` target string needs a harness
change or a synthetic target. The message is page data; 0.5 already logged
rejection text at error, which is the level these diagnostics need.

The blank pages are a Servo 0.7 issue. No preference or user-agent setting in
the adapter changes the outcome. The console callback is what would name the
web feature the challenge script misses. That name is still unknown.

## Still open

- The exception inside Cloudflare's precursor script and AWS WAF `challenge.js`.
- 0.5 request headers at `net::http_loader=debug` (client hints, and any header
  the two stacks send differently).
- A cipher/ALPN diff of `create_tls_config` between servo-net 0.5 and 0.7.
