//! A text being edited, shared by the editors (`edit` in the terminal, `textedit` in a window).
//!
//! The whole text is one array of bytes, positions are byte offsets into it, and a list of where
//! each line starts is rebuilt after every change. Simple, and quick enough for files of normal
//! size. Changes can be undone; typing a word undoes in one step.

const std = @import("std");
const sys = @import("sys.zig");

const gpa = std.heap.page_allocator;

const Change = struct {
    /// Inserted (undo removes it) or deleted (undo puts it back).
    inserted: bool,
    at: usize,
    /// Where the changed text is kept, in `Buffer.undo_text`.
    start: usize,
    len: usize,
    /// Undone together with the change before it (replacing a selection is one step).
    group: bool = false,
};

pub const Buffer = struct {
    text: std.ArrayList(u8) = .empty,
    /// Byte offset where each line starts; there's always at least one line.
    starts: std.ArrayList(usize) = .empty,
    modified: bool = false,
    undo: std.ArrayList(Change) = .empty,
    undo_text: std.ArrayList(u8) = .empty,
    /// The last change can grow by the next typed character (to undo a word at once).
    can_merge: bool = false,
    /// The next change is undone together with the one before it; see groupNext().
    group_next: bool = false,

    pub fn init() Buffer {
        var b = Buffer{};
        b.reindex();
        return b;
    }

    /// Reads a file; a missing file is an empty buffer (it's created on save). False on other errors.
    pub fn load(b: *Buffer, path: [*:0]const u8) bool {
        b.text.clearRetainingCapacity();
        b.undo.clearRetainingCapacity();
        b.undo_text.clearRetainingCapacity();
        b.modified = false;
        defer b.reindex();
        const fd: i32 = switch (sys.open(path, sys.O_RDONLY, 0)) {
            .ok => |fd| @intCast(fd),
            .err => |e| return e == sys.ENOENT,
        };
        defer sys.close(fd);
        var chunk: [16384]u8 = undefined;
        while (true) {
            switch (sys.read(fd, &chunk)) {
                .ok => |n| {
                    if (n == 0) break;
                    b.text.appendSlice(gpa, chunk[0..n]) catch return false;
                },
                .err => return false,
            }
        }
        return true;
    }

    /// Writes the text to a file. Returns the error number, or 0.
    pub fn save(b: *Buffer, path: [*:0]const u8) u16 {
        const fd: i32 = switch (sys.open(path, sys.O_WRONLY | sys.O_CREAT | sys.O_TRUNC, 0o644)) {
            .ok => |fd| @intCast(fd),
            .err => |e| return e,
        };
        defer sys.close(fd);
        var done: usize = 0;
        while (done < b.text.items.len) {
            switch (sys.write(fd, b.text.items[done..])) {
                .ok => |n| done += n,
                .err => |e| return e,
            }
        }
        b.modified = false;
        b.can_merge = false;
        return 0;
    }

    pub fn len(b: *const Buffer) usize {
        return b.text.items.len;
    }

    pub fn lines(b: *const Buffer) usize {
        return b.starts.items.len;
    }

    pub fn lineStart(b: *const Buffer, ln: usize) usize {
        return b.starts.items[ln];
    }

    /// Where a line ends (before its newline).
    pub fn lineEnd(b: *const Buffer, ln: usize) usize {
        return if (ln + 1 < b.starts.items.len) b.starts.items[ln + 1] - 1 else b.text.items.len;
    }

    pub fn line(b: *const Buffer, i: usize) []const u8 {
        return b.text.items[b.lineStart(i)..b.lineEnd(i)];
    }

    /// The line a position is on.
    pub fn lineOf(b: *const Buffer, at: usize) usize {
        var lo: usize = 0;
        var hi: usize = b.starts.items.len;
        while (hi - lo > 1) {
            const mid = (lo + hi) / 2;
            if (b.starts.items[mid] <= at) lo = mid else hi = mid;
        }
        return lo;
    }

    pub fn slice(b: *const Buffer, from: usize, to: usize) []const u8 {
        return b.text.items[from..to];
    }

    /// The start of the character before `at` (characters are UTF-8, one to four bytes).
    pub fn prev(b: *const Buffer, at: usize) usize {
        var i = at -| 1;
        while (i > 0 and b.text.items[i] & 0xC0 == 0x80) i -= 1;
        return i;
    }

    /// The start of the character after `at`.
    pub fn next(b: *const Buffer, at: usize) usize {
        if (at >= b.text.items.len) return b.text.items.len;
        var i = at + 1;
        while (i < b.text.items.len and b.text.items[i] & 0xC0 == 0x80) i += 1;
        return i;
    }

    /// Where the word around/before `at` starts, and where the one at/after it ends (for
    /// Ctrl+arrows and double-clicks).
    pub fn wordStart(b: *const Buffer, at: usize) usize {
        var i = at;
        while (i > 0 and !isWord(b.text.items[i - 1])) i -= 1;
        while (i > 0 and isWord(b.text.items[i - 1])) i -= 1;
        return i;
    }
    pub fn wordEnd(b: *const Buffer, at: usize) usize {
        var i = at;
        const t = b.text.items;
        while (i < t.len and !isWord(t[i])) i += 1;
        while (i < t.len and isWord(t[i])) i += 1;
        return i;
    }

    /// The column of a position within its line, counting characters, with tabs reaching the next
    /// multiple of `tab` (for monospaced display).
    pub fn column(b: *const Buffer, at: usize, tab: usize) usize {
        var col: usize = 0;
        for (b.text.items[b.lineStart(b.lineOf(at))..at]) |c| {
            if (c == '\t') {
                col = (col / tab + 1) * tab;
            } else if (c & 0xC0 != 0x80) {
                col += 1;
            }
        }
        return col;
    }

    /// The position on a line closest to (not past) a column.
    pub fn atColumn(b: *const Buffer, ln: usize, want: usize, tab: usize) usize {
        var at = b.lineStart(ln);
        const end = b.lineEnd(ln);
        var col: usize = 0;
        while (at < end) {
            const c = b.text.items[at];
            const w = if (c == '\t') (col / tab + 1) * tab - col else 1;
            // Past the middle of this character: the position after it is closer.
            if (col + w > want) {
                if (want - col > w / 2 and w > 1) at = b.next(at);
                break;
            }
            col += w;
            at = b.next(at);
        }
        return at;
    }

    /// The next change joins the last one, so both are undone in one step (deleting a selection
    /// and then typing over it, say).
    pub fn groupNext(b: *Buffer) void {
        b.group_next = true;
    }

    /// Inserts text at a position; returns the position after it.
    pub fn insert(b: *Buffer, at: usize, s: []const u8) usize {
        if (s.len == 0) return at;
        b.text.insertSlice(gpa, at, s) catch return at;
        b.record(true, at, s);
        b.modified = true;
        b.reindex();
        return at + s.len;
    }

    /// Removes the text between two positions.
    pub fn delete(b: *Buffer, from: usize, to: usize) void {
        if (to <= from) return;
        b.record(false, from, b.text.items[from..to]);
        b.can_merge = false;
        b.text.replaceRange(gpa, from, to - from, &.{}) catch return;
        b.modified = true;
        b.reindex();
    }

    /// Undoes the last change (with the ones grouped onto it). Returns where the cursor should go,
    /// or null if there's nothing to undo.
    pub fn undoLast(b: *Buffer) ?usize {
        var cursor: ?usize = null;
        while (true) {
            const c = b.undo.pop() orelse break;
            const saved = b.undo_text.items[c.start .. c.start + c.len];
            if (c.inserted) {
                b.text.replaceRange(gpa, c.at, c.len, &.{}) catch break;
                cursor = c.at;
            } else {
                b.text.insertSlice(gpa, c.at, saved) catch break;
                cursor = c.at + c.len;
            }
            b.undo_text.shrinkRetainingCapacity(c.start);
            if (!c.group) break;
        }
        if (cursor == null) return null;
        b.can_merge = false;
        b.group_next = false;
        b.modified = true;
        b.reindex();
        return cursor;
    }

    fn record(b: *Buffer, inserted: bool, at: usize, s: []const u8) void {
        const group = b.group_next and b.undo.items.len > 0;
        b.group_next = false;
        // Typing a word: one character right after the last typed text grows that change.
        if (!group and inserted and b.can_merge and s.len == 1 and s[0] != '\n' and s[0] != ' ' and b.undo.items.len > 0) {
            const last = &b.undo.items[b.undo.items.len - 1];
            if (last.inserted and last.at + last.len == at and last.start + last.len == b.undo_text.items.len) {
                b.undo_text.append(gpa, s[0]) catch return;
                last.len += 1;
                return;
            }
        }
        const start = b.undo_text.items.len;
        b.undo_text.appendSlice(gpa, s) catch return;
        b.undo.append(gpa, .{ .inserted = inserted, .at = at, .start = start, .len = s.len, .group = group }) catch return;
        b.can_merge = inserted and s.len == 1 and !group;
    }

    fn reindex(b: *Buffer) void {
        b.starts.clearRetainingCapacity();
        b.starts.append(gpa, 0) catch return;
        for (b.text.items, 0..) |c, i| {
            if (c == '\n') b.starts.append(gpa, i + 1) catch return;
        }
    }
};

fn isWord(c: u8) bool {
    return std.ascii.isAlphanumeric(c) or c == '_' or c >= 0x80;
}

/// The shared clipboard (/dev/clipboard): what was last copied, by any program.
pub fn copyToClipboard(s: []const u8) void {
    const fd: i32 = switch (sys.open("/dev/clipboard", sys.O_WRONLY | sys.O_TRUNC, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return,
    };
    defer sys.close(fd);
    _ = sys.write(fd, s);
}

/// Reads the clipboard into `buf` (cut short if it doesn't fit).
pub fn readClipboard(buf: []u8) []const u8 {
    const fd: i32 = switch (sys.open("/dev/clipboard", sys.O_RDONLY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return "",
    };
    defer sys.close(fd);
    var n: usize = 0;
    while (n < buf.len) {
        switch (sys.read(fd, buf[n..])) {
            .ok => |got| {
                if (got == 0) break;
                n += got;
            },
            .err => break,
        }
    }
    return buf[0..n];
}
