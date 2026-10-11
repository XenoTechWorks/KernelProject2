//! Talking over the network: the socket calls, wrapped so the programs here read plainly.
//! IPv4 datagrams only for now (UDP, and ICMP for ping).

const std = @import("std");
const rt = @import("rt.zig");
const sys = rt.sys;

pub const Ip = [4]u8;

const AF_INET: usize = 2;
const SOCK_DGRAM: usize = 2;
const IPPROTO_ICMP: usize = 1;
const IPPROTO_UDP: usize = 17;

/// Linux's sockaddr_in: family, port and address, the last two in network order.
const SockAddrIn = extern struct {
    family: u16 = AF_INET,
    port: u16 = 0,
    addr: Ip = .{ 0, 0, 0, 0 },
    zero: [8]u8 = @splat(0),
};

/// "10.0.2.2" to its four bytes.
pub fn parseIp(text: []const u8) ?Ip {
    var out: Ip = undefined;
    var parts = std.mem.splitScalar(u8, text, '.');
    for (&out) |*b| {
        const part = parts.next() orelse return null;
        b.* = std.fmt.parseInt(u8, part, 10) catch return null;
    }
    return if (parts.next() == null) out else null;
}

pub fn format(buf: []u8, ip: Ip) []const u8 {
    return std.fmt.bufPrint(buf, "{d}.{d}.{d}.{d}", .{ ip[0], ip[1], ip[2], ip[3] }) catch "?";
}

fn socket(protocol: usize) ?i32 {
    return switch (sys.check(sys.syscall3(41, AF_INET, SOCK_DGRAM, protocol))) {
        .ok => |fd| @intCast(fd),
        .err => null,
    };
}

pub fn udpSocket() ?i32 {
    return socket(IPPROTO_UDP);
}

pub fn icmpSocket() ?i32 {
    return socket(IPPROTO_ICMP);
}

pub fn sendTo(fd: i32, ip: Ip, port: u16, data: []const u8) bool {
    var to = SockAddrIn{ .port = std.mem.nativeToBig(u16, port), .addr = ip };
    const r = sys.syscall6(44, @bitCast(@as(isize, fd)), @intFromPtr(data.ptr), data.len, 0, @intFromPtr(&to), @sizeOf(SockAddrIn));
    return sys.check(r) == .ok;
}

/// Waits up to `timeout_ms` for something to arrive. Null when nothing does.
pub fn recvFrom(fd: i32, buf: []u8, timeout_ms: u32) ?usize {
    var pfd = extern struct { fd: i32, events: i16, revents: i16 }{ .fd = fd, .events = 1, .revents = 0 };
    switch (sys.check(sys.syscall3(7, @intFromPtr(&pfd), 1, timeout_ms))) { // poll
        .ok => |n| if (n == 0) return null,
        .err => return null,
    }
    const r = sys.syscall6(45, @bitCast(@as(isize, fd)), @intFromPtr(buf.ptr), buf.len, 0, 0, 0);
    return switch (sys.check(r)) {
        .ok => |n| n,
        .err => null,
    };
}

/// How the card is set up; see the kernel's NetConfig.
pub const Settings = extern struct {
    mac: [6]u8 = @splat(0),
    ip: Ip = .{ 0, 0, 0, 0 },
    netmask: Ip = .{ 0, 0, 0, 0 },
    gateway: Ip = .{ 0, 0, 0, 0 },
    dns: Ip = .{ 0, 0, 0, 0 },
};

const NET_GET: usize = 0x5800;
const NET_SET: usize = 0x5801;

fn ioctl(fd: i32, request: usize, arg: usize) bool {
    return sys.check(sys.syscall3(16, @bitCast(@as(isize, fd)), request, arg)) == .ok;
}

pub fn settings() ?Settings {
    const fd = udpSocket() orelse return null;
    defer sys.close(fd);
    var out = Settings{};
    return if (ioctl(fd, NET_GET, @intFromPtr(&out))) out else null;
}

pub fn setSettings(want: Settings) bool {
    const fd = udpSocket() orelse return false;
    defer sys.close(fd);
    var copy = want;
    return ioctl(fd, NET_SET, @intFromPtr(&copy));
}

// ---- DNS ----

pub const ResolveError = error{ NoNetwork, Timeout, NotFound, Malformed };

/// Looks a name up and fills `out` with the addresses found, returning how many.
pub fn resolve(name: []const u8, server: Ip, out: []Ip) ResolveError!usize {
    const fd = udpSocket() orelse return error.NoNetwork;
    defer sys.close(fd);

    // A query: a header, then the name as length-prefixed labels, then what we're asking for.
    var query: [512]u8 = undefined;
    var at: usize = 12;
    @memset(query[0..12], 0);
    query[0] = 0x2a; // an id; one query at a time, so anything does
    query[1] = 0x2a;
    query[2] = 0x01; // please look it up for us, if you know how
    query[5] = 1; // one question
    var labels = std.mem.splitScalar(u8, name, '.');
    while (labels.next()) |label| {
        if (label.len == 0 or label.len > 63 or at + label.len + 1 >= query.len) return error.Malformed;
        query[at] = @intCast(label.len);
        @memcpy(query[at + 1 ..][0..label.len], label);
        at += label.len + 1;
    }
    query[at] = 0;
    at += 1;
    std.mem.writeInt(u16, query[at..][0..2], 1, .big); // an address
    std.mem.writeInt(u16, query[at + 2 ..][0..2], 1, .big); // on the internet
    at += 4;

    if (!sendTo(fd, server, 53, query[0..at])) return error.NoNetwork;
    var reply: [1024]u8 = undefined;
    const n = recvFrom(fd, &reply, 3000) orelse return error.Timeout;
    return readAnswers(reply[0..n], out);
}

fn readAnswers(reply: []const u8, out: []Ip) ResolveError!usize {
    if (reply.len < 12) return error.Malformed;
    if (reply[3] & 0xf == 3) return error.NotFound; // the server says there's no such name
    if (reply[3] & 0xf != 0) return error.Malformed;
    const questions = std.mem.readInt(u16, reply[4..6], .big);
    const answers = std.mem.readInt(u16, reply[6..8], .big);
    var at: usize = 12;
    for (0..questions) |_| {
        at = try skipName(reply, at);
        at += 4;
    }
    var found: usize = 0;
    for (0..answers) |_| {
        at = try skipName(reply, at);
        if (at + 10 > reply.len) return error.Malformed;
        const kind = std.mem.readInt(u16, reply[at..][0..2], .big);
        const length = std.mem.readInt(u16, reply[at + 8 ..][0..2], .big);
        at += 10;
        if (at + length > reply.len) return error.Malformed;
        if (kind == 1 and length == 4 and found < out.len) {
            out[found] = reply[at..][0..4].*;
            found += 1;
        }
        at += length;
    }
    if (found == 0) return error.NotFound;
    return found;
}

/// Steps over a name, which may end in a pointer back to one earlier in the message.
fn skipName(reply: []const u8, start: usize) ResolveError!usize {
    var at = start;
    while (at < reply.len) {
        const len = reply[at];
        if (len == 0) return at + 1;
        if (len & 0xc0 == 0xc0) return at + 2; // a pointer, and always the last thing
        at += len + 1;
    }
    return error.Malformed;
}

pub fn sleepMs(ms: u64) void {
    var ts = extern struct { sec: i64, nsec: i64 }{ .sec = @intCast(ms / 1000), .nsec = @intCast(ms % 1000 * 1_000_000) };
    _ = sys.syscall2(sys.nr.nanosleep, @intFromPtr(&ts), 0);
}
