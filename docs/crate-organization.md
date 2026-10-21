# Crate organization and Rust customization

This document proposes how to organize Sighurt's crates and develop its customization API.
It records recommendations from a source review, rather than an approved implementation plan.
The review covered the current workspace, including local changes, without building or running it.

## Main recommendation

Keep one repository and Cargo workspace for now, organize the crates into clearer directory
groups, and make it possible to assemble a custom browser from a separate Rust project.

Sighurt's defining architectural goal is an engine-agnostic browser that users can customize
with Rust code. The supplied `sig` executable should be one assembly of the browser's libraries;
users should be able to build their own assembly with different engines, chrome, keymaps, and
behavior.

Moving implementations into separate repositories is useful only after the APIs let an external
application register and use those implementations without editing Sighurt itself.

## Responsibilities and proposed layout

How essential a responsibility is and how replaceable its implementation is are separate
questions. A browser requires content presentation and a user interface, but should not require
a particular engine or chrome implementation.

The existing crate boundaries mostly follow useful responsibilities and should be preserved.
Directory grouping can make those relationships easier to understand without merging crates
or changing Cargo package names.

```text
sighurt/
├── crates/          # API, core, runtime
├── frontends/       # GUI, TUI, Wayland integration
├── ui/              # Chrome API, drawing helpers, standard chrome
├── engines/         # Servo, WPE, Wasm, demo, shared semantic support
├── extensions/      # Line chrome, modal keymap
├── sdk/             # Wasm guest SDK
├── examples/        # Wasm apps and notes backend
├── experiments/     # Standalone engineering investigations
└── apps/            # The supplied sig executable
```

The main execution boundary should remain clear:

- **Core:** owns browser state and policy, turning events into effects without performing I/O.
- **Runtime:** executes shared effects and manages engines, services, persistence, and shutdown.
- **Frontends:** own the window or terminal, input integration, and presentation.
- **Engine adapters:** translate an embeddable engine into Sighurt's engine contract.
- **Extensions:** provide optional implementations of customizable browser behavior and UI.
- **Guest SDK:** provides the application-development API for Wasm guests.

`sighurt-semantic` is shared engine support, rather than a user-selected customization.
`sighurt-sdk` is the guest platform's public development surface. Neither belongs in the same
architectural category as an optional chrome or keymap merely because all four are libraries.

## Put implementation selection in the application

The highest-priority architectural change is to let an application supply its engine, chrome,
and keymap registrations.

Currently, shared libraries contain several fixed implementation lists:

- [Engine descriptors](../crates/sighurt-runtime/src/engines.rs) enumerate the supported engine names,
  capabilities, and honored settings.
- [The runtime registry](../crates/sighurt-runtime/src/registry.rs) contains named engine variants and
  a fixed keymap list.
- [The GUI chrome registry](../frontends/sighurt-gui/src/chrome.rs) lists the graphical chromes compiled
  into the browser.

These choices are workable during development, but an external implementation still requires
changes inside Sighurt to join the supplied application.

Instead, the application should supply registrations to the runtime and frontend. Each engine
adapter should describe its capabilities, validate its settings, and provide construction and
lifecycle integration. Frontend-specific presentation bindings may still be necessary.

The acceptance criterion is:

> An external Rust project can build a browser with its own chrome, keymap, and a new engine name
> without editing the Sighurt checkout.

This demonstrates engine independence more effectively than repository separation alone.

## Keep engine-specific knowledge with the adapter

Move implementation-specific configuration and lifecycle knowledge toward the implementation
that requires it.

For example, [the core configuration module](../crates/sighurt-core/src/config.rs) currently contains
Servo preference types and uses a generated Servo preference catalog. The shared configuration
system should eventually delegate engine-specific validation to the registered adapter, similar
to the validation callbacks already provided by keymaps and chromes.

Engines can require unusual process setup, presentation, and shutdown behavior. Registration
and lifecycle hooks should express those requirements explicitly, reducing the need for shared
code to recognize a particular engine by name.

## Separate the chrome toolkit from the standard implementation when useful

`sighurt-ui` currently contains both the chrome interface and reusable helpers, and the standard
tabs-and-toolbar implementation.

When developing external customization, consider extracting the standard implementation so
alternative chromes can depend on the interface and drawing helpers independently. Keep this a
focused change driven by an actual external consumer; there is no immediate need to divide the
UI toolkit into many small crates.

## Support broad Rust customization through explicit extension points

Trusted Rust configuration should have useful state access, typed operations, and hooks for
behavior that users should be able to replace. Potential areas include URL routing, commands,
tab selection, startup behavior, and permission decisions, in addition to chrome and keymaps.

Preserve coordinated state changes through the core. For example, a custom operation that
closes a tab should trigger the necessary engine teardown, focus changes, history handling, and
session updates through the same machinery as a built-in operation.

Develop these interfaces from real configurations. A broad API does not require predicting
every extension in advance, and exposing mutable internals everywhere would make customization
more fragile when internal representations change.

## Repository ownership

| Component | Recommendation now |
|---|---|
| Core, runtime, frontends, shared APIs | Keep together because they evolve together. |
| Supported engine adapters | Keep in the workspace under `engines/`, with optional dependencies. |
| Demo engine | Keep nearby as a lightweight implementation for exercising the engine contract. |
| Line chrome and modal keymap | Group as extensions, then use them to prove external customization. |
| Wasm SDK and example apps | Keep together for convenient development and teaching. |
| Experiments | Keep clearly identified as standalone engineering investigations. |

Separate repositories add dependency coordination, compatibility work, and CI maintenance.
Adopt them when independent ownership or release schedules provide enough benefit to justify
that work.

The first extraction should be a complete example configuration repository. It could contain a
small user-owned executable, the line chrome, the modal keymap, and documented engine and
behavior choices. It would both teach customization and expose missing API capabilities.

Initially, that repository can pin a Sighurt Git revision and commit its Cargo lockfile. Document
API changes without promising long-term API compatibility before the extension surface has
matured.

## Implementation order

1. Reorganize directories while preserving behavior and Cargo package names. Update workspace
   membership, relative dependencies, scripts, and documentation paths as part of the move.
2. Allow applications to supply engine, chrome, and keymap registrations.
3. Build one complete custom configuration outside the workspace that adds implementations
   without internal Sighurt edits.
4. Address the API gaps exposed by that external configuration, including engine-specific
   configuration and lifecycle integration.
5. Extract further components when separate maintenance has a concrete benefit.

The current crate count is manageable. The primary measure of success is whether those crate
boundaries let someone build and maintain their own browser assembly.
