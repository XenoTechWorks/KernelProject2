//! Drawing into a picture in memory: rectangles with smooth rounded corners, circles and text.
//! Colors are 0xRRGGBB.

const std = @import("std");
const Font = @import("font.zig").Font;

pub const Rect = struct {
    x: i32,
    y: i32,
    w: i32,
    h: i32,

    pub fn right(r: Rect) i32 {
        return r.x + r.w;
    }
    pub fn bottom(r: Rect) i32 {
        return r.y + r.h;
    }
    pub fn contains(r: Rect, x: i32, y: i32) bool {
        return x >= r.x and x < r.right() and y >= r.y and y < r.bottom();
    }
    /// Smaller by `n` on every side.
    pub fn inset(r: Rect, n: i32) Rect {
        return .{ .x = r.x + n, .y = r.y + n, .w = @max(0, r.w - 2 * n), .h = @max(0, r.h - 2 * n) };
    }
    pub fn intersect(r: Rect, o: Rect) Rect {
        const x0 = @max(r.x, o.x);
        const y0 = @max(r.y, o.y);
        return .{ .x = x0, .y = y0, .w = @max(0, @min(r.right(), o.right()) - x0), .h = @max(0, @min(r.bottom(), o.bottom()) - y0) };
    }
};

/// `a` mixed into `b`; `amount` 255 is all `a`.
pub fn mix(a: u32, b: u32, amount: u32) u32 {
    var out: u32 = 0;
    inline for (.{ 0, 8, 16 }) |shift| {
        const ca = a >> shift & 0xff;
        const cb = b >> shift & 0xff;
        out |= ((ca * amount + cb * (255 - amount)) / 255) << shift;
    }
    return out;
}

pub const Canvas = struct {
    pixels: []u32,
    w: u32,
    h: u32,
    /// Drawing outside this is ignored.
    clip: Rect,

    pub fn init(w: u32, h: u32) ?Canvas {
        const pixels = std.heap.page_allocator.alloc(u32, w * h) catch return null;
        return .{ .pixels = pixels, .w = w, .h = h, .clip = .{ .x = 0, .y = 0, .w = @intCast(w), .h = @intCast(h) } };
    }

    pub fn bounds(c: *const Canvas) Rect {
        return .{ .x = 0, .y = 0, .w = @intCast(c.w), .h = @intCast(c.h) };
    }

    /// Mixes `color` into one pixel with coverage `alpha` (0..255).
    pub fn blend(c: *Canvas, x: i32, y: i32, color: u32, alpha: u32) void {
        if (alpha == 0 or !c.clip.contains(x, y)) return;
        const p = &c.pixels[@as(usize, @intCast(y)) * c.w + @as(usize, @intCast(x))];
        p.* = if (alpha >= 255) color else mix(color, p.*, alpha);
    }

    pub fn fill(c: *Canvas, color: u32) void {
        c.rect(c.bounds(), color);
    }

    pub fn rect(c: *Canvas, r: Rect, color: u32) void {
        const a = r.intersect(c.clip);
        var y = a.y;
        while (y < a.bottom()) : (y += 1) {
            const row = @as(usize, @intCast(y)) * c.w;
            @memset(c.pixels[row + @as(usize, @intCast(a.x)) ..][0..@intCast(a.w)], color);
        }
    }

    /// A rectangle with rounded corners, smoothed at the edges.
    pub fn roundRect(c: *Canvas, r: Rect, radius: f32, color: u32) void {
        const rad = @min(radius, @as(f32, @floatFromInt(@min(r.w, r.h))) / 2);
        const ri: i32 = @intFromFloat(@ceil(rad));
        // The middle rows have no corners.
        c.rect(.{ .x = r.x, .y = r.y + ri, .w = r.w, .h = r.h - 2 * ri }, color);
        var y = r.y;
        while (y < r.bottom()) : (y += 1) {
            if (y >= r.y + ri and y < r.bottom() - ri) continue;
            var x = r.x;
            while (x < r.right()) : (x += 1) {
                const d = roundedDistance(r, rad, x, y);
                c.blend(x, y, color, coverage(d));
            }
        }
    }

    /// A rounded rectangle's outline, `width` pixels thick, inside `r`.
    pub fn roundRectOutline(c: *Canvas, r: Rect, radius: f32, width: f32, color: u32) void {
        var y = r.y;
        while (y < r.bottom()) : (y += 1) {
            const near_edge_y = y < r.y + @as(i32, @intFromFloat(@ceil(radius + width))) or y >= r.bottom() - @as(i32, @intFromFloat(@ceil(radius + width)));
            var x = r.x;
            while (x < r.right()) : (x += 1) {
                if (!near_edge_y and x >= r.x + @as(i32, @intFromFloat(@ceil(width))) and x < r.right() - @as(i32, @intFromFloat(@ceil(width)))) {
                    x = r.right() - @as(i32, @intFromFloat(@ceil(width))) - 1;
                    continue;
                }
                const d = roundedDistance(r, radius, x, y);
                const outer = coverage(d);
                const inner = coverage(d + width);
                c.blend(x, y, color, outer -| inner);
            }
        }
    }

    pub fn circle(c: *Canvas, cx: f32, cy: f32, radius: f32, color: u32) void {
        const x0: i32 = @intFromFloat(@floor(cx - radius - 1));
        const y0: i32 = @intFromFloat(@floor(cy - radius - 1));
        const x1: i32 = @intFromFloat(@ceil(cx + radius + 1));
        const y1: i32 = @intFromFloat(@ceil(cy + radius + 1));
        var y = y0;
        while (y <= y1) : (y += 1) {
            var x = x0;
            while (x <= x1) : (x += 1) {
                const dx = @as(f32, @floatFromInt(x)) + 0.5 - cx;
                const dy = @as(f32, @floatFromInt(y)) + 0.5 - cy;
                c.blend(x, y, color, coverage(@sqrt(dx * dx + dy * dy) - radius));
            }
        }
    }

    /// Draws text with its baseline at y, starting at x. Returns where it ends.
    pub fn text(c: *Canvas, font: *Font, size: f32, x: f32, y: i32, s: []const u8, color: u32) f32 {
        var pen = x;
        const px: u16 = @intFromFloat(@round(size));
        var it = std.unicode.Utf8View.initUnchecked(s).iterator();
        while (it.nextCodepoint()) |cp| {
            const g = font.glyph(font.glyphIndex(cp), px);
            const gx = @as(i32, @intFromFloat(@round(pen))) + g.left;
            const gy = y - g.top;
            var row: u32 = 0;
            while (row < g.h) : (row += 1) {
                var col: u32 = 0;
                while (col < g.w) : (col += 1) {
                    c.blend(gx + @as(i32, @intCast(col)), gy + @as(i32, @intCast(row)), color, g.alpha[row * g.w + col]);
                }
            }
            pen += g.advance;
        }
        return pen;
    }
};

/// How wide text is, in pixels.
pub fn measure(font: *Font, size: f32, s: []const u8) f32 {
    var w: f32 = 0;
    var it = std.unicode.Utf8View.initUnchecked(s).iterator();
    while (it.nextCodepoint()) |cp| w += font.advance(font.glyphIndex(cp), @round(size));
    return w;
}

/// Pixel coverage from the signed distance (in pixels) of its center to an edge.
fn coverage(d: f32) u32 {
    return @intFromFloat(std.math.clamp(0.5 - d, 0, 1) * 255);
}

/// Signed distance from a pixel's center to a rounded rectangle's edge (negative inside).
fn roundedDistance(r: Rect, radius: f32, x: i32, y: i32) f32 {
    const hw = @as(f32, @floatFromInt(r.w)) / 2;
    const hh = @as(f32, @floatFromInt(r.h)) / 2;
    const px = @as(f32, @floatFromInt(x - r.x)) + 0.5 - hw;
    const py = @as(f32, @floatFromInt(y - r.y)) + 0.5 - hh;
    const qx = @abs(px) - (hw - radius);
    const qy = @abs(py) - (hh - radius);
    const ox = @max(qx, 0);
    const oy = @max(qy, 0);
    return @sqrt(ox * ox + oy * oy) + @min(@max(qx, qy), 0) - radius;
}
