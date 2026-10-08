//! cat: print files (or standard input, or `-`) one after another.

const std = @import("std");
const rt = @import("lib/rt.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

fn copy(fd: i32, name: []const u8) bool {
    var buf: [16384]u8 = undefined;
    while (true) {
        switch (sys.read(fd, &buf)) {
            .ok => |n| {
                if (n == 0) return true;
                rt.writeAll(1, buf[0..n]);
            },
            .err => |e| {
                if (e == sys.EINTR) continue;
                rt.complain("cat", name, e);
                return false;
            },
        }
    }
}

pub fn main(args: rt.Args) u8 {
    if (args.argv.len < 2) return if (copy(0, "-")) 0 else 1;
    var ok = true;
    var i: usize = 1;
    while (args.get(i)) |name| : (i += 1) {
        if (std.mem.eql(u8, name, "-")) {
            ok = copy(0, "-") and ok;
            continue;
        }
        var buf: [1024]u8 = undefined;
        const path = rt.cstr(&buf, name) orelse continue;
        switch (sys.open(path, sys.O_RDONLY, 0)) {
            .ok => |fd| {
                ok = copy(@intCast(fd), name) and ok;
                sys.close(@intCast(fd));
            },
            .err => |e| {
                rt.complain("cat", name, e);
                ok = false;
            },
        }
    }
    return if (ok) 0 else 1;
}
