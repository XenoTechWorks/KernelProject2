//! terminal: a shell in a window. It opens a text window, makes a pseudo-terminal, starts kpsh on
//! its inside end, and passes bytes both ways: what the shell prints goes to the window, what's
//! typed in the window goes to the shell. When the window is resized, the shell is told the new
//! size. Closing the window hangs up on the shell; the shell exiting closes the window.

const std = @import("std");
const rt = @import("lib/rt.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const TIOCGWINSZ = 0x5413;
const TIOCSWINSZ = 0x5414;
const TIOCSCTTY = 0x540E;
const TIOCGPTN = 0x80045430;
const TIOCSPTLCK = 0x40045431;
const O_NOCTTY = 0o400;
const SIGHUP = 1;

const shell = "/bin/kpsh";

fn ioctl(fd: i32, request: usize, arg: usize) sys.Result {
    return sys.check(sys.syscall3(sys.nr.ioctl, @bitCast(@as(isize, fd)), request, arg));
}

fn openFd(path: [*:0]const u8, flags: usize) ?i32 {
    return switch (sys.open(path, flags, 0)) {
        .ok => |fd| @intCast(fd),
        .err => |e| {
            rt.complain("terminal", std.mem.span(path), e);
            return null;
        },
    };
}

pub fn main(args: rt.Args) u8 {
    const window = openFd("/dev/window", sys.O_RDWR | sys.O_CLOEXEC) orelse return 1;
    // Title and size (xterm's "resize" window operation): 90 x 26 cells.
    rt.writeAll(window, "\x1b]2;Terminal\x07\x1b[8;26;90t");

    const master = openFd("/dev/ptmx", sys.O_RDWR | O_NOCTTY | sys.O_CLOEXEC) orelse return 1;
    var unlock: i32 = 0;
    var number: u32 = 0;
    if (ioctl(master, TIOCSPTLCK, @intFromPtr(&unlock)) != .ok or ioctl(master, TIOCGPTN, @intFromPtr(&number)) != .ok) {
        rt.err.write("terminal: can't set up a pseudo-terminal\n");
        return 1;
    }
    var size = [4]u16{ 0, 0, 0, 0 };
    _ = ioctl(window, TIOCGWINSZ, @intFromPtr(&size));
    _ = ioctl(master, TIOCSWINSZ, @intFromPtr(&size));

    var path_buf: [32]u8 = undefined;
    const inside: [*:0]const u8 = @ptrCast((std.fmt.bufPrint(&path_buf, "/dev/pts/{d}\x00", .{number}) catch return 1).ptr);

    const pid = sys.syscall0(sys.nr.fork);
    if (pid == 0) {
        // The shell: its own session, with the pseudo-terminal as its terminal and its stdio.
        _ = sys.syscall0(112); // setsid
        const tty = openFd(inside, sys.O_RDWR) orelse sys.exit(127);
        _ = ioctl(tty, TIOCSCTTY, 0);
        for (0..3) |fd| _ = sys.syscall2(sys.nr.dup2, @bitCast(@as(isize, tty)), fd);
        if (tty > 2) sys.close(tty);
        const argv = [_:null]?[*:0]const u8{ "-kpsh", null };
        _ = sys.syscall3(sys.nr.execve, @intFromPtr(shell), @intFromPtr(&argv), @intFromPtr(args.envp));
        rt.complain("terminal", shell, 2);
        sys.exit(127);
    }

    var buf: [4096]u8 = undefined;
    while (true) {
        var fds = [2]extern struct { fd: i32, events: i16 = 1, revents: i16 = 0 }{ .{ .fd = master }, .{ .fd = window } };
        // Every half second at the latest, to notice the window being resized.
        _ = sys.syscall3(7, @intFromPtr(&fds), 2, 500); // poll

        if (fds[0].revents != 0) {
            switch (sys.read(master, &buf)) {
                .ok => |n| {
                    if (n == 0) break;
                    rt.writeAll(window, buf[0..n]);
                },
                // EIO: the shell (and everything it started) has gone.
                .err => |e| if (e != sys.EINTR) break,
            }
        }
        if (fds[1].revents != 0) {
            switch (sys.read(window, &buf)) {
                .ok => |n| {
                    if (n == 0) { // the user closed the window
                        // The shell leads its own process group: hang up on all of it.
                        _ = sys.syscall2(sys.nr.kill, @bitCast(-@as(isize, @intCast(pid))), SIGHUP);
                        break;
                    }
                    rt.writeAll(master, buf[0..n]);
                },
                .err => {},
            }
        }
        var now = [4]u16{ 0, 0, 0, 0 };
        _ = ioctl(window, TIOCGWINSZ, @intFromPtr(&now));
        if (now[0] != size[0] or now[1] != size[1]) {
            size = now;
            _ = ioctl(master, TIOCSWINSZ, @intFromPtr(&size));
        }
    }
    return 0;
}
