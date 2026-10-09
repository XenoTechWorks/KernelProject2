//! A virtio block device (QEMU's `-drive if=virtio`), using the modern virtio 1.x PCI interface.
//!
//! One request queue, used synchronously: put a request (header, data, status byte) on the queue,
//! tell the device, and wait for it to come back on the used ring. Simple and fine for one CPU; an
//! interrupt-driven version can come with more devices.

use super::virtio::{self, Descriptor, NEXT, Queue, WRITE, r32};
use crate::mm::{PAGE_SIZE, frame, phys_to_virt};
use core::sync::atomic::{Ordering, fence};
use spin::Mutex;

pub const SECTOR: usize = 512;
const QUEUE_SIZE: u16 = 64;

pub struct Disk {
    queue: Queue,
    last_used: u16,
    /// One page for the request header + status, one bounce page for data.
    request_page: u64,
}

unsafe impl Send for Disk {}

pub static DISK: Mutex<Option<Disk>> = Mutex::new(None);

/// Finds the first virtio block device and sets it up.
pub fn init() {
    let Some(mut dev) = virtio::start(&[0x1001, 0x1042], "virtio-blk") else { return };
    let (Some(queue), Some(request_page)) = (dev.queue(0, QUEUE_SIZE), frame::alloc()) else {
        crate::log!("virtio-blk: out of memory");
        return;
    };
    dev.ready();
    let sectors = r32(dev.config, 0) as u64 | (r32(dev.config, 4) as u64) << 32;
    crate::log!("virtio-blk: {} MiB disk", sectors * SECTOR as u64 / (1024 * 1024));
    *DISK.lock() = Some(Disk { queue, last_used: 0, request_page });
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
