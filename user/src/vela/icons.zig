//! Icons: the apps' pictures from the Oxygen icon theme (KDE's, in /usr/share/icons/oxygen at 32,
//! 48 and 64 pixels), loaded once and drawn with their transparency. Also Vela's star.

const std = @import("std");
const gui = @import("../lib/gui.zig");
const file = @import("../lib/file.zig");
const png = @import("../lib/png.zig");
const Icon = @import("apps.zig").Icon;
const Canvas = gui.Canvas;

/// Pictures that aren't apps: the star menu's own rows.
pub const Extra = enum { wallpaper, leave };

/// The Oxygen icon for each.
fn fileName(which: Which) []const u8 {
    return switch (which) {
        .app => |icon| switch (icon) {
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

pub const Which = union(enum) { app: Icon, extra: Extra };

const sizes = [_]u32{ 32, 48, 64 };
const app_count = @typeInfo(Icon).@"enum".field_names.len;
const extra_count = @typeInfo(Extra).@"enum".field_names.len;
/// Loaded pictures, by icon and size (null: not tried yet; an empty image: missing).
var cache: [app_count + extra_count][sizes.len]?png.Image = @splat(@splat(null));

fn slot(which: Which) usize {
    return switch (which) {
        .app => |i| @intFromEnum(i),
        .extra => |e| app_count + @intFromEnum(e),
    };
}

/// The picture for `which` at the smallest stored size of at least `size` (or the largest).
fn load(which: Which, size: u32) ?png.Image {
    var k: usize = 0;
    while (k + 1 < sizes.len and sizes[k] < size) k += 1;
    const entry = &cache[slot(which)][k];
    if (entry.* == null) {
        var path: [128]u8 = undefined;
        const p = std.fmt.bufPrint(&path, "/usr/share/icons/oxygen/{d}x{d}/{s}.png\x00", .{ sizes[k], sizes[k], fileName(which) }) catch return null;
        const data = file.read(@ptrCast(p.ptr)) catch {
            entry.* = .{ .w = 0, .h = 0, .pixels = &.{} };
            return null;
        };
        defer std.heap.page_allocator.free(data);
        entry.* = png.decode(data) catch png.Image{ .w = 0, .h = 0, .pixels = &.{} };
    }
    const img = entry.*.?;
    return if (img.w == 0) null else img;
}

/// Draws an app's icon in the s x s square at (x, y).
pub fn draw(c: *Canvas, icon: Icon, x: i32, y: i32, s: u32) void {
    drawWhich(c, .{ .app = icon }, x, y, s);
}

pub fn drawExtra(c: *Canvas, e: Extra, x: i32, y: i32, s: u32) void {
    drawWhich(c, .{ .extra = e }, x, y, s);
}

fn drawWhich(c: *Canvas, which: Which, x: i32, y: i32, s: u32) void {
    const img = load(which, s) orelse {
        c.roundRect(.{ .x = x, .y = y, .w = @intCast(s), .h = @intCast(s) }, 8, 0x4b5365);
        return;
    };
    image(c, img, x, y, s);
}

/// Draws a picture with alpha into the s x s square at (x, y), scaled (box filter) if its size
/// differs.
pub fn image(c: *Canvas, img: png.Image, x: i32, y: i32, s: u32) void {
    var dy: u32 = 0;
    while (dy < s) : (dy += 1) {
        var dx: u32 = 0;
        while (dx < s) : (dx += 1) {
            const p = if (img.w == s and img.h == s) img.pixels[dy * img.w + dx] else average(img, dx, dy, s);
            const a = p >> 24;
            if (a != 0) c.blend(x + @as(i32, @intCast(dx)), y + @as(i32, @intCast(dy)), p & 0xffffff, a);
        }
    }
}

/// The picture's pixels that fall in target pixel (dx, dy) of an s x s square, averaged (colors
/// weighted by alpha, so transparent pixels' colors don't bleed in).
fn average(img: png.Image, dx: u32, dy: u32, s: u32) u32 {
    const x0 = dx * img.w / s;
    const x1 = @max(x0 + 1, (dx + 1) * img.w / s);
    const y0 = dy * img.h / s;
    const y1 = @max(y0 + 1, (dy + 1) * img.h / s);
    var sum: [4]u32 = @splat(0);
    var y = y0;
    while (y < y1) : (y += 1) {
        var x = x0;
        while (x < x1) : (x += 1) {
            const p = img.pixels[y * img.w + x];
            const a = p >> 24;
            sum[0] += (p >> 16 & 0xff) * a;
            sum[1] += (p >> 8 & 0xff) * a;
            sum[2] += (p & 0xff) * a;
            sum[3] += a;
        }
    }
    const n = (x1 - x0) * (y1 - y0);
    if (sum[3] == 0) return 0;
    return (sum[3] / n) << 24 | (sum[0] / sum[3]) << 16 | (sum[1] / sum[3]) << 8 | (sum[2] / sum[3]);
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
