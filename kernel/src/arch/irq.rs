//! Hardware interrupts through the legacy 8259 PIC: the timer (PIT), the PS/2 keyboard and mouse,
//! and COM1.

use super::cpu::{inb, outb};
use super::idt::TrapFrame;
use core::sync::atomic::{AtomicU64, Ordering};

pub const TIMER_HZ: u64 = 100;
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Timer ticks since boot (TIMER_HZ per second).
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn init() {
    unsafe {
        // The 8259 PIC reaches the CPU through the local APIC's LINT0 input, which the bootloader
        // leaves masked. Set it to "ExtINT" (pass the PIC's interrupts through), the BIOS's
        // "virtual wire" setup. Fine for one CPU; multiple CPUs will need the I/O APIC instead.
        const IA32_APIC_BASE: u32 = 0x1B;
        let base = super::cpu::rdmsr(IA32_APIC_BASE);
        if base & (1 << 10) != 0 {
            // x2APIC mode: registers are MSRs at 0x800 + offset / 16.
            super::cpu::wrmsr(0x80F, 0x1FF); // spurious vector register: enable, vector 0xFF
            super::cpu::wrmsr(0x835, 0x700); // LINT0: ExtINT, unmasked
            super::cpu::wrmsr(0x836, 0x400); // LINT1: NMI
        } else {
            let apic = crate::mm::paging::map_mmio(base & 0xffff_f000);
            let reg = |offset: usize, value: u32| (apic.add(offset) as *mut u32).write_volatile(value);
            reg(0xF0, 0x1FF);
            reg(0x350, 0x700);
            reg(0x360, 0x400);
        }

        // Move the PIC's IRQs to vectors 32..47 (0..31 are CPU exceptions).
        outb(0x20, 0x11);
        outb(0xA0, 0x11);
        outb(0x21, 32);
        outb(0xA1, 40);
        outb(0x21, 4);
        outb(0xA1, 2);
        outb(0x21, 0x01);
        outb(0xA1, 0x01);
        // Unmask timer (0), keyboard (1), cascade (2), COM1 (4) and the mouse (12).
        outb(0x21, !0b1_0111);
        outb(0xA1, !(1 << 4));

        // PIT channel 0, rate generator.
        let divisor = (1_193_182 / TIMER_HZ) as u16;
        outb(0x43, 0x34);
        outb(0x40, divisor as u8);
        outb(0x40, (divisor >> 8) as u8);
    }
}

pub fn handle(frame: &mut TrapFrame) {
    let irq = (frame.vector - 32) as u8;
    match irq {
        0 => {
            TICKS.fetch_add(1, Ordering::Relaxed);
        }
        1 => {
            let scancode = unsafe { inb(0x60) };
            crate::keyboard::scancode(scancode);
        }
        12 => {
            let byte = unsafe { inb(0x60) };
            crate::mouse::byte(byte);
        }
        4 => {
            while let Some(byte) = super::serial::read_byte() {
                crate::tty::input(&[byte]);
            }
        }
        _ => {}
    }
    unsafe {
        if irq >= 8 {
            outb(0xA0, 0x20);
        }
        outb(0x20, 0x20);
    }
    if irq == 0 {
        crate::proc::timer_tick(frame);
    }
}
