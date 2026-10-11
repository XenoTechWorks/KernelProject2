//! tar: pack files into a .tar archive, or unpack one. With -z (or a .gz name) it runs the
//! archive through gzip, so .tar.gz works both ways.
//!
//!     tar -czf src.tar.gz user/src      pack a folder
//!     tar -tzf src.tar.gz               list what's inside
//!     tar -xzf src.tar.gz -C /tmp       unpack it into /tmp
//!
//! The headers are the usual "ustar" ones, so other tars read what this writes and the other way
//! round. Long names use the GNU 'L' entry.

const std = @import("std");
const rt = @import("lib/rt.zig");
const file = @import("lib/file.zig");
const gzip = @import("lib/gzip.zig");
const tarlib = @import("lib/tar.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const gpa = std.heap.page_allocator;
const block = 512;

const usage =
    \\usage: tar -c|-x|-t [-z] [-v] -f ARCHIVE [-C DIR] [FILE...]
    \\  -c  create    -x  extract    -t  list
    \\  -z  through gzip (assumed for a .gz or .tgz name)
    \\  -v  name each file   -f  the archive ("-" is standard in/out)
    \\  -C  work in this directory
    \\
;

var verbose = false;

pub fn main(args: rt.Args) u8 {
    var mode: u8 = 0;
    var compress = false;
    var archive: ?[]const u8 = null;
    var dir: ?[]const u8 = null;
    var files: [64][]const u8 = undefined;
    var nfiles: usize = 0;

    var i: usize = 1;
    while (args.get(i)) |arg| : (i += 1) {
        if (arg.len >= 2 and arg[0] == '-') {
            for (arg[1..]) |c| switch (c) {
                'c', 'x', 't' => mode = c,
                'z' => compress = true,
                'v' => verbose = true,
                'f' => {
                    i += 1;
                    archive = args.get(i);
                },
                'C' => {
                    i += 1;
                    dir = args.get(i);
                },
                'h' => {
                    rt.out.write(usage);
                    return 0;
                },
                else => {
                    rt.err.print("tar: unknown option -{c}\n", .{c});
                    return 2;
                },
            };
        } else if (nfiles < files.len) {
            files[nfiles] = arg;
            nfiles += 1;
        }
    }
    const path = archive orelse {
        rt.err.write(usage);
        return 2;
    };
    if (mode == 0) {
        rt.err.write(usage);
        return 2;
    }
    if (std.mem.endsWith(u8, path, ".gz") or std.mem.endsWith(u8, path, ".tgz")) compress = true;

    if (mode == 'c') return create(path, files[0..nfiles], compress, dir);
    return readArchive(path, mode, dir);
}

// ---- headers ----

fn octal(buf: []u8, value: u64) void {
    var v = value;
    var i = buf.len - 1;
    buf[i] = 0;
    while (i > 0) {
        i -= 1;
        buf[i] = '0' + @as(u8, @intCast(v & 7));
        v >>= 3;
    }
}

fn readOctal(field: []const u8) u64 {
    var v: u64 = 0;
    for (field) |c| {
        if (c < '0' or c > '7') break;
        v = v * 8 + (c - '0');
    }
    return v;
}

fn checksum(h: *[block]u8) void {
    @memset(h[148..156], ' ');
    var sum: u64 = 0;
    for (h) |b| sum += b;
    octal(h[148..155], sum);
    h[155] = ' ';
}

/// One header: the name, how big the thing is, and what kind it is.
fn header(out: *std.ArrayList(u8), name: []const u8, size: u64, kind: u8, mode: u64) !void {
    // A name too long for the field goes in a GNU 'L' entry before the real one.
    if (name.len > 100) {
        var long: [block]u8 = @splat(0);
        @memcpy(long[0..13], "././@LongLink");
        octal(long[100..108], 0o644);
        octal(long[124..136], name.len + 1);
        octal(long[136..148], 0);
        long[156] = 'L';
        @memcpy(long[257..263], "ustar ");
        @memcpy(long[263..265], " \x00");
        checksum(&long);
        try out.appendSlice(gpa, &long);
        try out.appendSlice(gpa, name);
        try out.append(gpa, 0);
        try pad(out);
    }
    var h: [block]u8 = @splat(0);
    const n = @min(name.len, 100);
    @memcpy(h[0..n], name[0..n]);
    octal(h[100..108], mode & 0o7777);
    octal(h[108..116], 0);
    octal(h[116..124], 0);
    octal(h[124..136], size);
    octal(h[136..148], 0);
    h[156] = kind;
    @memcpy(h[257..263], "ustar\x00");
    @memcpy(h[263..265], "00");
    @memcpy(h[265..269], "root");
    @memcpy(h[297..301], "root");
    checksum(&h);
    try out.appendSlice(gpa, &h);
}

/// Rounds the archive up to the next 512-byte block.
fn pad(out: *std.ArrayList(u8)) !void {
    const extra = out.items.len % block;
    if (extra != 0) try out.appendNTimes(gpa, 0, block - extra);
}

// ---- creating ----

fn create(path: []const u8, files: []const []const u8, compress: bool, dir: ?[]const u8) u8 {
    if (dir) |d| chdir(d);
    var out = std.ArrayList(u8).empty;
    defer out.deinit(gpa);
    var status: u8 = 0;
    for (files) |f| {
        const trimmed = std.mem.trimEnd(u8, f, "/");
        if (!add(&out, if (trimmed.len > 0) trimmed else f)) status = 1;
    }
    // Two empty blocks mark the end.
    out.appendNTimes(gpa, 0, 2 * block) catch return 1;

    var body: []const u8 = out.items;
    var packed_data: ?[]u8 = null;
    defer if (packed_data) |p| gpa.free(p);
    if (compress) {
        const base = path[if (std.mem.lastIndexOfScalar(u8, path, '/')) |i| i + 1 else 0..];
        packed_data = gzip.compress(out.items, base) catch {
            rt.err.write("tar: out of memory\n");
            return 1;
        };
        body = packed_data.?;
    }
    var buf: [1100]u8 = undefined;
    const dest = rt.cstr(&buf, path) orelse return 1;
    file.write(dest, body, 0o644) catch {
        rt.err.print("tar: can't write {s}\n", .{path});
        return 1;
    };
    return status;
}

/// Adds a file, or a directory and everything in it.
fn add(out: *std.ArrayList(u8), path: []const u8) bool {
    var buf: [1100]u8 = undefined;
    const p = rt.cstr(&buf, path) orelse return false;
    var st: sys.Stat = undefined;
    if (sys.lstat(p, &st) != .ok) {
        rt.err.print("tar: can't find {s}\n", .{path});
        return false;
    }
    if (verbose) rt.out.print("{s}\n", .{path});
    switch (st.mode & sys.S_IFMT) {
        sys.S_IFDIR => {
            var name: [1100]u8 = undefined;
            const with_slash = std.fmt.bufPrint(&name, "{s}/", .{path}) catch return false;
            header(out, with_slash, 0, '5', st.mode) catch return false;
            return addChildren(out, path);
        },
        sys.S_IFLNK => {
            // A link's target lives in the header, not the body.
            var target: [256]u8 = undefined;
            const n = switch (sys.check(sys.syscall4(267, @bitCast(@as(isize, -100)), @intFromPtr(p), @intFromPtr(&target), target.len))) {
                .ok => |n| n,
                .err => 0,
            };
            var h_start = out.items.len;
            header(out, path, 0, '2', st.mode) catch return false;
            const link = target[0..@min(n, 100)];
            @memcpy(out.items[h_start + 157 ..][0..link.len], link);
            const h: *[block]u8 = out.items[h_start..][0..block];
            checksum(h);
            h_start = 0;
            return true;
        },
        else => {
            const data = file.read(p) catch {
                rt.err.print("tar: can't read {s}\n", .{path});
                return false;
            };
            defer gpa.free(data);
            header(out, path, data.len, '0', st.mode) catch return false;
            out.appendSlice(gpa, data) catch return false;
            pad(out) catch return false;
            return true;
        },
    }
}

fn addChildren(out: *std.ArrayList(u8), path: []const u8) bool {
    var buf: [1100]u8 = undefined;
    const p = rt.cstr(&buf, path) orelse return false;
    const fd: i32 = switch (sys.open(p, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return false,
    };
    defer sys.close(fd);
    // The names first, then the files: reading a directory while descending into it is asking
    // for trouble.
    var names = std.ArrayList(u8).empty;
    defer names.deinit(gpa);
    var ends = std.ArrayList(usize).empty;
    defer ends.deinit(gpa);
    var dents: [8192]u8 align(8) = undefined;
    while (true) {
        const n = switch (sys.check(sys.syscall3(217, @bitCast(@as(isize, fd)), @intFromPtr(&dents), dents.len))) {
            .ok => |n| n,
            .err => break,
        };
        if (n == 0) break;
        var at: usize = 0;
        while (at < n) {
            const reclen = std.mem.readInt(u16, dents[at + 16 ..][0..2], .little);
            const entry = std.mem.sliceTo(dents[at + 19 .. at + reclen], 0);
            at += reclen;
            if (std.mem.eql(u8, entry, ".") or std.mem.eql(u8, entry, "..")) continue;
            names.appendSlice(gpa, entry) catch return false;
            ends.append(gpa, names.items.len) catch return false;
        }
    }
    var ok = true;
    var start: usize = 0;
    for (ends.items) |end| {
        var child: [1100]u8 = undefined;
        const full = std.fmt.bufPrint(&child, "{s}/{s}", .{ path, names.items[start..end] }) catch {
            ok = false;
            break;
        };
        if (!add(out, full)) ok = false;
        start = end;
    }
    return ok;
}

// ---- reading ----

fn readArchive(path: []const u8, mode: u8, dir: ?[]const u8) u8 {
    var buf: [1100]u8 = undefined;
    const p = rt.cstr(&buf, path) orelse return 1;
    const raw = file.read(p) catch {
        rt.err.print("tar: can't read {s}\n", .{path});
        return 1;
    };
    defer gpa.free(raw);
    var data = raw;
    var unpacked: ?[]u8 = null;
    defer if (unpacked) |u| gpa.free(u);
    if (gzip.looksGzipped(raw)) {
        unpacked = gzip.decompress(raw) catch {
            rt.err.print("tar: {s} is damaged\n", .{path});
            return 1;
        };
        data = unpacked.?;
    }
    if (mode == 't') {
        var r = tarlib.Reader{ .data = data };
        defer r.deinit();
        while (r.next()) |e| {
            rt.out.print("{s}\n", .{e.name});
            r.clearLong();
        }
        return 0;
    }
    if (dir) |d| {
        file.makePath(d);
        chdir(d);
    }
    return if (tarlib.extract(data, null, verbose) > 0) 1 else 0;
}


fn chdir(path: []const u8) void {
    var buf: [1100]u8 = undefined;
    const p = rt.cstr(&buf, path) orelse return;
    _ = sys.syscall1(sys.nr.chdir, @intFromPtr(p));
}
