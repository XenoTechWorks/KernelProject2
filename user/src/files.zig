//! files: browse folders and peek into files. Double-click a folder (or press Enter) to open it,
//! Up to go back; picking a file shows its first lines. Run it as `files &` or `files /some/dir &`.

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

var path_buf: [1024]u8 = undefined;
var path_len: usize = 0;

var path_label: *gui.Widget = undefined;
var status: *gui.Widget = undefined;
var entries: *gui.Widget = undefined;
var preview: *gui.Widget = undefined;

/// The folder's entries: names one after another, folders first. `is_dir` per entry.
const max_entries = 512;
var names: [64 * 1024]u8 = undefined;
var name_ends: [max_entries]usize = undefined;
var is_dir: [max_entries]bool = undefined;
var count: usize = 0;

fn path() []const u8 {
    return path_buf[0..path_len];
}

fn name(i: usize) []const u8 {
    return names[if (i == 0) 0 else name_ends[i - 1]..name_ends[i]];
}

fn load() void {
    count = 0;
    var cpath: [1025]u8 = undefined;
    const p = rt.cstr(&cpath, path()) orelse return;
    const fd: i32 = switch (sys.open(p, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => |e| {
            gui.setText(status, sys.errorText(e));
            gui.setItems(entries, &.{});
            return;
        },
    };
    defer sys.close(fd);

    // Read every entry (getdents64), keeping folders and files apart to put folders first.
    var dirs: [max_entries][]const u8 = undefined;
    var files: [max_entries][]const u8 = undefined;
    var nd: usize = 0;
    var nf: usize = 0;
    var scratch: [64 * 1024]u8 = undefined;
    var used: usize = 0;
    var buf: [8192]u8 align(8) = undefined;
    outer: while (true) {
        const n = switch (sys.check(sys.syscall3(217, @bitCast(@as(isize, fd)), @intFromPtr(&buf), buf.len))) {
            .ok => |n| n,
            .err => break,
        };
        if (n == 0) break;
        var at: usize = 0;
        while (at < n) {
            const reclen = std.mem.readInt(u16, buf[at + 16 ..][0..2], .little);
            const kind = buf[at + 18];
            const entry = std.mem.sliceTo(buf[at + 19 .. at + reclen], 0);
            at += reclen;
            if (std.mem.eql(u8, entry, ".") or std.mem.eql(u8, entry, "..")) continue;
            if (nd + nf >= max_entries or used + entry.len > scratch.len) break :outer;
            @memcpy(scratch[used..][0..entry.len], entry);
            const copy = scratch[used..][0..entry.len];
            used += entry.len;
            if (kind == 4 or (kind == 0 and isDir(copy))) { // DT_DIR (or unknown: ask)
                dirs[nd] = copy;
                nd += 1;
            } else {
                files[nf] = copy;
                nf += 1;
            }
        }
    }
    const lessThan = struct {
        fn f(_: void, a: []const u8, b: []const u8) bool {
            return std.ascii.lessThanIgnoreCase(a, b);
        }
    }.f;
    std.mem.sort([]const u8, dirs[0..nd], {}, lessThan);
    std.mem.sort([]const u8, files[0..nf], {}, lessThan);

    // Rows: folders end with "/".
    var rows: [max_entries][]const u8 = undefined;
    var at: usize = 0;
    for (dirs[0..nd]) |d| add(d, true, &at, &rows);
    for (files[0..nf]) |f| add(f, false, &at, &rows);
    gui.setItems(entries, rows[0..count]);
    gui.setText(path_label, path());
    var sbuf: [64]u8 = undefined;
    gui.setText(status, std.fmt.bufPrint(&sbuf, "{d} folders, {d} files", .{ nd, nf }) catch "");
    gui.setItems(preview, &.{});
}

fn add(entry: []const u8, dir: bool, at: *usize, rows: *[max_entries][]const u8) void {
    const len = entry.len + @intFromBool(dir);
    if (at.* + len > names.len) return;
    @memcpy(names[at.*..][0..entry.len], entry);
    if (dir) names[at.* + entry.len] = '/';
    rows[count] = names[at.*..][0..len];
    at.* += len;
    name_ends[count] = at.*;
    is_dir[count] = dir;
    count += 1;
}

fn isDir(entry: []const u8) bool {
    var full: [1100]u8 = undefined;
    const p = std.fmt.bufPrintSentinel(&full, "{s}/{s}", .{ path(), entry }, 0) catch return false;
    var st: sys.Stat = undefined;
    return sys.stat(p, &st) == .ok and st.mode & sys.S_IFMT == sys.S_IFDIR;
}

/// The selected entry's full path.
fn selectedPath(buf: []u8) ?[:0]const u8 {
    const i = gui.selected(entries) orelse return null;
    var n = name(i);
    if (is_dir[i]) n = n[0 .. n.len - 1];
    const sep = if (path_len > 0 and path_buf[path_len - 1] == '/') "" else "/";
    return std.fmt.bufPrintSentinel(buf, "{s}{s}{s}", .{ path(), sep, n }, 0) catch null;
}

fn opened(_: *gui.App, _: ?*anyopaque) void {
    const i = gui.selected(entries) orelse return;
    if (!is_dir[i]) return;
    var buf: [1100]u8 = undefined;
    const p = selectedPath(&buf) orelse return;
    if (p.len > path_buf.len) return;
    @memcpy(path_buf[0..p.len], p);
    path_len = p.len;
    load();
}

fn up(_: *gui.App, _: ?*anyopaque) void {
    if (path_len <= 1) return;
    const cut = std.mem.lastIndexOfScalar(u8, path(), '/') orelse return;
    path_len = if (cut == 0) 1 else cut;
    load();
}

/// Shows the first lines of the picked file (or what it is, for a folder or a binary file).
fn picked(_: *gui.App, _: ?*anyopaque) void {
    const i = gui.selected(entries) orelse return;
    var pbuf: [1100]u8 = undefined;
    const p = selectedPath(&pbuf) orelse return;
    var st: sys.Stat = undefined;
    var sbuf: [128]u8 = undefined;
    if (sys.stat(p, &st) == .ok) {
        gui.setText(status, std.fmt.bufPrint(&sbuf, "{s}: {d} bytes", .{ name(i), st.size }) catch "");
    }
    if (is_dir[i]) {
        gui.setItems(preview, &.{"(a folder: double-click to open it)"});
        return;
    }
    const fd: i32 = switch (sys.open(p, sys.O_RDONLY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => |e| {
            gui.setItems(preview, &.{sys.errorText(e)});
            return;
        },
    };
    defer sys.close(fd);
    var data: [16 * 1024]u8 = undefined;
    const n = switch (sys.read(fd, &data)) {
        .ok => |n| n,
        .err => 0,
    };
    if (std.mem.indexOfScalar(u8, data[0..n], 0) != null) {
        gui.setItems(preview, &.{"(a binary file)"});
        return;
    }
    var lines: [300][]const u8 = undefined;
    var nl: usize = 0;
    var it = std.mem.splitScalar(u8, data[0..n], '\n');
    while (it.next()) |line| {
        if (nl == lines.len) break;
        // Tabs as spaces would need a copy; trimming them keeps it simple.
        lines[nl] = std.mem.trimEnd(u8, line[0..@min(line.len, 160)], "\r");
        nl += 1;
    }
    gui.setItems(preview, lines[0..nl]);
}

pub fn main(args: rt.Args) u8 {
    const start = args.get(1) orelse blk: {
        const r = sys.check(sys.syscall2(sys.nr.getcwd, @intFromPtr(&path_buf), path_buf.len));
        break :blk switch (r) {
            .ok => |n| path_buf[0 .. n - 1],
            .err => "/",
        };
    };
    path_len = @min(start.len, path_buf.len);
    std.mem.copyForwards(u8, path_buf[0..path_len], start[0..path_len]);

    const app = gui.App.init("Files") orelse {
        rt.err.write("files: can't start (no /dev/window, or no font in /usr/share/fonts)\n");
        return 1;
    };
    path_label = app.label("", .{});
    path_label.expand = true;
    status = app.label("", .{ .dim = true });
    entries = app.list();
    entries.kind.list.on_activate = .{ .f = opened };
    entries.kind.list.on_select = .{ .f = picked };
    preview = app.list();
    load();

    const root = app.vbox(.{ .padding = 16, .spacing = 12 }, &.{
        app.hbox(.{ .spacing = 12 }, &.{ app.button("Up", .{ .f = up }), path_label }),
        app.hbox(.{ .spacing = 12, .expand = true, .homogeneous = true }, &.{ entries, preview }),
        status,
    });
    return app.run(root, 720, 460);
}
