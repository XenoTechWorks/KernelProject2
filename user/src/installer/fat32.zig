//! Making a FAT32 file system with a given set of folders and files, in one go: the EFI system
//! partition the firmware boots from. Everything is laid out up front, each folder and file in
//! one run of clusters, so the FAT is written once and the files are streamed straight to the disk.
//!
//! Names that don't fit 8.3 (or aren't upper case) get long-name entries, as Windows writes them.

const std = @import("std");
const sys = @import("../lib/sys.zig");
const Disk = @import("disk.zig").Disk;
const randomId = @import("disk.zig").randomId;

const gpa = std.heap.page_allocator;
pub const Error = error{ Io, TooSmall, OutOfMemory, Source };

/// What goes in it: folders (paths like "EFI/BOOT") and files (a path and where to read it from,
/// or its contents).
pub const Item = struct {
    path: []const u8,
    dir: bool = false,
    source: ?[*:0]const u8 = null,
    contents: []const u8 = "",
};

const reserved = 32;
const fats = 2;

pub const Progress = struct {
    ctx: ?*anyopaque = null,
    f: *const fn (?*anyopaque, bytes: u64) void,
};

/// Formats the partition from sector `first` (`count` sectors) and writes `items` into it. Calls
/// `progress` with the bytes of file data written so far.
pub fn make(d: Disk, first: u64, count: u64, items: []const Item, progress: Progress) Error!void {
    // Clusters as large as possible while there are still enough of them to be FAT32 (65525).
    var spc: u64 = 8;
    var fat_sectors: u64 = 0;
    var clusters: u64 = 0;
    while (true) : (spc /= 2) {
        fat_sectors = 1;
        for (0..4) |_| {
            clusters = (count - reserved - fats * fat_sectors) / spc;
            fat_sectors = ((clusters + 2) * 4 + 511) / 512;
        }
        clusters = (count - reserved - fats * fat_sectors) / spc;
        if (clusters >= 65600 or spc == 1) break;
    }
    if (clusters < 65600) return error.TooSmall;
    const cluster_bytes = spc * 512;
    const data_start = first + reserved + fats * fat_sectors;

    // Sizes, and a run of clusters for each folder and file (the root folder first: cluster 2).
    const n = items.len + 1;
    const sizes = try gpa.alloc(u64, n);
    defer gpa.free(sizes);
    const starts = try gpa.alloc(u32, n);
    defer gpa.free(starts);
    sizes[0] = cluster_bytes; // the root folder
    for (items, 1..) |it, i| {
        sizes[i] = if (it.dir) cluster_bytes else if (it.source) |src| try fileSize(src) else it.contents.len;
    }
    var next: u32 = 2;
    for (0..n) |i| {
        starts[i] = next;
        next += @intCast(@max(1, (sizes[i] + cluster_bytes - 1) / cluster_bytes));
    }
    if (next - 2 > clusters) return error.TooSmall;

    // The FAT: each run chained, ending in an end-of-chain mark.
    const fat = try gpa.alloc(u8, fat_sectors * 512);
    defer gpa.free(fat);
    @memset(fat, 0);
    std.mem.writeInt(u32, fat[0..4], 0x0FFF_FFF8, .little);
    std.mem.writeInt(u32, fat[4..8], 0x0FFF_FFFF, .little);
    for (0..n) |i| {
        const len: u32 = @intCast(@max(1, (sizes[i] + cluster_bytes - 1) / cluster_bytes));
        for (0..len) |k| {
            const c = starts[i] + @as(u32, @intCast(k));
            const value: u32 = if (k + 1 == len) 0x0FFF_FFFF else c + 1;
            std.mem.writeInt(u32, fat[c * 4 ..][0..4], value, .little);
        }
    }

    // The boot sector, the FSInfo sector, and their backups at 6 and 7.
    var boot: [512]u8 = @splat(0);
    boot[0..3].* = .{ 0xEB, 0x58, 0x90 };
    @memcpy(boot[3..11], "KP2 1.0 ");
    std.mem.writeInt(u16, boot[11..13], 512, .little);
    boot[13] = @intCast(spc);
    std.mem.writeInt(u16, boot[14..16], reserved, .little);
    boot[16] = fats;
    boot[21] = 0xF8; // a fixed disk
    std.mem.writeInt(u16, boot[24..26], 63, .little);
    std.mem.writeInt(u16, boot[26..28], 255, .little);
    std.mem.writeInt(u32, boot[28..32], @intCast(first), .little); // hidden sectors before it
    std.mem.writeInt(u32, boot[32..36], @intCast(count), .little);
    std.mem.writeInt(u32, boot[36..40], @intCast(fat_sectors), .little);
    std.mem.writeInt(u32, boot[44..48], 2, .little); // the root folder's cluster
    std.mem.writeInt(u16, boot[48..50], 1, .little); // FSInfo
    std.mem.writeInt(u16, boot[50..52], 6, .little); // backup boot sector
    boot[64] = 0x80;
    boot[66] = 0x29;
    const volume_id = randomId();
    @memcpy(boot[67..71], volume_id[0..4]);
    @memcpy(boot[71..82], "KP2 BOOT   ");
    @memcpy(boot[82..90], "FAT32   ");
    boot[510] = 0x55;
    boot[511] = 0xAA;
    var info: [512]u8 = @splat(0);
    std.mem.writeInt(u32, info[0..4], 0x4161_5252, .little);
    std.mem.writeInt(u32, info[484..488], 0x6141_7272, .little);
    std.mem.writeInt(u32, info[488..492], @intCast(clusters - (next - 2)), .little);
    std.mem.writeInt(u32, info[492..496], next, .little);
    std.mem.writeInt(u32, info[508..512], 0xAA55_0000, .little);
    try d.zero(first * 512, reserved * 512);
    for ([_]u64{ 0, 6 }) |at| {
        try d.write((first + at) * 512, &boot);
        try d.write((first + at + 1) * 512, &info);
    }
    for (0..fats) |k| try d.write((first + reserved + k * fat_sectors) * 512, fat);

    // The folders.
    const dir = try gpa.alloc(u8, cluster_bytes);
    defer gpa.free(dir);
    for (0..n) |i| {
        if (i > 0 and !items[i - 1].dir) continue;
        @memset(dir, 0);
        var at: usize = 0;
        const self_path = if (i == 0) "" else items[i - 1].path;
        if (i > 0) {
            const parent = parentIndex(items, self_path);
            putShort(dir, &at, ".          ".*, 0x10, starts[i], 0, 0);
            putShort(dir, &at, "..         ".*, 0x10, if (parent == 0) 0 else starts[parent], 0, 0);
        }
        for (items, 1..) |it, k| {
            if (parentIndex(items, it.path) != i) continue;
            const name = baseName(it.path);
            try putEntry(dir, &at, name, if (it.dir) 0x10 else 0x20, starts[k], if (it.dir) 0 else @intCast(sizes[k]), k);
        }
        try d.write((data_start + (starts[i] - 2) * spc) * 512, dir);
    }

    // The files' contents.
    var written: u64 = 0;
    const chunk = try gpa.alloc(u8, 1024 * 1024);
    defer gpa.free(chunk);
    for (items, 1..) |it, k| {
        if (it.dir) continue;
        const at = (data_start + (starts[k] - 2) * spc) * 512;
        if (it.source) |src| {
            const fd: i32 = switch (sys.open(src, sys.O_RDONLY, 0)) {
                .ok => |fd| @intCast(fd),
                .err => return error.Source,
            };
            defer sys.close(fd);
            var pos: u64 = 0;
            while (pos < sizes[k]) {
                const want: usize = @intCast(@min(chunk.len, sizes[k] - pos));
                var got: usize = 0;
                while (got < want) {
                    switch (sys.read(fd, chunk[got..want])) {
                        .ok => |r| {
                            if (r == 0) return error.Source;
                            got += r;
                        },
                        .err => |e| if (e != sys.EINTR) return error.Source,
                    }
                }
                const padded = (got + 511) / 512 * 512;
                @memset(chunk[got..padded], 0);
                try d.write(at + pos, chunk[0..padded]);
                pos += got;
                written += got;
                progress.f(progress.ctx, written);
            }
        } else {
            const padded = (it.contents.len + 511) / 512 * 512;
            @memset(chunk[0..padded], 0);
            @memcpy(chunk[0..it.contents.len], it.contents);
            try d.write(at, chunk[0..padded]);
        }
    }
}

fn fileSize(path: [*:0]const u8) Error!u64 {
    var st: sys.Stat = undefined;
    if (sys.stat(path, &st) != .ok) return error.Source;
    return @intCast(st.size);
}

fn baseName(path: []const u8) []const u8 {
    return if (std.mem.lastIndexOfScalar(u8, path, '/')) |i| path[i + 1 ..] else path;
}

/// Which entry is the folder holding `path` (0: the root folder; items are 1-based).
fn parentIndex(items: []const Item, path: []const u8) usize {
    const cut = std.mem.lastIndexOfScalar(u8, path, '/') orelse return 0;
    for (items, 1..) |it, i| {
        if (it.dir and std.mem.eql(u8, it.path, path[0..cut])) return i;
    }
    return 0;
}

/// A folder entry for `name`: the short 8.3 entry alone when the name is one already (upper
/// case), or else long-name entries before a made-up short name ("INITRA~1CPI").
fn putEntry(dir: []u8, at: *usize, name: []const u8, attr: u8, cluster: u32, size: u32, unique: usize) Error!void {
    var short: [11]u8 = @splat(' ');
    const dot = std.mem.lastIndexOfScalar(u8, name, '.');
    const base = if (dot) |i| name[0..i] else name;
    const ext = if (dot) |i| name[i + 1 ..] else "";
    var plain = base.len >= 1 and base.len <= 8 and ext.len <= 3;
    for (name) |c| {
        if (!(std.ascii.isUpper(c) or std.ascii.isDigit(c) or c == '.' or c == '_' or c == '-')) plain = false;
    }
    if (plain) {
        @memcpy(short[0..base.len], base);
        @memcpy(short[8..][0..ext.len], ext);
        return putShort(dir, at, short, attr, cluster, size, 0);
    }
    // A short alias: the first six letters of the name, "~" and a number, the extension's first three.
    var k: usize = 0;
    for (base) |c| {
        if (k == 6) break;
        if (std.ascii.isAlphanumeric(c)) {
            short[k] = std.ascii.toUpper(c);
            k += 1;
        }
    }
    short[k] = '~';
    short[k + 1] = '0' + @as(u8, @intCast(unique % 10));
    for (ext[0..@min(3, ext.len)], 0..) |c, e| short[8 + e] = std.ascii.toUpper(c);
    var sum: u8 = 0;
    for (short) |c| sum = ((sum & 1) << 7) +% (sum >> 1) +% c;
    // Long-name entries: 13 UTF-16 units each, the last part first.
    const parts = (name.len + 12) / 13;
    var p = parts;
    while (p > 0) : (p -= 1) {
        if (at.* + 32 > dir.len) return error.TooSmall;
        const e = dir[at.*..][0..32];
        @memset(e, 0);
        e[0] = @as(u8, @intCast(p)) | if (p == parts) @as(u8, 0x40) else 0;
        e[11] = 0x0F;
        e[13] = sum;
        const offsets = [_]usize{ 1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30 };
        for (offsets, 0..) |o, j| {
            const idx = (p - 1) * 13 + j;
            const unit: u16 = if (idx < name.len) name[idx] else if (idx == name.len) 0 else 0xFFFF;
            std.mem.writeInt(u16, e[o..][0..2], unit, .little);
        }
        at.* += 32;
    }
    putShort(dir, at, short, attr, cluster, size, 0);
}

fn putShort(dir: []u8, at: *usize, name: [11]u8, attr: u8, cluster: u32, size: u32, _: u8) void {
    const e = dir[at.*..][0..32];
    @memset(e, 0);
    e[0..11].* = name;
    e[11] = attr;
    // 2026-01-01 00:00 (the date the files are dated).
    const date: u16 = (2026 - 1980) << 9 | 1 << 5 | 1;
    std.mem.writeInt(u16, e[16..18], date, .little);
    std.mem.writeInt(u16, e[18..20], date, .little);
    std.mem.writeInt(u16, e[24..26], date, .little);
    std.mem.writeInt(u16, e[20..22], @intCast(cluster >> 16), .little);
    std.mem.writeInt(u16, e[26..28], @intCast(cluster & 0xffff), .little);
    std.mem.writeInt(u32, e[28..32], size, .little);
    at.* += 32;
}
