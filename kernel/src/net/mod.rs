//! A small IPv4 stack: Ethernet, ARP, IP, ICMP, UDP and TCP, on top of the virtio network card.
//!
//! Enough to answer a ping, ping something else, send and receive UDP (what a DNS lookup needs),
//! and hold TCP connections both ways (what the web needs).
//!
//! Everything runs in the kernel with one lock: frames arrive on the timer interrupt, are dealt
//! with there, and anything a program is waiting for is left in a socket's queue.

pub mod socket;
pub mod tcp;
pub mod unix;

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

pub type Mac = [u8; 6];
pub type Ip = [u8; 4];

pub const BROADCAST: Mac = [0xff; 6];
const ETHERTYPE_IP: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_UDP: u8 = 17;

/// How this machine is set up. QEMU's user-mode network always looks like this, so it's the
/// default; `ifconfig` can change it.
pub struct Config {
    pub mac: Mac,
    pub ip: Ip,
    pub netmask: Ip,
    pub gateway: Ip,
    pub dns: Ip,
}

static CONFIG: Mutex<Config> = Mutex::new(Config {
    mac: [0; 6],
    ip: [10, 0, 2, 15],
    netmask: [255, 255, 255, 0],
    gateway: [10, 0, 2, 2],
    dns: [10, 0, 2, 3],
});

/// Addresses we've learned, and what we're still waiting to hear about.
static ARP: Mutex<Vec<(Ip, Mac)>> = Mutex::new(Vec::new());
/// Packets held back until we know where to send them.
static PENDING: Mutex<VecDeque<(Ip, Vec<u8>)>> = Mutex::new(VecDeque::new());

pub fn init(mac: Mac) {
    CONFIG.lock().mac = mac;
}

pub fn config() -> Config {
    let c = CONFIG.lock();
    Config { mac: c.mac, ip: c.ip, netmask: c.netmask, gateway: c.gateway, dns: c.dns }
}

pub fn configure(ip: Ip, netmask: Ip, gateway: Ip, dns: Ip) {
    let mut c = CONFIG.lock();
    (c.ip, c.netmask, c.gateway, c.dns) = (ip, netmask, gateway, dns);
}

pub fn up() -> bool {
    crate::drivers::virtio_net::present()
}

// ---- reading numbers out of packets ----

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_be_bytes());
}

/// The ones-complement sum used by IP, ICMP and UDP.
fn checksum(parts: &[&[u8]]) -> u16 {
    let mut sum: u32 = 0;
    let mut odd: Option<u8> = None;
    for part in parts {
        let mut data = *part;
        if let Some(first) = odd.take() {
            if let Some((&next, rest)) = data.split_first() {
                sum += u16::from_be_bytes([first, next]) as u32;
                data = rest;
            } else {
                odd = Some(first);
                continue;
            }
        }
        let mut i = 0;
        while i + 1 < data.len() {
            sum += be16(data, i) as u32;
            i += 2;
        }
        if i < data.len() {
            odd = Some(data[i]);
        }
    }
    if let Some(last) = odd {
        sum += u16::from_be_bytes([last, 0]) as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

// ---- Ethernet ----

fn send_frame(to: Mac, ethertype: u16, payload: &[u8]) {
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.extend_from_slice(&to);
    frame.extend_from_slice(&CONFIG.lock().mac);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);
    // Ethernet's shortest frame is 60 bytes before the checksum the card adds.
    while frame.len() < 60 {
        frame.push(0);
    }
    crate::drivers::virtio_net::send(&frame);
}

/// A frame arrived from the card.
pub fn receive(frame: &[u8]) {
    if frame.len() < 14 {
        return;
    }
    match be16(frame, 12) {
        ETHERTYPE_ARP => arp_in(&frame[14..]),
        ETHERTYPE_IP => ip_in(&frame[14..]),
        _ => {}
    }
}

// ---- ARP: which card has which address ----

fn arp_in(p: &[u8]) {
    if p.len() < 28 || be16(p, 0) != 1 || be16(p, 2) != ETHERTYPE_IP {
        return;
    }
    let sender_mac: Mac = p[8..14].try_into().unwrap();
    let sender_ip: Ip = p[14..18].try_into().unwrap();
    let target_ip: Ip = p[24..28].try_into().unwrap();
    remember(sender_ip, sender_mac);
    let me = CONFIG.lock().ip;
    if be16(p, 6) == 1 && target_ip == me {
        // A request for us: say so.
        arp_send(2, sender_mac, sender_ip);
    }
    flush_pending();
}

fn arp_send(operation: u16, to_mac: Mac, to_ip: Ip) {
    let c = config();
    let mut p = [0u8; 28];
    put16(&mut p, 0, 1); // Ethernet
    put16(&mut p, 2, ETHERTYPE_IP);
    p[4] = 6;
    p[5] = 4;
    put16(&mut p, 6, operation);
    p[8..14].copy_from_slice(&c.mac);
    p[14..18].copy_from_slice(&c.ip);
    p[18..24].copy_from_slice(&to_mac);
    p[24..28].copy_from_slice(&to_ip);
    send_frame(if operation == 1 { BROADCAST } else { to_mac }, ETHERTYPE_ARP, &p);
}

fn remember(ip: Ip, mac: Mac) {
    let mut table = ARP.lock();
    if let Some(entry) = table.iter_mut().find(|(known, _)| *known == ip) {
        entry.1 = mac;
    } else {
        if table.len() >= 32 {
            table.remove(0);
        }
        table.push((ip, mac));
    }
}

fn lookup(ip: Ip) -> Option<Mac> {
    ARP.lock().iter().find(|(known, _)| *known == ip).map(|(_, mac)| *mac)
}

/// Packets waiting on an address we've now learned.
fn flush_pending() {
    loop {
        let next = {
            let mut pending = PENDING.lock();
            let at = pending.iter().position(|(ip, _)| lookup(*ip).is_some());
            match at {
                Some(i) => pending.remove(i),
                None => None,
            }
        };
        match next {
            Some((ip, packet)) => {
                if let Some(mac) = lookup(ip) {
                    send_frame(mac, ETHERTYPE_IP, &packet);
                }
            }
            None => return,
        }
    }
}

// ---- IPv4 ----

/// Who to hand a packet to: whoever it's for if they're on our network, otherwise the gateway.
fn next_hop(to: Ip) -> Ip {
    let c = config();
    let same_network = (0..4).all(|i| to[i] & c.netmask[i] == c.ip[i] & c.netmask[i]);
    if same_network { to } else { c.gateway }
}

static NEXT_ID: Mutex<u16> = Mutex::new(1);

/// Wraps a payload in an IP header and sends it, asking who has the address if we don't know.
pub fn send_ip(to: Ip, protocol: u8, payload: &[u8]) {
    let c = config();
    let total = 20 + payload.len();
    let mut packet = Vec::with_capacity(total);
    let id = {
        let mut n = NEXT_ID.lock();
        *n = n.wrapping_add(1);
        *n
    };
    packet.extend_from_slice(&[0x45, 0]); // version 4, 20-byte header
    packet.extend_from_slice(&(total as u16).to_be_bytes());
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&[0x40, 0]); // don't fragment
    packet.extend_from_slice(&[64, protocol, 0, 0]); // time to live, protocol, checksum later
    packet.extend_from_slice(&c.ip);
    packet.extend_from_slice(&to);
    let sum = checksum(&[&packet[..20]]);
    put16(&mut packet, 10, sum);
    packet.extend_from_slice(payload);

    let hop = next_hop(to);
    match lookup(hop) {
        Some(mac) => send_frame(mac, ETHERTYPE_IP, &packet),
        None => {
            // Ask, and send it once we're told.
            let mut pending = PENDING.lock();
            if pending.len() >= 16 {
                pending.pop_front();
            }
            pending.push_back((hop, packet));
            drop(pending);
            arp_send(1, BROADCAST, hop);
        }
    }
}

fn ip_in(p: &[u8]) {
    if p.len() < 20 || p[0] >> 4 != 4 {
        return;
    }
    let header = (p[0] & 0xf) as usize * 4;
    let total = be16(p, 2) as usize;
    if header < 20 || total < header || p.len() < total {
        return;
    }
    let protocol = p[9];
    let from: Ip = p[12..16].try_into().unwrap();
    let to: Ip = p[16..20].try_into().unwrap();
    let me = CONFIG.lock().ip;
    if to != me && to != [255, 255, 255, 255] {
        return;
    }
    let body = &p[header..total];
    match protocol {
        PROTO_ICMP => icmp_in(from, body),
        PROTO_UDP => udp_in(from, body),
        tcp::PROTO_TCP => tcp::receive(from, body),
        _ => {}
    }
}

// ---- ICMP: ping ----

fn icmp_in(from: Ip, p: &[u8]) {
    if p.len() < 8 {
        return;
    }
    match p[0] {
        8 => {
            // An echo request: send the same bytes back as a reply.
            let mut reply = p.to_vec();
            reply[0] = 0;
            put16(&mut reply, 2, 0);
            let sum = checksum(&[&reply]);
            put16(&mut reply, 2, sum);
            send_ip(from, PROTO_ICMP, &reply);
        }
        0 => socket::deliver_icmp(from, p),
        _ => {}
    }
}

/// Sends an ICMP message (a ping, from the `ping` command) with its checksum filled in.
pub fn send_icmp(to: Ip, message: &mut [u8]) {
    if message.len() < 8 {
        return;
    }
    put16(message, 2, 0);
    let sum = checksum(&[message]);
    put16(message, 2, sum);
    send_ip(to, PROTO_ICMP, message);
}

// ---- UDP ----

pub fn send_udp(to: Ip, from_port: u16, to_port: u16, payload: &[u8]) {
    let c = config();
    let length = 8 + payload.len();
    let mut datagram = Vec::with_capacity(length);
    datagram.extend_from_slice(&from_port.to_be_bytes());
    datagram.extend_from_slice(&to_port.to_be_bytes());
    datagram.extend_from_slice(&(length as u16).to_be_bytes());
    datagram.extend_from_slice(&[0, 0]); // checksum, filled in below
    datagram.extend_from_slice(payload);
    // UDP's checksum covers a made-up header of the addresses as well as the datagram.
    let pseudo = [c.ip[0], c.ip[1], c.ip[2], c.ip[3], to[0], to[1], to[2], to[3], 0, PROTO_UDP, (length >> 8) as u8, length as u8];
    let sum = checksum(&[&pseudo, &datagram]);
    put16(&mut datagram, 6, if sum == 0 { 0xffff } else { sum });
    send_ip(to, PROTO_UDP, &datagram);
}

fn udp_in(from: Ip, p: &[u8]) {
    if p.len() < 8 {
        return;
    }
    let length = be16(p, 4) as usize;
    if length < 8 || length > p.len() {
        return;
    }
    socket::deliver_udp(from, be16(p, 0), be16(p, 2), &p[8..length]);
}
