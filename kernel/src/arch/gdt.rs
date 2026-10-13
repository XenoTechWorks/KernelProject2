//! Segments and the TSS. In 64-bit mode segments only pick the privilege level (kernel or user), and the
//! TSS only says which stack to use when an interrupt arrives from user mode. Each CPU has its own
//! GDT and TSS (in its percpu record): the TSS holds that CPU's kernel stack.

use super::percpu;
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

impl Tss {
    pub const EMPTY: Tss = Tss {
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
}

const SEGMENTS: [u64; 7] = [
    0,
    0x00af_9a00_0000_ffff, // kernel code
    0x00cf_9200_0000_ffff, // kernel data
    0x00cf_f200_0000_ffff, // user data
    0x00af_fa00_0000_ffff, // user code
    0,                     // TSS (low), filled in by init
    0,                     // TSS (high)
];

/// A separate stack for double faults on each CPU, so a kernel stack overflow still prints something.
static mut DOUBLE_FAULT_STACKS: [[u8; 16384]; percpu::MAX_CPUS] = [[0; 16384]; percpu::MAX_CPUS];

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

/// Loads CPU `id`'s GDT and TSS.
pub fn init(id: usize) {
    let me = percpu::get(id);
    unsafe {
        me.tss = Tss::EMPTY;
        me.gdt = SEGMENTS;
        let tss = &raw const me.tss as u64;
        let limit = (size_of::<Tss>() - 1) as u64;
        me.gdt[5] = (limit & 0xffff) | ((tss & 0xff_ffff) << 16) | (0x89 << 40) | ((tss >> 24 & 0xff) << 56);
        me.gdt[6] = tss >> 32;
        let mut ist = [0u64; 7]; // (a packed field: set as a whole)
        ist[0] = (&raw const DOUBLE_FAULT_STACKS[id] as u64) + 16384;
        me.tss.ist = ist;

        let pointer = Pointer { limit: (size_of::<[u64; 7]>() - 1) as u16, base: &raw const me.gdt as u64 };
        asm!(
            "lgdt [{ptr}]",
            "mov ds, {data:x}", "mov es, {data:x}", "mov ss, {data:x}",
            // fs and gs: their selectors are 0 (their bases come from MSRs, set separately).
            "xor {tmp:e}, {tmp:e}", "mov fs, {tmp:x}",
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

/// The stack this CPU switches to when an interrupt arrives from user mode (the TSS), and that
/// system calls switch to (syscall_entry, through the percpu record).
pub fn set_kernel_stack(top: u64) {
    let me = percpu::this();
    me.tss.rsp0 = top;
    me.kernel_stack_top = top;
}
