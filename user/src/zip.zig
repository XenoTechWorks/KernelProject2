//! zip and unzip: the .zip archives Windows uses. `unzip` is the same program under another name.
//!
//!     zip -r notes.zip notes/     pack a folder
//!     unzip -l notes.zip          list what's inside
//!     unzip notes.zip -d /tmp     unpack it into /tmp
//!
//! Entries are either stored or deflated, which is what everything in practice uses.

const std = @import("std");
const rt = @import("lib/rt.zig");
const file = @import("lib/file.zig");
const deflate = @import("lib/deflate.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const gpa = std.heap.page_allocator;

const local_sig = 0x04034b50;
const central_sig = 0x02014b50;
const end_sig = 0x06054b50;

const zip_usage =
    \\usage: zip [-r] ARCHIVE FILE...
    \\  -r  include what's inside folders
    \\
;
const unzip_usage =
    \\usage: unzip [-l] ARCHIVE [-d DIR]
    \\  -l  list, don't unpack
    \\  -d  unpack into this directory
    \\
;

var verbose = true;

pub fn main(args: rt.Args) u8 {
    const me = args.get(0) orelse "zip";
    const name = me[if (std.mem.lastIndexOfScalar(u8, me, '/')) |i| i + 1 else 0..];
    return if (std.mem.startsWith(u8, name, "unzip")) unzipMain(args) else zipMain(args);
}

// ---- reading ----

fn readU16(d: []const u8, at: usize) u16 {
    return std.mem.readInt(u16, d[at..][0..2], .little);
}
fn readU32(d: []const u8, at: usize) u32 {
    return std.mem.readInt(u32, d[at..][0..4], .little);
}

fn unzipMain(args: rt.Args) u8 {
    var list_only = false;
    var archive: ?[]const u8 = null;
    var dir: ?[]const u8 = null;
    var i: usize = 1;
    while (args.get(i)) |arg| : (i += 1) {
        if (std.mem.eql(u8, arg, "-l")) {
            list_only = true;
        } else if (std.mem.eql(u8, arg, "-d")) {
            i += 1;
            dir = args.get(i);
        } else if (std.mem.eql(u8, arg, "-h")) {
            rt.out.write(unzip_usage);
            return 0;
        } else if (archive == null) {
            archive = arg;
        }
    }
    const path = archive orelse {
        rt.err.write(unzip_usage);
        return 2;
    };
    var buf: [1100]u8 = undefined;
    const p = rt.cstr(&buf, path) orelse return 1;
    const data = file.read(p) catch {
        rt.err.print("unzip: can't read {s}\n", .{path});
        return 1;
    };
    defer gpa.free(data);

    // The central directory is listed at the end; find the record that points at it.
    var eocd: ?usize = null;
    var at = data.len -| 22;
    while (true) {
        if (at + 22 <= data.len and readU32(data, at) == end_sig) {
            eocd = at;
            break;
        }
        if (at == 0) break;
        at -= 1;
    }
    const end = eocd orelse {
        rt.err.print("unzip: {s} is not a zip file\n", .{path});
        return 1;
    };
    const count = readU16(data, end + 10);
    var entry: usize = readU32(data, end + 16);

    if (dir) |d| {
        file.makePath(d);
        var dbuf: [1100]u8 = undefined;
        if (rt.cstr(&dbuf, d)) |dp| _ = sys.syscall1(sys.nr.chdir, @intFromPtr(dp));
    }
    if (list_only) rt.out.write("  Length  Name\n");

    var status: u8 = 0;
    for (0..count) |_| {
        if (entry + 46 > data.len or readU32(data, entry) != central_sig) break;
        const method = readU16(data, entry + 10);
        const want_crc = readU32(data, entry + 16);
        const packed_size = readU32(data, entry + 20);
        const full_size = readU32(data, entry + 24);
        const name_len = readU16(data, entry + 28);
        const extra_len = readU16(data, entry + 30);
        const comment_len = readU16(data, entry + 32);
        const attrs = readU32(data, entry + 38);
        const local = readU32(data, entry + 42);
        const name = data[entry + 46 ..][0..name_len];
        entry += 46 + @as(usize, name_len) + extra_len + comment_len;

        if (list_only) {
            rt.out.print("{d: >8}  {s}\n", .{ full_size, name });
            continue;
        }
        if (name.len == 0 or name[0] == '/' or std.mem.indexOf(u8, name, "..") != null) {
            rt.err.print("unzip: skipping {s}\n", .{name});
            status = 1;
            continue;
        }
        if (name[name.len - 1] == '/') {
            file.makePath(name[0 .. name.len - 1]);
            continue;
        }
        // The body sits after the local header, whose name and extra may differ in length.
        if (local + 30 > data.len or readU32(data, local) != local_sig) {
            status = 1;
            continue;
        }
        const body = local + 30 + @as(usize, readU16(data, local + 26)) + readU16(data, local + 28);
        if (body + packed_size > data.len) {
            status = 1;
            continue;
        }
        const raw = data[body..][0..packed_size];
        var contents: []const u8 = raw;
        var inflated: ?[]u8 = null;
        defer if (inflated) |x| gpa.free(x);
        switch (method) {
            0 => {},
            8 => {
                inflated = deflate.inflate(raw) catch {
                    rt.err.print("unzip: {s} is damaged\n", .{name});
                    status = 1;
                    continue;
                };
                contents = inflated.?;
            },
            else => {
                rt.err.print("unzip: {s} uses compression {d}, which isn't supported\n", .{ name, method });
                status = 1;
                continue;
            },
        }
        if (contents.len != full_size or deflate.crc32(contents) != want_crc) {
            rt.err.print("unzip: {s} is damaged\n", .{name});
            status = 1;
            continue;
        }
        file.makeParents(name);
        var nbuf: [1100]u8 = undefined;
        const dest = rt.cstr(&nbuf, name) orelse continue;
        // The high bits of the external attributes hold a Unix mode, when it came from Unix.
        const mode = if (attrs >> 16 & 0o777 != 0) attrs >> 16 & 0o7777 else 0o644;
        file.write(dest, contents, mode) catch {
            rt.err.print("unzip: can't write {s}\n", .{name});
            status = 1;
            continue;
        };
        if (verbose) rt.out.print("  {s}\n", .{name});
    }
    return status;
}

// ---- writing ----

const Entry = struct { name: []const u8, crc: u32, packed_size: u32, full_size: u32, method: u16, at: u32, mode: u32 };

fn zipMain(args: rt.Args) u8 {
    var recurse = false;
    var archive: ?[]const u8 = null;
    var names: [64][]const u8 = undefined;
    var n: usize = 0;
    var i: usize = 1;
    while (args.get(i)) |arg| : (i += 1) {
        if (std.mem.eql(u8, arg, "-r")) {
            recurse = true;
        } else if (std.mem.eql(u8, arg, "-h")) {
            rt.out.write(zip_usage);
            return 0;
        } else if (archive == null) {
            archive = arg;
        } else if (n < names.len) {
            names[n] = arg;
            n += 1;
        }
    }
    const path = archive orelse {
        rt.err.write(zip_usage);
        return 2;
    };
    if (n == 0) {
        rt.err.write(zip_usage);
        return 2;
    }

    var out = std.ArrayList(u8).empty;
    defer out.deinit(gpa);
    var entries = std.ArrayList(Entry).empty;
    defer entries.deinit(gpa);
    var status: u8 = 0;
    for (names[0..n]) |f| {
        if (!addPath(&out, &entries, std.mem.trimEnd(u8, f, "/"), recurse)) status = 1;
    }

    // The central directory: the same facts again, so a reader can find everything at once.
    const dir_at = out.items.len;
    for (entries.items) |e| {
        var h: [46]u8 = @splat(0);
        std.mem.writeInt(u32, h[0..4], central_sig, .little);
        std.mem.writeInt(u16, h[4..6], 0x031E, .little); // made by Unix, version 3.0
        std.mem.writeInt(u16, h[6..8], 20, .little);
        std.mem.writeInt(u16, h[10..12], e.method, .little);
        std.mem.writeInt(u32, h[16..20], e.crc, .little);
        std.mem.writeInt(u32, h[20..24], e.packed_size, .little);
        std.mem.writeInt(u32, h[24..28], e.full_size, .little);
        std.mem.writeInt(u16, h[28..30], @intCast(e.name.len), .little);
        std.mem.writeInt(u32, h[38..42], e.mode << 16, .little);
        std.mem.writeInt(u32, h[42..46], e.at, .little);
        out.appendSlice(gpa, &h) catch return 1;
        out.appendSlice(gpa, e.name) catch return 1;
    }
    var end: [22]u8 = @splat(0);
    std.mem.writeInt(u32, end[0..4], end_sig, .little);
    std.mem.writeInt(u16, end[8..10], @intCast(entries.items.len), .little);
    std.mem.writeInt(u16, end[10..12], @intCast(entries.items.len), .little);
    std.mem.writeInt(u32, end[12..16], @intCast(out.items.len - dir_at), .little);
    std.mem.writeInt(u32, end[16..20], @intCast(dir_at), .little);
    out.appendSlice(gpa, &end) catch return 1;

    var buf: [1100]u8 = undefined;
    const dest = rt.cstr(&buf, path) orelse return 1;
    file.write(dest, out.items, 0o644) catch {
        rt.err.print("zip: can't write {s}\n", .{path});
        return 1;
    };
    return status;
}

fn addPath(out: *std.ArrayList(u8), entries: *std.ArrayList(Entry), path: []const u8, recurse: bool) bool {
    var buf: [1100]u8 = undefined;
    const p = rt.cstr(&buf, path) orelse return false;
    var st: sys.Stat = undefined;
    if (sys.stat(p, &st) != .ok) {
        rt.err.print("zip: can't find {s}\n", .{path});
        return false;
    }
    if (st.mode & sys.S_IFMT == sys.S_IFDIR) {
        if (!recurse) return true;
        var name: [1100]u8 = undefined;
        const with_slash = std.fmt.bufPrint(&name, "{s}/", .{path}) catch return false;
        if (!addEntry(out, entries, with_slash, "", 0o755)) return false;
        return addChildren(out, entries, path);
    }
    const data = file.read(p) catch {
        rt.err.print("zip: can't read {s}\n", .{path});
        return false;
    };
    defer gpa.free(data);
    return addEntry(out, entries, path, data, @intCast(st.mode & 0o7777));
}

fn addEntry(out: *std.ArrayList(u8), entries: *std.ArrayList(Entry), name: []const u8, data: []const u8, mode: u32) bool {
    // Keep whichever is smaller: the deflated bytes, or the bytes as they are.
    var method: u16 = 0;
    var body: []const u8 = data;
    var squeezed: ?[]u8 = null;
    defer if (squeezed) |s| gpa.free(s);
    if (data.len > 0) {
        if (deflate.deflate(data)) |d| {
            if (d.len < data.len) {
                squeezed = d;
                body = d;
                method = 8;
            } else {
                gpa.free(d);
            }
        } else |_| {}
    }
    const at: u32 = @intCast(out.items.len);
    var h: [30]u8 = @splat(0);
    std.mem.writeInt(u32, h[0..4], local_sig, .little);
    std.mem.writeInt(u16, h[4..6], 20, .little);
    std.mem.writeInt(u16, h[8..10], method, .little);
    std.mem.writeInt(u32, h[14..18], deflate.crc32(data), .little);
    std.mem.writeInt(u32, h[18..22], @intCast(body.len), .little);
    std.mem.writeInt(u32, h[22..26], @intCast(data.len), .little);
    std.mem.writeInt(u16, h[26..28], @intCast(name.len), .little);
    out.appendSlice(gpa, &h) catch return false;
    out.appendSlice(gpa, name) catch return false;
    out.appendSlice(gpa, body) catch return false;
    entries.append(gpa, .{
        .name = gpa.dupe(u8, name) catch return false,
        .crc = deflate.crc32(data),
        .packed_size = @intCast(body.len),
        .full_size = @intCast(data.len),
        .method = method,
        .at = at,
        .mode = mode,
    }) catch return false;
    if (verbose) rt.out.print("  {s}\n", .{name});
    return true;
}

fn addChildren(out: *std.ArrayList(u8), entries: *std.ArrayList(Entry), path: []const u8) bool {
    var buf: [1100]u8 = undefined;
    const p = rt.cstr(&buf, path) orelse return false;
    const fd: i32 = switch (sys.open(p, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return false,
    };
    defer sys.close(fd);
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
        const full = std.fmt.bufPrint(&child, "{s}/{s}", .{ path, names.items[start..end] }) catch return false;
        if (!addPath(out, entries, full, true)) ok = false;
        start = end;
    }
    return ok;
}
