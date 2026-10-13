//! Decoding JPEG pictures: baseline (and extended 8-bit) Huffman JPEG, the kind cameras and most
//! programs save, in greyscale or YCbCr with any sampling (4:4:4, 4:2:2, 4:2:0 ...) and restart
//! markers. Progressive JPEGs aren't supported (decode returns error.Unsupported).
//!
//! It follows the JPEG standard (ITU T.81) step by step: Huffman-decode each 8x8 block's
//! coefficients, multiply them back up by the quantization table, inverse-DCT them into pixels,
//! then put the color planes together and convert YCbCr to RGB (JFIF).

const std = @import("std");

const gpa = std.heap.page_allocator;
pub const Error = error{ NotJpeg, Unsupported, Corrupt, OutOfMemory };

/// A decoded picture: w x h pixels, 0xRRGGBB, row by row.
pub const Image = struct {
    w: u32,
    h: u32,
    pixels: []u32,

    pub fn deinit(img: *Image) void {
        gpa.free(img.pixels);
    }
};

/// Where each of the 64 coefficients, in the order they're stored, goes in the 8x8 block.
const zigzag = [64]u8{
    0,  1,  8,  16, 9,  2,  3,  10, 17, 24, 32, 25, 18, 11, 4,  5,
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6,  7,  14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
};

/// A Huffman table, in the form the standard decodes with (F.2.2.3): for each code length, the
/// largest code of that length and where its values start.
const Huffman = struct {
    maxcode: [18]i32 = @splat(-1),
    valptr: [17]i32 = @splat(0),
    mincode: [17]i32 = @splat(0),
    values: [256]u8 = @splat(0),
    defined: bool = false,

    fn build(counts: [16]u8, values: []const u8) Error!Huffman {
        var t = Huffman{ .defined = true };
        if (values.len > 256) return error.Corrupt;
        @memcpy(t.values[0..values.len], values);
        var code: i32 = 0;
        var k: i32 = 0;
        for (counts, 1..) |n, len| {
            if (n == 0) {
                t.maxcode[len] = -1;
            } else {
                t.valptr[len] = k;
                t.mincode[len] = code;
                code += n;
                k += n;
                t.maxcode[len] = code - 1;
            }
            code <<= 1;
        }
        t.maxcode[17] = std.math.maxInt(i32); // past the longest code: stop
        return t;
    }
};

const Component = struct {
    id: u8,
    h: u32, // sampling factors
    v: u32,
    tq: u8, // quantization table
    td: u8 = 0, // Huffman tables (DC, AC) for this scan
    ta: u8 = 0,
    pred: i32 = 0, // the previous block's DC value
    plane: []u8 = &.{},
    stride: u32 = 0,
};

/// Reads the entropy-coded data bit by bit, skipping the 0x00 after each 0xFF (byte stuffing) and
/// stopping at markers.
const Bits = struct {
    data: []const u8,
    pos: usize,
    acc: u32 = 0,
    count: u5 = 0,
    marker: ?u8 = null,

    fn bit(b: *Bits) Error!u32 {
        if (b.count == 0) {
            var byte: u8 = 0;
            if (b.marker == null and b.pos < b.data.len) {
                byte = b.data[b.pos];
                b.pos += 1;
                if (byte == 0xFF) {
                    if (b.pos >= b.data.len) return error.Corrupt;
                    const next = b.data[b.pos];
                    if (next == 0) {
                        b.pos += 1;
                    } else {
                        // A marker in the middle of the data: from here on, read zeros (as decoders do).
                        b.marker = next;
                        b.pos -= 1;
                        byte = 0;
                    }
                }
            }
            b.acc = byte;
            b.count = 8;
        }
        b.count -= 1;
        return (b.acc >> b.count) & 1;
    }

    fn receive(b: *Bits, n: u32) Error!i32 {
        var v: i32 = 0;
        for (0..n) |_| v = (v << 1) | @as(i32, @intCast(try b.bit()));
        return v;
    }

    fn decode(b: *Bits, t: *const Huffman) Error!u8 {
        var code: i32 = @intCast(try b.bit());
        var len: usize = 1;
        while (code > t.maxcode[len]) {
            code = (code << 1) | @as(i32, @intCast(try b.bit()));
            len += 1;
            if (len > 16) return error.Corrupt;
        }
        const i = t.valptr[len] + code - t.mincode[len];
        if (i < 0 or i > 255) return error.Corrupt;
        return t.values[@intCast(i)];
    }

    /// After a restart interval: drop the rest of the byte and step over the RSTn marker.
    fn restart(b: *Bits) void {
        b.count = 0;
        if (b.marker) |m| {
            if (m >= 0xD0 and m <= 0xD7) b.pos += 2;
            b.marker = null;
        } else if (b.pos + 1 < b.data.len and b.data[b.pos] == 0xFF and b.data[b.pos + 1] >= 0xD0 and b.data[b.pos + 1] <= 0xD7) {
            b.pos += 2;
        }
    }
};

/// The value a coefficient's `n` bits stand for (F.2.2.1 EXTEND).
fn extend(v: i32, n: u32) i32 {
    if (n == 0) return 0;
    return if (v < @as(i32, 1) << @intCast(n - 1)) v - (@as(i32, 1) << @intCast(n)) + 1 else v;
}

/// cos_table[x][u] = C(u)/2 * cos((2x+1)u pi/16): one 1-D pass of the 8x8 inverse DCT.
const cos_table: [8][8]f32 = blk: {
    @setEvalBranchQuota(10000);
    var t: [8][8]f32 = undefined;
    for (0..8) |x| {
        for (0..8) |u| {
            const c: f64 = if (u == 0) 1.0 / @sqrt(2.0) else 1.0;
            t[x][u] = @floatCast(c / 2.0 * @cos(@as(f64, @floatFromInt((2 * x + 1) * u)) * std.math.pi / 16.0));
        }
    }
    break :blk t;
};

/// Turns 64 coefficients (row-major, v rows of u) into 8x8 pixels at `out`, `stride` apart.
fn idct(coef: *const [64]i32, out: []u8, stride: u32) void {
    var tmp: [64]f32 = undefined;
    for (0..8) |v| {
        for (0..8) |x| {
            var s: f32 = 0;
            for (0..8) |u| s += cos_table[x][u] * @as(f32, @floatFromInt(coef[v * 8 + u]));
            tmp[v * 8 + x] = s;
        }
    }
    for (0..8) |y| {
        for (0..8) |x| {
            var s: f32 = 0;
            for (0..8) |v| s += cos_table[y][v] * tmp[v * 8 + x];
            out[y * stride + x] = @intFromFloat(std.math.clamp(@round(s + 128), 0, 255));
        }
    }
}

pub fn decode(data: []const u8) Error!Image {
    if (data.len < 4 or data[0] != 0xFF or data[1] != 0xD8) return error.NotJpeg;
    var quant: [4][64]u16 = @splat(@splat(1));
    var dc: [4]Huffman = @splat(.{});
    var ac: [4]Huffman = @splat(.{});
    var comps: [4]Component = undefined;
    var ncomp: usize = 0;
    var width: u32 = 0;
    var height: u32 = 0;
    var restart_interval: u32 = 0;
    var adobe_transform: ?u8 = null;
    defer for (comps[0..ncomp]) |c| gpa.free(c.plane);

    var pos: usize = 2;
    while (true) {
        // Markers may be padded with extra 0xFF bytes.
        while (pos < data.len and data[pos] == 0xFF and pos + 1 < data.len and data[pos + 1] == 0xFF) pos += 1;
        if (pos + 4 > data.len or data[pos] != 0xFF) return error.Corrupt;
        const marker = data[pos + 1];
        const len = @as(usize, data[pos + 2]) << 8 | data[pos + 3];
        if (len < 2 or pos + 2 + len > data.len) return error.Corrupt;
        const seg = data[pos + 4 .. pos + 2 + len];
        pos += 2 + len;
        switch (marker) {
            0xC0, 0xC1 => { // baseline, extended (8-bit) Huffman
                if (seg.len < 6 or seg[0] != 8) return error.Unsupported;
                height = @as(u32, seg[1]) << 8 | seg[2];
                width = @as(u32, seg[3]) << 8 | seg[4];
                ncomp = seg[5];
                if (width == 0 or height == 0 or width > 16384 or height > 16384) return error.Unsupported;
                if (ncomp != 1 and ncomp != 3) return error.Unsupported;
                if (seg.len < 6 + 3 * ncomp) return error.Corrupt;
                for (0..ncomp) |i| {
                    const s = seg[6 + 3 * i ..];
                    comps[i] = .{ .id = s[0], .h = s[1] >> 4, .v = s[1] & 15, .tq = s[2] & 3 };
                    if (comps[i].h < 1 or comps[i].h > 4 or comps[i].v < 1 or comps[i].v > 4) return error.Corrupt;
                }
            },
            0xC2, 0xC3, 0xC5...0xC7, 0xC9...0xCB, 0xCD...0xCF => return error.Unsupported, // progressive, lossless, arithmetic
            0xC4 => { // Huffman tables
                var at: usize = 0;
                while (at < seg.len) {
                    if (at + 17 > seg.len) return error.Corrupt;
                    const class = seg[at] >> 4;
                    const slot = seg[at] & 3;
                    var counts: [16]u8 = undefined;
                    @memcpy(&counts, seg[at + 1 .. at + 17]);
                    var total: usize = 0;
                    for (counts) |n| total += n;
                    if (at + 17 + total > seg.len) return error.Corrupt;
                    const table = try Huffman.build(counts, seg[at + 17 .. at + 17 + total]);
                    if (class == 0) dc[slot] = table else ac[slot] = table;
                    at += 17 + total;
                }
            },
            0xDB => { // quantization tables (stored in zigzag order)
                var at: usize = 0;
                while (at < seg.len) {
                    const wide = seg[at] >> 4 != 0;
                    const slot = seg[at] & 3;
                    at += 1;
                    for (0..64) |k| {
                        if (wide) {
                            if (at + 2 > seg.len) return error.Corrupt;
                            quant[slot][k] = @as(u16, seg[at]) << 8 | seg[at + 1];
                            at += 2;
                        } else {
                            if (at >= seg.len) return error.Corrupt;
                            quant[slot][k] = seg[at];
                            at += 1;
                        }
                    }
                }
            },
            0xDD => { // restart interval
                if (seg.len < 2) return error.Corrupt;
                restart_interval = @as(u32, seg[0]) << 8 | seg[1];
            },
            0xEE => { // Adobe: whether three components are YCbCr (1) or RGB (0)
                if (seg.len >= 12 and std.mem.eql(u8, seg[0..5], "Adobe")) adobe_transform = seg[11];
            },
            0xDA => { // start of scan: the picture's data follows
                if (ncomp == 0) return error.Corrupt;
                if (seg.len < 1 or seg[0] != ncomp or seg.len < 1 + 2 * ncomp) return error.Unsupported; // one scan with every component
                for (0..ncomp) |i| {
                    const id = seg[1 + 2 * i];
                    for (comps[0..ncomp]) |*c| {
                        if (c.id == id) {
                            c.td = seg[2 + 2 * i] >> 4 & 3;
                            c.ta = seg[2 + 2 * i] & 3;
                        }
                    }
                }
                return finish(data, pos, comps[0..ncomp], &quant, &dc, &ac, width, height, restart_interval, adobe_transform);
            },
            0xD9 => return error.Corrupt, // end of image before any picture
            else => {}, // APPn, comments and the like
        }
    }
}

fn finish(data: []const u8, start: usize, comps: []Component, quant: *const [4][64]u16, dc: *const [4]Huffman, ac: *const [4]Huffman, width: u32, height: u32, restart_interval: u32, adobe_transform: ?u8) Error!Image {
    var hmax: u32 = 1;
    var vmax: u32 = 1;
    for (comps) |c| {
        hmax = @max(hmax, c.h);
        vmax = @max(vmax, c.v);
    }
    // One component alone is stored block by block, not in MCUs of several.
    const single = comps.len == 1;
    if (single) {
        comps[0].h = 1;
        comps[0].v = 1;
        hmax = 1;
        vmax = 1;
    }
    const mcu_w = 8 * hmax;
    const mcu_h = 8 * vmax;
    const mcus_x = (width + mcu_w - 1) / mcu_w;
    const mcus_y = (height + mcu_h - 1) / mcu_h;
    for (comps) |*c| {
        if (!dc[c.td].defined or !ac[c.ta].defined) return error.Corrupt;
        c.stride = mcus_x * c.h * 8;
        c.plane = try gpa.alloc(u8, c.stride * mcus_y * c.v * 8);
    }

    var bits = Bits{ .data = data, .pos = start };
    var coef: [64]i32 = undefined;
    var done: u32 = 0;
    var my: u32 = 0;
    while (my < mcus_y) : (my += 1) {
        var mx: u32 = 0;
        while (mx < mcus_x) : (mx += 1) {
            if (restart_interval != 0 and done != 0 and done % restart_interval == 0) {
                bits.restart();
                for (comps) |*c| c.pred = 0;
            }
            for (comps) |*c| {
                for (0..c.v) |by| {
                    for (0..c.h) |bx| {
                        // DC: the difference from the last block's.
                        @memset(&coef, 0);
                        const q = &quant[c.tq];
                        const t = try bits.decode(&dc[c.td]);
                        if (t > 16) return error.Corrupt;
                        c.pred += extend(try bits.receive(t), t);
                        coef[0] = c.pred * q[0];
                        // AC: runs of zeros and values, in zigzag order.
                        var k: usize = 1;
                        while (k < 64) {
                            const rs = try bits.decode(&ac[c.ta]);
                            const r = rs >> 4;
                            const s = rs & 15;
                            if (s == 0) {
                                if (r != 15) break; // end of block
                                k += 16;
                                continue;
                            }
                            k += r;
                            if (k > 63) return error.Corrupt;
                            coef[zigzag[k]] = extend(try bits.receive(s), s) * q[k];
                            k += 1;
                        }
                        const px = (mx * c.h + @as(u32, @intCast(bx))) * 8;
                        const py = (my * c.v + @as(u32, @intCast(by))) * 8;
                        idct(&coef, c.plane[py * c.stride + px ..], c.stride);
                    }
                }
            }
            done += 1;
        }
    }

    const pixels = try gpa.alloc(u32, width * height);
    errdefer gpa.free(pixels);
    const rgb = comps.len == 3 and adobe_transform != null and adobe_transform.? == 0;
    var y: u32 = 0;
    while (y < height) : (y += 1) {
        var x: u32 = 0;
        while (x < width) : (x += 1) {
            var s: [3]f32 = undefined;
            for (comps, 0..) |c, i| s[i] = sample(c, x, y, hmax, vmax, width, height);
            if (comps.len == 1) {
                const g: u32 = @intFromFloat(s[0]);
                pixels[y * width + x] = g << 16 | g << 8 | g;
                continue;
            }
            const r, const g, const b = if (rgb) .{ s[0], s[1], s[2] } else .{
                s[0] + 1.402 * (s[2] - 128),
                s[0] - 0.344136 * (s[1] - 128) - 0.714136 * (s[2] - 128),
                s[0] + 1.772 * (s[1] - 128),
            };
            pixels[y * width + x] = channel(r) << 16 | channel(g) << 8 | channel(b);
        }
    }
    return .{ .w = width, .h = height, .pixels = pixels };
}

/// A plane's value at pixel (x, y). A plane stored at full resolution is read directly; a smaller
/// one (the color planes, usually half size) is blended between its four nearest samples, whose
/// centers sit between the pixels', so edges come out smooth rather than blocky.
fn sample(c: Component, x: u32, y: u32, hmax: u32, vmax: u32, width: u32, height: u32) f32 {
    if (c.h == hmax and c.v == vmax) return @floatFromInt(c.plane[y * c.stride + x]);
    const fx = (@as(f32, @floatFromInt(x)) + 0.5) * @as(f32, @floatFromInt(c.h)) / @as(f32, @floatFromInt(hmax)) - 0.5;
    const fy = (@as(f32, @floatFromInt(y)) + 0.5) * @as(f32, @floatFromInt(c.v)) / @as(f32, @floatFromInt(vmax)) - 0.5;
    // The plane's samples that belong to the picture (the rest is padding to whole blocks).
    const last_x: i32 = @intCast((width * c.h + hmax - 1) / hmax - 1);
    const last_y: i32 = @intCast((height * c.v + vmax - 1) / vmax - 1);
    const x0f = @floor(fx);
    const y0f = @floor(fy);
    const tx = fx - x0f;
    const ty = fy - y0f;
    const x0: i32 = std.math.clamp(@as(i32, @intFromFloat(x0f)), 0, last_x);
    const y0: i32 = std.math.clamp(@as(i32, @intFromFloat(y0f)), 0, last_y);
    const x1: i32 = @min(x0 + 1, last_x);
    const y1: i32 = @min(y0 + 1, last_y);
    const at = struct {
        fn f(p: Component, px: i32, py: i32) f32 {
            return @floatFromInt(p.plane[@as(usize, @intCast(py)) * p.stride + @as(usize, @intCast(px))]);
        }
    }.f;
    const top = at(c, x0, y0) * (1 - tx) + at(c, x1, y0) * tx;
    const bottom = at(c, x0, y1) * (1 - tx) + at(c, x1, y1) * tx;
    return top * (1 - ty) + bottom * ty;
}

fn channel(v: f32) u32 {
    return @intFromFloat(std.math.clamp(@round(v), 0, 255));
}
