//! vela: the desktop. A night-sky wallpaper with app icons on it, a bar along the top (the star
//! menu, a button for each open window, the clock) and a dock of favorite apps at the bottom.
//! Run it from the shell as `vela`; "Leave Vela" in the star menu goes back to the command line.
//!
//! Its parts are windows with roles (kernel/src/console.rs): the wallpaper is a desktop window
//! under all others, the bar and the dock are panels, the star menu is a popup. One loop serves
//! them all.

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
const apps = @import("vela/apps.zig");
const icons = @import("vela/icons.zig");
const shell = @import("vela/shell.zig");
const wallpaper = @import("vela/wallpaper.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const Rect = gui.Rect;

const text = 0xe6e9ef;
const dim = 0x8b93a1;
const accent = 0x3584e4;
const panel_bg = 0x15181e;
const hover_bg = 0x2a2f39;
const gold = 0xf5c84c;

// ---- the desktop: wallpaper and icons ----

const icon_size = 56;
const cell_w = 96;
const cell_h = 92;

var desktop: gui.Window = undefined;
var sky: []u32 = undefined;
var selected: ?usize = null;
var last_press: struct { icon: usize = 0, at: i64 = 0 } = .{};

fn iconCell(i: usize) Rect {
    const per_column: usize = @max(1, @as(usize, @intCast(@divTrunc(@as(i32, @intCast(desktop.canvas.h)) - bar_h - dock_h - 40, cell_h))));
    const col: i32 = @intCast(i / per_column);
    const row: i32 = @intCast(i % per_column);
    return .{ .x = 16 + col * (cell_w + 8), .y = bar_h + 18 + row * cell_h, .w = cell_w, .h = cell_h };
}

fn iconAt(x: i32, y: i32) ?usize {
    for (0..apps.count) |i| {
        if (iconCell(i).contains(x, y)) return i;
    }
    return null;
}

fn drawDesktop() void {
    const c = &desktop.canvas;
    @memcpy(c.pixels, sky);
    for (apps.list(), 0..) |app, i| {
        const cell = iconCell(i);
        if (selected == i) tint(c, cell, 10, accent, 110);
        icons.draw(c, shell.font, app.icon, cell.x + (cell_w - icon_size) / 2, cell.y + 8, icon_size);
        const size = 13;
        var buf: [64]u8 = undefined;
        const label = shell.fit(&buf, app.name, size, cell_w - 8);
        const w = gui.measure(shell.font, size, label);
        const lx = @as(f32, @floatFromInt(cell.x)) + (cell_w - w) / 2;
        const ly = cell.y + 8 + icon_size + 18;
        _ = c.text(shell.font, size, lx + 1, ly + 1, label, 0x05070d);
        _ = c.text(shell.font, size, lx, ly, label, 0xf2f4f8);
    }
    desktop.present();
}

fn desktopEvent(e: gui.Event) void {
    switch (e) {
        .press => |p| {
            if (p.button != 1) return;
            const hit = iconAt(p.x, p.y);
            const now = shell.nowMs();
            if (hit) |i| {
                if (selected == i and last_press.icon == i and now - last_press.at < 450) {
                    shell.launch(apps.list()[i].path);
                    last_press.at = 0;
                } else {
                    last_press = .{ .icon = i, .at = now };
                }
            }
            if (selected != hit) {
                selected = hit;
                drawDesktop();
            }
        },
        .key => |k| if (k.code == 13) {
            if (selected) |i| shell.launch(apps.list()[i].path);
        },
        .windows => windowsChanged(),
        else => {},
    }
}

/// Lays `color` over a rounded rectangle, `alpha` of 255 strong.
fn tint(c: *gui.Canvas, r: Rect, radius: f32, color: u32, alpha: u32) void {
    var y = r.y;
    while (y < r.bottom()) : (y += 1) {
        var x = r.x;
        while (x < r.right()) : (x += 1) {
            const hw = @as(f32, @floatFromInt(r.w)) / 2;
            const hh = @as(f32, @floatFromInt(r.h)) / 2;
            const qx = @abs(@as(f32, @floatFromInt(x - r.x)) + 0.5 - hw) - (hw - radius);
            const qy = @abs(@as(f32, @floatFromInt(y - r.y)) + 0.5 - hh) - (hh - radius);
            const d = @sqrt(@max(qx, 0) * @max(qx, 0) + @max(qy, 0) * @max(qy, 0)) + @min(@max(qx, qy), 0) - radius;
            const cover = std.math.clamp(0.5 - d, 0, 1);
            c.blend(x, y, color, @intFromFloat(cover * @as(f32, @floatFromInt(alpha))));
        }
    }
}

// ---- the bar: star menu, open windows, clock ----

const bar_h = 34;

var bar: gui.Window = undefined;
const BarHit = union(enum) { star, task: u32 };
var bar_hover: ?BarHit = null;
var task_rects: [32]struct { id: u32, rect: Rect } = undefined;
var task_count: usize = 0;
var shown_minute: u64 = std.math.maxInt(u64);

const star_button = Rect{ .x = 6, .y = 4, .w = 82, .h = bar_h - 8 };

fn drawBar() void {
    const c = &bar.canvas;
    c.fill(panel_bg);
    c.rect(.{ .x = 0, .y = bar_h - 1, .w = @intCast(c.w), .h = 1 }, 0x262b35);

    const star_lit = menu != null or (bar_hover != null and bar_hover.? == .star);
    if (star_lit) c.roundRect(star_button, 7, hover_bg);
    icons.star(c, 22, @floatFromInt(bar_h / 2), 8, gold);
    _ = c.text(shell.font, 14, 36, 22, "Vela", text);

    // The clock, on the right.
    var clock_buf: [48]u8 = undefined;
    const clock = clockText(&clock_buf);
    const clock_w = gui.measure(shell.font, 14, clock);
    const clock_x = @as(f32, @floatFromInt(c.w)) - clock_w - 16;
    _ = c.text(shell.font, 14, clock_x, 22, clock, text);

    // A button for each window, in the order they opened.
    var order: [32]gui.Listed = undefined;
    const windows = order[0..shell.window_count];
    @memcpy(windows, shell.openWindows());
    std.mem.sort(gui.Listed, windows, {}, struct {
        fn less(_: void, a: gui.Listed, b: gui.Listed) bool {
            return a.id < b.id;
        }
    }.less);
    task_count = 0;
    const start: i32 = star_button.right() + 10;
    const room = @as(i32, @intFromFloat(clock_x)) - 16 - start;
    if (windows.len > 0 and room > 40) {
        const each = @min(200, @divTrunc(room, @as(i32, @intCast(windows.len))));
        for (windows, 0..) |win, i| {
            const r = Rect{ .x = start + @as(i32, @intCast(i)) * each, .y = 4, .w = each - 6, .h = bar_h - 8 };
            task_rects[task_count] = .{ .id = win.id, .rect = r };
            task_count += 1;
            const hovered = bar_hover != null and bar_hover.? == .task and bar_hover.?.task == win.id;
            const active = win.focused() and !win.minimized();
            if (active) {
                c.roundRect(r, 7, 0x2f3644);
                c.rect(.{ .x = r.x + 10, .y = r.bottom() - 2, .w = r.w - 20, .h = 2 }, accent);
            } else if (hovered) {
                c.roundRect(r, 7, hover_bg);
            }
            var buf: [96]u8 = undefined;
            const title = shell.fit(&buf, if (win.name().len > 0) win.name() else "Window", 13, @floatFromInt(r.w - 20));
            _ = c.text(shell.font, 13, @floatFromInt(r.x + 10), 21, title, if (win.minimized()) dim else text);
        }
    }
    bar.present();
}

fn barHit(x: i32, y: i32) ?BarHit {
    if (star_button.contains(x, y)) return .star;
    for (task_rects[0..task_count]) |t| {
        if (t.rect.contains(x, y)) return .{ .task = t.id };
    }
    return null;
}

fn barEvent(e: gui.Event) void {
    switch (e) {
        .motion => |m| setBarHover(barHit(m.x, m.y)),
        .leave => setBarHover(null),
        .press => |p| if (p.button == 1) {
            const hit = barHit(p.x, p.y) orelse return;
            switch (hit) {
                .star => if (menu != null) closeMenu() else openMenu(),
                .task => |id| toggleWindow(id),
            }
        },
        .windows => windowsChanged(),
        else => {},
    }
}

fn setBarHover(h: ?BarHit) void {
    const same = if (h == null or bar_hover == null) h == null and bar_hover == null else std.meta.eql(h.?, bar_hover.?);
    if (same) return;
    bar_hover = h;
    drawBar();
}

/// A taskbar click: the window in front goes down; any other comes forward.
fn toggleWindow(id: u32) void {
    for (shell.openWindows()) |w| {
        if (w.id != id) continue;
        if (w.focused() and !w.minimized()) bar.minimize(id) else bar.activate(id);
    }
}

const weekdays = [_][]const u8{ "Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat" };
const months = [_][]const u8{ "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec" };

fn clockText(buf: []u8) []const u8 {
    const now = shell.realSeconds();
    const days = now / 86400;
    // Days since 1970-01-01 to a date (Howard Hinnant's civil_from_days).
    const z = days + 719468;
    const era = z / 146097;
    const doe = z - era * 146097;
    const yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    const doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    const mp = (5 * doy + 2) / 153;
    const day = doy - (153 * mp + 2) / 5 + 1;
    const month = if (mp < 10) mp + 3 else mp - 9;
    return std.fmt.bufPrint(buf, "{s} {d} {s}   {d:0>2}:{d:0>2}", .{ weekdays[(days + 4) % 7], day, months[month - 1], now % 86400 / 3600, now % 3600 / 60 }) catch "";
}

// ---- the dock ----

const dock_icon = 48;
const dock_pad = 11;
const dock_gap = 10;
const dock_h = dock_pad + dock_icon + 13;

var dock: gui.Window = undefined;
var dock_apps: [16]*const apps.App = undefined;
var dock_count: usize = 0;
var dock_hover: ?usize = null;

fn dockWidth() u32 {
    return @intCast(2 * dock_pad + @as(i32, @intCast(dock_count)) * (dock_icon + dock_gap) - dock_gap);
}

fn dockSlot(i: usize) Rect {
    return .{ .x = dock_pad + @as(i32, @intCast(i)) * (dock_icon + dock_gap), .y = dock_pad, .w = dock_icon, .h = dock_icon };
}

fn drawDock() void {
    const c = &dock.canvas;
    c.fill(0x1c2029);
    c.roundRectOutline(c.bounds(), 18, 1, 0x343a47);
    for (dock_apps[0..dock_count], 0..) |app, i| {
        const r = dockSlot(i);
        if (dock_hover == i) c.roundRect(.{ .x = r.x - 5, .y = r.y - 5, .w = r.w + 10, .h = r.h + 10 }, 14, 0x2e3440);
        icons.draw(c, shell.font, app.icon, r.x, r.y, dock_icon);
        if (shell.findWindow(app.title) != null) {
            c.circle(@floatFromInt(r.x + dock_icon / 2), @floatFromInt(r.bottom() + 7), 2.5, text);
        }
    }
    dock.present();
}

fn dockEvent(e: gui.Event) void {
    switch (e) {
        .motion => |m| setDockHover(dockAt(m.x, m.y)),
        .leave => setDockHover(null),
        .press => |p| if (p.button == 1) {
            const i = dockAt(p.x, p.y) orelse return;
            const app = dock_apps[i];
            if (shell.findWindow(app.title)) |w| toggleWindow(w.id) else shell.launch(app.path);
        },
        .windows => windowsChanged(),
        else => {},
    }
}

fn dockAt(x: i32, y: i32) ?usize {
    for (0..dock_count) |i| {
        const r = dockSlot(i);
        if ((Rect{ .x = r.x - 5, .y = 0, .w = r.w + 10, .h = dock_h }).contains(x, y)) return i;
    }
    return null;
}

fn setDockHover(h: ?usize) void {
    if (h == dock_hover) return;
    dock_hover = h;
    drawDock();
}

// ---- the star menu ----

const menu_w = 280;
const menu_head = 70;
const menu_row = 42;

var menu: ?gui.Window = null;
var menu_hover: ?usize = null;

/// Rows: each app, then "Leave Vela".
fn menuRows() usize {
    return apps.count + 1;
}

fn menuRow(i: usize) Rect {
    const extra: i32 = if (i == apps.count) 13 else 0; // below the line before "Leave Vela"
    return .{ .x = 8, .y = menu_head + extra + @as(i32, @intCast(i)) * menu_row, .w = menu_w - 16, .h = menu_row - 4 };
}

fn openMenu() void {
    const h: u32 = @intCast(menuRow(menuRows() - 1).bottom() + 10);
    var m = gui.Window.open("Vela", menu_w, h) orelse return;
    if (!m.setRole(.popup, 6, bar_h + 6, menu_w, h, 14)) {
        m.close();
        return;
    }
    menu = m;
    menu_hover = null;
    drawMenu();
    drawBar();
}

fn closeMenu() void {
    if (menu) |*m| m.close();
    menu = null;
    drawBar();
}

fn drawMenu() void {
    const m = &(menu orelse return);
    const c = &m.canvas;
    c.fill(0x1c2029);
    c.roundRectOutline(c.bounds(), 14, 1, 0x343a47);
    icons.star(c, 34, 35, 14, gold);
    _ = c.text(shell.font, 19, 58, 33, "Vela", text);
    _ = c.text(shell.font, 12, 58, 51, "KernelProject2", dim);
    for (0..menuRows()) |i| {
        const r = menuRow(i);
        if (menu_hover == i) c.roundRect(r, 8, hover_bg);
        if (i < apps.count) {
            const app = apps.list()[i];
            icons.draw(c, shell.font, app.icon, r.x + 8, r.y + 5, 28);
            _ = c.text(shell.font, 14, @floatFromInt(r.x + 48), r.y + 24, app.name, text);
        } else {
            c.rect(.{ .x = 16, .y = r.y - 8, .w = menu_w - 32, .h = 1 }, 0x2e3440);
            _ = c.text(shell.font, 14, @floatFromInt(r.x + 12), r.y + 24, "Leave Vela", dim);
        }
    }
    m.present();
}

fn menuEvent(e: gui.Event) void {
    switch (e) {
        .motion => |p| setMenuHover(menuAt(p.x, p.y)),
        .leave => setMenuHover(null),
        .press => |p| if (p.button == 1) {
            if (menuAt(p.x, p.y)) |i| choose(i);
        },
        .key => |k| switch (k.code) {
            27 => closeMenu(),
            13 => if (menu_hover) |i| choose(i),
            gui.Key.down => setMenuHover(if (menu_hover) |i| (i + 1) % menuRows() else 0),
            gui.Key.up => setMenuHover(if (menu_hover) |i| (i + menuRows() - 1) % menuRows() else menuRows() - 1),
            else => {},
        },
        // Clicking anywhere else takes the keyboard away from the menu: it closes.
        .focus => |on| if (!on) closeMenu(),
        .windows => windowsChanged(),
        else => {},
    }
}

fn menuAt(x: i32, y: i32) ?usize {
    for (0..menuRows()) |i| {
        if (menuRow(i).contains(x, y)) return i;
    }
    return null;
}

fn setMenuHover(h: ?usize) void {
    if (h == menu_hover) return;
    menu_hover = h;
    drawMenu();
}

var leaving = false;

fn choose(i: usize) void {
    closeMenu();
    if (i < apps.count) shell.launch(apps.list()[i].path) else leaving = true;
}

// ---- together ----

var windows_dirty = false;

fn windowsChanged() void {
    windows_dirty = true;
}

pub fn main(_: rt.Args) u8 {
    shell.font = gui.Font.load(gui.theme.font_path) orelse {
        rt.err.write("vela: no font in /usr/share/fonts\n");
        return 1;
    };
    apps.init();
    for (apps.list()) |app| {
        if (app.pinned and dock_count < dock_apps.len) {
            dock_apps[dock_count] = app;
            dock_count += 1;
        }
    }

    desktop = gui.Window.open("Desktop", 64, 64) orelse {
        rt.err.write("vela: can't open a window (no /dev/window?)\n");
        return 1;
    };
    const screen = desktop.screenSize();
    if (!desktop.setRole(.desktop, 0, 0, screen.w, screen.h, 0)) {
        rt.err.write("vela: this kernel can't give windows roles\n");
        return 1;
    }
    sky = std.heap.page_allocator.alloc(u32, desktop.canvas.pixels.len) catch return 1;
    wallpaper.paint(&desktop.canvas);
    @memcpy(sky, desktop.canvas.pixels);

    bar = gui.Window.open("Bar", screen.w, bar_h) orelse return 1;
    _ = bar.setRole(.panel, 0, 0, screen.w, bar_h, 0);
    const dw = dockWidth();
    dock = gui.Window.open("Dock", dw, dock_h) orelse return 1;
    _ = dock.setRole(.panel, @intCast((screen.w - dw) / 2), @as(i32, @intCast(screen.h)) - dock_h - 10, dw, dock_h, 18);

    shell.refreshWindows(&bar);
    drawDesktop();
    drawBar();
    drawDock();

    while (!leaving) {
        var fds: [4]extern struct { fd: i32, events: i16 = 1, revents: i16 = 0 } = undefined;
        fds[0] = .{ .fd = desktop.fd };
        fds[1] = .{ .fd = bar.fd };
        fds[2] = .{ .fd = dock.fd };
        var n: usize = 3;
        if (menu) |m| {
            fds[3] = .{ .fd = m.fd };
            n = 4;
        }
        // Wake at least once a second for the clock.
        _ = sys.syscall3(7, @intFromPtr(&fds), n, 1000); // poll

        while (desktop.pending()) desktopEvent(desktop.next(0) orelse break);
        while (bar.pending()) barEvent(bar.next(0) orelse break);
        while (dock.pending()) dockEvent(dock.next(0) orelse break);
        while (menu != null and menu.?.pending()) menuEvent(menu.?.next(0) orelse break);

        if (windows_dirty) {
            windows_dirty = false;
            shell.refreshWindows(&bar);
            drawBar();
            drawDock();
        }
        const minute = shell.realSeconds() / 60;
        if (minute != shown_minute) {
            shown_minute = minute;
            drawBar();
        }
        shell.reap();
    }
    if (menu) |*m| m.close();
    dock.close();
    bar.close();
    desktop.close();
    return 0;
}
