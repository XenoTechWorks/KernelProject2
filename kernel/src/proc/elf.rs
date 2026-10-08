//! Loading programs: Linux x86-64 ELF executables (static, static-PIE, or dynamic through their
//! interpreter such as /lib64/ld-linux-x86-64.so.2) and `#!` scripts. The starting stack (argc, argv,
//! environment, auxiliary vector) is laid out exactly as Linux does it, so unmodified programs start.

use crate::fs::{self, Type};
use crate::mm::paging::{AddressSpace, NO_EXECUTE, USER, WRITABLE};
use crate::mm::PAGE_SIZE;
use crate::mm::user::poke;
use crate::sys::errno::*;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

pub struct Image {
    pub space: AddressSpace,
    pub entry: u64,
    pub stack: u64,
    pub brk: u64,
    pub path: String,
}

const PROGRAM_BASE: u64 = 0x0000_5555_5560_0000; // for position-independent executables
const INTERP_BASE: u64 = 0x0000_7fff_f7a0_0000;

/// Runs Windows programs (windows/ in the source tree).
const WINDOWS_LOADER: &str = "/bin/winload";

const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;

fn u16_at(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}
fn u32_at(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}
fn u64_at(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(d[at..at + 8].try_into().unwrap())
}

pub fn short_name(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.chars().take(15).collect()
}

/// The contents of an executable file, checking it exists and is a regular file.
fn read_file(path: &str) -> Result<Arc<fs::Node>, Errno> {
    let node = fs::lookup(path, true)?;
    if node.kind_of() != Type::File || node.mode_bits() & 0o111 == 0 {
        return Err(EACCES);
    }
    Ok(node)
}

struct Loaded {
    entry: u64,
    phdr: u64,
    phnum: u64,
    end: u64,
    interp: Option<String>,
    base: u64,
}

fn load_segments(space: &mut AddressSpace, data: &[u8], base_hint: u64) -> Result<Loaded, Errno> {
    if data.len() < 64 || &data[..4] != b"\x7fELF" || data[4] != 2 || data[5] != 1 || u16_at(data, 18) != 62 {
        return Err(ENOEXEC);
    }
    let kind = u16_at(data, 16);
    let base = match kind {
        2 => 0,
        3 => base_hint,
        _ => return Err(ENOEXEC),
    };
    let entry = u64_at(data, 24);
    let phoff = u64_at(data, 32) as usize;
    let phentsize = u16_at(data, 54) as usize;
    let phnum = u16_at(data, 56) as usize;
    if phentsize < 56 || phoff + phentsize * phnum > data.len() {
        return Err(ENOEXEC);
    }

    let mut phdr = 0;
    let mut end = 0;
    let mut interp = None;
    for i in 0..phnum {
        let h = phoff + i * phentsize;
        let kind = u32_at(data, h);
        let flags = u32_at(data, h + 4);
        let offset = u64_at(data, h + 8) as usize;
        let vaddr = u64_at(data, h + 16) + base;
        let filesz = u64_at(data, h + 32) as usize;
        let memsz = u64_at(data, h + 40);
        match kind {
            PT_LOAD => {
                if offset + filesz > data.len() || memsz < filesz as u64 {
                    return Err(ENOEXEC);
                }
                if offset <= phoff && phoff < offset + filesz {
                    phdr = vaddr + (phoff - offset) as u64;
                }
                let mut page_flags = USER;
                if flags & 2 != 0 {
                    page_flags |= WRITABLE;
                }
                if flags & 1 == 0 {
                    page_flags |= NO_EXECUTE;
                }
                map_merged(space, vaddr, vaddr + memsz, page_flags)?;
                poke(space, vaddr, &data[offset..offset + filesz])?;
                end = end.max(vaddr + memsz);
            }
            PT_INTERP => {
                let raw = &data[offset..offset + filesz];
                let raw = raw.split(|&b| b == 0).next().unwrap_or(raw);
                interp = Some(String::from_utf8_lossy(raw).into_owned());
            }
            PT_PHDR => phdr = vaddr,
            _ => {}
        }
    }
    Ok(Loaded { entry: entry + base, phdr, phnum: phnum as u64, end, interp, base })
}

/// Maps [start, end); pages two segments share get the permissions of both.
fn map_merged(space: &mut AddressSpace, start: u64, end: u64, flags: u64) -> Result<(), Errno> {
    let mut va = start & !(PAGE_SIZE - 1);
    while va < end {
        match space.translate(va) {
            Some((_, old)) => {
                let mut merged = (old | flags) & !NO_EXECUTE & 0xfff;
                if old & NO_EXECUTE != 0 && flags & NO_EXECUTE != 0 {
                    merged |= NO_EXECUTE;
                }
                space.protect_range(va, va + PAGE_SIZE, merged);
            }
            None => {
                let page = crate::mm::frame::alloc().ok_or(ENOMEM)?;
                if !space.map(va, page, flags) {
                    return Err(ENOMEM);
                }
            }
        }
        va += PAGE_SIZE;
    }
    Ok(())
}

pub fn load(path: &str, args: &[String], env: &[String]) -> Result<Image, Errno> {
    load_depth(path, args, env, 0)
}

fn load_depth(path: &str, args: &[String], env: &[String], depth: u32) -> Result<Image, Errno> {
    let node = read_file(path)?;
    let data = node.read_all()?;

    // Windows programs ("MZ" executables) run through the Windows layer, like a script through
    // its interpreter: `winload program.exe arguments...`.
    if data.starts_with(b"MZ") {
        if depth > 0 {
            return Err(ENOEXEC);
        }
        let mut new_args = alloc::vec![String::from(WINDOWS_LOADER), String::from(path)];
        new_args.extend(args.iter().skip(1).cloned());
        return load_depth(WINDOWS_LOADER, &new_args, env, depth + 1);
    }

    // "#!/bin/sh -e" scripts run their interpreter with the script's path as an argument.
    if data.starts_with(b"#!") {
        if depth > 4 {
            return Err(ELOOP);
        }
        let line_end = data.iter().position(|&b| b == b'\n').unwrap_or(data.len()).min(256);
        let line = String::from_utf8_lossy(&data[2..line_end]).into_owned();
        let mut parts = line.trim().splitn(2, [' ', '\t']);
        let interp = parts.next().filter(|s| !s.is_empty()).ok_or(ENOEXEC)?;
        let mut new_args = Vec::new();
        new_args.push(String::from(interp));
        if let Some(arg) = parts.next().map(str::trim).filter(|s| !s.is_empty()) {
            new_args.push(String::from(arg));
        }
        new_args.push(String::from(path));
        new_args.extend(args.iter().skip(1).cloned());
        return load_depth(interp, &new_args, env, depth + 1);
    }

    let mut space = AddressSpace::new().ok_or(ENOMEM)?;
    let program = load_segments(&mut space, &data, PROGRAM_BASE)?;
    let mut entry = program.entry;
    let mut interp_base = 0;
    if let Some(interp_path) = &program.interp {
        let interp_node = fs::lookup(interp_path, true).map_err(|_| ENOENT)?;
        if interp_node.kind_of() != Type::File {
            return Err(ENOEXEC);
        }
        let interp_data = interp_node.read_all()?;
        let interp = load_segments(&mut space, &interp_data, INTERP_BASE)?;
        if interp.interp.is_some() {
            return Err(ELIBBAD);
        }
        entry = interp.entry;
        interp_base = interp.base;
    }
    let brk = program.end.next_multiple_of(PAGE_SIZE);
    let stack = build_stack(&mut space, path, args, env, &program, interp_base)?;
    Ok(Image { space, entry, stack, brk, path: String::from(path) })
}

fn build_stack(space: &mut AddressSpace, path: &str, args: &[String], env: &[String], program: &Loaded, interp_base: u64) -> Result<u64, Errno> {
    use super::{STACK_TOP, STACK_LIMIT};
    // Map the top 256 KiB now; the page fault handler grows it further on demand.
    let initial = 256 * 1024;
    if !space.map_zeroed(STACK_TOP - initial, STACK_TOP, USER | WRITABLE | NO_EXECUTE) {
        return Err(ENOMEM);
    }
    let strings_size: usize = args.iter().chain(env).map(|s| s.len() + 1).sum::<usize>() + path.len() + 1 + 7 + 16;
    if strings_size > initial as usize / 2 || STACK_LIMIT < initial {
        return Err(E2BIG);
    }

    // Strings at the very top, then the pointer arrays below them.
    let mut sp = STACK_TOP;
    fn push(space: &AddressSpace, sp: &mut u64, bytes: &[u8]) -> Result<u64, Errno> {
        *sp -= bytes.len() as u64;
        poke(space, *sp, bytes)?;
        Ok(*sp)
    }
    fn push_str(space: &AddressSpace, sp: &mut u64, s: &str) -> Result<u64, Errno> {
        let mut v = Vec::with_capacity(s.len() + 1);
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        push(space, sp, &v)
    }
    let execfn = push_str(space, &mut sp, path)?;
    let platform = push_str(space, &mut sp, "x86_64")?;
    let mut arg_ptrs = Vec::new();
    for a in args {
        arg_ptrs.push(push_str(space, &mut sp, a)?);
    }
    let mut env_ptrs = Vec::new();
    for e in env {
        env_ptrs.push(push_str(space, &mut sp, e)?);
    }
    let mut random_bytes = [0u8; 16];
    crate::sys::misc::fill_random(&mut random_bytes);
    let random = push(space, &mut sp, &random_bytes)?;

    let auxv: [(u64, u64); 18] = [
        (3, program.phdr),        // AT_PHDR
        (4, 56),                  // AT_PHENT
        (5, program.phnum),       // AT_PHNUM
        (6, PAGE_SIZE),           // AT_PAGESZ
        (7, interp_base),         // AT_BASE
        (8, 0),                   // AT_FLAGS
        (9, program.entry),       // AT_ENTRY
        (11, 0),                  // AT_UID
        (12, 0),                  // AT_EUID
        (13, 0),                  // AT_GID
        (14, 0),                  // AT_EGID
        (15, platform),           // AT_PLATFORM
        (16, 0x178bfbff),         // AT_HWCAP (cpuid 1 edx of a typical x86-64)
        (17, 100),                // AT_CLKTCK
        (23, 0),                  // AT_SECURE
        (25, random),             // AT_RANDOM
        (31, execfn),             // AT_EXECFN
        (0, 0),                   // AT_NULL
    ];

    let words = 1 + arg_ptrs.len() + 1 + env_ptrs.len() + 1 + auxv.len() * 2;
    sp &= !15;
    if words % 2 == 1 {
        sp -= 8; // keep the final stack pointer 16-byte aligned at argc
    }
    sp -= words as u64 * 8;
    let mut table: Vec<u64> = Vec::with_capacity(words);
    table.push(args.len() as u64);
    table.extend(&arg_ptrs);
    table.push(0);
    table.extend(&env_ptrs);
    table.push(0);
    for (k, v) in auxv {
        table.push(k);
        table.push(v);
    }
    let bytes: Vec<u8> = table.iter().flat_map(|w| w.to_le_bytes()).collect();
    poke(space, sp, &bytes)?;
    Ok(sp)
}
