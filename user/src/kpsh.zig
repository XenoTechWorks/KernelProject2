//! kpsh: the KernelProject2 shell (a working name).
//!
//! A shell in the Linux/POSIX shape: read a line, split it into words, run builtins itself, and run
//! everything else as a separate program with fork + execve + wait4. Supports pipes (|), redirection
//! (> >> < 2> 2>&1), lists (; && ||), background jobs (&), quotes, backslash escapes, $VAR / $? / ~
//! expansion and scripts with arguments, $1 $# $* "$@" (`kpsh file`, `kpsh -c "command"`, or a
//! file starting with #!/bin/kpsh).

const std = @import("std");
const rt = @import("lib/rt.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const eql = std.mem.eql;

// ── Environment ─────────────────────────────────────────────────────────────────────────────────

const max_vars = 96;
const Var = struct { buf: [512]u8 = undefined, len: usize = 0 };
var vars: [max_vars]Var = @splat(.{});
var var_count: usize = 0;

fn varName(v: *const Var) []const u8 {
    const s = v.buf[0..v.len];
    return s[0 .. std.mem.indexOfScalar(u8, s, '=') orelse s.len];
}

fn getVar(name: []const u8) ?[]const u8 {
    for (vars[0..var_count]) |*v| {
        if (eql(u8, varName(v), name)) return v.buf[name.len + 1 .. v.len];
    }
    return null;
}

fn setVar(name: []const u8, value: []const u8) void {
    if (name.len + value.len + 2 > 512) return;
    const slot = for (vars[0..var_count]) |*v| {
        if (eql(u8, varName(v), name)) break v;
    } else blk: {
        if (var_count == max_vars) return;
        var_count += 1;
        break :blk &vars[var_count - 1];
    };
    @memcpy(slot.buf[0..name.len], name);
    slot.buf[name.len] = '=';
    @memcpy(slot.buf[name.len + 1 ..][0..value.len], value);
    slot.len = name.len + 1 + value.len;
    slot.buf[slot.len] = 0;
}

fn unsetVar(name: []const u8) void {
    for (vars[0..var_count], 0..) |*v, i| {
        if (eql(u8, varName(v), name)) {
            vars[i] = vars[var_count - 1];
            var_count -= 1;
            return;
        }
    }
}

var envp_buf: [max_vars + 1]?[*:0]const u8 = undefined;

fn envp() [*:null]const ?[*:0]const u8 {
    for (vars[0..var_count], 0..) |*v, i| envp_buf[i] = @ptrCast(&v.buf);
    envp_buf[var_count] = null;
    return @ptrCast(&envp_buf);
}

var last_status: u8 = 0;

// ── Words and tokens ────────────────────────────────────────────────────────────────────────────

const Kind = enum { word, pipe, and_if, or_if, semi, background, out, append, in, err_out, err_to_out };
const Token = struct { kind: Kind, text: [:0]const u8 = "" };

const max_tokens = 256;
var tokens: [max_tokens]Token = undefined;
var token_count: usize = 0;
var pool: [32 * 1024]u8 = undefined;
var pool_len: usize = 0;

const ParseError = error{ Unclosed, TooLong };

fn pushChar(c: u8) ParseError!void {
    if (pool_len + 2 >= pool.len) return error.TooLong;
    pool[pool_len] = c;
    pool_len += 1;
}

fn pushStr(s: []const u8) ParseError!void {
    for (s) |c| try pushChar(c);
}

fn pushToken(t: Token) ParseError!void {
    if (token_count == max_tokens) return error.TooLong;
    tokens[token_count] = t;
    token_count += 1;
}

fn isNameChar(c: u8) bool {
    return std.ascii.isAlphanumeric(c) or c == '_';
}

/// A script's name and arguments ($0, $1...): `kpsh script a b`, or `kpsh -c command name a b`.
var positional: []const [*:0]const u8 = &.{};

/// Expands $NAME, ${NAME}, $?, $0-$9, $# or $* starting at line[i] (just after the $). Returns the
/// new index. ($@ is the tokenizer's, since it can make more than one word.)
fn expandVar(line: []const u8, i: usize) ParseError!usize {
    if (i < line.len and line[i] == '?') {
        var tmp: [4]u8 = undefined;
        try pushStr(std.fmt.bufPrint(&tmp, "{d}", .{last_status}) catch "");
        return i + 1;
    }
    if (i < line.len and std.ascii.isDigit(line[i])) {
        const n = line[i] - '0';
        if (n < positional.len) try pushStr(std.mem.span(positional[n]));
        return i + 1;
    }
    if (i < line.len and line[i] == '#') {
        var tmp: [8]u8 = undefined;
        try pushStr(std.fmt.bufPrint(&tmp, "{d}", .{positional.len -| 1}) catch "");
        return i + 1;
    }
    if (i < line.len and line[i] == '*') {
        if (positional.len > 1) for (positional[1..], 1..) |a, k| {
            if (k > 1) try pushChar(' ');
            try pushStr(std.mem.span(a));
        };
        return i + 1;
    }
    if (i < line.len and line[i] == '{') {
        const end = std.mem.indexOfScalarPos(u8, line, i, '}') orelse return error.Unclosed;
        try pushStr(getVar(line[i + 1 .. end]) orelse "");
        return end + 1;
    }
    var j = i;
    while (j < line.len and isNameChar(line[j])) j += 1;
    if (j == i) {
        try pushChar('$');
        return i;
    }
    try pushStr(getVar(line[i..j]) orelse "");
    return j;
}

fn tokenize(line: []const u8) ParseError!void {
    token_count = 0;
    pool_len = 0;
    var i: usize = 0;
    while (i < line.len) {
        const c = line[i];
        switch (c) {
            ' ', '\t', '\r', '\n' => {
                i += 1;
                continue;
            },
            '#' => return,
            ';' => {
                try pushToken(.{ .kind = .semi });
                i += 1;
                continue;
            },
            '|' => {
                if (i + 1 < line.len and line[i + 1] == '|') {
                    try pushToken(.{ .kind = .or_if });
                    i += 2;
                } else {
                    try pushToken(.{ .kind = .pipe });
                    i += 1;
                }
                continue;
            },
            '&' => {
                if (i + 1 < line.len and line[i + 1] == '&') {
                    try pushToken(.{ .kind = .and_if });
                    i += 2;
                } else {
                    // A lone & ends the part (runLine splits there and runs it in the background).
                    try pushToken(.{ .kind = .semi });
                    i += 1;
                }
                continue;
            },
            '>' => {
                if (i + 1 < line.len and line[i + 1] == '>') {
                    try pushToken(.{ .kind = .append });
                    i += 2;
                } else {
                    try pushToken(.{ .kind = .out });
                    i += 1;
                }
                continue;
            },
            '<' => {
                try pushToken(.{ .kind = .in });
                i += 1;
                continue;
            },
            '2' => {
                if (std.mem.startsWith(u8, line[i..], "2>&1")) {
                    try pushToken(.{ .kind = .err_to_out });
                    i += 4;
                    continue;
                }
                if (std.mem.startsWith(u8, line[i..], "2>")) {
                    try pushToken(.{ .kind = .err_out });
                    i += 2;
                    continue;
                }
            },
            else => {},
        }

        // A word: everything up to an unquoted space or operator.
        var start = pool_len;
        // "$@" with no arguments makes no word at all (rather than an empty one).
        var only_args = false;
        if (c == '~' and (i + 1 == line.len or std.mem.indexOfScalar(u8, "/ \t\r\n;|&<>", line[i + 1]) != null)) {
            try pushStr(getVar("HOME") orelse "/");
            i += 1;
        }
        while (i < line.len) {
            const ch = line[i];
            switch (ch) {
                ' ', '\t', '\r', '\n', ';', '|', '&', '>', '<' => break,
                '\\' => {
                    only_args = false;
                    if (i + 1 < line.len) try pushChar(line[i + 1]);
                    i += 2;
                },
                '\'' => {
                    only_args = false;
                    const end = std.mem.indexOfScalarPos(u8, line, i + 1, '\'') orelse return error.Unclosed;
                    try pushStr(line[i + 1 .. end]);
                    i = end + 1;
                },
                '"' => {
                    // "$@": each argument a word of its own (the first and last joined to what's
                    // around them, as in other shells).
                    if (std.mem.startsWith(u8, line[i..], "\"$@\"")) {
                        only_args = pool_len == start and positional.len <= 1;
                        if (positional.len > 1) for (positional[1..], 1..) |a, k| {
                            if (k > 1) {
                                try pushChar(0);
                                try pushToken(.{ .kind = .word, .text = pool[start .. pool_len - 1 :0] });
                                start = pool_len;
                            }
                            try pushStr(std.mem.span(a));
                        };
                        i += 4;
                        continue;
                    }
                    only_args = false;
                    i += 1;
                    while (true) {
                        if (i >= line.len) return error.Unclosed;
                        const q = line[i];
                        if (q == '"') {
                            i += 1;
                            break;
                        } else if (q == '\\' and i + 1 < line.len and std.mem.indexOfScalar(u8, "\"\\$`", line[i + 1]) != null) {
                            try pushChar(line[i + 1]);
                            i += 2;
                        } else if (q == '$') {
                            i = try expandVar(line, i + 1);
                        } else {
                            try pushChar(q);
                            i += 1;
                        }
                    }
                },
                '$' => {
                    only_args = false;
                    i = try expandVar(line, i + 1);
                },
                else => {
                    only_args = false;
                    try pushChar(ch);
                    i += 1;
                },
            }
        }
        if (only_args and pool_len == start) continue;
        try pushChar(0);
        try pushToken(.{ .kind = .word, .text = pool[start .. pool_len - 1 :0] });
    }
}

// ── Commands ────────────────────────────────────────────────────────────────────────────────────

const Redirect = struct { kind: Kind, target: [:0]const u8 };
const Command = struct {
    argv: [64:null]?[*:0]const u8 = undefined,
    argc: usize = 0,
    redirects: [8]Redirect = undefined,
    redirect_count: usize = 0,

    fn arg(self: *const Command, i: usize) ?[]const u8 {
        if (i >= self.argc) return null;
        return std.mem.span(self.argv[i].?);
    }
};

/// Splits tokens[from..to] (one pipeline) into commands. Returns how many, or null after an error.
fn parsePipeline(from: usize, to: usize, commands: []Command) ?usize {
    var n: usize = 1;
    commands[0] = .{};
    var i = from;
    while (i < to) : (i += 1) {
        const t = tokens[i];
        var cmd = &commands[n - 1];
        switch (t.kind) {
            .word => {
                if (cmd.argc == 63) {
                    rt.err.write("kpsh: too many arguments\n");
                    return null;
                }
                cmd.argv[cmd.argc] = t.text.ptr;
                cmd.argc += 1;
            },
            .pipe => {
                if (cmd.argc == 0 or n == commands.len) {
                    rt.err.write("kpsh: syntax error near '|'\n");
                    return null;
                }
                n += 1;
                commands[n - 1] = .{};
            },
            .out, .append, .in, .err_out => {
                if (i + 1 >= to or tokens[i + 1].kind != .word or cmd.redirect_count == 8) {
                    rt.err.write("kpsh: syntax error: redirection needs a file name\n");
                    return null;
                }
                cmd.redirects[cmd.redirect_count] = .{ .kind = t.kind, .target = tokens[i + 1].text };
                cmd.redirect_count += 1;
                i += 1;
            },
            .err_to_out => {
                cmd.redirects[cmd.redirect_count] = .{ .kind = .err_to_out, .target = "" };
                cmd.redirect_count += 1;
            },
            else => unreachable,
        }
    }
    for (commands[0..n]) |*cmd| cmd.argv[cmd.argc] = null;
    if (commands[n - 1].argc == 0 and (n > 1 or commands[0].redirect_count > 0)) {
        rt.err.write("kpsh: syntax error: missing command\n");
        return null;
    }
    return n;
}

fn applyRedirects(cmd: *const Command) bool {
    for (cmd.redirects[0..cmd.redirect_count]) |r| {
        const target_fd: i32 = switch (r.kind) {
            .in => 0,
            .err_out => 2,
            .err_to_out => {
                _ = sys.syscall2(sys.nr.dup2, 1, 2);
                continue;
            },
            else => 1,
        };
        const flags: usize = switch (r.kind) {
            .in => sys.O_RDONLY,
            .append => sys.O_WRONLY | sys.O_CREAT | sys.O_APPEND,
            else => sys.O_WRONLY | sys.O_CREAT | sys.O_TRUNC,
        };
        switch (sys.open(r.target.ptr, flags | sys.O_CLOEXEC, 0o644)) {
            .ok => |fd| {
                _ = sys.syscall2(sys.nr.dup2, fd, @intCast(target_fd));
                sys.close(@intCast(fd));
            },
            .err => |e| {
                rt.complain("kpsh", r.target, e);
                return false;
            },
        }
    }
    return true;
}

/// Finds a command in $PATH. Writes the full path into `buf`.
fn findProgram(name: []const u8, buf: []u8) ?[*:0]const u8 {
    if (std.mem.indexOfScalar(u8, name, '/') != null) return rt.cstr(buf, name);
    var dirs = std.mem.splitScalar(u8, getVar("PATH") orelse "/bin", ':');
    while (dirs.next()) |dir| {
        if (dir.len == 0 or dir.len + name.len + 2 > buf.len) continue;
        @memcpy(buf[0..dir.len], dir);
        buf[dir.len] = '/';
        @memcpy(buf[dir.len + 1 ..][0..name.len], name);
        buf[dir.len + 1 + name.len] = 0;
        const path: [*:0]const u8 = @ptrCast(buf.ptr);
        var st: sys.Stat = undefined;
        if (sys.stat(path, &st) == .ok and st.mode & sys.S_IFMT == sys.S_IFREG and st.mode & 0o111 != 0) return path;
    }
    return null;
}

fn setSignal(sig: usize, handler: usize) void {
    const action = [4]usize{ handler, 0, 0, 0 };
    _ = sys.syscall4(sys.nr.rt_sigaction, sig, @intFromPtr(&action), 0, 8);
}

/// In a child process: run one command and never return. `pgid` is its job's process group (0:
/// start one).
fn runInChild(cmd: *const Command, background: bool, pgid: usize) noreturn {
    // Its own process group, so Ctrl+C and Ctrl+Z reach the job and not the shell. A job in front
    // takes the terminal itself too: the shell does the same, but the child may run first, and
    // reading the terminal before it has it would stop it (SIGTTIN). Only with job control: in
    // a script or `kpsh -c` it stays in the shell's group, which has the terminal (if anyone).
    if (job_control) {
        _ = sys.syscall2(sys.nr.setpgid, 0, pgid);
        if (!background) giveTerminal(if (pgid == 0) sys.syscall0(sys.nr.getpid) else pgid);
    }
    if (background) {
        // A background job doesn't read the terminal: its input is /dev/null unless redirected.
        switch (sys.open("/dev/null", sys.O_RDONLY, 0)) {
            .ok => |fd| {
                _ = sys.syscall2(sys.nr.dup2, fd, 0);
                sys.close(@intCast(fd));
            },
            .err => {},
        }
    }
    // The signals the shell ignores, back to what programs expect.
    for ([_]usize{ SIGINT, SIGQUIT, SIGTSTP, SIGTTIN, SIGTTOU }) |sig| setSignal(sig, 0);
    if (!applyRedirects(cmd)) sys.exit(1);
    if (cmd.argc == 0) sys.exit(0);
    if (builtin(cmd)) |status| {
        rt.out.flush();
        sys.exit(status);
    }
    const name = cmd.arg(0).?;
    var path_buf: [512]u8 = undefined;
    const path = findProgram(name, &path_buf) orelse {
        rt.err.print("kpsh: {s}: command not found\n", .{name});
        rt.err.flush();
        sys.exit(127);
    };
    const r = sys.check(sys.syscall3(sys.nr.execve, @intFromPtr(path), @intFromPtr(&cmd.argv), @intFromPtr(envp())));
    rt.complain("kpsh", name, r.err);
    rt.err.flush();
    sys.exit(if (r.err == sys.ENOENT) 127 else 126);
}

fn describeSignal(sig: u32) ?[]const u8 {
    return switch (sig) {
        2 => null, // Ctrl+C: the terminal already showed ^C
        4 => "Illegal instruction",
        6 => "Aborted",
        7 => "Bus error",
        8 => "Floating point exception",
        9 => "Killed",
        11 => "Segmentation fault",
        13 => null, // broken pipe: normal in pipelines like `cat big | head`
        15 => "Terminated",
        else => "Killed by a signal",
    };
}

// ── Jobs ────────────────────────────────────────────────────────────────────────────────────────
//
// Each pipeline is a job: its processes share a process group, which has the terminal while the
// job runs in the foreground. Ctrl+Z stops it (SIGTSTP to that group) and the shell takes the
// terminal back; `fg` gives it back and continues it, `bg` continues it in the background.

const SIGINT = 2;
const SIGQUIT = 3;
const SIGCONT = 18;
const SIGTSTP = 20;
const SIGTTIN = 21;
const SIGTTOU = 22;
const WNOHANG = 1;
const WUNTRACED = 2;
const WCONTINUED = 8;
const TIOCSPGRP = 0x5410;

const Job = struct {
    number: usize,
    pgid: usize,
    /// The last process of the pipeline: its exit status is the job's.
    last: usize,
    stopped: bool,
    label: [96]u8,
    label_len: usize,

    fn name(j: *const Job) []const u8 {
        return j.label[0..j.label_len];
    }
};

var jobs: [16]?Job = @splat(null);
/// The shell's own process group, and whether it runs the terminal's jobs (an interactive shell).
var shell_pgid: usize = 0;
var job_control = false;

/// Gives the terminal's keyboard to a process group.
fn giveTerminal(pgid: usize) void {
    if (job_control) {
        var group: u32 = @intCast(pgid);
        _ = sys.syscall3(sys.nr.ioctl, 0, TIOCSPGRP, @intFromPtr(&group));
    }
}

fn newJob(pgid: usize, last: usize, label: []const u8) Job {
    var job = Job{ .number = 0, .pgid = pgid, .last = last, .stopped = false, .label = undefined, .label_len = 0 };
    const trimmed = std.mem.trim(u8, label, " \t\r\n&");
    job.label_len = @min(trimmed.len, job.label.len);
    @memcpy(job.label[0..job.label_len], trimmed[0..job.label_len]);
    return job;
}

/// Puts a job in the table under the lowest free number.
fn addJob(job: Job) ?*Job {
    var number: usize = 1;
    while (true) : (number += 1) {
        for (jobs) |j| {
            if (j != null and j.?.number == number) break;
        } else break;
    }
    for (&jobs) |*slot| {
        if (slot.* == null) {
            slot.* = job;
            slot.*.?.number = number;
            return &slot.*.?;
        }
    }
    return null;
}

/// Waits while a job runs in the foreground: until its processes have all finished (its status
/// is the last one's) or it's stopped (then it's kept in the table, and 128 + the signal).
fn waitJob(job: Job) u8 {
    var status: u8 = 0;
    while (true) {
        var raw: u32 = 0;
        // The job's process group; without job control there isn't one (its processes are in
        // the shell's), so any child, until the job's last has ended.
        const minus_pgid: usize = @bitCast(-@as(isize, @intCast(job.pgid)));
        const which: usize = if (job_control) minus_pgid else @bitCast(@as(isize, -1));
        const r = sys.check(sys.syscall4(sys.nr.wait4, which, @intFromPtr(&raw), WUNTRACED, 0));
        const pid = switch (r) {
            .ok => |pid| pid,
            .err => |e| if (e == sys.EINTR) continue else break, // ECHILD: all done
        };
        if (raw & 0xff == 0x7f) {
            const sig = (raw >> 8) & 0xff;
            var stopped = job;
            stopped.stopped = true;
            const kept = findJob(job.pgid) orelse addJob(stopped);
            if (kept) |k| {
                k.stopped = true;
                rt.err.print("\n[{d}]+  Stopped                 {s}\n", .{ k.number, k.name() });
            }
            return @intCast(128 + sig);
        }
        if (pid != job.last) continue;
        const sig = raw & 0x7f;
        if (sig == 0) {
            status = @intCast((raw >> 8) & 0xff);
        } else {
            status = @intCast(128 + sig);
            if (describeSignal(sig)) |text| rt.err.print("{s}\n", .{text});
        }
        if (!job_control) break; // (the rest of a pipeline ended before its last, or will be collected later)
    }
    forgetJob(job.pgid);
    return status;
}

fn findJob(pgid: usize) ?*Job {
    for (&jobs) |*slot| {
        if (slot.*) |*j| {
            if (j.pgid == pgid) return j;
        }
    }
    return null;
}

fn forgetJob(pgid: usize) void {
    for (&jobs) |*slot| {
        if (slot.* != null and slot.*.?.pgid == pgid) slot.* = null;
    }
}

/// Says which background jobs finished or stopped (before each prompt), and collects them.
fn reapJobs() void {
    while (true) {
        var raw: u32 = 0;
        const any: usize = @bitCast(@as(isize, -1));
        const r = sys.check(sys.syscall4(sys.nr.wait4, any, @intFromPtr(&raw), WNOHANG | WUNTRACED | WCONTINUED, 0));
        const pid = switch (r) {
            .ok => |pid| pid,
            .err => return,
        };
        if (pid == 0) return;
        for (&jobs) |*slot| {
            const j = &(slot.* orelse continue);
            if (raw & 0xff == 0x7f) {
                // Stopped in the background (it read the terminal, or got SIGSTOP).
                if (j.pgid == pid or j.last == pid) {
                    if (!j.stopped) rt.err.print("[{d}]+  Stopped                 {s}\n", .{ j.number, j.name() });
                    j.stopped = true;
                }
            } else if (raw == 0xffff) {
                if (j.pgid == pid or j.last == pid) j.stopped = false;
            } else if (j.last == pid) {
                rt.err.print("[{d}]+  Done                    {s}\n", .{ j.number, j.name() });
                slot.* = null;
            }
        }
    }
}

/// The job `%n` names, or (with no argument) the most recent one.
fn pickJob(cmd: *const Command) ?*Job {
    if (cmd.arg(1)) |spec| {
        const digits = if (spec.len > 0 and spec[0] == '%') spec[1..] else spec;
        const number = std.fmt.parseInt(usize, digits, 10) catch {
            rt.err.print("kpsh: {s}: no such job\n", .{spec});
            return null;
        };
        for (&jobs) |*slot| {
            if (slot.*) |*j| {
                if (j.number == number) return j;
            }
        }
        rt.err.print("kpsh: {s}: no such job\n", .{spec});
        return null;
    }
    var newest: ?*Job = null;
    for (&jobs) |*slot| {
        if (slot.*) |*j| {
            if (newest == null or j.number > newest.?.number) newest = j;
        }
    }
    if (newest == null) rt.err.write("kpsh: no current job\n");
    return newest;
}

fn continueJob(job: *Job) void {
    const minus_pgid: usize = @bitCast(-@as(isize, @intCast(job.pgid)));
    _ = sys.syscall2(sys.nr.kill, minus_pgid, SIGCONT);
    job.stopped = false;
}

/// `fg`: the job gets the terminal back, goes on if stopped, and the shell waits for it.
fn fgJob(cmd: *const Command) u8 {
    const job = pickJob(cmd) orelse return 1;
    rt.err.print("{s}\n", .{job.name()});
    rt.err.flush();
    const copy = job.*;
    giveTerminal(copy.pgid);
    continueJob(job);
    const status = waitJob(copy);
    giveTerminal(shell_pgid);
    return status;
}

/// `wait [%n]`: waits for a background job to finish (with no argument: every one that's
/// running). Its status is the job's (the last one's).
fn waitJobs(cmd: *const Command) u8 {
    if (cmd.arg(1) != null) {
        const job = pickJob(cmd) orelse return 127;
        return waitJob(job.*);
    }
    var status: u8 = 0;
    for (&jobs) |*slot| {
        const j = slot.* orelse continue;
        if (!j.stopped) status = waitJob(j);
    }
    return status;
}

/// A signal by number ("9") or name ("KILL", "SIGKILL").
fn signalNumber(s: []const u8) ?usize {
    if (std.fmt.parseInt(usize, s, 10)) |n| return if (n < 65) n else null else |_| {}
    const name = if (std.mem.startsWith(u8, s, "SIG")) s[3..] else s;
    const names = [_]struct { []const u8, usize }{
        .{ "HUP", 1 },   .{ "INT", 2 },   .{ "QUIT", 3 },  .{ "KILL", 9 },  .{ "USR1", 10 },
        .{ "USR2", 12 }, .{ "TERM", 15 }, .{ "CONT", 18 }, .{ "STOP", 19 }, .{ "TSTP", 20 },
    };
    for (names) |n| {
        if (std.ascii.eqlIgnoreCase(name, n[0])) return n[1];
    }
    return null;
}

/// `kill [-SIGNAL] PID|%n...`: sends a signal (TERM unless another is named) to a process, or to
/// every process in a job.
fn killCommand(cmd: *const Command) u8 {
    var sig: usize = 15;
    var i: usize = 1;
    if (cmd.arg(1)) |a| if (a.len > 1 and a[0] == '-') {
        sig = signalNumber(a[1..]) orelse {
            rt.err.print("kpsh: kill: {s}: no such signal\n", .{a[1..]});
            return 1;
        };
        i = 2;
    };
    if (cmd.arg(i) == null) {
        rt.err.write("usage: kill [-SIGNAL] PID|%JOB...\n");
        return 2;
    }
    var status: u8 = 0;
    while (cmd.arg(i)) |target| : (i += 1) {
        // A job is its process group: a negative pid reaches all of it. (Without job control
        // there's no group of its own: its last process, then.)
        const pid: isize = if (target.len > 0 and target[0] == '%') blk: {
            const number = std.fmt.parseInt(usize, target[1..], 10) catch 0;
            for (&jobs) |*slot| {
                if (slot.*) |*j| if (j.number == number)
                    break :blk if (job_control) -@as(isize, @intCast(j.pgid)) else @as(isize, @intCast(j.last));
            }
            rt.err.print("kpsh: kill: {s}: no such job\n", .{target});
            status = 1;
            continue;
        } else std.fmt.parseInt(isize, target, 10) catch {
            rt.err.print("kpsh: kill: {s}: not a process id or %job\n", .{target});
            status = 1;
            continue;
        };
        switch (sys.check(sys.syscall2(sys.nr.kill, @bitCast(pid), sig))) {
            .ok => {},
            .err => |e| {
                rt.complain("kill", target, e);
                status = 1;
            },
        }
    }
    return status;
}

/// `bg`: a stopped job goes on, in the background.
fn bgJob(cmd: *const Command) u8 {
    const job = pickJob(cmd) orelse return 1;
    continueJob(job);
    rt.err.print("[{d}]+ {s} &\n", .{ job.number, job.name() });
    return 0;
}

fn listJobs() u8 {
    for (jobs) |slot| {
        const j = slot orelse continue;
        rt.out.print("[{d}]   {s}                 {s}\n", .{ j.number, if (j.stopped) "Stopped" else "Running", j.name() });
    }
    return 0;
}

fn runPipeline(commands: []Command, in_background: bool, label: []const u8) u8 {
    // A lone builtin runs in the shell itself, so `cd` and `export` affect the shell.
    if (commands.len == 1 and commands[0].redirect_count == 0 and !in_background) {
        if (commands[0].argc == 0) return 0;
        if (builtin(&commands[0])) |status| return status;
    }
    var pgid: usize = 0;
    var last: usize = 0;
    var prev_read: ?i32 = null;
    for (commands, 0..) |*cmd, i| {
        var fds: [2]i32 = .{ -1, -1 };
        const is_last = i + 1 == commands.len;
        if (!is_last) _ = sys.syscall2(293, @intFromPtr(&fds), sys.O_CLOEXEC); // pipe2
        const pid = sys.syscall0(sys.nr.fork);
        if (sys.Result.of(pid) == .err) {
            rt.err.write("kpsh: fork failed\n");
            return 1;
        }
        if (pid == 0) {
            if (prev_read) |r| _ = sys.syscall2(sys.nr.dup2, @intCast(r), 0);
            if (!is_last) _ = sys.syscall2(sys.nr.dup2, @intCast(fds[1]), 1);
            runInChild(cmd, in_background, pgid);
        }
        // The first process starts the job's group; the shell sets it too, so it's in place
        // whichever of the two runs first.
        if (pgid == 0) pgid = pid;
        if (job_control) _ = sys.syscall2(sys.nr.setpgid, pid, pgid);
        if (i == 0 and !in_background) giveTerminal(pgid);
        last = pid;
        if (prev_read) |r| sys.close(r);
        if (!is_last) {
            sys.close(fds[1]);
            prev_read = fds[0];
        }
    }
    const job = newJob(pgid, last, label);
    if (in_background) {
        // The other processes in the pipeline are collected by reapJobs too.
        if (addJob(job)) |j| rt.err.print("[{d}] {d}\n", .{ j.number, last });
        return 0;
    }
    const status = waitJob(job);
    giveTerminal(shell_pgid);
    return status;
}

// ── Builtins ────────────────────────────────────────────────────────────────────────────────────

const help_text =
    \\kpsh, the KernelProject2 shell. Builtins:
    \\  cd [dir]          change directory (cd - goes back, cd alone goes home)
    \\  pwd               print the current directory
    \\  export NAME=value set an environment variable
    \\  unset NAME        remove one
    \\  env               list them
    \\  jobs              list jobs (Ctrl+Z stops the one running; & starts one in the background)
    \\  fg [%n]           bring a job back to the foreground
    \\  bg [%n]           let a stopped job go on in the background
    \\  wait [%n]         wait for background jobs to finish
    \\  kill [-SIG] PID|%n  stop a process or a job (TERM, or -KILL, -INT, -9...)
    \\  exec PROG [ARGS]  become PROG (the shell is replaced by it)
    \\  exit [code]       leave the shell
    \\  help              this text
    \\Everything else runs as a program from $PATH. Supported syntax:
    \\  a | b   a > file   a >> file   a < file   a 2> file   a 2>&1   a; b   a && b   a || b   a &
    \\  'quotes' "quotes $VAR"   $?   ~
    \\
;

/// Runs a builtin; returns null when `cmd` isn't one.
fn builtin(cmd: *const Command) ?u8 {
    const name = cmd.arg(0) orelse return null;
    // exec PROGRAM ARGS: the shell becomes the program (debuggers start programs this way).
    if (eql(u8, name, "exec")) {
        const program = cmd.arg(1) orelse return 0;
        var path_buf: [512]u8 = undefined;
        const path = findProgram(program, &path_buf) orelse {
            rt.err.print("kpsh: exec: {s}: not found\n", .{program});
            return 127;
        };
        rt.out.flush();
        rt.err.flush();
        const r = sys.check(sys.syscall3(sys.nr.execve, @intFromPtr(path), @intFromPtr(&cmd.argv[1]), @intFromPtr(envp())));
        rt.complain("kpsh: exec", program, r.err);
        return 126;
    }
    if (eql(u8, name, "jobs")) return listJobs();
    if (eql(u8, name, "fg")) return fgJob(cmd);
    if (eql(u8, name, "bg")) return bgJob(cmd);
    if (eql(u8, name, "kill")) return killCommand(cmd);
    if (eql(u8, name, "wait")) return waitJobs(cmd);
    if (eql(u8, name, "cd")) {
        var target = cmd.arg(1) orelse (getVar("HOME") orelse "/");
        var old_buf: [512]u8 = undefined;
        const old = cwd(&old_buf);
        if (eql(u8, target, "-")) target = getVar("OLDPWD") orelse ".";
        var buf: [512]u8 = undefined;
        const path = rt.cstr(&buf, target) orelse return 1;
        const r = sys.check(sys.syscall1(sys.nr.chdir, @intFromPtr(path)));
        if (r == .err) {
            rt.complain("cd", target, r.err);
            return 1;
        }
        setVar("OLDPWD", old);
        var new_buf: [512]u8 = undefined;
        setVar("PWD", cwd(&new_buf));
        return 0;
    }
    if (eql(u8, name, "pwd")) {
        var buf: [512]u8 = undefined;
        rt.out.print("{s}\n", .{cwd(&buf)});
        return 0;
    }
    if (eql(u8, name, "exit")) {
        const code = if (cmd.arg(1)) |a| std.fmt.parseInt(u8, a, 10) catch 2 else last_status;
        rt.out.flush();
        sys.exit(code);
    }
    if (eql(u8, name, "export")) {
        var i: usize = 1;
        while (cmd.arg(i)) |a| : (i += 1) {
            if (std.mem.indexOfScalar(u8, a, '=')) |eq| setVar(a[0..eq], a[eq + 1 ..]);
        }
        return 0;
    }
    if (eql(u8, name, "unset")) {
        var i: usize = 1;
        while (cmd.arg(i)) |a| : (i += 1) unsetVar(a);
        return 0;
    }
    if (eql(u8, name, "env") and cmd.argc == 1) {
        for (vars[0..var_count]) |*v| rt.out.print("{s}\n", .{v.buf[0..v.len]});
        return 0;
    }
    if (eql(u8, name, "help")) {
        rt.out.write(help_text);
        return 0;
    }
    if (eql(u8, name, "true") or eql(u8, name, ":")) return 0;
    if (eql(u8, name, "false")) return 1;
    return null;
}

fn cwd(buf: []u8) []const u8 {
    const r = sys.check(sys.syscall2(sys.nr.getcwd, @intFromPtr(buf.ptr), buf.len));
    return switch (r) {
        .ok => |n| buf[0 .. n - 1],
        .err => "?",
    };
}

// ── Running lines ───────────────────────────────────────────────────────────────────────────────

/// Where the next `;`, `&&`, `||` or lone `&` outside quotes is: (index, length, kind).
fn nextSeparator(line: []const u8, from: usize) ?struct { at: usize, len: usize, kind: Kind } {
    var i = from;
    while (i < line.len) : (i += 1) {
        switch (line[i]) {
            '\\' => i += 1,
            '\'' => i = std.mem.indexOfScalarPos(u8, line, i + 1, '\'') orelse return null,
            '"' => {
                i += 1;
                while (i < line.len and line[i] != '"') : (i += 1) {
                    if (line[i] == '\\') i += 1;
                }
            },
            '#' => if (i == 0 or line[i - 1] == ' ' or line[i - 1] == '\t') return null,
            ';' => return .{ .at = i, .len = 1, .kind = .semi },
            // `>&` and `<&` are redirections (2>&1), not "run in the background".
            '&' => if (i > 0 and (line[i - 1] == '>' or line[i - 1] == '<')) {} else return if (i + 1 < line.len and line[i + 1] == '&') .{ .at = i, .len = 2, .kind = .and_if } else .{ .at = i, .len = 1, .kind = .background },
            '|' => if (i + 1 < line.len and line[i + 1] == '|') return .{ .at = i, .len = 2, .kind = .or_if } else {},
            else => {},
        }
    }
    return null;
}

/// Runs one line. Each part between ; && || is expanded just before it runs, so `$?` in
/// `false; echo $?` sees the status of `false`.
fn runLine(line: []const u8) void {
    var commands: [16]Command = undefined;
    var start: usize = 0;
    var skip = false;
    while (start <= line.len) {
        const sep = nextSeparator(line, start);
        const end = if (sep) |s| s.at else line.len;
        const kind: Kind = if (sep) |s| s.kind else .semi;
        if (!skip) runPart(line[start..end], &commands, kind == .background);
        // a && b runs b only after success; a || b only after failure.
        skip = switch (kind) {
            .and_if => last_status != 0,
            .or_if => last_status == 0,
            else => false,
        };
        start = if (sep) |s| s.at + s.len else break;
    }
}

fn runPart(part: []const u8, commands: *[16]Command, background: bool) void {
    tokenize(part) catch |e| {
        rt.err.write(switch (e) {
            error.Unclosed => "kpsh: syntax error: unclosed quote or ${\n",
            error.TooLong => "kpsh: line too long\n",
        });
        last_status = 2;
        return;
    };
    if (token_count == 0) return;
    const n = parsePipeline(0, token_count, commands) orelse {
        last_status = 2;
        return;
    };
    last_status = runPipeline(commands[0..n], background, part);
    rt.out.flush();
    rt.err.flush();
}

fn prompt() void {
    var buf: [512]u8 = undefined;
    var dir = cwd(&buf);
    const home = getVar("HOME") orelse "";
    var tilde = false;
    if (home.len > 1 and std.mem.startsWith(u8, dir, home) and (dir.len == home.len or dir[home.len] == '/')) {
        dir = dir[home.len..];
        tilde = true;
    }
    // Who we are, and the usual # for root, $ for everyone else.
    const uid = sys.syscall0(102); // the real user id
    const who = getVar("USER") orelse if (uid == 0) "root" else "user";
    rt.out.print("\x1b[1;32m{s}\x1b[0m:\x1b[1;34m{s}{s}\x1b[0m{s} ", .{ who, if (tilde) "~" else "", dir, if (uid == 0) "#" else "$" });
    rt.out.flush();
}

var line_buf: [8192]u8 = undefined;

fn runFile(fd: i32) void {
    var len: usize = 0;
    while (true) {
        switch (sys.read(fd, line_buf[len..])) {
            .ok => |n| {
                if (n == 0) break;
                len += n;
                if (len == line_buf.len) break;
            },
            .err => break,
        }
    }
    var lines = std.mem.splitScalar(u8, line_buf[0..len], '\n');
    var first = true;
    while (lines.next()) |line| {
        if (first and std.mem.startsWith(u8, line, "#!")) {
            first = false;
            continue;
        }
        first = false;
        var copy: [4096]u8 = undefined;
        const n = @min(line.len, copy.len);
        @memcpy(copy[0..n], line[0..n]);
        runLine(copy[0..n]);
    }
}

pub fn main(args: rt.Args) u8 {
    var i: usize = 0;
    while (args.envp[i]) |entry| : (i += 1) {
        const s = std.mem.span(entry);
        if (std.mem.indexOfScalar(u8, s, '=')) |eq| setVar(s[0..eq], s[eq + 1 ..]);
    }

    // kpsh -c "command"
    if (args.get(1)) |a| {
        if (eql(u8, a, "-c")) {
            const command = args.get(2) orelse {
                rt.err.write("kpsh: -c needs a command\n");
                return 2;
            };
            if (args.argv.len > 3) positional = args.argv[3..];
            runLine(command);
            return last_status;
        }
        // kpsh script.sh
        var buf: [512]u8 = undefined;
        const path = rt.cstr(&buf, a) orelse return 2;
        switch (sys.open(path, sys.O_RDONLY, 0)) {
            .ok => |fd| {
                positional = args.argv[1..];
                runFile(@intCast(fd));
                return last_status;
            },
            .err => |e| {
                rt.complain("kpsh", a, e);
                return 127;
            },
        }
    }

    // The shell itself ignores Ctrl+C, Ctrl+\ and Ctrl+Z; the programs it starts don't. On a
    // terminal it runs jobs: it leads a process group of its own, and gives the terminal to each
    // job it runs in the foreground (and takes it back).
    for ([_]usize{ SIGINT, SIGQUIT, SIGTSTP, SIGTTIN, SIGTTOU }) |sig| setSignal(sig, 1);
    var termios: [60]u8 = undefined;
    if (sys.Result.of(sys.syscall3(sys.nr.ioctl, 0, 0x5401, @intFromPtr(&termios))) == .ok) { // TCGETS: a terminal
        _ = sys.syscall2(sys.nr.setpgid, 0, 0);
        shell_pgid = sys.syscall0(sys.nr.getpid);
        job_control = true;
        giveTerminal(shell_pgid);
    }
    var buf: [512]u8 = undefined;
    setVar("PWD", cwd(&buf));

    while (true) {
        reapJobs();
        prompt();
        const n = switch (sys.read(0, line_buf[0 .. line_buf.len - 1])) {
            .ok => |n| n,
            .err => |e| {
                if (e == sys.EINTR) continue;
                return 1;
            },
        };
        if (n == 0) {
            rt.out.write("exit\n");
            return last_status;
        }
        runLine(line_buf[0..n]);
    }
}
