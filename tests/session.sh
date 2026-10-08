#!/usr/bin/env bash
# Boots the ISO without a window, types each argument as a command, and prints everything the
# system says (kernel log included). For trying things out while developing.
#
#   tests/session.sh "ls /" "fastfetch"
#   WAIT=5 tests/session.sh "git status"     # seconds to wait after each command (default 3)
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
# A throwaway copy of the disk, so tests start from the same state and leave the real one alone.
disk=()
scratch=""
if [[ -f "$root/out/disk.img" ]]; then
    scratch="$(mktemp --suffix=.img)"
    cp "$root/out/disk.img" "$scratch"
    disk=(-drive "file=$scratch,if=none,id=home,format=raw" -device virtio-blk-pci,drive=home,disable-legacy=on)
fi
trap 'rm -f "$scratch"' EXIT
wait="${WAIT:-3}"
total=$((10 + $# * wait))
(
    sleep 6
    for cmd in "$@"; do
        printf '%s\r' "$cmd"
        sleep "$wait"
    done
) | timeout "$total" qemu-system-x86_64 -M q35 -m 1G -cdrom "$root/out/kernelproject2.iso" -serial stdio "${disk[@]}" \
    -display none -no-reboot 2>/dev/null | sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g'
