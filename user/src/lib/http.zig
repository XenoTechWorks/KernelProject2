//! Fetching a file over HTTP (plain HTTP/1.0, no TLS): what Software and `pkg get` download
//! packages with.
//!
//!     const data = try http.get("http://10.0.2.2:8086/index", null);

const std = @import("std");
const rt = @import("rt.zig");
const net = @import("net.zig");
const sys = rt.sys;

const gpa = std.heap.page_allocator;

pub const Error = error{ BadUrl, NoNetwork, NoSuchHost, Refused, TimedOut, Failed, BadResponse, NotFound, OutOfMemory };

/// Told how far a download has got: bytes so far, and the total when the server says.
pub const Progress = struct {
    context: *anyopaque,
    report: *const fn (context: *anyopaque, got: usize, total: ?usize) void,
};

pub const Url = struct { host: []const u8, port: u16, path: []const u8 };

/// "http://host[:port]/path".
pub fn parseUrl(url: []const u8) ?Url {
    const prefix = "http://";
    if (!std.mem.startsWith(u8, url, prefix)) return null;
    const rest = url[prefix.len..];
    const slash = std.mem.indexOfScalar(u8, rest, '/') orelse rest.len;
    const authority = rest[0..slash];
    const path = if (slash < rest.len) rest[slash..] else "/";
    if (std.mem.indexOfScalar(u8, authority, ':')) |colon| {
        const port = std.fmt.parseInt(u16, authority[colon + 1 ..], 10) catch return null;
        return .{ .host = authority[0..colon], .port = port, .path = path };
    }
    return if (authority.len > 0) .{ .host = authority, .port = 80, .path = path } else null;
}

/// How long to wait for the server to say anything before giving up.
const timeout_ms = 20_000;

fn waitReadable(fd: i32) bool {
    var pfd = [1]extern struct { fd: i32, events: i16 = 1, revents: i16 = 0 }{.{ .fd = fd }};
    while (true) {
        const r = sys.check(sys.syscall3(7, @intFromPtr(&pfd), 1, timeout_ms)); // poll
        switch (r) {
            .ok => |n| return n > 0,
            .err => |e| if (e != sys.EINTR) return false,
        }
    }
}

/// The body of `url`, which the caller frees.
pub fn get(url: []const u8, progress: ?Progress) Error![]u8 {
    const u = parseUrl(url) orelse return error.BadUrl;
    const ip = net.lookup(u.host) orelse return error.NoSuchHost;
    const fd = net.tcpConnect(ip, u.port) catch |e| return switch (e) {
        error.NoNetwork => error.NoNetwork,
        error.Refused => error.Refused,
        error.TimedOut => error.TimedOut,
        error.Failed => error.Failed,
    };
    defer sys.close(fd);

    var req: [1200]u8 = undefined;
    const request = std.fmt.bufPrint(&req, "GET {s} HTTP/1.0\r\nHost: {s}\r\nUser-Agent: KernelProject2\r\nConnection: close\r\n\r\n", .{ u.path, u.host }) catch return error.BadUrl;
    rt.writeAll(fd, request);

    var data = std.ArrayList(u8).empty;
    errdefer data.deinit(gpa);
    var header_end: ?usize = null;
    var total: ?usize = null;
    var chunk: [64 * 1024]u8 = undefined;
    while (true) {
        if (!waitReadable(fd)) return error.TimedOut;
        const n = switch (sys.read(fd, &chunk)) {
            .ok => |n| n,
            .err => |e| if (e == sys.EINTR) continue else return error.Failed,
        };
        if (n == 0) break;
        try data.appendSlice(gpa, chunk[0..n]);
        if (header_end == null) {
            const end = std.mem.indexOf(u8, data.items, "\r\n\r\n") orelse continue;
            header_end = end + 4;
            const head = data.items[0..end];
            // "HTTP/1.x 200 OK"
            var words = std.mem.tokenizeScalar(u8, std.mem.sliceTo(head, '\r'), ' ');
            _ = words.next();
            const status = std.fmt.parseInt(u16, words.next() orelse "", 10) catch return error.BadResponse;
            if (status == 404) return error.NotFound;
            if (status != 200) return error.BadResponse;
            var lines = std.mem.splitSequence(u8, head, "\r\n");
            while (lines.next()) |line| {
                const colon = std.mem.indexOfScalar(u8, line, ':') orelse continue;
                if (std.ascii.eqlIgnoreCase(line[0..colon], "content-length"))
                    total = std.fmt.parseInt(usize, std.mem.trim(u8, line[colon + 1 ..], " "), 10) catch null;
            }
            if (total) |t| try data.ensureTotalCapacity(gpa, header_end.? + t);
        }
        if (progress) |p| p.report(p.context, data.items.len - header_end.?, total);
        if (total) |t| if (data.items.len - header_end.? >= t) break;
    }
    const start = header_end orelse return error.BadResponse;
    if (total) |t| if (data.items.len - start < t) return error.Failed; // cut off
    const body = try gpa.dupe(u8, data.items[start..]);
    data.deinit(gpa);
    return body;
}

pub fn errorText(e: Error) []const u8 {
    return switch (e) {
        error.BadUrl => "the address isn't an http:// one",
        error.NoNetwork => "there's no network",
        error.NoSuchHost => "the server's name can't be found",
        error.Refused => "the server isn't running (connection refused)",
        error.TimedOut => "the server didn't answer",
        error.Failed => "the connection failed",
        error.BadResponse => "the server sent something unexpected",
        error.NotFound => "the server doesn't have it",
        error.OutOfMemory => "out of memory",
    };
}
