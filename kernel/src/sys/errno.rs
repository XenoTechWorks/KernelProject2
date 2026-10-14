//! Linux error numbers (what a failed system call returns, negated). The full table is kept even
//! where a number isn't used yet.
#![allow(dead_code)]

pub type Errno = i64;

pub const EPERM: Errno = 1;
pub const ENOENT: Errno = 2;
pub const ESRCH: Errno = 3;
pub const EINTR: Errno = 4;
pub const EIO: Errno = 5;
pub const ENXIO: Errno = 6;
pub const E2BIG: Errno = 7;
pub const ENOEXEC: Errno = 8;
pub const EBADF: Errno = 9;
pub const ECHILD: Errno = 10;
pub const EAGAIN: Errno = 11;
pub const ENOMEM: Errno = 12;
pub const EACCES: Errno = 13;
pub const EFAULT: Errno = 14;
pub const EBUSY: Errno = 16;
pub const EEXIST: Errno = 17;
pub const EXDEV: Errno = 18;
pub const ENOTDIR: Errno = 20;
pub const EISDIR: Errno = 21;
pub const EINVAL: Errno = 22;
pub const EMFILE: Errno = 24;
pub const ENOTTY: Errno = 25;
pub const ENODEV: Errno = 19;
pub const EFBIG: Errno = 27;
pub const ENOSPC: Errno = 28;
pub const ESPIPE: Errno = 29;
pub const EPIPE: Errno = 32;
pub const ERANGE: Errno = 34;
pub const ENAMETOOLONG: Errno = 36;
pub const ENOSYS: Errno = 38;
pub const EADDRINUSE: Errno = 98;
pub const EISCONN: Errno = 106;
pub const ECONNRESET: Errno = 104;
pub const ECONNREFUSED: Errno = 111;
pub const EALREADY: Errno = 114;
pub const EINPROGRESS: Errno = 115;
pub const EOPNOTSUPP: Errno = 95;
pub const ENOTSOCK: Errno = 88;
pub const EDESTADDRREQ: Errno = 89;
pub const EPROTOTYPE: Errno = 91;
pub const ENOPROTOOPT: Errno = 92;
pub const EPROTONOSUPPORT: Errno = 93;
pub const EAFNOSUPPORT: Errno = 97;
pub const ENETDOWN: Errno = 100;
pub const ENETUNREACH: Errno = 101;
pub const ENOTCONN: Errno = 107;
pub const ENOTEMPTY: Errno = 39;
pub const ELOOP: Errno = 40;
pub const ELIBBAD: Errno = 80;
pub const ETIMEDOUT: Errno = 110;

pub const AT_FDCWD: i32 = -100;
pub const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
pub const AT_REMOVEDIR: u32 = 0x200;
pub const AT_EMPTY_PATH: u32 = 0x1000;
