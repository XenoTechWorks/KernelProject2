#!/usr/bin/env python3
"""Files, the file explorer, used with the mouse and keyboard in a scratch folder:

- a new folder from the toolbar, named by typing;
- a file copied (Ctrl+C), the folder opened with a double-click, and the file pasted (Ctrl+V);
- the copy deleted (Delete, then Enter to confirm);
- the details view; and the picture viewer opening a photo.

    python3 tests/files.py      (OUT=dir saves screenshots there)
"""

import os
import sys
import tempfile
import time

from machine import Machine, read_png

W, H = 1280, 800
WIN_W, WIN_H = 900, 560
SIDE_W, TOOLBAR_H = 176, 46     # files.zig
CELL_W, CELL_H = 104, 94

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


def click(m, x, y):
    m.tablet(x, y)
    m.tablet(x, y, buttons=1)
    m.tablet(x, y, buttons=0)


def double(m, x, y):
    """Two quick clicks, as a person double-clicks (tablet clicks take too long each)."""
    m.tablet(x, y)
    for _ in range(2):
        m.monitor("mouse_button 1", wait=0.05)
        m.monitor("mouse_button 0", wait=0.05)


def keys(m, names):
    for k in names:
        m.monitor(f"sendkey {k}", wait=0.1)


def shot(m, name):
    path = os.path.join(os.environ.get("OUT", tempfile.gettempdir()), name)
    m.screenshot(path)
    return read_png(path)[2]


# The window opens centered on a 40x10 text window (no other windows here), then keeps that
# center at its own size (console.rs open_window).
default_fw = 40 * 8 + 2 * 6 + 2
wx = (W - default_fw) // 2 + (default_fw - (WIN_W + 2)) // 2
cx, cy = wx + 1, 64 + 34  # the contents' top-left


def item(i):
    """The middle of item i in the icons view (one row is enough here)."""
    return cx + SIDE_W + 1 + 8 + i * CELL_W + CELL_W // 2, cy + TOOLBAR_H + 8 + 40


with Machine() as m:
    m.run("mkdir -p /tmp/t; echo hello > /tmp/t/a.txt")
    m.run("files /tmp/t &")
    time.sleep(6)
    rows = shot(m, "files-1.png")
    bar = rows[cy + 20][cx + 400]
    check("Files opens with its toolbar", bar != (0x1d, 0x21, 0x29) and bar[2] >= bar[0], bar)

    # New folder (the toolbar button left of the two view buttons), named "docs".
    click(m, cx + WIN_W - 118 + 17, cy + 23)
    time.sleep(2)
    keys(m, ["ctrl-a"])  # (nothing in a rename box: just to be sure the window has the keyboard)
    for _ in range(12):
        keys(m, ["backspace"])
    keys(m, ["d", "o", "c", "s", "ret"])
    time.sleep(2)
    listing = m.run("ls /tmp/t")
    check("a new folder, named by typing", listing is not None and "docs" in listing.split(), listing)

    # Folders come first: docs is item 0, a.txt item 1. Copy a.txt, open docs, paste.
    click(m, *item(1))
    time.sleep(1)
    keys(m, ["ctrl-c"])
    double(m, *item(0))
    time.sleep(2)
    keys(m, ["ctrl-v"])
    time.sleep(2)
    shot(m, "files-2.png")
    pasted = m.run("cat /tmp/t/docs/a.txt")
    check("copy, open the folder with a double-click, paste", pasted is not None and pasted.strip() == "hello", pasted)

    # Delete the copy: select it, Delete, Enter confirms.
    click(m, *item(0))
    time.sleep(1)
    keys(m, ["delete"])
    time.sleep(1)
    shot(m, "files-3.png")
    keys(m, ["ret"])
    time.sleep(2)
    gone = m.run("ls /tmp/t/docs")
    check("Delete asks, and Enter deletes", gone is not None and "a.txt" not in gone, gone)

    # The details view (the rightmost toolbar button), back in /tmp/t.
    keys(m, ["backspace"])
    time.sleep(1)
    click(m, cx + WIN_W - 42 + 17, cy + 23)
    time.sleep(2)
    rows = shot(m, "files-4.png")
    header = rows[cy + TOOLBAR_H + 12][cx + SIDE_W + 200]
    check("the details view has its column headings", header != rows[cy + TOOLBAR_H + 200][cx + SIDE_W + 200], header)

    # A photo opens in the picture viewer (a second window over the first).
    m.run("pictures /usr/share/backgrounds/lake-tahoe.jpg &")
    time.sleep(8)
    rows = shot(m, "files-5.png")
    middle = rows[H // 2][W // 2]
    check("pictures shows a photo", middle != rows[cy + 300][cx + 600] or middle[2] > 120, middle)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
