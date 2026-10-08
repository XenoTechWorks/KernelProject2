//! The terminal, with Linux's line discipline: in normal ("canonical") mode the kernel collects a line,
//! echoing keys and handling Backspace, Ctrl+U, Ctrl+C and Ctrl+D, and a program's read() gets the
//! finished line. Programs like editors switch it to raw mode with TCSETS and get every key at once.
//! Output goes to the screen and to the serial port.

use crate::arch::serial;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

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
const VTIME: usize = 5;
const VMIN: usize = 6;
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

struct Tty {
    termios: Termios,
    editing: Vec<u8>,
    ready: VecDeque<u8>,
    eofs: usize,
    foreground: u32,
}

static TTY: Mutex<Option<Tty>> = Mutex::new(None);

pub fn init() {
    let mut cc = [0u8; 19];
    cc[VINTR] = 3;
    cc[VQUIT] = 0x1c;
    cc[VERASE] = 0x7f;
    cc[VKILL] = 0x15;
    cc[VEOF] = 4;
    cc[VMIN] = 1;
    cc[8] = 0x11; // VSTART
    cc[9] = 0x13; // VSTOP
    cc[10] = 0x1a; // VSUSP
    cc[VWERASE] = 0x17;
    let termios = Termios {
        iflag: ICRNL | 0o2000, // ICRNL | IXON
        oflag: OPOST | ONLCR,
        cflag: 0o17 | 0o60 | 0o200, // B38400 | CS8 | CREAD
        lflag: ISIG | ICANON | ECHO | ECHOE | 0o40 | ECHOCTL | 0o4000 | 0o100000,
        line: 0,
        cc,
    };
    *TTY.lock() = Some(Tty { termios, editing: Vec::new(), ready: VecDeque::new(), eofs: 0, foreground: 1 });
}

/// Writes program output to the screen and serial port, applying output processing.
pub fn write(bytes: &[u8]) {
    let onlcr = TTY.lock().as_ref().is_some_and(|t| t.termios.oflag & (OPOST | ONLCR) == OPOST | ONLCR);
    let mut out = Vec::with_capacity(bytes.len() + 8);
    for &b in bytes {
        if b == b'\n' && onlcr {
            out.push(b'\r');
        }
        out.push(b);
    }
    raw_output(&out);
}

fn raw_output(bytes: &[u8]) {
    for &b in bytes {
        serial::write_byte(b);
    }
    crate::console::write(bytes);
}

/// Bytes typed on the keyboard or received on the serial port.
pub fn input(bytes: &[u8]) {
    let mut signal = None;
    {
        let mut guard = TTY.lock();
        let Some(tty) = guard.as_mut() else { return };
        let t = tty.termios;
        let mut echo = Vec::new();
        for &raw in bytes {
            let b = if raw == b'\r' && t.iflag & ICRNL != 0 { b'\n' } else { raw };
            if t.lflag & ISIG != 0 && (b == t.cc[VINTR] || b == t.cc[VQUIT]) {
                signal = Some(if b == t.cc[VINTR] { crate::proc::SIGINT } else { crate::proc::SIGQUIT });
                tty.editing.clear();
                if t.lflag & ECHO != 0 {
                    echo.extend_from_slice(if b == t.cc[VINTR] { b"^C\n" } else { b"^\\\n" });
                }
                continue;
            }
            if t.lflag & ICANON == 0 {
                tty.ready.push_back(b);
                if t.lflag & ECHO != 0 {
                    echo.push(b);
                }
                continue;
            }
            if b == t.cc[VERASE] || b == 0x08 {
                if let Some(c) = tty.editing.pop() {
                    if t.lflag & ECHO != 0 {
                        let width = if c < 0x20 && t.lflag & ECHOCTL != 0 { 2 } else { 1 };
                        for _ in 0..width {
                            echo.extend_from_slice(b"\x08 \x08");
                        }
                    }
                }
            } else if b == t.cc[VKILL] || b == t.cc[VWERASE] {
                let keep = if b == t.cc[VWERASE] {
                    let trimmed = tty.editing.iter().rposition(|&c| c != b' ').map_or(0, |i| i + 1);
                    tty.editing[..trimmed].iter().rposition(|&c| c == b' ').map_or(0, |i| i + 1)
                } else {
                    0
                };
                if t.lflag & ECHO != 0 {
                    for _ in keep..tty.editing.len() {
                        echo.extend_from_slice(b"\x08 \x08");
                    }
                }
                tty.editing.truncate(keep);
            } else if b == t.cc[VEOF] {
                if tty.editing.is_empty() {
                    tty.eofs += 1;
                } else {
                    let line = core::mem::take(&mut tty.editing);
                    tty.ready.extend(line);
                }
            } else if b == b'\n' {
                tty.editing.push(b'\n');
                let line = core::mem::take(&mut tty.editing);
                tty.ready.extend(line);
                if t.lflag & ECHO != 0 {
                    echo.push(b'\n');
                }
            } else {
                tty.editing.push(b);
                if t.lflag & ECHO != 0 {
                    if b < 0x20 && b != b'\t' && t.lflag & ECHOCTL != 0 {
                        echo.extend_from_slice(&[b'^', b + 0x40]);
                    } else {
                        echo.push(b);
                    }
                }
            }
        }
        drop(guard);
        if !echo.is_empty() {
            write(&echo);
        }
    }
    if let Some(sig) = signal {
        crate::proc::signal_group(foreground(), sig);
    }
    crate::proc::wake_all();
}

/// Reads typed input. None means "nothing yet, wait"; Some(0) is end of file (Ctrl+D).
pub fn try_read(buf: &mut [u8]) -> Option<usize> {
    let mut guard = TTY.lock();
    let tty = guard.as_mut()?;
    let canonical = tty.termios.lflag & ICANON != 0;
    if tty.ready.is_empty() {
        if tty.eofs > 0 && canonical {
            tty.eofs -= 1;
            return Some(0);
        }
        if !canonical && tty.termios.cc[VMIN] == 0 {
            return Some(0);
        }
        return None;
    }
    let mut n = 0;
    while n < buf.len() {
        let Some(b) = tty.ready.pop_front() else { break };
        buf[n] = b;
        n += 1;
        if canonical && b == b'\n' {
            break;
        }
    }
    let _ = VTIME;
    Some(n)
}

pub fn available() -> usize {
    TTY.lock().as_ref().map_or(0, |t| t.ready.len())
}

pub fn termios() -> Termios {
    TTY.lock().as_ref().map(|t| t.termios).unwrap_or_default()
}

pub fn set_termios(termios: Termios) {
    if let Some(t) = TTY.lock().as_mut() {
        // Leaving canonical mode hands over what was typed so far.
        if t.termios.lflag & ICANON != 0 && termios.lflag & ICANON == 0 {
            let line = core::mem::take(&mut t.editing);
            t.ready.extend(line);
        }
        t.termios = termios;
    }
}

pub fn foreground() -> u32 {
    TTY.lock().as_ref().map_or(0, |t| t.foreground)
}

pub fn set_foreground(pgid: u32) {
    if let Some(t) = TTY.lock().as_mut() {
        t.foreground = pgid;
    }
}
