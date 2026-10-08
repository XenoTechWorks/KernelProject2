//! Physical page allocator: a stack of free 4 KiB pages, linked through the free pages themselves.

use super::{PAGE_SIZE, phys_to_virt};
use core::ops::Range;
use spin::Mutex;

struct FreeList {
    head: u64, // physical address of the first free page, 0 = none
    free: u64,
    total: u64,
}

static FRAMES: Mutex<FreeList> = Mutex::new(FreeList { head: 0, free: 0, total: 0 });

pub fn init(memmap: &[&limine::memmap::Entry], skip: Range<u64>) {
    let mut list = FRAMES.lock();
    for entry in memmap.iter().filter(|e| e.type_ == limine::memmap::MEMMAP_USABLE) {
        let start = entry.base.next_multiple_of(PAGE_SIZE);
        let end = (entry.base + entry.length) & !(PAGE_SIZE - 1);
        let mut page = start;
        while page < end {
            // Page 0 stays unused, so 0 can mean "no page".
            if page != 0 && !skip.contains(&page) {
                unsafe { *(phys_to_virt(page) as *mut u64) = list.head };
                list.head = page;
                list.free += 1;
                list.total += 1;
            }
            page += PAGE_SIZE;
        }
    }
}

/// A zeroed physical page, or None when memory is full.
pub fn alloc() -> Option<u64> {
    let page = {
        let mut list = FRAMES.lock();
        let page = list.head;
        if page == 0 {
            return None;
        }
        list.head = unsafe { *(phys_to_virt(page) as *const u64) };
        list.free -= 1;
        page
    };
    unsafe { core::ptr::write_bytes(phys_to_virt(page), 0, PAGE_SIZE as usize) };
    Some(page)
}

pub fn free(page: u64) {
    let mut list = FRAMES.lock();
    unsafe { *(phys_to_virt(page) as *mut u64) = list.head };
    list.head = page;
    list.free += 1;
}

/// (free pages, total pages)
pub fn stats() -> (u64, u64) {
    let list = FRAMES.lock();
    (list.free, list.total)
}
