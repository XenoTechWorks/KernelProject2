#!/usr/bin/env python3
"""The network: the card, ARP and ping to the gateway, and a UDP round trip (a DNS lookup).

QEMU's user-mode network stands in for the outside world; the DNS checks need the build machine
to have a working connection.

    python3 tests/net.py
"""

import re
import sys

from machine import Machine

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


with Machine() as m:
    boot = m.text()
    check("the kernel finds the card", "virtio-net:" in boot,
          [l for l in boot.split("\n") if "virtio" in l])

    out = m.run("ifconfig")
    check("ifconfig shows an address and a gateway",
          out is not None and "10.0.2.15" in out and "10.0.2.2" in out, out)

    # Pinging the gateway exercises ARP (we have to learn its address first) and ICMP.
    out = m.run("ping -c 2 10.0.2.2", timeout=60)
    check("ping reaches the gateway", out is not None and "2 received" in out, out)

    # A DNS lookup is a UDP datagram out and another back.
    out = m.run("host example.com", timeout=60)
    found = re.search(r"has address (\d+\.\d+\.\d+\.\d+)", out or "")
    check("a DNS lookup comes back with an address", found is not None, out)

    out = m.run("host nope.invalid", timeout=60)
    check("a name that doesn't exist says so", out is not None and "no such name" in out, out)

    # Changing the setup, and putting it back.
    m.run("ifconfig 10.0.2.99 255.255.255.0 10.0.2.2 10.0.2.3")
    out = m.run("ifconfig")
    check("ifconfig can change the address", out is not None and "10.0.2.99" in out, out)
    m.run("ifconfig 10.0.2.15 255.255.255.0 10.0.2.2 10.0.2.3")

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
