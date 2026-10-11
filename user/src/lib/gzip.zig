//! The gzip file format (RFC 1952): a small header, DEFLATE data, then a CRC-32 and the length.

const std = @import("std");
const deflate = @import("deflate.zig");

const gpa = std.heap.page_allocator;
pub const Error = error{ NotGzip, Corrupt, OutOfMemory };

const FEXTRA = 4;
const FNAME = 8;
const FCOMMENT = 16;
const FHCRC = 2;

pub fn looksGzipped(data: []const u8) bool {
    return data.len >= 2 and data[0] == 0x1f and data[1] == 0x8b;
}

/// Wraps `data` as a .gz file. `name` goes in the header, as gzip does (empty for none).
pub fn compress(data: []const u8, name: []const u8) Error![]u8 {
    const body = try deflate.deflate(data);
    defer gpa.free(body);
    var out = std.ArrayList(u8).empty;
    errdefer out.deinit(gpa);
    try out.appendSlice(gpa, &.{ 0x1f, 0x8b, 8, if (name.len > 0) FNAME else 0, 0, 0, 0, 0, 0, 3 });
    if (name.len > 0) {
        try out.appendSlice(gpa, name);
        try out.append(gpa, 0);
    }
    try out.appendSlice(gpa, body);
    var tail: [8]u8 = undefined;
    std.mem.writeInt(u32, tail[0..4], deflate.crc32(data), .little);
    std.mem.writeInt(u32, tail[4..8], @truncate(data.len), .little);
    try out.appendSlice(gpa, &tail);
    return out.toOwnedSlice(gpa);
}

/// Unpacks a .gz file, checking the CRC-32 at its end.
pub fn decompress(data: []const u8) Error![]u8 {
    if (!looksGzipped(data) or data.len < 18) return error.NotGzip;
    if (data[2] != 8) return error.Corrupt; // only DEFLATE is defined
    const flags = data[3];
    var at: usize = 10;
    if (flags & FEXTRA != 0) {
        if (at + 2 > data.len) return error.Corrupt;
        at += 2 + (@as(usize, data[at]) | @as(usize, data[at + 1]) << 8);
    }
    for ([_]u8{ FNAME, FCOMMENT }) |flag| {
        if (flags & flag != 0) {
            while (at < data.len and data[at] != 0) at += 1;
            at += 1;
        }
    }
    if (flags & FHCRC != 0) at += 2;
    if (at + 8 > data.len) return error.Corrupt;
    const out = deflate.inflate(data[at .. data.len - 8]) catch return error.Corrupt;
    const want = std.mem.readInt(u32, data[data.len - 8 ..][0..4], .little);
    if (deflate.crc32(out) != want) {
        gpa.free(out);
        return error.Corrupt;
    }
    return out;
}
