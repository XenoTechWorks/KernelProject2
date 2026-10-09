#!/usr/bin/env bash
# Builds everything into out/kernelproject2.iso. Run inside Linux (WSL Ubuntu 24.04 works).
#
#   ./build.sh          build the kernel, the programs, the boot archive and the ISO
#
# Needs: Rust nightly (rustup), Zig 0.17, gcc, cpio, xorriso, and Limine's binaries (see README).
set -euo pipefail

root="$(cd "$(dirname "$0")" && pwd)"
out="$root/out"
# The root filesystem is put together on a Linux filesystem: a Windows drive (/mnt/c in WSL) can't
# keep Unix permissions, and every file would end up rwxrwxrwx.
stage="${KP2_STAGE:-$HOME/.cache/kernelproject2/root}"
zig="${ZIG:-$HOME/tools/zig-x86_64-linux-0.17.0/zig}"
limine="${LIMINE_DIR:-$HOME/tools/limine}"

step() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }

step "kernel"
(cd "$root/kernel" && cargo build --release --quiet)
kernel="$root/kernel/target/x86_64-unknown-none/release/kernel"

step "programs"
rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/sbin" "$out"
cp -r "$root/rootfs/." "$stage/"
find "$stage" -type d -exec chmod 755 {} +
find "$stage" -type f -exec chmod 644 {} +
for program in init kpsh ls cat echo uname fileops kpfetch; do
    dest="$stage/bin/$program"
    [[ $program == init ]] && dest="$stage/sbin/init"
    "$zig" build-exe "$root/user/src/$program.zig" -target x86_64-linux-none -O ReleaseSmall -fstrip \
        -fsingle-threaded --name "$program" -femit-bin="$dest" --cache-dir "$root/user/.zig-cache" \
        --global-cache-dir "$HOME/.cache/zig"
done
for name in mkdir rm touch clear; do ln -sf fileops "$stage/bin/$name"; done
ln -sf kpsh "$stage/bin/sh"

step "Linux test programs (built with gcc, copied in unchanged)"
gcc -O2 -static -o "$stage/bin/hello-static" "$root/tests/hello.c"
gcc -O2 -o "$stage/bin/hello-dynamic" "$root/tests/hello.c"
# Copies a Linux program from this machine, unchanged, with the shared libraries it needs.
add_host_program() {
    local program="$1"
    [[ "$(dirname "$program")" == "$stage/bin" ]] || cp -L "$program" "$stage/bin/"
    ldd "$program" | grep -o '/[^ ]*' | while read -r lib; do
        mkdir -p "$stage$(dirname "$lib")"
        cp -L "$lib" "$stage$lib"
    done
}
add_host_program "$stage/bin/hello-dynamic"
gcc -O2 -o "$stage/bin/signal-test" "$root/tests/signals.c"
gcc -O2 -pthread -o "$stage/bin/thread-test" "$root/tests/threads.c"

step "Windows layer (winload) and Windows test programs"
g++ -std=c++20 -O2 -static -fno-exceptions -fno-rtti -Wall -Wextra -o "$stage/bin/winload" "$root"/windows/src/*.cpp
# Each test twice: against msvcrt.dll (mingw's default) and against the UCRT (ucrtbase.dll, what
# Visual Studio uses). This GCC has no -mcrtdll, so a specs file swaps the runtime library.
ucrt_specs="$HOME/.cache/kernelproject2/ucrt.specs"
mkdir -p "$(dirname "$ucrt_specs")"
x86_64-w64-mingw32-gcc -dumpspecs | sed 's/-lmsvcrt/-lucrt/g' > "$ucrt_specs"
for program in hello winapi files threads; do
    x86_64-w64-mingw32-gcc -O2 -o "$stage/bin/$program.exe" "$root/tests/windows/$program.c"
    x86_64-w64-mingw32-gcc -O2 -specs="$ucrt_specs" -D_UCRT -o "$stage/bin/$program-ucrt.exe" "$root/tests/windows/$program.c"
done
# A DLL and a program that uses it (linked, and through LoadLibrary).
x86_64-w64-mingw32-gcc -O2 -shared -o "$stage/bin/mathlib.dll" -Wl,--out-implib,"$out/libmathlib.a" "$root/tests/windows/mathlib.c"
x86_64-w64-mingw32-gcc -O2 -o "$stage/bin/dlltest.exe" "$root/tests/windows/dlltest.c" -L"$out" -lmathlib
# C++ (iostream, containers, exceptions): once with the C++ runtime built in, once using mingw's own
# libstdc++-6.dll and libgcc_s_seh-1.dll, copied in unchanged.
x86_64-w64-mingw32-g++ -O2 -static-libgcc -static-libstdc++ -o "$stage/bin/cpp.exe" "$root/tests/windows/cpp.cpp"
x86_64-w64-mingw32-g++ -O2 -o "$stage/bin/cpp-dll.exe" "$root/tests/windows/cpp.cpp"
mingw_dlls="$(dirname "$(x86_64-w64-mingw32-g++ -print-file-name=libstdc++-6.dll)")"
cp "$mingw_dlls/libstdc++-6.dll" "$mingw_dlls/libgcc_s_seh-1.dll" "$stage/bin/"

# Unmodified Linux programs built from their own sources (see README: "Real Linux programs").
# git straight from Ubuntu, with the template files `git init` copies.
step "git (/usr/bin/git)"
add_host_program /usr/bin/git
mkdir -p "$stage/usr/share/git-core"
cp -r /usr/share/git-core/templates "$stage/usr/share/git-core/"

fastfetch="${FASTFETCH:-$HOME/src/fastfetch/build/fastfetch}"
if [[ -x "$fastfetch" ]]; then
    step "fastfetch ($fastfetch)"
    add_host_program "$fastfetch"
fi

step "boot archive"
rm -f "$out/initramfs.cpio"
chmod 755 "$stage"/bin/* "$stage"/sbin/*
chmod 700 "$stage/root"
chmod 1777 "$stage/tmp" 2>/dev/null || true
(cd "$stage" && find . -mindepth 1 | LC_ALL=C sort | cpio --quiet -o -H newc -R 0:0 > "$out/initramfs.cpio")

step "disk"
# The disk keeps what's saved in /root between boots, so it's only made when missing
# (delete out/disk.img to start over).
if [[ ! -f "$out/disk.img" ]]; then
    mkfs.ext2 -q -L home -d "$root/disk" "$out/disk.img" 256M
    echo "    made out/disk.img (256 MiB ext2)"
fi

step "ISO"
iso="$out/iso"
rm -rf "$iso"
mkdir -p "$iso/boot/limine" "$iso/EFI/BOOT"
cp "$kernel" "$iso/boot/kernel"
cp "$out/initramfs.cpio" "$iso/boot/initramfs.cpio"
cp "$root/limine.conf" "$limine/limine-bios.sys" "$limine/limine-bios-cd.bin" "$limine/limine-uefi-cd.bin" "$iso/boot/limine/"
cp "$limine/BOOTX64.EFI" "$iso/EFI/BOOT/"
xorriso -as mkisofs -quiet -R -r -J -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 \
    -boot-info-table -hfsplus -apm-block-size 2048 --efi-boot boot/limine/limine-uefi-cd.bin \
    -efi-boot-part --efi-boot-image --protective-msdos-label "$iso" -o "$out/kernelproject2.iso" 2>/dev/null
"$limine/limine" bios-install "$out/kernelproject2.iso" 2>/dev/null

step "done: $out/kernelproject2.iso ($(du -h "$out/kernelproject2.iso" | cut -f1))"
