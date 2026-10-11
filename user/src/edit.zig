//! edit: a text editor for the terminal, in the spirit of nano.
//!
//!     edit notes.txt
//!
//! Arrows, Home/End and Page Up/Down move; typing inserts. Ctrl+S saves, Ctrl+Q quits (twice if
//! there are unsaved changes), Ctrl+F finds, Ctrl+G goes to a line, Ctrl+Z undoes. Ctrl+K cuts the
//! line (several in a row add up), Ctrl+C copies it, Ctrl+U pastes: through the shared clipboard,
//! so text moves between programs and windows too.

const std = @import("std");
const rt = @import("lib/rt.zig");
const textbuf = @import("lib/textbuf.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const tab_width = 4;

var b: textbuf.Buffer = undefined;
var path: [:0]const u8 = undefined;
var cursor: usize = 0;
/// The column to aim for when moving up and down through shorter lines.
var goal_col: ?usize = null;
var top: usize = 0;
var left: usize = 0;
var rows: usize = 25;
var cols: usize = 80;
var message: [200]u8 = undefined;
var message_len: usize = 0;
var quit_armed = false;
/// Lines cut with Ctrl+K one after another, all on the clipboard together.
var cut: [64 * 1024]u8 = undefined;
var cut_len: usize = 0;
var last_was_cut = false;

// ---- the terminal ----

const Termios = extern struct { iflag: u32, oflag: u32, cflag: u32, lflag: u32, line: u8, cc: [19]u8 };
var saved_termios: Termios = undefined;

fn ioctl(fd: usize, request: usize, arg: usize) sys.Result {
    return sys.check(sys.syscall3(16, fd, request, arg));
}

/// Keys arrive one at a time, unechoed, with Ctrl+C and Ctrl+Z as ordinary keys.
fn rawMode() bool {
    if (ioctl(0, 0x5401, @intFromPtr(&saved_termios)) != .ok) return false; // TCGETS
    var t = saved_termios;
    t.lflag &= ~@as(u32, 0o2 | 0o10 | 0o1 | 0o100000); // ICANON ECHO ISIG IEXTEN
    t.iflag &= ~@as(u32, 0o400 | 0o2000); // ICRNL IXON
    t.cc[6] = 1; // VMIN
    t.cc[5] = 0; // VTIME
    return ioctl(0, 0x5402, @intFromPtr(&t)) == .ok; // TCSETS
}

fn restore() void {
    _ = ioctl(0, 0x5402, @intFromPtr(&saved_termios));
}

fn readSize() void {
    var ws: [4]u16 = .{ 0, 0, 0, 0 };
    if (ioctl(1, 0x5413, @intFromPtr(&ws)) == .ok and ws[0] > 3 and ws[1] > 10) { // TIOCGWINSZ
        rows = ws[0];
        cols = ws[1];
    }
}

const Key = union(enum) {
    char: u8,
    ctrl: u8, // the letter
    enter,
    backspace,
    delete,
    tab,
    escape,
    up,
    down,
    left,
    right,
    home,
    end,
    page_up,
    page_down,
};

var pending: [64]u8 = undefined;
var pending_len: usize = 0;

fn readKey() ?Key {
    if (pending_len == 0) {
        switch (sys.read(0, &pending)) {
            .ok => |n| {
                if (n == 0) return null;
                pending_len = n;
            },
            .err => |e| return if (e == sys.EINTR) readKey() else null,
        }
    }
    const in = pending[0..pending_len];
    var used: usize = 1;
    const key: Key = switch (in[0]) {
        13, 10 => .enter,
        127, 8 => .backspace,
        9 => .tab,
        1...7, 11, 12, 14...26 => .{ .ctrl = in[0] - 1 + 'a' },
        27 => blk: {
            if (in.len >= 3 and in[1] == '[') {
                used = 3;
                switch (in[2]) {
                    'A' => break :blk .up,
                    'B' => break :blk .down,
                    'C' => break :blk .right,
                    'D' => break :blk .left,
                    'H' => break :blk .home,
                    'F' => break :blk .end,
                    '3', '5', '6' => if (in.len >= 4 and in[3] == '~') {
                        used = 4;
                        break :blk switch (in[2]) {
                            '3' => .delete,
                            '5' => .page_up,
                            else => .page_down,
                        };
                    },
                    else => {},
                }
            }
            used = 1;
            break :blk .escape;
        },
        else => .{ .char = in[0] },
    };
    std.mem.copyForwards(u8, pending[0..], pending[used..pending_len]);
    pending_len -= used;
    return key;
}

// ---- positions and columns ----

fn column(at: usize) usize {
    return b.column(at, tab_width);
}

fn atColumn(ln: usize, want: usize) usize {
    return b.atColumn(ln, want, tab_width);
}

// ---- drawing ----

var screen: [256 * 1024]u8 = undefined;
var screen_len: usize = 0;

fn put(s: []const u8) void {
    const n = @min(s.len, screen.len - screen_len);
    @memcpy(screen[screen_len..][0..n], s[0..n]);
    screen_len += n;
}

fn print(comptime fmt: []const u8, args: anytype) void {
    const s = std.fmt.bufPrint(screen[screen_len..], fmt, args) catch return;
    screen_len += s.len;
}

fn say(comptime fmt: []const u8, args: anytype) void {
    const s = std.fmt.bufPrint(&message, fmt, args) catch message[0..0];
    message_len = s.len;
}

fn gutterWidth() usize {
    return std.fmt.count("{d}", .{b.lines()}) + 1;
}

fn draw(prompt: ?[]const u8) void {
    readSize();
    const text_rows = rows - 2;
    const gutter = gutterWidth();
    const text_cols = cols - gutter - 1;
    const cur_line = b.lineOf(cursor);
    const cur_col = column(cursor);
    // Keep the cursor on the screen.
    if (cur_line < top) top = cur_line;
    if (cur_line >= top + text_rows) top = cur_line - text_rows + 1;
    if (cur_col < left) left = cur_col;
    if (cur_col >= left + text_cols) left = cur_col - text_cols + 1;

    screen_len = 0;
    put("\x1b[?25l");
    for (0..text_rows) |r| {
        print("\x1b[{d};1H", .{r + 1});
        const ln = top + r;
        if (ln < b.lines()) {
            print("\x1b[{s}m{d: >[2]}\x1b[0m ", .{ if (ln == cur_line) "33" else "90", ln + 1, gutter });
            // The visible part of the line, tabs spread to spaces.
            var col: usize = 0;
            var shown: usize = 0;
            for (b.line(ln)) |c| {
                if (c == '\t') {
                    const next_col = (col / tab_width + 1) * tab_width;
                    while (col < next_col) : (col += 1) {
                        if (col >= left and shown < text_cols) {
                            put(" ");
                            shown += 1;
                        }
                    }
                    continue;
                }
                const starts_char = c & 0xC0 != 0x80;
                if (starts_char) col += 1;
                if (col - 1 >= left and (shown < text_cols or !starts_char)) {
                    put(&.{c});
                    if (starts_char) shown += 1;
                }
            }
        } else {
            put("\x1b[90m~\x1b[0m");
        }
        put("\x1b[K");
    }
    // The status bar.
    print("\x1b[{d};1H\x1b[7m", .{rows - 1});
    var status: [256]u8 = undefined;
    const left_part = std.fmt.bufPrint(&status, " edit  {s}{s}", .{ path, if (b.modified) "  (modified)" else "" }) catch "";
    var right_buf: [64]u8 = undefined;
    const right_part = std.fmt.bufPrint(&right_buf, "Ln {d}, Col {d} ", .{ cur_line + 1, cur_col + 1 }) catch "";
    put(left_part[0..@min(left_part.len, cols)]);
    var used = @min(left_part.len, cols);
    while (used + right_part.len < cols) : (used += 1) put(" ");
    if (used + right_part.len <= cols) put(right_part);
    put("\x1b[0m");
    // The message line: a prompt, a message, or the keys.
    print("\x1b[{d};1H\x1b[K", .{rows});
    if (prompt) |p| {
        put(p);
    } else if (message_len > 0) {
        put(message[0..message_len]);
    } else {
        put("\x1b[7m^S\x1b[0m Save  \x1b[7m^Q\x1b[0m Quit  \x1b[7m^F\x1b[0m Find  \x1b[7m^G\x1b[0m Go to line  \x1b[7m^K\x1b[0m Cut line  \x1b[7m^C\x1b[0m Copy line  \x1b[7m^U\x1b[0m Paste  \x1b[7m^Z\x1b[0m Undo");
    }
    if (prompt) |p| {
        print("\x1b[{d};{d}H", .{ rows, p.len + 1 });
    } else {
        print("\x1b[{d};{d}H", .{ cur_line - top + 1, gutter + 1 + cur_col - left + 1 });
    }
    put("\x1b[?25h");
    rt.writeAll(1, screen[0..screen_len]);
}

// ---- commands ----

/// Asks a question on the message line. Returns the answer, or null if Escape was pressed.
fn ask(question: []const u8, answer: []u8) ?[]const u8 {
    var n: usize = 0;
    while (true) {
        var line: [300]u8 = undefined;
        draw(std.fmt.bufPrint(&line, "{s}{s}", .{ question, answer[0..n] }) catch question);
        const key = readKey() orelse return null;
        switch (key) {
            .enter => return answer[0..n],
            .escape, .ctrl => return null,
            .backspace => n -|= 1,
            .char => |c| if (c >= 32 and n < answer.len) {
                answer[n] = c;
                n += 1;
            },
            else => {},
        }
    }
}

var last_find: [100]u8 = undefined;
var last_find_len: usize = 0;

fn find() void {
    var answer: [100]u8 = undefined;
    const q = ask("Find: ", &answer) orelse return;
    const query = if (q.len > 0) q else last_find[0..last_find_len];
    if (query.len == 0) return;
    @memcpy(last_find[0..query.len], query);
    last_find_len = query.len;
    const text = b.slice(0, b.len());
    const from = @min(cursor + 1, text.len);
    if (std.mem.indexOfPos(u8, text, from, query) orelse std.mem.indexOf(u8, text, query)) |at| {
        if (at < cursor) say("Found (from the top)", .{}) else message_len = 0;
        cursor = at;
    } else {
        say("Not found: {s}", .{query});
    }
}

fn goToLine() void {
    var answer: [16]u8 = undefined;
    const a = ask("Go to line: ", &answer) orelse return;
    const n = std.fmt.parseInt(usize, a, 10) catch return say("Not a line number: {s}", .{a});
    cursor = b.lineStart(@min(@max(n, 1), b.lines()) - 1);
}

/// The current line with its newline: (start, end).
fn currentLine() struct { usize, usize } {
    const ln = b.lineOf(cursor);
    const end = if (ln + 1 < b.lines()) b.lineStart(ln + 1) else b.lineEnd(ln);
    return .{ b.lineStart(ln), end };
}

fn cutLine() void {
    const start, const end = currentLine();
    if (!last_was_cut) cut_len = 0;
    const text = b.slice(start, end);
    const n = @min(text.len, cut.len - cut_len);
    @memcpy(cut[cut_len..][0..n], text[0..n]);
    cut_len += n;
    textbuf.copyToClipboard(cut[0..cut_len]);
    b.delete(start, end);
    cursor = @min(start, b.len());
}

fn save() void {
    const err = b.save(path);
    if (err != 0) return say("Can't save {s}: {s}", .{ path, sys.errorText(err) });
    say("Saved {s} ({d} lines)", .{ path, b.lines() });
}

fn handle(key: Key) bool {
    const was_cut = last_was_cut;
    last_was_cut = false;
    const vertical = switch (key) {
        .up, .down, .page_up, .page_down => true,
        else => false,
    };
    if (!vertical) goal_col = null;
    if (key != .ctrl or key.ctrl != 'q') quit_armed = false;
    const ln = b.lineOf(cursor);
    const page = rows -| 3;
    switch (key) {
        .char => |c| if (c >= 32 or c >= 0x80) {
            cursor = b.insert(cursor, &.{c});
        },
        .enter => {
            // Keep the indentation of the line.
            const line = b.line(ln);
            var indent: usize = 0;
            while (indent < line.len and (line[indent] == ' ' or line[indent] == '\t')) indent += 1;
            var tmp: [256]u8 = undefined;
            tmp[0] = '\n';
            const n = @min(indent, tmp.len - 1);
            @memcpy(tmp[1..][0..n], line[0..n]);
            cursor = b.insert(cursor, tmp[0 .. n + 1]);
        },
        .tab => cursor = b.insert(cursor, "\t"),
        .backspace => if (cursor > 0) {
            const from = b.prev(cursor);
            b.delete(from, cursor);
            cursor = from;
        },
        .delete => b.delete(cursor, b.next(cursor)),
        .left => cursor = b.prev(cursor),
        .right => cursor = b.next(cursor),
        .home => cursor = b.lineStart(ln),
        .end => cursor = b.lineEnd(ln),
        .up, .down, .page_up, .page_down => {
            const goal = goal_col orelse column(cursor);
            goal_col = goal;
            const target: usize = switch (key) {
                .up => ln -| 1,
                .down => @min(ln + 1, b.lines() - 1),
                .page_up => ln -| page,
                else => @min(ln + page, b.lines() - 1),
            };
            cursor = atColumn(target, goal);
        },
        .escape => message_len = 0,
        .ctrl => |c| switch (c) {
            's' => save(),
            'q' => {
                if (!b.modified or quit_armed) return false;
                quit_armed = true;
                say("Unsaved changes! Ctrl+S saves, Ctrl+Q again quits without saving.", .{});
            },
            'f' => find(),
            'g' => goToLine(),
            'z' => if (b.undoLast()) |at| {
                cursor = at;
            } else say("Nothing to undo", .{}),
            'k' => {
                last_was_cut = was_cut;
                cutLine();
                last_was_cut = true;
            },
            'c' => {
                const start, const end = currentLine();
                textbuf.copyToClipboard(b.slice(start, end));
                say("Copied the line", .{});
            },
            'u', 'v' => {
                var clip: [64 * 1024]u8 = undefined;
                cursor = b.insert(cursor, textbuf.readClipboard(&clip));
            },
            else => {},
        },
    }
    return true;
}

pub fn main(args: rt.Args) u8 {
    const arg = args.get(1) orelse {
        rt.err.write("usage: edit FILE\n");
        return 2;
    };
    path = std.mem.span(args.argv[1]);
    _ = arg;
    b = textbuf.Buffer.init();
    if (!b.load(path)) {
        rt.err.print("edit: can't read {s}\n", .{path});
        return 1;
    }
    if (!rawMode()) {
        rt.err.write("edit: needs a terminal\n");
        return 1;
    }
    if (b.len() == 0) say("New file: {s}", .{path});
    while (true) {
        draw(null);
        const key = readKey() orelse break;
        if (message_len > 0 and key != .ctrl) message_len = 0;
        if (!handle(key)) break;
    }
    restore();
    rt.writeAll(1, "\x1b[2J\x1b[H");
    return 0;
}
