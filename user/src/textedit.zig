//! textedit: a graphical text editor. Run it as `textedit [FILE] &`.
//!
//! A multi-line text area with line numbers, selection (drag, or shift and the arrows), the shared
//! clipboard (Ctrl+C/X/V, so text moves to and from `edit` and the terminal), and undo (Ctrl+Z).
//! Ctrl+S saves, Ctrl+O opens the name in the box.

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

var editor: *gui.Widget = undefined;
var view: *gui.TextView = undefined;
var path_entry: *gui.Widget = undefined;
var status: *gui.Widget = undefined;
var app: *gui.App = undefined;

fn showStatus() void {
    var buf: [128]u8 = undefined;
    const line = view.buf.lineOf(view.cursor) + 1;
    const col = view.buf.column(view.cursor, gui.textbuf_tab) + 1;
    gui.setText(status, std.fmt.bufPrint(&buf, "Ln {d}, Col {d}{s}", .{ line, col, if (view.buf.modified) "  •" else "" }) catch "");
}

fn edited(a: *gui.App, _: ?*anyopaque) void {
    _ = a;
    showStatus();
}

fn save(_: *gui.App, _: ?*anyopaque) void {
    var buf: [1024]u8 = undefined;
    const p = rt.cstr(&buf, gui.getText(path_entry)) orelse return;
    const err = view.buf.save(p);
    var msg: [256]u8 = undefined;
    if (err != 0) {
        gui.setText(status, std.fmt.bufPrint(&msg, "Can't save: {s}", .{rt.sys.errorText(err)}) catch "");
    } else {
        gui.setText(status, std.fmt.bufPrint(&msg, "Saved {s}", .{gui.getText(path_entry)}) catch "");
    }
}

fn open(a: *gui.App, _: ?*anyopaque) void {
    var buf: [1024]u8 = undefined;
    const name = gui.getText(path_entry);
    const p = rt.cstr(&buf, name) orelse return;
    var msg: [256]u8 = undefined;
    if (view.buf.load(p)) {
        view.cursor = 0;
        view.anchor = null;
        view.scroll_y = 0;
        a.setFocus(editor);
        gui.setText(status, std.fmt.bufPrint(&msg, "Opened {s}", .{name}) catch "");
    } else {
        gui.setText(status, std.fmt.bufPrint(&msg, "Can't open {s}", .{name}) catch "");
    }
}

/// Ctrl+S and Ctrl+O work wherever the keyboard is.
fn shortcut(a: *gui.App, key: gui.Key) bool {
    if (!key.ctrl) return false;
    switch (key.code) {
        's' => save(a, null),
        'o' => open(a, null),
        else => return false,
    }
    return true;
}

pub fn main(args: rt.Args) u8 {
    app = gui.App.init("Text Editor") orelse {
        rt.err.write("textedit: can't start (no /dev/window, or no font in /usr/share/fonts)\n");
        return 1;
    };
    app.on_key = shortcut;
    editor = app.textView();
    view = editor.kind.text.view;
    editor.kind.text.on_change = .{ .f = edited };

    path_entry = app.entry("file to open or save");
    path_entry.expand = true;
    path_entry.kind.entry.on_activate = .{ .f = open };
    status = app.label("", .{ .dim = true, .alignment = .end });

    if (args.get(1)) |name| {
        gui.setText(path_entry, name);
        var buf: [1024]u8 = undefined;
        if (rt.cstr(&buf, name)) |p| _ = view.buf.load(p);
    }
    showStatus();

    const root = app.vbox(.{ .padding = 12, .spacing = 10, .expand = true }, &.{
        app.hbox(.{ .spacing = 8 }, &.{
            path_entry,
            app.suggestedButton("Save", .{ .f = save }),
            app.button("Open", .{ .f = open }),
        }),
        editor,
        status,
    });
    app.setFocus(editor);
    return app.run(root, 640, 460);
}
