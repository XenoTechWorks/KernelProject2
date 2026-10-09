//! Windows for command line programs. Opening /dev/window gives a program a window floating over
//! the console: a small terminal of its own that the user can drag around and close. The program
//! draws with ordinary terminal escape sequences and reads keys and mouse clicks back.
//!
//!     var win = ui.Window.open("hello", 30, 5) orelse return 1;
//!     win.text(1, 1, "Hello!");
//!     win.flush();
//!     while (win.next(1000)) |event| { ... }   // null once the user closes it

const std = @import("std");
const rt = @import("rt.zig");
const sys = rt.sys;

pub const Rect = struct {
    x: usize,
    y: usize,
    w: usize,
    h: usize = 1,

    pub fn contains(r: Rect, x: usize, y: usize) bool {
        return x >= r.x and x < r.x + r.w and y >= r.y and y < r.y + r.h;
    }
};

pub const Event = union(enum) {
    /// A typed byte (letters, Enter is '\r', Backspace is 127, Ctrl+C is 3).
    key: u8,
    arrow: enum { up, down, right, left },
    /// A mouse button pressed inside the window, at a cell (0-based). Button 0 is the left one.
    click: struct { x: usize, y: usize, button: u8 },
    /// Nothing happened before the timeout given to next().
    tick,
};

pub const Color = enum(u8) { black, red, green, yellow, blue, magenta, cyan, white, gray, default = 9 };

pub const Window = struct {
    fd: i32,
    out: rt.Output,
    cols: usize,
    rows: usize,
    input: [64]u8 = undefined,
    input_len: usize = 0,

    /// Opens a window with a title and a size in character cells.
    pub fn open(title: []const u8, cols: usize, rows: usize) ?Window {
        const fd: i32 = switch (sys.open("/dev/window", sys.O_RDWR | sys.O_CLOEXEC, 0)) {
            .ok => |fd| @intCast(fd),
            .err => return null,
        };
        var win = Window{ .fd = fd, .out = .{ .fd = fd }, .cols = cols, .rows = rows };
        // Title, size (xterm's "resize" window operation), no text cursor, report mouse clicks.
        win.print("\x1b]2;{s}\x07\x1b[8;{d};{d}t\x1b[?25l\x1b[?1000h", .{ title, rows, cols });
        return win;
    }

    pub fn close(self: *Window) void {
        self.flush();
        sys.close(self.fd);
    }

    pub fn flush(self: *Window) void {
        self.out.flush();
    }

    /// Clears the whole window.
    pub fn clear(self: *Window) void {
        self.write("\x1b[0m\x1b[2J");
    }

    pub fn write(self: *Window, bytes: []const u8) void {
        // Not Output.write: that flushes at every newline, and a frame should appear all at once.
        if (self.out.len + bytes.len > self.out.buf.len) self.out.flush();
        if (bytes.len > self.out.buf.len) return rt.writeAll(self.fd, bytes);
        @memcpy(self.out.buf[self.out.len..][0..bytes.len], bytes);
        self.out.len += bytes.len;
    }

    pub fn print(self: *Window, comptime fmt: []const u8, args: anytype) void {
        var tmp: [512]u8 = undefined;
        self.write(std.fmt.bufPrint(&tmp, fmt, args) catch tmp[0..]);
    }

    /// Moves to column x, row y (0-based).
    pub fn at(self: *Window, x: usize, y: usize) void {
        self.print("\x1b[{d};{d}H", .{ y + 1, x + 1 });
    }

    pub fn color(self: *Window, fg: Color, bold: bool) void {
        self.print("\x1b[0;{d}{s}m", .{ 30 + @intFromEnum(fg), if (bold) ";1" else "" });
    }

    pub fn text(self: *Window, x: usize, y: usize, s: []const u8) void {
        self.at(x, y);
        self.write(s);
    }

    /// Draws a button, "[ label ]", and returns where it is (to check clicks against).
    pub fn button(self: *Window, x: usize, y: usize, label: []const u8) Rect {
        self.at(x, y);
        self.print("\x1b[0;7m {s} \x1b[0m", .{label});
        return .{ .x = x, .y = y, .w = label.len + 2 };
    }

    /// Waits up to `timeout_ms` (null: forever) for the next event. Null when the window was closed.
    pub fn next(self: *Window, timeout_ms: ?u32) ?Event {
        self.flush();
        if (self.input_len == 0) {
            var pfd = extern struct { fd: i32, events: i16, revents: i16 }{ .fd = self.fd, .events = 1, .revents = 0 };
            const timeout: isize = if (timeout_ms) |t| t else -1;
            switch (sys.check(sys.syscall3(7, @intFromPtr(&pfd), 1, @bitCast(timeout)))) { // poll
                .ok => |n| if (n == 0) return .tick,
                .err => return .tick, // interrupted by a signal
            }
            switch (sys.read(self.fd, &self.input)) {
                .ok => |n| {
                    if (n == 0) return null;
                    self.input_len = n;
                },
                .err => return .tick,
            }
        }
        return self.parse();
    }

    fn take(self: *Window, n: usize) void {
        std.mem.copyForwards(u8, &self.input, self.input[n..self.input_len]);
        self.input_len -= n;
    }

    fn parse(self: *Window) Event {
        const in = self.input[0..self.input_len];
        if (in.len >= 3 and in[0] == 0x1b and in[1] == '[') {
            // Arrows: ESC [ A..D
            const arrows = "ABCD";
            if (std.mem.indexOfScalar(u8, arrows, in[2])) |i| {
                self.take(3);
                return .{ .arrow = @enumFromInt(i) };
            }
            // Mouse: ESC [ < button ; x ; y M (pressed) or m (released, ignored)
            if (in[2] == '<') {
                if (std.mem.indexOfAny(u8, in, "Mm")) |end| {
                    var parts = std.mem.splitScalar(u8, in[3..end], ';');
                    const b = std.fmt.parseInt(u8, parts.next() orelse "", 10) catch 0;
                    const x = std.fmt.parseInt(usize, parts.next() orelse "", 10) catch 1;
                    const y = std.fmt.parseInt(usize, parts.next() orelse "", 10) catch 1;
                    const pressed = in[end] == 'M' and b & 32 == 0;
                    self.take(end + 1);
                    if (pressed) return .{ .click = .{ .x = x -| 1, .y = y -| 1, .button = b & 3 } };
                    return .tick;
                }
            }
        }
        const key = in[0];
        self.take(1);
        return .{ .key = key };
    }
};
