# KernelProject2

A kernel in the Linux/POSIX style whose goal is to run Linux programs (fastfetch, git, ...) as they are,
without porting, and later Windows programs too.

The trick is the same one WSL1 used: the kernel speaks Linux's *system call* language. A Linux program
asks the kernel for things by number (`read` is 0, `write` is 1, `execve` is 59, ...), and if this
kernel answers those calls the way Linux does, the program can't tell the difference.

## What works today

- Boots with Limine on a 64-bit PC (tested in QEMU).
- Processes and threads: `fork`, `clone` (threads share memory, files and signal handlers, as on
  Linux), `execve`, `wait4`, `exit`/`exit_group`, futexes, and a scheduler with time slices.
- Signals the Linux way: handlers run on Linux's `rt_sigframe` and return through `rt_sigreturn`,
  with masks, pending signals, `SA_RESTART`, `SIGCHLD`, and faults (`SIGSEGV` ...) a program can catch.
- Memory: each program gets its own address space, plus `brk`, `mmap`, `munmap`, `mprotect` and a
  stack that grows on demand.
- Programs: static Linux binaries, static-PIE, and **dynamically linked** ones through their own
  `ld-linux-x86-64.so.2` and `libc.so.6`. `#!` scripts too.
- Files: a VFS layer like Linux's, with an in-memory file system (like tmpfs) filled from a
  Linux-style initramfs (`cpio`), **ext2 on a virtio disk** (read and write, mounted at `/root`, so
  your home folder survives reboots), `/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/tty` and pipes.
  The disk is made by the real `mkfs.ext2`, and Linux's `e2fsck` checks it stays consistent.
- Terminal: Linux's line discipline (editing, echo, Ctrl+C/D/U/W, raw mode via `termios`) on the
  screen and on the serial port. The screen understands the usual escape codes and 16 colors.
- About 90 Linux system calls (see `kernel/src/sys/mod.rs`). Missing ones return `ENOSYS` and are
  logged once on the serial port, which shows what a new program needs next.

- `/proc` (meminfo, cpuinfo, uptime, stat, mounts, and each process's stat, status, cmdline, exe, fd)
  and the parts of `/sys` programs read today, including the machine's real identity from SMBIOS.
- The screen uses the Spleen 8x16 font with UTF-8, 160x50 characters at 1280x800.

The proof: `hello-static` and `hello-dynamic` are ordinary C programs built by gcc against glibc and
copied in unchanged, and they run. So do **fastfetch** (built from its own unchanged source) and
**git** (Ubuntu's own `/usr/bin/git`): init, add, commit, branches, merges, diff, stash, log --graph.
`signal-test` and `thread-test` check signals and glibc pthreads against the same results as on Linux.

## Real Linux programs

Programs built on the build machine can be copied in unchanged; `build.sh` brings their shared
libraries along (`add_host_program`). fastfetch is picked up automatically when it's built here:

```sh
sudo apt install cmake make gcc g++
git clone --depth=1 --branch=2.69.0 https://github.com/fastfetch-cli/fastfetch.git ~/src/fastfetch
cmake -S ~/src/fastfetch -B ~/src/fastfetch/build -DCMAKE_BUILD_TYPE=Release -DBUILD_TESTS=OFF
cmake --build ~/src/fastfetch/build -j8
./build.sh
```

## Windows programs

`hello.exe`, `winapi.exe`, `files.exe` and `threads.exe` are real Windows programs, built with
mingw-w64 (the GCC Windows toolchain), and they run when you type their names. They cover the C
runtime, files and directories (`CreateFile`, `FindFirstFile`, ...), threads (`CreateThread`,
events, mutexes, critical sections, waiting) and thread-local storage. Each is built twice: against
`msvcrt.dll` and against the UCRT (`ucrtbase.dll` and its `api-ms-win-crt-*` names), the runtime
Visual Studio programs use; both run. `dlltest.exe` uses its own DLL (`mathlib.dll`), both linked at build
time and through `LoadLibrary`/`GetProcAddress`. `cpp.exe` is C++ (iostream, containers, exceptions thrown
through several functions with destructors running), and `cpp-dll.exe` is the same program using
mingw's own `libstdc++-6.dll` and `libgcc_s_seh-1.dll`. The Windows layer (`windows/`, C++) works like
Windows' own loader plus our own versions of the system DLLs, the same idea as Wine:

- the kernel sees a file starting with `MZ` and runs it through `/bin/winload`, the way it runs a
  `#!` script through its interpreter;
- `winload` maps the `.exe` and the DLLs it uses (found next to it, as on Windows), applies
  relocations, links imports to the program's own DLLs or to our built-in `kernel32.dll`,
  `msvcrt.dll` and `ucrtbase.dll` (written in C++ with the Microsoft calling convention), sets up
  thread-local storage per image, the TEB and PEB (through the `gs` register, as on Windows), runs
  each DLL's `DllMain`, and jumps to the entry point;
- exceptions work the Windows x64 way (`seh.cpp`): `RaiseException` walks the stack with each
  function's unwind tables (`.pdata`/`.xdata`) calling exception handlers, and `RtlUnwindEx` unwinds
  to the catching frame. C++ `throw` and C `__try`/`__except` are built on it.
  `WINLOAD_SEH_DEBUG=1` prints every frame the walks visit;
- a function a program needs but we don't have yet stops it with a message naming the function.

`winload` is an ordinary Linux program, so it runs on Linux (WSL) too: handy for testing.

### Real Windows programs

[BusyBox for Windows](https://frippery.org/busybox/) (`busybox64.exe`, downloaded unchanged) runs
here: its shell with pipelines, loops and `$(...)`, and its tools (`ls`, `grep`, `sed`, `awk`,
`find`, `md5sum`, ...). Put it in `~/src/winapps/` (or set `WINAPPS`) and `build.sh` copies it into
`/bin`. It needed child processes (`CreateProcess`, `_spawnve`, pipes, a `.exe` started from
a `.exe` goes back through `winload`), file mappings, console modes and colors, and many smaller
`kernel32`/`msvcrt`/`advapi32` functions. System DLLs we don't write ourselves (`user32`, `shell32`,
...) load as empty ones, so a program only stops if it actually calls something missing.

Tools for this: `tools/winimports.sh program.exe` lists what a program imports, and
`WINLOAD_TRACE=1` prints every call it makes into our DLLs.

## Editing text

`edit FILE` is a text editor for the terminal, in the spirit of nano: arrows, Home/End and Page
Up/Down move, Ctrl+S saves, Ctrl+Q quits (twice with unsaved changes), Ctrl+F finds, Ctrl+G goes to
a line, Ctrl+Z undoes (a typed word at a time), Ctrl+K cuts a line, Ctrl+C copies one and Ctrl+U
pastes. `textedit` is the same editor in a window, with selection by dragging or shift and the
arrows, double-click to pick a word, and Ctrl+C/X/V/Z/A.

Both keep the text in `user/src/lib/textbuf.zig`, and both share one clipboard: `/dev/clipboard`.
Anything can use it, so text moves between the terminal, the windows and the shell:

```sh
echo -n hello > /dev/clipboard   # then Ctrl+U in edit, or Ctrl+V in textedit
cat /dev/clipboard               # what a window copied
```

## The command line

The same layout as Linux:

- `init` (pid 1) shows the welcome text, starts the shell, and starts a new one if it exits.
- `kpsh` is the shell (working name): pipes, `> >> < 2> 2>&1`, `; && ||`, quotes, `$VAR`, `$?`, `~`,
  `cd`/`pwd`/`export`/`unset`/`env`/`exit`/`help`, scripts and `kpsh -c`.
- Commands are separate programs, like coreutils: `ls` (with `-l -a -1`, colors, columns), `cat`,
  `echo`, `uname`, `mkdir -p`, `rm -r`, `touch`, `clear`.

They're written in Zig as ordinary Linux programs with no libc (`user/src`), so they also run on real
Linux. That keeps them honest: if one works on Linux but not here, the kernel is wrong.

## Apps in windows

Programs can open windows that float over the console, with a header, a close button and a soft
shadow. It isn't a desktop: the command line stays the whole screen, and windows sit on top of it.

```sh
clock &            # the time, big
calc &             # a calculator (click, or type: 7*6 Enter)
files /etc &       # browse folders, peek into files
textedit notes &   # a text editor
widgets &          # a tour of the widgets
textclock &        # a text app in a window
```

The shell keeps the keyboard. Drag a window by its header with the mouse, resize it by any edge or
corner, scroll with the wheel. `run.sh` gives QEMU a virtio tablet, so the pointer
sits exactly under your mouse; without one the PS/2 mouse works too (click into the QEMU window to
use it, Ctrl+Alt+G lets go), but QEMU's idea of where it is can drift from yours. Click a window to give it the keyboard, or press F12 to move the keyboard between
the console and the windows. × closes a window: its program reads end-of-file and exits.
- The kernel composites them (`kernel/src/console.rs`): the console is a layer of pixels, each
  window an image with its frame and shadow, blended together where something changed.

For programs it's a file: opening `/dev/window` makes a window.

- **Graphical**: the `WINDOW_PIXELS` ioctl makes the window a picture of a given size;
  `WINDOW_BLIT` copies a rectangle of the program's pixels into it, and `WINDOW_TITLE` names it.
  Reading returns 16-byte events: pointer motion, button presses and releases, the wheel, keys
  (with codes for arrows and the like, and shift/ctrl), focus, the pointer leaving, and a new size
  when the user resizes the window.
- **Text**: without that ioctl, what the program writes is drawn as a terminal (escape sequences
  work); xterm's title (`ESC ] 2 ; title BEL`) and resize (`ESC [ 8 ; rows ; cols t`) sequences set
  its title and size, and mouse clicks come back as xterm reports once turned on (`ESC [ ? 1000 h`).

The graphical toolkit (`user/src/lib/gui.zig`, Zig, a working name) is our own, in the spirit of GTK:

- `gui/font.zig` reads TrueType fonts and draws smooth (anti-aliased) text; apps use
  [Inter](https://rsms.me/inter/) (SIL Open Font License), downloaded at build time;
- `gui/canvas.zig` draws rounded rectangles, circles and text into a picture;
- `gui/widgets.zig` has the widgets (labels, buttons, text entries, switches, checkboxes,
  sliders, progress bars, lists, separators, and a text area for many lines) in vertical and
  horizontal boxes that share out the room as the window is resized, with hover, press, keyboard
  focus (Tab), selection and the clipboard, timers and a dark theme like GNOME's.
  `widgets`, `clock`, `calc`, `files` and `textedit` are the examples. Monospaced text uses
  [JetBrains Mono](https://www.jetbrains.com/lp/mono/) (SIL Open Font License).
- `user/src/lib/ui.zig` is the smaller helper for text windows (`textclock`).

## Layout

```
kernel/          the kernel (Rust)
  src/arch/        x86-64: GDT, interrupts, syscall entry, PIC/timer, serial
  src/mm/          physical pages, page tables, kernel heap, safe access to user memory
  src/fs/          the VFS, ext2, /proc, /sys, the initramfs reader, pipes
  src/drivers/     PCI, virtio (the disk, and the tablet for the pointer)
  src/proc/        processes, scheduler, fork/exec/wait, the ELF loader
  src/sys/         the Linux system calls
  src/tty.rs       terminal line discipline
  src/term.rs      a terminal: character cells and escape sequences
  src/console.rs   the screen: the console and windows over it, composited; the pointer
  src/keyboard.rs, src/mouse.rs   PS/2 keyboard and mouse
user/src/        init, the shell, the commands and apps (Zig); lib/gui is the toolkit
windows/src/     the Windows layer: PE loader, kernel32, msvcrt (C++)
rootfs/          files copied into the root file system (/etc ...)
disk/            what a new disk starts with (out/disk.img is made from it once)
tests/           test programs (C, and Windows ones in windows/) and the test scripts:
                 smoke.py, persist.py, windows.py, edit.py, textedit.py, session.sh
```

## Building and running

In Linux (WSL Ubuntu 24.04 works):

```sh
# once: tools
sudo apt install qemu-system-x86 xorriso cpio gcc g++ gcc-mingw-w64-x86-64
rustup toolchain install nightly --component rust-src --target x86_64-unknown-none
# Zig 0.17 in ~/tools/zig-x86_64-linux-0.17.0 (or set ZIG=/path/to/zig)
git clone --depth=1 --branch=v11.2.1-binary https://codeberg.org/Limine/Limine.git ~/tools/limine
make -C ~/tools/limine

./build.sh            # out/kernelproject2.iso
./run.sh              # boot it in a QEMU window (serial log in this terminal)
./run.sh --text       # no window: use the shell right here, over the serial port
python3 tests/smoke.py    # boot, run commands one after another, check the answers
python3 tests/persist.py  # save files and a git repo, reboot, find them again
python3 tests/windows.py  # open text and graphical windows, use them with the mouse
python3 tests/edit.py     # type in the text editor, undo, cut and paste, save
python3 tests/textedit.py # the same in a window, through the shared clipboard
```

## Roadmap

Roughly in order; each step is checked by running a real program.

1. **More of Linux for real tools**: real interval timers (`setitimer`/`alarm` are accepted but
   don't fire yet), `socketpair` and Unix sockets, `eventfd`, `epoll`.
2. **More disk**: install programs onto the disk (a root file system on ext2 instead of the boot
   archive), interrupt-driven I/O, and AHCI for real PCs.
3. **Run the targets**: fastfetch and git run; next then coreutils as a compatibility test.
4. **More Windows**: BusyBox for Windows runs. Next: more real console programs (7-Zip), then
   windows and graphics.
5. Later: more CPUs (APIC/IOAPIC), networking, a real login, and running make/gcc/cmake on the system itself.

## Licenses of what's used

- [Limine](https://codeberg.org/Limine/Limine) (bootloader, BSD-2-Clause) is downloaded at build time.
- Rust crates: `limine`, `spin`, `linked_list_allocator` (MIT/Apache-2.0).
- The console font is [Spleen](https://github.com/fcambus/spleen) 8x16 (BSD-2-Clause, `kernel/src/font-LICENSE`),
  converted by `tools/bdf2rs.py`.
- The apps' fonts, downloaded at build time: [Inter](https://rsms.me/inter/) and
  [JetBrains Mono](https://www.jetbrains.com/lp/mono/), both SIL Open Font License (their OFL.txt
  is copied next to them in `/usr/share/fonts`).
- The test programs link against glibc (LGPL) from the build machine.
