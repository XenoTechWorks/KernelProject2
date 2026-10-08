//! The file tree: an in-memory filesystem (like Linux's tmpfs) filled from the boot archive, plus device
//! files and pipes. Paths are resolved as text: the current directory is an absolute path, and
//! "." / ".." / symlinks are worked out before walking the tree.

pub mod cpio;
pub mod pipe;
pub mod procfs;
pub mod sysfs;

use crate::sys::errno::*;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
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
}

pub struct Node {
    pub ino: u64,
    pub mode: Mutex<u32>, // permission bits only
    pub kind: Kind,
    pub mtime: AtomicU64,
}

pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFMT_MASK: u32 = 0o170000;

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

    pub fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Dir(_))
    }

    /// The full st_mode: file type and permissions.
    pub fn st_mode(&self) -> u32 {
        let kind = match self.kind {
            Kind::Dir(_) => S_IFDIR,
            Kind::File(_) => S_IFREG,
            Kind::Symlink(_) => S_IFLNK,
            Kind::Device(_) => S_IFCHR,
        };
        kind | *self.mode.lock()
    }

    pub fn size(&self) -> u64 {
        match &self.kind {
            Kind::File(data) => data.lock().len() as u64,
            Kind::Symlink(target) => target.len() as u64,
            Kind::Dir(entries) => entries.lock().len() as u64 * 32,
            Kind::Device(_) => 0,
        }
    }

    pub fn touch(&self) {
        self.mtime.store(crate::time::realtime().0, Ordering::Relaxed);
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
        let next = match &node.kind {
            Kind::Dir(entries) => entries.lock().get(*part).cloned().ok_or(ENOENT)?,
            _ => return Err(ENOTDIR),
        };
        let last = i + 1 == parts.len();
        if let Kind::Symlink(target) = &next.kind {
            if !last || follow {
                let here = String::from("/") + &parts[..i].join("/");
                let mut rest = normalize(&here, target);
                for p in &parts[i + 1..] {
                    rest.push('/');
                    rest.push_str(p);
                }
                return lookup_depth(&rest, follow, depth + 1);
            }
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

/// Adds `node` as `name` in the directory `dir`.
pub fn link(dir: &Node, name: &str, node: Arc<Node>) -> Result<(), Errno> {
    match &dir.kind {
        Kind::Dir(entries) => {
            let mut entries = entries.lock();
            if entries.contains_key(name) {
                return Err(EEXIST);
            }
            entries.insert(name.to_string(), node);
            dir.touch();
            Ok(())
        }
        _ => Err(ENOTDIR),
    }
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

/// Puts a node at an absolute path, replacing what was there (boot-time setup).
pub fn install(path: &str, node: Arc<Node>) {
    let (dir, name) = path.rsplit_once('/').unwrap();
    let parent = mkdir_p(dir);
    if let Kind::Dir(entries) = &parent.kind {
        entries.lock().insert(name.to_string(), node);
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
    mkdir_p("/tmp");
    *mkdir_p("/tmp").mode.lock() = 0o1777;
    mkdir_p("/proc");
}
