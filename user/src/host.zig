//! host: look up a name with DNS.
//!
//!     host example.com
//!     host example.com 1.1.1.1    ask a particular server
//!
//! A DNS query is one UDP datagram and the answer is another, so this is the smallest real use of
//! the network beyond ping.

const std = @import("std");
const rt = @import("lib/rt.zig");
const net = @import("lib/net.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

pub fn main(args: rt.Args) u8 {
    const name = args.get(1) orelse {
        rt.err.write("usage: host NAME [SERVER]\n");
        return 2;
    };
    const server = if (args.get(2)) |s| net.parseIp(s) orelse {
        rt.err.print("host: {s} isn't an address\n", .{s});
        return 2;
    } else (net.settings() orelse {
        rt.err.write("host: no network\n");
        return 1;
    }).dns;

    var found: [8]net.Ip = undefined;
    const n = net.resolve(name, server, &found) catch |e| {
        rt.err.print("host: {s}\n", .{switch (e) {
            error.NoNetwork => "no network",
            error.Timeout => "no answer from the name server",
            error.NotFound => "no such name",
            else => "the answer made no sense",
        }});
        return 1;
    };
    var buf: [24]u8 = undefined;
    for (found[0..n]) |ip| rt.out.print("{s} has address {s}\n", .{ name, net.format(&buf, ip) });
    return 0;
}
