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
- Terminals: Linux's line discipline (editing, echo, Ctrl+C/D/U/W, raw mode via `termios`) on the
  screen and on the serial port, and pseudo-terminals for programs that run others (see The
  command line). The screen understands the usual escape codes and 16 colors.
- What event loops and servers are built on: `epoll` (level- and edge-triggered, one-shot),
  `eventfd`, `signalfd`, Unix sockets (see Networking), timers that really go off (`alarm`,
  `setitimer`, and the timeouts of `poll`, `ppoll` and `epoll_wait`), `sigwaitinfo`, and `flock`.
  Signals carry who sent them and why (a child's exit status for `SIGCHLD`). Python's `asyncio`
  runs on them.
- About 150 Linux system calls (see `kernel/src/sys/mod.rs`). Missing ones return `ENOSYS` and are
  logged once on the serial port, which shows what a new program needs next.

- `/proc` (meminfo, cpuinfo, uptime, stat, mounts, and each process's stat, status, cmdline, exe, fd)
  and the parts of `/sys` programs read today, including the machine's real identity from SMBIOS.
- The screen uses the Spleen 8x16 font with UTF-8, 160x50 characters at 1280x800.

The proof: `hello-static` and `hello-dynamic` are ordinary C programs built by gcc against glibc and
copied in unchanged, and they run. So do **fastfetch** (built from its own unchanged source) and
**git** (Ubuntu's own `/usr/bin/git`): init, add, commit, branches, merges, diff, stash, log --graph.
`signal-test`, `thread-test` and `events-test` check signals, glibc pthreads, and epoll, eventfd,
Unix sockets and timers, against the same results as on Linux.

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

## Networking

There's a network card and a stack: Ethernet, ARP, IPv4, ICMP, UDP and TCP.

```sh
ifconfig                       # the card, its address, the gateway
ping -c 3 10.0.2.2             # ARP for the gateway, then echo requests
host example.com               # a DNS lookup: a UDP datagram out, one back
curl https://example.com       # Ubuntu's own curl: DNS, TCP, then TLS
nc example.com 80              # a raw TCP connection; nc -l PORT waits for one
```

`run.sh` gives QEMU a virtio network card on its user-mode network, which puts the machine at
10.0.2.15 behind a gateway at 10.0.2.2 and a name server at 10.0.2.3 — the defaults the stack
starts with, since there's no DHCP yet. `ifconfig ADDRESS NETMASK GATEWAY DNS` changes them.

For programs it's the Linux socket calls: `socket`, `bind`, `listen`, `accept`, `connect`,
`send`/`recv` in all their forms (`sendto`, `recvmsg`, `sendmmsg`...), `shutdown`, `getsockopt`
(with `SO_ERROR` for a connect that didn't wait), non-blocking sockets and `poll`. That's enough
for software written for Linux to just work: `curl` is Ubuntu's own binary, and glibc's own
resolver finds names through `/etc/resolv.conf`. A `SOCK_DGRAM` socket on `IPPROTO_ICMP` is how
`ping` works without being root, as on Linux. `user/src/lib/net.zig` wraps it all for our tools.

- `kernel/src/drivers/virtio_net.rs` is the card: one queue of buffers for it to fill, one of
  frames to send, collected on each timer tick.
- `kernel/src/net/` is the stack. ARP learns addresses and holds a packet back until it knows
  where to send it; IP, ICMP and UDP are each about a screenful.
- `kernel/src/net/tcp.rs` is TCP: the handshake both ways, a send buffer resent from the oldest
  unacknowledged byte when the timer runs out (waiting twice as long each time), the other
  side's window, a clean close (and TIME-WAIT), and resets. It keeps things simple where that's
  safe: a segment that arrives out of order is dropped for the sender to resend, and there's no
  congestion control beyond the window.

Not there yet: IPv6, DHCP, and a loopback (`127.0.0.1`). Pinging something beyond the gateway
fails, but that's QEMU's user-mode network not passing ICMP on, not the stack.

### Unix sockets

Programs on the same machine talk through Unix sockets: `socketpair` for two already connected,
or a server that binds a name and clients that connect to it. A name is a socket file (`bind`
makes one; `ls -l` shows it with an `s`), or an "abstract" name that lives only in the kernel.
Streams and datagrams both work, and `SO_PEERCRED` tells a server which process and user is on
the other end. `kernel/src/net/unix.rs` is all of it: a stream connection is two pipes, one each
way, so end-of-file and `EPIPE` behave exactly as they do for pipes. Open files can be passed
along (`SCM_RIGHTS`), on a stream tied to the bytes they were sent with, as Linux does: that's
how a tmux client hands the server its terminal.

### SSH

Ubuntu's own OpenSSH client is in the image: `ssh`, `scp`, `ssh-keygen`, `ssh-agent` and
`ssh-add`.

```sh
ssh-keygen -t ed25519            # a key pair in ~/.ssh (on the disk, so it stays)
ssh you@10.0.2.2                 # log in to the machine QEMU runs on (if it runs an SSH server)
scp notes.txt you@10.0.2.2:      # copy a file there
ssh-agent -a /tmp/agent.sock     # keep keys unlocked; it listens on a Unix socket
export SSH_AUTH_SOCK=/tmp/agent.sock
ssh-add                          # hand it your key
```

`tests/ssh.py` runs an SSH server on the build machine (Ubuntu's `sshd`, as you, on a spare port)
and does all of the above against it, an interactive login included. `/usr/bin` is the same
folder as `/bin`, as on today's Linux distributions, since `scp` starts `/usr/bin/ssh` by that
name. An SSH server inside KernelProject2 needs `chroot` next (OpenSSH shuts its unprivileged
half away in an empty folder).

## Trying other programs

Anything on the build machine can be brought along, unchanged, with the libraries it needs:

```sh
EXTRA_PROGRAMS="jq bc sed grep dash less nano make" ./build.sh
```

These all run as they are, which is the point of the whole exercise:

| | |
|---|---|
| `jq`, `bc`, `sed`, `grep` | work on the system's own files |
| `dash` | another shell, loops and all |
| `less`, `nano` | full-screen terminal programs |
| `make` | reads a Makefile and runs the recipes |
| `git`, `fastfetch` | already in the image |
| `busybox64.exe` | a Windows program, through the Windows layer |

Two things to know. Everything in the image is unpacked into the kernel heap at boot, so the
image has to fit in it (`HEAP_SIZE` in `kernel/src/mm/mod.rs`, 256 MiB). And a program that
carries a tree of its own files alongside the binary — Python and its standard library, say —
needs that tree copied in too, which `EXTRA_PROGRAMS` doesn't do yet.

## Archives and packages

`gzip` (with `gunzip` and `zcat`), `tar` and `zip` (with `unzip`) are our own, built on a DEFLATE
of our own in `user/src/lib/deflate.zig`. They write the real formats and read other people's:
`tests/archive.py` checks every one of them both ways round against BusyBox.

```sh
tar -czf etc.tar.gz /etc     # pack        tar -tzf etc.tar.gz   # look inside
tar -xzf etc.tar.gz -C /tmp  # unpack      zip -r notes.zip notes/ ; unzip notes.zip
```

`pkg` installs things and remembers what each one left behind, so it can take it away again:

```sh
pkg info /usr/share/examples/hello_1.0_all.deb   # what's in it
pkg install /usr/share/examples/hello_1.0_all.deb
hello                                            # the program it brought
pkg list ; pkg files hello ; pkg remove hello
```

- **`.deb`** — a Linux package: an `ar` archive of `control.tar.gz` and `data.tar.gz`, unpacked
  onto the system. Debian's tools now compress with zstd by default, which we don't read yet; a
  package built with `dpkg-deb -Zgzip` (what the sample one is) works, and real `dpkg` reads those
  too.
- **`.zip`** — a Windows program kept as it is. It goes into `/opt/NAME`, and each `.exe` gets its
  plain name in `/bin`, so `winapi` runs `/opt/demo/winapi.exe` through the Windows layer.
- **`.exe`** — one Windows program, into `/bin`.

## Accounts

The machine boots straight into a root shell, as a thing you're building should. Beside that
there are real accounts:

```sh
useradd jax        # make one (root only)
passwd jax         # give them a password
su jax             # become them
whoami ; id        # who you are
login              # ask for a name and password
```

Accounts live in `/etc/passwd` and their passwords, hashed, in `/etc/shadow`, which only root can
read. The hashing is sha256crypt (`$5$...`), worked out byte for byte the same as glibc, so a line
copied from a Linux machine works here and the other way round.

The permissions are real, not for show. The kernel gives every process a user and group, checks a
file's mode bits against them when it's opened, and honours the set-user-id bit: `su` is owned by
root with that bit set, which is how it reads `/etc/shadow` when you can't. Try it:

```sh
su jax
cat /etc/shadow    # Permission denied
useradd bob        # useradd: only root can add users
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

### Pseudo-terminals, script and tmux

A pseudo-terminal is a terminal with a program on each side: opening `/dev/ptmx` makes one, the
program holding it is the "outside" (like the screen and keyboard), and `/dev/pts/N` is the
inside, where a shell runs with everything a terminal gives it: line editing, raw mode, Ctrl+C
for its foreground, a window size (and `SIGWINCH` when it changes), and a hang-up when the
outside goes. Each terminal has its own line discipline (`kernel/src/tty.rs`; the console is one
of them, `kernel/src/pty.rs` makes the others), and each session its controlling terminal, which
`/dev/tty` means.

Ubuntu's own `script` and `tmux` run on them, unchanged:

```sh
script -c 'ls -l' session.txt   # run a command and keep a copy of everything it showed
tmux                            # windows and panes inside the console; Ctrl+B d leaves it running
tmux attach                     # ...and this comes back to it
```

`pty-test` checks the details against Linux, and `tests/terminals.py` drives `script` and `tmux`.

## Apps in windows

Programs can open windows that float over the console, with a header, a close button and a soft
shadow. It isn't a desktop: the command line stays the whole screen, and windows sit on top of it.

```sh
clock &            # the time, big
calc &             # a calculator (click, or type: 7*6 Enter)
files /etc &       # browse folders, peek into files
textedit notes &   # a text editor
eyes &             # two eyes that follow the mouse, after X11's xeyes
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
  when the user resizes the window. Games also get every physical key going down and up, Shift
  and Ctrl included, as its PC scancode.
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

## Doom

Chocolate Doom runs: Ubuntu's own `chocolate-doom`, unchanged, playing
[Freedoom](https://freedoom.github.io/) (a free game for the Doom engine).

```sh
doom                    # Freedoom: Phase 1 in a window
doom -timedemo demo1    # play the first demo as fast as it goes, and say how fast
```

Click the window to give it the keyboard (in the demo, the click also opens the menu); arrows walk
and turn, Ctrl fires, Space opens doors, Esc is the menu. Saves and settings go in
`~/.local/share/chocolate-doom`, on the disk. There is no sound yet (no sound card driver).

It draws through SDL 2, the library most Linux games use. `sdl2/` holds a video driver for our
windows (`SDL_kp2video.c`: pixels go out with `WINDOW_BLIT`, events come back as SDL's mouse,
keyboard and window events), and `sdl2/build.sh` builds SDL 2.30.0 with only that in it. The result
answers to the same name and has the same functions as Ubuntu's `libSDL2-2.0.so.0`, so programs
built for Ubuntu load it instead. `build.sh` adds Doom when the build machine has it:

```sh
sudo apt install chocolate-doom freedoom cmake make gcc
./build.sh
```

## Layout

```
kernel/          the kernel (Rust)
  src/arch/        x86-64: GDT, interrupts, syscall entry, PIC/timer, serial
  src/mm/          physical pages, page tables, kernel heap, safe access to user memory
  src/fs/          the VFS, ext2, /proc, /sys, the initramfs reader, pipes
  src/drivers/     PCI, virtio (the disk, the tablet for the pointer, the network card)
  src/net/         Ethernet, ARP, IPv4, ICMP, UDP, TCP, sockets, and Unix sockets
  src/proc/        processes, scheduler, fork/exec/wait, the ELF loader
  src/sys/         the Linux system calls
  src/tty.rs       terminal line discipline
  src/term.rs      a terminal: character cells and escape sequences
  src/console.rs   the screen: the console and windows over it, composited; the pointer
  src/keyboard.rs, src/mouse.rs   PS/2 keyboard and mouse
user/src/        init, the shell, the commands and apps (Zig); lib/gui is the toolkit
windows/src/     the Windows layer: PE loader, kernel32, msvcrt (C++)
sdl2/            SDL 2's video driver for our windows, and the script that builds SDL with it
rootfs/          files copied into the root file system (/etc ...)
disk/            what a new disk starts with (out/disk.img is made from it once)
tests/           test programs (C, and Windows ones in windows/) and the test scripts:
                 smoke.py, persist.py, windows.py, edit.py, textedit.py, archive.py,
                 pkg.py, accounts.py, net.py, tcp.py, ssh.py, terminals.py, doom.py,
                 session.sh; events.c is events-test and ptys.c pty-test
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
python3 tests/archive.py  # gzip, tar and zip, checked against BusyBox both ways
python3 tests/pkg.py      # a real .deb, and a Windows program in a .zip, on and off again
python3 tests/accounts.py # a user, a password, and what they're not allowed to do
python3 tests/net.py      # the card, ping through ARP, and a DNS lookup
python3 tests/tcp.py      # curl over TCP (a big file byte for byte), a server in the guest, HTTPS
python3 tests/ssh.py      # ssh, scp and ssh-agent against an SSH server on this machine
python3 tests/terminals.py # script and tmux, in pseudo-terminals
python3 tests/doom.py     # Doom's demo, timed, then quitting it from its menu
```

## Roadmap

Roughly in order; each step is checked by running a real program.

1. **More of Linux for real tools**: `chroot` (for an SSH server inside), `timerfd` and `pidfd`,
   and signal masks in `ppoll`/`epoll_pwait`.
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
- [SDL 2](https://libsdl.org/) (zlib license) is downloaded at build time. Chocolate Doom
  (GPL-2.0) and Freedoom (BSD-3-Clause, its copyright file is copied next to the WAD) come from
  the build machine's packages.
