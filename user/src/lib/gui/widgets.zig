//! Widgets: the parts of a graphical app (labels, buttons, text entries, switches, checkboxes,
//! sliders, progress bars, lists) arranged in boxes, the way GTK does it. Build a tree, hand it to
//! App.run, and react in callbacks:
//!
//!     const app = gui.App.init("Hello") orelse return 1;
//!     const name = app.entry("Your name");
//!     const root = app.vbox(.{}, &.{ app.label("Who are you?", .{}), name, app.button("OK", .{ .f = ok }) });
//!     return app.run(root, 300, 0);
//!
//! The window can be resized; the boxes share out the room. The look follows GNOME's dark style.

const std = @import("std");
const canvas_mod = @import("canvas.zig");
const Canvas = canvas_mod.Canvas;
const Rect = canvas_mod.Rect;
const Font = @import("font.zig").Font;
const win = @import("window.zig");
const sys = @import("../sys.zig");

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
    pub const row_hover = 0x2a2f38;
    pub const radius = 8;
    pub const text_size = 15;
    pub const row_height = 34;
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
        check: Check,
        slider: Slider,
        progress: Progress,
        list: List,
        separator,
    };

    fn focusable(w: *const Widget) bool {
        return switch (w.kind) {
            .button, .entry, .toggle, .check, .slider, .list => true,
            else => false,
        };
    }
};

pub const Box = struct {
    vertical: bool,
    spacing: i32,
    padding: i32,
    /// Every child gets the same size along the box (like a row of a calculator's keys).
    homogeneous: bool = false,
    children: []const *Widget,
};

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

pub const Check = struct { label: []const u8, on: bool, on_change: ?Callback };

pub const Slider = struct {
    value: f32,
    min: f32,
    max: f32,
    /// How far an arrow key moves it.
    step: f32,
    on_change: ?Callback,

    fn fraction(s: *const Slider) f32 {
        return if (s.max > s.min) std.math.clamp((s.value - s.min) / (s.max - s.min), 0, 1) else 0;
    }
};

pub const Progress = struct { fraction: f32 };

/// A list of text rows, one of which can be selected.
pub const List = struct {
    /// All the rows' text, one after another; `ends[i]` is where row i ends.
    text: []u8 = &.{},
    ends: []u32 = &.{},
    selected: ?usize = null,
    /// How far the rows are scrolled up, in pixels.
    scroll: i32 = 0,
    /// Called when the selection changes, and when a row is opened (double click or Enter).
    on_select: ?Callback = null,
    on_activate: ?Callback = null,
    last_click: struct { row: usize = std.math.maxInt(usize), ms: i64 = 0 } = .{},

    pub fn len(l: *const List) usize {
        return l.ends.len;
    }

    pub fn item(l: *const List, i: usize) []const u8 {
        const start = if (i == 0) 0 else l.ends[i - 1];
        return l.text[start..l.ends[i]];
    }
};

pub const LabelOptions = struct { size: f32 = theme.text_size, dim: bool = false, alignment: Align = .fill };
pub const BoxOptions = struct { spacing: i32 = 12, padding: i32 = 0, expand: bool = false, alignment: Align = .fill, homogeneous: bool = false };
pub const SliderOptions = struct { min: f32 = 0, max: f32 = 100, value: f32 = 0, step: f32 = 0, on_change: ?Callback = null };

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
    timer: ?struct { ms: u32, callback: Callback, due: i64 } = null,
    quit: bool = false,
    /// Something changed since the window was last drawn.
    dirty: bool = true,
    /// The pointer, for hover effects inside a widget (list rows).
    pointer_x: i32 = -1,
    pointer_y: i32 = -1,
    /// Sees every key first; returns true if it used it (for keyboard shortcuts).
    on_key: ?*const fn (app: *App, key: win.Key) bool = null,

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

    /// A checkbox with its label.
    pub fn check(app: *App, text: []const u8, on: bool, on_change: ?Callback) *Widget {
        return app.new(.{ .alignment = .start, .kind = .{ .check = .{ .label = text, .on = on, .on_change = on_change } } });
    }

    pub fn slider(app: *App, o: SliderOptions) *Widget {
        const step = if (o.step > 0) o.step else (o.max - o.min) / 20;
        return app.new(.{ .kind = .{ .slider = .{ .value = o.value, .min = o.min, .max = o.max, .step = step, .on_change = o.on_change } } });
    }

    /// A bar showing how far along something is (0 to 1).
    pub fn progress(app: *App, fraction: f32) *Widget {
        return app.new(.{ .kind = .{ .progress = .{ .fraction = fraction } } });
    }

    /// A list of rows; fill it with setItems.
    pub fn list(app: *App) *Widget {
        return app.new(.{ .expand = true, .kind = .{ .list = .{} } });
    }

    /// A thin line between parts.
    pub fn separator(app: *App) *Widget {
        return app.new(.{ .kind = .separator });
    }

    /// Empty room that takes up what's spare (to push things apart).
    pub fn spacer(app: *App) *Widget {
        return app.box(true, .{ .expand = true }, &.{});
    }

    pub fn vbox(app: *App, o: BoxOptions, children: []const *Widget) *Widget {
        return app.box(true, o, children);
    }

    pub fn hbox(app: *App, o: BoxOptions, children: []const *Widget) *Widget {
        return app.box(false, o, children);
    }

    fn box(app: *App, vertical: bool, o: BoxOptions, children: []const *Widget) *Widget {
        const items = app.arena.allocator().dupe(*Widget, children) catch @panic("out of memory");
        return app.new(.{ .expand = o.expand, .alignment = o.alignment, .kind = .{ .box = .{ .vertical = vertical, .spacing = o.spacing, .padding = o.padding, .homogeneous = o.homogeneous, .children = items } } });
    }

    /// Calls `callback` every `ms` milliseconds.
    pub fn every(app: *App, ms: u32, callback: Callback) void {
        app.timer = .{ .ms = ms, .callback = callback, .due = nowMs() + ms };
    }

    /// Asks for the window to be drawn again (after changing widgets outside a callback).
    pub fn changed(app: *App) void {
        app.dirty = true;
    }

    /// Opens the window (big enough for `root`, and at least min_w x min_h) and runs until it's
    /// closed or exit() is called. Returns the exit code.
    pub fn run(app: *App, root: *Widget, min_w: u32, min_h: u32) u8 {
        app.root = root;
        const size = app.measure(root);
        const w = @max(min_w, @as(u32, @intCast(size[0])));
        const h = @max(min_h, @as(u32, @intCast(size[1])));
        app.window = win.Window.open(app.title, w, h) orelse return 1;
        app.layout(root, app.window.canvas.bounds());
        while (!app.quit) {
            // Draw once the events that are waiting have all been handled.
            if (app.dirty and !app.window.pending()) {
                app.redraw();
                app.dirty = false;
            }
            const timeout: ?u32 = if (app.timer) |t| @intCast(std.math.clamp(t.due - nowMs(), 0, t.ms)) else null;
            const event = app.window.next(timeout) orelse break;
            if (app.timer) |*t| {
                if (nowMs() >= t.due) {
                    t.due += t.ms;
                    if (t.due < nowMs()) t.due = nowMs() + t.ms; // fell behind: don't catch up
                    t.callback.f(app, t.callback.ctx);
                    app.dirty = true;
                }
            }
            app.handle(event);
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
            .motion => |m| {
                const over = hit(app.root, m.x, m.y);
                if (over != app.hover or (over != null and over.?.kind == .list)) app.dirty = true;
                app.pointer_x = m.x;
                app.pointer_y = m.y;
                app.hover = over;
                if (app.pressed) |p| if (p.kind == .slider) app.slide(p, m.x);
            },
            .leave => {
                app.hover = null;
                app.dirty = true;
            },
            .press => |p| {
                const target = hit(app.root, p.x, p.y);
                app.pressed = target;
                app.focus_visible = false;
                app.dirty = true;
                if (target) |t| {
                    app.focus = t;
                    switch (t.kind) {
                        .entry => app.placeCursor(t, p.x),
                        .slider => app.slide(t, p.x),
                        .list => app.clickRow(t, p.y),
                        else => {},
                    }
                }
            },
            .release => |r| {
                const target = hit(app.root, r.x, r.y);
                if (app.pressed != null and app.pressed == target) app.activate(target.?);
                app.pressed = null;
                app.dirty = true;
            },
            .scroll => |s| if (hit(app.root, s.x, s.y)) |t| if (t.kind == .list) {
                scrollList(t, s.steps * 3 * theme.row_height);
                app.dirty = true;
            },
            .resize => {
                app.layout(app.root, app.window.canvas.bounds());
                app.dirty = true;
            },
            .key => |k| {
                app.key(k);
                app.dirty = true;
            },
            .focus => app.dirty = true,
            .tick => {},
        }
    }

    fn key(app: *App, k: win.Key) void {
        // An entry with the keyboard gets typing first; otherwise the app's shortcuts do.
        const typing = app.focus != null and app.focus.?.kind == .entry;
        if (!typing) if (app.on_key) |f| if (f(app, k)) return;
        if (k.code == 9) { // Tab, Shift+Tab: the next or previous widget gets the keyboard
            app.focus = app.nextFocus(k.shift);
            app.focus_visible = true;
            return;
        }
        const f = app.focus orelse return;
        switch (f.kind) {
            .entry => |*e| editEntry(app, e, k),
            .button, .toggle, .check => if (k.code == 13 or k.code == ' ') app.activate(f),
            .slider => |*s| {
                const delta: f32 = switch (k.code) {
                    win.Key.left, win.Key.down => -s.step,
                    win.Key.right, win.Key.up => s.step,
                    else => return,
                };
                setSlider(app, s, s.value + delta);
            },
            .list => |*l| {
                if (l.len() == 0) return;
                const last = l.len() - 1;
                const at = l.selected orelse 0;
                const to: usize = switch (k.code) {
                    win.Key.up => at -| 1,
                    win.Key.down => if (l.selected == null) 0 else @min(at + 1, last),
                    win.Key.home => 0,
                    win.Key.end => last,
                    13 => {
                        if (l.on_activate) |cb| if (l.selected != null) cb.f(app, cb.ctx);
                        return;
                    },
                    else => return,
                };
                app.selectRow(f, to);
            },
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
            .check => |*c| {
                c.on = !c.on;
                if (c.on_change) |cb| cb.f(app, cb.ctx);
            },
            else => {},
        }
    }

    fn slide(app: *App, w: *Widget, x: i32) void {
        const s = &w.kind.slider;
        const frac = @as(f32, @floatFromInt(x - w.rect.x - 10)) / @as(f32, @floatFromInt(@max(1, w.rect.w - 20)));
        setSlider(app, s, s.min + std.math.clamp(frac, 0, 1) * (s.max - s.min));
    }

    fn clickRow(app: *App, w: *Widget, y: i32) void {
        const l = &w.kind.list;
        const row: usize = @intCast(@max(0, @divTrunc(y - w.rect.y - 4 + l.scroll, theme.row_height)));
        if (row >= l.len()) return;
        const now = nowMs();
        const double = l.last_click.row == row and now - l.last_click.ms < 450;
        l.last_click = .{ .row = row, .ms = now };
        app.selectRow(w, row);
        if (double) if (l.on_activate) |cb| cb.f(app, cb.ctx);
    }

    fn selectRow(app: *App, w: *Widget, row: usize) void {
        const l = &w.kind.list;
        const changed_row = l.selected != row;
        l.selected = row;
        // Scroll so the row shows.
        const top: i32 = @intCast(row * theme.row_height);
        const view = w.rect.h - 8;
        if (top < l.scroll) l.scroll = top;
        if (top + theme.row_height > l.scroll + view) l.scroll = top + theme.row_height - view;
        if (changed_row) if (l.on_select) |cb| cb.f(app, cb.ctx);
    }

    fn nextFocus(app: *App, backwards: bool) ?*Widget {
        var found: [64]*Widget = undefined;
        var n: usize = 0;
        collectFocusable(app.root, &found, &n);
        if (n == 0) return null;
        const at = for (found[0..n], 0..) |w, i| {
            if (app.focus == w) break i;
        } else return found[if (backwards) n - 1 else 0];
        return found[if (backwards) (at + n - 1) % n else (at + 1) % n];
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
        const text_w = struct {
            fn f(a: *App, s: []const u8, size: f32) i32 {
                return @intFromFloat(@ceil(canvas_mod.measure(a.font, size, s)));
            }
        }.f;
        return switch (w.kind) {
            .label => |*l| .{ text_w(app, l.text(), l.size), @intFromFloat(@ceil(app.font.lineHeight(l.size))) },
            .button => |b| .{ text_w(app, b.label, theme.text_size) + 32, @max(34, line + 12) },
            .entry => .{ 220, @max(34, line + 12) },
            .toggle => .{ 48, 26 },
            .check => |c| .{ 28 + text_w(app, c.label, theme.text_size), @max(22, line) },
            .slider => .{ 160, 24 },
            .progress => .{ 160, 8 },
            .list => .{ 220, 6 * theme.row_height + 8 },
            .separator => .{ 1, 1 },
            .box => |b| blk: {
                var main: i32 = 0;
                var cross: i32 = 0;
                var largest: i32 = 0;
                for (b.children, 0..) |c, i| {
                    const s = app.measure(c);
                    const m = if (b.vertical) s[1] else s[0];
                    const x = if (b.vertical) s[0] else s[1];
                    main += m + if (i > 0) b.spacing else 0;
                    largest = @max(largest, m);
                    cross = @max(cross, x);
                }
                const n: i32 = @intCast(b.children.len);
                if (b.homogeneous and n > 0) main = n * largest + (n - 1) * b.spacing;
                const p = 2 * b.padding;
                break :blk if (b.vertical) .{ cross + p, main + p } else .{ main + p, cross + p };
            },
        };
    }

    fn layout(app: *App, w: *Widget, r: Rect) void {
        w.rect = r;
        switch (w.kind) {
            .list => scrollList(w, 0), // keep the scroll in range for the new size
            .box => {},
            else => return,
        }
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
        var shared: i32 = 0;
        const count: i32 = @intCast(b.children.len);
        const even = @divTrunc((if (b.vertical) inner.h else inner.w) - (count - 1) * b.spacing, @max(count, 1));
        for (b.children, 0..) |c, ci| {
            const s = app.measure(c);
            var main = if (b.vertical) s[1] else s[0];
            if (b.homogeneous) {
                // The same for all; the last one also takes what rounding left over.
                main = if (ci + 1 == b.children.len) (if (b.vertical) inner.bottom() else inner.right()) - pos else even;
            } else if (c.expand and expanders > 0 and room > 0) {
                // The last one takes the remainder, so the room is all used.
                shared += 1;
                main += if (shared == expanders) room - @divTrunc(room, expanders) * (expanders - 1) else @divTrunc(room, expanders);
            }
            const cross_room = if (b.vertical) inner.w else inner.h;
            const want = if (b.vertical) s[0] else s[1];
            // Across a row: buttons, lists and boxes fill its height; the rest are centered.
            const fills_row = switch (c.kind) {
                .box, .button, .list, .separator => true,
                else => false,
            };
            const alignment = if (!b.vertical and c.alignment == .fill and !fills_row) Align.center else c.alignment;
            // A label in a column gets the whole width and places its text itself, so text that
            // grows later (a calculator's display) still fits.
            const whole = alignment == .fill or (b.vertical and c.kind == .label);
            const cross_size = if (whole) cross_room else @min(want, cross_room);
            const offset = if (whole) 0 else switch (alignment) {
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
        const ring = focused and app.focus_visible;
        switch (w.kind) {
            .box => |b| for (b.children) |child| app.draw(child, c),
            .label => |*l| {
                const asc: i32 = @intFromFloat(@round(app.font.ascender(l.size)));
                // Text wider than the label gets smaller to fit (a long number in a display).
                var size = l.size;
                const full = canvas_mod.measure(app.font, size, l.text());
                if (full > @as(f32, @floatFromInt(r.w)) and full > 0) size = @max(8, @floor(size * @as(f32, @floatFromInt(r.w)) / full));
                const lh: i32 = @intFromFloat(@ceil(app.font.lineHeight(l.size)));
                const tw = canvas_mod.measure(app.font, size, l.text());
                const room = @as(f32, @floatFromInt(r.w)) - tw;
                const x = @as(f32, @floatFromInt(r.x)) + switch (w.alignment) {
                    .center => @round(room / 2),
                    .end => room,
                    .fill, .start => 0,
                };
                _ = c.text(app.font, size, x, r.y + @divTrunc(r.h - lh, 2) + asc, l.text(), l.color);
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
                if (ring) c.roundRectOutline(r, theme.radius, 2, theme.accent_hover);
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
                if (ring) c.roundRectOutline(r, @as(f32, @floatFromInt(r.h)) / 2, 2, theme.accent_hover);
            },
            .check => |ch| {
                const square = Rect{ .x = r.x, .y = r.y + @divTrunc(r.h - 20, 2), .w = 20, .h = 20 };
                if (ch.on) {
                    c.roundRect(square, 5, if (hovered) theme.accent_hover else theme.accent);
                    const bx: f32 = @floatFromInt(square.x);
                    const by: f32 = @floatFromInt(square.y);
                    c.line(bx + 5, by + 10.5, bx + 8.5, by + 14, 2.2, 0xffffff);
                    c.line(bx + 8.5, by + 14, bx + 15, by + 6.5, 2.2, 0xffffff);
                } else {
                    c.roundRect(square, 5, if (hovered) theme.button_hover else theme.field);
                    c.roundRectOutline(square, 5, 1.5, theme.track);
                }
                if (ring) c.roundRectOutline(square, 5, 2, theme.accent_hover);
                _ = c.text(app.font, theme.text_size, @floatFromInt(r.x + 28), app.baseline(r), ch.label, theme.text);
            },
            .slider => |*s| {
                const cy = r.y + @divTrunc(r.h, 2);
                const left = r.x + 10;
                const width = r.w - 20;
                const knob_x = left + @as(i32, @intFromFloat(@round(s.fraction() * @as(f32, @floatFromInt(width)))));
                c.roundRect(.{ .x = left, .y = cy - 2, .w = width, .h = 4 }, 2, theme.track);
                c.roundRect(.{ .x = left, .y = cy - 2, .w = knob_x - left, .h = 4 }, 2, theme.accent);
                const kr: f32 = if (hovered or app.pressed == w) 10 else 9;
                c.circle(@floatFromInt(knob_x), @as(f32, @floatFromInt(cy)), kr, 0xffffff);
                if (ring) c.circle(@floatFromInt(knob_x), @as(f32, @floatFromInt(cy)), 4, theme.accent);
            },
            .progress => |p| {
                const bar = Rect{ .x = r.x, .y = r.y + @divTrunc(r.h - 8, 2), .w = r.w, .h = 8 };
                c.roundRect(bar, 4, theme.track);
                const filled: i32 = @intFromFloat(@round(std.math.clamp(p.fraction, 0, 1) * @as(f32, @floatFromInt(r.w))));
                if (filled > 0) c.roundRect(.{ .x = bar.x, .y = bar.y, .w = @max(filled, 8), .h = 8 }, 4, theme.accent);
            },
            .list => |*l| app.drawList(w, l, c, focused),
            .separator => c.rect(if (r.w >= r.h) .{ .x = r.x, .y = r.y + @divTrunc(r.h, 2), .w = r.w, .h = 1 } else .{ .x = r.x + @divTrunc(r.w, 2), .y = r.y, .w = 1, .h = r.h }, theme.field_border),
        }
    }

    fn drawList(app: *App, w: *Widget, l: *List, c: *Canvas, focused: bool) void {
        const r = w.rect;
        c.roundRect(r, theme.radius, theme.field);
        c.roundRectOutline(r, theme.radius, if (focused) 2 else 1, if (focused) theme.accent else theme.field_border);
        const inner = r.inset(4);
        const saved = c.clip;
        c.clip = inner.intersect(saved);
        defer c.clip = saved;
        const first: usize = @intCast(@divTrunc(l.scroll, theme.row_height));
        var i = first;
        while (i < l.len()) : (i += 1) {
            const y = inner.y + @as(i32, @intCast(i * theme.row_height)) - l.scroll;
            if (y >= inner.bottom()) break;
            const row = Rect{ .x = inner.x, .y = y, .w = inner.w, .h = theme.row_height };
            const is_selected = l.selected == i;
            const over = app.hover == w and row.contains(app.pointer_x, app.pointer_y);
            if (is_selected) {
                c.roundRect(row, 6, theme.accent);
            } else if (over) {
                c.roundRect(row, 6, theme.row_hover);
            }
            _ = c.text(app.font, theme.text_size, @floatFromInt(row.x + 10), app.baseline(row), l.item(i), if (is_selected) 0xffffff else theme.text);
        }
        // A scrollbar when there's more than fits.
        const total: i32 = @intCast(l.len() * theme.row_height);
        if (total > inner.h) {
            const bar_h = @max(24, @divTrunc(inner.h * inner.h, total));
            const bar_y = inner.y + @divTrunc((inner.h - bar_h) * l.scroll, total - inner.h);
            c.clip = saved;
            c.roundRect(.{ .x = r.right() - 7, .y = bar_y, .w = 4, .h = bar_h }, 2, theme.dim);
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

/// Replaces a list's rows (and clears its selection).
pub fn setItems(w: *Widget, items: []const []const u8) void {
    const l = &w.kind.list;
    const alloc = std.heap.page_allocator;
    var size: usize = 0;
    for (items) |s| size += s.len;
    const text = alloc.alloc(u8, size) catch return;
    const ends = alloc.alloc(u32, items.len) catch {
        alloc.free(text);
        return;
    };
    var at: usize = 0;
    for (items, 0..) |s, i| {
        @memcpy(text[at..][0..s.len], s);
        at += s.len;
        ends[i] = @intCast(at);
    }
    if (l.text.len > 0) alloc.free(l.text);
    if (l.ends.len > 0) alloc.free(l.ends);
    l.text = text;
    l.ends = ends;
    l.selected = null;
    l.scroll = 0;
}

/// The selected row of a list, if any.
pub fn selected(w: *const Widget) ?usize {
    return w.kind.list.selected;
}

pub fn setProgress(w: *Widget, fraction: f32) void {
    w.kind.progress.fraction = fraction;
}

fn setSlider(app: *App, s: *Slider, value: f32) void {
    const v = std.math.clamp(value, @min(s.min, s.max), @max(s.min, s.max));
    if (v == s.value) return;
    s.value = v;
    if (s.on_change) |cb| cb.f(app, cb.ctx);
}

fn scrollList(w: *Widget, by: i32) void {
    const l = &w.kind.list;
    const total: i32 = @intCast(l.len() * theme.row_height);
    const view = w.rect.h - 8;
    l.scroll = std.math.clamp(l.scroll + by, 0, @max(0, total - view));
}

fn nowMs() i64 {
    var ts: extern struct { sec: i64, nsec: i64 } = .{ .sec = 0, .nsec = 0 };
    _ = sys.syscall2(sys.nr.clock_gettime, 1, @intFromPtr(&ts)); // CLOCK_MONOTONIC
    return ts.sec * 1000 + @divTrunc(ts.nsec, 1_000_000);
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

fn collectFocusable(w: *Widget, found: *[64]*Widget, n: *usize) void {
    switch (w.kind) {
        .box => |b| for (b.children) |c| collectFocusable(c, found, n),
        else => if (w.focusable() and n.* < found.len) {
            found[n.*] = w;
            n.* += 1;
        },
    }
}

fn editEntry(app: *App, e: *Entry, k: win.Key) void {
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
