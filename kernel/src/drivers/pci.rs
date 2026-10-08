//! PCI: finding devices and reading their configuration (through the legacy 0xCF8/0xCFC ports, which
//! every PC and QEMU machine has).

use alloc::vec::Vec;
use core::arch::asm;

#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
}

fn outl(port: u16, value: u32) {
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags)) }
}

fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe { asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack, preserves_flags)) }
    value
}

fn address(d: &Device, offset: u8) -> u32 {
    0x8000_0000 | (d.bus as u32) << 16 | (d.slot as u32) << 11 | (d.func as u32) << 8 | (offset as u32 & 0xFC)
}

impl Device {
    pub fn read32(&self, offset: u8) -> u32 {
        outl(0xCF8, address(self, offset));
        inl(0xCFC)
    }

    pub fn read8(&self, offset: u8) -> u8 {
        (self.read32(offset) >> ((offset & 3) * 8)) as u8
    }

    pub fn write32(&self, offset: u8, value: u32) {
        outl(0xCF8, address(self, offset));
        outl(0xCFC, value);
    }

    /// Turns on memory access and bus mastering (the device reading and writing RAM itself).
    pub fn enable(&self) {
        let command = self.read32(0x04);
        self.write32(0x04, command | 0b110);
    }

    /// The physical address a memory BAR points at.
    pub fn bar(&self, index: u8) -> u64 {
        let offset = 0x10 + index * 4;
        let low = self.read32(offset);
        if low & 1 != 0 {
            return (low & !3) as u64; // I/O port BAR
        }
        let base = (low & !0xF) as u64;
        if (low >> 1) & 3 == 2 { base | (self.read32(offset + 4) as u64) << 32 } else { base }
    }

    /// Walks the capability list, calling `f(offset_of_capability, id)`.
    pub fn capabilities(&self, mut f: impl FnMut(u8, u8)) {
        if self.read32(0x04) >> 16 & 0x10 == 0 {
            return; // no capability list
        }
        let mut at = self.read8(0x34) & !3;
        let mut guard = 0;
        while at != 0 && guard < 48 {
            f(at, self.read8(at));
            at = self.read8(at + 1) & !3;
            guard += 1;
        }
    }
}

/// Every function on buses 0..=255 that answers.
pub fn devices() -> Vec<Device> {
    let mut found = Vec::new();
    for bus in 0..=255u8 {
        for slot in 0..32u8 {
            for func in 0..8u8 {
                let probe = Device { bus, slot, func, vendor: 0, device: 0 };
                let id = probe.read32(0);
                if id & 0xFFFF == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                found.push(Device { vendor: id as u16, device: (id >> 16) as u16, ..probe });
                // Single-function devices only answer on function 0.
                if func == 0 && probe.read32(0x0C) >> 16 & 0x80 == 0 {
                    break;
                }
            }
        }
    }
    found
}
