//! Signals, delivered the way Linux does on x86-64.
//!
//! Sending marks a signal pending. Whenever a process is about to return to its program (after a
//! system call, an interrupt or an exception) one pending, unblocked signal is acted on: ignored,
//! the process ended, or its handler started. Starting a handler pushes Linux's `rt_sigframe` (the
//! saved registers, signal mask, FPU state and a siginfo) onto the program's stack and jumps to the
//! handler; when the handler returns into the C library's restorer, that calls `rt_sigreturn`, which
//! reads the frame back. Because the layout is Linux's, glibc and musl programs work unchanged.

use super::{Process, SigAction, State, current, exit};
use crate::arch::idt::{SYSCALL_VECTOR, TrapFrame};
use crate::arch::gdt;
use crate::mm::paging::{NO_EXECUTE, USER, WRITABLE};
use crate::mm::user;
use crate::sys::errno::*;
use core::arch::asm;

pub const SIGHUP: usize = 1;
pub const SIGINT: usize = 2;
pub const SIGQUIT: usize = 3;
pub const SIGILL: usize = 4;
pub const SIGTRAP: usize = 5;
pub const SIGBUS: usize = 7;
pub const SIGFPE: usize = 8;
pub const SIGKILL: usize = 9;
pub const SIGSEGV: usize = 11;
pub const SIGPIPE: usize = 13;
pub const SIGALRM: usize = 14;
pub const SIGCHLD: usize = 17;
pub const SIGCONT: usize = 18;
pub const SIGSTOP: usize = 19;
pub const SIGTSTP: usize = 20;
pub const SIGTTIN: usize = 21;
pub const SIGTTOU: usize = 22;
pub const SIGURG: usize = 23;
pub const SIGWINCH: usize = 28;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

const SA_RESTORER: u64 = 0x0400_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;

/// Why a signal was sent, as a program sees it in its siginfo: the reason (si_code), who sent it,
/// and for SIGCHLD the child's exit status. Like Linux, one is kept for each pending signal.
#[derive(Clone, Copy, Default)]
pub struct Info {
    pub code: i32,
    pub pid: u32,
    pub uid: u32,
    pub status: i32,
}

pub const SI_USER: i32 = 0;
pub const SI_KERNEL: i32 = 0x80;
pub const SI_TKILL: i32 = -6;
const CLD_EXITED: i32 = 1;
const CLD_KILLED: i32 = 2;

impl Info {
    /// From the kernel itself (a terminal's Ctrl+C, SIGPIPE, timers).
    pub fn kernel() -> Info {
        Info { code: SI_KERNEL, ..Info::default() }
    }

    /// From the calling process (kill, tkill).
    pub fn from_caller(code: i32) -> Info {
        let p = current();
        Info { code, pid: p.tgid, uid: p.creds.uid, status: 0 }
    }

    /// SIGCHLD for a child that ended, from its wait status (exit code << 8, or the signal).
    pub fn child_ended(pid: u32, uid: u32, wait_status: i32) -> Info {
        let (code, status) = if wait_status & 0x7f == 0 { (CLD_EXITED, (wait_status >> 8) & 0xff) } else { (CLD_KILLED, wait_status & 0x7f) };
        Info { code, pid, uid, status }
    }

    /// A `siginfo_t`: number, errno, code, then the sender's pid and uid and, for SIGCHLD, the status.
    pub fn siginfo(&self, sig: usize) -> [u8; 128] {
        let mut b = [0u8; 128];
        b[0..4].copy_from_slice(&(sig as i32).to_le_bytes());
        b[8..12].copy_from_slice(&self.code.to_le_bytes());
        b[16..20].copy_from_slice(&self.pid.to_le_bytes());
        b[20..24].copy_from_slice(&self.uid.to_le_bytes());
        b[24..28].copy_from_slice(&self.status.to_le_bytes());
        b
    }
}

/// Takes a pending signal off: it's acted on, waited for, or read from a signalfd.
pub fn take(p: &mut Process, sig: usize) -> Info {
    p.pending &= !bit(sig);
    core::mem::take(&mut p.sig_info[sig])
}

pub const fn bit(sig: usize) -> u64 {
    1 << (sig - 1)
}

const UNBLOCKABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

/// Signals whose default action is to do nothing. Stop signals are in here too until there is job
/// control.
fn ignored_by_default(sig: usize) -> bool {
    matches!(sig, SIGCHLD | SIGCONT | SIGURG | SIGWINCH | SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU)
}

/// Pending signals the process isn't blocking.
fn ready(p: &Process) -> u64 {
    p.pending & !(p.sigmask & !UNBLOCKABLE)
}

/// Whether a sleeping system call should give up with EINTR so a signal can be acted on.
pub fn interrupted(p: &Process) -> bool {
    ready(p) != 0
}

pub fn send(p: &mut Process, sig: usize) {
    send_info(p, sig, Info::kernel());
}

pub fn send_info(p: &mut Process, sig: usize, info: Info) {
    if sig == 0 || sig > 64 || matches!(p.state, State::Zombie(_)) {
        return;
    }
    let handler = p.handlers()[sig].handler;
    // init only gets the signals it has handlers for, like on Linux.
    if p.tgid == 1 && handler == SIG_DFL {
        return;
    }
    // An ignored signal is dropped, unless it's blocked: the program may change its mind by the
    // time it unblocks it, or be waiting for it in sigwait (Linux keeps it too).
    let blocked = p.sigmask & bit(sig) != 0;
    if sig != SIGKILL && !blocked && (handler == SIG_IGN || (handler == SIG_DFL && ignored_by_default(sig))) {
        return;
    }
    // An ordinary signal already pending isn't queued again; the first one's details stay.
    if p.pending & bit(sig) == 0 {
        p.sig_info[sig] = info;
    }
    p.pending |= bit(sig);
    // Wake it either way: a sigwait waits for blocked signals too.
    if p.state == State::Sleeping {
        p.state = State::Runnable;
    }
}

/// Acts on pending signals before returning to the program described by `frame`.
pub fn deliver(frame: &mut TrapFrame) {
    loop {
        let p = current();
        let ready = ready(p);
        if ready == 0 {
            return;
        }
        let sig = ready.trailing_zeros() as usize + 1;
        let info = take(p, sig);
        let action = p.handlers()[sig];
        match action.handler {
            SIG_IGN => continue,
            SIG_DFL if ignored_by_default(sig) => continue,
            SIG_DFL => exit(sig as i32),
            _ => {
                if start_handler(p, frame, sig, info, action).is_err() {
                    exit(SIGSEGV as i32);
                }
                return;
            }
        }
    }
}

/// A fault in the program (bad memory access, invalid instruction, ...): run its handler if it has
/// one and isn't blocking the signal, otherwise end it, as Linux does.
pub fn fault(frame: &mut TrapFrame, sig: usize, addr: u64) {
    let p = current();
    let handler = p.handlers()[sig].handler;
    if handler == SIG_DFL || handler == SIG_IGN || p.sigmask & bit(sig) != 0 {
        exit(sig as i32);
    }
    p.fault_addr = Some(addr);
    p.pending |= bit(sig);
    deliver(frame);
}

// Offsets in Linux's x86-64 `struct rt_sigframe`.
const UC: usize = 8; // after the return address (the restorer)
const MCONTEXT: usize = UC + 40; // after uc_flags, uc_link, uc_stack
const SIGMASK: usize = MCONTEXT + 256;
const SIGINFO: usize = SIGMASK + 8;
const FRAME_SIZE: usize = SIGINFO + 128;

/// The registers in `struct sigcontext` order.
fn registers(frame: &TrapFrame) -> [u64; 18] {
    [
        frame.r8, frame.r9, frame.r10, frame.r11, frame.r12, frame.r13, frame.r14, frame.r15, frame.rdi, frame.rsi,
        frame.rbp, frame.rbx, frame.rdx, frame.rax, frame.rcx, frame.rsp, frame.rip, frame.rflags,
    ]
}

fn start_handler(p: &mut Process, frame: &mut TrapFrame, sig: usize, info: Info, action: SigAction) -> Result<(), Errno> {
    if action.flags & SA_RESTORER == 0 || action.restorer == 0 {
        return Err(EFAULT); // x86-64 programs always provide one (the C library does)
    }
    // A system call cut short by this signal: restart it after the handler, or report EINTR.
    if frame.vector == SYSCALL_VECTOR && frame.rax as i64 == -EINTR && action.flags & SA_RESTART != 0 {
        frame.rax = frame.error; // the system call number, saved on entry
        frame.rip -= 2; // back onto the `syscall` instruction
    }

    let old_mask = p.saved_mask.take().unwrap_or(p.sigmask);
    let mut fpu = super::FpuState([0; 512]);
    unsafe { asm!("fxsave [{}]", in(reg) &mut fpu) };

    // Below the red zone: the FPU state (64-byte aligned), then the frame, placed so the handler
    // starts with the stack aligned as if it had been called.
    let fpu_at = (frame.rsp - 128 - 512) & !63;
    let base = ((fpu_at - FRAME_SIZE as u64) & !15) - 8;
    let stack_low = super::STACK_TOP - super::STACK_LIMIT;
    if base >= stack_low && base < super::STACK_TOP {
        p.space_mut().map_zeroed(base & !0xfff, fpu_at + 512, USER | WRITABLE | NO_EXECUTE);
    }

    let mut f = [0u8; FRAME_SIZE];
    let mut put = |at: usize, v: u64| f[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put(0, action.restorer);
    put(UC + 16 + 8, 2); // uc_stack.ss_flags = SS_DISABLE
    for (i, r) in registers(frame).iter().enumerate() {
        put(MCONTEXT + i * 8, *r);
    }
    put(MCONTEXT + 144, frame.cs | (frame.ss << 48)); // cs, gs, fs, ss as four u16s
    put(MCONTEXT + 152, if frame.vector < 32 { frame.error } else { 0 });
    put(MCONTEXT + 160, if frame.vector < 32 { frame.vector } else { 0 });
    put(MCONTEXT + 168, old_mask);
    put(MCONTEXT + 176, p.fault_addr.unwrap_or(0)); // cr2
    put(MCONTEXT + 184, fpu_at);
    put(SIGMASK, old_mask);
    // siginfo: for a fault, SEGV_MAPERR-style code 1 and the address; otherwise why it was sent.
    match p.fault_addr.take() {
        Some(addr) => {
            put(SIGINFO, sig as u64);
            put(SIGINFO + 8, 1);
            put(SIGINFO + 16, addr);
        }
        None => f[SIGINFO..SIGINFO + 128].copy_from_slice(&info.siginfo(sig)),
    }

    let space = p.space();
    user::write(space, fpu_at, &fpu.0)?;
    user::write(space, base, &f)?;

    frame.rsp = base;
    frame.rip = action.handler;
    frame.rdi = sig as u64;
    frame.rsi = base + SIGINFO as u64;
    frame.rdx = base + UC as u64;
    frame.rax = 0;
    frame.rflags &= !(1 << 10 | 1 << 8); // clear DF and TF for the handler
    frame.cs = gdt::USER_CODE as u64;
    frame.ss = gdt::USER_DATA as u64;

    if action.flags & SA_NODEFER == 0 {
        p.sigmask |= bit(sig);
    }
    p.sigmask = (p.sigmask | action.mask) & !UNBLOCKABLE;
    if action.flags & SA_RESETHAND != 0 {
        p.handlers()[sig] = SigAction::default();
    }
    Ok(())
}

/// rt_sigreturn: back from a handler. Returns the saved rax so the system call path leaves it alone.
pub fn sigreturn(frame: &mut TrapFrame) -> Result<u64, Errno> {
    let p = current();
    // The handler's `ret` popped the restorer address, so the frame starts 8 bytes below rsp.
    let base = frame.rsp - 8;
    let f = user::read_vec(p.space(), base, SIGINFO)?;
    let get = |at: usize| u64::from_le_bytes(f[at..at + 8].try_into().unwrap());
    let r = |i: usize| get(MCONTEXT + i * 8);
    *frame = TrapFrame {
        r8: r(0),
        r9: r(1),
        r10: r(2),
        r11: r(3),
        r12: r(4),
        r13: r(5),
        r14: r(6),
        r15: r(7),
        rdi: r(8),
        rsi: r(9),
        rbp: r(10),
        rbx: r(11),
        rdx: r(12),
        rax: r(13),
        rcx: r(14),
        rsp: r(15),
        rip: r(16),
        // Only the flags a program may change; interrupts stay on.
        rflags: (r(17) & 0x0DD5) | 0x202,
        cs: gdt::USER_CODE as u64,
        ss: gdt::USER_DATA as u64,
        vector: SYSCALL_VECTOR,
        error: 0,
    };
    p.sigmask = get(SIGMASK) & !UNBLOCKABLE;

    let fpu_at = get(MCONTEXT + 184);
    if fpu_at != 0 {
        let mut fpu = super::FpuState([0; 512]);
        user::read(p.space(), fpu_at, &mut fpu.0)?;
        // Keep MXCSR to bits the CPU accepts, or fxrstor would fault.
        let mxcsr = u32::from_le_bytes(fpu.0[24..28].try_into().unwrap()) & 0xFFBF;
        fpu.0[24..28].copy_from_slice(&mxcsr.to_le_bytes());
        unsafe { asm!("fxrstor [{}]", in(reg) &fpu) };
    }
    Ok(frame.rax)
}

/// rt_sigsuspend: waits for a signal with a temporary mask; the old mask comes back after the handler.
/// rt_sigtimedwait (sigwait, sigwaitinfo, sigtimedwait): waits for one of the signals in the set
/// (usually blocked ones) and takes it instead of running a handler. EAGAIN if the timeout passes.
pub fn timed_wait(set_at: u64, info_at: u64, timeout_at: u64) -> Result<u64, Errno> {
    let want = user::read_u64(current().space(), set_at)? & !UNBLOCKABLE;
    let deadline = if timeout_at == 0 {
        None
    } else {
        let secs = user::read_u64(current().space(), timeout_at)?;
        let nanos = user::read_u64(current().space(), timeout_at + 8)?;
        let ticks = secs * crate::arch::irq::TIMER_HZ + nanos.div_ceil(1_000_000_000 / crate::arch::irq::TIMER_HZ);
        Some(crate::arch::irq::ticks() + ticks)
    };
    loop {
        let p = current();
        let hit = p.pending & want;
        if hit != 0 {
            let sig = hit.trailing_zeros() as usize + 1;
            let info = take(p, sig);
            if info_at != 0 {
                user::write(p.space(), info_at, &info.siginfo(sig))?;
            }
            return Ok(sig as u64);
        }
        if deadline.is_some_and(|d| crate::arch::irq::ticks() >= d) {
            return Err(EAGAIN);
        }
        super::sleep()?; // EINTR if another signal, one not waited for, needs acting on
    }
}

pub fn suspend(mask: u64) -> Result<u64, Errno> {
    let p = current();
    p.saved_mask = Some(p.sigmask);
    p.sigmask = mask & !UNBLOCKABLE;
    while !interrupted(current()) {
        if super::sleep().is_err() {
            break;
        }
    }
    Err(EINTR)
}
