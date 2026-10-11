//! The socket system calls, the Linux ones: socket, bind, connect, sendto, recvfrom, getsockname
//! and setsockopt. Only IPv4 datagrams so far (UDP, and ICMP for ping).

use super::SysResult;
use super::errno::*;
use crate::mm::user;
use crate::net::socket::{self, Socket};
use crate::net::{Ip, PROTO_ICMP, PROTO_UDP};
use crate::proc::{self, fd::Handle, fd::OpenFile};
use alloc::string::String;
use alloc::sync::Arc;

const AF_INET: u32 = 2;
const SOCK_DGRAM: u32 = 2;
const SOCK_RAW: u32 = 3;
const SOCK_NONBLOCK: u32 = 0o4000;
const SOCK_CLOEXEC: u32 = 0o2000000;

/// Linux's `struct sockaddr_in`: family, port and address, big-endian on the wire parts.
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

fn socket_of(fd: i32) -> Result<Arc<Socket>, Errno> {
    match &proc::current().files().fds.get(fd)?.handle {
        Handle::Socket(s) => Ok(s.clone()),
        _ => Err(ENOTSOCK),
    }
}

pub fn socket(family: u32, kind: u32, protocol: u32) -> SysResult {
    if family != AF_INET {
        return Err(EAFNOSUPPORT);
    }
    let flags = kind & (SOCK_NONBLOCK | SOCK_CLOEXEC);
    let icmp = protocol == PROTO_ICMP as u32;
    let protocol = match kind & 0xf {
        SOCK_DGRAM if icmp => PROTO_ICMP,
        SOCK_DGRAM if protocol == 0 || protocol == PROTO_UDP as u32 => PROTO_UDP,
        SOCK_RAW if icmp => PROTO_ICMP,
        _ => return Err(EPROTONOSUPPORT),
    };
    if !crate::net::up() {
        return Err(ENETDOWN);
    }
    let sock = Socket::new(protocol);
    // A ping socket needs an id straight away, since that's how replies find their way back.
    if protocol == PROTO_ICMP {
        *sock.port.lock() = socket::free_port();
    }
    socket::add(&sock);
    let p = proc::current();
    let open = Arc::new(OpenFile {
        handle: Handle::Socket(sock),
        offset: spin::Mutex::new(0),
        flags: spin::Mutex::new(if flags & SOCK_NONBLOCK != 0 { super::fs::O_NONBLOCK } else { 0 }),
        path: String::from("socket:"),
    });
    Ok(p.files().fds.insert(open, flags & SOCK_CLOEXEC != 0, 0)? as u64)
}

pub fn bind(fd: i32, at: u64, len: u64) -> SysResult {
    let sock = socket_of(fd)?;
    let (_, port) = read_addr(at, len)?;
    *sock.port.lock() = if port == 0 { socket::free_port() } else { port };
    Ok(0)
}

pub fn connect(fd: i32, at: u64, len: u64) -> SysResult {
    let sock = socket_of(fd)?;
    let (ip, port) = read_addr(at, len)?;
    *sock.peer.lock() = Some((ip, port));
    if *sock.port.lock() == 0 {
        *sock.port.lock() = socket::free_port();
    }
    Ok(0)
}

pub fn getsockname(fd: i32, at: u64, len_at: u64) -> SysResult {
    let sock = socket_of(fd)?;
    write_addr(at, len_at, crate::net::config().ip, *sock.port.lock())?;
    Ok(0)
}

pub fn getpeername(fd: i32, at: u64, len_at: u64) -> SysResult {
    let sock = socket_of(fd)?;
    let (ip, port) = sock.peer.lock().ok_or(ENOTCONN)?;
    write_addr(at, len_at, ip, port)?;
    Ok(0)
}

pub fn sendto(fd: i32, buf: u64, len: usize, to: u64, to_len: u64) -> SysResult {
    let file = proc::current().files().fds.get(fd)?;
    let Handle::Socket(sock) = &file.handle else { return Err(ENOTSOCK) };
    let (ip, port) = if to != 0 {
        read_addr(to, to_len)?
    } else {
        sock.peer.lock().ok_or(EDESTADDRREQ)?
    };
    if *sock.port.lock() == 0 {
        *sock.port.lock() = socket::free_port();
    }
    let data = user::read_vec(proc::current().space(), buf, len.min(65507))?;
    socket::send(sock, ip, port, &data);
    Ok(data.len() as u64)
}

pub fn recvfrom(fd: i32, buf: u64, len: usize, from: u64, from_len: u64) -> SysResult {
    let file = proc::current().files().fds.get(fd)?;
    let Handle::Socket(sock) = &file.handle else { return Err(ENOTSOCK) };
    loop {
        if let Some(packet) = sock.queue.lock().pop_front() {
            let n = packet.data.len().min(len);
            user::write(proc::current().space(), buf, &packet.data[..n])?;
            write_addr(from, from_len, packet.from, packet.port)?;
            return Ok(n as u64);
        }
        if *file.flags.lock() & super::fs::O_NONBLOCK != 0 {
            return Err(EAGAIN);
        }
        proc::sleep()?;
    }
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

/// SIOCGIFADDR-ish, but our own and much simpler: ioctl on any socket.
pub const NET_GET: u32 = 0x5800;
pub const NET_SET: u32 = 0x5801;

pub fn net_ioctl(request: u32, arg: u64) -> SysResult {
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
        _ => Err(ENOTTY),
    }
}

/// We keep no options worth setting yet; saying yes is kinder than failing.
pub fn setsockopt() -> SysResult {
    Ok(0)
}

pub fn getsockopt(out: u64, len_at: u64) -> SysResult {
    let p = proc::current();
    if out != 0 {
        user::write_struct(p.space(), out, &0u32)?;
    }
    if len_at != 0 {
        user::write_struct(p.space(), len_at, &4u32)?;
    }
    Ok(0)
}
