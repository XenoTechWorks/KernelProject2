//! TCP: reliable, ordered byte streams over IP (RFC 9293), the way most of the internet talks.
//!
//! Each connection is a `Conn`: its state, the sequence numbers on both sides, what we've sent
//! that hasn't been acknowledged yet, and what arrived that the program hasn't read. Segments are
//! dealt with as they arrive (on the timer interrupt, from the card); a timer resends what wasn't
//! acknowledged in time, waiting twice as long each try.
//!
//! Kept simple where that's safe: a segment that arrives out of order is dropped and the sender
//! resends it; there's no congestion control beyond the other side's window; options other than
//! the maximum segment size are ignored.

use super::{Ip, be16, checksum, put16};
use crate::sys::errno::*;
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

pub const PROTO_TCP: u8 = 6;
const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
const RST: u8 = 0x04;
const PSH: u8 = 0x08;
const ACK: u8 = 0x10;

/// The most we put in one segment (Ethernet's 1500 bytes, less the IP and TCP headers).
const MSS: usize = 1460;
/// How much we buffer each way.
pub const BUFFER: usize = 64 * 1024;
/// Timer ticks are 10 ms: the first resend after a second, then backing off to eight.
const RTO_START: u64 = 100;
const RTO_MAX: u64 = 800;
const RETRIES: u32 = 8;
/// How long a finished connection lingers (to catch a late resend from the other side), and how
/// long a closed one waits for the other side to finish before giving up.
const TIME_WAIT: u64 = 200;
const ORPHAN_WAIT: u64 = 6000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

pub struct Tcb {
    pub state: State,
    pub local_port: u16,
    pub remote_ip: Ip,
    pub remote_port: u16,
    iss: u32,
    /// The oldest byte not acknowledged yet, and the next one to send.
    snd_una: u32,
    snd_nxt: u32,
    /// How much the other side says it can take.
    snd_wnd: u32,
    /// The next byte we expect.
    rcv_nxt: u32,
    /// What we're sending, starting at snd_una: acknowledged bytes are dropped off the front.
    send_buf: VecDeque<u8>,
    /// What arrived and hasn't been read.
    pub recv_buf: VecDeque<u8>,
    /// The program is done writing: send FIN once everything else has gone.
    fin_queued: bool,
    fin_sent: bool,
    /// The other side is done writing (its FIN arrived).
    pub peer_fin: bool,
    /// Why the connection failed, for the program to find out.
    pub error: Option<Errno>,
    peer_mss: usize,
    rto: u64,
    retries: u32,
    /// When to resend (0: nothing waiting for an acknowledgement).
    deadline: u64,
    /// When a lingering connection goes (TIME-WAIT, or an orphan the other side never finished).
    expires: u64,
    last_window: usize,
    backlog: usize,
    pub accept_queue: VecDeque<Arc<Conn>>,
    parent: Option<Weak<Conn>>,
    /// No program holds it any more: it lives only to finish closing politely.
    orphan: bool,
}

pub struct Conn {
    pub tcb: Mutex<Tcb>,
}

/// Every connection the stack knows about (bound, listening, connecting or connected).
static CONNS: Mutex<Vec<Arc<Conn>>> = Mutex::new(Vec::new());
static NEXT_PORT: Mutex<u16> = Mutex::new(49152);
static ISS_COUNTER: AtomicU32 = AtomicU32::new(0);

fn now() -> u64 {
    crate::arch::irq::ticks()
}

/// a < b, in sequence-number arithmetic (which wraps).
fn before(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A starting sequence number that's hard to guess: the clock mixed with a counter.
fn new_iss() -> u32 {
    let n = ISS_COUNTER.fetch_add(0x9e37_79b9, Ordering::Relaxed);
    (now() as u32).wrapping_mul(2_654_435_761) ^ n
}

impl Tcb {
    fn new() -> Tcb {
        Tcb {
            state: State::Closed,
            local_port: 0,
            remote_ip: [0; 4],
            remote_port: 0,
            iss: 0,
            snd_una: 0,
            snd_nxt: 0,
            snd_wnd: 0,
            rcv_nxt: 0,
            send_buf: VecDeque::new(),
            recv_buf: VecDeque::new(),
            fin_queued: false,
            fin_sent: false,
            peer_fin: false,
            error: None,
            peer_mss: 536,
            rto: RTO_START,
            retries: 0,
            deadline: 0,
            expires: 0,
            last_window: 0,
            backlog: 0,
            accept_queue: VecDeque::new(),
            parent: None,
            orphan: false,
        }
    }

    fn window(&self) -> usize {
        (BUFFER - self.recv_buf.len()).min(65535)
    }

    /// Bytes sent and not acknowledged (SYN and FIN count as one each).
    fn in_flight(&self) -> usize {
        self.snd_nxt.wrapping_sub(self.snd_una) as usize
    }

    /// Room for the program to write.
    pub fn send_room(&self) -> usize {
        BUFFER - self.send_buf.len()
    }

    pub fn connected(&self) -> bool {
        matches!(self.state, State::Established | State::CloseWait)
    }
}

pub fn new() -> Arc<Conn> {
    Arc::new(Conn { tcb: Mutex::new(Tcb::new()) })
}

// ---- sending segments ----

fn segment(t: &mut Tcb, seq: u32, flags: u8, data: &[u8]) {
    let header = if flags & SYN != 0 { 24 } else { 20 };
    let window = t.window();
    t.last_window = window;
    let mut seg = Vec::with_capacity(header + data.len());
    seg.resize(header, 0);
    put16(&mut seg, 0, t.local_port);
    put16(&mut seg, 2, t.remote_port);
    seg[4..8].copy_from_slice(&seq.to_be_bytes());
    let ack = if flags & ACK != 0 { t.rcv_nxt } else { 0 };
    seg[8..12].copy_from_slice(&ack.to_be_bytes());
    seg[12] = ((header / 4) as u8) << 4;
    seg[13] = flags;
    put16(&mut seg, 14, window as u16);
    if flags & SYN != 0 {
        // Our maximum segment size, the one option we send.
        seg[20..24].copy_from_slice(&[2, 4, (MSS >> 8) as u8, MSS as u8]);
    }
    seg.extend_from_slice(data);
    send(t.remote_ip, &mut seg);
}

/// Fills in the checksum (over a made-up header of the addresses, as UDP does) and sends.
fn send(to: Ip, seg: &mut [u8]) {
    let me = super::config().ip;
    let len = seg.len();
    let pseudo = [me[0], me[1], me[2], me[3], to[0], to[1], to[2], to[3], 0, PROTO_TCP, (len >> 8) as u8, len as u8];
    put16(seg, 16, 0);
    let sum = checksum(&[&pseudo, seg]);
    put16(seg, 16, sum);
    super::send_ip(to, PROTO_TCP, seg);
}

/// A reset for a segment that has no connection to go to.
fn reset_reply(to: Ip, from_port: u16, to_port: u16, seg_seq: u32, seg_ack: u32, flags: u8, seg_len: usize) {
    let mut seg = [0u8; 20];
    put16(&mut seg, 0, from_port);
    put16(&mut seg, 2, to_port);
    if flags & ACK != 0 {
        seg[4..8].copy_from_slice(&seg_ack.to_be_bytes());
        seg[13] = RST;
    } else {
        let mut ack = seg_seq.wrapping_add(seg_len as u32);
        if flags & SYN != 0 {
            ack = ack.wrapping_add(1);
        }
        if flags & FIN != 0 {
            ack = ack.wrapping_add(1);
        }
        seg[8..12].copy_from_slice(&ack.to_be_bytes());
        seg[13] = RST | ACK;
    }
    seg[12] = 5 << 4;
    send(to, &mut seg);
}

fn arm(t: &mut Tcb) {
    if t.deadline == 0 {
        t.deadline = now() + t.rto;
    }
}

/// Sends whatever the window allows, then the FIN once everything else is out.
fn output(t: &mut Tcb) {
    if !matches!(t.state, State::Established | State::CloseWait | State::FinWait1 | State::Closing | State::LastAck) {
        return;
    }
    while !t.fin_sent {
        let flight = t.in_flight();
        let unsent = t.send_buf.len().saturating_sub(flight);
        let room = (t.snd_wnd as usize).saturating_sub(flight);
        let n = unsent.min(room).min(t.peer_mss);
        if n == 0 {
            break;
        }
        let data: Vec<u8> = t.send_buf.range(flight..flight + n).copied().collect();
        let seq = t.snd_nxt;
        segment(t, seq, ACK | PSH, &data);
        t.snd_nxt = t.snd_nxt.wrapping_add(n as u32);
        arm(t);
    }
    if t.fin_queued && !t.fin_sent && t.in_flight() == t.send_buf.len() {
        let seq = t.snd_nxt;
        segment(t, seq, FIN | ACK, &[]);
        t.snd_nxt = t.snd_nxt.wrapping_add(1);
        t.fin_sent = true;
        t.state = match t.state {
            State::Established => State::FinWait1,
            State::CloseWait => State::LastAck,
            other => other,
        };
        arm(t);
    }
}

/// The timer went off: send again from the oldest unacknowledged byte.
fn retransmit(t: &mut Tcb) {
    t.deadline = 0;
    match t.state {
        State::SynSent => {
            let iss = t.iss;
            segment(t, iss, SYN, &[]);
            arm(t);
        }
        State::SynReceived => {
            let iss = t.iss;
            segment(t, iss, SYN | ACK, &[]);
            arm(t);
        }
        _ => {
            t.snd_nxt = t.snd_una;
            t.fin_sent = false;
            output(t);
            // A window that shrank to nothing: poke it with one byte so we hear when it opens.
            if t.deadline == 0 && t.snd_wnd == 0 && !t.send_buf.is_empty() {
                let byte = [t.send_buf[0]];
                let seq = t.snd_nxt;
                segment(t, seq, ACK, &byte);
                t.snd_nxt = t.snd_nxt.wrapping_add(1);
                arm(t);
            }
        }
    }
}

fn finish(t: &mut Tcb, error: Option<Errno>) {
    t.state = State::Closed;
    t.deadline = 0;
    if t.error.is_none() {
        t.error = error;
    }
}

// ---- what programs do ----

fn port_in_use(port: u16) -> bool {
    CONNS.lock().iter().any(|c| {
        let t = c.tcb.lock();
        t.local_port == port && t.state != State::Closed
    })
}

fn free_port() -> u16 {
    let mut next = NEXT_PORT.lock();
    for _ in 0..16384 {
        *next = if *next >= 65535 { 49152 } else { *next + 1 };
        let port = *next;
        if !port_in_use(port) {
            return port;
        }
    }
    0
}

pub fn bind(conn: &Arc<Conn>, port: u16) -> Result<(), Errno> {
    let port = if port == 0 { free_port() } else { port };
    let listening = CONNS.lock().iter().any(|c| {
        let t = c.tcb.lock();
        t.local_port == port && t.state == State::Listen
    });
    if listening {
        return Err(EADDRINUSE);
    }
    conn.tcb.lock().local_port = port;
    Ok(())
}

pub fn listen(conn: &Arc<Conn>, backlog: usize) -> Result<(), Errno> {
    let mut t = conn.tcb.lock();
    if t.state != State::Closed {
        return if t.state == State::Listen { Ok(()) } else { Err(EINVAL) };
    }
    if t.local_port == 0 {
        drop(t);
        bind(conn, 0)?;
        t = conn.tcb.lock();
    }
    t.state = State::Listen;
    t.backlog = backlog.clamp(1, 128);
    drop(t);
    CONNS.lock().push(conn.clone());
    Ok(())
}

/// Starts connecting (sends the SYN). The caller waits for `state` to change.
pub fn connect(conn: &Arc<Conn>, ip: Ip, port: u16) -> Result<(), Errno> {
    {
        let t = conn.tcb.lock();
        match t.state {
            State::Closed => {}
            State::SynSent => return Err(EALREADY),
            _ => return Err(EISCONN),
        }
    }
    let local = if conn.tcb.lock().local_port == 0 { free_port() } else { conn.tcb.lock().local_port };
    {
        let mut t = conn.tcb.lock();
        t.local_port = local;
        t.remote_ip = ip;
        t.remote_port = port;
        t.iss = new_iss();
        t.snd_una = t.iss;
        t.snd_nxt = t.iss.wrapping_add(1);
        t.state = State::SynSent;
        t.error = None;
        let iss = t.iss;
        segment(&mut t, iss, SYN, &[]);
        arm(&mut t);
    }
    CONNS.lock().push(conn.clone());
    Ok(())
}

/// Queues bytes to send. Returns how many it took (0: no room, wait).
pub fn write(conn: &Arc<Conn>, data: &[u8]) -> Result<usize, Errno> {
    let mut t = conn.tcb.lock();
    if let Some(e) = t.error {
        return Err(e);
    }
    if matches!(t.state, State::SynSent | State::SynReceived) {
        return Ok(0); // not connected yet: wait
    }
    if t.fin_queued || !t.connected() {
        return Err(EPIPE);
    }
    let n = data.len().min(t.send_room());
    t.send_buf.extend(&data[..n]);
    output(&mut t);
    Ok(n)
}

/// Takes what arrived. Ok(None): nothing yet, wait. Ok(Some(0)): the other side has finished.
pub fn read(conn: &Arc<Conn>, buf: &mut [u8], peek: bool) -> Result<Option<usize>, Errno> {
    let mut t = conn.tcb.lock();
    if !t.recv_buf.is_empty() {
        let n = buf.len().min(t.recv_buf.len());
        for (i, b) in t.recv_buf.iter().take(n).enumerate() {
            buf[i] = *b;
        }
        if !peek {
            t.recv_buf.drain(..n);
            // Tell the other side when room opens up again, or it waits for its timer.
            if t.last_window < 2 * MSS && t.window() >= 2 * MSS && t.connected() {
                let seq = t.snd_nxt;
                segment(&mut t, seq, ACK, &[]);
            }
        }
        return Ok(Some(n));
    }
    if t.peer_fin {
        return Ok(Some(0));
    }
    if let Some(e) = t.error {
        return Err(e);
    }
    match t.state {
        State::Closed => Ok(Some(0)),
        State::Listen => Err(ENOTCONN),
        _ => Ok(None),
    }
}

/// shutdown(SHUT_WR): no more writing; the FIN goes once what's queued has.
pub fn shutdown_write(conn: &Arc<Conn>) {
    let mut t = conn.tcb.lock();
    t.fin_queued = true;
    output(&mut t);
}

/// The last descriptor closed: finish the connection politely, then forget it.
pub fn close(conn: &Arc<Conn>) {
    let children: Vec<Arc<Conn>>;
    {
        let mut t = conn.tcb.lock();
        t.orphan = true;
        children = t.accept_queue.drain(..).collect();
        match t.state {
            State::Listen | State::SynSent | State::Closed => finish(&mut t, None),
            State::SynReceived | State::Established | State::CloseWait => {
                t.fin_queued = true;
                output(&mut t);
                t.expires = now() + ORPHAN_WAIT;
            }
            _ => {}
        }
    }
    // Connections that arrived but were never accepted: reset them.
    for child in children {
        let mut c = child.tcb.lock();
        let seq = c.snd_nxt;
        segment(&mut c, seq, RST | ACK, &[]);
        finish(&mut c, None);
    }
    crate::proc::wake_all();
}

/// poll(): (something to read or an error, room to write, failed, other side gone).
pub fn readiness(conn: &Arc<Conn>) -> (bool, bool, bool, bool) {
    let t = conn.tcb.lock();
    let failed = t.error.is_some();
    if t.state == State::Listen {
        return (!t.accept_queue.is_empty(), false, false, false);
    }
    let readable = !t.recv_buf.is_empty() || t.peer_fin || failed || t.state == State::Closed && t.error.is_some();
    let writable = (t.connected() && !t.fin_queued && t.send_room() > 0) || failed;
    (readable, writable, failed, t.state == State::Closed)
}

// ---- segments arriving ----

fn find(local: u16, ip: Ip, port: u16) -> Option<Arc<Conn>> {
    let conns = CONNS.lock();
    let mut listener = None;
    for c in conns.iter() {
        let t = c.tcb.lock();
        if t.local_port != local || t.state == State::Closed {
            continue;
        }
        if t.state == State::Listen {
            listener = Some(c.clone());
        } else if t.remote_ip == ip && t.remote_port == port {
            return Some(c.clone());
        }
    }
    listener
}

fn parse_mss(options: &[u8]) -> usize {
    let mut i = 0;
    while i < options.len() {
        match options[i] {
            0 => break,
            1 => i += 1,
            kind => {
                let len = *options.get(i + 1).unwrap_or(&0) as usize;
                if len < 2 || i + len > options.len() {
                    break;
                }
                if kind == 2 && len == 4 {
                    return (be16(options, i + 2) as usize).clamp(64, MSS);
                }
                i += len;
            }
        }
    }
    536
}

pub fn receive(from: Ip, seg: &[u8]) {
    if seg.len() < 20 {
        return;
    }
    let me = super::config().ip;
    let len = seg.len();
    let pseudo = [from[0], from[1], from[2], from[3], me[0], me[1], me[2], me[3], 0, PROTO_TCP, (len >> 8) as u8, len as u8];
    if checksum(&[&pseudo, seg]) != 0 {
        return; // damaged on the way
    }
    let src = be16(seg, 0);
    let dst = be16(seg, 2);
    let seq = be32(seg, 4);
    let ack = be32(seg, 8);
    let offset = (seg[12] >> 4) as usize * 4;
    let flags = seg[13];
    let window = be16(seg, 14) as u32;
    if offset < 20 || offset > len {
        return;
    }
    let options = &seg[20..offset];
    let data = &seg[offset..];

    let Some(conn) = find(dst, from, src) else {
        if flags & RST == 0 {
            reset_reply(from, dst, src, seq, ack, flags, data.len());
        }
        return;
    };
    let state = conn.tcb.lock().state;
    match state {
        State::Listen => listening(&conn, from, src, dst, seq, flags, window, options),
        State::SynSent => syn_sent(&conn, seq, ack, flags, window, options),
        _ => connected(&conn, seq, ack, flags, window, data),
    }
    crate::proc::wake_all();
}

#[allow(clippy::too_many_arguments)]
fn listening(conn: &Arc<Conn>, from: Ip, src: u16, dst: u16, seq: u32, flags: u8, window: u32, options: &[u8]) {
    if flags & RST != 0 {
        return;
    }
    if flags & ACK != 0 {
        reset_reply(from, dst, src, seq, 0, flags, 0); // nothing to acknowledge here
        return;
    }
    if flags & SYN == 0 {
        return;
    }
    {
        let t = conn.tcb.lock();
        if t.accept_queue.len() >= t.backlog {
            return; // full: the other side will try again
        }
    }
    let child = new();
    {
        let mut c = child.tcb.lock();
        c.state = State::SynReceived;
        c.local_port = dst;
        c.remote_ip = from;
        c.remote_port = src;
        c.rcv_nxt = seq.wrapping_add(1);
        c.iss = new_iss();
        c.snd_una = c.iss;
        c.snd_nxt = c.iss.wrapping_add(1);
        c.snd_wnd = window;
        c.peer_mss = parse_mss(options);
        c.parent = Some(Arc::downgrade(conn));
        let iss = c.iss;
        segment(&mut c, iss, SYN | ACK, &[]);
        arm(&mut c);
    }
    CONNS.lock().push(child);
}

fn syn_sent(conn: &Arc<Conn>, seq: u32, ack: u32, flags: u8, window: u32, options: &[u8]) {
    let mut t = conn.tcb.lock();
    let ack_ok = flags & ACK != 0 && ack == t.iss.wrapping_add(1);
    if flags & ACK != 0 && !ack_ok {
        if flags & RST == 0 {
            let (ip, lp, rp) = (t.remote_ip, t.local_port, t.remote_port);
            drop(t);
            reset_reply(ip, lp, rp, seq, ack, flags, 0);
        }
        return;
    }
    if flags & RST != 0 {
        if ack_ok {
            finish(&mut t, Some(ECONNREFUSED));
        }
        return;
    }
    if flags & SYN != 0 && ack_ok {
        t.rcv_nxt = seq.wrapping_add(1);
        t.snd_una = ack;
        t.snd_wnd = window;
        t.peer_mss = parse_mss(options);
        t.state = State::Established;
        t.deadline = 0;
        t.rto = RTO_START;
        t.retries = 0;
        let seq = t.snd_nxt;
        segment(&mut t, seq, ACK, &[]);
        output(&mut t);
    }
}

fn connected(conn: &Arc<Conn>, seq: u32, ack: u32, flags: u8, window: u32, data: &[u8]) {
    let mut t = conn.tcb.lock();
    if flags & RST != 0 {
        // Only a reset that lands inside what we expect counts (a stray one could be forged).
        let in_window = !before(seq, t.rcv_nxt) && before(seq, t.rcv_nxt.wrapping_add(t.window().max(1) as u32));
        if in_window || seq == t.rcv_nxt {
            let error = if t.state == State::SynReceived { None } else { Some(ECONNRESET) };
            finish(&mut t, error);
        }
        return;
    }
    if flags & SYN != 0 && t.state != State::SynReceived {
        // A resent SYN from a connection we already have: just say where we are.
        let seq = t.snd_nxt;
        segment(&mut t, seq, ACK, &[]);
        return;
    }
    if flags & ACK == 0 {
        return;
    }

    // The handshake's last step, on the accepting side.
    if t.state == State::SynReceived {
        if ack != t.snd_nxt {
            return;
        }
        t.snd_una = ack;
        t.snd_wnd = window;
        t.state = State::Established;
        t.deadline = 0;
        t.retries = 0;
        let parent = t.parent.take().and_then(|p| p.upgrade());
        let fin_queued = t.fin_queued;
        drop(t);
        match parent {
            Some(p) => {
                let mut pt = p.tcb.lock();
                if pt.state == State::Listen && !pt.orphan {
                    pt.accept_queue.push_back(conn.clone());
                } else {
                    drop(pt);
                    let mut c = conn.tcb.lock();
                    let s = c.snd_nxt;
                    segment(&mut c, s, RST | ACK, &[]);
                    finish(&mut c, None);
                    return;
                }
            }
            None => {
                let mut c = conn.tcb.lock();
                let s = c.snd_nxt;
                segment(&mut c, s, RST | ACK, &[]);
                finish(&mut c, None);
                return;
            }
        }
        t = conn.tcb.lock();
        if fin_queued {
            output(&mut t);
        }
    }

    // What they've acknowledged.
    if before(t.snd_una, ack) && !before(t.snd_nxt, ack) {
        let acked = ack.wrapping_sub(t.snd_una) as usize;
        let data_acked = acked.min(t.send_buf.len());
        t.send_buf.drain(..data_acked);
        let fin_acked = t.fin_sent && acked > data_acked;
        t.snd_una = ack;
        t.rto = RTO_START;
        t.retries = 0;
        t.deadline = if t.snd_una == t.snd_nxt { 0 } else { now() + t.rto };
        if fin_acked {
            match t.state {
                State::FinWait1 => t.state = State::FinWait2,
                State::Closing => {
                    t.state = State::TimeWait;
                    t.expires = now() + TIME_WAIT;
                }
                State::LastAck => {
                    finish(&mut t, None);
                    return;
                }
                _ => {}
            }
        }
    }
    if !before(ack, t.snd_una) {
        t.snd_wnd = window;
    }

    // What they sent us. Their FIN, if any, comes right after the last byte of this segment.
    let fin_seq = seq.wrapping_add(data.len() as u32);
    let mut need_ack = false;
    let accepting = matches!(t.state, State::Established | State::FinWait1 | State::FinWait2);
    let mut seq = seq;
    let mut data = data;
    if !data.is_empty() {
        need_ack = true;
        // Part of it may be a resend of what we already have.
        if before(seq, t.rcv_nxt) {
            let skip = t.rcv_nxt.wrapping_sub(seq) as usize;
            data = if skip < data.len() { &data[skip..] } else { &[] };
            seq = t.rcv_nxt;
        }
        if accepting && seq == t.rcv_nxt && !data.is_empty() {
            let take = data.len().min(BUFFER - t.recv_buf.len());
            t.recv_buf.extend(&data[..take]);
            t.rcv_nxt = t.rcv_nxt.wrapping_add(take as u32);
            data = &data[take..];
        }
    }
    // Their FIN only counts once we have everything before it; a resent one is just acknowledged.
    if flags & FIN != 0 && (t.peer_fin || (data.is_empty() && fin_seq == t.rcv_nxt)) {
        need_ack = true;
        if !t.peer_fin {
            t.rcv_nxt = t.rcv_nxt.wrapping_add(1);
            t.peer_fin = true;
            match t.state {
                State::Established | State::SynReceived => t.state = State::CloseWait,
                State::FinWait1 => {
                    // Both closing at once; if ours was already acknowledged we'd be in FIN-WAIT-2.
                    t.state = State::Closing;
                }
                State::FinWait2 => {
                    t.state = State::TimeWait;
                    t.expires = now() + TIME_WAIT;
                }
                _ => {}
            }
        }
    }
    if need_ack {
        let s = t.snd_nxt;
        segment(&mut t, s, ACK, &[]);
    }
    output(&mut t);
}

/// Called on every timer tick: resend what's overdue, and let finished connections go.
pub fn tick() {
    let conns: Vec<Arc<Conn>> = CONNS.lock().clone();
    if conns.is_empty() {
        return;
    }
    let time = now();
    let mut woke = false;
    for c in &conns {
        let mut t = c.tcb.lock();
        if matches!(t.state, State::TimeWait) && time >= t.expires {
            finish(&mut t, None);
        }
        if t.orphan && t.expires != 0 && time >= t.expires && t.state != State::Closed {
            let s = t.snd_nxt;
            segment(&mut t, s, RST | ACK, &[]);
            finish(&mut t, None);
        }
        if t.state == State::Closed || t.deadline == 0 || time < t.deadline {
            continue;
        }
        t.retries += 1;
        if t.retries > RETRIES {
            finish(&mut t, Some(ETIMEDOUT));
            woke = true;
            continue;
        }
        t.rto = (t.rto * 2).min(RTO_MAX);
        retransmit(&mut t);
    }
    // Closed connections leave the table; a program still holding one keeps its own reference.
    CONNS.lock().retain(|c| c.tcb.lock().state != State::Closed);
    if woke {
        crate::proc::wake_all();
    }
}

pub fn local_port(conn: &Arc<Conn>) -> u16 {
    conn.tcb.lock().local_port
}

pub fn peer(conn: &Arc<Conn>) -> Option<(Ip, u16)> {
    let t = conn.tcb.lock();
    (t.state != State::Closed && t.state != State::Listen && t.remote_port != 0).then_some((t.remote_ip, t.remote_port))
}

/// SO_ERROR: the error from a failed connect, given once.
pub fn take_error(conn: &Arc<Conn>) -> Option<Errno> {
    conn.tcb.lock().error.take()
}

pub fn pending_bytes(conn: &Arc<Conn>) -> usize {
    conn.tcb.lock().recv_buf.len()
}
