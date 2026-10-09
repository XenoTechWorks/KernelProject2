//! kpsh: the KernelProject2 shell (a working name).
//!
//! A shell in the Linux/POSIX shape: read a line, split it into words, run builtins itself, and run
//! everything else as a separate program with fork + execve + wait4. Supports pipes (|), redirection
//! (> >> < 2> 2>&1), lists (; && ||), background jobs (&), quotes, backslash escapes, $VAR / $? / ~ expansion and scripts
//! (`kpsh file`, `kpsh -c "command"`, or a file starting with #!/bin/kpsh).

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

/// Expands $NAME, ${NAME} or $? starting at line[i] (just after the $). Returns the new index.
fn expandVar(line: []const u8, i: usize) ParseError!usize {
    if (i < line.len and line[i] == '?') {
        var tmp: [4]u8 = undefined;
        try pushStr(std.fmt.bufPrint(&tmp, "{d}", .{last_status}) catch "");
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
        const start = pool_len;
        if (c == '~' and (i + 1 == line.len or std.mem.indexOfScalar(u8, "/ \t\r\n;|&<>", line[i + 1]) != null)) {
            try pushStr(getVar("HOME") orelse "/");
            i += 1;
        }
        while (i < line.len) {
            const ch = line[i];
            switch (ch) {
                ' ', '\t', '\r', '\n', ';', '|', '&', '>', '<' => break,
                '\\' => {
                    if (i + 1 < line.len) try pushChar(line[i + 1]);
                    i += 2;
                },
                '\'' => {
                    const end = std.mem.indexOfScalarPos(u8, line, i + 1, '\'') orelse return error.Unclosed;
                    try pushStr(line[i + 1 .. end]);
                    i = end + 1;
                },
                '"' => {
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
                '$' => i = try expandVar(line, i + 1),
                else => {
                    try pushChar(ch);
                    i += 1;
                },
            }
        }
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

/// In a child process: run one command and never return.
fn runInChild(cmd: *const Command, background: bool) noreturn {
    if (background) {
        // Like other shells without job control: a background job ignores Ctrl+C and doesn't read
        // the terminal (its input is /dev/null unless redirected).
        switch (sys.open("/dev/null", sys.O_RDONLY, 0)) {
            .ok => |fd| {
                _ = sys.syscall2(sys.nr.dup2, fd, 0);
                sys.close(@intCast(fd));
            },
            .err => {},
        }
    } else {
        setSignal(2, 0); // SIGINT back to default, so Ctrl+C stops the program
        setSignal(3, 0);
    }
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

// Background jobs still running: their last process, and the job number shown for it.
const Job = struct { pid: usize, number: usize };
var jobs: [16]?Job = @splat(null);

fn addJob(pid: usize) void {
    var number: usize = 1;
    while (true) : (number += 1) {
        for (jobs) |j| {
            if (j != null and j.?.number == number) break;
        } else break;
    }
    for (&jobs) |*slot| {
        if (slot.* == null) {
            slot.* = .{ .pid = pid, .number = number };
            break;
        }
    }
    rt.err.print("[{d}] {d}\n", .{ number, pid });
}

/// Collects finished background processes and says which jobs are done (before each prompt).
fn reapJobs() void {
    while (true) {
        var raw: u32 = 0;
        const r = sys.check(sys.syscall4(sys.nr.wait4, @as(usize, @bitCast(@as(isize, -1))), @intFromPtr(&raw), 1, 0)); // WNOHANG
        const pid = switch (r) {
            .ok => |pid| pid,
            .err => return,
        };
        if (pid == 0) return;
        for (&jobs) |*slot| {
            if (slot.*) |j| {
                if (j.pid == pid) {
                    rt.err.print("[{d}] Done\n", .{j.number});
                    slot.* = null;
                }
            }
        }
    }
}

fn runPipeline(commands: []Command, background: bool) u8 {
    // A lone builtin runs in the shell itself, so `cd` and `export` affect the shell.
    if (commands.len == 1 and commands[0].redirect_count == 0 and !background) {
        if (commands[0].argc == 0) return 0;
        if (builtin(&commands[0])) |status| return status;
    }
    var pids: [16]usize = undefined;
    var prev_read: ?i32 = null;
    for (commands, 0..) |*cmd, i| {
        var fds: [2]i32 = .{ -1, -1 };
        const last = i + 1 == commands.len;
        if (!last) _ = sys.syscall2(293, @intFromPtr(&fds), sys.O_CLOEXEC); // pipe2
        const pid = sys.syscall0(sys.nr.fork);
        if (sys.Result.of(pid) == .err) {
            rt.err.write("kpsh: fork failed\n");
            return 1;
        }
        if (pid == 0) {
            if (prev_read) |r| _ = sys.syscall2(sys.nr.dup2, @intCast(r), 0);
            if (!last) _ = sys.syscall2(sys.nr.dup2, @intCast(fds[1]), 1);
            runInChild(cmd, background);
        }
        pids[i] = pid;
        if (prev_read) |r| sys.close(r);
        if (!last) {
            sys.close(fds[1]);
            prev_read = fds[0];
        }
    }
    if (background) {
        // The other processes in the pipeline are collected by reapJobs too.
        addJob(pids[commands.len - 1]);
        return 0;
    }
    var status: u8 = 0;
    for (pids[0..commands.len], 0..) |pid, i| {
        var raw: u32 = 0;
        while (true) {
            const r = sys.check(sys.syscall4(sys.nr.wait4, pid, @intFromPtr(&raw), 0, 0));
            if (r == .ok or r.err != sys.EINTR) break;
        }
        if (i + 1 < commands.len) continue;
        const sig = raw & 0x7f;
        if (sig == 0) {
            status = @intCast((raw >> 8) & 0xff);
        } else {
            status = @intCast(128 + sig);
            if (describeSignal(sig)) |text| rt.err.print("{s}\n", .{text});
        }
    }
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
            '&' => return if (i + 1 < line.len and line[i + 1] == '&') .{ .at = i, .len = 2, .kind = .and_if } else .{ .at = i, .len = 1, .kind = .background },
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
    last_status = runPipeline(commands[0..n], background);
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
    rt.out.print("\x1b[1;32mroot\x1b[0m:\x1b[1;34m{s}{s}\x1b[0m# ", .{ if (tilde) "~" else "", dir });
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
            runLine(command);
            return last_status;
        }
        // kpsh script.sh
        var buf: [512]u8 = undefined;
        const path = rt.cstr(&buf, a) orelse return 2;
        switch (sys.open(path, sys.O_RDONLY, 0)) {
            .ok => |fd| {
                runFile(@intCast(fd));
                return last_status;
            },
            .err => |e| {
                rt.complain("kpsh", a, e);
                return 127;
            },
        }
    }

    // The shell itself ignores Ctrl+C and Ctrl+\; the programs it starts don't.
    setSignal(2, 1);
    setSignal(3, 1);
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
