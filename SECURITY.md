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

Sighurt's security model is per engine. These areas are in scope:

**Browser (`sighurt-core`, `sighurt-ui`, `sighurt-ipc`)**

- Engine output that crashes `sig`, exhausts its memory or gets around the bounds on messages, images, draw lists and coordinates
- URL routing that sends content to an engine or scheme it should not reach
- Pages opening `sig://` URLs, or opening tabs or downloads without the throttle
- Download handling, such as files written outside the download folder

**WASM engine (`sighurt-engine-wasm`)**

- **Sandbox escapes**: guest modules reaching anything beyond the registered host functions
- **Host API abuse**: exploiting a host function beyond its intended scope, including guest pointers and lengths that read or write outside guest memory
- **Permission bypasses**: using the camera, microphone, geolocation or screen capture without a grant, or getting around a manifest's declarations
- **Cross-origin data leaks**: one app reading another origin's session or persistent storage, or inheriting its grants
- **Resource limits**: bugs in fuel metering, memory limits or the `wasmtime` integration
- **File-picker misuse**: an app reaching files the user did not pick

**Servo engine (`sighurt-engine-servo`)**

- Bugs in how `sig-servo` embeds Servo or talks to `sig`. Report vulnerabilities in Servo itself to the [Servo project](https://github.com/servo/servo).

## Out of Scope

- Denial of service through excessive fuel use in the WASM engine (mitigated by design)
- Bugs in upstream dependencies, including Servo and Wasmtime (report those to the respective projects)
- Anything an external engine you configured yourself can do. Engines in `[engines.*]` are programs that run with your privileges.
- Issues requiring physical access to the user's machine

## Disclosure Policy

- We follow **coordinated disclosure**. Please don't disclose a vulnerability publicly until a fix is released or 90 days have passed since acknowledgment, whichever comes first.
- Reporters are credited in the release notes unless they ask to stay anonymous.

## Security Design

`sig` runs no page code. Every engine is a separate process, and everything it sends over `sighurt-ipc` is untrusted: message sizes, text, URLs, images, draw lists and coordinates are bounded, an engine that doesn't say `Hello` within 10 seconds is stopped, pages can't open `sig://` URLs, and only the front tab may open a tab, at most once a second.

### WASM engine

1. **No WASI**: guests run in Wasmtime with no filesystem, environment or socket access.
2. **Capability-based APIs**: guests can only call host functions registered in the linker.
3. **Permission prompts**: camera, microphone, geolocation and screen capture each need an explicit Allow per origin, remembered per `(origin, capability)` for the life of the page's process.
4. **Manifest declarations**: an app that ships a manifest must declare the sensitive capabilities it may request. Undeclared ones are denied without a prompt.
5. **Origin-scoped storage**: session and persistent storage are per origin. Session storage is cleared on cross-origin navigation.
6. **Fuel metering and memory limits**: execution and guest memory (256 MB) are bounded.
7. **No clipboard**: clipboard access is always refused.

### Servo engine

Web content gets Servo's security model. Each page runs in its own `sig-servo` process.

### Engines you configure

An engine under `[engines.*]` is a program you chose to run, with your privileges. `sig` bounds what it reads from it, but otherwise trusts it as much as any program you run.
