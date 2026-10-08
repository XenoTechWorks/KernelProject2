//! Futexes: "wait while this memory word holds this value" and "wake N waiters on this word". The C
//! library builds mutexes, condition variables and pthread_join on them.

use super::{Process, all, current, sleep};
use crate::mm::user;
use crate::sys::errno::*;
use alloc::sync::Arc;

/// A futex is a word in a particular memory: (that memory's identity, address).
pub type Key = (usize, u64);

pub fn key(p: &Process, addr: u64) -> Key {
    (Arc::as_ptr(&p.mem) as usize, addr)
}

fn read_word(addr: u64) -> Result<u32, Errno> {
    if addr % 4 != 0 {
        return Err(EINVAL);
    }
    user::read_struct(current().space(), addr)
}

/// Sleeps while *addr == expected, until woken, until `deadline` (in timer ticks), or a signal.
pub fn wait(addr: u64, expected: u32, deadline: Option<u64>) -> Result<u64, Errno> {
    if read_word(addr)? != expected {
        return Err(EAGAIN);
    }
    let p = current();
    p.futex = Some(key(p, addr));
    loop {
        let p = current();
        if p.futex.is_none() {
            return Ok(0);
        }
        if deadline.is_some_and(|d| crate::arch::irq::ticks() >= d) {
            p.futex = None;
            return Err(ETIMEDOUT);
        }
        if let Err(e) = sleep() {
            current().futex = None;
            return Err(e);
        }
    }
}

/// Wakes up to `count` tasks waiting on `key`. Returns how many.
pub fn wake(key: Key, count: u32) -> u64 {
    let mut woken = 0;
    for p in all() {
        if woken >= count as u64 {
            break;
        }
        if p.futex == Some(key) {
            p.futex = None;
            if p.state == super::State::Sleeping {
                p.state = super::State::Runnable;
            }
            woken += 1;
        }
    }
    woken
}

/// Wakes `count` waiters on `from` and moves up to `requeue` more to wait on `to` instead.
pub fn requeue(from: Key, to: Key, count: u32, requeue: u32) -> u64 {
    let woken = wake(from, count);
    let mut moved = 0;
    for p in all() {
        if moved >= requeue as u64 {
            break;
        }
        if p.futex == Some(from) {
            p.futex = Some(to);
            moved += 1;
        }
    }
    woken + moved
}

/// The futex system call.
pub fn futex(addr: u64, op: u32, val: u32, timeout: u64, addr2: u64, val3: u32) -> Result<u64, Errno> {
    const WAIT: u32 = 0;
    const WAKE: u32 = 1;
    const REQUEUE: u32 = 3;
    const CMP_REQUEUE: u32 = 4;
    const WAIT_BITSET: u32 = 9;
    const WAKE_BITSET: u32 = 10;
    const CLOCK_REALTIME: u32 = 256;
    let p = current();
    match op & 0x7f {
        WAIT | WAIT_BITSET => {
            let deadline = if timeout == 0 {
                None
            } else {
                let [secs, nanos]: [u64; 2] = user::read_struct(p.space(), timeout)?;
                let ticks = secs * 100 + nanos.div_ceil(10_000_000);
                let now = crate::arch::irq::ticks();
                Some(if op & 0x7f == WAIT {
                    now + ticks // relative
                } else {
                    // absolute, on CLOCK_MONOTONIC or (with the flag) CLOCK_REALTIME
                    let clock = if op & CLOCK_REALTIME != 0 { crate::time::realtime() } else { crate::time::monotonic() };
                    let now_ticks = clock.0 * 100 + clock.1 / 10_000_000;
                    now + ticks.saturating_sub(now_ticks)
                })
            };
            wait(addr, val, deadline)
        }
        WAKE | WAKE_BITSET => Ok(wake(key(p, addr), val)),
        REQUEUE | CMP_REQUEUE => {
            if op & 0x7f == CMP_REQUEUE && read_word(addr)? != val3 {
                return Err(EAGAIN);
            }
            Ok(requeue(key(p, addr), key(p, addr2), val, timeout as u32))
        }
        _ => Err(ENOSYS),
    }
}
