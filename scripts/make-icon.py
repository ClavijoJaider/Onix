#!/usr/bin/env python3
"""Genera crates/app/assets/AppIcon.icns (solo stdlib + iconutil de macOS)."""
import math, os, struct, subprocess, sys, tempfile, zlib

N = 1024
BG, FG, CUR = (11, 11, 14), (200, 202, 210), (90, 141, 238)

def seg_d(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    t = max(0, min(1, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - ax - t * dx, py - ay - t * dy)

def master():
    # rect redondeado 824x824 centrado (margen estándar de macOS), radio 185
    lo, hi, r = 100, 924, 185
    half, cx = (hi - lo) / 2, N / 2
    W = 36  # semigrosor del trazo
    chev = [(330, 360, 520, 512), (520, 512, 330, 664)]
    px = bytearray()
    for y in range(N):
        row = bytearray()
        yy = y + 0.5
        for x in range(N):
            xx = x + 0.5
            qx, qy = abs(xx - cx) - (half - r), abs(yy - cx) - (half - r)
            d = math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - r
            a = min(1, max(0, 0.5 - d))
            if a == 0:
                row += b"\0\0\0\0"; continue
            col = BG
            if 300 < xx < 800 and 320 < yy < 720:
                dc = min(seg_d(xx, yy, *s) for s in chev) - W
                du = seg_d(xx, yy, 580, 690, 760, 690) - W
                c1, c2 = min(1, max(0, 0.5 - dc)), min(1, max(0, 0.5 - du))
                col = tuple(round(col[i] * (1 - c1) + FG[i] * c1) for i in range(3))
                col = tuple(round(col[i] * (1 - c2) + CUR[i] * c2) for i in range(3))
            row += bytes((*col, round(a * 255)))
        px += row
    return px

def down(px, s):
    f = N // s
    out = bytearray()
    for y in range(s):
        for x in range(s):
            acc = [0, 0, 0, 0]
            for j in range(f):
                o = ((y * f + j) * N + x * f) * 4
                for i in range(f):
                    a = px[o + 3]
                    acc[0] += px[o] * a; acc[1] += px[o + 1] * a; acc[2] += px[o + 2] * a; acc[3] += a
                    o += 4
            n = f * f
            if acc[3]:
                out += bytes((acc[0] // acc[3], acc[1] // acc[3], acc[2] // acc[3], acc[3] // n))
            else:
                out += b"\0\0\0\0"
    return out

def png(px, s):
    raw = b"".join(b"\0" + bytes(px[y * s * 4:(y + 1) * s * 4]) for y in range(s))
    def ch(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d))
    return b"\x89PNG\r\n\x1a\n" + ch(b"IHDR", struct.pack(">IIBBBBB", s, s, 8, 6, 0, 0, 0)) + ch(b"IDAT", zlib.compress(raw, 9)) + ch(b"IEND", b"")

out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..", "crates/app/assets/AppIcon.icns")
m = master()
with tempfile.TemporaryDirectory() as t:
    iset = os.path.join(t, "AppIcon.iconset"); os.mkdir(iset)
    cache = {N: m}
    for base in (16, 32, 128, 256, 512):
        for k in (1, 2):
            s = base * k
            if s not in cache: cache[s] = down(m, s)
            open(f"{iset}/icon_{base}x{base}{'@2x' if k == 2 else ''}.png", "wb").write(png(cache[s], s))
    subprocess.run(["iconutil", "-c", "icns", iset, "-o", out], check=True)
print("OK", out)
