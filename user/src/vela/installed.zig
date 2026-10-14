//! The games and apps Software installed (lib/software.zig), for the star menu's second column:
//! their names, how to start them and their pictures. Read again each time the menu opens, so
//! something just installed is there.

const std = @import("std");
const file = @import("../lib/file.zig");
const png = @import("../lib/png.zig");
const sw = @import("../lib/software.zig");

const gpa = std.heap.page_allocator;

pub const Entry = struct {
    name: []const u8,
    title: []const u8,
    run: []const u8,
    terminal: bool,
    picture: ?png.Image,
};

pub const max = 32;
var entries: [max]Entry = undefined;
var infos: [max][]u8 = undefined;
pub var count: usize = 0;

/// Pictures stay decoded between scans: keyed by name, freed when that package goes.
const Cached = struct { name: [64]u8 = undefined, len: usize = 0, picture: ?png.Image = null, seen: bool = false };
var cache: [max]Cached = @splat(.{});

pub fn list() []const Entry {
    return entries[0..count];
}

fn picture(name: []const u8) ?png.Image {
    for (&cache) |*c| {
        if (c.len == name.len and std.mem.eql(u8, c.name[0..c.len], name)) {
            c.seen = true;
            return c.picture;
        }
    }
    var buf: [600]u8 = undefined;
    const path = std.fmt.bufPrintSentinel(&buf, "{s}/{s}/.package/icon.png", .{ sw.store, name }, 0) catch return null;
    const decoded = if (file.read(path)) |data| blk: {
        defer gpa.free(data);
        break :blk png.decode(data) catch null;
    } else |_| null;
    for (&cache) |*c| {
        if (c.len != 0) continue;
        c.* = .{ .len = @min(name.len, c.name.len), .picture = decoded, .seen = true };
        @memcpy(c.name[0..c.len], name[0..c.len]);
        break;
    }
    return decoded;
}

pub fn scan() void {
    for (infos[0..count]) |t| gpa.free(t);
    count = 0;
    for (&cache) |*c| c.seen = false;
    var names_buf: [4096]u8 = undefined;
    var names: [max][]const u8 = undefined;
    const n = sw.installedNames(&names_buf, &names);
    for (names[0..n]) |name| {
        const info = sw.installedInfo(name) orelse continue;
        const p = sw.parseOne(info);
        if (p.run.len == 0) {
            gpa.free(info);
            continue;
        }
        infos[count] = info;
        entries[count] = .{ .name = p.name, .title = p.title, .run = p.run, .terminal = p.terminal, .picture = picture(name) };
        count += 1;
    }
    // Forget the pictures of what's been removed.
    for (&cache) |*c| {
        if (c.len != 0 and !c.seen) {
            if (c.picture) |*p| p.deinit();
            c.* = .{};
        }
    }
}
