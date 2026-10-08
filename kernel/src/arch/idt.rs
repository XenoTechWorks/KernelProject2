//! Interrupts, exceptions and the system call entry.
//!
//! Every way into the kernel from a program (an exception, a hardware interrupt or the `syscall`
//! instruction) saves the program's registers as the same [`TrapFrame`] on the process's kernel stack,
//! and every way back out goes through `trap_return` (`iretq`). That one shape is what lets the
//! scheduler switch processes, `fork` copy a process, and `execve` start a new program.

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

/// Top of the current process's kernel stack. syscall_entry switches to it (the CPU doesn't).
#[unsafe(no_mangle)]
pub static mut KERNEL_STACK_TOP: u64 = 0;
#[unsafe(no_mangle)]
static mut USER_RSP_SCRATCH: u64 = 0;

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
}

global_asm!(
    ".global trap_common",
    "trap_common:",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "cld",
    "call {dispatch}",
    ".global trap_return",
    "trap_return:",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "add rsp, 16",
    "iretq",
    "",
    // syscall: rcx = user rip, r11 = user rflags; interrupts are off (FMASK).
    ".global syscall_entry",
    "syscall_entry:",
    "mov [rip + USER_RSP_SCRATCH], rsp",
    "mov rsp, [rip + KERNEL_STACK_TOP]",
    "push {user_ss}",
    "push [rip + USER_RSP_SCRATCH]",
    "push r11",
    "push {user_cs}",
    "push rcx",
    "push 0",
    "push {syscall_vector}",
    "jmp trap_common",
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
    static isr_table: [u64; 48];
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
