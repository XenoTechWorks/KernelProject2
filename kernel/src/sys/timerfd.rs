//! timerfd: a timer read like a file, so an event loop can wait for it with everything else.
//! Reading gives how many times it has gone off since the last read (a u64), waiting until it
//! has at least once. Counted against the timer tick (10 ms), like every timer here.

use super::SysResult;
use super::errno::*;
use crate::arch::irq::{TIMER_HZ, ticks};
use crate::mm::user;
use crate::proc::{self, fd::Handle, fd::OpenFile};
use alloc::string::String;
use alloc::sync::Arc;
use spin::Mutex;

const CLOCK_REALTIME: u32 = 0;
const CLOCK_MONOTONIC: u32 = 1;
const CLOCK_BOOTTIME: u32 = 7;
const TFD_NONBLOCK: u32 = 0o4000;
const TFD_CLOEXEC: u32 = 0o2000000;
const TFD_TIMER_ABSTIME: u32 = 1;

const NANOS: u64 = 1_000_000_000;

pub struct TimerFd {
    clock: u32,
    timer: Mutex<Timer>,
}

#[derive(Default)]
struct Timer {
    /// When it next goes off, in nanoseconds since boot; None when it's off.
    next: Option<u64>,
    /// How often after that; 0 for once.
    every: u64,
}

/// `struct itimerspec`: the interval, then the time to the next expiry, each as seconds and
/// nanoseconds.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct ItimerSpec {
    every_secs: u64,
    every_nanos: u64,
    next_secs: u64,
    next_nanos: u64,
}

fn now() -> u64 {
    ticks() * (NANOS / TIMER_HZ)
}

/// Takes the expirations so far: how many, moving the timer on past them.
fn expire(t: &mut Timer) -> u64 {
    let Some(next) = t.next else { return 0 };
    let now = now();
    if now < next {
        return 0;
    }
    if t.every == 0 {
        t.next = None;
        return 1;
    }
    let count = 1 + (now - next) / t.every;
    t.next = Some(next + count * t.every);
    count
}

pub fn create(clock: u32, flags: u32) -> SysResult {
    if !matches!(clock, CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME) {
        return Err(EINVAL);
    }
    if flags & !(TFD_NONBLOCK | TFD_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    let file = OpenFile::new(
        Handle::TimerFd(Arc::new(TimerFd { clock, timer: Mutex::new(Timer::default()) })),
        2 | flags & TFD_NONBLOCK,
        String::from("anon_inode:[timerfd]"),
    );
    Ok(proc::current().files().fds.insert(file, flags & TFD_CLOEXEC != 0, 0)? as u64)
}

fn timer_of(fd: i32) -> Result<Arc<TimerFd>, Errno> {
    match &proc::current().files().fds.get(fd)?.handle {
        Handle::TimerFd(t) => Ok(t.clone()),
        _ => Err(EINVAL),
    }
}

/// The timer as itimerspec: the interval, and the time left (both 0 when it's off).
fn current(t: &Timer) -> ItimerSpec {
    let left = t.next.map_or(0, |next| next.saturating_sub(now()).max(1));
    ItimerSpec { every_secs: t.every / NANOS, every_nanos: t.every % NANOS, next_secs: left / NANOS, next_nanos: left % NANOS }
}

pub fn settime(fd: i32, flags: u32, new_at: u64, old_at: u64) -> SysResult {
    let tfd = timer_of(fd)?;
    let p = proc::current();
    let want: ItimerSpec = user::read_struct(p.space(), new_at)?;
    if want.every_nanos >= NANOS || want.next_nanos >= NANOS {
        return Err(EINVAL);
    }
    let mut t = tfd.timer.lock();
    if old_at != 0 {
        user::write_struct(p.space(), old_at, &current(&t))?;
    }
    let value = want.next_secs.saturating_mul(NANOS).saturating_add(want.next_nanos);
    t.every = want.every_secs.saturating_mul(NANOS).saturating_add(want.every_nanos);
    t.next = match value {
        0 => None,
        // An absolute time on the timer's clock; the real-time clock is boot time plus the date.
        v if flags & TFD_TIMER_ABSTIME != 0 && tfd.clock == CLOCK_REALTIME => {
            let (secs, nanos) = crate::time::realtime();
            let boot = (secs * NANOS + nanos).saturating_sub(now());
            Some(v.saturating_sub(boot))
        }
        v if flags & TFD_TIMER_ABSTIME != 0 => Some(v),
        v => Some(now() + v),
    };
    drop(t);
    proc::wake_all();
    Ok(0)
}

pub fn gettime(fd: i32, out: u64) -> SysResult {
    let tfd = timer_of(fd)?;
    let spec = current(&tfd.timer.lock());
    user::write_struct(proc::current().space(), out, &spec)?;
    Ok(0)
}

pub fn read(tfd: &TimerFd, buf: &mut [u8], nonblocking: bool) -> Result<usize, Errno> {
    if buf.len() < 8 {
        return Err(EINVAL);
    }
    loop {
        let count = expire(&mut tfd.timer.lock());
        if count > 0 {
            buf[..8].copy_from_slice(&count.to_le_bytes());
            return Ok(8);
        }
        if nonblocking {
            return Err(EAGAIN);
        }
        proc::sleep()?;
    }
}

/// Has it gone off since the last read?
pub fn readable(tfd: &TimerFd) -> bool {
    tfd.timer.lock().next.is_some_and(|next| now() >= next)
}
