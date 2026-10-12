//! Terminals, with Linux's line discipline: in normal ("canonical") mode the kernel collects a line,
//! echoing keys and handling Backspace, Ctrl+U, Ctrl+C and Ctrl+D, and a program's read() gets the
//! finished line. Programs like editors switch it to raw mode with TCSETS and get every key at once.
//!
//! Each terminal is a `Tty`. There's the console's, whose output goes to the screen and the serial
//! port, and one for the inside of each pseudo-terminal (pty.rs), whose output goes to the program
//! holding the outside (an SSH server, a terminal multiplexer...), which also types into it.

use crate::arch::serial;
use crate::sys::errno::*;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize};
use spin::{Mutex, Once};

// termios flags (the values Linux uses).
const ICRNL: u32 = 0o400;
const OPOST: u32 = 0o1;
const ONLCR: u32 = 0o4;
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const ECHOE: u32 = 0o20;
const ECHOCTL: u32 = 0o1000;

const VINTR: usize = 0;
const VQUIT: usize = 1;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;
const VMIN: usize = 6;
const VSUSP: usize = 10;
const VWERASE: usize = 14;

/// The kernel's `struct termios`, as read and written by the TCGETS/TCSETS ioctls.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 19],
}

impl Termios {
    /// What a new terminal starts with (Linux's defaults).
    fn standard() -> Termios {
        let mut cc = [0u8; 19];
        cc[VINTR] = 3;
        cc[VQUIT] = 0x1c;
        cc[VERASE] = 0x7f;
        cc[VKILL] = 0x15;
        cc[VEOF] = 4;
        cc[VMIN] = 1;
        cc[8] = 0x11; // VSTART
        cc[9] = 0x13; // VSTOP
        cc[VSUSP] = 0x1a;
        cc[VWERASE] = 0x17;
        Termios {
            iflag: ICRNL | 0o2000, // ICRNL | IXON
            oflag: OPOST | ONLCR,
            cflag: 0o17 | 0o60 | 0o200, // B38400 | CS8 | CREAD
            lflag: ISIG | ICANON | ECHO | ECHOE | 0o40 | ECHOCTL | 0o4000 | 0o100000,
            line: 0,
            cc,
        }
    }
}

/// The line discipline's state.
struct Line {
    termios: Termios,
    editing: Vec<u8>,
    ready: VecDeque<u8>,
    eofs: usize,
    /// The process group that gets the keyboard (and Ctrl+C).
    foreground: u32,
    /// The session this is the controlling terminal of (0: none).
    session: u32,
    /// Rows and columns, for a pseudo-terminal (the console's come from the screen).
    size: (u16, u16),
    /// The outside of a pseudo-terminal has gone: reads find end-of-file, writes fail.
    hung_up: bool,
}

/// The outside of a pseudo-terminal, as the inside sees it.
pub struct Pty {
    pub index: u32,
    /// What the inside wrote (and echoed), for the outside to read.
    pub output: Mutex<VecDeque<u8>>,
    /// How many times the inside (/dev/pts/N) is open, and whether it ever was.
    pub opens: AtomicUsize,
    pub opened: AtomicBool,
    /// Set until unlockpt(): the inside can't be opened yet.
    pub locked: AtomicBool,
}

pub struct Tty {
    line: Mutex<Line>,
    /// None for the console.
    pub pty: Option<Pty>,
}

/// Output a pseudo-terminal holds for its outside before programs writing to it wait.
const PTY_BUFFER: usize = 64 * 1024;

static CONSOLE: Once<Arc<Tty>> = Once::new();

pub fn init() {
    CONSOLE.call_once(|| Tty::new(None));
    let console = console();
    let mut line = console.line.lock();
    line.foreground = 1;
    line.session = 1;
}

/// The console's terminal: the screen and keyboard, and the serial port.
pub fn console() -> Arc<Tty> {
    CONSOLE.get().expect("tty::init not called").clone()
}

impl Tty {
    pub fn new(pty: Option<Pty>) -> Arc<Tty> {
        let line = Line {
            termios: Termios::standard(),
            editing: Vec::new(),
            ready: VecDeque::new(),
            eofs: 0,
            foreground: 0,
            session: 0,
            size: (0, 0),
            hung_up: false,
        };
        Arc::new(Tty { line: Mutex::new(line), pty })
    }

    pub fn is_console(&self) -> bool {
        self.pty.is_none()
    }

    /// Bytes for the terminal's screen, as they are (after output processing).
    fn emit(&self, bytes: &[u8]) {
        match &self.pty {
            Some(pty) => {
                pty.output.lock().extend(bytes);
                crate::proc::wake_all();
            }
            None => {
                for &b in bytes {
                    serial::write_byte(b);
                }
                crate::console::write(bytes);
            }
        }
    }

    /// Applies output processing (a newline becomes CR LF) and shows the result.
    fn put(&self, bytes: &[u8]) {
        let onlcr = self.line.lock().termios.oflag & (OPOST | ONLCR) == OPOST | ONLCR;
        let mut out = Vec::with_capacity(bytes.len() + 8);
        for &b in bytes {
            if b == b'\n' && onlcr {
                out.push(b'\r');
            }
            out.push(b);
        }
        self.emit(&out);
    }

    /// A program writes to the terminal. On a pseudo-terminal it waits while the outside has a
    /// lot still to read; EIO once the outside has gone.
    pub fn write(&self, data: &[u8], nonblocking: bool) -> Result<usize, Errno> {
        if let Some(pty) = &self.pty {
            loop {
                if self.line.lock().hung_up {
                    return Err(EIO);
                }
                if pty.output.lock().len() < PTY_BUFFER {
                    break;
                }
                if nonblocking {
                    return Err(EAGAIN);
                }
                crate::proc::sleep()?;
            }
        }
        self.put(data);
        Ok(data.len())
    }

    /// Bytes typed: on the keyboard or serial port for the console, or written to the outside of
    /// a pseudo-terminal.
    pub fn input(&self, bytes: &[u8]) {
        let mut signal = None;
        let mut echo = Vec::new();
        let foreground = {
            let mut line = self.line.lock();
            let t = line.termios;
            for &raw in bytes {
                let b = if raw == b'\r' && t.iflag & ICRNL != 0 { b'\n' } else { raw };
                if t.lflag & ISIG != 0 && (b == t.cc[VINTR] || b == t.cc[VQUIT] || b == t.cc[VSUSP]) {
                    // Ctrl+C interrupts, Ctrl+\ quits, Ctrl+Z stops what runs in the foreground.
                    let (sig, shown): (usize, &[u8]) = match b {
                        _ if b == t.cc[VINTR] => (crate::proc::SIGINT, b"^C\n"),
                        _ if b == t.cc[VQUIT] => (crate::proc::SIGQUIT, b"^\\\n"),
                        _ => (crate::proc::signal::SIGTSTP, b"^Z"),
                    };
                    signal = Some(sig);
                    line.editing.clear();
                    if t.lflag & ECHO != 0 {
                        echo.extend_from_slice(shown);
                    }
                    continue;
                }
                if t.lflag & ICANON == 0 {
                    line.ready.push_back(b);
                    if t.lflag & ECHO != 0 {
                        echo.push(b);
                    }
                    continue;
                }
                if b == t.cc[VERASE] || b == 0x08 {
                    if let Some(c) = line.editing.pop() {
                        if t.lflag & ECHO != 0 {
                            let width = if c < 0x20 && t.lflag & ECHOCTL != 0 { 2 } else { 1 };
                            for _ in 0..width {
                                echo.extend_from_slice(b"\x08 \x08");
                            }
                        }
                    }
                } else if b == t.cc[VKILL] || b == t.cc[VWERASE] {
                    let keep = if b == t.cc[VWERASE] {
                        let trimmed = line.editing.iter().rposition(|&c| c != b' ').map_or(0, |i| i + 1);
                        line.editing[..trimmed].iter().rposition(|&c| c == b' ').map_or(0, |i| i + 1)
                    } else {
                        0
                    };
                    if t.lflag & ECHO != 0 {
                        for _ in keep..line.editing.len() {
                            echo.extend_from_slice(b"\x08 \x08");
                        }
                    }
                    line.editing.truncate(keep);
                } else if b == t.cc[VEOF] {
                    if line.editing.is_empty() {
                        line.eofs += 1;
                    } else {
                        let typed = core::mem::take(&mut line.editing);
                        line.ready.extend(typed);
                    }
                } else if b == b'\n' {
                    line.editing.push(b'\n');
                    let typed = core::mem::take(&mut line.editing);
                    line.ready.extend(typed);
                    if t.lflag & ECHO != 0 {
                        echo.push(b'\n');
                    }
                } else {
                    line.editing.push(b);
                    if t.lflag & ECHO != 0 {
                        if b < 0x20 && b != b'\t' && t.lflag & ECHOCTL != 0 {
                            echo.extend_from_slice(&[b'^', b + 0x40]);
                        } else {
                            echo.push(b);
                        }
                    }
                }
            }
            line.foreground
        };
        if !echo.is_empty() {
            self.put(&echo);
        }
        if let Some(sig) = signal {
            crate::proc::signal_group(foreground, sig);
        }
        crate::proc::wake_all();
    }

    /// Reads typed input. None means "nothing yet, wait"; Some(0) is end of file (Ctrl+D).
    pub fn try_read(&self, buf: &mut [u8]) -> Option<usize> {
        let mut line = self.line.lock();
        let canonical = line.termios.lflag & ICANON != 0;
        if line.ready.is_empty() {
            if line.eofs > 0 && canonical {
                line.eofs -= 1;
                return Some(0);
            }
            if !canonical && line.termios.cc[VMIN] == 0 {
                return Some(0);
            }
            return None;
        }
        let mut n = 0;
        while n < buf.len() {
            let Some(b) = line.ready.pop_front() else { break };
            buf[n] = b;
            n += 1;
            if canonical && b == b'\n' {
                break;
            }
        }
        Some(n)
    }

    pub fn read(&self, buf: &mut [u8], nonblocking: bool) -> Result<usize, Errno> {
        self.check_background_read()?;
        loop {
            if let Some(n) = self.try_read(buf) {
                return Ok(n);
            }
            // The outside of a pseudo-terminal has gone, and everything typed has been read.
            if self.hung_up() {
                return Err(EIO);
            }
            if nonblocking {
                return Err(EAGAIN);
            }
            crate::proc::sleep()?;
        }
    }

    /// Job control: a process in the background that reads its terminal is stopped (SIGTTIN, to
    /// its whole group) so it doesn't take what's typed for the job in front; if it ignores or
    /// blocks SIGTTIN, the read fails instead.
    fn check_background_read(&self) -> Result<(), Errno> {
        let p = crate::proc::current();
        let foreground = self.foreground();
        let controls = p.ctty.as_ref().is_some_and(|c| core::ptr::eq(&**c, self));
        if !controls || foreground == 0 || p.pgid == foreground {
            return Ok(());
        }
        let sig = crate::proc::signal::SIGTTIN;
        if p.handlers()[sig].handler == crate::proc::signal::SIG_IGN || p.sigmask & crate::proc::signal::bit(sig) != 0 {
            return Err(EIO);
        }
        crate::proc::signal_group(p.pgid, sig);
        Err(EINTR)
    }

    pub fn available(&self) -> usize {
        self.line.lock().ready.len()
    }

    /// Would a read return without waiting?
    pub fn readable(&self) -> bool {
        let line = self.line.lock();
        !line.ready.is_empty() || line.eofs > 0 || line.hung_up
    }

    pub fn hung_up(&self) -> bool {
        self.line.lock().hung_up
    }

    /// Throws away what was typed and not read yet (tcflush).
    pub fn flush_input(&self) {
        let mut line = self.line.lock();
        line.ready.clear();
        line.editing.clear();
    }

    pub fn termios(&self) -> Termios {
        self.line.lock().termios
    }

    pub fn set_termios(&self, termios: Termios) {
        let mut line = self.line.lock();
        // Leaving canonical mode hands over what was typed so far.
        if line.termios.lflag & ICANON != 0 && termios.lflag & ICANON == 0 {
            let typed = core::mem::take(&mut line.editing);
            line.ready.extend(typed);
        }
        line.termios = termios;
    }

    pub fn foreground(&self) -> u32 {
        self.line.lock().foreground
    }

    pub fn set_foreground(&self, pgid: u32) {
        self.line.lock().foreground = pgid;
    }

    pub fn session(&self) -> u32 {
        self.line.lock().session
    }

    /// Becomes the controlling terminal of `session`, with `pgid` in the foreground.
    pub fn control(&self, session: u32, pgid: u32) {
        let mut line = self.line.lock();
        line.session = session;
        line.foreground = pgid;
    }

    /// (rows, columns)
    pub fn size(&self) -> (u16, u16) {
        if self.is_console() {
            let (cols, rows) = crate::console::size();
            return (rows as u16, cols as u16);
        }
        self.line.lock().size
    }

    /// A new size, from whoever holds the outside: the foreground hears about it (SIGWINCH).
    pub fn set_size(&self, rows: u16, cols: u16) {
        if self.is_console() {
            return;
        }
        let foreground = {
            let mut line = self.line.lock();
            if line.size == (rows, cols) {
                return;
            }
            line.size = (rows, cols);
            line.foreground
        };
        crate::proc::signal_group(foreground, crate::proc::signal::SIGWINCH);
    }

    /// The outside of a pseudo-terminal is gone: like a modem hanging up, the session's programs
    /// get SIGHUP, and reading and writing fail (EIO).
    pub fn hang_up(&self) {
        let (foreground, session) = {
            let mut line = self.line.lock();
            line.hung_up = true;
            (line.foreground, line.session)
        };
        crate::proc::signal_group(foreground, crate::proc::signal::SIGHUP);
        if session != 0 {
            let _ = crate::proc::signal_process(session, crate::proc::signal::SIGHUP);
        }
        crate::proc::wake_all();
    }
}

/// Typed on the keyboard or received on the serial port.
pub fn input(bytes: &[u8]) {
    if let Some(console) = CONSOLE.get() {
        console.input(bytes);
    }
}
