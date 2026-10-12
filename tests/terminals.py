#!/usr/bin/env python3
"""Programs that run others in a pseudo-terminal, from Ubuntu, unchanged: script records a
command's session; tmux keeps a session in the background, takes keys for it, shows what's on
its screen, and is attached to and detached from on the console.

    python3 tests/terminals.py
"""

import re
import sys
import time

from machine import ANSI, Machine

results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + ("" if ok else f"\n      {detail!r}"))


def wait_for(m, pattern, start, timeout=20):
    """Waits until the guest has printed something matching `pattern` since `start`."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        text = ANSI.sub(b"", m.output[start:]).replace(b"\r", b"").decode("utf-8", "replace")
        if re.search(pattern, text):
            return True
        m.drain(0.2)
    return False


with Machine() as m:
    out = m.run("script -q -c 'echo inside script' /tmp/typescript", timeout=30) or ""
    log = m.run("cat /tmp/typescript") or ""
    check("script runs a command in a terminal and records it",
          "inside script" in out and "Script started" in log and "inside script" in log, (out, log))

    m.run("tmux new-session -d -s work", timeout=30)
    sessions = m.run("tmux list-sessions") or ""
    check("tmux starts a session in the background", sessions.startswith("work: 1 windows"), sessions)

    m.run("tmux send-keys -t work 'echo typed into a pane > /tmp/pane' Enter")
    m.run("true")  # a moment for the pane's shell
    written = m.run("cat /tmp/pane") or ""
    screen = m.run("tmux capture-pane -p -t work") or ""
    check("keys sent to it run in its shell, and capture-pane shows its screen",
          written.strip() == "typed into a pane" and "echo typed into a pane" in screen, (written, screen))

    # Attach on the console: tmux takes over the screen (its status line names the session), and
    # Ctrl+B d gives it back.
    m.drain()
    start = len(m.output)
    m.type("tmux attach -t work\r")
    attached = wait_for(m, r"\[work\]", start)
    start = len(m.output)
    m.type("\x02d")
    detached = wait_for(m, r"detached \(from session work\)", start)
    check("attach shows the session on the console, Ctrl+B d leaves it running", attached and detached,
          m.text()[-400:])

    # Job control in the shell: Ctrl+Z stops what runs in front, jobs lists it, bg lets it go on
    # (a background job that reads the terminal is stopped again: SIGTTIN), fg brings it back.
    m.drain()
    start = len(m.output)
    m.type("cat\r")
    time.sleep(1)
    m.type("\x1a")
    stopped = wait_for(m, r"\[1\]\+\s+Stopped\s+cat", start)
    listed = m.run("jobs") or ""
    resumed = m.run("bg") or ""
    m.run("true")
    again = m.run("jobs") or ""
    m.drain()
    start = len(m.output)
    m.type("fg\r")
    time.sleep(1)
    m.type("back in front\r")
    echoed = wait_for(m, r"back in front\nback in front", start)
    m.type("\x04")
    finished = wait_for(m, r"root:[^\n]*# $", start)
    check("Ctrl+Z stops a job, jobs lists it, bg lets it go on until it reads the terminal",
          stopped and "Stopped" in listed and "cat &" in resumed and "Stopped" in again, (listed, resumed, again))
    check("fg brings it back to the terminal, where it carries on", echoed and finished, m.text()[-300:])

    m.run("tmux kill-server")
    gone = m.run("tmux list-sessions 2>&1") or ""
    check("kill-server ends it", "no server running" in gone, gone)

failed = results.count(False)
print(f"\n{len(results) - failed} of {len(results)} passed")
sys.exit(1 if failed else 0)
