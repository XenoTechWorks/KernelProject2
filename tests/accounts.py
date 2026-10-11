#!/usr/bin/env python3
"""Accounts: making a user, giving them a password, becoming them, and finding out that they
can't do root's things.

    python3 tests/accounts.py
"""

import sys
import time

from machine import Machine

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


def typed(m, *parts, wait=0.6):
    for p in parts:
        m.type(p)
        time.sleep(wait)


with Machine() as m:
    check("we start as root", m.run("whoami") == "root")
    check("id says so", m.run("id") == "uid=0(root) gid=0(root)")

    out = m.run("useradd jax")
    check("useradd makes an account", out is not None and "uid 1000" in out, out)
    check("it's in /etc/passwd", "jax:x:1000:1000" in (m.run("cat /etc/passwd") or ""))
    check("it has a home", "jax" in (m.run("ls /home") or ""))

    # Give them a password; it's stored hashed, never in the clear.
    typed(m, "passwd jax\r", "secret\r", "secret\r")
    time.sleep(1)
    m.drain()
    shadow = m.run("cat /etc/shadow")
    check("passwd stores a sha256crypt hash, not the password",
          shadow is not None and "jax:$5$" in shadow and "secret" not in shadow, shadow)

    # Become them. root is never asked for a password.
    typed(m, "su jax\r", wait=1.5)
    check("su becomes the other user", m.run("whoami") == "jax")
    check("their id follows", m.run("id") == "uid=1000(jax) gid=1000(jax)")

    # What they may not do.
    out = m.run("cat /etc/shadow")
    check("they can't read /etc/shadow", out is not None and "Permission denied" in out, out)
    out = m.run("useradd bob")
    check("they can't add users", out is not None and "only root" in out, out)

    # Becoming themselves again does ask for the password, and the right one works.
    typed(m, "su jax\r", wait=1.0)
    typed(m, "wrong\r", wait=1.0)
    m.drain()
    out = m.run("whoami")
    check("a wrong password is refused", out == "jax", out)

    # And the right one lets them through, into a shell inside the first.
    typed(m, "su jax\r", wait=1.0)
    typed(m, "secret\r", wait=2.0)
    m.drain()
    check("the right password is accepted", m.run("whoami") == "jax")
    typed(m, "exit\r", wait=2.0)

    typed(m, "exit\r", wait=2.5)
    check("leaving their shell puts root back", m.run("whoami") == "root")

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
