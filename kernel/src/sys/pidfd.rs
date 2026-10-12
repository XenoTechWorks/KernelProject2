//! pidfd: a descriptor that stands for a process. It becomes readable when the process ends, so
//! an event loop can wait for a child alongside everything else; signals can be sent through it;
//! and waitid can wait for "the process this pidfd is". Process ids aren't reused here, so a
//! pidfd can't come to mean someone else.

use super::SysResult;
use super::errno::*;
use crate::proc::{self, State, fd::Handle, fd::OpenFile, signal};
use alloc::string::String;
use alloc::sync::Arc;

pub struct PidFd {
    pid: u32,
}

const PIDFD_NONBLOCK: u32 = 0o4000;

/// pidfd_open: for a process (not a thread). Like Linux's, it's always close-on-exec.
pub fn open(pid: u32, flags: u32) -> SysResult {
    if flags & !PIDFD_NONBLOCK != 0 {
        return Err(EINVAL);
    }
    let p = proc::get(pid).ok_or(ESRCH)?;
    if p.pid != p.tgid {
        return Err(EINVAL);
    }
    let file = OpenFile::new(Handle::PidFd(Arc::new(PidFd { pid })), 2 | flags, String::from("anon_inode:[pidfd]"));
    Ok(proc::current().files().fds.insert(file, true, 0)? as u64)
}

/// The process a pidfd stands for (waitid's P_PIDFD).
pub fn pid_of(fd: i32) -> Result<u32, Errno> {
    match &proc::current().files().fds.get(fd)?.handle {
        Handle::PidFd(p) => Ok(p.pid),
        _ => Err(EINVAL),
    }
}

/// pidfd_send_signal: kill() through a pidfd.
pub fn send_signal(fd: i32, sig: usize, flags: u32) -> SysResult {
    if flags != 0 || sig > 64 {
        return Err(EINVAL);
    }
    let pid = pid_of(fd)?;
    if ended(pid) {
        return Err(ESRCH);
    }
    proc::signal_process_info(pid, sig, signal::Info::from_caller(signal::SI_USER))?;
    Ok(0)
}

fn ended(pid: u32) -> bool {
    proc::get(pid).is_none_or(|p| matches!(p.state, State::Zombie(_) | State::Dead))
}

/// Readable once the process has ended.
pub fn readable(p: &PidFd) -> bool {
    ended(p.pid)
}
