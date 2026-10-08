#!/usr/bin/env bash
# Boots out/kernelproject2.iso in QEMU. The serial port (kernel log + a second terminal) is this
# terminal; the screen opens in a QEMU window.
#
#   ./run.sh            window + serial here
#   ./run.sh --text     no window: the shell runs right in this terminal over the serial port
set -euo pipefail

root="$(cd "$(dirname "$0")" && pwd)"
display=(-display gtk)
if [[ "${1:-}" == "--text" ]]; then
    display=(-display none)
    shift
fi
exec qemu-system-x86_64 -M q35 -m 1G -cdrom "$root/out/kernelproject2.iso" \
    -serial stdio -no-reboot "${display[@]}" "$@"
