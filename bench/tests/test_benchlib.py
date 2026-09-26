"""Tests of the harness: statistics, parsers, metrics and the compare and report tools.

  python3 -m unittest discover -s bench/tests
"""

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

from benchlib import metrics, parsers, stats  # noqa: E402


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

    def test_percentile_is_nearest_rank(self):
        values = list(range(1, 11))
        self.assertEqual(metrics.percentile(values, 0.5), 5)
        self.assertEqual(metrics.percentile(values, 0.95), 10)
        self.assertEqual(metrics.percentile([3.0], 0.5), 3.0)
        self.assertTrue(math.isclose(metrics.percentile([1, 2], 0.5), 1))


def write_set(root, label, values, extra=None):
    """A result set with one workload, "w", whose runs have the given metric values."""
    path = root / label
    (path / "runs").mkdir(parents=True)
    env = {"host": "h", "kernel": "k", "os": "o", "cpu": "c", "cpus": 1, "mem_total_mib": 1024,
           "cpu_governor": ["g"], "cpu_epp": ["e"], "cpu_boost": "1", "gpu": "gpu", "compositor": "sway"}
    record = {"label": label, "host": "h", "started": "now", "renderer": "gles2", "size": "1280x800",
              "build": {"description": label, "sha256": {"sig": "0" * 64}, "info": None},
              "harness": {"revision": "r", "sha256": "1" * 64},
              "workloads": {"names": ["w"], "sha256": "2" * 64, "definitions": {"w": {"description": "d"}}},
              "environment": env, "rounds": len(values), "warmup_rounds": 1, "seed": 1, "failures": []}
    (path / "set.json").write_text(json.dumps(record))
    (path / "runs" / "3-w.failed.json").write_text(json.dumps({"status": 1, "stderr": "boom"}))
    runs = [(0, True, 999.0)] + [(i + 1, False, v) for i, v in enumerate(values)]
    for round_, warmup, v in runs:
        metrics_ = {"steady.cpu_pct": v, "startup.first_frame_s": 1.0 + round_ * 1e-3}
        metrics_.update(extra or {})
        (path / "runs" / f"{round_}-w.json").write_text(json.dumps(
            {"workload": "w", "round": round_, "warmup": warmup, "metrics": metrics_, "anomalies": []}))
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


if __name__ == "__main__":
    unittest.main()
