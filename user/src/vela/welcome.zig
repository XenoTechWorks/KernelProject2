//! The Welcome window, after Windows Vista's Welcome Center: what this computer is, and a few
//! things to start with. Vela opens it when it starts (until "Show this when Vela starts" is
//! unticked) and does what its tiles ask (welcomeAction in vela.zig).

const std = @import("std");
const gui = @import("../lib/gui.zig");
const file = @import("../lib/file.zig");
const apps = @import("apps.zig");
const icons = @import("icons.zig");
const glass = @import("glass.zig");
const settings = @import("settings.zig");
const shell = @import("shell.zig");
const Rect = gui.Rect;

pub const width = 640;
pub const height = 470;

const text = 0xf2f5f9;
const dim = 0xa9b2bf;
const bg = 0x22262e;
const head_h = 104;

pub const Action = union(enum) { personalize, all_apps, launch: *const apps.App, show_at_start, close };

const Tile = struct { title: []const u8, line: []const u8, icon: icons.Which, action: Action };

var tiles_buf: [6]Tile = undefined;
var tiles: []Tile = tiles_buf[0..0];
var hover: ?usize = null;
var facts: [3][2][]const u8 = undefined;
var facts_buf: [3][96]u8 = undefined;

fn find(icon: apps.Icon) ?*const apps.App {
    for (apps.list()) |app| {
        if (app.icon == icon) return app;
    }
    return null;
}

/// Sets up the tiles (from the apps that are installed) and reads what this computer is.
pub fn init() void {
    var n: usize = 0;
    tiles_buf[n] = .{ .title = "Personalize", .line = "Background, window color, glass", .icon = .{ .extra = .wallpaper }, .action = .personalize };
    n += 1;
    const wanted = [_]struct { icon: apps.Icon, title: []const u8, line: []const u8 }{
        .{ .icon = .terminal, .title = "Open a terminal", .line = "The shell, kpsh, in a window" },
        .{ .icon = .files, .title = "Browse files", .line = "Your home folder is kept on disk" },
        .{ .icon = .editor, .title = "Write something", .line = "A text editor with undo and copy" },
        .{ .icon = .doom, .title = "Play Doom", .line = "Chocolate Doom and Freedoom" },
    };
    for (wanted) |w| {
        const app = find(w.icon) orelse continue;
        tiles_buf[n] = .{ .title = w.title, .line = w.line, .icon = .{ .app = w.icon }, .action = .{ .launch = app } };
        n += 1;
    }
    if (n < tiles_buf.len) {
        // Running from the ISO, the last tile offers to install; once installed, all the apps.
        if (live() and find(.installer) != null) {
            tiles_buf[n] = .{ .title = "Install on a disk", .line = "Start without the ISO, keep your files", .icon = .{ .app = .installer }, .action = .{ .launch = find(.installer).? } };
        } else {
            tiles_buf[n] = .{ .title = "All apps", .line = "Everything in the star menu", .icon = .{ .app = .widgets }, .action = .all_apps };
        }
        n += 1;
    }
    tiles = tiles_buf[0..n];

    facts[0] = .{ "Processor", cpuName(&facts_buf[0]) };
    facts[1] = .{ "Memory", memory(&facts_buf[1]) };
    facts[2] = .{ "Machine", machine(&facts_buf[2]) };
}

/// Whether this is the system running from the ISO: /root isn't on an installed home partition
/// (/proc/mounts shows a whole disk, /dev/vda, or no disk at all).
fn live() bool {
    const data = file.read("/proc/mounts") catch return true;
    defer std.heap.page_allocator.free(data);
    var lines = std.mem.splitScalar(u8, data, '\n');
    while (lines.next()) |line| {
        var words = std.mem.tokenizeScalar(u8, line, ' ');
        const dev = words.next() orelse continue;
        const at = words.next() orelse continue;
        if (std.mem.eql(u8, at, "/root")) return !std.ascii.isDigit(dev[dev.len - 1]);
    }
    return true;
}

/// The value of a "name : value" line of a /proc file.
fn procField(path: [*:0]const u8, name: []const u8, out: []u8) ?[]const u8 {
    const data = file.read(path) catch return null;
    defer std.heap.page_allocator.free(data);
    var lines = std.mem.splitScalar(u8, data, '\n');
    while (lines.next()) |line| {
        if (!std.mem.startsWith(u8, line, name)) continue;
        const colon = std.mem.indexOfScalar(u8, line, ':') orelse continue;
        const value = std.mem.trim(u8, line[colon + 1 ..], " \t");
        const n = @min(value.len, out.len);
        @memcpy(out[0..n], value[0..n]);
        return out[0..n];
    }
    return null;
}

fn cpuName(out: []u8) []const u8 {
    return procField("/proc/cpuinfo", "model name", out) orelse "x86-64";
}

fn memory(out: []u8) []const u8 {
    var tmp: [64]u8 = undefined;
    const total = procField("/proc/meminfo", "MemTotal", &tmp) orelse return "?";
    const kib = std.fmt.parseInt(u64, std.mem.trimEnd(u8, total, " kB"), 10) catch return "?";
    return std.fmt.bufPrint(out, "{d} MiB", .{kib / 1024}) catch "?";
}

fn machine(out: []u8) []const u8 {
    const data = file.read("/sys/class/dmi/id/product_name") catch return "A PC";
    defer std.heap.page_allocator.free(data);
    const name = std.mem.trim(u8, data, " \n");
    const n = @min(name.len, out.len);
    @memcpy(out[0..n], name[0..n]);
    return out[0..n];
}

fn tileRect(i: usize) Rect {
    const col: i32 = @intCast(i % 2);
    const row: i32 = @intCast(i / 2);
    return .{ .x = 24 + col * 300, .y = head_h + 96 + row * 74, .w = 292, .h = 66 };
}

const check_box = Rect{ .x = 24, .y = height - 44, .w = 260, .h = 24 };
const close_button = Rect{ .x = width - 24 - 100, .y = height - 48, .w = 100, .h = 32 };

pub fn draw(c: *gui.Canvas) void {
    c.fill(bg);
    // The header, in the window color, with Vela's orb.
    const color = settings.colors[settings.color];
    var y: i32 = 0;
    while (y < head_h) : (y += 1) {
        const row = gui.mix(color.bottom, color.top, @intCast(@as(u32, @intCast(y)) * 255 / head_h));
        c.rect(.{ .x = 0, .y = y, .w = width, .h = 1 }, gui.mix(0xffffff, row, if (y < head_h / 2) @intCast(30 - @as(u32, @intCast(y)) * 30 / (head_h / 2)) else 0));
    }
    c.rect(.{ .x = 0, .y = head_h - 1, .w = width, .h = 1 }, 0x07090d);
    glass.orb(c, 58, head_h / 2, 30, false, icons.star);
    _ = c.text(shell.font, 25, 104, 50, "Welcome to KernelProject2", text);
    _ = c.text(shell.font, 14, 105, 74, "Vela, the desktop. Here's your computer, and some places to start.", 0xd8e2f0);

    // This computer.
    _ = c.text(shell.font, 14, 24, head_h + 30, "This computer", dim);
    for (facts, 0..) |f, i| {
        const fy: i32 = head_h + 54;
        const fx: f32 = 24 + @as(f32, @floatFromInt(i)) * 200;
        var buf: [96]u8 = undefined;
        _ = c.text(shell.font, 12, fx, fy - 2, f[0], dim);
        _ = c.text(shell.font, 14, fx, fy + 16, shell.fit(&buf, f[1], 14, 190), text);
    }

    // Get started.
    for (tiles, 0..) |t, i| {
        const r = tileRect(i);
        if (hover == i) {
            glass.tint(c, r, 8, 0x9cc8ff, 40);
            glass.outline(c, r, 8, 0xcfe4ff, 110);
        } else {
            glass.tint(c, r, 8, 0xffffff, 10);
        }
        switch (t.icon) {
            .app => |a| icons.draw(c, a, r.x + 9, r.y + 9, 48),
            .extra => |e| icons.drawExtra(c, e, r.x + 9, r.y + 9, 48),
        }
        _ = c.text(shell.font, 15, @floatFromInt(r.x + 68), r.y + 28, t.title, text);
        var buf: [96]u8 = undefined;
        _ = c.text(shell.font, 12, @floatFromInt(r.x + 68), r.y + 47, shell.fit(&buf, t.line, 12, 212), dim);
    }

    // Show at start; Close.
    const box = Rect{ .x = check_box.x, .y = check_box.y + 3, .w = 18, .h = 18 };
    if (settings.welcome) {
        glass.tint(c, box, 4, 0x3a8ee6, 255);
        c.line(@floatFromInt(box.x + 4), @floatFromInt(box.y + 9), @floatFromInt(box.x + 8), @floatFromInt(box.y + 13), 2, 0xffffff);
        c.line(@floatFromInt(box.x + 8), @floatFromInt(box.y + 13), @floatFromInt(box.x + 14), @floatFromInt(box.y + 5), 2, 0xffffff);
    } else {
        glass.tint(c, box, 4, 0xffffff, 25);
    }
    glass.outline(c, box, 4, 0xffffff, 110);
    _ = c.text(shell.font, 14, @floatFromInt(box.right() + 10), box.y + 14, "Show this when Vela starts", text);
    glass.tint(c, close_button, 6, 0xffffff, if (hover_close) 45 else 25);
    glass.outline(c, close_button, 6, 0xffffff, 90);
    const cw = gui.measure(shell.font, 14, "Close");
    _ = c.text(shell.font, 14, @as(f32, @floatFromInt(close_button.x)) + (@as(f32, @floatFromInt(close_button.w)) - cw) / 2, close_button.y + 21, "Close", text);
}

var hover_close = false;

/// Pointer moved: whether anything lit up or went dark (the window needs drawing again).
pub fn motion(x: i32, y: i32) bool {
    var h: ?usize = null;
    for (tiles, 0..) |_, i| {
        if (tileRect(i).contains(x, y)) h = i;
    }
    const hc = close_button.contains(x, y);
    defer {
        hover = h;
        hover_close = hc;
    }
    return h != hover or hc != hover_close;
}

pub fn leave() bool {
    return motion(-1, -1);
}

/// What a click at (x, y) asks for.
pub fn press(x: i32, y: i32) ?Action {
    for (tiles, 0..) |t, i| {
        if (tileRect(i).contains(x, y)) return t.action;
    }
    if (check_box.contains(x, y)) return .show_at_start;
    if (close_button.contains(x, y)) return .close;
    return null;
}
