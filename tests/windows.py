#!/usr/bin/env python3
"""Windows over the console, driven with QEMU's PS/2 mouse and keyboard:

- a `textclock` (text) window is dragged by its header and closed with its × button;
- another one is clicked to get the keyboard, and quit with q;
- the `widgets` (graphical) app draws, its switch turns on when clicked, it resizes by its right
  and left edges, and × closes it.

    python3 tests/windows.py      (OUT=dir saves screenshots there)
"""

import os
import struct
import sys
import tempfile
import time
import zlib

from machine import Machine

SCREEN_W = 1280
HEADER = 34  # console.rs
CELL_W, CELL_H, PAD = 8, 16, 6


def frame_x(content_w):
    """Where a window opens (console.rs open_window), once its program has set its size: centered
    on the default 40x10 text window."""
    default_fw = 40 * CELL_W + 2 * PAD + 2
    return (SCREEN_W - default_fw) // 2 + (default_fw - (content_w + 2)) // 2, 64


def close_button(x, y, content_w):
    return x + content_w + 2 - 20, y + HEADER // 2


class Pointer:
    """The mouse pointer, moved to screen pixels (the kernel starts it at the top-left corner)."""

    def __init__(self, machine):
        self.m, self.x, self.y = machine, 0, 0

    def to(self, x, y):
        self.m.mouse(x - self.x, y - self.y)
        self.x, self.y = x, y

    def click(self, x, y):
        self.to(x, y)
        self.m.mouse(buttons=1)
        self.m.mouse(buttons=0)


def read_png(path):
    """(width, height, rows of RGB tuples) from an 8-bit RGB/RGBA PNG like QEMU's screendump."""
    data = open(path, "rb").read()
    pos, idat, width, height, channels = 8, b"", 0, 0, 3
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            width, height, _, color = struct.unpack(">IIBB", body[:10])
            channels = 4 if color == 6 else 3
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    raw = zlib.decompress(idat)
    stride = width * channels
    rows, prev, i = [], bytearray(stride), 0
    for _ in range(height):
        kind, line = raw[i], bytearray(raw[i + 1:i + 1 + stride])
        i += 1 + stride
        for x in range(stride):
            a = line[x - channels] if x >= channels else 0
            b, c = prev[x], prev[x - channels] if x >= channels else 0
            if kind == 1:
                line[x] = (line[x] + a) & 255
            elif kind == 2:
                line[x] = (line[x] + b) & 255
            elif kind == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append([tuple(line[x:x + 3]) for x in range(0, stride, channels)])
        prev = line
    return width, height, rows


def pixel(m, x, y):
    path = os.path.join(tempfile.gettempdir(), f"kp2-shot-{os.getpid()}.png")
    m.screenshot(path)
    _, _, rows = read_png(path)
    os.unlink(path)
    return rows[y][x]


results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


out = os.environ.get("OUT")
with Machine() as m:
    p = Pointer(m)

    # A text window: clock is 26 columns wide.
    clock_w = 26 * CELL_W + 2 * PAD  # textclock
    x, y = frame_x(clock_w)
    started = m.run("textclock &")
    check("textclock & starts a background job", started is not None and "[1]" in started, started)
    time.sleep(1)

    # Drag it by its header 300 pixels left and 200 down, then close it with × where it is now.
    p.to(x + 40, y + 10)
    m.mouse(buttons=1)
    p.to(x + 40 - 300, y + 10 + 200)
    m.mouse(buttons=0)
    if out:
        m.screenshot(os.path.join(out, "windows-1.png"))
    p.click(*close_button(x - 300, y + 200, clock_w))
    time.sleep(1)
    done = m.run("true")
    check("dragged window closes with its × button", done is not None and "[1] Done" in done, done)

    # The tablet (absolute pointer) closes one too: the pointer lands exactly where it's sent.
    m.run("textclock &")
    time.sleep(1)
    cx, cy = close_button(x, y, clock_w)
    m.tablet(cx, cy)
    m.tablet(cx, cy, buttons=1)
    m.tablet(cx, cy, buttons=0)
    time.sleep(1)
    done = m.run("true")
    check("the tablet clicks × right where it points", done is not None and "[1] Done" in done, done)
    p.x, p.y = cx, cy  # the PS/2 pointer moves the same pointer

    # The keyboard stays with the shell until the window is clicked; then q goes to the clock.
    m.run("textclock &")
    time.sleep(1)
    p.click(x + 60, y + HEADER + 40)
    m.monitor("sendkey q")
    time.sleep(1)
    done = m.run("true")
    check("clicked window gets the keyboard", done is not None and "[1] Done" in done, done)

    # A graphical app. Its switch is the first thing in the last row (widgets.zig).
    m.run("widgets &")
    time.sleep(4)
    widgets_w = 640
    x, y = frame_x(widgets_w)
    content_x, content_y = x + 1, y + HEADER
    switch = (content_x + 24 + 12, content_y + 24 + 172 + 13)  # padding, rows above, half its height
    before = pixel(m, *switch)
    p.click(*switch)
    time.sleep(1)
    # The track's left end (the knob is now on the right, the pointer just below this).
    after = pixel(m, content_x + 24 + 8, switch[1] - 6)
    if out:
        m.screenshot(os.path.join(out, "windows-2.png"))
    check("graphical app draws its widgets", before != (0x1d, 0x21, 0x29), before)
    check("clicking the switch turns it on (blue)", after[2] > 180 and after[0] < 120, after)

    # Make it 100 pixels wider by dragging its right edge; × moves with it.
    edge = x + widgets_w + 2 + 2
    p.to(edge, y + 200)
    m.mouse(buttons=1)
    p.to(edge + 100, y + 200)
    m.mouse(buttons=0)
    time.sleep(2)
    if out:
        m.screenshot(os.path.join(out, "windows-3.png"))

    # Drag the left edge right by 80: the window shrinks and its right edge (and ×) stays put.
    p.to(x, y + 200)
    m.mouse(buttons=1)
    p.to(x + 80, y + 200)
    m.mouse(buttons=0)
    time.sleep(2)
    x += 80
    widgets_w -= 80
    widgets_w += 100
    p.click(*close_button(x, y, widgets_w))
    time.sleep(1)
    done = m.run("true")
    check("window resized by its right and left edges closes with × where it is now", done is not None and "[1] Done" in done, done)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
