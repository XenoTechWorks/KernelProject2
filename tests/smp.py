#!/usr/bin/env python3
"""More than one CPU: booted with 4, the kernel starts them all, says so, and programs really run
at the same time: four `spin`s at once (each a fixed amount of work, no system calls) take about
as long as one alone, where on one CPU they'd take four times as long.

    python3 tests/smp.py
"""

import re
import sys

from machine import Machine

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


with Machine(smp=4) as m:
    log = m.text()
    check("the boot starts 4 CPUs", "CPUs: 4" in log, [l for l in log.splitlines() if "CPU" in l])
    cpuinfo = m.run("cat /proc/cpuinfo")
    check("/proc/cpuinfo lists 4 processors", cpuinfo is not None and len(re.findall(r"^processor\s*:", cpuinfo, re.M)) == 4, cpuinfo)
    online = m.run("cat /sys/devices/system/cpu/online")
    check("/sys says 0-3 are online", online is not None and online.strip() == "0-3", online)

    alone = m.run("spin 300", timeout=300)
    together = m.run("spin 300 & spin 300 & spin 300 & spin 300 & wait", timeout=600)
    try:
        one = int(alone.strip().splitlines()[-1])
        four = [int(x) for x in re.findall(r"^\d+$", together, re.M)]
    except (AttributeError, ValueError, IndexError):
        one, four = 0, []
    check("four spins finish", len(four) == 4, together)
    print(f"      one alone: {one} ms; four at once: {four} ms")
    # In parallel each takes about as long as one alone; one CPU at a time would be about 4x.
    check("four at once take about as long as one alone (they run in parallel)", one > 0 and len(four) == 4 and max(four) < one * 2,
          (one, four))

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
