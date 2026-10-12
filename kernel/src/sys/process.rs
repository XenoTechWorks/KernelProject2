//! Process system calls: clone/fork, execve, wait4, kill, signals, process groups.

use super::SysResult;
use super::errno::*;
use crate::arch::idt::TrapFrame;
use crate::mm::user;
use crate::proc::{self, SigAction};

/// Changing who we are. Only root may become somebody else; anyone may go back to their real id
/// (which is how a set-user-id program drops the privilege it was given).
pub fn set_uid(uid: u32) -> SysResult {
    let p = proc::current();
    if !p.creds.is_root() && uid != p.creds.uid {
        return Err(EPERM);
    }
    set_all(proc::Credentials { uid, euid: uid, ..p.creds });
    Ok(0)
}

pub fn set_gid(gid: u32) -> SysResult {
    let p = proc::current();
    if !p.creds.is_root() && gid != p.creds.gid {
        return Err(EPERM);
    }
    set_all(proc::Credentials { gid, egid: gid, ..p.creds });
    Ok(0)
}

/// setreuid and setresuid: -1 (as an unsigned number) means "leave this one alone".
pub fn set_reuid(real: u32, effective: u32) -> SysResult {
    let p = proc::current();
    let mut creds = p.creds;
    if real != u32::MAX {
        creds.uid = real;
    }
    if effective != u32::MAX {
        creds.euid = effective;
    }
    if !p.creds.is_root() && (creds.uid != p.creds.uid || (creds.euid != p.creds.uid && creds.euid != p.creds.euid)) {
        return Err(EPERM);
    }
    set_all(creds);
    Ok(0)
}

pub fn set_regid(real: u32, effective: u32) -> SysResult {
    let p = proc::current();
    let mut creds = p.creds;
    if real != u32::MAX {
        creds.gid = real;
    }
    if effective != u32::MAX {
        creds.egid = effective;
    }
    if !p.creds.is_root() && (creds.gid != p.creds.gid || (creds.egid != p.creds.gid && creds.egid != p.creds.egid)) {
        return Err(EPERM);
    }
    set_all(creds);
    Ok(0)
}

/// getresuid and getresgid: the real, effective and saved ids (we keep no separate saved one).
pub fn get_res(n: u64, real: u64, effective: u64, saved: u64) -> SysResult {
    let p = proc::current();
    let (r, e) = if n == 118 { (p.creds.uid, p.creds.euid) } else { (p.creds.gid, p.creds.egid) };
    for (at, value) in [(real, r), (effective, e), (saved, e)] {
        user::write_struct(p.space(), at, &value)?;
    }
    Ok(0)
}

/// Credentials belong to the whole process, so every thread of it moves together.
fn set_all(creds: proc::Credentials) {
    let tgid = proc::current().tgid;
    for t in proc::threads(tgid) {
        t.creds = creds;
    }
}

pub fn clone(frame: &TrapFrame, flags: u64, new_sp: u64, parent_tid: u64, child_tid: u64, tls: u64) -> SysResult {
    Ok(proc::fork(frame, flags, new_sp, parent_tid, child_tid, tls)? as u64)
}

pub fn execve(frame: &mut TrapFrame, dirfd: i32, path_ptr: u64, argv: u64, envp: u64) -> SysResult {
    let path = super::fs::resolve(dirfd, path_ptr)?;
    let p = proc::current();
    let args = user::read_cstr_array(p.space(), argv)?;
    let env = user::read_cstr_array(p.space(), envp)?;
    proc::exec(frame, &path, &args, &env)?;
    Ok(0)
}

pub fn wait4(pid: i64, status_ptr: u64, options: u32) -> SysResult {
    const WNOHANG: u32 = 1;
    const WUNTRACED: u32 = 2;
    const WCONTINUED: u32 = 8;
    let want = proc::WaitFor {
        exited: true,
        stopped: options & WUNTRACED != 0,
        continued: options & WCONTINUED != 0,
        nohang: options & WNOHANG != 0,
        keep: false,
    };
    match proc::wait(pid, want)? {
        Some((pid, status, _)) => {
            if status_ptr != 0 {
                user::write_struct(proc::current().space(), status_ptr, &status)?;
            }
            Ok(pid as u64)
        }
        None => Ok(0),
    }
}

/// waitid: wait4 with the answer as a siginfo (who, how it ended), and a choice of whom: any
/// child, one by pid, a group, or the process a pidfd stands for. WNOWAIT leaves it waitable.
pub fn waitid(idtype: u32, id: u64, info_at: u64, options: u32) -> SysResult {
    const P_ALL: u32 = 0;
    const P_PID: u32 = 1;
    const P_PGID: u32 = 2;
    const P_PIDFD: u32 = 3;
    const WNOHANG: u32 = 1;
    const WSTOPPED: u32 = 2;
    const WEXITED: u32 = 4;
    const WCONTINUED: u32 = 8;
    const WNOWAIT: u32 = 0x0100_0000;
    if options & (WEXITED | WSTOPPED | WCONTINUED) == 0 {
        return Err(EINVAL);
    }
    let target = match idtype {
        P_ALL => -1,
        P_PID => id as u32 as i64,
        P_PGID if id == 0 => 0,
        P_PGID => -(id as u32 as i64),
        P_PIDFD => super::pidfd::pid_of(id as i32)? as i64,
        _ => return Err(EINVAL),
    };
    let want = proc::WaitFor {
        exited: options & WEXITED != 0,
        stopped: options & WSTOPPED != 0,
        continued: options & WCONTINUED != 0,
        nohang: options & WNOHANG != 0,
        keep: options & WNOWAIT != 0,
    };
    let found = proc::wait(target, want)?;
    if info_at != 0 {
        // Nothing yet (WNOHANG): a zeroed siginfo, as Linux leaves it.
        let info = match found {
            Some((pid, status, uid)) => proc::signal::Info::child_ended(pid, uid, status).siginfo(proc::signal::SIGCHLD),
            None => [0u8; 128],
        };
        user::write(proc::current().space(), info_at, &info)?;
    }
    Ok(0)
}

pub fn kill(pid: i64, sig: usize) -> SysResult {
    if sig > 64 {
        return Err(EINVAL);
    }
    let me = proc::current();
    let info = proc::signal::Info::from_caller(proc::signal::SI_USER);
    match pid {
        p if p > 0 => proc::signal_process_info(p as u32, sig, info)?,
        0 => proc::signal_group_info(me.pgid, sig, info),
        -1 => {
            let all: alloc::vec::Vec<u32> = proc::processes().filter(|t| t.tgid != 1).map(|t| t.tgid).collect();
            for tgid in all {
                let _ = proc::signal_process_info(tgid, sig, info);
            }
        }
        p => proc::signal_group_info((-p) as u32, sig, info),
    }
    Ok(0)
}

/// tkill / tgkill: a signal for one thread.
pub fn tkill(tid: u32, sig: usize) -> SysResult {
    if sig > 64 {
        return Err(EINVAL);
    }
    let info = proc::signal::Info::from_caller(proc::signal::SI_TKILL);
    let target = proc::get(tid).ok_or(ESRCH)?;
    proc::signal::send_info(target, sig, info);
    Ok(0)
}

pub fn rt_sigaction(sig: usize, act: u64, old: u64) -> SysResult {
    if sig == 0 || sig > 64 {
        return Err(EINVAL);
    }
    let p = proc::current();
    if old != 0 {
        user::write_struct(p.space(), old, &p.handlers()[sig])?;
    }
    if act != 0 {
        if sig == proc::SIGKILL || sig == proc::SIGSTOP {
            return Err(EINVAL);
        }
        let action: SigAction = user::read_struct(p.space(), act)?;
        p.handlers()[sig] = action;
    }
    Ok(0)
}

pub fn rt_sigprocmask(how: u32, set: u64, old: u64) -> SysResult {
    let p = proc::current();
    if old != 0 {
        user::write_struct(p.space(), old, &p.sigmask)?;
    }
    if set != 0 {
        let mask = user::read_u64(p.space(), set)?;
        p.sigmask = match how {
            0 => p.sigmask | mask,
            1 => p.sigmask & !mask,
            2 => mask,
            _ => return Err(EINVAL),
        };
    }
    Ok(0)
}

pub fn rt_sigpending(out: u64) -> SysResult {
    let p = proc::current();
    user::write_struct(p.space(), out, &(p.pending & p.sigmask))?;
    Ok(0)
}

pub fn sched_yield() -> SysResult {
    proc::yield_now();
    Ok(0)
}

pub fn umask(mask: u32) -> SysResult {
    let p = proc::current();
    let old = p.files().umask;
    p.files().umask = mask & 0o777;
    Ok(old as u64)
}

// Process groups and sessions belong to the whole process, so every thread is updated.

pub fn setpgid(pid: u32, pgid: u32) -> SysResult {
    let pid = if pid == 0 { proc::current().tgid } else { pid };
    proc::get(pid).ok_or(ESRCH)?;
    let pgid = if pgid == 0 { pid } else { pgid };
    for t in proc::threads(pid) {
        t.pgid = pgid;
    }
    Ok(0)
}

pub fn getpgid(pid: u32) -> SysResult {
    let pid = if pid == 0 { proc::current().tgid } else { pid };
    Ok(proc::get(pid).ok_or(ESRCH)?.pgid as u64)
}

pub fn getsid(pid: u32) -> SysResult {
    let pid = if pid == 0 { proc::current().tgid } else { pid };
    Ok(proc::get(pid).ok_or(ESRCH)?.sid as u64)
}

/// A new session, led by the caller, with no controlling terminal yet.
pub fn setsid() -> SysResult {
    let tgid = proc::current().tgid;
    for t in proc::threads(tgid) {
        t.sid = tgid;
        t.pgid = tgid;
        t.ctty = None;
    }
    Ok(tgid as u64)
}


pub fn arch_prctl(code: u32, addr: u64) -> SysResult {
    const ARCH_SET_GS: u32 = 0x1001;
    const ARCH_SET_FS: u32 = 0x1002;
    const ARCH_GET_FS: u32 = 0x1003;
    const ARCH_GET_GS: u32 = 0x1004;
    let p = proc::current();
    match code {
        ARCH_SET_FS => {
            p.fs_base = addr;
            unsafe { crate::arch::cpu::wrmsr(crate::arch::cpu::MSR_FS_BASE, addr) };
            Ok(0)
        }
        ARCH_GET_FS => {
            user::write_struct(p.space(), addr, &p.fs_base)?;
            Ok(0)
        }
        ARCH_SET_GS => {
            if addr >= crate::mm::paging::USER_END {
                return Err(EPERM);
            }
            p.gs_base = addr;
            unsafe { crate::arch::cpu::wrmsr(crate::arch::cpu::MSR_GS_BASE, addr) };
            Ok(0)
        }
        ARCH_GET_GS => {
            user::write_struct(p.space(), addr, &p.gs_base)?;
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}
