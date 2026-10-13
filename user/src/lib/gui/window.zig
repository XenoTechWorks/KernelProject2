//! A graphical window: opens /dev/window, makes it a picture of the size asked for, and shows what
//! the program draws on its canvas. Events (pointer, buttons, keys, focus) come back from next().

const std = @import("std");
const sys = @import("../sys.zig");
const Canvas = @import("canvas.zig").Canvas;
const Rect = @import("canvas.zig").Rect;

// The kernel's window ioctls (kernel/src/sys/fs.rs).
const WINDOW_PIXELS = 0x5701;
const WINDOW_TITLE = 0x5702;
const WINDOW_BLIT = 0x5703;
const WINDOW_POINTER = 0x5704;
const WINDOW_SCREEN = 0x5705;
const WINDOW_ROLE = 0x5706;
const WINDOW_LIST = 0x5707;
const WINDOW_ACTIVATE = 0x5708;
const WINDOW_MINIMIZE = 0x5709;
const WINDOW_THEME = 0x570A;

/// What a window is for (kernel/src/console.rs Role). A desktop uses the others for itself.
pub const Role = enum(u32) {
    normal,
    /// Frameless, under all other windows, filling the screen.
    desktop,
    /// Frameless, over normal windows, which stay out of its strip of screen. Clicks on it leave
    /// the keyboard where it was.
    panel,
    /// Frameless, over everything, with the keyboard.
    popup,
    /// Frameless, over everything, without the keyboard or room of its own (a tooltip).
    tooltip,
};

/// A normal window, as a taskbar sees it.
pub const Listed = extern struct {
    id: u32,
    flags: u32,
    title: [56]u8,

    pub fn focused(l: *const Listed) bool {
        return l.flags & 1 != 0;
    }
    pub fn minimized(l: *const Listed) bool {
        return l.flags & 2 != 0;
    }
    pub fn name(l: *const Listed) []const u8 {
        return std.mem.sliceTo(&l.title, 0);
    }
};

const Blit = extern struct { x: u32, y: u32, w: u32, h: u32, stride: u32, pad: u32 = 0, pixels: u64 };
const Raw = extern struct { kind: u32, x: i32, y: i32, data: u32 };

pub const Key = struct {
    /// The character (Enter 13, Backspace 8, Tab 9, Escape 27, Delete 127) or one of these.
    code: u32,
    shift: bool,
    ctrl: bool,

    pub const up = 0x100;
    pub const down = 0x101;
    pub const right = 0x102;
    pub const left = 0x103;
    pub const home = 0x104;
    pub const end = 0x105;
    pub const page_up = 0x106;
    pub const page_down = 0x107;
    /// F1..F11 (F12 belongs to the kernel).
    pub const f1 = 0x110;
    pub const f2 = 0x111;
    pub const f5 = 0x114;
};

pub const Event = union(enum) {
    motion: struct { x: i32, y: i32 },
    /// Button 1 is the left one, 2 the right, 3 the middle.
    press: struct { x: i32, y: i32, button: u8 },
    release: struct { x: i32, y: i32, button: u8 },
    key: Key,
    leave,
    focus: bool,
    /// The mouse wheel: steps, down is positive.
    scroll: struct { x: i32, y: i32, steps: i32 },
    /// The user resized the window; the canvas already has the new size.
    resize: struct { w: u32, h: u32 },
    /// Nothing happened before the timeout given to next().
    tick,
    /// (Desktop windows only) a window opened, closed, changed its title, or got the keyboard.
    windows,
};

pub const Window = struct {
    fd: i32,
    canvas: Canvas,
    queue: [32]Raw = undefined,
    queued: usize = 0,
    taken: usize = 0,

    pub fn open(title: [:0]const u8, w: u32, h: u32) ?Window {
        const fd: i32 = switch (sys.open("/dev/window", sys.O_RDWR | sys.O_CLOEXEC, 0)) {
            .ok => |fd| @intCast(fd),
            .err => return null,
        };
        var size = [2]u32{ w, h };
        if (ioctl(fd, WINDOW_PIXELS, @intFromPtr(&size)) != .ok) {
            sys.close(fd);
            return null;
        }
        _ = ioctl(fd, WINDOW_TITLE, @intFromPtr(title.ptr));
        // The kernel may have made it smaller, to fit the screen.
        const canvas = Canvas.init(size[0], size[1]) orelse return null;
        return .{ .fd = fd, .canvas = canvas };
    }

    pub fn setTitle(self: *Window, title: [:0]const u8) void {
        _ = ioctl(self.fd, WINDOW_TITLE, @intFromPtr(title.ptr));
    }

    /// Shows the whole canvas.
    pub fn present(self: *Window) void {
        self.presentRect(self.canvas.bounds());
    }

    /// Shows part of the canvas (what changed).
    pub fn presentRect(self: *Window, r: Rect) void {
        const a = r.intersect(self.canvas.bounds());
        if (a.w <= 0 or a.h <= 0) return;
        const start = @as(usize, @intCast(a.y)) * self.canvas.w + @as(usize, @intCast(a.x));
        var b = Blit{ .x = @intCast(a.x), .y = @intCast(a.y), .w = @intCast(a.w), .h = @intCast(a.h), .stride = self.canvas.w, .pixels = @intFromPtr(&self.canvas.pixels[start]) };
        _ = ioctl(self.fd, WINDOW_BLIT, @intFromPtr(&b));
    }

    /// Where the pointer is, in the window's pixels (outside it the numbers go negative or past
    /// its size). For following the pointer wherever it goes.
    pub fn pointer(self: *Window) struct { x: i32, y: i32 } {
        var at = [2]i32{ 0, 0 };
        _ = ioctl(self.fd, WINDOW_POINTER, @intFromPtr(&at));
        return .{ .x = at[0], .y = at[1] };
    }

    /// Events waiting to be read right now (to skip redrawing for each of many pointer moves).
    pub fn pending(self: *Window) bool {
        if (self.taken < self.queued) return true;
        var pfd = extern struct { fd: i32, events: i16, revents: i16 }{ .fd = self.fd, .events = 1, .revents = 0 };
        return switch (sys.check(sys.syscall3(7, @intFromPtr(&pfd), 1, 0))) {
            .ok => |n| n > 0,
            .err => false,
        };
    }

    pub fn close(self: *Window) void {
        sys.close(self.fd);
    }

    /// Gives the window a role, puts it at (x, y) and makes it w x h (a desktop fills the screen),
    /// with its corners rounded by `radius`. The canvas gets the new size.
    pub fn setRole(self: *Window, role: Role, x: i32, y: i32, w: u32, h: u32, radius: u32) bool {
        var arg = extern struct { role: u32, x: i32, y: i32, w: u32, h: u32, radius: u32 }{ .role = @intFromEnum(role), .x = x, .y = y, .w = w, .h = h, .radius = radius };
        if (ioctl(self.fd, WINDOW_ROLE, @intFromPtr(&arg)) != .ok) return false;
        const screen = self.screenSize();
        const size_w = if (role == .desktop) screen.w else w;
        const size_h = if (role == .desktop) screen.h else h;
        if (size_w != self.canvas.w or size_h != self.canvas.h) {
            const c = Canvas.init(size_w, size_h) orelse return false;
            self.canvas.deinit();
            self.canvas = c;
        }
        return true;
    }

    /// The whole screen's size in pixels.
    pub fn screenSize(self: *Window) struct { w: u32, h: u32 } {
        var size = [2]u32{ 0, 0 };
        _ = ioctl(self.fd, WINDOW_SCREEN, @intFromPtr(&size));
        return .{ .w = size[0], .h = size[1] };
    }

    /// The normal windows, bottom to top, into `out`. Returns how many it filled in.
    pub fn list(self: *Window, out: []Listed) usize {
        var arg = extern struct { entries: u64, capacity: u32, pad: u32 = 0 }{ .entries = @intFromPtr(out.ptr), .capacity = @intCast(out.len) };
        return switch (ioctl(self.fd, WINDOW_LIST, @intFromPtr(&arg))) {
            .ok => |n| @min(n, out.len),
            .err => 0,
        };
    }

    /// Brings a window (by id, from list) forward with the keyboard, back from minimized.
    pub fn activate(self: *Window, id: u32) void {
        _ = ioctl(self.fd, WINDOW_ACTIVATE, id);
    }

    pub fn minimize(self: *Window, id: u32) void {
        _ = ioctl(self.fd, WINDOW_MINIMIZE, id);
    }

    /// The window color: frames of the window in front go from `top` to `bottom` (0xRRGGBB),
    /// with a `rim` of light inside their edge.
    pub fn setTheme(self: *Window, top: u32, bottom: u32, rim: u32) void {
        var colors = [3]u32{ top, bottom, rim };
        _ = ioctl(self.fd, WINDOW_THEME, @intFromPtr(&colors));
    }

    /// Waits up to `timeout_ms` (null: forever) for the next event. Null once the window is closed.
    pub fn next(self: *Window, timeout_ms: ?u32) ?Event {
        if (self.taken == self.queued) {
            var pfd = extern struct { fd: i32, events: i16, revents: i16 }{ .fd = self.fd, .events = 1, .revents = 0 };
            const timeout: isize = if (timeout_ms) |t| t else -1;
            switch (sys.check(sys.syscall3(7, @intFromPtr(&pfd), 1, @bitCast(timeout)))) { // poll
                .ok => |n| if (n == 0) return .tick,
                .err => return .tick, // interrupted by a signal
            }
            switch (sys.read(self.fd, std.mem.sliceAsBytes(&self.queue))) {
                .ok => |n| {
                    if (n == 0) return null;
                    self.queued = n / @sizeOf(Raw);
                    self.taken = 0;
                },
                .err => return .tick,
            }
        }
        const e = self.queue[self.taken];
        self.taken += 1;
        return switch (e.kind) {
            1 => .{ .motion = .{ .x = e.x, .y = e.y } },
            2 => .{ .press = .{ .x = e.x, .y = e.y, .button = @truncate(e.data) } },
            3 => .{ .release = .{ .x = e.x, .y = e.y, .button = @truncate(e.data) } },
            4 => .{ .key = .{ .code = e.data, .shift = e.x & 1 != 0, .ctrl = e.x & 2 != 0 } },
            5 => .leave,
            6 => .{ .focus = e.data != 0 },
            7 => .{ .scroll = .{ .x = e.x, .y = e.y, .steps = @bitCast(e.data) } },
            8 => blk: {
                const w: u32 = @intCast(e.x);
                const h: u32 = @intCast(e.y);
                if (w != self.canvas.w or h != self.canvas.h) {
                    if (Canvas.init(w, h)) |c| {
                        self.canvas.deinit();
                        self.canvas = c;
                    }
                }
                break :blk .{ .resize = .{ .w = self.canvas.w, .h = self.canvas.h } };
            },
            10 => .windows,
            else => .tick,
        };
    }
};

fn ioctl(fd: i32, request: usize, arg: usize) sys.Result {
    return sys.check(sys.syscall3(16, @bitCast(@as(isize, fd)), request, arg));
}
