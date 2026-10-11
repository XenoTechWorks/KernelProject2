//! DEFLATE (RFC 1951), the compression inside gzip, zip and .deb files, plus the CRC-32 they
//! check themselves with.
//!
//! `inflate` reads every kind of block: stored, fixed Huffman codes and dynamic ones. `deflate`
//! writes fixed-Huffman blocks, finding repeats with a hash chain: smaller output than storing
//! the bytes as they are, and every reader understands it.

const std = @import("std");

const gpa = std.heap.page_allocator;
pub const Error = error{ Corrupt, OutOfMemory };

// How a length code (257..285) and a distance code (0..29) turn into a number.
const length_base = [_]u16{ 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258 };
const length_extra = [_]u3{ 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0 };
const dist_base = [_]u16{ 1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577 };
const dist_extra = [_]u4{ 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13 };
/// The order the code-length code lengths come in, in a dynamic block's header.
const length_order = [_]u5{ 16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15 };

// ---- reading ----

const BitReader = struct {
    data: []const u8,
    pos: usize = 0,
    bits: u32 = 0,
    count: u5 = 0,

    fn bit(r: *BitReader) Error!u32 {
        if (r.count == 0) {
            if (r.pos >= r.data.len) return error.Corrupt;
            r.bits = r.data[r.pos];
            r.pos += 1;
            r.count = 8;
        }
        const b = r.bits & 1;
        r.bits >>= 1;
        r.count -= 1;
        return b;
    }

    /// `n` bits, least significant first.
    fn take(r: *BitReader, n: u5) Error!u32 {
        var out: u32 = 0;
        var i: u5 = 0;
        while (i < n) : (i += 1) out |= try r.bit() << i;
        return out;
    }

    fn alignByte(r: *BitReader) void {
        r.bits = 0;
        r.count = 0;
    }
};

/// A canonical Huffman code: how many codes there are of each length, and the symbols in order.
const Huffman = struct {
    count: [16]u16 = @splat(0),
    symbol: [288]u16 = @splat(0),

    fn build(lengths: []const u8) Huffman {
        var h = Huffman{};
        for (lengths) |l| h.count[l] += 1;
        h.count[0] = 0;
        var offsets: [16]u16 = @splat(0);
        var total: u16 = 0;
        for (1..16) |l| {
            offsets[l] = total;
            total += h.count[l];
        }
        for (lengths, 0..) |l, sym| {
            if (l != 0) {
                h.symbol[offsets[l]] = @intCast(sym);
                offsets[l] += 1;
            }
        }
        return h;
    }

    /// Reads one symbol, a bit at a time (the way puff.c does it: short and clearly right).
    fn decode(h: *const Huffman, r: *BitReader) Error!u16 {
        var code: i32 = 0;
        var first: i32 = 0;
        var index: i32 = 0;
        for (1..16) |len| {
            code |= @intCast(try r.bit());
            const count: i32 = h.count[len];
            if (code - first < count) return h.symbol[@intCast(index + (code - first))];
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        return error.Corrupt;
    }
};

fn fixedCodes() struct { Huffman, Huffman } {
    var lit: [288]u8 = undefined;
    for (0..144) |i| lit[i] = 8;
    for (144..256) |i| lit[i] = 9;
    for (256..280) |i| lit[i] = 7;
    for (280..288) |i| lit[i] = 8;
    var dist: [30]u8 = @splat(5);
    return .{ Huffman.build(&lit), Huffman.build(&dist) };
}

/// Decompresses a DEFLATE stream. The caller frees the result.
pub fn inflate(data: []const u8) Error![]u8 {
    var out = std.ArrayList(u8).empty;
    errdefer out.deinit(gpa);
    var r = BitReader{ .data = data };
    while (true) {
        const last = try r.bit();
        switch (try r.take(2)) {
            0 => { // stored: the bytes as they are
                r.alignByte();
                if (r.pos + 4 > r.data.len) return error.Corrupt;
                const len = @as(usize, r.data[r.pos]) | @as(usize, r.data[r.pos + 1]) << 8;
                r.pos += 4; // the length, then its complement
                if (r.pos + len > r.data.len) return error.Corrupt;
                try out.appendSlice(gpa, r.data[r.pos..][0..len]);
                r.pos += len;
            },
            1, 2 => |kind| {
                var lit: Huffman = undefined;
                var dist: Huffman = undefined;
                if (kind == 1) {
                    lit, dist = fixedCodes();
                } else {
                    lit, dist = try dynamicCodes(&r);
                }
                try block(&r, &lit, &dist, &out);
            },
            else => return error.Corrupt,
        }
        if (last == 1) break;
    }
    return out.toOwnedSlice(gpa);
}

/// The Huffman codes at the start of a dynamic block, themselves Huffman-coded.
fn dynamicCodes(r: *BitReader) Error!struct { Huffman, Huffman } {
    const nlit = try r.take(5) + 257;
    const ndist = try r.take(5) + 1;
    const ncode = try r.take(4) + 4;
    if (nlit > 286 or ndist > 30) return error.Corrupt;
    var code_lengths: [19]u8 = @splat(0);
    for (0..ncode) |i| code_lengths[length_order[i]] = @intCast(try r.take(3));
    const code_huff = Huffman.build(&code_lengths);

    var lengths: [320]u8 = @splat(0);
    var i: usize = 0;
    while (i < nlit + ndist) {
        const sym = try code_huff.decode(r);
        switch (sym) {
            0...15 => {
                lengths[i] = @intCast(sym);
                i += 1;
            },
            16 => { // repeat the last length 3..6 times
                if (i == 0) return error.Corrupt;
                const prev = lengths[i - 1];
                var n = 3 + try r.take(2);
                while (n > 0 and i < lengths.len) : (n -= 1) {
                    lengths[i] = prev;
                    i += 1;
                }
            },
            17, 18 => { // a run of zeros
                var n = if (sym == 17) 3 + try r.take(3) else 11 + try r.take(7);
                while (n > 0 and i < lengths.len) : (n -= 1) {
                    lengths[i] = 0;
                    i += 1;
                }
            },
            else => return error.Corrupt,
        }
    }
    if (i != nlit + ndist) return error.Corrupt;
    return .{ Huffman.build(lengths[0..nlit]), Huffman.build(lengths[nlit .. nlit + ndist]) };
}

/// The symbols of one block: literal bytes, and lengths paired with distances back into the output.
fn block(r: *BitReader, lit: *const Huffman, dist: *const Huffman, out: *std.ArrayList(u8)) Error!void {
    while (true) {
        const sym = try lit.decode(r);
        if (sym < 256) {
            try out.append(gpa, @intCast(sym));
        } else if (sym == 256) {
            return;
        } else {
            const li = sym - 257;
            if (li >= length_base.len) return error.Corrupt;
            const len = length_base[li] + try r.take(length_extra[li]);
            const di = try dist.decode(r);
            if (di >= dist_base.len) return error.Corrupt;
            const back = dist_base[di] + try r.take(dist_extra[di]);
            if (back == 0 or back > out.items.len) return error.Corrupt;
            // Copies may overlap (repeating a short run), so one byte at a time.
            var from = out.items.len - back;
            for (0..len) |_| {
                try out.append(gpa, out.items[from]);
                from += 1;
            }
        }
    }
}

// ---- writing ----

const BitWriter = struct {
    out: *std.ArrayList(u8),
    bits: u32 = 0,
    count: u5 = 0,

    fn put(w: *BitWriter, value: u32, n: u5) Error!void {
        w.bits |= value << w.count;
        w.count += n;
        while (w.count >= 8) {
            try w.out.append(gpa, @truncate(w.bits));
            w.bits >>= 8;
            w.count -= 8;
        }
    }

    /// A Huffman code, which goes out most significant bit first.
    fn putCode(w: *BitWriter, code: u32, n: u5) Error!void {
        var i = n;
        while (i > 0) {
            i -= 1;
            try w.put(code >> i & 1, 1);
        }
    }

    fn finish(w: *BitWriter) Error!void {
        if (w.count > 0) try w.out.append(gpa, @truncate(w.bits));
        w.bits = 0;
        w.count = 0;
    }
};

/// A literal or length symbol in the fixed code: (code, bits).
fn fixedLit(sym: u16) struct { u32, u5 } {
    return switch (sym) {
        0...143 => .{ 0x30 + @as(u32, sym), 8 },
        144...255 => .{ 0x190 + @as(u32, sym) - 144, 9 },
        256...279 => .{ @as(u32, sym) - 256, 7 },
        else => .{ 0xC0 + @as(u32, sym) - 280, 8 },
    };
}

fn lengthCode(len: u16) struct { u16, u16, u3 } {
    var i: usize = length_base.len - 1;
    while (length_base[i] > len) i -= 1;
    return .{ @intCast(257 + i), len - length_base[i], length_extra[i] };
}

fn distCode(d: u16) struct { u16, u16, u4 } {
    var i: usize = dist_base.len - 1;
    while (dist_base[i] > d) i -= 1;
    return .{ @intCast(i), d - dist_base[i], dist_extra[i] };
}

const window_size = 1 << 15;
const min_match = 3;
const max_match = 258;
const hash_bits = 15;

/// Compresses with fixed Huffman codes, finding repeats with a chained hash of three bytes.
pub fn deflate(data: []const u8) Error![]u8 {
    var out = std.ArrayList(u8).empty;
    errdefer out.deinit(gpa);
    var w = BitWriter{ .out = &out };
    try w.put(1, 1); // the only block, and it is the last
    try w.put(1, 2); // fixed Huffman codes

    // head[hash] is the latest position with that hash; prev[pos] is the one before it.
    const head = try gpa.alloc(i32, 1 << hash_bits);
    defer gpa.free(head);
    @memset(head, -1);
    const prev = try gpa.alloc(i32, @max(data.len, 1));
    defer gpa.free(prev);

    var at: usize = 0;
    while (at < data.len) {
        var best_len: usize = 0;
        var best_dist: usize = 0;
        if (at + min_match <= data.len) {
            const h = hash(data[at..][0..min_match]);
            var candidate = head[h];
            // Only so many tries: good enough, and keeps it quick.
            var tries: usize = 0;
            while (candidate >= 0 and tries < 32) : (tries += 1) {
                const c: usize = @intCast(candidate);
                const d = at - c;
                if (d > window_size) break;
                var l: usize = 0;
                const room = @min(max_match, data.len - at);
                while (l < room and data[c + l] == data[at + l]) l += 1;
                if (l > best_len) {
                    best_len = l;
                    best_dist = d;
                    if (l == max_match) break;
                }
                candidate = prev[c];
            }
            prev[at] = head[h];
            head[h] = @intCast(at);
        }
        if (best_len >= min_match) {
            const sym, const extra, const nbits = lengthCode(@intCast(best_len));
            const code, const bits = fixedLit(sym);
            try w.putCode(code, bits);
            try w.put(extra, nbits);
            const dsym, const dextra, const dbits = distCode(@intCast(best_dist));
            try w.putCode(dsym, 5);
            try w.put(dextra, dbits);
            // Add the bytes we skipped to the hash, so later matches can start inside them.
            for (at + 1..at + best_len) |i| {
                if (i + min_match <= data.len) {
                    const h = hash(data[i..][0..min_match]);
                    prev[i] = head[h];
                    head[h] = @intCast(i);
                }
            }
            at += best_len;
        } else {
            const code, const bits = fixedLit(data[at]);
            try w.putCode(code, bits);
            at += 1;
        }
    }
    const code, const bits = fixedLit(256); // end of block
    try w.putCode(code, bits);
    try w.finish();
    return out.toOwnedSlice(gpa);
}

fn hash(three: *const [min_match]u8) usize {
    const v = @as(u32, three[0]) << 16 | @as(u32, three[1]) << 8 | three[2];
    return (v *% 2654435761) >> (32 - hash_bits);
}

// ---- CRC-32 ----

var crc_table: [256]u32 = undefined;
var crc_ready = false;

/// The CRC-32 gzip and zip use to check themselves.
pub fn crc32(data: []const u8) u32 {
    if (!crc_ready) {
        for (0..256) |i| {
            var c: u32 = @intCast(i);
            for (0..8) |_| c = if (c & 1 != 0) 0xEDB88320 ^ (c >> 1) else c >> 1;
            crc_table[i] = c;
        }
        crc_ready = true;
    }
    var c: u32 = 0xFFFFFFFF;
    for (data) |b| c = crc_table[(c ^ b) & 0xFF] ^ (c >> 8);
    return c ^ 0xFFFFFFFF;
}
