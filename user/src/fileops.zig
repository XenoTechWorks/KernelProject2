//! mkdir, rm, touch and clear in one program: what it does depends on the name it was started as
//! (each is a link to this file in /bin), the way BusyBox works.
//!
//!   mkdir [-p] DIR...    rm [-r] [-f] PATH...    touch FILE...    clear

const std = @import("std");
const rt = @import("lib/rt.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

fn mkdir(path: []const u8, parents: bool) bool {
    var buf: [1024]u8 = undefined;
    const c = rt.cstr(&buf, path) orelse return false;
    const r = sys.check(sys.syscall2(sys.nr.mkdir, @intFromPtr(c), 0o755));
    if (r == .ok) return true;
    if (parents and r.err == sys.EEXIST) return true;
    if (parents and r.err == sys.ENOENT) {
        const slash = std.mem.lastIndexOfScalar(u8, std.mem.trimEnd(u8, path, "/"), '/') orelse return false;
        if (slash > 0 and !mkdir(path[0..slash], true)) return false;
        return mkdir(path, false);
    }
    rt.complain("mkdir", path, r.err);
    return false;
}

fn remove(path: []const u8, recursive: bool, force: bool) bool {
    var buf: [1024]u8 = undefined;
    const c = rt.cstr(&buf, path) orelse return false;
    var st: sys.Stat = undefined;
    switch (sys.lstat(c, &st)) {
        .err => |e| {
            if (!force) rt.complain("rm", path, e);
            return force;
        },
        .ok => {},
    }
    if (st.mode & sys.S_IFMT == sys.S_IFDIR) {
        if (!recursive) {
            rt.err.print("rm: {s}: is a directory (use rm -r)\n", .{path});
            return false;
        }
        // Remove the contents first. Re-read the directory each time, since it changes as we go.
        const fd = switch (sys.open(c, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
            .ok => |fd| @as(i32, @intCast(fd)),
            .err => |e| {
                rt.complain("rm", path, e);
                return false;
            },
        };
        var ok = true;
        var dents: [4096]u8 align(8) = undefined;
        outer: while (true) {
            const n = switch (sys.check(sys.syscall3(sys.nr.getdents64, @intCast(fd), @intFromPtr(&dents), dents.len))) {
                .ok => |n| n,
                .err => break,
            };
            if (n == 0) break;
            var at: usize = 0;
            while (at < n) {
                const reclen = std.mem.readInt(u16, dents[at + 16 ..][0..2], .little);
                const name = std.mem.span(@as([*:0]const u8, @ptrCast(&dents[at + 19])));
                at += reclen;
                if (std.mem.eql(u8, name, ".") or std.mem.eql(u8, name, "..")) continue;
                var child: [1024]u8 = undefined;
                const child_path = std.fmt.bufPrint(&child, "{s}/{s}", .{ path, name }) catch continue;
                if (!remove(child_path, true, force)) {
                    ok = false;
                    break :outer;
                }
                _ = sys.syscall3(sys.nr.lseek, @intCast(fd), 0, 0);
                continue :outer;
            }
        }
        sys.close(fd);
        if (!ok) return false;
        const r = sys.check(sys.syscall1(sys.nr.rmdir, @intFromPtr(c)));
        if (r == .err) {
            rt.complain("rm", path, r.err);
            return false;
        }
        return true;
    }
    const r = sys.check(sys.syscall1(sys.nr.unlink, @intFromPtr(c)));
    if (r == .err) {
        rt.complain("rm", path, r.err);
        return false;
    }
    return true;
}

fn touch(path: []const u8) bool {
    var buf: [1024]u8 = undefined;
    const c = rt.cstr(&buf, path) orelse return false;
    switch (sys.open(c, sys.O_WRONLY | sys.O_CREAT, 0o644)) {
        .ok => |fd| {
            sys.close(@intCast(fd));
            _ = sys.syscall4(sys.nr.utimensat, @bitCast(@as(isize, -100)), @intFromPtr(c), 0, 0);
            return true;
        },
        .err => |e| {
            rt.complain("touch", path, e);
            return false;
        },
    }
}

pub fn main(args: rt.Args) u8 {
    const argv0 = args.get(0) orelse "";
    const name = if (std.mem.lastIndexOfScalar(u8, argv0, '/')) |slash| argv0[slash + 1 ..] else argv0;
    var flags: [128]bool = @splat(false);
    var i: usize = 1;
    while (args.get(i)) |a| : (i += 1) {
        if (a.len < 2 or a[0] != '-') continue;
        for (a[1..]) |c| flags[c & 127] = true;
    }
    if (std.mem.eql(u8, name, "clear")) {
        rt.out.write("\x1b[H\x1b[2J");
        return 0;
    }
    var ok = true;
    var any = false;
    i = 1;
    while (args.get(i)) |a| : (i += 1) {
        if (a.len > 1 and a[0] == '-') continue;
        any = true;
        const done = if (std.mem.eql(u8, name, "mkdir"))
            mkdir(a, flags['p'])
        else if (std.mem.eql(u8, name, "rm"))
            remove(a, flags['r'] or flags['R'], flags['f'])
        else if (std.mem.eql(u8, name, "touch"))
            touch(a)
        else {
            rt.err.print("{s}: unknown command name\n", .{name});
            return 1;
        };
        ok = ok and done;
    }
    if (!any and !(std.mem.eql(u8, name, "rm") and flags['f'])) {
        rt.err.print("{s}: missing operand\n", .{name});
        return 1;
    }
    return if (ok) 0 else 1;
}
