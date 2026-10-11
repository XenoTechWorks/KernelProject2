#!/usr/bin/env python3
"""The graphical text editor `textedit`: type, select all and copy, save; then check the file and
that the clipboard is shared with the rest of the system.

    python3 tests/textedit.py      (OUT=dir saves a screenshot)
"""

import os
import sys
import time

from machine import Machine

HEADER = 34
COLS = 1280 // 8


def frame_x(content_w):
    default_fw = 40 * 8 + 2 * 6 + 2
    return (1280 - default_fw) // 2 + (default_fw - (content_w + 2)) // 2, 64


results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


KEY = {"ctrl_s": "ctrl-s", "ctrl_a": "ctrl-a", "ctrl_c": "ctrl-c", "ctrl_v": "ctrl-v", "ctrl_z": "ctrl-z"}


out = os.environ.get("OUT")
with Machine() as m:
    x, y = frame_x(640)
    m.run("textedit /tmp/t.txt &")
    time.sleep(5)
    # Click the editor area so the window gets the keyboard (then the app focuses its text view).
    m.tablet(x + 200, y + HEADER + 120, buttons=1)
    m.tablet(x + 200, y + HEADER + 120, buttons=0)
    time.sleep(0.3)
    # Type three lines.
    for ch in "hello":
        m.monitor(f"sendkey {ch}")
    m.monitor("sendkey ret")
    for ch in "world":
        m.monitor(f"sendkey {ch}")
    time.sleep(0.3)
    m.monitor("sendkey ctrl-s")
    time.sleep(0.8)
    if out:
        m.screenshot(os.path.join(out, "textedit.png"))
    got = m.run("cat /tmp/t.txt; echo")
    check("typing and Ctrl+S save", got is not None and got.strip().split("\n") == ["hello", "world"], got)

    # Select all, copy: the clipboard holds the whole text, readable from the shell.
    m.monitor("sendkey ctrl-a")
    time.sleep(0.2)
    m.monitor("sendkey ctrl-c")
    time.sleep(0.5)
    clip = m.run("cat /dev/clipboard; echo")
    check("select-all and copy reach the shared clipboard", clip is not None and clip.strip() == "hello\nworld", clip)

    # Put something new on the clipboard from the shell, paste it into the editor after select-all.
    m.run("echo -n REPLACED > /dev/clipboard")
    m.monitor("sendkey ctrl-a")
    time.sleep(0.2)
    m.monitor("sendkey ctrl-v")
    time.sleep(0.3)
    m.monitor("sendkey ctrl-s")
    time.sleep(0.8)
    got = m.run("cat /tmp/t.txt; echo")
    check("paste from the shell's clipboard replaces the selection", got is not None and got.strip() == "REPLACED", got)

    # Undo brings the text back.
    m.monitor("sendkey ctrl-z")
    time.sleep(0.3)
    m.monitor("sendkey ctrl-s")
    time.sleep(0.8)
    got = m.run("cat /tmp/t.txt; echo")
    check("Ctrl+Z undoes the paste", got is not None and "hello" in got, got)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
