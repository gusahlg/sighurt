#!/usr/bin/env python3
"""CPU% and wait channel of one process's threads over a short interval.

Prints every thread at or above 2% and any thread whose name mentions a Servo
role, so an idle script thread is visible too. utime+stime are clock ticks.
"""

import os
import sys
import time

ROLES = ("script", "const", "wr", "render", "paint", "compositor", "gl", "ipc")


def snap(pid):
    out = {}
    base = f"/proc/{pid}/task"
    try:
        tids = os.listdir(base)
    except OSError:
        return out
    for tid in tids:
        try:
            comm = open(f"{base}/{tid}/comm").read().strip()
            stat = open(f"{base}/{tid}/stat").read()
            rest = stat.rsplit(")", 1)[1].split()
            state = rest[0]
            ticks = int(rest[11]) + int(rest[12])
            wchan = open(f"{base}/{tid}/wchan").read().strip()
        except (OSError, IndexError, ValueError):
            continue
        out[tid] = (comm, state, ticks, wchan)
    return out


def main():
    pid, seconds, label = sys.argv[1], float(sys.argv[2]), sys.argv[3]
    hz = os.sysconf("SC_CLK_TCK")
    before = snap(pid)
    time.sleep(seconds)
    after = snap(pid)
    rows = []
    for tid, (comm, state, ticks, wchan) in after.items():
        prev = before.get(tid)
        pct = (ticks - prev[2]) / hz / seconds * 100 if prev else 0.0
        rows.append((pct, tid, comm, state, wchan))
    rows.sort(reverse=True)
    print(f"threads {label} pid={pid} n={len(rows)} seconds={seconds}")
    for pct, tid, comm, state, wchan in rows:
        interesting = pct >= 2 or any(role in comm.lower() for role in ROLES)
        if interesting:
            print(f"  {pct:6.1f}% {state} {comm:16} {wchan}")


if __name__ == "__main__":
    main()
