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
        # QEMU's monitor on a socket, for screenshots.
        self._monitor = os.path.join(tempfile.gettempdir(), f"kp2-monitor-{os.getpid()}-{id(self)}")
        args = ["qemu-system-x86_64", "-M", "q35", "-m", "1G", "-cdrom", os.path.join(ROOT, "out/kernelproject2.iso"),
                "-serial", "stdio", "-display", "none", "-no-reboot",
                "-monitor", f"unix:{self._monitor},server,nowait",
                "-qmp", f"unix:{self._monitor}-qmp,server,nowait"]
        if disk:
            args += ["-drive", f"file={disk},if=none,id=home,format=raw",
                     "-device", "virtio-blk-pci,drive=home,disable-legacy=on"]
        args += ["-device", "virtio-tablet-pci,disable-legacy=on"]
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

    def monitor(self, command, wait=0.3):
        """Sends a command to QEMU's monitor."""
        import socket
        with socket.socket(socket.AF_UNIX) as s:
            s.connect(self._monitor)
            s.sendall(command.encode() + b"\n")
            time.sleep(wait)

    def screenshot(self, path):
        """Saves what's on the screen right now as a PNG."""
        self.monitor(f"screendump {os.path.abspath(path)} -f png", wait=1)

    def tablet(self, x, y, buttons=None, screen=(1280, 800)):
        """Puts the (virtio) tablet's pointer at screen pixel (x, y); then presses (1) or releases
        (0) the left button, if given. Uses QMP, which can send absolute pointer events."""
        import json
        import socket
        events = [{"type": "abs", "data": {"axis": "x", "value": x * 32768 // screen[0]}},
                  {"type": "abs", "data": {"axis": "y", "value": y * 32768 // screen[1]}}]
        if buttons is not None:
            events.append({"type": "btn", "data": {"down": bool(buttons), "button": "left"}})
        with socket.socket(socket.AF_UNIX) as s:
            s.connect(self._monitor + "-qmp")
            s.sendall(json.dumps({"execute": "qmp_capabilities"}).encode() + b"\n")
            s.sendall(json.dumps({"execute": "input-send-event", "arguments": {"events": events}}).encode() + b"\n")
            time.sleep(0.3)

    def mouse(self, dx=0, dy=0, buttons=None):
        """Moves the (PS/2) mouse by dx, dy pixels; then sets the buttons held, if given (1 left)."""
        # In steps like a real mouse's: QEMU may drop the rest of one big jump (a PS/2 packet
        # holds at most 127 pixels each way).
        while dx or dy:
            sx, sy = max(-100, min(100, dx)), max(-100, min(100, dy))
            self.monitor(f"mouse_move {sx} {sy}", wait=0.05)
            dx, dy = dx - sx, dy - sy
        time.sleep(0.25)
        if buttons is not None:
            self.monitor(f"mouse_button {buttons}")

    def type(self, text):
        """Sends keys without waiting for anything (for interactive programs)."""
        self.qemu.stdin.write(text.encode())
        self.qemu.stdin.flush()

    def close(self):
        if self.qemu.poll() is None:
            self.qemu.kill()
            self.qemu.wait()
        if self._scratch:
            os.unlink(self._scratch)
        for path in (self._monitor, self._monitor + "-qmp"):
            if os.path.exists(path):
                os.unlink(path)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
