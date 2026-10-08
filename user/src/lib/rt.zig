//! The tiny runtime every program shares: the `_start` entry point (reading argc/argv/envp off the
//! stack the kernel built), buffered output, and a few helpers. Each program's root file starts with
//!
//!     pub const _start = rt._start;
//!     comptime { _ = _start; }
//!
//! (the second line makes Zig compile it; Zig only compiles what is referenced) and defines
//! `pub fn main(args: rt.Args) u8`.

const std = @import("std");
pub const sys = @import("sys.zig");

pub const Args = struct {
    argv: []const [*:0]const u8,
    envp: [*:null]const ?[*:0]const u8,

    pub fn get(self: Args, i: usize) ?[]const u8 {
        if (i >= self.argv.len) return null;
        return std.mem.span(self.argv[i]);
    }

    /// The value of an environment variable.
    pub fn env(self: Args, name: []const u8) ?[]const u8 {
        var i: usize = 0;
        while (self.envp[i]) |entry| : (i += 1) {
            const s = std.mem.span(entry);
            if (s.len > name.len and s[name.len] == '=' and std.mem.eql(u8, s[0..name.len], name)) return s[name.len + 1 ..];
        }
        return null;
    }
};

pub export fn _start() callconv(.naked) noreturn {
    asm volatile (
        \\ xor %%ebp, %%ebp
        \\ mov %%rsp, %%rdi
        \\ and $-16, %%rsp
        \\ call %[start:P]
        \\ ud2
        :
        : [start] "X" (&start),
    );
}

fn start(sp: [*]const usize) callconv(.c) noreturn {
    const argc = sp[0];
    const argv: [*]const [*:0]const u8 = @ptrCast(sp + 1);
    const envp: [*:null]const ?[*:0]const u8 = @ptrCast(sp + 2 + argc);
    const code = @import("root").main(.{ .argv = argv[0..argc], .envp = envp });
    out.flush();
    err.flush();
    sys.exit(code);
}

/// Output buffered per line, so a program's text reaches the terminal in few writes.
pub const Output = struct {
    fd: i32,
    buf: [4096]u8 = undefined,
    len: usize = 0,

    pub fn write(self: *Output, bytes: []const u8) void {
        if (self.len + bytes.len > self.buf.len) self.flush();
        if (bytes.len > self.buf.len) {
            writeAll(self.fd, bytes);
            return;
        }
        @memcpy(self.buf[self.len..][0..bytes.len], bytes);
        self.len += bytes.len;
        if (std.mem.indexOfScalar(u8, bytes, '\n') != null) self.flush();
    }

    pub fn print(self: *Output, comptime fmt: []const u8, args: anytype) void {
        var tmp: [1024]u8 = undefined;
        const s = std.fmt.bufPrint(&tmp, fmt, args) catch tmp[0..];
        self.write(s);
    }

    pub fn flush(self: *Output) void {
        writeAll(self.fd, self.buf[0..self.len]);
        self.len = 0;
    }
};

pub var out = Output{ .fd = 1 };
pub var err = Output{ .fd = 2 };

pub fn writeAll(fd: i32, bytes: []const u8) void {
    var done: usize = 0;
    while (done < bytes.len) {
        switch (sys.write(fd, bytes[done..])) {
            .ok => |n| done += n,
            .err => |e| if (e != sys.EINTR) return,
        }
    }
}

/// Prints "program: what: reason" to stderr, like GNU tools do.
pub fn complain(program: []const u8, what: []const u8, e: u16) void {
    err.print("{s}: {s}: {s}\n", .{ program, what, sys.errorText(e) });
}

/// Copies a slice into a NUL-terminated buffer for a system call.
pub fn cstr(buf: []u8, s: []const u8) ?[*:0]const u8 {
    if (s.len + 1 > buf.len) return null;
    @memcpy(buf[0..s.len], s);
    buf[s.len] = 0;
    return @ptrCast(buf.ptr);
}

pub fn panicHandler(msg: []const u8, _: ?usize) noreturn {
    err.flush();
    writeAll(2, "panic: ");
    writeAll(2, msg);
    writeAll(2, "\n");
    sys.exit(134);
}
