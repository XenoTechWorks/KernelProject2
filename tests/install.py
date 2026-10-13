#!/usr/bin/env python3
"""The installer: puts KernelProject2 on a blank second disk, then boots that disk on its own
with UEFI firmware (no ISO).

- `install --list` sees both disks, and won't offer the one /root is on;
- `install --yes /dev/vdb` writes a GPT with an EFI system partition and a home partition;
- checked from outside: sgdisk reads the table, e2fsck finds the home ext2 clean, mtools reads the
  FAT32 boot partition and its kernel is the ISO's, byte for byte;
- booted with OVMF from that disk alone, /root is the home partition and keeps a file across a
  reboot.

    python3 tests/install.py
"""

import os
import re
import subprocess
import sys
import tempfile
import time

from machine import ROOT, Machine, read_png

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


fd, target = tempfile.mkstemp(suffix=".img")
os.close(fd)
os.truncate(target, 1024 * 1024 * 1024)
tmp = tempfile.mkdtemp()
try:
    with Machine(extra_disks=[target]) as m:
        listed = m.run("install --list")
        check("install --list shows both disks, /root's as not usable", listed is not None and "/dev/vdb  1024 MiB" in listed
              and "/dev/vda" in listed and "can't install here" in listed, listed)
        refused = m.run("install --yes /dev/vda")
        check("it won't install over the disk /root is on", refused is not None and "/root is on" in refused, refused)
        # Without arguments it's a window (here over the console, as from Vela's star menu).
        before = os.path.join(tmp, "before.png")
        m.screenshot(before)
        m.run("install &")
        time.sleep(5)
        shot = os.path.join(os.environ.get("OUT", tmp), "install-window.png")
        m.screenshot(shot)
        _, _, rows_before = read_png(before)
        _, _, rows = read_png(shot)
        check("without arguments it opens a window", rows[400][640] != rows_before[400][640])
        m.run("kill %1")
        done = m.run("install --yes /dev/vdb", timeout=900)
        check("install --yes /dev/vdb finishes", done is not None and "is installed on /dev/vdb" in done, done)

    # From outside: the partition table, the home file system, the boot partition.
    table = subprocess.run(["sgdisk", "-p", target], capture_output=True, text=True).stdout
    parts = re.findall(r"^\s+(\d+)\s+(\d+)\s+(\d+)\s+.*?\s+(EF00|8300)\s+(.*)$", table, re.M)
    check("sgdisk reads a GPT with the EFI system partition and home", [p[3] for p in parts] == ["EF00", "8300"]
          and parts[1][4].strip() == "KernelProject2 home", table)
    verify = subprocess.run(["sgdisk", "-v", target], capture_output=True, text=True).stdout
    check("sgdisk finds no problems", "No problems found" in verify, verify)
    if len(parts) == 2:
        esp_first, home_first, home_last = int(parts[0][1]), int(parts[1][1]), int(parts[1][2])
        home = os.path.join(tmp, "home.img")
        with open(target, "rb") as src, open(home, "wb") as dst:
            src.seek(home_first * 512)
            left = (home_last - home_first + 1) * 512
            while left:
                chunk = src.read(min(left, 1 << 24))
                dst.write(chunk)
                left -= len(chunk)
        fsck = subprocess.run(["e2fsck", "-fn", home], capture_output=True, text=True)
        check("e2fsck finds the home file system clean", fsck.returncode == 0, fsck.stdout + fsck.stderr)
        image = f"{target}@@{esp_first * 512}"
        listing = subprocess.run(["mdir", "-i", image, "::/boot"], capture_output=True, text=True,
                                 env={**os.environ, "MTOOLS_SKIP_CHECK": "1"}).stdout
        check("the boot partition has the kernel and the boot archive", "kernel" in listing and "initramfs.cpio" in listing, listing)
        copied = os.path.join(tmp, "kernel")
        subprocess.run(["mcopy", "-i", image, "::/boot/kernel", copied], env={**os.environ, "MTOOLS_SKIP_CHECK": "1"})
        same = os.path.exists(copied) and open(copied, "rb").read() == open(os.path.join(ROOT, "out/iso/boot/kernel"), "rb").read()
        check("its kernel is the ISO's, byte for byte", same)

    # Boot it: UEFI firmware, the installed disk alone.
    with Machine(uefi_disk=target) as m:
        log = m.text()
        check("it boots from the disk with UEFI", "Started /sbin/init (pid 1)" in log, log[-2000:])
        check("/root is the home partition", "Mounted /root (ext2 on /dev/vda2)" in log, log[-2000:])
        m.run("echo kept > /root/note.txt")
    with Machine(uefi_disk=target) as m:
        note = m.run("cat /root/note.txt")
        check("what's saved in /root is still there after a reboot", note is not None and note.strip() == "kept", note)
finally:
    os.unlink(target)
    for name in os.listdir(tmp):
        os.unlink(os.path.join(tmp, name))
    os.rmdir(tmp)

print(f"{sum(results)}/{len(results)} passed")
sys.exit(0 if all(results) else 1)
