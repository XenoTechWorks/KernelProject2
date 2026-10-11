//! Sockets: what a program holds when it talks over the network. UDP (SOCK_DGRAM) and ping
//! (SOCK_DGRAM with IPPROTO_ICMP, the way Linux lets an ordinary program ping).
//!
//! A socket is a queue of what arrived for it. The stack fills the queue from the timer
//! interrupt; a program reading an empty one sleeps until something lands.

use super::{Ip, PROTO_ICMP, PROTO_UDP};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

/// One thing that arrived: who from, and what.
pub struct Datagram {
    pub from: Ip,
    pub port: u16,
    pub data: Vec<u8>,
}

pub struct Socket {
    pub protocol: u8,
    /// The port we listen on (0 until bound or first used).
    pub port: Mutex<u16>,
    /// Where sendto defaults to, after connect().
    pub peer: Mutex<Option<(Ip, u16)>>,
    pub queue: Mutex<VecDeque<Datagram>>,
}

impl Socket {
    pub fn new(protocol: u8) -> Arc<Socket> {
        Arc::new(Socket { protocol, port: Mutex::new(0), peer: Mutex::new(None), queue: Mutex::new(VecDeque::new()) })
    }
}

/// Every open socket, so arriving packets can find the one they belong to.
static SOCKETS: Mutex<Vec<Arc<Socket>>> = Mutex::new(Vec::new());
static NEXT_PORT: Mutex<u16> = Mutex::new(49152);

pub fn add(socket: &Arc<Socket>) {
    SOCKETS.lock().push(socket.clone());
}

pub fn remove(socket: &Arc<Socket>) {
    SOCKETS.lock().retain(|s| !Arc::ptr_eq(s, socket));
}

/// A port nobody is using, from the range set aside for programs that didn't ask for one.
pub fn free_port() -> u16 {
    let sockets = SOCKETS.lock();
    let mut next = NEXT_PORT.lock();
    for _ in 0..16384 {
        *next = if *next >= 65535 { 49152 } else { *next + 1 };
        let port = *next;
        if !sockets.iter().any(|s| *s.port.lock() == port) {
            return port;
        }
    }
    0
}

fn arrived(socket: &Arc<Socket>, from: Ip, port: u16, data: &[u8]) {
    let mut queue = socket.queue.lock();
    // A program that isn't keeping up loses the oldest, as a datagram socket does.
    if queue.len() >= 32 {
        queue.pop_front();
    }
    queue.push_back(Datagram { from, port, data: data.to_vec() });
    drop(queue);
    crate::proc::wake_all();
}

pub fn deliver_udp(from: Ip, from_port: u16, to_port: u16, data: &[u8]) {
    let sockets = SOCKETS.lock();
    let found = sockets.iter().find(|s| s.protocol == PROTO_UDP && *s.port.lock() == to_port).cloned();
    drop(sockets);
    if let Some(socket) = found {
        arrived(&socket, from, from_port, data);
    }
}

/// An echo reply goes to the socket whose port matches the identifier the ping was sent with.
pub fn deliver_icmp(from: Ip, data: &[u8]) {
    let id = u16::from_be_bytes([data[4], data[5]]);
    let sockets = SOCKETS.lock();
    let found = sockets.iter().find(|s| s.protocol == PROTO_ICMP && *s.port.lock() == id).cloned();
    drop(sockets);
    if let Some(socket) = found {
        arrived(&socket, from, id, data);
    }
}

/// Sends from a socket: a UDP datagram, or an ICMP message with the socket's id in it.
pub fn send(socket: &Arc<Socket>, to: Ip, to_port: u16, data: &[u8]) {
    if socket.protocol == PROTO_ICMP {
        let mut message = data.to_vec();
        if message.len() >= 8 {
            let id = *socket.port.lock();
            message[4..6].copy_from_slice(&id.to_be_bytes());
        }
        super::send_icmp(to, &mut message);
    } else {
        super::send_udp(to, *socket.port.lock(), to_port, data);
    }
}
