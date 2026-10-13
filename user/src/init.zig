//! init: the first program the kernel starts (pid 1). Like Linux's init + getty, it shows the welcome
//! text, starts a shell on the terminal, starts a new one when it exits, and collects orphaned
//! processes so they don't linger as zombies. It also starts Vela, the desktop, unless the kernel
//! command line says `text` or Vela's settings say not to (`autostart=no` in
//! /root/.config/vela.conf). Each step it reports like the kernel does: "[  OK  ] Started ...".

const std = @import("std");
const rt = @import("lib/rt.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const shell = "/bin/kpsh";
const desktop = "/bin/vela";

const env = [_:null]?[*:0]const u8{
    "PATH=/bin:/sbin:/usr/bin:/usr/sbin",
    "HOME=/root",
    "USER=root",
    "LOGNAME=root",
    "SHELL=/bin/kpsh",
    "TERM=linux",
    "LANG=C.UTF-8",
    null,
};

fn report(ok: bool, comptime fmt: []const u8, args: anytype) void {
    rt.out.write(if (ok) "[\x1b[1;32m  OK  \x1b[0m] " else "[\x1b[1;33m WARN \x1b[0m] ");
    rt.out.print(fmt ++ "\n", args);
}

/// A small file's contents (into buf), or "" if it can't be read.
fn readSmall(path: [*:0]const u8, buf: []u8) []const u8 {
    const fd: i32 = switch (sys.open(path, sys.O_RDONLY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return "",
    };
    defer sys.close(fd);
    return switch (sys.read(fd, buf)) {
        .ok => |n| buf[0..n],
        .err => "",
    };
}

/// Whether to start the desktop: not with `text` on the kernel command line, nor when the user
/// turned it off in Vela's settings.
fn wantDesktop() ?[]const u8 {
    var cmd_buf: [512]u8 = undefined;
    var words = std.mem.tokenizeAny(u8, readSmall("/proc/cmdline", &cmd_buf), " \n");
    while (words.next()) |w| {
        if (std.mem.eql(u8, w, "text")) return "\"text\" is on the kernel command line";
    }
    var conf_buf: [512]u8 = undefined;
    var lines = std.mem.splitScalar(u8, readSmall("/root/.config/vela.conf", &conf_buf), '\n');
    while (lines.next()) |line| {
        if (std.mem.eql(u8, std.mem.trim(u8, line, " \r\t"), "autostart=no")) return "it's switched off in Personalize";
    }
    return null;
}

/// Starts Vela on its own (it doesn't take the terminal; the shell keeps it underneath).
fn startDesktop() void {
    if (wantDesktop()) |why| {
        report(true, "Not starting Vela, the desktop: {s}", .{why});
        return;
    }
    var st: sys.Stat = undefined;
    if (sys.stat(desktop, &st) != .ok) {
        report(false, "Vela, the desktop, isn't installed", .{});
        return;
    }
    const pid = sys.syscall0(sys.nr.fork);
    if (pid == 0) {
        _ = sys.syscall0(112); // setsid
        _ = sys.syscall3(sys.nr.chdir, @intFromPtr("/root"), 0, 0);
        const argv = [_:null]?[*:0]const u8{ desktop, null };
        _ = sys.syscall3(sys.nr.execve, @intFromPtr(desktop), @intFromPtr(&argv), @intFromPtr(&env));
        sys.exit(127);
    }
    report(true, "Started Vela, the desktop (pid {d})", .{pid});
}

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
        _ = sys.syscall3(sys.nr.chdir, @intFromPtr("/root"), 0, 0);
        const r = sys.check(sys.syscall3(sys.nr.execve, @intFromPtr(shell), @intFromPtr(&argv), @intFromPtr(&env)));
        rt.complain("init", shell, r.err);
        sys.exit(127);
    }
    return pid;
}

pub fn main(_: rt.Args) u8 {
    var first = true;
    while (true) {
        if (first) showFile("/etc/motd");
        const shell_pid = startShell();
        if (first) {
            report(true, "Started kpsh, the shell, on the console (pid {d})", .{shell_pid});
            startDesktop();
            first = false;
        }
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
