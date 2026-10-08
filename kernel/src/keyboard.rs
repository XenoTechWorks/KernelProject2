//! PS/2 keyboard: scancode set 1 (what QEMU and most PCs send by default) to bytes, US layout.
//! Arrow keys and friends become the same escape sequences a Linux terminal sends.

use spin::Mutex;

struct State {
    shift: bool,
    ctrl: bool,
    caps: bool,
    extended: bool,
}

static STATE: Mutex<State> = Mutex::new(State { shift: false, ctrl: false, caps: false, extended: false });

// Backspace sends DEL and Enter sends CR, like the Linux console; the tty turns CR into a newline.
const NORMAL: &[u8; 58] = b"\0\x1b1234567890-=\x7f\tqwertyuiop[]\r\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
const SHIFTED: &[u8; 58] = b"\0\x1b!@#$%^&*()_+\x7f\tQWERTYUIOP{}\r\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";

pub fn scancode(code: u8) {
    let mut s = STATE.lock();
    if code == 0xE0 {
        s.extended = true;
        return;
    }
    let released = code & 0x80 != 0;
    let key = code & 0x7f;
    let extended = core::mem::take(&mut s.extended);

    match key {
        0x2A | 0x36 => {
            s.shift = !released;
            return;
        }
        0x1D => {
            s.ctrl = !released;
            return;
        }
        0x3A if !released => {
            s.caps = !s.caps;
            return;
        }
        _ => {}
    }
    if released {
        return;
    }

    if extended {
        let seq: &[u8] = match key {
            0x48 => b"\x1b[A",
            0x50 => b"\x1b[B",
            0x4D => b"\x1b[C",
            0x4B => b"\x1b[D",
            0x47 => b"\x1b[H",
            0x4F => b"\x1b[F",
            0x53 => b"\x1b[3~",
            0x1C => b"\r", // keypad enter
            _ => return,
        };
        drop(s);
        crate::tty::input(seq);
        return;
    }

    let Some(&base) = NORMAL.get(key as usize) else { return };
    if base == 0 {
        return;
    }
    let mut byte = if s.shift { SHIFTED[key as usize] } else { base };
    if s.caps && byte.is_ascii_alphabetic() {
        byte ^= 0x20;
    }
    if s.ctrl && byte.is_ascii_alphabetic() {
        byte = byte.to_ascii_lowercase() - b'a' + 1; // Ctrl+C = 3, Ctrl+D = 4, ...
    }
    drop(s);
    crate::tty::input(&[byte]);
}
