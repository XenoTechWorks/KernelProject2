#!/usr/bin/env python3
"""Boots KernelProject2, runs the given commands one after another, and saves the screen.

    tests/screenshot.py kpfetch               -> out/screen.png
    OUT=out/fetch.png tests/screenshot.py fastfetch
"""

import os
import sys

from machine import ROOT, Machine

out = os.environ.get("OUT", os.path.join(ROOT, "out/screen.png"))
with Machine() as m:
    for command in sys.argv[1:]:
        m.run(command)
    m.screenshot(out)
print(out)
