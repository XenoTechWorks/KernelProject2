//! Pipes: a byte queue with a read end and a write end (`ls | grep x`). A Unix stream socket's
//! connection is two of them, one each way (net/unix.rs).

use crate::sys::errno::*;
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

/// Takes what's waiting, up to `buf.len()` bytes (or only looks, with `peek`). Waits while it's
/// empty and someone could still write; 0 is the end of the stream.
pub fn read(pipe: &Pipe, buf: &mut [u8], nonblocking: bool, peek: bool) -> Result<usize, Errno> {
    loop {
        {
            let mut q = pipe.buffer.lock();
            if !q.is_empty() {
                let n = q.len().min(buf.len());
                for (to, from) in buf.iter_mut().zip(q.iter()) {
                    *to = *from;
                }
                if !peek {
                    q.drain(..n);
                    drop(q);
                    crate::proc::wake_all(); // writers waiting for room
                }
                return Ok(n);
            }
        }
        if *pipe.writers.lock() == 0 {
            return Ok(0);
        }
        if nonblocking {
            return Err(EAGAIN);
        }
        crate::proc::sleep()?;
    }
}

/// Writes all of `data`, waiting for room as needed (or as much as fits, when it can't wait).
/// EPIPE when nobody is left to read it; the caller decides about SIGPIPE.
pub fn write(pipe: &Pipe, data: &[u8], nonblocking: bool) -> Result<usize, Errno> {
    let mut written = 0;
    while written < data.len() {
        if *pipe.readers.lock() == 0 {
            return if written > 0 { Ok(written) } else { Err(EPIPE) };
        }
        {
            let mut q = pipe.buffer.lock();
            let n = CAPACITY.saturating_sub(q.len()).min(data.len() - written);
            q.extend(&data[written..written + n]);
            written += n;
        }
        crate::proc::wake_all();
        if written < data.len() {
            if nonblocking {
                return if written > 0 { Ok(written) } else { Err(EAGAIN) };
            }
            if let Err(e) = crate::proc::sleep() {
                return if written > 0 { Ok(written) } else { Err(e) };
            }
        }
    }
    Ok(written)
}
