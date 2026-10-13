//! /sys: the few device and kernel files programs read today, set up once at boot.

use super::{Kind, Node, install};
use crate::smbios::Dmi;
use alloc::format;

/// /sys/devices/system/cpu/{online,possible,present}: "0" or "0-3".
pub fn cpus(count: usize) {
    let range = if count <= 1 { format!("0\n") } else { format!("0-{}\n", count - 1) };
    for name in ["online", "possible", "present"] {
        install(&format!("/sys/devices/system/cpu/{name}"), Node::file(range.clone().into_bytes(), 0o444));
    }
}

pub fn init(dmi: &Dmi) {
    let text = |s: &str| Node::file(format!("{s}\n").into_bytes(), 0o444);
    for (name, value) in &dmi.fields {
        install(&format!("/sys/devices/virtual/dmi/id/{name}"), text(value));
    }
    install("/sys/class/dmi/id", Node::new(Kind::Symlink("../../devices/virtual/dmi/id".into()), 0o777));
    cpus(1);
    install("/sys/kernel/mm/transparent_hugepage/enabled", text("always madvise [never]"));
    super::mkdir_p("/sys/class/power_supply");
    super::mkdir_p("/sys/class/drm");
    super::mkdir_p("/sys/class/block");
}
