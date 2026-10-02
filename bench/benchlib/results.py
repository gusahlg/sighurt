"""Reading result sets, and what each metric means for a comparison."""

import json
import pathlib
import re


def load(path):
    """A result set: its set.json as "set" and its runs (warmup included) as "runs". Failed
    runs are in set.json's "failures"."""
    path = pathlib.Path(path)
    runs = [json.loads(p.read_text()) for p in sorted((path / "runs").glob("*.json"))
            if not p.name.endswith(".failed.json")]
    return {"path": path, "set": json.loads((path / "set.json").read_text()), "runs": runs}


def values(result_set, exclude_anomalous=False):
    """{workload: {metric: [values of the measured (non-warmup) runs]}}, without the runs that
    have anomalies if `exclude_anomalous`."""
    out = {}
    for run in result_set["runs"]:
        if run.get("warmup") or (exclude_anomalous and run.get("anomalies")):
            continue
        per = out.setdefault(run["workload"], {})
        for metric, value in run["metrics"].items():
            if isinstance(value, (int, float)):
                per.setdefault(metric, []).append(value)
    return out


def problems(result_set):
    """{workload: (runs with anomalies, failed runs)}, for the measured rounds only."""
    warmup = result_set["set"]["warmup_rounds"]
    out = {}
    for run in result_set["runs"]:
        if not run.get("warmup") and run.get("anomalies"):
            anomalous, failed = out.get(run["workload"], (0, 0))
            out[run["workload"]] = (anomalous + 1, failed)
    for failure in result_set["set"]["failures"]:
        round_, workload = failure["run"].split("-", 1)
        if int(round_) >= warmup:
            anomalous, failed = out.get(workload, (0, 0))
            out[workload] = (anomalous, failed + 1)
    return out


def engine_placement(result_set):
    """Where the build runs its engines, from its definition's `engines`: "processes" (the
    baseline: a process per page) or "in-process". Sets from before the key existed are the
    baseline's."""
    return result_set["set"]["build"]["definition"].get("engines", "processes")


def incomparable(result_set):
    """[(workloads, metric pattern, reason)]: what the set's build declares its diagnostics can't
    measure honestly against another build's (benchlib/parsers.py, `INCOMPARABLE`)."""
    from benchlib import parsers

    s = result_set["set"]
    definitions = s["workloads"]["definitions"]
    out = []
    for scripts, pattern, reason in parsers.incomparable(s["build"]["definition"].get("parsers", [])):
        workloads = {w for w, d in definitions.items() if scripts is None or d.get("script", "none") in scripts}
        out.append((workloads, re.compile(pattern), f"{s['label']}: {reason}"))
    return out


# Metrics that describe the split between the browser process and engine processes. Across
# builds that place engines differently they compare process layouts, not work: in-process,
# `_engines` and `ipc_out_` are 0 by construction and `_browser` holds everything.
PROCESS_SPLIT = re.compile(r"_(browser|engines)$|\.ipc_out_")


# Metrics where a lower value is better. The rest (frame counts and rates, window lengths,
# process counts) depend on the workload: more frames is better for an animation and worse for
# a static page, so the tools report their changes without calling them better or worse.
LOWER_IS_BETTER = re.compile(
    r"\.(cpu_|runq_wait|ctxsw_|wakeups|rss_|pss_|readback|ipc_|gpu_util|gpu_fb|gpu_mem|compositor_cpu"
    r"|paint_|read_|send_|present_|swap_|browser_draw_|input_to_|mapped_s|first_frame|processes_exited"
    r"|page_readbacks|gl_reads|max_)|^shutdown\."
)


def lower_is_better(metric):
    return bool(LOWER_IS_BETTER.search(metric))


# The metrics the report and a default comparison show, in order. Everything else is in the
# run files and shown with --all.
KEY_METRICS = [
    "startup.mapped_s",
    "startup.first_frame_s",
    "startup.first_frame_engine_s",
    "startup.cpu_s",
    "steady.cpu_pct",
    "steady.cpu_pct_browser",
    "steady.cpu_pct_engines",
    "steady.wakeups_per_s",
    "steady.wakeups_per_s_browser",
    "steady.wakeups_per_s_engines",
    "steady.pss_mean_mib",
    "steady.fps_produced",
    "steady.fps_delivered",
    "steady.browser_fps",
    "steady.readback_mib_per_s",
    "steady.ipc_pixel_mib_per_s",
    "steady.ipc_out_mib_per_s",
    "steady.gpu_util_pct",
    "steady.gpu_fb_tree_mib",
    "steady.compositor_cpu_pct",
    "active.cpu_pct",
    "active.cpu_pct_browser",
    "active.cpu_pct_engines",
    "active.wakeups_per_s",
    "active.pss_peak_mib",
    "active.fps_produced",
    "active.fps_delivered",
    "active.browser_fps",
    "active.browser_draw_p50_ms",
    "active.input_to_frame_p50_ms",
    "active.input_to_screen_p50_ms",
    "active.input_to_screen_p95_ms",
    "active.input_to_pixels_p50_ms",
    "active.input_to_pixels_p95_ms",
    "active.input_to_pixels_missed",
    "active.readback_mib_per_s",
    "active.ipc_pixel_mib_per_s",
    "active.ipc_out_mib_per_s",
    "active.paint_p50_ms",
    "active.read_p50_ms",
    "active.send_p50_ms",
    "active.present_p50_ms",
    "active.swap_p50_ms",
    "active.gpu_util_pct",
    "active.compositor_cpu_pct",
    "after.rss_growth_mib",
    "after.pss_growth_mib",
    "shutdown.s",
    "exit.page_readbacks",
    "exit.gl_reads",
    "exit.max_present_ms",
    "exit.max_pump_ms",
    "exit.max_target_buffers",
]


# The columns of the report's overview table, one row per workload.
OVERVIEW = [
    "startup.first_frame_s",
    "steady.cpu_pct",
    "steady.wakeups_per_s",
    "steady.wakeups_per_s_browser",
    "steady.wakeups_per_s_engines",
    "steady.pss_mean_mib",
    "steady.browser_fps",
    "steady.fps_delivered",
    "active.cpu_pct",
    "active.fps_delivered",
    "active.readback_mib_per_s",
    "active.ipc_out_mib_per_s",
    "active.input_to_screen_p50_ms",
    "active.input_to_pixels_p50_ms",
]


def fmt(x):
    """A number with about three significant digits."""
    if x is None:
        return "–"
    ax = abs(x)
    if ax == 0:
        return "0"
    if ax >= 100:
        return f"{x:.0f}"
    if ax >= 10:
        return f"{x:.1f}"
    if ax >= 1:
        return f"{x:.2f}"
    return f"{x:.3g}"
