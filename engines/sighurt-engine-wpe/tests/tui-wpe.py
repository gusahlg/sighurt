#!/usr/bin/env python3
"""`sig --tui` with WPE WebKit in a pty: the last kitty image, and the web process's environment.

The fake terminal answers the startup probe the way apps/sighurt-browser/tests/tui.rs does: kitty
graphics, 10×20-pixel cells, DA1 with sixel. No Wayland display, and LIBGL_ALWAYS_SOFTWARE and
DRI_PRIME unset in sig's environment. Two runs:

- `wpe`: `sig --tui --engine wpe https://example.com/`. Writes OUT/wpe/page.png from the last
  transmitted frame and fails if it is all white.
- `servo+wpe`: Servo's page (tests/pages/colours.html) in the first tab and https://example.com/
  routed to WPE WebKit in the second, so Servo's offscreen context (which sets and restores
  DRI_PRIME or LIBGL_ALWAYS_SOFTWARE around its creation) exists in the same process.

In both, every WPEWebProcess and WPENetworkProcess descended from sig must have neither variable
in /proc/<pid>/environ, and none of them may outlive sig.
"""

import base64
import fcntl
import json
import os
import select
import struct
import sys
import termios
import time
import zlib

REPLIES = b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;4c"
DA1 = b"\x1b[c"
TIMEOUT = 120
LEAKY = ("LIBGL_ALWAYS_SOFTWARE", "DRI_PRIME")
KINDS = ("WPEWebProcess", "WPENetworkProcess")
PAGES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "pages")


def set_winsize(fd):
    # row, col, xpixel, ypixel. Cells are 10×20, so 80×24 is 800×480.
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 800, 480))


def write_png(path, width, height, rgba):
    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    raw = b"".join(b"\x00" + rgba[y * width * 4 : (y + 1) * width * 4] for y in range(height))
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    with open(path, "wb") as out:
        out.write(png)


def parse_controls(text):
    fields = {}
    for part in text.split(","):
        if "=" in part:
            key, value = part.split("=", 1)
            fields[key] = value
    return fields


def kitty_images(data):
    """Complete a=T images, in order: (id, controls, base64 payload)."""
    images = {}
    done = []
    current = None
    index = 0
    while True:
        start = data.find(b"\x1b_G", index)
        if start < 0:
            break
        end = data.find(b"\x1b\\", start)
        if end < 0:
            break
        body = data[start + 3 : end]
        index = end + 2
        semi = body.find(b";")
        if semi < 0:
            continue
        controls = parse_controls(body[:semi].decode("ascii", "replace"))
        payload = body[semi + 1 :]
        # Continuation chunks omit `i`. They belong to the image that is open.
        image_id = controls.get("i", current)
        if image_id is None:
            continue
        current = image_id
        slot = images.setdefault(image_id, {"controls": {}, "payload": b""})
        slot["controls"].update(controls)
        slot["payload"] += payload
        if controls.get("m", "0") == "0" and slot["controls"].get("a") == "T":
            done.append((image_id, dict(slot["controls"]), bytes(slot["payload"])))
            slot["payload"] = b""
            slot["controls"] = {}
    return done


def decode_image(controls, payload):
    raw = base64.b64decode(payload, validate=False)
    if controls.get("o") == "z":
        raw = zlib.decompress(raw)
    width = int(controls["s"])
    height = int(controls["v"])
    expect = width * height * 4
    if len(raw) < expect:
        raise SystemExit(f"kitty image is {len(raw)} bytes, {width}×{height} needs {expect}")
    return width, height, raw[:expect]


def processes():
    """pid -> (ppid, cmdline) for every process this user can read."""
    out = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat", encoding="utf-8", errors="replace") as stat:
                fields = stat.read().rsplit(")", 1)[1].split()
            with open(f"/proc/{name}/cmdline", "rb") as cmd:
                cmdline = cmd.read().replace(b"\0", b" ").decode("utf-8", "replace")
        except (OSError, IndexError):
            continue
        if fields[0] == "Z":
            continue
        out[int(name)] = (int(fields[1]), cmdline)
    return out


def webkit_descendants(root):
    """WebKit's processes below `root` (bubblewrap's parents included in the walk)."""
    procs = processes()
    family = {root}
    grew = True
    while grew:
        grew = False
        for pid, (ppid, _) in procs.items():
            if ppid in family and pid not in family:
                family.add(pid)
                grew = True
    # The program itself, not bubblewrap's parents, whose command lines name it too.
    return {
        pid: cmdline
        for pid, (_, cmdline) in procs.items()
        if pid in family
        and any(name in os.path.basename(cmdline.split(" ")[0]) for name in KINDS)
    }


def environ_of(pid):
    with open(f"/proc/{pid}/environ", "rb") as env:
        entries = env.read().split(b"\0")
    return {
        key: next(
            (e.split(b"=", 1)[1].decode() for e in entries if e.startswith(key.encode() + b"=")),
            None,
        )
        for key in LEAKY
    }


def check_environ(root):
    found = webkit_descendants(root)
    lines = []
    leaked = []
    for pid, cmdline in sorted(found.items()):
        kind = "web" if "WPEWebProcess" in cmdline else "network"
        try:
            env = environ_of(pid)
        except OSError as error:
            lines.append(f"{kind} pid={pid}: environ unreadable ({error})")
            continue
        lines.append(f"{kind} pid={pid} " + " ".join(f"{k}={v}" for k, v in env.items()))
        if any(value is not None for value in env.values()):
            leaked.append(pid)
    return found, lines, leaked


def run(sig, work, args, config):
    """Runs sig in a pty until its first image has settled, checks WebKit's environment, quits."""
    os.makedirs(work, exist_ok=True)
    config_home = os.path.join(work, "config")
    os.makedirs(os.path.join(config_home, "sighurt"), exist_ok=True)
    with open(os.path.join(config_home, "sighurt", "config.toml"), "w", encoding="utf-8") as cfg:
        cfg.write(config)
    diag = os.path.join(work, "diag.json")
    err_path = os.path.join(work, "stderr")
    master, slave = os.openpty()
    set_winsize(slave)
    pid = os.fork()
    if pid == 0:
        os.close(master)
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        os.dup2(slave, 0)
        os.dup2(slave, 1)
        err = os.open(err_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
        os.dup2(err, 2)
        env = os.environ.copy()
        for key in ("WAYLAND_DISPLAY", "DISPLAY") + LEAKY:
            env.pop(key, None)
        env["TERM"] = "xterm-kitty"
        env["XDG_CONFIG_HOME"] = config_home
        env["XDG_STATE_HOME"] = os.path.join(work, "state")
        env["XDG_DATA_HOME"] = os.path.join(work, "data")
        env["XDG_CACHE_HOME"] = os.path.join(work, "cache")
        os.execvpe(sig, [sig, "--tui", "--diagnostics", diag] + args, env)
    os.close(slave)
    flags = fcntl.fcntl(master, fcntl.F_GETFL)
    fcntl.fcntl(master, fcntl.F_SETFL, flags | os.O_NONBLOCK)
    got = bytearray()
    answered = False
    started = time.monotonic()
    deadline = started + TIMEOUT
    image_at = None
    first_image = None
    # Wait for the first image, then 4 s more: the page loads and WebKit's processes are up.
    while time.monotonic() < deadline:
        if image_at is not None and time.monotonic() - image_at > 4:
            break
        readable, _, _ = select.select([master], [], [], 0.2)
        if not readable:
            continue
        try:
            chunk = os.read(master, 1 << 16)
        except BlockingIOError:
            continue
        except OSError:
            break
        if not chunk:
            break
        got.extend(chunk)
        if not answered and DA1 in got:
            answered = True
            os.write(master, REPLIES)
        if image_at is None and b"\x1b_Ga=T," in got:
            image_at = time.monotonic()
            first_image = image_at - started
    found, env_lines, leaked = check_environ(pid)
    try:
        os.write(master, b"q")
    except OSError:
        pass
    quit_at = time.monotonic()
    status = None
    while time.monotonic() < quit_at + 15:
        readable, _, _ = select.select([master], [], [], 0.2)
        if readable:
            try:
                chunk = os.read(master, 1 << 16)
            except (BlockingIOError, OSError):
                chunk = b""
            if chunk:
                got.extend(chunk)
                continue
        waited, code = os.waitpid(pid, os.WNOHANG)
        if waited:
            status = code
            break
    else:
        os.kill(-pid, 9)
        os.waitpid(pid, 0)
    quit_took = time.monotonic() - quit_at
    os.close(master)
    with open(os.path.join(work, "pty.bin"), "wb") as raw:
        raw.write(got)
    # WebKit's processes are not sig's to reap once it has gone; give them a moment.
    left = set()
    for _ in range(50):
        left = {p for p in found if os.path.exists(f"/proc/{p}") and p in processes()}
        if not left:
            break
        time.sleep(0.1)
    print(
        f"answered={answered} bytes={len(got)} first_image_s="
        f"{'none' if first_image is None else f'{first_image:.2f}'} exit_status={status} "
        f"quit_s={quit_took:.2f} webkit_processes={len(found)} left_after_exit={sorted(left)}"
    )
    for line in env_lines:
        print("  " + line)
    log = os.path.join(work, "state", "sighurt", "tui.log")
    for name, path in (("stderr", err_path), ("tui.log", log)):
        if os.path.exists(path):
            text = open(path, encoding="utf-8", errors="replace").read()
            interesting = [
                line
                for line in text.splitlines()
                if "wpe" in line.lower() or "servo" in line.lower() or "error" in line.lower()
            ]
            print(f"--- {name}: {len(text.splitlines())} lines, the WebKit/Servo/error ones ---")
            print("\n".join(interesting[-40:]))
    if os.path.exists(diag):
        with open(diag, encoding="utf-8") as file:
            text = file.read()
        try:
            data = json.loads(text)
            print("--- diagnostics ---")
            print(json.dumps(data, indent=1)[:3000])
        except ValueError:
            print(f"--- diagnostics (not JSON) ---\n{text[:2000]}")
    if status != 0:
        raise SystemExit(f"sig exited with status {status}")
    if not found:
        raise SystemExit("no WebKit process was seen under sig")
    if leaked:
        raise SystemExit(f"WebKit processes {leaked} inherited {' or '.join(LEAKY)}")
    if left:
        raise SystemExit(f"WebKit processes {sorted(left)} outlived sig")
    return kitty_images(bytes(got))


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: tui-wpe.py SIG OUT")
    sig, out = sys.argv[1], sys.argv[2]

    print("== wpe: sig --tui --engine wpe https://example.com/")
    images = run(
        sig,
        os.path.join(out, "wpe"),
        ["--engine", "wpe", "https://example.com/"],
        "version = 1\n[tui]\nfps = 10\n",
    )
    if not images:
        raise SystemExit("no kitty image")
    # The last image that decodes. One cut short (sig quit while writing it) is reported.
    for index in range(len(images) - 1, -1, -1):
        image_id, controls, payload = images[index]
        try:
            width, height, rgba = decode_image(controls, payload)
            break
        except (zlib.error, ValueError, SystemExit) as error:
            print(f"image {index + 1} of {len(images)} does not decode: {error}")
    else:
        raise SystemExit("no kitty image decodes")
    png = os.path.join(out, "wpe", "page.png")
    write_png(png, width, height, rgba)
    pixels = width * height
    nonwhite = 0
    dark = 0
    for i in range(0, pixels * 4, 4):
        red, green, blue, alpha = rgba[i : i + 4]
        if not (red > 245 and green > 245 and blue > 245):
            nonwhite += 1
        if red < 80 and green < 80 and blue < 80 and alpha > 200:
            dark += 1
    print(
        f"images={len(images)} last {width}x{height} id={image_id} "
        f"nonwhite={nonwhite}/{pixels} ({nonwhite / pixels:.4f}) dark={dark} png={png}"
    )
    if nonwhite == 0:
        raise SystemExit("the kitty image is entirely white")

    print("== servo+wpe: Servo's colours.html, then https://example.com/ routed to wpe")
    colours = "file://" + os.path.join(PAGES, "colours.html")
    images = run(
        sig,
        os.path.join(out, "servo-wpe"),
        [colours, "https://example.com/"],
        'version = 1\n[tui]\nfps = 10\n[routing.wpe]\nhosts = ["example.com"]\n',
    )
    print(f"images={len(images)}")
    print("PASS")


if __name__ == "__main__":
    main()
