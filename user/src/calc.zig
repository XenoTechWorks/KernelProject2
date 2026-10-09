//! calc: a calculator. Click the buttons or type: digits, . + - * / % Enter (or =), Backspace,
//! Escape (or c) to clear. Run it as `calc &`.

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

var display: *gui.Widget = undefined;
var history: *gui.Widget = undefined;

/// What's being typed, the number before it, and the operation between them.
var typed: [32]u8 = undefined;
var typed_len: usize = 0;
var stored: ?f64 = null;
var op: ?u8 = null;
/// The display shows a result (typing a digit starts a new number).
var showing_result = false;
var failed = false;

fn current() f64 {
    return std.fmt.parseFloat(f64, typed[0..typed_len]) catch 0;
}

fn setTyped(s: []const u8) void {
    typed_len = @min(s.len, typed.len);
    @memcpy(typed[0..typed_len], s[0..typed_len]);
}

/// A number as short as it can be written (no "1.0000000002" noise from adding tenths).
fn format(buf: []u8, value: f64) []const u8 {
    if (std.math.isNan(value) or std.math.isInf(value)) return "Error";
    const rounded = @round(value * 1e10) / 1e10;
    if (@abs(rounded) < 1e15 and rounded == @trunc(rounded)) {
        return std.fmt.bufPrint(buf, "{d}", .{@as(i64, @intFromFloat(rounded))}) catch "Error";
    }
    return std.fmt.bufPrint(buf, "{d}", .{rounded}) catch "Error";
}

fn show() void {
    if (failed) {
        gui.setText(display, "Error");
    } else {
        gui.setText(display, if (typed_len == 0) "0" else typed[0..typed_len]);
    }
    var buf: [64]u8 = undefined;
    var num: [40]u8 = undefined;
    if (stored) |s| {
        gui.setText(history, std.fmt.bufPrint(&buf, "{s} {s}", .{ format(&num, s), symbol(op.?) }) catch "");
    } else {
        gui.setText(history, "");
    }
}

fn symbol(o: u8) []const u8 {
    return switch (o) {
        '+' => "+",
        '-' => "−",
        '*' => "×",
        '/' => "÷",
        else => "?",
    };
}

fn apply(a: f64, o: u8, b: f64) f64 {
    return switch (o) {
        '+' => a + b,
        '-' => a - b,
        '*' => a * b,
        '/' => if (b == 0) std.math.nan(f64) else a / b,
        else => b,
    };
}

fn press(key: u8) void {
    if (failed and key != 'c') return;
    switch (key) {
        '0'...'9', '.' => {
            if (showing_result) {
                typed_len = 0;
                showing_result = false;
            }
            if (key == '.' and std.mem.indexOfScalar(u8, typed[0..typed_len], '.') != null) return;
            if (typed_len == 1 and typed[0] == '0' and key != '.') typed_len = 0;
            if (typed_len == 0 and key == '.') {
                typed[0] = '0';
                typed_len = 1;
            }
            if (typed_len < 16) {
                typed[typed_len] = key;
                typed_len += 1;
            }
        },
        '+', '-', '*', '/' => {
            var buf: [40]u8 = undefined;
            if (stored != null and op != null and !showing_result) {
                const result = apply(stored.?, op.?, current());
                failed = std.math.isNan(result) or std.math.isInf(result);
                stored = result;
                setTyped(format(&buf, result));
            } else {
                stored = current();
            }
            op = key;
            showing_result = true;
        },
        '=' => if (stored != null and op != null) {
            var buf: [40]u8 = undefined;
            const result = apply(stored.?, op.?, current());
            failed = std.math.isNan(result) or std.math.isInf(result);
            setTyped(format(&buf, result));
            stored = null;
            op = null;
            showing_result = true;
        },
        '%' => {
            var buf: [40]u8 = undefined;
            setTyped(format(&buf, current() / 100));
        },
        'n' => { // ± : flip the sign
            if (typed_len > 0 and typed[0] == '-') {
                std.mem.copyForwards(u8, typed[0..], typed[1..typed_len]);
                typed_len -= 1;
            } else if (typed_len > 0 and !(typed_len == 1 and typed[0] == '0') and typed_len < typed.len) {
                std.mem.copyBackwards(u8, typed[1 .. typed_len + 1], typed[0..typed_len]);
                typed[0] = '-';
                typed_len += 1;
            }
        },
        'b' => if (!showing_result and typed_len > 0) { // Backspace
            typed_len -= 1;
        },
        'c' => {
            typed_len = 0;
            stored = null;
            op = null;
            failed = false;
            showing_result = false;
        },
        else => {},
    }
    show();
}

fn clicked(_: *gui.App, ctx: ?*anyopaque) void {
    press(@intCast(@intFromPtr(ctx.?)));
}

/// A key typed anywhere in the window.
fn typedKey(_: *gui.App, key: gui.Key) bool {
    const k: u8 = switch (key.code) {
        '0'...'9', '.', '+', '-', '*', '/', '%' => @intCast(key.code),
        'x' => '*',
        '=', 13 => '=',
        8 => 'b',
        27, 'c', 'C' => 'c',
        else => return false,
    };
    press(k);
    return true;
}

pub fn main(_: rt.Args) u8 {
    const app = gui.App.init("Calculator") orelse {
        rt.err.write("calc: can't start (no /dev/window, or no font in /usr/share/fonts)\n");
        return 1;
    };
    history = app.label("", .{ .dim = true, .alignment = .end });
    display = app.label("0", .{ .size = 40, .alignment = .end });
    app.on_key = typedKey;

    const Key = struct { text: []const u8, key: u8, accent: bool = false, wide: bool = false };
    const rows = [_][4]?Key{
        .{ .{ .text = "C", .key = 'c' }, .{ .text = "±", .key = 'n' }, .{ .text = "%", .key = '%' }, .{ .text = "÷", .key = '/' } },
        .{ .{ .text = "7", .key = '7' }, .{ .text = "8", .key = '8' }, .{ .text = "9", .key = '9' }, .{ .text = "×", .key = '*' } },
        .{ .{ .text = "4", .key = '4' }, .{ .text = "5", .key = '5' }, .{ .text = "6", .key = '6' }, .{ .text = "−", .key = '-' } },
        .{ .{ .text = "1", .key = '1' }, .{ .text = "2", .key = '2' }, .{ .text = "3", .key = '3' }, .{ .text = "+", .key = '+' } },
        .{ .{ .text = "0", .key = '0', .wide = true }, .{ .text = ".", .key = '.' }, .{ .text = "=", .key = '=', .accent = true }, null },
    };
    var row_widgets: [rows.len]*gui.Widget = undefined;
    for (rows, 0..) |row, r| {
        var buttons: [4]*gui.Widget = undefined;
        var n: usize = 0;
        for (row) |maybe| {
            const k = maybe orelse continue;
            const cb = gui.Callback{ .f = clicked, .ctx = @ptrFromInt(k.key) };
            const b = if (k.accent) app.suggestedButton(k.text, cb) else app.button(k.text, cb);
            b.expand = true;
            buttons[n] = b;
            n += 1;
        }
        // "0" is twice as wide: its row has three buttons, the first in a box with room for two.
        if (row[0].?.wide) {
            const zero = app.hbox(.{ .spacing = 0 }, &.{buttons[0]});
            const rest = app.hbox(.{ .spacing = 8, .homogeneous = true }, buttons[1..n]);
            row_widgets[r] = app.hbox(.{ .spacing = 8, .expand = true, .homogeneous = true }, &.{ zero, rest });
        } else {
            row_widgets[r] = app.hbox(.{ .spacing = 8, .expand = true, .homogeneous = true }, buttons[0..n]);
        }
    }
    const root = app.vbox(.{ .padding = 16, .spacing = 8 }, &.{
        history, display,
        row_widgets[0], row_widgets[1], row_widgets[2], row_widgets[3], row_widgets[4],
    });
    return app.run(root, 300, 400);
}
