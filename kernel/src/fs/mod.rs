//! The file tree. Like Linux's VFS, every file, whatever file system it lives on, answers the same
//! questions (read, write, list, look up a name, create, remove), so the system calls don't care
//! where a file is. File systems:
//!
//! - an in-memory one (like tmpfs) for `/`, filled from the boot archive;
//! - ext2 on a disk, mounted at `/root` when a disk is attached (see `mount_disk`);
//! - `/proc`, made up on each lookup; device files; and pipes (in `pipe`).
//!
//! Paths are resolved as text: the current directory is an absolute path, and "." / ".." / symlinks
//! are worked out before walking the tree.

pub mod cpio;
pub mod ext2;
pub mod pipe;
pub mod procfs;
pub mod sysfs;

use crate::sys::errno::*;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use ext2::Ext2;
use spin::{Mutex, Once};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Device {
    Tty,
    Null,
    Zero,
    Random,
}

pub enum Kind {
    Dir(Mutex<BTreeMap<String, Arc<Node>>>),
    File(Mutex<Vec<u8>>),
    Symlink(String),
    Device(Device),
    /// An inode on an ext2 disk (a file, directory or symlink).
    Ext2(Arc<Ext2>, u32),
}

/// What kind of thing a node is, whatever file system it's on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Type {
    Dir,
    File,
    Symlink,
    Device(Device),
}

/// What to create in a directory.
pub enum New<'a> {
    File(u32),
    Dir(u32),
    Symlink(&'a str),
}

pub struct Node {
    ino: u64,
    mode: Mutex<u32>, // permission bits, for in-memory nodes
    pub kind: Kind,
    mtime: AtomicU64,
}

pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFMT_MASK: u32 = 0o170000;

/// Device numbers reported by stat: the in-memory file system and the first disk (like /dev/vda).
const DEV_MEMORY: u64 = 1;
const DEV_DISK: u64 = (253 << 8) | 0;

static NEXT_INO: AtomicU64 = AtomicU64::new(1);

impl Node {
    pub fn new(kind: Kind, mode: u32) -> Arc<Node> {
        Arc::new(Node {
            ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
            mode: Mutex::new(mode & 0o7777),
            kind,
            mtime: AtomicU64::new(crate::time::realtime().0),
        })
    }

    pub fn dir() -> Arc<Node> {
        Node::new(Kind::Dir(Mutex::new(BTreeMap::new())), 0o755)
    }

    pub fn file(data: Vec<u8>, mode: u32) -> Arc<Node> {
        Node::new(Kind::File(Mutex::new(data)), mode)
    }

    pub fn ext2(fs: Arc<Ext2>, ino: u32) -> Arc<Node> {
        Arc::new(Node { ino: ino as u64, mode: Mutex::new(0), kind: Kind::Ext2(fs, ino), mtime: AtomicU64::new(0) })
    }

    fn ext2_info(&self) -> Option<ext2::Info> {
        match &self.kind {
            Kind::Ext2(fs, ino) => fs.info(*ino).ok(),
            _ => None,
        }
    }

    pub fn kind_of(&self) -> Type {
        match &self.kind {
            Kind::Dir(_) => Type::Dir,
            Kind::File(_) => Type::File,
            Kind::Symlink(_) => Type::Symlink,
            Kind::Device(d) => Type::Device(*d),
            Kind::Ext2(..) => match self.ext2_info().map_or(0, |i| i.mode as u32 & S_IFMT_MASK) {
                S_IFDIR => Type::Dir,
                S_IFLNK => Type::Symlink,
                _ => Type::File,
            },
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind_of() == Type::Dir
    }

    pub fn inode_number(&self) -> u64 {
        self.ino
    }

    pub fn dev(&self) -> u64 {
        match self.kind {
            Kind::Ext2(..) => DEV_DISK,
            _ => DEV_MEMORY,
        }
    }

    /// Permission bits.
    pub fn mode_bits(&self) -> u32 {
        match self.ext2_info() {
            Some(info) => info.mode as u32 & 0o7777,
            None => *self.mode.lock(),
        }
    }

    pub fn set_mode(&self, mode: u32) -> Result<(), Errno> {
        match &self.kind {
            Kind::Ext2(fs, ino) => fs.set_mode(*ino, mode),
            _ => {
                *self.mode.lock() = mode & 0o7777;
                Ok(())
            }
        }
    }

    /// The full st_mode: file type and permissions.
    pub fn st_mode(&self) -> u32 {
        let kind = match self.kind_of() {
            Type::Dir => S_IFDIR,
            Type::File => S_IFREG,
            Type::Symlink => S_IFLNK,
            Type::Device(_) => S_IFCHR,
        };
        kind | self.mode_bits()
    }

    pub fn size(&self) -> u64 {
        match &self.kind {
            Kind::File(data) => data.lock().len() as u64,
            Kind::Symlink(target) => target.len() as u64,
            Kind::Dir(entries) => entries.lock().len() as u64 * 32,
            Kind::Device(_) => 0,
            Kind::Ext2(..) => self.ext2_info().map_or(0, |i| i.size),
        }
    }

    pub fn nlink(&self) -> u64 {
        match self.ext2_info() {
            Some(info) => info.links as u64,
            None if self.is_dir() => 2,
            None => 1,
        }
    }

    /// Seconds since 1970 of the last change.
    pub fn mtime(&self) -> u64 {
        match self.ext2_info() {
            Some(info) => info.mtime as u64,
            None => self.mtime.load(Ordering::Relaxed),
        }
    }

    pub fn touch(&self) {
        self.mtime.store(crate::time::realtime().0, Ordering::Relaxed);
    }

    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        match &self.kind {
            Kind::File(data) => {
                let data = data.lock();
                let pos = (offset as usize).min(data.len());
                let n = (data.len() - pos).min(buf.len());
                buf[..n].copy_from_slice(&data[pos..pos + n]);
                Ok(n)
            }
            Kind::Ext2(fs, ino) if self.kind_of() == Type::File => fs.read_at(*ino, offset, buf),
            _ if self.is_dir() => Err(EISDIR),
            _ => Err(EINVAL),
        }
    }

    pub fn write_at(&self, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        match &self.kind {
            Kind::File(contents) => {
                let mut contents = contents.lock();
                let pos = offset as usize;
                if contents.len() < pos + data.len() {
                    contents.resize(pos + data.len(), 0);
                }
                contents[pos..pos + data.len()].copy_from_slice(data);
                drop(contents);
                self.touch();
                Ok(data.len())
            }
            Kind::Ext2(fs, ino) if self.kind_of() == Type::File => fs.write_at(*ino, offset, data),
            _ if self.is_dir() => Err(EISDIR),
            _ => Err(EINVAL),
        }
    }

    pub fn truncate(&self, len: u64) -> Result<(), Errno> {
        match &self.kind {
            Kind::File(data) => {
                data.lock().resize(len as usize, 0);
                self.touch();
                Ok(())
            }
            Kind::Ext2(fs, ino) if self.kind_of() == Type::File => fs.truncate(*ino, len),
            _ if self.is_dir() => Err(EISDIR),
            _ => Err(EINVAL),
        }
    }

    /// The whole contents of a file (loading programs, mapping files).
    pub fn read_all(&self) -> Result<Vec<u8>, Errno> {
        let mut data = alloc::vec![0u8; self.size() as usize];
        let n = self.read_at(0, &mut data)?;
        data.truncate(n);
        Ok(data)
    }

    pub fn readlink(&self) -> Result<String, Errno> {
        match &self.kind {
            Kind::Symlink(target) => Ok(target.clone()),
            Kind::Ext2(fs, ino) => fs.readlink(*ino),
            _ => Err(EINVAL),
        }
    }

    /// The entry `name` in this directory.
    pub fn child(&self, name: &str) -> Result<Arc<Node>, Errno> {
        match &self.kind {
            Kind::Dir(entries) => entries.lock().get(name).cloned().ok_or(ENOENT),
            Kind::Ext2(fs, ino) => Ok(Node::ext2(fs.clone(), fs.lookup(*ino, name)?)),
            _ => Err(ENOTDIR),
        }
    }

    /// (name, inode number, Linux d_type) for every entry, "." and ".." included.
    pub fn list(&self) -> Result<Vec<(String, u64, u8)>, Errno> {
        match &self.kind {
            Kind::Dir(entries) => {
                let mut out: Vec<(String, u64, u8)> = alloc::vec![(".".into(), self.ino, 4), ("..".into(), self.ino, 4)];
                for (name, child) in entries.lock().iter() {
                    let dtype = match child.kind_of() {
                        Type::Dir => 4,
                        Type::File => 8,
                        Type::Symlink => 10,
                        Type::Device(_) => 2,
                    };
                    out.push((name.clone(), child.ino, dtype));
                }
                Ok(out)
            }
            Kind::Ext2(fs, ino) => {
                // ext2's file type codes differ from d_type's.
                let to_dtype = |t: u8| match t {
                    1 => 8,
                    2 => 4,
                    3 => 2,
                    4 => 6,
                    5 => 1,
                    6 => 12,
                    7 => 10,
                    _ => 0,
                };
                Ok(fs.list(*ino)?.into_iter().map(|(n, i, t)| (n, i as u64, to_dtype(t))).collect())
            }
            _ => Err(ENOTDIR),
        }
    }

    /// Creates a file, directory or symlink called `name` here.
    pub fn create(&self, name: &str, new: New) -> Result<Arc<Node>, Errno> {
        match &self.kind {
            Kind::Dir(entries) => {
                let node = match new {
                    New::File(mode) => Node::file(Vec::new(), mode),
                    New::Dir(mode) => {
                        let d = Node::dir();
                        *d.mode.lock() = mode & 0o7777;
                        d
                    }
                    New::Symlink(target) => Node::new(Kind::Symlink(target.to_string()), 0o777),
                };
                let mut entries = entries.lock();
                if entries.contains_key(name) {
                    return Err(EEXIST);
                }
                entries.insert(name.to_string(), node.clone());
                self.touch();
                Ok(node)
            }
            Kind::Ext2(fs, ino) => {
                let new = match new {
                    New::File(mode) => ext2::New::File(mode as u16),
                    New::Dir(mode) => ext2::New::Dir(mode as u16),
                    New::Symlink(target) => ext2::New::Symlink(target),
                };
                Ok(Node::ext2(fs.clone(), fs.create(*ino, name, new)?))
            }
            _ => Err(ENOTDIR),
        }
    }

    /// A hard link: `node` gets the additional name `name` here.
    pub fn add_link(&self, name: &str, node: &Arc<Node>) -> Result<(), Errno> {
        match (&self.kind, &node.kind) {
            (Kind::Ext2(fs, dir), Kind::Ext2(other, ino)) if Arc::ptr_eq(fs, other) => fs.link(*dir, name, *ino),
            // Hard links can't cross file systems.
            (Kind::Ext2(..), _) | (Kind::Dir(_), Kind::Ext2(..)) => Err(EXDEV),
            (Kind::Dir(entries), _) => {
                let mut entries = entries.lock();
                if entries.contains_key(name) {
                    return Err(EEXIST);
                }
                entries.insert(name.to_string(), node.clone());
                self.touch();
                Ok(())
            }
            _ => Err(ENOTDIR),
        }
    }

    /// Removes the entry `name`. `dir` says whether a directory (rmdir) or anything else (unlink) is
    /// expected.
    pub fn remove(&self, name: &str, dir: bool) -> Result<(), Errno> {
        match &self.kind {
            Kind::Dir(entries) => {
                let mut entries = entries.lock();
                let node = entries.get(name).ok_or(ENOENT)?;
                match (node.is_dir(), dir) {
                    (true, true) => {
                        if node.list()?.len() > 2 {
                            return Err(ENOTEMPTY);
                        }
                    }
                    (true, false) => return Err(EISDIR),
                    (false, true) => return Err(ENOTDIR),
                    _ => {}
                }
                entries.remove(name);
                self.touch();
                Ok(())
            }
            Kind::Ext2(fs, ino) => fs.unlink(*ino, name, dir),
            _ => Err(ENOTDIR),
        }
    }
}

/// Moves `from_dir/from` to `to_dir/to`, on the same file system.
pub fn rename(from_dir: &Node, from: &str, to_dir: &Node, to: &str) -> Result<(), Errno> {
    match (&from_dir.kind, &to_dir.kind) {
        (Kind::Dir(a), Kind::Dir(b)) => {
            let node = a.lock().remove(from).ok_or(ENOENT)?;
            if let Some(existing) = b.lock().get(to) {
                if existing.is_dir() && existing.list().map_or(0, |l| l.len()) > 2 {
                    a.lock().insert(from.to_string(), node);
                    return Err(ENOTEMPTY);
                }
            }
            b.lock().insert(to.to_string(), node);
            from_dir.touch();
            to_dir.touch();
            Ok(())
        }
        (Kind::Ext2(fs, a), Kind::Ext2(other, b)) if Arc::ptr_eq(fs, other) => fs.rename(*a, from, *b, to),
        _ => Err(EXDEV),
    }
}

static ROOT: Once<Arc<Node>> = Once::new();

pub fn root() -> &'static Arc<Node> {
    ROOT.call_once(Node::dir)
}

/// Joins `path` onto `base` (an absolute directory) and removes "." and "..".
pub fn normalize(base: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let base = if path.starts_with('/') { "" } else { base };
    for part in base.split('/').chain(path.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

/// Finds the node at an absolute path. Symlinks along the way are followed; the last one only when
/// `follow` is set.
pub fn lookup(path: &str, follow: bool) -> Result<Arc<Node>, Errno> {
    lookup_depth(path, follow, 0)
}

fn lookup_depth(path: &str, follow: bool, depth: u32) -> Result<Arc<Node>, Errno> {
    if depth > 16 {
        return Err(ELOOP);
    }
    let path = normalize("/", path);
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let mut node = root().clone();
    for (i, part) in parts.iter().enumerate() {
        // /proc is made up fresh each time it's looked at.
        if i == 0 && *part == "proc" {
            let fresh = procfs::build();
            if parts.len() == 1 {
                return Ok(fresh);
            }
            node = fresh;
            continue;
        }
        let next = node.child(part)?;
        let last = i + 1 == parts.len();
        if next.kind_of() == Type::Symlink && (!last || follow) {
            let target = next.readlink()?;
            let here = String::from("/") + &parts[..i].join("/");
            let mut rest = normalize(&here, &target);
            for p in &parts[i + 1..] {
                rest.push('/');
                rest.push_str(p);
            }
            return lookup_depth(&rest, follow, depth + 1);
        }
        node = next;
    }
    Ok(node)
}

/// Splits an absolute path into (parent directory node, final name).
pub fn parent_of(path: &str) -> Result<(Arc<Node>, String), Errno> {
    let path = normalize("/", path);
    let (dir, name) = path.rsplit_once('/').ok_or(EINVAL)?;
    if name.is_empty() {
        return Err(EEXIST); // the root
    }
    let parent = lookup(if dir.is_empty() { "/" } else { dir }, true)?;
    if !parent.is_dir() {
        return Err(ENOTDIR);
    }
    Ok((parent, name.to_string()))
}

/// Creates every directory along an absolute path (used while building the tree at boot).
pub fn mkdir_p(path: &str) -> Arc<Node> {
    let mut node = root().clone();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        let next = match &node.kind {
            Kind::Dir(entries) => entries.lock().entry(part.to_string()).or_insert_with(Node::dir).clone(),
            _ => panic!("{path}: not a directory"),
        };
        node = next;
    }
    node
}

/// Puts a node at an absolute path, replacing what was there (boot-time setup, mounting).
pub fn install(path: &str, node: Arc<Node>) {
    let (dir, name) = path.rsplit_once('/').unwrap();
    let parent = mkdir_p(dir);
    if let Kind::Dir(entries) = &parent.kind {
        entries.lock().insert(name.to_string(), node);
    }
}

/// Where the disk is mounted, if one is (for /proc/mounts and statfs).
pub static DISK_MOUNT: Once<(String, Arc<Ext2>)> = Once::new();

/// Mounts an ext2 disk, if there is one, at `path`. Programs' files there survive a reboot.
pub fn mount_disk(path: &str) {
    if crate::drivers::virtio_blk::DISK.lock().is_none() {
        return;
    }
    match Ext2::mount() {
        Ok(fs) => {
            install(path, Node::ext2(fs.clone(), ext2::ROOT_INO));
            DISK_MOUNT.call_once(|| (path.to_string(), fs));
            crate::log!("ext2 disk mounted at {path}");
        }
        Err(why) => crate::log!("disk not mounted: {why}"),
    }
}

pub fn init_devices() {
    for (name, device, mode) in [
        ("console", Device::Tty, 0o620),
        ("tty", Device::Tty, 0o666),
        ("tty0", Device::Tty, 0o620),
        ("null", Device::Null, 0o666),
        ("zero", Device::Zero, 0o666),
        ("random", Device::Random, 0o666),
        ("urandom", Device::Random, 0o666),
    ] {
        install(&alloc::format!("/dev/{name}"), Node::new(Kind::Device(device), mode));
    }
    install("/dev/stdin", Node::new(Kind::Symlink("/proc/self/fd/0".into()), 0o777));
    install("/dev/stdout", Node::new(Kind::Symlink("/proc/self/fd/1".into()), 0o777));
    install("/dev/stderr", Node::new(Kind::Symlink("/proc/self/fd/2".into()), 0o777));
    *mkdir_p("/tmp").mode.lock() = 0o1777;
    mkdir_p("/proc");
}
