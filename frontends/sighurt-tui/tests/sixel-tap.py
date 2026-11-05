"""Runs a command on a pty of its own and copies its output to ours, logging the sixels in it.

    python3 -I sixel-tap.py LOG command args...

real-terminals.sh puts it between Rio and the tmux client, so LOG shows what tmux sends the
outer terminal: one line per read, "TIME COUNT BYTES" (time.time(), the sixels that begin in
the read, the read's length). Input and window size go through
unchanged. A sixel whose introducer is split across two reads is not counted.
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


def main():
    log = open(sys.argv[1], "w", buffering=1)
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
                log.write(f"{time.time():.3f} {count} {len(data)}\n")
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
