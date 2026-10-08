//! Everything else: time, uname, random numbers, limits, system information.

use super::SysResult;
use super::errno::*;
use crate::mm::user;
use crate::proc;
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

/// getitimer / setitimer: reports "no timer set" in the old-value struct.
pub fn itimer(old: u64) -> SysResult {
    if old != 0 {
        user::write(proc::current().space(), old, &[0u8; 32])?;
    }
    Ok(0)
}

pub fn sched_getaffinity(size: usize, mask: u64) -> SysResult {
    if size < 8 {
        return Err(EINVAL);
    }
    user::write_struct(proc::current().space(), mask, &1u64)?;
    Ok(8)
}
