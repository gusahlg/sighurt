#!/usr/bin/env python3
"""Generates the benchmark's article, long page, images and busy Wasm guest, deterministically.

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


def busy_wasm():
    i32, f32, func = 0x7F, 0x7D, 0x60
    functype = lambda params: bytes([func]) + vec([bytes([p]) for p in params]) + vec([])  # noqa: E731
    i32_const = lambda v: b"\x41" + sleb(v)  # noqa: E731
    f32_const = lambda v: b"\x43" + struct.pack("<f", v)  # noqa: E731
    call = lambda f: b"\x10" + uleb(f)  # noqa: E731
    types = [functype([i32] * 4), functype([f32] * 4 + [i32] * 4), functype([]), functype([i32])]
    imports = [name("oxide") + name("api_canvas_clear") + b"\x00" + uleb(0),
               name("oxide") + name("api_canvas_rect") + b"\x00" + uleb(1)]
    exports = [name("start_app") + b"\x00" + uleb(2), name("on_frame") + b"\x00" + uleb(3)]
    start_app = vec([]) + b"\x0b"
    on_frame = (
        vec([uleb(1) + bytes([i32])])
        + b"".join(i32_const(v) for v in (20, 24, 32, 255)) + call(0)
        + b"".join(f32_const(v) for v in (40, 40, 400, 200))
        + b"".join(i32_const(v) for v in (200, 90, 40, 255)) + call(1)
        + i32_const(BUSY_ITERATIONS) + b"\x21\x01"  # local.set 1
        + b"\x03\x40"  # loop
        + b"\x20\x01" + i32_const(1) + b"\x6b" + b"\x21\x01"  # local.get 1, i32.const 1, i32.sub, local.set 1
        + b"\x20\x01" + b"\x0d\x00"  # local.get 1, br_if 0
        + b"\x0b\x0b"  # end loop, end function
    )
    code = [uleb(len(body)) + body for body in (start_app, on_frame)]
    return (b"\x00asm\x01\x00\x00\x00" + section(1, vec(types)) + section(2, vec(imports))
            + section(3, vec([uleb(2), uleb(3)])) + section(7, vec(exports)) + section(10, vec(code)))


def main():
    (HERE / "img").mkdir(exist_ok=True)
    for name_, pixel in zip(IMAGES, [valley, rings, chart, mosaic]):
        png(HERE / "img" / name_, 960, 540, pixel)
    (HERE / "article.html").write_text(article())
    (HERE / "long.html").write_text(long_page())
    (HERE / "wasm").mkdir(exist_ok=True)
    (HERE / "wasm" / "busy.wasm").write_bytes(busy_wasm())


if __name__ == "__main__":
    main()
