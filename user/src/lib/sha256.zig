//! SHA-256, and the "$5$" password hashing built on it (sha256crypt, what Linux keeps in
//! /etc/shadow). Written out so the hashes are the same ones glibc makes: a password set here
//! works on a Linux box and the other way round.

const std = @import("std");

pub const digest_len = 32;

const k = [_]u32{
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
};

pub const Sha256 = struct {
    state: [8]u32 = .{ 0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19 },
    buf: [64]u8 = undefined,
    held: usize = 0,
    total: u64 = 0,

    pub fn update(h: *Sha256, data: []const u8) void {
        h.total += data.len;
        var at: usize = 0;
        while (at < data.len) {
            const n = @min(64 - h.held, data.len - at);
            @memcpy(h.buf[h.held..][0..n], data[at..][0..n]);
            h.held += n;
            at += n;
            if (h.held == 64) {
                h.compress(&h.buf);
                h.held = 0;
            }
        }
    }

    pub fn final(h: *Sha256) [digest_len]u8 {
        const bits = h.total * 8;
        h.update(&.{0x80});
        while (h.held != 56) h.update(&.{0});
        var tail: [8]u8 = undefined;
        std.mem.writeInt(u64, &tail, bits, .big);
        // update() would count these, but it's about to end anyway.
        @memcpy(h.buf[56..64], &tail);
        h.compress(&h.buf);
        var out: [digest_len]u8 = undefined;
        for (h.state, 0..) |word, i| std.mem.writeInt(u32, out[4 * i ..][0..4], word, .big);
        return out;
    }

    fn compress(h: *Sha256, chunk: *const [64]u8) void {
        var w: [64]u32 = undefined;
        for (0..16) |i| w[i] = std.mem.readInt(u32, chunk[4 * i ..][0..4], .big);
        for (16..64) |i| {
            const s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
            const s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16] +% s0 +% w[i - 7] +% s1;
        }
        var v = h.state;
        for (0..64) |i| {
            const s1 = rotr(v[4], 6) ^ rotr(v[4], 11) ^ rotr(v[4], 25);
            const ch = (v[4] & v[5]) ^ (~v[4] & v[6]);
            const t1 = v[7] +% s1 +% ch +% k[i] +% w[i];
            const s0 = rotr(v[0], 2) ^ rotr(v[0], 13) ^ rotr(v[0], 22);
            const maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            const t2 = s0 +% maj;
            // Built as its own value first: writing straight into `v` would overwrite the parts
            // still being read.
            const next = [8]u32{ t1 +% t2, v[0], v[1], v[2], v[3] +% t1, v[4], v[5], v[6] };
            v = next;
        }
        for (&h.state, v) |*slot, add| slot.* +%= add;
    }
};

fn rotr(x: u32, n: u5) u32 {
    return std.math.rotr(u32, x, n);
}

pub fn hash(data: []const u8) [digest_len]u8 {
    var h = Sha256{};
    h.update(data);
    return h.final();
}

// ---- sha256crypt ----

const alphabet = "./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
pub const default_rounds: u32 = 5000;

/// The hash of a password with a salt, in the "$5$salt$..." form /etc/shadow uses. `out` needs
/// about 80 bytes. The steps are Ulrich Drepper's, which is what glibc does.
pub fn crypt(out: []u8, password: []const u8, salt_in: []const u8, rounds: u32) []const u8 {
    const salt = salt_in[0..@min(salt_in.len, 16)];

    // B is the password and salt folded together.
    var b_ctx = Sha256{};
    b_ctx.update(password);
    b_ctx.update(salt);
    b_ctx.update(password);
    const b = b_ctx.final();

    var a_ctx = Sha256{};
    a_ctx.update(password);
    a_ctx.update(salt);
    var left = password.len;
    while (left > digest_len) : (left -= digest_len) a_ctx.update(&b);
    a_ctx.update(b[0..left]);
    // Then one pass per bit of the password's length: B for a one, the password for a zero.
    var bits = password.len;
    while (bits > 0) : (bits >>= 1) {
        if (bits & 1 != 0) a_ctx.update(&b) else a_ctx.update(password);
    }
    const a = a_ctx.final();

    // P is the password repeated, S the salt repeated, each to its own length.
    var dp_ctx = Sha256{};
    for (0..password.len) |_| dp_ctx.update(password);
    const dp = dp_ctx.final();
    var p_buf: [256]u8 = undefined;
    const p = repeat(&p_buf, &dp, password.len);

    var ds_ctx = Sha256{};
    for (0..16 + @as(usize, a[0])) |_| ds_ctx.update(salt);
    const ds = ds_ctx.final();
    var s_buf: [64]u8 = undefined;
    const s = repeat(&s_buf, &ds, salt.len);

    // Thousands of rounds, each mixing in a different order: what makes guessing slow.
    var c = a;
    for (0..rounds) |i| {
        var ctx = Sha256{};
        if (i & 1 != 0) ctx.update(p) else ctx.update(&c);
        if (i % 3 != 0) ctx.update(s);
        if (i % 7 != 0) ctx.update(p);
        if (i & 1 != 0) ctx.update(&c) else ctx.update(p);
        c = ctx.final();
    }

    var at: usize = 0;
    at += (std.fmt.bufPrint(out[at..], "$5$", .{}) catch return out[0..0]).len;
    if (rounds != default_rounds) {
        at += (std.fmt.bufPrint(out[at..], "rounds={d}$", .{rounds}) catch return out[0..0]).len;
    }
    at += (std.fmt.bufPrint(out[at..], "{s}$", .{salt}) catch return out[0..0]).len;
    // The digest's bytes go out in this shuffled order, three at a time.
    const order = [_][3]usize{
        .{ 0, 10, 20 },  .{ 21, 1, 11 },  .{ 12, 22, 2 }, .{ 3, 13, 23 }, .{ 24, 4, 14 },
        .{ 15, 25, 5 },  .{ 6, 16, 26 },  .{ 27, 7, 17 }, .{ 18, 28, 8 }, .{ 9, 19, 29 },
    };
    for (order) |g| at += encode(out[at..], c[g[0]], c[g[1]], c[g[2]], 4);
    at += encode(out[at..], 0, c[31], c[30], 3);
    return out[0..at];
}

fn repeat(buf: []u8, digest: *const [digest_len]u8, want: usize) []const u8 {
    const n = @min(want, buf.len);
    for (0..n) |i| buf[i] = digest[i % digest_len];
    return buf[0..n];
}

/// Three bytes to four characters, least significant first (the crypt flavour of base64).
fn encode(out: []u8, b2: u8, b1: u8, b0: u8, n: usize) usize {
    var v = @as(u32, b2) << 16 | @as(u32, b1) << 8 | b0;
    var i: usize = 0;
    while (i < n and i < out.len) : (i += 1) {
        out[i] = alphabet[v & 0x3f];
        v >>= 6;
    }
    return i;
}

/// Checks a password against a stored "$5$salt$..." hash.
pub fn verify(password: []const u8, stored: []const u8) bool {
    if (!std.mem.startsWith(u8, stored, "$5$")) return false;
    var rest = stored[3..];
    var rounds = default_rounds;
    if (std.mem.startsWith(u8, rest, "rounds=")) {
        const end = std.mem.indexOfScalar(u8, rest, '$') orelse return false;
        rounds = std.fmt.parseInt(u32, rest[7..end], 10) catch return false;
        rest = rest[end + 1 ..];
    }
    const end = std.mem.indexOfScalar(u8, rest, '$') orelse return false;
    var buf: [128]u8 = undefined;
    const made = crypt(&buf, password, rest[0..end], rounds);
    // Compare every byte, so how long it takes says nothing about how much matched.
    if (made.len != stored.len) return false;
    var same: u8 = 0;
    for (made, stored) |x, y| same |= x ^ y;
    return same == 0;
}
