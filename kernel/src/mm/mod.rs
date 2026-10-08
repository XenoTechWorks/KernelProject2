//! Memory: physical pages, page tables, the kernel heap and access to user memory.

pub mod frame;
pub mod paging;
pub mod user;

use core::sync::atomic::{AtomicU64, Ordering};
use linked_list_allocator::LockedHeap;

pub const PAGE_SIZE: u64 = 4096;

static HHDM: AtomicU64 = AtomicU64::new(0);

/// Limine maps all physical memory at a fixed offset (the "higher half direct map").
pub fn phys_to_virt(phys: u64) -> *mut u8 {
    (phys + HHDM.load(Ordering::Relaxed)) as *mut u8
}

#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();

const HEAP_SIZE: u64 = 64 * 1024 * 1024;

pub fn init(hhdm: u64, memmap: &[&limine::memmap::Entry]) {
    HHDM.store(hhdm, Ordering::Relaxed);
    // The heap is one contiguous piece carved out of the largest usable region; the rest of usable
    // memory goes to the page allocator.
    let largest = memmap
        .iter()
        .filter(|e| e.type_ == limine::memmap::MEMMAP_USABLE)
        .max_by_key(|e| e.length)
        .expect("no usable memory");
    assert!(largest.length > HEAP_SIZE * 2, "not enough memory for the kernel heap");
    let heap_base = largest.base;
    unsafe { HEAP.lock().init(phys_to_virt(heap_base), HEAP_SIZE as usize) };
    frame::init(memmap, heap_base..heap_base + HEAP_SIZE);
    paging::init();
}
