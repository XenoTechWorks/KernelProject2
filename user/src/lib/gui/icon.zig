//! Icons from the icon theme: KDE's Oxygen, in /usr/share/icons/oxygen/<size>x<size>/<name>.png
//! at 16, 22, 32, 48 and 64 pixels. Each is decoded once (lib/png.zig) and drawn with its
//! transparency, from the smallest stored size at least as big as asked (scaled down smoothly if it
//! isn't exactly that size).

const std = @import("std");
const Canvas = @import("canvas.zig").Canvas;
const file = @import("../file.zig");
const png = @import("../png.zig");

const sizes = [_]u32{ 16, 22, 32, 48, 64 };

const Entry = struct { name: [48]u8, len: usize, size: u32, image: ?png.Image };
var cache: [160]?Entry = @splat(null);

/// The picture for `name` at about `size` pixels, or null if the theme doesn't have it.
pub fn get(name: []const u8, size: u32) ?png.Image {
    // Smallest stored size >= size; then larger, then smaller ones, until one exists.
    var start: usize = 0;
    while (start + 1 < sizes.len and sizes[start] < size) start += 1;
    var order: [sizes.len]u32 = undefined;
    var n: usize = 0;
    var i = start;
    while (i < sizes.len) : (i += 1) {
        order[n] = sizes[i];
        n += 1;
    }
    i = start;
    while (i > 0) {
        i -= 1;
        order[n] = sizes[i];
        n += 1;
    }
    for (order[0..n]) |s| {
        if (load(name, s)) |img| return img;
    }
    return null;
}

fn load(name: []const u8, size: u32) ?png.Image {
    if (name.len > 48) return null;
    var free: ?usize = null;
    for (&cache, 0..) |*slot, k| {
        if (slot.*) |e| {
            if (e.size == size and std.mem.eql(u8, e.name[0..e.len], name)) return e.image;
        } else if (free == null) {
            free = k;
        }
    }
    var path: [128]u8 = undefined;
    const p = std.fmt.bufPrint(&path, "/usr/share/icons/oxygen/{d}x{d}/{s}.png\x00", .{ size, size, name }) catch return null;
    const decoded: ?png.Image = blk: {
        const data = file.read(@ptrCast(p.ptr)) catch break :blk null;
        defer std.heap.page_allocator.free(data);
        break :blk png.decode(data) catch null;
    };
    if (free) |k| {
        var e = Entry{ .name = undefined, .len = name.len, .size = size, .image = decoded };
        @memcpy(e.name[0..name.len], name);
        cache[k] = e;
    }
    return decoded;
}

/// Draws icon `name` in the s x s square at (x, y). False if the theme doesn't have it.
pub fn draw(c: *Canvas, name: []const u8, x: i32, y: i32, s: u32) bool {
    const img = get(name, s) orelse return false;
    image(c, img, x, y, s);
    return true;
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
