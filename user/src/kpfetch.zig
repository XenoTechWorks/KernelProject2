//! kpfetch: KernelProject2's own system summary, in the style of fastfetch and neofetch.
//!
//! Everything comes from the kernel the usual Linux way: uname, sysinfo, statfs, /proc and /etc.

const std = @import("std");
const rt = @import("lib/rt.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const reset = "\x1b[0m";
const accent = "\x1b[1;36m"; // bright cyan
const accent2 = "\x1b[1;34m"; // bright blue
const label = "\x1b[1;36m";

// "K2" in the ANSI Shadow lettering, K in cyan and 2 in blue.
const logo = [_][2][]const u8{
    .{ " ██╗  ██╗", "██████╗ " },
    .{ " ██║ ██╔╝", "╚════██╗" },
    .{ " █████╔╝ ", " █████╔╝" },
    .{ " ██╔═██╗ ", "██╔═══╝ " },
    .{ " ██║  ██╗", "███████╗" },
    .{ " ╚═╝  ╚═╝", "╚══════╝" },
};
const logo_width = 19;

var file_buf: [16384]u8 = undefined;

/// The whole of a (small) file, or null.
fn readFile(path: [*:0]const u8) ?[]const u8 {
    const fd = switch (sys.open(path, sys.O_RDONLY, 0)) {
        .ok => |fd| @as(i32, @intCast(fd)),
        .err => return null,
    };
    defer sys.close(fd);
    var len: usize = 0;
    while (len < file_buf.len) {
        switch (sys.read(fd, file_buf[len..])) {
            .ok => |n| {
                if (n == 0) break;
                len += n;
            },
            .err => break,
        }
    }
    return file_buf[0..len];
}

/// The value of `key` in "key=value" or "key : value" lines, quotes removed.
fn field(text: []const u8, key: []const u8, sep: u8) ?[]const u8 {
    var it = std.mem.splitScalar(u8, text, '\n');
    while (it.next()) |line| {
        const at = std.mem.indexOfScalar(u8, line, sep) orelse continue;
        if (!std.mem.eql(u8, std.mem.trim(u8, line[0..at], " \t"), key)) continue;
        return std.mem.trim(u8, line[at + 1 ..], " \t\"");
    }
    return null;
}

const SysInfo = extern struct {
    uptime: i64,
    loads: [3]u64,
    totalram: u64,
    freeram: u64,
    sharedram: u64,
    bufferram: u64,
    totalswap: u64,
    freeswap: u64,
    procs: u16,
    pad: u16,
    pad2: u32,
    totalhigh: u64,
    freehigh: u64,
    mem_unit: u32,
    pad3: [4]u8,
};

/// Counts directory entries whose names pass `keep`.
fn countEntries(path: [*:0]const u8, comptime keep: fn ([]const u8) bool) usize {
    const fd = switch (sys.open(path, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
        .ok => |fd| @as(i32, @intCast(fd)),
        .err => return 0,
    };
    defer sys.close(fd);
    var count: usize = 0;
    var buf: [8192]u8 align(8) = undefined;
    while (true) {
        const n = switch (sys.check(sys.syscall3(sys.nr.getdents64, @intCast(fd), @intFromPtr(&buf), buf.len))) {
            .ok => |n| n,
            .err => break,
        };
        if (n == 0) break;
        var at: usize = 0;
        while (at < n) {
            const reclen = std.mem.readInt(u16, buf[at + 16 ..][0..2], .little);
            const name = std.mem.span(@as([*:0]const u8, @ptrCast(&buf[at + 19])));
            if (keep(name)) count += 1;
            at += reclen;
        }
    }
    return count;
}

fn isPid(name: []const u8) bool {
    if (name.len == 0) return false;
    for (name) |c| if (!std.ascii.isDigit(c)) return false;
    return true;
}

fn isExe(name: []const u8) bool {
    return std.ascii.endsWithIgnoreCase(name, ".exe");
}

fn mib(bytes: u64) u64 {
    return bytes / (1024 * 1024);
}

// The info lines, built up front so they can sit next to the logo.
var lines: [16][160]u8 = undefined;
var line_lens: [16]usize = undefined;
var line_count: usize = 0;

fn add(comptime fmt: []const u8, args: anytype) void {
    if (line_count == lines.len) return;
    const s = std.fmt.bufPrint(&lines[line_count], fmt, args) catch return;
    line_lens[line_count] = s.len;
    line_count += 1;
}

fn item(name: []const u8, comptime fmt: []const u8, args: anytype) void {
    var value: [128]u8 = undefined;
    const v = std.fmt.bufPrint(&value, fmt, args) catch return;
    add(label ++ "{s}" ++ reset ++ ": {s}", .{ name, v });
}

pub fn main(args: rt.Args) u8 {
    var uts: [6][65]u8 = undefined;
    _ = sys.syscall1(sys.nr.uname, @intFromPtr(&uts));
    const user = args.env("USER") orelse "root";
    const host = std.mem.sliceTo(&uts[1], 0);

    add(accent ++ "{s}" ++ reset ++ "@" ++ accent ++ "{s}" ++ reset, .{ user, host });
    var rule: [64]u8 = @splat('-');
    add("{s}", .{rule[0..@min(rule.len, user.len + 1 + host.len)]});

    const os = if (readFile("/etc/os-release")) |t| field(t, "PRETTY_NAME", '=') orelse "KernelProject2" else "KernelProject2";
    var os_copy: [64]u8 = undefined;
    const os_name = os_copy[0..@min(os.len, os_copy.len)];
    @memcpy(os_name, os[0..os_name.len]);
    item("OS", "{s} {s}", .{ os_name, std.mem.sliceTo(&uts[4], 0) });
    item("Kernel", "{s} (speaks Linux)", .{std.mem.sliceTo(&uts[2], 0)});

    var info: SysInfo = std.mem.zeroes(SysInfo);
    _ = sys.syscall1(sys.nr.sysinfo, @intFromPtr(&info));
    const up: u64 = @intCast(@max(info.uptime, 0));
    if (up >= 3600) item("Uptime", "{d}h {d}m", .{ up / 3600, up % 3600 / 60 }) else if (up >= 60) item("Uptime", "{d} min {d} s", .{ up / 60, up % 60 }) else item("Uptime", "{d} seconds", .{up});

    const shell = args.env("SHELL") orelse "/bin/kpsh";
    const shell_name = if (std.mem.lastIndexOfScalar(u8, shell, '/')) |slash| shell[slash + 1 ..] else shell;
    item("Shell", "{s}", .{shell_name});
    var ws: [4]u16 = .{ 0, 0, 0, 0 };
    if (sys.check(sys.syscall3(sys.nr.ioctl, 1, 0x5413, @intFromPtr(&ws))) == .ok and ws[1] > 0)
        item("Terminal", "{s} ({d}x{d})", .{ args.env("TERM") orelse "linux", ws[1], ws[0] });

    if (readFile("/proc/cpuinfo")) |t| {
        const model = field(t, "model name", ':') orelse "unknown";
        var copy: [96]u8 = undefined;
        const m = copy[0..@min(model.len, copy.len)];
        @memcpy(m, model[0..m.len]);
        item("CPU", "{s}", .{m});
    }
    const unit: u64 = if (info.mem_unit == 0) 1 else info.mem_unit;
    const total = info.totalram * unit;
    const used = total - info.freeram * unit;
    item("Memory", "{d} MiB / {d} MiB ({d}%)", .{ mib(used), mib(total), if (total > 0) used * 100 / total else 0 });

    // statfs("/root"): the disk, when one is mounted there.
    var fs: [15]u64 = @splat(0);
    if (sys.check(sys.syscall2(137, @intFromPtr("/root"), @intFromPtr(&fs))) == .ok and fs[2] > 0) {
        const kind = if (fs[0] == 0xEF53) "ext2" else "memory";
        item("Home (~)", "{d} MiB / {d} MiB - {s}", .{ mib((fs[2] - fs[3]) * fs[1]), mib(fs[2] * fs[1]), kind });
    }
    item("Processes", "{d}", .{countEntries("/proc", isPid)});
    const exes = countEntries("/bin", isExe);
    if (exes > 0) item("Windows apps", "{d} in /bin (run through winload)", .{exes});

    // Logo on the left, the lines on the right; then the 16 terminal colors.
    const rows = @max(logo.len, line_count) + 1;
    rt.out.write("\n");
    for (0..rows) |row| {
        if (row < logo.len) {
            rt.out.print(accent ++ "{s}" ++ accent2 ++ "{s}" ++ reset, .{ logo[row][0], logo[row][1] });
        } else {
            for (0..logo_width - 2) |_| rt.out.write(" ");
        }
        rt.out.write("   ");
        if (row < line_count) rt.out.write(lines[row][0..line_lens[row]]);
        rt.out.write("\n");
    }
    for (0..2) |half| {
        for (0..logo_width + 1) |_| rt.out.write(" ");
        for (0..8) |c| rt.out.print("\x1b[{d}m   ", .{(if (half == 0) @as(usize, 40) else 100) + c});
        rt.out.write(reset ++ "\n");
    }
    rt.out.write("\n");
    return 0;
}
