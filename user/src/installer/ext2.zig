//! Making an empty ext2 file system (what `mkfs.ext2` does) on a partition: 4 KiB blocks in groups
//! of 32768, 8192 inodes a group, backup superblocks only in groups 0, 1 and the powers of 3, 5
//! and 7 (sparse_super), a root folder and lost+found. The kernel's ext2 driver mounts it as
//! /root, and e2fsck finds nothing to fix (tests/install.py checks).

const std = @import("std");
const sys = @import("../lib/sys.zig");
const Disk = @import("disk.zig").Disk;
const randomId = @import("disk.zig").randomId;

const gpa = std.heap.page_allocator;
pub const Error = error{ Io, TooSmall, OutOfMemory };

const block = 4096;
const per_group = 32768;
const inodes_per_group = 8192;
const inode_size = 128;
const table_blocks = inodes_per_group * inode_size / block; // 256
const root_ino = 2;
const lost_found_ino = 11;

fn hasBackup(g: u64) bool {
    if (g <= 1) return true;
    for ([_]u64{ 3, 5, 7 }) |base| {
        var p: u64 = base;
        while (p < g) p *= base;
        if (p == g) return true;
    }
    return false;
}

fn now() u32 {
    var ts: extern struct { sec: i64, nsec: i64 } = .{ .sec = 0, .nsec = 0 };
    _ = sys.syscall2(sys.nr.clock_gettime, 0, @intFromPtr(&ts));
    return @intCast(@max(ts.sec, 0));
}

/// Formats `bytes` from byte `at` of the disk as ext2, labelled `label`.
pub fn make(d: Disk, at: u64, bytes: u64, label: []const u8) Error!void {
    var blocks = bytes / block;
    var groups = (blocks + per_group - 1) / per_group;
    const gdt_blocks = (groups * 32 + block - 1) / block;
    const overhead_max = 1 + gdt_blocks + 2 + table_blocks;
    // A last group too small to hold its own bookkeeping and some room is left out.
    if (groups > 1 and blocks - (groups - 1) * per_group < overhead_max + 64) {
        groups -= 1;
        blocks = groups * per_group;
    }
    if (blocks < overhead_max + 256) return error.TooSmall;

    const group_table = try gpa.alloc(u8, gdt_blocks * block);
    defer gpa.free(group_table);
    @memset(group_table, 0);
    const buf = try gpa.alloc(u8, block);
    defer gpa.free(buf);
    var free_blocks_total: u64 = 0;
    var root_block: u32 = 0;
    var lost_found_block: u32 = 0;
    const stamp = now();

    // Each group's layout, its bitmaps and inode table.
    for (0..groups) |g| {
        const start = g * per_group;
        const size = @min(per_group, blocks - start);
        var off = start;
        if (hasBackup(g)) off += 1 + gdt_blocks;
        const block_bitmap = off;
        const inode_bitmap = off + 1;
        const inode_table = off + 2;
        var used = inode_table + table_blocks - start;
        if (g == 0) {
            root_block = @intCast(start + used);
            lost_found_block = @intCast(start + used + 1);
            used += 2;
        }
        // The block bitmap: what's used, and the bits past a short last group set too.
        @memset(buf, 0);
        for (0..block * 8) |b| {
            if (b < used or b >= size) buf[b / 8] |= @as(u8, 1) << @intCast(b % 8);
        }
        try d.write(at + block_bitmap * block, buf);
        // The inode bitmap: inodes 1..11 in the first group (the reserved ones and lost+found);
        // past the group's inodes, set.
        @memset(buf, 0);
        for (0..block * 8) |b| {
            if ((g == 0 and b < lost_found_ino) or b >= inodes_per_group) buf[b / 8] |= @as(u8, 1) << @intCast(b % 8);
        }
        try d.write(at + inode_bitmap * block, buf);
        try d.zero(at + inode_table * block, table_blocks * block);

        const free = size - used;
        free_blocks_total += free;
        const e = group_table[g * 32 ..][0..32];
        std.mem.writeInt(u32, e[0..4], @intCast(block_bitmap), .little);
        std.mem.writeInt(u32, e[4..8], @intCast(inode_bitmap), .little);
        std.mem.writeInt(u32, e[8..12], @intCast(inode_table), .little);
        std.mem.writeInt(u16, e[12..14], @intCast(free), .little);
        std.mem.writeInt(u16, e[14..16], if (g == 0) inodes_per_group - lost_found_ino else inodes_per_group, .little);
        std.mem.writeInt(u16, e[16..18], if (g == 0) 2 else 0, .little); // folders: / and lost+found
    }

    // The root folder (., .., lost+found) and lost+found (., ..), with their inodes.
    @memset(buf, 0);
    var pos: usize = 0;
    dirent(buf, &pos, root_ino, ".", 12);
    dirent(buf, &pos, root_ino, "..", 12);
    dirent(buf, &pos, lost_found_ino, "lost+found", block - 24);
    try d.write(at + @as(u64, root_block) * block, buf);
    @memset(buf, 0);
    pos = 0;
    dirent(buf, &pos, lost_found_ino, ".", 12);
    dirent(buf, &pos, root_ino, "..", block - 12);
    try d.write(at + @as(u64, lost_found_block) * block, buf);
    // Inodes 2 and 11 sit in the first block of group 0's inode table.
    @memset(buf, 0);
    inode(buf[(root_ino - 1) * inode_size ..][0..inode_size], 0o40755, 3, root_block, stamp);
    inode(buf[(lost_found_ino - 1) * inode_size ..][0..inode_size], 0o40700, 2, lost_found_block, stamp);
    const table0 = std.mem.readInt(u32, group_table[8..12], .little);
    try d.write(at + @as(u64, table0) * block, buf);

    // The superblock: at byte 1024 in group 0, and at the start of each backup group with its
    // group number; the group table right after it.
    var sb: [1024]u8 = @splat(0);
    const w32 = struct {
        fn f(b: []u8, o: usize, v: u64) void {
            std.mem.writeInt(u32, b[o..][0..4], @intCast(v), .little);
        }
    }.f;
    const w16 = struct {
        fn f(b: []u8, o: usize, v: u64) void {
            std.mem.writeInt(u16, b[o..][0..2], @intCast(v), .little);
        }
    }.f;
    w32(&sb, 0, groups * inodes_per_group);
    w32(&sb, 4, blocks);
    w32(&sb, 12, free_blocks_total);
    w32(&sb, 16, groups * inodes_per_group - lost_found_ino);
    w32(&sb, 20, 0); // first data block (4 KiB blocks: 0)
    w32(&sb, 24, 2); // log2(block size) - 10
    w32(&sb, 28, 2);
    w32(&sb, 32, per_group);
    w32(&sb, 36, per_group);
    w32(&sb, 40, inodes_per_group);
    w32(&sb, 48, stamp); // written
    w16(&sb, 54, 0xFFFF); // max mount count: never forces a check
    w16(&sb, 56, 0xEF53);
    w16(&sb, 58, 1); // clean
    w16(&sb, 60, 1); // on errors: continue
    w32(&sb, 64, stamp); // last checked
    w32(&sb, 76, 1); // revision 1: dynamic inode sizes, features
    w32(&sb, 84, lost_found_ino + 0); // first non-reserved inode: 11
    w16(&sb, 88, inode_size);
    w32(&sb, 96, 0x2); // incompatible features: file types in folder entries
    w32(&sb, 100, 0x1); // read-only compatible: sparse_super
    sb[104..120].* = randomId();
    @memcpy(sb[120..][0..@min(label.len, 16)], label[0..@min(label.len, 16)]);
    w32(&sb, 264, stamp); // made
    for (0..groups) |g| {
        if (!hasBackup(g)) continue;
        const start = g * per_group;
        w16(&sb, 90, g); // which group this copy is in
        @memset(buf, 0);
        if (g == 0) @memcpy(buf[1024..2048], &sb) else @memcpy(buf[0..1024], &sb);
        try d.write(at + start * block, buf);
        try d.write(at + (start + 1) * block, group_table);
    }
}

fn dirent(buf: []u8, pos: *usize, ino: u32, name: []const u8, rec_len: usize) void {
    const e = buf[pos.*..];
    std.mem.writeInt(u32, e[0..4], ino, .little);
    std.mem.writeInt(u16, e[4..6], @intCast(rec_len), .little);
    e[6] = @intCast(name.len);
    e[7] = 2; // a folder
    @memcpy(e[8..][0..name.len], name);
    pos.* += rec_len;
}

fn inode(e: []u8, mode: u16, links: u16, first_block: u32, stamp: u32) void {
    std.mem.writeInt(u16, e[0..2], mode, .little);
    std.mem.writeInt(u32, e[4..8], block, .little); // size
    std.mem.writeInt(u32, e[8..12], stamp, .little);
    std.mem.writeInt(u32, e[12..16], stamp, .little);
    std.mem.writeInt(u32, e[16..20], stamp, .little);
    std.mem.writeInt(u16, e[26..28], links, .little);
    std.mem.writeInt(u32, e[28..32], block / 512, .little); // 512-byte units
    std.mem.writeInt(u32, e[40..44], first_block, .little);
}
