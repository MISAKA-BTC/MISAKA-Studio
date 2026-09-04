#!/usr/bin/env python3
"""Generate MISAKA Studio's application icons: the MISAKA mark on a macOS-style tile.

The icons are *generated* rather than committed as opaque binaries so that the mark can be changed
by editing the numbers below instead of by opening a design tool, and so a reviewer can see what is
in the PNGs without decoding them. Run from this directory:

    python3 generate.py

It writes 32x32.png, 128x128.png, 128x128@2x.png and icon.png (1024).

**What it draws.** A rounded square in the brand's own gradient — `#e02424` to `#f97316`, the two
stops of the site's `misaka-logo.svg` — with the MISAKA torii mark knocked out of it in white, and
a transparent margin around the tile so it sits in the Dock the way other macOS apps do. The mark's
shape is not re-drawn here: it is read from `ui/src/assets/misaka-logo.png`, the same asset the
UI's sidebar shows, so the icon and the app cannot drift apart.

Until 2026-09-04 this script drew a teal rounded square with a plain M — a placeholder that had
outlived its purpose, and the app wore it in the Dock.

`icon.ico` sits beside them and is NOT written by this script. Windows needs it and only Windows
does: tauri-build looks for an `.ico` in `bundle.icon`, falls back to `icons/icon.ico`, and fails
the build outright when neither exists — so its absence is invisible on macOS and Linux and stops
the Windows installer every time. Regenerate it from icon.png with:

    cargo tauri icon icons/icon.png -o /tmp/icons   # then copy /tmp/icons/icon.ico back here

It is written elsewhere and only the .ico is taken, because the CLI also rewrites the PNGs above
and emits Store, Android and iOS sets this project does not bundle.

No third-party imaging library: a PNG's container is a zlib stream and five filter types, and the
alternative is a build-time dependency on Pillow for an asset that changes once a year.
"""

import os
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
MARK = os.path.join(HERE, "..", "..", "..", "ui", "src", "assets", "misaka-logo.png")

# The tile, in fractions of the canvas. macOS leaves air around an icon's art rather than bleeding
# it to the edge; without the margin this icon looks a size larger than its neighbours in the Dock.
MARGIN = 0.09
CORNER_RADIUS = 0.225  # of the tile's side — the macOS rounded square, near enough at icon sizes
GRADIENT_FROM = (0xE0, 0x24, 0x24)  # misaka-logo.svg's first stop
GRADIENT_TO = (0xF9, 0x73, 0x16)  # …and its last
MARK_COLOUR = (255, 255, 255)
MARK_WIDTH_FRACTION = 0.68  # of the tile's side; the mark is wider than tall, so this is its WIDTH
SUPERSAMPLE = 4  # 4x4 samples per pixel — the tile's corners and the mark's serifs are both curved


def read_png_rgba(path: str) -> tuple[int, int, bytearray]:
    """Decode an 8-bit RGBA, non-interlaced PNG — which is what the mark is."""
    with open(path, "rb") as handle:
        data = handle.read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} is not a PNG")
    width = height = 0
    idat = bytearray()
    offset = 8
    while offset < len(data):
        (length,) = struct.unpack(">I", data[offset : offset + 4])
        kind = data[offset + 4 : offset + 8]
        body = data[offset + 8 : offset + 8 + length]
        if kind == b"IHDR":
            width, height, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if (depth, colour, interlace) != (8, 6, 0):
                raise ValueError(f"{path}: expected an 8-bit RGBA, non-interlaced PNG")
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
        offset += 12 + length

    raw = zlib.decompress(bytes(idat))
    stride = width * 4
    out = bytearray(width * height * 4)
    previous = bytearray(stride)
    pos = 0
    for row in range(height):
        filter_type = raw[pos]
        pos += 1
        line = bytearray(raw[pos : pos + stride])
        pos += stride
        if filter_type:
            for i in range(stride):
                a = line[i - 4] if i >= 4 else 0
                b = previous[i]
                c = previous[i - 4] if i >= 4 else 0
                if filter_type == 1:
                    line[i] = (line[i] + a) & 0xFF
                elif filter_type == 2:
                    line[i] = (line[i] + b) & 0xFF
                elif filter_type == 3:
                    line[i] = (line[i] + ((a + b) >> 1)) & 0xFF
                elif filter_type == 4:
                    p = a + b - c
                    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                    pred = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                    line[i] = (line[i] + pred) & 0xFF
                else:
                    raise ValueError(f"{path}: unknown row filter {filter_type}")
        out[row * stride : (row + 1) * stride] = line
        previous = line
    return width, height, out


def mark_alpha() -> tuple[int, int, list[int]]:
    """The mark as coverage 0…255 — its own alpha channel, which is the shape and nothing else."""
    width, height, rgba = read_png_rgba(MARK)
    return width, height, [rgba[i * 4 + 3] for i in range(width * height)]


MARK_W, MARK_H, MARK_A = mark_alpha()


def box_resize(alpha: list[int], sw: int, sh: int, dw: int, dh: int) -> list[int]:
    """Area-average down to (dw, dh). Nearest-neighbour would shatter the serifs at 32 px."""
    out = [0] * (dw * dh)
    for dy in range(dh):
        y0 = dy * sh // dh
        y1 = max(y0 + 1, (dy + 1) * sh // dh)
        for dx in range(dw):
            x0 = dx * sw // dw
            x1 = max(x0 + 1, (dx + 1) * sw // dw)
            total = 0
            for sy in range(y0, y1):
                base = sy * sw
                total += sum(alpha[base + x0 : base + x1])
            out[dy * dw + dx] = total // ((y1 - y0) * (x1 - x0))
    return out


def tile_coverage(x: float, y: float) -> float:
    """1 inside the rounded square, 0 outside — sampled; the caller supersamples the edge."""
    inner = 1.0 - 2 * MARGIN
    tx, ty = (x - MARGIN) / inner, (y - MARGIN) / inner
    if not (0.0 <= tx <= 1.0 and 0.0 <= ty <= 1.0):
        return 0.0
    radius = CORNER_RADIUS
    cx = min(max(tx, radius), 1.0 - radius)
    cy = min(max(ty, radius), 1.0 - radius)
    return 1.0 if (tx - cx) ** 2 + (ty - cy) ** 2 <= radius * radius else 0.0


def render(size: int) -> bytes:
    """RGBA rows: the gradient tile, the white mark, and a transparent margin around both."""
    inner = 1.0 - 2 * MARGIN
    mark_w = max(1, round(MARK_WIDTH_FRACTION * inner * size))
    mark_h = max(1, round(mark_w * MARK_H / MARK_W))
    mark = box_resize(MARK_A, MARK_W, MARK_H, mark_w, mark_h)
    mark_x0 = (size - mark_w) // 2
    mark_y0 = (size - mark_h) // 2

    step = 1.0 / (size * SUPERSAMPLE)
    samples = SUPERSAMPLE * SUPERSAMPLE
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            covered = 0.0
            for sy in range(SUPERSAMPLE):
                for sx in range(SUPERSAMPLE):
                    covered += tile_coverage(
                        (px * SUPERSAMPLE + sx + 0.5) * step, (py * SUPERSAMPLE + sy + 0.5) * step
                    )
            alpha = covered / samples
            if alpha <= 0:
                row.extend((0, 0, 0, 0))
                continue
            t = min(1.0, max(0.0, (px + 0.5) / size))  # the gradient runs across the whole tile
            base = tuple(round(a + (b - a) * t) for a, b in zip(GRADIENT_FROM, GRADIENT_TO))
            mx, my = px - mark_x0, py - mark_y0
            ink = mark[my * mark_w + mx] / 255 if 0 <= mx < mark_w and 0 <= my < mark_h else 0.0
            colour = tuple(round(b + (m - b) * ink) for b, m in zip(base, MARK_COLOUR))
            row.extend((*colour, round(alpha * 255)))
        rows.append(bytes(row))
    return b"".join(b"\x00" + row for row in rows)


def write_png(path: str, size: int) -> None:
    raw = render(size)

    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)  # 8-bit RGBA
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    with open(path, "wb") as handle:
        handle.write(png)
    print(f"wrote {path} ({size}x{size}, {len(png)} bytes)")


if __name__ == "__main__":
    write_png("32x32.png", 32)
    write_png("128x128.png", 128)
    write_png("128x128@2x.png", 256)
    write_png("icon.png", 1024)
