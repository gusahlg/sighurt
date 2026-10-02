"""Turns one run's samples and diagnostic events into flat metrics.

A run has marks (named instants with a full /proc sample) and windows between them:

  startup  launch .. steady_start   start, first frame, settling
  steady   steady_start .. steady_end   no input; idle for a static page
  active   steady_end .. active_end     the workload's interaction script
  total    launch .. active_end

Metric names are "<window>.<metric>". Units are in the name (_s, _ms, _mib, _pct, _per_s).
"""

import bisect
import math

MIB = 1024 * 1024


def window(sampler, gpu, events, sway_pid, start, end, inputs=(), pixels=()):
    """Metrics for the window between marks `start` and `end`. `inputs` are the times of the
    inputs whose latency the script measures (scrolls, tab switches), `pixels` the (input time,
    seconds until the chrome changed on screen, or None) of chrome inputs."""
    ia, ib = sampler.marks[start], sampler.marks[end]
    ta, procs_a, extra_a = sampler.full[ia]
    tb, _, extra_b = sampler.full[ib]
    wall = tb - ta
    root = sampler.root
    m = {"wall_s": wall}

    # Processes and threads: the change from the start mark to the last sample of each one
    # up to the end mark, so a process or thread that ends inside the window still counts
    # (up to its last sample).
    last_proc, last_thread = {}, {}
    for _, procs, _ in sampler.full[ia : ib + 1]:
        for pid, s in procs.items():
            last_proc[pid] = s
            for tid, v in s["threads"].items():
                last_thread[(pid, tid)] = v
    exited = [pid for pid in last_proc if pid not in sampler.full[ib][1]]

    cpu = {"user": 0.0, "sys": 0.0, "browser": 0.0, "engines": 0.0}
    ipc_out = ipc_writes = 0
    for pid, s in last_proc.items():
        a = procs_a.get(pid, {"utime": 0.0, "stime": 0.0, "wchar": 0, "syscw": 0})
        user, sys = s["utime"] - a["utime"], s["stime"] - a["stime"]
        cpu["user"] += user
        cpu["sys"] += sys
        cpu["browser" if pid == root else "engines"] += user + sys
        if pid != root:
            ipc_out += s.get("wchar", 0) - a.get("wchar", 0)
            ipc_writes += s.get("syscw", 0) - a.get("syscw", 0)
    run_ns = wait_ns = vol = invol = vol_browser = 0
    for (pid, tid), v in last_thread.items():
        a = procs_a.get(pid, {"threads": {}})["threads"].get(tid, (0, 0, 0, 0))
        run_ns += v[0] - a[0]
        wait_ns += v[1] - a[1]
        vol += v[2] - a[2]
        invol += v[3] - a[3]
        if pid == root:
            vol_browser += v[2] - a[2]
    total_cpu = cpu["user"] + cpu["sys"]
    m.update(
        {
            "cpu_s": total_cpu,
            "cpu_user_s": cpu["user"],
            "cpu_sys_s": cpu["sys"],
            "cpu_pct": 100 * total_cpu / wall,
            "cpu_pct_browser": 100 * cpu["browser"] / wall,
            "cpu_pct_engines": 100 * cpu["engines"] / wall,
            "cpu_sched_ms": run_ns / 1e6,
            "runq_wait_ms": wait_ns / 1e6,
            "ctxsw_vol": vol,
            "ctxsw_invol": invol,
            "wakeups_per_s": vol / wall,
            "wakeups_per_s_browser": vol_browser / wall,
            "wakeups_per_s_engines": (vol - vol_browser) / wall,
            "processes": len(last_proc),
            "processes_exited": len(exited),
            "ipc_out_mib": ipc_out / MIB,
            "ipc_out_mib_per_s": ipc_out / MIB / wall,
            "ipc_out_writes": ipc_writes,
        }
    )

    rss = [sum(v or 0 for v in procs.values()) for t, procs in sampler.light if ta <= t <= tb]
    if rss:
        m["rss_mean_mib"] = sum(rss) / len(rss) / MIB
        m["rss_peak_mib"] = max(rss) / MIB
    pss = [sum(s.get("pss", 0) for s in procs.values()) for _, procs, _ in sampler.full[ia : ib + 1]]
    m["pss_mean_mib"] = sum(pss) / len(pss) / MIB
    m["pss_peak_mib"] = max(pss) / MIB
    m["pss_end_mib"] = pss[-1] / MIB
    rss_at = lambda i: sum(s["rss"] for s in sampler.full[i][1].values())  # noqa: E731
    m["rss_growth_mib"] = (rss_at(ib) - rss_at(ia)) / MIB
    m["pss_growth_mib"] = (pss[-1] - pss[0]) / MIB

    if sway_pid and sway_pid in extra_a and sway_pid in extra_b:
        a, b = extra_a[sway_pid], extra_b[sway_pid]
        m["compositor_cpu_pct"] = 100 * (b["utime"] + b["stime"] - a["utime"] - a["stime"]) / wall

    samples = [v for t, v in gpu.samples if ta <= t <= tb]
    for key, name in [("utilization.gpu", "gpu_util_pct"), ("memory.used", "gpu_mem_used_mib"), ("clocks.gr", "gpu_clock_mhz")]:
        values = [s[key] for s in samples if isinstance(s.get(key), float)]
        if values:
            m[name] = sum(values) / len(values)
    if end in gpu.pmon:
        tree_pids = set(sampler.full[ib][1])
        fb = [p["fb_mib"] for pid, p in gpu.pmon[end][1].items() if pid in tree_pids and p["fb_mib"] is not None]
        m["gpu_fb_tree_mib"] = sum(fb)

    frames = [e for t, e in events if e["kind"] == "frame" and ta <= t <= tb]
    if any(e["kind"] == "frame" for _, e in events):
        delivered = [e for e in frames if e["delivered"]]
        m["frames_produced"] = len(frames)
        m["frames_delivered"] = len(delivered)
        m["fps_produced"] = len(frames) / wall
        m["fps_delivered"] = len(delivered) / wall
        m["readback_mib"] = sum(e["readback_bytes"] for e in frames) / MIB
        m["readback_mib_per_s"] = m["readback_mib"] / wall
        m["ipc_pixel_mib"] = sum(e["ipc_pixel_bytes"] for e in frames) / MIB
        m["ipc_pixel_mib_per_s"] = m["ipc_pixel_mib"] / wall
        if delivered:
            m["ipc_pixel_mib_per_frame"] = m["ipc_pixel_mib"] / len(delivered)
        for key in ("paint_ms", "read_ms", "send_ms"):
            values = sorted(e[key] for e in frames if e.get(key) is not None)
            if values:
                m[f"{key.removesuffix('_ms')}_p50_ms"] = percentile(values, 0.5)
                m[f"{key.removesuffix('_ms')}_p95_ms"] = percentile(values, 0.95)

    # The browser's own window frames (baseline: every GPUI draw, chrome and page area together).
    if any(e["kind"] == "browser-frame" for _, e in events):
        drawn = sorted(e["draw_ms"] for t, e in events if e["kind"] == "browser-frame" and ta <= t <= tb)
        m["browser_frames"] = len(drawn)
        m["browser_fps"] = len(drawn) / wall
        if drawn:
            m["browser_draw_p50_ms"] = percentile(drawn, 0.5)
            m["browser_draw_p95_ms"] = percentile(drawn, 0.95)

    m.update(latency(events, [(t, kind) for t, kind in inputs if ta <= t <= tb]))
    m.update(pixel_latency([seconds for t, seconds in pixels if ta <= t <= tb]))
    m.update(counters(events, ta, tb, from_launch=start == "launch"))
    for name, (count, rate, scale) in FROM_COUNTERS.items():
        if count not in m and f"ctr.{name}" in m:
            m[count] = m[f"ctr.{name}"] * scale
            m[rate] = m[f"ctr.{name}_per_s"] * scale
    return m


# Counters that give the metrics the per-event diagnostics give, under the same names, for a
# build that prints counters and not a line per frame: counter -> (metric, its rate, scale).
# Per-event lines win where both exist. The latency metrics need the per-event lines.
FROM_COUNTERS = {
    "page_frames_painted": ("frames_produced", "fps_produced", 1),
    "page_frames_presented": ("frames_delivered", "fps_delivered", 1),
    "readback_bytes": ("readback_mib", "readback_mib_per_s", 1 / MIB),
    "chrome_paints": ("browser_frames", "browser_fps", 1),
}


# How long after a mark the counters snapshot it asked for may arrive (bench/measure sends the
# build's counters_signal at every mark but the first).
COUNTER_DELAY_S = 1.0


def counters(events, ta, tb, from_launch=False):
    """Each counter's increase over the window from `ta` to `tb` ("ctr.<name>") and its rate
    ("ctr.<name>_per_s"), between the first snapshot within COUNTER_DELAY_S after each end. The
    rate divides by the time between the two snapshots, not the marks. A window that starts at
    launch starts from zero, since the process has just started. Without both snapshots, none."""
    snapshots = [(t, e["values"]) for t, e in events if e["kind"] == "counters"]

    def after(mark):
        return next(((t, v) for t, v in snapshots if mark <= t <= mark + COUNTER_DELAY_S), None)

    a = (ta, {}) if from_launch else after(ta)
    b = after(tb)
    if a is None or b is None or b[0] <= a[0]:
        return {}
    out = {}
    for name, value in b[1].items():
        increase = value - a[1].get(name, 0)
        out[f"ctr.{name}"] = increase
        out[f"ctr.{name}_per_s"] = increase / (b[0] - a[0])
    return out


def latency(events, inputs, limit=1.0):
    """Input latency, for the inputs the script marks: (time, "page") for input the page handles
    (a scroll), (time, "chrome") for input the browser handles (a tab switch).

    input_to_frame     page input -> the engine's next delivered frame
    input_to_screen    page input -> that frame reaching the compositor: the frame itself if the
                       engine presents it, else the browser's next window frame after it;
                       chrome input -> the browser's next window frame

    An input whose frame doesn't come before the next input, or within `limit` seconds, is
    counted in *_missed instead: it changed nothing on screen, or its frame merged with the
    next input's."""
    frames = sorted((t, e.get("presented", False)) for t, e in events if e["kind"] == "frame" and e["delivered"])
    frame_times = [t for t, _ in frames]
    drawn = sorted(t for t, e in events if e["kind"] == "browser-frame")
    # Only inputs whose frames the build's diagnostics show can be measured.
    inputs = [(t, k) for t, k in inputs if (frame_times if k == "page" else drawn)]
    if not inputs:
        return {}

    def first_after(times, t):
        j = bisect.bisect_right(times, t)
        return j if j < len(times) else None

    values = {"input_to_frame": [], "input_to_screen": []}
    missed = {"input_to_frame": 0, "input_to_screen": 0}
    for i, (t, kind) in enumerate(inputs):
        later = [u for u, k in inputs[i + 1 :] if k == kind]
        deadline = min(later[0] if later else math.inf, t + limit)
        screen = None
        if kind == "page":
            j = first_after(frame_times, t)
            if j is None or frame_times[j] >= deadline:
                missed["input_to_frame"] += 1
            else:
                values["input_to_frame"].append(frame_times[j] - t)
                if frames[j][1]:
                    screen = frame_times[j]
                elif (d := first_after(drawn, frame_times[j])) is not None:
                    screen = drawn[d]
        elif (d := first_after(drawn, t)) is not None:
            screen = drawn[d]
        if screen is not None and screen < deadline:
            values["input_to_screen"].append(screen - t)
        else:
            missed["input_to_screen"] += 1

    out = {"inputs": len(inputs)}
    for name in values:
        if name == "input_to_frame" and all(k != "page" for _, k in inputs):
            continue
        out[f"{name}_missed"] = missed[name]
        if values[name]:
            ms = sorted(1000 * v for v in values[name])
            out[f"{name}_p50_ms"] = percentile(ms, 0.5)
            out[f"{name}_p95_ms"] = percentile(ms, 0.95)
    return out


def pixel_latency(seconds):
    """input_to_pixels: chrome input -> the chrome's pixels changed on screen (bench/measure
    chrome_keys; None: no change within its limit, counted in _missed)."""
    if not seconds:
        return {}
    ms = sorted(1000 * s for s in seconds if s is not None)
    out = {"input_to_pixels_missed": sum(s is None for s in seconds)}
    if ms:
        out["input_to_pixels_p50_ms"] = percentile(ms, 0.5)
        out["input_to_pixels_p95_ms"] = percentile(ms, 0.95)
    return out


def percentile(sorted_values, q):
    """Nearest-rank percentile of already sorted values."""
    return sorted_values[max(0, math.ceil(q * len(sorted_values)) - 1)]


def run_metrics(sampler, gpu, events, sway_pid, launch, screen, windows, inputs=(), pixels=()):
    """All of a run's metrics. `screen` is the ScreenWatcher, `launch` the launch time,
    `windows` the (name, start mark, end mark) triples to compute, `inputs` the times of the
    script's latency-probing inputs and `pixels` the chrome inputs' screen latencies."""
    out = {}
    for name, start, end in windows:
        if start in sampler.marks and end in sampler.marks:
            for key, value in window(sampler, gpu, events, sway_pid, start, end, inputs, pixels).items():
                out[f"{name}.{key}"] = value
    if screen.mapped is not None:
        out["startup.mapped_s"] = screen.mapped - launch
    if screen.content is not None:
        out["startup.first_frame_s"] = screen.content - launch
    delivered = [t for t, e in events if e["kind"] == "frame" and e["delivered"]]
    if delivered:
        out["startup.first_frame_engine_s"] = delivered[0] - launch
    firsts = [e["after_navigate_ms"] for _, e in events if e["kind"] == "first-frame"]
    if firsts:
        out["startup.first_frame_after_navigate_ms"] = firsts[0]
    return out
