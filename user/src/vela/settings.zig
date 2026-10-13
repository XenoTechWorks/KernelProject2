//! Vela's settings, kept in /root/.config/vela.conf (the disk keeps /root across boots), one
//! `name=value` per line:
//!
//!     wallpaper=lake-tahoe     a picture in /usr/share/backgrounds, without ".jpg"
//!     color=sky                the window color (see colors)
//!     glass=medium             clear, medium or dark
//!     autostart=yes            whether init starts Vela at boot (init reads this line itself)

const std = @import("std");
const file = @import("../lib/file.zig");

const path = "/root/.config/vela.conf";

/// A window color: the header of the window in front, from top to bottom, and the rim of light
/// inside its edge (kernel/src/console.rs). The bar and dock's glass takes its tint too.
pub const Color = struct { name: []const u8, label: []const u8, top: u32, bottom: u32, rim: u32 };

pub const colors = [_]Color{
    .{ .name = "sky", .label = "Sky", .top = 0x4d6488, .bottom = 0x253247, .rim = 0x8ea7cf },
    .{ .name = "graphite", .label = "Graphite", .top = 0x5a5f69, .bottom = 0x2a2d33, .rim = 0x9aa0aa },
    .{ .name = "teal", .label = "Teal", .top = 0x3f7f86, .bottom = 0x1d3d43, .rim = 0x83c3c9 },
    .{ .name = "leaf", .label = "Leaf", .top = 0x4f7d4c, .bottom = 0x24391f, .rim = 0x92c28c },
    .{ .name = "violet", .label = "Violet", .top = 0x6b5a92, .bottom = 0x2f2547, .rim = 0xab9bd6 },
    .{ .name = "rose", .label = "Rose", .top = 0x93566e, .bottom = 0x45212f, .rim = 0xd697ad },
    .{ .name = "amber", .label = "Amber", .top = 0x9a7342, .bottom = 0x46301a, .rim = 0xdcb27a },
    .{ .name = "ruby", .label = "Ruby", .top = 0x9a4a46, .bottom = 0x46201d, .rim = 0xdc8a85 },
};

/// How much of the glass's own tint covers what's behind it.
pub const Glass = struct { name: []const u8, label: []const u8, strength: u32 };

pub const glasses = [_]Glass{
    .{ .name = "clear", .label = "Clear", .strength = 105 },
    .{ .name = "medium", .label = "Medium", .strength = 150 },
    .{ .name = "dark", .label = "Dark", .strength = 205 },
};

var wallpaper_buf: [64]u8 = undefined;
var wallpaper_len: usize = 0;
pub var color: usize = 0;
pub var glass: usize = 1;
pub var autostart = true;

pub fn wallpaper() []const u8 {
    return if (wallpaper_len == 0) "lake-tahoe" else wallpaper_buf[0..wallpaper_len];
}

pub fn setWallpaper(slug: []const u8) void {
    const n = @min(slug.len, wallpaper_buf.len);
    std.mem.copyForwards(u8, wallpaper_buf[0..n], slug[0..n]);
    wallpaper_len = n;
}

pub fn load() void {
    const data = file.read(path) catch return;
    defer std.heap.page_allocator.free(data);
    var lines = std.mem.splitScalar(u8, data, '\n');
    while (lines.next()) |raw| {
        const line = std.mem.trim(u8, raw, " \r\t");
        const eq = std.mem.indexOfScalar(u8, line, '=') orelse continue;
        const key = line[0..eq];
        const value = line[eq + 1 ..];
        if (std.mem.eql(u8, key, "wallpaper") and value.len > 0) {
            setWallpaper(value);
        } else if (std.mem.eql(u8, key, "color")) {
            for (colors, 0..) |c, i| {
                if (std.mem.eql(u8, c.name, value)) color = i;
            }
        } else if (std.mem.eql(u8, key, "glass")) {
            for (glasses, 0..) |g, i| {
                if (std.mem.eql(u8, g.name, value)) glass = i;
            }
        } else if (std.mem.eql(u8, key, "autostart")) {
            autostart = !std.mem.eql(u8, value, "no");
        }
    }
}

pub fn save() void {
    var buf: [256]u8 = undefined;
    const text = std.fmt.bufPrint(&buf, "wallpaper={s}\ncolor={s}\nglass={s}\nautostart={s}\n", .{
        wallpaper(),
        colors[color].name,
        glasses[glass].name,
        if (autostart) "yes" else "no",
    }) catch return;
    file.makeParents(path);
    file.write(path, text, 0o644) catch {};
}
