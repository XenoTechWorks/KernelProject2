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

    # The Welcome window opens with Vela. New windows open centered on a 40x10 text window, a
    # little further down and right for each window already there (the desktop, the bar and the
    # dock: 3), then keep that center when the app sets its size (console.rs open_window).
    default_fw = 40 * 8 + 2 * 6 + 2
    wx = (W - default_fw) // 2 + 32 * 3 + (default_fw - (640 + 2)) // 2
    wy = BAR_H + 64 + 32 * 3
    content = (wx + 1, wy + 34)
    head = rows[content[1] + 50][content[0] + 320]
    check("the Welcome window opens with Vela", head[2] > head[0] + 20, head)
    click(m, content[0] + 640 - 24 - 50, content[1] + 470 - 32)  # its Close button
    time.sleep(2)
    gone = screen(m)[content[1] + 50][content[0] + 320]
    check("its Close button closes it", gone != head, (head, gone))

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

    # Personalize: the star menu's row after the apps (nine of them: Doom and the installer are in the image).
    PZ_W, PZ_H = 470, 414
    pzx, pzy = (W - PZ_W) // 2, (H - PZ_H) // 2 - 20

    def open_personalize():
        click(m, *ORB)
        time.sleep(3)
        click(m, 6 + 8 + 100, BAR_H + 6 + 70 + 13 + 9 * 42 + 19)
        time.sleep(4)

    header = (W // 2, BAR_H + 10)  # the maximized terminal's header, while it has the keyboard
    sky_header = screen(m)[header[1]][header[0]]
    inside = (pzx + 300, pzy + 30)
    under = screen(m)[inside[1]][inside[0]]
    open_personalize()
    save(m, "vela-5.png")
    check("Personalize opens from the star menu", screen(m)[inside[1]][inside[0]] != under)

    # The fifth swatch (Violet); then Escape gives the keyboard back to the terminal.
    click(m, pzx + 22 + 15 + 4 * 46, pzy + 252)
    time.sleep(2)
    m.monitor("sendkey esc")
    time.sleep(2)
    violet_header = screen(m)[header[1]][header[0]]
    check("a window color changes the frame of the window in front", violet_header != sky_header and violet_header[2] > violet_header[1],
          (sky_header, violet_header))

    # "Change…" opens the wallpaper picker; the second picture is Budapest (Lake Tahoe's the default).
    corner = (W - 30, H - 10)  # below the maximized terminal, beside the dock: wallpaper
    before = screen(m)[corner[1]][corner[0]]
    picker_w, picker_h = 2 * 22 + 4 * 192 + 3 * 16, 58 + 4 * 150 + 8
    px, py = (W - picker_w) // 2, max(BAR_H + 8, (H - picker_h) // 2 - 20)
    open_personalize()
    click(m, pzx + 202 + 56, pzy + 120 + 15)
    time.sleep(4)
    save(m, "vela-6.png")
    click(m, px + 22 + 208 + 96, py + 58 + 60)
    time.sleep(8)  # decoding a 1280x800 JPEG under emulation
    save(m, "vela-7.png")
    after = screen(m)[corner[1]][corner[0]]
    check("Change… opens the picker, and a picture changes the wallpaper", after != before, (before, after))
    m.monitor("sendkey esc")
    time.sleep(1)
    conf = m.run("cat /root/.config/vela.conf")
    check("the settings are saved in /root/.config/vela.conf", conf is not None and "wallpaper=budapest" in conf and "color=violet" in conf, conf)

    # Hovering over a dock icon shows its name above it.
    above = (DOCK_X + 11 + 24, DOCK_Y - 21)
    no_tip = screen(m)[above[1]][above[0]]
    m.tablet(DOCK_X + 11 + 24, DOCK_Y + 11 + 24)
    time.sleep(2)
    save(m, "vela-8.png")
    check("a dock icon shows its name on hover", screen(m)[above[1]][above[0]] != no_tip)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
