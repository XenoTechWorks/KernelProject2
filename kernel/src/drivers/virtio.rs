//! What all virtio devices share (the modern virtio 1.x PCI interface): finding the device's
//! configuration structures, the reset-and-agree-on-features handshake, and setting up queues.

use super::pci;
use crate::mm::paging::map_mmio;
use crate::mm::{PAGE_SIZE, frame, phys_to_virt};

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

pub fn r8(base: *mut u8, at: usize) -> u8 {
    unsafe { base.add(at).read_volatile() }
}
pub fn w8(base: *mut u8, at: usize, v: u8) {
    unsafe { base.add(at).write_volatile(v) }
}
pub fn r16(base: *mut u8, at: usize) -> u16 {
    unsafe { (base.add(at) as *mut u16).read_volatile() }
}
pub fn w16(base: *mut u8, at: usize, v: u16) {
    unsafe { (base.add(at) as *mut u16).write_volatile(v) }
}
pub fn r32(base: *mut u8, at: usize) -> u32 {
    unsafe { (base.add(at) as *mut u32).read_volatile() }
}
pub fn w32(base: *mut u8, at: usize, v: u32) {
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

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

pub const NEXT: u16 = 1;
pub const WRITE: u16 = 2; // the device writes this buffer

/// A device that has been reset and has agreed to the features.
pub struct Device {
    common: *mut u8,
    notify_base: *mut u8,
    notify_multiplier: u32,
    /// The device-specific configuration (what it is: a disk's size, an input device's name...).
    pub config: *mut u8,
    /// The feature bits (0..31) both sides agreed on.
    pub features: u32,
}

/// A queue: the descriptor table, the available ring (ours) and the used ring (the device's).
pub struct Queue {
    pub size: u16,
    pub desc: *mut Descriptor,
    pub avail: *mut u16,
    pub used: *mut u8,
    notify: *mut u16,
}

impl Queue {
    /// Offers descriptor `head` to the device.
    pub fn offer(&mut self, head: u16) {
        use core::sync::atomic::{Ordering, fence};
        unsafe {
            let idx = self.avail.add(1).read_volatile();
            self.avail.add(2 + (idx % self.size) as usize).write_volatile(head);
            fence(Ordering::SeqCst);
            self.avail.add(1).write_volatile(idx.wrapping_add(1));
            fence(Ordering::SeqCst);
        }
    }

    /// Tells the device to look at the queue.
    pub fn notify(&mut self) {
        unsafe { self.notify.write_volatile(0) };
    }

    /// How many buffers the device has given back so far (the used ring's index).
    pub fn used_idx(&self) -> u16 {
        unsafe { (self.used.add(2) as *const u16).read_volatile() }
    }

    /// The used ring's entry `n` (mod the queue size): (descriptor, bytes written).
    pub fn used_entry(&self, n: u16) -> (u32, u32) {
        let at = 4 + 8 * (n % self.size) as usize;
        unsafe { ((self.used.add(at) as *const u32).read_volatile(), (self.used.add(at + 4) as *const u32).read_volatile()) }
    }
}

/// Finds the first virtio device with one of these PCI device ids (legacy and modern), resets it
/// and agrees on features: VIRTIO_F_VERSION_1, plus any of `wanted` (the device's own feature
/// bits 0..31) that it offers. `name` is for the log. The features agreed on are in `features`.
pub fn start(ids: &[u16], name: &str, wanted: u32) -> Option<Device> {
    let dev = pci::devices().into_iter().find(|d| d.vendor == 0x1AF4 && ids.contains(&d.device))?;
    dev.enable();

    // virtio's PCI capabilities say where each configuration structure lives.
    let (mut common, mut notify, mut config, mut notify_multiplier) = (None, None, None, 0);
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
            4 => config = Some(region),
            _ => {}
        }
    });
    let (Some(common), Some(notify_base), Some(config)) = (common, notify, config) else {
        crate::log!("{name}: device has no modern interface (start QEMU with disable-legacy=on)");
        return None;
    };

    w8(common, DEVICE_STATUS, 0);
    while r8(common, DEVICE_STATUS) != 0 {}
    w8(common, DEVICE_STATUS, ACKNOWLEDGE | DRIVER);
    w32(common, DEVICE_FEATURE_SELECT, 1);
    if r32(common, DEVICE_FEATURE) & 1 == 0 {
        crate::log!("{name}: device lacks VIRTIO_F_VERSION_1");
        return None;
    }
    w32(common, DEVICE_FEATURE_SELECT, 0);
    let features = r32(common, DEVICE_FEATURE) & wanted;
    w32(common, DRIVER_FEATURE_SELECT, 0);
    w32(common, DRIVER_FEATURE, features);
    w32(common, DRIVER_FEATURE_SELECT, 1);
    w32(common, DRIVER_FEATURE, 1);
    w8(common, DEVICE_STATUS, ACKNOWLEDGE | DRIVER | FEATURES_OK);
    if r8(common, DEVICE_STATUS) & FEATURES_OK == 0 {
        crate::log!("{name}: features not accepted");
        return None;
    }
    Some(Device { common, notify_base, notify_multiplier, config, features })
}

impl Device {
    /// Sets up queue `index` with at most `max` entries (one page for each part).
    pub fn queue(&mut self, index: u16, max: u16) -> Option<Queue> {
        w16(self.common, QUEUE_SELECT, index);
        let size = r16(self.common, QUEUE_SIZE_REG).min(max);
        w16(self.common, QUEUE_SIZE_REG, size);
        let (desc, avail, used) = (frame::alloc()?, frame::alloc()?, frame::alloc()?);
        w64(self.common, QUEUE_DESC, desc);
        w64(self.common, QUEUE_DRIVER, avail);
        w64(self.common, QUEUE_DEVICE, used);
        let notify_off = r16(self.common, QUEUE_NOTIFY_OFF) as usize;
        w16(self.common, QUEUE_ENABLE, 1);
        Some(Queue {
            size,
            desc: phys_to_virt(desc) as *mut Descriptor,
            avail: phys_to_virt(avail) as *mut u16,
            used: phys_to_virt(used),
            notify: unsafe { self.notify_base.add(notify_off * self.notify_multiplier as usize) } as *mut u16,
        })
    }

    /// Done setting up: the device can start.
    pub fn ready(&mut self) {
        w8(self.common, DEVICE_STATUS, ACKNOWLEDGE | DRIVER | FEATURES_OK | DRIVER_OK);
    }
}
