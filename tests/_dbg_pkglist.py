import sys
sys.path.insert(0, "tests")
from machine import Machine

DEB = "/usr/share/examples/hello_1.0_all.deb"
with Machine() as m:
    print(m.run(f"pkg install {DEB}"))
    print("list1:", repr(m.run("pkg list")))
    print(m.run("pkg remove hello"))
    print("list2:", repr(m.run("pkg list")))
    print("ls /root/.local:", m.run("ls /root/.local 2>&1"))
    print("ls /var/lib/pkg:", m.run("ls /var/lib/pkg 2>&1"))
