//! imagecheck: decodes pictures with lib/jpeg.zig (.jpg) or lib/png.zig (.png) and compares each
//! with a reference decode made by another decoder (a binary PPM, P6, or PAM with alpha, P7),
//! printing the average and largest difference per channel. A build-machine tool for testing the
//! decoders (it runs on Linux too), not part of the system:
//!
//!     imagecheck a.jpg a.ppm b.png b.pam ...

const std = @import("std");
const rt = @import("lib/rt.zig");
const file = @import("lib/file.zig");
const jpeg = @import("lib/jpeg.zig");
const png = @import("lib/png.zig");
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const Reference = struct { w: u32, h: u32, channels: u32, data: []const u8 };

/// A PPM (P6, "P6 w h 255") or PAM (P7, "WIDTH w", "HEIGHT h", "DEPTH 4" ... "ENDHDR") file.
fn parseReference(ref: []const u8) ?Reference {
    if (std.mem.startsWith(u8, ref, "P7")) {
        const end = std.mem.indexOf(u8, ref, "ENDHDR\n") orelse return null;
        var r = Reference{ .w = 0, .h = 0, .channels = 4, .data = ref[end + 7 ..] };
        var lines = std.mem.splitScalar(u8, ref[0..end], '\n');
        while (lines.next()) |line| {
            var words = std.mem.splitScalar(u8, line, ' ');
            const key = words.next() orelse continue;
            const value = std.fmt.parseInt(u32, words.next() orelse "", 10) catch continue;
            if (std.mem.eql(u8, key, "WIDTH")) r.w = value;
            if (std.mem.eql(u8, key, "HEIGHT")) r.h = value;
            if (std.mem.eql(u8, key, "DEPTH")) r.channels = value;
        }
        return r;
    }
    var lines = std.mem.splitScalar(u8, ref, '\n');
    _ = lines.next();
    var dims = std.mem.splitScalar(u8, lines.next() orelse "", ' ');
    const w = std.fmt.parseInt(u32, dims.next() orelse "0", 10) catch return null;
    const h = std.fmt.parseInt(u32, dims.next() orelse "0", 10) catch return null;
    _ = lines.next();
    return .{ .w = w, .h = h, .channels = 3, .data = lines.rest() };
}

pub fn main(args: rt.Args) u8 {
    var failed = false;
    var i: usize = 1;
    while (i + 1 < args.argv.len) : (i += 2) {
        const name = args.get(i).?;
        const data = file.read(args.argv[i]) catch {
            rt.out.print("{s}: can't read\n", .{name});
            failed = true;
            continue;
        };
        // Both decoders give 0xAARRGGBB-style pixels (JPEG: no alpha, 0).
        const decoded: struct { w: u32, h: u32, pixels: []u32, alpha: bool } = if (std.mem.endsWith(u8, name, ".png")) blk: {
            const img = png.decode(data) catch |e| {
                rt.out.print("FAIL {s}: {s}\n", .{ name, @errorName(e) });
                failed = true;
                continue;
            };
            break :blk .{ .w = img.w, .h = img.h, .pixels = img.pixels, .alpha = true };
        } else blk: {
            const img = jpeg.decode(data) catch |e| {
                rt.out.print("FAIL {s}: {s}\n", .{ name, @errorName(e) });
                failed = true;
                continue;
            };
            break :blk .{ .w = img.w, .h = img.h, .pixels = img.pixels, .alpha = false };
        };
        defer std.heap.page_allocator.free(decoded.pixels);
        const ref_data = file.read(args.argv[i + 1]) catch {
            rt.out.print("{s}: no reference\n", .{name});
            failed = true;
            continue;
        };
        const ref = parseReference(ref_data) orelse {
            rt.out.print("{s}: bad reference\n", .{name});
            failed = true;
            continue;
        };
        if (ref.w != decoded.w or ref.h != decoded.h or ref.data.len < ref.w * ref.h * ref.channels) {
            rt.out.print("FAIL {s}: size {d}x{d}, reference {d}x{d}\n", .{ name, decoded.w, decoded.h, ref.w, ref.h });
            failed = true;
            continue;
        }
        var total: u64 = 0;
        var counted: u64 = 0;
        var worst: u32 = 0;
        for (decoded.pixels, 0..) |p, k| {
            const want = ref.data[k * ref.channels ..][0..ref.channels];
            const got = [4]u8{ @truncate(p >> 16), @truncate(p >> 8), @truncate(p), @truncate(p >> 24) };
            // Where both say fully transparent, the color doesn't matter.
            const invisible = decoded.alpha and ref.channels == 4 and got[3] == 0 and want[3] == 0;
            const n: usize = if (decoded.alpha and ref.channels == 4) 4 else 3;
            for (0..n) |c| {
                if (invisible and c < 3) continue;
                const d = @abs(@as(i32, got[c]) - @as(i32, want[c]));
                total += d;
                counted += 1;
                worst = @max(worst, d);
            }
        }
        const mean = @as(f64, @floatFromInt(total)) / @as(f64, @floatFromInt(@max(counted, 1)));
        // JPEG decoders differ a little in rounding and in how they scale up the color planes;
        // PNG is lossless, so it must match exactly.
        const ok = if (decoded.alpha) worst == 0 else mean < 2.0;
        if (!ok) failed = true;
        rt.out.print("{s} {s}: {d}x{d}, average difference {d:.2}, largest {d}\n", .{ if (ok) "ok  " else "FAIL", name, decoded.w, decoded.h, mean, worst });
    }
    return if (failed) 1 else 0;
}
