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
PANEL_BG = (0x15, 0x18, 0x1e)
MENU_BG = (0x1c, 0x20, 0x29)

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
    check("the bar is drawn along the top", rows[BAR_H // 2][W // 2] == PANEL_BG, rows[BAR_H // 2][W // 2])
    sky = rows[300][700]
    check("the wallpaper covers the console", sky != (0x1d, 0x21, 0x29) and sky[2] > sky[0], sky)

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

    # Maximize: the frame's middle button (46 pixels in from its right edge, in the header).
    rows = screen(m)
    y = 0
    for yy in range(BAR_H + 2, H // 2):  # the frame's top edge: the first row that isn't sky
        if rows[yy][W // 2 + 96] == terminal_bg:
            y = yy
            break
    header_y = y - 34 + 17  # the contents start below the 34-pixel header
    right = max(x for x in range(W // 2, W) if rows[y + 5][x] == terminal_bg) + 1
    click(m, right - 46, header_y)
    time.sleep(3)
    save(m, "vela-3.png")
    rows = screen(m)
    check("maximize fills from the bar to the dock", rows[BAR_H + 34 + 20][20] == terminal_bg and rows[DOCK_Y - 20][W - 20] == terminal_bg,
          (rows[BAR_H + 34 + 20][20], rows[DOCK_Y - 20][W - 20]))

    # The star menu.
    click(m, 47, BAR_H // 2)
    time.sleep(2)
    save(m, "vela-4.png")
    check("the star menu opens", screen(m)[BAR_H + 6 + 120][6 + 140] == MENU_BG)
    m.monitor("sendkey esc")
    time.sleep(2)
    check("Escape closes it", screen(m)[BAR_H + 6 + 120][6 + 140] != MENU_BG)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
