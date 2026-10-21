#!/usr/bin/env python3
"""Compare Servo 0.5 and 0.7 on the blank Stack Overflow and Amazon pages.

Runs on louise, from the servo-0.7 worktree, under tools/heavy. Each sig runs in
tools/headless-sway (private compositor) with throwaway XDG dirs. No product code.

  tools/heavy python3 experiments/servo07-blank/probe.py
"""

import json
import os
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SIG07 = ROOT / "target/debug/sig"
SIG05 = Path.home() / "repos/sighurt-rewrite/target/debug/sig"
UA05 = "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0"
UA07 = "Mozilla/5.0 (X11; Linux x86_64; rv:153.0) Gecko/20100101 Firefox/153.0"
CHROME_TOP = 84
STATUS_H = 20
LOAD_TIMEOUT_S = 40

PROBE_HTML = """<!doctype html>
<meta charset=utf-8>
<title>probe</title>
<style>html,body{margin:0;background:#c20;color:#fff;font:28px sans-serif}
div{padding:40px}</style>
<body><div>PROBE-VISIBLE</div>
<script>
document.title = "probe-ran";
console.error("console-error-probe");
setTimeout(function () { throw new ReferenceError("uncaught-probe"); }, 0);
Promise.reject(new ReferenceError("rejection-probe"));
</script>
"""

captured = []
captured_lock = threading.Lock()


class Echo(BaseHTTPRequestHandler):
    def do_GET(self):
        item = {"path": self.path, "headers": dict(self.headers)}
        with captured_lock:
            captured.append(item)
        body = PROBE_HTML.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        return


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


def decode_png(data):
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a png")
    pos, width, height, color, idat = 8, None, None, None, []
    while pos + 8 <= len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        tag = data[pos + 4 : pos + 8]
        chunk = data[pos + 8 : pos + 8 + length]
        pos += 12 + length
        if tag == b"IHDR":
            width, height, depth, color, comp, filt, inter = struct.unpack(">IIBBBBB", chunk)
            if depth != 8 or color not in (2, 6) or comp or filt or inter:
                raise ValueError(f"unsupported png {depth} {color}")
        elif tag == b"IDAT":
            idat.append(chunk)
        elif tag == b"IEND":
            break
    raw = zlib.decompress(b"".join(idat))
    channels = 3 if color == 2 else 4
    stride = width * channels
    rows, prev, i = [], bytearray(stride), 0
    for _ in range(height):
        filt = raw[i]
        i += 1
        row = bytearray(raw[i : i + stride])
        i += stride
        if filt == 1:
            for x in range(stride):
                left = row[x - channels] if x >= channels else 0
                row[x] = (row[x] + left) & 255
        elif filt == 2:
            for x in range(stride):
                row[x] = (row[x] + prev[x]) & 255
        elif filt == 3:
            for x in range(stride):
                left = row[x - channels] if x >= channels else 0
                row[x] = (row[x] + ((left + prev[x]) // 2)) & 255
        elif filt == 4:
            for x in range(stride):
                a = row[x - channels] if x >= channels else 0
                b, c = prev[x], prev[x - channels] if x >= channels else 0
                row[x] = (row[x] + paeth(a, b, c)) & 255
        elif filt != 0:
            raise ValueError(f"bad filter {filt}")
        prev = row
        rows.append(row)
    return width, height, channels, rows


def analyze_png(path):
    width, height, channels, rows = decode_png(Path(path).read_bytes())
    top = min(CHROME_TOP, height)
    status = STATUS_H if height - top > STATUS_H + 20 else 0
    y0, y1 = top, height - top - status + top
    counts, n = {}, 0
    for y in range(y0, min(len(rows), y1), 2):
        row = rows[y]
        for x in range(0, width, 2):
            i = x * channels
            pix = (row[i], row[i + 1], row[i + 2])
            counts[pix] = counts.get(pix, 0) + 1
            n += 1
    if not n:
        return None
    dominant, dom_n = max(counts.items(), key=lambda item: item[1])
    return {
        "nonblank": 1 - dom_n / n,
        "distinct": len(counts),
        "dominant": list(dominant),
        "image": [width, height],
    }


def config_text(ua, restore):
    servo = 'storage = "ephemeral"\n'
    if ua:
        servo += f'user-agent = "{ua}"\n'
    return (
        "version = 1\n\n[engines]\ndefault = \"servo\"\n\n"
        "[engines.servo]\n" + servo + "\n[session]\n"
        f"restore = {'true' if restore else 'false'}\n"
    )


def session_title_url(text):
    data = tomllib.loads(text)
    tabs = data.get("tabs") or []
    if not tabs:
        return None, None
    tab = tabs[min(int(data.get("active") or 0), len(tabs) - 1)]
    segments = tab.get("segments") or []
    if not segments:
        return None, None
    segment = segments[min(int(tab.get("current") or 0), len(segments) - 1)]
    entries = segment.get("entries") or []
    index = int(segment.get("current") or 0)
    url = entries[index] if entries and 0 <= index < len(entries) else (entries[-1] if entries else None)
    title = segment.get("title")
    return (str(title) if title is not None else None), url


def interesting(log):
    keep = []
    for line in log.splitlines():
        if "sig-diag " in line:
            continue
        if any(
            s in line
            for s in (
                "request for",
                "response for",
                "script_runtime",
                "Unhandled",
                "probe",
                "ERROR",
                "WARN",
                "panic",
                "ReferenceError",
                "TypeError",
                "console",
            )
        ):
            keep.append(line)
    return keep


def sway_window(pid):
    try:
        out = subprocess.run(
            ["swaymsg", "-t", "get_tree"], capture_output=True, text=True, timeout=5
        )
        tree = json.loads(out.stdout)
    except (OSError, subprocess.TimeoutExpired, json.JSONDecodeError):
        return None

    found = None

    def walk(node):
        nonlocal found
        if not isinstance(node, dict):
            return
        rect = node.get("rect") or {}
        if rect.get("width", 0) >= 100 and rect.get("height", 0) >= 100:
            if node.get("app_id") == "sighurt" or node.get("pid") == pid:
                found = rect
        for key in ("nodes", "floating_nodes"):
            for child in node.get(key) or []:
                walk(child)

    walk(tree)
    return found


def read_diag(path):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, json.JSONDecodeError):
        return None


def tab_loading(diag):
    if not diag:
        return None
    tabs = diag.get("tabs") or []
    if not tabs:
        return None
    return bool(tabs[0].get("loading"))


def run_inside(args):
    runtime = os.environ.get("XDG_RUNTIME_DIR", "")
    if not (os.environ.get("WAYLAND_DISPLAY") and runtime.startswith("/tmp/sighl.")):
        sys.exit("probe: --inside must run under tools/headless-sway")
    if os.environ.get("SIG_SWAY_RENDERER") != "gles2":
        sys.exit(f"probe: renderer {os.environ.get('SIG_SWAY_RENDERER')!r}, want gles2")
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    base = Path(tempfile.mkdtemp(prefix="s7b."))
    config = base / "config" / "sighurt" / "config.toml"
    config.parent.mkdir(parents=True, exist_ok=True)
    config.write_text(config_text(args.user_agent, restore=False))
    diag_path = base / "diag.json"
    log_path = out / "sig.log"
    shot = out / "shot.png"
    env = os.environ.copy()
    env.update(
        {
            "XDG_CONFIG_HOME": str(base / "config"),
            "XDG_STATE_HOME": str(base / "state"),
            "XDG_DATA_HOME": str(base / "data"),
            "XDG_CACHE_HOME": str(base / "cache"),
            "SIG_DIAG": "1",
            "RUST_LOG": args.rust_log,
        }
    )
    env.pop("DISPLAY", None)
    record = {"url": args.url, "user_agent_set": args.user_agent or None}
    log_file = open(log_path, "w")
    proc = subprocess.Popen(
        [args.sig, "--diagnostics", str(diag_path), args.url],
        stdout=log_file,
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )
    log_file.close()
    started = time.monotonic()
    seen = False
    loaded_at = None
    try:
        while time.monotonic() - started < 2:
            if proc.poll() is not None:
                break
            time.sleep(0.1)
        while proc.poll() is None:
            now = time.monotonic()
            try:
                os.kill(proc.pid, signal.SIGUSR1)
            except OSError:
                break
            time.sleep(0.3)
            loading = tab_loading(read_diag(diag_path))
            if loading:
                seen = True
                loaded_at = None
            elif loading is False and (seen or now - started >= 2):
                if loaded_at is None:
                    loaded_at = now
            if loaded_at is not None and now - loaded_at >= args.settle:
                break
            if loaded_at is None and now - started >= LOAD_TIMEOUT_S:
                break
            time.sleep(0.4)
        record["loaded"] = loaded_at is not None
        record["load_s"] = round(loaded_at - started, 3) if loaded_at else None
        record["elapsed_s"] = round(time.monotonic() - started, 3)
        if proc.poll() is None:
            rect = sway_window(proc.pid)
            if rect:
                geom = f"{int(rect['x'])},{int(rect['y'])} {int(rect['width'])}x{int(rect['height'])}"
                shot_run = subprocess.run(
                    ["grim", "-g", geom, str(shot)], capture_output=True, timeout=15
                )
                record["grim_rc"] = shot_run.returncode
                if shot.is_file() and shot.stat().st_size:
                    try:
                        record["png"] = analyze_png(shot)
                    except (OSError, ValueError) as error:
                        record["png_error"] = str(error)
            else:
                record["grim_error"] = "no window"
            before = ((read_diag(diag_path) or {}).get("counters") or {}).get("config_reloads", 0)
            config.write_text(config_text(args.user_agent, restore=True))
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline and proc.poll() is None:
                try:
                    os.kill(proc.pid, signal.SIGUSR1)
                except OSError:
                    break
                time.sleep(0.3)
                diag = read_diag(diag_path)
                if diag and (diag.get("counters") or {}).get("config_reloads", 0) > before:
                    break
            time.sleep(1.6)
        session = base / "state" / "sighurt" / "session.toml"
        if session.is_file():
            try:
                record["title"], record["final_url"] = session_title_url(session.read_text())
            except (OSError, tomllib.TOMLDecodeError, ValueError) as error:
                record["session_error"] = str(error)
            (out / "session.toml").write_text(session.read_text())
    finally:
        if proc.poll() is None:
            try:
                os.kill(proc.pid, signal.SIGTERM)
                proc.wait(timeout=10)
            except (OSError, subprocess.TimeoutExpired):
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=5)
        subprocess.run(["rm", "-rf", str(base)], check=False)
    log = log_path.read_text(errors="replace") if log_path.is_file() else ""
    record["exit"] = proc.returncode
    record["log_lines"] = interesting(log)
    (out / "summary.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({k: record[k] for k in record if k != "log_lines"}), flush=True)
    for line in record["log_lines"][:80]:
        print("LOG", line[:300], flush=True)


def launch(name, sig, url, ua, settle, rust_log, out_root):
    dest = out_root / name
    dest.mkdir(parents=True, exist_ok=True)
    cmd = [
        str(ROOT / "tools/headless-sway"),
        "--renderer", "gles2",
        "--size", "1600x1000",
        "--",
        str(ROOT / "sighurt-engine-wpe/tests/private-bus"),
        sys.executable,
        str(Path(__file__).resolve()),
        "--inside",
        "--sig", str(sig),
        "--url", url,
        "--out", str(dest),
        "--settle", str(settle),
        "--rust-log", rust_log,
    ]
    if ua:
        cmd += ["--user-agent", ua]
    print(f"=== {name} ===", flush=True)
    try:
        proc = subprocess.run(cmd, timeout=150)
        print(f"=== {name} rc={proc.returncode} ===", flush=True)
    except subprocess.TimeoutExpired:
        print(f"=== {name} TIMEOUT ===", flush=True)


def orchestrate():
    if not SIG07.is_file() or not SIG05.is_file():
        sys.exit(f"probe: missing sig ({SIG07} or {SIG05})")
    out_root = Path(sys.argv[2]) if len(sys.argv) > 2 else ROOT / "experiments/servo07-blank/out"
    out_root.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Echo)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    probe_url = f"http://127.0.0.1:{port}/"
    net_log = "error,net::http_loader=info,script::script_runtime=info,script::runtime::script_runtime=info"
    header_log = "error,net::http_loader=debug,script::script_runtime=info,script::runtime::script_runtime=info"
    cases = [
        ("probe-05", SIG05, probe_url, None, 4, net_log),
        ("probe-07", SIG07, probe_url, None, 4, net_log),
        ("so-07", SIG07, "https://stackoverflow.com/questions/tagged/rust", None, 12, header_log),
        ("so-07-ua140", SIG07, "https://stackoverflow.com/questions/tagged/rust", UA05, 12, net_log),
        ("so-05", SIG05, "https://stackoverflow.com/questions/tagged/rust", None, 12, net_log),
        ("so-05-ua153", SIG05, "https://stackoverflow.com/questions/tagged/rust", UA07, 12, net_log),
        ("amz-07", SIG07, "https://www.amazon.com/", None, 12, header_log),
        ("amz-07-ua140", SIG07, "https://www.amazon.com/", UA05, 12, net_log),
        ("amz-05", SIG05, "https://www.amazon.com/", None, 12, net_log),
        ("amz-05-ua153", SIG05, "https://www.amazon.com/", UA07, 12, net_log),
    ]
    for case in cases:
        launch(*case, out_root)
    server.shutdown()
    (out_root / "echo.json").write_text(json.dumps(captured, indent=2) + "\n")
    print("echo captures", len(captured), flush=True)
    for item in captured:
        ua = item["headers"].get("User-Agent") or item["headers"].get("user-agent")
        print("UA", ua, flush=True)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--inside":
        import argparse

        parser = argparse.ArgumentParser()
        parser.add_argument("--inside", action="store_true")
        parser.add_argument("--sig", required=True)
        parser.add_argument("--url", required=True)
        parser.add_argument("--out", required=True)
        parser.add_argument("--settle", type=float, required=True)
        parser.add_argument("--rust-log", required=True)
        parser.add_argument("--user-agent", default="")
        run_inside(parser.parse_args())
        return
    orchestrate()


if __name__ == "__main__":
    main()
