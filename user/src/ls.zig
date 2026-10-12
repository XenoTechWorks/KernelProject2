//! ls: list directory contents. `-a` shows hidden files, `-l` the long format, `-1` one per line.
//! On a terminal, names are colored (folders blue, programs green, links cyan) and put in columns.

const std = @import("std");
const rt = @import("lib/rt.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

var show_all = false;
var long = false;
var one_per_line = false;
var on_tty = false;
var term_width: usize = 80;
var status: u8 = 0;

const Entry = struct { name: []const u8, st: sys.Stat };
var names_pool: [64 * 1024]u8 = undefined;
var entries: [2048]Entry = undefined;

fn color(st: sys.Stat) []const u8 {
    if (!on_tty) return "";
    return switch (st.mode & sys.S_IFMT) {
        sys.S_IFDIR => "\x1b[1;34m",
        sys.S_IFLNK => "\x1b[1;36m",
        sys.S_IFCHR => "\x1b[1;33m",
        sys.S_IFIFO => "\x1b[33m",
        else => if (st.mode & 0o111 != 0) "\x1b[1;32m" else "",
    };
}

fn reset() []const u8 {
    return if (on_tty) "\x1b[0m" else "";
}

fn modeString(mode: u32, out: *[10]u8) void {
    out[0] = switch (mode & sys.S_IFMT) {
        sys.S_IFDIR => 'd',
        sys.S_IFLNK => 'l',
        sys.S_IFCHR => 'c',
        sys.S_IFIFO => 'p',
        sys.S_IFSOCK => 's',
        else => '-',
    };
    const chars = "rwxrwxrwx";
    for (0..9) |i| out[1 + i] = if (mode & (@as(u32, 0o400) >> @intCast(i)) != 0) chars[i] else '-';
}

/// Year-month-day hour:minute from seconds since 1970.
fn formatTime(t: i64, buf: []u8) []const u8 {
    const secs: u64 = @intCast(@max(t, 0));
    const days = secs / 86400;
    const rem = secs % 86400;
    // Civil date from days (Howard Hinnant's algorithm).
    const z = days + 719468;
    const era = z / 146097;
    const doe = z - era * 146097;
    const yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    const doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    const mp = (5 * doy + 2) / 153;
    const d = doy - (153 * mp + 2) / 5 + 1;
    const m = if (mp < 10) mp + 3 else mp - 9;
    const y = yoe + era * 400 + @as(u64, if (m <= 2) 1 else 0);
    return std.fmt.bufPrint(buf, "{d}-{d:0>2}-{d:0>2} {d:0>2}:{d:0>2}", .{ y, m, d, rem / 3600, rem % 3600 / 60 }) catch "";
}

fn showLong(dir: []const u8, e: Entry) void {
    var mode: [10]u8 = undefined;
    modeString(e.st.mode, &mode);
    var tbuf: [32]u8 = undefined;
    rt.out.print("{s} {d:>2} root root {d:>8} {s} {s}{s}{s}", .{ &mode, e.st.nlink, @as(u64, @intCast(@max(e.st.size, 0))), formatTime(e.st.mtime, &tbuf), color(e.st), e.name, reset() });
    if (e.st.mode & sys.S_IFMT == sys.S_IFLNK) {
        var pbuf: [1024]u8 = undefined;
        var target: [512]u8 = undefined;
        if (joinPath(&pbuf, dir, e.name)) |p| {
            const r = sys.check(sys.syscall3(sys.nr.readlink, @intFromPtr(p), @intFromPtr(&target), target.len));
            if (r == .ok) rt.out.print(" -> {s}", .{target[0..r.ok]});
        }
    }
    rt.out.write("\n");
}

fn joinPath(buf: []u8, dir: []const u8, name: []const u8) ?[*:0]const u8 {
    const s = std.fmt.bufPrint(buf, "{s}/{s}\x00", .{ dir, name }) catch return null;
    return @ptrCast(s.ptr);
}

fn lessThan(_: void, a: Entry, b: Entry) bool {
    return std.mem.lessThan(u8, a.name, b.name);
}

fn listDir(path: []const u8, heading: bool) void {
    var pbuf: [1024]u8 = undefined;
    const cpath = rt.cstr(&pbuf, path) orelse return;
    var st: sys.Stat = undefined;
    if (sys.lstat(cpath, &st) == .err or st.mode & sys.S_IFMT != sys.S_IFDIR) {
        switch (sys.stat(cpath, &st)) {
            .err => |e| {
                rt.complain("ls", path, e);
                status = 2;
                return;
            },
            .ok => {},
        }
        if (st.mode & sys.S_IFMT != sys.S_IFDIR) {
            const e = Entry{ .name = path, .st = st };
            if (long) showLong(".", e) else rt.out.print("{s}{s}{s}\n", .{ color(st), path, reset() });
            return;
        }
    }
    const fd = switch (sys.open(cpath, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
        .ok => |fd| @as(i32, @intCast(fd)),
        .err => |e| {
            rt.complain("ls", path, e);
            status = 2;
            return;
        },
    };
    defer sys.close(fd);

    var count: usize = 0;
    var pool_len: usize = 0;
    var buf: [8192]u8 align(8) = undefined;
    while (true) {
        const r = sys.check(sys.syscall3(sys.nr.getdents64, @intCast(fd), @intFromPtr(&buf), buf.len));
        const n = switch (r) {
            .ok => |n| n,
            .err => break,
        };
        if (n == 0) break;
        var at: usize = 0;
        while (at < n) {
            const reclen = std.mem.readInt(u16, buf[at + 16 ..][0..2], .little);
            const name = std.mem.span(@as([*:0]const u8, @ptrCast(&buf[at + 19])));
            at += reclen;
            if (!show_all and name.len > 0 and name[0] == '.') continue;
            if (count == entries.len or pool_len + name.len > names_pool.len) continue;
            @memcpy(names_pool[pool_len..][0..name.len], name);
            const stored = names_pool[pool_len .. pool_len + name.len];
            pool_len += name.len;
            var est: sys.Stat = std.mem.zeroes(sys.Stat);
            var full: [1024]u8 = undefined;
            if (joinPath(&full, path, name)) |p| _ = sys.lstat(p, &est);
            entries[count] = .{ .name = stored, .st = est };
            count += 1;
        }
    }
    const list = entries[0..count];
    std.mem.sort(Entry, list, {}, lessThan);

    if (heading) rt.out.print("{s}:\n", .{path});
    if (long) {
        for (list) |e| showLong(path, e);
    } else if (one_per_line or !on_tty) {
        for (list) |e| rt.out.print("{s}{s}{s}\n", .{ color(e.st), e.name, reset() });
    } else {
        // Columns, filled top to bottom like GNU ls.
        var widest: usize = 1;
        for (list) |e| widest = @max(widest, e.name.len);
        const col_width = widest + 2;
        const cols = @max(1, term_width / col_width);
        const rows = (list.len + cols - 1) / cols;
        for (0..rows) |row| {
            for (0..cols) |col| {
                const idx = col * rows + row;
                if (idx >= list.len) break;
                const e = list[idx];
                rt.out.print("{s}{s}{s}", .{ color(e.st), e.name, reset() });
                if (col + 1 < cols and (col + 1) * rows + row < list.len) {
                    for (0..col_width - e.name.len) |_| rt.out.write(" ");
                }
            }
            rt.out.write("\n");
        }
    }
}

pub fn main(args: rt.Args) u8 {
    var termios: [60]u8 = undefined;
    on_tty = sys.check(sys.syscall3(sys.nr.ioctl, 1, 0x5401, @intFromPtr(&termios))) == .ok;
    if (on_tty) {
        var ws: [4]u16 = undefined;
        if (sys.check(sys.syscall3(sys.nr.ioctl, 1, 0x5413, @intFromPtr(&ws))) == .ok and ws[1] > 0) term_width = ws[1];
    }

    var paths: [64][]const u8 = undefined;
    var path_count: usize = 0;
    var i: usize = 1;
    while (args.get(i)) |a| : (i += 1) {
        if (a.len > 1 and a[0] == '-') {
            for (a[1..]) |c| switch (c) {
                'a' => show_all = true,
                'l' => long = true,
                '1' => one_per_line = true,
                else => {
                    rt.err.print("ls: unknown option -{c}\n", .{c});
                    return 2;
                },
            };
        } else if (path_count < paths.len) {
            paths[path_count] = a;
            path_count += 1;
        }
    }
    if (path_count == 0) {
        listDir(".", false);
    } else {
        for (paths[0..path_count], 0..) |p, n| {
            if (n > 0) rt.out.write("\n");
            listDir(p, path_count > 1);
        }
    }
    return status;
}
