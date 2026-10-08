#!/usr/bin/env bash
# Checks that /root really lives on the disk: boots twice from the same (scratch) disk, saves files
# and a git repository in the first boot, finds them in the second, then checks the disk with e2fsck.
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
disk="$(mktemp --suffix=.img)"
trap 'rm -f "$disk"' EXIT
cp "$root/out/disk.img" "$disk"

boot() {
    (
        sleep 6
        for cmd in "$@"; do
            printf '%s\r' "$cmd"
            sleep 4
        done
    ) | timeout $((10 + $# * 4)) qemu-system-x86_64 -M q35 -m 1G -cdrom "$root/out/kernelproject2.iso" \
        -serial stdio -display none -no-reboot \
        -drive "file=$disk,if=none,id=home,format=raw" -device virtio-blk-pci,drive=home,disable-legacy=on 2>/dev/null |
        sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g' | grep -v '^\[kernel\]'
}

first="$(boot "echo remember me > ~/note.txt" \
    "mkdir ~/proj && cd ~/proj && git init -q && echo hi > f && git add f && git commit -qm first && git log --oneline")"
second="$(boot "cat ~/note.txt" "cd ~/proj && git log --oneline && git status --short && echo clean")"

failed=0
expect() {
    if grep -qF -- "$2" <<<"$1"; then
        printf '\033[32mok\033[0m    %s\n' "$3"
    else
        printf '\033[31mFAIL\033[0m  %s\n' "$3"
        failed=1
    fi
}
expect "$first" "first" "first boot: commit made"
expect "$second" "remember me" "second boot: the saved file is still there"
expect "$second" "(HEAD -> main) first" "second boot: the git history is still there"
expect "$second" "clean" "second boot: git sees a clean working tree"
if fsck="$(e2fsck -fn "$disk" 2>&1)"; then
    printf '\033[32mok\033[0m    e2fsck finds the disk consistent\n'
else
    printf '\033[31mFAIL\033[0m  e2fsck found problems:\n%s\n' "$fsck"
    failed=1
fi
if ((failed)); then
    printf -- '--- first boot ---\n%s\n--- second boot ---\n%s\n' "$first" "$second"
fi
exit $failed
