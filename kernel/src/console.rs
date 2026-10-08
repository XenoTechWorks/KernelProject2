//! Text console on the boot framebuffer. Understands the common VT100/xterm escape sequences
//! (cursor movement, clearing, 16 colors), which is what Linux command line programs print.

use alloc::vec;
use alloc::vec::Vec;
use font8x8::legacy::BASIC_LEGACY;
use spin::Mutex;

const SCALE: usize = 2;
const CELL_W: usize = 8 * SCALE;
const CELL_H: usize = 8 * SCALE;

// The standard 16 terminal colors (a calm palette, close to modern terminal defaults).
const PALETTE: [u32; 16] = [
    0x1d2129, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
    0x5c6370, 0xff7b86, 0xb5e890, 0xffd68a, 0x82c4ff, 0xde9bf0, 0x7fd8e2, 0xffffff,
];
const DEFAULT_FG: u8 = 7;
const DEFAULT_BG: u8 = 0;

#[derive(Clone, Copy)]
struct Cell {
    ch: u8,
    fg: u8,
    bg: u8,
}

const BLANK: Cell = Cell { ch: b' ', fg: DEFAULT_FG, bg: DEFAULT_BG };

enum Escape {
    None,
    Esc,
    Csi { params: [u16; 8], count: usize, private: bool },
}

pub struct Console {
    fb: *mut u32,
    pitch: usize, // in pixels
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    x: usize,
    y: usize,
    fg: u8,
    bg: u8,
    bold: bool,
    escape: Escape,
    saved: (usize, usize),
}

unsafe impl Send for Console {}

pub static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);

pub fn init(fb: &limine::framebuffer::Framebuffer) {
    if fb.bpp != 32 {
        return;
    }
    let cols = fb.width as usize / CELL_W;
    let rows = fb.height as usize / CELL_H;
    let mut console = Console {
        fb: fb.address() as *mut u32,
        pitch: fb.pitch as usize / 4,
        cols,
        rows,
        cells: vec![BLANK; cols * rows],
        x: 0,
        y: 0,
        fg: DEFAULT_FG,
        bg: DEFAULT_BG,
        bold: false,
        escape: Escape::None,
        saved: (0, 0),
    };
    console.redraw();
    *CONSOLE.lock() = Some(console);
}

/// Columns and rows, for TIOCGWINSZ.
pub fn size() -> (usize, usize) {
    CONSOLE.lock().as_ref().map_or((80, 25), |c| (c.cols, c.rows))
}

pub fn write(bytes: &[u8]) {
    if let Some(console) = CONSOLE.lock().as_mut() {
        console.write(bytes);
    }
}

/// Writes only if nobody holds the console (for the panic handler, which may have interrupted it).
pub fn try_write(bytes: &[u8]) {
    if let Some(mut guard) = CONSOLE.try_lock() {
        if let Some(console) = guard.as_mut() {
            console.write(bytes);
        }
    }
}

impl Console {
    fn write(&mut self, bytes: &[u8]) {
        self.hide_cursor();
        for &b in bytes {
            self.feed(b);
        }
        self.show_cursor();
    }

    fn feed(&mut self, b: u8) {
        match core::mem::replace(&mut self.escape, Escape::None) {
            Escape::None => self.plain(b),
            Escape::Esc => match b {
                b'[' => self.escape = Escape::Csi { params: [0; 8], count: 0, private: false },
                b'7' => self.saved = (self.x, self.y),
                b'8' => (self.x, self.y) = self.saved,
                b'c' => self.reset(),
                _ => {}
            },
            Escape::Csi { mut params, mut count, mut private } => match b {
                b'0'..=b'9' => {
                    if count == 0 {
                        count = 1;
                    }
                    if count <= params.len() {
                        let p = &mut params[count - 1];
                        *p = p.saturating_mul(10).saturating_add((b - b'0') as u16);
                    }
                    self.escape = Escape::Csi { params, count, private };
                }
                b';' => {
                    count = if count == 0 { 2 } else { count + 1 };
                    self.escape = Escape::Csi { params, count, private };
                }
                b'?' | b'>' | b'=' => {
                    private = true;
                    self.escape = Escape::Csi { params, count, private };
                }
                _ => {
                    if !private {
                        self.csi(b, &params[..count.min(params.len())]);
                    }
                }
            },
        }
    }

    fn plain(&mut self, b: u8) {
        match b {
            0x1b => self.escape = Escape::Esc,
            b'\n' => self.newline(),
            b'\r' => self.x = 0,
            0x08 => self.x = self.x.saturating_sub(1),
            b'\t' => {
                self.x = (self.x / 8 + 1) * 8;
                if self.x >= self.cols {
                    self.x = self.cols - 1;
                }
            }
            0x07 => {}
            b if b >= 0x20 => {
                if self.x >= self.cols {
                    self.newline();
                }
                let fg = if self.bold && self.fg < 8 { self.fg + 8 } else { self.fg };
                self.put(self.x, self.y, Cell { ch: b, fg, bg: self.bg });
                self.x += 1;
            }
            _ => {}
        }
    }

    fn csi(&mut self, command: u8, params: &[u16]) {
        let n = |i: usize, default: u16| params.get(i).copied().filter(|&v| v != 0).unwrap_or(default) as usize;
        match command {
            b'A' => self.y = self.y.saturating_sub(n(0, 1)),
            b'B' => self.y = (self.y + n(0, 1)).min(self.rows - 1),
            b'C' => self.x = (self.x + n(0, 1)).min(self.cols - 1),
            b'D' => self.x = self.x.saturating_sub(n(0, 1)),
            b'G' => self.x = (n(0, 1) - 1).min(self.cols - 1),
            b'd' => self.y = (n(0, 1) - 1).min(self.rows - 1),
            b'H' | b'f' => {
                self.y = (n(0, 1) - 1).min(self.rows - 1);
                self.x = (n(1, 1) - 1).min(self.cols - 1);
            }
            b'J' => {
                let here = self.y * self.cols + self.x;
                let range = match params.first().copied().unwrap_or(0) {
                    0 => here..self.cells.len(),
                    1 => 0..here + 1,
                    _ => 0..self.cells.len(),
                };
                self.clear(range);
            }
            b'K' => {
                let start = self.y * self.cols;
                let range = match params.first().copied().unwrap_or(0) {
                    0 => start + self.x..start + self.cols,
                    1 => start..start + self.x + 1,
                    _ => start..start + self.cols,
                };
                self.clear(range);
            }
            b'm' => self.sgr(params),
            b's' => self.saved = (self.x, self.y),
            b'u' => (self.x, self.y) = self.saved,
            _ => {}
        }
    }

    fn sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            return self.sgr(&[0]);
        }
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => (self.fg, self.bg, self.bold) = (DEFAULT_FG, DEFAULT_BG, false),
                1 => self.bold = true,
                22 => self.bold = false,
                7 => core::mem::swap(&mut self.fg, &mut self.bg),
                c @ 30..=37 => self.fg = (c - 30) as u8,
                39 => self.fg = DEFAULT_FG,
                c @ 40..=47 => self.bg = (c - 40) as u8,
                49 => self.bg = DEFAULT_BG,
                c @ 90..=97 => self.fg = (c - 90 + 8) as u8,
                c @ 100..=107 => self.bg = (c - 100 + 8) as u8,
                // 256-color and truecolor: map to the nearest of the 16 colors we have.
                38 | 48 => {
                    let target = if params[i] == 38 { &mut self.fg } else { &mut self.bg };
                    match params.get(i + 1) {
                        Some(5) => {
                            if let Some(&c) = params.get(i + 2) {
                                *target = if c < 16 { c as u8 } else { 7 };
                            }
                            i += 2;
                        }
                        Some(2) => {
                            if let (Some(&r), Some(&g), Some(&b)) = (params.get(i + 2), params.get(i + 3), params.get(i + 4)) {
                                *target = nearest(r as u32, g as u32, b as u32);
                            }
                            i += 4;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn reset(&mut self) {
        (self.fg, self.bg, self.bold) = (DEFAULT_FG, DEFAULT_BG, false);
        (self.x, self.y) = (0, 0);
        let len = self.cells.len();
        self.clear(0..len);
    }

    fn newline(&mut self) {
        self.x = 0;
        if self.y + 1 < self.rows {
            self.y += 1;
        } else {
            self.scroll();
        }
    }

    fn scroll(&mut self) {
        self.cells.copy_within(self.cols.., 0);
        let len = self.cells.len();
        for cell in &mut self.cells[len - self.cols..] {
            *cell = Cell { bg: self.bg, ..BLANK };
        }
        unsafe {
            let row_px = self.pitch * CELL_H;
            core::ptr::copy(self.fb.add(row_px), self.fb, row_px * (self.rows - 1));
        }
        for x in 0..self.cols {
            self.draw(x, self.rows - 1);
        }
    }

    fn clear(&mut self, range: core::ops::Range<usize>) {
        for i in range {
            if i < self.cells.len() {
                self.cells[i] = Cell { bg: self.bg, ..BLANK };
                self.draw(i % self.cols, i / self.cols);
            }
        }
    }

    fn put(&mut self, x: usize, y: usize, cell: Cell) {
        self.cells[y * self.cols + x] = cell;
        self.draw(x, y);
    }

    fn redraw(&mut self) {
        for y in 0..self.rows {
            for x in 0..self.cols {
                self.draw(x, y);
            }
        }
    }

    fn draw(&mut self, x: usize, y: usize) {
        let cell = self.cells[y * self.cols + x];
        self.paint(x, y, cell.ch, PALETTE[cell.fg as usize], PALETTE[cell.bg as usize], false);
    }

    fn paint(&mut self, x: usize, y: usize, ch: u8, fg: u32, bg: u32, cursor: bool) {
        let glyph = BASIC_LEGACY.get(ch as usize).copied().unwrap_or([0; 8]);
        for gy in 0..8 {
            let bits = glyph[gy];
            for sy in 0..SCALE {
                let py = y * CELL_H + gy * SCALE + sy;
                let row = unsafe { self.fb.add(py * self.pitch + x * CELL_W) };
                let underline = cursor && gy == 7;
                for gx in 0..8 {
                    let on = bits >> gx & 1 == 1 || underline;
                    for sx in 0..SCALE {
                        unsafe { row.add(gx * SCALE + sx).write_volatile(if on { fg } else { bg }) };
                    }
                }
            }
        }
    }

    fn show_cursor(&mut self) {
        let (x, y) = (self.x.min(self.cols - 1), self.y);
        let cell = self.cells[y * self.cols + x];
        self.paint(x, y, cell.ch, PALETTE[DEFAULT_FG as usize], PALETTE[cell.bg as usize], true);
    }

    fn hide_cursor(&mut self) {
        self.draw(self.x.min(self.cols - 1), self.y);
    }
}

fn nearest(r: u32, g: u32, b: u32) -> u8 {
    let mut best = (u32::MAX, 7);
    for (i, &c) in PALETTE.iter().enumerate() {
        let (cr, cg, cb) = (c >> 16 & 0xff, c >> 8 & 0xff, c & 0xff);
        let d = r.abs_diff(cr).pow(2) + g.abs_diff(cg).pow(2) + b.abs_diff(cb).pow(2);
        if d < best.0 {
            best = (d, i as u8);
        }
    }
    best.1
}
