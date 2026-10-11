//! A virtio network card (QEMU's `-device virtio-net-pci`): sending and receiving Ethernet frames.
//!
//! Two queues: one of empty buffers the card fills with what arrives, one of frames for it to
//! send. Every buffer starts with a 12-byte header the card uses for checksum and segmentation
//! offloading; we ask for none of that, so ours is all zeros and theirs is ignored.
//!
//! Arriving frames are collected on each timer tick and handed to the network stack (net/).

use super::virtio::{self, Descriptor, Queue, WRITE, r8};
use crate::mm::{PAGE_SIZE, frame, phys_to_virt};
use spin::Mutex;

/// The card tells us its address if it offers this.
const F_MAC: u32 = 1 << 5;

const HEADER: usize = 12;
/// Ethernet's biggest frame, plus the header, rounded to something tidy.
const BUFFER: usize = 2048;
const QUEUE_SIZE: u16 = 16;

struct Net {
    rx: Queue,
    tx: Queue,
    /// One page per two buffers, kept as physical addresses.
    rx_buffers: [u64; QUEUE_SIZE as usize],
    tx_buffers: [u64; QUEUE_SIZE as usize],
    rx_seen: u16,
    tx_seen: u16,
    /// The next transmit buffer to use, round robin.
    tx_next: u16,
    pub mac: [u8; 6],
}

unsafe impl Send for Net {}

static NET: Mutex<Option<Net>> = Mutex::new(None);

/// Two buffers fit in a page, so one page serves two slots.
fn buffer_pages(into: &mut [u64; QUEUE_SIZE as usize]) -> bool {
    let mut i = 0;
    while i < into.len() {
        let Some(page) = frame::alloc() else { return false };
        into[i] = page;
        into[i + 1] = page + BUFFER as u64;
        i += 2;
    }
    true
}

pub fn init() {
    let Some(mut dev) = virtio::start(&[0x1000, 0x1041], "virtio-net", F_MAC) else { return };
    let (Some(rx), Some(tx)) = (dev.queue(0, QUEUE_SIZE), dev.queue(1, QUEUE_SIZE)) else {
        crate::log!("virtio-net: out of memory");
        return;
    };
    let mut net = Net {
        rx,
        tx,
        rx_buffers: [0; QUEUE_SIZE as usize],
        tx_buffers: [0; QUEUE_SIZE as usize],
        rx_seen: 0,
        tx_seen: 0,
        tx_next: 0,
        mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
    };
    if !buffer_pages(&mut net.rx_buffers) || !buffer_pages(&mut net.tx_buffers) {
        crate::log!("virtio-net: out of memory");
        return;
    }
    if dev.features & F_MAC != 0 {
        for (i, byte) in net.mac.iter_mut().enumerate() {
            *byte = r8(dev.config, i);
        }
    }
    // Hand the card every receive buffer to fill.
    for i in 0..net.rx.size {
        let at = net.rx_buffers[i as usize];
        unsafe { net.rx.desc.add(i as usize).write(Descriptor { addr: at, len: BUFFER as u32, flags: WRITE, next: 0 }) };
        net.rx.offer(i);
    }
    dev.ready();
    net.rx.notify();
    let mac = net.mac;
    crate::log!(
        "virtio-net: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    *NET.lock() = Some(net);
    crate::net::init(mac);
}

pub fn present() -> bool {
    NET.lock().is_some()
}

/// Sends one Ethernet frame. False when there's no card, or nothing free to send with.
pub fn send(frame_bytes: &[u8]) -> bool {
    let mut guard = NET.lock();
    let Some(net) = guard.as_mut() else { return false };
    if frame_bytes.len() > BUFFER - HEADER {
        return false;
    }
    // Buffers the card has finished with are free again.
    net.tx_seen = net.tx.used_idx();
    let slot = net.tx_next % net.tx.size;
    net.tx_next = net.tx_next.wrapping_add(1);
    let at = net.tx_buffers[slot as usize];
    unsafe {
        let p = phys_to_virt(at);
        core::ptr::write_bytes(p, 0, HEADER);
        core::ptr::copy_nonoverlapping(frame_bytes.as_ptr(), p.add(HEADER), frame_bytes.len());
        net.tx.desc.add(slot as usize).write(Descriptor {
            addr: at,
            len: (HEADER + frame_bytes.len()) as u32,
            flags: 0,
            next: 0,
        });
    }
    net.tx.offer(slot);
    net.tx.notify();
    true
}

/// Collects what arrived and hands each frame to the stack (called from the timer interrupt).
pub fn poll() {
    loop {
        let mut copy = [0u8; BUFFER];
        let len;
        {
            let mut guard = NET.lock();
            let Some(net) = guard.as_mut() else { return };
            if net.rx.used_idx() == net.rx_seen {
                return;
            }
            let (desc, written) = net.rx.used_entry(net.rx_seen);
            net.rx_seen = net.rx_seen.wrapping_add(1);
            let bytes = (written as usize).saturating_sub(HEADER).min(BUFFER - HEADER);
            unsafe {
                let p = phys_to_virt(net.rx_buffers[desc as usize % net.rx_buffers.len()]);
                core::ptr::copy_nonoverlapping(p.add(HEADER), copy.as_mut_ptr(), bytes);
            }
            len = bytes;
            // The buffer goes straight back to the card.
            net.rx.offer(desc as u16);
            net.rx.notify();
        }
        // Outside the lock: the stack may want to send a reply.
        if len > 0 {
            crate::net::receive(&copy[..len]);
        }
    }
}

const _: () = assert!(BUFFER * 2 == PAGE_SIZE as usize);
