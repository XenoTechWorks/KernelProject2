//! widgets: a small tour of the graphical toolkit (lib/gui.zig). Type your name and say hi,
//! count up and down, flip a switch, tick a box, drag a slider, pick from a list.
//! Run it as `widgets &`.

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
var slider: *gui.Widget = undefined;
var bar: *gui.Widget = undefined;
var slider_label: *gui.Widget = undefined;
var fruit: *gui.Widget = undefined;
var fruit_label: *gui.Widget = undefined;
var count: i32 = 0;

const fruits = [_][]const u8{ "Apple", "Banana", "Cherry", "Dragon fruit", "Elderberry", "Fig", "Grape", "Honeydew", "Kiwi", "Lemon", "Mango", "Nectarine" };

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

fn slid(_: *gui.App, _: ?*anyopaque) void {
    const v = slider.kind.slider.value;
    gui.setProgress(bar, v / 100);
    var buf: [32]u8 = undefined;
    gui.setText(slider_label, std.fmt.bufPrint(&buf, "{d:.0}%", .{v}) catch "?");
}

fn picked(_: *gui.App, _: ?*anyopaque) void {
    var buf: [64]u8 = undefined;
    const i = gui.selected(fruit) orelse return;
    gui.setText(fruit_label, std.fmt.bufPrint(&buf, "You picked: {s}", .{fruits[i]}) catch "?");
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
    slider = app.slider(.{ .value = 40, .on_change = .{ .f = slid } });
    slider.expand = true;
    bar = app.progress(0.4);
    slider_label = app.label("40%", .{});
    fruit = app.list();
    gui.setItems(fruit, &fruits);
    fruit.kind.list.on_select = .{ .f = picked };
    fruit_label = app.label("Pick a fruit", .{ .dim = true });

    const left = app.vbox(.{ .spacing = 14, .expand = true }, &.{
        app.label("Hello from KernelProject2", .{ .size = 24 }),
        greeting,
        app.hbox(.{ .spacing = 8 }, &.{ name, app.suggestedButton("Say hi", .{ .f = sayHi }) }),
        app.hbox(.{ .spacing = 8 }, &.{ app.button("−", .{ .f = less }), count_label, app.button("+", .{ .f = more }) }),
        app.hbox(.{ .spacing = 12 }, &.{ sw, switch_label }),
        app.check("Remember me", true, null),
        app.hbox(.{ .spacing = 12 }, &.{ slider, slider_label }),
        bar,
    });
    const right = app.vbox(.{ .spacing = 10, .expand = true }, &.{ fruit, fruit_label });
    const root = app.hbox(.{ .padding = 24, .spacing = 24 }, &.{ left, app.separator(), right });
    return app.run(root, 640, 0);
}
