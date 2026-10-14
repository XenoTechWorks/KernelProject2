//! A file kept in memory pages rather than in the kernel's heap: what memfd_create makes. Mapping
//! it with MAP_SHARED maps these very pages (counted in mm/shared.rs), so every mapping, and
//! read and write on the file, see the same bytes. Mesa's Vulkan (lavapipe) keeps its memory this
//! way and maps it twice.

use crate::mm::{PAGE_SIZE, phys_to_virt, shared};
use crate::sys::errno::{Errno, EFBIG, ENOMEM};
use alloc::vec::Vec;

/// Bigger than this is refused (all of it is allocated when the size is set).
const MAX_SIZE: u64 = 1 << 32;

pub struct MemFile {
    pages: Vec<u64>,
    size: u64,
}

impl MemFile {
    pub fn new() -> MemFile {
        MemFile { pages: Vec::new(), size: 0 }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// The page holding byte `offset`'s page-sized block (for mapping it); None past the end.
    pub fn page(&self, offset: u64) -> Option<u64> {
        self.pages.get((offset / PAGE_SIZE) as usize).copied()
    }

    /// Makes it `len` bytes long: new pages are zero, and bytes cut off are gone (zeroed in the
    /// last page, which may still be mapped).
    pub fn truncate(&mut self, len: u64) -> Result<(), Errno> {
        if len > MAX_SIZE {
            return Err(EFBIG);
        }
        let want = len.div_ceil(PAGE_SIZE) as usize;
        while self.pages.len() < want {
            let page = shared::alloc().ok_or(ENOMEM)?;
            self.pages.push(page);
        }
        while self.pages.len() > want {
            shared::release(self.pages.pop().unwrap());
        }
        if len < self.size && len % PAGE_SIZE != 0 {
            let last = self.pages[want - 1];
            let from = (len % PAGE_SIZE) as usize;
            unsafe { core::ptr::write_bytes(phys_to_virt(last).add(from), 0, PAGE_SIZE as usize - from) };
        }
        self.size = len;
        Ok(())
    }

    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> usize {
        if offset >= self.size {
            return 0;
        }
        let len = buf.len().min((self.size - offset) as usize);
        self.each(offset, len, |page, within, at, n| unsafe {
            core::ptr::copy_nonoverlapping(phys_to_virt(page).add(within), buf[at..].as_mut_ptr(), n)
        });
        len
    }

    pub fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        let end = offset.checked_add(data.len() as u64).ok_or(EFBIG)?;
        if end > self.size {
            self.truncate(end)?;
        }
        self.each(offset, data.len(), |page, within, at, n| unsafe {
            core::ptr::copy_nonoverlapping(data[at..].as_ptr(), phys_to_virt(page).add(within), n)
        });
        Ok(data.len())
    }

    /// Calls `f(page, offset in it, offset in the caller's buffer, bytes)` for each page the
    /// range [offset, offset + len) touches.
    fn each(&self, offset: u64, len: usize, mut f: impl FnMut(u64, usize, usize, usize)) {
        let mut at = 0;
        while at < len {
            let pos = offset + at as u64;
            let within = (pos % PAGE_SIZE) as usize;
            let n = (PAGE_SIZE as usize - within).min(len - at);
            f(self.pages[(pos / PAGE_SIZE) as usize], within, at, n);
            at += n;
        }
    }
}

impl Drop for MemFile {
    fn drop(&mut self) {
        for &page in &self.pages {
            shared::release(page);
        }
    }
}
