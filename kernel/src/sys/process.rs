//! Process system calls: clone/fork, execve, wait4, kill, signals, process groups.

use super::SysResult;
use super::errno::*;
use crate::arch::idt::TrapFrame;
use crate::mm::user;
use crate::proc::{self, SigAction};

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
    match proc::wait(pid, options & WNOHANG != 0)? {
        Some((pid, status)) => {
            if status_ptr != 0 {
                user::write_struct(proc::current().space(), status_ptr, &status)?;
            }
            Ok(pid as u64)
        }
        None => Ok(0),
    }
}

pub fn kill(pid: i64, sig: usize) -> SysResult {
    if sig > 64 {
        return Err(EINVAL);
    }
    let me = proc::current();
    match pid {
        p if p > 0 => {
            let target = proc::get(p as u32).ok_or(ESRCH)?;
            proc::signal(target, sig);
        }
        0 => proc::signal_group(me.pgid, sig),
        -1 => {
            for target in proc::all() {
                if target.pid != 1 {
                    proc::signal(target, sig);
                }
            }
        }
        p => proc::signal_group((-p) as u32, sig),
    }
    Ok(0)
}

pub fn rt_sigaction(sig: usize, act: u64, old: u64) -> SysResult {
    if sig == 0 || sig > 64 {
        return Err(EINVAL);
    }
    let p = proc::current();
    if old != 0 {
        user::write_struct(p.space(), old, &p.sigactions[sig])?;
    }
    if act != 0 {
        if sig == proc::SIGKILL || sig == proc::SIGSTOP {
            return Err(EINVAL);
        }
        let action: SigAction = user::read_struct(p.space(), act)?;
        p.sigactions[sig] = action;
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
    let old = p.umask;
    p.umask = mask & 0o777;
    Ok(old as u64)
}

pub fn setpgid(pid: u32, pgid: u32) -> SysResult {
    let me = proc::current();
    let pid = if pid == 0 { me.pid } else { pid };
    let target = proc::get(pid).ok_or(ESRCH)?;
    target.pgid = if pgid == 0 { pid } else { pgid };
    Ok(0)
}

pub fn getpgid(pid: u32) -> SysResult {
    let pid = if pid == 0 { proc::current().pid } else { pid };
    Ok(proc::get(pid).ok_or(ESRCH)?.pgid as u64)
}

pub fn getsid(pid: u32) -> SysResult {
    let pid = if pid == 0 { proc::current().pid } else { pid };
    Ok(proc::get(pid).ok_or(ESRCH)?.sid as u64)
}

pub fn setsid() -> SysResult {
    let p = proc::current();
    p.sid = p.pid;
    p.pgid = p.pid;
    Ok(p.pid as u64)
}

/// getresuid / getresgid: everyone is root for now.
pub fn getresid(a: [u64; 6]) -> SysResult {
    let p = proc::current();
    for ptr in &a[..3] {
        if *ptr != 0 {
            user::write_struct(p.space(), *ptr, &0u32)?;
        }
    }
    Ok(0)
}

pub fn arch_prctl(code: u32, addr: u64) -> SysResult {
    const ARCH_SET_FS: u32 = 0x1002;
    const ARCH_GET_FS: u32 = 0x1003;
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
        _ => Err(EINVAL),
    }
}
