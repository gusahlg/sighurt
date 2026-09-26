# Hypotheses

Every change that is meant to make Sighurt faster or lighter gets an entry here **before** its
build is measured (README, "Method"): what should change, on which workloads and metrics, in which
direction and why, and which other metrics must not get worse. After the measurement, the entry
gets the two result sets it compared and the outcome: *supported*, *not supported* (the primary
metrics did not move beyond noise, or moved the wrong way) or *rejected with a regression*
(something else got worse beyond noise; the decision then goes into the decisions log of
`docs/rewrite/STATUS.md`).

Primary metrics are confirmatory; anything else `bench/compare` finds is a new hypothesis to
re-test, not a result. "Beyond noise" is `bench/compare`'s verdict: p < 0.05 and the medians
differ by more than the larger IQR.

The baseline numbers each hypothesis starts from are in [`results/BASELINE.md`](results/BASELINE.md).

## Open

### H1: Direct presentation removes Servo's readback (M1, M4)

- **Change:** Servo renders into a host-owned page subsurface and presents it (plan §9, §14),
  instead of `read_pixels` into a CPU array per frame.
- **Prediction:** `readback_mib_per_s` falls to 0 in the steady and active windows of every
  Servo workload (`article`, `long`, `css-animation`, `canvas`, `webgl`, `many-tabs`,
  `resize-storm`). This is §26's first hard criterion, so the new build must report its frames
  with `readback_bytes = 0` (not omit them).
- **Guards:** `startup.first_frame_s` and `active.fps_delivered` on the animation workloads.

### H2: No pixel transport, no second upload (M1, M4)

- **Change:** the same, seen from the browser: no frame crosses a pipe and GPUI no longer
  uploads it as a texture and draws it.
- **Prediction:** `ipc_pixel_mib_per_s` and `ipc_out_mib_per_s` fall to 0 on every workload,
  and `active.cpu_pct` and `steady.cpu_pct` fall on the animation workloads (`css-animation`,
  `canvas`, `webgl`), by at least the baseline's CPU share of reading, comparing and sending
  frames (`read_p50_ms` + `send_p50_ms` per produced frame, times the frame rate).
- **Guards:** `active.input_to_screen_p50_ms` on `long` and `article`; `steady.fps_produced`
  on the animations must not drop.

### H3: Page animation no longer repaints the chrome (M1, M3)

- **Change:** chrome and page are separate surfaces (§8, §13); a page frame is committed on the
  page's subsurface only.
- **Prediction:** on the animation workloads, the chrome's frame rate in the steady window
  (`steady.browser_fps`, which the new build must report for chrome paints only) falls from the
  baseline's, where GPUI redraws the whole window for every page frame, to 0 (§26: zero chrome
  repaints caused solely by page animation).
- **Guards:** `steady.cpu_pct`, `steady.wakeups_per_s`.

### H4: An idle browser sleeps (M3)

- **Change:** no permanent repaint timer in the chrome (§10, §26).
- **Prediction:** on `home`, `article` and `wasm-gradient`, `steady.browser_fps` is 0 and
  `steady.wakeups_per_s` and `steady.cpu_pct` are not higher than the baseline's; on `home`
  they fall, because the baseline's GPUI loop and engine processes are gone.
- **Guards:** `startup.mapped_s`.

### H5: One Servo runtime is lighter than one process per tab (M4)

- **Change:** one Servo runtime with a WebView per tab (§14, §18) instead of a `sig-servo`
  process each.
- **Prediction:** on `many-tabs`, `steady.pss_mean_mib` falls, by roughly seven times the
  per-process overhead the baseline's `article` shows beyond its page (one Servo, SpiderMonkey
  runtime, font and GL context each).
- **Risk:** hidden WebViews may keep GPU memory (§27): watch `steady.gpu_fb_tree_mib` and
  `steady.gpu_mem_used_mib`.
- **Guards:** `active.input_to_screen_p50_ms` (tab switches) on `many-tabs`.

### H6: Resizes stay bounded (M1, M3)

- **Change:** layout epochs, target recreation and SHM buffer rules (§10).
- **Prediction:** on `resize-storm`, `after.pss_growth_mib` and `after.rss_growth_mib` are no
  higher than the baseline's, and repeated sets show no trend with the round number.
- **Guards:** `active.cpu_pct` on `resize-storm`.

### H7: Startup does not get slower (M4, M5)

- **Change:** engines initialize in `sig` instead of in a spawned process per page.
- **Prediction:** `startup.first_frame_s` does not rise beyond noise on any workload, and falls
  on `many-tabs`, where the baseline starts eight Servo processes at once.
- **Guards:** `startup.pss_peak_mib`.

### H8: The Wasm guests keep their cost (M5)

- **Change:** the Wasm engine draws its guests on the CPU into its own surface (§15) instead of
  sending draw lists for GPUI to paint.
- **Prediction:** no change beyond noise in `steady.cpu_pct` and `active.cpu_pct` on
  `wasm-hello`, `wasm-gradient`, `wasm-raf` and `wasm-timer`; `ipc_out_mib_per_s` falls to 0.
  This is a guard hypothesis (§27: CPU Wasm rendering may be too slow), not an expected gain.

### H9: A busy guest in the same process doesn't slow the chrome (M5)

- **Change:** the Wasm engine runs in `sig`'s process (§15, §18) instead of its own.
- **Prediction:** on `wasm-busy`, `active.input_to_screen_p50_ms` and `_p95_ms` (opening and
  closing a tab while the guest burns most of its fuel every frame) are not higher than the
  baseline's beyond noise, where the guest has its own process. This is M5's "guest activity
  does not starve chrome input", measured.
- **Guards:** `steady.cpu_pct` on `wasm-busy` (the guest's own cost should not change much).

## Tested

None yet.
