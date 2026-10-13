//! pictures: shows a picture (JPEG or PNG), scaled to fit its window. Left and Right go to the
//! other pictures in the same folder; Escape closes it. Files opens pictures with it.
//!
//!     pictures /usr/share/backgrounds/dolomites.jpg &

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
const file = @import("lib/file.zig");
const jpeg = @import("lib/jpeg.zig");
const png = @import("lib/png.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const gpa = std.heap.page_allocator;
const bg = 0x16191f;

/// A decoded picture: 0xAARRGGBB (JPEG: opaque).
const Picture = struct { w: u32, h: u32, pixels: []u32 };

var font: *gui.Font = undefined;
var win: gui.Window = undefined;
var picture: ?Picture = null;
var message: []const u8 = "";

var dir_buf: [1024]u8 = undefined;
var dir_len: usize = 0;
var names: [64 * 1024]u8 = undefined;
var list: [1024][]const u8 = undefined;
var count: usize = 0;
var current: usize = 0;

fn decode(path: [*:0]const u8) ?Picture {
    const data = file.read(path) catch {
        message = "Can't read this file.";
        return null;
    };
    defer gpa.free(data);
    if (png.decode(data)) |img| return .{ .w = img.w, .h = img.h, .pixels = img.pixels } else |_| {}
    if (jpeg.decode(data)) |img| {
        for (img.pixels) |*p| p.* |= 0xff00_0000;
        return .{ .w = img.w, .h = img.h, .pixels = img.pixels };
    } else |e| {
        message = if (e == error.Unsupported) "This kind of JPEG (progressive) can't be shown yet." else "This isn't a picture that can be shown.";
        return null;
    }
}

fn isPicture(name: []const u8) bool {
    for ([_][]const u8{ ".jpg", ".jpeg", ".png" }) |ext| {
        if (name.len > ext.len and std.ascii.eqlIgnoreCase(name[name.len - ext.len ..], ext)) return true;
    }
    return false;
}

/// The pictures in the folder `path` is in, sorted, and which one `path` is.
fn scanFolder(path: []const u8) void {
    const cut = std.mem.lastIndexOfScalar(u8, path, '/') orelse 0;
    dir_len = @min(cut, dir_buf.len);
    @memcpy(dir_buf[0..dir_len], path[0..dir_len]);
    const base = path[cut + 1 ..];
    var cdir: [1025]u8 = undefined;
    const d = rt.cstr(&cdir, if (dir_len == 0) "/" else dir_buf[0..dir_len]) orelse return;
    const fd: i32 = switch (sys.open(d, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return,
    };
    defer sys.close(fd);
    var used: usize = 0;
    var buf: [8192]u8 align(8) = undefined;
    outer: while (true) {
        const n = switch (sys.check(sys.syscall3(sys.nr.getdents64, @bitCast(@as(isize, fd)), @intFromPtr(&buf), buf.len))) {
            .ok => |n| n,
            .err => break,
        };
        if (n == 0) break;
        var at: usize = 0;
        while (at < n) {
            const reclen = std.mem.readInt(u16, buf[at + 16 ..][0..2], .little);
            const name = std.mem.sliceTo(buf[at + 19 .. at + reclen], 0);
            at += reclen;
            if (!isPicture(name)) continue;
            if (count == list.len or used + name.len > names.len) break :outer;
            @memcpy(names[used..][0..name.len], name);
            list[count] = names[used..][0..name.len];
            used += name.len;
            count += 1;
        }
    }
    std.mem.sort([]const u8, list[0..count], {}, struct {
        fn less(_: void, a: []const u8, b: []const u8) bool {
            return std.ascii.lessThanIgnoreCase(a, b);
        }
    }.less);
    for (list[0..count], 0..) |name, i| {
        if (std.mem.eql(u8, name, base)) current = i;
    }
}

fn show(path: [*:0]const u8) void {
    if (picture) |p| gpa.free(p.pixels);
    message = "";
    picture = decode(path);
    var title: [300]u8 = undefined;
    const name = std.mem.span(path);
    const base = name[(std.mem.lastIndexOfScalar(u8, name, '/') orelse 0) + 1 ..];
    win.setTitle(std.fmt.bufPrintSentinel(&title, "{s} — Pictures", .{base[0..@min(base.len, 200)]}, 0) catch "Pictures");
}

fn step(delta: i32) void {
    if (count < 2) return;
    current = @intCast(@mod(@as(i32, @intCast(current)) + delta, @as(i32, @intCast(count))));
    var p: [1300]u8 = undefined;
    const full = std.fmt.bufPrintSentinel(&p, "{s}/{s}", .{ dir_buf[0..dir_len], list[current] }, 0) catch return;
    show(full);
}

fn draw() void {
    const c = &win.canvas;
    c.fill(bg);
    if (picture) |pic| {
        // Fit: the whole picture, as big as the window allows (never larger than it is), centered.
        const sx = @as(f32, @floatFromInt(c.w)) / @as(f32, @floatFromInt(pic.w));
        const sy = @as(f32, @floatFromInt(c.h - 28)) / @as(f32, @floatFromInt(pic.h));
        const scale = @min(1.0, @min(sx, sy));
        const dw: u32 = @max(1, @as(u32, @intFromFloat(@as(f32, @floatFromInt(pic.w)) * scale)));
        const dh: u32 = @max(1, @as(u32, @intFromFloat(@as(f32, @floatFromInt(pic.h)) * scale)));
        const ox = (c.w - dw) / 2;
        const oy = (c.h - 28 - dh) / 2;
        var y: u32 = 0;
        while (y < dh) : (y += 1) {
            const py = @min(pic.h - 1, @as(u32, @intFromFloat((@as(f32, @floatFromInt(y)) + 0.5) / scale)));
            var x: u32 = 0;
            while (x < dw) : (x += 1) {
                const px = @min(pic.w - 1, @as(u32, @intFromFloat((@as(f32, @floatFromInt(x)) + 0.5) / scale)));
                const p = pic.pixels[py * pic.w + px];
                c.blend(@intCast(ox + x), @intCast(oy + y), p & 0xffffff, p >> 24);
            }
        }
        var b: [96]u8 = undefined;
        const info = std.fmt.bufPrint(&b, "{d} × {d}{s}", .{ pic.w, pic.h, if (count > 1) "   ← → for the others in this folder" else "" }) catch "";
        _ = c.text(font, 13, 12, @intCast(c.h - 9), info, 0x98a1ae);
    } else {
        _ = c.text(font, 15, 24, 40, message, 0xe9edf2);
    }
    win.present();
}

pub fn main(args: rt.Args) u8 {
    const path = args.get(1) orelse {
        rt.err.write("usage: pictures FILE\n");
        return 2;
    };
    font = gui.Font.load(gui.theme.font_path) orelse return 1;
    win = gui.Window.open("Pictures", 960, 640) orelse {
        rt.err.write("pictures: can't open a window\n");
        return 1;
    };
    scanFolder(path);
    show(args.argv[1]);
    draw();
    while (win.next(null)) |e| {
        switch (e) {
            .key => |k| switch (k.code) {
                27 => break,
                gui.Key.left => step(-1),
                gui.Key.right, ' ' => step(1),
                else => continue,
            },
            .resize => {},
            else => continue,
        }
        draw();
    }
    win.close();
    return 0;
}
