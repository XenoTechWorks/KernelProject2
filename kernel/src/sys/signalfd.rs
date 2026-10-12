//! signalfd: signals read from a descriptor instead of run as handlers, so an event loop can wait
//! for them alongside everything else (with poll or epoll). The program blocks the signals, and
//! reading takes the pending ones in its set, each as a `struct signalfd_siginfo`.

use super::SysResult;
use super::errno::*;
use crate::mm::user;
use crate::proc::{self, fd::Handle, fd::OpenFile, signal};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

pub struct SignalFd {
    /// Which signals it reads (bit n-1 for signal n).
    mask: Mutex<u64>,
}

const SFD_NONBLOCK: u32 = 0o4000;
const SFD_CLOEXEC: u32 = 0o2000000;
/// `struct signalfd_siginfo` is 128 bytes: the number, errno, code, the sender's pid and uid,
/// and (at 40) a child's status; the rest stays 0.
const INFO_SIZE: usize = 128;

/// signalfd4: a new one (fd -1), or a new set for an existing one.
pub fn signalfd(fd: i32, mask_at: u64, flags: u32) -> SysResult {
    if flags & !(SFD_NONBLOCK | SFD_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    let p = proc::current();
    let mask = user::read_u64(p.space(), mask_at)? & !(signal::bit(proc::SIGKILL) | signal::bit(proc::SIGSTOP));
    if fd != -1 {
        let file = p.files().fds.get(fd)?;
        let Handle::SignalFd(s) = &file.handle else { return Err(EINVAL) };
        *s.mask.lock() = mask;
        return Ok(fd as u64);
    }
    let file = OpenFile::new(
        Handle::SignalFd(Arc::new(SignalFd { mask: Mutex::new(mask) })),
        2 | flags & SFD_NONBLOCK,
        String::from("anon_inode:[signalfd]"),
    );
    Ok(p.files().fds.insert(file, flags & SFD_CLOEXEC != 0, 0)? as u64)
}

/// Takes pending signals in the set, as many as fit; waits for one unless non-blocking.
pub fn read(s: &SignalFd, buf: &mut [u8], nonblocking: bool) -> Result<usize, Errno> {
    if buf.len() < INFO_SIZE {
        return Err(EINVAL);
    }
    loop {
        let p = proc::current();
        let mask = *s.mask.lock();
        let mut taken = Vec::new();
        while taken.len() < buf.len() / INFO_SIZE && p.pending & mask != 0 {
            let sig = (p.pending & mask).trailing_zeros() as usize + 1;
            taken.push((sig, signal::take(p, sig)));
        }
        if !taken.is_empty() {
            buf[..taken.len() * INFO_SIZE].fill(0);
            for (i, (sig, info)) in taken.iter().enumerate() {
                let b = &mut buf[i * INFO_SIZE..(i + 1) * INFO_SIZE];
                b[0..4].copy_from_slice(&(*sig as u32).to_le_bytes());
                b[8..12].copy_from_slice(&info.code.to_le_bytes());
                b[12..16].copy_from_slice(&info.pid.to_le_bytes());
                b[16..20].copy_from_slice(&info.uid.to_le_bytes());
                b[40..44].copy_from_slice(&info.status.to_le_bytes());
            }
            return Ok(taken.len() * INFO_SIZE);
        }
        if nonblocking {
            return Err(EAGAIN);
        }
        proc::sleep()?;
    }
}

/// Is one of its signals pending for the process asking?
pub fn readable(s: &SignalFd) -> bool {
    proc::current().pending & *s.mask.lock() != 0
}
