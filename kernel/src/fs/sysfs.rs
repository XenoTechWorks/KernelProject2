//! /sys: the few device and kernel files programs read today, set up once at boot.

use super::{Kind, Node, install};
use crate::smbios::Dmi;
use alloc::format;

pub fn init(dmi: &Dmi) {
    let text = |s: &str| Node::file(format!("{s}\n").into_bytes(), 0o444);
    for (name, value) in &dmi.fields {
        install(&format!("/sys/devices/virtual/dmi/id/{name}"), text(value));
    }
    install("/sys/class/dmi/id", Node::new(Kind::Symlink("../../devices/virtual/dmi/id".into()), 0o777));
    for name in ["online", "possible", "present"] {
        install(&format!("/sys/devices/system/cpu/{name}"), text("0"));
    }
    install("/sys/kernel/mm/transparent_hugepage/enabled", text("always madvise [never]"));
    super::mkdir_p("/sys/class/power_supply");
    super::mkdir_p("/sys/class/drm");
    super::mkdir_p("/sys/class/block");
}
