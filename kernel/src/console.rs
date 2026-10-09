//! The screen: the text console on the boot framebuffer, and windows on top of it.
//!
//! The console is a terminal (term.rs) filling the screen. A program can also open `/dev/window`
//! to get a window: a small terminal of its own, in a frame with a title and a close button,
//! floating over the console. Windows can be dragged by their title with the mouse; clicking one
//! raises it and sends it the keyboard. Everything is drawn in character cells, so a window is
//! just more text on the same screen, not a separate desktop.
//!
//! Painting composes one cell at a time: the topmost window covering a cell wins, else the
//! console; the mouse pointer is drawn over that.

use crate::term::{Cell, Term};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

const CELL_W: usize = 8;
const CELL_H: usize = 16;

// The 16 terminal colors (a calm palette, close to modern terminal defaults), then our own.
const PALETTE: [u32; 17] = [
    0x1d2129, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
    0x5c6370, 0xff7b86, 0xb5e890, 0xffd68a, 0x82c4ff, 0xde9bf0, 0x7fd8e2, 0xffffff,
    0x262b35, // window background
];
const CONSOLE_BG: u8 = 0;
const WINDOW_BG: u8 = 16;
const DEFAULT_WINDOW: (usize, usize) = (40, 10);

/// The part of a window the rest of the kernel sees: what was typed into it, and whether the user
/// closed it.
pub struct Window {
    pub id: u32,
    pub input: Mutex<VecDeque<u8>>,
    pub closed: AtomicBool,
}

struct View {
    window: Arc<Window>,
    /// Top-left corner of the frame, in cells.
    x: usize,
    y: usize,
    term: Term,
}

impl View {
    fn width(&self) -> usize {
        self.term.cols + 2
    }
    fn height(&self) -> usize {
        self.term.rows + 2
    }
    fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.width() && y >= self.y && y < self.y + self.height()
    }
}

/// Where a cell falls in a window.
enum Part {
    Title,
    Close,
    Border,
    Content(usize, usize),
}

#[derive(Default)]
struct Mouse {
    /// Position in pixels.
    x: usize,
    y: usize,
    buttons: u8,
    /// Hidden until the mouse first moves (so there's no pointer on machines without one).
    shown: bool,
    /// Dragging a window: its id, and where in the frame it was grabbed.
    drag: Option<(u32, usize, usize)>,
}

pub struct Screen {
    fb: *mut u32,
    pitch: usize, // in pixels
    cols: usize,
    rows: usize,
    console: Term,
    /// Bottom to top.
    views: Vec<View>,
    /// The window that gets the keyboard; None is the console.
    focus: Option<u32>,
    mouse: Mouse,
    next_id: u32,
}

unsafe impl Send for Screen {}

pub static CONSOLE: Mutex<Option<Screen>> = Mutex::new(None);

pub fn init(fb: &limine::framebuffer::Framebuffer) {
    if fb.bpp != 32 {
        return;
    }
    let cols = fb.width as usize / CELL_W;
    let rows = fb.height as usize / CELL_H;
    let mut console = Term::new(cols, rows, CONSOLE_BG);
    console.count_scrolls = true;
    let mut screen = Screen {
        fb: fb.address() as *mut u32,
        pitch: fb.pitch as usize / 4,
        cols,
        rows,
        console,
        views: Vec::new(),
        focus: None,
        mouse: Mouse::default(),
        next_id: 1,
    };
    screen.flush_console();
    *CONSOLE.lock() = Some(screen);
}

/// Columns and rows, for TIOCGWINSZ.
pub fn size() -> (usize, usize) {
    CONSOLE.lock().as_ref().map_or((80, 25), |s| (s.cols, s.rows))
}

pub fn write(bytes: &[u8]) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        screen.console.write(bytes);
        screen.flush_console();
    }
}

/// Writes only if nobody holds the console (for the panic handler, which may have interrupted it).
pub fn try_write(bytes: &[u8]) {
    if let Some(mut guard) = CONSOLE.try_lock() {
        if let Some(screen) = guard.as_mut() {
            screen.console.write(bytes);
            screen.flush_console();
        }
    }
}

/// Keys typed on the keyboard: to the focused window, or to the console's tty. (The serial port
/// always goes to the tty: it can't see windows.)
pub fn input(bytes: &[u8]) {
    let target = CONSOLE.lock().as_ref().and_then(|s| s.focused()).map(|v| v.window.clone());
    match target {
        Some(window) => {
            window.input.lock().extend(bytes);
            crate::proc::wake_all();
        }
        None => crate::tty::input(bytes),
    }
}

/// Moves the keyboard to the next window (and after the last one, back to the console).
pub fn cycle_focus() {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        let next = match screen.focus {
            None => screen.views.first().map(|v| v.window.id),
            Some(id) => {
                let i = screen.index_of(id).unwrap_or(0);
                screen.views.get(i + 1).map(|v| v.window.id)
            }
        };
        screen.set_focus(next);
    }
}

/// Opens a new window, on top. It doesn't take the keyboard: after `clock &` you keep typing in the
/// shell. Clicking the window (or F12) gives it the keyboard.
pub fn open_window() -> Option<Arc<Window>> {
    let mut guard = CONSOLE.lock();
    let screen = guard.as_mut()?;
    let window = Arc::new(Window { id: screen.next_id, input: Mutex::new(VecDeque::new()), closed: AtomicBool::new(false) });
    screen.next_id += 1;
    let (cols, rows) = DEFAULT_WINDOW;
    let term = Term::new(cols.min(screen.cols - 2), rows.min(screen.rows - 2), WINDOW_BG);
    // Each new window a little below and right of the last, so they don't hide each other.
    let n = screen.views.len();
    let x = (screen.cols.saturating_sub(cols + 2) / 2 + 3 * n) % screen.cols.saturating_sub(cols + 1).max(1);
    let y = (2 + 2 * n) % screen.rows.saturating_sub(rows + 1).max(1);
    screen.views.push(View { window: window.clone(), x, y, term });
    screen.repaint_view(screen.views.len() - 1);
    Some(window)
}

/// Removes a window from the screen (its program closed it or exited).
pub fn close_window(id: u32) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        screen.remove(id);
    }
}

/// Output from a window's program.
pub fn window_write(id: u32, bytes: &[u8]) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        screen.window_write(id, bytes);
    }
}

/// A window's size in columns and rows.
pub fn window_size(id: u32) -> (usize, usize) {
    let guard = CONSOLE.lock();
    let view = guard.as_ref().and_then(|s| s.index_of(id).map(|i| &s.views[i]));
    view.map_or(DEFAULT_WINDOW, |v| (v.term.cols, v.term.rows))
}

/// The mouse moved by (dx, dy) pixels (down is positive) with these buttons held (bit 0 left,
/// bit 1 right, bit 2 middle).
pub fn mouse(dx: i32, dy: i32, buttons: u8) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        screen.mouse_event(dx, dy, buttons);
    }
}

impl Screen {
    fn index_of(&self, id: u32) -> Option<usize> {
        self.views.iter().position(|v| v.window.id == id)
    }

    fn focused(&self) -> Option<&View> {
        self.focus.and_then(|id| self.index_of(id)).map(|i| &self.views[i])
    }

    /// The topmost window covering a cell.
    fn top_at(&self, x: usize, y: usize) -> Option<usize> {
        self.views.iter().rposition(|v| v.contains(x, y))
    }

    fn part(view: &View, x: usize, y: usize) -> Part {
        let (fx, fy) = (x - view.x, y - view.y);
        if fy == 0 {
            if fx == view.width() - 3 { Part::Close } else { Part::Title }
        } else if fx == 0 || fy == view.height() - 1 || fx == view.width() - 1 {
            Part::Border
        } else {
            Part::Content(fx - 1, fy - 1)
        }
    }

    fn set_focus(&mut self, focus: Option<u32>) {
        let old = core::mem::replace(&mut self.focus, focus);
        for id in [old, focus].into_iter().flatten() {
            if let Some(i) = self.index_of(id) {
                self.repaint_view(i);
            }
        }
        // The console's cursor shows only while it has the keyboard.
        let (x, y) = self.console.cursor();
        self.repaint_rect(x, y, 1, 1);
    }

    fn raise(&mut self, i: usize) -> usize {
        let view = self.views.remove(i);
        self.views.push(view);
        let top = self.views.len() - 1;
        self.repaint_view(top);
        top
    }

    fn remove(&mut self, id: u32) {
        let Some(i) = self.index_of(id) else { return };
        let view = self.views.remove(i);
        if self.mouse.drag.is_some_and(|(d, _, _)| d == id) {
            self.mouse.drag = None;
        }
        self.repaint_rect(view.x, view.y, view.width(), view.height());
        if self.focus == Some(id) {
            self.set_focus(self.views.last().map(|v| v.window.id));
        }
    }

    fn window_write(&mut self, id: u32, bytes: &[u8]) {
        let Some(i) = self.index_of(id) else { return };
        let view = &mut self.views[i];
        let title = view.term.title.clone();
        view.term.write(bytes);
        if let Some((cols, rows)) = view.term.size_request.take() {
            let (cols, rows) = (cols.clamp(8, self.cols - 2), rows.clamp(1, self.rows - 2));
            if (cols, rows) != (view.term.cols, view.term.rows) {
                let (x, y, w, h) = (view.x, view.y, view.width(), view.height());
                view.term.resize(cols, rows);
                view.x = view.x.min(self.cols - view.width());
                view.y = view.y.min(self.rows - view.height());
                self.repaint_rect(x, y, w, h);
                self.repaint_view(i);
                return;
            }
        }
        if view.term.title != title {
            let (x, y, w) = (view.x, view.y, view.width());
            self.repaint_rect(x, y, w, 1);
        }
        self.flush_view(i);
    }

    // ---- the mouse ----

    fn mouse_event(&mut self, dx: i32, dy: i32, buttons: u8) {
        let (old_x, old_y) = (self.mouse.x, self.mouse.y);
        let max_x = (self.cols * CELL_W) as i32 - 1;
        let max_y = (self.rows * CELL_H) as i32 - 1;
        self.mouse.x = (old_x as i32 + dx).clamp(0, max_x) as usize;
        self.mouse.y = (old_y as i32 + dy).clamp(0, max_y) as usize;
        let was_shown = core::mem::replace(&mut self.mouse.shown, true);
        if was_shown {
            self.repaint_pointer(old_x, old_y);
        }
        let (cx, cy) = (self.mouse.x / CELL_W, self.mouse.y / CELL_H);

        let pressed = buttons & !self.mouse.buttons;
        let released = self.mouse.buttons & !buttons;
        self.mouse.buttons = buttons;

        if let Some((id, gx, gy)) = self.mouse.drag {
            if let Some(i) = self.index_of(id) {
                self.move_view(i, cx.saturating_sub(gx), cy.saturating_sub(gy));
            }
            if released & 1 != 0 {
                self.mouse.drag = None;
            }
        } else if pressed != 0 {
            self.click(cx, cy, pressed);
        } else if released != 0 || buttons != 0 {
            // Releases and drags inside a window go to its program if it asked for the mouse.
            if let Some(i) = self.top_at(cx, cy).filter(|&i| Some(self.views[i].window.id) == self.focus) {
                if let Part::Content(x, y) = Self::part(&self.views[i], cx, cy) {
                    let button = if released != 0 { released } else { buttons };
                    self.report(i, button, x, y, released != 0, released == 0);
                }
            }
        }
        self.repaint_pointer(self.mouse.x, self.mouse.y);
    }

    fn click(&mut self, cx: usize, cy: usize, pressed: u8) {
        let Some(i) = self.top_at(cx, cy) else {
            return self.set_focus(None);
        };
        let i = if i + 1 == self.views.len() { i } else { self.raise(i) };
        let id = self.views[i].window.id;
        self.set_focus(Some(id));
        match Self::part(&self.views[i], cx, cy) {
            Part::Close if pressed & 1 != 0 => {
                // Closing tells the program (its reads end); the window goes away now.
                self.views[i].window.closed.store(true, Ordering::Relaxed);
                crate::proc::wake_all();
                self.remove(id);
            }
            Part::Title | Part::Border if pressed & 1 != 0 => {
                let view = &self.views[i];
                self.mouse.drag = Some((id, cx - view.x, cy - view.y));
            }
            Part::Content(x, y) => self.report(i, pressed, x, y, false, false),
            _ => {}
        }
    }

    /// Sends a mouse event to a window's program as an xterm SGR report: ESC [ < b ; x ; y M/m.
    fn report(&mut self, i: usize, buttons: u8, x: usize, y: usize, release: bool, motion: bool) {
        let view = &self.views[i];
        if !view.term.mouse_reporting {
            return;
        }
        let button = if buttons & 1 != 0 { 0 } else if buttons & 4 != 0 { 1 } else { 2 };
        let code = button + if motion { 32 } else { 0 };
        let text = alloc::format!("\x1b[<{};{};{}{}", code, x + 1, y + 1, if release { 'm' } else { 'M' });
        view.window.input.lock().extend(text.as_bytes());
        crate::proc::wake_all();
    }

    fn move_view(&mut self, i: usize, x: usize, y: usize) {
        let view = &mut self.views[i];
        let (x, y) = (x.min(self.cols - view.width()), y.min(self.rows - view.height()));
        if (x, y) == (view.x, view.y) {
            return;
        }
        let (old_x, old_y, w, h) = (view.x, view.y, view.width(), view.height());
        (view.x, view.y) = (x, y);
        self.repaint_rect(old_x, old_y, w, h);
        self.repaint_view(i);
    }

    // ---- painting ----

    /// Paints the console's changed cells. When it scrolled, the pixels are moved up instead of
    /// repainting everything, then whatever floated over the console is repainted.
    fn flush_console(&mut self) {
        let scrolled = core::mem::take(&mut self.console.scrolled);
        if scrolled >= self.rows {
            self.console.dirty.fill(true);
        } else if scrolled > 0 {
            let lines = scrolled * CELL_H;
            unsafe {
                core::ptr::copy(self.fb.add(lines * self.pitch), self.fb, (self.rows * CELL_H - lines) * self.pitch);
            }
        }
        for i in 0..self.console.dirty.len() {
            if core::mem::take(&mut self.console.dirty[i]) {
                let (x, y) = (i % self.cols, i / self.cols);
                if self.top_at(x, y).is_none() {
                    self.paint_cell(x, y);
                }
            }
        }
        if scrolled > 0 && scrolled < self.rows {
            // Windows and the pointer moved up with the pixels: put them back, and repaint what
            // they left behind.
            for i in 0..self.views.len() {
                let v = &self.views[i];
                let top = v.y.saturating_sub(scrolled);
                self.repaint_rect(v.x, top, v.width(), v.y + v.height() - top);
            }
            if self.mouse.shown {
                let (cx, cy) = (self.mouse.x / CELL_W, self.mouse.y / CELL_H);
                let top = cy.saturating_sub(scrolled);
                self.repaint_rect(cx, top, 3, cy + 2 - top);
            }
        }
    }

    /// Paints a window's changed cells.
    fn flush_view(&mut self, i: usize) {
        for c in 0..self.views[i].term.dirty.len() {
            let view = &mut self.views[i];
            if core::mem::take(&mut view.term.dirty[c]) {
                let (x, y) = (view.x + 1 + c % view.term.cols, view.y + 1 + c / view.term.cols);
                if self.top_at(x, y) == Some(i) {
                    self.paint_cell(x, y);
                }
            }
        }
    }

    fn repaint_view(&mut self, i: usize) {
        let v = &mut self.views[i];
        v.term.dirty.fill(false);
        let (x, y, w, h) = (v.x, v.y, v.width(), v.height());
        self.repaint_rect(x, y, w, h);
    }

    fn repaint_rect(&mut self, x: usize, y: usize, w: usize, h: usize) {
        for cy in y..(y + h).min(self.rows) {
            for cx in x..(x + w).min(self.cols) {
                self.paint_cell(cx, cy);
            }
        }
    }

    /// Repaints the cells under the pointer drawn at (px, py).
    fn repaint_pointer(&mut self, px: usize, py: usize) {
        let (x0, y0) = (px / CELL_W, py / CELL_H);
        let (x1, y1) = ((px + POINTER_W - 1) / CELL_W, (py + POINTER.len() - 1) / CELL_H);
        self.repaint_rect(x0, y0, x1 - x0 + 1, y1 - y0 + 1);
    }

    /// What a screen cell shows: (character, foreground, background, cursor here).
    fn compose(&self, x: usize, y: usize) -> (u32, u8, u8, bool) {
        let Some(i) = self.top_at(x, y) else {
            let cell = self.console.cells[y * self.cols + x];
            let cursor = self.focus.is_none() && self.console.cursor_visible && self.console.cursor() == (x, y);
            return (cell.ch, cell.fg, cell.bg, cursor);
        };
        let view = &self.views[i];
        let focused = self.focus == Some(view.window.id);
        let frame = if focused { 12 } else { 8 };
        let (fx, fy, w, h) = (x - view.x, y - view.y, view.width(), view.height());
        let line = |ch: char| (ch as u32, frame, WINDOW_BG, false);
        match Self::part(view, x, y) {
            Part::Content(cx, cy) => {
                let Cell { ch, fg, bg } = view.term.cells[cy * view.term.cols + cx];
                let cursor = focused && view.term.cursor_visible && view.term.cursor() == (cx, cy);
                (ch, fg, bg, cursor)
            }
            Part::Close => ('×' as u32, if focused { 9 } else { 8 }, WINDOW_BG, false),
            Part::Title => {
                // ╭─ title ───── × ╮
                let title = view.term.title.as_bytes();
                let room = w.saturating_sub(8);
                match fx {
                    0 => line('╭'),
                    _ if fx == w - 1 => line('╮'),
                    2 if !title.is_empty() => line(' '),
                    _ if fx == w - 4 || fx == w - 2 => line(' '),
                    _ if fx >= 3 && fx - 3 < title.len().min(room) => {
                        (title[fx - 3] as u32, if focused { 15 } else { 7 }, WINDOW_BG, false)
                    }
                    _ if fx == 3 + title.len().min(room) && !title.is_empty() => line(' '),
                    _ => line('─'),
                }
            }
            Part::Border => match (fx, fy) {
                (0, _) if fy == h - 1 => line('╰'),
                _ if fy == h - 1 && fx == w - 1 => line('╯'),
                _ if fy == h - 1 => line('─'),
                _ => line('│'),
            },
        }
    }

    fn paint_cell(&mut self, x: usize, y: usize) {
        let (ch, fg, bg, cursor) = self.compose(x, y);
        let (fg, bg) = (PALETTE[fg as usize], PALETTE[bg as usize]);
        let glyph = crate::font::glyph(ch).or_else(|| crate::font::glyph('?' as u32)).unwrap();
        for (gy, &bits) in glyph.iter().enumerate() {
            let row = unsafe { self.fb.add((y * CELL_H + gy) * self.pitch + x * CELL_W) };
            let underline = cursor && gy >= CELL_H - 2;
            for gx in 0..CELL_W {
                let on = bits >> gx & 1 == 1 || underline;
                unsafe { row.add(gx).write_volatile(if on { fg } else { bg }) };
            }
        }
        if self.mouse.shown {
            self.paint_pointer_over(x, y);
        }
    }

    /// Draws the part of the mouse pointer that falls in this cell.
    fn paint_pointer_over(&mut self, x: usize, y: usize) {
        let (cell_x, cell_y) = (x * CELL_W, y * CELL_H);
        for (py, row) in POINTER.iter().enumerate() {
            let sy = self.mouse.y + py;
            if sy < cell_y || sy >= cell_y + CELL_H {
                continue;
            }
            for (px, &p) in row.iter().enumerate() {
                let sx = self.mouse.x + px;
                if sx < cell_x || sx >= cell_x + CELL_W || p == b'.' {
                    continue;
                }
                let color = if p == b'#' { 0x101010 } else { 0xffffff };
                unsafe { self.fb.add(sy * self.pitch + sx).write_volatile(color) };
            }
        }
    }
}

// The mouse pointer: '#' outline, 'o' fill, '.' see-through. The tip is its top-left corner.
const POINTER_W: usize = 10;
const POINTER: [&[u8; POINTER_W]; 16] = [
    b"#.........", b"##........", b"#o#.......", b"#oo#......", b"#ooo#.....", b"#oooo#....",
    b"#ooooo#...", b"#oooooo#..", b"#ooooooo#.", b"#oooooooo#", b"#ooooo####", b"#oo#oo#...",
    b"#o#.#oo#..", b"##..#oo#..", b"#....#oo#.", b".....###..",
];

/// The nearest of the 16 terminal colors to an RGB color.
pub fn nearest(r: u32, g: u32, b: u32) -> u8 {
    let mut best = (u32::MAX, 7);
    for (i, &c) in PALETTE[..16].iter().enumerate() {
        let (cr, cg, cb) = (c >> 16 & 0xff, c >> 8 & 0xff, c & 0xff);
        let d = r.abs_diff(cr).pow(2) + g.abs_diff(cg).pow(2) + b.abs_diff(cb).pow(2);
        if d < best.0 {
            best = (d, i as u8);
        }
    }
    best.1
}
