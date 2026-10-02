#!/usr/bin/env python3
"""What engines.sh asks of sig's diagnostics, its session file, screenshots and its log.

    probe.py diag PID FILE EXPR     ask sig (SIGUSR1) to rewrite its diagnostics FILE, then
                                    evaluate EXPR over it
    probe.py session FILE EXPR      evaluate EXPR over sig's session file
    probe.py pixel PPM X Y          the colour of pixel (X, Y) of a screenshot, as "r g b"
    probe.py count PPM X0 Y0 X1 Y1 R G B
                                    how many pixels of the rectangle from (X0, Y0) to (X1, Y1),
                                    end excluded, are exactly that colour
    probe.py png PPM PNG            the screenshot as a PNG, to look at
    probe.py after LOG T0 REGEX     milliseconds from T0 (CLOCK_MONOTONIC seconds) to the first
                                    `sig-diag` line of LOG after T0 that matches REGEX
    probe.py switches PID SECONDS   context switches a second, over SECONDS, of every thread of
                                    PID and its descendants (WebKit's processes included): the
                                    total, then the busiest threads as process/thread=rate

EXPR is Python. It gives a value, or a pair (verdict, detail); the probe prints "0 detail" when
the verdict holds and "1 detail" when it doesn't, which engines.sh's `check` takes.

Names in diag expressions: d (the JSON), c (counters), p (peaks), t (targets), views (the view
targets), shown (the shown view's target, or None), tabs (each tab's state), engines (by name),
surface(name) (the target of the chrome's surface of that name, or None).
Names in session expressions: s (the session), tab(i), segs(i) (each segment's engine, number
of entries and current entry), url(i) (the current address of tab i, percent-decoding undone),
entries(i, k) (the addresses of segment k of tab i, the current segment if k is None).
Tabs count from 1, as in the tab strip.
"""

import json
import os
import re
import signal
import struct
import sys
import time
import tomllib
import urllib.parse
import zlib


def verdict(value):
    ok, detail = value if isinstance(value, tuple) and len(value) == 2 else (value, value)
    print(0 if ok else 1, detail)


def diag(pid, path, expr):
    before = os.stat(path).st_mtime_ns if os.path.exists(path) else 0
    os.kill(int(pid), signal.SIGUSR1)
    for _ in range(100):
        if os.path.exists(path) and os.stat(path).st_mtime_ns != before:
            break
        time.sleep(0.02)
    time.sleep(0.05)
    d = json.load(open(path))
    t = d["targets"]
    views = [x for x in t if x["role"] == "view"]
    shown = next((x for x in views if x["shown"]), None)
    names = dict(d=d, c=d["counters"], p=d["peaks"], t=t, views=views, shown=shown,
                 tabs=d["tabs"], engines={e["name"]: e for e in d["engines"]},
                 surface=lambda name: next((x for x in t if x["role"] == name), None))
    verdict(eval(expr, names))


def session(path, expr):
    try:
        s = tomllib.load(open(path, "rb"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        print(1, f"no session: {error}")
        return

    def tab(i):
        return s["tabs"][i - 1]

    def seg(i, k=None):
        return tab(i)["segments"][tab(i)["current"] if k is None else k]

    def entries(i, k=None):
        return [urllib.parse.unquote(u) for u in seg(i, k)["entries"]]

    def url(i):
        segment = seg(i)
        return urllib.parse.unquote(segment["entries"][segment["current"]])

    def segs(i):
        return [(x.get("engine"), len(x["entries"]), x["current"]) for x in tab(i)["segments"]]

    names = dict(s=s, tab=tab, seg=seg, entries=entries, url=url, segs=segs)
    try:
        verdict(eval(expr, names))
    except (IndexError, KeyError) as error:
        print(1, f"not there yet: {error!r}")


def pixel(path, x, y):
    data = open(path, "rb").read()
    m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+\d+\s", data)
    width, pixels = int(m[1]), data[m.end():]
    i = 3 * (int(y) * width + int(x))
    print(*pixels[i:i + 3])


def count(path, x0, y0, x1, y1, r, g, b):
    data = open(path, "rb").read()
    m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+\d+\s", data)
    width, pixels = int(m[1]), data[m.end():]
    colour = bytes((int(r), int(g), int(b)))
    print(sum(pixels[3 * (y * width + x):3 * (y * width + x) + 3] == colour
              for y in range(int(y0), int(y1)) for x in range(int(x0), int(x1))))


def png(ppm, out):
    data = open(ppm, "rb").read()
    m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+\d+\s", data)
    width, height, pixels = int(m[1]), int(m[2]), data[m.end():]
    rows = b"".join(b"\0" + pixels[3 * width * y:3 * width * (y + 1)] for y in range(height))

    def chunk(kind, body):
        return (struct.pack(">I", len(body)) + kind + body
                + struct.pack(">I", zlib.crc32(kind + body)))

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    with open(out, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
                + chunk(b"IDAT", zlib.compress(rows, 1)) + chunk(b"IEND", b""))


def after(log, t0, regex):
    t0, pattern = float(t0), re.compile(regex)
    for line in open(log, errors="replace"):
        m = re.match(r"sig-diag t=([0-9.]+) ", line)
        if m and float(m[1]) > t0 and pattern.search(line):
            print(f"{(float(m[1]) - t0) * 1e3:.1f}")
            return
    print("none")


def switches(pid, seconds):
    def tree():
        children = {}
        for p in filter(str.isdigit, os.listdir("/proc")):
            try:
                ppid = int(open(f"/proc/{p}/stat").read().rsplit(")", 1)[1].split()[1])
            except OSError:
                continue
            children.setdefault(ppid, []).append(int(p))
        todo, found = [int(pid)], []
        while todo:
            found.append(todo.pop())
            todo.extend(children.get(found[-1], []))
        return found

    def sample():
        counts = {}
        for p in tree():
            try:
                comm = open(f"/proc/{p}/comm").read().strip()
                for t in os.listdir(f"/proc/{p}/task"):
                    name = open(f"/proc/{p}/task/{t}/comm").read().strip()
                    status = open(f"/proc/{p}/task/{t}/status").read().splitlines()
                    n = sum(int(line.split()[1]) for line in status if "ctxt_switches" in line)
                    counts[(p, t)] = (f"{comm}/{name}", n)
            except OSError:
                pass
        return counts

    seconds = float(seconds)
    before = sample()
    time.sleep(seconds)
    rates = {}
    for key, (name, n) in sample().items():
        rates[name] = rates.get(name, 0) + (n - before.get(key, (name, 0))[1]) / seconds
    busiest = sorted(rates.items(), key=lambda kv: -kv[1])[:4]
    print(f"{sum(rates.values()):.1f}", " ".join(f"{k}={v:.1f}" for k, v in busiest if v))


commands = {"diag": diag, "session": session, "pixel": pixel, "count": count, "png": png,
            "after": after, "switches": switches}
commands[sys.argv[1]](*sys.argv[2:])
