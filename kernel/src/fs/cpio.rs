//! The boot archive: a "newc" cpio file, the same format Linux uses for its initramfs.

use super::{Kind, Node, S_IFDIR, S_IFLNK, S_IFMT_MASK, S_IFREG};
use alloc::string::String;

fn hex(field: &[u8]) -> usize {
    field.iter().fold(0, |n, &c| n * 16 + (c as char).to_digit(16).unwrap_or(0) as usize)
}

/// Unpacks every entry into the file tree. Returns how many entries there were.
pub fn unpack(archive: &[u8]) -> usize {
    let mut at = 0;
    let mut count = 0;
    while at + 110 <= archive.len() {
        let header = &archive[at..at + 110];
        if &header[..6] != b"070701" {
            crate::log!("initramfs: bad header at {at}");
            break;
        }
        let mode = hex(&header[14..22]) as u32;
        let size = hex(&header[54..62]);
        let name_len = hex(&header[94..102]);
        let name_start = at + 110;
        let name = &archive[name_start..name_start + name_len - 1];
        let data_start = (name_start + name_len).next_multiple_of(4);
        let data = &archive[data_start..data_start + size];
        at = (data_start + size).next_multiple_of(4);

        if name == b"TRAILER!!!" {
            break;
        }
        let name = String::from_utf8_lossy(name);
        let path = super::normalize("/", &name);
        if path == "/" {
            continue;
        }
        let node = match mode & S_IFMT_MASK {
            S_IFDIR => {
                let dir = super::mkdir_p(&path);
                *dir.mode.lock() = mode & 0o7777;
                count += 1;
                continue;
            }
            S_IFREG => Node::file(data.into(), mode),
            S_IFLNK => Node::new(Kind::Symlink(String::from_utf8_lossy(data).into()), mode),
            _ => continue,
        };
        super::install(&path, node);
        count += 1;
    }
    count
}
