//! eventfd: a 64-bit counter behind a descriptor, for one part of a program to wake another.
//! Writing adds to it; reading takes the whole count (or 1, as a semaphore) and waits while it's 0.

use crate::sys::errno::*;
use alloc::sync::Arc;
use spin::Mutex;

pub struct EventFd {
    count: Mutex<u64>,
    semaphore: bool,
}

/// The most the counter holds; a write that would pass it waits.
const MAX: u64 = u64::MAX - 1;

pub fn new(initial: u64, semaphore: bool) -> Arc<EventFd> {
    Arc::new(EventFd { count: Mutex::new(initial), semaphore })
}

pub fn read(e: &EventFd, nonblocking: bool) -> Result<u64, Errno> {
    loop {
        {
            let mut count = e.count.lock();
            if *count > 0 {
                let taken = if e.semaphore { 1 } else { *count };
                *count -= taken;
                drop(count);
                crate::proc::wake_all(); // writers waiting for room
                return Ok(taken);
            }
        }
        if nonblocking {
            return Err(EAGAIN);
        }
        crate::proc::sleep()?;
    }
}

pub fn write(e: &EventFd, add: u64, nonblocking: bool) -> Result<(), Errno> {
    if add == u64::MAX {
        return Err(EINVAL);
    }
    loop {
        {
            let mut count = e.count.lock();
            if *count <= MAX - add {
                *count += add;
                drop(count);
                crate::proc::wake_all();
                return Ok(());
            }
        }
        if nonblocking {
            return Err(EAGAIN);
        }
        crate::proc::sleep()?;
    }
}

/// (something to read, room to write)
pub fn readiness(e: &EventFd) -> (bool, bool) {
    let count = *e.count.lock();
    (count > 0, count < MAX)
}
