#!/usr/bin/env bash
# Boots out/kernelproject2.iso in QEMU. The serial port (kernel log + a second terminal) is this
# terminal; the screen opens in a QEMU window.
#
#   ./run.sh                     window + serial here, straight to the desktop (Vela)
#   ./run.sh --text              no window, no desktop: the shell runs right here over the serial port
#   ./run.sh --installed FILE    boot a disk `install` put the system on (UEFI firmware, no ISO)
#   CPUS=2 ./run.sh              how many CPUs (default 4)
# With out/software built (python3 software/build.py), it also serves that to Software.
set -euo pipefail

root="$(cd "$(dirname "$0")" && pwd)"
display=(-display gtk)
boot=(-cdrom "$root/out/kernelproject2.iso")
disk=()
if [[ -f "$root/out/disk.img" ]]; then
    disk=(-drive "file=$root/out/disk.img,if=none,id=home,format=raw" -device virtio-blk-pci,drive=home,disable-legacy=on)
fi
while [[ $# -gt 0 ]]; do
    case "$1" in
        --text)
            display=(-display none -fw_cfg name=opt/kernelproject2/cmdline,string=text)
            shift ;;
        --installed)
            boot=(-bios /usr/share/ovmf/OVMF.fd)
            disk=(-drive "file=$2,if=none,id=home,format=raw" -device virtio-blk-pci,drive=home,disable-legacy=on)
            shift 2 ;;
        *) break ;;
    esac
done
# Software's packages (software/build.py), served where the guest looks for them: port 8086 here,
# which QEMU's user network shows the guest as 10.0.2.2:8086. Stopped again with QEMU.
if [[ -f "$root/out/software/index" ]] && ! (exec 3<>/dev/tcp/127.0.0.1/8086) 2>/dev/null; then
    python3 -m http.server 8086 --bind 127.0.0.1 --directory "$root/out/software" >/dev/null 2>&1 &
    server=$!
    trap 'kill $server 2>/dev/null' EXIT
fi
qemu-system-x86_64 -M q35 -m 1G -smp "${CPUS:-4}" "${boot[@]}" \
    -device virtio-tablet-pci,disable-legacy=on \
    -netdev user,id=n0 -device virtio-net-pci,netdev=n0,disable-legacy=on \
    -serial stdio -no-reboot "${display[@]}" "${disk[@]}" "$@"
