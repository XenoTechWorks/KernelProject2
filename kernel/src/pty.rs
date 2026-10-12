//! Pseudo-terminals: a terminal with a program on each side. Opening /dev/ptmx makes one and
//! gives the outside ("master") end; its inside is /dev/pts/N, a terminal like the console, where
//! a shell runs. What the master writes is typed into the inside, through the line discipline
//! (tty.rs), and what programs inside write (and the echo of what's typed) the master reads.
//! That's how an SSH server, `script` or a terminal multiplexer gives programs a terminal.

use crate::fs::{self, Device, Kind, Node};
use crate::sys::errno::*;
use crate::tty::{Pty, Tty};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

/// Every pseudo-terminal whose master is open, by number.
static PTYS: Mutex<BTreeMap<u32, Arc<Tty>>> = Mutex::new(BTreeMap::new());

/// How many there can be at once.
const MAX: u32 = 256;

fn pty(tty: &Tty) -> &Pty {
    tty.pty.as_ref().expect("not a pseudo-terminal")
}

/// Makes a new pseudo-terminal (opening /dev/ptmx): its /dev/pts/N belongs to the caller.
pub fn open_master() -> Result<Arc<Tty>, Errno> {
    let mut ptys = PTYS.lock();
    let index = (0..MAX).find(|i| !ptys.contains_key(i)).ok_or(ENOSPC)?;
    let tty = Tty::new(Some(Pty {
        index,
        output: Mutex::new(VecDeque::new()),
        opens: AtomicUsize::new(0),
        opened: AtomicBool::new(false),
        locked: AtomicBool::new(true),
    }));
    ptys.insert(index, tty.clone());
    let node = Node::new(Kind::Device(Device::Pts(index)), 0o620);
    let creds = crate::proc::current().creds;
    let _ = node.set_owner(creds.euid, creds.egid);
    fs::install(&format!("/dev/pts/{index}"), node);
    Ok(tty)
}

/// The last descriptor for a master is closed: the inside hangs up, and /dev/pts/N goes away.
pub fn master_closed(tty: &Tty) {
    let index = pty(tty).index;
    PTYS.lock().remove(&index);
    fs::uninstall(&format!("/dev/pts/{index}"));
    tty.hang_up();
}

/// /dev/pts/N is being opened: its terminal, unless it's locked or gone.
pub fn open_inside(index: u32) -> Result<Arc<Tty>, Errno> {
    let tty = PTYS.lock().get(&index).cloned().ok_or(EIO)?;
    let p = pty(&tty);
    if p.locked.load(Ordering::Relaxed) {
        return Err(EIO);
    }
    p.opens.fetch_add(1, Ordering::Relaxed);
    p.opened.store(true, Ordering::Relaxed);
    Ok(tty)
}

/// The inside is opened another way (as /dev/tty, the controlling terminal): count it, since its
/// closing counts too.
pub fn inside_reopened(tty: &Tty) {
    let p = pty(tty);
    p.opens.fetch_add(1, Ordering::Relaxed);
    p.opened.store(true, Ordering::Relaxed);
}

pub fn inside_closed(tty: &Tty) {
    pty(tty).opens.fetch_sub(1, Ordering::Relaxed);
    crate::proc::wake_all();
}

/// Has every program inside let go of it (after one opened it)? The master then reads EIO.
fn deserted(p: &Pty) -> bool {
    p.opened.load(Ordering::Relaxed) && p.opens.load(Ordering::Relaxed) == 0
}

pub fn read(tty: &Tty, buf: &mut [u8], nonblocking: bool) -> Result<usize, Errno> {
    let p = pty(tty);
    loop {
        {
            let mut output = p.output.lock();
            if !output.is_empty() {
                let n = output.len().min(buf.len());
                for (to, from) in buf.iter_mut().zip(output.drain(..n)) {
                    *to = from;
                }
                drop(output);
                crate::proc::wake_all(); // programs inside waiting to write
                return Ok(n);
            }
        }
        if deserted(p) {
            return Err(EIO);
        }
        if nonblocking {
            return Err(EAGAIN);
        }
        crate::proc::sleep()?;
    }
}

/// What the master writes is typed into the inside.
pub fn write(tty: &Tty, data: &[u8]) -> usize {
    tty.input(data);
    data.len()
}

/// (something to read, the inside is deserted)
pub fn readiness(tty: &Tty) -> (bool, bool) {
    let p = pty(tty);
    let gone = deserted(p);
    (gone || !p.output.lock().is_empty(), gone)
}

pub fn pending(tty: &Tty) -> usize {
    pty(tty).output.lock().len()
}

pub fn index(tty: &Tty) -> u32 {
    pty(tty).index
}

pub fn set_locked(tty: &Tty, locked: bool) {
    pty(tty).locked.store(locked, Ordering::Relaxed);
}
