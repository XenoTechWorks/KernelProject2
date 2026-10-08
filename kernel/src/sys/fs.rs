//! File system calls: open, read, write, stat, directories, pipes, terminal ioctls, poll.

use super::SysResult;
use super::errno::*;
use crate::fs::{self, Device, Kind, Node, pipe};
use crate::mm::user;
use crate::proc::{self, fd::Handle, fd::OpenFile};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const O_ACCMODE: u32 = 3;
const O_WRONLY: u32 = 1;
const O_RDWR: u32 = 2;
const O_CREAT: u32 = 0o100;
const O_EXCL: u32 = 0o200;
const O_NOCTTY: u32 = 0o400;
const O_TRUNC: u32 = 0o1000;
const O_APPEND: u32 = 0o2000;
const O_NONBLOCK: u32 = 0o4000;
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

fn resolve_str(dirfd: i32, path: &str) -> Result<String, Errno> {
    let p = proc::current();
    if path.is_empty() {
        return Err(ENOENT);
    }
    if path.starts_with('/') {
        return Ok(fs::normalize("/", path));
    }
    let base = if dirfd == AT_FDCWD { p.files().cwd.clone() } else { p.files().fds.get(dirfd)?.path.clone() };
    Ok(fs::normalize(&base, path))
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
        Handle::Node(node) => match &node.kind {
            Kind::File(data) => {
                let data = data.lock();
                let mut offset = file.offset.lock();
                let pos = at.unwrap_or(*offset) as usize;
                let n = data.len().saturating_sub(pos).min(buf.len());
                buf[..n].copy_from_slice(&data[pos..pos + n]);
                if at.is_none() {
                    *offset += n as u64;
                }
                Ok(n as u64)
            }
            Kind::Dir(_) => Err(EISDIR),
            Kind::Symlink(_) => Err(EINVAL),
            Kind::Device(Device::Tty) => loop {
                if let Some(n) = crate::tty::try_read(buf) {
                    return Ok(n as u64);
                }
                if nonblocking(file) {
                    return Err(EAGAIN);
                }
                proc::sleep()?;
            },
            Kind::Device(Device::Null) => Ok(0),
            Kind::Device(Device::Zero) => {
                buf.fill(0);
                Ok(buf.len() as u64)
            }
            Kind::Device(Device::Random) => {
                super::misc::fill_random(buf);
                Ok(buf.len() as u64)
            }
        },
        Handle::PipeRead(pipe) => loop {
            {
                let mut q = pipe.buffer.lock();
                if !q.is_empty() {
                    let n = q.len().min(buf.len());
                    for (i, b) in q.drain(..n).enumerate() {
                        buf[i] = b;
                    }
                    drop(q);
                    proc::wake_all();
                    return Ok(n as u64);
                }
            }
            if *pipe.writers.lock() == 0 {
                return Ok(0);
            }
            if nonblocking(file) {
                return Err(EAGAIN);
            }
            proc::sleep()?;
        },
        Handle::PipeWrite(_) => Err(EBADF),
    }
}

fn write_file(file: &OpenFile, data: &[u8], at: Option<u64>) -> SysResult {
    if !writable(file) {
        return Err(EBADF);
    }
    match &file.handle {
        Handle::Node(node) => match &node.kind {
            Kind::File(contents) => {
                let mut contents = contents.lock();
                let mut offset = file.offset.lock();
                let pos = match at {
                    Some(at) => at as usize,
                    None if *file.flags.lock() & O_APPEND != 0 => contents.len(),
                    None => *offset as usize,
                };
                if contents.len() < pos + data.len() {
                    contents.resize(pos + data.len(), 0);
                }
                contents[pos..pos + data.len()].copy_from_slice(data);
                if at.is_none() {
                    *offset = (pos + data.len()) as u64;
                }
                node.touch();
                Ok(data.len() as u64)
            }
            Kind::Dir(_) => Err(EISDIR),
            Kind::Symlink(_) => Err(EINVAL),
            Kind::Device(Device::Tty) => {
                crate::tty::write(data);
                Ok(data.len() as u64)
            }
            Kind::Device(_) => Ok(data.len() as u64),
        },
        Handle::PipeWrite(pipe) => {
            let mut written = 0;
            while written < data.len() {
                if *pipe.readers.lock() == 0 {
                    proc::signal::send(proc::current(), proc::SIGPIPE);
                    return if written > 0 { Ok(written as u64) } else { Err(EPIPE) };
                }
                {
                    let mut q = pipe.buffer.lock();
                    let room = pipe::CAPACITY.saturating_sub(q.len());
                    let n = room.min(data.len() - written);
                    q.extend(&data[written..written + n]);
                    written += n;
                }
                proc::wake_all();
                if written < data.len() {
                    if nonblocking(file) {
                        return if written > 0 { Ok(written as u64) } else { Err(EAGAIN) };
                    }
                    proc::sleep()?;
                }
            }
            Ok(written as u64)
        }
        Handle::PipeRead(_) => Err(EBADF),
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
    if !matches!(&file.handle, Handle::Node(n) if matches!(n.kind, Kind::File(_))) {
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
    if !matches!(&file.handle, Handle::Node(n) if matches!(n.kind, Kind::File(_))) {
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
            let node = Node::file(Vec::new(), mode & !p.files().umask);
            fs::link(&dir, &name, node.clone())?;
            node
        }
        Err(e) => return Err(e),
    };
    if flags & O_PATH == 0 {
        if flags & O_DIRECTORY != 0 && !node.is_dir() {
            return Err(ENOTDIR);
        }
        if node.is_dir() && flags & O_ACCMODE != 0 {
            return Err(EISDIR);
        }
        if matches!(node.kind, Kind::Symlink(_)) {
            return Err(ELOOP);
        }
        if flags & O_TRUNC != 0 && flags & O_ACCMODE != 0 {
            if let Kind::File(data) = &node.kind {
                data.lock().clear();
                node.touch();
            }
        }
    }
    let kept = flags & !(O_CREAT | O_EXCL | O_NOCTTY | O_TRUNC | O_CLOEXEC);
    let file = OpenFile::node(node, kept, String::from(path));
    Ok(p.files().fds.insert(file, flags & O_CLOEXEC != 0, 0)? as u64)
}

pub fn close(fd: i32) -> SysResult {
    proc::current().files().fds.close(fd)?;
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
    if matches!(node.kind, Kind::Device(_)) {
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

fn rdev(node: &Node) -> u64 {
    let makedev = |major: u64, minor: u64| (major << 8) | minor;
    match node.kind {
        Kind::Device(Device::Tty) => makedev(5, 1),
        Kind::Device(Device::Null) => makedev(1, 3),
        Kind::Device(Device::Zero) => makedev(1, 5),
        Kind::Device(Device::Random) => makedev(1, 9),
        _ => 0,
    }
}

fn stat_of(handle: &Handle) -> Stat {
    match handle {
        Handle::Node(node) => {
            let size = node.size() as i64;
            let mtime = node.mtime.load(core::sync::atomic::Ordering::Relaxed) as i64;
            Stat {
                dev: 1,
                ino: node.ino,
                nlink: if node.is_dir() { 2 } else { 1 },
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
            Some(f) => match &f.handle {
                Handle::Node(n) => Handle::Node(n.clone()),
                Handle::PipeRead(pp) => Handle::PipeRead(pp.clone()),
                Handle::PipeWrite(pp) => Handle::PipeWrite(pp.clone()),
            },
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
    let Kind::Dir(entries) = &node.kind else { return Err(ENOTDIR) };
    let mut list: Vec<(String, u64, u8)> = vec![(".".into(), node.ino, 4), ("..".into(), node.ino, 4)];
    for (name, child) in entries.lock().iter() {
        let kind = match child.kind {
            Kind::Dir(_) => 4,
            Kind::File(_) => 8,
            Kind::Symlink(_) => 10,
            Kind::Device(_) => 2,
        };
        list.push((name.clone(), child.ino, kind));
    }
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
    const TCGETS: u32 = 0x5401;
    const TCSETS: u32 = 0x5402;
    const TCSETSW: u32 = 0x5403;
    const TCSETSF: u32 = 0x5404;
    const TIOCSCTTY: u32 = 0x540E;
    const TIOCGPGRP: u32 = 0x540F;
    const TIOCSPGRP: u32 = 0x5410;
    const TIOCGWINSZ: u32 = 0x5413;
    const TIOCSWINSZ: u32 = 0x5414;
    const FIONREAD: u32 = 0x541B;
    const TIOCNOTTY: u32 = 0x5422;
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
    let is_tty = matches!(&file.handle, Handle::Node(n) if matches!(n.kind, Kind::Device(Device::Tty)));
    if !is_tty {
        return Err(ENOTTY);
    }
    match request {
        TCGETS => {
            user::write_struct(p.space(), arg, &crate::tty::termios())?;
            Ok(0)
        }
        TCSETS | TCSETSW | TCSETSF => {
            let t: crate::tty::Termios = user::read_struct(p.space(), arg)?;
            crate::tty::set_termios(t);
            Ok(0)
        }
        TIOCGWINSZ => {
            let (cols, rows) = crate::console::size();
            let ws: [u16; 4] = [rows as u16, cols as u16, 0, 0];
            user::write_struct(p.space(), arg, &ws)?;
            Ok(0)
        }
        TIOCSWINSZ | TIOCSCTTY | TIOCNOTTY => Ok(0),
        TIOCGPGRP => {
            user::write_struct(p.space(), arg, &crate::tty::foreground())?;
            Ok(0)
        }
        TIOCSPGRP => {
            let pgid: u32 = user::read_struct(p.space(), arg)?;
            crate::tty::set_foreground(pgid);
            Ok(0)
        }
        FIONREAD => {
            user::write_struct(p.space(), arg, &(crate::tty::available() as u32))?;
            Ok(0)
        }
        _ => Err(EINVAL),
    }
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
    let mut cwd = p.files().cwd.clone().into_bytes();
    cwd.push(0);
    if cwd.len() > size {
        return Err(ERANGE);
    }
    user::write(p.space(), buf, &cwd)?;
    Ok(cwd.len() as u64)
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
    let node = Node::dir();
    *node.mode.lock() = mode & 0o7777 & !proc::current().files().umask;
    fs::link(&dir, &name, node)?;
    Ok(0)
}

pub fn unlinkat(dirfd: i32, path_ptr: u64, flags: u32) -> SysResult {
    let path = resolve(dirfd, path_ptr)?;
    let (dir, name) = fs::parent_of(&path)?;
    let Kind::Dir(entries) = &dir.kind else { return Err(ENOTDIR) };
    let mut entries = entries.lock();
    let node = entries.get(&name).ok_or(ENOENT)?;
    match (&node.kind, flags & AT_REMOVEDIR != 0) {
        (Kind::Dir(children), true) => {
            if !children.lock().is_empty() {
                return Err(ENOTEMPTY);
            }
        }
        (Kind::Dir(_), false) => return Err(EISDIR),
        (_, true) => return Err(ENOTDIR),
        _ => {}
    }
    entries.remove(&name);
    dir.touch();
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
    let (Kind::Dir(oentries), Kind::Dir(nentries)) = (&odir.kind, &ndir.kind) else { return Err(ENOTDIR) };
    let node = oentries.lock().remove(&oname).ok_or(ENOENT)?;
    if let Some(existing) = nentries.lock().get(&nname) {
        if let Kind::Dir(c) = &existing.kind {
            if !c.lock().is_empty() {
                oentries.lock().insert(oname, node);
                return Err(ENOTEMPTY);
            }
        }
    }
    nentries.lock().insert(nname, node);
    odir.touch();
    ndir.touch();
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
    fs::link(&dir, &name, node)?;
    Ok(0)
}

pub fn symlinkat(target_ptr: u64, dirfd: i32, path_ptr: u64) -> SysResult {
    let target = user::read_cstr(proc::current().space(), target_ptr, 4096)?;
    let path = resolve(dirfd, path_ptr)?;
    let (dir, name) = fs::parent_of(&path)?;
    fs::link(&dir, &name, Node::new(Kind::Symlink(target), 0o777))?;
    Ok(0)
}

pub fn readlinkat(dirfd: i32, path_ptr: u64, buf: u64, size: usize) -> SysResult {
    let path = resolve(dirfd, path_ptr)?;
    let node = fs::lookup(&path, false)?;
    let Kind::Symlink(target) = &node.kind else { return Err(EINVAL) };
    let n = target.len().min(size);
    user::write(proc::current().space(), buf, &target.as_bytes()[..n])?;
    Ok(n as u64)
}

pub fn faccessat(dirfd: i32, path_ptr: u64, mode: u32) -> SysResult {
    let path = resolve(dirfd, path_ptr)?;
    let node = fs::lookup(&path, true)?;
    if mode & 1 != 0 && !node.is_dir() && *node.mode.lock() & 0o111 == 0 {
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
    *node.mode.lock() = mode as u32 & 0o7777;
    Ok(0)
}

fn truncate_node(node: &Node, len: u64) -> SysResult {
    match &node.kind {
        Kind::File(data) => {
            data.lock().resize(len as usize, 0);
            node.touch();
            Ok(0)
        }
        Kind::Dir(_) => Err(EISDIR),
        _ => Err(EINVAL),
    }
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

const POLLIN: i16 = 1;
const POLLOUT: i16 = 4;
const POLLERR: i16 = 8;
const POLLHUP: i16 = 0x10;
const POLLNVAL: i16 = 0x20;

/// What is ready on a descriptor right now, as poll() bits.
fn readiness(fd: i32) -> i16 {
    let Ok(file) = proc::current().files().fds.get(fd) else { return POLLNVAL };
    match &file.handle {
        Handle::Node(n) => match n.kind {
            Kind::Device(Device::Tty) => POLLOUT | if crate::tty::available() > 0 { POLLIN } else { 0 },
            _ => POLLIN | POLLOUT,
        },
        Handle::PipeRead(pipe) => {
            let mut r = 0;
            if !pipe.buffer.lock().is_empty() {
                r |= POLLIN;
            }
            if *pipe.writers.lock() == 0 {
                r |= POLLHUP;
            }
            r
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
    }
}

/// Waits until `check` reports something ready or `timeout_ms` passes (negative: forever).
fn wait_ready(timeout_ms: i64, mut check: impl FnMut() -> Result<u64, Errno>) -> SysResult {
    let deadline = (timeout_ms >= 0).then(|| crate::arch::irq::ticks() + (timeout_ms as u64).div_ceil(10));
    loop {
        let n = check()?;
        if n > 0 || deadline.is_some_and(|d| crate::arch::irq::ticks() >= d) {
            return Ok(n);
        }
        proc::sleep()?;
    }
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

pub fn statfs(out: u64) -> SysResult {
    let (free, total) = crate::mm::frame::stats();
    let mut buf = [0u64; 15];
    buf[0] = 0x0102_1994; // TMPFS_MAGIC
    buf[1] = 4096;
    buf[2] = total;
    buf[3] = free;
    buf[4] = free;
    buf[5] = 1 << 20;
    buf[6] = 1 << 20;
    buf[8] = 255; // name length
    buf[9] = 4096;
    user::write_struct(proc::current().space(), out, &buf)?;
    Ok(0)
}
