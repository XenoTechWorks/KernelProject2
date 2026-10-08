#!/usr/bin/env bash
# Boots the ISO in QEMU without a window, types commands over the serial port, and checks the output.
#
#   tests/smoke.sh                  run the checks
#   tests/smoke.sh --screenshot     also save the screen to out/screen.png
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT

# command typed => text that must appear in the output
checks=(
    "uname -a => Linux kernelproject2"
    "ls / => lib64"
    "hello-static one two => argument 2: two"
    "hello-dynamic => Hello from glibc"
    "echo piped | cat => piped"
    "cat /etc/os-release > /tmp/x && cat /tmp/x => PRETTY_NAME=\"KernelProject2"
    "cd /tmp; mkdir -p a/b; ls a => b"
    "rm -r /tmp/a; ls /tmp => x"
    "nosuchcommand; echo status \$? => status 127"
    "false || echo fallback => fallback"
    "cat /proc/meminfo => MemTotal:"
    "ls /proc/self/ => cmdline"
    "fastfetch => Kernel: Linux 6.12.0-kp2"
)

screenshot=0
[[ "${1:-}" == "--screenshot" ]] && screenshot=1

(
    sleep 6
    for check in "${checks[@]}"; do
        printf '%s\r' "${check%% => *}"
        sleep 1.5
    done
    if ((screenshot)); then
        exec 3<>/dev/tcp/127.0.0.1/4555
        printf 'screendump %s -f png\n' "$root/out/screen.png" >&3
        sleep 2
    fi
) | timeout 40 qemu-system-x86_64 -M q35 -m 1G -cdrom "$root/out/kernelproject2.iso" -serial stdio \
    -display none -no-reboot -monitor tcp:127.0.0.1:4555,server,nowait 2>/dev/null |
    sed 's/\x1b\[[0-9;]*m//g' >"$log"

failed=0
for check in "${checks[@]}"; do
    expect="${check#* => }"
    # Ignore the prompt lines, which echo the typed command itself.
    if grep -v "^root:" "$log" | grep -qF -- "$expect"; then
        printf '\033[32mok\033[0m    %s\n' "${check%% => *}"
    else
        printf '\033[31mFAIL\033[0m  %s (expected "%s")\n' "${check%% => *}" "$expect"
        failed=1
    fi
done
if grep -q "kernel panic" "$log"; then
    echo "kernel panic:"
    grep -A20 "kernel panic" "$log"
    failed=1
fi
if ((failed)); then
    echo "--- serial output ---"
    cat "$log"
fi
exit $failed
