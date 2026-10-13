//! Writing a GPT partition table (UEFI's): a protective MBR in sector 0, the header in sector 1
//! and the partition entries after it, and a backup copy of both at the end of the disk, each with
//! its CRC-32.

const std = @import("std");
const crc32 = @import("../lib/deflate.zig").crc32;
const Disk = @import("disk.zig").Disk;
const randomId = @import("disk.zig").randomId;

/// The EFI system partition (FAT, where UEFI looks for boot programs).
pub const esp_type = guid("C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
/// "Linux filesystem data".
pub const linux_type = guid("0FC63DAF-8483-4772-8E79-3D69D8477DE4");

pub const Partition = struct { type_id: [16]u8, first: u64, last: u64, name: []const u8 };

const entries = 128;
const entry_size = 128;
const entry_sectors = entries * entry_size / 512; // 32

/// The first and last sectors partitions may use on a disk of `sectors`.
pub fn usable(sectors: u64) struct { first: u64, last: u64 } {
    return .{ .first = 2 + entry_sectors, .last = sectors - 2 - entry_sectors };
}

/// A GUID from its text form: the first three fields are stored little-endian.
fn guid(comptime s: []const u8) [16]u8 {
    var out: [16]u8 = undefined;
    var hex: [32]u8 = undefined;
    var n: usize = 0;
    for (s) |c| {
        if (c != '-') {
            hex[n] = c;
            n += 1;
        }
    }
    var bytes: [16]u8 = undefined;
    for (0..16) |i| bytes[i] = std.fmt.parseInt(u8, hex[2 * i .. 2 * i + 2], 16) catch unreachable;
    out[0..4].* = .{ bytes[3], bytes[2], bytes[1], bytes[0] };
    out[4..6].* = .{ bytes[5], bytes[4] };
    out[6..8].* = .{ bytes[7], bytes[6] };
    @memcpy(out[8..16], bytes[8..16]);
    return out;
}

pub fn write(d: Disk, parts: []const Partition) error{Io}!void {
    const sectors = d.sectors();
    // The partition entries (the same for both copies).
    var table: [entries * entry_size]u8 = @splat(0);
    for (parts, 0..) |p, i| {
        const e = table[i * entry_size ..][0..entry_size];
        e[0..16].* = p.type_id;
        e[16..32].* = randomId();
        std.mem.writeInt(u64, e[32..40], p.first, .little);
        std.mem.writeInt(u64, e[40..48], p.last, .little);
        for (p.name, 0..) |c, k| {
            if (k == 36) break;
            std.mem.writeInt(u16, e[56 + 2 * k ..][0..2], c, .little);
        }
    }
    const table_crc = crc32(&table);
    const disk_id = randomId();

    // Sector 0: a protective MBR, one partition of type 0xEE covering the disk, so old tools
    // leave it alone.
    var mbr: [512]u8 = @splat(0);
    mbr[446 + 1] = 0x00;
    mbr[446 + 2] = 0x02;
    mbr[446 + 4] = 0xEE;
    mbr[446 + 5] = 0xFF;
    mbr[446 + 6] = 0xFF;
    mbr[446 + 7] = 0xFF;
    std.mem.writeInt(u32, mbr[446 + 8 ..][0..4], 1, .little);
    std.mem.writeInt(u32, mbr[446 + 12 ..][0..4], @intCast(@min(sectors - 1, 0xFFFF_FFFF)), .little);
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    try d.write(0, &mbr);

    const span = usable(sectors);
    // Primary: header at 1, entries from 2. Backup: entries just before the last sector, header in it.
    for ([_]struct { at: u64, other: u64, table_at: u64 }{
        .{ .at = 1, .other = sectors - 1, .table_at = 2 },
        .{ .at = sectors - 1, .other = 1, .table_at = sectors - 1 - entry_sectors },
    }) |copy| {
        var h: [512]u8 = @splat(0);
        @memcpy(h[0..8], "EFI PART");
        std.mem.writeInt(u32, h[8..12], 0x0001_0000, .little); // revision 1.0
        std.mem.writeInt(u32, h[12..16], 92, .little);
        std.mem.writeInt(u64, h[24..32], copy.at, .little);
        std.mem.writeInt(u64, h[32..40], copy.other, .little);
        std.mem.writeInt(u64, h[40..48], span.first, .little);
        std.mem.writeInt(u64, h[48..56], span.last, .little);
        h[56..72].* = disk_id;
        std.mem.writeInt(u64, h[72..80], copy.table_at, .little);
        std.mem.writeInt(u32, h[80..84], entries, .little);
        std.mem.writeInt(u32, h[84..88], entry_size, .little);
        std.mem.writeInt(u32, h[88..92], table_crc, .little);
        std.mem.writeInt(u32, h[16..20], crc32(h[0..92]), .little); // with its own field still 0
        try d.write(copy.table_at * 512, &table);
        try d.write(copy.at * 512, &h);
    }
}
