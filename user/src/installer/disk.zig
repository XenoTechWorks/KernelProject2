//! Disks for the installer: which there are (/dev/vda, /dev/vdb...), how big, which one holds
//! /root right now (never a target), and writing whole sectors at a byte offset.

const std = @import("std");
const sys = @import("../lib/sys.zig");
const file = @import("../lib/file.zig");

const BLKGETSIZE64 = 0x80081272;
const pwrite64 = 18;

pub const Disk = struct {
    fd: i32,
    size: u64,

    pub fn open(path: [*:0]const u8) ?Disk {
        const fd: i32 = switch (sys.open(path, sys.O_RDWR | sys.O_CLOEXEC, 0)) {
            .ok => |fd| @intCast(fd),
            .err => return null,
        };
        var size: u64 = 0;
        if (sys.check(sys.syscall3(sys.nr.ioctl, @bitCast(@as(isize, fd)), BLKGETSIZE64, @intFromPtr(&size))) != .ok or size == 0) {
            sys.close(fd);
            return null;
        }
        return .{ .fd = fd, .size = size };
    }

    pub fn close(d: *Disk) void {
        sys.close(d.fd);
    }

    pub fn sectors(d: Disk) u64 {
        return d.size / 512;
    }

    /// Writes `data` (a whole number of sectors) at byte `at`.
    pub fn write(d: Disk, at: u64, data: []const u8) error{Io}!void {
        std.debug.assert(at % 512 == 0 and data.len % 512 == 0);
        var done: usize = 0;
        while (done < data.len) {
            const r = sys.check(sys.syscall4(pwrite64, @bitCast(@as(isize, d.fd)), @intFromPtr(data[done..].ptr), data.len - done, at + done));
            switch (r) {
                .ok => |n| {
                    if (n == 0) return error.Io;
                    done += n;
                },
                .err => |e| if (e != sys.EINTR) return error.Io,
            }
        }
    }

    /// Writes zeros over `len` bytes from `at` (both whole sectors).
    pub fn zero(d: Disk, at: u64, len: u64) error{Io}!void {
        const zeros: [64 * 1024]u8 = @splat(0);
        var pos: u64 = 0;
        while (pos < len) {
            const n: usize = @intCast(@min(len - pos, zeros.len));
            try d.write(at + pos, zeros[0..n]);
            pos += n;
        }
    }
};

pub const Info = struct { name: [4]u8, size: u64, in_use: bool };

/// The disks there are (up to 8), and whether each holds /root right now.
pub fn list(out: []Info) usize {
    var mounted_buf: [16]u8 = undefined;
    const mounted = mountedDisk(&mounted_buf);
    var n: usize = 0;
    for (0..8) |i| {
        if (n == out.len) break;
        var path = "/dev/vda\x00".*;
        path[7] = 'a' + @as(u8, @intCast(i));
        var d = Disk.open(@ptrCast(&path)) orelse continue;
        defer d.close();
        out[n] = .{ .name = path[5..9].*, .size = d.size, .in_use = mounted != null and std.mem.eql(u8, mounted.?, path[5..8]) };
        n += 1;
    }
    return n;
}

/// The disk /root is on ("vda"), from /proc/mounts ("/dev/vda2 /root ext2 ...").
pub fn mountedDisk(buf: []u8) ?[]const u8 {
    const data = file.read("/proc/mounts") catch return null;
    defer std.heap.page_allocator.free(data);
    var lines = std.mem.splitScalar(u8, data, '\n');
    while (lines.next()) |line| {
        var words = std.mem.tokenizeScalar(u8, line, ' ');
        const dev = words.next() orelse continue;
        const at = words.next() orelse continue;
        if (!std.mem.eql(u8, at, "/root") or !std.mem.startsWith(u8, dev, "/dev/vd") or dev.len < 8) continue;
        const name = dev[5..8];
        const n = @min(name.len, buf.len);
        @memcpy(buf[0..n], name[0..n]);
        return buf[0..n];
    }
    return null;
}

/// 16 random bytes made into a version-4 GUID (for partitions and file systems).
pub fn randomId() [16]u8 {
    var id: [16]u8 = @splat(0x5a);
    switch (sys.open("/dev/urandom", sys.O_RDONLY, 0)) {
        .ok => |fd| {
            _ = sys.read(@intCast(fd), &id);
            sys.close(@intCast(fd));
        },
        .err => {},
    }
    id[7] = (id[7] & 0x0f) | 0x40; // version 4 (the high nibble of the third field, little-endian)
    id[8] = (id[8] & 0x3f) | 0x80; // the variant
    return id;
}
