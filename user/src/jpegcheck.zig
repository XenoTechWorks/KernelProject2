//! jpegcheck: decodes JPEG files with lib/jpeg.zig and compares each with a reference decode (a
//! binary PPM made by another decoder), printing the average and largest difference per channel.
//! A build-machine tool for testing the decoder (it runs on Linux too), not part of the system:
//!
//!     jpegcheck a.jpg a.ppm [b.jpg b.ppm ...]

const std = @import("std");
const rt = @import("lib/rt.zig");
const file = @import("lib/file.zig");
const jpeg = @import("lib/jpeg.zig");
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

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
        var img = jpeg.decode(data) catch |e| {
            rt.out.print("{s}: {s}\n", .{ name, @errorName(e) });
            failed = true;
            continue;
        };
        defer img.deinit();
        const ref = file.read(args.argv[i + 1]) catch {
            rt.out.print("{s}: no reference\n", .{name});
            failed = true;
            continue;
        };
        // The reference: "P6\n<w> <h>\n255\n" then RGB.
        var lines = std.mem.splitScalar(u8, ref, '\n');
        _ = lines.next();
        var dims = std.mem.splitScalar(u8, lines.next() orelse "", ' ');
        const w = std.fmt.parseInt(u32, dims.next() orelse "0", 10) catch 0;
        const h = std.fmt.parseInt(u32, dims.next() orelse "0", 10) catch 0;
        _ = lines.next();
        const header = ref.len - lines.rest().len;
        if (w != img.w or h != img.h or ref.len < header + w * h * 3) {
            rt.out.print("{s}: size {d}x{d}, reference {d}x{d}\n", .{ name, img.w, img.h, w, h });
            failed = true;
            continue;
        }
        var total: u64 = 0;
        var worst: u32 = 0;
        for (img.pixels, 0..) |p, k| {
            const want = ref[header + k * 3 ..][0..3];
            const got = [3]u8{ @truncate(p >> 16), @truncate(p >> 8), @truncate(p) };
            for (0..3) |c| {
                const d = @abs(@as(i32, got[c]) - @as(i32, want[c]));
                total += d;
                worst = @max(worst, d);
            }
        }
        const mean = @as(f64, @floatFromInt(total)) / @as(f64, @floatFromInt(img.pixels.len * 3));
        // Decoders differ a little in rounding and in how they scale up the color planes.
        const ok = mean < 2.0;
        if (!ok) failed = true;
        rt.out.print("{s} {s}: {d}x{d}, average difference {d:.2}, largest {d}\n", .{ if (ok) "ok  " else "FAIL", name, img.w, img.h, mean, worst });
    }
    return if (failed) 1 else 0;
}
