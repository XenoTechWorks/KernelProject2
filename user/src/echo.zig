//! echo: print the arguments. `-n` leaves out the newline.

const rt = @import("lib/rt.zig");
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = @import("std").debug.FullPanic(rt.panicHandler);

pub fn main(args: rt.Args) u8 {
    var i: usize = 1;
    var newline = true;
    if (args.get(1)) |first| {
        if (@import("std").mem.eql(u8, first, "-n")) {
            newline = false;
            i = 2;
        }
    }
    const start = i;
    while (args.get(i)) |arg| : (i += 1) {
        if (i > start) rt.out.write(" ");
        rt.out.write(arg);
    }
    if (newline) rt.out.write("\n");
    return 0;
}
