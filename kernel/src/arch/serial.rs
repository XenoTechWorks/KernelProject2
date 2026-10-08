//! COM1 serial port: the kernel's log, and a second keyboard (QEMU's -serial stdio).

use super::cpu::{inb, outb};

const COM1: u16 = 0x3F8;

pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00); // no interrupts while setting up
        outb(COM1 + 3, 0x80); // divisor latch on
        outb(COM1, 0x01); // 115200 baud
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03); // 8 bits, no parity, one stop bit
        outb(COM1 + 2, 0xC7); // FIFO on
        outb(COM1 + 4, 0x0B); // RTS/DSR, OUT2 (routes the interrupt)
        outb(COM1 + 1, 0x01); // interrupt when a byte arrives
    }
}

pub fn write_byte(byte: u8) {
    unsafe {
        while inb(COM1 + 5) & 0x20 == 0 {}
        outb(COM1, byte);
    }
}

pub fn write_str(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            write_byte(b'\r');
        }
        write_byte(b);
    }
}

/// The next received byte, if any.
pub fn read_byte() -> Option<u8> {
    unsafe { if inb(COM1 + 5) & 1 != 0 { Some(inb(COM1)) } else { None } }
}
