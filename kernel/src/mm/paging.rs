//! Four-level x86-64 page tables. Each process gets its own lower half (user memory); the upper half
//! (kernel, heap, direct map) is shared by pointing every process at the kernel's upper-half tables.

use super::{PAGE_SIZE, frame, phys_to_virt};
use crate::arch::cpu;
use core::sync::atomic::{AtomicU64, Ordering};

pub const PRESENT: u64 = 1;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const HUGE: u64 = 1 << 7;
pub const NO_EXECUTE: u64 = 1 << 63;
/// One of the bits the processor leaves to us: the page is held by others too (mm/shared.rs), so
/// unmapping it lets go of it rather than freeing it.
pub const SHARED: u64 = 1 << 9;
const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;

/// Programs may use addresses below this; everything above belongs to the kernel.
pub const USER_END: u64 = 0x0000_8000_0000_0000;

static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);

/// Lets this CPU use the no-execute bit (each CPU, once).
pub fn init_cpu() {
    unsafe { cpu::wrmsr(cpu::MSR_EFER, cpu::rdmsr(cpu::MSR_EFER) | 1 << 11) };
}

/// The kernel's own top-level table (what the bootloader set up; every process shares its upper half).
pub fn kernel_pml4() -> u64 {
    KERNEL_PML4.load(Ordering::Relaxed)
}

/// Switches this CPU to the kernel's own tables (when it isn't running a task, so the tables of
/// one that ends can be freed).
pub fn activate_kernel() {
    unsafe { cpu::write_cr3(kernel_pml4()) }
}

pub fn init() {
    init_cpu(); // NX bit usable
    let pml4 = cpu::read_cr3() & ADDRESS_MASK;
    KERNEL_PML4.store(pml4, Ordering::Relaxed);
    // Make sure every upper-half slot has a table, so mappings the kernel adds later are seen by all
    // processes (they copy these slots once, when created).
    let table = table(pml4);
    for entry in &mut table[256..] {
        if *entry & PRESENT == 0 {
            *entry = frame::alloc().expect("out of memory") | PRESENT | WRITABLE;
        }
    }
}

/// Makes a page of device registers (memory-mapped I/O) reachable at phys_to_virt(phys), uncached.
pub fn map_mmio(phys: u64) -> *mut u8 {
    let va = super::phys_to_virt(phys) as u64;
    let kernel = AddressSpace { pml4: KERNEL_PML4.load(Ordering::Relaxed) };
    // Already covered (possibly by a large page of the direct map)?
    if kernel.translate(va).is_none() && !kernel.is_huge_mapped(va) {
        const CACHE_DISABLE: u64 = 1 << 4;
        const WRITE_THROUGH: u64 = 1 << 3;
        let e = kernel.entry(va, true).expect("out of memory");
        *e = (phys & ADDRESS_MASK) | PRESENT | WRITABLE | CACHE_DISABLE | WRITE_THROUGH | NO_EXECUTE;
        unsafe { core::arch::asm!("invlpg [{}]", in(reg) va) };
    }
    core::mem::forget(kernel); // it's the kernel's own tables: never free them
    va as *mut u8
}

/// Gives back the page a (present) page-table entry maps: freed, or let go of if it's shared.
fn drop_page(entry: u64) {
    if entry & SHARED != 0 {
        super::shared::release(entry & ADDRESS_MASK);
    } else {
        frame::free(entry & ADDRESS_MASK);
    }
}

fn table(phys: u64) -> &'static mut [u64; 512] {
    unsafe { &mut *(phys_to_virt(phys) as *mut [u64; 512]) }
}

fn index(va: u64, level: u32) -> usize {
    ((va >> (12 + 9 * level)) & 511) as usize
}

pub struct AddressSpace {
    pub pml4: u64,
}

impl AddressSpace {
    pub fn new() -> Option<Self> {
        let pml4 = frame::alloc()?;
        let kernel = table(KERNEL_PML4.load(Ordering::Relaxed));
        table(pml4)[256..].copy_from_slice(&kernel[256..]);
        Some(AddressSpace { pml4 })
    }

    pub fn activate(&self) {
        unsafe { cpu::write_cr3(self.pml4) }
    }

    /// The page table entry for `va`, creating the tables on the way when `create` is set.
    fn entry(&self, va: u64, create: bool) -> Option<&'static mut u64> {
        let mut t = table(self.pml4);
        for level in (1..4).rev() {
            let e = &mut t[index(va, level)];
            if *e & PRESENT == 0 {
                if !create {
                    return None;
                }
                *e = frame::alloc()? | PRESENT | WRITABLE | USER;
            }
            if *e & HUGE != 0 {
                return None;
            }
            t = table(*e & ADDRESS_MASK);
        }
        Some(&mut t[index(va, 0)])
    }

    /// Whether `va` sits inside a 2 MiB or 1 GiB page.
    fn is_huge_mapped(&self, va: u64) -> bool {
        let mut t = table(self.pml4);
        for level in (1..4).rev() {
            let e = t[index(va, level)];
            if e & PRESENT == 0 {
                return false;
            }
            if e & HUGE != 0 {
                return true;
            }
            t = table(e & ADDRESS_MASK);
        }
        false
    }

    /// Maps one page. Returns false when out of memory.
    pub fn map(&mut self, va: u64, phys: u64, flags: u64) -> bool {
        match self.entry(va, true) {
            Some(e) => {
                if *e & PRESENT != 0 {
                    drop_page(*e);
                }
                *e = phys | flags | PRESENT;
                true
            }
            None => false,
        }
    }

    /// Maps a fresh zeroed page at `va` if nothing is mapped there. Takes `&self` like `entry`: the
    /// page tables live outside this struct (for growing a stack while a system call reads it).
    pub fn map_new_page(&self, va: u64, flags: u64) -> bool {
        if self.translate(va).is_some() {
            return true;
        }
        let Some(page) = frame::alloc() else { return false };
        match self.entry(va & !(PAGE_SIZE - 1), true) {
            Some(e) => {
                *e = page | flags | PRESENT;
                true
            }
            None => {
                frame::free(page);
                false
            }
        }
    }

    /// Maps fresh zeroed pages over [start, end) that aren't mapped yet.
    pub fn map_zeroed(&mut self, start: u64, end: u64, flags: u64) -> bool {
        let mut va = start & !(PAGE_SIZE - 1);
        while va < end {
            if self.translate(va).is_none() {
                let Some(page) = frame::alloc() else { return false };
                if !self.map(va, page, flags) {
                    frame::free(page);
                    return false;
                }
            }
            va += PAGE_SIZE;
        }
        true
    }

    /// Removes and frees the pages in [start, end).
    pub fn unmap_range(&mut self, start: u64, end: u64) {
        let mut va = start & !(PAGE_SIZE - 1);
        while va < end {
            if let Some(e) = self.entry(va, false) {
                if *e & PRESENT != 0 {
                    drop_page(*e);
                    *e = 0;
                }
            }
            va += PAGE_SIZE;
        }
        self.flush();
    }

    pub fn protect_range(&mut self, start: u64, end: u64, flags: u64) {
        let mut va = start & !(PAGE_SIZE - 1);
        while va < end {
            if let Some(e) = self.entry(va, false) {
                if *e & PRESENT != 0 {
                    *e = (*e & ADDRESS_MASK) | (*e & SHARED) | flags | PRESENT;
                }
            }
            va += PAGE_SIZE;
        }
        self.flush();
    }

    /// (physical page, flags) for a mapped address.
    pub fn translate(&self, va: u64) -> Option<(u64, u64)> {
        let e = self.entry(va, false)?;
        if *e & PRESENT == 0 {
            return None;
        }
        Some((*e & ADDRESS_MASK, *e & !ADDRESS_MASK))
    }

    fn flush(&self) {
        if cpu::read_cr3() & ADDRESS_MASK == self.pml4 {
            unsafe { cpu::write_cr3(self.pml4) };
        }
    }

    /// Calls `f(va, entry)` for every mapped user page.
    fn walk(&self, mut f: impl FnMut(u64, u64)) {
        let l4 = table(self.pml4);
        for i4 in 0..256 {
            if l4[i4] & PRESENT == 0 {
                continue;
            }
            let l3 = table(l4[i4] & ADDRESS_MASK);
            for i3 in 0..512 {
                if l3[i3] & PRESENT == 0 {
                    continue;
                }
                let l2 = table(l3[i3] & ADDRESS_MASK);
                for i2 in 0..512 {
                    if l2[i2] & PRESENT == 0 {
                        continue;
                    }
                    let l1 = table(l2[i2] & ADDRESS_MASK);
                    for i1 in 0..512 {
                        if l1[i1] & PRESENT != 0 {
                            let va = (i4 as u64) << 39 | (i3 as u64) << 30 | (i2 as u64) << 21 | (i1 as u64) << 12;
                            f(va, l1[i1]);
                        }
                    }
                }
            }
        }
    }

    /// A full copy of the user half (for fork). Copies every page now; copy-on-write comes later.
    pub fn duplicate(&self) -> Option<AddressSpace> {
        let mut copy = AddressSpace::new()?;
        let mut ok = true;
        self.walk(|va, e| {
            if !ok {
                return;
            }
            // A shared page stays shared: the child maps the same one.
            if e & SHARED != 0 {
                super::shared::hold(e & ADDRESS_MASK);
                ok = copy.map(va, e & ADDRESS_MASK, e & !ADDRESS_MASK & !PRESENT);
                return;
            }
            let Some(page) = frame::alloc() else {
                ok = false;
                return;
            };
            unsafe {
                core::ptr::copy_nonoverlapping(phys_to_virt(e & ADDRESS_MASK), phys_to_virt(page), PAGE_SIZE as usize)
            };
            ok = copy.map(va, page, e & !ADDRESS_MASK & !PRESENT);
        });
        ok.then_some(copy)
    }

    /// Frees every user page and user page table.
    pub fn clear_user(&mut self) {
        self.walk(|_, e| drop_page(e));
        let l4 = table(self.pml4);
        for i4 in 0..256 {
            if l4[i4] & PRESENT == 0 {
                continue;
            }
            let l3 = table(l4[i4] & ADDRESS_MASK);
            for i3 in 0..512 {
                if l3[i3] & PRESENT == 0 {
                    continue;
                }
                let l2 = table(l3[i3] & ADDRESS_MASK);
                for i2 in 0..512 {
                    if l2[i2] & PRESENT != 0 {
                        frame::free(l2[i2] & ADDRESS_MASK);
                    }
                }
                frame::free(l3[i3] & ADDRESS_MASK);
            }
            frame::free(l4[i4] & ADDRESS_MASK);
            l4[i4] = 0;
        }
        self.flush();
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        if cpu::read_cr3() & ADDRESS_MASK == self.pml4 {
            unsafe { cpu::write_cr3(KERNEL_PML4.load(Ordering::Relaxed)) };
        }
        self.clear_user();
        frame::free(self.pml4);
    }
}
