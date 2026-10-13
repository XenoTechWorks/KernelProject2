//! App icons, drawn from shapes so they're sharp at any size: a rounded tile in the app's color
//! with a simple picture on it. Also Vela's star.

const std = @import("std");
const gui = @import("../lib/gui.zig");
const Icon = @import("apps.zig").Icon;
const Canvas = gui.Canvas;

/// Draws `icon` in the s x s square at (x, y).
pub fn draw(c: *Canvas, font: *gui.Font, icon: Icon, x: i32, y: i32, s: i32) void {
    const f: f32 = @floatFromInt(s);
    const ox: f32 = @floatFromInt(x);
    const oy: f32 = @floatFromInt(y);
    // A point in the square, from fractions of its size.
    const P = struct {
        fn x_(o: f32, size: f32, u: f32) f32 {
            return o + size * u;
        }
    };
    const px = struct {
        fn r(o: i32, size: f32, u: f32) i32 {
            return o + @as(i32, @intFromFloat(@round(size * u)));
        }
    }.r;
    const tile = gui.Rect{ .x = x, .y = y, .w = s, .h = s };
    const radius = f * 0.24;
    switch (icon) {
        .terminal => {
            c.roundRect(tile, radius, 0x2b303b);
            c.roundRectOutline(tile, radius, 1, 0x464d5c);
            const w = f * 0.075;
            c.line(P.x_(ox, f, 0.28), P.x_(oy, f, 0.36), P.x_(ox, f, 0.44), P.x_(oy, f, 0.5), w, 0x98c379);
            c.line(P.x_(ox, f, 0.44), P.x_(oy, f, 0.5), P.x_(ox, f, 0.28), P.x_(oy, f, 0.64), w, 0x98c379);
            c.line(P.x_(ox, f, 0.52), P.x_(oy, f, 0.66), P.x_(ox, f, 0.72), P.x_(oy, f, 0.66), w, 0xdcdfe4);
        },
        .files => {
            c.roundRect(tile, radius, 0x3584e4);
            c.roundRect(.{ .x = px(x, f, 0.2), .y = px(y, f, 0.27), .w = px(0, f, 0.26), .h = px(0, f, 0.16) }, f * 0.04, 0xa9cbf7);
            c.roundRect(.{ .x = px(x, f, 0.2), .y = px(y, f, 0.34), .w = px(0, f, 0.6), .h = px(0, f, 0.4) }, f * 0.05, 0xeaf3ff);
        },
        .editor => {
            c.roundRect(tile, radius, 0xe5c07b);
            c.roundRect(.{ .x = px(x, f, 0.27), .y = px(y, f, 0.18), .w = px(0, f, 0.46), .h = px(0, f, 0.64) }, f * 0.05, 0xfdfdfb);
            var i: u32 = 0;
            while (i < 4) : (i += 1) {
                const ly = P.x_(oy, f, 0.33 + 0.11 * @as(f32, @floatFromInt(i)));
                const end: f32 = if (i == 3) 0.52 else 0.64;
                c.line(P.x_(ox, f, 0.36), ly, P.x_(ox, f, end), ly, f * 0.04, 0x9aa3b1);
            }
        },
        .calculator => {
            c.roundRect(tile, radius, 0xf08a4b);
            c.roundRect(.{ .x = px(x, f, 0.22), .y = px(y, f, 0.2), .w = px(0, f, 0.56), .h = px(0, f, 0.16) }, f * 0.04, 0xfff3ea);
            var row: u32 = 0;
            while (row < 2) : (row += 1) {
                var col: u32 = 0;
                while (col < 3) : (col += 1) {
                    const kx = 0.22 + 0.2 * @as(f32, @floatFromInt(col));
                    const ky = 0.44 + 0.19 * @as(f32, @floatFromInt(row));
                    c.roundRect(.{ .x = px(x, f, kx), .y = px(y, f, ky), .w = px(0, f, 0.16), .h = px(0, f, 0.14) }, f * 0.03, 0xffd9c2);
                }
            }
        },
        .clock => {
            c.roundRect(tile, radius, 0x56b6c2);
            const cx = P.x_(ox, f, 0.5);
            const cy = P.x_(oy, f, 0.5);
            c.circle(cx, cy, f * 0.3, 0xfdfdfb);
            c.line(cx, cy, cx, cy - f * 0.19, f * 0.06, 0x2b303b);
            c.line(cx, cy, cx + f * 0.14, cy + f * 0.06, f * 0.06, 0x2b303b);
            c.circle(cx, cy, f * 0.04, 0xe06c75);
        },
        .widgets => {
            c.roundRect(tile, radius, 0xa66cd1);
            const pill = gui.Rect{ .x = px(x, f, 0.2), .y = px(y, f, 0.38), .w = px(0, f, 0.6), .h = px(0, f, 0.26) };
            c.roundRect(pill, f * 0.13, 0xf4eafb);
            c.circle(P.x_(ox, f, 0.67), P.x_(oy, f, 0.51), f * 0.1, 0xa66cd1);
        },
        .eyes => {
            c.roundRect(tile, radius, 0x4b5365);
            for ([_]f32{ 0.34, 0.66 }) |u| {
                c.ellipse(P.x_(ox, f, u), P.x_(oy, f, 0.5), f * 0.13, f * 0.2, 0xfdfdfb);
                c.circle(P.x_(ox, f, u + 0.03), P.x_(oy, f, 0.56), f * 0.06, 0x1d2129);
            }
        },
        .doom => {
            c.roundRect(tile, radius, 0x7a1d1d);
            const size = f * 0.27;
            const w = gui.measure(font, size, "DOOM");
            _ = c.text(font, size, ox + (f - w) / 2, y + @as(i32, @intFromFloat(f * 0.6)), "DOOM", 0xffc84a);
        },
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
