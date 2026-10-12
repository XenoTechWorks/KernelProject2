//! inotify: a program watches files and folders and reads what happens to them: created,
//! changed, closed, removed, renamed ... (file managers, `tail -f`, build tools watching sources).
//!
//! Watches are kept by file (device, inode), so they follow a file wherever its names go. The
//! file system calls (sys/fs.rs) report what they did through `changed`; a folder's watches hear
//! about what happens to the things in it, by name, and a file's own watches about it.

use crate::fs::Node;
use crate::sys::errno::*;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

pub const IN_ACCESS: u32 = 0x1;
pub const IN_MODIFY: u32 = 0x2;
pub const IN_ATTRIB: u32 = 0x4;
pub const IN_CLOSE_WRITE: u32 = 0x8;
pub const IN_CLOSE_NOWRITE: u32 = 0x10;
pub const IN_OPEN: u32 = 0x20;
pub const IN_MOVED_FROM: u32 = 0x40;
pub const IN_MOVED_TO: u32 = 0x80;
pub const IN_CREATE: u32 = 0x100;
pub const IN_DELETE: u32 = 0x200;
pub const IN_DELETE_SELF: u32 = 0x400;
pub const IN_MOVE_SELF: u32 = 0x800;
const IN_Q_OVERFLOW: u32 = 0x4000;
const IN_IGNORED: u32 = 0x8000;
pub const IN_ONLYDIR: u32 = 0x0100_0000;
pub const IN_DONT_FOLLOW: u32 = 0x0200_0000;
const IN_EXCL_UNLINK: u32 = 0x0400_0000;
const IN_MASK_CREATE: u32 = 0x1000_0000;
const IN_MASK_ADD: u32 = 0x2000_0000;
const IN_ISDIR: u32 = 0x4000_0000;
const IN_ONESHOT: u32 = 0x8000_0000;
const ALL_EVENTS: u32 = 0xfff;
const FLAGS: u32 = IN_ONLYDIR | IN_DONT_FOLLOW | IN_EXCL_UNLINK | IN_MASK_CREATE | IN_MASK_ADD | IN_ONESHOT;

/// Events an inotify holds before it says it lost some (IN_Q_OVERFLOW), as Linux's default.
const QUEUE_MAX: usize = 16384;

type Key = (u64, u64);

pub struct Inotify {
    events: Mutex<VecDeque<Vec<u8>>>,
    /// Its watches: watch descriptor → (the file, which events).
    watches: Mutex<BTreeMap<i32, (Key, u32)>>,
    next_wd: AtomicU32,
}

/// Who watches each file: (the inotify, its watch descriptor).
static WATCHERS: Mutex<BTreeMap<Key, Vec<(Weak<Inotify>, i32)>>> = Mutex::new(BTreeMap::new());
/// Ties a rename's two halves (MOVED_FROM and MOVED_TO) together.
static COOKIE: AtomicU32 = AtomicU32::new(1);

fn key(node: &Node) -> Key {
    (node.dev(), node.inode_number())
}

impl Drop for Inotify {
    fn drop(&mut self) {
        let mut watchers = WATCHERS.lock();
        for (k, _) in self.watches.get_mut().values() {
            if let Some(list) = watchers.get_mut(k) {
                list.retain(|(w, _)| w.strong_count() > 0);
                if list.is_empty() {
                    watchers.remove(k);
                }
            }
        }
    }
}

pub fn new() -> Arc<Inotify> {
    Arc::new(Inotify { events: Mutex::new(VecDeque::new()), watches: Mutex::new(BTreeMap::new()), next_wd: AtomicU32::new(1) })
}

/// Is anyone watching anything? (Most of the time not: then nothing needs looking up.)
pub fn active() -> bool {
    !WATCHERS.lock().is_empty()
}

/// `struct inotify_event`: the watch, the event, its cookie, and the name, padded with zeros to a
/// multiple of 16 bytes (as Linux does).
fn encode(wd: i32, mask: u32, cookie: u32, name: Option<&str>) -> Vec<u8> {
    let len = name.map_or(0, |n| (n.len() + 1).next_multiple_of(16));
    let mut e = Vec::with_capacity(16 + len);
    e.extend(wd.to_le_bytes());
    e.extend(mask.to_le_bytes());
    e.extend(cookie.to_le_bytes());
    e.extend((len as u32).to_le_bytes());
    if let Some(n) = name {
        e.extend(n.as_bytes());
        e.resize(16 + len, 0);
    }
    e
}

impl Inotify {
    fn push(&self, event: Vec<u8>) {
        let mut events = self.events.lock();
        // The same event twice in a row is told once.
        if events.back() == Some(&event) {
            return;
        }
        if events.len() >= QUEUE_MAX {
            let overflow = encode(-1, IN_Q_OVERFLOW, 0, None);
            if events.back() != Some(&overflow) {
                events.push_back(overflow);
            }
            return;
        }
        events.push_back(event);
        drop(events);
        crate::proc::wake_all();
    }

    /// A watch is gone (removed, one-shot done, or its file deleted): IN_IGNORED says so.
    fn drop_watch(&self, wd: i32) {
        if let Some((k, _)) = self.watches.lock().remove(&wd) {
            let mut watchers = WATCHERS.lock();
            if let Some(list) = watchers.get_mut(&k) {
                list.retain(|(w, d)| !(*d == wd && w.upgrade().is_some_and(|me| core::ptr::eq(&*me, self))));
                if list.is_empty() {
                    watchers.remove(&k);
                }
            }
            drop(watchers);
            self.push(encode(wd, IN_IGNORED, 0, None));
        }
    }
}

/// Tells the watches of file `k` about `mask` (with the name, for a folder's watches).
fn tell(k: Key, mask: u32, cookie: u32, name: Option<&str>, is_dir: bool) {
    let list = WATCHERS.lock().get(&k).cloned().unwrap_or_default();
    for (w, wd) in list {
        let Some(inotify) = w.upgrade() else { continue };
        let Some((_, wanted)) = inotify.watches.lock().get(&wd).copied() else { continue };
        if wanted & mask & ALL_EVENTS == 0 {
            continue;
        }
        let flags = if is_dir { IN_ISDIR } else { 0 };
        inotify.push(encode(wd, mask | flags, cookie, name));
        if wanted & IN_ONESHOT != 0 {
            inotify.drop_watch(wd);
        }
    }
}

/// Something happened: to `node` itself, and as the entry `name` of folder `dir`.
pub fn changed(dir: Option<&Node>, name: &str, node: Option<&Node>, mask: u32) {
    changed_with_cookie(dir, name, node, mask, 0);
}

fn changed_with_cookie(dir: Option<&Node>, name: &str, node: Option<&Node>, mask: u32, cookie: u32) {
    if !active() {
        return;
    }
    let is_dir = node.is_some_and(|n| n.is_dir());
    if let Some(n) = node {
        tell(key(n), mask, cookie, None, is_dir);
    }
    if let Some(d) = dir {
        tell(key(d), mask, cookie, Some(name), is_dir);
    }
}

/// The same, for a file known by its (whole-tree) path.
pub fn path_changed(path: &str, mask: u32) {
    if !active() {
        return;
    }
    let node = crate::fs::lookup(path, false).ok();
    let parent = crate::fs::parent_of(path).ok();
    changed(parent.as_ref().map(|(d, _)| &**d), parent.as_ref().map_or("", |(_, n)| n), node.as_deref(), mask);
}

/// A name was removed from a folder: the folder hears IN_DELETE; the file itself, if that was its
/// last name, IN_DELETE_SELF, and its watches end.
pub fn removed(dir: &Node, name: &str, node: &Node, last_name: bool) {
    if !active() {
        return;
    }
    changed(Some(dir), name, Some(node), IN_DELETE);
    if last_name {
        let k = key(node);
        tell(k, IN_DELETE_SELF, 0, None, false);
        let list = WATCHERS.lock().get(&k).cloned().unwrap_or_default();
        for (w, wd) in list {
            if let Some(inotify) = w.upgrade() {
                inotify.drop_watch(wd);
            }
        }
    }
}

/// A rename: MOVED_FROM and MOVED_TO with the same cookie, and MOVE_SELF for the file.
pub fn moved(from_dir: &Node, from: &str, to_dir: &Node, to: &str, node: &Node) {
    if !active() {
        return;
    }
    let cookie = COOKIE.fetch_add(1, Ordering::Relaxed);
    changed_with_cookie(Some(from_dir), from, None, IN_MOVED_FROM | if node.is_dir() { IN_ISDIR } else { 0 }, cookie);
    changed_with_cookie(Some(to_dir), to, None, IN_MOVED_TO | if node.is_dir() { IN_ISDIR } else { 0 }, cookie);
    tell(key(node), IN_MOVE_SELF, 0, None, node.is_dir());
}

/// inotify_add_watch: watches `node` for the events in `mask`; a second watch of the same file
/// replaces (or with IN_MASK_ADD, adds to) the first, and keeps its number.
pub fn add_watch(inotify: &Arc<Inotify>, node: &Node, mask: u32) -> Result<i32, Errno> {
    if mask & !(ALL_EVENTS | FLAGS) != 0 || mask & ALL_EVENTS == 0 {
        return Err(EINVAL);
    }
    if mask & IN_ONLYDIR != 0 && !node.is_dir() {
        return Err(ENOTDIR);
    }
    let k = key(node);
    let mut watches = inotify.watches.lock();
    if let Some((&wd, (_, old))) = watches.iter_mut().find(|(_, (wk, _))| *wk == k) {
        if mask & IN_MASK_CREATE != 0 {
            return Err(EEXIST);
        }
        *old = if mask & IN_MASK_ADD != 0 { *old | mask } else { mask };
        return Ok(wd);
    }
    let wd = inotify.next_wd.fetch_add(1, Ordering::Relaxed) as i32;
    watches.insert(wd, (k, mask));
    WATCHERS.lock().entry(k).or_default().push((Arc::downgrade(inotify), wd));
    Ok(wd)
}

pub fn rm_watch(inotify: &Inotify, wd: i32) -> Result<(), Errno> {
    if !inotify.watches.lock().contains_key(&wd) {
        return Err(EINVAL);
    }
    inotify.drop_watch(wd);
    Ok(())
}

/// Reads whole events, as many as fit; waits for one unless non-blocking.
pub fn read(inotify: &Inotify, buf: &mut [u8], nonblocking: bool) -> Result<usize, Errno> {
    loop {
        {
            let mut events = inotify.events.lock();
            if let Some(first) = events.front() {
                if first.len() > buf.len() {
                    return Err(EINVAL);
                }
                let mut n = 0;
                while let Some(e) = events.front() {
                    if n + e.len() > buf.len() {
                        break;
                    }
                    buf[n..n + e.len()].copy_from_slice(e);
                    n += e.len();
                    events.pop_front();
                }
                return Ok(n);
            }
        }
        if nonblocking {
            return Err(EAGAIN);
        }
        crate::proc::sleep()?;
    }
}

pub fn pending(inotify: &Inotify) -> usize {
    inotify.events.lock().iter().map(Vec::len).sum()
}
