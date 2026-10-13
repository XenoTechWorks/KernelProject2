//! A virtio input device used as a tablet (QEMU's `-device virtio-tablet-pci`): a pointing device
//! that reports where the pointer *is* rather than how far it moved, so the pointer on the screen
//! stays exactly under the host's mouse (a PS/2 mouse only reports movement, and drifts).
//!
//! The device sends Linux input events (type, code, value) into buffers we give it on queue 0,
//! ending each batch with a SYN event. We check for them on every timer tick.

use super::virtio::{self, Descriptor, Queue, WRITE, r32, w8};
use crate::mm::{frame, phys_to_virt};
use spin::Mutex;

const EVENTS: u16 = 64;

// Linux input event types and codes.
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const REL_WHEEL: u16 = 8;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;

struct Tablet {
    queue: Queue,
    last_used: u16,
    /// The page the events land in, 8 bytes each.
    events: *mut u8,
    /// The range each axis reports, from the device's configuration.
    range: [(u32, u32); 2],
    /// Where the pointer is (in the device's units), buttons held, wheel turned since the last SYN.
    at: [u32; 2],
    buttons: u8,
    wheel: i32,
    /// A position came since the last SYN (a click alone doesn't move the pointer: the PS/2 mouse
    /// may have moved it since).
    placed: bool,
}

unsafe impl Send for Tablet {}

static TABLET: Mutex<Option<Tablet>> = Mutex::new(None);

/// Sets up the virtio tablet, if there is one.
pub fn init() -> bool {
    let Some(mut dev) = virtio::start(&[0x1052], "virtio-input", 0) else { return false };
    let (Some(queue), Some(page)) = (dev.queue(0, EVENTS), frame::alloc()) else { return false };
    // Ask the device for each axis's range (select ABS_INFO, subselect the axis).
    let mut range = [(0, 32767); 2];
    for (axis, r) in range.iter_mut().enumerate() {
        w8(dev.config, 0, 0x12);
        w8(dev.config, 1, axis as u8);
        if virtio::r8(dev.config, 2) >= 8 {
            *r = (r32(dev.config, 8), r32(dev.config, 12));
        }
    }
    let mut tablet = Tablet { queue, last_used: 0, events: phys_to_virt(page), range, at: [0; 2], buttons: 0, wheel: 0, placed: false };
    // Give it every buffer.
    for i in 0..tablet.queue.size {
        unsafe { tablet.queue.desc.add(i as usize).write(Descriptor { addr: page + 8 * i as u64, len: 8, flags: WRITE, next: 0 }) };
        tablet.queue.offer(i);
    }
    dev.ready();
    tablet.queue.notify();
    crate::log!("virtio tablet: absolute pointer");
    *TABLET.lock() = Some(tablet);
    true
}

/// Handles the events that arrived (called from the timer interrupt).
pub fn poll() {
    let mut guard = TABLET.lock();
    let Some(t) = guard.as_mut() else { return };
    let mut moved = None;
    while t.queue.used_idx() != t.last_used {
        let (desc, _) = t.queue.used_entry(t.last_used);
        t.last_used = t.last_used.wrapping_add(1);
        let e = unsafe { t.events.add(8 * desc as usize) };
        let (kind, code, value) = unsafe { ((e as *const u16).read_volatile(), (e.add(2) as *const u16).read_volatile(), (e.add(4) as *const u32).read_volatile()) };
        match (kind, code) {
            (EV_ABS, ABS_X | ABS_Y) => {
                t.at[code as usize] = value;
                t.placed = true;
            }
            (EV_KEY, BTN_LEFT | BTN_RIGHT | BTN_MIDDLE) => {
                let bit = match code {
                    BTN_LEFT => 1,
                    BTN_RIGHT => 2,
                    _ => 4,
                };
                t.buttons = if value != 0 { t.buttons | bit } else { t.buttons & !bit };
            }
            (EV_REL, REL_WHEEL) => t.wheel -= value as i32, // up is positive here, down for us
            (EV_SYN, _) => moved = Some((core::mem::take(&mut t.placed).then_some(t.at), t.buttons, core::mem::take(&mut t.wheel))),
            _ => {}
        }
        // The buffer can take the next event.
        t.queue.offer(desc as u16);
    }
    if moved.is_some() {
        t.queue.notify();
    }
    let range = t.range;
    drop(guard);
    if let Some((at, buttons, wheel)) = moved {
        let (w, h) = crate::console::pixel_size();
        let scale = |v: u32, (min, max): (u32, u32), size: usize| {
            ((v.saturating_sub(min) as u64 * size as u64) / ((max - min) as u64 + 1)) as i32
        };
        let at = at.map(|at| (scale(at[0], range[0], w), scale(at[1], range[1], h)));
        crate::console::mouse_at(at, buttons, wheel);
    }
}
