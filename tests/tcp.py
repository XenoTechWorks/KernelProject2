#!/usr/bin/env python3
"""TCP, both ways: Ubuntu's own curl fetching from a web server on this machine (and a big file
that has to arrive byte for byte), a refused connection, and a server inside the guest that this
machine connects to. Then curl on the real internet, over HTTP and HTTPS.

The internet checks need this machine to have a working connection.

    python3 tests/tcp.py
"""

import hashlib
import os
import socket
import subprocess
import sys
import tempfile
import time

from machine import Machine

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# A web server here; QEMU's user network shows this machine to the guest as 10.0.2.2.
www = tempfile.mkdtemp()
open(os.path.join(www, "hello.txt"), "w").write("hello over TCP\n")
big = bytes((i * 7 + 3) % 251 for i in range(300_000))
open(os.path.join(www, "big.bin"), "wb").write(big)
web_port = free_port()
server = subprocess.Popen([sys.executable, "-m", "http.server", str(web_port), "--directory", www],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(1)
into_guest = free_port()

try:
    with Machine(forward=[(into_guest, 7777)]) as m:
        out = m.run(f"curl -s http://10.0.2.2:{web_port}/hello.txt", timeout=60)
        check("curl fetches a page over TCP", out == "hello over TCP", out)

        m.run(f"curl -s -o /tmp/big.bin http://10.0.2.2:{web_port}/big.bin", timeout=120)
        out = m.run("busybox64.exe md5sum /tmp/big.bin", timeout=60)
        check("a 300 KB download arrives byte for byte",
              out is not None and out.startswith(hashlib.md5(big).hexdigest()), out)

        out = m.run("nc 10.0.2.2 9 < /etc/hostname", timeout=60)
        check("nobody listening is 'connection refused'", out == "nc: connection refused", out)

        # A server inside the guest: nc -l, reached through QEMU's port forward.
        m.type("nc -l 7777 > /tmp/got.txt\r")
        time.sleep(1.5)
        with socket.create_connection(("127.0.0.1", into_guest), timeout=20) as c:
            c.sendall(b"sent from outside\n")
            c.shutdown(socket.SHUT_WR)
            c.settimeout(20)
            while c.recv(1024):  # until the guest closes its side
                pass
        time.sleep(1)
        m.drain()
        out = m.run("cat /tmp/got.txt")
        check("a server in the guest accepts a connection from outside", out == "sent from outside", out)

        # The internet.
        out = m.run("curl -sI http://example.com", timeout=120)
        check("curl over HTTP to the internet (DNS through glibc)",
              out is not None and out.startswith("HTTP/1.1 200"), out)
        out = m.run("curl -s https://example.com -o /tmp/page.html; cat /tmp/page.html; echo", timeout=120)
        check("curl over HTTPS: TLS and certificates over our TCP",
              out is not None and "<title>Example Domain</title>" in out, (out or "")[:200])
finally:
    server.kill()

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
