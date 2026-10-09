//! textclock: a small text window (see lib/ui.zig) with the time, the date and the uptime.
//! Run it as `textclock &` to keep using the shell (`clock` is the graphical one). Click the
//! button (or press t) to switch between 24-hour and 12-hour time; q or the window's × closes it.

const std = @import("std");
const rt = @import("lib/rt.zig");
const ui = @import("lib/ui.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const cols = 26;
const rows = 6;

const Timespec = extern struct { sec: i64, nsec: i64 };

fn now(clock: usize) Timespec {
    var ts: Timespec = .{ .sec = 0, .nsec = 0 };
    _ = sys.syscall2(sys.nr.clock_gettime, clock, @intFromPtr(&ts));
    return ts;
}

/// Writes `s` centered on row y.
fn centered(win: *ui.Window, y: usize, s: []const u8) void {
    win.text((cols -| s.len) / 2, y, s);
}

fn draw(win: *ui.Window, twelve_hour: bool) ui.Rect {
    const t = now(0); // CLOCK_REALTIME (UTC)
    const secs: u64 = @intCast(@max(t.sec, 0));
    const day_secs = secs % 86400;
    const days = secs / 86400;
    const ymd = civil(days);
    const hour = day_secs / 3600;
    const minute = day_secs / 60 % 60;
    const second = day_secs % 60;

    var buf: [64]u8 = undefined;
    win.clear();
    win.color(.cyan, true);
    if (twelve_hour) {
        const h12 = if (hour % 12 == 0) 12 else hour % 12;
        centered(win, 1, std.fmt.bufPrint(&buf, "{d}:{d:0>2}:{d:0>2} {s}", .{ h12, minute, second, if (hour < 12) "AM" else "PM" }) catch "");
    } else {
        centered(win, 1, std.fmt.bufPrint(&buf, "{d:0>2}:{d:0>2}:{d:0>2}", .{ hour, minute, second }) catch "");
    }
    win.color(.white, false);
    const weekday = weekdays[(days + 4) % 7]; // 1 January 1970 was a Thursday
    centered(win, 2, std.fmt.bufPrint(&buf, "{s} {d} {s} {d} UTC", .{ weekday, ymd.day, months[ymd.month - 1], ymd.year }) catch "");

    const up: u64 = @intCast(@max(now(1).sec, 0)); // CLOCK_MONOTONIC: time since boot
    win.color(.gray, false);
    const uptime = if (up >= 3600)
        std.fmt.bufPrint(&buf, "up {d}h {d:0>2}m", .{ up / 3600, up / 60 % 60 }) catch ""
    else
        std.fmt.bufPrint(&buf, "up {d}m {d:0>2}s", .{ up / 60, up % 60 }) catch "";
    centered(win, 3, uptime);

    const toggle = win.button(2, 5, if (twelve_hour) "24h" else "12h");
    win.color(.gray, false);
    win.text(cols - 10, 5, "q closes");
    return toggle;
}

const weekdays = [_][]const u8{ "Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat" };
const months = [_][]const u8{ "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec" };

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
    var win = ui.Window.open("textclock", cols, rows) orelse {
        rt.err.write("textclock: can't open a window (no /dev/window?)\n");
        return 1;
    };
    var twelve_hour = false;
    var toggle = draw(&win, twelve_hour);
    while (true) {
        // Wake up at the start of the next second.
        const ms: u32 = @intCast(1000 - @divTrunc(now(0).nsec, 1_000_000));
        const event = win.next(ms) orelse break; // closed with ×
        switch (event) {
            .key => |k| switch (k) {
                'q', 3 => break,
                't' => twelve_hour = !twelve_hour,
                else => {},
            },
            .click => |c| if (toggle.contains(c.x, c.y)) {
                twelve_hour = !twelve_hour;
            },
            else => {},
        }
        toggle = draw(&win, twelve_hour);
    }
    win.close();
    return 0;
}
