#!/usr/bin/env python3
"""Launch Neverball under strace in QEMU; save evidence even when it crashes.

    python3 tests/_dbg_gl.py [--single-threaded] [--seconds 180]

OUT selects the host artifact directory (default: out/neverball-debug).
"""

import argparse
import os
from pathlib import Path
import re
import time

from machine import Machine


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--single-threaded', action='store_true',
                        help='export LP_NUM_THREADS=0 to isolate llvmpipe worker issues')
    parser.add_argument('--seconds', type=float, default=180,
                        help='seconds to observe startup before collecting evidence')
    args = parser.parse_args()
    artifacts = Path(os.environ.get('OUT', 'out/neverball-debug'))
    artifacts.mkdir(parents=True, exist_ok=True)

    with Machine(software=True) as m:
        def run(command, timeout=60, show=True):
            print(f'$ {command}', flush=True)
            result = m.run(command, timeout=timeout)
            (artifacts / 'serial.log').write_text(m.text())
            if result is None:
                raise RuntimeError(f'Guest shell timed out: {command}; see {artifacts}/serial.log')
            if show:
                print(result, flush=True)
            return result

        installed = run('pkg get neverball', timeout=1800)
        if 'Installed neverball' not in installed and 'already installed' not in installed.lower():
            raise RuntimeError('Neverball installation failed')

        # Check both the packaged font and the game's relative symlink to it.
        for path in ('/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf',
                     '/usr/share/games/neverball/ttf/DejaVuSans-Bold.ttf'):
            run(f'cat {path} > /dev/null; echo FONT_EXIT=$?')
        run('ls /usr/bin/strace')
        if args.single_threaded:
            # kpsh does not parse NAME=value command syntax.
            run('export LP_NUM_THREADS=0')
        started = run('strace -f -o /tmp/neverball.trace neverball > /tmp/neverball.log 2>&1 &')
        job = re.search(r'\[(\d+)\] \d+', started)
        if not job:
            raise RuntimeError('The shell did not start the tracing job')
        time.sleep(max(0, args.seconds))
        jobs = run('jobs')  # Guest kpsh builtin; ps is not installed.
        (artifacts / 'jobs.log').write_text(jobs)
        m.screenshot(str(artifacts / 'neverball.png'))
        # Freeze a surviving job so cat reaches EOF even when the trace is growing.
        if re.search(rf'\[{job[1]}\]\s+Running', jobs):
            run(f'kill -STOP %{job[1]}')
        log = run('cat /tmp/neverball.log')
        (artifacts / 'neverball.log').write_text(log)
        trace = run('cat /tmp/neverball.trace', timeout=300, show=False)
        (artifacts / 'neverball.trace').write_text(trace)
        # Tail on the host: no dependency on a guest tail utility.
        print('\n'.join(trace.splitlines()[-80:]), flush=True)
        if not re.search(r'execve\("/usr/games/neverball".*= 0', trace):
            raise RuntimeError('Trace does not show a successful Neverball execve')
        print(f'Neverball evidence saved in {artifacts}', flush=True)


if __name__ == '__main__':
    main()
