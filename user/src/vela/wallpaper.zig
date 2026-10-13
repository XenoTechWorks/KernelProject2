//! Vela's wallpaper: a night sky, with the stars of Vela (the constellation) joined up on the right.

const std = @import("std");
const gui = @import("../lib/gui.zig");
const Canvas = gui.Canvas;

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

pub fn paint(c: *Canvas) void {
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
