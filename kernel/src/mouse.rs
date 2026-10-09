//! PS/2 mouse: the second port of the keyboard controller (the 8042), interrupt 12. It sends
//! 3-byte packets: buttons and sign bits, then movement in x and y. The console moves the pointer.

use crate::arch::cpu::{inb, outb};
use spin::Mutex;

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64; // reading: status; writing: a command for the controller

struct Packet {
    bytes: [u8; 3],
    len: usize,
}

static PACKET: Mutex<Packet> = Mutex::new(Packet { bytes: [0; 3], len: 0 });

/// Waits until the controller can take a byte (or gives up, so a missing controller can't hang us).
fn wait_write() -> bool {
    (0..100_000).any(|_| unsafe { inb(STATUS) } & 2 == 0)
}

fn wait_read() -> bool {
    (0..100_000).any(|_| unsafe { inb(STATUS) } & 1 != 0)
}

fn command(byte: u8) {
    if wait_write() {
        unsafe { outb(STATUS, byte) };
    }
}

/// Sends a byte to the mouse and reads its answer (0xFA, "acknowledged").
fn to_mouse(byte: u8) -> Option<u8> {
    command(0xD4); // the next data byte goes to the second port
    if !wait_write() {
        return None;
    }
    unsafe { outb(DATA, byte) };
    wait_read().then(|| unsafe { inb(DATA) })
}

/// Turns the mouse on. Runs before interrupts are enabled, so the answers can be read here.
pub fn init() {
    command(0xA8); // enable the second port
    command(0x20); // read the configuration byte
    if !wait_read() {
        return;
    }
    let config = unsafe { inb(DATA) };
    command(0x60); // write it back with the second port's interrupt on and its clock running
    if wait_write() {
        unsafe { outb(DATA, (config | 0x02) & !0x20) };
    }
    let defaults = to_mouse(0xF6);
    let enabled = to_mouse(0xF4); // start sending packets
    if defaults == Some(0xFA) && enabled == Some(0xFA) {
        crate::log!("ps/2 mouse enabled");
    }
}

/// A byte from the mouse (interrupt 12).
pub fn byte(b: u8) {
    let mut p = PACKET.lock();
    // The first byte always has bit 3 set; anything else means we lost our place.
    if p.len == 0 && b & 0x08 == 0 {
        return;
    }
    let i = p.len;
    p.bytes[i] = b;
    p.len += 1;
    if p.len < 3 {
        return;
    }
    p.len = 0;
    let [flags, x, y] = p.bytes;
    drop(p);
    if flags & 0xC0 != 0 {
        return; // overflowed: the numbers are meaningless
    }
    // 9-bit two's complement movement; the sign bits are in the first byte. Up is positive.
    let dx = x as i32 - (((flags as i32) << 4) & 0x100);
    let dy = y as i32 - (((flags as i32) << 3) & 0x100);
    crate::console::mouse(dx, -dy, flags & 0x07);
}
