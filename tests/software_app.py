#!/usr/bin/env python3
"""Software, the app, used with the mouse: it lists what the server has; Install on the first
one downloads and installs it (with its progress where the button was), and the button becomes
Play, which starts the game in a Terminal window. In Vela, the star menu then has a second
column with what Software installed.

Needs out/software (python3 software/build.py).

    python3 tests/software_app.py      (OUT=dir saves screenshots there)
"""

import os
import sys
import tempfile
import time

from machine import Machine, read_png

W, H = 1280, 800
WIN_W, WIN_H = 860, 600
TOOLBAR_H, ROW_H, MARGIN = 46, 84, 14  # software.zig

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


def click(m, x, y):
    m.tablet(x, y)
    m.tablet(x, y, buttons=1)
    m.tablet(x, y, buttons=0)


def shot(m, name):
    path = os.path.join(os.environ.get("OUT", tempfile.gettempdir()), name)
    m.screenshot(path)
    return read_png(path)[2]


def running(m, program):
    """Whether a process whose command line has `program` in it is running (from /proc)."""
    pids = [p for p in (m.run("ls /proc") or "").split() if p.isdigit()]
    out = m.run("cat " + " ".join(f"/proc/{p}/cmdline" for p in pids)) or ""
    return program in out


# The window opens centered on a 40x10 text window (the only other window), then keeps that
# center at its own size (console.rs open_window).
default_fw = 40 * 8 + 2 * 6 + 2
wx = (W - default_fw) // 2 + (default_fw - (WIN_W + 2)) // 2
cx, cy = wx + 1, 64 + 34  # the contents' top-left
row0 = (cx + MARGIN, cy + TOOLBAR_H + 10)  # the first package's card
button0 = (row0[0] + (WIN_W - 2 * MARGIN) - 16 - 48, row0[1] + (ROW_H - 10) // 2)  # its main button

with Machine(software=True) as m:
    m.run("software &")
    time.sleep(10)  # asking the server, then the pictures one by one
    rows = shot(m, "software-1.png")
    card = rows[row0[1] + 30][row0[0] + 200]
    check("Software opens with the packages listed", card != rows[cy + WIN_H - 60][cx + WIN_W // 2] or card != (0x22, 0x26, 0x2e), card)
    blue = rows[button0[1]][button0[0]]
    check("the first one has an Install button", blue[2] > blue[0] + 60, blue)

    click(m, *button0)
    installed = False
    for _ in range(60):
        time.sleep(2)
        out = m.run("ls /root/.local/software") or ""
        if "bastet" in out.split():
            installed = True
            break
    time.sleep(3)  # pkg links it in, then Software notices pkg has finished
    shot(m, "software-2.png")
    check("Install downloads and installs it", installed, m.run("ls /root/.local/software"))
    check("its program is in place", "bastet" in (m.run("ls /usr/games") or ""))

    click(m, *button0)  # now Play
    time.sleep(6)
    shot(m, "software-3.png")
    check("Play starts it, in a Terminal window", running(m, "bastet") and running(m, "terminal"))

    # Vela's star menu: Software's games in a second column.
    m.run("vela &")
    time.sleep(12)
    menu_spot = (6 + 280 + 120, 34 + 6 + 90)
    before = shot(m, "software-4.png")[menu_spot[1]][menu_spot[0]]
    click(m, 23, 17)  # the star
    time.sleep(3)
    after = shot(m, "software-5.png")[menu_spot[1]][menu_spot[0]]
    check("the star menu has a column for what Software installed", after != before, (before, after))
    m.monitor("sendkey esc")

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
