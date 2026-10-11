//! ext2, Linux's classic file system, read and write, on the virtio disk.
//!
//! The disk is made by the real `mkfs.ext2`, so this follows the on-disk format exactly: a
//! superblock, block groups (each with a block bitmap, an inode bitmap and an inode table), inodes
//! with 12 direct and three levels of indirect block pointers, and directories as lists of
//! variable-length entries. Every change is written to the disk straight away, so nothing is lost
//! if the machine stops.

use crate::drivers::virtio_blk::{DISK, SECTOR};
use crate::sys::errno::*;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

pub const ROOT_INO: u32 = 2;

// Inode mode types.
const S_IFMT: u16 = 0o170000;
const S_IFDIR: u16 = 0o040000;
const S_IFREG: u16 = 0o100000;
const S_IFLNK: u16 = 0o120000;

const INDEX_FL: u32 = 0x1000; // hashed directory index (we keep directories linear and clear it)
const INCOMPAT_FILETYPE: u32 = 0x2;

pub struct Ext2 {
    inner: Mutex<Inner>,
}

struct Group {
    block_bitmap: u32,
    inode_bitmap: u32,
    inode_table: u32,
    free_blocks: u16,
    free_inodes: u16,
    used_dirs: u16,
}

struct Inner {
    sb: Vec<u8>,
    block_size: usize,
    inode_size: usize,
    inodes_per_group: u32,
    blocks_per_group: u32,
    first_data_block: u32,
    blocks_count: u32,
    filetype: bool,
    groups: Vec<Group>,
    cache: BTreeMap<u32, Vec<u8>>,
}

/// What `stat` needs from an inode.
pub struct Info {
    pub mode: u16,
    pub size: u64,
    pub mtime: u32,
    pub links: u16,
    pub blocks: u32,
}

/// The type of a new entry.
pub enum New<'a> {
    File(u16),
    Dir(u16),
    Symlink(&'a str),
}

fn get16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn disk_read(byte: u64, buf: &mut [u8]) -> Result<(), Errno> {
    DISK.lock().as_mut().ok_or(EIO)?.read(byte / SECTOR as u64, buf).map_err(|_| EIO)
}

fn disk_write(byte: u64, buf: &[u8]) -> Result<(), Errno> {
    DISK.lock().as_mut().ok_or(EIO)?.write(byte / SECTOR as u64, buf).map_err(|_| EIO)
}

fn now() -> u32 {
    crate::time::realtime().0 as u32
}

impl Ext2 {
    /// Reads the superblock and group descriptors. Fails if the disk isn't an ext2 we can handle.
    pub fn mount() -> Result<Arc<Ext2>, &'static str> {
        let mut sb = vec![0u8; 1024];
        disk_read(1024, &mut sb).map_err(|_| "can't read the disk")?;
        if get16(&sb, 56) != 0xEF53 {
            return Err("not an ext2 file system");
        }
        let incompat = get32(&sb, 96);
        if incompat & !INCOMPAT_FILETYPE != 0 {
            return Err("uses features this ext2 driver doesn't have (ext3/ext4?)");
        }
        let block_size = 1024usize << get32(&sb, 24);
        let rev = get32(&sb, 76);
        let inode_size = if rev >= 1 { get16(&sb, 88) as usize } else { 128 };
        let blocks_count = get32(&sb, 4);
        let first_data_block = get32(&sb, 20);
        let blocks_per_group = get32(&sb, 32);
        let group_count = (blocks_count - first_data_block).div_ceil(blocks_per_group) as usize;
        let mut inner = Inner {
            block_size,
            inode_size,
            inodes_per_group: get32(&sb, 40),
            blocks_per_group,
            first_data_block,
            blocks_count,
            filetype: incompat & INCOMPAT_FILETYPE != 0,
            sb,
            groups: Vec::new(),
            cache: BTreeMap::new(),
        };
        let table_bytes = group_count * 32;
        let mut table = vec![0u8; table_bytes.next_multiple_of(block_size)];
        let table_block = first_data_block + 1;
        disk_read(table_block as u64 * block_size as u64, &mut table).map_err(|_| "can't read the group table")?;
        for g in 0..group_count {
            let d = &table[g * 32..g * 32 + 32];
            inner.groups.push(Group {
                block_bitmap: get32(d, 0),
                inode_bitmap: get32(d, 4),
                inode_table: get32(d, 8),
                free_blocks: get16(d, 12),
                free_inodes: get16(d, 14),
                used_dirs: get16(d, 16),
            });
        }
        Ok(Arc::new(Ext2 { inner: Mutex::new(inner) }))
    }

    pub fn info(&self, ino: u32) -> Result<Info, Errno> {
        let mut fs = self.inner.lock();
        let raw = fs.read_inode(ino)?;
        Ok(Info {
            mode: get16(&raw, 0),
            size: get32(&raw, 4) as u64 | if get16(&raw, 0) & S_IFMT == S_IFREG { (get32(&raw, 108) as u64) << 32 } else { 0 },
            mtime: get32(&raw, 16),
            links: get16(&raw, 26),
            blocks: get32(&raw, 28),
        })
    }

    /// (total blocks, free blocks, block size) for statfs.
    pub fn usage(&self) -> (u64, u64, u64) {
        let fs = self.inner.lock();
        (fs.blocks_count as u64, get32(&fs.sb, 12) as u64, fs.block_size as u64)
    }

    pub fn read_at(&self, ino: u32, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut fs = self.inner.lock();
        let mut raw = fs.read_inode(ino)?;
        let size = get32(&raw, 4) as u64 | (get32(&raw, 108) as u64) << 32;
        if offset >= size {
            return Ok(0);
        }
        let len = buf.len().min((size - offset) as usize);
        let bs = fs.block_size;
        let mut done = 0;
        while done < len {
            let pos = offset as usize + done;
            let index = (pos / bs) as u32;
            let within = pos % bs;
            let n = (bs - within).min(len - done);
            match fs.block_of(&mut raw, index, false)? {
                Some(block) => {
                    let data = fs.read_block(block)?;
                    buf[done..done + n].copy_from_slice(&data[within..within + n]);
                }
                None => buf[done..done + n].fill(0), // a hole
            }
            done += n;
        }
        Ok(len)
    }

    pub fn write_at(&self, ino: u32, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        let mut fs = self.inner.lock();
        let mut raw = fs.read_inode(ino)?;
        let bs = fs.block_size;
        let mut done = 0;
        while done < data.len() {
            let pos = offset as usize + done;
            let index = (pos / bs) as u32;
            let within = pos % bs;
            let n = (bs - within).min(data.len() - done);
            let block = fs.block_of(&mut raw, index, true)?.ok_or(ENOSPC)?;
            let mut contents = if n == bs { vec![0; bs] } else { fs.read_block(block)? };
            contents[within..within + n].copy_from_slice(&data[done..done + n]);
            fs.write_block(block, &contents)?;
            done += n;
        }
        let end = offset + data.len() as u64;
        let size = get32(&raw, 4) as u64 | (get32(&raw, 108) as u64) << 32;
        if end > size {
            put32(&mut raw, 4, end as u32);
            put32(&mut raw, 108, (end >> 32) as u32);
        }
        put32(&mut raw, 16, now());
        fs.write_inode(ino, &raw)?;
        Ok(data.len())
    }

    pub fn truncate(&self, ino: u32, len: u64) -> Result<(), Errno> {
        let mut fs = self.inner.lock();
        let mut raw = fs.read_inode(ino)?;
        let bs = fs.block_size as u64;
        let keep = len.div_ceil(bs) as u32;
        fs.free_blocks_from(&mut raw, keep)?;
        // Zero the tail of the last kept block so later growth reads zeros.
        if len % bs != 0 {
            if let Some(block) = fs.block_of(&mut raw, (len / bs) as u32, false)? {
                let mut data = fs.read_block(block)?;
                data[(len % bs) as usize..].fill(0);
                fs.write_block(block, &data)?;
            }
        }
        put32(&mut raw, 4, len as u32);
        put32(&mut raw, 108, (len >> 32) as u32);
        put32(&mut raw, 16, now());
        fs.write_inode(ino, &raw)
    }

    /// The owner and group recorded in an inode.
    pub fn owner(&self, ino: u32) -> (u32, u32) {
        let mut fs = self.inner.lock();
        match fs.read_inode(ino) {
            Ok(raw) => (get16(&raw, 2) as u32, get16(&raw, 24) as u32),
            Err(_) => (0, 0),
        }
    }

    pub fn set_owner(&self, ino: u32, uid: u32, gid: u32) -> Result<(), Errno> {
        let mut fs = self.inner.lock();
        let mut raw = fs.read_inode(ino)?;
        put16(&mut raw, 2, uid as u16);
        put16(&mut raw, 24, gid as u16);
        fs.write_inode(ino, &raw)
    }

    pub fn set_mode(&self, ino: u32, mode: u32) -> Result<(), Errno> {
        let mut fs = self.inner.lock();
        let mut raw = fs.read_inode(ino)?;
        let kind = get16(&raw, 0) & S_IFMT;
        put16(&mut raw, 0, kind | (mode as u16 & 0o7777));
        fs.write_inode(ino, &raw)
    }

    pub fn readlink(&self, ino: u32) -> Result<String, Errno> {
        let info = self.info(ino)?;
        if info.mode & S_IFMT != S_IFLNK {
            return Err(EINVAL);
        }
        // Short targets are stored right in the block pointers ("fast symlinks").
        let raw = self.inner.lock().read_inode(ino)?;
        let bytes = if info.blocks == 0 {
            raw[40..40 + info.size as usize].to_vec()
        } else {
            let mut buf = vec![0; info.size as usize];
            self.read_at(ino, 0, &mut buf)?;
            buf
        };
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The entries of a directory: (name, inode, Linux d_type).
    pub fn list(&self, dir: u32) -> Result<Vec<(String, u32, u8)>, Errno> {
        let mut fs = self.inner.lock();
        Ok(fs.entries(dir)?.into_iter().map(|e| (e.name, e.ino, e.dtype)).collect())
    }

    pub fn lookup(&self, dir: u32, name: &str) -> Result<u32, Errno> {
        let mut fs = self.inner.lock();
        fs.entries(dir)?.into_iter().find(|e| e.name == name).map(|e| e.ino).ok_or(ENOENT)
    }

    /// Creates a file, directory or symlink in `dir`. Returns its inode number.
    pub fn create(&self, dir: u32, name: &str, new: New) -> Result<u32, Errno> {
        let mut fs = self.inner.lock();
        if fs.entries(dir)?.iter().any(|e| e.name == name) {
            return Err(EEXIST);
        }
        let is_dir = matches!(new, New::Dir(_));
        let ino = fs.alloc_inode(is_dir)?;
        let mut raw = vec![0u8; fs.inode_size];
        let t = now();
        let (mode, links) = match new {
            New::File(m) => (S_IFREG | (m & 0o7777), 1),
            New::Dir(m) => (S_IFDIR | (m & 0o7777), 2),
            New::Symlink(_) => (S_IFLNK | 0o777, 1),
        };
        put16(&mut raw, 0, mode);
        put32(&mut raw, 8, t);
        put32(&mut raw, 12, t);
        put32(&mut raw, 16, t);
        put16(&mut raw, 26, links);
        if fs.inode_size > 128 {
            put16(&mut raw, 128, 32); // i_extra_isize
        }
        match new {
            New::Symlink(target) if target.len() < 60 => {
                raw[40..40 + target.len()].copy_from_slice(target.as_bytes());
                put32(&mut raw, 4, target.len() as u32);
                fs.write_inode(ino, &raw)?;
            }
            New::Symlink(target) => {
                fs.write_inode(ino, &raw)?;
                drop(fs);
                self.write_at(ino, 0, target.as_bytes())?;
                fs = self.inner.lock();
            }
            New::Dir(_) => {
                fs.write_inode(ino, &raw)?;
                let bs = fs.block_size;
                let mut block = vec![0u8; bs];
                let dtype = if fs.filetype { 2 } else { 0 };
                write_entry(&mut block, 0, ino, 12, ".", dtype);
                write_entry(&mut block, 12, dir, (bs - 12) as u16, "..", dtype);
                let mut raw = fs.read_inode(ino)?;
                let b = fs.block_of(&mut raw, 0, true)?.ok_or(ENOSPC)?;
                fs.write_block(b, &block)?;
                put32(&mut raw, 4, bs as u32);
                fs.write_inode(ino, &raw)?;
                let mut parent = fs.read_inode(dir)?;
                let links = get16(&parent, 26) + 1;
                put16(&mut parent, 26, links);
                fs.write_inode(dir, &parent)?;
            }
            New::File(_) => fs.write_inode(ino, &raw)?,
        }
        let dtype = match mode & S_IFMT {
            S_IFDIR => 2,
            S_IFLNK => 7,
            _ => 1,
        };
        fs.add_entry(dir, name, ino, dtype)?;
        Ok(ino)
    }

    /// A hard link: another name in `dir` for inode `ino`.
    pub fn link(&self, dir: u32, name: &str, ino: u32) -> Result<(), Errno> {
        let mut fs = self.inner.lock();
        if fs.entries(dir)?.iter().any(|e| e.name == name) {
            return Err(EEXIST);
        }
        let mut raw = fs.read_inode(ino)?;
        if get16(&raw, 0) & S_IFMT == S_IFDIR {
            return Err(EPERM);
        }
        let links = get16(&raw, 26) + 1;
                put16(&mut raw, 26, links);
        fs.write_inode(ino, &raw)?;
        let dtype = if get16(&raw, 0) & S_IFMT == S_IFLNK { 7 } else { 1 };
        fs.add_entry(dir, name, ino, dtype)
    }

    /// Removes a name. The inode is freed when no names (links) are left.
    pub fn unlink(&self, dir: u32, name: &str, want_dir: bool) -> Result<(), Errno> {
        let mut fs = self.inner.lock();
        let entry = fs.entries(dir)?.into_iter().find(|e| e.name == name).ok_or(ENOENT)?;
        if name == "." || name == ".." {
            return Err(EINVAL);
        }
        let mut raw = fs.read_inode(entry.ino)?;
        let is_dir = get16(&raw, 0) & S_IFMT == S_IFDIR;
        match (is_dir, want_dir) {
            (true, false) => return Err(EISDIR),
            (false, true) => return Err(ENOTDIR),
            (true, true) => {
                if fs.entries(entry.ino)?.iter().any(|e| e.name != "." && e.name != "..") {
                    return Err(ENOTEMPTY);
                }
            }
            _ => {}
        }
        fs.remove_entry(dir, name)?;
        let links = if is_dir { 0 } else { get16(&raw, 26).saturating_sub(1) };
        put16(&mut raw, 26, links);
        if links == 0 {
            // A short symlink keeps its target text where block pointers would be: nothing to free.
            let fast_symlink = get16(&raw, 0) & S_IFMT == S_IFLNK && get32(&raw, 28) == 0;
            if !fast_symlink {
                fs.free_blocks_from(&mut raw, 0)?;
            }
            put32(&mut raw, 20, now()); // i_dtime
            put32(&mut raw, 4, 0);
            fs.write_inode(entry.ino, &raw)?;
            fs.free_inode(entry.ino, is_dir)?;
            if is_dir {
                let mut parent = fs.read_inode(dir)?;
                let links = get16(&parent, 26).saturating_sub(1);
                put16(&mut parent, 26, links);
                fs.write_inode(dir, &parent)?;
            }
        } else {
            fs.write_inode(entry.ino, &raw)?;
        }
        Ok(())
    }

    /// Moves a name, possibly to another directory, replacing a file already there.
    pub fn rename(&self, from_dir: u32, from: &str, to_dir: u32, to: &str) -> Result<(), Errno> {
        let ino = self.lookup(from_dir, from)?;
        let is_dir = self.info(ino)?.mode & S_IFMT == S_IFDIR;
        if let Ok(existing) = self.lookup(to_dir, to) {
            if existing == ino {
                return Ok(());
            }
            self.unlink(to_dir, to, self.info(existing)?.mode & S_IFMT == S_IFDIR)?;
        }
        let mut fs = self.inner.lock();
        let dtype = fs.entries(from_dir)?.into_iter().find(|e| e.name == from).map_or(0, |e| e.dtype);
        fs.add_entry(to_dir, to, ino, dtype)?;
        fs.remove_entry(from_dir, from)?;
        if is_dir && from_dir != to_dir {
            // Point the moved directory's ".." at its new parent and fix both parents' link counts.
            fs.set_dotdot(ino, to_dir)?;
            for (d, delta) in [(from_dir, -1i32), (to_dir, 1)] {
                let mut raw = fs.read_inode(d)?;
                let links = (get16(&raw, 26) as i32 + delta) as u16;
                put16(&mut raw, 26, links);
                fs.write_inode(d, &raw)?;
            }
        }
        Ok(())
    }
}

struct Entry {
    name: String,
    ino: u32,
    dtype: u8,
}

fn write_entry(block: &mut [u8], at: usize, ino: u32, rec_len: u16, name: &str, dtype: u8) {
    put32(block, at, ino);
    put16(block, at + 4, rec_len);
    block[at + 6] = name.len() as u8;
    block[at + 7] = dtype;
    block[at + 8..at + 8 + name.len()].copy_from_slice(name.as_bytes());
}

fn entry_size(name_len: usize) -> usize {
    (8 + name_len).next_multiple_of(4)
}

impl Inner {
    fn read_block(&mut self, block: u32) -> Result<Vec<u8>, Errno> {
        if let Some(data) = self.cache.get(&block) {
            return Ok(data.clone());
        }
        let mut data = vec![0u8; self.block_size];
        disk_read(block as u64 * self.block_size as u64, &mut data)?;
        if self.cache.len() > 4096 {
            self.cache.clear();
        }
        self.cache.insert(block, data.clone());
        Ok(data)
    }

    fn write_block(&mut self, block: u32, data: &[u8]) -> Result<(), Errno> {
        disk_write(block as u64 * self.block_size as u64, data)?;
        self.cache.insert(block, data.to_vec());
        Ok(())
    }

    fn inode_location(&self, ino: u32) -> Result<(u32, usize), Errno> {
        if ino == 0 {
            return Err(EIO);
        }
        let group = ((ino - 1) / self.inodes_per_group) as usize;
        let index = ((ino - 1) % self.inodes_per_group) as usize;
        let table = self.groups.get(group).ok_or(EIO)?.inode_table;
        let byte = index * self.inode_size;
        Ok((table + (byte / self.block_size) as u32, byte % self.block_size))
    }

    fn read_inode(&mut self, ino: u32) -> Result<Vec<u8>, Errno> {
        let (block, at) = self.inode_location(ino)?;
        Ok(self.read_block(block)?[at..at + self.inode_size].to_vec())
    }

    fn write_inode(&mut self, ino: u32, raw: &[u8]) -> Result<(), Errno> {
        let (block, at) = self.inode_location(ino)?;
        let mut data = self.read_block(block)?;
        data[at..at + self.inode_size].copy_from_slice(raw);
        self.write_block(block, &data)
    }

    fn pointers_per_block(&self) -> u32 {
        (self.block_size / 4) as u32
    }

    /// The disk block holding block `index` of a file, allocating it (and any indirect blocks on the
    /// way) when `allocate` is set. Changes to the inode's own pointers are made in `raw`; the caller
    /// writes the inode back.
    fn block_of(&mut self, raw: &mut [u8], index: u32, allocate: bool) -> Result<Option<u32>, Errno> {
        let ppb = self.pointers_per_block();
        // The path through the tree: (slot in i_block, then indexes in indirect blocks).
        let (slot, path): (usize, Vec<u32>) = if index < 12 {
            (index as usize, vec![])
        } else if index < 12 + ppb {
            (12, vec![index - 12])
        } else if index < 12 + ppb + ppb * ppb {
            let i = index - 12 - ppb;
            (13, vec![i / ppb, i % ppb])
        } else {
            let i = index - 12 - ppb - ppb * ppb;
            (14, vec![i / (ppb * ppb), i / ppb % ppb, i % ppb])
        };
        let mut block = get32(raw, 40 + slot * 4);
        if block == 0 {
            if !allocate {
                return Ok(None);
            }
            block = self.alloc_block(raw)?;
            put32(raw, 40 + slot * 4, block);
        }
        for i in path {
            let mut table = self.read_block(block)?;
            let mut next = get32(&table, i as usize * 4);
            if next == 0 {
                if !allocate {
                    return Ok(None);
                }
                next = self.alloc_block(raw)?;
                put32(&mut table, i as usize * 4, next);
                self.write_block(block, &table)?;
            }
            block = next;
        }
        Ok(Some(block))
    }

    /// Frees every block of the file from block `keep` on (and indirect blocks no longer needed).
    fn free_blocks_from(&mut self, raw: &mut [u8], keep: u32) -> Result<(), Errno> {
        let ppb = self.pointers_per_block();
        for slot in 0..12u32 {
            let b = get32(raw, 40 + slot as usize * 4);
            if slot >= keep && b != 0 {
                self.free_block(raw, b)?;
                put32(raw, 40 + slot as usize * 4, 0);
            }
        }
        // Indirect trees: (slot, first file block index it covers, depth).
        let trees = [(12usize, 12u32, 1u32), (13, 12 + ppb, 2), (14, 12 + ppb + ppb * ppb, 3)];
        for (slot, first, depth) in trees {
            let b = get32(raw, 40 + slot * 4);
            if b != 0 && self.free_tree(raw, b, first, depth, keep)? {
                put32(raw, 40 + slot * 4, 0);
            }
        }
        Ok(())
    }

    /// Frees the part of an indirect tree at or after file block `keep`. Returns true when the whole
    /// tree (including `block` itself) was freed.
    fn free_tree(&mut self, raw: &mut [u8], block: u32, first: u32, depth: u32, keep: u32) -> Result<bool, Errno> {
        let ppb = self.pointers_per_block();
        let span = ppb.pow(depth - 1);
        let mut table = self.read_block(block)?;
        let mut all_free = true;
        let mut changed = false;
        for i in 0..ppb {
            let child = get32(&table, i as usize * 4);
            let start = first + i * span;
            if child == 0 {
                continue;
            }
            let freed = if depth == 1 {
                if start >= keep {
                    self.free_block(raw, child)?;
                    true
                } else {
                    false
                }
            } else if start + span <= keep {
                false
            } else {
                self.free_tree(raw, child, start, depth - 1, keep)?
            };
            if freed {
                put32(&mut table, i as usize * 4, 0);
                changed = true;
            } else {
                all_free = false;
            }
        }
        if all_free {
            self.free_block(raw, block)?;
        } else if changed {
            self.write_block(block, &table)?;
        }
        Ok(all_free)
    }

    fn save_metadata(&mut self, group: usize) -> Result<(), Errno> {
        // Superblock (always at byte 1024) and this group's descriptor.
        let sb = self.sb.clone();
        disk_write(1024, &sb)?;
        let g = &self.groups[group];
        let table_block = self.first_data_block + 1 + (group * 32 / self.block_size) as u32;
        let at = group * 32 % self.block_size;
        let (bb, ib, it, fb, fi, ud) = (g.block_bitmap, g.inode_bitmap, g.inode_table, g.free_blocks, g.free_inodes, g.used_dirs);
        let mut data = self.read_block(table_block)?;
        put32(&mut data, at, bb);
        put32(&mut data, at + 4, ib);
        put32(&mut data, at + 8, it);
        put16(&mut data, at + 12, fb);
        put16(&mut data, at + 14, fi);
        put16(&mut data, at + 16, ud);
        self.write_block(table_block, &data)
    }

    /// Finds a clear bit in a bitmap block, sets it, and returns its index.
    fn take_bit(&mut self, bitmap: u32, limit: u32) -> Result<Option<u32>, Errno> {
        let mut data = self.read_block(bitmap)?;
        for i in 0..limit {
            let (byte, bit) = ((i / 8) as usize, i % 8);
            if data[byte] & (1 << bit) == 0 {
                data[byte] |= 1 << bit;
                self.write_block(bitmap, &data)?;
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    fn clear_bit(&mut self, bitmap: u32, i: u32) -> Result<(), Errno> {
        let mut data = self.read_block(bitmap)?;
        data[(i / 8) as usize] &= !(1 << (i % 8));
        self.write_block(bitmap, &data)
    }

    /// A zeroed new block. Counts it in the inode's i_blocks (512-byte units).
    fn alloc_block(&mut self, raw: &mut [u8]) -> Result<u32, Errno> {
        for g in 0..self.groups.len() {
            if self.groups[g].free_blocks == 0 {
                continue;
            }
            let in_group = self.blocks_per_group.min(self.blocks_count - self.first_data_block - g as u32 * self.blocks_per_group);
            if let Some(bit) = self.take_bit(self.groups[g].block_bitmap, in_group)? {
                let block = self.first_data_block + g as u32 * self.blocks_per_group + bit;
                self.groups[g].free_blocks -= 1;
                let free = get32(&self.sb, 12) - 1;
                put32(&mut self.sb, 12, free);
                self.save_metadata(g)?;
                let zero = vec![0u8; self.block_size];
                self.write_block(block, &zero)?;
                put32(raw, 28, get32(raw, 28) + (self.block_size / 512) as u32);
                return Ok(block);
            }
        }
        Err(ENOSPC)
    }

    fn free_block(&mut self, raw: &mut [u8], block: u32) -> Result<(), Errno> {
        // A pointer outside the disk means a damaged inode: refuse rather than free something random.
        if block < self.first_data_block || block >= self.blocks_count {
            crate::log!("ext2: bad block pointer {block} (i_block {:?})", &raw[40..100]);
            return Err(EIO);
        }
        let rel = block - self.first_data_block;
        let g = (rel / self.blocks_per_group) as usize;
        self.clear_bit(self.groups[g].block_bitmap, rel % self.blocks_per_group)?;
        self.groups[g].free_blocks += 1;
        let free = get32(&self.sb, 12) + 1;
        put32(&mut self.sb, 12, free);
        self.save_metadata(g)?;
        self.cache.remove(&block);
        put32(raw, 28, get32(raw, 28).saturating_sub((self.block_size / 512) as u32));
        Ok(())
    }

    fn alloc_inode(&mut self, dir: bool) -> Result<u32, Errno> {
        let first_ino = if get32(&self.sb, 76) >= 1 { get32(&self.sb, 84) } else { 11 };
        for g in 0..self.groups.len() {
            if self.groups[g].free_inodes == 0 {
                continue;
            }
            let mut data = self.read_block(self.groups[g].inode_bitmap)?;
            for i in 0..self.inodes_per_group {
                let ino = g as u32 * self.inodes_per_group + i + 1;
                let (byte, bit) = ((i / 8) as usize, i % 8);
                if ino < first_ino || data[byte] & (1 << bit) != 0 {
                    continue;
                }
                data[byte] |= 1 << bit;
                let bitmap = self.groups[g].inode_bitmap;
                self.write_block(bitmap, &data)?;
                self.groups[g].free_inodes -= 1;
                if dir {
                    self.groups[g].used_dirs += 1;
                }
                let free = get32(&self.sb, 16) - 1;
                put32(&mut self.sb, 16, free);
                self.save_metadata(g)?;
                return Ok(ino);
            }
        }
        Err(ENOSPC)
    }

    fn free_inode(&mut self, ino: u32, dir: bool) -> Result<(), Errno> {
        let g = ((ino - 1) / self.inodes_per_group) as usize;
        self.clear_bit(self.groups[g].inode_bitmap, (ino - 1) % self.inodes_per_group)?;
        self.groups[g].free_inodes += 1;
        if dir {
            self.groups[g].used_dirs = self.groups[g].used_dirs.saturating_sub(1);
        }
        let free = get32(&self.sb, 16) + 1;
        put32(&mut self.sb, 16, free);
        self.save_metadata(g)
    }

    fn dir_blocks(&mut self, dir: u32) -> Result<(Vec<u8>, u32), Errno> {
        let raw = self.read_inode(dir)?;
        if get16(&raw, 0) & S_IFMT != S_IFDIR {
            return Err(ENOTDIR);
        }
        let blocks = get32(&raw, 4) / self.block_size as u32;
        Ok((raw, blocks))
    }

    fn entries(&mut self, dir: u32) -> Result<Vec<Entry>, Errno> {
        let (mut raw, count) = self.dir_blocks(dir)?;
        let mut out = Vec::new();
        for index in 0..count {
            let Some(block) = self.block_of(&mut raw, index, false)? else { continue };
            let data = self.read_block(block)?;
            let mut at = 0;
            while at + 8 <= data.len() {
                let ino = get32(&data, at);
                let rec_len = get16(&data, at + 4) as usize;
                if rec_len < 8 || at + rec_len > data.len() {
                    break;
                }
                let name_len = data[at + 6] as usize;
                if ino != 0 && at + 8 + name_len <= data.len() {
                    let name = String::from_utf8_lossy(&data[at + 8..at + 8 + name_len]).into_owned();
                    let dtype = if self.filetype { data[at + 7] } else { 0 };
                    out.push(Entry { name, ino, dtype });
                }
                at += rec_len;
            }
        }
        Ok(out)
    }

    fn add_entry(&mut self, dir: u32, name: &str, ino: u32, dtype: u8) -> Result<(), Errno> {
        if name.is_empty() || name.len() > 255 {
            return Err(ENAMETOOLONG);
        }
        let dtype = if self.filetype { dtype } else { 0 };
        let need = entry_size(name.len());
        let (mut raw, count) = self.dir_blocks(dir)?;
        // Writing to a directory makes any hashed index stale; Linux's ext2 clears the flag too.
        if get32(&raw, 32) & INDEX_FL != 0 {
            let flags = get32(&raw, 32) & !INDEX_FL;
            put32(&mut raw, 32, flags);
        }
        for index in 0..count {
            let Some(block) = self.block_of(&mut raw, index, false)? else { continue };
            let mut data = self.read_block(block)?;
            let mut at = 0;
            while at + 8 <= data.len() {
                let entry_ino = get32(&data, at);
                let rec_len = get16(&data, at + 4) as usize;
                if rec_len < 8 {
                    break;
                }
                let used = if entry_ino == 0 { 0 } else { entry_size(data[at + 6] as usize) };
                if rec_len - used >= need {
                    if entry_ino == 0 {
                        write_entry(&mut data, at, ino, rec_len as u16, name, dtype);
                    } else {
                        put16(&mut data, at + 4, used as u16);
                        write_entry(&mut data, at + used, ino, (rec_len - used) as u16, name, dtype);
                    }
                    self.write_block(block, &data)?;
                    put32(&mut raw, 16, now());
                    return self.write_inode(dir, &raw);
                }
                at += rec_len;
            }
        }
        // No room: a new block for the directory.
        let bs = self.block_size;
        let block = self.block_of(&mut raw, count, true)?.ok_or(ENOSPC)?;
        let mut data = vec![0u8; bs];
        write_entry(&mut data, 0, ino, bs as u16, name, dtype);
        self.write_block(block, &data)?;
        put32(&mut raw, 4, (count + 1) * bs as u32);
        put32(&mut raw, 16, now());
        self.write_inode(dir, &raw)
    }

    fn remove_entry(&mut self, dir: u32, name: &str) -> Result<(), Errno> {
        let (mut raw, count) = self.dir_blocks(dir)?;
        for index in 0..count {
            let Some(block) = self.block_of(&mut raw, index, false)? else { continue };
            let mut data = self.read_block(block)?;
            let (mut at, mut prev) = (0usize, None::<usize>);
            while at + 8 <= data.len() {
                let rec_len = get16(&data, at + 4) as usize;
                if rec_len < 8 {
                    break;
                }
                let name_len = data[at + 6] as usize;
                if get32(&data, at) != 0 && &data[at + 8..at + 8 + name_len] == name.as_bytes() {
                    match prev {
                        // Fold the space into the previous entry, or mark the first entry unused.
                        Some(p) => {
                            let merged = get16(&data, p + 4) as usize + rec_len;
                            put16(&mut data, p + 4, merged as u16);
                        }
                        None => put32(&mut data, at, 0),
                    }
                    self.write_block(block, &data)?;
                    put32(&mut raw, 16, now());
                    return self.write_inode(dir, &raw);
                }
                prev = Some(at);
                at += rec_len;
            }
        }
        Err(ENOENT)
    }

    fn set_dotdot(&mut self, dir: u32, parent: u32) -> Result<(), Errno> {
        let (mut raw, _) = self.dir_blocks(dir)?;
        let block = self.block_of(&mut raw, 0, false)?.ok_or(EIO)?;
        let mut data = self.read_block(block)?;
        let first = get16(&data, 4) as usize;
        put32(&mut data, first, parent);
        self.write_block(block, &data)
    }
}
