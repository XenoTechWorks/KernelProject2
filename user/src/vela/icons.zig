//! Icons: which Oxygen icon (lib/gui/icon.zig) each app and star-menu row has. Also Vela's star.

const std = @import("std");
const gui = @import("../lib/gui.zig");
const icon = @import("../lib/gui/icon.zig");
const Icon = @import("apps.zig").Icon;
const Canvas = gui.Canvas;

/// Pictures that aren't apps: the star menu's own rows.
pub const Extra = enum { wallpaper, leave };

pub const Which = union(enum) { app: Icon, extra: Extra };

/// The Oxygen icon for each.
pub fn fileName(which: Which) []const u8 {
    return switch (which) {
        .app => |a| switch (a) {
            .terminal => "utilities-terminal",
            .files => "system-file-manager",
            .editor => "accessories-text-editor",
            .calculator => "accessories-calculator",
            .clock => "clock",
            .widgets => "preferences-desktop",
            .eyes => "applications-toys",
            .doom => "applications-games",
            .installer => "system-software-install",
        },
        .extra => |e| switch (e) {
            .wallpaper => "preferences-desktop-wallpaper",
            .leave => "system-log-out",
        },
    };
}

/// Draws an app's icon in the s x s square at (x, y).
pub fn draw(c: *Canvas, a: Icon, x: i32, y: i32, s: u32) void {
    drawWhich(c, .{ .app = a }, x, y, s);
}

pub fn drawExtra(c: *Canvas, e: Extra, x: i32, y: i32, s: u32) void {
    drawWhich(c, .{ .extra = e }, x, y, s);
}

fn drawWhich(c: *Canvas, which: Which, x: i32, y: i32, s: u32) void {
    if (!icon.draw(c, fileName(which), x, y, s)) {
        c.roundRect(.{ .x = x, .y = y, .w = @intCast(s), .h = @intCast(s) }, 8, 0x4b5365);
    }
}

/// Vela's five-pointed star, `r` from its center to its points.
pub fn star(c: *Canvas, cx: f32, cy: f32, r: f32, color: u32) void {
    var pts: [10][2]f32 = undefined;
    for (&pts, 0..) |*p, i| {
        const angle = -std.math.pi / 2.0 + @as(f32, @floatFromInt(i)) * std.math.pi / 5.0;
        const d = if (i % 2 == 0) r else r * 0.45;
        p.* = .{ cx + d * @cos(angle), cy + d * @sin(angle) };
    }
    fillPolygon(c, &pts, color);
}

/// Fills a polygon, smoothing its edges by sampling each pixel 4 x 4 times.
pub fn fillPolygon(c: *Canvas, pts: []const [2]f32, color: u32) void {
    var x0: f32 = pts[0][0];
    var x1: f32 = x0;
    var y0: f32 = pts[0][1];
    var y1: f32 = y0;
    for (pts) |p| {
        x0 = @min(x0, p[0]);
        x1 = @max(x1, p[0]);
        y0 = @min(y0, p[1]);
        y1 = @max(y1, p[1]);
    }
    var y: i32 = @intFromFloat(@floor(y0));
    while (y <= @as(i32, @intFromFloat(@ceil(y1)))) : (y += 1) {
        var x: i32 = @intFromFloat(@floor(x0));
        while (x <= @as(i32, @intFromFloat(@ceil(x1)))) : (x += 1) {
            var hits: u32 = 0;
            var sy: u32 = 0;
            while (sy < 4) : (sy += 1) {
                var sx: u32 = 0;
                while (sx < 4) : (sx += 1) {
                    const qx = @as(f32, @floatFromInt(x)) + (@as(f32, @floatFromInt(sx)) + 0.5) / 4;
                    const qy = @as(f32, @floatFromInt(y)) + (@as(f32, @floatFromInt(sy)) + 0.5) / 4;
                    if (inside(pts, qx, qy)) hits += 1;
                }
            }
            c.blend(x, y, color, hits * 255 / 16);
        }
    }
}

/// Even-odd rule: a ray to the right crosses the edges an odd number of times.
fn inside(pts: []const [2]f32, x: f32, y: f32) bool {
    var in = false;
    var j = pts.len - 1;
    for (pts, 0..) |p, i| {
        const q = pts[j];
        if ((p[1] > y) != (q[1] > y) and x < (q[0] - p[0]) * (y - p[1]) / (q[1] - p[1]) + p[0]) in = !in;
        j = i;
    }
    return in;
}
