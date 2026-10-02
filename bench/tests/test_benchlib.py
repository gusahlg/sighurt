"""Tests of the harness: statistics, parsers, metrics and the compare and report tools.

  python3 -m unittest discover -s bench/tests
"""

import importlib.machinery
import importlib.util
import itertools
import json
import math
import pathlib
import random
import subprocess
import sys
import tempfile
import unittest

BENCH = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))

from benchlib import metrics, parsers, seat, stats  # noqa: E402


def script(name):
    """One of bench/'s executable scripts, imported as a module."""
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), str(BENCH / name))
    module = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
    loader.exec_module(module)
    return module


def brute_force_p(a, b):
    """The exact two-sided permutation p-value of the rank sum, by listing every split."""
    r = stats.ranks(list(a) + list(b))
    n1 = len(a)
    mean = n1 * (len(r) + 1) / 2
    observed = abs(sum(r[:n1]) - mean)
    splits = list(itertools.combinations(range(len(r)), n1))
    extreme = sum(1 for c in splits if abs(sum(r[i] for i in c) - mean) >= observed - 1e-9)
    return extreme / len(splits)


class Stats(unittest.TestCase):
    def test_quartiles_match_numpy_default(self):
        # numpy.percentile([1, 2, 3, 4, 5], [25, 75]) == [2, 4]; [1..4] gives [1.75, 3.25].
        self.assertEqual(stats.quartiles([5, 1, 4, 2, 3]), (2, 4))
        self.assertEqual(stats.quartiles([1, 2, 3, 4]), (1.75, 3.25))
        self.assertEqual(stats.iqr([7]), 0)

    def test_ranks_average_ties(self):
        self.assertEqual(stats.ranks([10, 20, 20, 5]), [2, 3.5, 3.5, 1])

    def test_completely_separated_samples(self):
        # The smallest two-sided p for 5 against 5 is 2 / C(10, 5) = 2 / 252.
        u, p = stats.mann_whitney([1, 2, 3, 4, 5], [6, 7, 8, 9, 10])
        self.assertEqual(u, 0)
        self.assertAlmostEqual(p, 2 / 252)
        self.assertAlmostEqual(stats.mann_whitney([6, 7, 8, 9, 10], [1, 2, 3, 4, 5])[1], 2 / 252)

    def test_identical_samples(self):
        self.assertEqual(stats.mann_whitney([3, 3, 3], [3, 3, 3])[1], 1.0)

    def test_exact_p_matches_brute_force_with_ties(self):
        rng = random.Random(1)
        for _ in range(200):
            n1, n2 = rng.randint(1, 7), rng.randint(1, 7)
            a = [rng.randint(0, 5) for _ in range(n1)]
            b = [rng.randint(0, 5) for _ in range(n2)]
            with self.subTest(a=a, b=b):
                self.assertAlmostEqual(stats.mann_whitney(a, b)[1], brute_force_p(a, b))

    def test_normal_approximation_is_close_for_large_samples(self):
        rng = random.Random(2)
        a = [rng.gauss(0, 1) for _ in range(25)]
        b = [rng.gauss(0.8, 1) for _ in range(25)]
        u, p = stats.mann_whitney(a, b)
        # The exact permutation p-value of the same data, which the approximation stands in for.
        exact = stats._exact_p(stats.ranks(a + b), len(a), sum(stats.ranks(a + b)[: len(a)]))
        self.assertLess(abs(p - exact), 0.01)
        self.assertLess(p, 0.05)

    def test_benjamini_hochberg(self):
        # R: p.adjust(c(0.01, 0.04, 0.03, 0.5), "BH") == 0.04 0.0533 0.0533 0.5
        q = stats.benjamini_hochberg([0.01, 0.04, 0.03, 0.5])
        for got, want in zip(q, [0.04, 0.16 / 3, 0.16 / 3, 0.5]):
            self.assertAlmostEqual(got, want)


class ServoTiming(unittest.TestCase):
    def test_lines(self):
        parse = parsers.make(["servo-timing"])
        sent = parse("sig-servo: frame 1280x696: paint 1.2 ms, read 3.4 ms, sent in 5.6 ms")
        self.assertEqual(sent["kind"], "frame")
        self.assertTrue(sent["delivered"])
        self.assertEqual(sent["ipc_pixel_bytes"], 1280 * 696 * 4)
        self.assertEqual((sent["paint_ms"], sent["read_ms"], sent["send_ms"]), (1.2, 3.4, 5.6))
        same = parse("sig-servo: frame 800x600: paint 0.5 ms, read 2.0 ms, unchanged, not sent")
        self.assertFalse(same["delivered"])
        self.assertEqual((same["readback_bytes"], same["ipc_pixel_bytes"]), (800 * 600 * 4, 0))
        renderer = parse("sig-servo: hardware rendering (NVIDIA GeForce RTX 4060/PCIe/SSE2), context created in 12.3 ms")
        self.assertEqual((renderer["mode"], renderer["gl_renderer"]), ("hardware", "NVIDIA GeForce RTX 4060/PCIe/SSE2"))
        self.assertEqual(parse("sig-servo: first frame 345 ms after navigating")["after_navigate_ms"], 345)
        self.assertIsNone(parse("some other line"))


class GpuiFrames(unittest.TestCase):
    def test_duration_units(self):
        parse = parsers.make(["gpui-frames"])
        # Rust's Debug for Duration: 1.5ms, 850.25µs, 999ns, 1.2s.
        for line, ms in [("frame duration: 1.5ms", 1.5), ("frame duration: 850.25µs", 0.85025),
                         ("frame duration: 999ns", 0.000999), ("frame duration: 1.2s", 1200.0)]:
            with self.subTest(line=line):
                event = parse(line)
                self.assertEqual(event["kind"], "browser-frame")
                self.assertAlmostEqual(event["draw_ms"], ms)
        # Only the whole line: sig-servo's and tracing's lines never count as browser frames.
        self.assertIsNone(parse("sig-servo: frame duration: 1ms"))


class SigDiag(unittest.TestCase):
    def test_lines(self):
        parse = parsers.make(["sig-diag"])
        frame = parse("sig-diag t=12.5 frame engine=servo view=3 w=1280 h=696 presented=1 readback_bytes=0 paint_ms=1.9 present_ms=2.25")
        self.assertEqual(frame, {"kind": "frame", "engine": "servo", "view": "3", "delivered": True, "presented": True,
                                 "readback_bytes": 0, "ipc_pixel_bytes": 0, "paint_ms": 1.9, "present_ms": 2.25,
                                 "read_ms": None, "send_ms": None, "t": 12.5})
        # An engine's own counts, as sig prints them after the counters (pairs() in diagnostics.rs).
        engine = parse('sig-diag t=2.5 engine name=servo renderer="gl, straight into the page\'s subsurface" '
                       'gl="hardware: NVIDIA GeForce RTX 4060/PCIe/SSE2" readbacks=2 gl_reads=0 state=running')
        self.assertEqual(engine, {"kind": "counters", "group": "engine.servo", "values": {"readbacks": 2, "gl_reads": 0}, "t": 2.5})
        unaudited = parse("sig-diag t=2.5 engine name=wasm renderer=cpu readbacks=0 state=running")
        self.assertEqual(unaudited["values"], {"readbacks": 0})
        self.assertIsNone(parse("sig-diag t=2.5 peaks max_pump_us=300"))
        chrome = parse("sig-diag t=13 chrome surface=status draw_ms=0.25")
        self.assertEqual((chrome["kind"], chrome["draw_ms"], chrome["t"]), ("browser-frame", 0.25, 13.0))
        renderer = parse('sig-diag t=1 renderer engine=servo mode=hardware gl="NVIDIA GeForce RTX 4060/PCIe/SSE2"')
        self.assertEqual(renderer["gl_renderer"], "NVIDIA GeForce RTX 4060/PCIe/SSE2")
        self.assertEqual(parse("sig-diag t=1 first-frame engine=wasm view=4 after_navigate_ms=41.5")["after_navigate_ms"], 41.5)
        self.assertEqual(parse("sig-diag t=2 counters pumps=10 chrome_paints=3")["values"], {"pumps": 10, "chrome_paints": 3})
        # Kinds the harness doesn't read, and other lines, are nothing.
        self.assertIsNone(parse("sig-diag t=2 layout epoch=4"))
        self.assertIsNone(parse("frame duration: 1ms"))

    def test_malformed_lines_are_errors(self):
        parse = parsers.make(["sig-diag"])
        for line in [
            "sig-diag t=1 frame engine=servo presented=1",  # no readback_bytes: not "no readback"
            "sig-diag frame engine=servo presented=1 readback_bytes=0",  # no time
            "sig-diag t=1 counters pumps=many",
            'sig-diag t=1 renderer engine=servo mode="unclosed',
        ]:
            with self.subTest(line=line), self.assertRaises(ValueError):
                parse(line)


class ServoLogTiming(unittest.TestCase):
    def test_lines(self):
        parse = parsers.make(["servo-log-timing"])
        frame = parse("[2026-10-02T10:00:00Z DEBUG sighurt_engine_servo::timing] "
                      "servo-timing frame view=3 w=1280 h=696 paint_ms=1.234 swap_ms=0.210")
        self.assertEqual(frame, {"kind": "engine-timing", "engine": "servo", "view": "3", "w": 1280, "h": 696,
                                 "paint_ms": 1.234, "swap_ms": 0.21})
        # sig-diag's first-frame line already says it; other lines are nothing.
        self.assertIsNone(parse("[x DEBUG sighurt_engine_servo::timing] servo-timing first-frame view=3 after_navigate_ms=345.6"))
        self.assertIsNone(parse("sig-diag t=1 frame engine=servo presented=1 readback_bytes=0"))
        with self.assertRaises(ValueError):
            parse("[x DEBUG sighurt_engine_servo::timing] servo-timing frame view=3 w=1280")


class FakeSampler:
    """Just the parts of ProcSampler that metrics.window reads."""

    def __init__(self, root, full, marks, light=()):
        self.root, self.full, self.marks, self.light = root, full, marks, list(light)


def proc(utime, stime, rss, pss, threads, wchar=0, syscw=0):
    return {"utime": utime, "stime": stime, "rss": rss, "pss": pss, "wchar": wchar, "syscw": syscw, "threads": threads}


class NoGpu:
    samples, pmon = [], {}


class Window(unittest.TestCase):
    def test_deltas_count_new_and_exited_processes_and_threads(self):
        mib = metrics.MIB
        full = [
            # start: browser (1) with one thread, engine (2) with two.
            (10.0, {1: proc(1.0, 0.5, 100 * mib, 50 * mib, {1: (1_000, 10, 5, 1)}),
                    2: proc(2.0, 0.0, 200 * mib, 80 * mib, {2: (2_000, 0, 10, 0), 3: (500, 0, 1, 0)}, wchar=1000, syscw=3)}, {}),
            # middle: engine thread 3 still there, a new engine (4) appeared.
            (11.0, {1: proc(1.2, 0.5, 100 * mib, 50 * mib, {1: (1_500, 10, 7, 1)}),
                    2: proc(2.5, 0.0, 200 * mib, 80 * mib, {2: (2_600, 0, 12, 0), 3: (900, 0, 4, 0)}, wchar=3000, syscw=5),
                    4: proc(0.3, 0.1, 50 * mib, 20 * mib, {4: (400, 0, 2, 0)}, wchar=10, syscw=1)}, {}),
            # end: thread 3 and process 4 are gone; their last samples still count.
            (12.0, {1: proc(1.4, 0.6, 110 * mib, 55 * mib, {1: (2_000, 20, 9, 2)}),
                    2: proc(3.0, 0.1, 210 * mib, 85 * mib, {2: (3_000, 0, 20, 1)}, wchar=5000, syscw=9)}, {}),
        ]
        sampler = FakeSampler(1, full, {"a": 0, "b": 2}, light=[(10.0, {1: 100 * mib, 2: 200 * mib}), (11.5, {1: 120 * mib, 2: 230 * mib})])
        m = metrics.window(sampler, NoGpu, [], None, "a", "b")
        self.assertAlmostEqual(m["wall_s"], 2.0)
        # browser 0.5; engines 1.1 (pid 2) + 0.4 (pid 4, new, then gone)
        self.assertAlmostEqual(m["cpu_s"], 2.0)
        self.assertAlmostEqual(m["cpu_pct_browser"], 25.0)
        self.assertAlmostEqual(m["cpu_pct_engines"], 75.0)
        self.assertAlmostEqual(m["cpu_sys_s"], 0.3)
        # threads: 1: 4 vol, 2: 10, 3 (gone): 3, 4 (new, gone): 2
        self.assertEqual(m["ctxsw_vol"], 19)
        self.assertEqual(m["ctxsw_invol"], 2)
        self.assertAlmostEqual(m["wakeups_per_s"], 9.5)
        self.assertAlmostEqual(m["wakeups_per_s_browser"], 2.0)
        self.assertAlmostEqual(m["wakeups_per_s_engines"], 7.5)
        self.assertAlmostEqual(m["cpu_sched_ms"], (1_000 + 1_000 + 400 + 400) / 1e6)
        self.assertEqual(m["processes_exited"], 1)
        self.assertAlmostEqual(m["ipc_out_mib"], (4000 + 10) / mib)
        self.assertEqual(m["ipc_out_writes"], 7)
        self.assertAlmostEqual(m["rss_peak_mib"], 350)
        self.assertAlmostEqual(m["pss_growth_mib"], 10)
        self.assertAlmostEqual(m["rss_growth_mib"], 20)

    def test_frames_inside_the_window_only(self):
        full = [(0.0, {1: proc(0, 0, 0, 0, {})}, {}), (10.0, {1: proc(0, 0, 0, 0, {})}, {})]
        sampler = FakeSampler(1, full, {"a": 0, "b": 1})
        parse = parsers.make(["servo-timing"])
        events = [
            (-1.0, parse("sig-servo: frame 100x100: paint 1.0 ms, read 1.0 ms, sent in 1.0 ms")),
            (1.0, parse("sig-servo: frame 100x100: paint 2.0 ms, read 3.0 ms, sent in 4.0 ms")),
            (2.0, parse("sig-servo: frame 100x100: paint 4.0 ms, read 5.0 ms, unchanged, not sent")),
        ]
        m = metrics.window(sampler, NoGpu, events, None, "a", "b")
        self.assertEqual((m["frames_produced"], m["frames_delivered"]), (2, 1))
        self.assertAlmostEqual(m["readback_mib"], 2 * 40_000 / metrics.MIB)
        self.assertAlmostEqual(m["ipc_pixel_mib_per_frame"], 40_000 / metrics.MIB)
        self.assertEqual((m["paint_p50_ms"], m["paint_p95_ms"]), (2.0, 4.0))
        self.assertNotIn("browser_frames", m)

    def test_browser_frames(self):
        full = [(0.0, {1: proc(0, 0, 0, 0, {})}, {}), (4.0, {1: proc(0, 0, 0, 0, {})}, {})]
        sampler = FakeSampler(1, full, {"a": 0, "b": 1})
        parse = parsers.make(["servo-timing", "gpui-frames"])
        events = [(t, parse(f"frame duration: {ms}ms")) for t, ms in [(-0.5, 9), (0.5, 1), (1.0, 3), (3.9, 2), (4.5, 9)]]
        m = metrics.window(sampler, NoGpu, events, None, "a", "b")
        self.assertEqual(m["browser_frames"], 3)
        self.assertAlmostEqual(m["browser_fps"], 0.75)
        self.assertEqual((m["browser_draw_p50_ms"], m["browser_draw_p95_ms"]), (2.0, 3.0))
        # Engine frames are a separate count, absent without engine diagnostics.
        self.assertNotIn("frames_produced", m)

    def test_input_latency(self):
        sent = {"kind": "frame", "delivered": True}
        drew = {"kind": "browser-frame", "draw_ms": 1.0}
        events = [
            (1.020, sent), (1.030, drew),  # scroll at 1.0: frame after 20 ms, on screen after 30
            (1.150, drew),                 # scroll at 1.1: no page frame before the next scroll
            (1.240, sent), (1.260, drew),  # scroll at 1.2: 40 and 60 ms
            (2.050, drew),                 # tab switch at 2.0: 50 ms
        ]
        inputs = [(1.0, "page"), (1.1, "page"), (1.2, "page"), (2.0, "chrome")]
        m = metrics.latency(events, inputs)
        self.assertEqual(m["inputs"], 4)
        self.assertEqual(m["input_to_frame_missed"], 1)
        self.assertAlmostEqual(m["input_to_frame_p50_ms"], 20)
        self.assertAlmostEqual(m["input_to_frame_p95_ms"], 40)
        self.assertEqual(m["input_to_screen_missed"], 1)
        self.assertEqual(sorted(round(x) for x in [m["input_to_screen_p50_ms"], m["input_to_screen_p95_ms"]]), [50, 60])
        # A frame the engine presents itself is on screen when it is delivered.
        m = metrics.latency([(1.02, {**sent, "presented": True})], [(1.0, "page")])
        self.assertAlmostEqual(m["input_to_screen_p50_ms"], 20)
        # Without diagnostics that show the frames, nothing is measured.
        self.assertEqual(metrics.latency([], inputs), {})
        self.assertNotIn("input_to_frame_missed", metrics.latency([(2.05, drew)], [(2.0, "chrome")]))

    def test_counters(self):
        full = [(10.0, {1: proc(0, 0, 0, 0, {})}, {}), (20.0, {1: proc(0, 0, 0, 0, {})}, {}),
                (30.0, {1: proc(0, 0, 0, 0, {})}, {})]
        sampler = FakeSampler(1, full, {"launch": 0, "a": 1, "b": 2})
        snapshot = lambda t, **values: (t, {"kind": "counters", "values": values})  # noqa: E731
        events = [
            snapshot(15.0, pumps=40),                      # between marks: not an end
            snapshot(20.2, pumps=100, chrome_paints=5),    # asked for at mark a (20.0)
            snapshot(30.4, pumps=160, chrome_paints=5, new=2),  # asked for at mark b (30.0)
        ]
        m = metrics.window(sampler, NoGpu, events, None, "a", "b")
        self.assertEqual((m["ctr.pumps"], m["ctr.chrome_paints"], m["ctr.new"]), (60, 0, 2))
        # The rate is over the snapshots' own interval: 60 in 10.2 s.
        self.assertAlmostEqual(m["ctr.pumps_per_s"], 60 / 10.2)
        # From launch, counting starts at zero.
        m = metrics.window(sampler, NoGpu, events, None, "launch", "a")
        self.assertEqual(m["ctr.pumps"], 100)
        self.assertAlmostEqual(m["ctr.pumps_per_s"], 100 / 10.2)
        # Counters with the diagnostics' names stand in for the per-event metrics.
        standard = [snapshot(20.1, page_frames_presented=10, readback_bytes=0, chrome_paints=1),
                    snapshot(30.1, page_frames_presented=610, readback_bytes=10 * metrics.MIB, chrome_paints=1)]
        m = metrics.window(sampler, NoGpu, standard, None, "a", "b")
        self.assertEqual((m["frames_delivered"], m["readback_mib"], m["browser_frames"]), (600, 10, 0))
        self.assertAlmostEqual(m["fps_delivered"], 60)
        self.assertAlmostEqual(m["readback_mib_per_s"], 1)
        self.assertNotIn("frames_produced", m)
        # A snapshot that came too late, or none at all, gives no counters.
        late = [snapshot(20.2, pumps=1), snapshot(31.5, pumps=2)]
        self.assertNotIn("ctr.pumps", metrics.window(sampler, NoGpu, late, None, "a", "b"))
        self.assertNotIn("ctr.pumps", metrics.window(sampler, NoGpu, [], None, "a", "b"))

    def test_counter_groups_and_engine_timings(self):
        full = [(10.0, {1: proc(0, 0, 0, 0, {})}, {}), (20.0, {1: proc(0, 0, 0, 0, {})}, {})]
        sampler = FakeSampler(1, full, {"a": 0, "b": 1})
        events = [
            (10.1, {"kind": "counters", "values": {"pumps": 5}}),
            (10.2, {"kind": "counters", "group": "engine.servo", "values": {"readbacks": 1, "gl_reads": 4}}),
            (20.1, {"kind": "counters", "values": {"pumps": 9}}),
            (20.2, {"kind": "counters", "group": "engine.servo", "values": {"readbacks": 1, "gl_reads": 10}}),
            (12.0, {"kind": "frame", "delivered": True, "presented": True, "readback_bytes": 0, "ipc_pixel_bytes": 0,
                    "paint_ms": 1.0, "present_ms": 3.0}),
            (13.0, {"kind": "frame", "delivered": True, "presented": True, "readback_bytes": 0, "ipc_pixel_bytes": 0,
                    "paint_ms": 2.0, "present_ms": 5.0}),
            (12.1, {"kind": "engine-timing", "swap_ms": 0.5}),
            (13.1, {"kind": "engine-timing", "swap_ms": 0.25}),
            (25.0, {"kind": "engine-timing", "swap_ms": 9.0}),  # after the window
        ]
        m = metrics.window(sampler, NoGpu, events, None, "a", "b")
        self.assertEqual((m["ctr.pumps"], m["ctr.engine.servo.readbacks"], m["ctr.engine.servo.gl_reads"]), (4, 0, 6))
        self.assertAlmostEqual(m["ctr.engine.servo.gl_reads_per_s"], 6 / 10.0)
        self.assertEqual((m["present_p50_ms"], m["present_p95_ms"], m["paint_p50_ms"]), (3.0, 5.0, 1.0))
        self.assertEqual((m["swap_p50_ms"], m["swap_p95_ms"]), (0.25, 0.5))
        # Frames without a present time (the baseline's) give no present metric.
        self.assertNotIn("present_p50_ms", metrics.window(sampler, NoGpu, [e for e in events if "present_ms" not in e[1]], None, "a", "b"))

    def test_exit_report(self):
        report = {"exit": True,
                  "counters": {"page_readbacks": 0, "readback_bytes": 3 * metrics.MIB, "present_errors": 1, "pumps": 7},
                  "peaks": {"max_pump_us": 1500, "max_target_buffers": 3, "max_buffer_bytes": 2 * metrics.MIB},
                  "engines": [{"name": "servo", "readbacks": 0, "gl_reads": 2}, {"name": "wasm", "readbacks": 0, "gl_reads": None}]}
        self.assertEqual(metrics.exit_metrics(report), {
            "exit.page_readbacks": 0, "exit.present_errors": 1, "exit.readback_mib": 3.0, "exit.gl_reads": 2,
            "exit.max_pump_ms": 1.5, "exit.max_target_buffers": 3, "exit.max_buffer_mib": 2.0})
        # Without an audit there are no GL reads to report, not zero of them.
        report["engines"][0]["gl_reads"] = None
        self.assertNotIn("exit.gl_reads", metrics.exit_metrics(report))

    def test_pixel_latency(self):
        self.assertEqual(metrics.pixel_latency([]), {})
        m = metrics.pixel_latency([0.030, None, 0.010, 0.020])
        self.assertEqual(m["input_to_pixels_missed"], 1)
        self.assertAlmostEqual(m["input_to_pixels_p50_ms"], 20)
        self.assertAlmostEqual(m["input_to_pixels_p95_ms"], 30)
        # Only the inputs inside the window count.
        full = [(0.0, {1: proc(0, 0, 0, 0, {})}, {}), (10.0, {1: proc(0, 0, 0, 0, {})}, {})]
        m = metrics.window(FakeSampler(1, full, {"a": 0, "b": 1}), NoGpu, [], None, "a", "b",
                           pixels=[(-1.0, 0.5), (2.0, 0.040), (11.0, None)])
        self.assertEqual((m["input_to_pixels_missed"], m["input_to_pixels_p50_ms"]), (0, 40))

    def test_percentile_is_nearest_rank(self):
        values = list(range(1, 11))
        self.assertEqual(metrics.percentile(values, 0.5), 5)
        self.assertEqual(metrics.percentile(values, 0.95), 10)
        self.assertEqual(metrics.percentile([3.0], 0.5), 3.0)
        self.assertTrue(math.isclose(metrics.percentile([1, 2], 0.5), 1))


def write_set(root, label, values, extra=None, engines="processes", anomalous=(), parsers=(), script="none"):
    """A result set with one workload, "w" (running `script`), whose runs have the given metric
    values; the runs of the rounds in `anomalous` have an anomaly."""
    path = root / label
    (path / "runs").mkdir(parents=True)
    env = {"host": "h", "kernel": "k", "os": "o", "cpu": "c", "cpus": 1, "mem_total_mib": 1024,
           "cpu_governor": ["g"], "cpu_epp": ["e"], "cpu_boost": "1", "gpu": "gpu", "compositor": "sway"}
    record = {"label": label, "host": "h", "started": "now", "renderer": "gles2", "size": "1280x800",
              "build": {"description": label, "sha256": {"sig": "0" * 64}, "info": None,
                        "definition": {"engines": engines, "parsers": list(parsers)}},
              "harness": {"revision": "r", "sha256": "1" * 64},
              "workloads": {"names": ["w"], "sha256": "2" * 64, "definitions": {"w": {"description": "d", "script": script}}},
              "environment": env, "rounds": len(values), "warmup_rounds": 1, "seed": 1,
              "failures": [{"run": "3-w", "status": 1, "stderr": "boom"}]}
    (path / "set.json").write_text(json.dumps(record))
    (path / "runs" / "3-w.failed.json").write_text(json.dumps({"status": 1, "stderr": "boom"}))
    runs = [(0, True, 999.0)] + [(i + 1, False, v) for i, v in enumerate(values)]
    for round_, warmup, v in runs:
        metrics_ = {"steady.cpu_pct": v, "startup.first_frame_s": 1.0 + round_ * 1e-3}
        metrics_.update(extra or {})
        (path / "runs" / f"{round_}-w.json").write_text(json.dumps(
            {"workload": "w", "round": round_, "warmup": warmup, "metrics": metrics_,
             "anomalies": ["sig ended abnormally: killed by SIGABRT"] if round_ in anomalous else []}))
    return path


class Tools(unittest.TestCase):
    def run_tool(self, *args):
        return subprocess.run([sys.executable, str(BENCH / args[0]), *map(str, args[1:])],
                              capture_output=True, text=True, check=True).stdout

    def test_compare_flags_real_changes_and_noise(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            a = write_set(root, "a", [10, 11, 12, 13, 14], {"active.readback_mib_per_s": 50.0})
            b = write_set(root, "b", [5, 5.5, 6, 6.5, 7])
            out = self.run_tool("compare", a, b)
            # cpu: every B run below every A run, p = 2/252; first frame: the same.
            self.assertRegex(out, r"\| w \| steady.cpu_pct \| 12.0 \[2.00\] \| 6.00 \[1.00\] \| -50.0% \| 0.00794 \| [\d.]+ \| better \|")
            self.assertRegex(out, r"\| w \| startup.first_frame_s \|.*\| noise \|")
            self.assertIn("only A measured active.readback_mib_per_s", out)
            # The warmup run's 999 is never used.
            self.assertNotIn("999", out)
            report = self.run_tool("report", a)
            self.assertIn("| steady.cpu_pct | 12.0 [11.0–13.0] | 5 |", report)

    def test_compare_counts_and_can_leave_out_anomalous_runs(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            # Round 5's 100 is a crashed run's outlier.
            a = write_set(root, "a", [10, 11, 12, 13, 100], anomalous={5})
            b = write_set(root, "b", [10, 11, 12, 13, 14])
            out = self.run_tool("compare", a, b)
            self.assertIn("- A w: 1 run(s) with anomalies (included), 1 failed", out)
            self.assertIn("- B w: 0 run(s) with anomalies (included), 1 failed", out)
            self.assertRegex(out, r"\| w \| steady.cpu_pct \| 12.0 \[2.00\] \|")
            out = self.run_tool("compare", "--exclude-anomalous", a, b)
            self.assertIn("(left out)", out)
            self.assertRegex(out, r"\| w \| steady.cpu_pct \| 11.5 \[1.50\] \|")

    def test_compare_leaves_out_the_process_split_across_placements(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            split = {"steady.cpu_pct_engines": 30.0, "steady.cpu_pct_browser": 10.0, "active.ipc_out_mib_per_s": 5.0}
            a = write_set(root, "a", [10, 11, 12, 13, 14], split)
            b = write_set(root, "b", [10, 11, 12, 13, 14], {**split, "steady.cpu_pct_engines": 0.0}, engines="in-process")
            out = self.run_tool("compare", "--all", a, b)
            self.assertIn("engines: processes → in-process", out)
            for row in ["| steady.cpu_pct_engines", "| steady.cpu_pct_browser", "| active.ipc_out"]:
                self.assertNotIn(f"| w {row}", out)
            self.assertIn("| w | steady.cpu_pct |", out)
            same = write_set(root, "c", [10, 11, 12, 13, 14], split)
            self.assertIn("| w | steady.cpu_pct_engines |", self.run_tool("compare", "--all", a, same))


    def test_compare_leaves_out_what_a_build_cannot_measure_honestly(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            keys = {"active.browser_fps": 2.0, "active.input_to_screen_p50_ms": 30.0, "active.input_to_pixels_p50_ms": 33.0}
            a = write_set(root, "a", [10, 11, 12, 13, 14], keys, parsers=["gpui-frames"], script="tab-toggle")
            b = write_set(root, "b", [5, 5.5, 6, 6.5, 7], keys, parsers=["sig-diag"], script="tab-toggle")
            out = self.run_tool("compare", "--all", a, b)
            self.assertNotIn("| w | active.browser_fps |", out)
            self.assertNotIn("| w | active.input_to_screen_p50_ms |", out)
            self.assertIn("| w | active.input_to_pixels_p50_ms |", out)
            self.assertIn("not compared (a: GPUI's log misses the draws it makes while dispatching a key): "
                          "active.browser_fps, active.input_to_screen_p50_ms", out)
            # Where the script presses no keys, GPUI's frames count.
            c = write_set(root, "c", [10, 11, 12, 13, 14], keys, parsers=["gpui-frames"], script="pointer-circle")
            d = write_set(root, "d", [5, 5.5, 6, 6.5, 7], keys, parsers=["sig-diag"], script="pointer-circle")
            self.assertIn("| w | active.browser_fps |", self.run_tool("compare", "--all", c, d))
            # A rule for every script: sig-servo's navigation time against sig's.
            nav = {"startup.first_frame_after_navigate_ms": 9.0}
            e = write_set(root, "e", [10, 11, 12, 13, 14], nav, parsers=["servo-timing"], script="scroll-down")
            f = write_set(root, "f", [10, 11, 12, 13, 14], {"startup.first_frame_after_navigate_ms": 100.0}, parsers=["sig-diag"])
            self.assertNotIn("| w | startup.first_frame_after_navigate_ms |", self.run_tool("compare", "--all", e, f))
            # --metrics picks rows by name.
            out = self.run_tool("compare", "--all", "--metrics", "cpu_pct$", c, d)
            self.assertIn("| w | steady.cpu_pct |", out)
            self.assertNotIn("| w | active.browser_fps |", out)


    def test_pipe_cost_moves_every_frame(self):
        out = self.run_tool("pipe-cost", "--size", "32x16", "--fps", "200", "--seconds", "0.1", "--runs", "2").splitlines()
        runs, summary = [json.loads(line) for line in out[:-1]], json.loads(out[-1])
        self.assertEqual(len(runs), 2)
        for run in runs:
            self.assertEqual((run["frames"], run["received"]), (20, 20))
            self.assertGreater(run["wakeups_per_s_reader"], 0)
            self.assertAlmostEqual(run["mib_per_s"], 20 * 32 * 16 * 4 / 2**20 / run["wall_s"])
        self.assertEqual(summary["median"]["frames"], 20)


class Run(unittest.TestCase):
    def test_interleave_runs_every_build_and_workload_once_a_round(self):
        run = script("run")
        order = run.interleave(["baseline", "rewrite"], ["a", "b", "c"], 4, seed=7)
        self.assertEqual(len(order), 4)
        for runs in order:
            self.assertEqual(sorted(map(tuple, runs)), [(l, w) for l in ("baseline", "rewrite") for w in "abc"])
        # Reproducible from the seed, different between rounds.
        self.assertEqual(order, run.interleave(["baseline", "rewrite"], ["a", "b", "c"], 4, seed=7))
        self.assertGreater(len({tuple(map(tuple, runs)) for runs in order}), 1)


class Seat(unittest.TestCase):
    def test_key_codes_are_evdev(self):
        # linux/input-event-codes.h
        for key, code in [("escape", 1), ("1", 2), ("0", 11), ("tab", 15), ("q", 16), ("t", 20), ("l", 38),
                          ("a", 30), ("z", 44), ("m", 50), ("ctrl", 29), ("space", 57)]:
            self.assertEqual(seat.KEYS[key], code, key)

    def test_press_sends_keys_and_modifiers_in_order(self):
        import socket
        import struct

        ours, theirs = socket.socketpair()
        device = object.__new__(seat.Seat)
        device.sock, device.keyboard = ours, 7
        device.press("ctrl+t")
        ours.close()
        data = b"".join(iter(lambda: theirs.recv(4096), b""))
        theirs.close()
        requests = []
        while data:
            obj, size_opcode = struct.unpack_from("<II", data)
            size, opcode = size_opcode >> 16, size_opcode & 0xFFFF
            args = struct.unpack_from(f"<{(size - 8) // 4}I", data, 8)
            requests.append((obj, opcode, args[1:] if opcode == 1 else args))  # drop key()'s time
            data = data[size:]
        key, modifiers = 1, 2
        self.assertEqual(requests, [
            (7, key, (29, 1)), (7, modifiers, (4, 0, 0, 0)),  # ctrl down: Control is depressed
            (7, key, (20, 1)), (7, key, (20, 0)),              # t
            (7, key, (29, 0)), (7, modifiers, (0, 0, 0, 0)),  # ctrl up
        ])


class Measure(unittest.TestCase):
    def test_crash_lines(self):
        crash = script("measure").CRASH
        # From the first baseline set's 3-many-tabs, whose GPU crash went unnoticed.
        for line in ["thread 'main' (316855) panicked at /home/u/.cargo/registry/src/blade-graphics-0.7.1/src/vulkan/command.rs:441:21:",
                     "GPU has crashed, and no debug information is available.",
                     "Segmentation fault (core dumped)", "fatal runtime error: stack overflow"]:
            self.assertTrue(crash.search(line), line)
        for line in ["sig-servo: frame 1280x696: paint 0.2 ms, read 4363.0 ms, unchanged, not sent",
                     "frame duration: 1.2ms", "sig-diag t=1 counters panics_caught=0"]:
            self.assertFalse(crash.search(line), line)

    def test_exit_description(self):
        describe = script("measure").exit_description
        self.assertEqual(describe(0), "exit status 0")
        self.assertEqual(describe(-6), "killed by SIGABRT")
        self.assertEqual(describe(None), "still running")


class BuildCost(unittest.TestCase):
    def test_cargo_time(self):
        seconds = script("build-cost").cargo_seconds
        self.assertEqual(seconds("    Finished `release` profile [optimized] target(s) in 12m 03s\n"), 723)
        self.assertEqual(seconds("Finished `release` profile [optimized] target(s) in 1h 02m 3.5s"), 3723.5)
        self.assertEqual(seconds("a\n Finished `dev` profile in 4.2s\n Finished `release` profile in 45.67s\n"), 45.67)
        self.assertIsNone(seconds("error: could not compile"))


if __name__ == "__main__":
    unittest.main()
