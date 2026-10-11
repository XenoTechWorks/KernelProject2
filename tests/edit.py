#!/usr/bin/env python3
"""The `edit` text editor, typed at over the serial port: write, move around, undo, cut and paste
lines (through /dev/clipboard), save, and check the file.

    python3 tests/edit.py
"""

import sys
import time

from machine import Machine

CTRL = {c: chr(ord(c) - ord("a") + 1) for c in "abcdefghijklmnopqrstuvwxyz"}
UP, DOWN, END = "\x1b[A", "\x1b[B", "\x1b[F"

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


def keys(m, *parts):
    for p in parts:
        m.type(p)
        time.sleep(0.3)


with Machine() as m:
    # Write three lines and save.
    m.type("edit /tmp/a.txt\r")
    time.sleep(1.5)
    keys(m, "first line\r", "second line\r", "third", CTRL["s"], CTRL["q"])
    time.sleep(1)
    out = m.run("cat /tmp/a.txt; echo")
    check("typing and saving", out is not None and out.strip().split("\n") == ["first line", "second line", "third"], out)

    # Undo the last word typed, then quit without saving: Ctrl+Q twice.
    m.type("edit /tmp/a.txt\r")
    time.sleep(1.5)
    keys(m, DOWN, DOWN, END, " more", CTRL["z"], " end", CTRL["s"], CTRL["q"])
    time.sleep(1)
    out = m.run("cat /tmp/a.txt; echo")
    check("undo takes back a whole word", out is not None and out.strip().split("\n")[-1] == "third end", out)

    # Cut the first line and paste it at the end; the clipboard holds it for other programs.
    m.type("edit /tmp/a.txt\r")
    time.sleep(1.5)
    keys(m, CTRL["k"], DOWN, END, "\r", CTRL["u"], CTRL["s"], CTRL["q"])
    time.sleep(1)
    out = m.run("cat /tmp/a.txt; echo")
    lines = out.strip().split("\n") if out else []
    check("cut and paste a line", lines[:3] == ["second line", "third end", "first line"], out)
    clip = m.run("cat /dev/clipboard")
    check("the cut line is on the clipboard", clip is not None and "first line" in clip, clip)

    # Unsaved changes: the first Ctrl+Q warns, the second quits; the file is unchanged.
    m.type("edit /tmp/a.txt\r")
    time.sleep(1.5)
    keys(m, "junk", CTRL["q"])
    m.drain()
    warned = "Unsaved changes" in m.text()
    keys(m, CTRL["q"])
    time.sleep(1)
    out = m.run("cat /tmp/a.txt; echo")
    check("quitting with unsaved changes warns first", warned and out is not None and "junk" not in out, out)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
