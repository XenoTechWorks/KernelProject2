//! KernelProject2: a kernel that runs Linux programs as they are, by speaking Linux's system call
//! language. This file brings the machine up and starts the first program.

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod boot;
mod console;
mod drivers;
mod font;
mod fs;
mod keyboard;
mod mm;
mod mouse;
mod net;
mod proc;
mod pty;
mod smbios;
mod sys;
mod term;
mod time;
mod title_font;
mod tty;

use arch::{gdt, idt, irq, serial};
use core::fmt::Write;
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};
use limine::request::{ExecutableCmdlineRequest, FramebufferRequest, HhdmRequest, MemmapRequest, ModulesRequest, SmbiosRequest};

// What we ask Limine for. It fills in the responses before jumping to kmain.
#[used]
#[unsafe(link_section = ".limine_requests_start")]
static REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static HHDM: HhdmRequest = HhdmRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static MEMMAP: MemmapRequest = MemmapRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static FRAMEBUFFER: FramebufferRequest = FramebufferRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static MODULES: ModulesRequest = ModulesRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static SMBIOS: SmbiosRequest = SmbiosRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static CMDLINE: ExecutableCmdlineRequest = ExecutableCmdlineRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests_end")]
static REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

pub const VERSION: &str = "0.1.0";

struct SerialWriter;

impl Write for SerialWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        serial::write_str(s);
        Ok(())
    }
}

/// Kernel log line, on the serial port only (the screen belongs to programs).
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = write!($crate::LogWriter, "[kernel] ");
        let _ = writeln!($crate::LogWriter, $($arg)*);
    }};
}

pub struct LogWriter;

impl Write for LogWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        SerialWriter.write_str(s)
    }
}

/// Message for both the serial port and the screen.
fn announce(text: &str) {
    serial::write_str(text);
    console::write(text.as_bytes());
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    log!("KernelProject2 {VERSION} starting");
    if !BASE_REVISION.is_supported() {
        log!("the bootloader doesn't support Limine base revision {}", BaseRevision::MAX_SUPPORTED);
        arch::cpu::halt_forever();
    }

    gdt::init();
    idt::init();

    let hhdm = HHDM.response().expect("no HHDM response").offset;
    let memmap = MEMMAP.response().expect("no memory map").entries();
    mm::init(hhdm, memmap);
    let screen = FRAMEBUFFER.response().and_then(|r| r.framebuffers().first().copied());
    if let Some(fb) = screen {
        console::init(fb);
    }
    boot::set_cmdline(CMDLINE.response().map_or("", |r| r.cmdline()));
    announce(&alloc::format!("\x1b[1;36mKernelProject2\x1b[0m {VERSION}\n\n"));
    // From here on, each step says how it went, the way Linux distributions do.
    if !boot::cmdline().is_empty() {
        boot::ok(&alloc::format!("Kernel command line: {}", boot::cmdline()));
    }
    let (free, total) = mm::frame::stats();
    boot::ok(&alloc::format!("Memory: {} MiB free of {} MiB, kernel heap 256 MiB", free * 4 / 1024, total * 4 / 1024));
    match screen {
        Some(fb) => boot::ok(&alloc::format!("Screen: {}x{}, {}-bit framebuffer from the bootloader", fb.width, fb.height, fb.bpp)),
        None => boot::warn("No screen: only the serial port works"),
    }

    time::init();
    arch::cpuid::mark_boot();
    boot::ok("Started the clock (CMOS real-time clock, PIT timer)");
    tty::init();
    proc::init();
    boot::ok("Started the scheduler (processes, threads, signals)");
    fs::init_devices();
    let dmi = match SMBIOS.response() {
        Some(r) => smbios::read(r.entry_32 as *const u8, r.entry_64 as *const u8),
        None => smbios::Dmi::default(),
    };
    match dmi.fields.iter().find(|(n, _)| *n == "product_name") {
        Some((_, name)) => boot::ok(&alloc::format!("Machine: {name} (SMBIOS)")),
        None => boot::warn("Machine: unknown (no SMBIOS tables)"),
    }
    fs::sysfs::init(&dmi);
    match MODULES.response().and_then(|r| r.modules().iter().find(|m| m.cmdline() == "initramfs").copied()) {
        Some(module) => {
            let count = fs::cpio::unpack(module.data());
            boot::ok(&alloc::format!("Unpacked the root file system: {count} files (cpio initramfs)"));
        }
        None => boot::report(boot::Status::Failed, "No root file system (the initramfs module is missing)"),
    }
    // Home lives on the disk when there is one, so what you make there survives a reboot.
    match drivers::virtio_blk::init() {
        Some(mib) => boot::ok(&alloc::format!("Disk: virtio-blk, {mib} MiB")),
        None => boot::warn("No disk: files in /root last only until a reboot"),
    }
    let tablet = drivers::virtio_input::init();
    match drivers::virtio_net::init() {
        Some(mac) => {
            let c = net::config();
            let ip = |a: [u8; 4]| alloc::format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]);
            boot::ok(&alloc::format!(
                "Network: virtio-net {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, IPv4 {} (gateway {}, DNS {}), TCP and UDP",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], ip(c.ip), ip(c.gateway), ip(c.dns)
            ));
        }
        None => boot::warn("No network card: networking is off"),
    }
    match fs::mount_disk("/root") {
        Some(Ok(())) => boot::ok("Mounted /root (ext2 on the virtio disk)"),
        Some(Err(why)) => boot::warn(&alloc::format!("Couldn't mount the disk at /root: {why}")),
        None => {}
    }
    match (mouse::init(), tablet) {
        (_, true) => boot::ok("Pointer: virtio tablet (absolute), PS/2 keyboard"),
        (Some(wheel), false) => boot::ok(if wheel { "Pointer: PS/2 mouse with a wheel, PS/2 keyboard" } else { "Pointer: PS/2 mouse, PS/2 keyboard" }),
        (None, false) => boot::warn("No mouse found; PS/2 keyboard"),
    }
    irq::init();
    boot::ok("Interrupts on: 8259 PIC through the local APIC, timer at 100 Hz");

    // The same order Linux tries.
    let started = ["/sbin/init", "/etc/init", "/bin/init", "/bin/sh"].iter().find(|path| match proc::spawn_init(path) {
        Ok(()) => {
            log!("started {path} as pid 1");
            boot::ok(&alloc::format!("Started {path} (pid 1)"));
            true
        }
        Err(e) => {
            if e != sys::errno::ENOENT {
                log!("{path}: error {e}");
            }
            false
        }
    });
    if started.is_none() {
        announce("No init program found (tried /sbin/init, /etc/init, /bin/init, /bin/sh).\n");
        arch::cpu::halt_forever();
    }
    proc::run();
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let message = alloc::format!("\n\x1b[1;31mkernel panic:\x1b[0m {info}\n");
    serial::write_str(&message);
    // The console lock might be held by the code that panicked; don't wait for it.
    console::try_write(message.as_bytes());
    arch::cpu::halt_forever();
}
