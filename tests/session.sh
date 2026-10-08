#!/usr/bin/env bash
# Boots the ISO without a window, types each argument as a command, and prints everything the
# system says (kernel log included). For trying things out while developing.
#
#   tests/session.sh "ls /" "fastfetch"
#   WAIT=5 tests/session.sh "git status"     # seconds to wait after each command (default 3)
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
wait="${WAIT:-3}"
total=$((10 + $# * wait))
(
    sleep 6
    for cmd in "$@"; do
        printf '%s\r' "$cmd"
        sleep "$wait"
    done
) | timeout "$total" qemu-system-x86_64 -M q35 -m 1G -cdrom "$root/out/kernelproject2.iso" -serial stdio \
    -display none -no-reboot 2>/dev/null | sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g'
