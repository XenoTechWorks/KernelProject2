#!/usr/bin/env python3
"""Chocolate Doom (Ubuntu's, unchanged) with our SDL: the first demo plays to the end as fast as it
can, without drawing and then in a window; then it's quit from its menu: a click gives the window
the keyboard (and, in the demo, opens the menu), up goes round to Quit Game, Enter picks it, y says
yes, and a click closes the ENDOOM screen it shows on the way out (a new window, so like any
other it doesn't have the keyboard until it's clicked).

Skipped when the image was built without Doom (build.sh adds it when this machine has the
chocolate-doom and freedoom packages).

    python3 tests/doom.py
"""

import re
import sys
import time

from machine import Machine

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


with Machine() as m:
    if "chocolate-doom" not in (m.run("ls /bin") or ""):
        print("skipped: this image has no Doom")
        sys.exit(0)

    timed = re.compile(r"timed (\d+) gametics in (\d+) realtics")
    out = m.run("doom -timedemo demo1 -nodraw 2>&1", timeout=300) or ""
    found = timed.search(out)
    check("demo1 plays to the end", found and int(found.group(1)) > 1000, out[-500:])

    out = m.run("doom -timedemo demo1 2>&1", timeout=600) or ""
    found = timed.search(out)
    # Both count 35ths of a second: keeping up with real time means no more realtics than gametics.
    fast = found and int(found.group(1)) >= int(found.group(2))
    check("demo1 drawn in a window, at least in real time", fast, out[-500:])

    # Doom in the foreground; the shell's prompt comes back only once it has quit.
    m.drain()
    start = len(m.output)
    m.type("doom\r")
    time.sleep(12)
    m.tablet(640, 300, buttons=1)  # the window is centered on the screen
    m.tablet(640, 300, buttons=0)
    time.sleep(1)
    for key in ["up", "ret", "y"]:
        m.monitor(f"sendkey {key}", wait=1)
    time.sleep(3)
    m.tablet(640, 300, buttons=1)
    m.tablet(640, 300, buttons=0)
    check("quit from the menu with the keyboard", m._wait_for_prompt(30, start + 4), m.text()[-500:])

failed = results.count(False)
print(f"\n{len(results) - failed} of {len(results)} passed")
sys.exit(1 if failed else 0)
