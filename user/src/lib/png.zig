//! Decoding PNG pictures (the format icons come in): every color type (grey, RGB, palette, each
//! with or without alpha), bit depths 1 to 16, transparency (tRNS). Interlaced PNGs aren't
//! supported (decode returns error.Unsupported); icon sets don't use them.
//!
//! A PNG is a list of chunks; the picture is the IDAT chunks joined together, zlib-compressed
//! (DEFLATE, which lib/deflate.zig inflates), each row starting with a filter byte that says how it
//! was predicted from the row above and the pixel to its left.

const std = @import("std");
const deflate = @import("deflate.zig");

const gpa = std.heap.page_allocator;
pub const Error = error{ NotPng, Unsupported, Corrupt, OutOfMemory };

/// A decoded picture: w x h pixels, 0xAARRGGBB (straight, not premultiplied alpha), row by row.
pub const Image = struct {
    w: u32,
    h: u32,
    pixels: []u32,

    pub fn deinit(img: *Image) void {
        gpa.free(img.pixels);
    }
};

const signature = "\x89PNG\r\n\x1a\n";

pub fn decode(data: []const u8) Error!Image {
    if (data.len < 8 or !std.mem.eql(u8, data[0..8], signature)) return error.NotPng;
    var width: u32 = 0;
    var height: u32 = 0;
    var depth: u8 = 0;
    var color: u8 = 0;
    var palette: [256]u32 = @splat(0xff000000);
    var trns_key: ?[3]u16 = null; // the one color that's transparent (grey or RGB pictures)
    var idat = std.ArrayList(u8).empty;
    defer idat.deinit(gpa);

    var pos: usize = 8;
    while (pos + 12 <= data.len) {
        const len = std.mem.readInt(u32, data[pos..][0..4], .big);
        const kind = data[pos + 4 .. pos + 8];
        if (len > data.len - pos - 12) return error.Corrupt;
        const body = data[pos + 8 .. pos + 8 + len];
        pos += 12 + len;
        if (std.mem.eql(u8, kind, "IHDR")) {
            if (body.len < 13) return error.Corrupt;
            width = std.mem.readInt(u32, body[0..4], .big);
            height = std.mem.readInt(u32, body[4..8], .big);
            depth = body[8];
            color = body[9];
            if (body[12] != 0) return error.Unsupported; // interlaced
            if (width == 0 or height == 0 or width > 16384 or height > 16384) return error.Unsupported;
        } else if (std.mem.eql(u8, kind, "PLTE")) {
            for (0..@min(256, body.len / 3)) |i| {
                palette[i] = 0xff000000 | @as(u32, body[3 * i]) << 16 | @as(u32, body[3 * i + 1]) << 8 | body[3 * i + 2];
            }
        } else if (std.mem.eql(u8, kind, "tRNS")) {
            if (color == 3) {
                for (body, 0..) |a, i| {
                    if (i < 256) palette[i] = (palette[i] & 0xffffff) | @as(u32, a) << 24;
                }
            } else if (color == 0 and body.len >= 2) {
                const g = std.mem.readInt(u16, body[0..2], .big);
                trns_key = .{ g, g, g };
            } else if (color == 2 and body.len >= 6) {
                trns_key = .{ std.mem.readInt(u16, body[0..2], .big), std.mem.readInt(u16, body[2..4], .big), std.mem.readInt(u16, body[4..6], .big) };
            }
        } else if (std.mem.eql(u8, kind, "IDAT")) {
            try idat.appendSlice(gpa, body);
        } else if (std.mem.eql(u8, kind, "IEND")) {
            break;
        }
    }
    if (width == 0) return error.Corrupt;

    const channels: u32 = switch (color) {
        0, 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        else => return error.Corrupt,
    };
    switch (depth) {
        1, 2, 4 => if (color != 0 and color != 3) return error.Corrupt,
        8 => {},
        16 => if (color == 3) return error.Corrupt,
        else => return error.Corrupt,
    }
    // zlib: a two-byte header, the DEFLATE data, then a checksum.
    if (idat.items.len < 6) return error.Corrupt;
    const raw = deflate.inflate(idat.items[2 .. idat.items.len - 4]) catch return error.Corrupt;
    defer gpa.free(raw);

    const bits_per_pixel = channels * depth;
    const stride = (width * bits_per_pixel + 7) / 8;
    const bpp = @max(1, bits_per_pixel / 8); // bytes to the pixel on the left, for the filters
    if (raw.len < (stride + 1) * height) return error.Corrupt;

    // Undo the filters, row by row, in place.
    var prev: []u8 = &.{};
    for (0..height) |y| {
        const row = raw[y * (stride + 1) + 1 ..][0..stride];
        const filter = raw[y * (stride + 1)];
        for (row, 0..) |*b, i| {
            const a: u32 = if (i >= bpp) row[i - bpp] else 0;
            const up: u32 = if (prev.len > 0) prev[i] else 0;
            const c: u32 = if (prev.len > 0 and i >= bpp) prev[i - bpp] else 0;
            const add: u32 = switch (filter) {
                0 => 0,
                1 => a,
                2 => up,
                3 => (a + up) / 2,
                4 => paeth(a, up, c),
                else => return error.Corrupt,
            };
            b.* = @truncate(b.* +% add);
        }
        prev = row;
    }

    const pixels = try gpa.alloc(u32, width * height);
    errdefer gpa.free(pixels);
    for (0..height) |y| {
        const row = raw[y * (stride + 1) + 1 ..][0..stride];
        for (0..width) |x| {
            pixels[y * width + x] = pixel(row, @intCast(x), color, depth, channels, &palette, trns_key);
        }
    }
    return .{ .w = width, .h = height, .pixels = pixels };
}

fn paeth(a: u32, b: u32, c: u32) u32 {
    const p: i32 = @as(i32, @intCast(a)) + @as(i32, @intCast(b)) - @as(i32, @intCast(c));
    const pa = @abs(p - @as(i32, @intCast(a)));
    const pb = @abs(p - @as(i32, @intCast(b)));
    const pc = @abs(p - @as(i32, @intCast(c)));
    if (pa <= pb and pa <= pc) return a;
    if (pb <= pc) return b;
    return c;
}

/// Sample `n` of a row (counting in samples of `depth` bits), at full precision.
fn sample(row: []const u8, n: u32, depth: u8) u16 {
    return switch (depth) {
        16 => std.mem.readInt(u16, row[n * 2 ..][0..2], .big),
        8 => row[n],
        else => blk: {
            const bit = n * depth;
            const shift: u3 = @intCast(8 - depth - bit % 8);
            const mask: u8 = @intCast((@as(u16, 1) << @intCast(depth)) - 1);
            break :blk (row[bit / 8] >> shift) & mask;
        },
    };
}

fn pixel(row: []const u8, x: u32, color: u8, depth: u8, channels: u32, palette: *const [256]u32, trns_key: ?[3]u16) u32 {
    if (color == 3) return palette[sample(row, x, depth)];
    var s: [4]u16 = undefined;
    for (0..channels) |i| s[i] = sample(row, x * channels + @as(u32, @intCast(i)), depth);
    const max: u32 = (@as(u32, 1) << @intCast(depth)) - 1;
    const to8 = struct {
        fn f(v: u16, m: u32) u32 {
            // 16 bits: the high byte, as other decoders do; fewer than 8: spread over 0..255.
            return if (m == 65535) v >> 8 else @as(u32, v) * 255 / m;
        }
    }.f;
    return switch (color) {
        0 => blk: {
            const g = to8(s[0], max);
            const a: u32 = if (trns_key != null and s[0] == trns_key.?[0]) 0 else 255;
            break :blk a << 24 | g << 16 | g << 8 | g;
        },
        2 => blk: {
            const a: u32 = if (trns_key != null and s[0] == trns_key.?[0] and s[1] == trns_key.?[1] and s[2] == trns_key.?[2]) 0 else 255;
            break :blk a << 24 | to8(s[0], max) << 16 | to8(s[1], max) << 8 | to8(s[2], max);
        },
        4 => blk: {
            const g = to8(s[0], max);
            break :blk to8(s[1], max) << 24 | g << 16 | g << 8 | g;
        },
        else => to8(s[3], max) << 24 | to8(s[0], max) << 16 | to8(s[1], max) << 8 | to8(s[2], max),
    };
}
