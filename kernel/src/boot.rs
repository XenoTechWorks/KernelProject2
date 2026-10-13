//! Boot messages, the way Linux distributions show them: one line per step, "[  OK  ]" when it
//! worked, "[ WARN ]" when the system carries on without it, "[FAILED]" when it didn't work, on the
//! screen and the serial port. And the kernel command line (/proc/cmdline).

use alloc::format;
use alloc::string::String;
use spin::Once;

pub enum Status {
    Ok,
    Warn,
    Failed,
}

pub fn report(status: Status, text: &str) {
    let tag = match status {
        Status::Ok => "\x1b[1;32m  OK  \x1b[0m",
        Status::Warn => "\x1b[1;33m WARN \x1b[0m",
        Status::Failed => "\x1b[1;31mFAILED\x1b[0m",
    };
    let line = format!("[{tag}] {text}\n");
    crate::arch::serial::write_str(&line);
    // Under the splash too: any key shows them.
    crate::console::write(line.as_bytes());
    if crate::console::splash_up() {
        crate::console::splash_step(text);
    }
}

/// Whether `option` is one of the kernel command line's words.
pub fn option(option: &str) -> bool {
    cmdline().split_whitespace().any(|w| w == option)
}

pub fn ok(text: &str) {
    report(Status::Ok, text);
}

pub fn warn(text: &str) {
    report(Status::Warn, text);
}

static CMDLINE: Once<String> = Once::new();

/// The kernel command line: the bootloader's (limine.conf), then whatever QEMU adds with
/// `-fw_cfg name=opt/kernelproject2/cmdline,string=...`.
pub fn set_cmdline(from_bootloader: &str) {
    let mut line = String::from(from_bootloader.trim());
    if let Some(extra) = crate::arch::fwcfg::read_file("opt/kernelproject2/cmdline") {
        if let Ok(extra) = core::str::from_utf8(&extra) {
            if !line.is_empty() && !extra.trim().is_empty() {
                line.push(' ');
            }
            line.push_str(extra.trim());
        }
    }
    CMDLINE.call_once(|| line);
}

pub fn cmdline() -> &'static str {
    CMDLINE.get().map_or("", |s| s.as_str())
}
