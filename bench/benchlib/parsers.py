"""Parsers for the diagnostics a browser build writes to stderr.

A parser turns one stderr line into an event dict, or None. Every event has "kind"; the metrics
code understands these kinds:

  frame     "delivered" (bool): the frame reached the next stage (baseline: sent over IPC to the
            browser; a directly presenting engine: presented). "presented" (bool, default
            false): the engine committed it to the compositor itself, so no browser-frame has
            to follow for it to be on screen. "readback_bytes" and "ipc_pixel_bytes": page
            pixels read back to the CPU and sent over IPC for it. Optional timings in ms:
            "paint_ms", "read_ms", "send_ms".
  renderer  how an engine renders: "engine", "mode" (hardware/software/...), "gl_renderer".
  first-frame  "after_navigate_ms": the engine's own time from navigation to its first frame.
  browser-frame  the browser drew its window (chrome and page area) once; "draw_ms".

A build names its parsers in builds/<build>.toml (`parsers = [...]`). To support a new binary's
diagnostics, add a class with `name` and `parse(line)` and list it in PARSERS.
"""

import re


class ServoTiming:
    """sig-servo's SIG_SERVO_TIMING=1 lines at gpui-baseline:

    sig-servo: hardware rendering (NVIDIA GeForce RTX 4060/PCIe/SSE2), context created in 12.3 ms
    sig-servo: frame 1280x696: paint 1.2 ms, read 3.4 ms, sent in 5.6 ms
    sig-servo: frame 1280x696: paint 1.2 ms, read 3.4 ms, unchanged, not sent
    sig-servo: first frame 345 ms after navigating
    """

    name = "servo-timing"
    FRAME = re.compile(
        r"sig-servo: frame (\d+)x(\d+): paint ([\d.]+) ms, read ([\d.]+) ms, "
        r"(?:sent in ([\d.]+) ms|unchanged, not sent)"
    )
    RENDERER = re.compile(r"sig-servo: (\w+) rendering \((.*)\), context created in ([\d.]+) ms")
    FIRST = re.compile(r"sig-servo: first frame (\d+) ms after navigating")

    def parse(self, line):
        if m := self.FRAME.search(line):
            w, h = int(m[1]), int(m[2])
            delivered = m[5] is not None
            return {
                "kind": "frame",
                "engine": "servo",
                "w": w,
                "h": h,
                "delivered": delivered,
                "readback_bytes": w * h * 4,
                "ipc_pixel_bytes": w * h * 4 if delivered else 0,
                "paint_ms": float(m[3]),
                "read_ms": float(m[4]),
                "send_ms": float(m[5]) if delivered else None,
            }
        if m := self.RENDERER.search(line):
            return {
                "kind": "renderer",
                "engine": "servo",
                "mode": m[1],
                "gl_renderer": m[2],
                "setup_ms": float(m[3]),
            }
        if m := self.FIRST.search(line):
            return {"kind": "first-frame", "engine": "servo", "after_navigate_ms": float(m[1])}
        return None


class GpuiFrames:
    """GPUI 0.2.2's ZED_MEASUREMENTS=1 line, one per window frame it draws (`gpui_util::measure`
    around `Window::draw` and `present`; a frame that only re-presents is not logged). The value
    is a Rust `Duration` in `Debug` form:

    frame duration: 1.234567ms
    """

    name = "gpui-frames"
    LINE = re.compile(r"^frame duration: ([\d.]+)(ns|µs|us|ms|s)$")
    UNIT_MS = {"ns": 1e-6, "µs": 1e-3, "us": 1e-3, "ms": 1.0, "s": 1e3}

    def parse(self, line):
        if m := self.LINE.match(line):
            return {"kind": "browser-frame", "draw_ms": float(m[1]) * self.UNIT_MS[m[2]]}
        return None


PARSERS = {cls.name: cls for cls in [ServoTiming, GpuiFrames]}


def make(names):
    """The parsers a build asks for, as one function from a line to an event or None."""
    parsers = [PARSERS[name]() for name in names]

    def parse(line):
        for parser in parsers:
            if (event := parser.parse(line)) is not None:
                return event
        return None

    return parse
