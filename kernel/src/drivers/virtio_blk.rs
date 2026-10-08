//! A virtio block device (QEMU's `-drive if=virtio`), using the modern virtio 1.x PCI interface.
//!
//! One request queue, used synchronously: put a request (header, data, status byte) on the queue,
//! tell the device, and wait for it to come back on the used ring. Simple and fine for one CPU; an
//! interrupt-driven version can come with more devices.

use super::pci;
use crate::mm::paging::map_mmio;
use crate::mm::{PAGE_SIZE, frame, phys_to_virt};
use core::sync::atomic::{Ordering, fence};
use spin::Mutex;

pub const SECTOR: usize = 512;
const QUEUE_SIZE: u16 = 64;

// Status bits.
const ACKNOWLEDGE: u8 = 1;
const DRIVER: u8 = 2;
const DRIVER_OK: u8 = 4;
const FEATURES_OK: u8 = 8;

// Offsets in the common configuration structure (virtio 1.x, section 4.1.4.3).
const DEVICE_FEATURE_SELECT: usize = 0x00;
const DEVICE_FEATURE: usize = 0x04;
const DRIVER_FEATURE_SELECT: usize = 0x08;
const DRIVER_FEATURE: usize = 0x0C;
const DEVICE_STATUS: usize = 0x14;
const QUEUE_SELECT: usize = 0x16;
const QUEUE_SIZE_REG: usize = 0x18;
const QUEUE_ENABLE: usize = 0x1C;
const QUEUE_NOTIFY_OFF: usize = 0x1E;
const QUEUE_DESC: usize = 0x20;
const QUEUE_DRIVER: usize = 0x28;
const QUEUE_DEVICE: usize = 0x30;

#[repr(C)]
#[derive(Clone, Copy)]
struct Descriptor {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

const NEXT: u16 = 1;
const WRITE: u16 = 2; // the device writes this buffer

pub struct Disk {
    notify: *mut u16,
    desc: *mut Descriptor,
    avail: *mut u16,
    used: *mut u8,
    last_used: u16,
    /// One page for the request header + status, one bounce page for data.
    request_page: u64,
}

unsafe impl Send for Disk {}

pub static DISK: Mutex<Option<Disk>> = Mutex::new(None);

fn r8(base: *mut u8, at: usize) -> u8 {
    unsafe { base.add(at).read_volatile() }
}
fn w8(base: *mut u8, at: usize, v: u8) {
    unsafe { base.add(at).write_volatile(v) }
}
fn r16(base: *mut u8, at: usize) -> u16 {
    unsafe { (base.add(at) as *mut u16).read_volatile() }
}
fn w16(base: *mut u8, at: usize, v: u16) {
    unsafe { (base.add(at) as *mut u16).write_volatile(v) }
}
fn r32(base: *mut u8, at: usize) -> u32 {
    unsafe { (base.add(at) as *mut u32).read_volatile() }
}
fn w32(base: *mut u8, at: usize, v: u32) {
    unsafe { (base.add(at) as *mut u32).write_volatile(v) }
}
fn w64(base: *mut u8, at: usize, v: u64) {
    w32(base, at, v as u32);
    w32(base, at + 4, (v >> 32) as u32);
}

/// Maps `len` bytes of a BAR region and returns its address.
fn map_region(phys: u64, len: usize) -> *mut u8 {
    let mut page = phys & !(PAGE_SIZE - 1);
    while page < phys + len as u64 {
        map_mmio(page);
        page += PAGE_SIZE;
    }
    phys_to_virt(phys)
}

/// Finds the first virtio block device and sets it up.
pub fn init() {
    let Some(dev) = pci::devices().into_iter().find(|d| d.vendor == 0x1AF4 && (d.device == 0x1001 || d.device == 0x1042)) else {
        return;
    };
    dev.enable();

    // virtio's PCI capabilities say where each configuration structure lives.
    let (mut common, mut notify, mut device_cfg, mut notify_multiplier) = (None, None, None, 0);
    dev.capabilities(|at, id| {
        if id != 0x09 {
            return;
        }
        let kind = dev.read8(at + 3);
        let bar = dev.read8(at + 4);
        let offset = dev.read32(at + 8) as u64;
        let length = dev.read32(at + 12) as usize;
        let region = map_region(dev.bar(bar) + offset, length.max(4));
        match kind {
            1 => common = Some(region),
            2 => {
                notify = Some(region);
                notify_multiplier = dev.read32(at + 16);
            }
            4 => device_cfg = Some(region),
            _ => {}
        }
    });
    let (Some(common), Some(notify_base), Some(device_cfg)) = (common, notify, device_cfg) else {
        crate::log!("virtio-blk: device has no modern interface (start QEMU with disable-legacy=on)");
        return;
    };

    // Reset, then say hello and agree on features (only VIRTIO_F_VERSION_1).
    w8(common, DEVICE_STATUS, 0);
    while r8(common, DEVICE_STATUS) != 0 {}
    w8(common, DEVICE_STATUS, ACKNOWLEDGE | DRIVER);
    w32(common, DEVICE_FEATURE_SELECT, 1);
    let high = r32(common, DEVICE_FEATURE);
    if high & 1 == 0 {
        crate::log!("virtio-blk: device lacks VIRTIO_F_VERSION_1");
        return;
    }
    w32(common, DRIVER_FEATURE_SELECT, 0);
    w32(common, DRIVER_FEATURE, 0);
    w32(common, DRIVER_FEATURE_SELECT, 1);
    w32(common, DRIVER_FEATURE, 1);
    w8(common, DEVICE_STATUS, ACKNOWLEDGE | DRIVER | FEATURES_OK);
    if r8(common, DEVICE_STATUS) & FEATURES_OK == 0 {
        crate::log!("virtio-blk: features not accepted");
        return;
    }

    // Queue 0: descriptor table, available ring and used ring, one page each.
    w16(common, QUEUE_SELECT, 0);
    let size = r16(common, QUEUE_SIZE_REG).min(QUEUE_SIZE);
    w16(common, QUEUE_SIZE_REG, size);
    let pages = [frame::alloc(), frame::alloc(), frame::alloc(), frame::alloc()];
    let [Some(desc), Some(avail), Some(used), Some(request_page)] = pages else {
        crate::log!("virtio-blk: out of memory");
        return;
    };
    w64(common, QUEUE_DESC, desc);
    w64(common, QUEUE_DRIVER, avail);
    w64(common, QUEUE_DEVICE, used);
    let notify_off = r16(common, QUEUE_NOTIFY_OFF) as usize;
    w16(common, QUEUE_ENABLE, 1);
    w8(common, DEVICE_STATUS, ACKNOWLEDGE | DRIVER | FEATURES_OK | DRIVER_OK);

    let sectors = r32(device_cfg, 0) as u64 | (r32(device_cfg, 4) as u64) << 32;
    let disk = Disk {
        notify: unsafe { notify_base.add(notify_off * notify_multiplier as usize) } as *mut u16,
        desc: phys_to_virt(desc) as *mut Descriptor,
        avail: phys_to_virt(avail) as *mut u16,
        used: phys_to_virt(used),
        last_used: 0,
        request_page,
    };
    crate::log!("virtio-blk: {} MiB disk", sectors * SECTOR as u64 / (1024 * 1024));
    *DISK.lock() = Some(disk);
}

impl Disk {
    /// Reads or writes up to 7 sectors (one page) through the bounce buffer.
    fn transfer(&mut self, sector: u64, buf: &mut [u8], write: bool) -> Result<(), ()> {
        assert!(buf.len() % SECTOR == 0 && buf.len() <= PAGE_SIZE as usize - SECTOR);
        let page = phys_to_virt(self.request_page);
        // Layout of the request page: header (16 bytes) at 0, status byte at 16, data from 512.
        unsafe {
            (page as *mut u32).write(if write { 1 } else { 0 }); // VIRTIO_BLK_T_OUT / _IN
            (page.add(4) as *mut u32).write(0);
            (page.add(8) as *mut u64).write(sector);
            page.add(16).write(0xFF);
            if write {
                core::ptr::copy_nonoverlapping(buf.as_ptr(), page.add(SECTOR), buf.len());
            }
            let base = self.request_page;
            self.desc.write(Descriptor { addr: base, len: 16, flags: NEXT, next: 1 });
            self.desc.add(1).write(Descriptor {
                addr: base + SECTOR as u64,
                len: buf.len() as u32,
                flags: NEXT | if write { 0 } else { WRITE },
                next: 2,
            });
            self.desc.add(2).write(Descriptor { addr: base + 16, len: 1, flags: WRITE, next: 0 });

            // Available ring: flags, idx, ring[]. Put descriptor 0 in the next slot, then bump idx.
            let idx = self.avail.add(1).read_volatile();
            self.avail.add(2 + (idx % QUEUE_SIZE) as usize).write_volatile(0);
            fence(Ordering::SeqCst);
            self.avail.add(1).write_volatile(idx.wrapping_add(1));
            fence(Ordering::SeqCst);
            self.notify.write_volatile(0);

            // Wait for the used ring's idx to move.
            while (self.used.add(2) as *const u16).read_volatile() == self.last_used {
                core::hint::spin_loop();
            }
            self.last_used = self.last_used.wrapping_add(1);
            fence(Ordering::SeqCst);
            if page.add(16).read_volatile() != 0 {
                return Err(());
            }
            if !write {
                core::ptr::copy_nonoverlapping(page.add(SECTOR), buf.as_mut_ptr(), buf.len());
            }
        }
        Ok(())
    }

    pub fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), ()> {
        for (i, chunk) in buf.chunks_mut(SECTOR * 7).enumerate() {
            self.transfer(sector + (i * 7) as u64, chunk, false)?;
        }
        Ok(())
    }

    pub fn write(&mut self, sector: u64, buf: &[u8]) -> Result<(), ()> {
        let mut tmp = alloc::vec::Vec::from(buf);
        for (i, chunk) in tmp.chunks_mut(SECTOR * 7).enumerate() {
            self.transfer(sector + (i * 7) as u64, chunk, true)?;
        }
        Ok(())
    }
}
