//! The screen: the text console on the boot framebuffer, and windows on top of it.
//!
//! The console is a terminal (term.rs) filling the screen. A program can also open `/dev/window`
//! to get a window floating over the console, in a frame with a title, a close button and a soft
//! shadow. A window shows either a small terminal of its own (what the program writes), or pixels
//! the program draws itself (after the WINDOW_PIXELS ioctl), which is what graphical apps use.
//! Windows are dragged by their header with the mouse; clicking one raises it and gives it the
//! keyboard. It's not a desktop: the command line stays underneath.
//!
//! This is a small compositor. The console is drawn into its own layer of pixels, and each window
//! into its own image (frame, shadow and contents, with alpha at the edges). Showing part of the
//! screen blends them, bottom to top, into a back buffer, then copies that to the framebuffer.

use crate::term::{Cell, Term};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
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

// Window frames, in pixels.
const HEADER: i32 = 34;
const RADIUS: i32 = 10;
const SHADOW: i32 = 14;
const SHADOW_DROP: i32 = 3;
const SHADOW_ALPHA: i32 = 100;
const PAD: i32 = 6; // around a text window's cells
const CLOSE_RADIUS: i32 = 9;

const HEADER_COLOR: [u32; 2] = [0x23272f, 0x2c313c]; // [unfocused, focused]
const BORDER_COLOR: [u32; 2] = [0x353b47, 0x4b5365];
const TITLE_COLOR: [u32; 2] = [0x8b93a1, 0xe6e9ef];
const CLOSE_COLOR: u32 = 0x3a404d;
const CLOSE_HOVER: u32 = 0xe06c75;

/// Events a graphical window's program reads, 16 bytes each: kind, x, y, data (all 32-bit).
pub mod event {
    pub const MOTION: u32 = 1; // the pointer moved over the window (x, y in its pixels)
    pub const PRESS: u32 = 2; // a mouse button went down (data: 1 left, 2 right, 3 middle)
    pub const RELEASE: u32 = 3;
    pub const KEY: u32 = 4; // data: the key (see keyboard.rs), x: modifiers (1 shift, 2 ctrl)
    pub const LEAVE: u32 = 5; // the pointer left the window
    pub const FOCUS: u32 = 6; // data: 1 got the keyboard, 0 lost it
}

/// The part of a window the rest of the kernel sees: what was typed into it, and whether the user
/// closed it.
pub struct Window {
    pub id: u32,
    pub input: Mutex<VecDeque<u8>>,
    pub closed: AtomicBool,
    /// The program draws pixels (and reads events) rather than writing text.
    pub pixels: AtomicBool,
}

impl Window {
    fn event(&self, kind: u32, x: i32, y: i32, data: u32) {
        let mut q = self.input.lock();
        for word in [kind, x as u32, y as u32, data] {
            q.extend(word.to_le_bytes());
        }
        crate::proc::wake_all();
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn right(self) -> i32 {
        self.x + self.w
    }
    fn bottom(self) -> i32 {
        self.y + self.h
    }
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    fn intersect(self, o: Rect) -> Option<Rect> {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = (self.right().min(o.right()), self.bottom().min(o.bottom()));
        (x1 > x0 && y1 > y0).then_some(Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
    }
    fn union(self, o: Rect) -> Rect {
        let (x0, y0) = (self.x.min(o.x), self.y.min(o.y));
        Rect { x: x0, y: y0, w: self.right().max(o.right()) - x0, h: self.bottom().max(o.bottom()) - y0 }
    }
}

struct View {
    window: Arc<Window>,
    /// Top-left corner of the frame, on the screen.
    x: i32,
    y: i32,
    /// Size of the contents, in pixels.
    cw: i32,
    ch: i32,
    /// A text window's terminal; None when the program draws pixels.
    text: Option<Term>,
    title: String,
    /// The contents, as opaque pixels (cw x ch).
    content: Vec<u32>,
    /// Frame, shadow and contents: premultiplied ARGB, iw() x ih().
    image: Vec<u32>,
    /// Hidden until the program first draws something (so it doesn't flash at the default size).
    visible: bool,
    /// The user dragged it (until then it stays centered on where it opened when it changes size).
    moved: bool,
    close_hover: bool,
}

impl View {
    fn fw(&self) -> i32 {
        self.cw + 2
    }
    fn fh(&self) -> i32 {
        self.ch + HEADER + 1
    }
    fn iw(&self) -> i32 {
        self.fw() + 2 * SHADOW
    }
    fn ih(&self) -> i32 {
        self.fh() + 2 * SHADOW
    }
    fn frame(&self) -> Rect {
        Rect { x: self.x, y: self.y, w: self.fw(), h: self.fh() }
    }
    fn bounds(&self) -> Rect {
        Rect { x: self.x - SHADOW, y: self.y - SHADOW, w: self.iw(), h: self.ih() }
    }
    fn content_rect(&self) -> Rect {
        Rect { x: self.x + 1, y: self.y + HEADER, w: self.cw, h: self.ch }
    }
    /// Center of the close button, in frame coordinates.
    fn close_center(&self) -> (i32, i32) {
        (self.fw() - 20, HEADER / 2)
    }
}

/// Where a point falls in a window.
enum Part {
    Close,
    Header,
    Border,
    /// In the contents, at these pixel coordinates.
    Content(i32, i32),
}

#[derive(Default)]
struct Mouse {
    x: i32,
    y: i32,
    buttons: u8,
    /// Hidden until the mouse first moves (so there's no pointer on machines without one).
    shown: bool,
    /// Dragging a window: its id, and where in the frame it was grabbed.
    drag: Option<(u32, i32, i32)>,
    /// The window a button was pressed in: it gets the motion and the release, even outside.
    grab: Option<u32>,
    /// The graphical window the pointer is over (to tell it when the pointer leaves).
    hover: Option<u32>,
}

pub struct Screen {
    fb: *mut u32,
    pitch: usize, // in pixels
    width: usize,
    height: usize,
    cols: usize,
    rows: usize,
    console: Term,
    /// The console, drawn (width x height).
    layer: Vec<u32>,
    /// Where a piece of the screen is put together before it goes to the framebuffer.
    back: Vec<u32>,
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
    let (width, height) = (fb.width as usize, fb.height as usize);
    let cols = width / CELL_W;
    let rows = height / CELL_H;
    let mut console = Term::new(cols, rows, CONSOLE_BG);
    console.count_scrolls = true;
    let mut screen = Screen {
        fb: fb.address() as *mut u32,
        pitch: fb.pitch as usize / 4,
        width,
        height,
        cols,
        rows,
        console,
        layer: vec![PALETTE[CONSOLE_BG as usize]; width * height],
        back: vec![0; width * height],
        views: Vec::new(),
        focus: None,
        mouse: Mouse::default(),
        next_id: 1,
    };
    screen.flush_console();
    screen.show(screen.screen_rect());
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

/// A key pressed on the keyboard: `bytes` is what a terminal gets, `key` and `mods` what a
/// graphical window gets. Goes to the focused window, or to the console's tty. (The serial port
/// always goes to the tty: it can't see windows.)
pub fn key(bytes: &[u8], key: u32, mods: u32) {
    let target = CONSOLE.lock().as_ref().and_then(|s| s.focused()).map(|v| v.window.clone());
    match target {
        Some(window) if window.pixels.load(Ordering::Relaxed) => window.event(event::KEY, mods as i32, 0, key),
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
        let visible: Vec<u32> = screen.views.iter().filter(|v| v.visible).map(|v| v.window.id).collect();
        let next = match screen.focus.and_then(|id| visible.iter().position(|&v| v == id)) {
            None => visible.first().copied(),
            Some(i) => visible.get(i + 1).copied(),
        };
        screen.set_focus(next);
    }
}

/// Opens a new window, on top. It doesn't take the keyboard: after `clock &` you keep typing in the
/// shell. Clicking the window (or F12) gives it the keyboard.
pub fn open_window() -> Option<Arc<Window>> {
    let mut guard = CONSOLE.lock();
    let screen = guard.as_mut()?;
    let window = Arc::new(Window {
        id: screen.next_id,
        input: Mutex::new(VecDeque::new()),
        closed: AtomicBool::new(false),
        pixels: AtomicBool::new(false),
    });
    screen.next_id += 1;
    let (cols, rows) = DEFAULT_WINDOW;
    let term = Term::new(cols.min(screen.cols - 4), rows.min(screen.rows - 6), WINDOW_BG);
    let n = screen.views.len() as i32;
    let mut view = View {
        window: window.clone(),
        x: 0,
        y: 0,
        cw: 0,
        ch: 0,
        text: Some(term),
        title: String::new(),
        content: Vec::new(),
        image: Vec::new(),
        visible: false,
        moved: false,
        close_hover: false,
    };
    let (cw, ch) = text_size(view.text.as_ref().unwrap());
    // Each new window a little below and right of the last, so they don't hide each other.
    view.x = (screen.width as i32 - (cw + 2)) / 2 + 32 * (n % 8);
    view.y = 64 + 32 * (n % 8);
    screen.views.push(view);
    let i = screen.views.len() - 1;
    screen.resize_view(i, cw, ch);
    Some(window)
}

/// Removes a window from the screen (its program closed it or exited).
pub fn close_window(id: u32) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        screen.remove(id);
    }
}

/// Output from a text window's program.
pub fn window_write(id: u32, bytes: &[u8]) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        screen.window_write(id, bytes);
    }
}

/// A window's size: (columns, rows, width, height in pixels).
pub fn window_size(id: u32) -> (usize, usize, usize, usize) {
    let guard = CONSOLE.lock();
    let view = guard.as_ref().and_then(|s| s.index_of(id).map(|i| &s.views[i]));
    view.map_or((DEFAULT_WINDOW.0, DEFAULT_WINDOW.1, 0, 0), |v| {
        let (cols, rows) = v.text.as_ref().map_or((0, 0), |t| (t.cols, t.rows));
        (cols, rows, v.cw as usize, v.ch as usize)
    })
}

/// Makes a window graphical, with contents of this many pixels. Returns the size it got (no larger
/// than the screen).
pub fn window_set_pixels(id: u32, w: u32, h: u32) -> Option<(u32, u32)> {
    let mut guard = CONSOLE.lock();
    let screen = guard.as_mut()?;
    let i = screen.index_of(id)?;
    let w = (w as i32).clamp(16, screen.width as i32 - 2);
    let h = (h as i32).clamp(16, screen.height as i32 - HEADER - 1);
    let view = &mut screen.views[i];
    view.text = None;
    view.window.pixels.store(true, Ordering::Relaxed);
    screen.resize_view(i, w, h);
    Some((w as u32, h as u32))
}

pub fn window_set_title(id: u32, title: &str) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        if let Some(i) = screen.index_of(id) {
            screen.views[i].title = String::from(title);
            screen.refresh_frame(i);
        }
    }
}

/// New pixels for part of a graphical window: w x h of them, row by row.
pub fn window_blit(id: u32, x: i32, y: i32, w: i32, h: i32, pixels: &[u32]) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        if let Some(i) = screen.index_of(id) {
            let view = &mut screen.views[i];
            let Some(r) = (Rect { x, y, w, h }).intersect(Rect { x: 0, y: 0, w: view.cw, h: view.ch }) else { return };
            for row in r.y..r.bottom() {
                let from = ((row - y) * w + (r.x - x)) as usize;
                let to = (row * view.cw + r.x) as usize;
                view.content[to..to + r.w as usize].copy_from_slice(&pixels[from..from + r.w as usize]);
            }
            screen.content_changed(i, r);
        }
    }
}

/// The mouse moved by (dx, dy) pixels (down is positive) with these buttons held (bit 0 left,
/// bit 1 right, bit 2 middle).
pub fn mouse(dx: i32, dy: i32, buttons: u8) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        screen.mouse_event(dx, dy, buttons);
    }
}

/// The contents size of a text window: its cells plus padding.
fn text_size(term: &Term) -> (i32, i32) {
    ((term.cols * CELL_W) as i32 + 2 * PAD, (term.rows * CELL_H) as i32 + 2 * PAD)
}

impl Screen {
    fn screen_rect(&self) -> Rect {
        Rect { x: 0, y: 0, w: self.width as i32, h: self.height as i32 }
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.views.iter().position(|v| v.window.id == id)
    }

    fn focused(&self) -> Option<&View> {
        self.focus.and_then(|id| self.index_of(id)).map(|i| &self.views[i])
    }

    /// The topmost visible window whose frame covers a point.
    fn top_at(&self, x: i32, y: i32) -> Option<usize> {
        self.views.iter().rposition(|v| v.visible && v.frame().contains(x, y))
    }

    fn part(view: &View, x: i32, y: i32) -> Part {
        let (fx, fy) = (x - view.x, y - view.y);
        let (cx, cy) = view.close_center();
        if (fx - cx).pow(2) + (fy - cy).pow(2) <= (CLOSE_RADIUS + 2).pow(2) {
            Part::Close
        } else if fy < HEADER {
            Part::Header
        } else if view.content_rect().contains(x, y) {
            Part::Content(fx - 1, fy - HEADER)
        } else {
            Part::Border
        }
    }

    fn set_focus(&mut self, focus: Option<u32>) {
        let old = core::mem::replace(&mut self.focus, focus);
        if old == focus {
            return;
        }
        for (id, now) in [(old, false), (focus, true)] {
            let Some(i) = id.and_then(|id| self.index_of(id)) else { continue };
            if self.views[i].window.pixels.load(Ordering::Relaxed) {
                self.views[i].window.event(event::FOCUS, 0, 0, now as u32);
            }
            // The frame's colors change, and a text window's cursor shows only while focused.
            self.render_text(i, true);
            self.refresh_frame(i);
        }
        // The console's cursor shows only while it has the keyboard.
        let (x, y) = self.console.cursor();
        self.console.dirty[y * self.cols + x] = true;
        self.flush_console();
    }

    fn raise(&mut self, i: usize) -> usize {
        let view = self.views.remove(i);
        let bounds = view.bounds();
        self.views.push(view);
        self.show(bounds);
        self.views.len() - 1
    }

    fn remove(&mut self, id: u32) {
        let Some(i) = self.index_of(id) else { return };
        let view = self.views.remove(i);
        for slot in [&mut self.mouse.grab, &mut self.mouse.hover] {
            if *slot == Some(id) {
                *slot = None;
            }
        }
        if self.mouse.drag.is_some_and(|(d, _, _)| d == id) {
            self.mouse.drag = None;
        }
        self.show(view.bounds());
        if self.focus == Some(id) {
            self.focus = None;
            let next = self.views.iter().rev().find(|v| v.visible).map(|v| v.window.id);
            self.set_focus(next);
            if next.is_none() {
                let (x, y) = self.console.cursor();
                self.console.dirty[y * self.cols + x] = true;
                self.flush_console();
            }
        }
    }

    /// Gives a window new contents size (keeping its center until the user has moved it).
    fn resize_view(&mut self, i: usize, cw: i32, ch: i32) {
        let screen_w = self.width as i32;
        let view = &mut self.views[i];
        let old = view.bounds();
        let old_fw = view.fw();
        (view.cw, view.ch) = (cw, ch);
        if !view.moved && !view.content.is_empty() {
            view.x += (old_fw - view.fw()) / 2;
        }
        view.x = view.x.clamp(-(view.fw() - 60), screen_w - 60);
        let fill = if view.text.is_some() { PALETTE[WINDOW_BG as usize] } else { 0x262b35 };
        view.content = vec![fill; (cw * ch) as usize];
        view.image = vec![0; (view.iw() * view.ih()) as usize];
        self.render_text(i, true);
        self.render_frame(i);
        if self.views[i].visible {
            let new = self.views[i].bounds();
            self.show(old.union(new));
        }
    }

    fn window_write(&mut self, id: u32, bytes: &[u8]) {
        let Some(i) = self.index_of(id) else { return };
        let view = &mut self.views[i];
        let Some(term) = view.text.as_mut() else { return };
        term.write(bytes);
        if let Some((cols, rows)) = term.size_request.take() {
            let cols = cols.clamp(8, self.cols - 4);
            let rows = rows.clamp(1, self.rows - 6);
            if (cols, rows) != (term.cols, term.rows) {
                term.resize(cols, rows);
                let (cw, ch) = text_size(term);
                self.resize_view(i, cw, ch);
            }
        }
        let view = &mut self.views[i];
        let term = view.text.as_ref().unwrap();
        if term.title != view.title {
            view.title = term.title.clone();
            self.refresh_frame(i);
        }
        let changed = self.render_text(i, false);
        if changed.is_some() || !self.views[i].visible {
            let all = Rect { x: 0, y: 0, w: self.views[i].cw, h: self.views[i].ch };
            self.content_changed(i, changed.unwrap_or(all));
        }
    }

    // ---- the mouse ----

    fn mouse_event(&mut self, dx: i32, dy: i32, buttons: u8) {
        let old_pointer = self.pointer_rect();
        self.mouse.x = (self.mouse.x + dx).clamp(0, self.width as i32 - 1);
        self.mouse.y = (self.mouse.y + dy).clamp(0, self.height as i32 - 1);
        let was_shown = core::mem::replace(&mut self.mouse.shown, true);
        let (mx, my) = (self.mouse.x, self.mouse.y);
        let pressed = buttons & !self.mouse.buttons;
        let released = self.mouse.buttons & !buttons;
        self.mouse.buttons = buttons;

        if let Some((id, gx, gy)) = self.mouse.drag {
            if let Some(i) = self.index_of(id) {
                self.move_view(i, mx - gx, my - gy);
            }
            if released & 1 != 0 {
                self.mouse.drag = None;
            }
        } else {
            self.hover(mx, my, dx != 0 || dy != 0);
            if pressed != 0 {
                self.click(mx, my, pressed);
            }
            if released != 0 {
                self.release(mx, my, released);
            }
        }
        let pointer = self.pointer_rect();
        self.show(if was_shown { old_pointer.union(pointer) } else { pointer });
    }

    /// The pointer moved: tell graphical windows, and light up close buttons.
    fn hover(&mut self, mx: i32, my: i32, moved: bool) {
        let top = self.top_at(mx, my);
        for i in 0..self.views.len() {
            let over_close = top == Some(i) && matches!(Self::part(&self.views[i], mx, my), Part::Close);
            if self.views[i].close_hover != over_close {
                self.views[i].close_hover = over_close;
                self.refresh_frame(i);
            }
        }
        // While a button is held, the window it was pressed in gets the motion, wherever it goes.
        let target = match self.mouse.grab {
            Some(id) => self.index_of(id),
            None => top.filter(|&i| self.views[i].content_rect().contains(mx, my)),
        };
        let target = target.filter(|&i| self.views[i].window.pixels.load(Ordering::Relaxed));
        let target_id = target.map(|i| self.views[i].window.id);
        if self.mouse.hover != target_id {
            if let Some(i) = self.mouse.hover.and_then(|id| self.index_of(id)) {
                self.views[i].window.event(event::LEAVE, 0, 0, 0);
            }
            self.mouse.hover = target_id;
        }
        if let (Some(i), true) = (target, moved) {
            let c = self.views[i].content_rect();
            self.views[i].window.event(event::MOTION, mx - c.x, my - c.y, 0);
        }
    }

    fn click(&mut self, mx: i32, my: i32, pressed: u8) {
        let Some(i) = self.top_at(mx, my) else {
            return self.set_focus(None);
        };
        let i = if i + 1 == self.views.len() { i } else { self.raise(i) };
        let id = self.views[i].window.id;
        self.set_focus(Some(id));
        match Self::part(&self.views[i], mx, my) {
            Part::Close if pressed & 1 != 0 => {
                // Closing tells the program (its reads end); the window goes away now.
                self.views[i].window.closed.store(true, Ordering::Relaxed);
                crate::proc::wake_all();
                self.remove(id);
            }
            Part::Header | Part::Border if pressed & 1 != 0 => {
                let view = &mut self.views[i];
                view.moved = true;
                self.mouse.drag = Some((id, mx - view.x, my - view.y));
            }
            Part::Content(x, y) => {
                self.mouse.grab = Some(id);
                self.button_event(i, pressed, x, y, false);
            }
            _ => {}
        }
    }

    fn release(&mut self, mx: i32, my: i32, released: u8) {
        if let Some(i) = self.mouse.grab.and_then(|id| self.index_of(id)) {
            let c = self.views[i].content_rect();
            self.button_event(i, released, mx - c.x, my - c.y, true);
        }
        if self.mouse.buttons == 0 {
            self.mouse.grab = None;
        }
    }

    /// A button pressed or released in a window's contents: an event for a graphical window, an
    /// xterm SGR report (ESC [ < b ; x ; y M/m) for a text window that asked for the mouse.
    fn button_event(&mut self, i: usize, buttons: u8, x: i32, y: i32, release: bool) {
        let view = &self.views[i];
        let (number, sgr) = if buttons & 1 != 0 { (1, 0) } else if buttons & 2 != 0 { (2, 2) } else { (3, 1) };
        if view.window.pixels.load(Ordering::Relaxed) {
            view.window.event(if release { event::RELEASE } else { event::PRESS }, x, y, number);
        } else if view.text.as_ref().is_some_and(|t| t.mouse_reporting) {
            let (col, row) = ((x - PAD).max(0) as usize / CELL_W, (y - PAD).max(0) as usize / CELL_H);
            let text = alloc::format!("\x1b[<{};{};{}{}", sgr, col + 1, row + 1, if release { 'm' } else { 'M' });
            view.window.input.lock().extend(text.as_bytes());
            crate::proc::wake_all();
        }
    }

    fn move_view(&mut self, i: usize, x: i32, y: i32) {
        let (w, h) = (self.width as i32, self.height as i32);
        let view = &mut self.views[i];
        // Keep enough of the header on the screen to grab it again.
        let (x, y) = (x.clamp(-(view.fw() - 60), w - 60), y.clamp(0, h - HEADER));
        if (x, y) == (view.x, view.y) {
            return;
        }
        let old = view.bounds();
        (view.x, view.y) = (x, y);
        let new = view.bounds();
        self.show(old.union(new));
    }

    fn pointer_rect(&self) -> Rect {
        Rect { x: self.mouse.x, y: self.mouse.y, w: POINTER_W as i32, h: POINTER.len() as i32 }
    }

    // ---- drawing ----

    /// Draws the console's changed cells. When it scrolled, its pixels move up instead.
    fn flush_console(&mut self) {
        let scrolled = core::mem::take(&mut self.console.scrolled);
        if scrolled >= self.rows {
            self.console.dirty.fill(true);
        } else if scrolled > 0 {
            let shift = scrolled * CELL_H * self.width;
            let len = self.layer.len();
            self.layer.copy_within(shift..len, 0);
        }
        let mut spans: Vec<(usize, usize, usize)> = Vec::new(); // (row, first column, last column)
        for i in 0..self.console.dirty.len() {
            if core::mem::take(&mut self.console.dirty[i]) {
                let (x, y) = (i % self.cols, i / self.cols);
                self.paint_console_cell(x, y);
                match spans.last_mut() {
                    Some(span) if span.0 == y => span.2 = x,
                    _ => spans.push((y, x, x)),
                }
            }
        }
        if scrolled > 0 {
            self.show(self.screen_rect());
        } else {
            for (y, x0, x1) in spans {
                let r = Rect { x: (x0 * CELL_W) as i32, y: (y * CELL_H) as i32, w: ((x1 - x0 + 1) * CELL_W) as i32, h: CELL_H as i32 };
                self.show(r);
            }
        }
    }

    fn paint_console_cell(&mut self, x: usize, y: usize) {
        let cell = self.console.cells[y * self.cols + x];
        let cursor = self.focus.is_none() && self.console.cursor_visible && self.console.cursor() == (x, y);
        let at = y * CELL_H * self.width + x * CELL_W;
        let width = self.width;
        draw_glyph(&mut self.layer[at..], width, cell, cursor);
    }

    /// Draws a text window's changed cells (or all of them) into its contents. Returns the area
    /// that changed, in contents coordinates.
    fn render_text(&mut self, i: usize, all: bool) -> Option<Rect> {
        let focused = self.focus == Some(self.views[i].window.id);
        let view = &mut self.views[i];
        let cw = view.cw as usize;
        let term = view.text.as_mut()?;
        let mut changed: Option<Rect> = None;
        for c in 0..term.cells.len() {
            if !core::mem::take(&mut term.dirty[c]) && !all {
                continue;
            }
            let (cx, cy) = (c % term.cols, c / term.cols);
            let cursor = focused && term.cursor_visible && term.cursor() == (cx, cy);
            let (px, py) = (PAD as usize + cx * CELL_W, PAD as usize + cy * CELL_H);
            draw_glyph(&mut view.content[py * cw + px..], cw, term.cells[c], cursor);
            let r = Rect { x: px as i32, y: py as i32, w: CELL_W as i32, h: CELL_H as i32 };
            changed = Some(changed.map_or(r, |c| c.union(r)));
        }
        if all {
            changed = Some(Rect { x: 0, y: 0, w: view.cw, h: view.ch });
        }
        changed
    }

    /// Part of a window's contents changed: copy it into the window's image and show it.
    fn content_changed(&mut self, i: usize, r: Rect) {
        let view = &mut self.views[i];
        copy_content(view, r);
        let first = !view.visible;
        view.visible = true;
        let shown = if first { view.bounds() } else { Rect { x: view.x + 1 + r.x, y: view.y + HEADER + r.y, w: r.w, h: r.h } };
        self.show(shown);
    }

    /// Redraws a window's frame (focus, title or hover changed) and shows it.
    fn refresh_frame(&mut self, i: usize) {
        self.render_frame(i);
        let view = &self.views[i];
        if view.visible {
            self.show(view.bounds());
        }
    }

    /// Draws a window's whole image: shadow, rounded frame, header with title and close button,
    /// and its contents.
    fn render_frame(&mut self, i: usize) {
        let focused = (self.focus == Some(self.views[i].window.id)) as usize;
        let view = &mut self.views[i];
        let (iw, ih, fw, fh) = (view.iw(), view.ih(), view.fw(), view.fh());
        let (header, border) = (HEADER_COLOR[focused], BORDER_COLOR[focused]);
        for iy in 0..ih {
            for ix in 0..iw {
                let (fx, fy) = (ix - SHADOW, iy - SHADOW);
                // The shadow, a little below the window, fading out.
                let ds = rounded_distance(fx, fy - SHADOW_DROP, fw, fh, RADIUS);
                let reach = SHADOW * 16;
                let k = (reach - ds.max(0)).max(0);
                let mut pixel = ((SHADOW_ALPHA * k / reach * k / reach) as u32) << 24;
                // The window itself, smoothed at its edges.
                let d = rounded_distance(fx, fy, fw, fh, RADIUS);
                let cover = (8 - d).clamp(0, 16) as u32;
                if cover > 0 {
                    let color = if d > -24 || fy == HEADER - 1 { border } else { header };
                    pixel = over(premultiply(color, cover * 255 / 16), pixel);
                }
                view.image[(iy * iw + ix) as usize] = pixel;
            }
        }
        // The title, centered (and cut short before the close button).
        let room = ((fw - 2 * 44) / CELL_W as i32).max(0) as usize;
        let title: Vec<char> = view.title.chars().take(room).collect();
        let tx = (fw - (title.len() * CELL_W) as i32) / 2;
        let ty = (HEADER - CELL_H as i32) / 2;
        for (n, &c) in title.iter().enumerate() {
            let at = ((SHADOW + ty) * iw + SHADOW + tx + (n * CELL_W) as i32) as usize;
            let cell = Cell { ch: c as u32, fg: 0, bg: 0 };
            draw_glyph_colors(&mut view.image[at..], iw as usize, cell, TITLE_COLOR[focused], header);
        }
        // The close button: a circle with an ×, red under the pointer.
        let (cx, cy) = view.close_center();
        let circle = if view.close_hover { CLOSE_HOVER } else { CLOSE_COLOR };
        let mark = if view.close_hover { 0xffffff } else { TITLE_COLOR[focused] };
        for dy in -CLOSE_RADIUS - 1..=CLOSE_RADIUS + 1 {
            for dx in -CLOSE_RADIUS - 1..=CLOSE_RADIUS + 1 {
                let d = isqrt(((dx * 16).pow(2) + (dy * 16).pow(2)) as u64) as i32 - CLOSE_RADIUS * 16;
                let cover = (8 - d).clamp(0, 16) as u32;
                if cover == 0 {
                    continue;
                }
                let on_mark = dx.abs() <= 3 && dy.abs() <= 3 && ((dx - dy).abs() <= 0 || (dx + dy).abs() <= 0);
                let color = if on_mark { mark } else { circle };
                let at = ((SHADOW + cy + dy) * iw + SHADOW + cx + dx) as usize;
                view.image[at] = mix(color, header, cover * 255 / 16) | 0xff00_0000;
            }
        }
        copy_content(view, Rect { x: 0, y: 0, w: view.cw, h: view.ch });
    }

    /// Puts a piece of the screen together (console, windows, pointer) and puts it on the display.
    fn show(&mut self, r: Rect) {
        let Some(r) = r.intersect(self.screen_rect()) else { return };
        let w = self.width;
        let views: Vec<usize> = (0..self.views.len()).filter(|&i| self.views[i].visible && self.views[i].bounds().intersect(r).is_some()).collect();
        let pointer = self.mouse.shown && self.pointer_rect().intersect(r).is_some();
        let (x0, x1) = (r.x as usize, r.right() as usize);
        let source = if views.is_empty() && !pointer {
            &self.layer
        } else {
            for y in r.y as usize..r.bottom() as usize {
                self.back[y * w + x0..y * w + x1].copy_from_slice(&self.layer[y * w + x0..y * w + x1]);
            }
            for i in views {
                let view = &self.views[i];
                let b = view.bounds();
                let c = b.intersect(r).unwrap();
                let iw = view.iw();
                for y in c.y..c.bottom() {
                    let src = &view.image[((y - b.y) * iw + (c.x - b.x)) as usize..][..c.w as usize];
                    let dst = &mut self.back[y as usize * w + c.x as usize..][..c.w as usize];
                    for (d, &s) in dst.iter_mut().zip(src) {
                        *d = over(s, *d);
                    }
                }
            }
            if pointer {
                self.draw_pointer(r);
            }
            &self.back
        };
        for y in r.y as usize..r.bottom() as usize {
            unsafe {
                let from = source.as_ptr().add(y * w + x0);
                core::ptr::copy_nonoverlapping(from, self.fb.add(y * self.pitch + x0), x1 - x0);
            }
        }
    }

    fn draw_pointer(&mut self, clip: Rect) {
        for (py, row) in POINTER.iter().enumerate() {
            for (px, &p) in row.iter().enumerate() {
                let (x, y) = (self.mouse.x + px as i32, self.mouse.y + py as i32);
                if p == b'.' || !clip.contains(x, y) {
                    continue;
                }
                self.back[y as usize * self.width + x as usize] = if p == b'#' { 0x101010 } else { 0xffffff };
            }
        }
    }
}

/// Copies part of a window's contents into its image, leaving the rounded bottom corners alone.
fn copy_content(view: &mut View, r: Rect) {
    let (iw, fw, fh) = (view.iw(), view.fw(), view.fh());
    for y in r.y..r.bottom() {
        let fy = HEADER + y;
        let near_bottom = fy >= fh - RADIUS - 1;
        for x in r.x..r.right() {
            let fx = 1 + x;
            if near_bottom && (fx <= RADIUS || fx >= fw - RADIUS - 1) && rounded_distance(fx, fy, fw, fh, RADIUS) > -24 {
                continue;
            }
            view.image[((SHADOW + fy) * iw + SHADOW + fx) as usize] = view.content[(y * view.cw + x) as usize] | 0xff00_0000;
        }
    }
}

/// Draws one character cell at the start of `to` (a buffer `stride` pixels wide).
fn draw_glyph(to: &mut [u32], stride: usize, cell: Cell, cursor: bool) {
    let (fg, bg) = (PALETTE[cell.fg as usize], PALETTE[cell.bg as usize]);
    let glyph = crate::font::glyph(cell.ch).or_else(|| crate::font::glyph('?' as u32)).unwrap();
    for (gy, &bits) in glyph.iter().enumerate() {
        let underline = cursor && gy >= CELL_H - 2;
        for gx in 0..CELL_W {
            let on = bits >> gx & 1 == 1 || underline;
            to[gy * stride + gx] = if on { if underline { PALETTE[7] } else { fg } } else { bg };
        }
    }
}

/// Draws a character's lit pixels in `fg` on `bg`, opaque (for window titles).
fn draw_glyph_colors(to: &mut [u32], stride: usize, cell: Cell, fg: u32, bg: u32) {
    let glyph = crate::font::glyph(cell.ch).or_else(|| crate::font::glyph('?' as u32)).unwrap();
    for (gy, &bits) in glyph.iter().enumerate() {
        for gx in 0..CELL_W {
            to[gy * stride + gx] = 0xff00_0000 | if bits >> gx & 1 == 1 { fg } else { bg };
        }
    }
}

/// Signed distance from a pixel's center to the edge of a w x h rectangle with rounded corners of
/// radius r, in 1/16 pixels: negative inside.
fn rounded_distance(px: i32, py: i32, w: i32, h: i32, r: i32) -> i32 {
    let (x, y) = (px * 16 + 8, py * 16 + 8);
    let (hw, hh, r) = (w * 8, h * 8, r * 16);
    let qx = (x - hw).abs() - (hw - r);
    let qy = (y - hh).abs() - (hh - r);
    let outside = if qx > 0 && qy > 0 { isqrt((qx as u64).pow(2) + (qy as u64).pow(2)) as i32 } else { qx.max(qy).max(0) };
    outside + qx.max(qy).min(0) - r
}

fn isqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// An opaque color with an alpha (0..255), premultiplied.
fn premultiply(color: u32, alpha: u32) -> u32 {
    let channel = |shift: u32| ((color >> shift & 0xff) * alpha / 255) << shift;
    alpha << 24 | channel(16) | channel(8) | channel(0)
}

/// `top` (premultiplied ARGB) over `bottom`.
fn over(top: u32, bottom: u32) -> u32 {
    let a = top >> 24;
    if a == 255 {
        return top;
    }
    if a == 0 {
        return bottom;
    }
    let inv = 255 - a;
    let channel = |shift: u32| ((top >> shift & 0xff) + (bottom >> shift & 0xff) * inv / 255).min(255) << shift;
    let alpha = (a + (bottom >> 24) * inv / 255).min(255);
    alpha << 24 | channel(16) | channel(8) | channel(0)
}

/// `a` mixed into `b`, `amount` of 255 being all `a`.
fn mix(a: u32, b: u32, amount: u32) -> u32 {
    let channel = |shift: u32| ((a >> shift & 0xff) * amount + (b >> shift & 0xff) * (255 - amount)) / 255 << shift;
    channel(16) | channel(8) | channel(0)
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
