#!/usr/bin/env python3
"""The archive tools: gzip, tar and zip. Each is checked both ways round against BusyBox, so the
files we write are the real formats and we read what other tools write.

    python3 tests/archive.py
"""

import sys

from machine import Machine

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


with Machine() as m:
    original = m.run("cat /etc/os-release; echo")

    # gzip: smaller than it was, and the same text when unpacked.
    m.run("gzip -c /etc/os-release > /tmp/o.gz")
    sizes = m.run("ls -l /etc/os-release /tmp/o.gz")
    check("gzip writes a smaller file", sizes is not None and "o.gz" in sizes, sizes)
    check("zcat gets the text back", m.run("zcat /tmp/o.gz; echo") == original)
    check("BusyBox reads our .gz", m.run("busybox64.exe gzip -d -c /tmp/o.gz; echo") == original)
    m.run("busybox64.exe gzip -c /etc/os-release > /tmp/b.gz")
    check("we read BusyBox's .gz", m.run("zcat /tmp/b.gz; echo") == original)

    # gzip on a named file removes the original and puts it back.
    m.run("cat /etc/os-release > /tmp/r.txt")
    m.run("gzip /tmp/r.txt")
    left = m.run("ls /tmp")
    m.run("gunzip /tmp/r.txt.gz")
    check("gzip FILE replaces it with .gz, gunzip puts it back",
          left is not None and "r.txt.gz" in left and m.run("cat /tmp/r.txt; echo") == original, left)

    # tar, through gzip, keeping the folder inside it.
    m.run("cd / && tar -czf /tmp/e.tgz etc")
    listing = m.run("tar -tzf /tmp/e.tgz")
    check("tar lists what it packed", listing is not None and "etc/os-release" in listing and "etc/xdg/fastfetch/config.jsonc" in listing, listing)
    m.run("mkdir -p /tmp/t && tar -xzf /tmp/e.tgz -C /tmp/t")
    check("tar unpacks the same text", m.run("cat /tmp/t/etc/os-release; echo") == original)
    bb = m.run("busybox64.exe tar -tzf /tmp/e.tgz")
    check("BusyBox reads our .tar.gz", bb is not None and "etc/os-release" in bb, bb)
    m.run("cd / && busybox64.exe tar -czf /tmp/bb.tgz etc")
    m.run("mkdir -p /tmp/t2 && tar -xzf /tmp/bb.tgz -C /tmp/t2")
    check("we unpack BusyBox's .tar.gz", m.run("cat /tmp/t2/etc/os-release; echo") == original)

    # zip.
    m.run("cd / && zip -r /tmp/e.zip etc")
    listing = m.run("unzip -l /tmp/e.zip")
    check("unzip lists what zip packed", listing is not None and "etc/os-release" in listing, listing)
    m.run("mkdir -p /tmp/z && unzip /tmp/e.zip -d /tmp/z")
    check("unzip gets the same text back", m.run("cat /tmp/z/etc/os-release; echo") == original)
    bb = m.run("busybox64.exe unzip -l /tmp/e.zip")
    check("BusyBox reads our .zip", bb is not None and "etc/os-release" in bb, bb)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
