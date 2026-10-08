//! What the processor says about itself (the CPUID instruction), for /proc/cpuinfo.

use alloc::string::String;
use alloc::vec::Vec;
use core::arch::x86_64::{__cpuid, __cpuid_count, _rdtsc};
use core::sync::atomic::{AtomicU64, Ordering};

pub struct CpuInfo {
    pub vendor: String,
    pub brand: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub flags: Vec<&'static str>,
}

fn bytes_of(regs: &[u32]) -> String {
    let bytes: Vec<u8> = regs.iter().flat_map(|r| r.to_le_bytes()).filter(|&b| b != 0).collect();
    String::from_utf8_lossy(&bytes).trim().into()
}

// Linux's names for the feature bits, in the order /proc/cpuinfo lists them (a common subset).
const EDX1: [(u32, &str); 21] = [
    (0, "fpu"), (1, "vme"), (2, "de"), (3, "pse"), (4, "tsc"), (5, "msr"), (6, "pae"), (7, "mce"),
    (8, "cx8"), (9, "apic"), (11, "sep"), (12, "mtrr"), (13, "pge"), (14, "mca"), (15, "cmov"),
    (16, "pat"), (19, "clflush"), (23, "mmx"), (24, "fxsr"), (25, "sse"), (26, "sse2"),
];
const ECX1: [(u32, &str); 14] = [
    (0, "pni"), (1, "pclmulqdq"), (9, "ssse3"), (12, "fma"), (13, "cx16"), (19, "sse4_1"),
    (20, "sse4_2"), (21, "x2apic"), (22, "movbe"), (23, "popcnt"), (25, "aes"), (26, "xsave"),
    (28, "avx"), (30, "rdrand"),
];
const EBX7: [(u32, &str); 6] = [(3, "bmi1"), (5, "avx2"), (8, "bmi2"), (16, "avx512f"), (18, "rdseed"), (29, "sha_ni")];

pub fn info() -> CpuInfo {
    let leaf0 = __cpuid(0);
    let vendor = bytes_of(&[leaf0.ebx, leaf0.edx, leaf0.ecx]);
    let leaf1 = __cpuid(1);
    let base_family = (leaf1.eax >> 8) & 0xf;
    let family = if base_family == 0xf { base_family + ((leaf1.eax >> 20) & 0xff) } else { base_family };
    let mut model = (leaf1.eax >> 4) & 0xf;
    if base_family == 6 || base_family == 0xf {
        model |= ((leaf1.eax >> 16) & 0xf) << 4;
    }
    let brand = if __cpuid(0x8000_0000).eax >= 0x8000_0004 {
        let parts: Vec<u32> = (0x8000_0002..=0x8000_0004u32)
            .flat_map(|leaf| {
                let r = __cpuid(leaf);
                [r.eax, r.ebx, r.ecx, r.edx]
            })
            .collect();
        bytes_of(&parts)
    } else {
        vendor.clone()
    };
    let mut flags = Vec::new();
    flags.extend(EDX1.iter().filter(|(bit, _)| leaf1.edx >> bit & 1 == 1).map(|(_, n)| *n));
    flags.extend(ECX1.iter().filter(|(bit, _)| leaf1.ecx >> bit & 1 == 1).map(|(_, n)| *n));
    if leaf0.eax >= 7 {
        let leaf7 = __cpuid_count(7, 0);
        flags.extend(EBX7.iter().filter(|(bit, _)| leaf7.ebx >> bit & 1 == 1).map(|(_, n)| *n));
    }
    CpuInfo { vendor, brand, family, model, stepping: leaf1.eax & 0xf, flags }
}

static BOOT_TSC: AtomicU64 = AtomicU64::new(0);

pub fn mark_boot() {
    BOOT_TSC.store(unsafe { _rdtsc() }, Ordering::Relaxed);
}

/// The time-stamp counter's rate, measured against the timer since boot. 0 until enough time passed.
pub fn tsc_mhz() -> u64 {
    let ticks = crate::arch::irq::ticks();
    if ticks < 10 {
        return 0;
    }
    let cycles = unsafe { _rdtsc() } - BOOT_TSC.load(Ordering::Relaxed);
    cycles / (ticks * (1_000_000 / crate::arch::irq::TIMER_HZ))
}
