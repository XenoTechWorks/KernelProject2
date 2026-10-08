//! Reading and writing a program's memory from a system call. Every address a program passes in is
//! checked page by page against its page tables first, so a bad pointer returns EFAULT instead of
//! crashing the kernel.

use super::paging::{AddressSpace, USER, USER_END, WRITABLE};
use super::{PAGE_SIZE, phys_to_virt};
use crate::sys::errno::{EFAULT, Errno};
use alloc::string::String;
use alloc::vec::Vec;

fn each_chunk(
    space: &AddressSpace,
    addr: u64,
    len: usize,
    write: bool,
    mut f: impl FnMut(*mut u8, usize, usize),
) -> Result<(), Errno> {
    if addr.checked_add(len as u64).is_none_or(|end| end > USER_END) {
        return Err(EFAULT);
    }
    let mut done = 0;
    while done < len {
        let va = addr + done as u64;
        let (page, flags) = space.translate(va).ok_or(EFAULT)?;
        if flags & USER == 0 || (write && flags & WRITABLE == 0) {
            return Err(EFAULT);
        }
        let offset = (va % PAGE_SIZE) as usize;
        let n = (PAGE_SIZE as usize - offset).min(len - done);
        f(unsafe { phys_to_virt(page).add(offset) }, done, n);
        done += n;
    }
    Ok(())
}

pub fn read(space: &AddressSpace, addr: u64, out: &mut [u8]) -> Result<(), Errno> {
    let len = out.len();
    each_chunk(space, addr, len, false, |src, at, n| unsafe {
        core::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(at), n)
    })
}

pub fn read_vec(space: &AddressSpace, addr: u64, len: usize) -> Result<Vec<u8>, Errno> {
    let mut v = alloc::vec![0; len];
    read(space, addr, &mut v)?;
    Ok(v)
}

pub fn write(space: &AddressSpace, addr: u64, data: &[u8]) -> Result<(), Errno> {
    each_chunk(space, addr, data.len(), true, |dst, at, n| unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr().add(at), dst, n)
    })
}

pub fn read_u64(space: &AddressSpace, addr: u64) -> Result<u64, Errno> {
    let mut b = [0; 8];
    read(space, addr, &mut b)?;
    Ok(u64::from_le_bytes(b))
}

pub fn write_struct<T: Copy>(space: &AddressSpace, addr: u64, value: &T) -> Result<(), Errno> {
    let bytes = unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) };
    write(space, addr, bytes)
}

pub fn read_struct<T: Copy + Default>(space: &AddressSpace, addr: u64) -> Result<T, Errno> {
    let mut value = T::default();
    let bytes = unsafe { core::slice::from_raw_parts_mut(&mut value as *mut T as *mut u8, size_of::<T>()) };
    read(space, addr, bytes)?;
    Ok(value)
}

/// A NUL-terminated string, at most `max` bytes long.
pub fn read_cstr(space: &AddressSpace, addr: u64, max: usize) -> Result<String, Errno> {
    let mut out = Vec::new();
    let mut va = addr;
    loop {
        let mut b = [0u8; 1];
        read(space, va, &mut b)?;
        if b[0] == 0 {
            break;
        }
        out.push(b[0]);
        if out.len() >= max {
            return Err(crate::sys::errno::ENAMETOOLONG);
        }
        va += 1;
    }
    String::from_utf8(out).map_err(|_| crate::sys::errno::EINVAL)
}

/// A NULL-terminated array of string pointers (argv, envp).
pub fn read_cstr_array(space: &AddressSpace, addr: u64) -> Result<Vec<String>, Errno> {
    let mut out = Vec::new();
    if addr == 0 {
        return Ok(out);
    }
    loop {
        let p = read_u64(space, addr + out.len() as u64 * 8)?;
        if p == 0 {
            return Ok(out);
        }
        out.push(read_cstr(space, p, 128 * 1024)?);
        if out.len() > 4096 {
            return Err(crate::sys::errno::E2BIG);
        }
    }
}

/// Writes into a program's memory regardless of page permissions (loading programs, file mappings).
pub fn poke(space: &AddressSpace, addr: u64, data: &[u8]) -> Result<(), Errno> {
    if addr.checked_add(data.len() as u64).is_none_or(|end| end > USER_END) {
        return Err(EFAULT);
    }
    let mut done = 0;
    while done < data.len() {
        let va = addr + done as u64;
        let (page, _) = space.translate(va).ok_or(EFAULT)?;
        let offset = (va % PAGE_SIZE) as usize;
        let n = (PAGE_SIZE as usize - offset).min(data.len() - done);
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(done), phys_to_virt(page).add(offset), n) };
        done += n;
    }
    Ok(())
}
