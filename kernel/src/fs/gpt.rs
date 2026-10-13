//! Reading GPT partition tables (what the installer writes): which partition has a given name.

use crate::drivers::virtio_blk;
use alloc::vec;

/// The partition named `name` on disk `disk`: its number (from 1) and first sector. None if the
/// disk has no GPT or no such partition.
pub fn find(disk: usize, name: &str) -> Option<(usize, u64)> {
    let mut header = [0u8; 512];
    virtio_blk::read(disk, 1, &mut header).ok()?;
    if &header[..8] != b"EFI PART" {
        return None;
    }
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    let u64_at = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    let entries_lba = u64_at(&header, 72);
    let count = u32_at(&header, 80).min(256) as usize;
    let size = u32_at(&header, 84) as usize;
    if !(128..=1024).contains(&size) {
        return None;
    }
    let mut table = vec![0u8; (count * size).next_multiple_of(512)];
    virtio_blk::read(disk, entries_lba, &mut table).ok()?;
    for i in 0..count {
        let entry = &table[i * size..(i + 1) * size];
        if entry[..16].iter().all(|&b| b == 0) {
            continue; // unused
        }
        // The name: 36 UTF-16LE code units from byte 56.
        let units = (0..36).map(|k| u16::from_le_bytes([entry[56 + 2 * k], entry[57 + 2 * k]])).take_while(|&u| u != 0);
        if units.eq(name.encode_utf16()) {
            return Some((i + 1, u64_at(entry, 32)));
        }
    }
    None
}
