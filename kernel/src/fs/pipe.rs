//! Pipes: a byte queue with a read end and a write end (`ls | grep x`).

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use spin::Mutex;

pub struct Pipe {
    pub buffer: Mutex<VecDeque<u8>>,
    pub readers: Mutex<usize>,
    pub writers: Mutex<usize>,
}

/// Bytes a pipe holds before writers wait (Linux's default is 64 KiB too).
pub const CAPACITY: usize = 64 * 1024;

pub fn new() -> Arc<Pipe> {
    Arc::new(Pipe { buffer: Mutex::new(VecDeque::new()), readers: Mutex::new(0), writers: Mutex::new(0) })
}
