//! What Vela's parts share: the font, the list of open windows, starting apps, and the time.

const std = @import("std");
const sys = @import("../lib/sys.zig");
const gui = @import("../lib/gui.zig");

pub var font: *gui.Font = undefined;

/// The normal windows (for the taskbar and the dock's dots), bottom to top.
pub var windows: [32]gui.Listed = undefined;
pub var window_count: usize = 0;

pub fn refreshWindows(any: *gui.Window) void {
    window_count = any.list(&windows);
}

pub fn openWindows() []const gui.Listed {
    return windows[0..window_count];
}

/// The topmost window whose title has `part` in it.
pub fn findWindow(part: []const u8) ?gui.Listed {
    var i = window_count;
    while (i > 0) {
        i -= 1;
        if (std.mem.indexOf(u8, windows[i].name(), part) != null) return windows[i];
    }
    return null;
}

const env = [_:null]?[*:0]const u8{
    "PATH=/bin:/sbin:/usr/bin:/usr/sbin:/usr/games",
    "HOME=/root",
    "USER=root",
    "LOGNAME=root",
    "SHELL=/bin/kpsh",
    "TERM=linux",
    "LANG=C.UTF-8",
    null,
};

/// Starts a program on its own (its own session, in the home folder), not waiting for it.
pub fn launch(path: [*:0]const u8) void {
    const pid = sys.syscall0(sys.nr.fork);
    if (pid != 0) return;
    _ = sys.syscall0(112); // setsid
    _ = sys.syscall1(sys.nr.chdir, @intFromPtr("/root"));
    const argv = [_:null]?[*:0]const u8{ path, null };
    _ = sys.syscall3(sys.nr.execve, @intFromPtr(path), @intFromPtr(&argv), @intFromPtr(&env));
    sys.exit(127);
}

/// Collects apps that have ended, so they don't stay around as zombies.
pub fn reap() void {
    while (true) {
        var status: i32 = 0;
        const pid = sys.syscall4(sys.nr.wait4, @bitCast(@as(isize, -1)), @intFromPtr(&status), 1, 0); // WNOHANG
        if (sys.Result.of(pid) == .err or pid == 0) return;
    }
}

pub fn nowMs() i64 {
    var ts: extern struct { sec: i64, nsec: i64 } = .{ .sec = 0, .nsec = 0 };
    _ = sys.syscall2(sys.nr.clock_gettime, 1, @intFromPtr(&ts)); // CLOCK_MONOTONIC
    return ts.sec * 1000 + @divTrunc(ts.nsec, 1_000_000);
}

pub fn realSeconds() u64 {
    var ts: extern struct { sec: i64, nsec: i64 } = .{ .sec = 0, .nsec = 0 };
    _ = sys.syscall2(sys.nr.clock_gettime, 0, @intFromPtr(&ts)); // CLOCK_REALTIME (UTC)
    return @intCast(@max(ts.sec, 0));
}

/// Text that fits in `width` pixels: cut short with "…" if it doesn't.
pub fn fit(buf: []u8, s: []const u8, size: f32, width: f32) []const u8 {
    if (gui.measure(font, size, s) <= width) return s;
    const ellipsis = "…";
    var end = s.len;
    while (end > 0) {
        end -= 1;
        while (end > 0 and s[end] & 0xc0 == 0x80) end -= 1; // whole characters
        const candidate = std.fmt.bufPrint(buf, "{s}{s}", .{ s[0..end], ellipsis }) catch return s[0..0];
        if (gui.measure(font, size, candidate) <= width) return candidate;
    }
    return s[0..0];
}
