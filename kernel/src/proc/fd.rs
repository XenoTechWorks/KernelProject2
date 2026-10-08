//! Open files and file descriptor tables. Like Linux, a descriptor points at an "open file
//! description" (the file plus a position and flags), which fork and dup share.

use crate::fs::Node;
use crate::fs::pipe::Pipe;
use crate::sys::errno::*;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

pub enum Handle {
    Node(Arc<Node>),
    PipeRead(Arc<Pipe>),
    PipeWrite(Arc<Pipe>),
}

pub struct OpenFile {
    pub handle: Handle,
    pub offset: Mutex<u64>,
    pub flags: Mutex<u32>,
    /// The absolute path it was opened with (for *at() calls relative to a directory fd).
    pub path: String,
}

impl OpenFile {
    pub fn node(node: Arc<Node>, flags: u32, path: String) -> Arc<OpenFile> {
        Arc::new(OpenFile { handle: Handle::Node(node), offset: Mutex::new(0), flags: Mutex::new(flags), path })
    }

    pub fn pipe(pipe: &Arc<Pipe>, write: bool) -> Arc<OpenFile> {
        let handle = if write {
            *pipe.writers.lock() += 1;
            Handle::PipeWrite(pipe.clone())
        } else {
            *pipe.readers.lock() += 1;
            Handle::PipeRead(pipe.clone())
        };
        let flags = if write { 1 } else { 0 };
        Arc::new(OpenFile { handle, offset: Mutex::new(0), flags: Mutex::new(flags), path: String::from("pipe:") })
    }
}

impl Drop for OpenFile {
    fn drop(&mut self) {
        match &self.handle {
            Handle::PipeRead(p) => *p.readers.lock() -= 1,
            Handle::PipeWrite(p) => *p.writers.lock() -= 1,
            Handle::Node(_) => return,
        }
        crate::proc::wake_all();
    }
}

#[derive(Clone)]
pub struct Fd {
    pub file: Arc<OpenFile>,
    pub cloexec: bool,
}

pub const MAX_FDS: usize = 1024;

#[derive(Clone, Default)]
pub struct FdTable {
    slots: Vec<Option<Fd>>,
}

impl FdTable {
    pub fn get(&self, fd: i32) -> Result<Arc<OpenFile>, Errno> {
        self.slots.get(fd as usize).and_then(|s| s.as_ref()).map(|s| s.file.clone()).ok_or(EBADF)
    }

    pub fn entry(&mut self, fd: i32) -> Result<&mut Fd, Errno> {
        self.slots.get_mut(fd as usize).and_then(|s| s.as_mut()).ok_or(EBADF)
    }

    /// Puts a file in the lowest free slot at or above `min`.
    pub fn insert(&mut self, file: Arc<OpenFile>, cloexec: bool, min: usize) -> Result<i32, Errno> {
        let slot = (min..MAX_FDS).find(|&i| self.slots.get(i).is_none_or(|s| s.is_none())).ok_or(EMFILE)?;
        self.place(slot, Fd { file, cloexec });
        Ok(slot as i32)
    }

    pub fn place(&mut self, slot: usize, fd: Fd) {
        if self.slots.len() <= slot {
            self.slots.resize(slot + 1, None);
        }
        self.slots[slot] = Some(fd);
    }

    pub fn close(&mut self, fd: i32) -> Result<(), Errno> {
        self.slots.get_mut(fd as usize).and_then(|s| s.take()).map(|_| ()).ok_or(EBADF)
    }

    pub fn close_on_exec(&mut self) {
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|s| s.cloexec) {
                *slot = None;
            }
        }
    }

    /// (descriptor number, open file) for every open descriptor.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &Arc<OpenFile>)> {
        self.slots.iter().enumerate().filter_map(|(i, s)| s.as_ref().map(|s| (i, &s.file)))
    }

    pub fn clear(&mut self) {
        self.slots.clear();
    }
}
