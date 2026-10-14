//! Reading a .tar archive: walking its entries, and unpacking them into the current directory.
//! The `tar` command and the package installer both use this.

const std = @import("std");
const rt = @import("rt.zig");
const file = @import("file.zig");
const sys = rt.sys;

const gpa = std.heap.page_allocator;
pub const block = 512;

pub const Kind = enum { file, dir, symlink, other };

pub const Entry = struct {
    name: []const u8,
    kind: Kind,
    mode: u64,
    /// Where a symlink points; empty otherwise.
    link: []const u8,
    data: []const u8,
};

fn readOctal(field: []const u8) u64 {
    var v: u64 = 0;
    for (field) |c| {
        if (c < '0' or c > '7') break;
        v = v * 8 + (c - '0');
    }
    return v;
}

/// Walks the entries of an unpacked (not gzipped) tar.
pub const Reader = struct {
    data: []const u8,
    at: usize = 0,
    /// A GNU long name read from the entry before this one.
    long: ?[]u8 = null,

    pub fn deinit(r: *Reader) void {
        if (r.long) |l| gpa.free(l);
        r.long = null;
    }

    pub fn next(r: *Reader) ?Entry {
        while (r.at + block <= r.data.len) {
            const h = r.data[r.at..][0..block];
            r.at += block;
            if (h[0] == 0) return null; // the empty blocks at the end
            const size = readOctal(h[124..136]);
            const kind = h[156];
            const body = @min(size, r.data.len - r.at);
            const blocks: usize = @intCast((size + block - 1) / block * block);
            if (kind == 'L') { // a GNU long name: the real name is this entry's body
                if (r.long) |l| gpa.free(l);
                r.long = gpa.dupe(u8, std.mem.sliceTo(r.data[r.at..][0..@intCast(body)], 0)) catch null;
                r.at += blocks;
                continue;
            }
            const name = r.long orelse std.mem.sliceTo(h[0..100], 0);
            const entry = Entry{
                .name = name,
                .kind = switch (kind) {
                    '5' => .dir,
                    '2' => .symlink,
                    0, '0' => .file,
                    else => .other,
                },
                .mode = readOctal(h[100..108]),
                .link = std.mem.sliceTo(h[157..257], 0),
                .data = r.data[r.at..][0..@intCast(body)],
            };
            r.at += blocks;
            if (entry.name.len == 0) {
                r.clearLong();
                continue;
            }
            return entry;
        }
        return null;
    }

    /// Frees the long name after the caller is done with the entry that used it.
    pub fn clearLong(r: *Reader) void {
        if (r.long) |l| gpa.free(l);
        r.long = null;
    }
};

/// Is this name safe to write, relative to where we are? (No absolute paths, no "..".)
pub fn safeName(name: []const u8) bool {
    return name.len > 0 and name[0] != '/' and std.mem.indexOf(u8, name, "..") == null;
}

/// Unpacks into the current directory. Every path written is added to `record` (one per line),
/// so a package can be removed later. Returns the number of entries it couldn't handle.
pub fn extract(data: []const u8, record: ?*std.ArrayList(u8), verbose: bool) usize {
    var r = Reader{ .data = data };
    defer r.deinit();
    var failed: usize = 0;
    while (r.next()) |e| {
        defer r.clearLong();
        if (!safeName(e.name)) {
            rt.err.print("skipping {s}\n", .{e.name});
            failed += 1;
            continue;
        }
        if (verbose) rt.out.print("  {s}\n", .{e.name});
        switch (e.kind) {
            .dir => file.makePath(std.mem.trimEnd(u8, e.name, "/")),
            .symlink => {
                // Software archives list files and links without directory entries. A
                // directory containing only a symlink (Neverball's ttf/) still needs making.
                file.makeParents(e.name);
                var tbuf: [256]u8 = undefined;
                var nbuf: [1100]u8 = undefined;
                const t = rt.cstr(&tbuf, e.link);
                const n = rt.cstr(&nbuf, e.name);
                if (t == null or n == null or
                    sys.check(sys.syscall3(266, @intFromPtr(t.?), @bitCast(@as(isize, -100)), @intFromPtr(n.?))) == .err)
                {
                    rt.err.print("can't link {s}\n", .{e.name});
                    failed += 1;
                    continue;
                }
                note(record, e.name);
            },
            .file => {
                file.makeParents(e.name);
                var nbuf: [1100]u8 = undefined;
                const dest = rt.cstr(&nbuf, e.name) orelse continue;
                file.write(dest, e.data, if (e.mode == 0) 0o644 else e.mode) catch {
                    rt.err.print("can't write {s}\n", .{e.name});
                    failed += 1;
                    continue;
                };
                note(record, e.name);
            },
            .other => {},
        }
    }
    return failed;
}

fn note(record: ?*std.ArrayList(u8), name: []const u8) void {
    const list = record orelse return;
    list.appendSlice(gpa, name) catch return;
    list.append(gpa, '\n') catch return;
}
