//! Memory system calls: brk, mmap, munmap, mprotect.

use super::SysResult;
use super::errno::*;
use crate::mm::PAGE_SIZE;
use crate::mm::paging::{NO_EXECUTE, USER, USER_END, WRITABLE};
use crate::proc::{self, fd::Handle};
use crate::fs::Kind;

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
    if addr < p.brk_start || addr >= proc::MMAP_TOP {
        return Ok(p.brk);
    }
    let old_end = p.brk.next_multiple_of(PAGE_SIZE);
    let new_end = addr.next_multiple_of(PAGE_SIZE);
    if new_end > old_end {
        if !p.space_mut().map_zeroed(old_end, new_end, USER | WRITABLE | NO_EXECUTE) {
            return Ok(p.brk);
        }
    } else if new_end < old_end {
        p.space_mut().unmap_range(new_end, old_end);
    }
    p.brk = addr;
    Ok(addr)
}

pub fn mmap(addr: u64, len: u64, prot: u32, flags: u32, fd: i32, offset: u64) -> SysResult {
    if len == 0 || offset % PAGE_SIZE != 0 {
        return Err(EINVAL);
    }
    let len = len.checked_next_multiple_of(PAGE_SIZE).ok_or(ENOMEM)?;
    let p = proc::current();

    // The file contents, read up front (mappings are private copies for now).
    let file_data = if flags & MAP_ANONYMOUS == 0 {
        let file = p.fds.get(fd)?;
        let Handle::Node(node) = &file.handle else { return Err(EACCES) };
        match &node.kind {
            Kind::File(data) => {
                let data = data.lock();
                let start = (offset as usize).min(data.len());
                let end = (offset as usize).saturating_add(len as usize).min(data.len());
                Some(data[start..end].to_vec())
            }
            Kind::Device(_) => None, // /dev/zero
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
        let start = p.mmap_next.checked_sub(len).ok_or(ENOMEM)?;
        if start < p.brk + (64 << 20) {
            return Err(ENOMEM);
        }
        p.mmap_next = start;
        start
    };
    let end = start.checked_add(len).filter(|&e| e <= USER_END).ok_or(ENOMEM)?;

    let space = p.space_mut();
    space.unmap_range(start, end);
    if !space.map_zeroed(start, end, page_flags(prot)) {
        space.unmap_range(start, end);
        return Err(ENOMEM);
    }
    if let Some(data) = file_data {
        crate::mm::user::poke(space, start, &data)?;
    }
    Ok(start)
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
