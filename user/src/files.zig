//! files: the file explorer, after Windows Vista's Explorer. A toolbar (back, forward, up, the
//! folder's path as buttons, icons or details), places on the left, the folder's contents in the
//! middle, and a status line. Double-click (or Enter) opens: a folder here, a picture in the
//! picture viewer, text in the text editor, a program by running it. Right-click for the rest:
//! cut, copy, paste, rename, delete, a new folder, properties.
//!
//!     files [folder] &
//!
//! Keys: arrows move, Enter opens, Backspace goes up, F2 renames,
//! Delete deletes, Ctrl+C/X/V copy, cut and paste, Ctrl+Shift+N makes a folder, Ctrl+H shows hidden
//! files, F5 reloads.

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
const icon = @import("lib/gui/icon.zig");
const file = @import("lib/file.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const Rect = gui.Rect;
const Canvas = gui.Canvas;

// ---- colors and sizes ----

const bg = 0x22262e;
const pane = 0x1c2027;
const line = 0x343a46;
const text = 0xe9edf2;
const dim = 0x98a1ae;
const hover_fill = 0x2b3341;
const select_fill = 0x2d4c78;
const select_edge = 0x6a9ee0;
const toolbar_h = 46;
const status_h = 26;
const side_w = 176;
const cell_w = 104;
const cell_h = 94;
const row_h = 24;
const header_h = 26;

// ---- the folder ----

const Kind = enum { folder, text, picture, program, windows, package, archive, script, audio, html, source, other };

const Entry = struct {
    name: []const u8,
    kind: Kind,
    size: u64,
    mtime: i64,
    mode: u32,
};

const max_entries = 2048;
var names: [256 * 1024]u8 = undefined;
var entries: [max_entries]Entry = undefined;
var count: usize = 0;
var selected: [max_entries]bool = @splat(false);
var focus: ?usize = null;
var show_hidden = false;

var path_buf: [1024]u8 = undefined;
var path_len: usize = 1;

fn path() []const u8 {
    return path_buf[0..path_len];
}

fn setPath(p: []const u8) void {
    var n = @min(p.len, path_buf.len);
    std.mem.copyForwards(u8, path_buf[0..n], p[0..n]);
    while (n > 1 and path_buf[n - 1] == '/') n -= 1;
    path_len = @max(1, n);
}

/// The full path of `name` in the current folder, NUL-terminated.
fn join(buf: []u8, name: []const u8) [:0]const u8 {
    const sep = if (path_len == 1) "" else "/";
    return std.fmt.bufPrintSentinel(buf, "{s}{s}{s}", .{ path(), sep, name }, 0) catch buf[0..0 :0];
}

fn kindOf(name: []const u8, mode: u32) Kind {
    if (mode & sys.S_IFMT == sys.S_IFDIR) return .folder;
    const dot = std.mem.lastIndexOfScalar(u8, name, '.');
    const ext = if (dot) |d| name[d + 1 ..] else "";
    const is = struct {
        fn f(e: []const u8, list: []const []const u8) bool {
            for (list) |x| {
                if (std.ascii.eqlIgnoreCase(e, x)) return true;
            }
            return false;
        }
    }.f;
    if (is(ext, &.{ "jpg", "jpeg", "png", "ppm", "bmp", "gif" })) return .picture;
    if (is(ext, &.{ "txt", "md", "conf", "cfg", "ini", "log", "csv", "json", "toml", "yaml", "yml" }) or std.mem.eql(u8, name, "README")) return .text;
    if (is(ext, &.{ "c", "h", "cpp", "hpp", "zig", "rs", "py", "js", "go", "java" })) return .source;
    if (is(ext, &.{ "sh", "bash" })) return .script;
    if (is(ext, &.{ "html", "htm", "css" })) return .html;
    if (is(ext, &.{ "exe", "dll" })) return .windows;
    if (is(ext, &.{ "deb", "rpm" })) return .package;
    if (is(ext, &.{ "zip", "gz", "tgz", "tar", "xz", "zst", "bz2", "7z", "cpio" })) return .archive;
    if (is(ext, &.{ "wav", "mp3", "ogg", "flac" })) return .audio;
    if (mode & 0o111 != 0) return .program;
    return .other;
}

fn iconName(k: Kind, name: []const u8) []const u8 {
    return switch (k) {
        .folder => if (std.mem.eql(u8, name, "root")) "user-home" else "folder",
        .text => "text-plain",
        .picture => "image-x-generic",
        .program => "application-x-executable",
        .windows => "application-x-ms-dos-executable",
        .package => "application-x-deb",
        .archive => if (std.mem.endsWith(u8, name, ".zip")) "application-zip" else "application-x-compressed-tar",
        .script => "application-x-shellscript",
        .audio => "audio-x-generic",
        .html => "text-html",
        .source => "text-x-csrc",
        .other => "application-octet-stream",
    };
}

fn typeName(k: Kind) []const u8 {
    return switch (k) {
        .folder => "Folder",
        .text => "Text document",
        .picture => "Picture",
        .program => "Program",
        .windows => "Windows program",
        .package => "Package",
        .archive => "Archive",
        .script => "Script",
        .audio => "Sound",
        .html => "Web page",
        .source => "Source code",
        .other => "File",
    };
}

/// Reads the current folder: folders first, then files, each by name.
fn load() void {
    count = 0;
    focus = null;
    @memset(&selected, false);
    scroll = 0;
    var cpath: [1025]u8 = undefined;
    const p = rt.cstr(&cpath, path()) orelse return;
    const fd: i32 = switch (sys.open(p, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => |e| {
            setStatusErr("Can't open this folder", e);
            return;
        },
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
            const entry = std.mem.sliceTo(buf[at + 19 .. at + reclen], 0);
            at += reclen;
            if (std.mem.eql(u8, entry, ".") or std.mem.eql(u8, entry, "..")) continue;
            if (!show_hidden and entry[0] == '.') continue;
            if (count == max_entries or used + entry.len > names.len) break :outer;
            @memcpy(names[used..][0..entry.len], entry);
            const name = names[used..][0..entry.len];
            used += entry.len;
            var full: [1300]u8 = undefined;
            var st: sys.Stat = undefined;
            const ok = sys.stat(join(&full, name), &st) == .ok;
            const mode: u32 = if (ok) st.mode else 0;
            entries[count] = .{ .name = name, .kind = kindOf(name, mode), .size = if (ok) @intCast(@max(st.size, 0)) else 0, .mtime = if (ok) st.mtime else 0, .mode = mode };
            count += 1;
        }
    }
    std.mem.sort(Entry, entries[0..count], {}, struct {
        fn less(_: void, a: Entry, b: Entry) bool {
            if ((a.kind == .folder) != (b.kind == .folder)) return a.kind == .folder;
            return std.ascii.lessThanIgnoreCase(a.name, b.name);
        }
    }.less);
    status_override = false;
}

// ---- history ----

const History = struct {
    paths: [32][256]u8 = undefined,
    lens: [32]usize = undefined,
    n: usize = 0,

    fn push(h: *History, p: []const u8) void {
        if (p.len > 256) return;
        if (h.n == h.paths.len) {
            std.mem.copyForwards([256]u8, h.paths[0 .. h.n - 1], h.paths[1..h.n]);
            std.mem.copyForwards(usize, h.lens[0 .. h.n - 1], h.lens[1..h.n]);
            h.n -= 1;
        }
        @memcpy(h.paths[h.n][0..p.len], p);
        h.lens[h.n] = p.len;
        h.n += 1;
    }

    fn pop(h: *History) ?[]const u8 {
        if (h.n == 0) return null;
        h.n -= 1;
        return h.paths[h.n][0..h.lens[h.n]];
    }
};

var back: History = .{};
var forward: History = .{};

/// Goes to a folder (remembering where we were, for Back).
fn go(p: []const u8) void {
    var tmp: [1024]u8 = undefined;
    const n = @min(p.len, tmp.len);
    @memcpy(tmp[0..n], p[0..n]);
    back.push(path());
    forward.n = 0;
    setPath(tmp[0..n]);
    load();
}

fn goBack() void {
    var tmp: [256]u8 = undefined;
    const p = back.pop() orelse return;
    @memcpy(tmp[0..p.len], p);
    forward.push(path());
    setPath(tmp[0..p.len]);
    load();
}

fn goForward() void {
    var tmp: [256]u8 = undefined;
    const p = forward.pop() orelse return;
    @memcpy(tmp[0..p.len], p);
    back.push(path());
    setPath(tmp[0..p.len]);
    load();
}

fn goUp() void {
    if (path_len <= 1) return;
    const cut = std.mem.lastIndexOfScalar(u8, path(), '/') orelse return;
    var tmp: [1024]u8 = undefined;
    const up = if (cut == 0) "/" else path()[0..cut];
    @memcpy(tmp[0..up.len], up);
    go(tmp[0..up.len]);
}

// ---- opening things ----

const env = [_:null]?[*:0]const u8{
    "PATH=/bin:/sbin:/usr/bin:/usr/sbin:/usr/games",
    "HOME=/root",
    "USER=root",
    "TERM=linux",
    "LANG=C.UTF-8",
    null,
};

/// Starts a program on its own (not waiting), in the current folder.
fn launch(program: [*:0]const u8, arg: ?[*:0]const u8) void {
    const pid = sys.syscall0(sys.nr.fork);
    if (pid != 0) return;
    _ = sys.syscall0(112); // setsid
    var cdir: [1025]u8 = undefined;
    if (rt.cstr(&cdir, path())) |d| _ = sys.syscall1(sys.nr.chdir, @intFromPtr(d));
    const argv = [_:null]?[*:0]const u8{ program, arg, null };
    _ = sys.syscall3(sys.nr.execve, @intFromPtr(program), @intFromPtr(&argv), @intFromPtr(&env));
    sys.exit(127);
}

fn reap() void {
    while (true) {
        var status: i32 = 0;
        const pid = sys.syscall4(sys.nr.wait4, @bitCast(@as(isize, -1)), @intFromPtr(&status), 1, 0);
        if (sys.Result.of(pid) == .err or pid == 0) return;
    }
}

fn open(i: usize) void {
    const e = entries[i];
    var full: [1300]u8 = undefined;
    const p = join(&full, e.name);
    switch (e.kind) {
        .folder => go(p),
        .picture => launch("/bin/pictures", p),
        .text, .source, .html, .script => launch("/bin/textedit", p),
        .program, .windows => launch(p, null),
        .package, .archive => setStatus("Packages and archives: install with the Software app, or pkg install in a terminal."),
        .audio => setStatus("There's no sound player yet."),
        .other => if (looksLikeText(p)) launch("/bin/textedit", p) else setStatus("Nothing opens this kind of file yet."),
    }
}

fn looksLikeText(p: [*:0]const u8) bool {
    const fd: i32 = switch (sys.open(p, sys.O_RDONLY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return false,
    };
    defer sys.close(fd);
    var buf: [1024]u8 = undefined;
    const n = switch (sys.read(fd, &buf)) {
        .ok => |n| n,
        .err => return false,
    };
    return std.mem.indexOfScalar(u8, buf[0..n], 0) == null;
}

// ---- changing things: new folder, rename, delete, copy and paste ----

var clip_buf: [16 * 1024]u8 = undefined;
var clip_len: usize = 0; // paths separated by NUL
var clip_cut = false;

fn copySelection(cut: bool) void {
    clip_len = 0;
    var n: usize = 0;
    for (0..count) |i| {
        if (!selected[i]) continue;
        var full: [1300]u8 = undefined;
        const p = join(&full, entries[i].name);
        if (clip_len + p.len + 1 > clip_buf.len) break;
        @memcpy(clip_buf[clip_len..][0..p.len], p);
        clip_buf[clip_len + p.len] = 0;
        clip_len += p.len + 1;
        n += 1;
    }
    clip_cut = cut;
    var b: [96]u8 = undefined;
    setStatus(std.fmt.bufPrint(&b, "{d} item{s} {s}. Paste them in another folder.", .{ n, if (n == 1) "" else "s", if (cut) "cut" else "copied" }) catch "");
}

fn paste() void {
    var it = std.mem.splitScalar(u8, clip_buf[0..clip_len], 0);
    var done: usize = 0;
    var failed: usize = 0;
    while (it.next()) |src| {
        if (src.len == 0) continue;
        const base = src[(std.mem.lastIndexOfScalar(u8, src, '/') orelse 0) + 1 ..];
        var dst_buf: [1300]u8 = undefined;
        const dst = freeName(&dst_buf, base);
        var src_buf: [1300]u8 = undefined;
        const src_z = std.fmt.bufPrintSentinel(&src_buf, "{s}", .{src}, 0) catch continue;
        const ok = if (clip_cut)
            (sys.check(sys.syscall2(sys.nr.rename, @intFromPtr(src_z.ptr), @intFromPtr(dst.ptr))) == .ok or (copyTree(src_z, dst) and file.removeTree(src_z)))
        else
            copyTree(src_z, dst);
        if (ok) done += 1 else failed += 1;
    }
    if (clip_cut) clip_len = 0;
    load();
    var b: [96]u8 = undefined;
    setStatus(std.fmt.bufPrint(&b, "Pasted {d} item{s}{s}", .{ done, if (done == 1) "" else "s", if (failed > 0) "; some couldn't be." else "." }) catch "");
}

/// A name in the current folder that isn't taken: `base`, else "base (2)", "base (3)"...
fn freeName(buf: []u8, base: []const u8) [:0]const u8 {
    var n: usize = 1;
    while (n < 100) : (n += 1) {
        var name_buf: [300]u8 = undefined;
        const name = if (n == 1) base else blk: {
            const dot = std.mem.lastIndexOfScalar(u8, base, '.');
            break :blk if (dot != null and dot.? > 0)
                std.fmt.bufPrint(&name_buf, "{s} ({d}){s}", .{ base[0..dot.?], n, base[dot.?..] }) catch base
            else
                std.fmt.bufPrint(&name_buf, "{s} ({d})", .{ base, n }) catch base;
        };
        const p = join(buf, name);
        var st: sys.Stat = undefined;
        if (sys.lstat(p, &st) != .ok) return p;
    }
    return join(buf, base);
}

/// Copies a file, or a folder and everything in it.
fn copyTree(src: [*:0]const u8, dst: [*:0]const u8) bool {
    var st: sys.Stat = undefined;
    if (sys.lstat(src, &st) != .ok) return false;
    if (st.mode & sys.S_IFMT == sys.S_IFDIR) {
        if (sys.check(sys.syscall2(sys.nr.mkdir, @intFromPtr(dst), st.mode & 0o7777)) != .ok) return false;
        var ok = true;
        var kids = Children.open(src) orelse return false;
        defer kids.close();
        while (kids.next()) |name| {
            var a: [1300]u8 = undefined;
            var b: [1300]u8 = undefined;
            const s = std.fmt.bufPrintSentinel(&a, "{s}/{s}", .{ std.mem.span(src), name }, 0) catch continue;
            const d = std.fmt.bufPrintSentinel(&b, "{s}/{s}", .{ std.mem.span(dst), name }, 0) catch continue;
            if (!copyTree(s, d)) ok = false;
        }
        return ok;
    }
    const in: i32 = switch (sys.open(src, sys.O_RDONLY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return false,
    };
    defer sys.close(in);
    const out: i32 = switch (sys.open(dst, sys.O_WRONLY | sys.O_CREAT | sys.O_TRUNC, st.mode & 0o7777)) {
        .ok => |fd| @intCast(fd),
        .err => return false,
    };
    defer sys.close(out);
    var buf: [64 * 1024]u8 = undefined;
    while (true) {
        const n = switch (sys.read(in, &buf)) {
            .ok => |n| n,
            .err => return false,
        };
        if (n == 0) return true;
        rt.writeAll(out, buf[0..n]);
    }
}

/// A folder's entries, one at a time.
const Children = struct {
    fd: i32,
    buf: [4096]u8 align(8) = undefined,
    n: usize = 0,
    at: usize = 0,

    fn open(p: [*:0]const u8) ?Children {
        return switch (sys.open(p, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
            .ok => |fd| .{ .fd = @intCast(fd) },
            .err => null,
        };
    }

    fn close(c: *Children) void {
        sys.close(c.fd);
    }

    fn next(c: *Children) ?[]const u8 {
        while (true) {
            if (c.at >= c.n) {
                c.n = switch (sys.check(sys.syscall3(sys.nr.getdents64, @bitCast(@as(isize, c.fd)), @intFromPtr(&c.buf), c.buf.len))) {
                    .ok => |n| n,
                    .err => return null,
                };
                c.at = 0;
                if (c.n == 0) return null;
            }
            const reclen = std.mem.readInt(u16, c.buf[c.at + 16 ..][0..2], .little);
            const name = std.mem.sliceTo(c.buf[c.at + 19 .. c.at + reclen], 0);
            c.at += reclen;
            if (!std.mem.eql(u8, name, ".") and !std.mem.eql(u8, name, "..")) return name;
        }
    }
};

fn newFolder() void {
    var buf: [1300]u8 = undefined;
    const p = freeName(&buf, "New folder");
    if (sys.check(sys.syscall2(sys.nr.mkdir, @intFromPtr(p.ptr), 0o755)) != .ok) {
        setStatus("Couldn't make a folder here.");
        return;
    }
    const name = p[(std.mem.lastIndexOfScalar(u8, p, '/') orelse 0) + 1 ..];
    var keep: [300]u8 = undefined;
    @memcpy(keep[0..name.len], name);
    load();
    for (0..count) |i| {
        if (std.mem.eql(u8, entries[i].name, keep[0..name.len])) {
            selectOnly(i);
            startRename(i);
        }
    }
}

// Renaming: a text field over the item's name.
var renaming: ?usize = null;
var rename_buf: [255]u8 = undefined;
var rename_len: usize = 0;

fn startRename(i: usize) void {
    renaming = i;
    rename_len = @min(entries[i].name.len, rename_buf.len);
    @memcpy(rename_buf[0..rename_len], entries[i].name[0..rename_len]);
}

fn finishRename() void {
    const i = renaming orelse return;
    renaming = null;
    const new = rename_buf[0..rename_len];
    if (new.len == 0 or std.mem.eql(u8, new, entries[i].name) or std.mem.indexOfScalar(u8, new, '/') != null) return;
    var a: [1300]u8 = undefined;
    var b: [1300]u8 = undefined;
    const from = join(&a, entries[i].name);
    const to = join(&b, new);
    var st: sys.Stat = undefined;
    if (sys.lstat(to, &st) == .ok) {
        setStatus("There's already something with that name.");
        return;
    }
    if (sys.check(sys.syscall2(sys.nr.rename, @intFromPtr(from.ptr), @intFromPtr(to.ptr))) != .ok) {
        setStatus("Couldn't rename it.");
        return;
    }
    var keep: [255]u8 = undefined;
    @memcpy(keep[0..new.len], new);
    load();
    for (0..count) |k| {
        if (std.mem.eql(u8, entries[k].name, keep[0..new.len])) selectOnly(k);
    }
}

// ---- overlays: the right-click menu, asking before deleting, properties ----

const MenuItem = enum { open, cut, copy, paste, rename, delete, new_folder, refresh, properties };

const Overlay = union(enum) {
    none,
    menu: struct { x: i32, y: i32, on_item: bool },
    confirm_delete,
    properties: usize,
};
var overlay: Overlay = .none;
var menu_hover: ?usize = null;

fn menuItems(on_item: bool) []const MenuItem {
    return if (on_item)
        &.{ .open, .cut, .copy, .rename, .delete, .properties }
    else
        &.{ .paste, .new_folder, .refresh };
}

fn menuLabel(m: MenuItem) []const u8 {
    return switch (m) {
        .open => "Open",
        .cut => "Cut",
        .copy => "Copy",
        .paste => "Paste",
        .rename => "Rename",
        .delete => "Delete",
        .new_folder => "New folder",
        .refresh => "Refresh",
        .properties => "Properties",
    };
}

fn menuIcon(m: MenuItem) []const u8 {
    return switch (m) {
        .open => "document-open",
        .cut => "edit-cut",
        .copy => "edit-copy",
        .paste => "edit-paste",
        .rename => "edit-rename",
        .delete => "edit-delete",
        .new_folder => "folder-new",
        .refresh => "view-refresh",
        .properties => "document-properties",
    };
}

const menu_w = 190;
const menu_row = 28;

fn menuRect() Rect {
    const m = overlay.menu;
    const n: i32 = @intCast(menuItems(m.on_item).len);
    const h = n * menu_row + 10;
    const x = @min(m.x, @as(i32, @intCast(win.canvas.w)) - menu_w - 4);
    const y = @min(m.y, @as(i32, @intCast(win.canvas.h)) - h - 4);
    return .{ .x = x, .y = y, .w = menu_w, .h = h };
}

fn doMenu(m: MenuItem) void {
    overlay = .none;
    switch (m) {
        .open => if (firstSelected()) |i| open(i),
        .cut => copySelection(true),
        .copy => copySelection(false),
        .paste => paste(),
        .rename => if (firstSelected()) |i| startRename(i),
        .delete => if (firstSelected() != null) {
            overlay = .confirm_delete;
        },
        .new_folder => newFolder(),
        .refresh => load(),
        .properties => if (firstSelected()) |i| {
            overlay = .{ .properties = i };
        },
    }
}

fn deleteSelection() void {
    var done: usize = 0;
    var failed: usize = 0;
    for (0..count) |i| {
        if (!selected[i]) continue;
        var full: [1300]u8 = undefined;
        if (file.removeTree(join(&full, entries[i].name))) done += 1 else failed += 1;
    }
    load();
    var b: [96]u8 = undefined;
    setStatus(std.fmt.bufPrint(&b, "Deleted {d} item{s}{s}", .{ done, if (done == 1) "" else "s", if (failed > 0) "; some couldn't be." else "." }) catch "");
}

// ---- selection ----

fn firstSelected() ?usize {
    for (0..count) |i| {
        if (selected[i]) return i;
    }
    return null;
}

fn selectOnly(i: usize) void {
    @memset(selected[0..count], false);
    selected[i] = true;
    focus = i;
    ensureVisible(i);
}

fn selectionCount() usize {
    var n: usize = 0;
    for (selected[0..count]) |s| n += @intFromBool(s);
    return n;
}

// ---- the window ----

var win: gui.Window = undefined;
var font: *gui.Font = undefined;
var details = false;
var scroll: i32 = 0;
var hover: ?usize = null;
var hover_button: ?Button = null;
var status_buf: [200]u8 = undefined;
var status_len: usize = 0;
var status_override = false;

const Button = enum { back, forward, up, icons, list, new_folder };

fn setStatus(s: []const u8) void {
    status_len = @min(s.len, status_buf.len);
    @memcpy(status_buf[0..status_len], s[0..status_len]);
    status_override = true;
}

fn setStatusErr(what: []const u8, e: u16) void {
    var b: [160]u8 = undefined;
    setStatus(std.fmt.bufPrint(&b, "{s}: {s}.", .{ what, sys.errorText(e) }) catch what);
}

fn mainRect() Rect {
    return .{ .x = side_w + 1, .y = toolbar_h, .w = @as(i32, @intCast(win.canvas.w)) - side_w - 1, .h = @as(i32, @intCast(win.canvas.h)) - toolbar_h - status_h };
}

fn columns() i32 {
    return @max(1, @divTrunc(mainRect().w - 16, cell_w));
}

/// Where item i is drawn (in window pixels, scrolled).
fn itemRect(i: usize) Rect {
    const m = mainRect();
    if (details) {
        return .{ .x = m.x + 6, .y = m.y + header_h + @as(i32, @intCast(i)) * row_h - scroll, .w = m.w - 12, .h = row_h };
    }
    const cols = columns();
    const col: i32 = @mod(@as(i32, @intCast(i)), cols);
    const row: i32 = @divTrunc(@as(i32, @intCast(i)), cols);
    return .{ .x = m.x + 8 + col * cell_w, .y = m.y + 8 + row * cell_h - scroll, .w = cell_w - 6, .h = cell_h - 6 };
}

fn contentHeight() i32 {
    if (details) return header_h + @as(i32, @intCast(count)) * row_h + 8;
    const rows = @divTrunc(@as(i32, @intCast(count)) + columns() - 1, columns());
    return rows * cell_h + 16;
}

fn clampScroll() void {
    const room = mainRect().h;
    scroll = std.math.clamp(scroll, 0, @max(0, contentHeight() - room));
}

fn ensureVisible(i: usize) void {
    const m = mainRect();
    const r = itemRect(i);
    const top = m.y + if (details) @as(i32, header_h) else 0;
    if (r.y < top) scroll -= top - r.y;
    if (r.bottom() > m.bottom()) scroll += r.bottom() - m.bottom();
    clampScroll();
}

fn itemAt(x: i32, y: i32) ?usize {
    const m = mainRect();
    if (!m.contains(x, y) or (details and y < m.y + header_h)) return null;
    for (0..count) |i| {
        if (itemRect(i).contains(x, y)) return i;
    }
    return null;
}

const Place = struct { label: []const u8, path: []const u8, icon: []const u8 };
const places = [_]Place{
    .{ .label = "Home", .path = "/root", .icon = "user-home" },
    .{ .label = "Computer", .path = "/", .icon = "computer" },
    .{ .label = "Programs", .path = "/bin", .icon = "application-x-executable" },
    .{ .label = "Pictures", .path = "/usr/share/backgrounds", .icon = "folder-pictures" },
    .{ .label = "Temporary", .path = "/tmp", .icon = "folder" },
};

fn placeRect(i: usize) Rect {
    return .{ .x = 8, .y = toolbar_h + 36 + @as(i32, @intCast(i)) * 32, .w = side_w - 16, .h = 30 };
}

fn buttonRect(b: Button) Rect {
    return switch (b) {
        .back => .{ .x = 8, .y = 8, .w = 34, .h = 30 },
        .forward => .{ .x = 44, .y = 8, .w = 34, .h = 30 },
        .up => .{ .x = 84, .y = 8, .w = 34, .h = 30 },
        .new_folder => .{ .x = @as(i32, @intCast(win.canvas.w)) - 118, .y = 8, .w = 34, .h = 30 },
        .icons => .{ .x = @as(i32, @intCast(win.canvas.w)) - 78, .y = 8, .w = 34, .h = 30 },
        .list => .{ .x = @as(i32, @intCast(win.canvas.w)) - 42, .y = 8, .w = 34, .h = 30 },
    };
}

fn buttonIcon(b: Button) []const u8 {
    return switch (b) {
        .back => "go-previous",
        .forward => "go-next",
        .up => "go-up",
        .icons => "view-list-icons",
        .list => "view-list-details",
        .new_folder => "folder-new",
    };
}

fn addressRect() Rect {
    return .{ .x = 128, .y = 8, .w = @as(i32, @intCast(win.canvas.w)) - 128 - 128, .h = 30 };
}

/// The path's parts as buttons ("root" › "projects" ›): where each ends in the path, and its rect.
var crumb_ends: [32]usize = undefined;
var crumb_rects: [32]Rect = undefined;
var crumb_count: usize = 0;

// ---- drawing ----

fn vgradient(c: *Canvas, r: Rect, top: u32, bottom: u32) void {
    var y: i32 = 0;
    while (y < r.h) : (y += 1) {
        c.rect(.{ .x = r.x, .y = r.y + y, .w = r.w, .h = 1 }, gui.mix(bottom, top, @intCast(@as(u32, @intCast(y)) * 255 / @as(u32, @intCast(@max(r.h, 1))))));
    }
}

fn glossyButton(c: *Canvas, r: Rect, lit: bool, enabled: bool) void {
    if (!enabled) return;
    if (lit) {
        c.roundRect(r, 6, 0x4a5d7c);
        c.roundRect(.{ .x = r.x + 1, .y = r.y + 1, .w = r.w - 2, .h = @divTrunc(r.h, 2) }, 5, 0x5a6f92);
    }
}

fn draw() void {
    const c = &win.canvas;
    c.clip = c.bounds();
    c.fill(bg);
    const w: i32 = @intCast(c.w);
    const h: i32 = @intCast(c.h);

    // The toolbar: glassy dark, after Vista's.
    vgradient(c, .{ .x = 0, .y = 0, .w = w, .h = toolbar_h }, 0x3a4352, 0x262c36);
    c.rect(.{ .x = 0, .y = toolbar_h - 1, .w = w, .h = 1 }, 0x101317);
    for ([_]Button{ .back, .forward, .up, .new_folder, .icons, .list }) |b| {
        const r = buttonRect(b);
        const enabled = switch (b) {
            .back => back.n > 0,
            .forward => forward.n > 0,
            .up => path_len > 1,
            else => true,
        };
        const on = (b == .icons and !details) or (b == .list and details);
        glossyButton(c, r, enabled and (hover_button == b or on), true);
        _ = icon.draw(c, buttonIcon(b), r.x + 6, r.y + 4, 22);
        // Nowhere to go: the button fades into the toolbar.
        if (!enabled) {
            var yy: i32 = r.y;
            while (yy < r.bottom()) : (yy += 1) {
                var xx: i32 = r.x;
                while (xx < r.right()) : (xx += 1) c.blend(xx, yy, 0x2c333e, 170);
            }
        }
    }

    // The address bar: the path's parts as buttons.
    const a = addressRect();
    c.roundRect(a, 6, 0x15181d);
    c.roundRectOutline(a, 6, 1, 0x4a5466);
    var x: f32 = @floatFromInt(a.x + 10);
    crumb_count = 0;
    const p = path();
    {
        // "Computer" for "/", then each part.
        crumb_ends[0] = 1;
        const label0 = "Computer";
        const w0 = gui.measure(font, 14, label0);
        crumb_rects[0] = .{ .x = @intFromFloat(x - 4), .y = a.y + 3, .w = @intFromFloat(w0 + 8), .h = a.h - 6 };
        if (hover_crumb == 0) c.roundRect(crumb_rects[0], 4, hover_fill);
        _ = c.text(font, 14, x, a.y + 20, label0, text);
        x += w0 + 8;
        crumb_count = 1;
        var at: usize = 1;
        while (at < p.len and crumb_count < crumb_ends.len) {
            const end = std.mem.indexOfScalarPos(u8, p, at, '/') orelse p.len;
            const part = p[at..end];
            _ = c.text(font, 13, x, a.y + 20, "›", dim);
            x += 14;
            const pw = gui.measure(font, 14, part);
            if (x + pw > @as(f32, @floatFromInt(a.right() - 8))) break;
            crumb_ends[crumb_count] = end;
            crumb_rects[crumb_count] = .{ .x = @intFromFloat(x - 4), .y = a.y + 3, .w = @intFromFloat(pw + 8), .h = a.h - 6 };
            if (hover_crumb == crumb_count) c.roundRect(crumb_rects[crumb_count], 4, hover_fill);
            _ = c.text(font, 14, x, a.y + 20, part, text);
            x += pw + 8;
            crumb_count += 1;
            at = end + 1;
        }
    }

    // The places.
    c.rect(.{ .x = 0, .y = toolbar_h, .w = side_w, .h = h - toolbar_h - status_h }, pane);
    c.rect(.{ .x = side_w, .y = toolbar_h, .w = 1, .h = h - toolbar_h - status_h }, line);
    _ = c.text(font, 12, 16, toolbar_h + 24, "Places", dim);
    for (places, 0..) |pl, i| {
        const r = placeRect(i);
        const here = std.mem.eql(u8, pl.path, p);
        if (here) {
            c.roundRect(r, 5, select_fill);
        } else if (hover_place == i) {
            c.roundRect(r, 5, hover_fill);
        }
        _ = icon.draw(c, pl.icon, r.x + 6, r.y + 3, 22);
        _ = c.text(font, 14, @floatFromInt(r.x + 36), r.y + 20, pl.label, text);
    }

    // The folder.
    const m = mainRect();
    c.clip = m;
    if (details) {
        c.rect(.{ .x = m.x, .y = m.y, .w = m.w, .h = header_h }, 0x262b34);
        c.rect(.{ .x = m.x, .y = m.y + header_h - 1, .w = m.w, .h = 1 }, line);
        const cols = detailColumns(m);
        _ = c.text(font, 12, @floatFromInt(cols[0]), m.y + 17, "Name", dim);
        _ = c.text(font, 12, @floatFromInt(cols[1]), m.y + 17, "Size", dim);
        _ = c.text(font, 12, @floatFromInt(cols[2]), m.y + 17, "Type", dim);
        _ = c.text(font, 12, @floatFromInt(cols[3]), m.y + 17, "Modified", dim);
        c.clip = .{ .x = m.x, .y = m.y + header_h, .w = m.w, .h = m.h - header_h };
    }
    for (0..count) |i| {
        const r = itemRect(i);
        if (r.bottom() < c.clip.y or r.y > c.clip.bottom()) continue;
        drawItem(c, i, r);
    }
    if (count == 0) _ = c.text(font, 14, @floatFromInt(m.x + 20), m.y + 40, "This folder is empty.", dim);
    c.clip = c.bounds();

    // The status line.
    vgradient(c, .{ .x = 0, .y = h - status_h, .w = w, .h = status_h }, 0x2a303a, 0x22272f);
    c.rect(.{ .x = 0, .y = h - status_h, .w = w, .h = 1 }, 0x101317);
    var sb: [200]u8 = undefined;
    const st = if (status_override) status_buf[0..status_len] else statusText(&sb);
    _ = c.text(font, 13, 12, h - 8, st, dim);

    drawOverlay(c);
    win.present();
}

fn detailColumns(m: Rect) [4]i32 {
    const right = m.right();
    return .{ m.x + 36, right - 380, right - 290, right - 150 };
}

fn drawItem(c: *Canvas, i: usize, r: Rect) void {
    const e = entries[i];
    if (selected[i]) {
        c.roundRect(r, 5, select_fill);
        c.roundRectOutline(r, 5, 1, select_edge);
    } else if (hover == i) {
        c.roundRect(r, 5, hover_fill);
    }
    var nb: [300]u8 = undefined;
    const label = if (renaming == i) rename_buf[0..rename_len] else e.name;
    if (details) {
        _ = icon.draw(c, iconName(e.kind, e.name), r.x + 6, r.y + 4, 16);
        const cols = detailColumns(mainRect());
        _ = c.text(font, 14, @floatFromInt(cols[0]), r.y + 17, fit(&nb, label, 14, @floatFromInt(cols[1] - cols[0] - 12)), text);
        var sz: [32]u8 = undefined;
        if (e.kind != .folder) _ = c.text(font, 13, @floatFromInt(cols[1]), r.y + 17, sizeText(&sz, e.size), dim);
        _ = c.text(font, 13, @floatFromInt(cols[2]), r.y + 17, typeName(e.kind), dim);
        var tb: [32]u8 = undefined;
        _ = c.text(font, 13, @floatFromInt(cols[3]), r.y + 17, dateText(&tb, e.mtime), dim);
        if (renaming == i) drawRenameBox(c, .{ .x = cols[0] - 4, .y = r.y + 1, .w = cols[1] - cols[0] - 4, .h = r.h - 2 }, 14);
    } else {
        _ = icon.draw(c, iconName(e.kind, e.name), r.x + @divTrunc(r.w - 48, 2), r.y + 6, 48);
        const shown = fit(&nb, label, 13, @floatFromInt(r.w - 8));
        const tw = gui.measure(font, 13, shown);
        _ = c.text(font, 13, @as(f32, @floatFromInt(r.x)) + (@as(f32, @floatFromInt(r.w)) - tw) / 2, r.y + 72, shown, text);
        if (renaming == i) drawRenameBox(c, .{ .x = r.x - 2, .y = r.y + 56, .w = r.w + 4, .h = 24 }, 13);
    }
}

fn drawRenameBox(c: *Canvas, r: Rect, size: f32) void {
    c.rect(r, 0x0f1216);
    c.roundRectOutline(r, 3, 1, select_edge);
    const end = c.text(font, size, @floatFromInt(r.x + 4), r.y + @divTrunc(r.h, 2) + 5, rename_buf[0..rename_len], text);
    c.rect(.{ .x = @intFromFloat(end + 1), .y = r.y + 4, .w = 1, .h = r.h - 8 }, text);
}

fn drawOverlay(c: *Canvas) void {
    switch (overlay) {
        .none => {},
        .menu => |m| {
            const r = menuRect();
            c.roundRect(.{ .x = r.x + 3, .y = r.y + 4, .w = r.w, .h = r.h }, 8, 0x0b0d10);
            c.roundRect(r, 8, 0x2a303b);
            c.roundRectOutline(r, 8, 1, 0x505a6c);
            for (menuItems(m.on_item), 0..) |item, k| {
                const row = Rect{ .x = r.x + 4, .y = r.y + 5 + @as(i32, @intCast(k)) * menu_row, .w = r.w - 8, .h = menu_row - 2 };
                if (menu_hover == k) c.roundRect(row, 5, select_fill);
                _ = icon.draw(c, menuIcon(item), row.x + 6, row.y + 5, 16);
                _ = c.text(font, 14, @floatFromInt(row.x + 32), row.y + 18, menuLabel(item), text);
            }
        },
        .confirm_delete => {
            const r = dialogRect(380, 150);
            dialogFrame(c, r, "Delete");
            var b: [200]u8 = undefined;
            const n = selectionCount();
            const msg = if (n == 1)
                std.fmt.bufPrint(&b, "Delete \"{s}\"? It can't be brought back.", .{entries[firstSelected().?].name}) catch ""
            else
                std.fmt.bufPrint(&b, "Delete these {d} items? They can't be brought back.", .{n}) catch "";
            var fb: [200]u8 = undefined;
            _ = c.text(font, 14, @floatFromInt(r.x + 20), r.y + 66, fit(&fb, msg, 14, @floatFromInt(r.w - 40)), text);
            dialogButton(c, deleteButton(r), "Delete", true);
            dialogButton(c, cancelButton(r), "Cancel", false);
        },
        .properties => |i| {
            const r = dialogRect(400, 230);
            dialogFrame(c, r, "Properties");
            const e = entries[i];
            _ = icon.draw(c, iconName(e.kind, e.name), r.x + 20, r.y + 50, 48);
            var nb: [300]u8 = undefined;
            _ = c.text(font, 16, @floatFromInt(r.x + 80), r.y + 70, fit(&nb, e.name, 16, @floatFromInt(r.w - 100)), text);
            var lines: [4][2][]const u8 = undefined;
            var sb: [64]u8 = undefined;
            var tb: [32]u8 = undefined;
            var mb: [16]u8 = undefined;
            var full: [1300]u8 = undefined;
            lines[0] = .{ "Type", typeName(e.kind) };
            lines[1] = .{ "Size", if (e.kind == .folder) folderSize(&sb, join(&full, e.name)) else sizeText(&sb, e.size) };
            lines[2] = .{ "Modified", dateText(&tb, e.mtime) };
            lines[3] = .{ "Permissions", modeText(&mb, e.mode) };
            for (lines, 0..) |l, k| {
                const y = r.y + 112 + @as(i32, @intCast(k)) * 22;
                _ = c.text(font, 13, @floatFromInt(r.x + 20), y, l[0], dim);
                _ = c.text(font, 13, @floatFromInt(r.x + 120), y, l[1], text);
            }
            dialogButton(c, cancelButton(r), "Close", false);
        },
    }
}

fn dialogRect(dw: i32, dh: i32) Rect {
    return .{ .x = @divTrunc(@as(i32, @intCast(win.canvas.w)) - dw, 2), .y = @divTrunc(@as(i32, @intCast(win.canvas.h)) - dh, 2), .w = dw, .h = dh };
}

fn dialogFrame(c: *Canvas, r: Rect, title: []const u8) void {
    // The rest of the window dims behind it.
    var y: i32 = 0;
    while (y < c.h) : (y += 1) {
        var xx: i32 = 0;
        while (xx < c.w) : (xx += 1) c.blend(xx, y, 0x000000, 90);
    }
    c.roundRect(.{ .x = r.x + 4, .y = r.y + 6, .w = r.w, .h = r.h }, 10, 0x08090c);
    c.roundRect(r, 10, 0x262b34);
    vgradient(c, .{ .x = r.x + 1, .y = r.y + 1, .w = r.w - 2, .h = 34 }, 0x4d6488, 0x253247);
    c.roundRectOutline(r, 10, 1, 0x56627a);
    _ = c.text(font, 14, @floatFromInt(r.x + 14), r.y + 23, title, 0xffffff);
}

fn deleteButton(r: Rect) Rect {
    return .{ .x = r.right() - 200, .y = r.bottom() - 46, .w = 90, .h = 32 };
}

fn cancelButton(r: Rect) Rect {
    return .{ .x = r.right() - 104, .y = r.bottom() - 46, .w = 90, .h = 32 };
}

fn dialogButton(c: *Canvas, r: Rect, label: []const u8, strong: bool) void {
    c.roundRect(r, 6, if (strong) 0xc24a3a else 0x3b4352);
    c.roundRect(.{ .x = r.x + 1, .y = r.y + 1, .w = r.w - 2, .h = @divTrunc(r.h, 2) }, 5, if (strong) 0xd65c4b else 0x485162);
    const tw = gui.measure(font, 14, label);
    _ = c.text(font, 14, @as(f32, @floatFromInt(r.x)) + (@as(f32, @floatFromInt(r.w)) - tw) / 2, r.y + 21, label, 0xffffff);
}

fn statusText(buf: []u8) []const u8 {
    const n = selectionCount();
    if (n == 1) {
        const e = entries[firstSelected().?];
        var sz: [32]u8 = undefined;
        if (e.kind == .folder) return std.fmt.bufPrint(buf, "{s} — Folder", .{e.name}) catch "";
        return std.fmt.bufPrint(buf, "{s} — {s} — {s}", .{ e.name, typeName(e.kind), sizeText(&sz, e.size) }) catch "";
    }
    if (n > 1) return std.fmt.bufPrint(buf, "{d} items selected", .{n}) catch "";
    return std.fmt.bufPrint(buf, "{d} item{s}", .{ count, if (count == 1) "" else "s" }) catch "";
}

fn sizeText(buf: []u8, size: u64) []const u8 {
    if (size < 1024) return std.fmt.bufPrint(buf, "{d} bytes", .{size}) catch "";
    if (size < 1024 * 1024) return std.fmt.bufPrint(buf, "{d}.{d} KB", .{ size / 1024, size % 1024 * 10 / 1024 }) catch "";
    if (size < 1024 * 1024 * 1024) return std.fmt.bufPrint(buf, "{d}.{d} MB", .{ size >> 20, (size & 0xfffff) * 10 >> 20 }) catch "";
    return std.fmt.bufPrint(buf, "{d}.{d} GB", .{ size >> 30, (size & 0x3fffffff) * 10 >> 30 }) catch "";
}

fn folderSize(buf: []u8, p: [*:0]const u8) []const u8 {
    var kids = Children.open(p) orelse return "?";
    defer kids.close();
    var n: usize = 0;
    while (kids.next()) |_| n += 1;
    return std.fmt.bufPrint(buf, "{d} item{s}", .{ n, if (n == 1) "" else "s" }) catch "";
}

fn modeText(buf: []u8, mode: u32) []const u8 {
    const chars = "rwxrwxrwx";
    for (0..9) |k| buf[k] = if (mode & (@as(u32, 0o400) >> @intCast(k)) != 0) chars[k] else '-';
    return buf[0..9];
}

fn dateText(buf: []u8, t: i64) []const u8 {
    if (t <= 0) return "";
    const s: u64 = @intCast(t);
    const days = s / 86400;
    const z = days + 719468;
    const era = z / 146097;
    const doe = z - era * 146097;
    const yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    const doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    const mp = (5 * doy + 2) / 153;
    const day = doy - (153 * mp + 2) / 5 + 1;
    const month = if (mp < 10) mp + 3 else mp - 9;
    const year = yoe + era * 400 + @intFromBool(month <= 2);
    return std.fmt.bufPrint(buf, "{d}-{d:0>2}-{d:0>2} {d:0>2}:{d:0>2}", .{ year, month, day, s % 86400 / 3600, s % 3600 / 60 }) catch "";
}

/// Text that fits in `width` pixels: cut short with "…" if it doesn't.
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

// ---- events ----

var hover_place: ?usize = null;
var hover_crumb: ?usize = null;
var last_click: struct { item: usize = 0, at: i64 = 0 } = .{};

fn nowMs() i64 {
    var ts: extern struct { sec: i64, nsec: i64 } = .{ .sec = 0, .nsec = 0 };
    _ = sys.syscall2(sys.nr.clock_gettime, 1, @intFromPtr(&ts));
    return ts.sec * 1000 + @divTrunc(ts.nsec, 1_000_000);
}

fn motion(x: i32, y: i32) bool {
    var changed = false;
    if (overlay == .menu) {
        const r = menuRect();
        var h: ?usize = null;
        for (0..menuItems(overlay.menu.on_item).len) |k| {
            const row = Rect{ .x = r.x + 4, .y = r.y + 5 + @as(i32, @intCast(k)) * menu_row, .w = r.w - 8, .h = menu_row - 2 };
            if (row.contains(x, y)) h = k;
        }
        if (h != menu_hover) {
            menu_hover = h;
            changed = true;
        }
        return changed;
    }
    if (overlay != .none) return false;
    const it = itemAt(x, y);
    if (it != hover) {
        hover = it;
        changed = true;
    }
    var hb: ?Button = null;
    for ([_]Button{ .back, .forward, .up, .new_folder, .icons, .list }) |b| {
        if (buttonRect(b).contains(x, y)) hb = b;
    }
    if (hb != hover_button) {
        hover_button = hb;
        changed = true;
    }
    var hp: ?usize = null;
    for (0..places.len) |i| {
        if (placeRect(i).contains(x, y)) hp = i;
    }
    if (hp != hover_place) {
        hover_place = hp;
        changed = true;
    }
    var hc: ?usize = null;
    for (0..crumb_count) |i| {
        if (crumb_rects[i].contains(x, y)) hc = i;
    }
    if (hc != hover_crumb) {
        hover_crumb = hc;
        changed = true;
    }
    return changed;
}

fn press(x: i32, y: i32, button: u8, ctrl: bool) void {
    switch (overlay) {
        .menu => {
            if (button == 1 and menuRect().contains(x, y)) {
                if (menu_hover) |k| doMenu(menuItems(overlay.menu.on_item)[k]);
            } else {
                overlay = .none;
            }
            return;
        },
        .confirm_delete => {
            const r = dialogRect(380, 150);
            if (deleteButton(r).contains(x, y)) {
                overlay = .none;
                deleteSelection();
            } else if (cancelButton(r).contains(x, y)) {
                overlay = .none;
            }
            return;
        },
        .properties => {
            if (cancelButton(dialogRect(400, 230)).contains(x, y)) overlay = .none;
            return;
        },
        .none => {},
    }
    if (renaming != null) finishRename();
    if (button == 1) {
        for ([_]Button{ .back, .forward, .up, .new_folder, .icons, .list }) |b| {
            if (!buttonRect(b).contains(x, y)) continue;
            switch (b) {
                .back => goBack(),
                .forward => goForward(),
                .up => goUp(),
                .new_folder => newFolder(),
                .icons => details = false,
                .list => details = true,
            }
            clampScroll();
            return;
        }
        for (0..crumb_count) |i| {
            if (crumb_rects[i].contains(x, y)) {
                var tmp: [1024]u8 = undefined;
                const p = path()[0..crumb_ends[i]];
                @memcpy(tmp[0..p.len], p);
                go(tmp[0..p.len]);
                return;
            }
        }
        for (places, 0..) |pl, i| {
            if (placeRect(i).contains(x, y)) {
                go(pl.path);
                return;
            }
        }
    }
    const it = itemAt(x, y);
    if (button == 3) {
        if (it) |i| {
            if (!selected[i]) selectOnly(i);
        } else {
            @memset(selected[0..count], false);
        }
        overlay = .{ .menu = .{ .x = x, .y = y, .on_item = it != null } };
        menu_hover = null;
        return;
    }
    if (button != 1 or !mainRect().contains(x, y)) return;
    const i = it orelse {
        @memset(selected[0..count], false);
        focus = null;
        return;
    };
    const now = nowMs();
    if (last_click.item == i and now - last_click.at < 450 and selected[i]) {
        last_click.at = 0;
        open(i);
        return;
    }
    last_click = .{ .item = i, .at = now };
    if (ctrl) {
        selected[i] = !selected[i];
        focus = i;
    } else {
        selectOnly(i);
    }
}

fn key(k: gui.Key) void {
    if (renaming != null) {
        switch (k.code) {
            13 => finishRename(),
            27 => renaming = null,
            8 => if (rename_len > 0) {
                rename_len -= 1;
                while (rename_len > 0 and rename_buf[rename_len] & 0xc0 == 0x80) rename_len -= 1;
            },
            else => if (k.code >= 32 and k.code < 0x100 and rename_len < rename_buf.len - 4) {
                const n = std.unicode.utf8Encode(@intCast(k.code), rename_buf[rename_len..]) catch 0;
                rename_len += n;
            },
        }
        return;
    }
    if (overlay != .none) {
        if (k.code == 27) overlay = .none;
        if (k.code == 13 and overlay == .confirm_delete) {
            overlay = .none;
            deleteSelection();
        }
        return;
    }
    const step: i32 = if (details) 1 else columns();
    switch (k.code) {
        13 => if (focus) |i| open(i),
        8 => goUp(),
        127 => if (firstSelected() != null) {
            overlay = .confirm_delete;
        },
        gui.Key.up => moveFocus(-step),
        gui.Key.down => moveFocus(step),
        gui.Key.left => moveFocus(-1),
        gui.Key.right => moveFocus(1),
        gui.Key.home => if (count > 0) selectOnly(0),
        gui.Key.end => if (count > 0) selectOnly(count - 1),
        gui.Key.f5 => load(),
        gui.Key.f2 => if (focus) |i| startRename(i),
        else => if (k.ctrl) switch (k.code | 0x20) {
            'c' => copySelection(false),
            'x' => copySelection(true),
            'v' => paste(),
            'a' => {
                @memset(selected[0..count], true);
            },
            'h' => {
                show_hidden = !show_hidden;
                load();
            },
            'n' => newFolder(),
            else => {},
        },
    }
}

fn moveFocus(delta: i32) void {
    if (count == 0) return;
    const from: i32 = if (focus) |f| @intCast(f) else -delta;
    const to = std.math.clamp(from + delta, 0, @as(i32, @intCast(count)) - 1);
    selectOnly(@intCast(to));
}

var titled: [1024]u8 = undefined;
var titled_len: usize = 0;

/// The window's title: the folder's name.
fn setTitle() void {
    var title_buf: [300]u8 = undefined;
    const base = path()[(std.mem.lastIndexOfScalar(u8, path(), '/') orelse 0) + 1 ..];
    const title = std.fmt.bufPrintSentinel(&title_buf, "{s} — Files", .{if (base.len == 0) "Computer" else fitName(base)}, 0) catch "Files";
    win.setTitle(title);
    titled_len = path_len;
    @memcpy(titled[0..path_len], path());
}

fn fitName(s: []const u8) []const u8 {
    return s[0..@min(s.len, 200)];
}

pub fn main(args: rt.Args) u8 {
    const start = args.get(1) orelse blk: {
        const r = sys.check(sys.syscall2(sys.nr.getcwd, @intFromPtr(&path_buf), path_buf.len));
        break :blk switch (r) {
            .ok => |n| path_buf[0 .. n - 1],
            .err => "/root",
        };
    };
    var start_buf: [1024]u8 = undefined;
    @memcpy(start_buf[0..start.len], start);
    setPath(start_buf[0..start.len]);
    font = gui.Font.load(gui.theme.font_path) orelse {
        rt.err.write("files: no font in /usr/share/fonts\n");
        return 1;
    };
    win = gui.Window.open("Files", 900, 560) orelse {
        rt.err.write("files: can't open a window (no /dev/window?)\n");
        return 1;
    };
    load();
    setTitle();
    draw();
    while (true) {
        const e = win.next(1000) orelse break;
        var redraw = true;
        switch (e) {
            .motion => |m| redraw = motion(m.x, m.y),
            .leave => {
                hover = null;
                hover_button = null;
                hover_place = null;
                hover_crumb = null;
            },
            .press => |p| press(p.x, p.y, p.button, false),
            .key => |k| key(k),
            .scroll => |s| {
                scroll += s.steps * 48;
                clampScroll();
            },
            .resize => clampScroll(),
            .tick => {
                reap();
                redraw = false;
            },
            else => redraw = false,
        }
        if (redraw and !win.pending()) draw();
        if (titled_len != path_len or !std.mem.eql(u8, titled[0..titled_len], path())) setTitle();
    }
    win.close();
    return 0;
}
