//! File system calls: open, read, write, stat, directories, pipes, eventfd, terminal ioctls, poll.

use super::SysResult;
use super::errno::*;
use crate::fs::{self, Device, New, Node, Type, eventfd, pipe};
use crate::net::unix;
use crate::mm::user;
use crate::proc::{self, fd::Handle, fd::OpenFile};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

const O_ACCMODE: u32 = 3;
const O_WRONLY: u32 = 1;
const O_RDWR: u32 = 2;
const O_CREAT: u32 = 0o100;
const O_EXCL: u32 = 0o200;
const O_NOCTTY: u32 = 0o400;
const O_TRUNC: u32 = 0o1000;
const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
const O_DIRECTORY: u32 = 0o200000;
const O_NOFOLLOW: u32 = 0o400000;
const O_CLOEXEC: u32 = 0o2000000;
const O_PATH: u32 = 0o10000000;

/// The absolute path a (dirfd, path) pair names.
pub fn resolve(dirfd: i32, path_ptr: u64) -> Result<String, Errno> {
    let p = proc::current();
    let path = user::read_cstr(p.space(), path_ptr, 4096)?;
    resolve_str(dirfd, &path)
}

pub fn resolve_str(dirfd: i32, path: &str) -> Result<String, Errno> {
    let p = proc::current();
    if path.is_empty() {
        return Err(ENOENT);
    }
    let root = p.files().root.clone();
    if path.starts_with('/') {
        return Ok(fs::rooted(&root, path));
    }
    let base = if dirfd == AT_FDCWD { p.files().cwd.clone() } else { p.files().fds.get(dirfd)?.path.clone() };
    // Relative to a directory inside the root, ".." stops at the root; from one outside it (the
    // current directory, if chroot didn't move it) the path is taken as it is, as on Linux.
    Ok(match fs::unrooted(&root, &base) {
        Some(inside) => fs::rooted(&root, &fs::normalize(&inside, path)),
        None => fs::normalize(&base, path),
    })
}

fn readable(file: &OpenFile) -> bool {
    *file.flags.lock() & O_ACCMODE != O_WRONLY
}

fn writable(file: &OpenFile) -> bool {
    matches!(*file.flags.lock() & O_ACCMODE, O_WRONLY | O_RDWR)
}

fn nonblocking(file: &OpenFile) -> bool {
    *file.flags.lock() & O_NONBLOCK != 0
}

/// Reads from an open file into a kernel buffer, at the file's position or at `at`.
fn read_file(file: &OpenFile, buf: &mut [u8], at: Option<u64>) -> SysResult {
    if !readable(file) {
        return Err(EBADF);
    }
    match &file.handle {
        Handle::Node(node) => match node.kind_of() {
            Type::File => {
                let mut offset = file.offset.lock();
                let n = node.read_at(at.unwrap_or(*offset), buf)?;
                if at.is_none() {
                    *offset += n as u64;
                }
                Ok(n as u64)
            }
            Type::Dir => Err(EISDIR),
            Type::Symlink => Err(EINVAL),
            Type::Socket => Err(ENXIO),
            // Terminals are opened as Handle::Tty; only an O_PATH descriptor gets here.
            Type::Device(Device::Console | Device::Tty | Device::Ptmx | Device::Pts(_)) => Err(EBADF),
            Type::Device(Device::Null) => Ok(0),
            Type::Device(Device::Zero) => {
                buf.fill(0);
                Ok(buf.len() as u64)
            }
            Type::Device(Device::Random) => {
                super::misc::fill_random(buf);
                Ok(buf.len() as u64)
            }
            Type::Device(Device::Window) => Err(ENXIO),
            Type::Device(Device::Clipboard) => {
                let clip = fs::CLIPBOARD.lock();
                let mut offset = file.offset.lock();
                let pos = at.unwrap_or(*offset).min(clip.len() as u64) as usize;
                let n = buf.len().min(clip.len() - pos);
                buf[..n].copy_from_slice(&clip[pos..pos + n]);
                if at.is_none() {
                    *offset += n as u64;
                }
                Ok(n as u64)
            }
        },
        // What was typed into the window (and mouse clicks); end of file once the user closes it.
        Handle::Window(window) => loop {
            {
                let mut q = window.input.lock();
                // A graphical window's events are 16 bytes each, and only whole ones are read.
                let unit = if window.pixels.load(Ordering::Relaxed) { 16 } else { 1 };
                if buf.len() < unit {
                    return Err(EINVAL);
                }
                if !q.is_empty() {
                    let n = q.len().min(buf.len()) / unit * unit;
                    for (i, b) in q.drain(..n).enumerate() {
                        buf[i] = b;
                    }
                    return Ok(n as u64);
                }
            }
            if window.closed.load(Ordering::Relaxed) {
                return Ok(0);
            }
            if nonblocking(file) {
                return Err(EAGAIN);
            }
            proc::sleep()?;
        },
        Handle::Tty(tty, _) => Ok(tty.read(buf, nonblocking(file))? as u64),
        Handle::PtyMaster(tty) => Ok(crate::pty::read(tty, buf, nonblocking(file))? as u64),
        Handle::Tcp(conn) => super::net::stream_read(file, conn, buf, 0),
        Handle::Socket(sock) => loop {
            if let Some(packet) = sock.queue.lock().pop_front() {
                let n = packet.data.len().min(buf.len());
                buf[..n].copy_from_slice(&packet.data[..n]);
                return Ok(n as u64);
            }
            if nonblocking(file) {
                return Err(EAGAIN);
            }
            proc::sleep()?;
        },
        Handle::PipeRead(pipe) => Ok(pipe::read(pipe, buf, nonblocking(file), false)? as u64),
        Handle::PipeWrite(_) => Err(EBADF),
        Handle::UnixStream(s) => Ok(unix::read(s, buf, nonblocking(file), false)? as u64),
        Handle::UnixDatagram(d) => Ok(unix::recv(d, buf, nonblocking(file), false)?.len as u64),
        Handle::EventFd(e) => {
            if buf.len() < 8 {
                return Err(EINVAL);
            }
            buf[..8].copy_from_slice(&eventfd::read(e, nonblocking(file))?.to_le_bytes());
            Ok(8)
        }
        Handle::SignalFd(s) => Ok(super::signalfd::read(s, buf, nonblocking(file))? as u64),
        Handle::Epoll(_) => Err(EINVAL),
    }
}

fn write_file(file: &OpenFile, data: &[u8], at: Option<u64>) -> SysResult {
    if !writable(file) {
        return Err(EBADF);
    }
    match &file.handle {
        Handle::Node(node) => match node.kind_of() {
            Type::File => {
                let mut offset = file.offset.lock();
                let pos = match at {
                    Some(at) => at,
                    None if *file.flags.lock() & O_APPEND != 0 => node.size(),
                    None => *offset,
                };
                let n = node.write_at(pos, data)?;
                if at.is_none() {
                    *offset = pos + n as u64;
                }
                Ok(n as u64)
            }
            Type::Dir => Err(EISDIR),
            Type::Symlink => Err(EINVAL),
            Type::Socket => Err(ENXIO),
            Type::Device(Device::Console | Device::Tty | Device::Ptmx | Device::Pts(_)) => Err(EBADF),
            Type::Device(Device::Clipboard) => {
                // A write at the start replaces what was there; later writes add to it.
                let mut clip = fs::CLIPBOARD.lock();
                let mut offset = file.offset.lock();
                let pos = at.unwrap_or(*offset) as usize;
                let keep = pos.min(clip.len());
                clip.truncate(keep);
                clip.extend_from_slice(data);
                if at.is_none() {
                    *offset = (pos + data.len()) as u64;
                }
                Ok(data.len() as u64)
            }
            Type::Device(_) => Ok(data.len() as u64),
        },
        Handle::Window(window) => {
            crate::console::window_write(window.id, data);
            Ok(data.len() as u64)
        }
        Handle::Tty(tty, _) => Ok(tty.write(data, nonblocking(file))? as u64),
        Handle::PtyMaster(tty) => Ok(crate::pty::write(tty, data) as u64),
        Handle::Tcp(conn) => super::net::stream_write(file, conn, data, 0),
        Handle::Socket(sock) => {
            let Some((ip, port)) = *sock.peer.lock() else { return Err(EDESTADDRREQ) };
            if *sock.port.lock() == 0 {
                *sock.port.lock() = crate::net::socket::free_port();
            }
            crate::net::socket::send(sock, ip, port, data);
            Ok(data.len() as u64)
        }
        Handle::PipeWrite(pipe) => match pipe::write(pipe, data, nonblocking(file)) {
            Err(EPIPE) => {
                proc::signal::send(proc::current(), proc::SIGPIPE);
                Err(EPIPE)
            }
            result => Ok(result? as u64),
        },
        Handle::PipeRead(_) => Err(EBADF),
        Handle::UnixStream(s) => super::net::unix_write(file, s, data, 0),
        Handle::UnixDatagram(d) => Ok(unix::send(d, None, data, Vec::new(), nonblocking(file))? as u64),
        Handle::EventFd(e) => {
            let value = u64::from_le_bytes(data.get(..8).ok_or(EINVAL)?.try_into().unwrap());
            eventfd::write(e, value, nonblocking(file))?;
            Ok(8)
        }
        Handle::Epoll(_) | Handle::SignalFd(_) => Err(EINVAL),
    }
}

pub fn read(fd: i32, buf: u64, len: usize) -> SysResult {
    let file = proc::current().files().fds.get(fd)?;
    let mut tmp = vec![0u8; len.min(1 << 20)];
    let n = read_file(&file, &mut tmp, None)? as usize;
    user::write(proc::current().space(), buf, &tmp[..n])?;
    Ok(n as u64)
}

pub fn write(fd: i32, buf: u64, len: usize) -> SysResult {
    let p = proc::current();
    let file = p.files().fds.get(fd)?;
    let data = user::read_vec(p.space(), buf, len.min(1 << 24))?;
    write_file(&file, &data, None)
}

pub fn pread(fd: i32, buf: u64, len: usize, offset: u64) -> SysResult {
    let file = proc::current().files().fds.get(fd)?;
    if !matches!(&file.handle, Handle::Node(n) if n.kind_of() == Type::File) {
        return Err(ESPIPE);
    }
    let mut tmp = vec![0u8; len.min(1 << 20)];
    let n = read_file(&file, &mut tmp, Some(offset))? as usize;
    user::write(proc::current().space(), buf, &tmp[..n])?;
    Ok(n as u64)
}

pub fn pwrite(fd: i32, buf: u64, len: usize, offset: u64) -> SysResult {
    let p = proc::current();
    let file = p.files().fds.get(fd)?;
    if !matches!(&file.handle, Handle::Node(n) if n.kind_of() == Type::File) {
        return Err(ESPIPE);
    }
    let data = user::read_vec(p.space(), buf, len.min(1 << 24))?;
    write_file(&file, &data, Some(offset))
}

fn iovecs(iov: u64, count: usize) -> Result<Vec<(u64, usize)>, Errno> {
    if count > 1024 {
        return Err(EINVAL);
    }
    let p = proc::current();
    (0..count)
        .map(|i| Ok((user::read_u64(p.space(), iov + i as u64 * 16)?, user::read_u64(p.space(), iov + i as u64 * 16 + 8)? as usize)))
        .collect()
}

pub fn readv(fd: i32, iov: u64, count: usize) -> SysResult {
    let mut total = 0;
    for (base, len) in iovecs(iov, count)? {
        if len == 0 {
            continue;
        }
        let n = read(fd, base, len)?;
        total += n;
        if (n as usize) < len {
            break;
        }
    }
    Ok(total)
}

pub fn writev(fd: i32, iov: u64, count: usize) -> SysResult {
    // Gather first, so a line written in pieces reaches the terminal in one go.
    let p = proc::current();
    let file = p.files().fds.get(fd)?;
    let mut data = Vec::new();
    for (base, len) in iovecs(iov, count)? {
        data.extend(user::read_vec(p.space(), base, len.min(1 << 24))?);
    }
    write_file(&file, &data, None)
}

pub fn openat(dirfd: i32, path_ptr: u64, flags: u32, mode: u32) -> SysResult {
    let path = resolve(dirfd, path_ptr)?;
    open_path(&path, flags, mode)
}

pub fn open_path(path: &str, flags: u32, mode: u32) -> SysResult {
    let p = proc::current();
    let follow = flags & O_NOFOLLOW == 0;
    let node = match fs::lookup(path, follow) {
        Ok(node) => {
            if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                return Err(EEXIST);
            }
            node
        }
        Err(ENOENT) if flags & O_CREAT != 0 => {
            let (dir, name) = fs::parent_of(path)?;
            dir.create(&name, New::File(mode & 0o7777 & !p.files().umask))?
        }
        Err(e) => return Err(e),
    };
    if flags & O_PATH == 0 {
        // Read, write, or both: 4 and 2 of the permission bits.
        let want = match flags & O_ACCMODE {
            O_WRONLY => 2,
            O_RDWR => 6,
            _ => 4,
        } | if flags & O_TRUNC != 0 { 2 } else { 0 };
        if !allowed(&node, want) {
            return Err(EACCES);
        }
        if flags & O_DIRECTORY != 0 && !node.is_dir() {
            return Err(ENOTDIR);
        }
        if node.is_dir() && flags & O_ACCMODE != 0 {
            return Err(EISDIR);
        }
        if node.kind_of() == Type::Symlink {
            return Err(ELOOP);
        }
        if node.kind_of() == Type::Socket {
            return Err(ENXIO);
        }
        if flags & O_TRUNC != 0 && flags & O_ACCMODE != 0 && node.kind_of() == Type::File {
            node.truncate(0)?;
        }
    }
    let kept = flags & !(O_CREAT | O_EXCL | O_NOCTTY | O_TRUNC | O_CLOEXEC);
    let handle = match node.kind_of() {
        _ if flags & O_PATH != 0 => Handle::Node(node),
        Type::Device(Device::Window) => Handle::Window(crate::console::open_window().ok_or(ENXIO)?),
        Type::Device(Device::Console) => Handle::Tty(crate::tty::console(), node),
        Type::Device(Device::Tty) => {
            let tty = p.ctty.clone().ok_or(ENXIO)?;
            if !tty.is_console() {
                crate::pty::inside_reopened(&tty);
            }
            Handle::Tty(tty, node)
        }
        Type::Device(Device::Ptmx) => Handle::PtyMaster(crate::pty::open_master()?),
        Type::Device(Device::Pts(n)) => {
            let inside = crate::pty::open_inside(n)?;
            // A session leader without a terminal gets this one, as on Linux.
            if flags & O_NOCTTY == 0 && p.tgid == p.sid && p.ctty.is_none() && inside.session() == 0 {
                let _ = take_control(&inside, false);
            }
            Handle::Tty(inside, node)
        }
        _ => Handle::Node(node),
    };
    let file = OpenFile::new(handle, kept, String::from(path));
    Ok(p.files().fds.insert(file, flags & O_CLOEXEC != 0, 0)? as u64)
}

/// May this process touch the file that way? `want` is 4 to read, 2 to write, 1 to run.
pub fn allowed(node: &Node, want: u32) -> bool {
    let (uid, gid) = node.owner();
    proc::current().creds.allows(node.st_mode() & 0o7777, uid, gid, want)
}

pub fn close(fd: i32) -> SysResult {
    proc::current().files().fds.close(fd)?;
    Ok(0)
}

/// flock: an advisory lock on the whole file (fs/lock.rs). Only files have them; on anything else
/// it succeeds without doing anything.
pub fn flock(fd: i32, op: u32) -> SysResult {
    let file = proc::current().files().fds.get(fd)?;
    if let Handle::Node(node) = &file.handle {
        fs::lock::flock((node.dev(), node.inode_number()), alloc::sync::Arc::as_ptr(&file) as usize, op)?;
    }
    Ok(0)
}

/// close_range: what a program does before running another, so the other inherits nothing.
pub fn close_range(first: u32, last: u32, flags: u32) -> SysResult {
    // "Get a table of your own first": not done, but programs ask right after fork, when theirs is.
    const CLOSE_RANGE_UNSHARE: u32 = 2;
    const CLOSE_RANGE_CLOEXEC: u32 = 4;
    if first > last || flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    proc::current().files().fds.close_range(first as usize, last as usize, flags & CLOSE_RANGE_CLOEXEC != 0);
    Ok(0)
}

pub fn dup(fd: i32) -> SysResult {
    let p = proc::current();
    let file = p.files().fds.get(fd)?;
    Ok(p.files().fds.insert(file, false, 0)? as u64)
}

pub fn dup3(old: i32, new: i32, flags: u32, is_dup2: bool) -> SysResult {
    let p = proc::current();
    let file = p.files().fds.get(old)?;
    if old == new {
        return if is_dup2 { Ok(new as u64) } else { Err(EINVAL) };
    }
    if new < 0 || new as usize >= proc::fd::MAX_FDS {
        return Err(EBADF);
    }
    p.files().fds.place(new as usize, proc::fd::Fd { file, cloexec: flags & O_CLOEXEC != 0 });
    Ok(new as u64)
}

pub fn lseek(fd: i32, offset: i64, whence: u32) -> SysResult {
    let file = proc::current().files().fds.get(fd)?;
    let Handle::Node(node) = &file.handle else { return Err(ESPIPE) };
    if matches!(node.kind_of(), Type::Device(_)) {
        return Ok(0);
    }
    let mut pos = file.offset.lock();
    let base = match whence {
        0 => 0,
        1 => *pos as i64,
        2 => node.size() as i64,
        _ => return Err(EINVAL),
    };
    let new = base.checked_add(offset).filter(|&v| v >= 0).ok_or(EINVAL)?;
    *pos = new as u64;
    Ok(new as u64)
}

/// Linux's `struct stat` on x86-64.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Stat {
    dev: u64,
    ino: u64,
    nlink: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    _pad0: u32,
    rdev: u64,
    size: i64,
    blksize: i64,
    blocks: i64,
    atime: i64,
    atime_ns: i64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
    _unused: [i64; 3],
}

/// A device number the way glibc's makedev() packs it.
pub fn makedev(major: u64, minor: u64) -> u64 {
    ((major & 0xfffff000) << 32) | ((major & 0xfff) << 8) | ((minor & 0xffffff00) << 12) | (minor & 0xff)
}

fn rdev(node: &Node) -> u64 {
    match node.kind_of() {
        Type::Device(Device::Tty) => makedev(5, 0),
        Type::Device(Device::Console) => makedev(5, 1),
        Type::Device(Device::Ptmx) => makedev(5, 2),
        Type::Device(Device::Pts(n)) => makedev(136, n as u64),
        Type::Device(Device::Null) => makedev(1, 3),
        Type::Device(Device::Zero) => makedev(1, 5),
        Type::Device(Device::Random) => makedev(1, 9),
        Type::Device(Device::Clipboard) => makedev(10, 240),
        _ => 0,
    }
}

fn stat_of(handle: &Handle) -> Stat {
    match handle {
        Handle::Node(node) | Handle::Tty(_, node) => {
            let size = node.size() as i64;
            let mtime = node.mtime() as i64;
            let (uid, gid) = node.owner();
            Stat {
                dev: node.dev(),
                ino: node.inode_number(),
                uid,
                gid,
                nlink: node.nlink(),
                mode: node.st_mode(),
                rdev: rdev(node),
                size,
                blksize: 4096,
                blocks: (size + 511) / 512,
                atime: mtime,
                mtime,
                ctime: mtime,
                ..Default::default()
            }
        }
        Handle::PipeRead(_) | Handle::PipeWrite(_) => Stat { dev: 2, mode: fs::S_IFIFO | 0o600, nlink: 1, blksize: 4096, ..Default::default() },
        Handle::Window(_) => Stat { mode: fs::S_IFCHR | 0o666, nlink: 1, blksize: 4096, ..Default::default() },
        Handle::PtyMaster(_) => Stat { mode: fs::S_IFCHR | 0o666, rdev: makedev(5, 2), nlink: 1, blksize: 4096, ..Default::default() },
        Handle::Socket(_) | Handle::Tcp(_) | Handle::UnixStream(_) | Handle::UnixDatagram(_) => {
            Stat { mode: fs::S_IFSOCK | 0o777, nlink: 1, blksize: 4096, ..Default::default() }
        }
        // Like Linux's "anonymous inodes": no file type, only permissions.
        Handle::EventFd(_) | Handle::Epoll(_) | Handle::SignalFd(_) => Stat { mode: 0o600, nlink: 1, blksize: 4096, ..Default::default() },
    }
}

pub fn fstat(fd: i32, out: u64) -> SysResult {
    let p = proc::current();
    let file = p.files().fds.get(fd)?;
    user::write_struct(p.space(), out, &stat_of(&file.handle))?;
    Ok(0)
}

fn handle_for(dirfd: i32, path_ptr: u64, flags: u32) -> Result<Handle, Errno> {
    let p = proc::current();
    let path = user::read_cstr(p.space(), path_ptr, 4096)?;
    if path.is_empty() && flags & AT_EMPTY_PATH != 0 {
        let file = if dirfd == AT_FDCWD { None } else { Some(p.files().fds.get(dirfd)?) };
        // A bare Handle doesn't count as a pipe end (only OpenFile does), so cloning one is harmless.
        return Ok(match file {
            Some(f) => f.handle.clone(),
            None => Handle::Node(fs::lookup(&p.files().cwd, true)?),
        });
    }
    let abs = resolve_str(dirfd, &path)?;
    Ok(Handle::Node(fs::lookup(&abs, flags & AT_SYMLINK_NOFOLLOW == 0)?))
}

pub fn fstatat(dirfd: i32, path_ptr: u64, out: u64, flags: u32) -> SysResult {
    let handle = handle_for(dirfd, path_ptr, flags)?;
    user::write_struct(proc::current().space(), out, &stat_of(&handle))?;
    Ok(0)
}

pub fn statx(dirfd: i32, path_ptr: u64, flags: u32, out: u64) -> SysResult {
    let handle = handle_for(dirfd, path_ptr, flags)?;
    let st = stat_of(&handle);
    let mut buf = [0u8; 256];
    let put32 = |b: &mut [u8; 256], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |b: &mut [u8; 256], at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put32(&mut buf, 0, 0x7ff); // stx_mask: the basic fields
    put32(&mut buf, 4, 4096); // blksize
    put32(&mut buf, 16, st.nlink as u32);
    buf[28..30].copy_from_slice(&(st.mode as u16).to_le_bytes());
    put64(&mut buf, 32, st.ino);
    put64(&mut buf, 40, st.size as u64);
    put64(&mut buf, 48, st.blocks as u64);
    for at in [64, 80, 96, 112] {
        put64(&mut buf, at, st.mtime as u64); // atime, btime, ctime, mtime
    }
    put32(&mut buf, 128, (st.rdev >> 8) as u32); // rdev major
    put32(&mut buf, 132, (st.rdev & 0xff) as u32);
    put32(&mut buf, 136, 0); // dev major
    put32(&mut buf, 140, st.dev as u32);
    user::write(proc::current().space(), out, &buf)?;
    Ok(0)
}

pub fn getdents64(fd: i32, buf: u64, len: usize) -> SysResult {
    let p = proc::current();
    let file = p.files().fds.get(fd)?;
    let Handle::Node(node) = &file.handle else { return Err(ENOTDIR) };
    let list = node.list()?;
    let mut pos = file.offset.lock();
    let mut out = Vec::new();
    while (*pos as usize) < list.len() {
        let (name, ino, kind) = &list[*pos as usize];
        let reclen = (19 + name.len() + 1).next_multiple_of(8);
        if out.len() + reclen > len {
            if out.is_empty() {
                return Err(EINVAL);
            }
            break;
        }
        let start = out.len();
        out.extend(ino.to_le_bytes());
        out.extend((*pos + 1).to_le_bytes());
        out.extend((reclen as u16).to_le_bytes());
        out.push(*kind);
        out.extend(name.as_bytes());
        out.resize(start + reclen, 0);
        *pos += 1;
    }
    user::write(p.space(), buf, &out)?;
    Ok(out.len() as u64)
}

pub fn ioctl(fd: i32, request: u32, arg: u64) -> SysResult {
    const TIOCGWINSZ: u32 = 0x5413;
    const FIONREAD: u32 = 0x541B;
    const FIONBIO: u32 = 0x5421;
    const FIONCLEX: u32 = 0x5450;
    const FIOCLEX: u32 = 0x5451;

    let p = proc::current();
    let file = p.files().fds.get(fd)?;
    match request {
        FIONBIO => {
            let on = user::read_u64(p.space(), arg).map(|v| v as u32 != 0)?;
            let mut flags = file.flags.lock();
            *flags = if on { *flags | O_NONBLOCK } else { *flags & !O_NONBLOCK };
            return Ok(0);
        }
        FIOCLEX | FIONCLEX => {
            p.files().fds.entry(fd)?.cloexec = request == FIOCLEX;
            return Ok(0);
        }
        _ => {}
    }
    if let Handle::Socket(_) | Handle::Tcp(_) | Handle::UnixStream(_) | Handle::UnixDatagram(_) = &file.handle {
        return super::net::net_ioctl(&file.handle, request, arg);
    }
    if let Handle::Window(window) = &file.handle {
        return match request {
            TIOCGWINSZ => {
                let (cols, rows, w, h) = crate::console::window_size(window.id);
                let ws: [u16; 4] = [rows as u16, cols as u16, w as u16, h as u16];
                user::write_struct(p.space(), arg, &ws)?;
                Ok(0)
            }
            // Graphical windows: the program draws pixels itself. See console.rs.
            WINDOW_PIXELS => {
                let [w, h]: [u32; 2] = user::read_struct(p.space(), arg)?;
                let (w, h) = crate::console::window_set_pixels(window.id, w, h).ok_or(EINVAL)?;
                user::write_struct(p.space(), arg, &[w, h])?;
                Ok(0)
            }
            WINDOW_TITLE => {
                let title = user::read_cstr(p.space(), arg, 256)?;
                crate::console::window_set_title(window.id, &title);
                Ok(0)
            }
            WINDOW_POINTER => {
                let (x, y) = crate::console::window_pointer(window.id);
                user::write_struct(p.space(), arg, &[x, y])?;
                Ok(0)
            }
            WINDOW_BLIT => {
                let b: Blit = user::read_struct(p.space(), arg)?;
                if b.w > 4096 || b.h > 4096 || b.stride < b.w {
                    return Err(EINVAL);
                }
                let mut pixels = vec![0u32; (b.w * b.h) as usize];
                let mut row = vec![0u8; b.w as usize * 4];
                for y in 0..b.h as usize {
                    user::read(p.space(), b.pixels + (y * b.stride as usize * 4) as u64, &mut row)?;
                    for (x, px) in row.chunks_exact(4).enumerate() {
                        pixels[y * b.w as usize + x] = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                    }
                }
                crate::console::window_blit(window.id, b.x as i32, b.y as i32, b.w as i32, b.h as i32, &pixels);
                Ok(0)
            }
            FIONREAD => {
                user::write_struct(p.space(), arg, &(window.input.lock().len() as u32))?;
                Ok(0)
            }
            _ => Err(ENOTTY),
        };
    }
    match &file.handle {
        Handle::Tty(tty, _) => tty_ioctl(tty, request, arg),
        Handle::PtyMaster(tty) => master_ioctl(tty, request, arg),
        _ => Err(ENOTTY),
    }
}

/// The terminal ioctls: termios, window size, process groups, controlling terminal.
fn tty_ioctl(tty: &alloc::sync::Arc<crate::tty::Tty>, request: u32, arg: u64) -> SysResult {
    const TCGETS: u32 = 0x5401;
    const TCSETS: u32 = 0x5402;
    const TCSETSW: u32 = 0x5403;
    const TCSETSF: u32 = 0x5404;
    const TCSBRK: u32 = 0x5409;
    const TCXONC: u32 = 0x540A;
    const TCFLSH: u32 = 0x540B;
    const TIOCSCTTY: u32 = 0x540E;
    const TIOCGPGRP: u32 = 0x540F;
    const TIOCSPGRP: u32 = 0x5410;
    const TIOCOUTQ: u32 = 0x5411;
    const TIOCGWINSZ: u32 = 0x5413;
    const TIOCSWINSZ: u32 = 0x5414;
    const FIONREAD: u32 = 0x541B;
    const TIOCNOTTY: u32 = 0x5422;
    const TIOCGSID: u32 = 0x5429;
    const TCSBRKP: u32 = 0x5425;

    let p = proc::current();
    match request {
        TCGETS => {
            user::write_struct(p.space(), arg, &tty.termios())?;
            Ok(0)
        }
        TCSETS | TCSETSW | TCSETSF => {
            let t: crate::tty::Termios = user::read_struct(p.space(), arg)?;
            if request == TCSETSF {
                tty.flush_input();
            }
            tty.set_termios(t);
            Ok(0)
        }
        TIOCGWINSZ => {
            let (rows, cols) = tty.size();
            user::write_struct(p.space(), arg, &[rows, cols, 0, 0])?;
            Ok(0)
        }
        TIOCSWINSZ => {
            let [rows, cols, _, _]: [u16; 4] = user::read_struct(p.space(), arg)?;
            tty.set_size(rows, cols);
            Ok(0)
        }
        TIOCSCTTY => {
            take_control(tty, true)?;
            Ok(0)
        }
        TIOCNOTTY => {
            if p.ctty.as_ref().is_some_and(|c| alloc::sync::Arc::ptr_eq(c, tty)) {
                for t in proc::threads(p.tgid) {
                    t.ctty = None;
                }
            }
            Ok(0)
        }
        TIOCGPGRP => {
            user::write_struct(p.space(), arg, &tty.foreground())?;
            Ok(0)
        }
        TIOCSPGRP => {
            let pgid: u32 = user::read_struct(p.space(), arg)?;
            tty.set_foreground(pgid);
            Ok(0)
        }
        TIOCGSID => {
            user::write_struct(p.space(), arg, &tty.session())?;
            Ok(0)
        }
        FIONREAD => {
            user::write_struct(p.space(), arg, &(tty.available() as u32))?;
            Ok(0)
        }
        TIOCOUTQ => {
            user::write_struct(p.space(), arg, &0u32)?; // output never waits in the kernel
            Ok(0)
        }
        TCFLSH => {
            if arg != 1 {
                tty.flush_input(); // TCIFLUSH or TCIOFLUSH
            }
            Ok(0)
        }
        // tcdrain (output is shown at once), breaks, and flow control: nothing to do.
        TCSBRK | TCSBRKP | TCXONC => Ok(0),
        _ => Err(ENOTTY),
    }
}

/// The outside of a pseudo-terminal: its number, unlocking the inside, opening the inside, and
/// everything a terminal answers (termios and the size are the inside's).
fn master_ioctl(tty: &alloc::sync::Arc<crate::tty::Tty>, request: u32, arg: u64) -> SysResult {
    const TIOCGPTN: u32 = 0x80045430;
    const TIOCSPTLCK: u32 = 0x40045431;
    const TIOCGPTPEER: u32 = 0x5441;
    const FIONREAD: u32 = 0x541B;
    let p = proc::current();
    match request {
        TIOCGPTN => {
            user::write_struct(p.space(), arg, &crate::pty::index(tty))?;
            Ok(0)
        }
        TIOCSPTLCK => {
            let lock: i32 = user::read_struct(p.space(), arg)?;
            crate::pty::set_locked(tty, lock != 0);
            Ok(0)
        }
        // Opens the inside straight away, without going through its name.
        TIOCGPTPEER => {
            let flags = arg as u32;
            let path = alloc::format!("/dev/pts/{}", crate::pty::index(tty));
            let node = fs::lookup(&path, true)?;
            let inside = crate::pty::open_inside(crate::pty::index(tty))?;
            let kept = flags & (O_ACCMODE | O_NONBLOCK);
            let file = OpenFile::new(Handle::Tty(inside, node), kept, path);
            Ok(p.files().fds.insert(file, flags & O_CLOEXEC != 0, 0)? as u64)
        }
        FIONREAD => {
            user::write_struct(p.space(), arg, &(crate::pty::pending(tty) as u32))?;
            Ok(0)
        }
        _ => tty_ioctl(tty, request, arg),
    }
}

/// Makes `tty` the caller's controlling terminal: it must lead a session that has none (or, with
/// `steal`, be root). Opening a terminal does this too, unless O_NOCTTY.
fn take_control(tty: &alloc::sync::Arc<crate::tty::Tty>, steal: bool) -> Result<(), Errno> {
    let p = proc::current();
    if p.ctty.as_ref().is_some_and(|c| alloc::sync::Arc::ptr_eq(c, tty)) {
        return Ok(());
    }
    if p.tgid != p.sid || p.ctty.is_some() {
        return Err(EPERM);
    }
    if tty.session() != 0 && !(steal && p.creds.is_root()) {
        return Err(EPERM);
    }
    tty.control(p.sid, p.pgid);
    for t in proc::threads(p.tgid) {
        t.ctty = Some(tty.clone());
    }
    Ok(())
}

const WINDOW_PIXELS: u32 = 0x5701;
const WINDOW_TITLE: u32 = 0x5702;
const WINDOW_BLIT: u32 = 0x5703;
const WINDOW_POINTER: u32 = 0x5704;

/// The WINDOW_BLIT argument: a w x h piece of a picture in the program's memory (rows `stride`
/// pixels apart), for (x, y) in the window.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct Blit {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    stride: u32,
    _pad: u32,
    pixels: u64,
}

pub fn fcntl(fd: i32, cmd: u32, arg: u64) -> SysResult {
    let p = proc::current();
    let file = p.files().fds.get(fd)?;
    match cmd {
        0 | 1030 => Ok(p.files().fds.insert(file, cmd == 1030, arg as usize)? as u64), // F_DUPFD, F_DUPFD_CLOEXEC
        1 => Ok(p.files().fds.entry(fd)?.cloexec as u64),                              // F_GETFD
        2 => {
            p.files().fds.entry(fd)?.cloexec = arg & 1 != 0; // F_SETFD
            Ok(0)
        }
        3 => Ok(*file.flags.lock() as u64), // F_GETFL
        4 => {
            let mut flags = file.flags.lock(); // F_SETFL: only O_APPEND and O_NONBLOCK change
            *flags = (*flags & !(O_APPEND | O_NONBLOCK)) | (arg as u32 & (O_APPEND | O_NONBLOCK));
            Ok(0)
        }
        5..=7 | 36..=38 => Ok(0), // record locks: one user, nothing to lock against yet
        1031 | 1032 => Ok(0x10000), // F_SETPIPE_SZ / F_GETPIPE_SZ
        _ => Err(EINVAL),
    }
}

pub fn getcwd(buf: u64, size: usize) -> SysResult {
    let p = proc::current();
    // As the process sees it; Linux marks a current directory outside the root "(unreachable)".
    let cwd = &p.files().cwd;
    let mut cwd = fs::unrooted(&p.files().root, cwd).unwrap_or_else(|| String::from("(unreachable)") + cwd).into_bytes();
    cwd.push(0);
    if cwd.len() > size {
        return Err(ERANGE);
    }
    user::write(p.space(), buf, &cwd)?;
    Ok(cwd.len() as u64)
}

/// chroot: from now on, "/" is this directory for the calling process and its children. Root only.
pub fn chroot(path_ptr: u64) -> SysResult {
    if !proc::current().creds.is_root() {
        return Err(EPERM);
    }
    let path = resolve(AT_FDCWD, path_ptr)?;
    if !fs::lookup(&path, true)?.is_dir() {
        return Err(ENOTDIR);
    }
    proc::current().files().root = path;
    Ok(0)
}

pub fn chdir(path_ptr: u64) -> SysResult {
    let path = resolve(AT_FDCWD, path_ptr)?;
    if !fs::lookup(&path, true)?.is_dir() {
        return Err(ENOTDIR);
    }
    proc::current().files().cwd = path;
    Ok(0)
}

pub fn fchdir(fd: i32) -> SysResult {
    let p = proc::current();
    let file = p.files().fds.get(fd)?;
    match &file.handle {
        Handle::Node(n) if n.is_dir() => {
            p.files().cwd = file.path.clone();
            Ok(0)
        }
        _ => Err(ENOTDIR),
    }
}

pub fn mkdirat(dirfd: i32, path_ptr: u64, mode: u32) -> SysResult {
    let path = resolve(dirfd, path_ptr)?;
    let (dir, name) = fs::parent_of(&path)?;
    dir.create(&name, New::Dir(mode & 0o7777 & !proc::current().files().umask))?;
    Ok(0)
}

pub fn unlinkat(dirfd: i32, path_ptr: u64, flags: u32) -> SysResult {
    let path = resolve(dirfd, path_ptr)?;
    let (dir, name) = fs::parent_of(&path)?;
    dir.remove(&name, flags & AT_REMOVEDIR != 0)?;
    Ok(0)
}

pub fn renameat(olddir: i32, old_ptr: u64, newdir: i32, new_ptr: u64) -> SysResult {
    let old = resolve(olddir, old_ptr)?;
    let new = resolve(newdir, new_ptr)?;
    if new.starts_with(&(old.clone() + "/")) {
        return Err(EINVAL);
    }
    let (odir, oname) = fs::parent_of(&old)?;
    let (ndir, nname) = fs::parent_of(&new)?;
    fs::rename(&odir, &oname, &ndir, &nname)?;
    Ok(0)
}

/// A hard link: a second name for the same file.
pub fn linkat(olddir: i32, old_ptr: u64, newdir: i32, new_ptr: u64, flags: u32) -> SysResult {
    const AT_SYMLINK_FOLLOW: u32 = 0x400;
    let old = resolve(olddir, old_ptr)?;
    let new = resolve(newdir, new_ptr)?;
    let node = fs::lookup(&old, flags & AT_SYMLINK_FOLLOW != 0)?;
    if node.is_dir() {
        return Err(EPERM);
    }
    let (dir, name) = fs::parent_of(&new)?;
    dir.add_link(&name, &node)?;
    Ok(0)
}

pub fn symlinkat(target_ptr: u64, dirfd: i32, path_ptr: u64) -> SysResult {
    let target = user::read_cstr(proc::current().space(), target_ptr, 4096)?;
    let path = resolve(dirfd, path_ptr)?;
    let (dir, name) = fs::parent_of(&path)?;
    dir.create(&name, New::Symlink(&target))?;
    Ok(0)
}

pub fn readlinkat(dirfd: i32, path_ptr: u64, buf: u64, size: usize) -> SysResult {
    let path = resolve(dirfd, path_ptr)?;
    let target = fs::lookup(&path, false)?.readlink()?;
    let n = target.len().min(size);
    user::write(proc::current().space(), buf, &target.as_bytes()[..n])?;
    Ok(n as u64)
}

pub fn faccessat(dirfd: i32, path_ptr: u64, mode: u32) -> SysResult {
    let path = resolve(dirfd, path_ptr)?;
    let node = fs::lookup(&path, true)?;
    if mode & 1 != 0 && !node.is_dir() && node.mode_bits() & 0o111 == 0 {
        return Err(EACCES);
    }
    Ok(0)
}

pub fn chmod(n: u64, a: [u64; 6]) -> SysResult {
    let (node, mode) = match n {
        90 => (fs::lookup(&resolve(AT_FDCWD, a[0])?, true)?, a[1]),
        91 => match &proc::current().files().fds.get(a[0] as i32)?.handle {
            Handle::Node(n) => (n.clone(), a[1]),
            _ => return Ok(0),
        },
        _ => (fs::lookup(&resolve(a[0] as i32, a[1])?, true)?, a[2]),
    };
    let p = proc::current();
    let (owner, _) = node.owner();
    if !p.creds.is_root() && p.creds.euid != owner {
        return Err(EPERM);
    }
    node.set_mode(mode as u32 & 0o7777)?;
    Ok(0)
}

/// chown, fchown, fchownat: only root may hand a file to somebody else.
pub fn chown(n: u64, a: [u64; 6]) -> SysResult {
    let p = proc::current();
    let (node, uid, gid) = match n {
        92 => (fs::lookup(&resolve(AT_FDCWD, a[0])?, true)?, a[1] as u32, a[2] as u32),
        93 => match &p.files().fds.get(a[0] as i32)?.handle {
            Handle::Node(node) => (node.clone(), a[1] as u32, a[2] as u32),
            _ => return Ok(0),
        },
        _ => (fs::lookup(&resolve(a[0] as i32, a[1])?, a[4] as u32 & AT_SYMLINK_NOFOLLOW == 0)?, a[2] as u32, a[3] as u32),
    };
    if !p.creds.is_root() {
        return Err(EPERM);
    }
    let (old_uid, old_gid) = node.owner();
    node.set_owner(if uid == u32::MAX { old_uid } else { uid }, if gid == u32::MAX { old_gid } else { gid })?;
    Ok(0)
}

fn truncate_node(node: &Node, len: u64) -> SysResult {
    node.truncate(len)?;
    Ok(0)
}

pub fn truncate_path(path_ptr: u64, len: u64) -> SysResult {
    truncate_node(&*fs::lookup(&resolve(AT_FDCWD, path_ptr)?, true)?, len)
}

pub fn ftruncate(fd: i32, len: u64) -> SysResult {
    match &proc::current().files().fds.get(fd)?.handle {
        Handle::Node(n) => truncate_node(n, len),
        _ => Err(EINVAL),
    }
}

pub fn pipe2(fds_ptr: u64, flags: u32) -> SysResult {
    let p = proc::current();
    let pipe = pipe::new();
    let read_end = OpenFile::pipe(&pipe, false);
    let write_end = OpenFile::pipe(&pipe, true);
    if flags & O_NONBLOCK != 0 {
        *read_end.flags.lock() |= O_NONBLOCK;
        *write_end.flags.lock() |= O_NONBLOCK;
    }
    let cloexec = flags & O_CLOEXEC != 0;
    let r = p.files().fds.insert(read_end, cloexec, 0)?;
    let w = match p.files().fds.insert(write_end, cloexec, 0) {
        Ok(w) => w,
        Err(e) => {
            let _ = p.files().fds.close(r);
            return Err(e);
        }
    };
    let pair: [i32; 2] = [r, w];
    user::write_struct(p.space(), fds_ptr, &pair)?;
    Ok(0)
}

/// eventfd and eventfd2: a counter to wake on (fs/eventfd.rs).
pub fn eventfd(initial: u64, flags: u32) -> SysResult {
    const EFD_SEMAPHORE: u32 = 1;
    if flags & !(EFD_SEMAPHORE | O_NONBLOCK | O_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    let file = alloc::sync::Arc::new(OpenFile {
        handle: Handle::EventFd(eventfd::new(initial as u32 as u64, flags & EFD_SEMAPHORE != 0)),
        offset: spin::Mutex::new(0),
        flags: spin::Mutex::new(O_RDWR | flags & O_NONBLOCK),
        path: String::from("anon_inode:[eventfd]"),
    });
    Ok(proc::current().files().fds.insert(file, flags & O_CLOEXEC != 0, 0)? as u64)
}

const POLLIN: i16 = 1;
const POLLOUT: i16 = 4;
const POLLERR: i16 = 8;
const POLLHUP: i16 = 0x10;
const POLLNVAL: i16 = 0x20;
const POLLRDHUP: i16 = 0x2000;

/// What is ready on a descriptor right now, as poll() bits.
fn readiness(fd: i32) -> i16 {
    match proc::current().files().fds.get(fd) {
        Ok(file) => file_readiness(&file),
        Err(_) => POLLNVAL,
    }
}

/// What is ready on an open file right now, as poll() bits (epoll's are the same).
pub fn file_readiness(file: &OpenFile) -> i16 {
    match &file.handle {
        Handle::Node(n) => match n.kind_of() {
            _ => POLLIN | POLLOUT,
        },
        Handle::PipeRead(pipe) => {
            let mut r = 0;
            if !pipe.buffer.lock().is_empty() {
                r |= POLLIN;
            }
            if *pipe.writers.lock() == 0 {
                r |= POLLHUP | POLLRDHUP;
            }
            r
        }
        Handle::Tty(tty, _) => {
            let hung_up = tty.hung_up();
            (if hung_up { POLLHUP } else { POLLOUT }) | if tty.readable() { POLLIN } else { 0 }
        }
        Handle::PtyMaster(tty) => {
            let (read, deserted) = crate::pty::readiness(tty);
            POLLOUT | (if read { POLLIN } else { 0 }) | if deserted { POLLHUP } else { 0 }
        }
        Handle::Socket(sock) => POLLOUT | if sock.queue.lock().is_empty() { 0 } else { POLLIN },
        Handle::Tcp(conn) => {
            let (read, write, failed, gone) = crate::net::tcp::readiness(conn);
            (if read { POLLIN } else { 0 }) | (if write { POLLOUT } else { 0 }) | (if failed { POLLERR } else { 0 }) | (if gone { POLLHUP } else { 0 })
        }
        Handle::Window(window) => {
            let closed = window.closed.load(Ordering::Relaxed);
            let input = !window.input.lock().is_empty();
            POLLOUT | if input || closed { POLLIN } else { 0 } | if closed { POLLHUP } else { 0 }
        }
        Handle::PipeWrite(pipe) => {
            if *pipe.readers.lock() == 0 {
                POLLERR
            } else if pipe.buffer.lock().len() < pipe::CAPACITY {
                POLLOUT
            } else {
                0
            }
        }
        Handle::UnixStream(s) => {
            let r = unix::stream_readiness(s);
            (if r.read { POLLIN } else { 0 })
                | (if r.write { POLLOUT } else { 0 })
                | (if r.read_closed { POLLRDHUP } else { 0 })
                | (if r.hangup { POLLHUP } else { 0 })
        }
        Handle::UnixDatagram(d) => {
            let (read, write) = unix::datagram_readiness(d);
            (if read { POLLIN } else { 0 }) | (if write { POLLOUT } else { 0 })
        }
        Handle::EventFd(e) => {
            let (read, write) = eventfd::readiness(e);
            (if read { POLLIN } else { 0 }) | (if write { POLLOUT } else { 0 })
        }
        Handle::Epoll(ep) => if super::epoll::has_ready(ep) { POLLIN } else { 0 },
        Handle::SignalFd(s) => if super::signalfd::readable(s) { POLLIN } else { 0 },
    }
}

/// Waits until `check` reports something ready or `timeout_ms` passes (negative: forever).
pub fn wait_ready(timeout_ms: i64, mut check: impl FnMut() -> Result<u64, Errno>) -> SysResult {
    let deadline = (timeout_ms >= 0).then(|| crate::arch::irq::ticks() + (timeout_ms as u64).div_ceil(10));
    loop {
        let n = check()?;
        if n > 0 || deadline.is_some_and(|d| crate::arch::irq::ticks() >= d) {
            return Ok(n);
        }
        proc::sleep()?;
    }
}

/// ppoll: poll with a timespec timeout (none: forever). The signal mask isn't applied.
pub fn ppoll(fds_ptr: u64, count: usize, timeout: u64) -> SysResult {
    let timeout_ms = if timeout == 0 {
        -1
    } else {
        let space = proc::current().space();
        let secs = user::read_u64(space, timeout)? as i64;
        let nanos = user::read_u64(space, timeout + 8)? as i64;
        (secs * 1000 + (nanos as u64).div_ceil(1_000_000) as i64).min(i32::MAX as i64) as i32
    };
    poll(fds_ptr, count, timeout_ms)
}

pub fn poll(fds_ptr: u64, count: usize, timeout_ms: i32) -> SysResult {
    if count > 1024 {
        return Err(EINVAL);
    }
    wait_ready(timeout_ms as i64, || {
        let p = proc::current();
        let mut ready = 0;
        for i in 0..count as u64 {
            let at = fds_ptr + i * 8;
            let mut entry = [0u8; 8];
            user::read(p.space(), at, &mut entry)?;
            let fd = i32::from_le_bytes(entry[0..4].try_into().unwrap());
            let events = i16::from_le_bytes(entry[4..6].try_into().unwrap());
            let revents = if fd < 0 { 0 } else { readiness(fd) & (events | POLLERR | POLLHUP | POLLNVAL) };
            user::write(p.space(), at + 6, &revents.to_le_bytes())?;
            if revents != 0 {
                ready += 1;
            }
        }
        Ok(ready)
    })
}

/// select() (timeval timeout) and pselect6() (timespec timeout).
pub fn select(n: usize, readfds: u64, writefds: u64, _exceptfds: u64, timeout: u64, timespec: bool) -> SysResult {
    let p = proc::current();
    let n = n.min(1024);
    let words = n.div_ceil(64);
    let read_set = |ptr: u64| -> Result<Vec<u64>, Errno> {
        if ptr == 0 { Ok(vec![0; words]) } else { (0..words).map(|i| user::read_u64(p.space(), ptr + i as u64 * 8)).collect() }
    };
    let want_read = read_set(readfds)?;
    let want_write = read_set(writefds)?;
    let timeout_ms = if timeout == 0 {
        -1
    } else {
        let secs = user::read_u64(p.space(), timeout)? as i64;
        let frac = user::read_u64(p.space(), timeout + 8)? as i64;
        secs * 1000 + if timespec { frac / 1_000_000 } else { frac / 1000 }
    };
    let mut out_read = vec![0u64; words];
    let mut out_write = vec![0u64; words];
    let count = wait_ready(timeout_ms, || {
        let mut count = 0;
        for fd in 0..n {
            let (w, bit) = (fd / 64, 1u64 << (fd % 64));
            if (want_read[w] | want_write[w]) & bit == 0 {
                continue;
            }
            let r = readiness(fd as i32);
            if want_read[w] & bit != 0 && r & (POLLIN | POLLHUP) != 0 {
                out_read[w] |= bit;
                count += 1;
            }
            if want_write[w] & bit != 0 && r & (POLLOUT | POLLERR) != 0 {
                out_write[w] |= bit;
                count += 1;
            }
        }
        Ok(count)
    })?;
    let p = proc::current();
    for (ptr, set) in [(readfds, &out_read), (writefds, &out_write)] {
        if ptr != 0 {
            for (i, w) in set.iter().enumerate() {
                user::write(p.space(), ptr + i as u64 * 8, &w.to_le_bytes())?;
            }
        }
    }
    Ok(count)
}

pub fn statfs_path(path_ptr: u64, out: u64) -> SysResult {
    let node = fs::lookup(&resolve(AT_FDCWD, path_ptr)?, true)?;
    statfs(&node, out)
}

pub fn fstatfs(fd: i32, out: u64) -> SysResult {
    match &proc::current().files().fds.get(fd)?.handle {
        Handle::Node(node) => statfs(node, out),
        _ => statfs(fs::root(), out),
    }
}

/// `struct statfs` for the file system a node is on: the disk's numbers for ext2, memory otherwise.
fn statfs(node: &Node, out: u64) -> SysResult {
    let (magic, block, total, free) = match &node.kind {
        fs::Kind::Ext2(disk, _) => {
            let (total, free, block) = disk.usage();
            (0xEF53, block, total, free)
        }
        _ => {
            let (free, total) = crate::mm::frame::stats();
            (0x0102_1994, 4096, total, free) // TMPFS_MAGIC
        }
    };
    let mut buf = [0u64; 15];
    buf[0] = magic;
    buf[1] = block;
    buf[2] = total;
    buf[3] = free;
    buf[4] = free;
    buf[5] = 1 << 20;
    buf[6] = 1 << 20;
    buf[8] = 255; // name length
    buf[9] = block;
    user::write_struct(proc::current().space(), out, &buf)?;
    Ok(0)
}
