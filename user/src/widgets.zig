//! widgets: a small tour of the graphical toolkit (lib/gui.zig). Type your name and say hi,
//! count up and down, flip a switch. Run it as `widgets &`.

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

var name: *gui.Widget = undefined;
var greeting: *gui.Widget = undefined;
var count_label: *gui.Widget = undefined;
var switch_label: *gui.Widget = undefined;
var count: i32 = 0;

fn sayHi(_: *gui.App, _: ?*anyopaque) void {
    var buf: [300]u8 = undefined;
    const who = gui.getText(name);
    gui.setText(greeting, if (who.len == 0) "Type your name first :)" else std.fmt.bufPrint(&buf, "Hi, {s}! Welcome aboard.", .{who}) catch "Hi!");
}

fn showCount() void {
    var buf: [32]u8 = undefined;
    gui.setText(count_label, std.fmt.bufPrint(&buf, "{d}", .{count}) catch "?");
}

fn less(_: *gui.App, _: ?*anyopaque) void {
    count -= 1;
    showCount();
}

fn more(_: *gui.App, _: ?*anyopaque) void {
    count += 1;
    showCount();
}

fn flipped(_: *gui.App, ctx: ?*anyopaque) void {
    const w: *gui.Widget = @ptrCast(@alignCast(ctx.?));
    gui.setText(switch_label, if (w.kind.toggle.on) "Switch: on" else "Switch: off");
}

pub fn main(_: rt.Args) u8 {
    const app = gui.App.init("Widgets") orelse {
        rt.err.write("widgets: can't start (no /dev/window, or no font in /usr/share/fonts)\n");
        return 1;
    };
    name = app.entry("Your name");
    name.expand = true;
    name.kind.entry.on_activate = .{ .f = sayHi };
    greeting = app.label("Nice to meet you.", .{ .dim = true });
    count_label = app.label("0", .{ .size = 22 });
    switch_label = app.label("Switch: off", .{});
    const sw = app.toggle(false, null);
    sw.kind.toggle.on_change = .{ .f = flipped, .ctx = sw };

    const root = app.vbox(.{ .padding = 24, .spacing = 14 }, &.{
        app.label("Hello from KernelProject2", .{ .size = 24 }),
        greeting,
        app.hbox(.{ .spacing = 8 }, &.{ name, app.suggestedButton("Say hi", .{ .f = sayHi }) }),
        app.hbox(.{ .spacing = 8 }, &.{ app.button("−", .{ .f = less }), count_label, app.button("+", .{ .f = more }) }),
        app.hbox(.{ .spacing = 12 }, &.{ sw, switch_label }),
    });
    return app.run(root, 380, 0);
}
