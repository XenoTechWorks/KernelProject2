//! doom: starts Chocolate Doom (Ubuntu's, unchanged) the way it suits this machine: in a window
//! rather than taking the whole screen, playing Freedoom (the free game data) unless told
//! otherwise. Anything after `doom` is passed on, so `doom -warp 1 3` or `doom -iwad other.wad`.

const std = @import("std");
const rt = @import("lib/rt.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const program = "/bin/chocolate-doom";
const freedoom = "/usr/share/games/doom/freedoom1.wad";

pub fn main(args: rt.Args) u8 {
    var argv: [64:null]?[*:0]const u8 = @splat(null);
    var n: usize = 0;
    argv[n] = "chocolate-doom";
    n += 1;
    // Our defaults first, so anything the user gives comes later and wins.
    var user_iwad = false;
    for (args.argv[1..]) |a| {
        if (std.mem.eql(u8, std.mem.span(a), "-iwad")) user_iwad = true;
    }
    const defaults = [_][*:0]const u8{ "-window", "-geometry", "640x400" };
    for (defaults) |d| {
        argv[n] = d;
        n += 1;
    }
    if (!user_iwad) {
        argv[n] = "-iwad";
        argv[n + 1] = freedoom;
        n += 2;
    }
    for (args.argv[1..]) |a| {
        if (n + 1 >= argv.len) break;
        argv[n] = a;
        n += 1;
    }
    _ = sys.syscall3(sys.nr.execve, @intFromPtr(program), @intFromPtr(&argv), @intFromPtr(args.envp));
    rt.err.write("doom: Chocolate Doom isn't installed here (build on a machine with the chocolate-doom package)\n");
    return 127;
}
