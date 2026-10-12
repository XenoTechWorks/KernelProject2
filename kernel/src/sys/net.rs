//! The socket system calls, the Linux ones: socket, bind, listen, accept, connect, send and
//! receive (in all their forms), shutdown, and the get/set calls. IPv4 only: TCP streams, UDP
//! datagrams, and ICMP datagrams for ping.

use super::SysResult;
use super::errno::*;
use crate::mm::user;
use crate::net::socket::{self, Socket};
use crate::net::tcp::{self, Conn};
use crate::net::{Ip, PROTO_ICMP, PROTO_UDP};
use crate::proc::{self, fd::Handle, fd::OpenFile};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

const AF_INET: u32 = 2;
const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;
const SOCK_RAW: u32 = 3;
const SOCK_NONBLOCK: u32 = 0o4000;
const SOCK_CLOEXEC: u32 = 0o2000000;
const IPPROTO_TCP: u32 = 6;

const MSG_PEEK: u32 = 0x2;
const MSG_DONTWAIT: u32 = 0x40;
const MSG_NOSIGNAL: u32 = 0x4000;

const SOL_SOCKET: u32 = 1;
const SO_TYPE: u32 = 3;
const SO_ERROR: u32 = 4;
const SO_SNDBUF: u32 = 7;
const SO_RCVBUF: u32 = 8;

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

enum Sock {
    Datagram(Arc<Socket>),
    Stream(Arc<Conn>),
}

fn sock_of(fd: i32) -> Result<(Arc<OpenFile>, Sock), Errno> {
    let file = proc::current().files().fds.get(fd)?;
    let sock = match &file.handle {
        Handle::Socket(s) => Sock::Datagram(s.clone()),
        Handle::Tcp(c) => Sock::Stream(c.clone()),
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
    if family != AF_INET {
        return Err(EAFNOSUPPORT);
    }
    if !crate::net::up() {
        return Err(ENETDOWN);
    }
    let flags = kind & (SOCK_NONBLOCK | SOCK_CLOEXEC);
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

pub fn bind(fd: i32, at: u64, len: u64) -> SysResult {
    let (_, sock) = sock_of(fd)?;
    let (_, port) = read_addr(at, len)?;
    match sock {
        Sock::Datagram(s) => *s.port.lock() = if port == 0 { socket::free_port() } else { port },
        Sock::Stream(c) => tcp::bind(&c, port)?,
    }
    Ok(0)
}

pub fn listen(fd: i32, backlog: u32) -> SysResult {
    match sock_of(fd)?.1 {
        Sock::Stream(c) => tcp::listen(&c, backlog as usize).map(|_| 0),
        Sock::Datagram(_) => Err(EOPNOTSUPP),
    }
}

pub fn accept(fd: i32, at: u64, len_at: u64, flags: u32) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    let Sock::Stream(listener) = sock else { return Err(EOPNOTSUPP) };
    loop {
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
    }
}

pub fn connect(fd: i32, at: u64, len: u64) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    let (ip, port) = read_addr(at, len)?;
    match sock {
        Sock::Datagram(s) => {
            *s.peer.lock() = Some((ip, port));
            if *s.port.lock() == 0 {
                *s.port.lock() = socket::free_port();
            }
            Ok(0)
        }
        Sock::Stream(c) => {
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
    }
}

pub fn shutdown(fd: i32, how: u32) -> SysResult {
    if let Sock::Stream(c) = sock_of(fd)?.1 {
        if how != 0 {
            tcp::shutdown_write(&c); // SHUT_WR or SHUT_RDWR
        }
    }
    Ok(0)
}

pub fn getsockname(fd: i32, at: u64, len_at: u64) -> SysResult {
    let port = match sock_of(fd)?.1 {
        Sock::Datagram(s) => *s.port.lock(),
        Sock::Stream(c) => tcp::local_port(&c),
    };
    write_addr(at, len_at, crate::net::config().ip, port)?;
    Ok(0)
}

pub fn getpeername(fd: i32, at: u64, len_at: u64) -> SysResult {
    let peer = match sock_of(fd)?.1 {
        Sock::Datagram(s) => *s.peer.lock(),
        Sock::Stream(c) => tcp::peer(&c),
    };
    let (ip, port) = peer.ok_or(ENOTCONN)?;
    write_addr(at, len_at, ip, port)?;
    Ok(0)
}

// ---- sending and receiving, shared by every form of the calls ----

/// Sends from a kernel buffer, to `to` or to whoever the socket is connected to.
fn send_on(file: &OpenFile, sock: &Sock, data: &[u8], to: Option<(Ip, u16)>, flags: u32) -> SysResult {
    match sock {
        Sock::Datagram(s) => {
            let (ip, port) = match to {
                Some(t) => t,
                None => s.peer.lock().ok_or(EDESTADDRREQ)?,
            };
            if *s.port.lock() == 0 {
                *s.port.lock() = socket::free_port();
            }
            let n = data.len().min(65507);
            socket::send(s, ip, port, &data[..n]);
            Ok(n as u64)
        }
        Sock::Stream(c) => stream_write(file, c, data, flags),
    }
}

/// Receives into a kernel buffer; the address it came from, for datagrams.
fn recv_on(file: &OpenFile, sock: &Sock, buf: &mut [u8], flags: u32) -> Result<(usize, Option<(Ip, u16)>), Errno> {
    match sock {
        Sock::Datagram(s) => loop {
            {
                let mut queue = s.queue.lock();
                if let Some(packet) = queue.front() {
                    let n = packet.data.len().min(buf.len());
                    buf[..n].copy_from_slice(&packet.data[..n]);
                    let from = (packet.from, packet.port);
                    if flags & MSG_PEEK == 0 {
                        queue.pop_front();
                    }
                    return Ok((n, Some(from)));
                }
            }
            if nonblocking(file, flags) {
                return Err(EAGAIN);
            }
            proc::sleep()?;
        },
        Sock::Stream(c) => Ok((stream_read(file, c, buf, flags)? as usize, None)),
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
            Err(EPIPE) => {
                // Writing to a connection the other side has closed: SIGPIPE, unless asked not to.
                if flags & MSG_NOSIGNAL == 0 {
                    proc::signal::send(proc::current(), proc::SIGPIPE);
                }
                return if written > 0 { Ok(written as u64) } else { Err(EPIPE) };
            }
            Err(e) => return if written > 0 { Ok(written as u64) } else { Err(e) },
        }
    }
    Ok(written as u64)
}

pub fn sendto(fd: i32, buf: u64, len: usize, flags: u32, to: u64, to_len: u64) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    let dest = if to != 0 { Some(read_addr(to, to_len)?) } else { None };
    let data = user::read_vec(proc::current().space(), buf, len.min(1 << 20))?;
    send_on(&file, &sock, &data, dest, flags)
}

pub fn recvfrom(fd: i32, buf: u64, len: usize, flags: u32, from: u64, from_len: u64) -> SysResult {
    let (file, sock) = sock_of(fd)?;
    let mut tmp = vec![0u8; len.min(1 << 20)];
    let (n, sender) = recv_on(&file, &sock, &mut tmp, flags)?;
    user::write(proc::current().space(), buf, &tmp[..n])?;
    if let Some((ip, port)) = sender {
        write_addr(from, from_len, ip, port)?;
    }
    Ok(n as u64)
}

// ---- sendmsg / recvmsg, and their batch forms ----

/// `struct msghdr`: where to, the pieces of the message (an iovec), and ancillary data we ignore.
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

fn send_msg(file: &OpenFile, sock: &Sock, at: u64, flags: u32) -> SysResult {
    let m: MsgHdr = user::read_struct(proc::current().space(), at)?;
    let dest = if m.name != 0 { Some(read_addr(m.name, m.name_len as u64)?) } else { None };
    let mut data = Vec::new();
    for v in iovecs(&m)? {
        data.extend(user::read_vec(proc::current().space(), v.base, (v.len as usize).min(1 << 20))?);
    }
    send_on(file, sock, &data, dest, flags)
}

fn recv_msg(file: &OpenFile, sock: &Sock, at: u64, flags: u32) -> SysResult {
    let space = proc::current().space();
    let m: MsgHdr = user::read_struct(space, at)?;
    let vecs = iovecs(&m)?;
    let total: usize = vecs.iter().map(|v| v.len as usize).sum();
    let mut tmp = vec![0u8; total.min(1 << 20)];
    let (n, sender) = recv_on(file, sock, &mut tmp, flags)?;
    // Spread it back over the pieces.
    let mut done = 0;
    for v in &vecs {
        if done >= n {
            break;
        }
        let k = (v.len as usize).min(n - done);
        user::write(space, v.base, &tmp[done..done + k])?;
        done += k;
    }
    if let (Some((ip, port)), true) = (sender, m.name != 0) {
        write_addr(m.name, 0, ip, port)?;
        user::write_struct(space, at + 8, &16u32)?; // msg_namelen
    }
    user::write_struct(space, at + 40, &0u64)?; // no ancillary data
    user::write_struct(space, at + 48, &0i32)?; // msg_flags
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
    let value: i32 = match (level, name) {
        (SOL_SOCKET, SO_ERROR) => match &sock {
            Sock::Stream(c) => tcp::take_error(c).unwrap_or(0) as i32,
            Sock::Datagram(_) => 0,
        },
        (SOL_SOCKET, SO_TYPE) => match &sock {
            Sock::Stream(_) => SOCK_STREAM as i32,
            Sock::Datagram(_) => SOCK_DGRAM as i32,
        },
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => tcp::BUFFER as i32,
        _ => 0,
    };
    let p = proc::current();
    if out != 0 {
        user::write_struct(p.space(), out, &value)?;
    }
    if len_at != 0 {
        user::write_struct(p.space(), len_at, &4u32)?;
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
                _ => 0,
            };
            user::write_struct(p.space(), arg, &(n as u32))?;
            Ok(0)
        }
        _ => Err(ENOTTY),
    }
}
