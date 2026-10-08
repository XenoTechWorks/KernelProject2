#!/usr/bin/env python3
"""Boots KernelProject2, runs each command after the previous one has finished, and checks that
its output contains the expected text. Prints ok/FAIL per check; exit status 1 if any failed.

    tests/smoke.py              all checks
    tests/smoke.py fastfetch    only checks whose command contains "fastfetch"
"""

import sys

from machine import Machine

# (command, text its output must contain)
CHECKS = [
    ("uname -a", "Linux kernelproject2"),
    ("ls /", "lib64"),
    ("hello-static one two", "argument 2: two"),
    ("hello-dynamic", "Hello from glibc"),
    ("echo piped | cat", "piped"),
    ("cat /etc/os-release > /tmp/x && cat /tmp/x", 'PRETTY_NAME="KernelProject2'),
    ("cd /tmp; mkdir -p a/b; ls a", "b"),
    ("rm -r /tmp/a; ls /tmp", "x"),
    ("nosuchcommand; echo status $?", "status 127"),
    ("false || echo fallback", "fallback"),
    ("cat /proc/meminfo", "MemTotal:"),
    ("ls /proc/self/", "cmdline"),
    ("fastfetch", "Kernel: Linux 6.12.0-kp2"),
    ("signal-test", "signals: 7/7 passed"),
    ("thread-test", "threads: 6/6 passed"),
    ("hello.exe one", "Hello from Windows! 2 argument(s)"),
    ("winapi.exe", "winapi: 9/9 passed"),
    ("winapi.exe > /dev/null; echo windows exit $?", "windows exit 0"),
    ("files.exe", "files: 16/16 passed"),
    ("threads.exe", "winthreads: 9/9 passed"),
    ("hello-ucrt.exe", "Hello from Windows!"),
    ("winapi-ucrt.exe", "winapi: 9/9 passed"),
    ("files-ucrt.exe", "files: 16/16 passed"),
    ("threads-ucrt.exe", "winthreads: 9/9 passed"),
    ("dlltest.exe", "dlls: 10/10 passed"),
    ("cpp.exe", "cpp: 7/7 passed"),
    ("cpp-dll.exe", "cpp: 7/7 passed"),
    ("mkdir /root/r && cd /root/r && git init -q && echo hi > f && git add f && git commit -qm one && git log --oneline", "one"),
]


def main():
    only = sys.argv[1] if len(sys.argv) > 1 else None
    checks = [c for c in CHECKS if only is None or only in c[0]]
    failed = 0
    with Machine() as m:
        for command, expect in checks:
            out = m.run(command)
            if out is not None and expect in out and "kernel panic" not in out:
                print(f"\033[32mok\033[0m    {command}")
            else:
                failed += 1
                why = "no prompt came back (hung or crashed)" if out is None else f'expected "{expect}"'
                print(f"\033[31mFAIL\033[0m  {command} ({why})")
                print("      " + (out or m.text()[-2000:]).replace("\n", "\n      "))
    print(f"{len(checks) - failed}/{len(checks)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
