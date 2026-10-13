//! The apps Vela knows: what they're called, how to start them, their icon, and the title their
//! window has (so the dock can tell which are running).

const sys = @import("../lib/sys.zig");

pub const Icon = enum { terminal, files, editor, calculator, clock, widgets, eyes, doom };

pub const App = struct {
    name: []const u8,
    path: [:0]const u8,
    icon: Icon,
    /// Part of its window's title.
    title: []const u8,
    /// On the dock.
    pinned: bool = false,
    /// Only there when this file is (Doom needs the game itself, not just our launcher).
    needs: ?[:0]const u8 = null,
};

const catalog = [_]App{
    .{ .name = "Terminal", .path = "/bin/terminal", .icon = .terminal, .title = "Terminal", .pinned = true },
    .{ .name = "Files", .path = "/bin/files", .icon = .files, .title = "Files", .pinned = true },
    .{ .name = "Text Editor", .path = "/bin/textedit", .icon = .editor, .title = "Text Editor", .pinned = true },
    .{ .name = "Calculator", .path = "/bin/calc", .icon = .calculator, .title = "Calculator", .pinned = true },
    .{ .name = "Clock", .path = "/bin/clock", .icon = .clock, .title = "Clock", .pinned = true },
    .{ .name = "Widgets", .path = "/bin/widgets", .icon = .widgets, .title = "Widgets" },
    .{ .name = "Eyes", .path = "/bin/eyes", .icon = .eyes, .title = "Eyes" },
    .{ .name = "Doom", .path = "/bin/doom", .icon = .doom, .title = "Doom", .needs = "/bin/chocolate-doom" },
};

/// The apps installed here, in catalog order.
pub var all: [catalog.len]*const App = undefined;
pub var count: usize = 0;

pub fn init() void {
    count = 0;
    for (&catalog) |*app| {
        if (!exists(app.path)) continue;
        if (app.needs) |p| if (!exists(p)) continue;
        all[count] = app;
        count += 1;
    }
}

pub fn list() []const *const App {
    return all[0..count];
}

fn exists(path: [*:0]const u8) bool {
    var st: sys.Stat = undefined;
    return sys.stat(path, &st) == .ok;
}
