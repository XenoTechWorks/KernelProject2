//! A terminal: a grid of character cells and the VT100/xterm escape sequences that change it
//! (cursor movement, clearing, colors, titles). It knows nothing about the screen; the console
//! (console.rs) paints the cells, both for the main console and for each window.
//!
//! The terminal records which cells changed (`dirty`) so the console repaints only those.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: u32,
    pub fg: u8,
    pub bg: u8,
}

pub const DEFAULT_FG: u8 = 7;

enum Escape {
    None,
    Esc,
    Csi { params: [u16; 8], count: usize, private: bool },
    /// Operating system command (ESC ] number ; text BEL), used for the window title.
    Osc(String),
    OscEsc(String),
}

pub struct Term {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Cell>,
    /// Cells changed since the console last painted them.
    pub dirty: Vec<bool>,
    /// Lines scrolled since the last paint, when `count_scrolls` is set (the console then moves
    /// the pixels instead of repainting). Otherwise a scroll just marks every cell dirty.
    pub scrolled: usize,
    pub count_scrolls: bool,
    pub x: usize,
    pub y: usize,
    pub cursor_visible: bool,
    /// The program asked for mouse clicks (xterm mode 1000/1002), reported as SGR sequences.
    pub mouse_reporting: bool,
    pub title: String,
    /// A size the program asked for with "CSI 8 ; rows ; cols t" (columns, rows).
    pub size_request: Option<(usize, usize)>,
    default_bg: u8,
    fg: u8,
    bg: u8,
    bold: bool,
    escape: Escape,
    saved: (usize, usize),
    /// A UTF-8 character in progress: (bytes still expected, bits so far).
    utf8: (u8, u32),
}

impl Term {
    pub fn new(cols: usize, rows: usize, default_bg: u8) -> Term {
        let blank = Cell { ch: ' ' as u32, fg: DEFAULT_FG, bg: default_bg };
        Term {
            cols,
            rows,
            cells: vec![blank; cols * rows],
            dirty: vec![true; cols * rows],
            scrolled: 0,
            count_scrolls: false,
            x: 0,
            y: 0,
            cursor_visible: true,
            mouse_reporting: false,
            title: String::new(),
            size_request: None,
            default_bg,
            fg: DEFAULT_FG,
            bg: default_bg,
            bold: false,
            escape: Escape::None,
            saved: (0, 0),
            utf8: (0, 0),
        }
    }

    pub fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.feed(b);
        }
    }

    /// Where the cursor is drawn (it may sit just past the last column).
    pub fn cursor(&self) -> (usize, usize) {
        (self.x.min(self.cols - 1), self.y)
    }

    /// Changes the size, keeping the top-left of what was there.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let mut cells = vec![self.blank(); cols * rows];
        for y in 0..rows.min(self.rows) {
            for x in 0..cols.min(self.cols) {
                cells[y * cols + x] = self.cells[y * self.cols + x];
            }
        }
        (self.cols, self.rows, self.cells) = (cols, rows, cells);
        self.dirty = vec![true; cols * rows];
        self.x = self.x.min(cols - 1);
        self.y = self.y.min(rows - 1);
    }

    fn blank(&self) -> Cell {
        Cell { ch: ' ' as u32, fg: DEFAULT_FG, bg: self.bg }
    }

    fn feed(&mut self, b: u8) {
        match core::mem::replace(&mut self.escape, Escape::None) {
            Escape::None => self.utf8(b),
            Escape::Esc => match b {
                b'[' => self.escape = Escape::Csi { params: [0; 8], count: 0, private: false },
                b']' => self.escape = Escape::Osc(String::new()),
                b'7' => self.saved = (self.x, self.y),
                b'8' => (self.x, self.y) = self.saved,
                b'c' => self.reset(),
                _ => {}
            },
            Escape::Osc(mut text) => match b {
                0x07 => self.osc(&text),
                0x1b => self.escape = Escape::OscEsc(text),
                _ => {
                    if text.len() < 256 {
                        text.push(b as char);
                    }
                    self.escape = Escape::Osc(text);
                }
            },
            // ESC \ ends an OSC too.
            Escape::OscEsc(text) => self.osc(&text),
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
                    let params = &params[..count.min(params.len())];
                    if private { self.mode(b, params) } else { self.csi(b, params) }
                }
            },
        }
    }

    fn osc(&mut self, text: &str) {
        // 0 and 2 set the title (1 is the icon name, which we don't have).
        if let Some(title) = text.strip_prefix("0;").or_else(|| text.strip_prefix("2;")) {
            self.title = String::from(title);
        }
    }

    /// "CSI ? n h" and "CSI ? n l": turn terminal modes on and off.
    fn mode(&mut self, command: u8, params: &[u16]) {
        let on = match command {
            b'h' => true,
            b'l' => false,
            _ => return,
        };
        for &p in params {
            match p {
                25 => {
                    self.cursor_visible = on;
                    self.touch_cursor();
                }
                1000 | 1002 => self.mouse_reporting = on,
                _ => {}
            }
        }
    }

    fn utf8(&mut self, b: u8) {
        let (left, acc) = self.utf8;
        if left > 0 && b & 0xC0 == 0x80 {
            let acc = acc << 6 | (b & 0x3F) as u32;
            self.utf8 = if left == 1 { (0, 0) } else { (left - 1, acc) };
            if left == 1 {
                self.plain(acc);
            }
            return;
        }
        self.utf8 = (0, 0);
        match b {
            0xC0..=0xDF => self.utf8 = (1, (b & 0x1F) as u32),
            0xE0..=0xEF => self.utf8 = (2, (b & 0x0F) as u32),
            0xF0..=0xF7 => self.utf8 = (3, (b & 0x07) as u32),
            0x80..=0xFF => self.plain(0xFFFD),
            _ => self.plain(b as u32),
        }
    }

    fn plain(&mut self, c: u32) {
        let Ok(b) = u8::try_from(c) else {
            return self.printable(c);
        };
        match b {
            0x1b => self.escape = Escape::Esc,
            b'\n' => self.newline(),
            b'\r' => self.move_to(0, self.y),
            0x08 => self.move_to(self.x.saturating_sub(1), self.y),
            b'\t' => self.move_to(((self.x / 8 + 1) * 8).min(self.cols - 1), self.y),
            0x07 => {}
            b if b >= 0x20 => self.printable(b as u32),
            _ => {}
        }
    }

    fn printable(&mut self, c: u32) {
        if self.x >= self.cols {
            self.newline();
        }
        let fg = if self.bold && self.fg < 8 { self.fg + 8 } else { self.fg };
        self.put(self.x, self.y, Cell { ch: c, fg, bg: self.bg });
        self.x += 1;
        self.touch_cursor();
    }

    fn csi(&mut self, command: u8, params: &[u16]) {
        let n = |i: usize, default: u16| params.get(i).copied().filter(|&v| v != 0).unwrap_or(default) as usize;
        let (cols, rows) = (self.cols, self.rows);
        match command {
            b'A' => self.move_to(self.x, self.y.saturating_sub(n(0, 1))),
            b'B' => self.move_to(self.x, (self.y + n(0, 1)).min(rows - 1)),
            b'C' => self.move_to((self.x + n(0, 1)).min(cols - 1), self.y),
            b'D' => self.move_to(self.x.saturating_sub(n(0, 1)), self.y),
            b'G' => self.move_to((n(0, 1) - 1).min(cols - 1), self.y),
            b'd' => self.move_to(self.x, (n(0, 1) - 1).min(rows - 1)),
            b'H' | b'f' => self.move_to((n(1, 1) - 1).min(cols - 1), (n(0, 1) - 1).min(rows - 1)),
            b'J' => {
                let here = self.y * cols + self.x.min(cols - 1);
                let range = match params.first().copied().unwrap_or(0) {
                    0 => here..self.cells.len(),
                    1 => 0..here + 1,
                    _ => 0..self.cells.len(),
                };
                self.clear(range);
            }
            b'K' => {
                let start = self.y * cols;
                let x = self.x.min(cols - 1);
                let range = match params.first().copied().unwrap_or(0) {
                    0 => start + x..start + cols,
                    1 => start..start + x + 1,
                    _ => start..start + cols,
                };
                self.clear(range);
            }
            b'm' => self.sgr(params),
            b's' => self.saved = (self.x, self.y),
            b'u' => self.move_to(self.saved.0, self.saved.1),
            // xterm window operations: 8 = resize to rows x columns.
            b't' if n(0, 0) == 8 && params.len() >= 3 => self.size_request = Some((n(2, cols as u16), n(1, rows as u16))),
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
                0 => (self.fg, self.bg, self.bold) = (DEFAULT_FG, self.default_bg, false),
                1 => self.bold = true,
                22 => self.bold = false,
                7 => core::mem::swap(&mut self.fg, &mut self.bg),
                c @ 30..=37 => self.fg = (c - 30) as u8,
                39 => self.fg = DEFAULT_FG,
                c @ 40..=47 => self.bg = (c - 40) as u8,
                49 => self.bg = self.default_bg,
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
                                *target = crate::console::nearest(r as u32, g as u32, b as u32);
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
        (self.fg, self.bg, self.bold) = (DEFAULT_FG, self.default_bg, false);
        self.move_to(0, 0);
        let len = self.cells.len();
        self.clear(0..len);
    }

    fn newline(&mut self) {
        if self.y + 1 < self.rows {
            self.move_to(0, self.y + 1);
        } else {
            self.move_to(0, self.y);
            self.scroll();
        }
    }

    fn scroll(&mut self) {
        let cols = self.cols;
        self.cells.copy_within(cols.., 0);
        let len = self.cells.len();
        let blank = self.blank();
        self.cells[len - cols..].fill(blank);
        if self.count_scrolls {
            // The console moves the pixels up a line, so what was dirty moves up with them.
            self.dirty.copy_within(cols.., 0);
            self.dirty[len - cols..].fill(true);
            self.scrolled += 1;
        } else {
            self.dirty.fill(true);
        }
    }

    fn clear(&mut self, range: core::ops::Range<usize>) {
        let blank = self.blank();
        for i in range {
            if i < self.cells.len() && self.cells[i] != blank {
                self.cells[i] = blank;
                self.dirty[i] = true;
            }
        }
    }

    fn put(&mut self, x: usize, y: usize, cell: Cell) {
        let i = y * self.cols + x;
        // Writing what's already there (a full-screen program redrawing) changes nothing.
        if self.cells[i] != cell {
            self.cells[i] = cell;
            self.dirty[i] = true;
        }
    }

    fn move_to(&mut self, x: usize, y: usize) {
        self.touch_cursor();
        (self.x, self.y) = (x, y);
        self.touch_cursor();
    }

    /// The cell under the cursor needs repainting (the cursor moved, or appeared or disappeared).
    fn touch_cursor(&mut self) {
        let (x, y) = self.cursor();
        self.dirty[y * self.cols + x] = true;
    }
}
