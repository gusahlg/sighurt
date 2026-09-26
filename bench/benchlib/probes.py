"""What the harness observes while a browser runs: its process tree (/proc), the GPU (nvidia-smi)
and the screen (grim). Each probe keeps timestamped samples; metrics.py turns them into numbers.

All times are time.monotonic() seconds.
"""

import os
import subprocess
import threading
import time

PAGE = os.sysconf("SC_PAGE_SIZE")
TICK = os.sysconf("SC_CLK_TCK")


def _read(path):
    try:
        with open(path, "rb") as f:
            return f.read().decode(errors="replace")
    except OSError:
        return None


def children(pid):
    """Direct children of `pid`, through every thread's children file."""
    out = []
    try:
        tids = os.listdir(f"/proc/{pid}/task")
    except OSError:
        return out
    for tid in tids:
        text = _read(f"/proc/{pid}/task/{tid}/children")
        if text:
            out.extend(int(c) for c in text.split())
    return out


def tree(root):
    pids, todo = [], [root]
    while todo:
        pid = todo.pop()
        if os.path.exists(f"/proc/{pid}"):
            pids.append(pid)
            todo.extend(children(pid))
    return pids


def stat(pid):
    """(comm, ppid, utime s, stime s) from /proc/<pid>/stat, or None if it is gone."""
    text = _read(f"/proc/{pid}/stat")
    if not text:
        return None
    # comm may contain spaces and parentheses; it ends at the last ')'.
    comm = text[text.index("(") + 1 : text.rindex(")")]
    fields = text[text.rindex(")") + 2 :].split()
    return comm, int(fields[1]), int(fields[11]) / TICK, int(fields[12]) / TICK


def stat_state(pid):
    """The process state letter (R, S, Z, ...), or None if it is gone."""
    text = _read(f"/proc/{pid}/stat")
    return text[text.rindex(")") + 2] if text else None


def rss(pid):
    text = _read(f"/proc/{pid}/statm")
    return int(text.split()[1]) * PAGE if text else None


def smaps_rollup(pid):
    """{"rss": bytes, "pss": bytes} from smaps_rollup."""
    text = _read(f"/proc/{pid}/smaps_rollup")
    out = {}
    for line in (text or "").splitlines():
        key, _, rest = line.partition(":")
        if key in ("Rss", "Pss"):
            out[key.lower()] = int(rest.split()[0]) * 1024
    return out


def io(pid):
    text = _read(f"/proc/{pid}/io")
    out = {}
    for line in (text or "").splitlines():
        key, _, value = line.partition(":")
        if key in ("rchar", "wchar", "syscr", "syscw"):
            out[key] = int(value)
    return out


def threads(pid):
    """{tid: (run ns, runqueue wait ns, voluntary switches, involuntary switches)}."""
    out = {}
    try:
        tids = os.listdir(f"/proc/{pid}/task")
    except OSError:
        return out
    for tid in tids:
        sched = _read(f"/proc/{pid}/task/{tid}/schedstat")
        status = _read(f"/proc/{pid}/task/{tid}/status")
        if not sched or not status:
            continue
        run, wait, _ = (int(x) for x in sched.split())
        vol = invol = 0
        for line in status.splitlines():
            if line.startswith("voluntary_ctxt_switches"):
                vol = int(line.split()[1])
            elif line.startswith("nonvoluntary_ctxt_switches"):
                invol = int(line.split()[1])
        out[int(tid)] = (run, wait, vol, invol)
    return out


class ProcSampler:
    """Samples a process tree: RSS of every process `interval` seconds, and everything else
    (CPU, PSS, I/O, per-thread scheduling) every `full_every` samples and at each `mark`.

    `extra` pids (the compositor) are sampled at full samples too, but are not part of the tree.
    """

    def __init__(self, root, extra=(), interval=0.1, full_every=10):
        self.root = root
        self.extra = list(extra)
        self.interval = interval
        self.full_every = full_every
        self.light = []  # (t, {pid: rss})
        self.full = []  # (t, {pid: {...}}, {pid: {...}} for extra)
        self.marks = {}  # name -> index into self.full
        self.comms = {}
        self.parents = {}
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def start(self):
        self._thread.start()

    def stop(self):
        self._stop.set()
        self._thread.join()

    def mark(self, name):
        """Takes a full sample now and names it."""
        with self._lock:
            self.full.append(self._full_sample())
            self.marks[name] = len(self.full) - 1
        return self.full[-1][0]

    def _run(self):
        n = 0
        while not self._stop.is_set():
            with self._lock:
                if n % self.full_every == 0:
                    self.full.append(self._full_sample())
                else:
                    t = time.monotonic()
                    self.light.append((t, {pid: rss(pid) for pid in tree(self.root)}))
            n += 1
            self._stop.wait(self.interval)

    def _one(self, pid):
        st = stat(pid)
        if st is None:
            return None
        comm, ppid, utime, stime = st
        self.comms[pid] = comm
        self.parents[pid] = ppid
        sample = {"utime": utime, "stime": stime, "rss": rss(pid) or 0}
        sample.update(smaps_rollup(pid))
        sample.update(io(pid))
        sample["threads"] = threads(pid)
        return sample

    def _full_sample(self):
        t = time.monotonic()
        procs = {pid: s for pid in tree(self.root) if (s := self._one(pid)) is not None}
        extra = {pid: s for pid in self.extra if (s := self._one(pid)) is not None}
        # A full sample's RSS counts as a light sample too.
        self.light.append((t, {pid: s["rss"] for pid, s in procs.items()}))
        return t, procs, extra


class GpuSampler:
    """nvidia-smi's GPU-wide utilization, memory, clocks and temperature, every `interval_ms`,
    and per-process framebuffer memory and SM use (`pmon`) on request. Does nothing without
    nvidia-smi."""

    QUERY = "utilization.gpu,utilization.memory,memory.used,clocks.gr,temperature.gpu,power.draw,pstate"

    def __init__(self, interval_ms=200):
        self.interval_ms = interval_ms
        self.samples = []  # (t, {field: value})
        self.pmon = {}  # name -> (t, {pid: {"fb_mib": .., "sm": ..}})
        self.available = _which("nvidia-smi")
        self._proc = None
        self._threads = []

    def start(self):
        if not self.available:
            return
        self._proc = subprocess.Popen(
            ["nvidia-smi", f"--query-gpu={self.QUERY}", "--format=csv,noheader,nounits", "-lms", str(self.interval_ms)],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        thread = threading.Thread(target=self._read, daemon=True)
        thread.start()
        self._threads.append(thread)

    def _read(self):
        names = self.QUERY.split(",")
        for line in self._proc.stdout:
            values = {}
            for name, raw in zip(names, line.strip().split(", ")):
                try:
                    values[name] = float(raw)
                except ValueError:
                    values[name] = raw
            self.samples.append((time.monotonic(), values))

    def snapshot(self, name):
        """Starts a per-process sample (about 50 ms) in the background, stored under `name`."""
        if not self.available:
            return

        def run():
            t = time.monotonic()
            try:
                out = subprocess.run(
                    ["nvidia-smi", "pmon", "-c", "1", "-s", "um"], capture_output=True, text=True, timeout=10
                ).stdout
            except (OSError, subprocess.SubprocessError):
                return
            procs = {}
            for line in out.splitlines():
                if line.startswith("#"):
                    continue
                f = line.split()
                if len(f) < 10:
                    continue
                procs[int(f[1])] = {"sm": _num(f[3]), "fb_mib": _num(f[9])}
            self.pmon[name] = (t, procs)

        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        self._threads.append(thread)

    def stop(self):
        if self._proc:
            self._proc.terminate()
            self._proc.wait()
        for thread in self._threads:
            thread.join(timeout=10)


def _num(s):
    try:
        return float(s)
    except ValueError:
        return None


def _which(program):
    return any(os.access(os.path.join(d, program), os.X_OK) for d in os.environ.get("PATH", "").split(":"))


def screenshot(region):
    """(width, height, RGB bytes) of `region` (x, y, w, h in logical pixels) via grim, or None."""
    x, y, w, h = region
    try:
        out = subprocess.run(
            ["grim", "-t", "ppm", "-g", f"{x},{y} {w}x{h}", "-"], capture_output=True, timeout=5
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    # P6\n<w> <h>\n255\n<data>
    parts = out.split(b"\n", 3)
    if len(parts) < 4 or parts[0] != b"P6":
        return None
    width, height = (int(v) for v in parts[1].split())
    return width, height, parts[3]


def uniform(data):
    return len(data) >= 3 and data == data[:3] * (len(data) // 3)


class ScreenWatcher:
    """Finds when the window first shows (its chrome region stops being uniform) and when the
    page first shows content (the page region changes from what it showed when the window
    appeared). Polls grim every `interval` seconds until both are seen or `timeout` passes.

    Screenshots are taken of the compositor's output, so these are times at which the pixels
    had reached the compositor; they are at most one poll interval late."""

    def __init__(self, chrome_region, page_region, interval=0.05, timeout=60):
        self.chrome_region = chrome_region
        self.page_region = page_region
        self.interval = interval
        self.timeout = timeout
        self.mapped = None
        self.content = None
        self.polls = 0

    def watch(self, since, stop_if=lambda: False):
        """Polls until both times are known, `timeout` passes or `stop_if()` is true."""
        empty = None
        while time.monotonic() - since < self.timeout and not stop_if():
            t = time.monotonic()
            if self.mapped is None:
                if self.chrome_region is None:
                    self.mapped = t
                else:
                    shot = screenshot(self.chrome_region)
                    self.polls += 1
                    if shot and not uniform(shot[2]):
                        self.mapped = t
            if self.mapped is not None:
                shot = screenshot(self.page_region)
                self.polls += 1
                if shot:
                    data = shot[2]
                    if empty is None:
                        # What the page area showed when the window appeared: either still empty
                        # (uniform), or already the page.
                        if not uniform(data):
                            self.content = t
                            return
                        empty = data
                    elif data != empty:
                        self.content = t
                        return
            time.sleep(max(0.0, self.interval - (time.monotonic() - t)))
