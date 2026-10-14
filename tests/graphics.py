#!/usr/bin/env python3
"""OpenGL and Vulkan, from Software's "mesa" package (Mesa's llvmpipe and lavapipe, drawing with
the processor), through our SDL 2:

- installing a game installs Mesa first (it depends on it), and Mesa can't be removed under it;
- glcube draws with OpenGL, vkcube with Vulkan, each in a window: they say which renderer they
  got, draw frames, and the window shows the cube (not the empty background);
- Neverball, a 3D game, starts and draws its title screen.

Needs out/software (python3 software/build.py). Slow under emulation (no KVM): Mesa is 200 MB.

    python3 tests/graphics.py      (OUT=dir saves screenshots there)
"""

import os
import sys
import tempfile
import time

from machine import Machine, read_png

W, H = 1280, 800
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


def shot(m, name):
    path = os.path.join(os.environ.get("OUT", tempfile.gettempdir()), name)
    m.screenshot(path)
    return read_png(path)[2]


def colorful(rows, x0, y0, x1, y1):
    """How many pixels in the box are clearly colored (not the greys of windows and the console)."""
    n = 0
    for y in range(y0, y1, 4):
        for x in range(x0, x1, 4):
            r, g, b = rows[y][x]
            n += max(r, g, b) - min(r, g, b) > 60
    return n


# A 640x480 window opens centered on the default text window, then keeps that center
# (console.rs open_window); the cube is in its middle.
middle = (W // 2, 64 + 34 + 5 * 16 + 6)
box = (middle[0] - 120, middle[1] - 100, middle[0] + 120, middle[1] + 100)


with Machine(software=True) as m:
    out = m.run("pkg get neverball", timeout=1800)
    check(
        "a game brings Mesa along: it's installed first",
        out is not None
        and "neverball needs mesa first" in out
        and "Installed mesa" in out
        and "Installed neverball" in out,
        out
    )

    out = m.run("pkg remove mesa")
    check(
        "Mesa can't be removed while a game needs it",
        out is not None and "needed by neverball" in out,
        out
    )

    # The font package and the game's symlink must both survive installation.
    for path in ("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
                 "/usr/share/games/neverball/ttf/DejaVuSans-Bold.ttf"):
        out = m.run(f"cat {path} > /dev/null; echo FONT_EXIT=$?")
        check(f"Neverball's font is readable at {path}",
              out is not None and "FONT_EXIT=0" in out, out)

    # Test OpenGL rendering.
    out = m.run("glcube --frames 10 2>&1", timeout=600)
    check(
        "glcube gets OpenGL from llvmpipe and draws",
        out is not None
        and "llvmpipe" in out
        and "10 frames" in out,
        out
    )


    m.run("glcube > /tmp/gl.log 2>&1 &")
    time.sleep(40)
    rows = shot(m, "graphics-gl.png")
    check("and its window shows the cube", colorful(rows, *box) > 50, colorful(rows, *box))
    m.run("kill %1")
    time.sleep(3)

    out = m.run("vkcube --frames 10 2>&1", timeout=600)
    check("vkcube gets Vulkan from lavapipe and draws", out is not None and "llvmpipe" in out and "10 frames" in out, out)
    m.run("vkcube > /tmp/vk.log 2>&1 &")
    time.sleep(40)
    rows = shot(m, "graphics-vk.png")
    check("and its window shows the cube (through the present layer)", colorful(rows, *box) > 50, colorful(rows, *box))
    m.run("kill %2")
    time.sleep(3)

    m.run("neverball > /tmp/nb.log 2>&1 &")
    time.sleep(90)
    rows = shot(m, "graphics-neverball.png")
    log = m.run("cat /tmp/nb.log")
    jobs = m.run("jobs")
    check("Neverball stays running with Mesa's default worker settings",
          jobs is not None and "Running" in jobs and "neverball" in jobs
          and log is not None and "GL renderer: llvmpipe" in log, (jobs, log))
    check("Neverball starts and draws", colorful(rows, 0, 0, W, H) > 500, (colorful(rows, 0, 0, W, H), log))

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
