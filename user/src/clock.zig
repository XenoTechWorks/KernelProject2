//! clock: the time, big, with the date and how long the system has been up. A 24-hour checkbox
//! switches the format. Run it as `clock &`.

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

var time_label: *gui.Widget = undefined;
var date_label: *gui.Widget = undefined;
var uptime_label: *gui.Widget = undefined;
var hours24: *gui.Widget = undefined;

const weekdays = [_][]const u8{ "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday" };
const months = [_][]const u8{ "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December" };

fn seconds(clock: usize) u64 {
    var ts: extern struct { sec: i64, nsec: i64 } = .{ .sec = 0, .nsec = 0 };
    _ = sys.syscall2(sys.nr.clock_gettime, clock, @intFromPtr(&ts));
    return @intCast(@max(ts.sec, 0));
}

fn update(_: *gui.App, _: ?*anyopaque) void {
    const now = seconds(0); // CLOCK_REALTIME (UTC)
    const days = now / 86400;
    const hour = now % 86400 / 3600;
    const minute = now % 3600 / 60;
    const second = now % 60;
    var buf: [64]u8 = undefined;
    if (hours24.kind.check.on) {
        gui.setText(time_label, std.fmt.bufPrint(&buf, "{d:0>2}:{d:0>2}:{d:0>2}", .{ hour, minute, second }) catch "");
    } else {
        const h12 = if (hour % 12 == 0) 12 else hour % 12;
        gui.setText(time_label, std.fmt.bufPrint(&buf, "{d}:{d:0>2}:{d:0>2} {s}", .{ h12, minute, second, if (hour < 12) "AM" else "PM" }) catch "");
    }
    const d = civil(days);
    gui.setText(date_label, std.fmt.bufPrint(&buf, "{s}, {d} {s} {d} (UTC)", .{ weekdays[(days + 4) % 7], d.day, months[d.month - 1], d.year }) catch "");
    const up = seconds(1); // CLOCK_MONOTONIC: since boot
    const text = if (up >= 3600)
        std.fmt.bufPrint(&buf, "Up for {d} h {d} min", .{ up / 3600, up / 60 % 60 })
    else
        std.fmt.bufPrint(&buf, "Up for {d} min {d} s", .{ up / 60, up % 60 });
    gui.setText(uptime_label, text catch "");
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's civil_from_days).
fn civil(days_since_epoch: u64) struct { year: u64, month: u64, day: u64 } {
    const z = days_since_epoch + 719468;
    const era = z / 146097;
    const doe = z - era * 146097;
    const yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    const doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    const mp = (5 * doy + 2) / 153;
    const day = doy - (153 * mp + 2) / 5 + 1;
    const month = if (mp < 10) mp + 3 else mp - 9;
    return .{ .year = yoe + era * 400 + @intFromBool(month <= 2), .month = month, .day = day };
}

pub fn main(_: rt.Args) u8 {
    const app = gui.App.init("Clock") orelse {
        rt.err.write("clock: can't start (no /dev/window, or no font in /usr/share/fonts)\n");
        return 1;
    };
    time_label = app.label("00:00:00", .{ .size = 52, .alignment = .center });
    date_label = app.label("", .{ .alignment = .center });
    uptime_label = app.label("", .{ .dim = true, .alignment = .center });
    hours24 = app.check("24-hour time", true, .{ .f = update });
    hours24.alignment = .center;
    update(app, null);
    app.every(1000, .{ .f = update });
    const root = app.vbox(.{ .padding = 24, .spacing = 8 }, &.{
        app.spacer(), time_label, date_label, uptime_label, app.spacer(), hours24,
    });
    return app.run(root, 420, 0);
}
