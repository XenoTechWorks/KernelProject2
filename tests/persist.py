#!/usr/bin/env python3
"""Checks that /root really lives on the disk: boots twice from the same (scratch) disk, saves a
file and a git repository in the first boot, finds them in the second, then checks the disk with
Linux's e2fsck."""

import os
import shutil
import subprocess
import sys
import tempfile

from machine import ROOT, Machine


def main():
    fd, disk = tempfile.mkstemp(suffix=".img")
    os.close(fd)
    shutil.copyfile(os.path.join(ROOT, "out/disk.img"), disk)
    results = []
    try:
        with Machine(disk=disk, scratch_disk=False) as m:
            m.run("echo remember me > ~/note.txt")
            out = m.run("mkdir ~/proj && cd ~/proj && git init -q && echo hi > f && git add f && git commit -qm first && git log --oneline")
            results.append(("first boot: commit made", out is not None and "first" in out))
        with Machine(disk=disk, scratch_disk=False) as m:
            note = m.run("cat ~/note.txt")
            log = m.run("cd ~/proj && git log --oneline && git status --short && echo clean")
            results.append(("second boot: the saved file is still there", note is not None and "remember me" in note))
            results.append(("second boot: the git history is still there", log is not None and "(HEAD -> main) first" in log))
            results.append(("second boot: git sees a clean working tree", log is not None and log.rstrip().endswith("clean")))
        fsck = subprocess.run(["e2fsck", "-fn", disk], capture_output=True, text=True)
        results.append(("e2fsck finds the disk consistent", fsck.returncode == 0))
    finally:
        os.unlink(disk)
    for name, ok in results:
        print(f"\033[{'32mok' if ok else '31mFAIL'}\033[0m    {name}")
    return 0 if all(ok for _, ok in results) else 1


if __name__ == "__main__":
    sys.exit(main())
