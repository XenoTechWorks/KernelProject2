//! ptrace: one process watching and steering another, as strace and debuggers do.
//!
//! A traced thread stops at the points its tracer asked for (each system call going in and
//! coming out, each signal before it's acted on, exec, fork, exit, or when asked to) and stays
//! stopped until the tracer lets it go. Each stop is reported to the tracer through wait, as a
//! status (`sig << 8 | 0x7f`, with an event number above that for events), and while it lasts
//! the tracer can read and change the thread's registers and memory.
//!
//! The tracer can be the process's parent (`strace program`) or not (attaching to a running one).
//! The process ids here are thread ids (`pid` in Process), as Linux's ptrace uses.

use super::{Process, State, current, get, signal, wake_all, yield_now};
use crate::arch::idt::TrapFrame;
use crate::mm::user;
use crate::sys::SysResult;
use crate::sys::errno::*;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

// Requests.
const TRACEME: u64 = 0;
const PEEKTEXT: u64 = 1;
const PEEKDATA: u64 = 2;
const PEEKUSER: u64 = 3;
const POKETEXT: u64 = 4;
const POKEDATA: u64 = 5;
const CONT: u64 = 7;
const KILL: u64 = 8;
const GETREGS: u64 = 12;
const SETREGS: u64 = 13;
const ATTACH: u64 = 16;
const DETACH: u64 = 17;
const SYSCALL: u64 = 24;
const SETOPTIONS: u64 = 0x4200;
const GETEVENTMSG: u64 = 0x4201;
const GETSIGINFO: u64 = 0x4202;
const SETSIGINFO: u64 = 0x4203;
const GETREGSET: u64 = 0x4204;
const SETREGSET: u64 = 0x4205;
const SEIZE: u64 = 0x4206;
const INTERRUPT: u64 = 0x4207;
const LISTEN: u64 = 0x4208;
const GET_SYSCALL_INFO: u64 = 0x420e;

// Options.
const O_TRACESYSGOOD: u32 = 0x1;
const O_TRACEFORK: u32 = 0x2;
const O_TRACEVFORK: u32 = 0x4;
const O_TRACECLONE: u32 = 0x8;
const O_TRACEEXEC: u32 = 0x10;
const O_TRACEEXIT: u32 = 0x40;
const O_EXITKILL: u32 = 0x10_0000;
const O_ALL: u32 = 0x7f | O_EXITKILL | 0x8_0000 /* SUSPEND_SECCOMP */ | 0x80 /* TRACESECCOMP */;

// Events, in the status above the signal.
pub const EVENT_FORK: u32 = 1;
pub const EVENT_VFORK: u32 = 2;
pub const EVENT_CLONE: u32 = 3;
pub const EVENT_EXEC: u32 = 4;
pub const EVENT_EXIT: u32 = 6;
const EVENT_STOP: u32 = 128;

const SIGTRAP: usize = 5;
const NT_PRSTATUS: u64 = 1;
const CLD_TRAPPED: i32 = 4;
const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;

/// Why a traced thread is stopped.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// Going into a system call, or coming out of one.
    Entry,
    Exit,
    /// About to act on this signal.
    Signal(usize),
    /// An event (exec, fork ...) or PTRACE_INTERRUPT.
    Event,
    /// Its process was stopped (SIGSTOP and the like) while traced.
    Group,
}

pub struct Trace {
    /// The tracer's process id.
    pub tracer: u32,
    seized: bool,
    options: u32,
    /// Stop at system calls (let go with PTRACE_SYSCALL rather than PTRACE_CONT).
    syscalls: bool,
    /// The stop it's in, and the wait status for it; told to the tracer once.
    stop: Option<(Why, i32)>,
    reported: bool,
    /// The signal the tracer let it go with.
    resume_sig: usize,
    /// PTRACE_INTERRUPT asked for a stop.
    interrupt: bool,
    /// PTRACE_LISTEN: in the group stop until SIGCONT, then a stop for the tracer.
    listening: bool,
    /// The group stop ended (SIGCONT) while the tracer held the thread.
    continued: bool,
    /// The exit was told to a tracer that isn't the parent.
    pub exit_reported: bool,
    event_msg: u64,
    siginfo: (usize, signal::Info),
    /// The registers as the program left them: the trap frame of its system call or interrupt.
    frame: usize,
    /// The system call it's in (number, arguments), and what it returned.
    syscall: (u64, [u64; 6]),
    result: u64,
    in_syscall: bool,
}

impl Trace {
    fn new(tracer: u32, seized: bool, options: u32) -> Box<Trace> {
        Box::new(Trace {
            tracer,
            seized,
            options,
            syscalls: false,
            stop: None,
            reported: false,
            resume_sig: 0,
            interrupt: false,
            listening: false,
            continued: false,
            exit_reported: false,
            event_msg: 0,
            siginfo: (0, signal::Info::default()),
            frame: 0,
            syscall: (0, [0; 6]),
            result: 0,
            in_syscall: false,
        })
    }

    fn frame(&self) -> Option<&'static mut TrapFrame> {
        (self.frame != 0).then(|| unsafe { &mut *(self.frame as *mut TrapFrame) })
    }
}

fn stopped(sig: usize) -> i32 {
    (sig as i32) << 8 | 0x7f
}

fn event_status(event: u32) -> i32 {
    stopped(SIGTRAP) | (event as i32) << 16
}

// ---- the traced side ----

/// Tells the tracer about a stop: SIGCHLD, and anyone waiting looks again.
fn notify(p: &Process) {
    let Some(t) = &p.trace else { return };
    let sig = t.stop.map_or(0, |(_, status)| (status >> 8) & 0xff);
    let info = signal::Info { code: CLD_TRAPPED, pid: p.pid, uid: p.creds.uid, status: sig };
    if let Some(tracer) = get(t.tracer) {
        signal::send_info(tracer, signal::SIGCHLD, info);
    }
    wake_all();
}

/// Stops the running thread for its tracer until it's let go, and returns the signal it was let
/// go with (0: none).
pub fn stop(why: Why, status: i32) -> usize {
    let p = current();
    let Some(t) = p.trace.as_mut() else { return 0 };
    t.stop = Some((why, status));
    t.reported = false;
    t.resume_sig = 0;
    p.state = State::Traced;
    notify(p);
    while current().state == State::Traced {
        yield_now();
    }
    current().trace.as_mut().map_or(0, |t| core::mem::take(&mut t.resume_sig))
}

/// A signal the tracer let a stop go with (other than at a signal stop) is sent on.
fn pass_on(sig: usize) {
    if sig != 0 {
        signal::send(current(), sig);
    }
}

fn traced() -> Option<&'static mut Trace> {
    current().trace.as_deref_mut()
}

/// Every system call going in. Keeps the registers for the tracer, stops if asked to, and returns
/// the call to make: the tracer may have changed it (or set -1 to skip it).
pub fn syscall_entry(frame: &mut TrapFrame, number: u64) -> u64 {
    let Some(t) = traced() else { return number };
    t.frame = frame as *mut TrapFrame as usize;
    t.syscall = (number, [frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9]);
    t.in_syscall = t.syscalls;
    if !t.syscalls {
        return number;
    }
    let good = if t.options & O_TRACESYSGOOD != 0 { 0x80 } else { 0 };
    pass_on(stop(Why::Entry, stopped(SIGTRAP | good)));
    traced().map_or(number, |t| t.syscall.0)
}

/// Every system call coming out (with its result in rax).
pub fn syscall_exit(frame: &mut TrapFrame) {
    let Some(t) = traced() else { return };
    if !core::mem::take(&mut t.in_syscall) {
        return;
    }
    t.frame = frame as *mut TrapFrame as usize;
    t.result = frame.rax;
    // Cut short by a signal still to be acted on: whether the program sees EINTR or the call
    // starts again isn't known yet, so the tracer is told what Linux tells it: ERESTARTSYS.
    if frame.rax as i64 == -EINTR && signal::interrupted(current()) {
        const ERESTARTSYS: i64 = 512;
        t.result = (-ERESTARTSYS) as u64;
    }
    let good = if t.options & O_TRACESYSGOOD != 0 { 0x80 } else { 0 };
    pass_on(stop(Why::Exit, stopped(SIGTRAP | good)));
}

/// Before a signal is acted on: the tracer sees it first and may let it through, change it, or
/// drop it (0). SIGKILL never stops.
pub fn signal_stop(frame: &mut TrapFrame, sig: usize, info: signal::Info) -> usize {
    let Some(t) = traced() else { return sig };
    if sig == signal::SIGKILL {
        return sig;
    }
    t.frame = frame as *mut TrapFrame as usize;
    t.siginfo = (sig, info);
    stop(Why::Signal(sig), stopped(sig))
}

/// Returning to the program: a stop PTRACE_INTERRUPT asked for.
pub fn interrupted(frame: &mut TrapFrame) {
    let Some(t) = traced() else { return };
    if core::mem::take(&mut t.interrupt) {
        t.frame = frame as *mut TrapFrame as usize;
        pass_on(stop(Why::Event, event_status(EVENT_STOP)));
    }
}

/// Inside a call that waits: a PTRACE_INTERRUPT stops it there (the call then goes on waiting).
pub fn interrupted_while_waiting() {
    if let Some(t) = traced() {
        if core::mem::take(&mut t.interrupt) {
            pass_on(stop(Why::Event, event_status(EVENT_STOP)));
        }
    }
}

/// A stop signal stopped the process: a traced thread's part of that is a stop for its tracer.
/// Returns false if it isn't traced (it then stops the ordinary way).
pub fn group_stop(sig: usize) -> bool {
    let Some(t) = traced() else { return false };
    t.continued = false;
    let status = if t.seized { stopped(sig) | (EVENT_STOP as i32) << 16 } else { stopped(sig) };
    stop(Why::Group, status);
    true
}

/// SIGCONT ended a group stop: a listening tracee stops again for its tracer (to say so); one
/// the tracer holds remembers it, for PTRACE_LISTEN. True if it was in the group stop.
pub fn group_continued(p: &mut Process) -> bool {
    let Some(t) = p.trace.as_deref_mut() else { return false };
    if p.state != State::Traced || !matches!(t.stop, Some((Why::Group, _))) {
        return false;
    }
    if core::mem::take(&mut t.listening) {
        t.stop = Some((Why::Event, event_status(EVENT_STOP)));
        t.reported = false;
        notify(p);
    } else {
        t.continued = true;
    }
    true
}

/// After a successful exec.
pub fn after_exec(old_pid: u32) {
    let Some(t) = traced() else { return };
    if t.options & O_TRACEEXEC != 0 {
        t.event_msg = old_pid as u64;
        pass_on(stop(Why::Event, event_status(EVENT_EXEC)));
    } else if !t.seized {
        // The old way: a plain SIGTRAP after exec.
        signal::send(current(), SIGTRAP);
    }
}

/// Before the process ends (exit_group or a fatal signal).
pub fn before_exit(status: i32) {
    let Some(t) = traced() else { return };
    if t.options & O_TRACEEXIT != 0 {
        t.event_msg = status as u64;
        stop(Why::Event, event_status(EVENT_EXIT));
    }
}

/// fork, vfork or clone by a traced thread: if its tracer asked to follow those, the new task is
/// traced too and starts stopped. Returns the event for the parent to stop at, once the child is
/// in the task table.
pub fn on_fork(child: &mut Process, flags: u64, child_frame_at: u64) -> Option<u32> {
    const CLONE_VFORK: u64 = 0x4000;
    const CLONE_THREAD: u64 = 0x10000;
    let parent = current().trace.as_deref()?;
    let (event, option) = if flags & CLONE_VFORK != 0 {
        (EVENT_VFORK, O_TRACEVFORK)
    } else if flags & CLONE_THREAD != 0 || flags & 0xff != signal::SIGCHLD as u64 {
        (EVENT_CLONE, O_TRACECLONE)
    } else {
        (EVENT_FORK, O_TRACEFORK)
    };
    if parent.options & option == 0 {
        return None;
    }
    let mut t = Trace::new(parent.tracer, parent.seized, parent.options);
    t.stop = Some(if t.seized { (Why::Event, event_status(EVENT_STOP)) } else { (Why::Signal(signal::SIGSTOP), stopped(signal::SIGSTOP)) });
    t.frame = child_frame_at as usize;
    child.trace = Some(t);
    child.state = State::Traced;
    Some(event)
}

/// The parent's side of following a fork: the child is told to the tracer, and the parent stops
/// for the event, with the child's id to read.
pub fn after_fork(event: u32, child_pid: u32) {
    if let Some(child) = get(child_pid) {
        notify(child);
    }
    if let Some(t) = traced() {
        t.event_msg = child_pid as u64;
        pass_on(stop(Why::Event, event_status(event)));
    }
}

/// The tracer's process ended: its tracees go free (or end, if it asked for that).
pub fn tracer_gone(tracer: u32) {
    for p in super::all() {
        if p.trace.as_ref().is_some_and(|t| t.tracer == tracer) {
            let kill = p.trace.as_ref().is_some_and(|t| t.options & O_EXITKILL != 0);
            p.trace = None;
            if p.state == State::Traced {
                p.state = State::Runnable;
            }
            if kill {
                signal::send(p, signal::SIGKILL);
            }
        }
    }
}

/// For wait: a stop the tracer `tracer` hasn't been told about, as (tid, status).
pub fn unreported_stop(p: &mut Process, tracer: u32, keep: bool) -> Option<i32> {
    let t = p.trace.as_deref_mut().filter(|t| t.tracer == tracer)?;
    if p.state != State::Traced || t.reported {
        return None;
    }
    let (_, status) = t.stop?;
    if !keep {
        t.reported = true;
    }
    Some(status)
}

// ---- the tracer's side: the system call ----

/// May the caller trace `target`? Root may trace anyone; others, their own processes.
fn may_trace(target: &Process) -> bool {
    let me = current();
    me.creds.is_root() || (me.creds.uid == target.creds.uid && me.creds.euid == target.creds.euid)
}

/// The tracee `pid`, traced by the caller; with `stopped`, only while it's in a stop.
fn tracee(pid: u32, stopped: bool) -> Result<&'static mut Process, Errno> {
    let me = current().tgid;
    let p = get(pid).filter(|p| p.trace.as_ref().is_some_and(|t| t.tracer == me)).ok_or(ESRCH)?;
    if stopped && p.state != State::Traced {
        return Err(ESRCH);
    }
    Ok(p)
}

/// Lets a tracee go on, with a signal to deliver (for a signal stop) or to send.
fn resume(p: &mut Process, syscalls: bool, sig: u64) -> SysResult {
    if sig > 64 {
        return Err(EIO);
    }
    let t = p.trace.as_deref_mut().ok_or(ESRCH)?;
    t.syscalls = syscalls;
    t.listening = false;
    let why = t.stop.take().map(|(why, _)| why);
    t.resume_sig = match why {
        Some(Why::Group) | None => 0,
        _ => sig as usize,
    };
    p.state = State::Runnable;
    Ok(0)
}

/// `struct user_regs_struct`, the way Linux lays out a thread's registers for ptrace.
fn registers(p: &Process) -> Result<[u64; 27], Errno> {
    let t = p.trace.as_deref().ok_or(ESRCH)?;
    let f = t.frame().ok_or(EIO)?;
    let in_syscall = matches!(t.stop, Some((Why::Entry | Why::Exit, _)));
    let orig_rax = if in_syscall { t.syscall.0 } else { u64::MAX };
    // Going into a system call, rax holds -ENOSYS until it's made, as on Linux.
    let rax = if matches!(t.stop, Some((Why::Entry, _))) { (-(ENOSYS as i64)) as u64 } else { f.rax };
    Ok([
        f.r15, f.r14, f.r13, f.r12, f.rbp, f.rbx, f.r11, f.r10, f.r9, f.r8, rax, f.rcx, f.rdx, f.rsi, f.rdi, orig_rax, f.rip, f.cs, f.rflags,
        f.rsp, f.ss, p.fs_base, p.gs_base, 0, 0, 0, 0,
    ])
}

fn set_registers(p: &mut Process, r: &[u64; 27]) -> Result<(), Errno> {
    let t = p.trace.as_deref_mut().ok_or(ESRCH)?;
    let f = t.frame().ok_or(EIO)?;
    (f.r15, f.r14, f.r13, f.r12, f.rbp, f.rbx, f.r11, f.r10, f.r9, f.r8) = (r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8], r[9]);
    (f.rcx, f.rdx, f.rsi, f.rdi, f.rip, f.rsp) = (r[11], r[12], r[13], r[14], r[16], r[19]);
    // Only the flags a program may change (carry ... overflow, direction, trap).
    const USER_FLAGS: u64 = 0xdd5 | 0x100 | 0x400;
    f.rflags = (f.rflags & !USER_FLAGS) | (r[18] & USER_FLAGS);
    match t.stop {
        // Going into a system call: orig_rax is the call to make (-1: none); its arguments are
        // read from the registers again.
        Some((Why::Entry, _)) => t.syscall = (r[15], [r[14], r[13], r[12], r[7], r[9], r[8]]),
        _ => f.rax = r[10],
    }
    p.fs_base = r[21];
    p.gs_base = r[22];
    Ok(())
}

/// `struct ptrace_syscall_info` for the stop the tracee is in. Returns its bytes; the meaningful
/// length depends on the kind (24 for none, 80 going in, 33 coming out).
fn syscall_info(p: &Process) -> Result<Vec<u8>, Errno> {
    let t = p.trace.as_deref().ok_or(ESRCH)?;
    let f = t.frame().ok_or(EIO)?;
    let mut b = vec![0u8; 88];
    let op: u8 = match t.stop {
        Some((Why::Entry, _)) => 1,
        Some((Why::Exit, _)) => 2,
        _ => 0,
    };
    b[0] = op;
    b[4..8].copy_from_slice(&AUDIT_ARCH_X86_64.to_le_bytes());
    b[8..16].copy_from_slice(&f.rip.to_le_bytes());
    b[16..24].copy_from_slice(&f.rsp.to_le_bytes());
    let len = match op {
        1 => {
            b[24..32].copy_from_slice(&t.syscall.0.to_le_bytes());
            for (i, arg) in t.syscall.1.iter().enumerate() {
                b[32 + i * 8..40 + i * 8].copy_from_slice(&arg.to_le_bytes());
            }
            80
        }
        2 => {
            b[24..32].copy_from_slice(&t.result.to_le_bytes());
            b[32] = ((t.result as i64) < 0 && (t.result as i64) >= -4095) as u8;
            33
        }
        _ => 24,
    };
    b.truncate(len);
    Ok(b)
}

pub fn ptrace(request: u64, pid: u32, addr: u64, data: u64) -> SysResult {
    let me = current();
    match request {
        TRACEME => {
            if me.trace.is_some() {
                return Err(EPERM);
            }
            me.trace = Some(Trace::new(me.ppid, false, 0));
            Ok(0)
        }
        ATTACH | SEIZE => {
            if request == SEIZE && (addr != 0 || data as u32 & !O_ALL != 0) {
                return Err(EIO);
            }
            let tracer = me.tgid;
            let target = get(pid).ok_or(ESRCH)?;
            if target.tgid == tracer || target.trace.is_some() || target.tgid == 1 || !may_trace(target) {
                return Err(EPERM);
            }
            let seized = request == SEIZE;
            target.trace = Some(Trace::new(tracer, seized, if seized { data as u32 } else { 0 }));
            if !seized {
                signal::send(target, signal::SIGSTOP);
            }
            Ok(0)
        }
        INTERRUPT => {
            let p = tracee(pid, false)?;
            let t = p.trace.as_deref_mut().ok_or(ESRCH)?;
            if !t.seized {
                return Err(EIO);
            }
            match p.state {
                State::Traced => {}
                // Stopped with its process: that becomes a stop for the tracer.
                State::Stopped => {
                    let sig = get(p.tgid).and_then(|l| match l.job_event {
                        Some(super::JobEvent::Stopped(sig)) => Some(sig as usize),
                        _ => None,
                    });
                    t.stop = Some((Why::Group, stopped(sig.unwrap_or(signal::SIGSTOP)) | (EVENT_STOP as i32) << 16));
                    t.reported = false;
                    t.continued = false;
                    p.state = State::Traced;
                    notify(p);
                }
                _ => {
                    t.interrupt = true;
                    if p.state == State::Sleeping {
                        p.state = State::Runnable;
                    }
                }
            }
            Ok(0)
        }
        KILL => {
            let p = tracee(pid, false)?;
            signal::send(p, signal::SIGKILL);
            Ok(0)
        }
        _ => {
            let p = tracee(pid, true)?;
            request_on_stopped(p, request, addr, data)
        }
    }
}

/// The requests that need the tracee in a stop.
fn request_on_stopped(p: &'static mut Process, request: u64, addr: u64, data: u64) -> SysResult {
    let space = current().space();
    match request {
        CONT | SYSCALL => resume(p, request == SYSCALL, data),
        DETACH => {
            let sig = data;
            resume(p, false, sig)?;
            let pass = p.trace.as_ref().map_or(0, |t| t.resume_sig);
            p.trace = None;
            if pass != 0 {
                signal::send(p, pass as usize);
            }
            Ok(0)
        }
        LISTEN => {
            let t = p.trace.as_deref_mut().ok_or(ESRCH)?;
            if !t.seized || !matches!(t.stop, Some((Why::Group, _))) {
                return Err(EIO);
            }
            if core::mem::take(&mut t.continued) {
                // Already continued: it stops again right away to say so.
                t.stop = Some((Why::Event, event_status(EVENT_STOP)));
                t.reported = false;
                notify(p);
            } else {
                t.listening = true;
            }
            Ok(0)
        }
        SETOPTIONS => {
            if data as u32 & !O_ALL != 0 {
                return Err(EINVAL);
            }
            p.trace.as_deref_mut().ok_or(ESRCH)?.options = data as u32;
            Ok(0)
        }
        GETEVENTMSG => {
            user::write_struct(space, data, &p.trace.as_deref().ok_or(ESRCH)?.event_msg)?;
            Ok(0)
        }
        GETSIGINFO => {
            let t = p.trace.as_deref().ok_or(ESRCH)?;
            let info = match t.stop {
                Some((Why::Signal(sig), _)) => t.siginfo.1.siginfo(sig),
                // Other stops are SIGTRAPs (with the event in the code, like Linux).
                Some((_, status)) => signal::Info { code: (status >> 8) & 0xffff, ..signal::Info::default() }.siginfo(SIGTRAP),
                None => return Err(EINVAL),
            };
            user::write(space, data, &info)?;
            Ok(0)
        }
        SETSIGINFO => Ok(0),
        PEEKTEXT | PEEKDATA => {
            let word = user::read_u64(p.space(), addr).map_err(|_| EIO)?;
            user::write_struct(space, data, &word)?;
            Ok(0)
        }
        POKETEXT | POKEDATA => {
            user::write(p.space(), addr, &data.to_le_bytes()).map_err(|_| EIO)?;
            Ok(0)
        }
        PEEKUSER => {
            // The registers, as user_regs_struct; past them (debug registers ...), zeros.
            let regs = registers(p)?;
            let word = regs.get(addr as usize / 8).copied().unwrap_or(0);
            user::write_struct(space, data, &word)?;
            Ok(0)
        }
        GETREGS => {
            user::write_struct(space, data, &registers(p)?)?;
            Ok(0)
        }
        SETREGS => {
            let regs: [u64; 27] = user::read_struct(space, data)?;
            set_registers(p, &regs)?;
            Ok(0)
        }
        GETREGSET | SETREGSET => {
            if addr != NT_PRSTATUS {
                return Err(EINVAL);
            }
            let [base, len]: [u64; 2] = user::read_struct(space, data)?;
            if len < 27 * 8 {
                return Err(EINVAL);
            }
            if request == GETREGSET {
                user::write_struct(space, base, &registers(p)?)?;
                user::write_struct(space, data + 8, &(27u64 * 8))?;
            } else {
                let regs: [u64; 27] = user::read_struct(space, base)?;
                set_registers(p, &regs)?;
            }
            Ok(0)
        }
        GET_SYSCALL_INFO => {
            let info = syscall_info(p)?;
            let n = info.len().min(addr as usize);
            user::write(space, data, &info[..n])?;
            Ok(info.len() as u64)
        }
        _ => Err(EIO),
    }
}

/// process_vm_readv / process_vm_writev: copy between the caller's memory and another process's
/// (one it may trace), piece list to piece list. Returns the bytes copied.
pub fn process_vm(pid: u32, local: u64, local_count: u64, remote: u64, remote_count: u64, flags: u64, write: bool) -> SysResult {
    if flags != 0 || local_count > 1024 || remote_count > 1024 {
        return Err(EINVAL);
    }
    let target = get(pid).ok_or(ESRCH)?;
    if !may_trace(target) {
        return Err(EPERM);
    }
    let me = current();
    let pieces = |at: u64, count: u64| -> Result<Vec<(u64, u64)>, Errno> {
        (0..count).map(|i| user::read_struct::<[u64; 2]>(me.space(), at + i * 16).map(|[b, l]| (b, l))).collect()
    };
    let locals = pieces(local, local_count)?;
    let remotes = pieces(remote, remote_count)?;
    let (from_space, to_space) = if write { (me.space(), target.space()) } else { (target.space(), me.space()) };
    let (from, to) = if write { (&locals, &remotes) } else { (&remotes, &locals) };
    // Walk both lists at once, a chunk at a time, stopping at the first address that fails.
    let (mut fi, mut fo, mut ti, mut to_off) = (0, 0u64, 0, 0u64);
    let mut done = 0u64;
    let mut chunk = [0u8; 4096];
    while fi < from.len() && ti < to.len() {
        let (fb, fl) = from[fi];
        let (tb, tl) = to[ti];
        if fo == fl {
            (fi, fo) = (fi + 1, 0);
            continue;
        }
        if to_off == tl {
            (ti, to_off) = (ti + 1, 0);
            continue;
        }
        let n = (fl - fo).min(tl - to_off).min(chunk.len() as u64) as usize;
        if user::read(from_space, fb + fo, &mut chunk[..n]).is_err() || user::write(to_space, tb + to_off, &chunk[..n]).is_err() {
            break;
        }
        fo += n as u64;
        to_off += n as u64;
        done += n as u64;
    }
    if done == 0 && from.iter().any(|&(_, l)| l > 0) && to.iter().any(|&(_, l)| l > 0) {
        return Err(EFAULT);
    }
    Ok(done)
}
