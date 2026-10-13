//! More than one CPU.
//!
//! Programs run on every CPU at once; the kernel itself runs on one at a time, under one lock (the
//! "big kernel lock", as Linux had at first). A CPU takes it on the way into the kernel (a system
//! call, an interrupt, an exception) and lets go on the way back out to a program, or when it has
//! nothing to run and waits for an interrupt. So all the kernel's code, written for one CPU, stays
//! right. The lock belongs to the CPU, not the task: a task that sleeps in the kernel and wakes on
//! another CPU just carries on there.
//!
//! The other CPUs ("application processors") are started through the bootloader (Limine's MP
//! request). Each gets its own GDT and TSS, the shared IDT, and its local APIC's timer at 100 Hz to
//! take turns between programs; device interrupts all go to the first CPU.

use super::{cpu, gdt, idt, percpu};
use alloc::boxed::Box;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use limine::mp::MpInfo;

/// The big kernel lock: a ticket lock (fair: CPUs get it in the order they asked).
static NEXT_TICKET: AtomicU32 = AtomicU32::new(0);
static SERVING: AtomicU32 = AtomicU32::new(0);

/// How many CPUs are running (1 until the others start).
static ONLINE: AtomicUsize = AtomicUsize::new(1);

pub fn count() -> usize {
    ONLINE.load(Ordering::Acquire)
}

/// On the way into the kernel: take the lock, unless this CPU is already in the kernel.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_enter() {
    let c = percpu::this();
    if c.depth == 0 {
        let ticket = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
        while SERVING.load(Ordering::Acquire) != ticket {
            core::hint::spin_loop();
        }
    }
    c.depth += 1;
}

/// On the way out: let go when leaving the kernel altogether.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_exit() {
    let c = percpu::this();
    c.depth -= 1;
    if c.depth == 0 {
        SERVING.fetch_add(1, Ordering::Release);
    }
}

/// Nothing to run: let others into the kernel while this CPU waits for an interrupt.
pub fn idle() {
    kernel_exit();
    cpu::wait_for_interrupt();
    kernel_enter();
}

// ---- the local APIC (each CPU's own interrupt controller) ----

pub const TIMER_VECTOR: u64 = 48;
static APIC_MMIO: AtomicU64 = AtomicU64::new(0);

fn x2apic() -> bool {
    unsafe { cpu::rdmsr(0x1B) & (1 << 10) != 0 }
}

fn apic_write(reg: u32, value: u32) {
    if x2apic() {
        unsafe { cpu::wrmsr(0x800 + reg / 16, value as u64) }
    } else {
        unsafe { ((APIC_MMIO.load(Ordering::Relaxed) + reg as u64) as *mut u32).write_volatile(value) }
    }
}

fn apic_read(reg: u32) -> u32 {
    if x2apic() {
        unsafe { cpu::rdmsr(0x800 + reg / 16) as u32 }
    } else {
        unsafe { ((APIC_MMIO.load(Ordering::Relaxed) + reg as u64) as *const u32).read_volatile() }
    }
}

/// Tells this CPU's local APIC the interrupt it raised is handled.
pub fn eoi() {
    apic_write(0xB0, 0);
}

/// Turns on this (application) CPU's local APIC: the PIC's line stays masked here (devices
/// interrupt the first CPU only), and the timer ticks at the PIT's rate, measured against it.
fn start_local_apic() {
    apic_write(0xF0, 0x1FF); // enabled, spurious vector 0xFF
    apic_write(0x350, 1 << 16); // LINT0 masked
    apic_write(0x360, 0x400); // LINT1: NMI
    apic_write(0x3E0, 0x3); // timer: divide by 16
    // Count down from the top for 5 PIT ticks (50 ms) to learn the timer's speed.
    apic_write(0x320, 1 << 16); // masked, one-shot
    let start = super::irq::ticks();
    while super::irq::ticks() == start {
        core::hint::spin_loop();
    }
    apic_write(0x380, u32::MAX);
    let from = super::irq::ticks();
    while super::irq::ticks() < from + 5 {
        core::hint::spin_loop();
    }
    let per_tick = (u32::MAX - apic_read(0x390)) / 5;
    apic_write(0x320, (1 << 17) | TIMER_VECTOR as u32); // periodic
    apic_write(0x380, per_tick.max(1000));
}

// ---- starting the other CPUs ----

const AP_STACK: usize = 64 * 1024;
/// Each application processor's boot stack (its scheduler runs on it), by CPU number.
#[unsafe(no_mangle)]
static mut AP_STACK_TOPS: [u64; percpu::MAX_CPUS] = [0; percpu::MAX_CPUS];

// The bootloader starts an application processor here, in 64-bit mode on a stack of its own,
// with rdi = its MpInfo (whose extra argument, at 24, is our CPU number). Switch to our stack.
global_asm!(
    ".global ap_trampoline",
    "ap_trampoline:",
    "mov rax, [rdi + 24]",
    "lea rcx, [rip + AP_STACK_TOPS]",
    "mov rsp, [rcx + rax * 8]",
    "xor rbp, rbp",
    "call {main}",
    "ud2",
    main = sym ap_main,
);

unsafe extern "C" {
    fn ap_trampoline(info: &MpInfo) -> !;
}

/// Starts the other CPUs the bootloader found. Returns how many there are in all.
pub fn start(response: Option<&limine::request::MpResponse>) -> usize {
    let Some(response) = response else { return 1 };
    // The local APIC's registers, for the CPUs to share (each sees its own at the same address).
    let base = unsafe { cpu::rdmsr(0x1B) } & 0xffff_f000;
    if !x2apic() {
        APIC_MMIO.store(crate::mm::paging::map_mmio(base) as u64, Ordering::Relaxed);
    }
    let mut next = 1;
    for info in response.cpus() {
        if info.lapic_id == response.bsp_lapic_id || next == percpu::MAX_CPUS {
            continue;
        }
        let stack = Box::leak(alloc::vec![0u8; AP_STACK].into_boxed_slice());
        unsafe { AP_STACK_TOPS[next] = (stack.as_ptr() as u64 + AP_STACK as u64) & !15 };
        let expected = ONLINE.load(Ordering::Acquire) + 1;
        info.bootstrap(ap_trampoline, next as u64);
        // Wait for it to come up (it needs the PIT ticking, which the first CPU's interrupts do).
        let deadline = super::irq::ticks() + 200;
        while ONLINE.load(Ordering::Acquire) < expected && super::irq::ticks() < deadline {
            // Interrupts nest inside the lock this CPU holds (smp depth), so the PIT keeps ticking.
            cpu::wait_for_interrupt();
        }
        next += 1;
    }
    count()
}

extern "C" fn ap_main(info: &MpInfo) -> ! {
    let id = info.extra_argument() as usize;
    unsafe { cpu::write_cr3(crate::mm::paging::kernel_pml4()) };
    crate::mm::paging::init_cpu();
    gdt::init(id);
    percpu::init(id);
    idt::load();
    cpu::enable_sse();
    start_local_apic();
    crate::log!("cpu {id} (local APIC {}) running", info.lapic_id);
    ONLINE.fetch_add(1, Ordering::Release);
    kernel_enter();
    crate::proc::run();
}
