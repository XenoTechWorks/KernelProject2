//! software: installs games and apps. It shows what Software's server has (lib/software.zig),
//! by category, each with its picture, what it is and how big; Install downloads and installs it
//! (with `pkg get`, in the background, with a progress bar), then Play or Open starts it and
//! Remove takes it away. Installed things are kept on the disk, so they stay after a restart.
//!
//!     software &
//!
//! Keys: Up and Down choose, Enter installs or starts the chosen one, Tab changes the category,
//! F5 asks the server again.

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
const icon = @import("lib/gui/icon.zig");
const png = @import("lib/png.zig");
const file = @import("lib/file.zig");
const http = @import("lib/http.zig");
const sw = @import("lib/software.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const Rect = gui.Rect;
const Canvas = gui.Canvas;
const gpa = std.heap.page_allocator;

// ---- colors and sizes (Files' dark Vista look) ----

const bg = 0x22262e;
const card = 0x272c35;
const card_hover = 0x2c323d;
const line = 0x343a46;
const text = 0xe9edf2;
const dim = 0x98a1ae;
const faint = 0x6f7886;
const select_edge = 0x6a9ee0;
const toolbar_h = 46;
const status_h = 26;
const row_h = 84;
const margin = 14;

// ---- the packages ----

const Item = struct {
    pkg: sw.Package,
    installed: bool = false,
    picture: ?png.Image = null,
    /// Asked the server (or the disk) for the picture already, whether or not it came.
    asked: bool = false,
};

var items: [256]Item = undefined;
var count: usize = 0;
/// What the items' text points into: the server's index, and installed packages' info files.
var texts: [257]?[]u8 = @splat(null);
var text_count: usize = 0;
/// Why there's nothing from the server, if there isn't.
var offline: ?[]const u8 = null;

const Tab = enum { all, games, apps, installed };
const tab_names = [_][]const u8{ "All", "Games", "Apps", "Installed" };
var tab: Tab = .all;

/// The items the current tab shows, by index into `items`.
var shown: [256]usize = undefined;
var shown_count: usize = 0;
var chosen: ?usize = null; // into `items`
var scroll: i32 = 0;

fn keepText(t: []u8) void {
    if (text_count < texts.len) {
        texts[text_count] = t;
        text_count += 1;
    }
}

fn freeAll() void {
    for (items[0..count]) |*it| if (it.picture) |*p| p.deinit();
    for (texts[0..text_count]) |t| if (t) |s| gpa.free(s);
    text_count = 0;
    count = 0;
}

/// What the server has, plus whatever is installed that it doesn't (or everything installed,
/// when the server can't be reached).
fn load() void {
    freeAll();
    offline = null;
    if (sw.fetch("index", null)) |index| {
        keepText(index);
        var found: [256]sw.Package = undefined;
        const n = sw.parseIndex(index, &found);
        for (found[0..n]) |p| {
            items[count] = .{ .pkg = p };
            count += 1;
        }
    } else |e| {
        offline = http.errorText(e);
    }
    var names_buf: [8192]u8 = undefined;
    var names: [256][]const u8 = undefined;
    const n = sw.installedNames(&names_buf, &names);
    for (names[0..n]) |name| {
        if (find(name) != null or count == items.len) continue;
        const info = sw.installedInfo(name) orelse continue;
        keepText(info);
        items[count] = .{ .pkg = sw.parseOne(info) };
        count += 1;
    }
    for (items[0..count]) |*it| it.installed = sw.isInstalled(it.pkg.name);
    filter();
}

fn find(name: []const u8) ?usize {
    for (items[0..count], 0..) |it, i| {
        if (std.mem.eql(u8, it.pkg.name, name)) return i;
    }
    return null;
}

fn filter() void {
    shown_count = 0;
    for (items[0..count], 0..) |it, i| {
        const keep = switch (tab) {
            .all => true,
            .games => std.mem.eql(u8, it.pkg.category, "Games"),
            .apps => !std.mem.eql(u8, it.pkg.category, "Games"),
            .installed => it.installed,
        };
        if (keep) {
            shown[shown_count] = i;
            shown_count += 1;
        }
    }
    if (chosen) |c| {
        if (std.mem.indexOfScalar(usize, shown[0..shown_count], c) == null) chosen = null;
    }
    clampScroll();
}

/// One picture a turn, after the window is up: the installed copy's, else the server's.
fn fetchOnePicture() bool {
    for (items[0..count]) |*it| {
        if (it.asked) continue;
        it.asked = true;
        var buf: [600]u8 = undefined;
        const local = std.fmt.bufPrintSentinel(&buf, "{s}/{s}/.package/icon.png", .{ sw.store, it.pkg.name }, 0) catch return true;
        const data = file.read(local) catch blk: {
            if (offline != null) return true;
            var what: [100]u8 = undefined;
            break :blk sw.fetch(std.fmt.bufPrint(&what, "icons/{s}.png", .{it.pkg.name}) catch return true, null) catch return true;
        };
        defer gpa.free(data);
        it.picture = png.decode(data) catch null;
        return true;
    }
    return false;
}

// ---- installing and removing, in the background ----

const Job = struct {
    item: usize,
    removing: bool,
    pid: usize,
    out: i32,
    got: usize = 0,
    total: usize = 0,
    unpacking: bool = false,
    partial: [256]u8 = undefined,
    partial_len: usize = 0,
    complaint: [200]u8 = undefined,
    complaint_len: usize = 0,
};
var job: ?Job = null;

var status_buf: [240]u8 = undefined;
var status_len: usize = 0;

fn setStatus(comptime fmt: []const u8, args: anytype) void {
    status_len = if (std.fmt.bufPrint(&status_buf, fmt, args)) |s| s.len else |_| 0;
}

const env = [_:null]?[*:0]const u8{
    "PATH=/bin:/sbin:/usr/bin:/usr/sbin:/usr/games",
    "HOME=/root",
    "USER=root",
    "TERM=linux",
    "LANG=C.UTF-8",
    null,
};

/// Runs `pkg` with these arguments, its output coming back through a pipe.
fn startPkg(i: usize, removing: bool) void {
    if (job != null) return;
    var fds: [2]i32 = undefined;
    if (sys.check(sys.syscall1(sys.nr.pipe, @intFromPtr(&fds))) != .ok) {
        setStatus("Can't start pkg (no pipe).", .{});
        return;
    }
    var name_buf: [80]u8 = undefined;
    const name = std.fmt.bufPrintSentinel(&name_buf, "{s}", .{items[i].pkg.name}, 0) catch return;
    const pid = sys.syscall0(sys.nr.fork);
    if (pid == 0) {
        _ = sys.syscall2(sys.nr.dup2, @bitCast(@as(isize, fds[1])), 1);
        _ = sys.syscall2(sys.nr.dup2, @bitCast(@as(isize, fds[1])), 2);
        sys.close(fds[0]);
        sys.close(fds[1]);
        const get = [_:null]?[*:0]const u8{ "pkg", "get", "--progress", name, null };
        const remove = [_:null]?[*:0]const u8{ "pkg", "remove", name, null };
        const argv: [*:null]const ?[*:0]const u8 = if (removing) &remove else &get;
        _ = sys.syscall3(sys.nr.execve, @intFromPtr("/bin/pkg"), @intFromPtr(argv), @intFromPtr(&env));
        sys.exit(127);
    }
    sys.close(fds[1]);
    job = .{ .item = i, .removing = removing, .pid = pid, .out = fds[0] };
    setStatus("{s} {s}…", .{ if (removing) "Removing" else "Downloading", items[i].pkg.title });
}

fn jobLine(j: *Job, l: []const u8) void {
    if (std.mem.startsWith(u8, l, "progress ")) {
        var words = std.mem.tokenizeScalar(u8, l["progress ".len..], ' ');
        j.got = std.fmt.parseInt(usize, words.next() orelse "0", 10) catch j.got;
        j.total = std.fmt.parseInt(usize, words.next() orelse "0", 10) catch j.total;
    } else if (std.mem.startsWith(u8, l, "Installing ")) {
        j.unpacking = true;
        setStatus("Installing {s}…", .{items[j.item].pkg.title});
    } else if (std.mem.startsWith(u8, l, "pkg: ")) {
        const say = l["pkg: ".len..];
        j.complaint_len = @min(say.len, j.complaint.len);
        @memcpy(j.complaint[0..j.complaint_len], say[0..j.complaint_len]);
    }
}

/// Reads what pkg has said, and notices it finishing. True when something changed.
fn pollJob() bool {
    const j = if (job) |*j| j else return false;
    var changed = false;
    while (true) {
        var pfd = [1]extern struct { fd: i32, events: i16 = 1, revents: i16 = 0 }{.{ .fd = j.out }};
        if (sys.check(sys.syscall3(7, @intFromPtr(&pfd), 1, 0)) != .ok or pfd[0].revents == 0) break;
        var buf: [512]u8 = undefined;
        const n = switch (sys.read(j.out, &buf)) {
            .ok => |n| n,
            .err => 0,
        };
        if (n == 0) break;
        changed = true;
        for (buf[0..n]) |ch| {
            if (ch == '\n') {
                jobLine(j, j.partial[0..j.partial_len]);
                j.partial_len = 0;
            } else if (j.partial_len < j.partial.len) {
                j.partial[j.partial_len] = ch;
                j.partial_len += 1;
            }
        }
    }
    var status: i32 = 0;
    const done = sys.syscall4(sys.nr.wait4, j.pid, @intFromPtr(&status), 1, 0); // WNOHANG
    if (done != j.pid) return changed;
    sys.close(j.out);
    const it = &items[j.item];
    const ok = status == 0;
    it.installed = sw.isInstalled(it.pkg.name);
    if (ok and j.removing) {
        setStatus("Removed {s}.", .{it.pkg.title});
    } else if (ok) {
        setStatus("Installed {s}. It's in the star menu too.", .{it.pkg.title});
    } else {
        setStatus("Couldn't {s} {s}: {s}", .{ if (j.removing) "remove" else "install", it.pkg.title, if (j.complaint_len > 0) j.complaint[0..j.complaint_len] else "pkg failed" });
    }
    job = null;
    filter();
    return true;
}

/// Starts an installed package, in a Terminal window if it's a text-mode one. It's started from
/// a child that exits straight away, so it isn't ours to wait for (only pkg is).
fn play(i: usize) void {
    const p = items[i].pkg;
    var run_buf: [300]u8 = undefined;
    const run = std.fmt.bufPrintSentinel(&run_buf, "{s}", .{p.run}, 0) catch return;
    const pid = sys.syscall0(sys.nr.fork);
    if (pid == 0) {
        _ = sys.syscall0(112); // setsid
        if (sys.syscall0(sys.nr.fork) != 0) sys.exit(0);
        _ = sys.syscall1(sys.nr.chdir, @intFromPtr("/root"));
        const in_terminal = [_:null]?[*:0]const u8{ "terminal", "-e", run, null };
        const by_shell = [_:null]?[*:0]const u8{ "kpsh", "-c", run, null };
        const path: [*:0]const u8 = if (p.terminal) "/bin/terminal" else "/bin/kpsh";
        const argv: [*:null]const ?[*:0]const u8 = if (p.terminal) &in_terminal else &by_shell;
        _ = sys.syscall3(sys.nr.execve, @intFromPtr(path), @intFromPtr(argv), @intFromPtr(&env));
        sys.exit(127);
    }
    var status: i32 = 0;
    _ = sys.syscall4(sys.nr.wait4, pid, @intFromPtr(&status), 0, 0);
    setStatus("Started {s}.", .{p.title});
}

// ---- layout ----

var font: *gui.Font = undefined;
var win: gui.Window = undefined;

fn listRect() Rect {
    return .{ .x = 0, .y = toolbar_h, .w = @intCast(win.canvas.w), .h = @as(i32, @intCast(win.canvas.h)) - toolbar_h - status_h };
}

fn rowRect(k: usize) Rect {
    const l = listRect();
    return .{ .x = margin, .y = l.y + 10 + @as(i32, @intCast(k)) * row_h - scroll, .w = l.w - 2 * margin, .h = row_h - 10 };
}

fn contentHeight() i32 {
    return 10 + @as(i32, @intCast(shown_count)) * row_h;
}

fn clampScroll() void {
    const room = listRect().h;
    scroll = @max(0, @min(scroll, contentHeight() - room));
}

fn ensureVisible(k: usize) void {
    const l = listRect();
    const r = rowRect(k);
    if (r.y < l.y) scroll -= l.y - r.y + 10;
    if (r.bottom() > l.bottom()) scroll += r.bottom() - l.bottom() + 10;
    clampScroll();
}

fn tabRect(t: usize) Rect {
    var x: i32 = 132;
    for (tab_names[0..t]) |name| x += @as(i32, @intFromFloat(gui.measure(font, 14, name))) + 30;
    return .{ .x = x, .y = 9, .w = @as(i32, @intFromFloat(gui.measure(font, 14, tab_names[t]))) + 24, .h = 28 };
}

/// The row's main button (Install / Play / Open) and, when installed, Remove.
fn mainButton(r: Rect) Rect {
    return .{ .x = r.right() - 16 - 96, .y = r.y + @divTrunc(r.h - 32, 2), .w = 96, .h = 32 };
}

fn removeButton(r: Rect) Rect {
    return .{ .x = r.right() - 16 - 96 - 10 - 88, .y = r.y + @divTrunc(r.h - 32, 2), .w = 88, .h = 32 };
}

fn rowAt(x: i32, y: i32) ?usize {
    if (!listRect().contains(x, y)) return null;
    for (0..shown_count) |k| {
        if (rowRect(k).contains(x, y)) return k;
    }
    return null;
}

// ---- drawing ----

fn vgradient(c: *Canvas, r: Rect, top: u32, bottom: u32) void {
    var y: i32 = 0;
    while (y < r.h) : (y += 1) {
        c.rect(.{ .x = r.x, .y = r.y + y, .w = r.w, .h = 1 }, gui.mix(bottom, top, @intCast(@as(u32, @intCast(y)) * 255 / @as(u32, @intCast(@max(r.h, 1))))));
    }
}

const Look = enum { primary, plain, danger };

fn button(c: *Canvas, r: Rect, label: []const u8, look: Look, lit: bool, enabled: bool) void {
    const base: u32, const shine: u32 = switch (look) {
        .primary => if (lit) .{ 0x3a7ed4, 0x4d92e6 } else .{ 0x2f6dbb, 0x3f80d0 },
        .plain => if (lit) .{ 0x465063, 0x535e73 } else .{ 0x3b4352, 0x485162 },
        .danger => if (lit) .{ 0xc24a3a, 0xd65c4b } else .{ 0x3b4352, 0x485162 },
    };
    c.roundRect(r, 6, base);
    c.roundRect(.{ .x = r.x + 1, .y = r.y + 1, .w = r.w - 2, .h = @divTrunc(r.h, 2) }, 5, shine);
    c.roundRectOutline(r, 6, 1, 0x12151a);
    const tw = gui.measure(font, 14, label);
    _ = c.text(font, 14, @as(f32, @floatFromInt(r.x)) + (@as(f32, @floatFromInt(r.w)) - tw) / 2, r.y + 21, label, if (enabled) 0xffffff else 0x8a93a2);
    if (!enabled) {
        var yy: i32 = r.y;
        while (yy < r.bottom()) : (yy += 1) {
            var xx: i32 = r.x;
            while (xx < r.right()) : (xx += 1) c.blend(xx, yy, card, 120);
        }
    }
}

fn sizeText(buf: []u8, size: usize) []const u8 {
    if (size < 1024 * 1024) return std.fmt.bufPrint(buf, "{d} KB", .{(size + 1023) / 1024}) catch "";
    return std.fmt.bufPrint(buf, "{d}.{d} MB", .{ size >> 20, (size & 0xfffff) * 10 >> 20 }) catch "";
}

/// Text cut short with "…" to fit `width`.
fn fit(buf: []u8, s: []const u8, size: f32, width: f32) []const u8 {
    if (gui.measure(font, size, s) <= width) return s;
    var end = s.len;
    while (end > 0) {
        end -= 1;
        while (end > 0 and s[end] & 0xc0 == 0x80) end -= 1;
        const candidate = std.fmt.bufPrint(buf, "{s}…", .{s[0..end]}) catch return s[0..0];
        if (gui.measure(font, size, candidate) <= width) return candidate;
    }
    return s[0..0];
}

var hover_row: ?usize = null;
var hover_tab: ?usize = null;
var hover_main = false;
var hover_remove = false;

fn draw() void {
    const c = &win.canvas;
    c.clip = c.bounds();
    c.fill(bg);
    const w: i32 = @intCast(c.w);
    const h: i32 = @intCast(c.h);

    // The toolbar: the name, then the categories as tabs.
    vgradient(c, .{ .x = 0, .y = 0, .w = w, .h = toolbar_h }, 0x3a4352, 0x262c36);
    c.rect(.{ .x = 0, .y = toolbar_h - 1, .w = w, .h = 1 }, 0x101317);
    _ = icon.draw(c, "system-software-install", 14, 9, 28);
    _ = c.text(font, 17, 50, 30, "Software", text);
    for (tab_names, 0..) |name, t| {
        const r = tabRect(t);
        const on = @intFromEnum(tab) == t;
        if (on) {
            c.roundRect(r, 6, 0x4a5d7c);
            c.roundRect(.{ .x = r.x + 1, .y = r.y + 1, .w = r.w - 2, .h = @divTrunc(r.h, 2) }, 5, 0x5a6f92);
        } else if (hover_tab == t) {
            c.roundRect(r, 6, 0x343d4b);
        }
        _ = c.text(font, 14, @floatFromInt(r.x + 12), r.y + 19, name, if (on) 0xffffff else text);
    }

    // The packages.
    const l = listRect();
    c.clip = l;
    for (shown[0..shown_count], 0..) |i, k| {
        const r = rowRect(k);
        if (r.bottom() < l.y or r.y > l.bottom()) continue;
        drawRow(c, i, k, r);
    }
    if (shown_count == 0) {
        const msg: []const u8 = if (tab == .installed) "Nothing installed yet. Pick something from All, Games or Apps." else if (offline != null) "Software's server can't be reached, so there's nothing to show." else "Nothing here.";
        _ = c.text(font, 15, @floatFromInt(l.x + 24), l.y + 44, msg, dim);
    }
    c.clip = c.bounds();

    // The status line.
    vgradient(c, .{ .x = 0, .y = h - status_h, .w = w, .h = status_h }, 0x2a303a, 0x22272f);
    c.rect(.{ .x = 0, .y = h - status_h, .w = w, .h = 1 }, 0x101317);
    var sb: [300]u8 = undefined;
    const st = if (status_len > 0) status_buf[0..status_len] else if (offline) |why|
        std.fmt.bufPrint(&sb, "Can't reach Software's server: {s}. Installed things still work.", .{why}) catch ""
    else blk: {
        var installed: usize = 0;
        for (items[0..count]) |it| installed += @intFromBool(it.installed);
        var srv: [512]u8 = undefined;
        break :blk std.fmt.bufPrint(&sb, "{d} available, {d} installed   ·   from {s}", .{ count, installed, sw.server(&srv) }) catch "";
    };
    var fb: [300]u8 = undefined;
    _ = c.text(font, 13, 12, h - 8, fit(&fb, st, 13, @floatFromInt(w - 24)), dim);
    win.present();
}

fn drawRow(c: *Canvas, i: usize, k: usize, r: Rect) void {
    const it = &items[i];
    const p = it.pkg;
    c.roundRect(r, 8, if (hover_row == k) card_hover else card);
    if (chosen == i) c.roundRectOutline(r, 8, 1.5, select_edge) else c.roundRectOutline(r, 8, 1, line);

    if (it.picture) |pic| {
        icon.image(c, pic, r.x + 14, r.y + @divTrunc(r.h - 48, 2), 48);
    } else {
        _ = icon.draw(c, if (std.mem.eql(u8, p.category, "Games")) "applications-games" else "package-x-generic", r.x + 14, r.y + @divTrunc(r.h - 48, 2), 48);
    }

    const tx: f32 = @floatFromInt(r.x + 78);
    const room: f32 = @floatFromInt(removeButton(r).x - 16 - (r.x + 78));
    var b1: [200]u8 = undefined;
    var b2: [300]u8 = undefined;
    var b3: [200]u8 = undefined;
    _ = c.text(font, 16, tx, r.y + 26, fit(&b1, p.title, 16, room), text);
    _ = c.text(font, 13, tx, r.y + 46, fit(&b2, p.about, 13, room), dim);
    var sz: [32]u8 = undefined;
    const meta = std.fmt.bufPrint(&b3, "{s}  ·  {s}{s}  ·  version {s}", .{
        p.category,
        sizeText(&sz, if (p.installed_size > 0) p.installed_size else p.size),
        if (p.terminal) "  ·  runs in Terminal" else "",
        p.version,
    }) catch "";
    _ = c.text(font, 12, tx, r.y + 64, fit(&b1, meta, 12, room), faint);

    const main_r = mainButton(r);
    const lit = hover_row == k;
    if (job) |j| if (j.item == i) {
        // Its progress, where the buttons were.
        const bar = Rect{ .x = removeButton(r).x, .y = main_r.y + 4, .w = main_r.right() - removeButton(r).x, .h = 8 };
        c.roundRect(bar, 4, 0x1a1e24);
        const frac: f32 = if (j.removing or j.unpacking) 1.0 else if (j.total > 0) @as(f32, @floatFromInt(j.got)) / @as(f32, @floatFromInt(j.total)) else 0.0;
        if (frac > 0) c.roundRect(.{ .x = bar.x, .y = bar.y, .w = @max(8, @as(i32, @intFromFloat(@as(f32, @floatFromInt(bar.w)) * frac))), .h = bar.h }, 4, 0x3f86dc);
        var pb: [80]u8 = undefined;
        var g: [32]u8 = undefined;
        var t: [32]u8 = undefined;
        const what = if (j.removing) "Removing…" else if (j.unpacking) "Installing…" else if (j.total > 0)
            std.fmt.bufPrint(&pb, "{s} of {s}", .{ sizeText(&g, j.got), sizeText(&t, j.total) }) catch ""
        else
            "Downloading…";
        _ = c.text(font, 12, @floatFromInt(bar.x), bar.bottom() + 16, what, dim);
        return;
    };
    const busy = job != null;
    if (it.installed) {
        const games = std.mem.eql(u8, p.category, "Games");
        button(c, main_r, if (games) "Play" else "Open", .primary, lit and hover_main, true);
        button(c, removeButton(r), "Remove", .danger, lit and hover_remove and !busy, !busy);
    } else {
        button(c, main_r, "Install", .primary, lit and hover_main and !busy, !busy and offline == null);
    }
}

// ---- events ----

fn motion(x: i32, y: i32) bool {
    const before = .{ hover_row, hover_tab, hover_main, hover_remove };
    hover_row = rowAt(x, y);
    hover_tab = null;
    for (0..tab_names.len) |t| {
        if (tabRect(t).contains(x, y)) hover_tab = t;
    }
    hover_main = false;
    hover_remove = false;
    if (hover_row) |k| {
        hover_main = mainButton(rowRect(k)).contains(x, y);
        hover_remove = items[shown[k]].installed and removeButton(rowRect(k)).contains(x, y);
    }
    return !std.meta.eql(before, .{ hover_row, hover_tab, hover_main, hover_remove });
}

fn setTab(t: Tab) void {
    tab = t;
    scroll = 0;
    filter();
}

fn act(i: usize) void {
    if (items[i].installed) play(i) else if (offline == null) startPkg(i, false);
}

fn press(x: i32, y: i32) void {
    for (0..tab_names.len) |t| {
        if (tabRect(t).contains(x, y)) return setTab(@enumFromInt(t));
    }
    const k = rowAt(x, y) orelse return;
    const i = shown[k];
    chosen = i;
    const r = rowRect(k);
    if (job != null and job.?.item == i) return;
    if (mainButton(r).contains(x, y)) {
        act(i);
    } else if (items[i].installed and removeButton(r).contains(x, y)) {
        startPkg(i, true);
    }
}

fn key(k: gui.Key) void {
    const at: ?usize = if (chosen) |c| std.mem.indexOfScalar(usize, shown[0..shown_count], c) else null;
    switch (k.code) {
        gui.Key.down, gui.Key.up => {
            if (shown_count == 0) return;
            const next: usize = if (at) |a|
                (if (k.code == gui.Key.down) @min(a + 1, shown_count - 1) else a -| 1)
            else
                0;
            chosen = shown[next];
            ensureVisible(next);
        },
        '\r', '\n' => if (chosen) |c| act(c),
        '\t' => setTab(@enumFromInt((@intFromEnum(tab) + 1) % tab_names.len)),
        gui.Key.f5 => if (job == null) {
            status_len = 0;
            load();
        },
        else => {},
    }
}

pub fn main(_: rt.Args) u8 {
    font = gui.Font.load(gui.theme.font_path) orelse {
        rt.err.write("software: no font in /usr/share/fonts\n");
        return 1;
    };
    win = gui.Window.open("Software", 860, 600) orelse {
        rt.err.write("software: can't open a window (no /dev/window?)\n");
        return 1;
    };
    setStatus("Asking Software's server what it has…", .{});
    draw();
    status_len = 0;
    load();
    draw();
    while (true) {
        // Quick turns while there's work going on (pictures to fetch, pkg running).
        const busy = job != null;
        const e = win.next(if (busy) 100 else 250) orelse break;
        var redraw = true;
        switch (e) {
            .motion => |m| redraw = motion(m.x, m.y),
            .leave => {
                hover_row = null;
                hover_tab = null;
                hover_main = false;
                hover_remove = false;
            },
            .press => |p| if (p.button == 1) press(p.x, p.y),
            .key => |k| key(k),
            .scroll => |s| {
                scroll += s.steps * 48;
                clampScroll();
            },
            .resize => clampScroll(),
            .tick => {
                redraw = pollJob();
                if (fetchOnePicture()) redraw = true;
            },
            else => redraw = false,
        }
        if (job != null and e != .tick) _ = pollJob();
        if (redraw and !win.pending()) draw();
    }
    win.close();
    return 0;
}
