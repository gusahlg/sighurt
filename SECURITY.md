# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| latest  | :white_check_mark: |

## Reporting a Vulnerability

If you find a security vulnerability in Sighurt, please report it privately. Use GitHub's private vulnerability reporting on this repository: open the **Security** tab and choose **Report a vulnerability**. Do not open a public issue.

Please include:

- A description of the vulnerability and its potential impact
- Steps to reproduce the issue
- Any relevant logs, screenshots, or proof-of-concept code
- The version or commit hash you tested against

We will acknowledge the report and keep you informed while we investigate.

## Scope

These areas are in scope:

**Browser (`sighurt-core`, `sighurt-ui`, `sighurt-wayland`, `sighurt-browser`)**

- What pages send (titles, status text, addresses, requests) crashing `sig`, exhausting its memory or getting around the bounds below
- A page that writes the address line, makes its title look like the address, opens `sig:` pages or runs a browser command
- A page opening tabs without a click or key press of the user's, or moving its tab to another engine without the user's click on that link, or while the user's own address loads; downloads that get around the throttle; files written outside the download folder, or named to hide what they are
- Privileged services (clipboard, downloads, file pickers, external URLs, permissions) granted to the wrong document or without the user
- Routing that sends content to an engine or a scheme it should not reach

**WASM engine (`sighurt-engine-wasm`)**

- **Sandbox escapes**: guest modules reaching anything beyond the registered host functions
- **Host API abuse**: exploiting a host function beyond its intended scope, including guest pointers and lengths that read or write outside guest memory
- **Resource exhaustion**: making the host allocate without bound (draw lists, images, text, timers, workers, network buffers), or a guest that keeps the browser from responding despite the fuel, time and memory limits
- **Permission bypasses**: using the camera, microphone, geolocation or screen capture without a grant, or getting around a manifest's declarations
- **Cross-origin data leaks**: one app reading another origin's session or persistent storage, or inheriting its grants
- **File-picker misuse**: an app reaching files the user did not pick

**Servo and WPE WebKit engines (`sighurt-engine-servo`, `sighurt-engine-wpe`)**

- Bugs in how Sighurt embeds them: requests that bypass the browser's policy, navigations the browser was not asked about, page data in the logs. Report vulnerabilities in Servo itself to the [Servo project](https://github.com/servo/servo) and in WebKit to [WebKit](https://webkit.org/security-policy/).

## Out of Scope

- Denial of service through excessive fuel use in the WASM engine (mitigated by design)
- Bugs in upstream dependencies, including Servo, WebKit and Wasmtime (report those to the respective projects)
- That a compromised Servo or WebKit can take over `sig`: the engines run in its process, as described below
- Keymaps and chromes compiled into `sig`: like the engines, they are trusted code
- Issues requiring physical access to the user's machine

## Disclosure Policy

- We follow **coordinated disclosure**. Please don't disclose a vulnerability publicly until a fix is released or 90 days have passed since acknowledgment, whichever comes first.
- Reporters are credited in the release notes unless they ask to stay anonymous.

## Security Design

### The engines run in `sig`'s process

Servo, WPE WebKit's UI-process half and the Wasm engine are libraries compiled into `sig`, and there is no IPC between them and the browser. The contracts between the browser and its engines keep correct code correct; they are not a sandbox. Code execution in an engine is code execution in `sig`, with the user's privileges, and a native crash in an engine ends the whole browser. Isolating an engine would take a restricted worker process, which Sighurt doesn't have.

What each engine adds of its own:

- **Servo** runs its script, layout and network threads in `sig`'s process.
- **WPE WebKit** runs pages in its own web processes, each in a bubblewrap sandbox (its own network, PID and UTS namespaces, a seccomp filter, read-only binds), and networking in a network process that WebKit doesn't sandbox. A crashed web process fails its tab, which a reload brings back. WebKit's UI-process code runs in `sig`.
- **The WASM engine** runs apps under Wasmtime, which isolates their memory, with the limits below.

### What pages get

Engines report what pages do as events, and the browser core decides. Everything a page says is untrusted:

- Titles and status text are cut to 16 KiB and shown on one line with control and bidi characters removed; URLs over 2 MiB are dropped, never shortened. Addresses are shown decoded only where that can't hide or fake part of them.
- A page can't write the address line, run a browser command through a URL, open `sig:` pages, or open `file:` URLs unless it is a `file:` page itself.
- A page opens a new tab only within 5 seconds of a click or key press in it, one tab per click or key press. It moves its tab to another engine (a link the routing gives another engine) only within 5 seconds of the user's click on that very link (the link the page's status text named at the press), using up the same click, and never while the tab loads an address the user asked for.
- Page-initiated downloads, a Wasm app's included, are limited to one a second per tab, and at most 8 run at once in all; more are refused with a notice. Files go into the download folder (never the home folder itself), named after the last part of the suggested name without dots or whitespace at its ends and without control or invisible formatting characters, cut to 200 bytes.
- The clipboard, file pickers and handing a URL to the desktop need a click or key press in the page. Only URLs `sig` doesn't open itself go to the desktop (`mailto:`, an app's own scheme), and never one that makes the desktop mount a share or connect to another machine, perhaps with credentials it keeps (`smb:`, `nfs:`, `sftp:`, `ftp:`, `dav:`, `ssh:`, `telnet:`, `vnc:`, `rdp:`, GVfs's `admin:` and the like). Camera, microphone, location and screen capture need the user's answer. A request acts for one document and is dropped when that document goes. `sig` has no permission prompt or file picker yet, so those requests are refused, and anything it doesn't know is refused too.
- Password fields go to the input method marked sensitive and hidden, without their text.
- The diagnostics and the logs hold no clipboard contents, credentials or page data.

### `sig://terminal` and `sig://config` run programs with your full rights

`sig://terminal` runs your shell, and `sig://config` runs your editor on `sig`'s configuration file. Each program runs as you, with your full rights, as in any terminal: your files, your credentials, your agent sockets, your shell's startup files; an editor can run shell commands too. Sighurt's Wasm sandbox contains the pages' terminal emulator, the code that reads what the program prints; it does not contain the program. `sig://terminal` is off unless the configuration turns it on (`[pages.terminal] enabled = true`). **`sig://config` is on by default**, so that the configuration can be edited from the browser before it is ever written; `[pages.config] enabled = false` turns it off.

- **What runs is the browser's choice, and each page has one program.** `sig://terminal` may start only the shell and `sig://config` only the editor; the core checks it from its own record of the page. `sig` builds the command (`[pages.terminal] command`, else `$SHELL`, else `/bin/sh`; `[pages.config] editor`, else `$VISUAL`, else `$EDITOR`, else the first of `nvim`, `vim`, `vi` and `nano` in an absolute directory of `PATH`, never a relative or empty entry, with the configuration file as an argument of its own; never through a shell), its environment and its directory. A program starts only after a fresh Enter press or a click in the page itself while its tab is shown and the window has the keyboard: a held key's repeats, a key pressed in another tab whose commands switch to this one, and keys and clicks while a program runs (they are its own) start nothing. Web pages and Wasm apps can't open these pages or reach a terminal.
- **The program gets the terminal and nothing else of `sig`'s**: the terminal on descriptors 0 to 2 (every other descriptor of `sig`'s is close-on-exec), default signal handling, an empty signal mask and a session of its own. Its environment drops `sig`'s own variables and those describing the terminal `sig` runs in; that is hygiene, not isolation.
- **The emulator reflects nothing.** It answers only queries with fixed answers (device attributes, status, cursor position) and never sends back what the program sent (no title report, no clipboard access). A paste has ESC and the other control characters removed (tabs and line breaks stay), so it can't type keys or end a bracketed paste early.
- **Ending it.** Closing the tab, navigating away, reloading, turning the page's setting off and quitting send SIGHUP to the program's process group and SIGKILL after 2 seconds, and `sig` reaps the program: no child of `sig` is left. Jobs the shell put in process groups of their own, programs that ignore SIGHUP after `disown`, and detached sessions get the terminal's hang-up and may survive it, as in any terminal. At most eight programs run at once, shells and editors together.
- **A flood waits.** The program's output is held in a queue of 1 MiB; a program that writes faster than the page reads waits in its `write`.

### WASM engine

1. **No WASI**: guests run in Wasmtime with no filesystem, environment or socket access.
2. **Capability-based APIs**: guests can only call host functions registered in the linker, and every guest pointer and length is checked.
3. **Permission prompts**: camera, microphone, geolocation and screen capture each need an explicit Allow per origin, which the engine's own prompt asks for and remembers per `(origin, capability)` for the life of the tab's view. A page can't hide the prompt and still have it answered.
4. **Manifest declarations**: an app that ships a manifest must declare the sensitive capabilities it may request. Undeclared ones are denied without a prompt.
5. **Origin-scoped storage**: session and persistent storage are per origin. Session storage is cleared on cross-origin navigation.
6. **Limits**: fuel (500M instructions to start, 50M per callback), wall-clock time (10 s to start, 1 s per callback, so a guest that keeps calling the host is stopped too), 256 MB of guest memory, and bounds on everything a guest can make the host allocate.
7. **No clipboard**: clipboard access is always refused.

### Sighurt's own pages

`sig://help`, `sig://config` and `sig://terminal` are Wasm guests built into `sig` from source in
this repository and run by the WASM engine. They are where the browser gives a page more than any
web page gets, so what they get is held at several layers:

- **Only the user opens them.** The core refuses a `sig:` address to every page that is not one
  of these, by any route (a link, a page's own navigation, a new tab, a navigation request, a
  history entry, a session file); only the address line, commands and session restore, and the
  pages themselves, open them. A restored page loads again and starts nothing.
- **Identity is the browser's, not the page's.** The core records which page a view is when it
  opens it, from its own routing, and keeps that for the view's life; a page's view loads only
  that page, and a view of anything else never loads a page. Privileges come from that record and
  from the module embedded in `sig`, never from an address the engine reports, an origin a request
  claims, or a downloaded copy of the same guest.
- **The privileged import module `sig`** is linked only into an internal page's document, never a
  worker's or a module it loads, and every function checks that page's grants again. The core
  then decides each request from its own record: help may ask for facts about the build and the
  configuration (never the configuration's text); config may start the editor and ask which it
  is; terminal may start the shell. Starting a program also needs the page in front, with the
  keyboard, in a window that has it, and a click or key press in it.
- **No network, no code loading.** An internal page has a deny-all network policy, and fails to
  load if it imports the network, workers, module loading, files or devices.
- **Typing goes where you meant it.** After a tab comes to the front without your command (a page
  that closed its tab), its page gets no keys, text or paste until you click it or run a command.

`sig://terminal` is off unless `[pages.terminal] enabled = true`; `sig://config` is on unless
`[pages.config] enabled = false`. They run your shell and your editor with all your rights: the
Wasm sandbox contains the terminal emulator that draws them, not the programs or what you run in
them, and the programs' environment is hygiene, not isolation.
