"""Drives KernelProject2 in QEMU over its serial port: boot, type a command, wait for the shell's
prompt to come back, and hand back exactly what that command printed."""

import os
import re
import select
import shutil
import subprocess
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]")
PROMPT = re.compile(rb"root:[^\r\n#]*# $")


class Machine:
    def __init__(self, disk=None, scratch_disk=True):
        """disk: a disk image to attach (default: out/disk.img when it exists). With scratch_disk,
        a copy is used, so tests don't change the real one."""
        self.output = b""
        self._scratch = None
        if disk is None and os.path.exists(os.path.join(ROOT, "out/disk.img")):
            disk = os.path.join(ROOT, "out/disk.img")
        if disk and scratch_disk:
            fd, self._scratch = tempfile.mkstemp(suffix=".img")
            os.close(fd)
            shutil.copyfile(disk, self._scratch)
            disk = self._scratch
        args = ["qemu-system-x86_64", "-M", "q35", "-m", "1G", "-cdrom", os.path.join(ROOT, "out/kernelproject2.iso"),
                "-serial", "stdio", "-display", "none", "-no-reboot", "-monitor", "none"]
        if disk:
            args += ["-drive", f"file={disk},if=none,id=home,format=raw",
                     "-device", "virtio-blk-pci,drive=home,disable-legacy=on"]
        self.qemu = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        self._wait_for_prompt(60)

    def _read(self, timeout):
        ready, _, _ = select.select([self.qemu.stdout], [], [], timeout)
        if not ready:
            return False
        chunk = os.read(self.qemu.stdout.fileno(), 65536)
        if not chunk:
            raise RuntimeError("QEMU exited:\n" + self.text())
        self.output += chunk
        return True

    def _wait_for_prompt(self, timeout, start=0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            clean = ANSI.sub(b"", self.output[start:]).replace(b"\r", b"")
            if PROMPT.search(clean):
                return True
            self._read(0.2)
        return False

    def run(self, command, timeout=60):
        """Types `command` and returns its output (without the echoed command and the next prompt).
        None if the prompt didn't come back in time."""
        start = len(self.output)
        self.qemu.stdin.write(command.encode() + b"\r")
        self.qemu.stdin.flush()
        # Wait for the echo of the command first, then for the prompt after its output.
        if not self._wait_for_prompt(timeout, start + len(command)):
            return None
        text = ANSI.sub(b"", self.output[start:]).replace(b"\r", b"").decode("utf-8", "replace")
        lines = text.split("\n")
        return "\n".join(lines[1:-1])  # drop the echoed command and the new prompt

    def text(self):
        return ANSI.sub(b"", self.output).replace(b"\r", b"").decode("utf-8", "replace")

    def screenshot(self, path):
        """Only with a monitor attached; kept simple for now."""
        raise NotImplementedError

    def close(self):
        if self.qemu.poll() is None:
            self.qemu.kill()
            self.qemu.wait()
        if self._scratch:
            os.unlink(self._scratch)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
