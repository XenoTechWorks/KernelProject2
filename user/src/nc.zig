//! nc: a TCP connection with standard input and output on either end, like netcat.
//!
//!     nc example.com 80        connect, send what you type, print what comes back
//!     nc -l 7777               wait for someone to connect, then the same
//!
//! When the input ends, nc says so to the other side (half-closes) and keeps printing until the
//! other side is done too.

const std = @import("std");
const rt = @import("lib/rt.zig");
const net = @import("lib/net.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const usage = "usage: nc HOST PORT | nc -l PORT\n";

pub fn main(args: rt.Args) u8 {
    var fd: i32 = undefined;
    if (std.mem.eql(u8, args.get(1) orelse "", "-l")) {
        const port = std.fmt.parseInt(u16, args.get(2) orelse return fail(usage), 10) catch return fail(usage);
        const listener = net.tcpListen(port) orelse return fail("nc: can't listen there\n");
        fd = net.accept(listener) orelse return fail("nc: accept failed\n");
        sys.close(listener);
    } else {
        const host = args.get(1) orelse return fail(usage);
        const port = std.fmt.parseInt(u16, args.get(2) orelse return fail(usage), 10) catch return fail(usage);
        const ip = net.lookup(host) orelse {
            rt.err.print("nc: can't find {s}\n", .{host});
            return 1;
        };
        fd = net.tcpConnect(ip, port) catch |e| {
            rt.err.print("nc: {s}\n", .{switch (e) {
                error.Refused => "connection refused",
                error.TimedOut => "no answer",
                else => "can't connect",
            }});
            return 1;
        };
    }
    defer sys.close(fd);
    pump(fd);
    return 0;
}

fn fail(msg: []const u8) u8 {
    rt.err.write(msg);
    return 2;
}

/// Copies standard input to the socket and the socket to standard output until both are done.
fn pump(fd: i32) void {
    var input_open = true;
    var buf: [4096]u8 = undefined;
    while (true) {
        var fds = [2]extern struct { fd: i32, events: i16, revents: i16 }{
            .{ .fd = fd, .events = 1, .revents = 0 },
            .{ .fd = if (input_open) 0 else -1, .events = 1, .revents = 0 },
        };
        _ = sys.syscall3(7, @intFromPtr(&fds), 2, @bitCast(@as(isize, -1))); // poll
        if (fds[0].revents != 0) {
            switch (sys.read(fd, &buf)) {
                .ok => |n| {
                    if (n == 0) return; // the other side is done
                    rt.writeAll(1, buf[0..n]);
                },
                .err => |e| if (e != sys.EINTR) return,
            }
        }
        if (input_open and fds[1].revents != 0) {
            switch (sys.read(0, &buf)) {
                .ok => |n| {
                    if (n == 0) {
                        input_open = false;
                        _ = sys.syscall2(48, @bitCast(@as(isize, fd)), 1); // shutdown(SHUT_WR)
                    } else rt.writeAll(fd, buf[0..n]);
                },
                .err => |e| if (e != sys.EINTR) {
                    input_open = false;
                },
            }
        }
    }
}
