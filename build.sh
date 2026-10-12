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
for program in init kpsh ls cat echo uname fileops kpfetch clock textclock widgets calc files edit textedit eyes gzip tar zip pkg users ping ifconfig host nc doom; do
    dest="$stage/bin/$program"
    [[ $program == init ]] && dest="$stage/sbin/init"
    "$zig" build-exe "$root/user/src/$program.zig" -target x86_64-linux-none -O ${ZIG_MODE:-ReleaseSmall} -fstrip \
        -fsingle-threaded --name "$program" -femit-bin="$dest" --cache-dir "$root/user/.zig-cache" \
        --global-cache-dir "$HOME/.cache/zig"
done
for name in mkdir rm touch clear; do ln -sf fileops "$stage/bin/$name"; done
for name in gunzip zcat; do ln -sf gzip "$stage/bin/$name"; done
ln -sf zip "$stage/bin/unzip"
for name in id whoami su login useradd passwd; do ln -sf users "$stage/bin/$name"; done
chmod 600 "$stage/etc/shadow"
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

# Real Windows programs downloaded as they are (see README: "Real Windows programs").
winapps="${WINAPPS:-$HOME/src/winapps}"
for app in busybox64.exe 7za.exe; do
    [[ -f "$winapps/$app" ]] && install -m 755 "$winapps/$app" "$stage/bin/$app"
done

# Unmodified Linux programs built from their own sources (see README: "Real Linux programs").
# git straight from Ubuntu, with the template files `git init` copies.
step "git (/usr/bin/git)"
add_host_program /usr/bin/git
mkdir -p "$stage/usr/share/git-core"
cp -r /usr/share/git-core/templates "$stage/usr/share/git-core/"

# curl straight from Ubuntu too: HTTP over our TCP, HTTPS through its own OpenSSL.
if [[ -x /usr/bin/curl ]]; then
    step "curl (/usr/bin/curl)"
    add_host_program /usr/bin/curl
fi

# Anything else you want to try, by name or path, as in:
#     EXTRA_PROGRAMS="jq bc sed" ./build.sh
# Each is copied in with the shared libraries it needs. A good way to find out whether a program
# runs here: if it does, nothing about it was changed to make it.
for extra in ${EXTRA_PROGRAMS:-}; do
    found="$(command -v "$extra" 2>/dev/null || true)"
    if [[ -n "$found" ]]; then
        step "$extra ($found)"
        add_host_program "$(readlink -f "$found")"
    else
        echo "  (no $extra on this machine, skipping)"
    fi
done

# Python needs its standard library beside the binary, so it gets a step of its own. It adds
# about 30 MiB to the image, so ask for it:  WITH_PYTHON=1 ./build.sh
if [[ -n "${WITH_PYTHON:-}" ]] && command -v python3 >/dev/null; then
    py_bin="$(readlink -f "$(command -v python3)")"
    py_lib="$(python3 -c 'import sysconfig; print(sysconfig.get_paths()["stdlib"])')"
    step "python3 ($py_bin)"
    add_host_program "$py_bin"
    ln -sf "$(basename "$py_bin")" "$stage/bin/python3"
    mkdir -p "$stage$(dirname "$py_lib")"
    # Everything but the test suite, which is a third of it and of no use here.
    rsync -a --exclude 'test' --exclude 'idlelib' --exclude '__pycache__' "$py_lib/" "$stage$py_lib/" 2>/dev/null ||
        cp -r "$py_lib" "$stage$(dirname "$py_lib")/"
fi

fastfetch="${FASTFETCH:-$HOME/src/fastfetch/build/fastfetch}"
if [[ -x "$fastfetch" ]]; then
    step "fastfetch ($fastfetch)"
    add_host_program "$fastfetch"
fi

# Chocolate Doom, straight from Ubuntu (apt install chocolate-doom freedoom), drawing through
# our own build of SDL 2 (sdl2/), which puts its windows on /dev/window. Its libraries are worked
# out with ours in place, so none of Ubuntu's SDL's X11 and Wayland ones come along.
if [[ -x /usr/games/chocolate-doom && -f /usr/share/games/doom/freedoom1.wad ]]; then
    step "Chocolate Doom (/usr/games/chocolate-doom), with SDL 2 for /dev/window"
    sdl="$(mktemp -d)"
    bash "$root/sdl2/build.sh" "$sdl"
    cp -L /usr/games/chocolate-doom "$stage/bin/"
    LD_LIBRARY_PATH="$sdl" ldd /usr/games/chocolate-doom | grep -o '/[^ ]*' | while read -r lib; do
        dest="$lib"
        [[ "$lib" == "$sdl"/* ]] && dest="/lib/x86_64-linux-gnu/$(basename "$lib")"
        mkdir -p "$stage$(dirname "$dest")"
        cp -L "$lib" "$stage$dest"
    done
    rm -rf "$sdl"
    # Freedoom: free game data in Doom's format (BSD license), so there's something to play.
    mkdir -p "$stage/usr/share/games/doom" "$stage/usr/share/doc/freedoom"
    cp /usr/share/games/doom/freedoom1.wad "$stage/usr/share/games/doom/"
    cp /usr/share/doc/freedoom/copyright "$stage/usr/share/doc/freedoom/"
fi

step "a sample package"
# A real .deb, built by dpkg-deb, for `pkg install` to chew on. Debian's tools default to zstd
# these days, which we don't unpack, so this one is made with gzip (dpkg reads it either way).
if command -v dpkg-deb >/dev/null; then
    pkgdir="$(mktemp -d)"
    mkdir -p "$pkgdir/DEBIAN" "$pkgdir/usr/bin" "$pkgdir/usr/share/doc/hello"
    cat > "$pkgdir/DEBIAN/control" <<'CONTROL'
Package: hello
Version: 1.0
Architecture: all
Maintainer: KernelProject2
Description: a sample package, to try `pkg install` with
CONTROL
    printf '#!/bin/kpsh\necho "Hello from a package!"\n' > "$pkgdir/usr/bin/hello"
    echo "Installed by pkg(1)." > "$pkgdir/usr/share/doc/hello/README"
    chmod 755 "$pkgdir/usr/bin/hello"
    mkdir -p "$stage/usr/share/examples"
    dpkg-deb --build -Zgzip "$pkgdir" "$stage/usr/share/examples/hello_1.0_all.deb" >/dev/null
    rm -rf "$pkgdir"
fi

step "certificates"
# The certificate authorities HTTPS checks servers against (what curl and OpenSSL look for).
if [[ -f /etc/ssl/certs/ca-certificates.crt ]]; then
    mkdir -p "$stage/etc/ssl/certs"
    cp /etc/ssl/certs/ca-certificates.crt "$stage/etc/ssl/certs/"
fi

step "fonts"
# Inter (SIL Open Font License) for graphical apps, downloaded once.
fonts="$HOME/.cache/kernelproject2/fonts"
mkdir -p "$fonts" "$stage/usr/share/fonts"
if [[ ! -s "$fonts/Inter.ttf" ]]; then
    curl -sSLf -o "$fonts/Inter.ttf" "https://github.com/google/fonts/raw/main/ofl/inter/Inter%5Bopsz%2Cwght%5D.ttf"
    curl -sSLf -o "$fonts/Inter-OFL.txt" "https://github.com/google/fonts/raw/main/ofl/inter/OFL.txt"
fi
# JetBrains Mono (SIL Open Font License) for the editors and other monospaced text.
if [[ ! -s "$fonts/JetBrainsMono.ttf" ]]; then
    curl -sSLf -o "$fonts/JetBrainsMono.ttf" "https://github.com/google/fonts/raw/main/ofl/jetbrainsmono/JetBrainsMono%5Bwght%5D.ttf"
    curl -sSLf -o "$fonts/JetBrainsMono-OFL.txt" "https://github.com/google/fonts/raw/main/ofl/jetbrainsmono/OFL.txt"
fi
cp "$fonts"/Inter.ttf "$fonts"/Inter-OFL.txt "$fonts"/JetBrainsMono.ttf "$fonts"/JetBrainsMono-OFL.txt "$stage/usr/share/fonts/"

step "boot archive"
rm -f "$out/initramfs.cpio"
chmod 755 "$stage"/bin/* "$stage"/sbin/*
# su, login and passwd read /etc/shadow, which only root may: set-user-id lets them (after the
# blanket chmod above, which would wipe the bit).
chmod 4755 "$stage/bin/users"
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
