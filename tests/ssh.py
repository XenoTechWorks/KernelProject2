#!/usr/bin/env python3
"""SSH, with Ubuntu's own OpenSSH client inside the guest, against an SSH server on this machine:
a key made with ssh-keygen, a command run on the server, an interactive login (the terminal in
raw mode, typing going one way and the remote shell's output the other), files copied both ways
with scp, and ssh-agent holding the key (over a Unix socket).

The server is Ubuntu's sshd, run as you, on a spare port, with its own keys and settings in a
temporary folder. If this machine has no sshd, the openssh-server package is downloaded and
unpacked into ~/.cache/kernelproject2/sshd (nothing is installed).

    python3 tests/ssh.py
"""

import getpass
import glob
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time

from machine import ANSI, Machine

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


def find_sshd():
    """(sshd's path, the library folder it needs, or None)."""
    if os.path.exists("/usr/sbin/sshd"):
        return "/usr/sbin/sshd", None
    cache = os.path.expanduser("~/.cache/kernelproject2/sshd")
    root, libs = os.path.join(cache, "root"), os.path.join(cache, "root/usr/lib/x86_64-linux-gnu")
    if not (os.path.exists(os.path.join(root, "usr/sbin/sshd")) and os.path.exists(os.path.join(libs, "libwrap.so.0"))):
        os.makedirs(cache, exist_ok=True)
        # sshd, and libwrap, the one library it needs that a desktop Ubuntu may not have.
        subprocess.run(["apt-get", "download", "openssh-server", "libwrap0"], cwd=cache, check=True,
                       stdout=subprocess.DEVNULL)
        for deb in glob.glob(os.path.join(cache, "*.deb")):
            subprocess.run(["dpkg-deb", "-x", deb, root], check=True)
    return os.path.join(root, "usr/sbin/sshd"), libs


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for(m, pattern, start, timeout=30):
    """Waits until the guest has printed something matching `pattern` since `start`."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        text = ANSI.sub(b"", m.output[start:]).replace(b"\r", b"").decode("utf-8", "replace")
        found = re.search(pattern, text)
        if found:
            return found
        m.drain(0.2)
    return None


# ---- the server, on this machine ----
work = tempfile.mkdtemp(prefix="kp2-ssh-")
sshd, libs = find_sshd()
subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", f"{work}/host_key"], check=True)
port = free_port()
user = getpass.getuser()
open(f"{work}/authorized_keys", "w").close()
with open(f"{work}/sshd_config", "w") as f:
    f.write(f"""Port {port}
ListenAddress 127.0.0.1
HostKey {work}/host_key
PidFile {work}/sshd.pid
AuthorizedKeysFile {work}/authorized_keys
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
Subsystem sftp internal-sftp
""")
env = dict(os.environ, LD_LIBRARY_PATH=libs) if libs else None
server = subprocess.Popen([sshd, "-D", "-e", "-f", f"{work}/sshd_config"], env=env,
                          stdout=subprocess.DEVNULL, stderr=open(f"{work}/sshd.log", "w"))
time.sleep(1)
if server.poll() is not None:
    print("sshd didn't start:\n" + open(f"{work}/sshd.log").read())
    sys.exit(1)

# QEMU's user network shows this machine to the guest as 10.0.2.2.
target = f"-p {port} {user}@10.0.2.2"
try:
    with Machine() as m:
        out = m.run('ssh-keygen -q -t ed25519 -N "" -f /root/.ssh/id_ed25519', timeout=60)
        key = m.run("cat /root/.ssh/id_ed25519.pub") or ""
        check("ssh-keygen makes a key pair", key.startswith("ssh-ed25519 "), (out, key))
        with open(f"{work}/authorized_keys", "w") as f:
            f.write(key.strip() + "\n")

        out = m.run(f"ssh -o StrictHostKeyChecking=accept-new {target} uname -n", timeout=60) or ""
        check("ssh runs a command on the server", socket.gethostname() in out, out)
        known = m.run("cat /root/.ssh/known_hosts") or ""
        check("...and remembers the server's key", "ssh-ed25519" in known, known)

        # An interactive login: our terminal goes raw, and the remote shell answers.
        m.drain()
        start = len(m.output)
        m.type(f"ssh {target}\r")
        logged_in = wait_for(m, r"[#$] $", start + 10, timeout=60) is not None
        start = len(m.output)
        m.type("echo $((6 * 7))\r")
        answered = wait_for(m, r"\n42\n", start) is not None
        start = len(m.output)
        m.type("exit\r")
        back = wait_for(m, r"root:[^\n]*# $", start) is not None
        check("an interactive login: type a command, see its answer, log out", logged_in and answered and back,
              m.text()[-600:])

        # scp, both ways (over SFTP, scp's default now).
        out = m.run(f"scp -P {port} /etc/hostname {user}@10.0.2.2:{work}/from-guest", timeout=60)
        sent = os.path.exists(f"{work}/from-guest") and open(f"{work}/from-guest").read().strip() == "kernelproject2"
        check("scp copies a file to the server", sent, out)
        payload = "".join(f"line {i}\n" for i in range(2000))
        open(f"{work}/to-guest", "w").write(payload)
        out = m.run(f"scp -P {port} {user}@10.0.2.2:{work}/to-guest /tmp/to-guest", timeout=60)
        tail = m.run("cat /tmp/to-guest") or ""
        check("scp copies a file from the server", "line 1999" in tail, (out, tail[-200:]))

        # ssh-agent keeps the key behind a Unix socket; ssh asks it instead of reading the file.
        m.run("rm -f /tmp/agent.sock")
        m.run("ssh-agent -a /tmp/agent.sock > /dev/null")
        m.run("export SSH_AUTH_SOCK=/tmp/agent.sock")
        added = m.run("ssh-add /root/.ssh/id_ed25519 2>&1") or ""
        listed = m.run("ssh-add -l") or ""
        check("ssh-agent takes the key (ssh-add, over a Unix socket)", "ED25519" in listed, (added, listed))
        m.run("mv /root/.ssh/id_ed25519 /root/.ssh/away")
        out = m.run(f"ssh -o IdentitiesOnly=no {target} echo through the agent", timeout=60) or ""
        check("ssh logs in with the agent's key", "through the agent" in out, out)
finally:
    server.terminate()
    server.wait()
    shutil.rmtree(work, ignore_errors=True)

failed = results.count(False)
print(f"\n{len(results) - failed} of {len(results)} passed")
sys.exit(1 if failed else 0)
