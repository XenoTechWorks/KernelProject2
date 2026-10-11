#!/usr/bin/env python3
"""Packages: a real .deb (built by dpkg-deb, shipped in /usr/share/examples) goes on and comes off
again, and a Windows program in a .zip becomes something you can run by name.

    python3 tests/pkg.py
"""

import sys

from machine import Machine

DEB = "/usr/share/examples/hello_1.0_all.deb"
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


with Machine() as m:
    # A Linux package.
    out = m.run(f"pkg info {DEB}")
    check("pkg info reads the .deb's control file",
          out is not None and "Package: hello" in out and "sample package" in out, out)

    out = m.run(f"pkg install {DEB}")
    check("pkg install unpacks it", out is not None and "Installed hello" in out, out)
    check("the program it brought runs", m.run("hello") == "Hello from a package!")
    check("the other file is there too", m.run("cat /usr/share/doc/hello/README").startswith("Installed by"))

    out = m.run("pkg list")
    check("pkg list names it", out is not None and "hello" in out, out)
    out = m.run("pkg files hello")
    check("pkg files says what it put there",
          out is not None and "usr/bin/hello" in out and "README" in out, out)

    out = m.run("pkg remove hello")
    check("pkg remove takes it away", out is not None and "Removed hello" in out, out)
    gone = m.run("hello")
    check("the program is gone", gone is not None and "command not found" in gone, gone)
    check("pkg list is empty again", "No packages installed" in (m.run("pkg list") or ""))

    # A Windows program kept in a .zip, the way portable apps come.
    m.run("mkdir -p /tmp/w && cat /bin/winapi.exe > /tmp/w/winapi.exe")
    m.run("cd /tmp/w && zip /tmp/demo.zip winapi.exe")
    out = m.run("pkg install /tmp/demo.zip")
    check("a .zip of Windows programs installs into /opt",
          out is not None and "Installed demo into /opt/demo" in out, out)
    ran = m.run("winapi")
    check("the Windows program runs by its plain name", ran is not None and "9/9 passed" in ran, ran)
    m.run("pkg remove demo")
    gone = m.run("winapi")
    check("removing it takes the name away too", gone is not None and "command not found" in gone, gone)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
