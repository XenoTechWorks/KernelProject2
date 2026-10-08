# KernelProject2

A kernel in the Linux/POSIX style whose goal is to run Linux programs (fastfetch, git, ...) as they are,
without porting, and later Windows programs too.

The trick is the same one WSL1 used: the kernel speaks Linux's *system call* language. A Linux program
asks the kernel for things by number (`read` is 0, `write` is 1, `execve` is 59, ...), and if this
kernel answers those calls the way Linux does, the program can't tell the difference.

## What works today

- Boots with Limine on a 64-bit PC (tested in QEMU).
- Processes: `fork`, `execve`, `wait4`, `exit`, a scheduler with time slices, Ctrl+C.
- Memory: each program gets its own address space, plus `brk`, `mmap`, `munmap`, `mprotect` and a
  stack that grows on demand.
- Programs: static Linux binaries, static-PIE, and **dynamically linked** ones through their own
  `ld-linux-x86-64.so.2` and `libc.so.6`. `#!` scripts too.
- Files: an in-memory file system (like tmpfs) filled from a Linux-style initramfs (`cpio`), plus
  `/dev/null`, `/dev/zero`, `/dev/urandom` and `/dev/tty`. Pipes.
- Terminal: Linux's line discipline (editing, echo, Ctrl+C/D/U/W, raw mode via `termios`) on the
  screen and on the serial port. The screen understands the usual escape codes and 16 colors.
- About 90 Linux system calls (see `kernel/src/sys/mod.rs`). Missing ones return `ENOSYS` and are
  logged once on the serial port, which shows what a new program needs next.

The proof: `hello-static` and `hello-dynamic` are ordinary C programs built by gcc against glibc and
copied in unchanged, and they run.

## The command line

The same layout as Linux:

- `init` (pid 1) shows the welcome text, starts the shell, and starts a new one if it exits.
- `kpsh` is the shell (working name): pipes, `> >> < 2> 2>&1`, `; && ||`, quotes, `$VAR`, `$?`, `~`,
  `cd`/`pwd`/`export`/`unset`/`env`/`exit`/`help`, scripts and `kpsh -c`.
- Commands are separate programs, like coreutils: `ls` (with `-l -a -1`, colors, columns), `cat`,
  `echo`, `uname`, `mkdir -p`, `rm -r`, `touch`, `clear`.

They're written in Zig as ordinary Linux programs with no libc (`user/src`), so they also run on real
Linux. That keeps them honest: if one works on Linux but not here, the kernel is wrong.

## Layout

```
kernel/          the kernel (Rust)
  src/arch/        x86-64: GDT, interrupts, syscall entry, PIC/timer, serial
  src/mm/          physical pages, page tables, kernel heap, safe access to user memory
  src/fs/          the file tree, the initramfs reader, pipes
  src/proc/        processes, scheduler, fork/exec/wait, the ELF loader
  src/sys/         the Linux system calls
  src/tty.rs       terminal line discipline
  src/console.rs   text console on the screen
user/src/        init, the shell and the commands (Zig)
rootfs/          files copied into the root file system (/etc ...)
tests/           hello.c (the unchanged Linux program) and smoke.sh (boot test)
```

## Building and running

In Linux (WSL Ubuntu 24.04 works):

```sh
# once: tools
sudo apt install qemu-system-x86 xorriso cpio gcc
rustup toolchain install nightly --component rust-src --target x86_64-unknown-none
# Zig 0.17 in ~/tools/zig-x86_64-linux-0.17.0 (or set ZIG=/path/to/zig)
git clone --depth=1 --branch=v11.2.1-binary https://codeberg.org/Limine/Limine.git ~/tools/limine
make -C ~/tools/limine

./build.sh            # out/kernelproject2.iso
./run.sh              # boot it in a QEMU window (serial log in this terminal)
./run.sh --text       # no window: use the shell right here, over the serial port
tests/smoke.sh        # boot, type commands, check the answers
```

## Roadmap

Roughly in order; each step is checked by running a real program.

1. **More of Linux for real tools**: `/proc` (meminfo, cpuinfo, self/exe, per-process files),
   `/sys` basics, signal handlers (`rt_sigaction` + delivery + `rt_sigreturn`), `socketpair` and
   Unix sockets, `eventfd`, `epoll`.
2. **Threads**: `clone(CLONE_VM | CLONE_THREAD)`, real futexes, per-thread TLS. git and most modern
   programs use threads.
3. **Disks**: a virtio or AHCI driver and a real file system (ext2 first), so programs and files
   persist and don't all live in the boot archive.
4. **Run the targets**: fastfetch, then git, then BusyBox or coreutils as a compatibility test.
5. **Windows programs**: our own Windows layer (C++). A PE loader for `.exe` files and the Windows
   system DLLs (`kernel32`, `ntdll`, ...) implemented on top of the kernel, the same idea as Wine but
   built into the system.
6. Later: more CPUs (APIC/IOAPIC), networking, a nicer 8x16 font, a real login.

## Licenses of what's used

- [Limine](https://codeberg.org/Limine/Limine) (bootloader, BSD-2-Clause) is downloaded at build time.
- Rust crates: `limine`, `spin`, `linked_list_allocator`, `font8x8` (MIT/Apache-2.0).
- The test programs link against glibc (LGPL) from the build machine.
