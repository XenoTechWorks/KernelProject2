//! /proc: files whose contents are made up when they're opened, describing the system and every
//! process in the formats Linux uses (programs like fastfetch, ps and top parse them).

use super::{Kind, Node};
use crate::proc::{self, State};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

fn file(text: impl Into<Vec<u8>>) -> Arc<Node> {
    Node::file(text.into(), 0o444)
}

fn link(target: impl Into<String>) -> Arc<Node> {
    Node::new(Kind::Symlink(target.into()), 0o777)
}

/// A link to a file, by its path as the process looking sees it (inside its chroot).
fn path_link(path: &str) -> Arc<Node> {
    let root = super::current_root();
    link(super::unrooted(&root, path).unwrap_or_else(|| String::from(path)))
}

fn dir(entries: Vec<(String, Arc<Node>)>) -> Arc<Node> {
    let node = Node::new(Kind::Dir(Mutex::new(BTreeMap::new())), 0o555);
    if let Kind::Dir(map) = &node.kind {
        map.lock().extend(entries);
    }
    node
}

fn meminfo() -> String {
    let (free, total) = crate::mm::frame::stats();
    let (free, total) = (free * 4, total * 4);
    let mut s = String::new();
    for (name, kb) in [
        ("MemTotal", total),
        ("MemFree", free),
        ("MemAvailable", free),
        ("Buffers", 0),
        ("Cached", 0),
        ("SwapCached", 0),
        ("Active", total - free),
        ("Inactive", 0),
        ("SwapTotal", 0),
        ("SwapFree", 0),
        ("Shmem", 0),
        ("SReclaimable", 0),
    ] {
        s += &format!("{:<16}{:>8} kB\n", format!("{name}:"), kb);
    }
    s
}

fn cpuinfo() -> String {
    let cpu = crate::arch::cpuid::info();
    let mhz = crate::arch::cpuid::tsc_mhz();
    format!(
        "processor\t: 0\nvendor_id\t: {}\ncpu family\t: {}\nmodel\t\t: {}\nmodel name\t: {}\nstepping\t: {}\n\
         cpu MHz\t\t: {mhz}.000\nphysical id\t: 0\nsiblings\t: 1\ncore id\t\t: 0\ncpu cores\t: 1\n\
         flags\t\t: {}\nbogomips\t: {}.00\naddress sizes\t: 48 bits physical, 48 bits virtual\n\n",
        cpu.vendor,
        cpu.family,
        cpu.model,
        cpu.brand,
        cpu.stepping,
        cpu.flags.join(" "),
        mhz * 2,
    )
}

fn uptime() -> String {
    let (secs, nanos) = crate::time::monotonic();
    format!("{secs}.{:02} {secs}.{:02}\n", nanos / 10_000_000, nanos / 10_000_000)
}

fn stat() -> String {
    let ticks = crate::arch::irq::ticks();
    let boot = crate::time::realtime().0 - crate::time::monotonic().0;
    let count = proc::all().count();
    format!(
        "cpu  0 0 0 {ticks} 0 0 0 0 0 0\ncpu0 0 0 0 {ticks} 0 0 0 0 0 0\nintr 0\nctxt 0\nbtime {boot}\n\
         processes {count}\nprocs_running 1\nprocs_blocked 0\n"
    )
}

const MOUNTS: &str = "rootfs / tmpfs rw 0 0\nproc /proc proc rw,nosuid,nodev,noexec 0 0\n\
                      sysfs /sys sysfs rw,nosuid,nodev,noexec 0 0\ndevtmpfs /dev devtmpfs rw 0 0\n";

fn mounts() -> String {
    let mut s = String::from(MOUNTS);
    if let Some((path, _)) = super::DISK_MOUNT.get() {
        s += &format!("/dev/vda {path} ext2 rw,relatime 0 0
");
    }
    s
}

/// The state as /proc/<pid>/status spells it out (debuggers compare these words).
fn state_name(state: State) -> &'static str {
    match state {
        State::Runnable => "R (running)",
        State::Sleeping => "S (sleeping)",
        State::Zombie(_) | State::Lingering => "Z (zombie)",
        State::Dead => "X (dead)",
        State::Stopped => "T (stopped)",
        State::Traced => "t (tracing stop)",
    }
}

fn state_letter(state: State) -> char {
    match state {
        State::Runnable => 'R',
        State::Sleeping => 'S',
        State::Zombie(_) | State::Lingering => 'Z',
        State::Dead => 'X',
        State::Stopped => 'T',
        State::Traced => 't',
    }
}

/// /proc/<pid>: a process; and inside it, task/<tid> for each of its threads, which looks the same
/// (without task/ of its own).
fn process_dir(p: &proc::Process) -> Arc<Node> {
    let tasks: Vec<(String, Arc<Node>)> = proc::threads(p.tgid).map(|t| (t.pid.to_string(), thread_dir(t))).collect();
    let me = thread_dir(p);
    if let Kind::Dir(entries) = &me.kind {
        entries.lock().insert(String::from("task"), dir(tasks));
    }
    me
}

fn thread_dir(p: &proc::Process) -> Arc<Node> {
    let state = state_letter(p.state);
    // The controlling terminal's device number (/dev/console is 5,1; /dev/pts/N is 136,N), and
    // its foreground process group.
    let (tty, tpgid) = match &p.ctty {
        Some(t) if t.is_console() => (0x501, t.foreground() as i64),
        Some(t) => (crate::sys::fs::makedev(136, crate::pty::index(t) as u64), t.foreground() as i64),
        None => (0, -1),
    };
    let stat = format!(
        "{} ({}) {state} {} {} {} {tty} {} 0 0 0 0 0 0 0 0 0 20 0 1 0 {} 0 0\n",
        p.pid,
        p.name,
        p.ppid,
        p.pgid,
        p.sid,
        tpgid,
        p.start_ticks,
    );
    let c = &p.creds;
    let tracer = p.trace.as_ref().map_or(0, |t| t.tracer);
    let status = format!(
        "Name:\t{}\nState:\t{}\nTgid:\t{}\nPid:\t{}\nPPid:\t{}\nTracerPid:\t{tracer}\nUid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\nThreads:\t{}\n",
        p.name,
        state_name(p.state),
        p.tgid,
        p.pid,
        p.ppid,
        c.uid,
        c.euid,
        c.euid,
        c.euid,
        c.gid,
        c.egid,
        c.egid,
        c.egid,
        proc::threads(p.tgid).count(),
    );
    let fds = p
        .files().fds.iter()
        .map(|(n, file)| (n.to_string(), if file.path.starts_with('/') { path_link(&file.path) } else { link(file.path.clone()) }))
        .collect();
    dir(alloc::vec![
        ("stat".into(), file(stat)),
        ("status".into(), file(status)),
        ("comm".into(), file(format!("{}\n", p.name))),
        ("cmdline".into(), file(p.cmdline.clone())),
        ("environ".into(), file(Vec::new())),
        ("maps".into(), file(Vec::new())),
        ("auxv".into(), file(p.auxv.clone())),
        ("mem".into(), {
            // Its own user's, like the process (the kernel checks who may trace it, too).
            let mem = Node::new(Kind::ProcMem(p.pid), 0o600);
            let _ = mem.set_owner(p.creds.euid, p.creds.egid);
            mem
        }),
        ("mounts".into(), file(mounts())),
        ("exe".into(), path_link(&p.exe)),
        ("cwd".into(), path_link(&p.files().cwd)),
        ("root".into(), path_link(&p.files().root)),
        ("fd".into(), dir(fds)),
    ])
}

/// The whole /proc tree as it is right now.
pub fn build() -> Arc<Node> {
    let mut entries: Vec<(String, Arc<Node>)> = alloc::vec![
        ("meminfo".into(), file(meminfo())),
        ("cpuinfo".into(), file(cpuinfo())),
        ("uptime".into(), file(uptime())),
        ("stat".into(), file(stat())),
        ("loadavg".into(), file(format!("0.00 0.00 0.00 1/{} 0\n", proc::all().count()))),
        ("version".into(), file(format!("Linux version 6.12.0-kp2 (KernelProject2 {}) #1\n", crate::VERSION))),
        ("mounts".into(), file(mounts())),
        ("swaps".into(), file("Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n")),
        ("filesystems".into(), file("nodev\ttmpfs\nnodev\tproc\nnodev\tsysfs\nnodev\tdevtmpfs\n")),
        ("cmdline".into(), file("\n")),
        (
            "sys".into(),
            dir(alloc::vec![
                (
                    "kernel".into(),
                    dir(alloc::vec![
                        ("hostname".into(), file("kernelproject2\n")),
                        ("ostype".into(), file("Linux\n")),
                        ("osrelease".into(), file("6.12.0-kp2\n")),
                    ])
                ),
                (
                    "vm".into(),
                    dir(alloc::vec![("overcommit_memory".into(), file("0\n")), ("nr_hugepages".into(), file("0\n"))])
                ),
            ])
        ),
    ];
    if let Some(pid) = proc::current_pid() {
        entries.push(("self".into(), link(pid.to_string())));
        entries.push(("thread-self".into(), link(format!("{pid}/task/{pid}"))));
    }
    for p in proc::processes() {
        entries.push((p.pid.to_string(), process_dir(p)));
    }
    dir(entries)
}
