//! epoll: a descriptor that watches others and says which are ready, the way event loops
//! (Python's asyncio, libuv in Node, ...) wait for many things at once.
//!
//! An entry follows the open file, not the number: once every descriptor for that file is closed,
//! the entry goes too. Edge-triggered entries (EPOLLET) are reported again only after something
//! has happened since (any wakeup in the kernel counts), so a program may be told twice about the
//! same data, which programs written for edge triggering handle, but never misses new data.

use super::SysResult;
use super::errno::*;
use crate::fs::Type;
use crate::mm::user;
use crate::proc::{self, fd::Handle, fd::OpenFile};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::Mutex;

pub struct Epoll {
    entries: Mutex<BTreeMap<i32, Entry>>,
}

struct Entry {
    file: Weak<OpenFile>,
    events: u32,
    data: u64,
    /// For EPOLLET: what was last reported, and when (proc::changes()).
    reported: Option<(u32, u64)>,
    /// EPOLLONESHOT turns an entry off once reported, until EPOLL_CTL_MOD.
    armed: bool,
}

const EPOLLERR: u32 = 0x8;
const EPOLLHUP: u32 = 0x10;
const EPOLLONESHOT: u32 = 1 << 30;
const EPOLLET: u32 = 1 << 31;

const EPOLL_CTL_ADD: u32 = 1;
const EPOLL_CTL_DEL: u32 = 2;
const EPOLL_CTL_MOD: u32 = 3;
const EPOLL_CLOEXEC: u32 = 0o2000000;

/// How deep epolls may watch epolls (Linux's limit too).
const MAX_NESTING: usize = 5;

/// `struct epoll_event`: packed on x86-64, 12 bytes.
#[derive(Clone, Copy, Default)]
#[repr(C, packed)]
struct Event {
    events: u32,
    data: u64,
}

fn ready_bits(file: &OpenFile, wanted: u32) -> u32 {
    super::fs::file_readiness(file) as u16 as u32 & (wanted | EPOLLERR | EPOLLHUP)
}

/// Is anything ready? (What poll on the epoll descriptor itself reports.)
pub fn has_ready(ep: &Epoll) -> bool {
    ep.entries.lock().values().any(|e| e.armed && e.file.upgrade().is_some_and(|f| ready_bits(&f, e.events) != 0))
}

/// Up to `max` ready entries, as (events, data); marks edge-triggered and one-shot ones reported.
fn collect(ep: &Epoll, max: usize) -> Vec<Event> {
    let now = proc::changes();
    let mut out = Vec::new();
    let mut entries = ep.entries.lock();
    entries.retain(|_, e| e.file.strong_count() > 0);
    for e in entries.values_mut() {
        if out.len() >= max {
            break;
        }
        let Some(file) = e.file.upgrade() else { continue };
        if !e.armed {
            continue;
        }
        let ready = ready_bits(&file, e.events);
        if ready == 0 {
            e.reported = None;
            continue;
        }
        if e.events & EPOLLET != 0 {
            if e.reported == Some((ready, now)) {
                continue;
            }
            e.reported = Some((ready, now));
        }
        if e.events & EPOLLONESHOT != 0 {
            e.armed = false;
        }
        out.push(Event { events: ready, data: e.data });
    }
    out
}

/// Does `ep` watch `target`, itself or through epolls it watches, within `depth` levels?
fn watches(ep: &Epoll, target: &Epoll, depth: usize) -> bool {
    if core::ptr::eq(ep, target) {
        return true;
    }
    if depth == 0 {
        return true; // too deep counts as a loop
    }
    ep.entries.lock().values().filter_map(|e| e.file.upgrade()).any(|f| match &f.handle {
        Handle::Epoll(inner) => watches(inner, target, depth - 1),
        _ => false,
    })
}

pub fn create(flags: u32) -> SysResult {
    if flags & !EPOLL_CLOEXEC != 0 {
        return Err(EINVAL);
    }
    let ep = Arc::new(Epoll { entries: Mutex::new(BTreeMap::new()) });
    let file = Arc::new(OpenFile {
        handle: Handle::Epoll(ep),
        offset: Mutex::new(0),
        flags: Mutex::new(2),
        path: String::from("anon_inode:[eventpoll]"),
    });
    Ok(proc::current().files().fds.insert(file, flags & EPOLL_CLOEXEC != 0, 0)? as u64)
}

fn epoll_of(epfd: i32) -> Result<Arc<Epoll>, Errno> {
    match &proc::current().files().fds.get(epfd)?.handle {
        Handle::Epoll(ep) => Ok(ep.clone()),
        _ => Err(EINVAL),
    }
}

pub fn ctl(epfd: i32, op: u32, fd: i32, event_ptr: u64) -> SysResult {
    let ep = epoll_of(epfd)?;
    let file = proc::current().files().fds.get(fd)?;
    if fd == epfd {
        return Err(EINVAL);
    }
    // Files on disk are always ready, so Linux refuses to watch them.
    if let Handle::Node(n) = &file.handle {
        if matches!(n.kind_of(), Type::File | Type::Dir) {
            return Err(EPERM);
        }
    }
    let event: Event = if op == EPOLL_CTL_DEL { Event::default() } else { user::read_struct(proc::current().space(), event_ptr)? };
    let mut entries = ep.entries.lock();
    // An entry for a file that has since been closed doesn't count.
    let existing = entries.get(&fd).is_some_and(|e| e.file.upgrade().is_some_and(|f| Arc::ptr_eq(&f, &file)));
    match op {
        EPOLL_CTL_ADD => {
            if existing {
                return Err(EEXIST);
            }
            if let Handle::Epoll(inner) = &file.handle {
                drop(entries);
                if watches(inner, &ep, MAX_NESTING) {
                    return Err(ELOOP);
                }
                entries = ep.entries.lock();
            }
            let entry = Entry { file: Arc::downgrade(&file), events: event.events, data: event.data, reported: None, armed: true };
            entries.insert(fd, entry);
        }
        EPOLL_CTL_MOD => {
            let e = entries.get_mut(&fd).filter(|_| existing).ok_or(ENOENT)?;
            e.events = event.events;
            e.data = event.data;
            e.reported = None;
            e.armed = true;
        }
        EPOLL_CTL_DEL => {
            if !existing {
                return Err(ENOENT);
            }
            entries.remove(&fd);
        }
        _ => return Err(EINVAL),
    }
    drop(entries);
    proc::wake_all(); // anyone waiting on this epoll looks again
    Ok(0)
}

/// epoll_wait, and epoll_pwait, which waits with the given signal mask.
pub fn pwait(epfd: i32, out: u64, max: i32, timeout_ms: i64, mask_at: u64) -> SysResult {
    proc::signal::with_mask(mask_at, || wait(epfd, out, max, timeout_ms))
}

pub fn wait(epfd: i32, out: u64, max: i32, timeout_ms: i64) -> SysResult {
    if max <= 0 {
        return Err(EINVAL);
    }
    let ep = epoll_of(epfd)?;
    let mut ready = Vec::new();
    super::fs::wait_ready(timeout_ms, || {
        ready = collect(&ep, max as usize);
        Ok(ready.len() as u64)
    })?;
    let space = proc::current().space();
    for (i, event) in ready.iter().enumerate() {
        user::write_struct(space, out + i as u64 * 12, event)?;
    }
    Ok(ready.len() as u64)
}

/// epoll_pwait2: the same with a timespec timeout (none: forever).
pub fn wait_timespec(epfd: i32, out: u64, max: i32, timeout: u64, mask_at: u64) -> SysResult {
    let timeout_ms = if timeout == 0 {
        -1
    } else {
        let space = proc::current().space();
        let secs = user::read_u64(space, timeout)? as i64;
        let nanos = user::read_u64(space, timeout + 8)? as i64;
        secs * 1000 + (nanos as u64).div_ceil(1_000_000) as i64
    };
    pwait(epfd, out, max, timeout_ms, mask_at)
}
