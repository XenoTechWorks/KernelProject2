#!/usr/bin/env python3
"""Booting: the boot messages ("[  OK  ] ..." for each step, like Linux distributions show), and
what init starts.

- with `text` on the kernel command line, the console and a shell, and no desktop;
- as a user boots, Vela starts by itself, over the console;
- "autostart=no" in /root/.config/vela.conf keeps it from starting.

    python3 tests/boot.py      (OUT=dir saves screenshots there)
"""

import os
import re
import shutil
import sys
import tempfile
import time

from machine import ROOT, Machine, read_png

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


def statuses(text):
    return re.findall(r"\[ *(OK|WARN|FAILED) *\] (.*)", text)


out = os.environ.get("OUT")

with Machine() as m:
    m.drain(1.5)  # init's last lines can come just after the prompt
    log = m.text()
    lines = statuses(log)
    if out:
        m.screenshot(os.path.join(out, "boot-text.png"))
    check("the kernel reports its steps", any(s == "OK" and t.startswith("Memory:") for s, t in lines), lines[:5])
    check("nothing failed", not any(s == "FAILED" for s, _ in lines), [l for l in lines if l[0] == "FAILED"])
    check("the disk and the network come up", any(t.startswith("Mounted /root") for _, t in lines) and any(t.startswith("Network: virtio-net") for _, t in lines), lines)
    check("init is pid 1", any(t == "Started /sbin/init (pid 1)" for _, t in lines), lines)
    check("init starts the shell", any(t.startswith("Started kpsh") for _, t in lines), lines)
    check("`text` keeps the desktop off", any("Not starting Vela" in t and "text" in t for _, t in lines), lines)
    cmdline = m.run("cat /proc/cmdline")
    check("/proc/cmdline has the command line", cmdline is not None and cmdline.strip() == "text", cmdline)


def bar_shown(m):
    path = os.path.join(tempfile.gettempdir(), f"kp2-boot-{os.getpid()}.png")
    m.screenshot(path)
    _, _, rows = read_png(path)
    os.unlink(path)
    # The console is a flat color under where the bar goes; Vela's glass isn't.
    return rows[17][640] != (0x1d, 0x21, 0x29)


# Two boots from one copy of the disk: the setting from the first is there in the second.
fd, disk = tempfile.mkstemp(suffix=".img")
os.close(fd)
shutil.copyfile(os.path.join(ROOT, "out/disk.img"), disk)
try:
    with Machine(disk=disk, scratch_disk=False, desktop=True) as m:
        m.drain(1.5)  # init's last lines can come just after the prompt
        lines = statuses(m.text())
        check("booting normally starts Vela", any(t.startswith("Started Vela") for _, t in lines), lines)
        time.sleep(12)  # decoding the wallpaper under emulation
        if out:
            m.screenshot(os.path.join(out, "boot-desktop.png"))
        check("and it's on the screen", bar_shown(m))
        m.run("mkdir -p /root/.config; echo autostart=no >> /root/.config/vela.conf")
    with Machine(disk=disk, scratch_disk=False, desktop=True) as m:
        m.drain(1.5)
        lines = statuses(m.text())
        check("switched off in its settings, it isn't started", any("Not starting Vela" in t for _, t in lines)
              and not any(t.startswith("Started Vela") for _, t in lines), lines)
finally:
    os.unlink(disk)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
