"""Parsers for the diagnostics a browser build writes to stderr.

A parser turns one stderr line into an event dict, or None, and raises ValueError for a line
that is meant for it but malformed (bench/measure counts those as an anomaly). Every event has
"kind"; an event with "t" (CLOCK_MONOTONIC seconds, the clock of Python's time.monotonic()) is
placed at that time, others at the time the harness read the line. The metrics code understands
these kinds:

  frame     "delivered" (bool): the frame reached the next stage (baseline: sent over IPC to the
            browser; a directly presenting engine: presented). "presented" (bool, default
            false): the engine committed it to the compositor itself, so no browser-frame has
            to follow for it to be on screen. "readback_bytes" and "ipc_pixel_bytes": page
            pixels read back to the CPU and sent over IPC for it. Optional timings in ms:
            "paint_ms", "read_ms", "send_ms", "present_ms".
  renderer  how an engine renders: "engine", "mode" (hardware/software/...), "gl_renderer".
  first-frame  "after_navigate_ms": the engine's own time from navigation to its first frame.
  browser-frame  the browser drew its window (baseline: chrome and page area) once; "draw_ms".
  counters  "values": {name: int}, cumulative counts since the process started (never reset).
            metrics.window reports each one's increase over a window and its rate. An optional
            "group" keeps snapshots of different things apart (the counters of one engine).
  engine-timing  an engine's own timing of one frame, besides the frame event that counts it:
            "swap_ms".

A build names its parsers in builds/<build>.toml (`parsers = [...]`). To support a new binary's
diagnostics, add a class with `name` and `parse(line)` and list it in PARSERS. A class can
declare in `INCOMPARABLE` what its diagnostics can't measure honestly against another build's:
(scripts, metric pattern, reason), where scripts are the workload scripts it applies to (None:
all); bench/compare leaves those rows out and says why.
"""

import re
import shlex


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
    # Its navigation-to-first-frame time starts once the process has built Servo and its GL
    # renderer; sig's starts before the view's renderer exists. The launch-to-screen times compare.
    INCOMPARABLE = [
        (None, r"^startup\.first_frame_after_navigate_ms$",
         "sig-servo's navigation-to-first-frame time leaves out building Servo's renderer, sig's doesn't"),
    ]

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
    # A draw GPUI makes while dispatching a key is not logged (README, "Known limits"), so where
    # the script presses keys its window frames, and the chrome's input_to_screen, undercount.
    INCOMPARABLE = [
        (("type-url", "cycle-tabs", "tab-toggle", "tab-churn"), r"^(active|after|total)\.(browser_|input_to_screen)",
         "GPUI's log misses the draws it makes while dispatching a key"),
    ]
    UNIT_MS = {"ns": 1e-6, "µs": 1e-3, "us": 1e-3, "ms": 1.0, "s": 1e3}

    def parse(self, line):
        if m := self.LINE.match(line):
            return {"kind": "browser-frame", "draw_ms": float(m[1]) * self.UNIT_MS[m[2]]}
        return None


class SigDiag:
    """The rewrite's `sig` with SIG_DIAG=1 (sighurt-browser's diagnostics module). One line per
    event, a kind and `key=value` fields after the time; a value with spaces is double-quoted
    (Rust's `{:?}` of a string does it):

    sig-diag t=1234.567890 renderer engine=servo mode=hardware gl="NVIDIA GeForce RTX 4060/PCIe/SSE2"
    sig-diag t=1234.600000 first-frame engine=servo view=3 after_navigate_ms=41.5
    sig-diag t=1234.612345 frame engine=servo view=3 w=1280 h=696 presented=1 readback_bytes=0 paint_ms=1.9 present_ms=2.3
    sig-diag t=1234.612900 chrome surface=chrome draw_ms=0.42
    sig-diag t=1240.000000 counters input_events=12 pumps=5301 chrome_paints=4 page_frames=580
    sig-diag t=1240.000100 engine name=servo renderer="gl, straight into the page's subsurface" readbacks=0 gl_reads=0 state=running

    `t` is CLOCK_MONOTONIC in seconds. `frame` is a page frame an engine painted; `presented=1`
    when it was committed to its surface (the engines present themselves), and
    `readback_bytes` is required, 0 on the direct path: an omitted value would read as "no
    readback". No frame crosses a process, so `ipc_pixel_bytes` is 0. `paint_ms` is the
    engine's own painting (Servo's WebRender paint; the CPU engines' whole present) and
    `present_ms` the whole `present` call. `chrome` is one paint of a chrome-drawn surface (the
    chrome, the status strip, a page the chrome draws). `counters` is a snapshot of cumulative
    integer counters, printed when sig gets the build's `counters_signal` (bench/measure sends
    it at every mark) and at exit; each `engine` line after it is an engine's own cumulative
    counts, the page readbacks it counted and (with SIG_AUDIT_GL=1) its GL reads, kept as the
    counters group "engine.<name>". Other kinds (`peaks`, `target`) are ignored, so sig can
    print more than the harness reads; `--diagnostics FILE` has all of it as JSON.

    Counters alone give the volumes and rates: `page_frames_painted`, `page_frames_presented`,
    `readback_bytes` and `chrome_paints` become the frame, readback and browser-frame metrics
    (metrics.FROM_COUNTERS), and any other counter is reported as it is. The latency metrics
    need the `frame` and `chrome` lines, each an event with its time.
    """

    name = "sig-diag"
    PREFIX = "sig-diag "

    def parse(self, line):
        if not line.startswith(self.PREFIX):
            return None
        try:
            stamp, kind, *pairs = shlex.split(line[len(self.PREFIX) :])
            f = dict(pair.split("=", 1) for pair in pairs)
            if not stamp.startswith("t="):
                raise ValueError("no t= first")
            event = self._event(kind, f)
            if event is not None:
                event["t"] = float(stamp[2:])
            return event
        except (KeyError, ValueError) as e:
            raise ValueError(f"sig-diag: {e!r}") from None

    @staticmethod
    def _event(kind, f):
        ms = lambda key: float(f[key]) if key in f else None  # noqa: E731
        if kind == "frame":
            presented = f["presented"] == "1"
            return {"kind": "frame", "engine": f["engine"], "view": f.get("view"), "delivered": presented,
                    "presented": presented, "readback_bytes": int(f["readback_bytes"]), "ipc_pixel_bytes": 0,
                    "paint_ms": ms("paint_ms"), "present_ms": ms("present_ms"), "read_ms": ms("read_ms"), "send_ms": None}
        if kind == "chrome":
            return {"kind": "browser-frame", "surface": f.get("surface"), "draw_ms": float(f["draw_ms"])}
        if kind == "renderer":
            return {"kind": "renderer", "engine": f["engine"], "mode": f["mode"], "gl_renderer": f.get("gl", "")}
        if kind == "first-frame":
            return {"kind": "first-frame", "engine": f["engine"], "after_navigate_ms": float(f["after_navigate_ms"])}
        if kind == "counters":
            return {"kind": "counters", "values": {name: int(value) for name, value in f.items()}}
        if kind == "engine":
            values = {key: int(f[key]) for key in ("readbacks", "gl_reads") if key in f}
            return {"kind": "counters", "group": f"engine.{f['name']}", "values": values}
        return None


class ServoLogTiming:
    """sighurt-engine-servo's timing lines, which its `log` target sighurt_engine_servo::timing
    writes at debug level (`RUST_LOG=sighurt_engine_servo::timing=debug`) through the process's
    logger, after the logger's own prefix (`[<time> DEBUG sighurt_engine_servo::timing] `):

    servo-timing frame view=3 w=1280 h=696 paint_ms=1.234 swap_ms=0.210
    servo-timing first-frame view=3 after_navigate_ms=345.6

    They have no time stamp of their own, so they are placed when read. A frame line is an
    `engine-timing` event, not a `frame`: sig-diag's frame line counts the same frame. The
    first-frame line repeats sig-diag's and is left out. Enabling a debug target raises the
    `log` crate's maximum level for the whole process, so every `debug!` in Servo then asks the
    logger: measure with it apart from the runs whose CPU is compared.
    """

    name = "servo-log-timing"
    FRAME = re.compile(r"servo-timing frame view=(\d+) w=(\d+) h=(\d+) paint_ms=([\d.]+) swap_ms=([\d.]+)$")

    def parse(self, line):
        if "servo-timing " not in line:
            return None
        if m := self.FRAME.search(line):
            return {"kind": "engine-timing", "engine": "servo", "view": m[1], "w": int(m[2]), "h": int(m[3]),
                    "paint_ms": float(m[4]), "swap_ms": float(m[5])}
        if "servo-timing first-frame " in line:
            return None
        raise ValueError("servo-timing: not a frame or first-frame line")


PARSERS = {cls.name: cls for cls in [ServoTiming, GpuiFrames, SigDiag, ServoLogTiming]}


def incomparable(names):
    """What the parsers `names` declare they can't measure honestly against another build."""
    return [rule for name in names for rule in getattr(PARSERS[name], "INCOMPARABLE", [])]


def make(names):
    """The parsers a build asks for, as one function from a line to an event or None."""
    parsers = [PARSERS[name]() for name in names]

    def parse(line):
        for parser in parsers:
            if (event := parser.parse(line)) is not None:
                return event
        return None

    return parse
