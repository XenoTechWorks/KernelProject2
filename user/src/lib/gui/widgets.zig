//! Widgets: the parts of a graphical app (labels, buttons, text entries, switches) arranged in
//! boxes, the way GTK does it. Build a tree, hand it to App.run, and react in callbacks:
//!
//!     const app = gui.App.init("Hello") orelse return 1;
//!     const name = app.entry("Your name");
//!     const root = app.vbox(.{}, &.{ app.label("Who are you?", .{}), name, app.button("OK", .{ .f = ok }) });
//!     return app.run(root);
//!
//! Each change redraws the window. The look follows GNOME's dark style.

const std = @import("std");
const canvas_mod = @import("canvas.zig");
const Canvas = canvas_mod.Canvas;
const Rect = canvas_mod.Rect;
const Font = @import("font.zig").Font;
const win = @import("window.zig");

pub const theme = struct {
    pub const bg = 0x22262e;
    pub const text = 0xe6e9ef;
    pub const dim = 0x8b93a1;
    pub const button = 0x353b47;
    pub const button_hover = 0x3f4654;
    pub const button_down = 0x2c313b;
    pub const accent = 0x3584e4;
    pub const accent_hover = 0x4b93ea;
    pub const accent_down = 0x2a6fc4;
    pub const field = 0x1a1d23;
    pub const field_border = 0x3a404d;
    pub const track = 0x3e4552;
    pub const radius = 8;
    pub const text_size = 15;
    pub const font_path = "/usr/share/fonts/Inter.ttf";
};

pub const Callback = struct {
    f: *const fn (app: *App, ctx: ?*anyopaque) void,
    ctx: ?*anyopaque = null,
};

pub const Align = enum { fill, start, center, end };

pub const Widget = struct {
    /// Where layout put it, in window pixels.
    rect: Rect = .{ .x = 0, .y = 0, .w = 0, .h = 0 },
    /// Takes a share of spare room in its box.
    expand: bool = false,
    /// Across its box: stretched, or at the start, center or end.
    alignment: Align = .fill,
    kind: Kind,

    pub const Kind = union(enum) {
        box: Box,
        label: Label,
        button: Button,
        entry: Entry,
        toggle: Toggle,
    };

    fn focusable(w: *const Widget) bool {
        return switch (w.kind) {
            .button, .entry, .toggle => true,
            else => false,
        };
    }
};

pub const Box = struct { vertical: bool, spacing: i32, padding: i32, children: []const *Widget };

pub const Label = struct {
    buf: [256]u8 = undefined,
    len: usize = 0,
    size: f32,
    color: u32,

    pub fn text(l: *const Label) []const u8 {
        return l.buf[0..l.len];
    }
};

pub const Button = struct { label: []const u8, on_click: ?Callback, suggested: bool };

pub const Entry = struct {
    buf: [256]u8 = undefined,
    len: usize = 0,
    /// Byte position of the cursor.
    cursor: usize = 0,
    /// How far the text is scrolled left (when it's longer than the field), in pixels.
    scroll: f32 = 0,
    placeholder: []const u8,
    on_activate: ?Callback = null,

    pub fn text(e: *const Entry) []const u8 {
        return e.buf[0..e.len];
    }
};

pub const Toggle = struct { on: bool, on_change: ?Callback };

pub const LabelOptions = struct { size: f32 = theme.text_size, dim: bool = false, alignment: Align = .start };
pub const BoxOptions = struct { spacing: i32 = 12, padding: i32 = 0, expand: bool = false, alignment: Align = .fill };

pub const App = struct {
    arena: std.heap.ArenaAllocator,
    font: *Font,
    title: [:0]const u8,
    window: win.Window = undefined,
    root: *Widget = undefined,
    focus: ?*Widget = null,
    /// Show which widget has the keyboard (after Tab, like GTK; not after clicks).
    focus_visible: bool = false,
    hover: ?*Widget = null,
    pressed: ?*Widget = null,
    timer: ?struct { ms: u32, callback: Callback } = null,
    quit: bool = false,

    pub fn init(title: [:0]const u8) ?*App {
        const font = Font.load(theme.font_path) orelse return null;
        const app = std.heap.page_allocator.create(App) catch return null;
        app.* = .{ .arena = std.heap.ArenaAllocator.init(std.heap.page_allocator), .font = font, .title = title };
        return app;
    }

    fn new(app: *App, w: Widget) *Widget {
        const p = app.arena.allocator().create(Widget) catch @panic("out of memory");
        p.* = w;
        return p;
    }

    pub fn label(app: *App, text: []const u8, o: LabelOptions) *Widget {
        const w = app.new(.{ .alignment = o.alignment, .kind = .{ .label = .{ .size = o.size, .color = if (o.dim) theme.dim else theme.text } } });
        setText(w, text);
        return w;
    }

    pub fn button(app: *App, text: []const u8, on_click: ?Callback) *Widget {
        return app.new(.{ .kind = .{ .button = .{ .label = text, .on_click = on_click, .suggested = false } } });
    }

    /// A button for the main thing to do here, in the accent color.
    pub fn suggestedButton(app: *App, text: []const u8, on_click: ?Callback) *Widget {
        return app.new(.{ .kind = .{ .button = .{ .label = text, .on_click = on_click, .suggested = true } } });
    }

    pub fn entry(app: *App, placeholder: []const u8) *Widget {
        return app.new(.{ .kind = .{ .entry = .{ .placeholder = placeholder } } });
    }

    /// An on/off switch.
    pub fn toggle(app: *App, on: bool, on_change: ?Callback) *Widget {
        return app.new(.{ .alignment = .start, .kind = .{ .toggle = .{ .on = on, .on_change = on_change } } });
    }

    pub fn vbox(app: *App, o: BoxOptions, children: []const *Widget) *Widget {
        return app.box(true, o, children);
    }

    pub fn hbox(app: *App, o: BoxOptions, children: []const *Widget) *Widget {
        return app.box(false, o, children);
    }

    fn box(app: *App, vertical: bool, o: BoxOptions, children: []const *Widget) *Widget {
        const list = app.arena.allocator().dupe(*Widget, children) catch @panic("out of memory");
        return app.new(.{ .expand = o.expand, .alignment = o.alignment, .kind = .{ .box = .{ .vertical = vertical, .spacing = o.spacing, .padding = o.padding, .children = list } } });
    }

    /// Calls `callback` every `ms` milliseconds.
    pub fn every(app: *App, ms: u32, callback: Callback) void {
        app.timer = .{ .ms = ms, .callback = callback };
    }

    /// Opens the window (big enough for `root`, and at least min_w x min_h) and runs until it's
    /// closed or quit() is called. Returns the exit code.
    pub fn run(app: *App, root: *Widget, min_w: u32, min_h: u32) u8 {
        app.root = root;
        const size = app.measure(root);
        const w = @max(min_w, @as(u32, @intCast(size[0])));
        const h = @max(min_h, @as(u32, @intCast(size[1])));
        app.window = win.Window.open(app.title, w, h) orelse return 1;
        app.layout(root, app.window.canvas.bounds());
        app.redraw();
        while (!app.quit) {
            const event = app.window.next(if (app.timer) |t| t.ms else null) orelse break;
            switch (event) {
                .tick => if (app.timer) |t| t.callback.f(app, t.callback.ctx),
                else => app.handle(event),
            }
            app.redraw();
        }
        app.window.close();
        return 0;
    }

    /// Ends run() after the current event.
    pub fn exit(app: *App) void {
        app.quit = true;
    }

    // ---- events ----

    fn handle(app: *App, event: win.Event) void {
        switch (event) {
            .motion => |m| app.hover = hit(app.root, m.x, m.y),
            .leave => app.hover = null,
            .press => |p| {
                const target = hit(app.root, p.x, p.y);
                app.pressed = target;
                app.focus_visible = false;
                if (target) |t| {
                    app.focus = t;
                    if (t.kind == .entry) app.placeCursor(t, p.x);
                }
            },
            .release => |r| {
                const target = hit(app.root, r.x, r.y);
                if (app.pressed != null and app.pressed == target) app.activate(target.?);
                app.pressed = null;
            },
            .key => |k| app.key(k),
            else => {},
        }
    }

    fn key(app: *App, k: win.Key) void {
        if (k.code == 9) { // Tab, Shift+Tab: the next or previous widget gets the keyboard
            app.focus = app.nextFocus(k.shift);
            app.focus_visible = true;
            return;
        }
        const f = app.focus orelse return;
        switch (f.kind) {
            .entry => |*e| editEntry(app, f, e, k),
            .button, .toggle => if (k.code == 13 or k.code == ' ') app.activate(f),
            else => {},
        }
    }

    fn activate(app: *App, w: *Widget) void {
        switch (w.kind) {
            .button => |b| if (b.on_click) |cb| cb.f(app, cb.ctx),
            .toggle => |*t| {
                t.on = !t.on;
                if (t.on_change) |cb| cb.f(app, cb.ctx);
            },
            else => {},
        }
    }

    fn nextFocus(app: *App, backwards: bool) ?*Widget {
        var list: [64]*Widget = undefined;
        var n: usize = 0;
        collectFocusable(app.root, &list, &n);
        if (n == 0) return null;
        const at = for (list[0..n], 0..) |w, i| {
            if (app.focus == w) break i;
        } else return list[if (backwards) n - 1 else 0];
        return list[if (backwards) (at + n - 1) % n else (at + 1) % n];
    }

    fn placeCursor(app: *App, w: *Widget, x: i32) void {
        const e = &w.kind.entry;
        const want = @as(f32, @floatFromInt(x - w.rect.x - 10)) + e.scroll;
        var pos: usize = 0;
        var best: f32 = std.math.floatMax(f32);
        var i: usize = 0;
        while (i <= e.len) : (i += 1) {
            if (i < e.len and e.buf[i] & 0xC0 == 0x80) continue; // inside a character
            const d = @abs(canvas_mod.measure(app.font, theme.text_size, e.buf[0..i]) - want);
            if (d < best) {
                best = d;
                pos = i;
            }
        }
        e.cursor = pos;
    }

    // ---- layout ----

    fn measure(app: *App, w: *const Widget) [2]i32 {
        const line: i32 = @intFromFloat(@ceil(app.font.lineHeight(theme.text_size)));
        return switch (w.kind) {
            .label => |*l| .{ @intFromFloat(@ceil(canvas_mod.measure(app.font, l.size, l.text()))), @intFromFloat(@ceil(app.font.lineHeight(l.size))) },
            .button => |b| .{ @as(i32, @intFromFloat(@ceil(canvas_mod.measure(app.font, theme.text_size, b.label)))) + 32, @max(34, line + 12) },
            .entry => .{ 220, @max(34, line + 12) },
            .toggle => .{ 48, 26 },
            .box => |b| blk: {
                var main: i32 = 0;
                var cross: i32 = 0;
                for (b.children, 0..) |c, i| {
                    const s = app.measure(c);
                    const m = if (b.vertical) s[1] else s[0];
                    const x = if (b.vertical) s[0] else s[1];
                    main += m + if (i > 0) b.spacing else 0;
                    cross = @max(cross, x);
                }
                const p = 2 * b.padding;
                break :blk if (b.vertical) .{ cross + p, main + p } else .{ main + p, cross + p };
            },
        };
    }

    fn layout(app: *App, w: *Widget, r: Rect) void {
        w.rect = r;
        const b = switch (w.kind) {
            .box => |b| b,
            else => return,
        };
        const inner = r.inset(b.padding);
        var used: i32 = 0;
        var expanders: i32 = 0;
        for (b.children, 0..) |c, i| {
            const s = app.measure(c);
            used += (if (b.vertical) s[1] else s[0]) + if (i > 0) b.spacing else 0;
            if (c.expand) expanders += 1;
        }
        const room = (if (b.vertical) inner.h else inner.w) - used;
        var pos = if (b.vertical) inner.y else inner.x;
        for (b.children) |c| {
            const s = app.measure(c);
            var main = if (b.vertical) s[1] else s[0];
            if (c.expand and expanders > 0 and room > 0) main += @divTrunc(room, expanders);
            const cross_room = if (b.vertical) inner.w else inner.h;
            const want = if (b.vertical) s[0] else s[1];
            // Across the box: stretched, or placed. Rows of an hbox center vertically.
            const alignment = if (!b.vertical and c.alignment == .fill and c.kind != .box) Align.center else c.alignment;
            const cross_size = if (alignment == .fill) cross_room else @min(want, cross_room);
            const offset = switch (alignment) {
                .fill, .start => 0,
                .center => @divTrunc(cross_room - cross_size, 2),
                .end => cross_room - cross_size,
            };
            const cr = if (b.vertical)
                Rect{ .x = inner.x + offset, .y = pos, .w = cross_size, .h = main }
            else
                Rect{ .x = pos, .y = inner.y + offset, .w = main, .h = cross_size };
            app.layout(c, cr);
            pos += main + b.spacing;
        }
    }

    // ---- drawing ----

    pub fn redraw(app: *App) void {
        const c = &app.window.canvas;
        c.fill(theme.bg);
        app.draw(app.root, c);
        app.window.present();
    }

    fn draw(app: *App, w: *Widget, c: *Canvas) void {
        const r = w.rect;
        const hovered = app.hover == w;
        const down = app.pressed == w and hovered;
        const focused = app.focus == w;
        switch (w.kind) {
            .box => |b| for (b.children) |child| app.draw(child, c),
            .label => |*l| {
                const asc: i32 = @intFromFloat(@round(app.font.ascender(l.size)));
                const lh: i32 = @intFromFloat(@ceil(app.font.lineHeight(l.size)));
                _ = c.text(app.font, l.size, @floatFromInt(r.x), r.y + @divTrunc(r.h - lh, 2) + asc, l.text(), l.color);
            },
            .button => |b| {
                const color: u32 = if (b.suggested)
                    (if (down) theme.accent_down else if (hovered) theme.accent_hover else theme.accent)
                else
                    (if (down) theme.button_down else if (hovered) theme.button_hover else theme.button);
                c.roundRect(r, theme.radius, color);
                const tw = canvas_mod.measure(app.font, theme.text_size, b.label);
                const x = @as(f32, @floatFromInt(r.x)) + (@as(f32, @floatFromInt(r.w)) - tw) / 2;
                _ = c.text(app.font, theme.text_size, @round(x), app.baseline(r), b.label, if (b.suggested) 0xffffff else theme.text);
                if (focused and app.focus_visible) c.roundRectOutline(r, theme.radius, 2, theme.accent_hover);
            },
            .entry => |*e| {
                c.roundRect(r, theme.radius, theme.field);
                if (focused) c.roundRectOutline(r, theme.radius, 2, theme.accent) else c.roundRectOutline(r, theme.radius, 1, theme.field_border);
                const inner = Rect{ .x = r.x + 10, .y = r.y, .w = r.w - 20, .h = r.h };
                const cursor_x = canvas_mod.measure(app.font, theme.text_size, e.buf[0..e.cursor]);
                const room: f32 = @floatFromInt(inner.w - 2);
                if (cursor_x - e.scroll > room) e.scroll = cursor_x - room;
                if (cursor_x < e.scroll) e.scroll = cursor_x;
                const saved = c.clip;
                c.clip = inner.intersect(saved);
                const x0 = @as(f32, @floatFromInt(inner.x)) - e.scroll;
                if (e.len == 0 and !focused) {
                    _ = c.text(app.font, theme.text_size, @floatFromInt(inner.x), app.baseline(r), e.placeholder, theme.dim);
                } else {
                    _ = c.text(app.font, theme.text_size, x0, app.baseline(r), e.text(), theme.text);
                }
                if (focused) {
                    const cx: i32 = @intFromFloat(@round(x0 + cursor_x));
                    c.rect(.{ .x = cx, .y = r.y + 8, .w = 1, .h = r.h - 16 }, theme.text);
                }
                c.clip = saved;
            },
            .toggle => |t| {
                const track: u32 = if (t.on) (if (hovered) theme.accent_hover else theme.accent) else (if (hovered) theme.button_hover else theme.track);
                c.roundRect(r, @as(f32, @floatFromInt(r.h)) / 2, track);
                const knob_r = @as(f32, @floatFromInt(r.h)) / 2 - 3;
                const ky = @as(f32, @floatFromInt(r.y)) + @as(f32, @floatFromInt(r.h)) / 2;
                const kx = if (t.on) @as(f32, @floatFromInt(r.right())) - 3 - knob_r else @as(f32, @floatFromInt(r.x)) + 3 + knob_r;
                c.circle(kx, ky, knob_r, 0xffffff);
                if (focused and app.focus_visible) c.roundRectOutline(r, @as(f32, @floatFromInt(r.h)) / 2, 2, theme.accent_hover);
            },
        }
    }

    /// Where the baseline goes to center a line of body text in `r`.
    fn baseline(app: *App, r: Rect) i32 {
        const asc = app.font.ascender(theme.text_size);
        const lh = app.font.lineHeight(theme.text_size);
        return r.y + @as(i32, @intFromFloat(@round((@as(f32, @floatFromInt(r.h)) - lh) / 2 + asc)));
    }
};

/// Changes a label's text (or an entry's).
pub fn setText(w: *Widget, s: []const u8) void {
    switch (w.kind) {
        .label => |*l| {
            l.len = @min(s.len, l.buf.len);
            @memcpy(l.buf[0..l.len], s[0..l.len]);
        },
        .entry => |*e| {
            e.len = @min(s.len, e.buf.len);
            @memcpy(e.buf[0..e.len], s[0..e.len]);
            e.cursor = e.len;
        },
        else => {},
    }
}

/// An entry's text (or a label's).
pub fn getText(w: *const Widget) []const u8 {
    return switch (w.kind) {
        .label => |*l| l.text(),
        .entry => |*e| e.text(),
        else => "",
    };
}

/// The deepest widget you can click at a point.
fn hit(w: *Widget, x: i32, y: i32) ?*Widget {
    if (!w.rect.contains(x, y)) return null;
    switch (w.kind) {
        .box => |b| {
            for (b.children) |c| if (hit(c, x, y)) |found| return found;
            return null;
        },
        else => return if (w.focusable()) w else null,
    }
}

fn collectFocusable(w: *Widget, list: *[64]*Widget, n: *usize) void {
    switch (w.kind) {
        .box => |b| for (b.children) |c| collectFocusable(c, list, n),
        else => if (w.focusable() and n.* < list.len) {
            list[n.*] = w;
            n.* += 1;
        },
    }
}

fn editEntry(app: *App, w: *Widget, e: *Entry, k: win.Key) void {
    _ = w;
    switch (k.code) {
        8 => if (e.cursor > 0) { // Backspace: the character before the cursor
            var start = e.cursor - 1;
            while (start > 0 and e.buf[start] & 0xC0 == 0x80) start -= 1;
            std.mem.copyForwards(u8, e.buf[start..], e.buf[e.cursor..e.len]);
            e.len -= e.cursor - start;
            e.cursor = start;
        },
        127 => if (e.cursor < e.len) { // Delete: the character after it
            var end = e.cursor + 1;
            while (end < e.len and e.buf[end] & 0xC0 == 0x80) end += 1;
            std.mem.copyForwards(u8, e.buf[e.cursor..], e.buf[end..e.len]);
            e.len -= end - e.cursor;
        },
        win.Key.left => if (e.cursor > 0) {
            e.cursor -= 1;
            while (e.cursor > 0 and e.buf[e.cursor] & 0xC0 == 0x80) e.cursor -= 1;
        },
        win.Key.right => if (e.cursor < e.len) {
            e.cursor += 1;
            while (e.cursor < e.len and e.buf[e.cursor] & 0xC0 == 0x80) e.cursor += 1;
        },
        win.Key.home => e.cursor = 0,
        win.Key.end => e.cursor = e.len,
        13 => if (e.on_activate) |cb| cb.f(app, cb.ctx),
        else => if (k.code >= 32 and k.code < 127 and !k.ctrl and e.len < e.buf.len) {
            std.mem.copyBackwards(u8, e.buf[e.cursor + 1 .. e.len + 1], e.buf[e.cursor..e.len]);
            e.buf[e.cursor] = @intCast(k.code);
            e.len += 1;
            e.cursor += 1;
        },
    }
}
