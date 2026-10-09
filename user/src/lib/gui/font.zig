//! TrueType fonts: reads the glyph outlines in a .ttf file and draws them as smooth (anti-aliased)
//! coverage maps, cached per glyph and size.
//!
//! Only what drawing text needs: the character map (cmap formats 4 and 12), metrics (head, hhea,
//! hmtx) and outlines (glyf/loca, simple and composite glyphs). A variable font draws its default
//! instance. The rasterizer accumulates the signed area each outline edge covers in every pixel
//! (the approach of font-rs), which gives exact coverage without supersampling.

const std = @import("std");
const sys = @import("../sys.zig");

pub const Glyph = struct {
    /// Coverage, 0..255, w x h, row by row.
    alpha: []const u8,
    w: u32,
    h: u32,
    /// Where the bitmap's top-left corner goes, from the pen position on the baseline (up is
    /// negative y on the screen, so the top is usually above the baseline: top > 0).
    left: i32,
    top: i32,
    /// How far the pen moves, in pixels.
    advance: f32,
};

const cache_size = 2048;

const Entry = struct { glyph: u16, size: u16, value: Glyph };

pub const Font = struct {
    data: []const u8,
    units_per_em: f32,
    ascent: f32,
    descent: f32,
    line_gap: f32,
    num_hmetrics: u16,
    long_loca: bool,
    glyf: usize,
    loca: usize,
    hmtx: usize,
    cmap: usize,
    cmap_format: u16,
    cache: [cache_size]?Entry = @splat(null),
    arena: std.heap.ArenaAllocator,

    /// Reads a font file. The font lives as long as the program.
    pub fn load(path: [*:0]const u8) ?*Font {
        var st: sys.Stat = undefined;
        if (sys.stat(path, &st) != .ok) return null;
        const fd: i32 = switch (sys.open(path, sys.O_RDONLY, 0)) {
            .ok => |fd| @intCast(fd),
            .err => return null,
        };
        defer sys.close(fd);
        const data = std.heap.page_allocator.alloc(u8, @intCast(st.size)) catch return null;
        var done: usize = 0;
        while (done < data.len) {
            switch (sys.read(fd, data[done..])) {
                .ok => |n| if (n == 0) break else {
                    done += n;
                },
                .err => return null,
            }
        }
        const font = std.heap.page_allocator.create(Font) catch return null;
        font.* = parse(data[0..done]) orelse return null;
        return font;
    }

    pub fn parse(data: []const u8) ?Font {
        if (data.len < 12) return null;
        var tables: struct { head: ?usize = null, hhea: ?usize = null, hmtx: ?usize = null, loca: ?usize = null, glyf: ?usize = null, cmap: ?usize = null } = .{};
        const count = be(u16, data, 4);
        for (0..count) |i| {
            const rec = 12 + 16 * i;
            if (rec + 16 > data.len) return null;
            const offset: usize = be(u32, data, rec + 8);
            const tag = data[rec..][0..4];
            if (std.mem.eql(u8, tag, "head")) tables.head = offset;
            if (std.mem.eql(u8, tag, "hhea")) tables.hhea = offset;
            if (std.mem.eql(u8, tag, "hmtx")) tables.hmtx = offset;
            if (std.mem.eql(u8, tag, "loca")) tables.loca = offset;
            if (std.mem.eql(u8, tag, "glyf")) tables.glyf = offset;
            if (std.mem.eql(u8, tag, "cmap")) tables.cmap = offset;
        }
        const head = tables.head orelse return null;
        const hhea = tables.hhea orelse return null;
        const cmap = tables.cmap orelse return null;
        var font = Font{
            .data = data,
            .units_per_em = @floatFromInt(be(u16, data, head + 18)),
            .long_loca = be(i16, data, head + 50) != 0,
            .ascent = @floatFromInt(be(i16, data, hhea + 4)),
            .descent = @floatFromInt(be(i16, data, hhea + 6)),
            .line_gap = @floatFromInt(be(i16, data, hhea + 8)),
            .num_hmetrics = be(u16, data, hhea + 34),
            .hmtx = tables.hmtx orelse return null,
            .loca = tables.loca orelse return null,
            .glyf = tables.glyf orelse return null,
            .cmap = 0,
            .cmap_format = 0,
            .arena = std.heap.ArenaAllocator.init(std.heap.page_allocator),
        };
        // Pick a Unicode character map: full Unicode (format 12) if there is one, else the basic one.
        var best: u8 = 0;
        for (0..be(u16, data, cmap + 2)) |i| {
            const rec = cmap + 4 + 8 * i;
            const platform = be(u16, data, rec);
            const encoding = be(u16, data, rec + 2);
            const sub = cmap + be(u32, data, rec + 4);
            const format = be(u16, data, sub);
            const score: u8 = if (format == 12 and (platform == 0 or (platform == 3 and encoding == 10))) 2 else if (format == 4 and (platform == 0 or (platform == 3 and encoding == 1))) 1 else 0;
            if (score > best) {
                best = score;
                font.cmap = sub;
                font.cmap_format = format;
            }
        }
        if (best == 0) return null;
        return font;
    }

    /// The glyph for a character (0, the "missing" glyph, if the font doesn't have it).
    pub fn glyphIndex(self: *const Font, c: u21) u16 {
        const d = self.data;
        const t = self.cmap;
        if (self.cmap_format == 12) {
            const groups = be(u32, d, t + 12);
            var lo: usize = 0;
            var hi: usize = groups;
            while (lo < hi) {
                const mid = (lo + hi) / 2;
                const g = t + 16 + 12 * mid;
                const start = be(u32, d, g);
                const end = be(u32, d, g + 4);
                if (c < start) {
                    hi = mid;
                } else if (c > end) {
                    lo = mid + 1;
                } else {
                    return @truncate(be(u32, d, g + 8) + (c - start));
                }
            }
            return 0;
        }
        if (c > 0xFFFF) return 0;
        const segs2: usize = be(u16, d, t + 6);
        var i: usize = 0;
        while (i < segs2) : (i += 2) {
            const end = be(u16, d, t + 14 + i);
            if (end < c) continue;
            const start = be(u16, d, t + 16 + segs2 + i);
            if (start > c) return 0;
            const delta = be(u16, d, t + 16 + 2 * segs2 + i);
            const range_at = t + 16 + 3 * segs2 + i;
            const range = be(u16, d, range_at);
            if (range == 0) return @truncate(@as(u32, c) +% delta);
            const g = be(u16, d, range_at + range + 2 * (c - start));
            return if (g == 0) 0 else g +% delta;
        }
        return 0;
    }

    /// How far the pen moves after a glyph, in pixels at this size.
    pub fn advance(self: *const Font, index: u16, size: f32) f32 {
        const i = @min(index, self.num_hmetrics - 1);
        return @as(f32, @floatFromInt(be(u16, self.data, self.hmtx + 4 * @as(usize, i)))) * size / self.units_per_em;
    }

    /// Distance from the top of a line to the baseline, and the whole line's height, in pixels.
    pub fn ascender(self: *const Font, size: f32) f32 {
        return self.ascent * size / self.units_per_em;
    }
    pub fn lineHeight(self: *const Font, size: f32) f32 {
        return (self.ascent - self.descent + self.line_gap) * size / self.units_per_em;
    }

    /// A glyph drawn at a size (in pixels per em), from the cache when it's been drawn before.
    pub fn glyph(self: *Font, index: u16, size: u16) *const Glyph {
        var slot = (@as(usize, index) *% 2654435761 +% size *% 40503) % cache_size;
        for (0..cache_size) |_| {
            if (self.cache[slot]) |*e| {
                if (e.glyph == index and e.size == size) return &e.value;
                slot = (slot + 1) % cache_size;
            } else {
                self.cache[slot] = .{ .glyph = index, .size = size, .value = self.render(index, @floatFromInt(size)) };
                return &self.cache[slot].?.value;
            }
        }
        // The cache is full (thousands of glyphs and sizes): draw it again each time.
        const g = self.arena.allocator().create(Glyph) catch unreachable;
        g.* = self.render(index, @floatFromInt(size));
        return g;
    }

    fn glyphData(self: *const Font, index: u16) ?usize {
        const d = self.data;
        const i: usize = index;
        const start: usize, const end: usize = if (self.long_loca)
            .{ be(u32, d, self.loca + 4 * i), be(u32, d, self.loca + 4 * i + 4) }
        else
            .{ @as(usize, be(u16, d, self.loca + 2 * i)) * 2, @as(usize, be(u16, d, self.loca + 2 * i + 2)) * 2 };
        if (end <= start) return null; // an empty glyph, like space
        return self.glyf + start;
    }

    fn render(self: *Font, index: u16, size: f32) Glyph {
        const adv = self.advance(index, size);
        const g = self.glyphData(index) orelse return .{ .alpha = &.{}, .w = 0, .h = 0, .left = 0, .top = 0, .advance = adv };
        const d = self.data;
        const scale = size / self.units_per_em;
        const x_min: f32 = @floatFromInt(be(i16, d, g + 2));
        const y_min: f32 = @floatFromInt(be(i16, d, g + 4));
        const x_max: f32 = @floatFromInt(be(i16, d, g + 6));
        const y_max: f32 = @floatFromInt(be(i16, d, g + 8));
        const left: i32 = @intFromFloat(@floor(x_min * scale));
        const top: i32 = @intFromFloat(@ceil(y_max * scale));
        const w: u32 = @intCast(@max(1, @as(i32, @intFromFloat(@ceil(x_max * scale))) - left + 1));
        const h: u32 = @intCast(@max(1, top - @as(i32, @intFromFloat(@floor(y_min * scale))) + 1));

        const allocator = self.arena.allocator();
        var raster = Raster{ .w = w, .h = h, .acc = allocator.alloc(f32, w * h + w + 4) catch unreachable };
        defer allocator.free(raster.acc);
        @memset(raster.acc, 0);
        // Font units to pixels: y flips, and the bitmap starts at (left, top).
        const xf = Transform{ .a = scale, .d = -scale, .e = -@as(f32, @floatFromInt(left)), .f = @floatFromInt(top) };
        self.outline(index, xf, &raster, 0);

        const alpha = allocator.alloc(u8, w * h) catch unreachable;
        var sum: f32 = 0;
        for (alpha, 0..) |*a, i| {
            sum += raster.acc[i];
            a.* = @intFromFloat(@min(@abs(sum), 1.0) * 255.0 + 0.5);
        }
        return .{ .alpha = alpha, .w = w, .h = h, .left = left, .top = top, .advance = adv };
    }

    /// Draws a glyph's outline into the raster (composite glyphs draw their parts).
    fn outline(self: *const Font, index: u16, xf: Transform, r: *Raster, depth: u8) void {
        const g = self.glyphData(index) orelse return;
        const d = self.data;
        const contours = be(i16, d, g);
        if (contours >= 0) return simpleOutline(d, g, @intCast(contours), xf, r);
        if (depth > 4) return;
        // A composite: other glyphs, each moved (and maybe scaled).
        var p = g + 10;
        while (true) {
            const flags = be(u16, d, p);
            const part = be(u16, d, p + 2);
            p += 4;
            var dx: f32 = 0;
            var dy: f32 = 0;
            if (flags & 1 != 0) { // arguments are 16-bit
                dx = @floatFromInt(be(i16, d, p));
                dy = @floatFromInt(be(i16, d, p + 2));
                p += 4;
            } else {
                dx = @floatFromInt(@as(i8, @bitCast(d[p])));
                dy = @floatFromInt(@as(i8, @bitCast(d[p + 1])));
                p += 2;
            }
            var m = Transform{ .a = 1, .d = 1, .e = dx, .f = dy };
            if (flags & 8 != 0) {
                m.a = f2dot14(d, p);
                m.d = m.a;
                p += 2;
            } else if (flags & 0x40 != 0) {
                m.a = f2dot14(d, p);
                m.d = f2dot14(d, p + 2);
                p += 4;
            } else if (flags & 0x80 != 0) {
                m.a = f2dot14(d, p);
                m.b = f2dot14(d, p + 2);
                m.c = f2dot14(d, p + 4);
                m.d = f2dot14(d, p + 6);
                p += 8;
            }
            if (flags & 2 != 0) self.outline(part, xf.after(m), r, depth + 1); // (point matching isn't supported)
            if (flags & 0x20 == 0) break;
        }
    }
};

fn simpleOutline(d: []const u8, g: usize, contours: usize, xf: Transform, r: *Raster) void {
    if (contours == 0) return;
    const ends = g + 10;
    const points: usize = @as(usize, be(u16, d, ends + 2 * (contours - 1))) + 1;
    const max_points = 4096;
    if (points > max_points) return;
    var flags: [max_points]u8 = undefined;
    var xs: [max_points]f32 = undefined;
    var ys: [max_points]f32 = undefined;

    var p = ends + 2 * contours;
    p += 2 + be(u16, d, p); // skip the hinting instructions
    var i: usize = 0;
    while (i < points) {
        const f = d[p];
        p += 1;
        flags[i] = f;
        i += 1;
        if (f & 8 != 0) { // repeated
            var n = d[p];
            p += 1;
            while (n > 0 and i < points) : (n -= 1) {
                flags[i] = f;
                i += 1;
            }
        }
    }
    // Coordinates are deltas: 1 byte (with a sign flag) or 2 bytes, or "same as before".
    inline for (.{ .{ &xs, 2, 16 }, .{ &ys, 4, 32 } }) |axis| {
        var v: i32 = 0;
        for (0..points) |k| {
            const f = flags[k];
            if (f & axis[1] != 0) {
                const delta: i32 = d[p];
                p += 1;
                v += if (f & axis[2] != 0) delta else -delta;
            } else if (f & axis[2] == 0) {
                v += be(i16, d, p);
                p += 2;
            }
            axis[0][k] = @floatFromInt(v);
        }
    }

    var start: usize = 0;
    for (0..contours) |c| {
        const end: usize = be(u16, d, ends + 2 * c);
        if (end < start or end >= points) return;
        contour(xs[start .. end + 1], ys[start .. end + 1], flags[start .. end + 1], xf, r);
        start = end + 1;
    }
}

/// One closed contour of on-curve points and quadratic control (off-curve) points. Two control
/// points in a row have an implied on-curve point halfway between them.
fn contour(xs: []const f32, ys: []const f32, flags: []const u8, xf: Transform, r: *Raster) void {
    const n = xs.len;
    const P = struct { x: f32, y: f32 };
    const at = struct {
        fn f(x: []const f32, y: []const f32, k: usize, t: Transform) P {
            return .{ .x = t.a * x[k] + t.c * y[k] + t.e, .y = t.b * x[k] + t.d * y[k] + t.f };
        }
    }.f;
    const mid = struct {
        fn f(a: P, b: P) P {
            return .{ .x = (a.x + b.x) / 2, .y = (a.y + b.y) / 2 };
        }
    }.f;
    // Start at an on-curve point (or between two control points if there's none).
    var first_on: ?usize = null;
    for (0..n) |k| {
        if (flags[k] & 1 != 0) {
            first_on = k;
            break;
        }
    }
    const s0 = first_on orelse 0;
    const start = if (first_on != null) at(xs, ys, s0, xf) else mid(at(xs, ys, n - 1, xf), at(xs, ys, 0, xf));
    var cur = start;
    var ctrl: ?P = if (first_on != null) null else at(xs, ys, 0, xf);
    // Visit the points after the start, wrapping around (back onto the start when it's a point).
    const count = if (first_on != null) n else n - 1;
    for (1..count + 1) |k| {
        const idx = (s0 + k) % n;
        const q = at(xs, ys, idx, xf);
        if (flags[idx] & 1 != 0) {
            if (ctrl) |c| r.quad(cur.x, cur.y, c.x, c.y, q.x, q.y) else r.line(cur.x, cur.y, q.x, q.y);
            cur = q;
            ctrl = null;
        } else if (ctrl) |c| {
            const m = mid(c, q);
            r.quad(cur.x, cur.y, c.x, c.y, m.x, m.y);
            cur = m;
            ctrl = q;
        } else {
            ctrl = q;
        }
    }
    if (ctrl) |c| r.quad(cur.x, cur.y, c.x, c.y, start.x, start.y);
}

/// x' = a x + c y + e, y' = b x + d y + f
const Transform = struct {
    a: f32 = 1,
    b: f32 = 0,
    c: f32 = 0,
    d: f32 = 1,
    e: f32 = 0,
    f: f32 = 0,

    /// This transform applied after `m`.
    fn after(t: Transform, m: Transform) Transform {
        return .{
            .a = t.a * m.a + t.c * m.b,
            .b = t.b * m.a + t.d * m.b,
            .c = t.a * m.c + t.c * m.d,
            .d = t.b * m.c + t.d * m.d,
            .e = t.a * m.e + t.c * m.f + t.e,
            .f = t.b * m.e + t.d * m.f + t.f,
        };
    }
};

/// Accumulates, for each pixel, the signed area the outline's edges cover; a running sum along each
/// row then gives the coverage.
const Raster = struct {
    w: u32,
    h: u32,
    acc: []f32,

    fn line(r: *Raster, x0_: f32, y0_: f32, x1_: f32, y1_: f32) void {
        if (@abs(y0_ - y1_) < 1e-6) return;
        const dir: f32 = if (y0_ < y1_) 1 else -1;
        const x0, const y0, const x1, const y1 = if (y0_ < y1_) .{ x0_, y0_, x1_, y1_ } else .{ x1_, y1_, x0_, y0_ };
        const dxdy = (x1 - x0) / (y1 - y0);
        var x = x0;
        if (y0 < 0) x -= y0 * dxdy;
        const w: usize = r.w;
        var y: usize = @intFromFloat(@max(y0, 0));
        const y_end: usize = @min(r.h, @as(usize, @intFromFloat(@max(@ceil(y1), 0))));
        while (y < y_end) : (y += 1) {
            const row = y * w;
            const fy: f32 = @floatFromInt(y);
            const dy = @min(fy + 1, y1) - @max(fy, y0);
            const xnext = x + dxdy * dy;
            const dd = dy * dir;
            const lo = @max(@min(x, xnext), 0);
            const hi = @max(@max(x, xnext), 0);
            const lo_floor = @floor(lo);
            const lo_i: usize = @intFromFloat(lo_floor);
            const hi_ceil = @ceil(hi);
            const hi_i: usize = @intFromFloat(hi_ceil);
            if (hi_i <= lo_i + 1) {
                // The edge stays within one pixel on this row.
                const xm = 0.5 * (x + xnext) - lo_floor;
                r.add(row + lo_i, dd - dd * xm);
                r.add(row + lo_i + 1, dd * xm);
            } else {
                const s = 1 / (hi - lo);
                const lo_f = lo - lo_floor;
                const a0 = 0.5 * s * (1 - lo_f) * (1 - lo_f);
                const hi_f = hi - hi_ceil + 1;
                const am = 0.5 * s * hi_f * hi_f;
                r.add(row + lo_i, dd * a0);
                if (hi_i == lo_i + 2) {
                    r.add(row + lo_i + 1, dd * (1 - a0 - am));
                } else {
                    const a1 = s * (1.5 - lo_f);
                    r.add(row + lo_i + 1, dd * (a1 - a0));
                    var xi = lo_i + 2;
                    while (xi < hi_i - 1) : (xi += 1) r.add(row + xi, dd * s);
                    const a2 = a1 + @as(f32, @floatFromInt(hi_i - lo_i - 3)) * s;
                    r.add(row + hi_i - 1, dd * (1 - a2 - am));
                }
                r.add(row + hi_i, dd * am);
            }
            x = xnext;
        }
    }

    fn add(r: *Raster, i: usize, v: f32) void {
        if (i < r.acc.len) r.acc[i] += v;
    }

    /// A quadratic curve, as enough straight pieces that nobody can tell.
    fn quad(r: *Raster, x0: f32, y0: f32, cx: f32, cy: f32, x1: f32, y1: f32) void {
        const devx = x0 - 2 * cx + x1;
        const devy = y0 - 2 * cy + y1;
        const devsq = devx * devx + devy * devy;
        if (devsq < 0.333) return r.line(x0, y0, x1, y1);
        const n: usize = 1 + @as(usize, @intFromFloat(@floor(@sqrt(@sqrt(3 * devsq)))));
        var px = x0;
        var py = y0;
        for (1..n) |k| {
            const t = @as(f32, @floatFromInt(k)) / @as(f32, @floatFromInt(n));
            const ax = x0 + (cx - x0) * t;
            const ay = y0 + (cy - y0) * t;
            const bx = cx + (x1 - cx) * t;
            const by = cy + (y1 - cy) * t;
            const nx = ax + (bx - ax) * t;
            const ny = ay + (by - ay) * t;
            r.line(px, py, nx, ny);
            px = nx;
            py = ny;
        }
        r.line(px, py, x1, y1);
    }
};

fn be(comptime T: type, data: []const u8, at: usize) T {
    if (at + @sizeOf(T) > data.len) return 0;
    return std.mem.readInt(T, data[at..][0..@sizeOf(T)], .big);
}

fn f2dot14(data: []const u8, at: usize) f32 {
    return @as(f32, @floatFromInt(be(i16, data, at))) / 16384.0;
}
