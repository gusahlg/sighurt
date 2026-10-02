#!/usr/bin/env python3
"""Generates the benchmark's article, long page, images and hand-assembled Wasm guests (busy,
flood, workers), deterministically.

The outputs are checked in, so running the benchmark never needs this; it exists so they can be
reviewed and regenerated. Same script, same bytes: text comes from a fixed seed and the PNGs are
written without timestamps.

    python3 bench/workloads/generate.py
"""

import pathlib
import struct
import zlib

HERE = pathlib.Path(__file__).resolve().parent

WORDS = """the of and to in is that for it as was with be by on not he this are or his from
at which but have an they you were her she there been one all we their has would when if so
no will what can more out up about into than them only other time new some could these two may
first then do any like my now over such our man me even most made after also did many before
must through back years where much your way well down should because each just those people how
too little state good very make world still own see men work long get here between both life
being under never day same another know while last might us great old year off come since
against go came right used take three states himself few house use during without again place
american around however home small found thought went say part once general high upon school
every don does got united left number course war until always away something fact though water
less public put think almost hand enough far took head yet government system better set told
nothing night end why called didn eyes find going look asked later knew point next program city
business give group toward young days let room president side social given present several order
national possible rather second face per among form important often things looked early white
case john become large big need four within felt along children saw best church ever least power
development light seemed family interest want members mind country area others done turned
although open god service certain kind problem began different door thus help means sense whole
matter perhaps itself york times law human line above name example action company hands local
show five history whether gave either act feet across taken past quite anything seen having death
week experience body word half really field am car words already themselves information tell
together college shall money period held keep sure probably free seems real behind cannot miss
political air question making office brought whose special heard major problems ago became
federal moment study available known result street economic boy position reason change south
board individual job areas society west close turn love community true court force full seem
wall music bränsle, räksmörgås, översättning, kärlek, ängel, åker, café, naïve, façade, résumé,
""".split()


class Lcg:
    """A fixed pseudo-random sequence (Numerical Recipes' LCG), identical on every Python."""

    def __init__(self, seed):
        self.state = seed & 0xFFFFFFFF

    def next(self):
        self.state = (self.state * 1664525 + 1013904223) & 0xFFFFFFFF
        return self.state

    def below(self, n):
        return self.next() % n


def sentence(rng):
    words = [WORDS[rng.below(len(WORDS))].rstrip(",") for _ in range(6 + rng.below(18))]
    if rng.below(4) == 0:
        at = 2 + rng.below(len(words) - 3)
        words[at] += "," if rng.below(3) else " —"
    text = " ".join(words)
    end = ".?!"[0 if rng.below(10) else 1 + rng.below(2)]
    return text[0].upper() + text[1:] + end


def paragraph(rng, sentences=None):
    count = sentences or 3 + rng.below(5)
    return " ".join(sentence(rng) for _ in range(count))


def title(rng, words=4):
    return " ".join(WORDS[rng.below(len(WORDS))].rstrip(",").capitalize() for _ in range(words))


STYLE = """
  body { margin: 0; background: #fbfaf7; color: #1d1d1f;
         font: 18px/1.6 Georgia, "DejaVu Serif", serif; }
  header { background: #20364f; color: #fff; padding: 24px 32px; }
  header h1 { margin: 0; font: 700 34px/1.2 "DejaVu Sans", sans-serif; }
  header p { margin: 6px 0 0; color: #c9d6e3; font: 15px/1.4 "DejaVu Sans", sans-serif; }
  main { max-width: 44em; margin: 0 auto; padding: 16px 32px 64px; }
  h2 { font: 700 26px/1.3 "DejaVu Sans", sans-serif; margin: 40px 0 8px; color: #20364f; }
  figure { margin: 24px 0; }
  figure img { width: 100%; height: auto; display: block; border-radius: 6px; }
  figcaption { font: italic 14px/1.4 sans-serif; color: #5b5b60; margin-top: 6px; }
  blockquote { margin: 24px 0; padding: 8px 24px; border-left: 4px solid #d9822b;
               background: #f3eee6; font-style: italic; }
  table { border-collapse: collapse; width: 100%; font: 15px/1.4 sans-serif; margin: 24px 0; }
  th, td { border: 1px solid #d0d0d4; padding: 6px 10px; text-align: left; }
  th { background: #e8edf2; }
  tr:nth-child(even) td { background: #f5f6f8; }
  pre { background: #1e1e24; color: #e6e6ea; padding: 16px; border-radius: 6px;
        font: 14px/1.5 "DejaVu Sans Mono", monospace; overflow: auto; }
  .lede { font-size: 21px; color: #333; }
"""


def table(rng, rows):
    cells = ["<tr><th>Region</th><th>Units</th><th>Change</th><th>Notes</th></tr>"]
    for _ in range(rows):
        cells.append(
            f"<tr><td>{title(rng, 2)}</td><td>{rng.below(90000) + 1000}</td>"
            f"<td>{rng.below(400) / 10 - 20:+.1f}%</td><td>{sentence(rng)}</td></tr>"
        )
    return "<table>\n" + "\n".join(cells) + "\n</table>"


def figure(rng, image):
    return (
        f'<figure><img src="img/{image}" width="960" height="540" alt="">'
        f"<figcaption>{sentence(rng)}</figcaption></figure>"
    )


def page(heading, subtitle, body):
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width">
<title>{heading}</title>
<style>{STYLE}</style>
</head>
<body>
<header><h1>{heading}</h1><p>{subtitle}</p></header>
<main>
{body}
</main>
</body>
</html>
"""


IMAGES = ["valley.png", "rings.png", "chart.png", "mosaic.png"]


def article():
    rng = Lcg(2026)
    parts = [f'<p class="lede">{paragraph(rng, 3)}</p>']
    for section in range(8):
        parts.append(f"<h2>{title(rng)}</h2>")
        for _ in range(3 + rng.below(3)):
            parts.append(f"<p>{paragraph(rng)}</p>")
        if section % 2 == 0:
            parts.append(figure(rng, IMAGES[section // 2]))
        if section == 3:
            parts.append(f"<blockquote>{paragraph(rng, 2)}</blockquote>")
        if section == 5:
            parts.append(table(rng, 8))
        if section == 6:
            items = "".join(f"<li>{sentence(rng)}</li>" for _ in range(6))
            parts.append(f"<ul>{items}</ul>")
    return page(
        "A Long Read With Pictures",
        "Benchmark workload: a text-heavy article with four images. Generated by generate.py.",
        "\n".join(parts),
    )


def long_page():
    rng = Lcg(7)
    parts = []
    for section in range(80):
        parts.append(f"<h2>{section + 1}. {title(rng)}</h2>")
        for _ in range(4):
            parts.append(f"<p>{paragraph(rng)}</p>")
        if section % 6 == 0:
            parts.append(figure(rng, IMAGES[(section // 6) % len(IMAGES)]))
        elif section % 6 == 3:
            parts.append(table(rng, 5))
        elif section % 6 == 4:
            items = "".join(f"<li>{sentence(rng)}</li>" for _ in range(5))
            parts.append(f"<ol>{items}</ol>")
    return page(
        "A Very Long Page",
        "Benchmark workload: 80 sections to scroll through. Generated by generate.py.",
        "\n".join(parts),
    )


def png(path, width, height, pixel):
    """Writes an RGB PNG; `pixel(x, y)` returns (r, g, b)."""
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            rows.extend(pixel(x, y))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    data = zlib.compress(bytes(rows), 9)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", data) + chunk(b"IEND", b""))


def clamp(v):
    return max(0, min(255, int(v)))


def valley(x, y):
    import math

    ridge = 300 + 60 * math.sin(x / 90) + 30 * math.sin(x / 23 + 1)
    near = 400 + 40 * math.sin(x / 140 + 2) + 12 * math.sin(x / 11)
    if (x - 720) ** 2 + (y - 120) ** 2 < 55**2:
        return (255, 236, 170)
    if y > near:
        return (clamp(40 + (y - near) / 3), clamp(90 + (y - near) / 4), 50)
    if y > ridge:
        return (clamp(70 + (y - ridge) / 2), clamp(80 + (y - ridge) / 3), clamp(110 - (y - ridge) / 4))
    return (clamp(90 + y / 3), clamp(150 + y / 4), clamp(230 - y / 10))


def rings(x, y):
    import math

    d = math.hypot(x - 480, y - 270)
    a = math.atan2(y - 270, x - 480)
    band = int(d / 18 + 3 * math.sin(a * 5)) % 6
    shade = [(34, 87, 122), (69, 162, 158), (116, 198, 157), (250, 243, 221), (242, 167, 94), (199, 81, 70)]
    r, g, b = shade[band]
    fade = max(0.35, 1 - d / 700)
    return (clamp(r * fade), clamp(g * fade), clamp(b * fade))


def chart(x, y):
    if y < 30 or y > 510 or x < 40 or x > 920:
        return (250, 250, 252)
    if x in (40, 41) or y in (509, 510) or (y - 30) % 80 == 0:
        return (200, 200, 208)
    column = (x - 60) // 70
    inset = (x - 60) % 70
    height = 60 + (column * 97 + 131) % 420
    if column >= 0 and 8 < inset < 58 and y > 510 - height:
        return (40 + column * 13 % 120, 110, clamp(200 - column * 9))
    return (255, 255, 255)


def mosaic(x, y):
    tx, ty = x // 40, y // 40
    h = (tx * 7919 + ty * 104729 + tx * ty * 31) % 360
    edge = x % 40 < 2 or y % 40 < 2
    if edge:
        return (30, 30, 34)
    # A cheap HSV-ish wheel, so tiles have distinct, stable colors.
    sector, f = divmod(h, 60)
    f = f / 60
    v, s = 225, 0.55
    p, q, t = v * (1 - s), v * (1 - s * f), v * (1 - s * (1 - f))
    return tuple(clamp(c) for c in [(v, t, p), (q, v, p), (p, v, t), (p, q, v), (t, p, v), (v, p, q)][sector])


# ── A busy Wasm guest ──────────────────────────────────────────────────────────────────────
#
# Plan §26's "hostile workload": a guest that keeps the Wasm engine busy on every frame without
# being stopped. Hand-assembled, since the harness has no Wasm toolchain; equivalent to
#
#   (module
#     (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
#     (import "oxide" "api_canvas_rect" (func $rect (param f32 f32 f32 f32 i32 i32 i32 i32)))
#     (func (export "start_app"))
#     (func (export "on_frame") (param i32) (local i32)
#       (call $clear (i32.const 20) (i32.const 24) (i32.const 32) (i32.const 255))
#       (call $rect (f32.const 40) (f32.const 40) (f32.const 400) (f32.const 200)
#                   (i32.const 200) (i32.const 90) (i32.const 40) (i32.const 255))
#       (local.set 1 (i32.const 6000000))
#       (loop (local.set 1 (i32.sub (local.get 1) (i32.const 1))) (br_if 0 (local.get 1)))))
#
# Each frame draws the same rectangle (so the page shows content and its draw list never
# changes) and then counts down from 6 000 000: 6 operators an iteration, about 36M of the 50M
# fuel sig-wasm gives a frame (gpui-baseline:sighurt-engine-wasm/src/runtime.rs), so it is
# never stopped for running out.

BUSY_ITERATIONS = 6_000_000


def uleb(n):
    out = bytearray()
    while True:
        byte, n = n & 0x7F, n >> 7
        out.append(byte | (0x80 if n else 0))
        if not n:
            return bytes(out)


def sleb(n):
    out = bytearray()
    while True:
        byte, n = n & 0x7F, n >> 7
        done = (n == 0 and not byte & 0x40) or (n == -1 and byte & 0x40)
        out.append(byte | (0 if done else 0x80))
        if done:
            return bytes(out)


def vec(items):
    return uleb(len(items)) + b"".join(items)


def name(text):
    return vec([bytes([c]) for c in text.encode()])


def section(id_, body):
    return bytes([id_]) + uleb(len(body)) + body


I32, I64, F32 = 0x7F, 0x7E, 0x7D


def functype(params, results=()):
    return b"\x60" + vec([bytes([p]) for p in params]) + vec([bytes([r]) for r in results])


def i32_const(v):
    return b"\x41" + sleb(v)


def f32_const(v):
    return b"\x43" + struct.pack("<f", v)


def call(f):
    return b"\x10" + uleb(f)


def local_get(i):
    return b"\x20" + uleb(i)


def local_set(i):
    return b"\x21" + uleb(i)


def global_get(i):
    return b"\x23" + uleb(i)


def global_set(i):
    return b"\x24" + uleb(i)


# Opcodes without immediates, and memory accesses with their alignment and offset 0.
I32_ADD, I32_SUB, I32_AND, I32_SHL, I32_SHR_U, I32_LT_U = b"\x6a", b"\x6b", b"\x71", b"\x74", b"\x76", b"\x49"
I64_LT_S, F32_CONVERT_I32_U, DROP, END = b"\x53", b"\xb3", b"\x1a", b"\x0b"
I32_LOAD, I32_STORE = b"\x28\x02\x00", b"\x36\x02\x00"
LOOP, BLOCK = b"\x03\x40", b"\x02\x40"


def br(depth):
    return b"\x0c" + uleb(depth)


def br_if(depth):
    return b"\x0d" + uleb(depth)


def count_down(local, n):
    """(local.set L n) (loop (local.set L (i32.sub (local.get L) 1)) (br_if 0 (local.get L))):
    6 operators an iteration."""
    return (i32_const(n) + local_set(local) + LOOP + local_get(local) + i32_const(1) + I32_SUB + local_set(local)
            + local_get(local) + br_if(0) + END)


def module(types, imports, functions, exports, bodies, memory=False, globals_=(), data=()):
    """A module: `imports` are (name, type index) from "oxide", `functions` the type index of
    each defined function, `exports` (name, function index), `bodies` (locals, code) with
    locals as (count, type) pairs, `memory` one exported page, `globals_` mutable i32s with
    these initial values, `data` (offset, bytes) segments."""
    out = b"\x00asm\x01\x00\x00\x00" + section(1, vec(types))
    out += section(2, vec([name("oxide") + name(n) + b"\x00" + uleb(t) for n, t in imports]))
    out += section(3, vec([uleb(t) for t in functions]))
    if memory:
        out += section(5, vec([b"\x00" + uleb(1)]))
    if globals_:
        out += section(6, vec([bytes([I32, 1]) + i32_const(v) + END for v in globals_]))
    exported = [name(n) + b"\x00" + uleb(f) for n, f in exports]
    if memory:
        exported.append(name("memory") + b"\x02" + uleb(0))
    out += section(7, vec(exported))
    code = [vec([uleb(n) + bytes([t]) for n, t in locals_]) + body for locals_, body in bodies]
    out += section(10, vec([uleb(len(c)) + c for c in code]))
    if data:
        out += section(11, vec([b"\x00" + i32_const(offset) + END + vec([bytes([b]) for b in blob]) for offset, blob in data]))
    return out


def busy_wasm():
    types = [functype([I32] * 4), functype([F32] * 4 + [I32] * 4), functype([]), functype([I32])]
    on_frame = (
        b"".join(i32_const(v) for v in (20, 24, 32, 255)) + call(0)
        + b"".join(f32_const(v) for v in (40, 40, 400, 200))
        + b"".join(i32_const(v) for v in (200, 90, 40, 255)) + call(1)
        + count_down(1, BUSY_ITERATIONS) + END
    )
    return module(types, [("api_canvas_clear", 0), ("api_canvas_rect", 1)], [2, 3],
                  [("start_app", 2), ("on_frame", 3)], [([], END), ([(1, I32)], on_frame)])


# ── An output flood ─────────────────────────────────────────────────────────────────────────
#
# §26's "output limits": a guest that asks the host for more than it should keep, every frame,
# without running out of fuel. Equivalent to
#
#   (module
#     (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
#     (import "oxide" "api_canvas_rect" (func $rect (param f32 f32 f32 f32 i32 i32 i32 i32)))
#     (import "oxide" "api_log" (func $log (param i32 i32)))
#     (memory (export "memory") 1)
#     (data (i32.const 0) "<FLOOD_LINE>")
#     (global $frame (mut i32) (i32.const 0))
#     (func (export "start_app"))
#     (func (export "on_frame") (param i32) (local $i i32)
#       (global.set $frame (i32.add (global.get $frame) (i32.const 1)))
#       (call $clear (i32.const 16) (i32.const 16) (i32.const 20) (i32.const 255))
#       (loop                              ;; FLOOD_RECTS 4x4 rectangles, moving every frame
#         (call $rect (f32.convert_i32_u (i32.add (i32.and (local.get $i) (i32.const 1023))
#                                                 (i32.and (global.get $frame) (i32.const 63))))
#                     (f32.convert_i32_u (i32.shr_u (local.get $i) (i32.const 10)))
#                     (f32.const 4) (f32.const 4)
#                     (i32.const 200) (i32.and (local.get $i) (i32.const 255)) (i32.const 60) (i32.const 255))
#         (local.set $i (i32.add (local.get $i) (i32.const 1)))
#         (br_if 0 (i32.lt_u (local.get $i) (i32.const FLOOD_RECTS))))
#       (local.set $i (i32.const 0))
#       (loop                              ;; FLOOD_LOGS console lines
#         (call $log (i32.const 0) (i32.const <length of FLOOD_LINE>))
#         (local.set $i (i32.add (local.get $i) (i32.const 1)))
#         (br_if 0 (i32.lt_u (local.get $i) (i32.const FLOOD_LOGS))))))
#
# 250 000 draw commands a frame is more than the rewrite's Wasm engine keeps (200 000,
# sighurt-engine-wasm MAX_COMMANDS), and 200 console lines a frame fill its 1000-line console in
# five frames; the old sig-wasm has neither limit. The rectangles move one pixel a frame, so the
# draw list changes every frame (the old sig-wasm sends only changed lists). About 30 operators
# a rectangle: some 8M of the 50M fuel a frame.

FLOOD_RECTS = 250_000
FLOOD_LOGS = 200
FLOOD_LINE = b"flood: one of many console lines this guest writes on every frame, 100 bytes long, for the limits."


def flood_wasm():
    types = [functype([I32] * 4), functype([F32] * 4 + [I32] * 4), functype([I32] * 2), functype([]), functype([I32])]
    frame, i = 0, 1
    rect = (
        local_get(i) + i32_const(1023) + I32_AND + global_get(frame) + i32_const(63) + I32_AND + I32_ADD + F32_CONVERT_I32_U
        + local_get(i) + i32_const(10) + I32_SHR_U + F32_CONVERT_I32_U + f32_const(4) + f32_const(4)
        + i32_const(200) + local_get(i) + i32_const(255) + I32_AND + i32_const(60) + i32_const(255) + call(1)
    )
    increment = local_get(i) + i32_const(1) + I32_ADD + local_set(i)
    on_frame = (
        global_get(frame) + i32_const(1) + I32_ADD + global_set(frame)
        + b"".join(i32_const(v) for v in (16, 16, 20, 255)) + call(0)
        + LOOP + rect + increment + local_get(i) + i32_const(FLOOD_RECTS) + I32_LT_U + br_if(0) + END
        + i32_const(0) + local_set(i)
        + LOOP + i32_const(0) + i32_const(len(FLOOD_LINE)) + call(2) + increment
        + local_get(i) + i32_const(FLOOD_LOGS) + I32_LT_U + br_if(0) + END
        + END
    )
    return module(types, [("api_canvas_clear", 0), ("api_canvas_rect", 1), ("api_log", 2)], [3, 4],
                  [("start_app", 3), ("on_frame", 4)], [([], END), ([(1, I32)], on_frame)],
                  memory=True, globals_=[0], data=[(0, FLOOD_LINE)])


# ── Background workers ──────────────────────────────────────────────────────────────────────
#
# §26's "worker fairness": a page whose workers keep WORKERS threads busy while it animates.
# The page, workers-main.wasm:
#
#   (module
#     (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
#     (import "oxide" "api_canvas_rect" (func $rect (param f32 f32 f32 f32 i32 i32 i32 i32)))
#     (import "oxide" "api_get_url" (func $get_url (param i32 i32) (result i32)))
#     (import "oxide" "api_spawn_worker" (func $spawn (param i32 i32) (result i32)))
#     (import "oxide" "api_worker_post_message" (func $post (param i32 i32 i32) (result i32)))
#     (import "oxide" "api_worker_recv" (func $recv (param i32 i32 i32) (result i64)))
#     (memory (export "memory") 1)
#     (global $frame (mut i32) (i32.const 0))
#     (func (export "start_app") (local $len i32) (local $w i32) (local $h i32) (local $k i32)
#       ;; Its own URL ".../workers-main.wasm" at 0, made ".../workers-busy.wasm".
#       (local.set $len (call $get_url (i32.const 0) (i32.const 4096)))
#       (i32.store (i32.sub (local.get $len) (i32.const 9)) (i32.const 0x79737562))  ;; "busy"
#       (loop                                ;; WORKERS workers, handles at 8192
#         (local.set $h (call $spawn (i32.const 0) (local.get $len)))
#         (i32.store (i32.add (i32.const 8192) (i32.shl (local.get $w) (i32.const 2))) (local.get $h))
#         (local.set $k (i32.const 0))
#         (loop                              ;; IN_FLIGHT messages each to start with
#           (drop (call $post (local.get $h) (i32.const 0) (i32.const 1)))
#           (local.set $k (i32.add (local.get $k) (i32.const 1)))
#           (br_if 0 (i32.lt_u (local.get $k) (i32.const IN_FLIGHT))))
#         (local.set $w (i32.add (local.get $w) (i32.const 1)))
#         (br_if 0 (i32.lt_u (local.get $w) (i32.const WORKERS)))))
#     (func (export "on_frame") (param i32) (local $w i32) (local $h i32)
#       (global.set $frame (i32.add (global.get $frame) (i32.const 1)))
#       (call $clear (i32.const 20) (i32.const 24) (i32.const 32) (i32.const 255))
#       ;; A square crossing the page, 1 px a frame: an animation.
#       (call $rect (f32.convert_i32_u (i32.and (global.get $frame) (i32.const 511))) (f32.const 100)
#                   (f32.const 80) (f32.const 80) (i32.const 90) (i32.const 160) (i32.const 220) (i32.const 255))
#       (loop                                ;; every reply is answered with a new message
#         (local.set $h (i32.load (i32.add (i32.const 8192) (i32.shl (local.get $w) (i32.const 2)))))
#         (block (loop
#           (br_if 1 (i64.lt_s (call $recv (local.get $h) (i32.const 4096) (i32.const 64)) (i64.const 0)))
#           (drop (call $post (local.get $h) (i32.const 0) (i32.const 1)))
#           (br 0)))
#         (local.set $w (i32.add (local.get $w) (i32.const 1)))
#         (br_if 0 (i32.lt_u (local.get $w) (i32.const WORKERS))))))
#
# and each worker, workers-busy.wasm:
#
#   (module
#     (import "oxide" "api_worker_post" (func $reply (param i32 i32) (result i32)))
#     (memory (export "memory") 1)
#     (func (export "start_app"))
#     (func (export "on_message") (param i32) (local $n i32)
#       <count down from WORKER_ITERATIONS>
#       (drop (call $reply (i32.const 0) (i32.const 1)))))
#
# A message costs a worker WORKER_ITERATIONS * 6 = 42M of its 50M fuel (about 4 ms on louise-pc,
# from busy.wasm's cost); with IN_FLIGHT messages queued each, a worker always has the next one
# while the page answers the last at its next frame, so WORKERS threads stay busy. The queues
# stay at IN_FLIGHT one-byte messages.

WORKERS = 4
IN_FLIGHT = 8
WORKER_ITERATIONS = 7_000_000


def workers_main_wasm():
    types = [functype([I32] * 4), functype([F32] * 4 + [I32] * 4), functype([I32] * 2, [I32]),
             functype([I32] * 3, [I32]), functype([I32] * 3, [I64]), functype([]), functype([I32])]
    imports = [("api_canvas_clear", 0), ("api_canvas_rect", 1), ("api_get_url", 2), ("api_spawn_worker", 2),
               ("api_worker_post_message", 3), ("api_worker_recv", 4)]
    clear, rect, get_url, spawn, post, recv = range(6)
    frame = 0
    handle_at = lambda w: i32_const(8192) + local_get(w) + i32_const(2) + I32_SHL + I32_ADD  # noqa: E731
    length, w, h, k = 0, 1, 2, 3
    start_app = (
        i32_const(0) + i32_const(4096) + call(get_url) + local_set(length)
        + local_get(length) + i32_const(9) + I32_SUB + i32_const(0x79737562) + I32_STORE
        + LOOP
        + i32_const(0) + local_get(length) + call(spawn) + local_set(h)
        + handle_at(w) + local_get(h) + I32_STORE
        + i32_const(0) + local_set(k)
        + LOOP + local_get(h) + i32_const(0) + i32_const(1) + call(post) + DROP
        + local_get(k) + i32_const(1) + I32_ADD + local_set(k) + local_get(k) + i32_const(IN_FLIGHT) + I32_LT_U + br_if(0) + END
        + local_get(w) + i32_const(1) + I32_ADD + local_set(w) + local_get(w) + i32_const(WORKERS) + I32_LT_U + br_if(0) + END
        + END
    )
    w, h = 1, 2  # on_frame's param is local 0
    on_frame = (
        global_get(frame) + i32_const(1) + I32_ADD + global_set(frame)
        + b"".join(i32_const(v) for v in (20, 24, 32, 255)) + call(clear)
        + global_get(frame) + i32_const(511) + I32_AND + F32_CONVERT_I32_U
        + b"".join(f32_const(v) for v in (100, 80, 80)) + b"".join(i32_const(v) for v in (90, 160, 220, 255)) + call(rect)
        + LOOP
        + handle_at(w) + I32_LOAD + local_set(h)
        + BLOCK + LOOP
        + local_get(h) + i32_const(4096) + i32_const(64) + call(recv) + b"\x42" + sleb(0) + I64_LT_S + br_if(1)
        + local_get(h) + i32_const(0) + i32_const(1) + call(post) + DROP + br(0)
        + END + END
        + local_get(w) + i32_const(1) + I32_ADD + local_set(w) + local_get(w) + i32_const(WORKERS) + I32_LT_U + br_if(0) + END
        + END
    )
    return module(types, imports, [5, 6], [("start_app", 6), ("on_frame", 7)],
                  [([(4, I32)], start_app), ([(2, I32)], on_frame)], memory=True, globals_=[0])


def workers_busy_wasm():
    types = [functype([I32] * 2, [I32]), functype([]), functype([I32])]
    on_message = count_down(1, WORKER_ITERATIONS) + i32_const(0) + i32_const(1) + call(0) + DROP + END
    return module(types, [("api_worker_post", 0)], [1, 2], [("start_app", 1), ("on_message", 2)],
                  [([], END), ([(1, I32)], on_message)], memory=True)


def main():
    (HERE / "img").mkdir(exist_ok=True)
    for name_, pixel in zip(IMAGES, [valley, rings, chart, mosaic]):
        png(HERE / "img" / name_, 960, 540, pixel)
    (HERE / "article.html").write_text(article())
    (HERE / "long.html").write_text(long_page())
    (HERE / "wasm").mkdir(exist_ok=True)
    (HERE / "wasm" / "busy.wasm").write_bytes(busy_wasm())
    (HERE / "wasm" / "flood.wasm").write_bytes(flood_wasm())
    (HERE / "wasm" / "workers-main.wasm").write_bytes(workers_main_wasm())
    (HERE / "wasm" / "workers-busy.wasm").write_bytes(workers_busy_wasm())


if __name__ == "__main__":
    main()
