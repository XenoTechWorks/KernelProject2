//! The screen: the text console on the boot framebuffer, and windows on top of it.
//!
//! The console is a terminal (term.rs) filling the screen. A program can also open `/dev/window`
//! to get a window floating over the console, in a frame with a title, a close button and a soft
//! shadow. A window shows either a small terminal of its own (what the program writes), or pixels
//! the program draws itself (after the WINDOW_PIXELS ioctl), which is what graphical apps use.
//! Windows are dragged by their header with the mouse and resized by their edges; clicking one raises it and gives it the keyboard. Their buttons close, maximize and minimize them.
//!
//! A desktop (Vela) gives some of its windows a role (WINDOW_ROLE): its wallpaper is a frameless
//! window under all the others, its bar and dock are panels over them, and its menus are popups.
//! Panels keep normal windows out of the strip of screen they sit on (the work area), and a desktop
//! can list the normal windows and bring one forward or minimize it (its taskbar). Without one the
//! command line stays underneath.
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
/// The header's buttons (minimize, maximize, close) sit side by side at its right end, after
/// Windows Vista's: close is wider, and red on the window in front.
const BUTTON_H: i32 = 20;
const CLOSE_W: i32 = 44;
const SMALL_W: i32 = 28;
const BUTTONS_RIGHT: i32 = 8; // from the frame's right edge
const BUTTON_GAP: i32 = 2;
/// How far outside a window's edges the pointer can grab them to resize.
const GRIP: i32 = 6;
const MIN_SIZE: (i32, i32) = (120, 60);

// Frames after Windows Vista's Aero, a darker modern take: a glassy header (a gradient with a
// shine over its top half), a light rim inside a dark edge, titles with a soft shadow.
// [unfocused, focused]
const HEADER_TOP: [u32; 2] = [0x41464f, 0x4d6488];
const HEADER_BOTTOM: [u32; 2] = [0x2c3037, 0x253247];
const RIM: [u32; 2] = [0x5d636e, 0x8ea7cf];
const EDGE: u32 = 0x07090d;
const TITLE_COLOR: [u32; 2] = [0xb4bbc6, 0xffffff];

/// Events a graphical window's program reads, 16 bytes each: kind, x, y, data (all 32-bit).
pub mod event {
    pub const MOTION: u32 = 1; // the pointer moved over the window (x, y in its pixels)
    pub const PRESS: u32 = 2; // a mouse button went down (data: 1 left, 2 right, 3 middle)
    pub const RELEASE: u32 = 3;
    pub const KEY: u32 = 4; // data: the key (see keyboard.rs), x: modifiers (1 shift, 2 ctrl)
    pub const LEAVE: u32 = 5; // the pointer left the window
    pub const FOCUS: u32 = 6; // data: 1 got the keyboard, 0 lost it
    pub const SCROLL: u32 = 7; // the mouse wheel (data: steps, as a signed number; down is positive)
    pub const RESIZE: u32 = 8; // the user resized the window: x, y are its new width and height
    pub const RAWKEY: u32 = 9; // a physical key: x its PC scancode (0x100+ after 0xE0), data 1 down, 0 up
    pub const WINDOWS: u32 = 10; // (to desktop windows) the list of windows, or which has the keyboard, changed
}

/// What a window is for. Normal windows have a frame; the others are a desktop's own.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Role {
    Normal,
    /// Frameless, under every other window (wallpaper and desktop icons).
    Desktop,
    /// Frameless, over normal windows, in a strip they don't go into (a bar or a dock). Clicking
    /// it doesn't take the keyboard from the window that has it.
    Panel,
    /// Frameless, over everything, and takes the keyboard (a menu).
    Popup,
}

impl Role {
    pub fn from(n: u32) -> Option<Role> {
        [Role::Normal, Role::Desktop, Role::Panel, Role::Popup].get(n as usize).copied()
    }
}

/// A normal window, for a desktop's taskbar.
pub struct ListedWindow {
    pub id: u32,
    pub focused: bool,
    pub minimized: bool,
    pub title: String,
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
    /// The header button under the pointer (lit up).
    hover_button: Option<Button>,
    role: Role,
    /// A frameless window's corners are rounded this much.
    radius: i32,
    minimized: bool,
    /// Maximized: where it was and its size before, to go back to.
    restore: Option<(i32, i32, i32, i32)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Button {
    Close,
    Maximize,
    Minimize,
}

impl View {
    fn framed(&self) -> bool {
        self.role == Role::Normal
    }
    fn header(&self) -> i32 {
        if self.framed() { HEADER } else { 0 }
    }
    fn border(&self) -> i32 {
        self.framed() as i32
    }
    fn shadow(&self) -> i32 {
        if matches!(self.role, Role::Normal | Role::Popup) { SHADOW } else { 0 }
    }
    fn corner(&self) -> i32 {
        if self.framed() { RADIUS } else { self.radius }
    }
    /// Bottom to top: the desktop, normal windows, panels, popups.
    fn layer(&self) -> u8 {
        match self.role {
            Role::Desktop => 0,
            Role::Normal => 1,
            Role::Panel => 2,
            Role::Popup => 3,
        }
    }
    /// On the screen now: it has drawn something and isn't minimized.
    fn shown(&self) -> bool {
        self.visible && !self.minimized
    }
    fn fw(&self) -> i32 {
        self.cw + 2 * self.border()
    }
    fn fh(&self) -> i32 {
        self.ch + self.header() + self.border()
    }
    fn iw(&self) -> i32 {
        self.fw() + 2 * self.shadow()
    }
    fn ih(&self) -> i32 {
        self.fh() + 2 * self.shadow()
    }
    fn frame_rect(&self) -> Rect {
        Rect { x: self.x, y: self.y, w: self.fw(), h: self.fh() }
    }
    /// Where the pointer reaches the window: its frame and a little past its edges (to resize).
    fn reach(&self) -> Rect {
        if !self.framed() {
            return self.frame_rect();
        }
        Rect { x: self.x - GRIP, y: self.y - GRIP, w: self.fw() + 2 * GRIP, h: self.fh() + 2 * GRIP }
    }
    fn bounds(&self) -> Rect {
        let s = self.shadow();
        Rect { x: self.x - s, y: self.y - s, w: self.iw(), h: self.ih() }
    }
    fn content_rect(&self) -> Rect {
        Rect { x: self.x + self.border(), y: self.y + self.header(), w: self.cw, h: self.ch }
    }
    /// Where a header button is, in frame coordinates.
    fn button_rect(&self, button: Button) -> Rect {
        let close_x = self.fw() - BUTTONS_RIGHT - CLOSE_W;
        let x = match button {
            Button::Close => close_x,
            Button::Maximize => close_x - BUTTON_GAP - SMALL_W,
            Button::Minimize => close_x - 2 * (BUTTON_GAP + SMALL_W),
        };
        let w = if button == Button::Close { CLOSE_W } else { SMALL_W };
        Rect { x, y: (HEADER - BUTTON_H) / 2, w, h: BUTTON_H }
    }
}

/// Where a point falls in a window.
enum Part {
    Button(Button),
    Header,
    Border,
    /// Edges that resize the window: bit 0 right, 1 bottom, 2 left, 3 top (two at a corner).
    Resize(u8),
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
    /// Resizing a window: its id, which edges, and at the start: the pointer, the contents size and
    /// where the window was.
    resize: Option<(u32, u8, (i32, i32), (i32, i32), (i32, i32))>,
    /// Over a resizable edge: the pointer shows that.
    resize_shape: bool,
    /// Buttons held on each pointing device (the PS/2 mouse, the tablet): QEMU sends moves to one
    /// and clicks to the other, and each reports only its own buttons.
    device_buttons: [u8; 2],
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
/// A physical key went down or up. Only graphical windows hear about these (games, mostly, which
/// need to know a key is still held); everyone else gets the characters from `key`.
pub fn raw_key(scancode: u32, down: bool) {
    let target = CONSOLE.lock().as_ref().and_then(|s| s.focused()).map(|v| v.window.clone());
    if let Some(window) = target {
        if window.pixels.load(Ordering::Relaxed) {
            window.event(event::RAWKEY, scancode as i32, 0, down as u32);
        }
    }
}

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
        let visible: Vec<u32> = screen.views.iter().filter(|v| v.shown() && v.framed()).map(|v| v.window.id).collect();
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
        hover_button: None,
        role: Role::Normal,
        radius: 0,
        minimized: false,
        restore: None,
    };
    let (cw, ch) = text_size(view.text.as_ref().unwrap());
    // Each new window a little below and right of the last, so they don't hide each other.
    let work = screen.work_area();
    view.x = (screen.width as i32 - (cw + 2)) / 2 + 32 * (n % 8);
    view.y = work.y + 64 + 32 * (n % 8);
    let i = screen.insert(view);
    screen.resize_view(i, cw, ch);
    Some(window)
}

/// Gives a graphical window a role (see Role) and puts it at x, y with contents w x h (a desktop
/// always covers the whole screen), its corners rounded by `radius`.
pub fn window_set_role(id: u32, role: Role, x: i32, y: i32, w: i32, h: i32, radius: i32) -> Option<()> {
    let mut guard = CONSOLE.lock();
    let screen = guard.as_mut()?;
    let i = screen.index_of(id)?;
    if !screen.views[i].window.pixels.load(Ordering::Relaxed) {
        return None;
    }
    let mut view = screen.views.remove(i);
    let old = view.bounds();
    let was_shown = view.shown();
    view.role = role;
    view.radius = radius.clamp(0, 64);
    view.moved = true;
    view.restore = None;
    view.minimized = false;
    let (sw, sh) = (screen.width as i32, screen.height as i32);
    let (x, y, w, h) = if role == Role::Desktop { (0, 0, sw, sh) } else { (x, y, w, h) };
    (view.x, view.y) = (x.clamp(0, sw - 16), y.clamp(0, sh - 16));
    let i = screen.insert(view);
    screen.resize_view(i, w.clamp(16, sw), h.clamp(16, sh));
    if was_shown {
        screen.show(old);
    }
    if role == Role::Popup {
        screen.set_focus(Some(id));
    }
    screen.windows_changed();
    Some(())
}

/// The normal windows (that have shown themselves), bottom to top.
pub fn window_list() -> Vec<ListedWindow> {
    let guard = CONSOLE.lock();
    let Some(screen) = guard.as_ref() else { return Vec::new() };
    screen.views.iter().filter(|v| v.framed() && v.visible).map(|v| ListedWindow {
        id: v.window.id,
        focused: screen.focus == Some(v.window.id),
        minimized: v.minimized,
        title: v.title.clone(),
    }).collect()
}

/// Brings a window forward (back from minimized) and gives it the keyboard.
pub fn window_activate(id: u32) -> Option<()> {
    let mut guard = CONSOLE.lock();
    let screen = guard.as_mut()?;
    let i = screen.index_of(id)?;
    screen.views[i].minimized = false;
    let i = screen.raise(i);
    screen.show(screen.views[i].bounds());
    screen.set_focus(Some(id));
    screen.windows_changed();
    Some(())
}

pub fn window_minimize(id: u32) -> Option<()> {
    let mut guard = CONSOLE.lock();
    let screen = guard.as_mut()?;
    let i = screen.index_of(id)?;
    screen.minimize(i);
    Some(())
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
            screen.windows_changed();
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
/// bit 1 right, bit 2 middle), and its wheel turned `wheel` steps (down is positive).
pub fn mouse(dx: i32, dy: i32, buttons: u8, wheel: i32) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        screen.mouse.device_buttons[0] = buttons;
        let held = screen.mouse.device_buttons[0] | screen.mouse.device_buttons[1];
        screen.mouse_event(dx, dy, held, wheel);
    }
}

/// The pointer is at `at` on the screen (from a tablet, which knows where it is; None: where it
/// was), with these buttons held and the wheel turned `wheel` steps.
pub fn mouse_at(at: Option<(i32, i32)>, buttons: u8, wheel: i32) {
    if let Some(screen) = CONSOLE.lock().as_mut() {
        let (dx, dy) = at.map_or((0, 0), |(x, y)| (x - screen.mouse.x, y - screen.mouse.y));
        screen.mouse.device_buttons[1] = buttons;
        let held = screen.mouse.device_buttons[0] | screen.mouse.device_buttons[1];
        screen.mouse_event(dx, dy, held, wheel);
    }
}

/// Where the pointer is, relative to a window's contents (outside it the numbers go negative or
/// past its size). For apps that follow the pointer, like `eyes`.
pub fn window_pointer(id: u32) -> (i32, i32) {
    let guard = CONSOLE.lock();
    let Some(screen) = guard.as_ref() else { return (0, 0) };
    let Some(i) = screen.index_of(id) else { return (0, 0) };
    let c = screen.views[i].content_rect();
    (screen.mouse.x - c.x, screen.mouse.y - c.y)
}

/// The screen's size in pixels.
pub fn pixel_size() -> (usize, usize) {
    CONSOLE.lock().as_ref().map_or((0, 0), |s| (s.width, s.height))
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

    /// The topmost visible window the pointer reaches at a point.
    fn top_at(&self, x: i32, y: i32) -> Option<usize> {
        self.views.iter().rposition(|v| v.shown() && v.reach().contains(x, y))
    }

    /// Adds a window above the others of its layer. Returns where it went.
    fn insert(&mut self, view: View) -> usize {
        let layer = view.layer();
        let at = self.views.iter().position(|v| v.layer() > layer).unwrap_or(self.views.len());
        self.views.insert(at, view);
        at
    }

    /// The part of the screen the panels leave for normal windows: below panels in the top half,
    /// above those in the bottom half.
    fn work_area(&self) -> Rect {
        let (mut top, mut bottom) = (0, self.height as i32);
        for v in self.views.iter().filter(|v| v.role == Role::Panel && v.visible) {
            let r = v.frame_rect();
            if r.y + r.h / 2 < self.height as i32 / 2 {
                top = top.max(r.bottom());
            } else {
                bottom = bottom.min(r.y);
            }
        }
        Rect { x: 0, y: top, w: self.width as i32, h: (bottom - top).max(2 * HEADER) }
    }

    /// Tells the desktop's windows (its taskbar) that the windows changed.
    fn windows_changed(&self) {
        for v in self.views.iter().filter(|v| !v.framed()) {
            v.window.event(event::WINDOWS, 0, 0, 0);
        }
    }

    /// Who gets the keyboard when the window that had it goes: the top normal window, or else the
    /// desktop.
    fn fallback_focus(&self) -> Option<u32> {
        let normal = self.views.iter().rev().find(|v| v.shown() && v.framed());
        normal.or_else(|| self.views.iter().find(|v| v.shown() && v.role == Role::Desktop)).map(|v| v.window.id)
    }

    fn minimize(&mut self, i: usize) {
        let view = &mut self.views[i];
        if !view.framed() || view.minimized {
            return;
        }
        view.minimized = true;
        let (id, bounds) = (view.window.id, view.bounds());
        self.show(bounds);
        if self.focus == Some(id) {
            self.focus = None;
            let next = self.fallback_focus();
            self.set_focus(next);
        }
        self.windows_changed();
    }

    /// Fills the work area with a window, or puts it back how it was.
    fn toggle_maximize(&mut self, i: usize) {
        let old = self.views[i].bounds();
        let (x, y, w, h) = match self.views[i].restore.take() {
            Some(before) => before,
            None => {
                let v = &self.views[i];
                let before = (v.x, v.y, v.cw, v.ch);
                self.views[i].restore = Some(before);
                let work = self.work_area();
                (work.x, work.y, work.w - 2, work.h - HEADER - 1)
            }
        };
        (self.views[i].x, self.views[i].y) = (x, y);
        self.user_resize(i, w, h);
        self.show(old);
        let bounds = self.views[i].bounds();
        self.show(bounds);
    }

    fn part(view: &View, x: i32, y: i32) -> Part {
        let (fx, fy) = (x - view.x, y - view.y);
        if !view.framed() {
            return Part::Content(fx, fy);
        }
        for button in [Button::Close, Button::Maximize, Button::Minimize] {
            if view.button_rect(button).contains(fx, fy) {
                return Part::Button(button);
            }
        }
        let (right, bottom) = (fx >= view.fw() - 3, fy >= view.fh() - 3);
        let (left, top) = (fx < 3, fy < 3);
        // The sides below the header (its sides are for dragging); the top only at its very edge.
        let edges = (right && (fy >= HEADER || top)) as u8
            | (bottom as u8) << 1
            | ((left && (fy >= HEADER || top)) as u8) << 2
            | (top as u8) << 3;
        if edges != 0 {
            Part::Resize(edges)
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
        self.windows_changed();
    }

    /// Puts a window on top of the others of its layer. Returns where it is now.
    fn raise(&mut self, i: usize) -> usize {
        let layer = self.views[i].layer();
        if self.views.get(i + 1).is_none_or(|v| v.layer() > layer) {
            return i;
        }
        let view = self.views.remove(i);
        let bounds = view.bounds();
        let i = self.insert(view);
        self.show(bounds);
        i
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
        if self.mouse.resize.is_some_and(|r| r.0 == id) {
            self.mouse.resize = None;
        }
        if view.shown() {
            self.show(view.bounds());
        }
        if self.focus == Some(id) {
            self.focus = None;
            let next = self.fallback_focus();
            self.set_focus(next);
            if next.is_none() {
                let (x, y) = self.console.cursor();
                self.console.dirty[y * self.cols + x] = true;
                self.flush_console();
            }
        }
        self.windows_changed();
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
        // Keep what was there (top-left) until the program draws the new size.
        let old_w = if view.content.is_empty() { 0 } else { old_fw - 2 };
        let old_content = core::mem::replace(&mut view.content, vec![fill; (cw * ch) as usize]);
        if old_w > 0 {
            let old_h = old_content.len() as i32 / old_w;
            for y in 0..old_h.min(ch) {
                let n = old_w.min(cw) as usize;
                let (from, to) = ((y * old_w) as usize, (y * cw) as usize);
                view.content[to..to + n].copy_from_slice(&old_content[from..from + n]);
            }
        }
        view.image = vec![0; (view.iw() * view.ih()) as usize];
        self.render_text(i, true);
        self.render_frame(i);
        if self.views[i].shown() {
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
            self.windows_changed();
        }
        let changed = self.render_text(i, false);
        if changed.is_some() || !self.views[i].visible {
            let all = Rect { x: 0, y: 0, w: self.views[i].cw, h: self.views[i].ch };
            self.content_changed(i, changed.unwrap_or(all));
        }
    }

    // ---- the mouse ----

    fn mouse_event(&mut self, dx: i32, dy: i32, buttons: u8, wheel: i32) {
        let old_pointer = self.pointer_rect();
        self.mouse.x = (self.mouse.x + dx).clamp(0, self.width as i32 - 1);
        self.mouse.y = (self.mouse.y + dy).clamp(0, self.height as i32 - 1);
        let was_shown = core::mem::replace(&mut self.mouse.shown, true);
        let (mx, my) = (self.mouse.x, self.mouse.y);
        let pressed = buttons & !self.mouse.buttons;
        let released = self.mouse.buttons & !buttons;
        self.mouse.buttons = buttons;

        if let Some((id, edges, (sx, sy), (cw, ch), (x0, y0))) = self.mouse.resize {
            if let Some(i) = self.index_of(id) {
                let (dx, dy) = (mx - sx, my - sy);
                let w = if edges & 1 != 0 { cw + dx } else if edges & 4 != 0 { cw - dx } else { cw };
                let mut h = if edges & 2 != 0 { ch + dy } else if edges & 8 != 0 { ch - dy } else { ch };
                if edges & 8 != 0 {
                    h = h.min(ch + y0); // the top stops at the top of the screen
                }
                self.user_resize(i, w, h);
                // Left and top edges: the opposite edge stays where it was.
                if edges & 12 != 0 {
                    let view = &mut self.views[i];
                    let old = view.bounds();
                    if edges & 4 != 0 {
                        view.x = x0 + cw - view.cw;
                    }
                    if edges & 8 != 0 {
                        view.y = y0 + ch - view.ch;
                    }
                    let new = view.bounds();
                    self.show(old.union(new));
                }
            }
            if released & 1 != 0 {
                self.mouse.resize = None;
            }
        } else if let Some((id, gx, gy)) = self.mouse.drag {
            if let Some(i) = self.index_of(id) {
                self.move_view(i, mx - gx, my - gy);
            }
            if released & 1 != 0 {
                self.mouse.drag = None;
            }
        } else {
            if wheel != 0 {
                self.wheel(mx, my, wheel);
            }
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

    /// The mouse wheel turned over a window: graphical windows get it as an event, text windows
    /// that asked for the mouse as xterm's buttons 4 and 5.
    fn wheel(&mut self, mx: i32, my: i32, steps: i32) {
        let Some(i) = self.top_at(mx, my) else { return };
        let view = &self.views[i];
        let c = view.content_rect();
        if !c.contains(mx, my) {
            return;
        }
        if view.window.pixels.load(Ordering::Relaxed) {
            view.window.event(event::SCROLL, mx - c.x, my - c.y, steps as u32);
        } else if view.text.as_ref().is_some_and(|t| t.mouse_reporting) {
            let (col, row) = ((mx - c.x - PAD).max(0) as usize / CELL_W, (my - c.y - PAD).max(0) as usize / CELL_H);
            let code = if steps > 0 { 65 } else { 64 };
            let text = alloc::format!("\x1b[<{};{};{}M", code, col + 1, row + 1);
            view.window.input.lock().extend(text.as_bytes());
            crate::proc::wake_all();
        }
    }

    /// The user dragged an edge: a new contents size (text windows keep whole cells). Graphical
    /// programs are told, and draw the new size.
    fn user_resize(&mut self, i: usize, w: i32, h: i32) {
        let (max_w, max_h) = (self.width as i32 - 2, self.height as i32 - HEADER - 1);
        let (w, h) = (w.clamp(MIN_SIZE.0, max_w), h.clamp(MIN_SIZE.1, max_h));
        let view = &mut self.views[i];
        view.moved = true;
        let (w, h) = match view.text.as_mut() {
            Some(term) => {
                let cols = ((w - 2 * PAD) as usize / CELL_W).max(8);
                let rows = ((h - 2 * PAD) as usize / CELL_H).max(1);
                if (cols, rows) == (term.cols, term.rows) {
                    return;
                }
                term.resize(cols, rows);
                text_size(term)
            }
            None => (w, h),
        };
        if (w, h) == (view.cw, view.ch) {
            return;
        }
        if view.text.is_none() {
            view.window.event(event::RESIZE, w, h, 0);
        }
        self.resize_view(i, w, h);
    }

    /// The pointer moved: tell graphical windows, and light up close buttons.
    fn hover(&mut self, mx: i32, my: i32, moved: bool) {
        let top = self.top_at(mx, my);
        self.mouse.resize_shape = top.is_some_and(|i| matches!(Self::part(&self.views[i], mx, my), Part::Resize(_)));
        for i in 0..self.views.len() {
            let over = match top == Some(i) {
                true => match Self::part(&self.views[i], mx, my) {
                    Part::Button(b) => Some(b),
                    _ => None,
                },
                false => None,
            };
            if self.views[i].hover_button != over {
                self.views[i].hover_button = over;
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
        let i = self.raise(i);
        let id = self.views[i].window.id;
        // A bar or dock leaves the keyboard where it was (its taskbar acts on that window).
        if self.views[i].role != Role::Panel {
            self.set_focus(Some(id));
        }
        match Self::part(&self.views[i], mx, my) {
            Part::Button(Button::Close) if pressed & 1 != 0 => {
                // Closing tells the program (its reads end); the window goes away now.
                self.views[i].window.closed.store(true, Ordering::Relaxed);
                crate::proc::wake_all();
                self.remove(id);
            }
            Part::Button(Button::Maximize) if pressed & 1 != 0 => self.toggle_maximize(i),
            Part::Button(Button::Minimize) if pressed & 1 != 0 => self.minimize(i),
            Part::Resize(edges) if pressed & 1 != 0 => {
                let view = &self.views[i];
                self.mouse.resize = Some((id, edges, (mx, my), (view.cw, view.ch), (view.x, view.y)));
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
        let w = self.width as i32;
        let work = self.work_area();
        let view = &mut self.views[i];
        // Keep enough of the header on the screen to grab it again, and out from under the panels.
        let (x, y) = (x.clamp(-(view.fw() - 60), w - 60), y.clamp(work.y, work.bottom() - HEADER));
        if (x, y) == (view.x, view.y) {
            return;
        }
        let old = view.bounds();
        (view.x, view.y) = (x, y);
        view.restore = None; // dragged away: no longer maximized
        let new = view.bounds();
        self.show(old.union(new));
    }

    /// The pointer's picture and where its top-left corner goes.
    fn pointer(&self) -> (&'static [&'static [u8; POINTER_W]], i32, i32) {
        if self.mouse.resize_shape || self.mouse.resize.is_some() {
            (&RESIZE_POINTER, self.mouse.x - 7, self.mouse.y - 7)
        } else {
            (&POINTER, self.mouse.x, self.mouse.y)
        }
    }

    fn pointer_rect(&self) -> Rect {
        let (_, x, y) = self.pointer();
        Rect { x, y, w: POINTER_W as i32, h: 16 }
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
        if view.minimized {
            return;
        }
        let c = view.content_rect();
        let shown = if first { view.bounds() } else { Rect { x: c.x + r.x, y: c.y + r.y, w: r.w, h: r.h } };
        let (id, framed) = (view.window.id, view.framed());
        self.show(shown);
        if first {
            // On a desktop, an app that opens is what the user wants to use next. (Over the
            // command line it isn't: after `clock &` you keep typing in the shell.)
            if framed && self.views.iter().any(|v| v.role == Role::Desktop) {
                self.set_focus(Some(id));
            }
            self.windows_changed();
        }
    }

    /// Redraws a window's frame (focus, title or hover changed) and shows it.
    fn refresh_frame(&mut self, i: usize) {
        self.render_frame(i);
        let view = &self.views[i];
        if view.shown() {
            self.show(view.bounds());
        }
    }

    /// Draws a window's whole image: shadow, rounded frame, header with title and close button,
    /// and its contents.
    fn render_frame(&mut self, i: usize) {
        let focused = (self.focus == Some(self.views[i].window.id)) as usize;
        let view = &mut self.views[i];
        let (iw, ih, fw, fh, s, r) = (view.iw(), view.ih(), view.fw(), view.fh(), view.shadow(), view.corner());
        for iy in 0..ih {
            for ix in 0..iw {
                let (fx, fy) = (ix - s, iy - s);
                let mut pixel = if s > 0 { shadow_at(fx, fy, fw, fh, r) } else { 0 };
                // The window itself, smoothed at its edges (a frameless one is all contents).
                if view.framed() {
                    let d = rounded_distance(fx, fy, fw, fh, r);
                    let cover = (8 - d).clamp(0, 16) as u32;
                    if cover > 0 {
                        pixel = over(premultiply(frame_color(fy, d, focused), cover * 255 / 16), pixel);
                    }
                }
                view.image[(iy * iw + ix) as usize] = pixel;
            }
        }
        if view.framed() {
            draw_title(view, focused);
            for button in [Button::Close, Button::Maximize, Button::Minimize] {
                draw_button(view, button, focused);
            }
        }
        copy_content(view, Rect { x: 0, y: 0, w: view.cw, h: view.ch });
    }

    /// Puts a piece of the screen together (console, windows, pointer) and puts it on the display.
    fn show(&mut self, r: Rect) {
        let Some(r) = r.intersect(self.screen_rect()) else { return };
        let w = self.width;
        let views: Vec<usize> = (0..self.views.len()).filter(|&i| self.views[i].shown() && self.views[i].bounds().intersect(r).is_some()).collect();
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
        let (picture, x0, y0) = self.pointer();
        for (py, row) in picture.iter().enumerate() {
            for (px, &p) in row.iter().enumerate() {
                let (x, y) = (x0 + px as i32, y0 + py as i32);
                if p == b'.' || !clip.contains(x, y) {
                    continue;
                }
                self.back[y as usize * self.width + x as usize] = if p == b'#' { 0x101010 } else { 0xffffff };
            }
        }
    }
}

/// Copies part of a window's contents into its image. In a frame, the rounded bottom corners
/// are left alone; a frameless window's corners are rounded off, smoothly, over its shadow.
fn copy_content(view: &mut View, r: Rect) {
    let (iw, fw, fh, s, rad) = (view.iw(), view.fw(), view.fh(), view.shadow(), view.corner());
    let (left, top) = (view.border(), view.header());
    let framed = view.framed();
    for y in r.y..r.bottom() {
        let fy = top + y;
        let near_edge = fy < rad + 1 || fy >= fh - rad - 1;
        for x in r.x..r.right() {
            let fx = left + x;
            let color = view.content[(y * view.cw + x) as usize] & 0xff_ffff;
            let at = ((s + fy) * iw + s + fx) as usize;
            if rad == 0 || !near_edge || (fx > rad && fx < fw - rad - 1) {
                view.image[at] = color | 0xff00_0000;
                continue;
            }
            let d = rounded_distance(fx, fy, fw, fh, rad);
            if framed {
                if d <= -24 {
                    view.image[at] = color | 0xff00_0000;
                }
            } else {
                let cover = (8 - d).clamp(0, 16) as u32;
                let under = if s > 0 { shadow_at(fx, fy, fw, fh, rad) } else { 0 };
                view.image[at] = over(premultiply(color, cover * 255 / 16), under);
            }
        }
    }
}

/// The soft shadow under a window, a little below it, at a pixel of its image (in frame
/// coordinates), as premultiplied black.
fn shadow_at(fx: i32, fy: i32, fw: i32, fh: i32, radius: i32) -> u32 {
    let ds = rounded_distance(fx, fy - SHADOW_DROP, fw, fh, radius);
    let reach = SHADOW * 16;
    let k = (reach - ds.max(0)).max(0);
    ((SHADOW_ALPHA * k / reach * k / reach) as u32) << 24
}

/// The frame's color at row `fy`, `d` (1/16 pixels, negative inside) from its edge: a dark edge, a
/// light rim inside it, and the header's glassy gradient with a shine over its top half.
fn frame_color(fy: i32, d: i32, focused: usize) -> u32 {
    if d > -16 {
        return EDGE;
    }
    let below = if fy < HEADER { HEADER_TOP[focused] } else { HEADER_BOTTOM[focused] };
    if d > -32 {
        // The rim: bright along the header, fading down the sides.
        let k = if fy < HEADER { 150 } else { 70 };
        return mix(RIM[focused], below, k);
    }
    if fy >= HEADER - 1 {
        return if fy == HEADER - 1 { EDGE } else { HEADER_BOTTOM[focused] };
    }
    let base = mix(HEADER_BOTTOM[focused], HEADER_TOP[focused], (fy * 255 / HEADER) as u32);
    let half = HEADER / 2;
    if fy < half { mix(0xffffff, base, (34 * (half - fy) / half) as u32) } else { base }
}

/// Sets a frame pixel (frame coordinates) to `color` laid over what's there, `alpha` strong.
fn blend_frame(view: &mut View, fx: i32, fy: i32, color: u32, alpha: u32) {
    let (iw, s) = (view.iw(), view.shadow());
    if fx < 0 || fy < 0 || fx >= view.fw() || fy >= view.fh() || alpha == 0 {
        return;
    }
    let at = ((s + fy) * iw + s + fx) as usize;
    let under = view.image[at];
    view.image[at] = mix(color, under, alpha.min(255)) | (under & 0xff00_0000);
}

/// The title: left-aligned in the smooth title font, with a soft shadow, cut short with "…" before
/// the buttons.
fn draw_title(view: &mut View, focused: usize) {
    let room = view.button_rect(Button::Minimize).x - 14 - 12;
    let mut title = view.title.clone();
    if crate::title_font::width(&title) > room {
        while !title.is_empty() && crate::title_font::width(&title) + crate::title_font::width("…") > room {
            title.pop();
        }
        title.push('…');
    }
    let baseline = HEADER / 2 + 5;
    for (dx, dy, color, strength) in [(0, 1, 0x000000u32, 150u32), (0, 0, TITLE_COLOR[focused], 255)] {
        let mut pen = 14 * 64;
        for c in title.chars() {
            let Some(g) = crate::title_font::glyph_or_box(c) else { continue };
            let gx = pen / 64 + g.left + dx;
            let gy = baseline - g.top + dy;
            for y in 0..g.h {
                for x in 0..g.w {
                    let a = g.alpha[y * g.w + x] as u32 * strength / 255;
                    blend_frame(view, gx + x as i32, gy + y as i32, color, a);
                }
            }
            pen += g.advance;
        }
    }
}

/// A header button: a small glassy pill (close is red on the window in front, and every button
/// brightens under the pointer) with its mark in white: a bar, a box, or an ×.
fn draw_button(view: &mut View, button: Button, focused: usize) {
    let r = view.button_rect(button);
    let hover = view.hover_button == Some(button);
    let (top, bottom) = match (button, focused == 1, hover) {
        (Button::Close, _, true) => (0xf0705a, 0xc4321c),
        (Button::Close, true, false) => (0xd65a45, 0xa22b18),
        (_, _, true) => (0x6f86ad, 0x3a4c6c),
        (_, true, false) => (0x55657f, 0x2d394d),
        _ => (0x4e535c, 0x30343b),
    };
    for y in 0..r.h {
        for x in 0..r.w {
            // Rounded corners, 4 pixels, smoothed.
            let d = rounded_distance(x, y, r.w, r.h, 4);
            let cover = (8 - d).clamp(0, 16) as u32 * 255 / 16;
            if cover == 0 {
                continue;
            }
            let mut color = if d > -16 { EDGE } else { mix(bottom, top, (y * 255 / r.h) as u32) };
            if d <= -16 && y < r.h / 2 {
                color = mix(0xffffff, color, (60 * (r.h / 2 - y) / (r.h / 2)) as u32);
            }
            blend_frame(view, r.x + x, r.y + y, color, cover);
        }
    }
    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
    let white = if focused == 1 || hover { 0xffffff } else { 0xd0d4da };
    match button {
        Button::Minimize => {
            for x in -4..4 {
                for y in 2..4 {
                    blend_frame(view, cx + x, cy + y, white, 255);
                }
            }
        }
        Button::Maximize => {
            for x in -5..5 {
                for y in -4..4 {
                    let on = y <= -3 || y == 3 || x == -5 || x == 4;
                    if on {
                        blend_frame(view, cx + x, cy + y, white, 255);
                    }
                }
            }
        }
        Button::Close => {
            // Two smooth diagonal strokes, about 1.6 pixels wide and 8 across. In 1/16 pixels:
            // the distance from a pixel's center to each stroke (a segment through the middle).
            let stroke = |across: i32, along: i32| {
                let off = across.abs() * 181 / 256; // / sqrt(2)
                let past = (along.abs() * 181 / 256 - 90).max(0); // beyond the stroke's end
                isqrt((off * off + past * past) as u64) as i32
            };
            for y in -7..=7 {
                for x in -7..=7 {
                    let (px, py) = (x * 16 + 8, y * 16 + 8);
                    let d = stroke(px - py, px + py).min(stroke(px + py, px - py)) - 13;
                    let cover = (8 - d).clamp(0, 16) as u32 * 255 / 16;
                    blend_frame(view, cx + x, cy + y, white, cover);
                }
            }
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
const POINTER_W: usize = 15;
const POINTER: [&[u8; POINTER_W]; 16] = [
    b"#..............", b"##.............", b"#o#............", b"#oo#...........",
    b"#ooo#..........", b"#oooo#.........", b"#ooooo#........", b"#oooooo#.......",
    b"#ooooooo#......", b"#oooooooo#.....", b"#ooooo####.....", b"#oo#oo#........",
    b"#o#.#oo#.......", b"##..#oo#.......", b"#....#oo#......", b".....###.......",
];
// Over a resizable edge: a diagonal double arrow, centered on the pointer.
const RESIZE_POINTER: [&[u8; POINTER_W]; 15] = [
    b"#####..........", b"#ooo#..........", b"#oo#...........", b"#o#o#..........",
    b"##.#o#.........", b"....#o#........", b".....#o#.......", b"......#o#......",
    b".......#o#.....", b"........#o#....", b".........#o#.##", b"..........#o#o#",
    b"...........#oo#", b"..........#ooo#", b"..........#####",
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
