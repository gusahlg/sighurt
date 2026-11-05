"""Runs a command on a pty of its own and copies its output to ours, logging the sixels in it.

    python3 -I sixel-tap.py LOG command args...

real-terminals.sh puts it between Rio and the tmux client, so LOG shows what tmux sends the
outer terminal: one line per read, "TIME COUNT BYTES ROWS" (time.time(), the sixels that begin
in the read, the read's length, and the rows, from 1, that the read writes a cell on or erases,
comma-separated, or "-"; "*" for an erase of the whole screen or part of it). Escape strings
(sixel, OSC, APC) and cursor moves are not writes. Input and window size go through unchanged.
A sixel whose introducer is split across two reads is not counted. TAP_RAW=FILE also keeps
the bytes.
"""

import fcntl
import os
import pty
import re
import select
import signal
import sys
import termios
import time
import tty

SIXEL = re.compile(rb"\x1bP[0-9;]*q")


class Rows:
    """Follows the cursor's row through the output and collects the rows written on."""

    def __init__(self):
        self.row, self.saved = 1, 1
        self.state, self.csi = "text", b""

    def feed(self, data):
        touched = set()
        at, n = 0, len(data)
        while at < n:
            if self.state == "string":
                # Inside a DCS, OSC or APC: skip to its end (BEL or ESC \), which may straddle
                # two reads.
                if self.csi == b"\x1b" and at == 0 and data[:1] == b"\\":
                    self.state, self.csi, at = "text", b"", 1
                    continue
                end = min((i for i in (data.find(b"\x07", at), data.find(b"\x1b\\", at)) if i >= 0), default=-1)
                if end < 0:
                    self.csi = b"\x1b" if data.endswith(b"\x1b") else b""
                    return touched
                at = end + (1 if data[end] == 7 else 2)
                self.state = "text"
                continue
            byte = data[at]
            at += 1
            if self.state == "esc2":
                # The charset or line-attribute byte of `ESC (`, `ESC #` and their kin.
                self.state = "text"
            elif self.state == "esc":
                if byte in b"()*+-./#%":
                    self.state = "esc2"
                elif byte in b"P]_":
                    self.state, self.csi = "string", b""
                elif byte == ord("["):
                    self.state, self.csi = "csi", b""
                else:
                    if byte == ord("7"):
                        self.saved = self.row
                    elif byte == ord("8"):
                        self.row = self.saved
                    elif byte == ord("M"):
                        self.row = max(1, self.row - 1)
                    self.state = "text"
            elif self.state == "csi":
                if 0x40 <= byte <= 0x7E:
                    self.csi_final(chr(byte), self.csi.decode("latin-1"), touched)
                    self.state = "text"
                else:
                    self.csi += bytes([byte])
            elif byte == 0x1B:
                self.state = "esc"
            elif byte in (0x0A, 0x0B, 0x0C):
                self.row += 1
            elif byte >= 0x20 and byte != 0x7F:
                touched.add(self.row)
        return touched

    def csi_final(self, final, params, touched):
        try:
            first = int(params.lstrip("?>").split(";")[0] or 0)
        except ValueError:
            first = 0
        step = max(first, 1)
        if final in "Hfd":
            self.row = step
        elif final in "AF":
            self.row = max(1, self.row - step)
        elif final in "BEe":
            self.row += step
        elif final == "s":
            self.saved = self.row
        elif final == "u":
            self.row = self.saved
        elif final in "KX@PLM":
            touched.add(self.row)
        elif final == "J" and not params.startswith("?"):
            touched.add("*")


def main():
    log = open(sys.argv[1], "w", buffering=1)
    raw = open(os.environ["TAP_RAW"], "wb") if os.environ.get("TAP_RAW") else None
    rows = Rows()
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(sys.argv[2], sys.argv[2:])

    def winch(*_):
        size = fcntl.ioctl(0, termios.TIOCGWINSZ, b"\0" * 8)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, size)

    winch()
    signal.signal(signal.SIGWINCH, winch)
    saved = termios.tcgetattr(0)
    tty.setraw(0)
    try:
        while True:
            try:
                ready, _, _ = select.select([0, fd], [], [])
            except InterruptedError:
                continue
            if fd in ready:
                try:
                    data = os.read(fd, 1 << 16)
                except OSError:
                    break
                if not data:
                    break
                count = len(SIXEL.findall(data))
                touched = rows.feed(data)
                if raw:
                    raw.write(data)
                written = ",".join(str(r) for r in sorted(touched, key=str)) or "-"
                log.write(f"{time.time():.3f} {count} {len(data)} {written}\n")
                view = memoryview(data)
                while view:
                    view = view[os.write(1, view):]
            if 0 in ready:
                data = os.read(0, 1 << 16)
                if not data:
                    break
                os.write(fd, data)
    finally:
        termios.tcsetattr(0, termios.TCSAFLUSH, saved)
        os.waitpid(pid, 0)


main()
