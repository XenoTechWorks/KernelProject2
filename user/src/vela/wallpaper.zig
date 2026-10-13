//! Vela's wallpaper: a photo of the Vela supernova remnant (ESO, see
//! /usr/share/backgrounds/vela.txt), or, when it's missing, a night sky drawn here with the stars
//! of Vela joined up on the right.

const std = @import("std");
const gui = @import("../lib/gui.zig");
const file = @import("../lib/file.zig");
const gzip = @import("../lib/gzip.zig");
const Canvas = gui.Canvas;

const photo_path = "/usr/share/backgrounds/vela.ppm.gz";

/// Fills the canvas with the photo, or the drawn sky if the photo can't be read.
pub fn paint(c: *Canvas) void {
    if (!paintPhoto(c, photo_path)) paintSky(c);
}

/// A gzipped binary PPM (P6, 8 bits), scaled to cover the canvas (cut at the sides or top and
/// bottom if the shape differs) and dimmed a little so icons and their labels stand out.
fn paintPhoto(c: *Canvas, path: [*:0]const u8) bool {
    const packed_data = file.read(path) catch return false;
    defer std.heap.page_allocator.free(packed_data);
    const ppm = gzip.decompress(packed_data) catch return false;
    defer std.heap.page_allocator.free(ppm);

    // The header: "P6", width, height, maxval, then one whitespace character and the pixels.
    var fields: [4]u32 = undefined;
    var at: usize = 0;
    for (&fields, 0..) |*f, i| {
        while (at < ppm.len and std.ascii.isWhitespace(ppm[at])) at += 1;
        const start = at;
        while (at < ppm.len and !std.ascii.isWhitespace(ppm[at])) at += 1;
        const word = ppm[start..at];
        if (i == 0) {
            if (!std.mem.eql(u8, word, "P6")) return false;
            f.* = 0;
        } else {
            f.* = std.fmt.parseInt(u32, word, 10) catch return false;
        }
    }
    at += 1;
    const pw = fields[1];
    const ph = fields[2];
    if (fields[3] != 255 or pw == 0 or ph == 0 or ppm.len < at + @as(usize, pw) * ph * 3) return false;
    const pixels = ppm[at..];

    // Cover: the same scale both ways, big enough to fill the canvas, centered.
    const sx = @as(f32, @floatFromInt(pw)) / @as(f32, @floatFromInt(c.w));
    const sy = @as(f32, @floatFromInt(ph)) / @as(f32, @floatFromInt(c.h));
    const scale = @min(sx, sy);
    const ox = (@as(f32, @floatFromInt(pw)) - scale * @as(f32, @floatFromInt(c.w))) / 2;
    const oy = (@as(f32, @floatFromInt(ph)) - scale * @as(f32, @floatFromInt(c.h))) / 2;
    var y: u32 = 0;
    while (y < c.h) : (y += 1) {
        const py: u32 = @min(ph - 1, @as(u32, @intFromFloat(oy + (@as(f32, @floatFromInt(y)) + 0.5) * scale)));
        var x: u32 = 0;
        while (x < c.w) : (x += 1) {
            const px: u32 = @min(pw - 1, @as(u32, @intFromFloat(ox + (@as(f32, @floatFromInt(x)) + 0.5) * scale)));
            const p = pixels[(@as(usize, py) * pw + px) * 3 ..][0..3];
            const dimmed = [3]u32{ @as(u32, p[0]) * 218 / 255, @as(u32, p[1]) * 218 / 255, @as(u32, p[2]) * 218 / 255 };
            c.pixels[y * c.w + x] = dimmed[0] << 16 | dimmed[1] << 8 | dimmed[2];
        }
    }
    return true;
}

const top = 0x0a0f1f;
const bottom = 0x1a2245;

/// Soft light in the sky: where (as fractions of the screen), how far it reaches, its color.
const Glow = struct { x: f32, y: f32, reach: f32, color: u32, strength: f32 };
const glows = [_]Glow{
    .{ .x = 0.74, .y = 0.3, .reach = 0.55, .color = 0x3d2c70, .strength = 0.6 },
    .{ .x = 0.15, .y = 0.85, .reach = 0.5, .color = 0x123a5c, .strength = 0.55 },
};

/// The constellation, as fractions of the screen, joined in this order.
const vela = [_][2]f32{ .{ 0.66, 0.26 }, .{ 0.73, 0.2 }, .{ 0.82, 0.23 }, .{ 0.87, 0.34 }, .{ 0.8, 0.43 }, .{ 0.7, 0.4 } };

fn paintSky(c: *Canvas) void {
    const w: f32 = @floatFromInt(c.w);
    const h: f32 = @floatFromInt(c.h);
    var y: u32 = 0;
    while (y < c.h) : (y += 1) {
        const fy = @as(f32, @floatFromInt(y)) / h;
        const base = gui.mix(bottom, top, @intFromFloat(fy * 255));
        var x: u32 = 0;
        while (x < c.w) : (x += 1) {
            const fx = @as(f32, @floatFromInt(x)) / w;
            var color = base;
            for (glows) |g| {
                // Distance in screen widths, so the glow is round.
                const dx = fx - g.x;
                const dy = (fy - g.y) * h / w;
                const t = @max(0, 1 - @sqrt(dx * dx + dy * dy) / g.reach);
                color = gui.mix(g.color, color, @intFromFloat(t * t * g.strength * 255));
            }
            c.pixels[y * c.w + x] = color;
        }
    }

    // Stars: always the same ones (a fixed seed), mostly faint points, a few brighter.
    var seed: u32 = 0x5eed_7a1a;
    var n: u32 = 0;
    while (n < 320) : (n += 1) {
        const sx: f32 = @floatFromInt(next(&seed) % c.w);
        const sy: f32 = @floatFromInt(next(&seed) % c.h);
        const brightness = 60 + next(&seed) % 196;
        const tint: u32 = if (next(&seed) % 4 == 0) 0xffe9c8 else 0xdfe7ff;
        if (next(&seed) % 12 == 0) {
            c.circle(sx, sy, 1.3, gui.mix(tint, 0x10162e, brightness));
        } else {
            c.blend(@intFromFloat(sx), @intFromFloat(sy), tint, brightness);
        }
    }

    // Vela, faintly joined, its stars a little bigger with a halo.
    for (vela, 0..) |p, i| {
        const q = vela[(i + 1) % vela.len];
        c.line(p[0] * w, p[1] * h, q[0] * w, q[1] * h, 1.2, 0x39457a);
    }
    for (vela) |p| {
        c.circle(p[0] * w, p[1] * h, 6, 0x252f5c);
        c.circle(p[0] * w, p[1] * h, 2.4, 0xeef2ff);
    }
}

/// A small, fixed sequence of numbers (xorshift).
fn next(s: *u32) u32 {
    s.* ^= s.* << 13;
    s.* ^= s.* >> 17;
    s.* ^= s.* << 5;
    return s.*;
}
