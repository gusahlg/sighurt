I would keep the scene-based architecture, but revise the presentation, lifetime and cache contracts before implementation. The following are ranked by severity. References to sections are to [RENDER.md](/home/gusahlg/repos/sighurt/docs/rewrite/RENDER.md).

1. **Blocking: FIFO can deadlock the owner thread during synchronized layout transactions.** §§4, 7–8.

   The concrete cycle is: a child commit is cached because the subsurface is synchronized; a newer layout needs another child frame; the presenter waits for the previous frame callback; that callback requires the parent commit; the parent commit cannot happen until rendering returns. The host explicitly bootstraps these cases in [scheduler.rs](/home/gusahlg/repos/sighurt/frontends/sighurt-wayland/src/scheduler.rs:9), but that only bypasses **its own** callback gate.

   Mesa’s legacy Vulkan FIFO path requests a private `wl_surface.frame` callback and waits for it inside a subsequent `queue_present`. MAILBOX avoids that gate. Mesa with `wp_fifo_v1` uses compositor-side barriers instead; that is a materially different implementation, not evidence that all FIFO paths are safe. [Mesa WSI source](https://chromium.googlesource.com/external/gitlab.freedesktop.org/mesa/mesa/+/7215c4ef5fba349dbd5debb6c8fd336f1e3fef48/src/vulkan/wsi/wsi_common_wayland.c). The FIFO protocol explicitly ignores its wait constraint for synchronized subsurfaces. [Protocol](https://wayland.app/protocols/fifo-v1).

   Host and driver callbacks are separate objects, normally on separate event queues. They do not replace or consume each other. Forgetting the host callback on unmap does not cancel the driver’s wait. Compositors may withhold callbacks for invisible surfaces. [Wayland specification](https://wayland.freedesktop.org/docs/html/apa.html#protocol-spec-wl_surface-request-frame).

   **Change:** require MAILBOX for the initial Vulkan path, with CPU fallback when the tested presentation policy cannot be met. Do not silently descend to FIFO. Treat IMMEDIATE as a separately negotiated option, not an alias for nonblocking presentation. Make hide/remap explicitly suspend and recreate presentation resources, with retirement independent of frame callbacks.

2. **Blocking: “never blocking” is incompatible with the selected wgpu API and synchronization policy.** §§4, 7–8.

   Local wgpu 30.0.0 passes a **1,000 ms timeout** to surface acquisition. Reconfiguration waits for GPU work, and its Vulkan swapchain retirement calls `vkDeviceWaitIdle`. MAILBOX does not eliminate acquisition, GPU or buffer-release waits. See [acquisition](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/wgpu-core-30.0.0/src/present.rs:32) and [retirement](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/wgpu-hal-30.0.0/src/vulkan/swapchain/native.rs:370).

   The GL fallback is worse for this host: wgpu 30’s EGL backend advertises only FIFO on non-Windows platforms and does not set swap interval zero. Servo explicitly does so. Mesa’s EGL path waits on its throttle callback when the interval is positive; interval zero still permits synchronization and buffer-availability waits. [wgpu capabilities](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/wgpu-hal-30.0.0/src/gles/adapter.rs:1370), [Mesa EGL source](https://chromium.googlesource.com/external/gitlab.freedesktop.org/mesa/mesa/+/c541f5e93061f98bbe83c98bea2336184966142b/src/egl/drivers/dri2/platform_wayland.c).

   **Change:** cut wgpu GL from v1. Distinguish “does not intentionally wait for vsync” from “cannot block.” If bounded owner-thread latency is mandatory, a wgpu acquisition change or a presentation worker protocol is required; the current trait cannot promise it. Measure acquire, configure, submit, present and teardown separately.

3. **Blocking: `bind` is a safe function with an unenforced unsafe lifetime requirement.** §4.

   Any safe caller can supply a short-lived `NativeWindow`, successfully bind, then destroy it without calling `unbind`. The backend retains native pointers after the borrow ends. Documentation cannot make that safe.

   The existing host destruction order is broadly correct: graphics resources first, then lease retirement, then native surface destruction. Destroying a subsurface role to unmap it is different from destroying its `wl_surface`; the lease preserves the latter. However, the proposed `trim` recreates a surface later without receiving handles again, so the backend necessarily retains either handles or an owner.

   **Change:** either make binding explicitly unsafe with a precise lifetime contract, or transfer an owned native-target guard that retains the backing objects. Specify cleanup for partial binding failures, ordinary close, fallback, unwind and device loss.

   Separate **surface lifetime** from **display lifetime**. A shared EGL instance can outlive an individual target; it needs a connection guard independent of that target’s lease. The backend must destroy its native surfaces before their guards and release display-level state before the connection. [wgpu’s handle requirements](https://wgpu.rs/doc/wgpu/enum.SurfaceTargetUnsafe.html).

4. **Blocking: the ash synchronization scheme reuses presentation resources too early.** §8.

   One render-finished semaphore per surface, reused after its rendering fence signals, is invalid. A submission fence proves rendering completion; it does not prove that the presentation engine has finished waiting on that semaphore.

   **Change:** use acquisition semaphores and command resources per submission slot, but render-finished semaphores **per swapchain image**, or use supported swapchain-maintenance presentation fences. Retire swapchains and their presentation resources using the appropriate completion mechanism. “Device idle for that surface’s fence” is not a Vulkan operation, and a render fence alone does not establish safe presentation-resource destruction. [Khronos synchronization guidance](https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html).

   Reset a submission fence only when a submission will actually follow; zero-timeout acquisition failure must not leave an unsignaled fence that the next frame waits on. Handle `NOT_READY`/`TIMEOUT` without losing the repaint.

   Also define synchronization for shared atlas images, descriptors and staging memory across **all surfaces**. One frame in flight per surface still permits several frames to reference shared resources concurrently. Include noncoherent mapped-memory flushing and transfer/sampling barriers.

5. **High: the EGL fallback can interfere with Servo’s display ownership.** §§7, 10.

   wgpu and surfman can obtain the same `EGLDisplay` for the same native connection. wgpu maintains its own display reference count, but that count cannot see Servo’s users. Servo separately calls `eglTerminate` at runtime shutdown. Dropping one library’s display owner can therefore invalidate another library’s resources when they share that display. See [wgpu’s private reference count](/home/gusahlg/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/wgpu-hal-30.0.0/src/gles/egl.rs:354) and [Servo’s termination contract](/home/gusahlg/repos/sighurt/engines/sighurt-engine-servo/src/presentation.rs:410).

   **Change:** keep the initial chrome GPU path Vulkan-only. Adding EGL later requires an explicit shared-display ownership policy, plus context-binding discipline around interleaved Servo/wgpu calls. Merely changing final shutdown order does not resolve independent runtime teardown.

   Separate Vulkan instances/devices for wgpu and ash are otherwise reasonable. Servo’s EGL presentation and WPE’s DMA-BUF presentation can coexist on distinct leaves without sharing textures or synchronization objects with the chrome. Keep each leaf under one presenter, retain the existing libwayland interoperation, and do not mutate process-wide GPU-selection environment variables.

6. **High: the result/error model cannot preserve progress or express the scope of failure.** §§4, 10.

   `Output` and `Lost` omit temporary acquisition failure, occlusion, reconfiguration, surface loss, device loss, allocation failure and validation failure. Device loss affects every surface sharing that device; falling back only the surface currently rendering leaves the others broken.

   More immediately, [the scheduler consumes invalidation when issuing a permit](/home/gusahlg/repos/sighurt/frontends/sighurt-wayland/src/scheduler.rs:92), and `NotReady` deliberately waits for another readiness signal. A backend forbidden to wake its owner can lose the last repaint indefinitely after a failed acquisition.

   **Change:** introduce explicit outcomes such as submitted, unchanged, deferred and failed, with failure scope distinguished between surface and device. A deferred frame must retain pending content and arrange a concrete retry source. Permit completion, retirement and retry behavior need to be part of the contract. Work-dependent completion notifications or bounded retry deadlines do not imply a permanent repaint timer.

   Update the design to the actual wgpu 30 API: `get_current_texture` returns `CurrentSurfaceTexture`, including `Suboptimal(texture)`, `Timeout`, `Occluded`, `Outdated`, `Lost` and `Validation`. Lost surfaces require recreation; suboptimal acquisition still yields a usable texture. Presentation uses `Queue::present`. Device-loss and uncaptured-error handling also need explicit installation. [Acquisition outcomes](https://wgpu.rs/doc/wgpu/api/surface_texture/enum.CurrentSurfaceTexture.html), [presentation API](https://wgpu.rs/doc/wgpu/struct.Queue.html#method.present).

   CPU fallback must also reinstall placement, stacking, input regions, focus and IME targeting for the replacement `TargetId`. It is more than changing a renderer field.

7. **High: startup and binding happen before the information needed to choose a working backend exists.** §§7–10.

   `start(Fonts)` chooses an adapter without a compatible surface. `bind` supposedly configures using metrics it does not receive. Host targets initially have empty metrics; configuring zero dimensions is invalid.

   There is also a concrete wgpu 30 mismatch: `SurfaceTargetUnsafe::from_window` supplies **only the window handle**. With the proposed instance construction, the Wayland display is missing. GLES additionally requires the display at instance creation. [Surface target API](https://wgpu.rs/doc/wgpu/enum.SurfaceTargetUnsafe.html), [instance display requirement](https://wgpu.rs/doc/wgpu/struct.InstanceDescriptor.html).

   **Change:** separate instance creation, native-surface creation, compatible-adapter selection and configuration. Configure lazily on the first nonempty metrics snapshot. Define startup success as a usable presentation path, not merely finding an adapter.

   Query formats, color spaces, alpha modes, present modes, extent limits and image counts. Treat `Bgra8Unorm` as a preference. Verify compatibility for every additional surface. `LowPower` is only a preference; nothing here establishes that Servo uses the discrete GPU, or that selecting another GPU reduces total composition cost.

8. **High: atlas growth/reset can corrupt the frame currently being prepared.** §6.

   If preparation emits UVs and subsequently doubles the atlas dimensions, earlier normalized UVs become wrong. Resetting after overflow invalidates earlier allocations outright. Restarting preparation can loop forever when one frame’s working set exceeds the maximum atlas.

   **Change:** use a two-stage operation: collect/rasterize the frame’s required resources, then allocate them before emitting final geometry. Give atlas allocations a generation. Preserve generations referenced by submitted work until completion.

   Define an oversized-working-set outcome: bounded additional pages, ordered batches with safe resource lifetimes, or a clear fallback. “Reset and retry” is insufficient.

   Dirty rows also need defined allocation/recreation semantics: a newly created texture needs all retained content uploaded. Atlas tiles require filtering gutters or a precisely defined nearest-sampling policy. Coalesce upload regions rather than making one API call per row.

9. **High: incremental CPU painting is not implementable through the proposed API and would corrupt retained pixels if moved mechanically.** §5.

   `Output::Pixels(PixelBuffer)` carries no buffer age. A three-buffer limit does not imply that a reused buffer is at most three submissions old.

   Correct repair requires clearing each repair region and replaying **every intersecting item in painter order**, including unchanged translucent items. Removed or moved items require old and new bounds; strokes, glyph bearings and antialiasing require conservative bounds. Clear-color changes damage the whole surface. Comparing `RunId` or `PathId` values alone misses changed resource contents.

   Today’s [canvas](/home/gusahlg/repos/sighurt/ui/sighurt-ui/src/canvas.rs:146) swaps red and blue across the whole packed buffer after painting. Unchanged pixels in a reused SHM buffer are already BGRA: swapping them again corrupts them. The shared padded-stride scratch pixmap is not necessarily the acquired buffer’s previous image either.

   **Change:** ship full CPU repaint first. For incremental painting, pass age and content-generation information explicitly; advance history only on successful submission; repaint fully when history is insufficient. One simple implementation renders cleared dirty tiles into RGBA scratch and converts/copies only those tiles into SHM. Distinguish buffer repair damage from changes relative to the last submitted scene. Do not promise that compositor upload behavior follows the reported rectangles exactly.

10. **High: opaque presentation changes the existing chrome semantics.** §§7–8.

    The scene supports alpha, and the current chrome API permits transparent surfaces. Selecting opaque composite alpha unconditionally can make CPU and GPU versions visibly different, especially overlays.

    **Change:** either declare and validate an opaque-surface restriction, or negotiate premultiplied surface composition. Keep this separate from shader source-over blending and the host’s opaque-region hint.

    Specify the color contract now: input color encoding, blending space, premultiplication and color-glyph conversion. Switching between UNORM and sRGB attachments changes blending results unless the pipeline compensates. Sharing Swash bitmaps does not make text antialiasing identical after filtering, placement, blending and quantization. The comparison tests must cover translucent backgrounds, colored glyphs and sampling boundaries—not just SDF corners.

11. **Medium: logical glyph positions are the right representation, but the proposed flattening loses existing placement rules.** §3.

    Current painting maps the text origin using actual buffer-to-logical ratios, but scales glyph spacing with nominal scale. It rounds the line baseline separately from the glyph’s vertical offset. One flattened logical position does not preserve all those operations at fractional scales and odd buffer sizes.

    `LayoutGlyph::physical` also incorporates font-relative offsets and each glyph’s font size. Combining those offsets and the baseline before rounding can move combining marks. See [current text placement](/home/gusahlg/repos/sighurt/ui/sighurt-ui/src/canvas.rs:242).

    **Change:** preserve run origin, baseline and glyph-local offsets separately, then use one shared physical-glyph resolver for both CPU and GPU. Preserve per-glyph size where necessary. Test combining marks, negative bearings, fallback fonts, emoji and clipped text at odd dimensions.

    Wrap cosmic-text IDs and flags instead of exposing them through the supposedly library-neutral scene. Associate font IDs with a font-resource identity/generation. Share the font database, but keep shaping policy and cache invalidation explicit; changing a shared family must not leave another window’s shaped-text cache stale.

12. **Medium: icon cache identity and rasterization phase are underspecified.** §§3, 6.

    `PathId` indexes one scene’s vector. Path zero can mean “back” in one scene and “close” in another. `(path id, stroke, scale)` is therefore not a persistent cache key.

    Nominal scale alone is insufficient: current paths use actual X/Y ratios, and unsnapped fractional translation changes coverage. Rasterizing at the origin and translating the bitmap later is not generally identical to directly rasterizing the transformed path.

    **Change:** key normalized path content or immutable resource identity, stroke/fill style, actual transform and rasterization phase. Alternatively, explicitly snap icon origins and accept/test that behavior change. Include stroke expansion and filter padding in tile bounds.

13. **Medium: shared GPU preparation is useful, but “thin backends” is drawn too aggressively.** §6.

    CPU rendering directly from the scene is correct. GPU preparation should share geometry, rasterization and packing decisions. Device capabilities, resource lifetimes, upload scheduling and pipeline specialization still belong to each backend.

    The proposed representation already contradicts itself: scissor changes require separate draw commands, so there is not one draw call per surface. Rectangle center, half-size and radius require five scalars, exceeding `params: vec4`.

    **Change:** return ordered batches with clip, resource bindings and index ranges. Let primitive data evolve without freezing a universal vertex layout into the backend contract. Define shader binding layouts, buffer alignment, coordinate conventions and Naga translation options explicitly. WGSL validation alone does not verify matching Rust layouts or Vulkan pipeline interfaces.

14. **Medium: the stated memory bound is false, and future resource ownership needs a contract now.** §§6, 9, 11.

    Mesa’s Wayland MAILBOX implementation provisions at least four images in the examined code; “minimum + 1” can request more. The promised two-to-three-image bound is incorrect. [Mesa image-count logic](https://chromium.googlesource.com/external/gitlab.freedesktop.org/mesa/mesa/+/7215c4ef5fba349dbd5debb6c8fd336f1e3fef48/src/vulkan/wsi/wsi_common_wayland.c).

    A 4096² R8 atlas plus RGBA8 atlas occupies **80 MiB** per copy. CPU backing plus GPU textures is 160 MiB before cached glyph images, staging, meshes and retiring generations.

    **Change:** budget bytes and retiring generations, report actual swapchain counts, and bound transient growth.

    Three missing contracts deserve decisions before implementation:

    - **Images:** `#[non_exhaustive]` does not solve resource ownership. Reserve immutable/versioned image identities, dimensions, alpha/color metadata, sampling and update rules. Large thumbnails need independent textures or tiling; an RGBA glyph atlas is not enough.
    - **Native damage:** damage returned after `render` has already presented is too late to affect that presentation. Keep full native redraw/damage for v1. Later, provide damage before presentation through supported WSI mechanisms; separate repaint damage from presentation damage.
    - **Scale and multiple windows:** use explicit per-surface configuration generations, including scale changes with unchanged buffer dimensions. Prefer a renderer/device service shared across windows where compatible. Keep surface histories separate and caches keyed by scale rather than clearing one global cache whenever a different-scale window renders.

15. **Medium: the acceptance tests prove appearance but omit the principal failure modes.** §12.

    Add lifecycle tests for two layouts before the parent commit, hide immediately after submission, remap, workspace occlusion, repeated resize, zero-size restoration, acquisition exhaustion, device-wide loss, closing one of several windows, and atlas overflow while another surface remains in flight.

    Test both legacy callback-based FIFO and compositor FIFO-protocol environments if FIFO remains supported. Inject surface loss separately from device loss. Measure owner-loop stalls and retirement memory, not only rendering duration. A screenshot and a renderer-name check cannot establish liveness or correct GPU synchronization.

For **v1**, I would ship scene extraction, the existing full CPU renderer, a wgpu Vulkan path with an explicit presentation policy, scoped CPU fallback, correct text/icon caches, and lifecycle diagnostics. I would defer incremental CPU painting, wgpu EGL fallback, native partial damage, the separate ash implementation, Metal-specific preparation and Wasm-canvas reuse. Keep ash as the next opt-in backend after the presentation and resource contracts survive implementation.

I would retain the backend-neutral scene, surface-local logical coordinates, shared snapping and glyph rasterization, CPU-rasterized small icons, CPU rendering directly from scene semantics, host-owned target leases and frame permits, independent chrome surfaces, per-surface fallback for surface-specific failures, and the prohibitions on routine readback and permanent repaint timers.