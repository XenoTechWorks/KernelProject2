//! Processes, threads and the scheduler.
//!
//! Like Linux, the scheduler runs *tasks*. A task has its own registers, kernel stack, signal mask and
//! thread-local storage, and points at parts it may share with other tasks: its memory, its open
//! files (with the current directory) and its signal handlers. A process is a group of tasks sharing
//! all three; `fork` copies them instead. A task's `pid` is Linux's thread id; its `tgid` (thread
//! group id) is what programs call the process id.
//!
//! One CPU for now, and the kernel itself is never interrupted: interrupts are only enabled while a
//! program runs or while the CPU idles. So the task table and the shared parts need no locks, only
//! care not to keep a reference across a switch. The scheduler loop runs on the boot stack; each task
//! has its own kernel stack, and switching is saving one stack pointer and loading another.

pub mod elf;
pub mod fd;
pub mod futex;
pub mod ptrace;
pub mod signal;
pub mod timer;

use crate::arch::idt::{self, TrapFrame};
use crate::arch::{cpu, gdt};
use crate::mm::paging::AddressSpace;
use crate::sys::errno::*;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};
use fd::FdTable;

pub use signal::{SIGINT, SIGKILL, SIGPIPE, SIGQUIT, SIGSEGV, SIGSTOP};

const KERNEL_STACK_SIZE: usize = 64 * 1024;
/// Highest address of every program's stack, and how far down it may grow.
pub const STACK_TOP: u64 = 0x0000_7fff_ffff_f000;
pub const STACK_LIMIT: u64 = 8 * 1024 * 1024;
/// Anonymous and file mappings are placed downward from here.
pub const MMAP_TOP: u64 = 0x0000_7000_0000_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Runnable,
    Sleeping,
    /// The process ended; kept until its parent collects the status with wait4.
    Zombie(i32),
    /// The first thread of a process ended while others still run; it becomes the zombie later.
    Lingering,
    /// A finished thread, removed by the scheduler once it has switched away from it.
    Dead,
    /// Stopped by SIGSTOP, Ctrl+Z (SIGTSTP) and the like, until SIGCONT (or SIGKILL).
    Stopped,
    /// Stopped for its tracer (ptrace.rs), until the tracer lets it go (or SIGKILL).
    Traced,
}

impl State {
    fn alive(self) -> bool {
        matches!(self, State::Runnable | State::Sleeping | State::Stopped | State::Traced)
    }
}

/// What a process's parent hears through wait about it stopping and going on (job control).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobEvent {
    Stopped(u8),
    Continued,
}

/// What wait4 and waitid look for.
#[derive(Clone, Copy, Default)]
pub struct WaitFor {
    pub exited: bool,
    pub stopped: bool,
    pub continued: bool,
    /// Don't wait if there's nothing yet (WNOHANG).
    pub nohang: bool,
    /// Leave what's reported to be waited for again (WNOWAIT).
    pub keep: bool,
}

#[repr(C, align(16))]
#[derive(Clone)]
pub struct FpuState([u8; 512]);

/// What `rt_sigaction` stores per signal (the kernel's `struct sigaction` on x86-64).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

/// Something tasks share. See the module comment for why no lock is needed.
pub struct Shared<T>(UnsafeCell<T>);
unsafe impl<T> Sync for Shared<T> {}
unsafe impl<T> Send for Shared<T> {}

impl<T> Shared<T> {
    pub fn new(value: T) -> Arc<Self> {
        Arc::new(Shared(UnsafeCell::new(value)))
    }

    #[allow(clippy::mut_from_ref)]
    pub fn get(&self) -> &mut T {
        unsafe { &mut *self.0.get() }
    }
}

pub struct Memory {
    pub space: Option<AddressSpace>,
    pub brk_start: u64,
    pub brk: u64,
    pub mmap_next: u64,
}

pub struct Files {
    pub fds: FdTable,
    pub cwd: String,
    /// The root directory (chroot), as a path in the whole tree.
    pub root: String,
    pub umask: u32,
}

/// Who a process is. The real id is who started it; the effective one is what permissions are
/// checked against, and what a set-user-id program runs as. uid 0 is root, who may do anything.
#[derive(Clone, Copy, PartialEq)]
pub struct Credentials {
    pub uid: u32,
    pub gid: u32,
    pub euid: u32,
    pub egid: u32,
}

impl Credentials {
    pub const ROOT: Credentials = Credentials { uid: 0, gid: 0, euid: 0, egid: 0 };

    pub fn is_root(&self) -> bool {
        self.euid == 0
    }

    /// What a file's permission bits have to allow, given who owns it: the owner's digit, the
    /// group's, or everyone else's.
    pub fn allows(&self, mode: u32, owner: u32, group: u32, want: u32) -> bool {
        if self.is_root() {
            // root may read and write anything, but still needs a least one execute bit set.
            return want != 1 || mode & 0o111 != 0;
        }
        let bits = if self.euid == owner {
            mode >> 6
        } else if self.egid == group {
            mode >> 3
        } else {
            mode
        };
        bits & want == want
    }
}

pub type Handlers = [SigAction; 65];

pub struct Process {
    /// Thread id (equal to `tgid` for a process's first thread).
    pub pid: u32,
    /// Process id: the thread group.
    pub tgid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub sid: u32,
    pub state: State,
    /// Who it runs as.
    pub creds: Credentials,
    pub name: String,
    pub exe: String,
    /// The arguments it was started with, NUL-separated (/proc/<pid>/cmdline).
    pub cmdline: Vec<u8>,
    pub mem: Arc<Shared<Memory>>,
    pub files: Arc<Shared<Files>>,
    pub handlers: Arc<Shared<Handlers>>,
    pub fs_base: u64,
    /// The gs segment base. Linux programs rarely set it; Windows programs keep their TEB there.
    pub gs_base: u64,
    pub clear_child_tid: u64,
    pub sigmask: u64,
    /// Signals sent but not acted on yet (bit n-1 for signal n), and why each was sent.
    pub pending: u64,
    pub sig_info: [signal::Info; 65],
    /// The mask to restore after a handler started from rt_sigsuspend.
    pub saved_mask: Option<u64>,
    /// The faulting address for the signal being delivered (SIGSEGV etc.).
    pub fault_addr: Option<u64>,
    /// Set while a vfork parent waits for this child to exec or exit.
    pub vfork_parent: Option<u32>,
    /// For the first thread of a process: a stop or continue its parent hasn't waited for yet.
    pub job_event: Option<JobEvent>,
    /// personality(2)'s flags.
    pub personality: u32,
    /// The auxiliary vector its program started with.
    pub auxv: Vec<u8>,
    /// Its tracer, and how it's being traced (ptrace.rs).
    pub trace: Option<Box<ptrace::Trace>>,
    /// The terminal that controls it (its session's), if any: /dev/tty, and where Ctrl+C comes from.
    pub ctty: Option<Arc<crate::tty::Tty>>,
    /// The futex this task waits on, if any.
    pub futex: Option<futex::Key>,
    pub start_ticks: u64,
    fpu: Box<FpuState>,
    kstack: Box<[u8]>,
    ctx_rsp: u64,
}

impl Process {
    /// The saved floating point and SSE registers (FXSAVE's 512 bytes).
    pub fn fpu_state(&self) -> &[u8] {
        &self.fpu.0
    }

    pub fn set_fpu_state(&mut self, state: &[u8; 512]) {
        self.fpu.0.copy_from_slice(state);
    }

    pub fn space(&self) -> &AddressSpace {
        self.mem.get().space.as_ref().expect("process without memory")
    }

    pub fn space_mut(&mut self) -> &mut AddressSpace {
        self.mem.get().space.as_mut().expect("process without memory")
    }

    pub fn mem(&self) -> &mut Memory {
        self.mem.get()
    }

    pub fn files(&self) -> &mut Files {
        self.files.get()
    }

    pub fn handlers(&self) -> &mut Handlers {
        self.handlers.get()
    }

    fn kstack_top(&self) -> u64 {
        (self.kstack.as_ptr() as u64 + self.kstack.len() as u64) & !15
    }
}

struct Scheduler {
    procs: BTreeMap<u32, Box<Process>>,
    current: Option<u32>,
    sched_rsp: u64,
    next_pid: u32,
    last: u32,
}

struct Global(UnsafeCell<Scheduler>);
// Single CPU, and kernel code runs with interrupts off: see the module comment.
unsafe impl Sync for Global {}

static SCHED: Global = Global(UnsafeCell::new(Scheduler {
    procs: BTreeMap::new(),
    current: None,
    sched_rsp: 0,
    next_pid: 1,
    last: 0,
}));

fn sched() -> &'static mut Scheduler {
    unsafe { &mut *SCHED.0.get() }
}

static mut INITIAL_FPU: FpuState = FpuState([0; 512]);

pub fn init() {
    cpu::enable_sse();
    unsafe { asm!("fxsave [{}]", in(reg) &raw mut INITIAL_FPU) };
}

fn cmdline_of(args: &[String]) -> Vec<u8> {
    args.iter().flat_map(|a| a.bytes().chain(core::iter::once(0))).collect()
}

/// The running task's process id, if a task is running.
/// The running process's root directory, or "/" outside any process (at boot).
pub fn current_root() -> String {
    match sched().current.and_then(get) {
        Some(p) => p.files().root.clone(),
        None => String::from("/"),
    }
}

pub fn current_pid() -> Option<u32> {
    sched().current.and_then(get).map(|p| p.tgid)
}

/// The running task. Only valid while running on its behalf (in a system call or interrupt).
pub fn current() -> &'static mut Process {
    let s = sched();
    let pid = s.current.expect("no current process");
    s.procs.get_mut(&pid).expect("current process missing")
}

/// A task by thread id.
pub fn get(pid: u32) -> Option<&'static mut Process> {
    sched().procs.get_mut(&pid).map(|p| &mut **p).filter(|p| p.state != State::Dead)
}

/// Every task that hasn't finished.
pub fn all() -> impl Iterator<Item = &'static mut Process> {
    sched().procs.values_mut().map(|p| &mut **p).filter(|p| p.state != State::Dead)
}

/// One entry per process (its first thread), as /proc and kill() see them.
pub fn processes() -> impl Iterator<Item = &'static mut Process> {
    all().filter(|p| p.pid == p.tgid)
}

/// The tasks of process `tgid`.
pub fn threads(tgid: u32) -> impl Iterator<Item = &'static mut Process> {
    all().filter(move |p| p.tgid == tgid)
}

global_asm!(
    ".global switch_context",
    "switch_context:", // (save: *mut u64, next: u64)
    "push rbp", "push rbx", "push r12", "push r13", "push r14", "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop rbx", "pop rbp",
    "ret",
);

unsafe extern "C" {
    fn switch_context(save: *mut u64, next: u64);
}

fn new_kernel_stack() -> Box<[u8]> {
    let layout = core::alloc::Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
    unsafe {
        let ptr = alloc::alloc::alloc(layout);
        if ptr.is_null() {
            alloc::alloc::handle_alloc_error(layout);
        }
        Box::from_raw(core::ptr::slice_from_raw_parts_mut(ptr, KERNEL_STACK_SIZE))
    }
}

/// Prepares a kernel stack so the first switch to it returns to the program described by `frame`.
fn prime_stack(p: &mut Process, frame: TrapFrame) {
    let top = p.kstack_top();
    let frame_at = top - size_of::<TrapFrame>() as u64;
    unsafe {
        (frame_at as *mut TrapFrame).write(frame);
        let words = (frame_at - 7 * 8) as *mut u64;
        for i in 0..6 {
            words.add(i).write(0); // r15, r14, r13, r12, rbx, rbp
        }
        words.add(6).write(idt::trap_return as *const () as u64);
        p.ctx_rsp = words as u64;
    }
}

fn new_task(pid: u32, mem: Arc<Shared<Memory>>, files: Arc<Shared<Files>>, handlers: Arc<Shared<Handlers>>) -> Box<Process> {
    Box::new(Process {
        pid,
        tgid: pid,
        creds: Credentials::ROOT,
        ppid: 0,
        pgid: pid,
        sid: pid,
        state: State::Runnable,
        name: String::new(),
        exe: String::new(),
        cmdline: Vec::new(),
        mem,
        files,
        handlers,
        fs_base: 0,
        gs_base: 0,
        clear_child_tid: 0,
        sigmask: 0,
        pending: 0,
        sig_info: [signal::Info::default(); 65],
        saved_mask: None,
        fault_addr: None,
        vfork_parent: None,
        job_event: None,
        auxv: Vec::new(),
        personality: 0,
        trace: None,
        ctty: None,
        futex: None,
        start_ticks: crate::arch::irq::ticks(),
        fpu: Box::new(unsafe { (*(&raw const INITIAL_FPU)).clone() }),
        kstack: new_kernel_stack(),
        ctx_rsp: 0,
    })
}

fn memory_of(image: &mut Option<elf::Image>) -> Arc<Shared<Memory>> {
    let image = image.take().unwrap();
    Shared::new(Memory { space: Some(image.space), brk_start: image.brk, brk: image.brk, mmap_next: MMAP_TOP })
}

/// Starts the first program (pid 1) with the terminal as stdin, stdout and stderr.
pub fn spawn_init(path: &str) -> Result<(), Errno> {
    let s = sched();
    let node = crate::fs::lookup("/dev/console", true)?;
    let console = fd::OpenFile::new(fd::Handle::Tty(crate::tty::console(), node), 2, String::from("/dev/console"));
    let mut fds = FdTable::default();
    for _ in 0..3 {
        fds.insert(console.clone(), false, 0)?;
    }
    let args = alloc::vec![String::from(path)];
    let env = alloc::vec![String::from("PATH=/bin:/usr/bin"), String::from("HOME=/root"), String::from("TERM=linux")];
    let mut image = Some(elf::load(path, &args, &env)?);
    let (entry, stack) = image.as_ref().map(|i| (i.entry, i.stack)).unwrap();
    let auxv = image.as_mut().map(|i| core::mem::take(&mut i.auxv)).unwrap_or_default();
    let files = Shared::new(Files { fds, cwd: String::from("/"), root: String::from("/"), umask: 0o022 });
    let mut p = new_task(1, memory_of(&mut image), files, Shared::new([SigAction::default(); 65]));
    s.next_pid = 2;
    p.name = elf::short_name(path);
    p.exe = String::from(path);
    p.cmdline = cmdline_of(&args);
    p.auxv = auxv;
    p.ctty = Some(crate::tty::console());
    prime_stack(&mut p, TrapFrame::user(entry, stack));
    s.procs.insert(1, p);
    Ok(())
}

/// The scheduler loop. Runs on the boot stack forever.
pub fn run() -> ! {
    let s = sched();
    loop {
        let next = {
            let after = s.procs.range(s.last + 1..).find(|(_, p)| p.state == State::Runnable).map(|(&pid, _)| pid);
            after.or_else(|| s.procs.iter().find(|(_, p)| p.state == State::Runnable).map(|(&pid, _)| pid))
        };
        let Some(pid) = next else {
            cpu::wait_for_interrupt();
            continue;
        };
        s.last = pid;
        s.current = Some(pid);
        let p = s.procs.get_mut(&pid).unwrap();
        if let Some(space) = &p.mem.get().space {
            space.activate();
        }
        let top = p.kstack_top();
        gdt::set_kernel_stack(top);
        unsafe {
            idt::KERNEL_STACK_TOP = top;
            cpu::wrmsr(cpu::MSR_FS_BASE, p.fs_base);
            cpu::wrmsr(cpu::MSR_GS_BASE, p.gs_base);
            asm!("fxrstor [{}]", in(reg) &*p.fpu);
        }
        let ctx = p.ctx_rsp;
        unsafe { switch_context(&mut s.sched_rsp, ctx) };
        s.current = None;
        // A finished thread can only be freed once we're off its kernel stack, which is now.
        if s.procs.get(&pid).is_some_and(|p| p.state == State::Dead) {
            s.procs.remove(&pid);
        }
    }
}

/// Gives the CPU back to the scheduler; returns when this task is picked again.
pub fn yield_now() {
    let s = sched();
    let p = current();
    unsafe {
        asm!("fxsave [{}]", in(reg) &mut *p.fpu);
        switch_context(&mut p.ctx_rsp, s.sched_rsp);
    }
}

/// Waits until something happens (input, a child exiting, a timer tick). Callers check their
/// condition again afterwards. Fails with EINTR when a signal needs acting on.
pub fn sleep() -> Result<(), Errno> {
    let p = current();
    if signal::interrupted(p) {
        return Err(EINTR);
    }
    p.state = State::Sleeping;
    yield_now();
    ptrace::interrupted_while_waiting();
    if signal::interrupted(current()) { Err(EINTR) } else { Ok(()) }
}

/// Counts the times something happened that could make a waiter ready (every wake_all), so
/// edge-triggered epoll can tell whether anything has happened since it last reported.
static CHANGES: AtomicU64 = AtomicU64::new(0);

pub fn changes() -> u64 {
    CHANGES.load(Ordering::Relaxed)
}

/// Something changed: everyone waiting looks again.
pub fn wake_all() {
    CHANGES.fetch_add(1, Ordering::Relaxed);
    wake_sleepers();
}

fn wake_sleepers() {
    for p in sched().procs.values_mut() {
        if p.state == State::Sleeping {
            p.state = State::Runnable;
        }
    }
}

pub fn timer_tick(frame: &TrapFrame) {
    // Time passing wakes sleepers (for timeouts) but isn't a change in anything they watch.
    wake_sleepers();
    if frame.from_user() && sched().current.is_some() {
        yield_now();
    }
}

/// Clears the thread id word the C library asked for (CLONE_CHILD_CLEARTID / set_tid_address) and
/// wakes whoever waits on it: that's how pthread_join learns a thread has finished.
fn clear_tid(p: &mut Process) {
    if p.clear_child_tid != 0 && p.mem.get().space.is_some() {
        let _ = crate::mm::user::write(p.space(), p.clear_child_tid, &0u32.to_le_bytes());
        futex::wake(futex::key(p, p.clear_child_tid), 1);
        p.clear_child_tid = 0;
    }
}

/// exit(): ends the current thread; the process ends with its last thread.
pub fn exit_thread(status: i32) -> ! {
    ptrace::before_exit(status);
    let p = current();
    clear_tid(p);
    let others = threads(p.tgid).any(|t| t.pid != p.pid && t.state.alive());
    if !others {
        finish_process(status);
    }
    p.state = if p.pid == p.tgid { State::Lingering } else { State::Dead };
    p.vfork_parent = None;
    wake_all();
    yield_now();
    unreachable!("a finished thread was scheduled");
}

/// exit_group(), or a fatal signal: ends every thread of the current process.
pub fn exit(status: i32) -> ! {
    ptrace::before_exit(status);
    let p = current();
    let (me, tgid) = (p.pid, p.tgid);
    // The other threads are parked in the kernel (one CPU), so they can simply be dropped.
    let others: Vec<u32> = threads(tgid).filter(|t| t.pid != me && t.pid != tgid).map(|t| t.pid).collect();
    for pid in others {
        sched().procs.remove(&pid);
    }
    if let Some(leader) = get(tgid) {
        if leader.pid != me && leader.state.alive() {
            leader.state = State::Lingering;
        }
    }
    clear_tid(p);
    finish_process(status);
}

/// The last thread of a process is ending: release what it holds and leave a zombie for the parent.
fn finish_process(status: i32) -> ! {
    let p = current();
    if p.tgid == 1 {
        panic!("init exited (status {status:#x})");
    }
    // Drop this task's hold on memory and files. Shared with no one else, they're freed now; a vfork
    // parent keeps the memory it lent.
    p.files = Shared::new(Files { fds: FdTable::default(), cwd: String::from("/"), root: String::from("/"), umask: 0o022 });
    p.mem = Shared::new(Memory { space: None, brk_start: 0, brk: 0, mmap_next: MMAP_TOP });
    p.vfork_parent = None;
    let tgid = p.tgid;
    timer::remove(tgid);
    ptrace::tracer_gone(tgid);
    for child in all() {
        if child.ppid == tgid {
            child.ppid = 1;
        }
    }
    let ppid = p.ppid;
    if p.pid == tgid {
        p.state = State::Zombie(status);
    } else {
        p.state = State::Dead;
        if let Some(leader) = sched().procs.get_mut(&tgid) {
            leader.state = State::Zombie(status);
        }
    }
    let uid = p.creds.uid;
    let info = signal::Info::child_ended(tgid, uid, status);
    if let Some(parent) = get(ppid) {
        signal::send_info(parent, signal::SIGCHLD, info);
    }
    // A tracer that isn't the parent hears about it too.
    let tracer = get(tgid).and_then(|leader| leader.trace.as_ref().map(|t| t.tracer)).filter(|&t| t != ppid);
    if let Some(tracer) = tracer.and_then(get) {
        signal::send_info(tracer, signal::SIGCHLD, info);
    }
    wake_all();
    yield_now();
    unreachable!("a zombie was scheduled");
}

pub fn fork(frame: &TrapFrame, flags: u64, new_sp: u64, parent_tid: u64, child_tid: u64, tls: u64) -> Result<u32, Errno> {
    const CLONE_VM: u64 = 0x100;
    const CLONE_FILES: u64 = 0x400;
    const CLONE_SIGHAND: u64 = 0x800;
    const CLONE_VFORK: u64 = 0x4000;
    const CLONE_PARENT: u64 = 0x8000;
    const CLONE_THREAD: u64 = 0x10000;
    const CLONE_SETTLS: u64 = 0x80000;
    const CLONE_PARENT_SETTID: u64 = 0x100000;
    const CLONE_CHILD_CLEARTID: u64 = 0x200000;
    const CLONE_CHILD_SETTID: u64 = 0x1000000;

    // Linux's rules: threads share handlers, and sharing handlers needs shared memory.
    if (flags & CLONE_THREAD != 0 && flags & CLONE_SIGHAND == 0) || (flags & CLONE_SIGHAND != 0 && flags & CLONE_VM == 0) {
        return Err(EINVAL);
    }

    let s = sched();
    let parent = current();
    let mem = if flags & CLONE_VM != 0 {
        parent.mem.clone()
    } else {
        let m = parent.mem();
        let space = parent.space().duplicate().ok_or(ENOMEM)?;
        Shared::new(Memory { space: Some(space), brk_start: m.brk_start, brk: m.brk, mmap_next: m.mmap_next })
    };
    let files = if flags & CLONE_FILES != 0 {
        parent.files.clone()
    } else {
        let f = parent.files();
        Shared::new(Files { fds: f.fds.clone(), cwd: f.cwd.clone(), root: f.root.clone(), umask: f.umask })
    };
    let handlers = if flags & CLONE_SIGHAND != 0 { parent.handlers.clone() } else { Shared::new(*parent.handlers()) };

    let pid = s.next_pid;
    s.next_pid += 1;
    let mut child = new_task(pid, mem, files, handlers);
    let thread = flags & CLONE_THREAD != 0;
    child.creds = parent.creds;
    child.tgid = if thread { parent.tgid } else { pid };
    child.ppid = if thread || flags & CLONE_PARENT != 0 { parent.ppid } else { parent.tgid };
    child.pgid = parent.pgid;
    child.sid = parent.sid;
    child.ctty = parent.ctty.clone();
    child.name = parent.name.clone();
    child.exe = parent.exe.clone();
    child.cmdline = parent.cmdline.clone();
    child.auxv = parent.auxv.clone();
    child.personality = parent.personality;
    child.fs_base = if flags & CLONE_SETTLS != 0 { tls } else { parent.fs_base };
    child.gs_base = parent.gs_base;
    child.sigmask = parent.sigmask;
    unsafe { asm!("fxsave [{}]", in(reg) &mut *child.fpu) };

    if flags & CLONE_CHILD_SETTID != 0 {
        crate::mm::user::write(child.space(), child_tid, &pid.to_le_bytes())?;
    }
    if flags & CLONE_PARENT_SETTID != 0 {
        crate::mm::user::write(parent.space(), parent_tid, &pid.to_le_bytes())?;
    }
    if flags & CLONE_CHILD_CLEARTID != 0 {
        child.clear_child_tid = child_tid;
    }

    let mut child_frame = *frame;
    child_frame.rax = 0;
    if new_sp != 0 {
        child_frame.rsp = new_sp;
    }
    prime_stack(&mut child, child_frame);
    let vfork = flags & CLONE_VFORK != 0;
    if vfork {
        child.vfork_parent = Some(parent.pid);
    }
    let frame_at = child.kstack_top() - size_of::<TrapFrame>() as u64;
    let traced = ptrace::on_fork(&mut child, flags, frame_at);
    s.procs.insert(pid, child);
    if let Some(event) = traced {
        ptrace::after_fork(event, pid);
    }

    // vfork: the parent waits until the child has exec'd or exited (it may be using our memory).
    if vfork {
        while get(pid).is_some_and(|c| c.vfork_parent.is_some() && c.state.alive()) {
            if sleep().is_err() {
                break;
            }
        }
    }
    Ok(pid)
}

/// Replaces the current program (execve). On success the system call "returns" into the new program.
pub fn exec(frame: &mut TrapFrame, path: &str, args: &[String], env: &[String]) -> Result<(), Errno> {
    // A set-user-id program runs as whoever owns it: that's how `su` reads /etc/shadow without
    // the person running it being able to.
    let grant = match crate::fs::lookup(path, true) {
        Ok(node) => {
            let mode = node.st_mode();
            let (uid, gid) = node.owner();
            (
                (mode & crate::fs::S_ISUID != 0).then_some(uid),
                (mode & crate::fs::S_ISGID != 0).then_some(gid),
            )
        }
        Err(_) => (None, None),
    };
    let mut image = Some(elf::load(path, args, env)?);
    let p = current();
    // Other threads end, as on Linux.
    let (me, tgid) = (p.pid, p.tgid);
    let others: Vec<u32> = threads(tgid).filter(|t| t.pid != me).map(|t| t.pid).collect();
    for pid in others {
        sched().procs.remove(&pid);
    }
    // A thread other than the first one takes over the process id, so the parent still finds it.
    // Moving the Box keeps the task (and the kernel stack we're on) where it is.
    let p = if me != tgid {
        let s = sched();
        let mut task = s.procs.remove(&me).unwrap();
        task.pid = tgid;
        s.procs.insert(tgid, task);
        s.current = Some(tgid);
        current()
    } else {
        p
    };
    let (entry, stack, exe) = image.as_ref().map(|i| (i.entry, i.stack, i.path.clone())).unwrap();
    p.auxv = image.as_mut().map(|i| core::mem::take(&mut i.auxv)).unwrap_or_default();
    image.as_ref().unwrap().space.activate();
    // New memory, our own copies of the files table and handlers (whoever shared them keeps theirs).
    p.mem = memory_of(&mut image);
    let f = p.files();
    let mut fds = f.fds.clone();
    fds.close_on_exec();
    p.files = Shared::new(Files { fds, cwd: f.cwd.clone(), root: f.root.clone(), umask: f.umask });
    let mut handlers = *p.handlers();
    for action in handlers.iter_mut() {
        if action.handler != signal::SIG_IGN {
            *action = SigAction::default();
        }
    }
    p.handlers = Shared::new(handlers);
    if let Some(uid) = grant.0 {
        p.creds.euid = uid;
    }
    if let Some(gid) = grant.1 {
        p.creds.egid = gid;
    }
    p.name = elf::short_name(args.first().map_or(path, |a| a.as_str()));
    p.exe = exe;
    p.cmdline = cmdline_of(args);
    p.fs_base = 0;
    p.gs_base = 0;
    unsafe { cpu::wrmsr(cpu::MSR_GS_BASE, 0) };
    p.clear_child_tid = 0;
    unsafe { cpu::wrmsr(cpu::MSR_FS_BASE, 0) };
    *p.fpu = unsafe { (*(&raw const INITIAL_FPU)).clone() };
    unsafe { asm!("fxrstor [{}]", in(reg) &*p.fpu) };
    p.vfork_parent = None;
    wake_all();
    *frame = TrapFrame::user(entry, stack);
    Ok(())
}

/// wait4: returns (pid, status) of an ended child, or None for WNOHANG with nothing ready.
/// Waits for a child to end (or, if asked, to stop or go on): (its pid, its wait status, the user
/// it ran as). The status is wait4's: exit code << 8 or the signal that ended it; a stop is
/// sig << 8 | 0x7f, going on is 0xffff. `target` is wait4's pid: -1 any child, 0 or -pgid one in a
/// group, or a pid.
pub fn wait(target: i64, want: WaitFor) -> Result<Option<(u32, i32, u32)>, Errno> {
    loop {
        let s = sched();
        let me = current();
        let (my_tgid, my_pgid) = (me.tgid, me.pgid);
        let chosen = |p: &Process| match target {
            -1 => true,
            0 => p.pgid == my_pgid,
            t if t > 0 => p.pid as i64 == t,
            t => p.pgid as i64 == -t,
        };
        // Its children, and the threads it traces (which may be anyone's).
        let mine = |p: &Process| p.ppid == my_tgid && p.pid == p.tgid && p.state != State::Dead && chosen(p);
        let traced = |p: &Process| p.trace.as_ref().is_some_and(|t| t.tracer == my_tgid) && p.state != State::Dead && chosen(p);
        if !s.procs.values().any(|p| mine(p) || traced(p)) {
            return Err(ECHILD);
        }
        // A tracee stopped for it, whether or not asked for stops.
        for p in s.procs.values_mut() {
            if traced(p) {
                if let Some(status) = ptrace::unreported_stop(p, my_tgid, want.keep) {
                    return Ok(Some((p.pid, status, p.creds.uid)));
                }
            }
        }
        let done = s.procs.values().find(|p| want.exited && mine(p) && matches!(p.state, State::Zombie(_))).map(|p| p.pid);
        if let Some(pid) = done {
            let child = s.procs.get(&pid).unwrap();
            let (State::Zombie(status), uid) = (child.state, child.creds.uid) else { unreachable!() };
            if !want.keep {
                s.procs.remove(&pid);
            }
            return Ok(Some((pid, status, uid)));
        }
        // A tracee of someone else's that ended: its tracer is told too (once); its parent reaps it.
        let ended = s.procs.values_mut().find(|p| {
            traced(p) && !mine(p) && matches!(p.state, State::Zombie(_)) && p.trace.as_ref().is_some_and(|t| !t.exit_reported)
        });
        if let Some(p) = ended {
            let State::Zombie(status) = p.state else { unreachable!() };
            if !want.keep {
                p.trace.as_mut().unwrap().exit_reported = true;
            }
            return Ok(Some((p.pid, status, p.creds.uid)));
        }
        let matches = mine;
        let event = |p: &Process| match p.job_event {
            Some(JobEvent::Stopped(sig)) if want.stopped => Some((sig as i32) << 8 | 0x7f),
            Some(JobEvent::Continued) if want.continued => Some(0xffff),
            _ => None,
        };
        let changed = s.procs.values_mut().find(|p| matches(p) && event(p).is_some());
        if let Some(child) = changed {
            let status = event(child).unwrap();
            if !want.keep {
                child.job_event = None;
            }
            return Ok(Some((child.pid, status, child.creds.uid)));
        }
        if want.nohang {
            return Ok(None);
        }
        sleep()?;
    }
}

/// Sends a signal to a process: to one of its threads that isn't blocking it (Linux picks any).
pub fn signal_process(tgid: u32, sig: usize) -> Result<(), Errno> {
    signal_process_info(tgid, sig, signal::Info::kernel())
}

pub fn signal_process_info(tgid: u32, sig: usize, info: signal::Info) -> Result<(), Errno> {
    let leader = get(tgid).filter(|p| p.pid == p.tgid).ok_or(ESRCH)?;
    if sig == 0 {
        return Ok(());
    }
    let target = threads(tgid).find(|t| t.state.alive() && t.sigmask & signal::bit(sig) == 0).map(|t| t.pid);
    match target.and_then(get) {
        Some(t) => signal::send_info(t, sig, info),
        None => signal::send_info(leader, sig, info),
    }
    Ok(())
}

pub fn signal_group(pgid: u32, sig: usize) {
    signal_group_info(pgid, sig, signal::Info::kernel());
}

pub fn signal_group_info(pgid: u32, sig: usize, info: signal::Info) {
    let tgids: Vec<u32> = processes().filter(|p| p.pgid == pgid).map(|p| p.tgid).collect();
    for tgid in tgids {
        let _ = signal_process_info(tgid, sig, info);
    }
}

/// An exception in a program: grow its stack, or deliver the matching signal.
pub fn user_fault(frame: &mut TrapFrame, name: &str) {
    let p = current();
    if frame.vector == 14 {
        let addr = cpu::read_cr2();
        let page = addr & !0xfff;
        let not_present = frame.error & 1 == 0;
        if not_present && page < STACK_TOP && page >= STACK_TOP - STACK_LIMIT {
            use crate::mm::paging::{NO_EXECUTE, USER, WRITABLE};
            if p.space_mut().map_zeroed(page, page + 4096, USER | WRITABLE | NO_EXECUTE) {
                return;
            }
        }
    }
    use signal::{SIGBUS, SIGFPE, SIGILL, SIGTRAP};
    let sig = match frame.vector {
        0 | 16 | 19 => SIGFPE,
        1 | 3 => SIGTRAP,
        6 => SIGILL,
        17 => SIGBUS,
        _ => SIGSEGV,
    };
    // (A traced program's tracer deals with it: a debugger's breakpoint isn't worth a log line.)
    let handled = p.trace.is_some() || (p.handlers()[sig].handler > signal::SIG_IGN && p.sigmask & signal::bit(sig) == 0);
    if !handled {
        crate::log!(
            "pid {} ({}): {name} at {:#x}, address {:#x}, error {:#x}",
            p.tgid,
            p.name,
            frame.rip,
            cpu::read_cr2(),
            frame.error
        );
    }
    let addr = if frame.vector == 14 { cpu::read_cr2() } else { frame.rip };
    signal::fault(frame, sig, addr);
}
