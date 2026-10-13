//! Glass, after Windows Vista's Aero: what's behind shows through, blurred and tinted, with a soft
//! shine over the top half. Vela's bar and dock always have the wallpaper behind them (the kernel
//! keeps windows out of their strips), so the glass is the wallpaper itself, blurred here.

const std = @import("std");
const gui = @import("../lib/gui.zig");
const Canvas = gui.Canvas;
const Rect = gui.Rect;

pub const Style = struct {
    /// The glass's own color, and how much of it (0..255) over the blurred background.
    tint: u32 = 0x0c1018,
    strength: u32 = 150,
    /// How far the blur reaches (pixels, each of three passes).
    radius: u32 = 9,
    /// The shine over the top half (alpha of white at the very top).
    shine: u32 = 34,
};

/// Fills `c` with glass over the part of `sky` (sky_w pixels wide) at (x, y) on the screen.
pub fn frost(c: *Canvas, sky: []const u32, sky_w: u32, x: i32, y: i32, style: Style) void {
    const sky_h: u32 = @intCast(sky.len / sky_w);
    // The background, a little bigger than the glass so the blur at its edges sees what's beyond.
    const m: i32 = @intCast(style.radius * 3);
    const bw: u32 = c.w + @as(u32, @intCast(2 * m));
    const bh: u32 = c.h + @as(u32, @intCast(2 * m));
    const buf = std.heap.page_allocator.alloc(u32, bw * bh) catch return;
    defer std.heap.page_allocator.free(buf);
    const tmp = std.heap.page_allocator.alloc(u32, bw * bh) catch return;
    defer std.heap.page_allocator.free(tmp);
    for (0..bh) |by| {
        const sy: u32 = @intCast(std.math.clamp(y - m + @as(i32, @intCast(by)), 0, @as(i32, @intCast(sky_h)) - 1));
        for (0..bw) |bx| {
            const sx: u32 = @intCast(std.math.clamp(x - m + @as(i32, @intCast(bx)), 0, @as(i32, @intCast(sky_w)) - 1));
            buf[by * bw + bx] = sky[sy * sky_w + sx];
        }
    }
    // Three box blurs each way come close to a Gaussian blur.
    for (0..3) |_| {
        boxBlur(buf, tmp, bw, bh, style.radius, true);
        boxBlur(tmp, buf, bw, bh, style.radius, false);
    }
    for (0..c.h) |cy| {
        // The shine: strongest at the top, gone by the middle.
        const half = @max(1, c.h / 2);
        const shine: u32 = if (cy < half) style.shine * (half - @as(u32, @intCast(cy))) / half else 0;
        for (0..c.w) |cx| {
            const behind = buf[(cy + @as(usize, @intCast(m))) * bw + cx + @as(usize, @intCast(m))];
            var p = gui.mix(style.tint, behind, style.strength);
            if (shine > 0) p = gui.mix(0xffffff, p, shine);
            c.pixels[cy * c.w + cx] = p;
        }
    }
    // A thin bright line along the top edge and a dark one along the bottom, like an edge of glass.
    lineAlpha(c, 0, 0xffffff, 60);
    lineAlpha(c, @intCast(c.h - 1), 0x000000, 90);
}

/// Averages each pixel with its neighbors `r` to either side (horizontally or vertically).
fn boxBlur(src: []const u32, dst: []u32, w: u32, h: u32, r: u32, horizontal: bool) void {
    const n: u32 = if (horizontal) w else h;
    const lines: u32 = if (horizontal) h else w;
    const div = 2 * r + 1;
    for (0..lines) |line| {
        const at = struct {
            fn f(l: usize, i: i64, nn: u32, ww: u32, horiz: bool) usize {
                const k: usize = @intCast(std.math.clamp(i, 0, @as(i64, nn) - 1));
                return if (horiz) l * ww + k else k * ww + l;
            }
        }.f;
        var sum: [3]u32 = @splat(0);
        var i: i64 = -@as(i64, r);
        while (i <= r) : (i += 1) add(&sum, src[at(line, i, n, w, horizontal)], 1);
        for (0..n) |k| {
            dst[at(line, @intCast(k), n, w, horizontal)] = (sum[0] / div) << 16 | (sum[1] / div) << 8 | sum[2] / div;
            add(&sum, src[at(line, @as(i64, @intCast(k)) + r + 1, n, w, horizontal)], 1);
            add(&sum, src[at(line, @as(i64, @intCast(k)) - r, n, w, horizontal)], -1);
        }
    }
}

fn add(sum: *[3]u32, p: u32, sign: i32) void {
    const ch = [3]u32{ p >> 16 & 0xff, p >> 8 & 0xff, p & 0xff };
    for (0..3) |k| {
        if (sign > 0) sum[k] += ch[k] else sum[k] -= ch[k];
    }
}

fn lineAlpha(c: *Canvas, y: i32, color: u32, alpha: u32) void {
    var x: i32 = 0;
    while (x < c.w) : (x += 1) c.blend(x, y, color, alpha);
}

/// Lays `color` over a rounded rectangle, `alpha` of 255 strong (hover and pressed glows).
pub fn tint(c: *Canvas, r: Rect, radius: f32, color: u32, alpha: u32) void {
    var y = r.y;
    while (y < r.bottom()) : (y += 1) {
        var x = r.x;
        while (x < r.right()) : (x += 1) {
            const cover = coverage(r, radius, x, y);
            if (cover > 0) c.blend(x, y, color, @intFromFloat(cover * @as(f32, @floatFromInt(alpha))));
        }
    }
}

/// A rounded rectangle's outline, 1 pixel, `alpha` strong.
pub fn outline(c: *Canvas, r: Rect, radius: f32, color: u32, alpha: u32) void {
    var y = r.y;
    while (y < r.bottom()) : (y += 1) {
        var x = r.x;
        while (x < r.right()) : (x += 1) {
            const inner = Rect{ .x = r.x + 1, .y = r.y + 1, .w = r.w - 2, .h = r.h - 2 };
            const cover = coverage(r, radius, x, y) - coverage(inner, @max(0, radius - 1), x, y);
            if (cover > 0) c.blend(x, y, color, @intFromFloat(cover * @as(f32, @floatFromInt(alpha))));
        }
    }
}

/// How much of pixel (x, y) a rounded rectangle covers (0..1).
fn coverage(r: Rect, radius: f32, x: i32, y: i32) f32 {
    const hw = @as(f32, @floatFromInt(r.w)) / 2;
    const hh = @as(f32, @floatFromInt(r.h)) / 2;
    const qx = @abs(@as(f32, @floatFromInt(x - r.x)) + 0.5 - hw) - (hw - radius);
    const qy = @abs(@as(f32, @floatFromInt(y - r.y)) + 0.5 - hh) - (hh - radius);
    const d = @sqrt(@max(qx, 0) * @max(qx, 0) + @max(qy, 0) * @max(qy, 0)) + @min(@max(qx, qy), 0) - radius;
    return std.math.clamp(0.5 - d, 0, 1);
}

/// Vela's orb (the star menu's button): a glossy blue sphere with a white star, glowing when the
/// pointer is over it or the menu is open.
pub fn orb(c: *Canvas, cx: f32, cy: f32, r: f32, lit: bool, star: *const fn (*Canvas, f32, f32, f32, u32) void) void {
    if (lit) glow(c, cx, cy, r + 6, 0x7fc4ff, 90);
    c.circle(cx, cy, r + 1, 0x08101e); // dark rim
    // The body: from a deep blue at the edge to a bright blue low in the middle (light comes
    // through the sphere from above and collects at the bottom).
    const steps: u32 = 12;
    for (0..steps) |i| {
        const t = @as(f32, @floatFromInt(i)) / @as(f32, @floatFromInt(steps));
        const color = gui.mix(if (lit) 0x5fb2ff else 0x3d93f0, 0x0a3a8c, @intFromFloat(t * 255));
        c.circle(cx, cy + r * 0.18 * t, r * (1 - t * 0.85), color);
    }
    star(c, cx, cy + 0.5, r * 0.55, 0xffffff);
    // The gloss: a soft white oval over the top half.
    gloss(c, cx, cy - r * 0.42, r * 0.72, r * 0.45, 120);
}

/// A glossy ball: `top` fading to `bottom` downward, a dark rim, and a shine over its top half
/// (the window color swatches).
pub fn sphere(c: *Canvas, cx: f32, cy: f32, r: f32, top: u32, bottom: u32) void {
    c.circle(cx, cy, r + 1, 0x07090d);
    const y0: i32 = @intFromFloat(@floor(cy - r - 1));
    var y = y0;
    while (y <= @as(i32, @intFromFloat(@ceil(cy + r + 1)))) : (y += 1) {
        const t = std.math.clamp((@as(f32, @floatFromInt(y)) + 0.5 - (cy - r)) / (2 * r), 0, 1);
        const color = gui.mix(bottom, top, @intFromFloat(t * 255));
        var x: i32 = @intFromFloat(@floor(cx - r - 1));
        while (x <= @as(i32, @intFromFloat(@ceil(cx + r + 1)))) : (x += 1) {
            const dx = @as(f32, @floatFromInt(x)) + 0.5 - cx;
            const dy = @as(f32, @floatFromInt(y)) + 0.5 - cy;
            const cover = std.math.clamp(r - @sqrt(dx * dx + dy * dy) + 0.5, 0, 1);
            c.blend(x, y, color, @intFromFloat(cover * 255));
        }
    }
    gloss(c, cx, cy - r * 0.42, r * 0.7, r * 0.45, 110);
}

/// A soft round light: `alpha` at the center fading to nothing at `r`.
fn glow(c: *Canvas, cx: f32, cy: f32, r: f32, color: u32, alpha: u32) void {
    const x0: i32 = @intFromFloat(@floor(cx - r));
    const y0: i32 = @intFromFloat(@floor(cy - r));
    var y = y0;
    while (y <= @as(i32, @intFromFloat(@ceil(cy + r)))) : (y += 1) {
        var x = x0;
        while (x <= @as(i32, @intFromFloat(@ceil(cx + r)))) : (x += 1) {
            const dx = @as(f32, @floatFromInt(x)) + 0.5 - cx;
            const dy = @as(f32, @floatFromInt(y)) + 0.5 - cy;
            const t = @max(0, 1 - @sqrt(dx * dx + dy * dy) / r);
            c.blend(x, y, color, @intFromFloat(t * t * @as(f32, @floatFromInt(alpha))));
        }
    }
}

/// White fading downward inside an oval: the shine on a glossy button.
fn gloss(c: *Canvas, cx: f32, cy: f32, rx: f32, ry: f32, alpha: u32) void {
    const x0: i32 = @intFromFloat(@floor(cx - rx));
    const y0: i32 = @intFromFloat(@floor(cy - ry));
    var y = y0;
    while (y <= @as(i32, @intFromFloat(@ceil(cy + ry)))) : (y += 1) {
        var x = x0;
        while (x <= @as(i32, @intFromFloat(@ceil(cx + rx)))) : (x += 1) {
            const dx = (@as(f32, @floatFromInt(x)) + 0.5 - cx) / rx;
            const dy = (@as(f32, @floatFromInt(y)) + 0.5 - cy) / ry;
            const d = @sqrt(dx * dx + dy * dy);
            if (d >= 1) continue;
            const edge = std.math.clamp((1 - d) * rx, 0, 1); // smooth the oval's edge
            const fade = std.math.clamp(0.5 - dy * 0.5, 0, 1); // brighter at the top
            c.blend(x, y, 0xffffff, @intFromFloat(edge * fade * @as(f32, @floatFromInt(alpha))));
        }
    }
}
