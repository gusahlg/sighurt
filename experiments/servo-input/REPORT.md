# Why github.com freezes and chatgpt.com's login can't be used in Servo

Investigation by a Claude Opus subagent, 21 October 2026 (written down from its report). No
product code changed. Scripts here: `drive.sh` (runs sig in a private headless sway with a
throwaway profile and drives it; `SIG_BIN`, `SIG_EXTRA_CONF`, `SIG_OLD_ARGS`, `SIG_CONSOLE=1`
prints the page's console messages through gdb on the delegate's `show_console_message`,
`SIG_GDB_BREAKS` prints the page's JavaScript stack when named DOM functions are called),
`gdb-stacks.sh` (the script thread's JavaScript stack while it is stuck), `sample_threads.py`,
`pages/{hover,signed-in,modal,modal-z}.html`.

Builds: Servo 0.5 (`target/debug/sig` of 21 October, 07:45; `target/try/sig` of 17 October
predates `[engines.servo]` settings), Servo 0.7 (louise-pc's build of 17 October, no `--engine`
or `[engines.servo]`). All runs in a private headless sway with throwaway XDG directories.

## Our input forwarding is not the cause (measured)

On a local page, hover changed a link's colour, a click navigated, a field took focus and
"abc" was typed. On both sites sig counted the pointer and key events and Servo logged no
"Empty hit test".

## github.com: the page's script never yields (high confidence)

- About 3 s after load the script thread (`Script#1`) sits at 99% CPU for as long as measured
  (60 s, from /proc). Only 3–5 page frames are shown; hovering changes 0 pixels; 8 header
  clicks do nothing; a newly typed address never loads, since the old page never yields. Servo
  handles queued input when it next renders, which never happens.
- perf: ~22% forced layout (`offsetWidth`/`offsetHeight`), ~30% `new Error()` with stack
  capture, plus appendChild/removeChild/setAttribute. gdb's JavaScript stacks: one scheduler
  task that never ends, in which React re-renders 130–160 levels deep, catches errors and starts
  again, synchronously, forever.
- Console: `IntersectionObserver is not defined` in three of GitHub's scripts,
  `e.adoptedStyleSheets is undefined` in one. Sighurt turns both off in Servo on purpose.
- **`[engines.servo.preferences] dom_intersection_observer_enabled = true` alone fixes it**: CPU
  falls to 34–40%, then idle; hover opens the Platform menu; the first click navigates to
  /signup; /login loads; typing after a click fills the field. `experimental-features = true`
  does the same; adding `dom_adoptedstylesheet_enabled = true` removes the last error, with no
  layout panic.
- Servo 0.7 with its defaults freezes the same way (IntersectionObserver is still off by default
  there).
- Left over: github.com/login's autofocused field doesn't take focus in Servo (it does in WPE).

## chatgpt.com: the login form is drawn under the page (high confidence)

- "Log in" opens the form with `<dialog>.showModal()` (JavaScript stack in
  `octane-shell-shared-*.js`). Neither Servo 0.5 nor 0.7 puts a modal dialog in the top layer
  (a TODO in their `htmldialogelement.rs`): no top layer, no inertness of the rest of the page,
  no `::backdrop`, and no popover API. The "Ask ChatGPT" box and its suggestions are drawn over
  the dialog and take its clicks: a click on "Email address" picked a suggestion, and typing went
  into the "Ask ChatGPT" box. The code field the user saw is presumably in the same dialog
  (inferred; it needs a real account).
- Local reproduction (`pages/modal.html`): Servo hands a click inside the dialog to the element
  behind it (z-index 10); WPE shows the dialog on top with its backdrop. With
  `dialog:modal { z-index: 2147483647 }` (`pages/modal-z.html`) Servo draws the dialog on top and
  sends it the clicks.
- No setting fixes it: not `experimental-features`, not any preference, not Servo 0.7.

## Fix options

1. **Ours, small: turn IntersectionObserver on by default.** It was off because it made
   reddit.com and microsoft.com spin, a Servo bug fixed by servo/servo#47693 (not checked
   whether 0.7 contains it). Re-measure those sites with `bench/compat/` on 0.5 and 0.7 first.
   Until then users can set `experimental-features = true` or that one preference.
2. **Ours, small to medium: a user stylesheet through Servo's `UserContentManager`**
   (`dialog:modal { z-index: 2147483647 !important }` and a backdrop). Verified on the local
   page only; on chatgpt.com it works only if the dialog isn't confined beneath the "Ask
   ChatGPT" box by an ancestor's stacking context (untested). It doesn't make the rest of the
   page inert or trap focus.
3. **No watchdog from our side:** Servo 0.5 gives the embedder no way to interrupt a long
   script. Routing such sites to WPE stays the user's choice (decision 94).
4. **Servo's own work:** the top layer for `<dialog>` and popovers is large upstream work, not
   in 0.7; IntersectionObserver by default is upstream's call.

Open: the chatgpt code field itself (needs an account); Servo 0.7 with IntersectionObserver on
(needs a 0.7 build with `[engines.servo]` settings); the reddit.com/microsoft.com regression
check for option 1.
