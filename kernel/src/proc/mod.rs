//! Processes and the scheduler.
//!
//! One CPU for now, and the kernel itself is never interrupted: interrupts are only enabled while a
//! program runs or while the CPU idles. So the process table needs no locks, only care not to keep a
//! reference across a switch. The scheduler loop runs on the boot stack; each process has its own
//! kernel stack, and switching is saving one stack pointer and loading another.

pub mod elf;
pub mod fd;

use crate::arch::idt::{self, TrapFrame};
use crate::arch::{cpu, gdt};
use crate::mm::paging::AddressSpace;
use crate::sys::errno::*;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use fd::FdTable;

pub const SIGINT: usize = 2;
pub const SIGQUIT: usize = 3;
pub const SIGILL: usize = 4;
pub const SIGTRAP: usize = 5;
pub const SIGBUS: usize = 7;
pub const SIGFPE: usize = 8;
pub const SIGKILL: usize = 9;
pub const SIGSEGV: usize = 11;
pub const SIGPIPE: usize = 13;
pub const SIGCHLD: usize = 17;
pub const SIGCONT: usize = 18;
pub const SIGSTOP: usize = 19;
pub const SIGURG: usize = 23;
pub const SIGWINCH: usize = 28;
pub const SIG_IGN: u64 = 1;

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
    Zombie(i32),
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

pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub sid: u32,
    pub state: State,
    pub name: String,
    pub exe: String,
    pub cwd: String,
    pub umask: u32,
    pub space: Option<AddressSpace>,
    pub fds: FdTable,
    pub brk_start: u64,
    pub brk: u64,
    pub mmap_next: u64,
    pub fs_base: u64,
    pub clear_child_tid: u64,
    pub sigactions: [SigAction; 65],
    pub sigmask: u64,
    pub pending_kill: Option<usize>,
    /// Set while a vfork parent waits for this child to exec or exit.
    pub vfork_parent: Option<u32>,
    pub start_ticks: u64,
    fpu: Box<FpuState>,
    kstack: Box<[u8]>,
    ctx_rsp: u64,
}

impl Process {
    pub fn space(&self) -> &AddressSpace {
        self.space.as_ref().expect("process without memory")
    }

    pub fn space_mut(&mut self) -> &mut AddressSpace {
        self.space.as_mut().expect("process without memory")
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

/// The running process. Only valid while running on its behalf (in a system call or interrupt).
pub fn current() -> &'static mut Process {
    let s = sched();
    let pid = s.current.expect("no current process");
    s.procs.get_mut(&pid).expect("current process missing")
}

pub fn get(pid: u32) -> Option<&'static mut Process> {
    sched().procs.get_mut(&pid).map(|p| &mut **p)
}

pub fn all() -> impl Iterator<Item = &'static mut Process> {
    sched().procs.values_mut().map(|p| &mut **p)
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

fn blank_process(pid: u32) -> Box<Process> {
    Box::new(Process {
        pid,
        ppid: 0,
        pgid: pid,
        sid: pid,
        state: State::Runnable,
        name: String::new(),
        exe: String::new(),
        cwd: String::from("/"),
        umask: 0o022,
        space: None,
        fds: FdTable::default(),
        brk_start: 0,
        brk: 0,
        mmap_next: MMAP_TOP,
        fs_base: 0,
        clear_child_tid: 0,
        sigactions: [SigAction::default(); 65],
        sigmask: 0,
        pending_kill: None,
        vfork_parent: None,
        start_ticks: crate::arch::irq::ticks(),
        fpu: Box::new(unsafe { (*(&raw const INITIAL_FPU)).clone() }),
        kstack: new_kernel_stack(),
        ctx_rsp: 0,
    })
}

/// Starts the first program (pid 1) with the terminal as stdin, stdout and stderr.
pub fn spawn_init(path: &str) -> Result<(), Errno> {
    let s = sched();
    let mut p = blank_process(1);
    s.next_pid = 2;
    let tty = crate::fs::lookup("/dev/console", true)?;
    let console = fd::OpenFile::node(tty, 2, String::from("/dev/console"));
    for _ in 0..3 {
        p.fds.insert(console.clone(), false, 0)?;
    }
    let args = alloc::vec![String::from(path)];
    let env = alloc::vec![String::from("PATH=/bin:/usr/bin"), String::from("HOME=/root"), String::from("TERM=linux")];
    let image = elf::load(path, &args, &env)?;
    p.name = elf::short_name(path);
    p.exe = String::from(path);
    p.brk_start = image.brk;
    p.brk = image.brk;
    p.space = Some(image.space);
    prime_stack(&mut p, TrapFrame::user(image.entry, image.stack));
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
        if let Some(space) = &p.space {
            space.activate();
        }
        let top = p.kstack_top();
        gdt::set_kernel_stack(top);
        unsafe {
            idt::KERNEL_STACK_TOP = top;
            cpu::wrmsr(cpu::MSR_FS_BASE, p.fs_base);
            asm!("fxrstor [{}]", in(reg) &*p.fpu);
        }
        let ctx = p.ctx_rsp;
        unsafe { switch_context(&mut s.sched_rsp, ctx) };
        s.current = None;
    }
}

/// Gives the CPU back to the scheduler; returns when this process is picked again.
pub fn yield_now() {
    let s = sched();
    let p = current();
    unsafe {
        asm!("fxsave [{}]", in(reg) &mut *p.fpu);
        switch_context(&mut p.ctx_rsp, s.sched_rsp);
    }
}

/// Waits until something happens (input, a child exiting, a timer tick). Callers check their
/// condition again afterwards. Fails with EINTR when the process is being killed.
pub fn sleep() -> Result<(), Errno> {
    let p = current();
    if p.pending_kill.is_some() {
        return Err(EINTR);
    }
    p.state = State::Sleeping;
    yield_now();
    if current().pending_kill.is_some() { Err(EINTR) } else { Ok(()) }
}

pub fn wake_all() {
    for p in sched().procs.values_mut() {
        if p.state == State::Sleeping {
            p.state = State::Runnable;
        }
    }
}

pub fn timer_tick(frame: &TrapFrame) {
    wake_all();
    if frame.from_user() && sched().current.is_some() {
        check_kill();
        yield_now();
    }
}

/// Ends the current process if a signal killed it.
pub fn check_kill() {
    if let Some(sig) = current().pending_kill {
        exit(sig as i32);
    }
}

/// Ends the current process. `status` is a wait status: (code << 8) for exit(code), or a signal number.
pub fn exit(status: i32) -> ! {
    let s = sched();
    let p = current();
    if p.pid == 1 {
        panic!("init exited (status {status:#x})");
    }
    p.fds.clear();
    p.space = None; // frees its memory (and switches to the kernel's page tables)
    p.state = State::Zombie(status);
    let pid = p.pid;
    for child in s.procs.values_mut() {
        if child.ppid == pid {
            child.ppid = 1;
        }
    }
    release_vfork(p);
    wake_all();
    yield_now();
    unreachable!("a zombie was scheduled");
}

fn release_vfork(p: &mut Process) {
    p.vfork_parent = None;
}

pub fn fork(frame: &TrapFrame, flags: u64, new_sp: u64, parent_tid: u64, child_tid: u64, tls: u64) -> Result<u32, Errno> {
    const CLONE_VM: u64 = 0x100;
    const CLONE_VFORK: u64 = 0x4000;
    const CLONE_THREAD: u64 = 0x10000;
    const CLONE_SETTLS: u64 = 0x80000;
    const CLONE_PARENT_SETTID: u64 = 0x100000;
    const CLONE_CHILD_CLEARTID: u64 = 0x200000;
    const CLONE_CHILD_SETTID: u64 = 0x1000000;

    // Threads share memory with their parent; they come later. vfork (CLONE_VM | CLONE_VFORK) is fine
    // as a copy, since the parent waits until the child has exec'd.
    if flags & CLONE_THREAD != 0 || (flags & CLONE_VM != 0 && flags & CLONE_VFORK == 0) {
        return Err(ENOSYS);
    }

    let s = sched();
    let parent = current();
    let space = parent.space().duplicate().ok_or(ENOMEM)?;
    let pid = s.next_pid;
    s.next_pid += 1;

    let mut child = blank_process(pid);
    child.ppid = parent.pid;
    child.pgid = parent.pgid;
    child.sid = parent.sid;
    child.name = parent.name.clone();
    child.exe = parent.exe.clone();
    child.cwd = parent.cwd.clone();
    child.umask = parent.umask;
    child.fds = parent.fds.clone();
    child.brk_start = parent.brk_start;
    child.brk = parent.brk;
    child.mmap_next = parent.mmap_next;
    child.fs_base = if flags & CLONE_SETTLS != 0 { tls } else { parent.fs_base };
    child.sigactions = parent.sigactions;
    child.sigmask = parent.sigmask;
    unsafe { asm!("fxsave [{}]", in(reg) &mut *child.fpu) };
    child.space = Some(space);

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
    s.procs.insert(pid, child);

    if vfork {
        while get(pid).is_some_and(|c| c.vfork_parent.is_some() && !matches!(c.state, State::Zombie(_))) {
            if sleep().is_err() {
                break;
            }
        }
    }
    Ok(pid)
}

/// Replaces the current program (execve). On success the system call "returns" into the new program.
pub fn exec(frame: &mut TrapFrame, path: &str, args: &[String], env: &[String]) -> Result<(), Errno> {
    let image = elf::load(path, args, env)?;
    let p = current();
    image.space.activate();
    p.space = Some(image.space);
    p.name = elf::short_name(args.first().map_or(path, |a| a.as_str()));
    p.exe = image.path;
    p.brk_start = image.brk;
    p.brk = image.brk;
    p.mmap_next = MMAP_TOP;
    p.fs_base = 0;
    p.clear_child_tid = 0;
    unsafe { cpu::wrmsr(cpu::MSR_FS_BASE, 0) };
    p.fds.close_on_exec();
    for action in p.sigactions.iter_mut() {
        if action.handler != SIG_IGN {
            *action = SigAction::default();
        }
    }
    *p.fpu = unsafe { (*(&raw const INITIAL_FPU)).clone() };
    unsafe { asm!("fxrstor [{}]", in(reg) &*p.fpu) };
    release_vfork(p);
    wake_all();
    *frame = TrapFrame::user(image.entry, image.stack);
    Ok(())
}

/// wait4: returns (pid, status) of an ended child, or None for WNOHANG with nothing ready.
pub fn wait(target: i64, nohang: bool) -> Result<Option<(u32, i32)>, Errno> {
    loop {
        let s = sched();
        let me = current();
        let matches = |p: &Process| {
            p.ppid == me.pid
                && match target {
                    -1 => true,
                    0 => p.pgid == me.pgid,
                    t if t > 0 => p.pid as i64 == t,
                    t => p.pgid as i64 == -t,
                }
        };
        if !s.procs.values().any(|p| matches(p)) {
            return Err(ECHILD);
        }
        let done = s.procs.values().find(|p| matches(p) && matches!(p.state, State::Zombie(_))).map(|p| p.pid);
        if let Some(pid) = done {
            let child = s.procs.remove(&pid).unwrap();
            let State::Zombie(status) = child.state else { unreachable!() };
            return Ok(Some((pid, status)));
        }
        if nohang {
            return Ok(None);
        }
        sleep()?;
    }
}

/// Default action of a signal: true when it ends the process.
fn kills_by_default(sig: usize) -> bool {
    !matches!(sig, SIGCHLD | SIGCONT | SIGURG | SIGWINCH | 0)
}

/// Sends a signal. Handlers aren't run yet: a handled signal is treated like the default action
/// (end the process), an ignored one does nothing.
pub fn signal(p: &mut Process, sig: usize) {
    if sig == 0 || sig > 64 || matches!(p.state, State::Zombie(_)) {
        return;
    }
    if p.pid == 1 && sig != SIGKILL {
        return;
    }
    let ignored = sig != SIGKILL && sig != SIGSTOP && p.sigactions[sig].handler == SIG_IGN;
    if ignored || !kills_by_default(sig) {
        return;
    }
    p.pending_kill.get_or_insert(sig);
    if p.state == State::Sleeping {
        p.state = State::Runnable;
    }
}

pub fn signal_group(pgid: u32, sig: usize) {
    for p in sched().procs.values_mut() {
        if p.pgid == pgid {
            signal(p, sig);
        }
    }
}

/// An exception in a program: grow its stack, or end it with the matching signal.
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
    let sig = match frame.vector {
        0 | 16 | 19 => SIGFPE,
        3 => SIGTRAP,
        6 => SIGILL,
        17 => SIGBUS,
        _ => SIGSEGV,
    };
    crate::log!(
        "pid {} ({}): {name} at {:#x}, address {:#x}, error {:#x}",
        p.pid,
        p.name,
        frame.rip,
        cpu::read_cr2(),
        frame.error
    );
    exit(sig as i32);
}
