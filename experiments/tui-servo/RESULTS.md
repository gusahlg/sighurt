# Servo 0.5 offscreen, no display server

Measured on louise-pc (NVIDIA GeForce RTX 4060, driver 595.80) over ssh. `WAYLAND_DISPLAY` and `DISPLAY` were unset for every run. The page is the local file `experiments/tui-servo/pages/page.html` (orange header, blue body, yellow second heading, three links, a CSS-animated red box, a green form, a fixed magenta 96×96 corner). No `https://` URL was loaded.

The binary is `target/debug/tui-servo` from a dev-profile build: the `tui-servo` crate itself is unoptimized, and `[profile.dev.package."*"]` builds dependencies (including Servo) at `opt-level = 3`. Timings below are from that binary.

`WebView::paint` and `glReadPixels` are timed separately. Readback includes the wait for the GPU to finish the paint (there is no `glFinish` inside paint). Median and p95 are over all 30 frames, including the first. `sum` is the sum of per-frame (paint + readback), not process wall time. A frame is treated as blank only when one RGB color covers at least 99% of the pixels. None of these frames were blank. Every run printed the same script result, `{"title":"Sighurt offscreen","headings":2,"links":3,"inputs":4}`, and exited 0. Shutdown is `Drop` of the webview, then Servo, then the GL context. The program does not call `process::exit`.

The connection opened is surfman's Mesa surfaceless backend (`EGL_PLATFORM_SURFACELESS_MESA`), chosen directly. `RenderingContext::connection` returns the unix multi-backend `Connection::Alternate` around that display, because `servo-paint-api` 0.5 enables surfman's `sm-x11` feature and that is the type Servo stores. The printed `surfman_connection_type` on every row is `surfman::multi::connection::Connection<Device<wayland::Device, x11::Device>, mesa_surfaceless::Device>`. No Wayland or X11 display was opened.

## Results

| Case | Setup | EGL vendor | GL renderer | OK | Paint ms med / p95 | Readback ms med / p95 | Sum ms | `LIBGL_ALWAYS_SOFTWARE` after | `DRI_PRIME` after |
|---|---|---|---|---|---|---|---|---|---|
| a | `--adapter hw`, default glvnd, 1280×720 | NVIDIA 1.5 | NVIDIA GeForce RTX 4060/PCIe/SSE2, GL 3.2.0 NVIDIA 595.80 | yes | 0.248 / 5.838 | 0.978 / 1.283 | 262.274 | unset | 1 |
| b | `--adapter sw`, default glvnd, 1280×720 | NVIDIA 1.5 | same RTX 4060, GL 3.2.0 NVIDIA 595.80 | yes | 0.221 / 0.913 | 0.990 / 1.406 | 51.002 | 1 | unset |
| c | `--adapter hw`, Mesa ICD only, 1280×720 | Mesa Project 1.5 | llvmpipe (LLVM 21.1.8, 256 bits), GL 4.6 Core Mesa 26.2.3 | yes | 1.187 / 1.428 | 0.560 / 0.692 | 1334.897 | unset | 1 |
| sw+mesa | `--adapter sw`, Mesa ICD only, 1280×720 | Mesa Project 1.5 | same llvmpipe, GL 4.6 Core Mesa 26.2.3 | yes | 1.291 / 1.739 | 0.498 / 0.693 | 255.158 | 1 | unset |
| d | `--adapter hw`, `/dev/dri` hidden, 1280×720 | NVIDIA 1.5 | same RTX 4060, GL 3.2.0 NVIDIA 595.80 | yes | 0.243 / 0.927 | 0.958 / 1.421 | 51.274 | unset | 1 |
| e 640 | `--adapter hw`, default glvnd, 640×360 | NVIDIA 1.5 | same RTX 4060 | yes | 0.149 / 0.200 | 0.352 / 0.816 | 22.722 | unset | 1 |
| e 1280 | same run as case a | NVIDIA 1.5 | same RTX 4060 | yes | 0.248 / 5.838 | 0.978 / 1.283 | 262.274 | unset | 1 |
| e 1920 | `--adapter hw`, default glvnd, 1920×1080 | NVIDIA 1.5 | same RTX 4060 | yes | 0.279 / 0.885 | 1.982 / 2.503 | 84.602 | unset | 1 |

Backend on every row: `mesa_surfaceless`. Adapter enum: `HardwarePrime` for `--adapter hw`, `Software` for `--adapter sw`. Both variables were unset before the context descriptor was created. First-paint times, which dominate `sum` on some rows: a 211.767 ms, b 10.616 ms, c 1282.916 ms, sw+mesa 199.214 ms, d 11.704 ms, e 640 6.550 ms, e 1920 10.603 ms. Case a's paint p95 is 5.838 because that 211.767 ms frame is inside the 30. Steady NVIDIA paints after the first frame sit near the median, about 0.2 ms.

Pixel samples on every 1280×720 frame: `(0,0)=#e67e22` (header), `(640,360)=#1b4f72` (body), `(1272,712)=#ff00ff` (marker). Mode color `#1b4f72` at fraction 0.6837. The 640×360 frame: mode fraction 0.5393, marker sample `(632,352)=#ff00ff`. The 1920×1080 frame: mode fraction 0.7980, center sample `(960,540)=#196f3d` (the green form), marker `(1912,1072)=#ff00ff`.

## What each case shows

**a.** Surfaceless EGL works with glvnd's NVIDIA vendor and no display server. The page, the animation flag, script evaluation, PNG write, and a zlib kitty-graphics payload (`/tmp/tui-servo-a/frame.kitty`) all succeeded. The PNG shows the header, heading, paragraph, three links, red box, form (`ada`, `hello`, `one`, Send), and the magenta corner.

**b.** `--adapter sw` does not select llvmpipe while NVIDIA's EGL is the vendor. NVIDIA ignores `LIBGL_ALWAYS_SOFTWARE`. The renderer stayed the RTX 4060. The variable was still set to `1` in this process. The PNG matches case a. A kitty payload was written to `/tmp/tui-servo-b/frame.kitty`.

**c.** `__EGL_VENDOR_LIBRARY_FILENAMES` pointed at the nix-shell Mesa 26.2.3 ICD, `/nix/store/1vkzwrp4ny20iljfnv9valq0bfn8czlv-mesa-26.2.3/share/glvnd/egl_vendor.d/50_mesa.json`. Mesa cannot drive this GPU: stderr reported pci id `10de:2882` with driver `(null)`, `failed to create dri2 screen`, and `failed to open /dev/dri/card1: Permission denied`. Mesa then used llvmpipe even though the adapter was `HardwarePrime` and `LIBGL_ALWAYS_SOFTWARE` stayed unset. `DRI_PRIME` was still set to `1`. The PNG is a real page, same chrome as case a. The first paint is 1.3 s; later frames are about 1.7 ms (paint + readback median).

**sw+mesa.** Same Mesa ICD with `--adapter sw`. This is the run that actually got llvmpipe together with `LIBGL_ALWAYS_SOFTWARE=1` and `DRI_PRIME` left unset. Same dri2 warnings, then llvmpipe. First paint 199 ms, later frames about 1.8 ms. Not one of the requested cases; included so case b is not described as llvmpipe. The PNG was not opened; its samples match the viewed frames (`blank=false`, same four sample colors, same script JSON).

**d.** Inside `tools/heavy`, `unshare --user --map-root-user --mount` mounted a tmpfs on `/dev/dri` (`mount --make-rprivate /` first). `ls /dev/dri` showed only `.` and `..`. No host root. NVIDIA still rendered the RTX 4060, because this path uses `/dev/nvidia*`, which was left in place. The PNG matches case a. `bwrap` was not used.

**e.** Same NVIDIA hardware path at three sizes. Readback median scales with the buffer: 0.352 ms at 640×360, 0.978 ms at 1280×720, 1.982 ms at 1920×1080. Paint median stays under 0.3 ms. The 1920×1080 PNG shows the full page and a large empty body under the form. The 640×360 PNG shows the header, the heading, the paragraph, and the start of the links. The document is taller than 360 px, so the red box and the form are below the fold, and the fixed 96×96 magenta marker covers the lower right of the visible text. That is the page's CSS in a short viewport, and the corner sample is still `#ff00ff`.

PNGs opened and checked by eye: cases a, b, c, d, e 640, e 1920. None is black, white, or a single flat color.

## What the terminal frontend should do

Use mesa surfaceless with the hardware adapter and the machine's normal glvnd vendor list. On this NVIDIA host that is the RTX 4060, with no display connection. It is the only configuration here that both renders on the GPU and does not force later Mesa clients onto llvmpipe.

Do not treat `--adapter sw` as "use llvmpipe" when the NVIDIA EGL vendor is selected. It does not change the renderer, and it does set `LIBGL_ALWAYS_SOFTWARE=1` for every child of this process.

Hiding the NVIDIA ICD is a working software fallback (llvmpipe paints a correct page), not a hardware path. Mesa 26.2.3 has no kernel driver for this 4060, so a hardware adapter under that ICD still lands on llvmpipe.

Hiding `/dev/dri` does not disable the NVIDIA surfaceless path.

`LIBGL_ALWAYS_SOFTWARE` and `DRI_PRIME` are mutated inside surfman's `create_context_descriptor`, before the GL context exists, and they are still set afterwards:

- hardware adapter: removes `LIBGL_ALWAYS_SOFTWARE` (it stays unset here), sets `DRI_PRIME=1`
- software adapter: sets `LIBGL_ALWAYS_SOFTWARE=1`, leaves `DRI_PRIME` unset

Those values are inherited by any process spawned later. WPE WebKit's helper processes would see them. Save the environment before the context descriptor is created and restore it after the descriptor call, before spawning WPE. Clearing the variables after the GL context exists does not change the context that was already created. This experiment only records the variables; it does not restore them.

`create_adapter` on the `Connection::Alternate` value Servo stores returns a hardware adapter and does not itself change the environment. A later WebGL device created from that stored connection would ask for a window config on the surfaceless display and reset the environment to hardware. The test page has no canvas and no WebGL, so that path was not measured.

## Not measured

- Displaying the kitty files with `cat` in a kitty terminal. The files for cases a and b were written (direct transmission, `f=32` RGBA, zlib `o=z`, base64 chunks of 4096). Their contents were not decoded or shown.
- `https://example.com/` or any other network page.
- A crash during `Servo`'s `Drop`. Clean exits were exit code 0.
- Hiding `/dev/nvidia*` as well as `/dev/dri`.
- Whether restoring `LIBGL_ALWAYS_SOFTWARE` and `DRI_PRIME` after context creation leaves the existing GL context on the same renderer. The recommendation above follows from when surfman reads the variables, not from a second probe.
