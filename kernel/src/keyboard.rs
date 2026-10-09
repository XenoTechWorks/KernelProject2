//! PS/2 keyboard: scancode set 1 (what QEMU and most PCs send by default) to bytes, US layout.
//! Arrow keys and friends become the same escape sequences a Linux terminal sends. Graphical
//! windows get a key code instead: the character for ordinary keys (Enter 13, Backspace 8, Tab 9,
//! Escape 27, Delete 127) and these for the others, with modifiers (1 shift, 2 ctrl).

use spin::Mutex;

struct State {
    shift: bool,
    ctrl: bool,
    caps: bool,
    extended: bool,
}

static STATE: Mutex<State> = Mutex::new(State { shift: false, ctrl: false, caps: false, extended: false });

// Backspace sends DEL and Enter sends CR, like the Linux console; the tty turns CR into a newline.
pub const KEY_UP: u32 = 0x100;
pub const KEY_DOWN: u32 = 0x101;
pub const KEY_RIGHT: u32 = 0x102;
pub const KEY_LEFT: u32 = 0x103;
pub const KEY_HOME: u32 = 0x104;
pub const KEY_END: u32 = 0x105;

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
        // F12 moves the keyboard between the console and the windows.
        0x58 if !released && !extended => {
            drop(s);
            crate::console::cycle_focus();
            return;
        }
        _ => {}
    }
    if released {
        return;
    }

    if extended {
        let (seq, code): (&[u8], u32) = match key {
            0x48 => (b"\x1b[A", KEY_UP),
            0x50 => (b"\x1b[B", KEY_DOWN),
            0x4D => (b"\x1b[C", KEY_RIGHT),
            0x4B => (b"\x1b[D", KEY_LEFT),
            0x47 => (b"\x1b[H", KEY_HOME),
            0x4F => (b"\x1b[F", KEY_END),
            0x53 => (b"\x1b[3~", 127),
            0x1C => (b"\r", 13), // keypad enter
            _ => return,
        };
        let mods = mods(&s);
        drop(s);
        crate::console::key(seq, code, mods);
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
    let code = if byte == 0x7f { 8 } else { byte as u32 };
    if s.ctrl && byte.is_ascii_alphabetic() {
        byte = byte.to_ascii_lowercase() - b'a' + 1; // Ctrl+C = 3, Ctrl+D = 4, ...
    }
    let mods = mods(&s);
    drop(s);
    crate::console::key(&[byte], code, mods);
}

fn mods(s: &State) -> u32 {
    s.shift as u32 | (s.ctrl as u32) << 1
}
