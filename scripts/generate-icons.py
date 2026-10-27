#!/usr/bin/env python3
"""Deterministic icon assets; Python standard library, no build-time tooling."""
import math
import struct
import zlib
from pathlib import Path

OUT = Path(__file__).resolve().parents[1] / "crates/app/assets"


def rounded(x, y, left, top, right, bottom, radius):
    dx = max(left + radius - x, 0, x - (right - radius))
    dy = max(top + radius - y, 0, y - (bottom - radius))
    return left <= x <= right and top <= y <= bottom and dx * dx + dy * dy <= radius * radius


def pixel(x, y, recording):
    if recording and math.hypot(x - 0.81, y - 0.81) < 0.165:
        return (240, 62, 76, 255) if math.hypot(x - 0.81, y - 0.81) < 0.125 else (255, 255, 255, 255)
    if not rounded(x, y, 0.04, 0.04, 0.96, 0.96, 0.22):
        return (0, 0, 0, 0)
    # Notebook edge and a clear audio waveform, visible even at 16 px.
    if rounded(x, y, 0.23, 0.20, 0.77, 0.80, 0.07) and not rounded(x, y, 0.28, 0.25, 0.72, 0.75, 0.025):
        return (244, 250, 255, 255)
    for cx, height in [(0.36, 0.13), (0.46, 0.30), (0.56, 0.44), (0.66, 0.20)]:
        if rounded(x, y, cx - 0.025, 0.50 - height / 2, cx + 0.025, 0.50 + height / 2, 0.024):
            return (244, 250, 255, 255)
    return (29, int(108 + 24 * (1 - y)), int(139 + 18 * (1 - y)), 255)


def png(size, recording=False):
    scale = 4
    rows = bytearray()
    for y in range(size):
        rows.append(0)
        for x in range(size):
            samples = [pixel((x + (sx + .5) / scale) / size, (y + (sy + .5) / scale) / size, recording)
                       for sy in range(scale) for sx in range(scale)]
            alpha = sum(c[3] for c in samples)
            rgb = [round(sum(c[i] * c[3] for c in samples) / alpha) if alpha else 0 for i in range(3)]
            rows.extend([*rgb, round(alpha / len(samples))])

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b"")


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    sizes = (16, 24, 32, 48, 64, 256)
    images = [png(size) for size in sizes]
    offset = 6 + 16 * len(sizes)
    entries = bytearray()
    for size, data in zip(sizes, images):
        (OUT / f"notetaker-{size}.png").write_bytes(data)
        entries.extend(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset))
        offset += len(data)
    (OUT / "notetaker.ico").write_bytes(struct.pack("<HHH", 0, 1, len(sizes)) + entries + b"".join(images))
    (OUT / "notetaker-recording-32.png").write_bytes(png(32, True))
