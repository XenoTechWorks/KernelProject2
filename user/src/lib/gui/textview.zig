//! A text area for editing many lines: a monospaced view of a text buffer (lib/textbuf.zig) with
//! line numbers, a cursor, a selection (shift and the arrows, or dragging with the mouse; a
//! double-click picks a word), the clipboard (Ctrl+C, Ctrl+X, Ctrl+V), undo (Ctrl+Z) and scrolling.

const std = @import("std");
const textbuf = @import("../textbuf.zig");
const canvas_mod = @import("canvas.zig");
const Canvas = canvas_mod.Canvas;
const Rect = canvas_mod.Rect;
const Font = @import("font.zig").Font;
const win = @import("window.zig");

pub const tab = 4;
const pad = 8;

pub const colors = struct {
    pub const bg = 0x1a1d23;
    pub const gutter = 0x16191e;
    pub const numbers = 0x5c6370;
    pub const current_number = 0xe5c07b;
    pub const current_line = 0x1f232a;
    pub const text = 0xe6e9ef;
    pub const selection = 0x2a4a75;
    pub const cursor = 0x82c4ff;
    pub const border = 0x3a404d;
    pub const focus = 0x3584e4;
};

pub const TextView = struct {
    buf: textbuf.Buffer,
    font: *Font,
    size: f32 = 14,
    cursor: usize = 0,
    /// The other end of the selection (the cursor is one end).
    anchor: ?usize = null,
    /// The column to aim for when moving up and down through shorter lines.
    goal_col: ?usize = null,
    scroll_x: i32 = 0,
    scroll_y: i32 = 0,
    line_numbers: bool = true,
    last_click: struct { at: usize = std.math.maxInt(usize), ms: i64 = 0 } = .{},

    pub fn init(font: *Font) TextView {
        return .{ .buf = textbuf.Buffer.init(), .font = font };
    }

    fn advance(tv: *const TextView) f32 {
        return tv.font.advance(tv.font.glyphIndex('M'), @round(tv.size));
    }

    fn lineHeight(tv: *const TextView) i32 {
        return @as(i32, @intFromFloat(@ceil(tv.font.lineHeight(tv.size)))) + 2;
    }

    fn gutterWidth(tv: *const TextView) i32 {
        if (!tv.line_numbers) return 0;
        const digits = std.fmt.count("{d}", .{tv.buf.lines()});
        return @as(i32, @intFromFloat(@ceil(@as(f32, @floatFromInt(@max(digits, 3))) * tv.advance()))) + 2 * pad;
    }

    /// Where the text itself goes inside the widget.
    fn textArea(tv: *const TextView, r: Rect) Rect {
        const g = tv.gutterWidth();
        return .{ .x = r.x + 1 + g + pad, .y = r.y + pad, .w = r.w - 2 - g - 2 * pad, .h = r.h - 2 * pad };
    }

    /// The selection, start before end; null when nothing is selected.
    pub fn selection(tv: *const TextView) ?[2]usize {
        const a = tv.anchor orelse return null;
        if (a == tv.cursor) return null;
        return .{ @min(a, tv.cursor), @max(a, tv.cursor) };
    }

    pub fn selectedText(tv: *const TextView) []const u8 {
        const s = tv.selection() orelse return "";
        return tv.buf.slice(s[0], s[1]);
    }

    fn deleteSelection(tv: *TextView) bool {
        const s = tv.selection() orelse return false;
        tv.buf.delete(s[0], s[1]);
        tv.cursor = s[0];
        tv.anchor = null;
        return true;
    }

    /// Types (or pastes) text over the selection, as one step to undo.
    pub fn insertText(tv: *TextView, s: []const u8) void {
        if (tv.deleteSelection()) tv.buf.groupNext();
        tv.cursor = tv.buf.insert(tv.cursor, s);
        tv.anchor = null;
    }

    /// Moves the cursor; with `select` the selection stretches, without it the selection ends.
    fn moveTo(tv: *TextView, at: usize, select: bool) void {
        if (select) {
            if (tv.anchor == null) tv.anchor = tv.cursor;
        } else {
            tv.anchor = null;
        }
        tv.cursor = at;
    }

    /// Handles a key. Returns true if it did something (the text or the cursor changed).
    pub fn key(tv: *TextView, k: win.Key, view_h: i32) bool {
        const b = &tv.buf;
        const ln = b.lineOf(tv.cursor);
        const vertical = k.code == win.Key.up or k.code == win.Key.down or k.code == win.Key.page_up or k.code == win.Key.page_down;
        if (!vertical) tv.goal_col = null;
        if (k.ctrl) {
            switch (k.code) {
                'a' => {
                    tv.anchor = 0;
                    tv.cursor = b.len();
                },
                'c' => if (tv.selection() != null) textbuf.copyToClipboard(tv.selectedText()),
                'x' => if (tv.selection() != null) {
                    textbuf.copyToClipboard(tv.selectedText());
                    _ = tv.deleteSelection();
                },
                'v' => {
                    var clip: [64 * 1024]u8 = undefined;
                    tv.insertText(textbuf.readClipboard(&clip));
                },
                'z' => if (b.undoLast()) |at| {
                    tv.cursor = @min(at, b.len());
                    tv.anchor = null;
                },
                win.Key.left => tv.moveTo(b.wordStart(tv.cursor), k.shift),
                win.Key.right => tv.moveTo(b.wordEnd(tv.cursor), k.shift),
                win.Key.home => tv.moveTo(0, k.shift),
                win.Key.end => tv.moveTo(b.len(), k.shift),
                else => return false,
            }
            return true;
        }
        switch (k.code) {
            win.Key.left => {
                const s = tv.selection();
                if (s != null and !k.shift) tv.moveTo(s.?[0], false) else tv.moveTo(b.prev(tv.cursor), k.shift);
            },
            win.Key.right => {
                const s = tv.selection();
                if (s != null and !k.shift) tv.moveTo(s.?[1], false) else tv.moveTo(b.next(tv.cursor), k.shift);
            },
            win.Key.home => {
                // To the first non-blank character, then (pressed again) the very start.
                const start = b.lineStart(ln);
                var first = start;
                while (first < b.lineEnd(ln) and (b.slice(first, first + 1)[0] == ' ' or b.slice(first, first + 1)[0] == '\t')) first += 1;
                tv.moveTo(if (tv.cursor == first) start else first, k.shift);
            },
            win.Key.end => tv.moveTo(b.lineEnd(ln), k.shift),
            win.Key.up, win.Key.down, win.Key.page_up, win.Key.page_down => {
                const goal = tv.goal_col orelse b.column(tv.cursor, tab);
                tv.goal_col = goal;
                const page: usize = @intCast(@max(1, @divTrunc(view_h, tv.lineHeight()) - 1));
                const target: usize = switch (k.code) {
                    win.Key.up => ln -| 1,
                    win.Key.down => @min(ln + 1, b.lines() - 1),
                    win.Key.page_up => ln -| page,
                    else => @min(ln + page, b.lines() - 1),
                };
                // Up from the first line goes to its start, down from the last to its end.
                const at = if (k.code == win.Key.up and ln == 0) 0 else if (k.code == win.Key.down and ln + 1 == b.lines()) b.len() else b.atColumn(target, goal, tab);
                tv.moveTo(at, k.shift);
            },
            8 => if (!tv.deleteSelection() and tv.cursor > 0) {
                const from = b.prev(tv.cursor);
                b.delete(from, tv.cursor);
                tv.cursor = from;
            },
            127 => if (!tv.deleteSelection()) b.delete(tv.cursor, b.next(tv.cursor)),
            13 => {
                // A new line, indented like this one.
                const line = b.line(ln);
                var indent: usize = 0;
                while (indent < line.len and (line[indent] == ' ' or line[indent] == '\t')) indent += 1;
                var tmp: [256]u8 = undefined;
                tmp[0] = '\n';
                const n = @min(indent, tmp.len - 1);
                @memcpy(tmp[1..][0..n], line[0..n]);
                tv.insertText(tmp[0 .. n + 1]);
            },
            9 => tv.insertText("\t"),
            else => {
                if (k.code < 32 or k.code >= 127) return false;
                tv.insertText(&.{@intCast(k.code)});
            },
        }
        return true;
    }

    /// The text position nearest a point in the widget `r`.
    fn positionAt(tv: *const TextView, r: Rect, x: i32, y: i32) usize {
        const area = tv.textArea(r);
        const row = @divFloor(y - area.y + tv.scroll_y, tv.lineHeight());
        const ln: usize = @intCast(std.math.clamp(row, 0, @as(i32, @intCast(tv.buf.lines() - 1))));
        if (row < 0) return 0;
        if (row >= tv.buf.lines()) return tv.buf.len();
        const col_f = @as(f32, @floatFromInt(x - area.x + tv.scroll_x)) / tv.advance();
        const col: usize = @intFromFloat(@max(0, @round(col_f)));
        return tv.buf.atColumn(ln, col, tab);
    }

    /// A mouse button went down at (x, y): put the cursor there (a double-click selects a word).
    pub fn press(tv: *TextView, r: Rect, x: i32, y: i32, now_ms: i64) void {
        const at = tv.positionAt(r, x, y);
        tv.goal_col = null;
        if (tv.last_click.at == at and now_ms - tv.last_click.ms < 450) {
            tv.anchor = tv.buf.wordStart(@min(at + 1, tv.buf.len()));
            tv.cursor = tv.buf.wordEnd(tv.anchor.?);
            tv.last_click = .{};
            return;
        }
        tv.last_click = .{ .at = at, .ms = now_ms };
        tv.cursor = at;
        tv.anchor = at;
    }

    /// The pointer moved with the button held: stretch the selection to it.
    pub fn drag(tv: *TextView, r: Rect, x: i32, y: i32) void {
        if (tv.anchor == null) tv.anchor = tv.cursor;
        tv.cursor = tv.positionAt(r, x, y);
        tv.ensureVisible(r);
    }

    pub fn scroll(tv: *TextView, r: Rect, lines: i32) void {
        tv.scroll_y += lines * tv.lineHeight();
        tv.clampScroll(r);
    }

    fn clampScroll(tv: *TextView, r: Rect) void {
        const area = tv.textArea(r);
        const total: i32 = @intCast(tv.buf.lines() * @as(usize, @intCast(tv.lineHeight())));
        tv.scroll_y = std.math.clamp(tv.scroll_y, 0, @max(0, total - area.h));
        tv.scroll_x = @max(0, tv.scroll_x);
    }

    /// Scrolls so the cursor shows.
    pub fn ensureVisible(tv: *TextView, r: Rect) void {
        const area = tv.textArea(r);
        const lh = tv.lineHeight();
        const top: i32 = @as(i32, @intCast(tv.buf.lineOf(tv.cursor))) * lh;
        if (top < tv.scroll_y) tv.scroll_y = top;
        if (top + lh > tv.scroll_y + area.h) tv.scroll_y = top + lh - area.h;
        const x: i32 = @intFromFloat(@as(f32, @floatFromInt(tv.buf.column(tv.cursor, tab))) * tv.advance());
        const margin: i32 = @intFromFloat(tv.advance() * 4);
        if (x < tv.scroll_x) tv.scroll_x = @max(0, x - margin);
        if (x > tv.scroll_x + area.w - 2) tv.scroll_x = x - area.w + margin;
        tv.clampScroll(r);
    }

    pub fn draw(tv: *TextView, c: *Canvas, r: Rect, focused: bool) void {
        c.roundRect(r, 8, colors.bg);
        const saved = c.clip;
        defer c.clip = saved;
        const inner = r.inset(1);
        c.clip = inner.intersect(saved);
        const g = tv.gutterWidth();
        if (g > 0) c.rect(.{ .x = inner.x, .y = inner.y, .w = g, .h = inner.h }, colors.gutter);

        const area = tv.textArea(r);
        const lh = tv.lineHeight();
        const adv = tv.advance();
        const asc: i32 = @intFromFloat(@round(tv.font.ascender(tv.size)));
        const b = &tv.buf;
        const cur_line = b.lineOf(tv.cursor);
        const sel = tv.selection();
        const first: usize = @intCast(@divTrunc(tv.scroll_y, lh));
        var ln = first;
        while (ln < b.lines()) : (ln += 1) {
            const y = area.y + @as(i32, @intCast(ln)) * lh - tv.scroll_y;
            if (y >= inner.bottom()) break;
            // The line's number in the gutter.
            if (g > 0) {
                var num: [12]u8 = undefined;
                const s = std.fmt.bufPrint(&num, "{d}", .{ln + 1}) catch "";
                const nx = @as(f32, @floatFromInt(inner.x + g - pad)) - @as(f32, @floatFromInt(s.len)) * adv;
                c.clip = inner.intersect(saved);
                _ = c.text(tv.font, tv.size, nx, y + asc + 1, s, if (ln == cur_line and focused) colors.current_number else colors.numbers);
            }
            c.clip = (Rect{ .x = area.x - pad, .y = inner.y, .w = area.w + 2 * pad, .h = inner.h }).intersect(inner).intersect(saved);
            const row = Rect{ .x = area.x - pad, .y = y, .w = area.w + 2 * pad, .h = lh };
            if (focused and sel == null and ln == cur_line) c.rect(row, colors.current_line);
            // The selection on this line (one more cell where it carries on past the line's end).
            const ls = b.lineStart(ln);
            const le = b.lineEnd(ln);
            if (sel) |s| if (s[0] <= le and s[1] >= ls) {
                const from = b.column(@max(s[0], ls), tab);
                const to = b.column(@min(s[1], le), tab) + @intFromBool(s[1] > le);
                const x0 = area.x + @as(i32, @intFromFloat(@as(f32, @floatFromInt(from)) * adv)) - tv.scroll_x;
                const x1 = area.x + @as(i32, @intFromFloat(@as(f32, @floatFromInt(to)) * adv)) - tv.scroll_x;
                c.rect(.{ .x = x0, .y = y, .w = x1 - x0, .h = lh }, colors.selection);
            };
            // The text, one character per cell.
            var col: usize = 0;
            var i = ls;
            while (i < le) {
                const next = b.next(i);
                const ch = b.slice(i, next);
                if (ch[0] == '\t') {
                    col = (col / tab + 1) * tab;
                } else {
                    const x = @as(f32, @floatFromInt(area.x - tv.scroll_x)) + @as(f32, @floatFromInt(col)) * adv;
                    if (x > @as(f32, @floatFromInt(inner.right()))) break;
                    if (x + adv >= @as(f32, @floatFromInt(area.x - pad))) _ = c.text(tv.font, tv.size, x, y + asc + 1, ch, colors.text);
                    col += 1;
                }
                i = next;
            }
        }
        // The cursor.
        if (focused) {
            const cl: i32 = @intCast(cur_line);
            const cx = area.x + @as(i32, @intFromFloat(@as(f32, @floatFromInt(b.column(tv.cursor, tab))) * adv)) - tv.scroll_x;
            const cy = area.y + cl * lh - tv.scroll_y;
            c.rect(.{ .x = cx, .y = cy + 1, .w = 2, .h = lh - 2 }, colors.cursor);
        }
        c.clip = saved;
        c.roundRectOutline(r, 8, if (focused) 2 else 1, if (focused) colors.focus else colors.border);
    }
};
