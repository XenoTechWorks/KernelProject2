//! init: the first program the kernel starts (pid 1). Like Linux's init + getty, it shows the welcome
//! text, starts a shell on the terminal, starts a new one when it exits, and collects orphaned
//! processes so they don't linger as zombies.

const std = @import("std");
const rt = @import("lib/rt.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const shell = "/bin/kpsh";

fn showFile(path: [*:0]const u8) void {
    switch (sys.open(path, sys.O_RDONLY, 0)) {
        .ok => |fd| {
            var buf: [4096]u8 = undefined;
            while (true) switch (sys.read(@intCast(fd), &buf)) {
                .ok => |n| {
                    if (n == 0) break;
                    rt.writeAll(1, buf[0..n]);
                },
                .err => break,
            };
            sys.close(@intCast(fd));
        },
        .err => {},
    }
}

fn startShell() usize {
    const pid = sys.syscall0(sys.nr.fork);
    if (pid == 0) {
        const argv = [_:null]?[*:0]const u8{ "-kpsh", null };
        const envp = [_:null]?[*:0]const u8{
            "PATH=/bin:/sbin:/usr/bin:/usr/sbin",
            "HOME=/root",
            "USER=root",
            "LOGNAME=root",
            "SHELL=/bin/kpsh",
            "TERM=linux",
            "LANG=C.UTF-8",
            null,
        };
        _ = sys.syscall3(sys.nr.chdir, @intFromPtr("/root"), 0, 0);
        const r = sys.check(sys.syscall3(sys.nr.execve, @intFromPtr(shell), @intFromPtr(&argv), @intFromPtr(&envp)));
        rt.complain("init", shell, r.err);
        sys.exit(127);
    }
    return pid;
}

pub fn main(_: rt.Args) u8 {
    showFile("/etc/motd");
    while (true) {
        const shell_pid = startShell();
        // Wait for the shell, collecting any other ended processes along the way.
        while (true) {
            var status: i32 = 0;
            const pid = sys.syscall4(sys.nr.wait4, @bitCast(@as(isize, -1)), @intFromPtr(&status), 0, 0);
            if (pid == shell_pid) break;
            if (sys.Result.of(pid) == .err) {
                // No children at all: wait a moment before starting a new shell.
                const ts = [2]usize{ 1, 0 };
                _ = sys.syscall2(sys.nr.nanosleep, @intFromPtr(&ts), 0);
                break;
            }
        }
        rt.writeAll(1, "\n[init] the shell ended; starting a new one\n\n");
    }
}
