//! The socket system calls, the Linux ones: socket, socketpair, bind, listen, accept, connect, send
//! and receive (in all their forms), shutdown, and the get/set calls. IPv4 (TCP streams, UDP
//! datagrams, and ICMP datagrams for ping) and Unix sockets (streams and datagrams, net/unix.rs).

use super::SysResult;
use super::errno::*;
use crate::fs::{self, New, Type};
use crate::mm::user;
use crate::net::socket::{self, Socket};
use crate::net::tcp::{self, Conn};
use crate::net::unix::{self, Name};
use crate::net::{Ip, PROTO_ICMP, PROTO_UDP};
use crate::proc::{self, fd::Handle, fd::OpenFile};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

const AF_UNIX: u32 = 1;
const AF_INET: u32 = 2;
const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;
const SOCK_RAW: u32 = 3;
const SOCK_NONBLOCK: u32 = 0o4000;
const SOCK_CLOEXEC: u32 = 0o2000000;
const IPPROTO_TCP: u32 = 6;

const MSG_PEEK: u32 = 0x2;
const MSG_CTRUNC: u32 = 0x8;
const MSG_TRUNC: u32 = 0x20;
const MSG_DONTWAIT: u32 = 0x40;
const MSG_NOSIGNAL: u32 = 0x4000;
const MSG_CMSG_CLOEXEC: u32 = 0x40000000;

/// Ancillary data: open files passed along (SCM_RIGHTS), and credentials (accepted, not used).
const SCM_RIGHTS: i32 = 1;
const SCM_CREDENTIALS: i32 = 2;
/// The most files one message carries (Linux's SCM_MAX_FD).
const MAX_FILES: usize = 253;

const SOL_SOCKET: u32 = 1;
const SO_TYPE: u32 = 3;
const SO_ERROR: u32 = 4;
const SO_SNDBUF: u32 = 7;
const SO_RCVBUF: u32 = 8;
const SO_PEERCRED: u32 = 17;
const IPPROTO_IP: u32 = 0;
const IP_OPTIONS: u32 = 4;

/// Linux's `struct sockaddr_in`: family, port and address, the last two in network order.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct SockAddrIn {
    family: u16,
    port: u16,
    addr: [u8; 4],
    zero: [u8; 8],
}

fn read_addr(at: u64, len: u64) -> Result<(Ip, u16), Errno> {
    if at == 0 || len < 8 {
        return Err(EINVAL);
    }
    let a: SockAddrIn = user::read_struct(proc::current().space(), at)?;
    if a.family != AF_INET as u16 {
        return Err(EAFNOSUPPORT);
    }
    Ok((a.addr, u16::from_be(a.port)))
}

fn write_addr(at: u64, len_at: u64, ip: Ip, port: u16) -> Result<(), Errno> {
    if at == 0 {
        return Ok(());
    }
    let p = proc::current();
    let a = SockAddrIn { family: AF_INET as u16, port: port.to_be(), addr: ip, zero: [0; 8] };
    user::write_struct(p.space(), at, &a)?;
    if len_at != 0 {
        user::write_struct(p.space(), len_at, &(core::mem::size_of::<SockAddrIn>() as u32))?;
    }
    Ok(())
}

/// A Unix socket address as a program gives it (`struct sockaddr_un`): a file's path, made
/// absolute, or an abstract name (starting with a 0 byte).
enum UnixAddr {
    Path(String),
    Abstract(Vec<u8>),
}

fn read_unix_addr(at: u64, len: u64) -> Result<UnixAddr, Errno> {
    // Two bytes of family, then up to 108 of path.
    if at == 0 || len <= 2 || len > 110 {
        return Err(EINVAL);
    }
    let raw = user::read_vec(proc::current().space(), at, len as usize)?;
    if u16::from_le_bytes([raw[0], raw[1]]) != AF_UNIX as u16 {
        return Err(EINVAL);
    }
    let path = &raw[2..];
    if path[0] == 0 {
        return Ok(UnixAddr::Abstract(path[1..].to_vec()));
    }
    let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
    let text = core::str::from_utf8(&path[..end]).map_err(|_| EINVAL)?;
    Ok(UnixAddr::Path(super::fs::resolve_str(AT_FDCWD, text)?))
}

/// The name of a socket that's already there, to connect or send to.
fn existing_name(addr: UnixAddr) -> Result<Name, Errno> {
    match addr {
        UnixAddr::Abstract(bytes) => Ok(Name::Abstract(bytes)),
        UnixAddr::Path(path) => {
            let node = fs::lookup(&path, true)?;
            if node.kind_of() != Type::Socket {
                return Err(ECONNREFUSED);
            }
            if !super::fs::allowed(&node, 2) {
                return Err(EACCES);
            }
            let id = (node.dev(), node.inode_number());
            Ok(Name::Path(path, id))
        }
    }
}

/// A new name, for bind: a path gets its socket file made now, and mustn't exist already.
fn new_name(addr: UnixAddr) -> Result<Name, Errno> {
    match addr {
        UnixAddr::Abstract(bytes) => Ok(Name::Abstract(bytes)),
        UnixAddr::Path(path) => {
            let (dir, file) = fs::parent_of(&path)?;
            let mode = 0o777 & !proc::current().files().umask;
            let node = dir.create(&file, New::Socket(mode)).map_err(|e| if e == EEXIST { EADDRINUSE } else { e })?;
            let id = (node.dev(), node.inode_number());
            Ok(Name::Path(path, id))
        }
    }
}

/// Writes an address for the program, cut to the room it gave (the length it passed in at
/// `len_at`), and the address's full length there, as Linux does.
fn write_sockaddr(at: u64, len_at: u64, raw: &[u8]) -> Result<(), Errno> {
    if at == 0 {
        return Ok(());
    }
    let space = proc::current().space();
    let room = if len_at != 0 { user::read_struct::<u32>(space, len_at)? as usize } else { raw.len() };
    user::write(space, at, &raw[..raw.len().min(room)])?;
    if len_at != 0 {
        user::write_struct(space, len_at, &(raw.len() as u32))?;
    }
    Ok(())
}

fn write_unix_addr(at: u64, len_at: u64, name: Option<&Name>) -> Result<(), Errno> {
    let mut raw = (AF_UNIX as u16).to_le_bytes().to_vec();
    match name {
        Some(Name::Path(path, _)) => {
            let root = fs::current_root();
            raw.extend(fs::unrooted(&root, path).as_deref().unwrap_or(path).as_bytes());
            raw.push(0);
        }
        Some(Name::Abstract(bytes)) => {
            raw.push(0);
            raw.extend(bytes);
        }
        None => {} // unnamed: the family alone
    }
    write_sockaddr(at, len_at, &raw)
}

enum Sock {
    Datagram(Arc<Socket>),
    Stream(Arc<Conn>),
    UnixStream(Arc<unix::Stream>),
    UnixDatagram(Arc<unix::Datagram>),
}

fn sock_of(fd: i32) -> Result<(Arc<OpenFile>, Sock), Errno> {
    let file = proc::current().files().fds.get(fd)?;
    let sock = match &file.handle {
        Handle::Socket(s) => Sock::Datagram(s.clone()),
        Handle::Tcp(c) => Sock::Stream(c.clone()),
        Handle::UnixStream(s) => Sock::UnixStream(s.clone()),
        Handle::UnixDatagram(d) => Sock::UnixDatagram(d.clone()),
        _ => return Err(ENOTSOCK),
    };
    Ok((file, sock))
}

fn nonblocking(file: &OpenFile, flags: u32) -> bool {
    *file.flags.lock() & super::fs::O_NONBLOCK != 0 || flags & MSG_DONTWAIT != 0
}

/// A socket's descriptor: open for reading and writing (O_RDWR), maybe non-blocking.
fn new_fd(handle: Handle, flags: u32) -> SysResult {
    let open = Arc::new(OpenFile {
        handle,
        offset: spin::Mutex::new(0),
        flags: spin::Mutex::new(2 | if flags & SOCK_NONBLOCK != 0 { super::fs::O_NONBLOCK } else { 0 }),
        path: String::from("socket:"),
    });
    Ok(proc::current().files().fds.insert(open, flags & SOCK_CLOEXEC != 0, 0)? as u64)
}

pub fn socket(family: u32, kind: u32, protocol: u32) -> SysResult {
    let flags = kind & (SOCK_NONBLOCK | SOCK_CLOEXEC);
    if family == AF_UNIX {
        let handle = match kind & 0xf {
            _ if protocol != 0 => return Err(EPROTONOSUPPORT),
            SOCK_STREAM => Handle::UnixStream(unix::Stream::new()),
            SOCK_DGRAM => Handle::UnixDatagram(unix::Datagram::new()),
            _ => return Err(EPROTONOSUPPORT),
        };
        return new_fd(handle, flags);
    }
    if family != AF_INET {
        return Err(EAFNOSUPPORT);
    }
    if !crate::net::up() {
        return Err(ENETDOWN);
    }
    let icmp = protocol == PROTO_ICMP as u32;
    let handle = match kind & 0xf {
        SOCK_STREAM if protocol == 0 || protocol == IPPROTO_TCP => Handle::Tcp(tcp::new()),
        SOCK_DGRAM | SOCK_RAW if icmp => {
            // A ping socket needs an id straight away: it's how replies find their way back.
            let sock = Socket::new(PROTO_ICMP);
            *sock.port.lock() = socket::free_port();
            socket::add(&sock);
            Handle::Socket(sock)
        }
        SOCK_DGRAM if protocol == 0 || protocol == PROTO_UDP as u32 => {
            let sock = Socket::new(PROTO_UDP);
            socket::add(&sock);
            Handle::Socket(sock)
        }
        _ => return Err(EPROTONOSUPPORT),
    };
    new_fd(handle, flags)
}

/// Two Unix sockets already connected to each other (only Unix sockets come in pairs).
pub fn socketpair(family: u32, kind: u32, protocol: u32, fds_at: u64) -> SysResult {
    if family != AF_UNIX {
        return Err(EOPNOTSUPP);
    }
    if protocol != 0 {
        return Err(EPROTONOSUPPORT);
    }
    let flags = kind & (SOCK_NONBLOCK | SOCK_CLOEXEC);
    let (a, b) = match kind & 0xf {
        SOCK_STREAM => {
            let (a, b) = unix::Stream::pair();
            (Handle::UnixStream(a), Handle::UnixStream(b))
        }
        SOCK_DGRAM => {
            let (a, b) = unix::Datagram::pair();
            (Handle::UnixDatagram(a), Handle::UnixDatagram(b))
        }
        _ => return Err(EPROTONOSUPPORT),
    };
    let first = new_fd(a, flags)? as i32;
    let second = match new_fd(b, flags) {
        Ok(fd) => fd as i32,
        Err(e) => {
            let _ = proc::current().files().fds.close(first);
            return Err(e);
        }
    };
    user::write_struct(proc::current().space(), fds_at, &[first, second])?;
    Ok(0)
}

pub fn bind(fd: i32, at: u64, len: u64) -> SysResult {
    match sock_of(fd)?.1 {
        Sock::Datagram(s) => {
            let (_, port) = read_addr(at, len)?;
            *s.port.lock() = if port == 0 { socket::free_port() } else { port };
        }
        Sock::Stream(c) => tcp::bind(&c, read_addr(at, len)?.1)?,
        Sock::UnixStream(s) => {
            if unix::stream_name(&s).is_some() {
                return Err(EINVAL);
            }
            unix::bind_stream(&s, new_name(read_unix_addr(at, len)?)?)?;
        }
        Sock::UnixDatagram(d) => {
            if unix::datagram_name(&d).is_some() {
                return Err(EINVAL);
            }
            unix::bind_datagram(&d, new_name(read_unix_addr(at, len)?)?)?;
        }
    }
    Ok(0)
}

pub fn listen(fd: i32, backlog: u32) -> SysResult {
    match sock_of(fd)?.1 {
        Sock::Stream(c) => tcp::listen(&c, backlog as usize).map(|_| 0),
        Sock::UnixStream(s) => unix::listen(&s, backlog as usize).map(|_| 0),
        Sock::Datagram(_) | Sock::UnixDatagram(_) => Err(EOPNOTSUPP),
    }
}

pub fn accept(fd: i32, at: u64, len_at: u64, flags: u32) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    match sock {
        Sock::Stream(listener) => loop {
            let next = listener.tcb.lock().accept_queue.pop_front();
            if let Some(child) = next {
                if let Some((ip, port)) = tcp::peer(&child) {
                    write_addr(at, len_at, ip, port)?;
                }
                return new_fd(Handle::Tcp(child), flags);
            }
            if listener.tcb.lock().state != tcp::State::Listen {
                return Err(EINVAL);
            }
            if nonblocking(&file, 0) {
                return Err(EAGAIN);
            }
            proc::sleep()?;
        },
        Sock::UnixStream(listener) => {
            let conn = unix::accept(&listener, nonblocking(&file, 0))?;
            write_unix_addr(at, len_at, unix::stream_peer(&conn)?.0.as_ref())?;
            new_fd(Handle::UnixStream(conn), flags)
        }
        Sock::Datagram(_) | Sock::UnixDatagram(_) => Err(EOPNOTSUPP),
    }
}

pub fn connect(fd: i32, at: u64, len: u64) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    match sock {
        Sock::Datagram(s) => {
            *s.peer.lock() = Some(read_addr(at, len)?);
            if *s.port.lock() == 0 {
                *s.port.lock() = socket::free_port();
            }
            Ok(0)
        }
        Sock::Stream(c) => {
            let (ip, port) = read_addr(at, len)?;
            tcp::connect(&c, ip, port)?;
            if nonblocking(&file, 0) {
                return Err(EINPROGRESS);
            }
            loop {
                let state = c.tcb.lock().state;
                match state {
                    tcp::State::Established | tcp::State::CloseWait => return Ok(0),
                    tcp::State::Closed => return Err(tcp::take_error(&c).unwrap_or(ECONNREFUSED)),
                    _ => proc::sleep()?,
                }
            }
        }
        Sock::UnixStream(s) => {
            let name = existing_name(read_unix_addr(at, len)?)?;
            unix::connect(&s, &name, nonblocking(&file, 0)).map(|_| 0)
        }
        Sock::UnixDatagram(d) => {
            let name = existing_name(read_unix_addr(at, len)?)?;
            unix::connect_datagram(&d, &name).map(|_| 0)
        }
    }
}

pub fn shutdown(fd: i32, how: u32) -> SysResult {
    if how > 2 {
        return Err(EINVAL);
    }
    match sock_of(fd)?.1 {
        Sock::Stream(c) if how != 0 => {
            tcp::shutdown_write(&c); // SHUT_WR or SHUT_RDWR
        }
        Sock::UnixStream(s) => unix::shutdown(&s, how)?,
        _ => {}
    }
    Ok(0)
}

pub fn getsockname(fd: i32, at: u64, len_at: u64) -> SysResult {
    let port = match sock_of(fd)?.1 {
        Sock::Datagram(s) => *s.port.lock(),
        Sock::Stream(c) => tcp::local_port(&c),
        Sock::UnixStream(s) => return write_unix_addr(at, len_at, unix::stream_name(&s).as_ref()).map(|_| 0),
        Sock::UnixDatagram(d) => return write_unix_addr(at, len_at, unix::datagram_name(&d).as_ref()).map(|_| 0),
    };
    write_addr(at, len_at, crate::net::config().ip, port)?;
    Ok(0)
}

pub fn getpeername(fd: i32, at: u64, len_at: u64) -> SysResult {
    let peer = match sock_of(fd)?.1 {
        Sock::Datagram(s) => *s.peer.lock(),
        Sock::Stream(c) => tcp::peer(&c),
        Sock::UnixStream(s) => return write_unix_addr(at, len_at, unix::stream_peer(&s)?.0.as_ref()).map(|_| 0),
        Sock::UnixDatagram(d) => return write_unix_addr(at, len_at, unix::datagram_peer(&d)?.0.as_ref()).map(|_| 0),
    };
    let (ip, port) = peer.ok_or(ENOTCONN)?;
    write_addr(at, len_at, ip, port)?;
    Ok(0)
}

// ---- sending and receiving, shared by every form of the calls ----

/// Sends from a kernel buffer, to the address at `to` (if any) or to whoever the socket is
/// connected to. A stream ignores the address.
fn send_on(file: &OpenFile, sock: &Sock, data: &[u8], to: u64, to_len: u64, files: Vec<Arc<OpenFile>>, flags: u32) -> SysResult {
    if !files.is_empty() && !matches!(sock, Sock::UnixStream(_) | Sock::UnixDatagram(_)) {
        return Err(EINVAL);
    }
    match sock {
        Sock::Datagram(s) => {
            let (ip, port) = match to {
                0 => s.peer.lock().ok_or(EDESTADDRREQ)?,
                _ => read_addr(to, to_len)?,
            };
            if *s.port.lock() == 0 {
                *s.port.lock() = socket::free_port();
            }
            let n = data.len().min(65507);
            socket::send(s, ip, port, &data[..n]);
            Ok(n as u64)
        }
        Sock::Stream(c) => stream_write(file, c, data, flags),
        Sock::UnixStream(s) => match unix::send_stream(s, data, files, nonblocking(file, flags)) {
            Err(EPIPE) => broken_pipe(0, flags),
            result => Ok(result? as u64),
        },
        Sock::UnixDatagram(d) => {
            let target = if to != 0 { Some(existing_name(read_unix_addr(to, to_len)?)?) } else { None };
            Ok(unix::send(d, target.as_ref(), data, files, nonblocking(file, flags))? as u64)
        }
    }
}

/// Who a received message came from.
enum Sender {
    None,
    Inet(Ip, u16),
    Unix(Option<Name>),
}

fn write_sender(at: u64, len_at: u64, sender: &Sender) -> Result<(), Errno> {
    match sender {
        Sender::None => Ok(()),
        Sender::Inet(ip, port) => write_addr(at, len_at, *ip, *port),
        Sender::Unix(name) => write_unix_addr(at, len_at, name.as_ref()),
    }
}

/// Receives into a kernel buffer: how many bytes (the whole datagram's length with MSG_TRUNC),
/// who sent them, and files that came with them.
fn recv_on(file: &OpenFile, sock: &Sock, buf: &mut [u8], flags: u32) -> Result<(usize, Sender, Vec<Arc<OpenFile>>), Errno> {
    match sock {
        Sock::Datagram(s) => loop {
            {
                let mut queue = s.queue.lock();
                if let Some(packet) = queue.front() {
                    let n = packet.data.len().min(buf.len());
                    buf[..n].copy_from_slice(&packet.data[..n]);
                    let full = if flags & MSG_TRUNC != 0 { packet.data.len() } else { n };
                    let from = Sender::Inet(packet.from, packet.port);
                    if flags & MSG_PEEK == 0 {
                        queue.pop_front();
                    }
                    return Ok((full, from, Vec::new()));
                }
            }
            if nonblocking(file, flags) {
                return Err(EAGAIN);
            }
            proc::sleep()?;
        },
        Sock::Stream(c) => Ok((stream_read(file, c, buf, flags)? as usize, Sender::None, Vec::new())),
        Sock::UnixStream(s) => {
            let (n, files) = unix::receive(s, buf, nonblocking(file, flags), flags & MSG_PEEK != 0)?;
            Ok((n, Sender::None, files))
        }
        Sock::UnixDatagram(d) => {
            let got = unix::recv(d, buf, nonblocking(file, flags), flags & MSG_PEEK != 0)?;
            Ok((if flags & MSG_TRUNC != 0 { got.full } else { got.len }, Sender::Unix(got.from), got.files))
        }
    }
}

/// read() and recv() on a TCP socket.
pub fn stream_read(file: &OpenFile, conn: &Arc<Conn>, buf: &mut [u8], flags: u32) -> SysResult {
    loop {
        match tcp::read(conn, buf, flags & MSG_PEEK != 0)? {
            Some(n) => return Ok(n as u64),
            None if nonblocking(file, flags) => return Err(EAGAIN),
            None => proc::sleep()?,
        }
    }
}

/// write() and send() on a TCP socket: everything, unless it can't wait.
pub fn stream_write(file: &OpenFile, conn: &Arc<Conn>, data: &[u8], flags: u32) -> SysResult {
    let mut written = 0;
    while written < data.len() {
        match tcp::write(conn, &data[written..]) {
            Ok(0) => {
                if nonblocking(file, flags) {
                    return if written > 0 { Ok(written as u64) } else { Err(EAGAIN) };
                }
                if let Err(e) = proc::sleep() {
                    return if written > 0 { Ok(written as u64) } else { Err(e) };
                }
            }
            Ok(n) => written += n,
            Err(EPIPE) => return broken_pipe(written, flags),
            Err(e) => return if written > 0 { Ok(written as u64) } else { Err(e) },
        }
    }
    Ok(written as u64)
}

/// write() and send() on a Unix stream socket.
pub fn unix_write(file: &OpenFile, s: &unix::Stream, data: &[u8], flags: u32) -> SysResult {
    match unix::send_stream(s, data, Vec::new(), nonblocking(file, flags)) {
        Err(EPIPE) => broken_pipe(0, flags),
        result => Ok(result? as u64),
    }
}

/// Writing to a connection the other side has closed: SIGPIPE, unless asked not to.
fn broken_pipe(written: usize, flags: u32) -> SysResult {
    if flags & MSG_NOSIGNAL == 0 {
        proc::signal::send(proc::current(), proc::SIGPIPE);
    }
    if written > 0 { Ok(written as u64) } else { Err(EPIPE) }
}

pub fn sendto(fd: i32, buf: u64, len: usize, flags: u32, to: u64, to_len: u64) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    let data = user::read_vec(proc::current().space(), buf, len.min(1 << 20))?;
    send_on(&file, &sock, &data, to, to_len, Vec::new(), flags)
}

pub fn recvfrom(fd: i32, buf: u64, len: usize, flags: u32, from: u64, from_len: u64) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    let mut tmp = vec![0u8; len.min(1 << 20)];
    let (n, sender, _) = recv_on(&file, &sock, &mut tmp, flags)?; // files without recvmsg are closed
    user::write(proc::current().space(), buf, &tmp[..n.min(tmp.len())])?;
    write_sender(from, from_len, &sender)?;
    Ok(n as u64)
}

// ---- sendmsg / recvmsg, and their batch forms ----

/// `struct msghdr`: where to, the pieces of the message (an iovec), and ancillary data: a list of
/// `struct cmsghdr` (length, level, type, then the data, each padded to 8 bytes).
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct MsgHdr {
    name: u64,
    name_len: u32,
    _pad: u32,
    iov: u64,
    iov_len: u64,
    control: u64,
    control_len: u64,
    flags: i32,
    _pad2: u32,
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct IoVec {
    base: u64,
    len: u64,
}

fn iovecs(m: &MsgHdr) -> Result<Vec<IoVec>, Errno> {
    if m.iov_len > 1024 {
        return Err(EINVAL);
    }
    let space = proc::current().space();
    (0..m.iov_len).map(|i| user::read_struct::<IoVec>(space, m.iov + i * 16)).collect()
}

/// The open files a message's ancillary data passes along (SCM_RIGHTS).
fn files_to_send(m: &MsgHdr) -> Result<Vec<Arc<OpenFile>>, Errno> {
    let p = proc::current();
    let control = user::read_vec(p.space(), m.control, (m.control_len as usize).min(4096))?;
    let mut files = Vec::new();
    let mut at = 0;
    while at + 16 <= control.len() {
        let len = u64::from_le_bytes(control[at..at + 8].try_into().unwrap()) as usize;
        let level = i32::from_le_bytes(control[at + 8..at + 12].try_into().unwrap());
        let kind = i32::from_le_bytes(control[at + 12..at + 16].try_into().unwrap());
        if len < 16 || at + len > control.len() {
            return Err(EINVAL);
        }
        match (level, kind) {
            (1, SCM_RIGHTS) => {
                for fd in control[at + 16..at + len].chunks_exact(4) {
                    files.push(p.files().fds.get(i32::from_le_bytes(fd.try_into().unwrap()))?);
                }
            }
            (1, SCM_CREDENTIALS) => {} // the kernel knows who's sending; nothing to pass on
            _ => return Err(EINVAL),
        }
        at += len.next_multiple_of(8);
    }
    if files.len() > MAX_FILES {
        return Err(EINVAL);
    }
    Ok(files)
}

/// Gives received files descriptors here, and describes them in the receiver's ancillary data
/// buffer. Returns the bytes of it used, and whether some didn't fit (those are closed).
fn files_received(m: &MsgHdr, files: Vec<Arc<OpenFile>>, flags: u32) -> Result<(usize, bool), Errno> {
    if files.is_empty() {
        return Ok((0, false));
    }
    let p = proc::current();
    let room = (m.control_len as usize).saturating_sub(16) / 4;
    let (count, fit) = (files.len(), files.len().min(room));
    if fit == 0 {
        return Ok((0, true));
    }
    let mut fds = Vec::new();
    for file in files.into_iter().take(fit) {
        fds.push(p.files().fds.insert(file, flags & MSG_CMSG_CLOEXEC != 0, 0)?);
    }
    let len = 16 + 4 * fit;
    let mut control = Vec::with_capacity(len);
    control.extend((len as u64).to_le_bytes());
    control.extend(1i32.to_le_bytes()); // SOL_SOCKET
    control.extend(SCM_RIGHTS.to_le_bytes());
    for fd in &fds {
        control.extend(fd.to_le_bytes());
    }
    user::write(p.space(), m.control, &control)?;
    Ok((len.next_multiple_of(8).min(m.control_len as usize), fit < count))
}

fn send_msg(file: &OpenFile, sock: &Sock, at: u64, flags: u32) -> SysResult {
    let m: MsgHdr = user::read_struct(proc::current().space(), at)?;
    let files = if m.control != 0 && m.control_len > 0 { files_to_send(&m)? } else { Vec::new() };
    let mut data = Vec::new();
    for v in iovecs(&m)? {
        data.extend(user::read_vec(proc::current().space(), v.base, (v.len as usize).min(1 << 20))?);
    }
    send_on(file, sock, &data, m.name, m.name_len as u64, files, flags)
}

fn recv_msg(file: &OpenFile, sock: &Sock, at: u64, flags: u32) -> SysResult {
    let space = proc::current().space();
    let m: MsgHdr = user::read_struct(space, at)?;
    let vecs = iovecs(&m)?;
    let total: usize = vecs.iter().map(|v| v.len as usize).sum();
    let mut tmp = vec![0u8; total.min(1 << 20)];
    let (n, sender, files) = recv_on(file, sock, &mut tmp, flags)?;
    // Spread it back over the pieces.
    let got = n.min(tmp.len());
    let mut done = 0;
    for v in &vecs {
        if done >= got {
            break;
        }
        let k = (v.len as usize).min(got - done);
        user::write(space, v.base, &tmp[done..done + k])?;
        done += k;
    }
    if m.name != 0 {
        write_sender(m.name, at + 8, &sender)?; // msg_namelen: the room given, then the length
    }
    let (control_used, cut) = files_received(&m, files, flags)?;
    let truncated = n > got; // only seen when MSG_TRUNC asked for the full length
    let msg_flags = (if cut { MSG_CTRUNC } else { 0 }) | if truncated { MSG_TRUNC } else { 0 };
    user::write_struct(space, at + 40, &(control_used as u64))?; // msg_controllen
    user::write_struct(space, at + 48, &(msg_flags as i32))?;
    Ok(n as u64)
}

pub fn sendmsg(fd: i32, at: u64, flags: u32) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    send_msg(&file, &sock, at, flags)
}

pub fn recvmsg(fd: i32, at: u64, flags: u32) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    recv_msg(&file, &sock, at, flags)
}

/// Several messages at once (glibc's resolver sends its A and AAAA questions together this way).
/// Each `struct mmsghdr` is a msghdr and then how many bytes went, 64 bytes in all.
pub fn sendmmsg(fd: i32, at: u64, count: u32, flags: u32) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    let mut sent = 0;
    for i in 0..count.min(1024) as u64 {
        let entry = at + i * 64;
        match send_msg(&file, &sock, entry, flags) {
            Ok(n) => user::write_struct(proc::current().space(), entry + 56, &(n as u32))?,
            Err(e) => return if sent > 0 { Ok(sent) } else { Err(e) },
        }
        sent += 1;
    }
    Ok(sent)
}

pub fn recvmmsg(fd: i32, at: u64, count: u32, flags: u32) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    let mut got = 0;
    for i in 0..count.min(1024) as u64 {
        let entry = at + i * 64;
        // Only the first one waits; the rest take what's already there.
        let f = if got > 0 { flags | MSG_DONTWAIT } else { flags };
        match recv_msg(&file, &sock, entry, f) {
            Ok(n) => user::write_struct(proc::current().space(), entry + 56, &(n as u32))?,
            Err(e) => return if got > 0 { Ok(got) } else { Err(e) },
        }
        got += 1;
    }
    Ok(got)
}

// ---- options ----

/// Nothing we'd do differently for any option yet; saying yes is kinder than failing.
pub fn setsockopt() -> SysResult {
    Ok(0)
}

pub fn getsockopt(fd: i32, level: u32, name: u32, out: u64, len_at: u64) -> SysResult {
    let (_, sock) = sock_of(fd)?;
    let int = |v: i32| v.to_le_bytes().to_vec();
    let value: Vec<u8> = match (level, name) {
        (SOL_SOCKET, SO_ERROR) => int(match &sock {
            Sock::Stream(c) => tcp::take_error(c).unwrap_or(0) as i32,
            _ => 0,
        }),
        (SOL_SOCKET, SO_TYPE) => int(match &sock {
            Sock::Stream(_) | Sock::UnixStream(_) => SOCK_STREAM as i32,
            Sock::Datagram(_) | Sock::UnixDatagram(_) => SOCK_DGRAM as i32,
        }),
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => int(match &sock {
            Sock::UnixStream(_) | Sock::UnixDatagram(_) => fs::pipe::CAPACITY as i32,
            _ => tcp::BUFFER as i32,
        }),
        // Who's on the other end: their process, user and group (`struct ucred`).
        (SOL_SOCKET, SO_PEERCRED) => {
            let cred = match &sock {
                Sock::UnixStream(s) => unix::stream_peer(s)?.1,
                Sock::UnixDatagram(d) => unix::datagram_peer(d)?.1,
                _ => return Err(ENOPROTOOPT),
            };
            [cred.pid, cred.uid, cred.gid].iter().flat_map(|v| v.to_le_bytes()).collect()
        }
        // No IP options come with our packets (sshd refuses connections that have any).
        (IPPROTO_IP, IP_OPTIONS) => Vec::new(),
        _ => int(0),
    };
    let p = proc::current();
    let room = if len_at != 0 { user::read_struct::<u32>(p.space(), len_at)? as usize } else { value.len() };
    let n = value.len().min(room);
    if out != 0 {
        user::write(p.space(), out, &value[..n])?;
    }
    if len_at != 0 {
        user::write_struct(p.space(), len_at, &(n as u32))?;
    }
    Ok(0)
}

/// How the one network card is set up, for `ifconfig`: six bytes of address followed by four
/// addresses. Reading fills it in; writing sets everything but the card's own address.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct NetConfig {
    pub mac: [u8; 6],
    pub ip: Ip,
    pub netmask: Ip,
    pub gateway: Ip,
    pub dns: Ip,
}

pub const NET_GET: u32 = 0x5800;
pub const NET_SET: u32 = 0x5801;
const FIONREAD: u32 = 0x541B;

/// ioctl on a socket: our own two for ifconfig, and FIONREAD (how much is waiting).
pub fn net_ioctl(handle: &Handle, request: u32, arg: u64) -> SysResult {
    let p = proc::current();
    match request {
        NET_GET => {
            let c = crate::net::config();
            let out = NetConfig { mac: c.mac, ip: c.ip, netmask: c.netmask, gateway: c.gateway, dns: c.dns };
            user::write_struct(p.space(), arg, &out)?;
            Ok(0)
        }
        NET_SET => {
            if !p.creds.is_root() {
                return Err(EPERM);
            }
            let want: NetConfig = user::read_struct(p.space(), arg)?;
            crate::net::configure(want.ip, want.netmask, want.gateway, want.dns);
            Ok(0)
        }
        FIONREAD => {
            let n = match handle {
                Handle::Tcp(c) => tcp::pending_bytes(c),
                Handle::Socket(s) => s.queue.lock().front().map_or(0, |d| d.data.len()),
                Handle::UnixStream(s) => unix::pending(s),
                Handle::UnixDatagram(d) => unix::datagram_pending(d),
                _ => 0,
            };
            user::write_struct(p.space(), arg, &(n as u32))?;
            Ok(0)
        }
        _ => Err(ENOTTY),
    }
}
