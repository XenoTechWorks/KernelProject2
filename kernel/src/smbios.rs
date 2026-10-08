//! SMBIOS: the firmware's description of the machine (maker, model, BIOS). Linux shows it in
//! /sys/class/dmi/id, where programs like fastfetch read the "Host" line.

use crate::mm::paging::map_mmio;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Default)]
pub struct Dmi {
    pub fields: Vec<(&'static str, String)>,
}

/// Copies `len` bytes of physical memory (mapping the pages as needed).
fn read_phys(phys: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut at = phys;
    while out.len() < len {
        let page = at & !0xfff;
        let base = map_mmio(page);
        let offset = (at - page) as usize;
        let n = (4096 - offset).min(len - out.len());
        for i in 0..n {
            out.push(unsafe { base.add(offset + i).read_volatile() });
        }
        at += n as u64;
    }
    out
}

/// Reads the structures behind an SMBIOS entry point (given as a virtual address by Limine).
pub fn read(entry_32: *const u8, entry_64: *const u8) -> Dmi {
    let (table, length) = unsafe {
        if !entry_64.is_null() && core::slice::from_raw_parts(entry_64, 5) == b"_SM3_" {
            let max = (entry_64.add(0x0C) as *const u32).read_unaligned() as usize;
            ((entry_64.add(0x10) as *const u64).read_unaligned(), max)
        } else if !entry_32.is_null() && core::slice::from_raw_parts(entry_32, 4) == b"_SM_" {
            let len = (entry_32.add(0x16) as *const u16).read_unaligned() as usize;
            ((entry_32.add(0x18) as *const u32).read_unaligned() as u64, len)
        } else {
            return Dmi::default();
        }
    };
    if length == 0 || length > 1 << 20 {
        return Dmi::default();
    }
    let data = read_phys(table, length);
    let mut dmi = Dmi::default();
    let mut at = 0;
    while at + 4 <= data.len() {
        let kind = data[at];
        let len = data[at + 1] as usize;
        if kind == 127 || len < 4 || at + len > data.len() {
            break;
        }
        // Strings follow the formatted area, numbered from 1; the structure ends with a double NUL.
        let strings_start = at + len;
        let mut end = strings_start;
        while end + 1 < data.len() && !(data[end] == 0 && data[end + 1] == 0) {
            end += 1;
        }
        let strings: Vec<String> = data[strings_start..end]
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).trim().into())
            .collect();
        let next = end + 2;
        let string = |offset: usize| -> String {
            let index = data.get(at + offset).copied().unwrap_or(0) as usize;
            if offset < len && index > 0 { strings.get(index - 1).cloned().unwrap_or_default() } else { String::new() }
        };
        let fields: &[(&'static str, usize)] = match kind {
            0 => &[("bios_vendor", 4), ("bios_version", 5), ("bios_date", 8)],
            1 => &[("sys_vendor", 4), ("product_name", 5), ("product_version", 6), ("product_family", 0x1A)],
            2 => &[("board_vendor", 4), ("board_name", 5), ("board_version", 6)],
            3 => &[("chassis_vendor", 4)],
            _ => &[],
        };
        for &(name, offset) in fields {
            let value = string(offset);
            if !value.is_empty() && !dmi.fields.iter().any(|(n, _)| *n == name) {
                dmi.fields.push((name, value));
            }
        }
        at = next;
    }
    dmi
}
