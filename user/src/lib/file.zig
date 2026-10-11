//! Reading and writing whole files, and making the directories on the way: what the archive
//! tools need.

const std = @import("std");
const rt = @import("rt.zig");
const sys = rt.sys;

const gpa = std.heap.page_allocator;
pub const Error = error{ Io, OutOfMemory };

/// The whole file, as one piece of memory the caller frees. "-" is standard input.
pub fn read(path: [*:0]const u8) Error![]u8 {
    const fd: i32 = if (std.mem.eql(u8, std.mem.span(path), "-")) 0 else switch (sys.open(path, sys.O_RDONLY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return error.Io,
    };
    defer if (fd != 0) sys.close(fd);
    var out = std.ArrayList(u8).empty;
    errdefer out.deinit(gpa);
    var chunk: [64 * 1024]u8 = undefined;
    while (true) {
        switch (sys.read(fd, &chunk)) {
            .ok => |n| {
                if (n == 0) break;
                try out.appendSlice(gpa, chunk[0..n]);
            },
            .err => |e| if (e == sys.EINTR) continue else return error.Io,
        }
    }
    return out.toOwnedSlice(gpa);
}

/// Writes a whole file (creating it). "-" is standard output.
pub fn write(path: [*:0]const u8, data: []const u8, mode: usize) Error!void {
    const fd: i32 = if (std.mem.eql(u8, std.mem.span(path), "-")) 1 else switch (sys.open(path, sys.O_WRONLY | sys.O_CREAT | sys.O_TRUNC, mode)) {
        .ok => |fd| @intCast(fd),
        .err => return error.Io,
    };
    defer if (fd != 1) sys.close(fd);
    var done: usize = 0;
    while (done < data.len) {
        switch (sys.write(fd, data[done..])) {
            .ok => |n| done += n,
            .err => |e| if (e == sys.EINTR) continue else return error.Io,
        }
    }
}

pub fn remove(path: [*:0]const u8) void {
    _ = sys.syscall3(263, @bitCast(@as(isize, -100)), @intFromPtr(path), 0); // unlinkat
}

pub fn makeDir(path: []const u8, mode: usize) void {
    var buf: [1024]u8 = undefined;
    const p = rt.cstr(&buf, path) orelse return;
    _ = sys.syscall3(258, @bitCast(@as(isize, -100)), @intFromPtr(p), mode); // mkdirat
}

/// Makes every directory on the way to `path` (which is a directory itself).
pub fn makePath(path: []const u8) void {
    var i: usize = 0;
    while (i < path.len) : (i += 1) {
        if (path[i] == '/' and i > 0) makeDir(path[0..i], 0o755);
    }
    if (path.len > 0) makeDir(path, 0o755);
}

/// Makes the directories a file sits in.
pub fn makeParents(path: []const u8) void {
    if (std.mem.lastIndexOfScalar(u8, path, '/')) |cut| {
        if (cut > 0) makePath(path[0..cut]);
    }
}
