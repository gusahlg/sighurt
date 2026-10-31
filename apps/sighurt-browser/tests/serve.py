#!/usr/bin/env python3
"""The web server of engines.sh: serves tests/pages on 127.0.0.1, and a few answers of its own.

    serve.py PAGES GUESTS PORT_FILE LOG

Writes the port it listens on to PORT_FILE and one line per request to LOG (the monotonic time
and the path), so the tests can count loads. Besides the files in PAGES:

    /guests/NAME       the file NAME in GUESTS (Wasm apps)
    /attachment/NAME   the same file as an attachment (a download)
    /count.html        a page whose title is "load N", N counting the requests for it
    /ua.html           a page that puts the User-Agent header of its request and what its script
                       sees as navigator.userAgent in its address: #["header","script"]
    /slow.html?s=N     a page that answers after N seconds
    /hold.png          an image held for a minute, or until the client gives up
"""

import http.server
import json
import sys
import threading
import time
import urllib.parse

pages, guests, port_file, log_file = sys.argv[1:5]
lock = threading.Lock()
counts = {}


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=pages, **kwargs)

    def log_message(self, *args):
        pass

    def page(self, body):
        data = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        query = urllib.parse.parse_qs(url.query)
        with lock:
            counts[url.path] = counts.get(url.path, 0) + 1
            count = counts[url.path]
            with open(log_file, "a") as log:
                log.write(f"{time.monotonic():.6f} GET {url.path}\n")
        if url.path == "/count.html":
            self.page(f"<!DOCTYPE html><meta charset=utf-8><title>load {count}</title>"
                      f"<h1>Load {count}</h1><p>This page counts its loads.</p>")
        elif url.path == "/ua.html":
            header = json.dumps(self.headers.get("User-Agent", ""))
            self.page("<!DOCTYPE html><meta charset=utf-8><title>user agent</title><script>"
                      f"history.replaceState(null, '', '#' + JSON.stringify([{header}, "
                      "navigator.userAgent]));</script>")
        elif url.path == "/slow.html":
            time.sleep(float(query.get("s", ["3"])[0]))
            self.page("<!DOCTYPE html><meta charset=utf-8><title>slow</title>"
                      "<h1>A slow answer</h1>")
        elif url.path.startswith("/guests/"):
            name = url.path.removeprefix("/guests/")
            try:
                data = open(f"{guests}/{name}", "rb").read() if "/" not in name else None
            except OSError:
                data = None
            if data is None:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/wasm")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        elif url.path.startswith("/attachment/"):
            # A file every engine must save rather than show (the files of GUESTS).
            name = url.path.removeprefix("/attachment/")
            try:
                data = open(f"{guests}/{name}", "rb").read() if "/" not in name else None
            except OSError:
                data = None
            if data is None:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Disposition", f'attachment; filename="{name}"')
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        elif url.path == "/hold.png":
            # Nothing is sent, so the document stays loading; a client that gives up closes
            # the connection and the write below fails.
            for _ in range(600):
                time.sleep(0.1)
            try:
                self.send_error(404)
            except OSError:
                pass
        else:
            super().do_GET()


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
server.daemon_threads = True
with open(port_file, "w") as f:
    f.write(str(server.server_address[1]))
server.serve_forever()
