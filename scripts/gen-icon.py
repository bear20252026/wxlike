#!/usr/bin/env python3
"""生成桌面端图标占位图（`ui/src-tauri/icons/*.png`）。

为什么需要这个脚本：`tauri::generate_context!()` 在 Linux/macOS 上会去找
`icons/icon.png`，而仓库里只有 Windows 用的 `icon.ico`——于是**只有 Linux
构建会失败**，本地（Windows）永远看不到这个错。CI 的 `desktop` job 第一次
跑就撞上了。

画的是一张占位图：绿色圆角方块 + 白色对话气泡。等有正式视觉稿，直接替换
PNG 并删掉这个脚本即可（脚本本身不参与构建）。

用法：python scripts/gen-icon.py
"""

import pathlib
import struct
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "ui" / "src-tauri" / "icons"

GREEN = (7, 193, 96, 255)
WHITE = (255, 255, 255, 255)
CLEAR = (0, 0, 0, 0)


def rounded_rect(x, y, x0, y0, x1, y1, r):
    if not (x0 <= x <= x1 and y0 <= y <= y1):
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def triangle(x, y, a, b, c):
    def sign(p, q, r):
        return (p[0] - r[0]) * (q[1] - r[1]) - (q[0] - r[0]) * (p[1] - r[1])

    d1, d2, d3 = sign((x, y), a, b), sign((x, y), b, c), sign((x, y), c, a)
    has_neg = d1 < 0 or d2 < 0 or d3 < 0
    has_pos = d1 > 0 or d2 > 0 or d3 > 0
    return not (has_neg and has_pos)


def shade(x, y, size):
    """一个像素的颜色（坐标按 512 画布给，size 用于等比缩放）。"""
    s = size / 512.0
    x, y = x / s, y / s
    if not rounded_rect(x, y, 32, 32, 480, 480, 96):
        return CLEAR
    bubble = rounded_rect(x, y, 128, 152, 384, 336, 48) or triangle(
        x, y, (168, 330), (248, 330), (168, 400)
    )
    if not bubble:
        return GREEN
    for cx in (200, 256, 312):
        if (x - cx) ** 2 + (y - 244) ** 2 <= 22 * 22:
            return GREEN
    return WHITE


def render(size, ss=2):
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            r = g = b = a = 0
            for dy in range(ss):
                for dx in range(ss):
                    px = shade(x * ss + dx, y * ss + dy, size * ss)
                    r += px[0] * px[3]
                    g += px[1] * px[3]
                    b += px[2] * px[3]
                    a += px[3]
            n = ss * ss
            if a == 0:
                row += bytes(4)
            else:
                row += bytes((r // a, g // a, b // a, a // n))
        rows.append(bytes(row))
    return rows


def write_png(path, size):
    rows = render(size)
    raw = b"".join(b"\x00" + r for r in rows)

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    path.write_bytes(png)
    print(f"{path.relative_to(ROOT)}  {size}x{size}  {len(png)} bytes")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for name, size in [
        ("icon.png", 512),
        ("128x128@2x.png", 256),
        ("128x128.png", 128),
        ("32x32.png", 32),
    ]:
        write_png(OUT / name, size)


if __name__ == "__main__":
    main()
