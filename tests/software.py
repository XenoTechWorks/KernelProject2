#!/usr/bin/env python3
"""Software's packages, from the command line (pkg), served by this machine as run.sh does:

- `pkg available` lists what the server has;
- `pkg get` downloads and installs, and the programs run (with the libraries they brought);
- they're still installed after a restart (kept on the disk, linked again at boot);
- `pkg remove` takes one away, and a library two packages shared stays, from the other one.

Needs out/software (python3 software/build.py).

    python3 tests/software.py
"""

import os
import shutil
import sys
import tempfile

from machine import Machine, ROOT

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


fd, disk = tempfile.mkstemp(suffix=".img")
os.close(fd)
shutil.copyfile(os.path.join(ROOT, "out/disk.img"), disk)
try:
    with Machine(disk=disk, scratch_disk=False, software=True) as m:
        out = m.run("pkg available", timeout=60)
        check("pkg available lists the server's packages",
              out is not None and "nethack" in out and "crispy-doom" in out and "Games" in out, out)

        # nsnake and cmatrix both bring ncurses (the system doesn't have it).
        out = m.run("pkg get nsnake cmatrix nethack", timeout=180)
        check("pkg get downloads and installs", out is not None and out.count("Installed ") == 3, out)
        out = m.run("cmatrix -V")
        check("an installed program runs", out is not None and "CMatrix version" in out, out)
        out = m.run("nethack --version", timeout=30)
        check("NetHack runs, through its launcher", out is not None and "NetHack Version 3.6" in out, out)
        if out is None:  # it started the game instead: leave it
            m.run("q", timeout=10)
        out = m.run("ls /usr/games")
        check("its programs are in /usr/games", out is not None and "nsnake" in out.split() and "nethack" in out.split(), out)
        out = m.run("pkg list")
        check("pkg list names them", out is not None and "cmatrix" in out and "(Software)" in out, out)
        out = m.run("pkg get no-such-thing")
        check("asking for something the server hasn't got says so", out is not None and "no package by that name" in out, out)

    # The same disk again: the root filesystem starts afresh, the packages are linked back.
    with Machine(disk=disk, scratch_disk=False, software=True) as m:
        check("after a restart, init links them in again", "Linked 3 apps installed with Software" in m.text(),
              [l for l in m.text().split("\n") if "Software" in l])
        out = m.run("nsnake -v")
        check("and they still run", out is not None and "nsnake" in out.lower(), out)

        out = m.run("pkg remove nsnake")
        check("pkg remove takes it away", out is not None and "Removed nsnake" in out, out)
        gone = m.run("nsnake -v")
        check("its program is gone", gone is not None and "command not found" in gone, gone)
        out = m.run("cmatrix -V")
        check("cmatrix keeps working: the library they shared comes from its own copy now",
              out is not None and "CMatrix version" in out, out)
        out = m.run("pkg list")
        check("pkg list doesn't name it any more", out is not None and "nsnake" not in out and "cmatrix" in out, out)
finally:
    os.unlink(disk)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
