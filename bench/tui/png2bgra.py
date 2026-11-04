#!/usr/bin/env python3
"""Turns a PNG screenshot (8-bit RGB or RGBA, not interlaced, as grim writes them) into the raw
frame the terminal encoders take: premultiplied B, G, R, A, rows packed, top row first, cropped
to a rectangle. Standard library only.

    bench/tui/png2bgra.py SHOT.png X Y W H OUT_DIR NAME

writes OUT_DIR/NAME-WxH.bgra, which frontends/sighurt-tui/tests/encode_cost.rs reads.
"""
import struct
import sys
import zlib


def decode(path):
    data = open(path, 'rb').read()
    assert data[:8] == b'\x89PNG\r\n\x1a\n', 'not a PNG'
    at, idat, header = 8, b'', None
    while at < len(data):
        length, kind = struct.unpack('>I4s', data[at:at + 8])
        body = data[at + 8:at + 8 + length]
        if kind == b'IHDR':
            header = struct.unpack('>IIBBBBB', body)
        elif kind == b'IDAT':
            idat += body
        at += 12 + length
    width, height, depth, color, _, _, interlace = header
    assert depth == 8 and color in (2, 6) and interlace == 0, header
    channels = 4 if color == 6 else 3
    raw = zlib.decompress(idat)
    row = width * channels
    out, previous = bytearray(), bytearray(row)
    for y in range(height):
        kind = raw[y * (row + 1)]
        line = bytearray(raw[y * (row + 1) + 1:(y + 1) * (row + 1)])
        for i in range(row):
            left = line[i - channels] if i >= channels else 0
            up = previous[i]
            corner = previous[i - channels] if i >= channels else 0
            if kind == 1:
                line[i] = (line[i] + left) & 255
            elif kind == 2:
                line[i] = (line[i] + up) & 255
            elif kind == 3:
                line[i] = (line[i] + (left + up) // 2) & 255
            elif kind == 4:
                p = left + up - corner
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - corner)
                line[i] = (line[i] + (left if pa <= pb and pa <= pc else up if pb <= pc else corner)) & 255
        out += line
        previous = line
    return width, height, channels, out


def main():
    path, x, y, w, h, out_dir, name = sys.argv[1:]
    x, y, w, h = int(x), int(y), int(w), int(h)
    width, height, channels, pixels = decode(path)
    assert x + w <= width and y + h <= height, (width, height)
    frame = bytearray(w * h * 4)
    for row in range(h):
        for column in range(w):
            i = ((y + row) * width + x + column) * channels
            r, g, b = pixels[i:i + 3]
            a = pixels[i + 3] if channels == 4 else 255
            o = (row * w + column) * 4
            frame[o:o + 4] = bytes(((b * a + 127) // 255, (g * a + 127) // 255, (r * a + 127) // 255, a))
    open(f'{out_dir}/{name}-{w}x{h}.bgra', 'wb').write(frame)


if __name__ == '__main__':
    main()
