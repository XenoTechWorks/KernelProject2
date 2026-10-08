//! Time: the date from the CMOS real-time clock at boot, plus timer ticks since then.

use crate::arch::cpu::{inb, outb};
use crate::arch::irq::{TIMER_HZ, ticks};
use core::sync::atomic::{AtomicU64, Ordering};

static BOOT_EPOCH: AtomicU64 = AtomicU64::new(0);

fn cmos(register: u8) -> u8 {
    unsafe {
        outb(0x70, register);
        inb(0x71)
    }
}

pub fn init() {
    // Wait until the clock isn't mid-update, then read it.
    while cmos(0x0A) & 0x80 != 0 {}
    let status_b = cmos(0x0B);
    let bcd = |v: u8| if status_b & 0x04 == 0 { (v & 0x0f) + (v >> 4) * 10 } else { v };
    let second = bcd(cmos(0x00)) as u64;
    let minute = bcd(cmos(0x02)) as u64;
    let hour_raw = cmos(0x04);
    let mut hour = bcd(hour_raw & 0x7f) as u64;
    if status_b & 0x02 == 0 && hour_raw & 0x80 != 0 {
        hour = (hour + 12) % 24;
    }
    let day = bcd(cmos(0x07)) as u64;
    let month = bcd(cmos(0x08)) as u64;
    let year = 2000 + bcd(cmos(0x09)) as u64;
    BOOT_EPOCH.store(days_since_epoch(year, month, day) * 86400 + hour * 3600 + minute * 60 + second, Ordering::Relaxed);
}

/// Days from 1970-01-01 to the given date (proleptic Gregorian calendar).
fn days_since_epoch(year: u64, month: u64, day: u64) -> u64 {
    let (y, m) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// (seconds, nanoseconds) since boot.
pub fn monotonic() -> (u64, u64) {
    let t = ticks();
    (t / TIMER_HZ, (t % TIMER_HZ) * (1_000_000_000 / TIMER_HZ))
}

/// (seconds, nanoseconds) since 1970.
pub fn realtime() -> (u64, u64) {
    let (s, ns) = monotonic();
    (BOOT_EPOCH.load(Ordering::Relaxed) + s, ns)
}
