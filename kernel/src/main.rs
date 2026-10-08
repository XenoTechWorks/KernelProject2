//! KernelProject2: a kernel that runs Linux programs as they are, by speaking Linux's system call
//! language. This file brings the machine up and starts the first program.

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod console;
mod drivers;
mod font;
mod fs;
mod keyboard;
mod mm;
mod proc;
mod smbios;
mod sys;
mod time;
mod tty;

use arch::{gdt, idt, irq, serial};
use core::fmt::Write;
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};
use limine::request::{FramebufferRequest, HhdmRequest, MemmapRequest, ModulesRequest, SmbiosRequest};

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
    let (free, total) = mm::frame::stats();
    log!("memory: {} MiB free of {} MiB", free * 4 / 1024, total * 4 / 1024);

    if let Some(fb) = FRAMEBUFFER.response().and_then(|r| r.framebuffers().first().copied()) {
        console::init(fb);
        log!("screen: {}x{}", fb.width, fb.height);
    }
    announce(&alloc::format!("\x1b[1;36mKernelProject2\x1b[0m {VERSION}\n"));

    time::init();
    arch::cpuid::mark_boot();
    tty::init();
    proc::init();
    fs::init_devices();
    let dmi = match SMBIOS.response() {
        Some(r) => smbios::read(r.entry_32 as *const u8, r.entry_64 as *const u8),
        None => smbios::Dmi::default(),
    };
    if let Some((_, name)) = dmi.fields.iter().find(|(n, _)| *n == "product_name") {
        log!("machine: {name}");
    }
    fs::sysfs::init(&dmi);
    match MODULES.response().and_then(|r| r.modules().iter().find(|m| m.cmdline() == "initramfs").copied()) {
        Some(module) => {
            let count = fs::cpio::unpack(module.data());
            log!("initramfs: {count} entries");
        }
        None => log!("no initramfs module"),
    }
    // Home lives on the disk when there is one, so what you make there survives a reboot.
    drivers::virtio_blk::init();
    fs::mount_disk("/root");
    irq::init();

    // The same order Linux tries.
    let started = ["/sbin/init", "/etc/init", "/bin/init", "/bin/sh"].iter().find(|path| match proc::spawn_init(path) {
        Ok(()) => {
            log!("started {path} as pid 1");
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
