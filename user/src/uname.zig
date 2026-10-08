//! uname: print system information. `-a` for everything, or pick with -s -n -r -v -m.

const std = @import("std");
const rt = @import("lib/rt.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

pub fn main(args: rt.Args) u8 {
    var info: [6][65]u8 = undefined;
    if (sys.check(sys.syscall1(sys.nr.uname, @intFromPtr(&info))) == .err) return 1;
    var want: [5]bool = @splat(false);
    var i: usize = 1;
    while (args.get(i)) |a| : (i += 1) {
        if (a.len < 2 or a[0] != '-') continue;
        for (a[1..]) |c| switch (c) {
            'a' => want = .{ true, true, true, true, true },
            's' => want[0] = true,
            'n' => want[1] = true,
            'r' => want[2] = true,
            'v' => want[3] = true,
            'm' => want[4] = true,
            else => {
                rt.err.print("uname: unknown option -{c}\n", .{c});
                return 1;
            },
        };
    }
    if (std.mem.indexOfScalar(bool, &want, true) == null) want[0] = true;
    var first = true;
    for (want, 0..) |on, field| {
        if (!on) continue;
        if (!first) rt.out.write(" ");
        first = false;
        rt.out.write(std.mem.sliceTo(&info[field], 0));
    }
    rt.out.write("\n");
    return 0;
}
