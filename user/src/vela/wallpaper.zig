//! Vela's wallpaper: one of the photos in /usr/share/backgrounds (JPEGs, with small copies in
//! thumbs/ for the picker; credits in CREDITS.txt there), the one chosen in Vela's settings.
//! When none can be read, a night sky drawn here, with the stars of Vela joined up on the right.

const std = @import("std");
const gui = @import("../lib/gui.zig");
const file = @import("../lib/file.zig");
const jpeg = @import("../lib/jpeg.zig");
const sys = @import("../lib/sys.zig");
const settings = @import("settings.zig");
const Canvas = gui.Canvas;

const gpa = std.heap.page_allocator;
const dir = "/usr/share/backgrounds";

/// A wallpaper: its file's name without ".jpg", and what the picker calls it.
pub const Choice = struct { slug: []const u8, title: []const u8 };

var slugs_buf: [4096]u8 = undefined;
var choices_buf: [48]Choice = undefined;
pub var choices: []Choice = choices_buf[0..0];

/// The wallpaper in use (a slug).
pub fn chosen() []const u8 {
    return settings.wallpaper();
}

/// Finds the wallpapers (every .jpg in /usr/share/backgrounds, named after the index file there
/// when it lists them).
pub fn init() void {
    var names_buf: [4096]u8 = undefined;
    const index = readSmall(dir ++ "/index", &names_buf) orelse "";
    var used: usize = 0;
    var n: usize = 0;
    const fd: i32 = switch (sys.open(dir, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return,
    };
    defer sys.close(fd);
    var buf: [4096]u8 = undefined;
    while (true) {
        const got = switch (sys.check(sys.syscall3(sys.nr.getdents64, @bitCast(@as(isize, fd)), @intFromPtr(&buf), buf.len))) {
            .ok => |k| k,
            .err => break,
        };
        if (got == 0) break;
        var at: usize = 0;
        while (at < got) {
            const reclen = std.mem.readInt(u16, buf[at + 16 ..][0..2], .little);
            const entry = std.mem.sliceTo(buf[at + 19 .. at + reclen], 0);
            at += reclen;
            if (!std.mem.endsWith(u8, entry, ".jpg") or n == choices_buf.len) continue;
            const slug = entry[0 .. entry.len - 4];
            if (used + 2 * slug.len + 2 > slugs_buf.len) continue;
            @memcpy(slugs_buf[used..][0..slug.len], slug);
            const kept = slugs_buf[used..][0..slug.len];
            used += slug.len;
            choices_buf[n] = .{ .slug = kept, .title = titleFor(index, kept, &used) };
            n += 1;
        }
    }
    choices = choices_buf[0..n];
    std.mem.sort(Choice, choices, {}, struct {
        fn less(_: void, a: Choice, b: Choice) bool {
            return std.mem.order(u8, a.title, b.title) == .lt;
        }
    }.less);
}

/// The index file's name for a slug ("slug<TAB>Title" lines), or the slug itself.
fn titleFor(index: []const u8, slug: []const u8, used: *usize) []const u8 {
    var lines = std.mem.splitScalar(u8, index, '\n');
    while (lines.next()) |line| {
        var parts = std.mem.splitScalar(u8, line, '\t');
        if (!std.mem.eql(u8, parts.next() orelse "", slug)) continue;
        const title = parts.next() orelse break;
        if (used.* + title.len > slugs_buf.len) break;
        @memcpy(slugs_buf[used.*..][0..title.len], title);
        defer used.* += title.len;
        return slugs_buf[used.*..][0..title.len];
    }
    return slug;
}

fn readSmall(path: [*:0]const u8, buf: []u8) ?[]const u8 {
    const data = file.read(path) catch return null;
    defer gpa.free(data);
    const n = @min(data.len, buf.len);
    @memcpy(buf[0..n], data[0..n]);
    return buf[0..n];
}

/// Makes `slug` the wallpaper, and remembers it (in /root, which is kept across boots).
pub fn choose(slug: []const u8) void {
    settings.setWallpaper(slug);
    settings.save();
}

/// Fills the canvas with the chosen wallpaper (or the first one there is, or the drawn sky).
pub fn paint(c: *Canvas) void {
    if (paintPhoto(c, chosen())) return;
    for (choices) |ch| {
        if (paintPhoto(c, ch.slug)) return;
    }
    paintSky(c);
}

/// A wallpaper's small picture for the picker, decoded (the caller deinits it).
pub fn thumbnail(slug: []const u8) ?jpeg.Image {
    var path: [256]u8 = undefined;
    const p = std.fmt.bufPrint(&path, dir ++ "/thumbs/{s}.jpg\x00", .{slug}) catch return null;
    const data = file.read(@ptrCast(p.ptr)) catch return null;
    defer gpa.free(data);
    return jpeg.decode(data) catch null;
}

/// A photo scaled to cover the canvas (cut at the sides or top and bottom if the shape differs),
/// dimmed a little so icons and their labels stand out.
fn paintPhoto(c: *Canvas, slug: []const u8) bool {
    var path: [256]u8 = undefined;
    const p = std.fmt.bufPrint(&path, dir ++ "/{s}.jpg\x00", .{slug}) catch return false;
    const data = file.read(@ptrCast(p.ptr)) catch return false;
    defer gpa.free(data);
    var img = jpeg.decode(data) catch return false;
    defer img.deinit();
    drawCover(c, img, c.bounds(), 218);
    return true;
}

/// Draws `img` scaled to cover `r` (centered, cut to fit), each channel times `level`/255.
pub fn drawCover(c: *Canvas, img: jpeg.Image, r: gui.Rect, level: u32) void {
    const sx = @as(f32, @floatFromInt(img.w)) / @as(f32, @floatFromInt(r.w));
    const sy = @as(f32, @floatFromInt(img.h)) / @as(f32, @floatFromInt(r.h));
    const scale = @min(sx, sy);
    const ox = (@as(f32, @floatFromInt(img.w)) - scale * @as(f32, @floatFromInt(r.w))) / 2;
    const oy = (@as(f32, @floatFromInt(img.h)) - scale * @as(f32, @floatFromInt(r.h))) / 2;
    const clip = r.intersect(c.clip);
    var y = clip.y;
    while (y < clip.bottom()) : (y += 1) {
        const py: u32 = @min(img.h - 1, @as(u32, @intFromFloat(oy + (@as(f32, @floatFromInt(y - r.y)) + 0.5) * scale)));
        var x = clip.x;
        while (x < clip.right()) : (x += 1) {
            const px: u32 = @min(img.w - 1, @as(u32, @intFromFloat(ox + (@as(f32, @floatFromInt(x - r.x)) + 0.5) * scale)));
            const p = img.pixels[py * img.w + px];
            const rr = (p >> 16 & 0xff) * level / 255;
            const gg = (p >> 8 & 0xff) * level / 255;
            const bb = (p & 0xff) * level / 255;
            c.pixels[@as(usize, @intCast(y)) * c.w + @as(usize, @intCast(x))] = rr << 16 | gg << 8 | bb;
        }
    }
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
