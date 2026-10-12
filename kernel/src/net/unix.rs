//! Unix sockets: connections between programs on this machine, found by a file name (a socket
//! file, made by bind), by an "abstract" name that lives only in the kernel, or made already
//! connected in pairs by socketpair.
//!
//! - A stream connection is two pipes, one each way (fs/pipe.rs); closing an end is closing its
//!   pipe ends, so end-of-file and EPIPE work just as they do for pipes.
//! - A datagram socket has a queue of whole messages, each with the sender's name.
//!
//! Either kind can carry open files along (SCM_RIGHTS): a datagram has them with it; on a stream
//! they're tied to the bytes they were sent with, and as on Linux a read that takes them ends
//! with those bytes, so the next files come with the next read.
//!
//! The file system call layer (sys/net.rs) turns addresses into names; this file only sees names.

use crate::fs::pipe::{self, Pipe};
use crate::proc::fd::OpenFile;
use crate::sys::errno::*;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::Mutex;

/// Where a socket can be found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Name {
    /// A socket file: the path it was bound at, and which file that is (device, inode), since a
    /// file can be renamed or reached by other paths.
    Path(String, (u64, u64)),
    /// A name in the kernel only (an address starting with a 0 byte); gone with the socket.
    Abstract(Vec<u8>),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    File(u64, u64),
    Abstract(Vec<u8>),
}

impl Name {
    fn key(&self) -> Key {
        match self {
            Name::Path(_, (dev, ino)) => Key::File(*dev, *ino),
            Name::Abstract(bytes) => Key::Abstract(bytes.clone()),
        }
    }
}

/// Who made a socket, for SO_PEERCRED: the other side can ask who it's talking to.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Cred {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

fn caller() -> Cred {
    let p = crate::proc::current();
    Cred { pid: p.tgid, uid: p.creds.euid, gid: p.creds.egid }
}

enum Bound {
    Stream(Weak<Stream>),
    Datagram(Weak<Datagram>),
}

impl Bound {
    fn alive(&self) -> bool {
        match self {
            Bound::Stream(s) => s.strong_count() > 0,
            Bound::Datagram(d) => d.strong_count() > 0,
        }
    }
}

/// Every bound socket, by name.
static NAMES: Mutex<BTreeMap<Key, Bound>> = Mutex::new(BTreeMap::new());

fn claim(name: &Name, bound: Bound) -> Result<(), Errno> {
    let mut names = NAMES.lock();
    if names.get(&name.key()).is_some_and(Bound::alive) {
        return Err(EADDRINUSE);
    }
    names.insert(name.key(), bound);
    Ok(())
}

/// Forgets a name whose socket is gone (called as it goes).
fn release(name: &Option<Name>) {
    if let Some(name) = name {
        let mut names = NAMES.lock();
        if names.get(&name.key()).is_some_and(|b| !b.alive()) {
            names.remove(&name.key());
        }
    }
}

// ---- streams ----

pub struct Stream {
    state: Mutex<State>,
    name: Mutex<Option<Name>>,
    owner: Cred,
}

enum State {
    Idle,
    /// Connections that have arrived, waiting for accept.
    Listening { queue: VecDeque<Arc<Stream>>, max: usize },
    Connected(Link),
}

/// One way of a stream connection: the bytes, and the files sent along with them.
#[derive(Clone)]
struct Way {
    pipe: Arc<Pipe>,
    files: Arc<Mutex<Attached>>,
}

impl Way {
    fn new() -> Way {
        Way { pipe: pipe::new(), files: Arc::new(Mutex::new(Attached::default())) }
    }
}

/// Files sent on a stream, with where in the stream the bytes they came with start and end.
#[derive(Default)]
struct Attached {
    sent: u64,
    read: u64,
    queue: VecDeque<(u64, u64, Vec<Arc<OpenFile>>)>,
}

struct Link {
    rx: Way,
    tx: Way,
    peer: Option<Name>,
    peer_cred: Cred,
    reading: bool,
    writing: bool,
}

impl Link {
    fn new(rx: Way, tx: Way, peer: Option<Name>, peer_cred: Cred) -> Link {
        *rx.pipe.readers.lock() += 1;
        *tx.pipe.writers.lock() += 1;
        Link { rx, tx, peer, peer_cred, reading: true, writing: true }
    }

    fn stop_reading(&mut self) {
        if core::mem::take(&mut self.reading) {
            *self.rx.pipe.readers.lock() -= 1;
            self.rx.files.lock().queue.clear(); // nobody will take them now
        }
    }

    fn stop_writing(&mut self) {
        if core::mem::take(&mut self.writing) {
            *self.tx.pipe.writers.lock() -= 1;
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let State::Connected(link) = self.state.get_mut() {
            link.stop_reading();
            link.stop_writing();
        }
        release(self.name.get_mut());
        crate::proc::wake_all();
    }
}

impl Stream {
    pub fn new() -> Arc<Stream> {
        Arc::new(Stream { state: Mutex::new(State::Idle), name: Mutex::new(None), owner: caller() })
    }

    pub fn pair() -> (Arc<Stream>, Arc<Stream>) {
        let (a, b) = (Stream::new(), Stream::new());
        let (up, down) = (Way::new(), Way::new());
        *a.state.lock() = State::Connected(Link::new(down.clone(), up.clone(), None, b.owner));
        *b.state.lock() = State::Connected(Link::new(up, down, None, a.owner));
        (a, b)
    }
}

pub fn bind_stream(s: &Arc<Stream>, name: Name) -> Result<(), Errno> {
    let mut mine = s.name.lock();
    if mine.is_some() {
        return Err(EINVAL);
    }
    claim(&name, Bound::Stream(Arc::downgrade(s)))?;
    *mine = Some(name);
    Ok(())
}

pub fn listen(s: &Stream, backlog: usize) -> Result<(), Errno> {
    let mut state = s.state.lock();
    let max = backlog.clamp(1, 4096);
    match &mut *state {
        State::Idle => *state = State::Listening { queue: VecDeque::new(), max },
        State::Listening { max: m, .. } => *m = max,
        State::Connected(_) => return Err(EINVAL),
    }
    Ok(())
}

/// Connects to the stream socket listening at `name`. Like Linux, it's done at once: the new
/// connection waits in the listener's queue for accept, and data can flow straight away.
pub fn connect(s: &Stream, name: &Name, nonblocking: bool) -> Result<(), Errno> {
    loop {
        if !matches!(*s.state.lock(), State::Idle) {
            return Err(EISCONN);
        }
        let listener = match NAMES.lock().get(&name.key()) {
            Some(Bound::Stream(l)) => l.upgrade(),
            Some(Bound::Datagram(_)) => return Err(EPROTOTYPE),
            None => None,
        };
        let listener = listener.ok_or(ECONNREFUSED)?;
        {
            let mut state = listener.state.lock();
            let State::Listening { queue, max } = &mut *state else { return Err(ECONNREFUSED) };
            if queue.len() < *max {
                let (up, down) = (Way::new(), Way::new());
                let server = Arc::new(Stream {
                    state: Mutex::new(State::Connected(Link::new(up.clone(), down.clone(), s.name.lock().clone(), caller()))),
                    name: Mutex::new(listener.name.lock().clone()),
                    owner: listener.owner,
                });
                queue.push_back(server);
                *s.state.lock() = State::Connected(Link::new(down, up, Some(name.clone()), listener.owner));
                drop(state);
                crate::proc::wake_all();
                return Ok(());
            }
        }
        // The listener's queue is full: wait for it to accept some.
        if nonblocking {
            return Err(EAGAIN);
        }
        crate::proc::sleep()?;
    }
}

pub fn accept(s: &Stream, nonblocking: bool) -> Result<Arc<Stream>, Errno> {
    loop {
        {
            let mut state = s.state.lock();
            let State::Listening { queue, .. } = &mut *state else { return Err(EINVAL) };
            if let Some(conn) = queue.pop_front() {
                drop(state);
                crate::proc::wake_all(); // connectors waiting for room in the queue
                return Ok(conn);
            }
        }
        if nonblocking {
            return Err(EAGAIN);
        }
        crate::proc::sleep()?;
    }
}

/// The two ways of a connection, and whether each is still open on this side.
fn link_of(s: &Stream) -> Result<(Way, Way, bool, bool), Errno> {
    match &*s.state.lock() {
        State::Connected(l) => Ok((l.rx.clone(), l.tx.clone(), l.reading, l.writing)),
        State::Listening { .. } => Err(EINVAL),
        State::Idle => Err(ENOTCONN),
    }
}

/// Reads, and takes the files of the first bytes read that have some. A read that reaches bytes
/// with files ends where those bytes end.
pub fn receive(s: &Stream, buf: &mut [u8], nonblocking: bool, peek: bool) -> Result<(usize, Vec<Arc<OpenFile>>), Errno> {
    let (rx, _, reading, _) = link_of(s)?;
    if !reading {
        return Ok((0, Vec::new()));
    }
    let limit = {
        let files = rx.files.lock();
        files.queue.front().map_or(buf.len(), |(_, end, _)| ((end - files.read) as usize).min(buf.len()))
    };
    let n = pipe::read(&rx.pipe, &mut buf[..limit], nonblocking, peek)?;
    if peek || n == 0 {
        return Ok((n, Vec::new()));
    }
    let mut files = rx.files.lock();
    files.read += n as u64;
    let reached = files.queue.front().is_some_and(|(at, _, _)| *at < files.read);
    let came = if reached { files.queue.pop_front().map(|(_, _, f)| f).unwrap_or_default() } else { Vec::new() };
    Ok((n, came))
}

pub fn read(s: &Stream, buf: &mut [u8], nonblocking: bool, peek: bool) -> Result<usize, Errno> {
    // A plain read can't take files; any that came are closed, as on Linux.
    receive(s, buf, nonblocking, peek).map(|(n, _)| n)
}

/// Writes, with `files` going along with the first byte. EPIPE once the other side has gone (or
/// this side shut its writing); the caller sends SIGPIPE.
pub fn send_stream(s: &Stream, data: &[u8], files: Vec<Arc<OpenFile>>, nonblocking: bool) -> Result<usize, Errno> {
    let (_, tx, _, writing) = link_of(s)?;
    if !writing {
        return Err(EPIPE);
    }
    if files.is_empty() {
        let n = pipe::write(&tx.pipe, data, nonblocking)?;
        tx.files.lock().sent += n as u64;
        return Ok(n);
    }
    if data.is_empty() {
        return Err(EINVAL); // files need at least one byte to travel with
    }
    let at = tx.files.lock().sent;
    tx.files.lock().queue.push_back((at, at + data.len() as u64, files));
    match pipe::write(&tx.pipe, data, nonblocking) {
        Ok(n) => {
            let mut attached = tx.files.lock();
            attached.sent += n as u64;
            if let Some(entry) = attached.queue.iter_mut().find(|(a, _, _)| *a == at) {
                entry.1 = at + n as u64; // all that went, if not all did
            }
            Ok(n)
        }
        Err(e) => {
            tx.files.lock().queue.retain(|(a, _, _)| *a != at);
            Err(e)
        }
    }
}

/// shutdown(): 0 stops reading, 1 stops writing (the other side reads end-of-file), 2 both.
pub fn shutdown(s: &Stream, how: u32) -> Result<(), Errno> {
    let mut state = s.state.lock();
    let State::Connected(link) = &mut *state else { return Err(ENOTCONN) };
    if how != 1 {
        link.stop_reading();
    }
    if how != 0 {
        link.stop_writing();
    }
    drop(state);
    crate::proc::wake_all();
    Ok(())
}

pub struct Ready {
    pub read: bool,
    pub write: bool,
    /// The other side won't send any more.
    pub read_closed: bool,
    /// Neither way is open any more.
    pub hangup: bool,
}

pub fn stream_readiness(s: &Stream) -> Ready {
    match &*s.state.lock() {
        State::Idle => Ready { read: false, write: false, read_closed: false, hangup: false },
        State::Listening { queue, .. } => Ready { read: !queue.is_empty(), write: false, read_closed: false, hangup: false },
        State::Connected(l) => {
            let ended = *l.rx.pipe.writers.lock() == 0 || !l.reading;
            let peer_reading = *l.tx.pipe.readers.lock() > 0 && l.writing;
            let room = l.tx.pipe.buffer.lock().len() < pipe::CAPACITY;
            Ready {
                read: ended || !l.rx.pipe.buffer.lock().is_empty(),
                // A write that would fail with EPIPE counts as ready too: it doesn't wait.
                write: room || !peer_reading,
                read_closed: ended,
                hangup: ended && !peer_reading,
            }
        }
    }
}

/// Bytes waiting to be read (FIONREAD).
pub fn pending(s: &Stream) -> usize {
    match &*s.state.lock() {
        State::Connected(l) => l.rx.pipe.buffer.lock().len(),
        _ => 0,
    }
}

pub fn stream_name(s: &Stream) -> Option<Name> {
    s.name.lock().clone()
}

pub fn stream_peer(s: &Stream) -> Result<(Option<Name>, Cred), Errno> {
    match &*s.state.lock() {
        State::Connected(l) => Ok((l.peer.clone(), l.peer_cred)),
        _ => Err(ENOTCONN),
    }
}

// ---- datagrams ----

pub struct Datagram {
    queue: Mutex<VecDeque<Message>>,
    name: Mutex<Option<Name>>,
    /// Where send() without an address goes. Once set, the peer going away is end-of-file.
    peer: Mutex<Option<(Weak<Datagram>, Option<Name>)>>,
    owner: Cred,
}

pub struct Message {
    pub data: Vec<u8>,
    pub from: Option<Name>,
    pub files: Vec<Arc<OpenFile>>,
}

/// Messages a socket holds before senders wait.
const QUEUE_MAX: usize = 256;

impl Drop for Datagram {
    fn drop(&mut self) {
        release(self.name.get_mut());
        crate::proc::wake_all();
    }
}

impl Datagram {
    pub fn new() -> Arc<Datagram> {
        Arc::new(Datagram { queue: Mutex::new(VecDeque::new()), name: Mutex::new(None), peer: Mutex::new(None), owner: caller() })
    }

    pub fn pair() -> (Arc<Datagram>, Arc<Datagram>) {
        let (a, b) = (Datagram::new(), Datagram::new());
        *a.peer.lock() = Some((Arc::downgrade(&b), None));
        *b.peer.lock() = Some((Arc::downgrade(&a), None));
        (a, b)
    }
}

pub fn bind_datagram(d: &Arc<Datagram>, name: Name) -> Result<(), Errno> {
    let mut mine = d.name.lock();
    if mine.is_some() {
        return Err(EINVAL);
    }
    claim(&name, Bound::Datagram(Arc::downgrade(d)))?;
    *mine = Some(name);
    Ok(())
}

fn find_datagram(name: &Name) -> Result<Arc<Datagram>, Errno> {
    match NAMES.lock().get(&name.key()) {
        Some(Bound::Datagram(d)) => d.upgrade().ok_or(ECONNREFUSED),
        Some(Bound::Stream(_)) => Err(EPROTOTYPE),
        None => Err(ECONNREFUSED),
    }
}

pub fn connect_datagram(d: &Datagram, name: &Name) -> Result<(), Errno> {
    let target = find_datagram(name)?;
    *d.peer.lock() = Some((Arc::downgrade(&target), Some(name.clone())));
    Ok(())
}

pub fn send(d: &Datagram, to: Option<&Name>, data: &[u8], files: Vec<Arc<OpenFile>>, nonblocking: bool) -> Result<usize, Errno> {
    let target = match to {
        Some(name) => find_datagram(name)?,
        None => match &*d.peer.lock() {
            Some((peer, _)) => peer.upgrade().ok_or(ECONNREFUSED)?,
            None => return Err(ENOTCONN),
        },
    };
    loop {
        {
            let mut queue = target.queue.lock();
            if queue.len() < QUEUE_MAX {
                queue.push_back(Message { data: data.to_vec(), from: d.name.lock().clone(), files: files.clone() });
                drop(queue);
                crate::proc::wake_all();
                return Ok(data.len());
            }
        }
        if nonblocking {
            return Err(EAGAIN);
        }
        crate::proc::sleep()?;
    }
}

/// A message received: how much of it fitted, its full length, who sent it, and files with it.
pub struct Received {
    pub len: usize,
    pub full: usize,
    pub from: Option<Name>,
    pub files: Vec<Arc<OpenFile>>,
}

/// The next message (cut to `buf.len()`).
pub fn recv(d: &Datagram, buf: &mut [u8], nonblocking: bool, peek: bool) -> Result<Received, Errno> {
    loop {
        {
            let mut queue = d.queue.lock();
            if let Some(m) = queue.front() {
                let n = m.data.len().min(buf.len());
                buf[..n].copy_from_slice(&m.data[..n]);
                let mut got = Received { len: n, full: m.data.len(), from: m.from.clone(), files: Vec::new() };
                if !peek {
                    got.files = queue.pop_front().map(|m| m.files).unwrap_or_default();
                    drop(queue);
                    crate::proc::wake_all();
                }
                return Ok(got);
            }
        }
        if peer_gone(d) {
            return Ok(Received { len: 0, full: 0, from: None, files: Vec::new() });
        }
        if nonblocking {
            return Err(EAGAIN);
        }
        crate::proc::sleep()?;
    }
}

fn peer_gone(d: &Datagram) -> bool {
    d.peer.lock().as_ref().is_some_and(|(p, _)| p.strong_count() == 0)
}

/// (something to read, room to send)
pub fn datagram_readiness(d: &Datagram) -> (bool, bool) {
    let read = !d.queue.lock().is_empty() || peer_gone(d);
    let write = match &*d.peer.lock() {
        Some((peer, _)) => peer.upgrade().is_none_or(|p| p.queue.lock().len() < QUEUE_MAX),
        None => true,
    };
    (read, write)
}

pub fn datagram_pending(d: &Datagram) -> usize {
    d.queue.lock().front().map_or(0, |m| m.data.len())
}

pub fn datagram_name(d: &Datagram) -> Option<Name> {
    d.name.lock().clone()
}

pub fn datagram_peer(d: &Datagram) -> Result<(Option<Name>, Cred), Errno> {
    match &*d.peer.lock() {
        Some((peer, name)) => Ok((name.clone(), peer.upgrade().map_or(Cred::default(), |p| p.owner))),
        None => Err(ENOTCONN),
    }
}
