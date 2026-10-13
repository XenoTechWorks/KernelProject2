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
const glass = @import("vela/glass.zig");
const settings = @import("vela/settings.zig");
const jpeg = @import("lib/jpeg.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const Rect = gui.Rect;

const text = 0xf2f5f9;
const dim = 0xa9b2bf;
const accent = 0x3a8ee6;

/// The glass of the bar and the dock: tinted with the window color, as clear as Personalize says.
fn panelGlass() glass.Style {
    const color = settings.colors[settings.color];
    return .{ .tint = gui.mix(color.bottom, 0x000000, 110), .strength = settings.glasses[settings.glass].strength };
}

/// The darker glass of the menus (which may be over windows as well as the wallpaper, so less
/// shows through).
fn popupGlass() glass.Style {
    const color = settings.colors[settings.color];
    return .{ .tint = gui.mix(color.bottom, 0x05070a, 70), .strength = 205, .radius = 12, .shine = 22 };
}

/// Text that stays readable on glass: a soft dark shadow under it.
fn shadowText(c: *gui.Canvas, size: f32, x: f32, y: i32, s: []const u8, color: u32) void {
    _ = c.text(shell.font, size, x + 1, y + 1, s, 0x000000);
    _ = c.text(shell.font, size, x, y, s, color);
}

// ---- the desktop: wallpaper and icons ----

const icon_size = 64;
const cell_w = 96;
const cell_h = 100;

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
        if (selected == i) {
            glass.tint(c, cell, 6, 0x9cc8ff, 70);
            glass.outline(c, cell, 6, 0xcfe4ff, 120);
        }
        icons.draw(c, app.icon, cell.x + (cell_w - icon_size) / 2, cell.y + 6, icon_size);
        const size = 13;
        var buf: [64]u8 = undefined;
        const label = shell.fit(&buf, app.name, size, cell_w - 8);
        const w = gui.measure(shell.font, size, label);
        const lx = @as(f32, @floatFromInt(cell.x)) + (cell_w - w) / 2;
        const ly = cell.y + 6 + icon_size + 17;
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

// ---- the bar: star menu, open windows, clock ----

const bar_h = 34;

var bar: gui.Window = undefined;
const BarHit = union(enum) { star, task: u32 };
var bar_hover: ?BarHit = null;
var task_rects: [32]struct { id: u32, rect: Rect } = undefined;
var task_count: usize = 0;
var shown_minute: u64 = std.math.maxInt(u64);

/// The orb: the star menu's button, at the left end of the bar.
const star_button = Rect{ .x = 3, .y = 0, .w = 40, .h = bar_h };
const orb_center = [2]f32{ 23, bar_h / 2 };

fn drawBar() void {
    const c = &bar.canvas;
    glass.frost(c, sky, desktop.canvas.w, 0, 0, panelGlass());

    const star_lit = menu != null or (bar_hover != null and bar_hover.? == .star);
    glass.orb(c, orb_center[0], orb_center[1], 13, star_lit, icons.star);

    // The clock, on the right.
    var clock_buf: [48]u8 = undefined;
    const clock = clockText(&clock_buf);
    const clock_w = gui.measure(shell.font, 14, clock);
    const clock_x = @as(f32, @floatFromInt(c.w)) - clock_w - 16;
    shadowText(c, 14, clock_x, 22, clock, text);

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
    const start: i32 = star_button.right() + 8;
    const room = @as(i32, @intFromFloat(clock_x)) - 16 - start;
    if (windows.len > 0 and room > 40) {
        const each = @min(200, @divTrunc(room, @as(i32, @intCast(windows.len))));
        for (windows, 0..) |win, i| {
            const r = Rect{ .x = start + @as(i32, @intCast(i)) * each, .y = 4, .w = each - 4, .h = bar_h - 8 };
            task_rects[task_count] = .{ .id = win.id, .rect = r };
            task_count += 1;
            const hovered = bar_hover != null and bar_hover.? == .task and bar_hover.?.task == win.id;
            const active = win.focused() and !win.minimized();
            // Glassy buttons: the one in front lit up and edged in light, others only on hover.
            if (active) {
                glass.tint(c, r, 4, 0x9cc8ff, if (hovered) 70 else 50);
                glass.outline(c, r, 4, 0xffffff, 90);
            } else if (hovered) {
                glass.tint(c, r, 4, 0xffffff, 28);
                glass.outline(c, r, 4, 0xffffff, 55);
            }
            var buf: [96]u8 = undefined;
            const title = shell.fit(&buf, if (win.name().len > 0) win.name() else "Window", 13, @floatFromInt(r.w - 20));
            shadowText(c, 13, @floatFromInt(r.x + 10), 21, title, if (win.minimized()) dim else text);
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
/// Where the dock is on the screen (its glass shows the wallpaper there).
var dock_x: i32 = 0;
var dock_y: i32 = 0;
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
    glass.frost(c, sky, desktop.canvas.w, dock_x, dock_y, panelGlass());
    glass.outline(c, c.bounds(), 18, 0xffffff, 70);
    for (dock_apps[0..dock_count], 0..) |app, i| {
        const r = dockSlot(i);
        if (dock_hover == i) {
            const lit = Rect{ .x = r.x - 5, .y = r.y - 5, .w = r.w + 10, .h = r.h + 10 };
            glass.tint(c, lit, 10, 0xffffff, 34);
            glass.outline(c, lit, 10, 0xffffff, 60);
        }
        icons.draw(c, app.icon, r.x, r.y, dock_icon);
        if (shell.findWindow(app.title) != null) {
            c.circle(@floatFromInt(r.x + dock_icon / 2), @floatFromInt(r.bottom() + 7), 2.5, 0xd9ecff);
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
    showTooltip(if (h) |i| i else null);
}

// ---- the dock's tooltips: an app's name above its icon ----

var tooltip: ?gui.Window = null;

fn showTooltip(slot: ?usize) void {
    if (tooltip) |*t| t.close();
    tooltip = null;
    const i = slot orelse return;
    const name = dock_apps[i].name;
    const w: u32 = @intFromFloat(gui.measure(shell.font, 13, name) + 22);
    const h: u32 = 26;
    var t = gui.Window.open("Tooltip", w, h) orelse return;
    const r = dockSlot(i);
    const x = dock_x + r.x + dock_icon / 2 - @as(i32, @intCast(w / 2));
    if (!t.setRole(.tooltip, x, dock_y - @as(i32, h) - 8, w, h, 7)) {
        t.close();
        return;
    }
    const c = &t.canvas;
    c.fill(0x1a202b);
    glass.outline(c, c.bounds(), 7, 0xffffff, 70);
    shadowText(c, 13, 11, 18, name, text);
    t.present();
    tooltip = t;
}

// ---- the star menu ----

const menu_w = 280;
const menu_head = 70;
const menu_row = 42;

var menu: ?gui.Window = null;
var menu_hover: ?usize = null;
const menu_x = 6;
const menu_y = bar_h + 6;

/// Rows: each app, then "Personalize…" and "Leave Vela".
fn menuRows() usize {
    return apps.count + 2;
}

fn menuRow(i: usize) Rect {
    const extra: i32 = if (i >= apps.count) 13 else 0; // below the line before the last two
    return .{ .x = 8, .y = menu_head + extra + @as(i32, @intCast(i)) * menu_row, .w = menu_w - 16, .h = menu_row - 4 };
}

fn openMenu() void {
    const h: u32 = @intCast(menuRow(menuRows() - 1).bottom() + 10);
    var m = gui.Window.open("Vela", menu_w, h) orelse return;
    if (!m.setRole(.popup, menu_x, menu_y, menu_w, h, 14)) {
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
    glass.frost(c, sky, desktop.canvas.w, menu_x, menu_y, popupGlass());
    glass.outline(c, c.bounds(), 14, 0xffffff, 60);
    glass.orb(c, 33, 35, 15, false, icons.star);
    shadowText(c, 19, 58, 33, "Vela", text);
    shadowText(c, 12, 58, 51, "KernelProject2", dim);
    for (0..menuRows()) |i| {
        const r = menuRow(i);
        if (menu_hover == i) {
            glass.tint(c, r, 5, 0x9cc8ff, 55);
            glass.outline(c, r, 5, 0xcfe4ff, 110);
        }
        if (i < apps.count) {
            const app = apps.list()[i];
            icons.draw(c, app.icon, r.x + 6, r.y + 3, 32);
            shadowText(c, 14, @floatFromInt(r.x + 48), r.y + 24, app.name, text);
        } else {
            if (i == apps.count) glass.tint(c, .{ .x = 16, .y = r.y - 8, .w = menu_w - 32, .h = 1 }, 0, 0xffffff, 40);
            icons.drawExtra(c, if (i == apps.count) .wallpaper else .leave, r.x + 6, r.y + 3, 32);
            shadowText(c, 14, @floatFromInt(r.x + 48), r.y + 24, if (i == apps.count) "Personalize…" else "Leave Vela", text);
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
    if (i < apps.count) {
        shell.launch(apps.list()[i].path);
    } else if (i == apps.count) {
        openPersonalize();
    } else {
        leaving = true;
    }
}

// ---- Personalize: the wallpaper, the window color, the glass, starting at boot ----

const pz_w = 470;
const pz_pad = 22;
const swatch_r = 15;
const swatch_step = 46;

var personalize: ?gui.Window = null;
var pz_x: i32 = 0;
var pz_y: i32 = 0;
var pz_thumb: ?jpeg.Image = null;

const PzHit = union(enum) { change, color: usize, glass: usize, autostart };

const pz_change = Rect{ .x = 202, .y = 120, .w = 112, .h = 30 };
fn pzSwatch(i: usize) [2]f32 {
    return .{ @floatFromInt(pz_pad + swatch_r + @as(i32, @intCast(i)) * swatch_step), 252 };
}
fn pzGlass(i: usize) Rect {
    return .{ .x = pz_pad + @as(i32, @intCast(i)) * 104, .y = 312, .w = 98, .h = 30 };
}
const pz_check = Rect{ .x = pz_pad, .y = 372, .w = 330, .h = 22 };
const pz_h = 414;

fn openPersonalize() void {
    if (personalize != null) return;
    const screen = desktop.screenSize();
    var p = gui.Window.open("Personalize", pz_w, pz_h) orelse return;
    const x: i32 = @intCast((screen.w - pz_w) / 2);
    const y: i32 = @intCast((screen.h - pz_h) / 2 - 20);
    if (!p.setRole(.popup, x, y, pz_w, pz_h, 14)) {
        p.close();
        return;
    }
    personalize = p;
    pz_x = x;
    pz_y = y;
    loadPzThumb();
    drawPersonalize();
}

fn loadPzThumb() void {
    if (pz_thumb) |*t| t.deinit();
    pz_thumb = wallpaper.thumbnail(wallpaper.chosen());
}

fn closePersonalize() void {
    if (personalize) |*p| p.close();
    personalize = null;
}

fn drawPersonalize() void {
    const p = &(personalize orelse return);
    const c = &p.canvas;
    glass.frost(c, sky, desktop.canvas.w, pz_x, pz_y, popupGlass());
    glass.outline(c, c.bounds(), 14, 0xffffff, 60);
    shadowText(c, 18, pz_pad, 36, "Personalize", text);

    // The wallpaper: what's there now, and a button to choose another.
    shadowText(c, 13, pz_pad, 72, "Desktop background", dim);
    const thumb = Rect{ .x = pz_pad, .y = 82, .w = 160, .h = 100 };
    if (pz_thumb) |img| wallpaper.drawCover(c, img, thumb, 255) else c.rect(thumb, 0x2a2f39);
    glass.outline(c, thumb, 0, 0xffffff, 80);
    var title: []const u8 = wallpaper.chosen();
    for (wallpaper.choices) |ch| {
        if (std.mem.eql(u8, ch.slug, wallpaper.chosen())) title = ch.title;
    }
    shadowText(c, 15, 202, 108, title, text);
    button(c, pz_change, "Change…", false);

    // The window color: a row of swatches.
    var label_buf: [64]u8 = undefined;
    const label = std.fmt.bufPrint(&label_buf, "Window color: {s}", .{settings.colors[settings.color].label}) catch "";
    shadowText(c, 13, pz_pad, 222, label, dim);
    for (settings.colors, 0..) |color, i| {
        const at = pzSwatch(i);
        if (i == settings.color) c.circle(at[0], at[1], swatch_r + 4, 0xffffff);
        glass.sphere(c, at[0], at[1], swatch_r, color.rim, color.bottom);
    }

    // The glass: how much the bar and dock let through.
    shadowText(c, 13, pz_pad, 302, "Glass", dim);
    for (settings.glasses, 0..) |g, i| button(c, pzGlass(i), g.label, i == settings.glass);

    // Starting at boot.
    const box = Rect{ .x = pz_check.x, .y = pz_check.y + 2, .w = 18, .h = 18 };
    if (settings.autostart) {
        glass.tint(c, box, 4, accent, 255);
        c.line(@floatFromInt(box.x + 4), @floatFromInt(box.y + 9), @floatFromInt(box.x + 8), @floatFromInt(box.y + 13), 2, 0xffffff);
        c.line(@floatFromInt(box.x + 8), @floatFromInt(box.y + 13), @floatFromInt(box.x + 14), @floatFromInt(box.y + 5), 2, 0xffffff);
    } else {
        glass.tint(c, box, 4, 0xffffff, 30);
    }
    glass.outline(c, box, 4, 0xffffff, 120);
    shadowText(c, 14, @floatFromInt(box.right() + 10), box.y + 14, "Start Vela when the computer starts", text);
    p.present();
}

/// A glassy push button; `on` lights it up (the chosen one of a set).
fn button(c: *gui.Canvas, r: Rect, label: []const u8, on: bool) void {
    glass.tint(c, r, 6, if (on) accent else 0xffffff, if (on) 200 else 30);
    glass.outline(c, r, 6, 0xffffff, if (on) 150 else 80);
    const w = gui.measure(shell.font, 14, label);
    shadowText(c, 14, @as(f32, @floatFromInt(r.x)) + (@as(f32, @floatFromInt(r.w)) - w) / 2, r.y + 20, label, text);
}

fn pzAt(x: i32, y: i32) ?PzHit {
    if (pz_change.contains(x, y)) return .change;
    for (0..settings.colors.len) |i| {
        const at = pzSwatch(i);
        const dx = @as(f32, @floatFromInt(x)) - at[0];
        const dy = @as(f32, @floatFromInt(y)) - at[1];
        if (dx * dx + dy * dy <= (swatch_r + 4) * (swatch_r + 4)) return .{ .color = i };
    }
    for (0..settings.glasses.len) |i| {
        if (pzGlass(i).contains(x, y)) return .{ .glass = i };
    }
    if (pz_check.contains(x, y)) return .autostart;
    return null;
}

fn personalizeEvent(e: gui.Event) void {
    switch (e) {
        .press => |m| if (m.button == 1) {
            const hit = pzAt(m.x, m.y) orelse return;
            switch (hit) {
                .change => {
                    closePersonalize();
                    openPicker();
                    return;
                },
                .color => |i| {
                    settings.color = i;
                    applyColor();
                },
                .glass => |i| settings.glass = i,
                .autostart => settings.autostart = !settings.autostart,
            }
            settings.save();
            drawBar();
            drawDock();
            drawPersonalize();
        },
        .key => |k| if (k.code == 27) closePersonalize(),
        .focus => |on| if (!on) closePersonalize(),
        .windows => windowsChanged(),
        else => {},
    }
}

/// Tells the kernel the window color (the frames of the window in front take it).
fn applyColor() void {
    const color = settings.colors[settings.color];
    desktop.setTheme(color.top, color.bottom, color.rim);
}

// ---- the wallpaper picker ----

const thumb_w = 192;
const thumb_h = 120;
const picker_cols = 4;
const picker_pad = 22;
const picker_gap = 16;
const picker_head = 58;
const picker_row = thumb_h + 30;

var picker: ?gui.Window = null;
var picker_x: i32 = 0;
var picker_y: i32 = 0;
var picker_hover: ?usize = null;
var thumbs: [48]?jpeg.Image = @splat(null);
var thumbs_loaded = false;

fn pickerSlot(i: usize) Rect {
    const col: i32 = @intCast(i % picker_cols);
    const row: i32 = @intCast(i / picker_cols);
    return .{ .x = picker_pad + col * (thumb_w + picker_gap), .y = picker_head + row * picker_row, .w = thumb_w, .h = thumb_h };
}

fn openPicker() void {
    if (picker != null or wallpaper.choices.len == 0) return;
    if (!thumbs_loaded) {
        for (wallpaper.choices, 0..) |ch, i| thumbs[i] = wallpaper.thumbnail(ch.slug);
        thumbs_loaded = true;
    }
    const rows = (wallpaper.choices.len + picker_cols - 1) / picker_cols;
    const w: u32 = 2 * picker_pad + picker_cols * thumb_w + (picker_cols - 1) * picker_gap;
    const h: u32 = @intCast(picker_head + @as(i32, @intCast(rows)) * picker_row + 8);
    const screen = desktop.screenSize();
    var p = gui.Window.open("Desktop Background", w, h) orelse return;
    const x: i32 = @intCast((screen.w -| w) / 2);
    const y: i32 = @max(bar_h + 8, @as(i32, @intCast((screen.h -| h) / 2)) - 20);
    if (!p.setRole(.popup, x, y, w, h, 14)) {
        p.close();
        return;
    }
    picker = p;
    picker_x = x;
    picker_y = y;
    picker_hover = null;
    drawPicker();
}

fn closePicker() void {
    if (picker) |*p| p.close();
    picker = null;
}

fn drawPicker() void {
    const p = &(picker orelse return);
    const c = &p.canvas;
    glass.frost(c, sky, desktop.canvas.w, picker_x, picker_y, popupGlass());
    glass.outline(c, c.bounds(), 14, 0xffffff, 60);
    shadowText(c, 18, picker_pad, 36, "Desktop Background", text);
    const hint = "Click a picture to use it. Esc closes.";
    shadowText(c, 13, @as(f32, @floatFromInt(c.w)) - picker_pad - gui.measure(shell.font, 13, hint), 35, hint, dim);
    for (wallpaper.choices, 0..) |ch, i| {
        const r = pickerSlot(i);
        const current = std.mem.eql(u8, ch.slug, wallpaper.chosen());
        const ring = Rect{ .x = r.x - 4, .y = r.y - 4, .w = r.w + 8, .h = r.h + 8 };
        if (current) {
            glass.tint(c, ring, 6, 0x9cc8ff, 150);
            glass.outline(c, ring, 6, 0xffffff, 160);
        } else if (picker_hover == i) {
            glass.tint(c, ring, 6, 0xffffff, 60);
        }
        if (thumbs[i]) |img| wallpaper.drawCover(c, img, r, 255) else c.rect(r, 0x2a2f39);
        var buf: [64]u8 = undefined;
        const label = shell.fit(&buf, ch.title, 13, thumb_w);
        const lw = gui.measure(shell.font, 13, label);
        shadowText(c, 13, @as(f32, @floatFromInt(r.x)) + (thumb_w - lw) / 2, r.bottom() + 19, label, if (current) text else 0xd5dbe4);
    }
    p.present();
}

fn pickerAt(x: i32, y: i32) ?usize {
    for (0..wallpaper.choices.len) |i| {
        const r = pickerSlot(i);
        if ((Rect{ .x = r.x - 4, .y = r.y - 4, .w = r.w + 8, .h = r.h + 26 }).contains(x, y)) return i;
    }
    return null;
}

fn pickerEvent(e: gui.Event) void {
    switch (e) {
        .motion => |m| {
            const h = pickerAt(m.x, m.y);
            if (h != picker_hover) {
                picker_hover = h;
                drawPicker();
            }
        },
        .leave => if (picker_hover != null) {
            picker_hover = null;
            drawPicker();
        },
        .press => |m| if (m.button == 1) {
            const i = pickerAt(m.x, m.y) orelse return;
            wallpaper.choose(wallpaper.choices[i].slug);
            drawPicker();
            applyWallpaper();
        },
        .key => |k| if (k.code == 27) closePicker(),
        .focus => |on| if (!on) closePicker(),
        .windows => windowsChanged(),
        else => {},
    }
}

/// Paints the chosen wallpaper and the icons over it.
fn applyWallpaper() void {
    wallpaper.paint(&desktop.canvas);
    @memcpy(sky, desktop.canvas.pixels);
    drawDesktop();
    drawBar();
    drawDock();
    drawPicker();
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
    settings.load();
    applyColor();
    wallpaper.init();
    wallpaper.paint(&desktop.canvas);
    @memcpy(sky, desktop.canvas.pixels);

    bar = gui.Window.open("Bar", screen.w, bar_h) orelse return 1;
    _ = bar.setRole(.panel, 0, 0, screen.w, bar_h, 0);
    const dw = dockWidth();
    dock = gui.Window.open("Dock", dw, dock_h) orelse return 1;
    dock_x = @intCast((screen.w - dw) / 2);
    dock_y = @as(i32, @intCast(screen.h)) - dock_h - 10;
    _ = dock.setRole(.panel, dock_x, dock_y, dw, dock_h, 18);

    shell.refreshWindows(&bar);
    drawDesktop();
    drawBar();
    drawDock();

    while (!leaving) {
        var fds: [7]extern struct { fd: i32, events: i16 = 1, revents: i16 = 0 } = undefined;
        fds[0] = .{ .fd = desktop.fd };
        fds[1] = .{ .fd = bar.fd };
        fds[2] = .{ .fd = dock.fd };
        var n: usize = 3;
        for ([_]?gui.Window{ menu, picker, personalize, tooltip }) |w| {
            if (w) |m| {
                fds[n] = .{ .fd = m.fd };
                n += 1;
            }
        }
        // Wake at least once a second for the clock.
        _ = sys.syscall3(7, @intFromPtr(&fds), n, 1000); // poll

        while (desktop.pending()) desktopEvent(desktop.next(0) orelse break);
        while (bar.pending()) barEvent(bar.next(0) orelse break);
        while (dock.pending()) dockEvent(dock.next(0) orelse break);
        while (menu != null and menu.?.pending()) menuEvent(menu.?.next(0) orelse break);
        while (picker != null and picker.?.pending()) pickerEvent(picker.?.next(0) orelse break);
        while (personalize != null and personalize.?.pending()) personalizeEvent(personalize.?.next(0) orelse break);
        while (tooltip != null and tooltip.?.pending()) _ = tooltip.?.next(0); // nothing to do

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
    closePicker();
    closePersonalize();
    showTooltip(null);
    dock.close();
    bar.close();
    desktop.close();
    return 0;
}
