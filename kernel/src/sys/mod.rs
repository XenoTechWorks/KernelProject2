//! System calls, numbered and behaving like Linux x86-64's. That's the whole compatibility story: a
//! program built for Linux makes these calls with these numbers and gets the answers Linux would give.
//!
//! Calls that aren't written yet return ENOSYS and are logged on the serial port once, which is how
//! to find out what a new program needs next.

pub mod errno;
pub mod fs;
pub mod mem;
pub mod misc;
pub mod process;

use crate::arch::idt::TrapFrame;
use errno::*;

pub type SysResult = Result<u64, Errno>;

pub fn dispatch(frame: &mut TrapFrame) {
    let number = frame.rax;
    let a = [frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9];
    let result = call(frame, number, a);
    frame.rax = match result {
        Ok(v) => v,
        Err(e) => (-e) as u64,
    };
    crate::proc::check_kill();
}

fn call(frame: &mut TrapFrame, n: u64, a: [u64; 6]) -> SysResult {
    match n {
        0 => fs::read(a[0] as i32, a[1], a[2] as usize),
        1 => fs::write(a[0] as i32, a[1], a[2] as usize),
        2 => fs::openat(AT_FDCWD, a[0], a[1] as u32, a[2] as u32),
        3 => fs::close(a[0] as i32),
        4 => fs::fstatat(AT_FDCWD, a[0], a[1], 0),
        5 => fs::fstat(a[0] as i32, a[1]),
        6 => fs::fstatat(AT_FDCWD, a[0], a[1], AT_SYMLINK_NOFOLLOW),
        7 => fs::poll(a[0], a[1] as usize, a[2] as i32),
        8 => fs::lseek(a[0] as i32, a[1] as i64, a[2] as u32),
        9 => mem::mmap(a[0], a[1], a[2] as u32, a[3] as u32, a[4] as i32, a[5]),
        10 => mem::mprotect(a[0], a[1], a[2] as u32),
        11 => mem::munmap(a[0], a[1]),
        12 => mem::brk(a[0]),
        13 => process::rt_sigaction(a[0] as usize, a[1], a[2]),
        14 => process::rt_sigprocmask(a[0] as u32, a[1], a[2]),
        16 => fs::ioctl(a[0] as i32, a[1] as u32, a[2]),
        17 => fs::pread(a[0] as i32, a[1], a[2] as usize, a[3]),
        18 => fs::pwrite(a[0] as i32, a[1], a[2] as usize, a[3]),
        19 => fs::readv(a[0] as i32, a[1], a[2] as usize),
        20 => fs::writev(a[0] as i32, a[1], a[2] as usize),
        21 => fs::faccessat(AT_FDCWD, a[0], a[1] as u32),
        22 => fs::pipe2(a[0], 0),
        23 => fs::select(a[0] as usize, a[1], a[2], a[3], a[4], false),
        24 => process::sched_yield(),
        28 => Ok(0), // madvise: advice only
        32 => fs::dup(a[0] as i32),
        33 => fs::dup3(a[0] as i32, a[1] as i32, 0, true),
        35 => misc::nanosleep(a[0]),
        39 => Ok(crate::proc::current().pid as u64),
        57 => process::clone(frame, 17, 0, 0, 0, 0),
        58 => process::clone(frame, 0x100 | 0x4000 | 17, 0, 0, 0, 0),
        56 => process::clone(frame, a[0], a[1], a[2], a[3], a[4]),
        59 => process::execve(frame, AT_FDCWD, a[0], a[1], a[2]),
        60 | 231 => crate::proc::exit(((a[0] & 0xff) << 8) as i32),
        61 => process::wait4(a[0] as i32 as i64, a[1], a[2] as u32),
        62 => process::kill(a[0] as i32 as i64, a[1] as usize),
        63 => misc::uname(a[0]),
        72 => fs::fcntl(a[0] as i32, a[1] as u32, a[2]),
        74 | 75 | 162 => Ok(0), // fsync, fdatasync, sync: everything is in memory
        76 => fs::truncate_path(a[0], a[1]),
        77 => fs::ftruncate(a[0] as i32, a[1]),
        79 => fs::getcwd(a[0], a[1] as usize),
        80 => fs::chdir(a[0]),
        81 => fs::fchdir(a[0] as i32),
        82 => fs::renameat(AT_FDCWD, a[0], AT_FDCWD, a[1]),
        83 => fs::mkdirat(AT_FDCWD, a[0], a[1] as u32),
        84 => fs::unlinkat(AT_FDCWD, a[0], AT_REMOVEDIR),
        85 => fs::openat(AT_FDCWD, a[0], 0o1101, a[1] as u32), // creat: O_CREAT|O_WRONLY|O_TRUNC
        87 => fs::unlinkat(AT_FDCWD, a[0], 0),
        88 => fs::symlinkat(a[0], AT_FDCWD, a[1]),
        89 => fs::readlinkat(AT_FDCWD, a[0], a[1], a[2] as usize),
        90 | 91 | 268 => fs::chmod(n, a),
        92 | 93 | 94 | 260 => Ok(0), // chown family: everyone is root for now
        95 => process::umask(a[0] as u32),
        96 => misc::gettimeofday(a[0]),
        97 => misc::getrlimit(a[0] as u32, a[1]),
        98 => misc::getrusage(a[1]),
        99 => misc::sysinfo(a[0]),
        100 => misc::times(a[0]),
        102 | 104 | 107 | 108 => Ok(0), // getuid, getgid, geteuid, getegid: root
        105 | 106 | 113 | 114 | 117 | 119 => Ok(0), // setuid and friends
        115 => Ok(0),                               // getgroups: none
        118 | 120 => process::getresid(a),
        109 => process::setpgid(a[0] as u32, a[1] as u32),
        110 => Ok(crate::proc::current().ppid as u64),
        111 => Ok(crate::proc::current().pgid as u64),
        112 => process::setsid(),
        121 => process::getpgid(a[0] as u32),
        124 => process::getsid(a[0] as u32),
        131 => Ok(0), // sigaltstack
        137 => fs::statfs(a[1]),
        138 => fs::statfs(a[1]),
        157 => Ok(0), // prctl: accept and ignore (PR_SET_NAME etc.)
        158 => process::arch_prctl(a[0] as u32, a[1]),
        160 => Ok(0), // setrlimit
        186 => Ok(crate::proc::current().pid as u64), // gettid: one thread per process for now
        200 | 234 => process::kill(a[0] as i32 as i64, a[if n == 200 { 1 } else { 2 }] as usize), // tkill, tgkill
        201 => misc::time(a[0]),
        202 => misc::futex(a[0], a[1] as u32, a[2] as u32),
        204 => misc::sched_getaffinity(a[1] as usize, a[2]),
        217 => fs::getdents64(a[0] as i32, a[1], a[2] as usize),
        218 => {
            crate::proc::current().clear_child_tid = a[0];
            Ok(crate::proc::current().pid as u64)
        }
        228 => misc::clock_gettime(a[0] as u32, a[1]),
        229 => misc::clock_getres(a[1]),
        230 => misc::clock_nanosleep(a[2]),
        257 => fs::openat(a[0] as i32, a[1], a[2] as u32, a[3] as u32),
        258 => fs::mkdirat(a[0] as i32, a[1], a[2] as u32),
        262 => fs::fstatat(a[0] as i32, a[1], a[2], a[3] as u32),
        263 => fs::unlinkat(a[0] as i32, a[1], a[2] as u32),
        264 => fs::renameat(a[0] as i32, a[1], a[2] as i32, a[3]),
        266 => fs::symlinkat(a[0], a[1] as i32, a[2]),
        267 => fs::readlinkat(a[0] as i32, a[1], a[2], a[3] as usize),
        269 | 439 => fs::faccessat(a[0] as i32, a[1], a[2] as u32),
        270 => fs::select(a[0] as usize, a[1], a[2], a[3], a[4], true),
        271 => fs::poll(a[0], a[1] as usize, -1),
        273 => Ok(0), // set_robust_list
        280 => Ok(0), // utimensat
        292 => fs::dup3(a[0] as i32, a[1] as i32, a[2] as u32, false),
        293 => fs::pipe2(a[0], a[1] as u32),
        302 => misc::prlimit64(a[1] as u32, a[3]),
        316 => fs::renameat(a[0] as i32, a[1], a[2] as i32, a[3]),
        318 => misc::getrandom(a[0], a[1] as usize),
        332 => fs::statx(a[0] as i32, a[1], a[2] as u32, a[4]),
        334 => Err(ENOSYS), // rseq: glibc works without it
        435 => Err(ENOSYS), // clone3: glibc falls back to clone
        _ => {
            unimplemented(n);
            Err(ENOSYS)
        }
    }
}

fn unimplemented(n: u64) {
    static SEEN: spin::Mutex<[u64; 8]> = spin::Mutex::new([0; 8]);
    let mut seen = SEEN.lock();
    let (word, bit) = ((n / 64) as usize, n % 64);
    if word < seen.len() && seen[word] & (1 << bit) == 0 {
        seen[word] |= 1 << bit;
        let p = crate::proc::current();
        crate::log!("syscall {n} is not implemented yet (first used by pid {} {})", p.pid, p.name);
    }
}
