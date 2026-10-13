//! QEMU's firmware configuration device (fw_cfg): named files QEMU hands the guest, given with
//! `-fw_cfg name=opt/...,string=...` (or `file=`). On a real PC there's nothing at these ports and
//! the signature check fails.

use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;

const SELECTOR: u16 = 0x510;
const DATA: u16 = 0x511;
const SIGNATURE: u16 = 0x0000;
const FILE_DIR: u16 = 0x0019;

fn select(key: u16) {
    unsafe { asm!("out dx, ax", in("dx") SELECTOR, in("ax") key, options(nomem, nostack, preserves_flags)) };
}

fn read(buf: &mut [u8]) {
    for b in buf {
        *b = unsafe { super::cpu::inb(DATA) };
    }
}

/// A file QEMU was given for the guest, by its name.
pub fn read_file(name: &str) -> Option<Vec<u8>> {
    let mut sig = [0u8; 4];
    select(SIGNATURE);
    read(&mut sig);
    if &sig != b"QEMU" {
        return None;
    }
    select(FILE_DIR);
    let mut count = [0u8; 4];
    read(&mut count);
    // Each entry: size (big-endian u32), selector (u16), reserved (u16), name (56 bytes).
    let mut found = None;
    for _ in 0..u32::from_be_bytes(count).min(1024) {
        let mut entry = [0u8; 64];
        read(&mut entry);
        let len = entry[8..].iter().position(|&b| b == 0).unwrap_or(56);
        if &entry[8..8 + len] == name.as_bytes() {
            let size = u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]) as usize;
            found = Some((u16::from_be_bytes([entry[4], entry[5]]), size.min(4096)));
        }
    }
    let (key, size) = found?;
    let mut data = vec![0u8; size];
    select(key);
    read(&mut data);
    Some(data)
}
