//! Open files and file descriptor tables. Like Linux, a descriptor points at an "open file
//! description" (the file plus a position and flags), which fork and dup share.

use crate::fs::{Node, notify};
use crate::fs::pipe::Pipe;
use crate::sys::errno::*;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

#[derive(Clone)]
pub enum Handle {
    Node(Arc<Node>),
    PipeRead(Arc<Pipe>),
    PipeWrite(Arc<Pipe>),
    /// A terminal (tty.rs): the console, or the inside of a pseudo-terminal; and the device file
    /// it was opened by, for stat.
    Tty(Arc<crate::tty::Tty>, Arc<Node>),
    /// The outside of a pseudo-terminal (pty.rs), made by opening /dev/ptmx.
    PtyMaster(Arc<crate::tty::Tty>),
    /// A window on the screen, made by opening /dev/window.
    Window(Arc<crate::console::Window>),
    /// A datagram socket: UDP, or ICMP for ping (net/socket.rs).
    Socket(Arc<crate::net::socket::Socket>),
    /// A TCP connection, or one waiting to be (net/tcp.rs).
    Tcp(Arc<crate::net::tcp::Conn>),
    /// Unix sockets (net/unix.rs): a stream (connected, listening or neither) or datagrams.
    UnixStream(Arc<crate::net::unix::Stream>),
    UnixDatagram(Arc<crate::net::unix::Datagram>),
    /// eventfd (fs/eventfd.rs).
    EventFd(Arc<crate::fs::eventfd::EventFd>),
    /// epoll (sys/epoll.rs).
    Epoll(Arc<crate::sys::epoll::Epoll>),
    /// inotify (fs/notify.rs).
    Inotify(Arc<crate::fs::notify::Inotify>),
    /// signalfd (sys/signalfd.rs).
    SignalFd(Arc<crate::sys::signalfd::SignalFd>),
    /// timerfd (sys/timerfd.rs).
    TimerFd(Arc<crate::sys::timerfd::TimerFd>),
    /// pidfd (sys/pidfd.rs).
    PidFd(Arc<crate::sys::pidfd::PidFd>),
}

pub struct OpenFile {
    pub handle: Handle,
    pub offset: Mutex<u64>,
    pub flags: Mutex<u32>,
    /// The absolute path it was opened with (for *at() calls relative to a directory fd).
    pub path: String,
}

impl OpenFile {
    pub fn new(handle: Handle, flags: u32, path: String) -> Arc<OpenFile> {
        Arc::new(OpenFile { handle, offset: Mutex::new(0), flags: Mutex::new(flags), path })
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
            Handle::Node(node) => {
                crate::fs::lock::release((node.dev(), node.inode_number()), self as *const OpenFile as usize);
                // Closed: watchers hear whether it was open for writing. (O_PATH isn't an open.)
                const O_PATH: u32 = 0o10000000;
                let flags = *self.flags.get_mut();
                if flags & O_PATH == 0 && self.path.starts_with('/') {
                    let mask = if flags & 3 != 0 { notify::IN_CLOSE_WRITE } else { notify::IN_CLOSE_NOWRITE };
                    notify::path_changed(&self.path, mask);
                }
                return;
            }
            // The last descriptor for a window is gone: take it off the screen.
            Handle::Window(w) => crate::console::close_window(w.id),
            Handle::Socket(s) => crate::net::socket::remove(s),
            Handle::Tcp(c) => crate::net::tcp::close(c),
            Handle::Tty(t, _) if !t.is_console() => crate::pty::inside_closed(t),
            Handle::Tty(..) => return,
            Handle::PtyMaster(t) => crate::pty::master_closed(t),
            // These let go when the last reference to them goes (their Drop).
            Handle::UnixStream(_) | Handle::UnixDatagram(_) | Handle::EventFd(_) | Handle::Epoll(_) | Handle::SignalFd(_) | Handle::TimerFd(_) | Handle::PidFd(_) | Handle::Inotify(_) => {}
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

    /// Closes every descriptor from `first` to `last`, or with `cloexec` only marks them
    /// close-on-exec (close_range).
    pub fn close_range(&mut self, first: usize, last: usize, cloexec: bool) {
        for slot in self.slots.iter_mut().skip(first).take(last.saturating_sub(first).saturating_add(1)) {
            match slot {
                Some(fd) if cloexec => fd.cloexec = true,
                _ => *slot = None,
            }
        }
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
}
