//! flock(): advisory locks on whole files. Programs that cooperate take one before touching a file
//! (or to make sure only one copy of themselves runs: tmux locks its socket). A lock belongs to an
//! open file description, so it's shared by dup and fork and goes when that's closed.

use crate::sys::errno::*;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;

const LOCK_SH: u32 = 1;
const LOCK_EX: u32 = 2;
const LOCK_NB: u32 = 4;
const LOCK_UN: u32 = 8;

#[derive(Default)]
struct Lock {
    exclusive: Option<usize>,
    shared: Vec<usize>,
}

/// Locked files, by (device, inode); holders are open file descriptions, by address.
static LOCKS: Mutex<BTreeMap<(u64, u64), Lock>> = Mutex::new(BTreeMap::new());

fn drop_holds(lock: &mut Lock, owner: usize) {
    if lock.exclusive == Some(owner) {
        lock.exclusive = None;
    }
    lock.shared.retain(|&o| o != owner);
}

pub fn flock(file: (u64, u64), owner: usize, op: u32) -> Result<(), Errno> {
    if op & LOCK_UN != 0 {
        release_one(file, owner);
        return Ok(());
    }
    let exclusive = match op & (LOCK_SH | LOCK_EX) {
        LOCK_SH => false,
        LOCK_EX => true,
        _ => return Err(EINVAL),
    };
    loop {
        {
            let mut locks = LOCKS.lock();
            let lock = locks.entry(file).or_default();
            let others_exclusive = lock.exclusive.is_some_and(|o| o != owner);
            let others_shared = lock.shared.iter().any(|&o| o != owner);
            if !(others_exclusive || (exclusive && others_shared)) {
                drop_holds(lock, owner);
                if exclusive {
                    lock.exclusive = Some(owner);
                } else {
                    lock.shared.push(owner);
                }
                return Ok(());
            }
        }
        if op & LOCK_NB != 0 {
            return Err(EAGAIN); // EWOULDBLOCK
        }
        crate::proc::sleep()?;
    }
}

fn release_one(file: (u64, u64), owner: usize) {
    let mut locks = LOCKS.lock();
    if let Some(lock) = locks.get_mut(&file) {
        drop_holds(lock, owner);
        if lock.exclusive.is_none() && lock.shared.is_empty() {
            locks.remove(&file);
        }
    }
    drop(locks);
    crate::proc::wake_all();
}

/// An open file description is closed: its locks go.
pub fn release(file: (u64, u64), owner: usize) {
    if LOCKS.lock().contains_key(&file) {
        release_one(file, owner);
    }
}
