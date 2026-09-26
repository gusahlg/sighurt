# sighurt-engine-servo

The Servo (HTML, CSS, JavaScript) engine for Sighurt, being rebuilt as a library that `sig`
compiles in (feature `engine-servo`) and runs in its own process, presenting straight into a
Wayland subsurface. See `sighurt-rewrite-plan.md` §14 for the design and
`docs/rewrite/STATUS.md` for progress.

It is pinned at Servo 0.5 and surfman 0.13. Building it compiles Servo: do that on louise-pc
(`SIG_WHAT="servo build" tools/remote tools/cargo build -p sighurt-browser`), not casually here.

The previous out-of-process engine, `sig-servo`, is at git tag `gpui-baseline`. Its README
(`git show gpui-baseline:sighurt-engine-servo/README.md`) records knowledge that still applies
to Servo 0.5: which preferences to change and why (grid and other features on, `SharedWorker`,
`IntersectionObserver` and `adoptedStyleSheets` off, two layout threads, the user agent), the
profile location, site compatibility, and measured rendering costs.

## License

`Apache-2.0 AND MPL-2.0`. Files adapted from Servo stay under the
[Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/) and keep their notices; the rest of
the crate is Apache-2.0 like the rest of Sighurt. Servo itself is MPL-2.0. See
[NOTICE](../NOTICE).
