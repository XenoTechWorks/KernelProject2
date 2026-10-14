//! Memory system calls: brk, mmap, munmap, mprotect.

use super::SysResult;
use super::errno::*;
use crate::mm::PAGE_SIZE;
use crate::mm::paging::{NO_EXECUTE, PRESENT, SHARED, USER, USER_END, WRITABLE};
use crate::proc::{self, fd::Handle};
use crate::fs::{Kind, Type};
use crate::mm::shared;

const MAP_SHARED: u32 = 0x01;
const MAP_SHARED_VALIDATE: u32 = 0x03;
const MAP_FIXED: u32 = 0x10;
const MAP_ANONYMOUS: u32 = 0x20;
const MAP_FIXED_NOREPLACE: u32 = 0x100000;

fn page_flags(prot: u32) -> u64 {
    if prot == 0 {
        return NO_EXECUTE; // PROT_NONE: mapped, but any access from the program faults
    }
    let mut flags = USER;
    if prot & 2 != 0 {
        flags |= WRITABLE;
    }
    if prot & 4 == 0 {
        flags |= NO_EXECUTE;
    }
    flags
}

pub fn brk(addr: u64) -> SysResult {
    let p = proc::current();
    if addr < p.mem().brk_start || addr >= proc::MMAP_TOP {
        return Ok(p.mem().brk);
    }
    let old_end = p.mem().brk.next_multiple_of(PAGE_SIZE);
    let new_end = addr.next_multiple_of(PAGE_SIZE);
    if new_end > old_end {
        if !p.space_mut().map_zeroed(old_end, new_end, USER | WRITABLE | NO_EXECUTE) {
            return Ok(p.mem().brk);
        }
    } else if new_end < old_end {
        p.space_mut().unmap_range(new_end, old_end);
    }
    p.mem().brk = addr;
    Ok(addr)
}

pub fn mmap(addr: u64, len: u64, prot: u32, flags: u32, fd: i32, offset: u64) -> SysResult {
    if len == 0 || offset % PAGE_SIZE != 0 {
        return Err(EINVAL);
    }
    let len = len.checked_next_multiple_of(PAGE_SIZE).ok_or(ENOMEM)?;
    let p = proc::current();

    // The file to map.
    let file_node = if flags & MAP_ANONYMOUS == 0 {
        let file = p.files().fds.get(fd)?;
        let Handle::Node(node) = &file.handle else { return Err(EACCES) };
        match node.kind_of() {
            Type::File => Some(node.clone()),
            Type::Device(_) => None, // /dev/zero
            _ => return Err(EACCES),
        }
    } else {
        None
    };

    let start = if flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0 {
        if addr % PAGE_SIZE != 0 {
            return Err(EINVAL);
        }
        if flags & MAP_FIXED_NOREPLACE != 0 && p.space().translate(addr).is_some() {
            return Err(EEXIST);
        }
        addr
    } else {
        let start = p.mem().mmap_next.checked_sub(len).ok_or(ENOMEM)?;
        if start < p.mem().brk + (64 << 20) {
            return Err(ENOMEM);
        }
        p.mem().mmap_next = start;
        start
    };
    let end = start.checked_add(len).filter(|&e| e <= USER_END).ok_or(ENOMEM)?;

    let space = p.space_mut();
    space.unmap_range(start, end);

    // MAP_SHARED (or MAP_SHARED_VALIDATE): the same pages as the file's (a memory file), or new
    // ones children made by fork keep sharing (anonymous). Other files are still copied in.
    if matches!(flags & 0xF, MAP_SHARED | MAP_SHARED_VALIDATE) {
        let memory = match &file_node {
            Some(node) => match &node.kind {
                Kind::Memory(m) => Some(m),
                _ => None,
            },
            None => None,
        };
        if memory.is_some() || flags & MAP_ANONYMOUS != 0 {
            let mut va = start;
            while va < end {
                let page = match memory {
                    Some(m) => match m.lock().page(offset + (va - start)) {
                        Some(page) => {
                            shared::hold(page);
                            Some(page)
                        }
                        None => None, // past the file's end: a page of zeros, its own
                    },
                    None => Some(shared::alloc().ok_or(ENOMEM)?),
                };
                let ok = match page {
                    Some(page) => space.map(va, page, page_flags(prot) | SHARED),
                    None => space.map_zeroed(va, va + PAGE_SIZE, page_flags(prot)),
                };
                if !ok {
                    space.unmap_range(start, end);
                    return Err(ENOMEM);
                }
                va += PAGE_SIZE;
            }
            return Ok(start);
        }
    }

    if !space.map_zeroed(start, end, page_flags(prot)) {
        space.unmap_range(start, end);
        return Err(ENOMEM);
    }
    if let Some(node) = file_node {
        // A piece at a time, straight into the new pages: a big library (LLVM is 140 MB) never
        // has to fit in the kernel's heap.
        let want = len.min(node.size().saturating_sub(offset)) as usize;
        let mut chunk = alloc::vec![0u8; want.min(128 << 10)];
        let mut done = 0;
        while done < want {
            let n = (want - done).min(chunk.len());
            let got = node.read_at(offset + done as u64, &mut chunk[..n])?;
            if got == 0 {
                break;
            }
            crate::mm::user::poke(space, start + done as u64, &chunk[..got])?;
            done += got;
        }
    }
    Ok(start)
}

/// Grows or shrinks a mapping, moving it when it has to (what a C library's allocator uses to
/// resize its heap). Shrinking frees the tail; growing finds room elsewhere and copies.
pub fn mremap(addr: u64, old_len: u64, new_len: u64, flags: u64) -> SysResult {
    const MREMAP_MAYMOVE: u64 = 1;
    if addr % PAGE_SIZE != 0 || new_len == 0 {
        return Err(EINVAL);
    }
    let old_len = old_len.checked_next_multiple_of(PAGE_SIZE).ok_or(ENOMEM)?;
    let new_len = new_len.checked_next_multiple_of(PAGE_SIZE).ok_or(ENOMEM)?;
    let p = proc::current();
    if new_len <= old_len {
        if new_len < old_len {
            p.space_mut().unmap_range(addr + new_len, addr + old_len);
        }
        return Ok(addr);
    }
    if flags & MREMAP_MAYMOVE == 0 {
        return Err(ENOMEM);
    }
    // Somewhere new, the way mmap picks a place.
    let start = p.mem().mmap_next.checked_sub(new_len).ok_or(ENOMEM)?;
    if start < p.mem().brk + (64 << 20) {
        return Err(ENOMEM);
    }
    p.mem().mmap_next = start;
    let end = start.checked_add(new_len).filter(|&e| e <= USER_END).ok_or(ENOMEM)?;
    let space = p.space_mut();
    if !space.map_zeroed(start, end, USER | WRITABLE | NO_EXECUTE) {
        space.unmap_range(start, end);
        return Err(ENOMEM);
    }
    let mut at = 0;
    while at < old_len {
        if let (Some((from, flags)), Some((to, _))) = (space.translate(addr + at), space.translate(start + at)) {
            if flags & SHARED != 0 {
                // Still the same page others hold, not a copy of it.
                crate::mm::shared::hold(from);
                space.map(start + at, from, flags & !PRESENT);
            } else {
                unsafe {
                    core::ptr::copy_nonoverlapping(crate::mm::phys_to_virt(from), crate::mm::phys_to_virt(to), PAGE_SIZE as usize)
                };
            }
        }
        at += PAGE_SIZE;
    }
    space.unmap_range(addr, addr + old_len);
    Ok(start)
}

/// mincore: which pages of a range are in memory. We never page anything out, so every page
/// that's mapped is there. Answering properly matters: a caller given a success but no answer
/// reads whatever was in the buffer.
pub fn mincore(addr: u64, len: u64, vec: u64) -> SysResult {
    if addr % PAGE_SIZE != 0 {
        return Err(EINVAL);
    }
    let pages = len.div_ceil(PAGE_SIZE);
    let p = proc::current();
    let mut out = alloc::vec![0u8; pages as usize];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = p.space().translate(addr + i as u64 * PAGE_SIZE).is_some() as u8;
    }
    if out.iter().any(|&resident| resident == 0) {
        return Err(ENOMEM); // the range has a hole in it, as Linux reports
    }
    crate::mm::user::write(p.space(), vec, &out)?;
    Ok(0)
}

pub fn munmap(addr: u64, len: u64) -> SysResult {
    if addr % PAGE_SIZE != 0 || addr.saturating_add(len) > USER_END {
        return Err(EINVAL);
    }
    proc::current().space_mut().unmap_range(addr, addr + len);
    Ok(0)
}

pub fn mprotect(addr: u64, len: u64, prot: u32) -> SysResult {
    if addr % PAGE_SIZE != 0 || addr.saturating_add(len) > USER_END {
        return Err(EINVAL);
    }
    proc::current().space_mut().protect_range(addr, addr + len, page_flags(prot));
    Ok(0)
}
