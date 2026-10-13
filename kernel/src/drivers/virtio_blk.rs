//! A virtio block device (QEMU's `-drive if=virtio`), using the modern virtio 1.x PCI interface.
//!
//! One request queue per disk, used synchronously: put a request (header, data, status byte) on
//! the queue, tell the device, and wait for it to come back on the used ring. Simple and fine for
//! one CPU; an interrupt-driven version can come later. Every disk is found (vda, vdb, ...).

use super::virtio::{self, Descriptor, NEXT, Queue, WRITE, r32};
use crate::mm::{PAGE_SIZE, frame, phys_to_virt};
use core::sync::atomic::{Ordering, fence};
use alloc::vec::Vec;
use spin::Mutex;

pub const SECTOR: usize = 512;
const QUEUE_SIZE: u16 = 64;

pub struct Disk {
    queue: Queue,
    last_used: u16,
    /// One page for the request header + status, one bounce page for data.
    request_page: u64,
    /// How big it is, in sectors.
    pub sectors: u64,
}

unsafe impl Send for Disk {}

/// The disks, in the order they were found: vda, vdb, ...
pub static DISKS: Mutex<Vec<Disk>> = Mutex::new(Vec::new());

/// Finds every virtio block device (up to 8) and sets them up. Returns their sizes in MiB.
pub fn init() -> Vec<u64> {
    let mut sizes = Vec::new();
    for nth in 0..8 {
        let Some(mut dev) = virtio::start_nth(&[0x1001, 0x1042], "virtio-blk", 0, nth) else { break };
        let (Some(queue), Some(request_page)) = (dev.queue(0, QUEUE_SIZE), frame::alloc()) else {
            crate::log!("virtio-blk: out of memory");
            break;
        };
        dev.ready();
        let sectors = r32(dev.config, 0) as u64 | (r32(dev.config, 4) as u64) << 32;
        let mib = sectors * SECTOR as u64 / (1024 * 1024);
        crate::log!("virtio-blk: {} {mib} MiB disk", name(nth));
        DISKS.lock().push(Disk { queue, last_used: 0, request_page, sectors });
        sizes.push(mib);
    }
    sizes
}

/// A disk's name, as Linux calls virtio disks: vda, vdb, ...
pub fn name(index: usize) -> alloc::string::String {
    alloc::format!("vd{}", (b'a' + index as u8) as char)
}

pub fn count() -> usize {
    DISKS.lock().len()
}

/// A disk's size in bytes.
pub fn size(index: usize) -> Option<u64> {
    DISKS.lock().get(index).map(|d| d.sectors * SECTOR as u64)
}

/// Reads whole sectors of disk `index` from `sector` on.
pub fn read(index: usize, sector: u64, buf: &mut [u8]) -> Result<(), ()> {
    DISKS.lock().get_mut(index).ok_or(())?.read(sector, buf)
}

pub fn write(index: usize, sector: u64, buf: &[u8]) -> Result<(), ()> {
    DISKS.lock().get_mut(index).ok_or(())?.write(sector, buf)
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
            let desc = self.queue.desc;
            desc.write(Descriptor { addr: base, len: 16, flags: NEXT, next: 1 });
            desc.add(1).write(Descriptor {
                addr: base + SECTOR as u64,
                len: buf.len() as u32,
                flags: NEXT | if write { 0 } else { WRITE },
                next: 2,
            });
            desc.add(2).write(Descriptor { addr: base + 16, len: 1, flags: WRITE, next: 0 });
            self.queue.offer(0);
            self.queue.notify();

            // Wait for the used ring's idx to move.
            while self.queue.used_idx() == self.last_used {
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
