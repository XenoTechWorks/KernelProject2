//! ping: send ICMP echo requests and time the replies.
//!
//!     ping 10.0.2.2        the gateway
//!     ping -c 3 1.1.1.1
//!
//! It uses a datagram socket on IPPROTO_ICMP, which is the way Linux lets a program ping without
//! being root; the kernel puts the socket's id in each message so replies find their way back.

const std = @import("std");
const rt = @import("lib/rt.zig");
const net = @import("lib/net.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

fn nowMs() i64 {
    var ts: extern struct { sec: i64, nsec: i64 } = .{ .sec = 0, .nsec = 0 };
    _ = sys.syscall2(sys.nr.clock_gettime, 1, @intFromPtr(&ts)); // CLOCK_MONOTONIC
    return ts.sec * 1000 + @divTrunc(ts.nsec, 1_000_000);
}

pub fn main(args: rt.Args) u8 {
    var count: usize = 4;
    var target: ?[]const u8 = null;
    var i: usize = 1;
    while (args.get(i)) |arg| : (i += 1) {
        if (std.mem.eql(u8, arg, "-c")) {
            i += 1;
            count = std.fmt.parseInt(usize, args.get(i) orelse "4", 10) catch 4;
        } else target = arg;
    }
    const where = target orelse {
        rt.err.write("usage: ping [-c COUNT] ADDRESS\n");
        return 2;
    };
    const ip = net.parseIp(where) orelse {
        rt.err.print("ping: {s} isn't an address (names need DNS, which isn't here yet)\n", .{where});
        return 2;
    };

    const fd = net.icmpSocket() orelse {
        rt.err.write("ping: can't open a socket (is there a network card?)\n");
        return 1;
    };
    defer sys.close(fd);

    rt.out.print("PING {d}.{d}.{d}.{d}\n", .{ ip[0], ip[1], ip[2], ip[3] });
    var sent: usize = 0;
    var got: usize = 0;
    var seq: u16 = 1;
    while (sent < count) : (seq += 1) {
        // An echo request: type 8, then an id the kernel fills in, a sequence number and anything.
        var message: [32]u8 = @splat(0);
        message[0] = 8;
        std.mem.writeInt(u16, message[6..8], seq, .big);
        for (message[8..], 0..) |*b, n| b.* = @intCast('a' + n % 26);
        const started = nowMs();
        if (!net.sendTo(fd, ip, 0, &message)) {
            rt.err.write("ping: can't send\n");
            return 1;
        }
        sent += 1;

        // Wait up to a second for the reply with our sequence number.
        var reply: [1024]u8 = undefined;
        while (true) {
            const waited = nowMs() - started;
            if (waited >= 1000) {
                rt.out.print("no reply for seq {d}\n", .{seq});
                break;
            }
            const n = net.recvFrom(fd, &reply, @intCast(1000 - waited)) orelse break;
            if (n >= 8 and reply[0] == 0 and std.mem.readInt(u16, reply[6..8], .big) == seq) {
                got += 1;
                rt.out.print("{d} bytes from {d}.{d}.{d}.{d}: seq={d} time={d} ms\n", .{ n, ip[0], ip[1], ip[2], ip[3], seq, nowMs() - started });
                break;
            }
        }
        if (sent < count) net.sleepMs(300);
    }
    rt.out.print("\n{d} sent, {d} received, {d}% lost\n", .{ sent, got, if (sent > 0) (sent - got) * 100 / sent else 0 });
    return if (got > 0) 0 else 1;
}
