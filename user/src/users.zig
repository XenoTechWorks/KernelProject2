//! Accounts: who you are, and becoming somebody else. One program under several names, the way
//! BusyBox does it.
//!
//!     whoami              the name you're running as
//!     id [NAME]           the numbers behind it
//!     login               ask for a name and password, then start that person's shell
//!     su [NAME]           become them in a new shell (root is the default)
//!     useradd NAME        make an account
//!     passwd [NAME]       set a password
//!
//! Accounts live in /etc/passwd, and their passwords, hashed, in /etc/shadow, which only root can
//! read. The hashing is sha256crypt ("$5$"), the same as Linux, so the files can be carried over.

const std = @import("std");
const rt = @import("lib/rt.zig");
const file = @import("lib/file.zig");
const sha = @import("lib/sha256.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const gpa = std.heap.page_allocator;
const passwd_path = "/etc/passwd";
const shadow_path = "/etc/shadow";

/// Who started this program. The program is set-user-id root, so the effective id is root
/// whoever runs it: every "are you allowed to" question has to ask this one instead.
fn realUid() u32 {
    return @intCast(sys.syscall0(102));
}

fn realGid() u32 {
    return @intCast(sys.syscall0(104));
}

const Account = struct {
    name: []const u8,
    uid: u32,
    gid: u32,
    home: []const u8,
    shell: []const u8,
};

pub fn main(args: rt.Args) u8 {
    const me = args.get(0) orelse "id";
    const name = me[if (std.mem.lastIndexOfScalar(u8, me, '/')) |i| i + 1 else 0..];
    // Only su, login and passwd need the root this program was given; the rest hand it straight
    // back, so they run as whoever typed the command.
    const needs_root = std.mem.startsWith(u8, name, "su") or
        std.mem.startsWith(u8, name, "login") or
        std.mem.startsWith(u8, name, "passwd");
    if (!needs_root) {
        _ = sys.syscall1(106, realGid());
        _ = sys.syscall1(105, realUid());
    }
    if (std.mem.startsWith(u8, name, "whoami")) return whoami();
    if (std.mem.startsWith(u8, name, "login")) return login();
    if (std.mem.startsWith(u8, name, "su")) return su(args.get(1) orelse "root");
    if (std.mem.startsWith(u8, name, "useradd")) return useradd(args);
    if (std.mem.startsWith(u8, name, "passwd")) return passwd(args.get(1));
    return id(args.get(1));
}

// ---- the files ----

/// Walks /etc/passwd. `text` must stay alive as long as the account is used.
fn findAccount(text: []const u8, wanted_name: ?[]const u8, wanted_uid: ?u32) ?Account {
    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |line| {
        if (line.len == 0 or line[0] == '#') continue;
        var fields = std.mem.splitScalar(u8, line, ':');
        const name = fields.next() orelse continue;
        _ = fields.next() orelse continue; // the password field, always "x" these days
        const uid = std.fmt.parseInt(u32, fields.next() orelse "", 10) catch continue;
        const gid = std.fmt.parseInt(u32, fields.next() orelse "", 10) catch continue;
        _ = fields.next(); // the full name
        const home = fields.next() orelse "/";
        const shell = std.mem.trimEnd(u8, fields.next() orelse "/bin/kpsh", "\r");
        if (wanted_name) |w| {
            if (!std.mem.eql(u8, name, w)) continue;
        }
        if (wanted_uid) |w| {
            if (uid != w) continue;
        }
        return .{ .name = name, .uid = uid, .gid = gid, .home = home, .shell = shell };
    }
    return null;
}

fn readFileOrEmpty(path: [*:0]const u8) []u8 {
    return file.read(path) catch gpa.alloc(u8, 0) catch unreachable;
}

/// Somebody's hashed password, or null when there's no /etc/shadow line for them.
fn storedHash(text: []const u8, name: []const u8) ?[]const u8 {
    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |line| {
        var fields = std.mem.splitScalar(u8, line, ':');
        const who = fields.next() orelse continue;
        if (!std.mem.eql(u8, who, name)) continue;
        return std.mem.trimEnd(u8, fields.next() orelse "", "\r");
    }
    return null;
}

// ---- asking ----

const Termios = extern struct { iflag: u32, oflag: u32, cflag: u32, lflag: u32, line: u8, cc: [19]u8 };

/// Reads a line with nothing shown, for a password.
fn askSecret(prompt: []const u8, buf: []u8) ?[]const u8 {
    rt.out.write(prompt);
    rt.out.flush();
    var saved: Termios = undefined;
    const have_tty = sys.check(sys.syscall3(16, 0, 0x5401, @intFromPtr(&saved))) == .ok;
    if (have_tty) {
        var quiet = saved;
        quiet.lflag &= ~@as(u32, 0o10); // ECHO off
        _ = sys.syscall3(16, 0, 0x5402, @intFromPtr(&quiet));
    }
    defer {
        if (have_tty) _ = sys.syscall3(16, 0, 0x5402, @intFromPtr(&saved));
        rt.out.write("\n");
        rt.out.flush();
    }
    return readLine(buf);
}

fn ask(prompt: []const u8, buf: []u8) ?[]const u8 {
    rt.out.write(prompt);
    rt.out.flush();
    return readLine(buf);
}

fn readLine(buf: []u8) ?[]const u8 {
    var n: usize = 0;
    while (n < buf.len) {
        var one: [1]u8 = undefined;
        switch (sys.read(0, &one)) {
            .ok => |got| {
                if (got == 0) return if (n == 0) null else buf[0..n];
                if (one[0] == '\n' or one[0] == '\r') return buf[0..n];
                buf[n] = one[0];
                n += 1;
            },
            .err => |e| if (e == sys.EINTR) continue else return null,
        }
    }
    return buf[0..n];
}

/// A salt from the clock and the process id: not a secret, it just has to differ each time.
fn makeSalt(buf: *[16]u8) []const u8 {
    var ts: extern struct { sec: i64, nsec: i64 } = .{ .sec = 0, .nsec = 0 };
    _ = sys.syscall2(sys.nr.clock_gettime, 0, @intFromPtr(&ts));
    var v: u64 = @as(u64, @bitCast(ts.nsec)) ^ (@as(u64, @bitCast(ts.sec)) << 20) ^ sys.syscall0(39) << 7;
    const alphabet = "./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    for (buf) |*c| {
        c.* = alphabet[v & 63];
        v = v *% 6364136223846793005 +% 1442695040888963407;
    }
    return buf[0..];
}

// ---- the commands ----

fn currentName(text: []const u8, uid: u32, buf: []u8) []const u8 {
    if (findAccount(text, null, uid)) |a| return a.name;
    return std.fmt.bufPrint(buf, "{d}", .{uid}) catch "?";
}

fn whoami() u8 {
    const text = readFileOrEmpty(passwd_path);
    defer gpa.free(text);
    var buf: [16]u8 = undefined;
    rt.out.print("{s}\n", .{currentName(text, realUid(), &buf)});
    return 0;
}

fn id(who: ?[]const u8) u8 {
    const text = readFileOrEmpty(passwd_path);
    defer gpa.free(text);
    var uid: u32 = realUid();
    var gid: u32 = realGid();
    if (who) |w| {
        const a = findAccount(text, w, null) orelse {
            rt.err.print("id: no such user: {s}\n", .{w});
            return 1;
        };
        uid = a.uid;
        gid = a.gid;
    }
    var ubuf: [16]u8 = undefined;
    var gbuf: [16]u8 = undefined;
    rt.out.print("uid={d}({s}) gid={d}({s})\n", .{ uid, currentName(text, uid, &ubuf), gid, currentName(text, gid, &gbuf) });
    return 0;
}

/// Checks a password, unless we're already root (who never has to prove anything).
fn allowedToBecome(name: []const u8) bool {
    if (realUid() == 0) return true; // already root
    // This program is set-user-id root so it can read the hashes. If it still can't, say no
    // rather than letting everyone in.
    const shadow = file.read(shadow_path) catch {
        rt.err.write("can't read /etc/shadow\n");
        return false;
    };
    defer gpa.free(shadow);
    const stored = storedHash(shadow, name) orelse "";
    // As on Linux: nothing means no password, and "!" or "*" means nobody may log in as them.
    if (stored.len == 0) return true;
    if (stored[0] == '!' or stored[0] == '*') return false;
    var buf: [256]u8 = undefined;
    const given = askSecret("Password: ", &buf) orelse return false;
    return sha.verify(given, stored);
}

fn become(a: Account) u8 {
    // The group first: once the user id is gone, changing the group isn't allowed any more.
    // setgid/setuid set both the real and effective ids, so the root we borrowed is gone.
    if (sys.check(sys.syscall1(106, a.gid)) == .err or sys.check(sys.syscall1(105, a.uid)) == .err) {
        rt.err.write("can't change user\n");
        return 1;
    }
    var home: [512]u8 = undefined;
    if (rt.cstr(&home, a.home)) |h| _ = sys.syscall1(sys.nr.chdir, @intFromPtr(h));

    var shell_buf: [512]u8 = undefined;
    const shell = rt.cstr(&shell_buf, a.shell) orelse return 1;
    var home_env: [600]u8 = undefined;
    var user_env: [600]u8 = undefined;
    const home_line = std.fmt.bufPrintSentinel(&home_env, "HOME={s}", .{a.home}, 0) catch return 1;
    const user_line = std.fmt.bufPrintSentinel(&user_env, "USER={s}", .{a.name}, 0) catch return 1;
    const argv = [_:null]?[*:0]const u8{ shell, null };
    const envp = [_:null]?[*:0]const u8{ home_line.ptr, user_line.ptr, "PATH=/bin:/sbin", "TERM=linux", null };
    _ = sys.syscall3(sys.nr.execve, @intFromPtr(shell), @intFromPtr(&argv), @intFromPtr(&envp));
    rt.err.print("can't run {s}\n", .{a.shell});
    return 1;
}

fn su(who: []const u8) u8 {
    const text = readFileOrEmpty(passwd_path);
    defer gpa.free(text);
    const a = findAccount(text, who, null) orelse {
        rt.err.print("su: no such user: {s}\n", .{who});
        return 1;
    };
    if (!allowedToBecome(who)) {
        rt.err.write("su: wrong password\n");
        return 1;
    }
    return become(a);
}

fn login() u8 {
    while (true) {
        var namebuf: [64]u8 = undefined;
        const who = ask("login: ", &namebuf) orelse return 0;
        if (who.len == 0) continue;
        const text = readFileOrEmpty(passwd_path);
        defer gpa.free(text);
        const found = findAccount(text, who, null);
        // Ask for the password either way, so a wrong name looks the same as a wrong password.
        const ok = allowedToBecome(who) and found != null;
        if (!ok) {
            rt.out.write("Login incorrect\n\n");
            continue;
        }
        return become(found.?);
    }
}

fn useradd(args: rt.Args) u8 {
    if (realUid() != 0) {
        rt.err.write("useradd: only root can add users\n");
        return 1;
    }
    var who: ?[]const u8 = null;
    var want_uid: ?u32 = null;
    var i: usize = 1;
    while (args.get(i)) |arg| : (i += 1) {
        if (std.mem.eql(u8, arg, "-u")) {
            i += 1;
            want_uid = std.fmt.parseInt(u32, args.get(i) orelse "", 10) catch null;
        } else who = arg;
    }
    const name = who orelse {
        rt.err.write("usage: useradd [-u UID] NAME\n");
        return 2;
    };
    const text = readFileOrEmpty(passwd_path);
    defer gpa.free(text);
    if (findAccount(text, name, null) != null) {
        rt.err.print("useradd: {s} already exists\n", .{name});
        return 1;
    }
    // The next free id from 1000 up, as Linux numbers ordinary people.
    var uid: u32 = want_uid orelse 1000;
    while (want_uid == null and findAccount(text, null, uid) != null) uid += 1;

    var line: [512]u8 = undefined;
    const entry = std.fmt.bufPrint(&line, "{s}{s}:x:{d}:{d}::/home/{s}:/bin/kpsh\n", .{
        if (text.len > 0 and text[text.len - 1] != '\n') "\n" else "",
        name,
        uid,
        uid,
        name,
    }) catch return 1;
    if (!append(passwd_path, entry)) return 1;

    var shadow_line: [128]u8 = undefined;
    const shadow_entry = std.fmt.bufPrint(&shadow_line, "{s}::::::::\n", .{name}) catch return 1;
    _ = append(shadow_path, shadow_entry);

    var home: [256]u8 = undefined;
    const home_dir = std.fmt.bufPrint(&home, "/home/{s}", .{name}) catch return 1;
    file.makePath(home_dir);
    var hbuf: [256]u8 = undefined;
    if (rt.cstr(&hbuf, home_dir)) |h| {
        _ = sys.syscall3(260, @intFromPtr(h), uid, uid); // chown
        _ = sys.syscall2(90, @intFromPtr(h), 0o700); // chmod
    }
    rt.out.print("Added {s} (uid {d}). Give them a password with: passwd {s}\n", .{ name, uid, name });
    return 0;
}

fn append(path: [*:0]const u8, line: []const u8) bool {
    const old = readFileOrEmpty(path);
    defer gpa.free(old);
    var both = std.ArrayList(u8).empty;
    defer both.deinit(gpa);
    both.appendSlice(gpa, old) catch return false;
    both.appendSlice(gpa, line) catch return false;
    file.write(path, both.items, 0o600) catch {
        rt.err.print("can't write {s}\n", .{path});
        return false;
    };
    return true;
}

fn appendLine(out: *std.ArrayList(u8), name: []const u8, hashed: []const u8) !void {
    var line: [256]u8 = undefined;
    const text = std.fmt.bufPrint(&line, "{s}:{s}:::::::\n", .{ name, hashed }) catch return error.TooLong;
    try out.appendSlice(gpa, text);
}

fn passwd(who: ?[]const u8) u8 {
    const text = readFileOrEmpty(passwd_path);
    defer gpa.free(text);
    const uid = realUid();
    var namebuf: [16]u8 = undefined;
    const name = who orelse currentName(text, uid, &namebuf);
    if (uid != 0 and !std.mem.eql(u8, name, currentName(text, uid, &namebuf))) {
        rt.err.write("passwd: only root can change someone else's password\n");
        return 1;
    }
    if (findAccount(text, name, null) == null) {
        rt.err.print("passwd: no such user: {s}\n", .{name});
        return 1;
    }
    var first: [256]u8 = undefined;
    var again: [256]u8 = undefined;
    const a = askSecret("New password: ", &first) orelse return 1;
    const b = askSecret("Again: ", &again) orelse return 1;
    if (!std.mem.eql(u8, a, b)) {
        rt.err.write("passwd: they don't match\n");
        return 1;
    }
    var salt_buf: [16]u8 = undefined;
    var hash_buf: [128]u8 = undefined;
    const hashed = sha.crypt(&hash_buf, a, makeSalt(&salt_buf), sha.default_rounds);

    // Rewrite /etc/shadow with this person's line changed.
    const shadow = readFileOrEmpty(shadow_path);
    defer gpa.free(shadow);
    var out = std.ArrayList(u8).empty;
    defer out.deinit(gpa);
    var replaced = false;
    var lines = std.mem.splitScalar(u8, shadow, '\n');
    while (lines.next()) |line| {
        if (line.len == 0) continue;
        const who_here = std.mem.sliceTo(line, ':');
        if (std.mem.eql(u8, who_here, name)) {
            appendLine(&out, name, hashed) catch return 1;
            replaced = true;
        } else {
            out.appendSlice(gpa, line) catch return 1;
            out.append(gpa, '\n') catch return 1;
        }
    }
    if (!replaced) appendLine(&out, name, hashed) catch return 1;
    file.write(shadow_path, out.items, 0o600) catch {
        rt.err.write("passwd: can't write /etc/shadow\n");
        return 1;
    };
    rt.out.print("Password set for {s}.\n", .{name});
    return 0;
}
