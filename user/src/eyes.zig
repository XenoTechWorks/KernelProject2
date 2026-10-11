//! eyes: two eyes that follow the mouse, after X11's xeyes. Run it as `eyes &`.
//!
//! It draws on its window itself rather than using widgets, and asks the kernel where the pointer
//! is (it follows the pointer everywhere, not only over its own window).

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const bg = 0x22262e;
const white = 0xf2f4f8;
const outline = 0x11141a;
const pupil = 0x11141a;

fn drawEye(c: *gui.Canvas, cx: f32, cy: f32, rx: f32, ry: f32, to_x: f32, to_y: f32) void {
    const edge = @max(2, @min(rx, ry) * 0.12);
    c.ellipse(cx, cy, rx, ry, outline);
    c.ellipse(cx, cy, rx - edge, ry - edge, white);
    // How far the pupil can wander, and where it wants to go.
    const pr = @min(rx, ry) * 0.32;
    const room_x = rx - edge - pr - 2;
    const room_y = ry - edge - pr - 2;
    var dx = (to_x - cx) / @max(room_x, 1);
    var dy = (to_y - cy) / @max(room_y, 1);
    const dist = @sqrt(dx * dx + dy * dy);
    if (dist > 1) {
        dx /= dist;
        dy /= dist;
    }
    c.circle(cx + dx * room_x, cy + dy * room_y, pr, pupil);
}

pub fn main(_: rt.Args) u8 {
    var window = gui.Window.open("Eyes", 220, 150) orelse {
        rt.err.write("eyes: can't open a window (no /dev/window?)\n");
        return 1;
    };
    var last = [2]i32{ std.math.maxInt(i32), 0 };
    while (true) {
        const at = window.pointer();
        const c = &window.canvas;
        // Only redraw when the pointer has actually moved.
        if (at.x != last[0] or at.y != last[1]) {
            last = .{ at.x, at.y };
            const w: f32 = @floatFromInt(c.w);
            const h: f32 = @floatFromInt(c.h);
            const rx = w / 4.4;
            const ry = h / 2.3;
            c.fill(bg);
            drawEye(c, w / 2 - rx - 4, h / 2, rx, ry, @floatFromInt(at.x), @floatFromInt(at.y));
            drawEye(c, w / 2 + rx + 4, h / 2, rx, ry, @floatFromInt(at.x), @floatFromInt(at.y));
            window.present();
        }
        // 20 times a second is smooth enough and leaves the machine alone.
        const event = window.next(50) orelse break; // closed with ×
        switch (event) {
            .key => |k| if (k.code == 'q' or k.code == 27) break,
            .resize => last[0] = std.math.maxInt(i32), // draw again at the new size
            else => {},
        }
    }
    window.close();
    return 0;
}
