# Sighurt's patch of surfman 0.14.0

This directory is the crates.io source of surfman 0.14.0 (the version in `Cargo.lock`),
including its `LICENSE-APACHE`, `LICENSE-MIT` and `LICENSE-MPL` files. The workspace
uses it through `[patch.crates-io]`.

One behavioral change: `create_context_descriptor` no longer calls
`Adapter::set_environment_variables`. That method calls `std::env::set_var` and
`remove_var` for `LIBGL_ALWAYS_SOFTWARE` and `DRI_PRIME`. Servo calls
`create_context_descriptor` after its threads exist, including when a page creates a
WebGL context, which is undefined behavior.

Removed from the unmodified 0.14.0 sources, immediately before the `unsafe` block that
builds the EGL config (a comment and the call):

- `src/mesa_surfaceless/device.rs` lines 143–144. `create_context_descriptor` began at
  line 139. The body now begins with `unsafe {` (line 145 after the attribute below).
- `src/wayland/device.rs` lines 95–96. The function began at line 91.
- `src/x11/device.rs` lines 90–91. The function began at line 86.

Each removed pair was:

```rust
        // Set environment variables as appropriate.
        self.adapter.set_environment_variables();
```

`set_environment_variables` itself is unchanged and is now unused (the Wayland and X11
modules re-export `Adapter` from `mesa_surfaceless`). It is marked `#[allow(dead_code)]`
in `src/mesa_surfaceless/device.rs` (the comment and attribute sit above the function,
which is now at line 58) so the body can still be compared with upstream.

Adapter selection is otherwise the same: Sighurt tries `create_hardware_adapter` first
and `create_software_adapter` only if that context cannot be made. Without the
environment writes, Mesa uses the device the process was started with (the GPU, unless
the process was launched with `LIBGL_ALWAYS_SOFTWARE`). The software adapter cannot
force llvmpipe on its own. Sighurt's offscreen path does not set either variable.
