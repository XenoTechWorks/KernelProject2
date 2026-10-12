#!/usr/bin/env python3
"""gdb, Ubuntu's own, debugging programs inside the guest through ptrace: a breakpoint at main,
a backtrace, a single step; a breakpoint in the C library, with the call's arguments read from
the registers; and attaching to a program that's already running.

The image has gdb only when built with it (it brings Python's standard library):
    WITH_GDB=1 ./build.sh && python3 tests/gdb.py
"""

import re
import sys

from machine import Machine

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


with Machine() as m:
    if "gdb" not in (m.run("ls /bin") or "").split():
        print("skipped: this image has no gdb (WITH_GDB=1 ./build.sh)")
        sys.exit(0)

    out = m.run("gdb -batch -ex 'break main' -ex run -ex bt -ex 'info registers rip' -ex stepi "
                "-ex 'info registers rip' -ex continue hello-dynamic 2>&1", timeout=300) or ""
    rips = re.findall(r"^rip\s+(0x[0-9a-f]+)", out, re.M)
    check("a breakpoint at main stops the program there", "Breakpoint 1, " in out and "in main ()" in out, out[-800:])
    check("...the backtrace shows it", re.search(r"^#0 .* in main \(\)", out, re.M) is not None, out[-800:])
    check("...one instruction steps on", len(rips) == 2 and rips[0] != rips[1], rips)
    check("...and it runs to the end", "exited normally" in out, out[-400:])

    out = m.run("gdb -batch -ex 'set breakpoint pending on' -ex 'break write' -ex run -ex 'print $rdi' "
                "-ex 'x/s $rsi' -ex continue hello-dynamic 2>&1", timeout=300) or ""
    check("a breakpoint in the C library's write, with its arguments read",
          re.search(r"Breakpoint 1, .*write", out) is not None and "$1 = 1" in out and '"Hello from glibc!' in out, out[-800:])

    # Ubuntu's ssh-agent, which waits in poll(), in glibc (which has symbols; our own tools don't).
    started = m.run("ssh-agent -D -a /tmp/gdb-agent.sock > /dev/null &") or ""
    pid = re.search(r"\[\d+\] (\d+)", started)
    out = m.run(f"gdb -batch -p {pid.group(1)} -ex bt 2>&1", timeout=300) if pid else ""
    check("attaching to a running program shows where it waits", out is not None and re.search(r"^#0 .*poll", out or "", re.M) is not None,
          (started, (out or "")[-800:]))

failed = results.count(False)
print(f"\n{len(results) - failed} of {len(results)} passed")
sys.exit(1 if failed else 0)
