#!/usr/bin/env python3
"""Vela, the desktop, driven with QEMU's tablet (absolute pointer) and keyboard:

- `vela &` draws the wallpaper, the bar and the dock;
- the dock's Terminal starts a shell in a window, which gets the keyboard;
- the taskbar button minimizes that window and brings it back;
- the window's maximize button fills the space between the bar and the dock;
- the star menu opens from the bar and Escape closes it.

    python3 tests/vela.py      (OUT=dir saves screenshots there)
"""

import os
import sys
import tempfile
import time

from machine import Machine, read_png

W, H = 1280, 800
BAR_H = 34                       # vela.zig
DOCK_W = 2 * 11 + 5 * 58 - 10    # five pinned apps
DOCK_X, DOCK_Y = (W - DOCK_W) // 2, H - (11 + 48 + 13) - 10
ORB = (23, BAR_H // 2)            # the star menu's button


def brightness(p):
    return sum(p) / 3

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


def screen(m):
    path = os.path.join(tempfile.gettempdir(), f"kp2-vela-{os.getpid()}.png")
    m.screenshot(path)
    _, _, rows = read_png(path)
    os.unlink(path)
    return rows


def click(m, x, y):
    m.tablet(x, y)
    m.tablet(x, y, buttons=1)
    m.tablet(x, y, buttons=0)


def keys(m, text):
    names = {" ": "spc", "/": "slash", ".": "dot", "-": "minus", "\n": "ret", ">": "shift-dot"}
    for ch in text:
        m.monitor(f"sendkey {names.get(ch, ch)}", wait=0.08)


out = os.environ.get("OUT")


def save(m, name):
    if out:
        m.screenshot(os.path.join(out, name))


with Machine() as m:
    started = m.run("vela &")
    check("vela & starts in the background", started is not None and "[1]" in started, started)
    time.sleep(10)  # the wallpaper takes a while under emulation
    rows = screen(m)
    save(m, "vela-1.png")
    # The bar is dark glass: darker than the wallpaper just below it.
    bar_px, below = rows[BAR_H // 2][W // 2], rows[BAR_H + 12][W // 2]
    check("the bar is drawn along the top", brightness(bar_px) < brightness(below), (bar_px, below))
    sky = rows[300][700]
    check("the wallpaper covers the console", sky != (0x1d, 0x21, 0x29), sky)

    # The dock's first app is Terminal.
    click(m, DOCK_X + 11 + 24, DOCK_Y + 11 + 24)
    time.sleep(4)
    save(m, "vela-2.png")
    keys(m, "touch /tmp/from-vela\n")
    time.sleep(2)
    made = m.run("ls /tmp/from-vela")
    check("Terminal runs a shell that gets the keyboard", made is not None and "from-vela" in made and "No such" not in made, made)

    # The terminal opens centered: its middle is the window's. Its taskbar button is the first.
    middle = (W // 2 + 96, 430)
    terminal_bg = screen(m)[middle[1]][middle[0]]
    task = (100 + 90, BAR_H // 2)
    click(m, *task)
    time.sleep(2)
    hidden = screen(m)[middle[1]][middle[0]]
    check("its taskbar button minimizes it", hidden != terminal_bg, (terminal_bg, hidden))
    click(m, *task)
    time.sleep(2)
    back = screen(m)[middle[1]][middle[0]]
    check("and brings it back", back == terminal_bg, (terminal_bg, back))

    # Maximize: the middle of the frame's three buttons (console.rs: close is 44 wide, 8 in from the
    # right edge; maximize 28 wide, 2 to its left), in the header.
    rows = screen(m)
    y = 0
    for yy in range(BAR_H + 2, H // 2):  # the frame's top edge: the first row that isn't sky
        if rows[yy][W // 2 + 96] == terminal_bg:
            y = yy
            break
    header_y = y - 34 + 17  # the contents start below the 34-pixel header
    right = max(x for x in range(W // 2, W) if rows[y + 5][x] == terminal_bg) + 1
    click(m, right - 8 - 44 - 2 - 14, header_y)
    time.sleep(3)
    save(m, "vela-3.png")
    rows = screen(m)
    check("maximize fills from the bar to the dock", rows[BAR_H + 34 + 20][20] == terminal_bg and rows[DOCK_Y - 20][W - 20] == terminal_bg,
          (rows[BAR_H + 34 + 20][20], rows[DOCK_Y - 20][W - 20]))

    # The star menu.
    spot = (6 + 140, BAR_H + 6 + 120)
    closed = screen(m)[spot[1]][spot[0]]
    click(m, *ORB)
    time.sleep(3)
    save(m, "vela-4.png")
    opened = screen(m)[spot[1]][spot[0]]
    check("the star menu opens", opened != closed, (closed, opened))
    m.monitor("sendkey esc")
    time.sleep(2)
    check("Escape closes it", screen(m)[spot[1]][spot[0]] == closed)

    # The wallpaper picker: the star menu's row after the apps (eight of them: Doom is in the
    # image), then the second picture (alphabetically Budapest; Lake Tahoe is the default).
    corner = (W - 30, H - 10)  # below the maximized terminal, beside the dock: wallpaper
    before = screen(m)[corner[1]][corner[0]]
    picker_w, picker_h = 2 * 22 + 4 * 192 + 3 * 16, 58 + 4 * 150 + 8
    px, py = (W - picker_w) // 2, max(BAR_H + 8, (H - picker_h) // 2 - 20)
    first = (px + 22 + 40, py + 58 + 40)  # inside the first thumbnail
    under = screen(m)[first[1]][first[0]]
    click(m, *ORB)
    time.sleep(3)
    click(m, 6 + 8 + 100, BAR_H + 6 + 70 + 13 + 8 * 42 + 19)
    time.sleep(4)
    save(m, "vela-5.png")
    shown = screen(m)[first[1]][first[0]]
    check("Desktop Background opens the picker", shown != under, (under, shown))
    click(m, px + 22 + 208 + 96, py + 58 + 60)
    time.sleep(8)  # decoding a 1280x800 JPEG under emulation
    save(m, "vela-6.png")
    after = screen(m)[corner[1]][corner[0]]
    check("clicking a picture changes the wallpaper", after != before, (before, after))
    m.monitor("sendkey esc")
    time.sleep(1)
    conf = m.run("cat /root/.config/vela.conf")
    check("the choice is saved in /root/.config/vela.conf", conf is not None and "wallpaper=budapest" in conf, conf)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
