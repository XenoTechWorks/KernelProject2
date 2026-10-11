//! Linux x86-64 system calls, made directly with the `syscall` instruction. These programs use no libc
//! and none of Zig's OS layer, so they run on KernelProject2 and on real Linux alike.

pub const nr = struct {
    pub const read = 0;
    pub const write = 1;
    pub const open = 2;
    pub const close = 3;
    pub const stat = 4;
    pub const fstat = 5;
    pub const lstat = 6;
    pub const lseek = 8;
    pub const rt_sigaction = 13;
    pub const ioctl = 16;
    pub const pipe = 22;
    pub const dup2 = 33;
    pub const nanosleep = 35;
    pub const getpid = 39;
    pub const fork = 57;
    pub const execve = 59;
    pub const exit = 60;
    pub const wait4 = 61;
    pub const kill = 62;
    pub const uname = 63;
    pub const getcwd = 79;
    pub const chdir = 80;
    pub const rename = 82;
    pub const mkdir = 83;
    pub const rmdir = 84;
    pub const unlink = 87;
    pub const readlink = 89;
    pub const sysinfo = 99;
    pub const getppid = 110;
    pub const setpgid = 109;
    pub const getdents64 = 217;
    pub const clock_gettime = 228;
    pub const exit_group = 231;
    pub const openat = 257;
    pub const utimensat = 280;
};

pub inline fn syscall0(n: usize) usize {
    return asm volatile ("syscall"
        : [ret] "={rax}" (-> usize),
        : [n] "{rax}" (n),
        : .{ .rcx = true, .r11 = true, .memory = true });
}

pub inline fn syscall1(n: usize, a: usize) usize {
    return asm volatile ("syscall"
        : [ret] "={rax}" (-> usize),
        : [n] "{rax}" (n),
          [a] "{rdi}" (a),
        : .{ .rcx = true, .r11 = true, .memory = true });
}

pub inline fn syscall2(n: usize, a: usize, b: usize) usize {
    return asm volatile ("syscall"
        : [ret] "={rax}" (-> usize),
        : [n] "{rax}" (n),
          [a] "{rdi}" (a),
          [b] "{rsi}" (b),
        : .{ .rcx = true, .r11 = true, .memory = true });
}

pub inline fn syscall3(n: usize, a: usize, b: usize, c: usize) usize {
    return asm volatile ("syscall"
        : [ret] "={rax}" (-> usize),
        : [n] "{rax}" (n),
          [a] "{rdi}" (a),
          [b] "{rsi}" (b),
          [c] "{rdx}" (c),
        : .{ .rcx = true, .r11 = true, .memory = true });
}

pub inline fn syscall4(n: usize, a: usize, b: usize, c: usize, d: usize) usize {
    return asm volatile ("syscall"
        : [ret] "={rax}" (-> usize),
        : [n] "{rax}" (n),
          [a] "{rdi}" (a),
          [b] "{rsi}" (b),
          [c] "{rdx}" (c),
          [d] "{r10}" (d),
        : .{ .rcx = true, .r11 = true, .memory = true });
}

pub inline fn syscall5(n: usize, a: usize, b: usize, c: usize, d: usize, e: usize) usize {
    return asm volatile ("syscall"
        : [ret] "={rax}" (-> usize),
        : [n] "{rax}" (n),
          [a] "{rdi}" (a),
          [b] "{rsi}" (b),
          [c] "{rdx}" (c),
          [d] "{r10}" (d),
          [e] "{r8}" (e),
        : .{ .rcx = true, .r11 = true, .memory = true });
}

pub inline fn syscall6(n: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) usize {
    return asm volatile ("syscall"
        : [ret] "={rax}" (-> usize),
        : [n] "{rax}" (n),
          [a] "{rdi}" (a),
          [b] "{rsi}" (b),
          [c] "{rdx}" (c),
          [d] "{r10}" (d),
          [e] "{r8}" (e),
          [f] "{r9}" (f),
        : .{ .rcx = true, .r11 = true, .memory = true });
}

/// A system call result: a value, or a Linux error number.
pub const Result = union(enum) {
    ok: usize,
    err: u16,

    pub fn of(raw: usize) Result {
        const signed: isize = @bitCast(raw);
        return if (signed < 0 and signed > -4096) .{ .err = @intCast(-signed) } else .{ .ok = raw };
    }
};

pub fn check(raw: usize) Result {
    return Result.of(raw);
}

pub const ENOENT = 2;
pub const EINTR = 4;
pub const EACCES = 13;
pub const EEXIST = 17;
pub const ENOTDIR = 20;
pub const EISDIR = 21;
pub const ENOEXEC = 8;
pub const ENOTEMPTY = 39;

/// A short explanation of an error number, like strerror().
pub fn errorText(e: u16) []const u8 {
    return switch (e) {
        1 => "Operation not permitted",
        2 => "No such file or directory",
        4 => "Interrupted",
        8 => "Exec format error",
        9 => "Bad file descriptor",
        10 => "No child processes",
        12 => "Out of memory",
        13 => "Permission denied",
        14 => "Bad address",
        17 => "File exists",
        20 => "Not a directory",
        21 => "Is a directory",
        22 => "Invalid argument",
        24 => "Too many open files",
        25 => "Not a terminal",
        28 => "No space left on device",
        32 => "Broken pipe",
        36 => "File name too long",
        38 => "Function not implemented",
        39 => "Directory not empty",
        40 => "Too many levels of symbolic links",
        else => "Unknown error",
    };
}

pub fn write(fd: i32, bytes: []const u8) Result {
    return check(syscall3(nr.write, @bitCast(@as(isize, fd)), @intFromPtr(bytes.ptr), bytes.len));
}

pub fn read(fd: i32, buf: []u8) Result {
    return check(syscall3(nr.read, @bitCast(@as(isize, fd)), @intFromPtr(buf.ptr), buf.len));
}

pub fn open(path: [*:0]const u8, flags: usize, mode: usize) Result {
    return check(syscall4(nr.openat, @bitCast(@as(isize, -100)), @intFromPtr(path), flags, mode));
}

pub fn close(fd: i32) void {
    _ = syscall1(nr.close, @bitCast(@as(isize, fd)));
}

pub fn exit(code: u8) noreturn {
    _ = syscall1(nr.exit_group, code);
    unreachable;
}

pub const O_RDONLY = 0;
pub const O_WRONLY = 1;
pub const O_RDWR = 2;
pub const O_CREAT = 0o100;
pub const O_TRUNC = 0o1000;
pub const O_APPEND = 0o2000;
pub const O_DIRECTORY = 0o200000;
pub const O_CLOEXEC = 0o2000000;

/// Linux's `struct stat` on x86-64.
pub const Stat = extern struct {
    dev: u64,
    ino: u64,
    nlink: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    pad0: u32,
    rdev: u64,
    size: i64,
    blksize: i64,
    blocks: i64,
    atime: i64,
    atime_ns: i64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
    unused: [3]i64,
};

pub fn stat(path: [*:0]const u8, out: *Stat) Result {
    return check(syscall2(nr.stat, @intFromPtr(path), @intFromPtr(out)));
}

pub fn lstat(path: [*:0]const u8, out: *Stat) Result {
    return check(syscall2(nr.lstat, @intFromPtr(path), @intFromPtr(out)));
}

pub const S_IFMT = 0o170000;
pub const S_IFDIR = 0o040000;
pub const S_IFREG = 0o100000;
pub const S_IFLNK = 0o120000;
pub const S_IFCHR = 0o020000;
pub const S_IFIFO = 0o010000;
