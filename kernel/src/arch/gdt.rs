//! Segments and the TSS. In 64-bit mode segments only pick the privilege level (kernel or user), and the
//! TSS only says which stack to use when an interrupt arrives from user mode.

use core::arch::asm;
use core::mem::size_of;

pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
// sysret needs user data right before user code, in this order.
pub const USER_DATA: u16 = 0x18 | 3;
pub const USER_CODE: u16 = 0x20 | 3;
const TSS_SELECTOR: u16 = 0x28;

#[repr(C, packed)]
pub struct Tss {
    _reserved0: u32,
    pub rsp0: u64,
    _rsp1: u64,
    _rsp2: u64,
    _reserved1: u64,
    pub ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    iomap_base: u16,
}

pub static mut TSS: Tss = Tss {
    _reserved0: 0,
    rsp0: 0,
    _rsp1: 0,
    _rsp2: 0,
    _reserved1: 0,
    ist: [0; 7],
    _reserved2: 0,
    _reserved3: 0,
    iomap_base: size_of::<Tss>() as u16,
};

static mut GDT: [u64; 7] = [
    0,
    0x00af_9a00_0000_ffff, // kernel code
    0x00cf_9200_0000_ffff, // kernel data
    0x00cf_f200_0000_ffff, // user data
    0x00af_fa00_0000_ffff, // user code
    0,                     // TSS (low), filled in by init
    0,                     // TSS (high)
];

/// A separate stack for double faults, so a kernel stack overflow still prints something.
static mut DOUBLE_FAULT_STACK: [u8; 16384] = [0; 16384];

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

pub fn init() {
    unsafe {
        let tss = &raw const TSS as u64;
        let limit = (size_of::<Tss>() - 1) as u64;
        GDT[5] = (limit & 0xffff) | ((tss & 0xff_ffff) << 16) | (0x89 << 40) | ((tss >> 24 & 0xff) << 56);
        GDT[6] = tss >> 32;
        TSS.ist[0] = (&raw const DOUBLE_FAULT_STACK as u64) + 16384;

        let pointer = Pointer { limit: (size_of::<[u64; 7]>() - 1) as u16, base: &raw const GDT as u64 };
        asm!(
            "lgdt [{ptr}]",
            "mov ds, {data:x}", "mov es, {data:x}", "mov ss, {data:x}",
            "xor {tmp:e}, {tmp:e}", "mov fs, {tmp:x}", "mov gs, {tmp:x}",
            // Reload CS with a far return.
            "push {code}", "lea {tmp}, [rip + 2f]", "push {tmp}", "retfq",
            "2:",
            "ltr {tss:x}",
            ptr = in(reg) &pointer,
            data = in(reg) KERNEL_DATA as u64,
            code = in(reg) KERNEL_CODE as u64,
            tss = in(reg) TSS_SELECTOR as u64,
            tmp = out(reg) _,
        );
    }
}

/// The stack the CPU switches to when an interrupt or exception arrives from user mode.
pub fn set_kernel_stack(top: u64) {
    unsafe { TSS.rsp0 = top }
}
