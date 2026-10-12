//! alarm() and setitimer(ITIMER_REAL): a process asks for SIGALRM after a while, once or again
//! every so often. Counted in timer ticks (10 ms). Like Linux, a process keeps its timer across
//! exec, and a forked child starts without one.

use crate::arch::irq::{TIMER_HZ, ticks};
use alloc::collections::BTreeMap;
use spin::Mutex;

#[derive(Clone, Copy)]
struct Timer {
    /// The tick it goes off at.
    at: u64,
    /// Ticks until the next time after that; 0 for once.
    every: u64,
}

/// By process (thread group) id.
static TIMERS: Mutex<BTreeMap<u32, Timer>> = Mutex::new(BTreeMap::new());

const MICROS_PER_TICK: u64 = 1_000_000 / TIMER_HZ;

/// Microseconds to ticks, rounding up: a timer never goes off early.
pub fn ticks_for(micros: u64) -> u64 {
    micros.div_ceil(MICROS_PER_TICK)
}

pub fn micros_for(ticks: u64) -> u64 {
    ticks * MICROS_PER_TICK
}

/// Sets the timer (`after` 0 turns it off) and returns the old one as (ticks left, interval).
pub fn set(tgid: u32, after: u64, every: u64) -> (u64, u64) {
    let mut timers = TIMERS.lock();
    let old = get_locked(&timers, tgid);
    if after == 0 {
        timers.remove(&tgid);
    } else {
        timers.insert(tgid, Timer { at: ticks() + after, every });
    }
    old
}

/// (ticks left, interval); (0, 0) when no timer is set.
pub fn get(tgid: u32) -> (u64, u64) {
    get_locked(&TIMERS.lock(), tgid)
}

fn get_locked(timers: &BTreeMap<u32, Timer>, tgid: u32) -> (u64, u64) {
    timers.get(&tgid).map_or((0, 0), |t| (t.at.saturating_sub(ticks()).max(1), t.every))
}

/// The process is gone: so is its timer.
pub fn remove(tgid: u32) {
    TIMERS.lock().remove(&tgid);
}

/// Each timer tick: send SIGALRM to every process whose time has come.
pub fn tick() {
    let now = ticks();
    let mut due = alloc::vec::Vec::new();
    {
        let mut timers = TIMERS.lock();
        timers.retain(|&tgid, t| {
            if t.at > now {
                return true;
            }
            due.push(tgid);
            t.at = now + t.every;
            t.every > 0
        });
    }
    for tgid in due {
        let _ = super::signal_process(tgid, super::signal::SIGALRM);
    }
}
