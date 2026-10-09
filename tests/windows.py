#!/usr/bin/env python3
"""Windows over the console: opens `clock` windows, drags one with the PS/2 mouse and closes it
with its × button, then clicks another to give it the keyboard and quits it with a key press.

    python3 tests/windows.py      (OUT=dir saves screenshots there)
"""

import os
import sys
import time

from machine import Machine

CELL_W, CELL_H = 8, 16
COLS = 1280 // CELL_W
# Where a new window's frame starts (console.rs open_window), and the clock's frame width.
FIRST_X, FIRST_Y = (COLS - 42) // 2, 2
FRAME_W = 26 + 2


class Pointer:
    """The mouse pointer, moved to cells (the kernel starts it at the top-left corner)."""

    def __init__(self, machine):
        self.m, self.x, self.y = machine, 0, 0

    def to(self, cx, cy):
        x, y = cx * CELL_W + CELL_W // 2, cy * CELL_H + CELL_H // 2
        self.m.mouse(x - self.x, y - self.y)
        self.x, self.y = x, y

    def click(self, cx, cy):
        self.to(cx, cy)
        self.m.mouse(buttons=1)
        self.m.mouse(buttons=0)


results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


out = os.environ.get("OUT")
with Machine() as m:
    p = Pointer(m)

    started = m.run("clock &")
    check("clock & starts a background job", started is not None and "[1]" in started, started)
    time.sleep(1)
    if out:
        m.screenshot(os.path.join(out, "windows-1.png"))

    # Drag it by its title 30 cells left and 10 down, then close it with × where it is now.
    p.to(FIRST_X + 5, FIRST_Y)
    m.mouse(buttons=1)
    p.to(FIRST_X + 5 - 30, FIRST_Y + 10)
    m.mouse(buttons=0)
    if out:
        m.screenshot(os.path.join(out, "windows-2.png"))
    p.click(FIRST_X - 30 + FRAME_W - 3, FIRST_Y + 10)
    time.sleep(1)
    done = m.run("true")
    check("dragged window closes with its × button", done is not None and "[1] Done" in done, done)

    # The keyboard stays with the shell until the window is clicked; then q goes to the clock.
    m.run("clock &")
    time.sleep(1)
    p.click(FIRST_X + 3, FIRST_Y + 3)
    m.monitor("sendkey q")
    time.sleep(1)
    done = m.run("true")
    check("clicked window gets the keyboard", done is not None and "[1] Done" in done, done)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
