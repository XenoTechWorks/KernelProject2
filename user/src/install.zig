//! install: puts KernelProject2 on a disk, so it boots from it without the ISO (UEFI).
//!
//! The disk gets a GPT partition table with two partitions: an EFI system partition (FAT32) with
//! Limine's UEFI program, its configuration, the kernel and the boot archive (all copied from the
//! running system: /boot holds what the bootloader loaded); and "KernelProject2 home", an empty
//! ext2 file system the kernel mounts as /root. Everything on the disk is erased.
//!
//!     install                 a window that asks which disk (in Vela)
//!     install --list          the disks
//!     install /dev/vdb        install there (asks you to type "yes" first)
//!     install --yes /dev/vdb  without asking

const std = @import("std");
const rt = @import("lib/rt.zig");
const gui = @import("lib/gui.zig");
const disk = @import("installer/disk.zig");
const gpt = @import("installer/gpt.zig");
const fat32 = @import("installer/fat32.zig");
const ext2 = @import("installer/ext2.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const limine_conf =
    \\timeout: 2
    \\
    \\/KernelProject2
    \\    protocol: limine
    \\    kernel_path: boot():/boot/kernel
    \\    module_path: boot():/boot/initramfs.cpio
    \\    module_cmdline: initramfs
    \\
    \\/KernelProject2 (command line only)
    \\    protocol: limine
    \\    kernel_path: boot():/boot/kernel
    \\    cmdline: text
    \\    module_path: boot():/boot/initramfs.cpio
    \\    module_cmdline: initramfs
    \\
;

/// What goes on the EFI system partition.
const boot_files = [_]fat32.Item{
    .{ .path = "EFI", .dir = true },
    .{ .path = "EFI/BOOT", .dir = true },
    .{ .path = "EFI/BOOT/BOOTX64.EFI", .source = "/usr/share/limine/BOOTX64.EFI" },
    .{ .path = "boot", .dir = true },
    .{ .path = "boot/limine", .dir = true },
    .{ .path = "boot/limine/limine.conf", .contents = limine_conf },
    .{ .path = "boot/kernel", .source = "/boot/kernel" },
    .{ .path = "boot/initramfs.cpio", .source = "/boot/initramfs.cpio" },
};

const mib = 1024 * 1024;

/// Reports how far the install has got: a fraction 0..1 and what it's doing.
pub const Report = struct {
    ctx: ?*anyopaque = null,
    f: *const fn (?*anyopaque, fraction: f32, what: []const u8) void,
};

var copy_report: Report = undefined;
var copy_total: u64 = 1;

fn copied(_: ?*anyopaque, bytes: u64) void {
    const f = 0.25 + 0.6 * @as(f32, @floatFromInt(bytes)) / @as(f32, @floatFromInt(copy_total));
    var buf: [64]u8 = undefined;
    const what = std.fmt.bufPrint(&buf, "Copying the system: {d} of {d} MiB", .{ bytes / mib, copy_total / mib }) catch "Copying the system";
    copy_report.f(copy_report.ctx, f, what);
}

fn sizeOf(path: [*:0]const u8) u64 {
    var st: sys.Stat = undefined;
    return if (sys.stat(path, &st) == .ok) @intCast(st.size) else 0;
}

pub const Failure = error{ NoDisk, InUse, TooSmall, Io, Source, OutOfMemory };

/// Installs onto `path` (a whole disk, e.g. /dev/vdb).
pub fn install(path: [*:0]const u8, report: Report) Failure!void {
    var mounted_buf: [16]u8 = undefined;
    if (disk.mountedDisk(&mounted_buf)) |m| {
        if (std.mem.eql(u8, std.mem.span(path)[5..@min(8, std.mem.span(path).len)], m)) return error.InUse;
    }
    var d = disk.Disk.open(path) orelse return error.NoDisk;
    defer d.close();

    // The boot partition: what it holds, with room to spare, from 1 MiB in. Home: the rest.
    var payload: u64 = 0;
    for (boot_files) |it| payload += if (it.source) |s| sizeOf(s) else it.contents.len;
    copy_total = sizeOf("/boot/kernel") + sizeOf("/boot/initramfs.cpio") + sizeOf("/usr/share/limine/BOOTX64.EFI");
    const esp_bytes = @max(64 * mib, (payload + payload / 8 + 16 * mib + mib - 1) / mib * mib);
    const span = gpt.usable(d.sectors());
    const esp_first: u64 = 2048;
    const esp_last = esp_first + esp_bytes / 512 - 1;
    const home_first = (esp_last + 1 + 2047) / 2048 * 2048;
    if (home_first + 64 * mib / 512 > span.last) return error.TooSmall;
    // Whole 4 KiB blocks for ext2.
    const home_last = home_first + (span.last - home_first + 1) / 8 * 8 - 1;

    report.f(report.ctx, 0.02, "Writing the partition table");
    gpt.write(d, &.{
        .{ .type_id = gpt.esp_type, .first = esp_first, .last = esp_last, .name = "EFI system partition" },
        .{ .type_id = gpt.linux_type, .first = home_first, .last = home_last, .name = "KernelProject2 home" },
    }) catch return error.Io;

    report.f(report.ctx, 0.08, "Making the home file system (ext2)");
    ext2.make(d, home_first * 512, (home_last - home_first + 1) * 512, "home") catch |e| return switch (e) {
        error.TooSmall => error.TooSmall,
        error.OutOfMemory => error.OutOfMemory,
        error.Io => error.Io,
    };

    report.f(report.ctx, 0.25, "Making the boot partition (FAT32)");
    copy_report = report;
    fat32.make(d, esp_first, esp_last - esp_first + 1, &boot_files, .{ .f = copied }) catch |e| return switch (e) {
        error.TooSmall => error.TooSmall,
        error.OutOfMemory => error.OutOfMemory,
        error.Io => error.Io,
        error.Source => error.Source,
    };
    _ = sys.syscall1(74, @bitCast(@as(isize, d.fd))); // fsync
    report.f(report.ctx, 1.0, "Done");
}

fn explain(e: Failure) []const u8 {
    return switch (e) {
        error.NoDisk => "there's no such disk",
        error.InUse => "that's the disk this system's /root is on",
        error.TooSmall => "the disk is too small (it needs room for the system and at least 64 MiB for home)",
        error.Io => "writing to the disk failed",
        error.Source => "the system's own files (/boot) couldn't be read",
        error.OutOfMemory => "out of memory",
    };
}

// ---- the command line ----

fn printReport(_: ?*anyopaque, fraction: f32, what: []const u8) void {
    rt.out.print("\r[{d:>3}%] {s}\x1b[K", .{ @as(u32, @intFromFloat(fraction * 100)), what });
    rt.out.flush();
}

fn listDisks() void {
    var found: [8]disk.Info = undefined;
    const n = disk.list(&found);
    if (n == 0) rt.out.write("No disks.\n");
    for (found[0..n]) |inf| {
        rt.out.print("/dev/{s}  {d} MiB{s}\n", .{ inf.name[0..3], inf.size / mib, if (inf.in_use) "  (holds /root: can't install here)" else "" });
    }
}

pub fn main(args: rt.Args) u8 {
    var yes = false;
    var target: ?[*:0]const u8 = null;
    for (args.argv[1..]) |a| {
        const s = std.mem.span(a);
        if (std.mem.eql(u8, s, "--list")) {
            listDisks();
            return 0;
        } else if (std.mem.eql(u8, s, "--yes")) {
            yes = true;
        } else if (std.mem.startsWith(u8, s, "/dev/")) {
            target = a;
        } else {
            rt.err.write("usage: install [--list] [--yes] [/dev/vdX]\n");
            return 2;
        }
    }
    const path = target orelse return window();
    if (!yes) {
        rt.out.print("Everything on {s} will be erased. Type yes to install KernelProject2 there: ", .{std.mem.span(path)});
        rt.out.flush();
        var answer: [16]u8 = undefined;
        const n = switch (sys.read(0, &answer)) {
            .ok => |n| n,
            .err => 0,
        };
        if (!std.mem.eql(u8, std.mem.trim(u8, answer[0..n], " \r\n"), "yes")) {
            rt.out.write("Not installed.\n");
            return 1;
        }
    }
    install(path, .{ .f = printReport }) catch |e| {
        rt.out.write("\n");
        rt.err.print("install: {s}\n", .{explain(e)});
        return 1;
    };
    rt.out.print("\nKernelProject2 is installed on {s}. Start the computer from that disk (UEFI) to use it.\n", .{std.mem.span(path)});
    return 0;
}

// ---- the window ----

var app: *gui.App = undefined;
var disk_list: *gui.Widget = undefined;
var erase_check: *gui.Widget = undefined;
var bar: *gui.Widget = undefined;
var status: *gui.Widget = undefined;
var infos: [8]disk.Info = undefined;
var choices: [8]usize = undefined; // which infos the list shows (the ones that can be targets)
var shown: usize = 0;
var item_text: [8][64]u8 = undefined;

fn windowReport(_: ?*anyopaque, fraction: f32, what: []const u8) void {
    gui.setProgress(bar, fraction);
    gui.setText(status, what);
    app.redraw();
}

fn start(_: *gui.App, _: ?*anyopaque) void {
    const pick = gui.selected(disk_list) orelse {
        gui.setText(status, "Choose a disk first.");
        return;
    };
    if (!erase_check.kind.check.on) {
        gui.setText(status, "Tick \"Erase everything on this disk\" to go ahead.");
        return;
    }
    var path = "/dev/vda\x00".*;
    @memcpy(path[5..8], infos[choices[pick]].name[0..3]);
    install(@ptrCast(&path), .{ .f = windowReport }) catch |e| {
        var buf: [160]u8 = undefined;
        gui.setText(status, std.fmt.bufPrint(&buf, "Not installed: {s}.", .{explain(e)}) catch "Not installed.");
        return;
    };
    gui.setText(status, "Installed. Start the computer from that disk (UEFI) to use it.");
}

fn window() u8 {
    app = gui.App.init("Install KernelProject2") orelse {
        rt.err.write("install: no window (try install --list, install /dev/vdX)\n");
        return 1;
    };
    const n = disk.list(&infos);
    var labels: [8][]const u8 = undefined;
    for (infos[0..n], 0..) |inf, i| {
        if (inf.in_use) continue;
        choices[shown] = i;
        labels[shown] = std.fmt.bufPrint(&item_text[shown], "/dev/{s}  ({d} MiB)", .{ inf.name[0..3], inf.size / mib }) catch "disk";
        shown += 1;
    }
    disk_list = app.list();
    gui.setItems(disk_list, labels[0..shown]);
    erase_check = app.check("Erase everything on this disk", false, null);
    bar = app.progress(0);
    status = app.label(if (shown == 0) "There's no other disk to install on (the one /root is on can't be used)." else "", .{ .dim = true });
    const root = app.vbox(.{ .padding = 24, .spacing = 12 }, &.{
        app.label("Install KernelProject2", .{ .size = 22 }),
        app.label("Puts the system on a disk so it starts without the ISO (UEFI). Your home folder there starts empty.", .{ .dim = true }),
        app.label("Disk", .{}),
        disk_list,
        erase_check,
        bar,
        status,
        app.hbox(.{ .alignment = .end }, &.{app.suggestedButton("Install", .{ .f = start })}),
    });
    return app.run(root, 520, 420);
}
