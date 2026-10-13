//! Interrupts, exceptions and the system call entry.
//!
//! Every way into the kernel from a program (an exception, a hardware interrupt or the `syscall`
//! instruction) saves the program's registers as the same [`TrapFrame`] on the process's kernel stack,
//! and every way back out goes through `trap_return` (`iretq`). That one shape is what lets the
//! scheduler switch processes, `fork` copy a process, and `execve` start a new program.
//!
//! Coming from a program, `swapgs` makes GS point at this CPU's percpu record (and back on the
//! way out); every way in takes the big kernel lock and every way out lets it go (smp.rs).

use super::cpu;
use super::gdt::{self, KERNEL_CODE};
use core::arch::{asm, global_asm};
use core::mem::size_of;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error: u64,
    // Pushed by the CPU (or by syscall_entry in the same layout).
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    pub fn from_user(&self) -> bool {
        self.cs & 3 == 3
    }

    /// A fresh frame that starts a program at `rip` with stack `rsp`.
    pub fn user(rip: u64, rsp: u64) -> Self {
        TrapFrame {
            rip,
            rsp,
            cs: gdt::USER_CODE as u64,
            ss: gdt::USER_DATA as u64,
            rflags: 0x202, // interrupts on
            ..Default::default()
        }
    }
}

pub const SYSCALL_VECTOR: u64 = 0x100;


// One small entry stub per vector. Exceptions without an error code push a 0 so every frame has the
// same shape; `isr_table` lists the stubs' addresses for the IDT.
macro_rules! stubs {
    ($($vector:literal $kind:ident),* $(,)?) => {
        $( stubs!(@one $vector $kind); )*
        global_asm!(
            ".pushsection .rodata", ".balign 8", ".global isr_table", "isr_table:",
            $( concat!(".quad isr_", $vector), )*
            ".popsection",
        );
    };
    (@one $vector:literal plain) => {
        global_asm!(concat!(".global isr_", $vector, "\nisr_", $vector, ":\n push 0\n push ", $vector, "\n jmp trap_common"));
    };
    (@one $vector:literal code) => {
        global_asm!(concat!(".global isr_", $vector, "\nisr_", $vector, ":\n push ", $vector, "\n jmp trap_common"));
    };
}

stubs! {
    0 plain, 1 plain, 2 plain, 3 plain, 4 plain, 5 plain, 6 plain, 7 plain,
    8 code, 9 plain, 10 code, 11 code, 12 code, 13 code, 14 code, 15 plain,
    16 plain, 17 code, 18 plain, 19 plain, 20 plain, 21 code, 22 plain, 23 plain,
    24 plain, 25 plain, 26 plain, 27 plain, 28 plain, 29 code, 30 code, 31 plain,
    32 plain, 33 plain, 34 plain, 35 plain, 36 plain, 37 plain, 38 plain, 39 plain,
    40 plain, 41 plain, 42 plain, 43 plain, 44 plain, 45 plain, 46 plain, 47 plain,
    48 plain,
}

global_asm!(
    ".global trap_common",
    "trap_common:",
    // From a program (its CS, at rsp+24, has RPL 3): GS to this CPU's record.
    "test qword ptr [rsp + 24], 3",
    "jz 1f",
    "swapgs",
    "1:",
    "trap_saved:",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "cld",
    "call {enter}",
    "mov rdi, rsp",
    "call {dispatch}",
    ".global trap_return",
    "trap_return:",
    "call {exit}",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "add rsp, 16",
    // Back to a program: GS back to its own.
    "test qword ptr [rsp + 8], 3",
    "jz 2f",
    "swapgs",
    "2:",
    "iretq",
    "",
    // syscall: rcx = user rip, r11 = user rflags; interrupts are off (FMASK). This CPU's record
    // holds the kernel stack to switch to (gs:[8]) and a place for the program's (gs:[16]).
    ".global syscall_entry",
    "syscall_entry:",
    "swapgs",
    "mov gs:[16], rsp",
    "mov rsp, gs:[8]",
    "push {user_ss}",
    "push qword ptr gs:[16]",
    "push r11",
    "push {user_cs}",
    "push rcx",
    "push 0",
    "push {syscall_vector}",
    "jmp trap_saved",
    enter = sym crate::arch::smp::kernel_enter,
    exit = sym crate::arch::smp::kernel_exit,
    dispatch = sym trap_dispatch,
    user_ss = const gdt::USER_DATA as u64,
    user_cs = const gdt::USER_CODE as u64,
    syscall_vector = const SYSCALL_VECTOR,
);

// The local APIC's "spurious interrupt": nothing to do, not even an EOI.
global_asm!(".global isr_spurious", "isr_spurious:", "iretq");

pub const SPURIOUS_VECTOR: usize = 0xFF;

unsafe extern "C" {
    fn syscall_entry();
    pub fn trap_return();
    fn isr_spurious();
    static isr_table: [u64; 49];
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    flags: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}

impl Gate {
    const EMPTY: Gate = Gate { offset_low: 0, selector: 0, ist: 0, flags: 0, offset_mid: 0, offset_high: 0, _reserved: 0 };

    fn new(address: u64, flags: u8, ist: u8) -> Gate {
        Gate {
            offset_low: address as u16,
            selector: KERNEL_CODE,
            ist,
            flags,
            offset_mid: (address >> 16) as u16,
            offset_high: (address >> 32) as u32,
            _reserved: 0,
        }
    }
}

static mut IDT: [Gate; 256] = [Gate::EMPTY; 256];

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

pub fn init() {
    unsafe {
        for (vector, &address) in isr_table.iter().enumerate() {
            // Present interrupt gates; int3 may be used from user mode; double faults get their own stack.
            let flags = if vector == 3 { 0xEE } else { 0x8E };
            IDT[vector] = Gate::new(address, flags, if vector == 8 { 1 } else { 0 });
        }
        IDT[SPURIOUS_VECTOR] = Gate::new(isr_spurious as *const () as u64, 0x8E, 0);
    }
    load();
}

/// Loads the IDT and sets up `syscall` on this CPU (every CPU does it once).
pub fn load() {
    unsafe {
        let pointer = Pointer { limit: (size_of::<[Gate; 256]>() - 1) as u16, base: &raw const IDT as u64 };
        asm!("lidt [{}]", in(reg) &pointer);

        // syscall/sysret: kernel CS in STAR[47:32]; user base selector (user data - 8) in STAR[63:48].
        cpu::wrmsr(cpu::MSR_EFER, cpu::rdmsr(cpu::MSR_EFER) | 1);
        let star = ((KERNEL_CODE as u64) << 32) | (((gdt::USER_DATA as u64 & !3) - 8) << 48);
        cpu::wrmsr(cpu::MSR_STAR, star);
        cpu::wrmsr(cpu::MSR_LSTAR, syscall_entry as *const () as u64);
        cpu::wrmsr(cpu::MSR_FMASK, 0x4_0700); // clear IF, TF, DF and AC on entry
    }
}

const EXCEPTION_NAMES: [&str; 32] = [
    "divide error", "debug", "NMI", "breakpoint", "overflow", "bound range", "invalid opcode",
    "device not available", "double fault", "coprocessor overrun", "invalid TSS", "segment not present",
    "stack fault", "general protection fault", "page fault", "reserved", "x87 floating point",
    "alignment check", "machine check", "SIMD floating point", "virtualization", "control protection",
    "reserved", "reserved", "reserved", "reserved", "reserved", "reserved", "hypervisor injection",
    "VMM communication", "security", "reserved",
];

extern "C" fn trap_dispatch(frame: &mut TrapFrame) {
    match frame.vector {
        SYSCALL_VECTOR => crate::sys::dispatch(frame),
        32..=47 => {
            crate::arch::irq::handle(frame);
            if frame.from_user() {
                crate::proc::signal::deliver(frame);
            }
        }
        // The local APIC timer of the other CPUs: time to take turns.
        crate::arch::smp::TIMER_VECTOR => {
            crate::arch::smp::eoi();
            crate::proc::preempt(frame);
            if frame.from_user() {
                crate::proc::signal::deliver(frame);
            }
        }
        vector => {
            let name = EXCEPTION_NAMES[vector as usize & 31];
            if frame.from_user() {
                crate::proc::user_fault(frame, name);
            } else {
                panic!(
                    "{name} in the kernel at {:#x} (error {:#x}, cr2 {:#x})\n{frame:#x?}",
                    frame.rip,
                    frame.error,
                    cpu::read_cr2()
                );
            }
        }
    }
}
