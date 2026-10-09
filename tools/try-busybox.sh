#!/bin/bash
# Builds winload and runs some BusyBox for Windows commands through it on Linux (development aid).
cd "$(dirname "$0")/.."
g++ -std=c++20 -O2 -static -fno-exceptions -fno-rtti -Wall -Wextra -o /tmp/winload windows/src/*.cpp 2>&1 | grep -E "error|warning" -A4 | head -30
cd ~/src/winapps
run() { echo "== $*"; timeout 10 /tmp/winload ./busybox64.exe "$@" 2>&1 | head -${LINES_EACH:-6}; [[ ${PIPESTATUS[0]} == 124 ]] && echo "(timed out)"; }
if [[ $# -gt 0 ]]; then run "$@"; exit; fi
run echo hello from busybox
run uname -a
run ls /
run cat /etc/os-release
run pwd
run wc -l /etc/os-release
run grep NAME /etc/os-release
run date
run sh -c "echo shell; echo works"
run uname -a
run sh -c "ls / | wc -l"
run sh -c "echo b a c | tr ' ' '\n' | sort"
run sed -n 2p /etc/os-release
run awk -F= "/^ID=/ {print \$2}" /etc/os-release
run find /etc -maxdepth 1 -name "os-*"
run md5sum /etc/os-release
run sh -c "for i in 1 2 3; do echo n=\$i; done"
run sh -c "x=\$(echo sub); echo got \$x"
