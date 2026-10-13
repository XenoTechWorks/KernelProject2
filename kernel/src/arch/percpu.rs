//! What each CPU has of its own: its GDT and TSS, the stack system calls switch to, which task it's
//! running, and its place in the scheduler. In the kernel the GS segment base points at this CPU's
//! record (`swapgs` on the way in from a program swaps it with the program's own GS base, and back
//! on the way out), so the system call entry finds its kernel stack at gs:[8].

use super::gdt::Tss;
use core::arch::asm;

pub const MAX_CPUS: usize = 16;

/// The first fields are used from assembly (idt.rs): keep their offsets.
#[repr(C)]
pub struct PerCpu {
    /// 0: this record's own address (gs:[0] gives it back).
    this: *mut PerCpu,
    /// 8: the top of the running task's kernel stack (syscall_entry switches to it).
    pub kernel_stack_top: u64,
    /// 16: the program's stack pointer while syscall_entry saves it.
    pub user_rsp: u64,
    /// 24: which CPU (0 is the one that booted).
    pub id: u64,
    /// 32: how deep in the kernel this CPU is (the big kernel lock: see smp.rs).
    pub depth: u64,
    /// The task running here (a thread id), if any.
    pub current: Option<u32>,
    /// Where the scheduler loop's stack was when it switched to a task.
    pub sched_rsp: u64,
    /// The last task this CPU ran (round robin starts after it).
    pub last: u32,
    pub tss: Tss,
    pub gdt: [u64; 7],
}

const EMPTY: PerCpu = PerCpu {
    this: core::ptr::null_mut(),
    kernel_stack_top: 0,
    user_rsp: 0,
    id: 0,
    depth: 0,
    current: None,
    sched_rsp: 0,
    last: 0,
    tss: Tss::EMPTY,
    gdt: [0; 7],
};

static mut CPUS: [PerCpu; MAX_CPUS] = [EMPTY; MAX_CPUS];

/// Points this CPU's GS base at record `id` (and the program's GS base, held while in the kernel,
/// at 0).
pub fn init(id: usize) {
    unsafe {
        let me = &raw mut CPUS[id];
        (*me).this = me;
        (*me).id = id as u64;
        super::cpu::wrmsr(super::cpu::MSR_GS_BASE, me as u64);
        super::cpu::wrmsr(super::cpu::MSR_KERNEL_GS_BASE, 0);
    }
}

/// This CPU's record (valid in the kernel, once init has run on this CPU).
pub fn this() -> &'static mut PerCpu {
    unsafe {
        let me: *mut PerCpu;
        asm!("mov {}, gs:[0]", out(reg) me, options(nostack, preserves_flags, readonly));
        &mut *me
    }
}

/// Another CPU's record.
pub fn get(id: usize) -> &'static mut PerCpu {
    unsafe { &mut *(&raw mut CPUS[id]) }
}
