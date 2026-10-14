//! Everything else: time, uname, random numbers, limits, system information.

use super::SysResult;
use super::errno::*;
use crate::mm::user;
use crate::proc::{self, timer};
use crate::time;
use core::sync::atomic::{AtomicU64, Ordering};

pub fn uname(out: u64) -> SysResult {
    // "Linux" as the system name, like WSL1 did: programs and scripts check it to pick Linux behaviour.
    let fields = ["Linux", "kernelproject2", "6.12.0-kp2", "#1 KernelProject2 0.1", "x86_64", "(none)"];
    let mut buf = [0u8; 65 * 6];
    for (i, f) in fields.iter().enumerate() {
        buf[i * 65..i * 65 + f.len()].copy_from_slice(f.as_bytes());
    }
    user::write(proc::current().space(), out, &buf)?;
    Ok(0)
}

fn write_time(ptr: u64, (secs, nanos): (u64, u64), micro: bool) -> SysResult {
    if ptr != 0 {
        let pair = [secs, if micro { nanos / 1000 } else { nanos }];
        user::write_struct(proc::current().space(), ptr, &pair)?;
    }
    Ok(0)
}

pub fn clock_gettime(clock: u32, ptr: u64) -> SysResult {
    let t = match clock {
        0 | 5 | 8 => time::realtime(), // REALTIME, REALTIME_COARSE, TAI
        1 | 4 | 6 | 7 => time::monotonic(), // MONOTONIC, _RAW, _COARSE, BOOTTIME
        2 | 3 => {
            // process / thread CPU time: time since the process started is close enough for now
            let ticks = crate::arch::irq::ticks() - proc::current().start_ticks;
            (ticks / 100, (ticks % 100) * 10_000_000)
        }
        _ => return Err(EINVAL),
    };
    write_time(ptr, t, false)
}

pub fn clock_getres(ptr: u64) -> SysResult {
    write_time(ptr, (0, 10_000_000), false)
}

pub fn gettimeofday(ptr: u64) -> SysResult {
    write_time(ptr, time::realtime(), true)
}

pub fn time(ptr: u64) -> SysResult {
    let now = time::realtime().0;
    if ptr != 0 {
        user::write_struct(proc::current().space(), ptr, &now)?;
    }
    Ok(now)
}

fn sleep_ticks(ticks: u64) -> SysResult {
    let deadline = crate::arch::irq::ticks() + ticks;
    while crate::arch::irq::ticks() < deadline {
        proc::sleep()?;
    }
    Ok(0)
}

fn read_timespec(ptr: u64) -> Result<u64, Errno> {
    let [secs, nanos]: [u64; 2] = user::read_struct(proc::current().space(), ptr)?;
    if nanos >= 1_000_000_000 {
        return Err(EINVAL);
    }
    Ok(secs * 100 + nanos.div_ceil(10_000_000))
}

pub fn nanosleep(req: u64) -> SysResult {
    sleep_ticks(read_timespec(req)?)
}

pub fn clock_nanosleep(req: u64) -> SysResult {
    sleep_ticks(read_timespec(req)?)
}

static RANDOM_STATE: AtomicU64 = AtomicU64::new(0x2545_f491_4f6c_dd1d);

/// Not cryptographic yet: xorshift mixed with the CPU's cycle counter.
pub fn fill_random(buf: &mut [u8]) {
    let mut x = RANDOM_STATE.load(Ordering::Relaxed) ^ unsafe { core::arch::x86_64::_rdtsc() };
    for chunk in buf.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let v = x.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
    RANDOM_STATE.store(x, Ordering::Relaxed);
}

pub fn getrandom(buf: u64, len: usize) -> SysResult {
    let len = len.min(1 << 16);
    let mut tmp = alloc::vec![0u8; len];
    fill_random(&mut tmp);
    user::write(proc::current().space(), buf, &tmp)?;
    Ok(len as u64)
}

pub fn sysinfo(out: u64) -> SysResult {
    let (free, total) = crate::mm::frame::stats();
    let mut buf = [0u8; 112];
    let put = |b: &mut [u8; 112], at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put(&mut buf, 0, time::monotonic().0);
    put(&mut buf, 32, total * 4096);
    put(&mut buf, 40, free * 4096);
    buf[80..82].copy_from_slice(&(proc::all().count() as u16).to_le_bytes());
    buf[104..108].copy_from_slice(&1u32.to_le_bytes()); // mem_unit
    user::write(proc::current().space(), out, &buf)?;
    Ok(0)
}

fn rlimit(resource: u32) -> [u64; 2] {
    const INFINITY: u64 = u64::MAX;
    match resource {
        3 => [8 << 20, INFINITY], // RLIMIT_STACK
        7 => [1024, 1024],        // RLIMIT_NOFILE
        _ => [INFINITY, INFINITY],
    }
}

pub fn getrlimit(resource: u32, out: u64) -> SysResult {
    user::write_struct(proc::current().space(), out, &rlimit(resource))?;
    Ok(0)
}

pub fn prlimit64(resource: u32, old: u64) -> SysResult {
    if old != 0 {
        user::write_struct(proc::current().space(), old, &rlimit(resource))?;
    }
    Ok(0)
}

pub fn getrusage(out: u64) -> SysResult {
    user::write(proc::current().space(), out, &[0u8; 144])?;
    Ok(0)
}

pub fn times(out: u64) -> SysResult {
    if out != 0 {
        user::write(proc::current().space(), out, &[0u8; 32])?;
    }
    Ok(crate::arch::irq::ticks())
}

const ITIMER_REAL: u32 = 0;

/// `struct itimerval`: the interval, then the time left, each as seconds and microseconds.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct ItimerVal {
    every_secs: u64,
    every_micros: u64,
    left_secs: u64,
    left_micros: u64,
}

fn itimerval(left: u64, every: u64) -> ItimerVal {
    let (left, every) = (timer::micros_for(left), timer::micros_for(every));
    ItimerVal { every_secs: every / 1_000_000, every_micros: every % 1_000_000, left_secs: left / 1_000_000, left_micros: left % 1_000_000 }
}

/// getitimer. Only the real-time timer counts; the CPU-time ones (virtual, profiling) are
/// accepted but never go off, so they always read as not set.
pub fn getitimer(which: u32, out: u64) -> SysResult {
    let (left, every) = if which == ITIMER_REAL { timer::get(proc::current().tgid) } else { (0, 0) };
    user::write_struct(proc::current().space(), out, &itimerval(left, every))?;
    Ok(0)
}

pub fn setitimer(which: u32, new: u64, old: u64) -> SysResult {
    let p = proc::current();
    let want: ItimerVal = if new != 0 { user::read_struct(p.space(), new)? } else { ItimerVal::default() };
    if want.every_micros >= 1_000_000 || want.left_micros >= 1_000_000 {
        return Err(EINVAL);
    }
    let (left, every) = if which == ITIMER_REAL {
        let micros = |secs: u64, us: u64| secs.saturating_mul(1_000_000).saturating_add(us);
        let after = timer::ticks_for(micros(want.left_secs, want.left_micros));
        timer::set(p.tgid, after, timer::ticks_for(micros(want.every_secs, want.every_micros)))
    } else {
        (0, 0)
    };
    if old != 0 {
        user::write_struct(p.space(), old, &itimerval(left, every))?;
    }
    Ok(0)
}

/// alarm(): SIGALRM in so many seconds (0 cancels); returns the seconds the last one had left.
pub fn alarm(secs: u64) -> SysResult {
    let (left, _) = timer::set(proc::current().tgid, timer::ticks_for(secs.saturating_mul(1_000_000)), 0);
    Ok(timer::micros_for(left).div_ceil(1_000_000))
}

/// setpriority: accepted, and does nothing (every task has the same priority; `nice` values are
/// an illusion Mesa and others check for without needing to matter).
pub fn setpriority() -> SysResult {
    Ok(0)
}

/// sched_setscheduler, sched_setaffinity: accepted and ignored. There's one scheduling policy,
/// and a task may run on any CPU, which is what asking for "any of them" would get anyway.
pub fn sched_setscheduler() -> SysResult {
    Ok(0)
}

pub fn sched_setaffinity(size: usize, mask: u64) -> SysResult {
    // Only refuse a mask that excludes every CPU there is: that's a logic error worth surfacing,
    // not pretending to grant.
    if size < 8 {
        return Err(EINVAL);
    }
    let requested: u64 = user::read_struct(proc::current().space(), mask)?;
    let n = crate::arch::smp::count().min(64);
    let all: u64 = if n == 64 { u64::MAX } else { (1 << n) - 1 };
    if requested & all == 0 {
        return Err(EINVAL);
    }
    Ok(0)
}

/// sched_getparam: writes a priority of 0 (every task's, since there's only one scheduling
/// policy here) into `struct sched_param { int sched_priority; }`.
pub fn sched_getparam(param: u64) -> SysResult {
    let zero: i32 = 0;
    user::write_struct(proc::current().space(), param, &zero)?;
    Ok(0)
}

/// sched_getscheduler: always SCHED_OTHER (0), the one policy there is.
pub fn sched_getscheduler() -> SysResult {
    Ok(0)
}

pub fn sched_getaffinity(size: usize, mask: u64) -> SysResult {
    if size < 8 {
        return Err(EINVAL);
    }
    // Every CPU: a task may run on any of them.
    let n = crate::arch::smp::count().min(64);
    let all: u64 = if n == 64 { u64::MAX } else { (1 << n) - 1 };
    user::write_struct(proc::current().space(), mask, &all)?;
    Ok(8)
}
