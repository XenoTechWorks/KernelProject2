//! pkg: install and remove packages, keeping track of what each one put on the disk.
//!
//!     pkg info hello.deb       what's in it
//!     pkg install hello.deb    unpack it onto the system
//!     pkg list                 what's installed
//!     pkg files hello          what a package put there
//!     pkg remove hello         take it away again
//!
//! Three kinds of package:
//!   .deb          a Linux package: an `ar` archive holding control.tar.gz and data.tar.gz
//!   .zip          a Windows program kept as it is: unpacked into /opt/NAME, with its .exe
//!                 files reachable by name (they run through the Windows layer, see the README)
//!   .exe          one Windows program, copied into /bin
//!
//! What a package installed is written down in /var/lib/pkg, so `pkg remove` can undo it.

const std = @import("std");
const rt = @import("lib/rt.zig");
const file = @import("lib/file.zig");
const gzip = @import("lib/gzip.zig");
const tar = @import("lib/tar.zig");
const deflate = @import("lib/deflate.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const gpa = std.heap.page_allocator;
const db = "/var/lib/pkg";

const usage =
    \\usage: pkg install FILE | remove NAME | list | files NAME | info FILE
    \\  FILE is a .deb (Linux), or a .zip or .exe (Windows).
    \\
;

pub fn main(args: rt.Args) u8 {
    const command = args.get(1) orelse {
        rt.err.write(usage);
        return 2;
    };
    const arg = args.get(2);
    if (std.mem.eql(u8, command, "list")) return list();
    if (std.mem.eql(u8, command, "install")) return install(arg orelse return missing());
    if (std.mem.eql(u8, command, "remove")) return remove(arg orelse return missing());
    if (std.mem.eql(u8, command, "files")) return showFiles(arg orelse return missing());
    if (std.mem.eql(u8, command, "info")) return info(arg orelse return missing());
    rt.err.write(usage);
    return 2;
}

fn missing() u8 {
    rt.err.write(usage);
    return 2;
}

// ---- the `ar` archive a .deb is made of ----

const Member = struct { name: []const u8, data: []const u8 };

/// Finds a member of an `ar` archive whose name starts with `prefix`.
fn arMember(data: []const u8, prefix: []const u8) ?Member {
    if (!std.mem.startsWith(u8, data, "!<arch>\n")) return null;
    var at: usize = 8;
    while (at + 60 <= data.len) {
        const name = std.mem.trimEnd(u8, data[at..][0..16], " /");
        var size: usize = 0;
        for (std.mem.trim(u8, data[at + 48 ..][0..10], " ")) |c| {
            if (c < '0' or c > '9') break;
            size = size * 10 + (c - '0');
        }
        at += 60;
        if (at + size > data.len) return null;
        if (std.mem.startsWith(u8, name, prefix)) return .{ .name = name, .data = data[at..][0..size] };
        at += size + (size & 1); // members start on an even byte
    }
    return null;
}

/// A .deb's data or control part, unpacked. Says what to do when it's compressed some other way.
fn debPart(data: []const u8, which: []const u8) ?[]u8 {
    const member = arMember(data, which) orelse {
        rt.err.print("pkg: no {s} part in this .deb\n", .{which});
        return null;
    };
    if (std.mem.endsWith(u8, member.name, ".gz")) {
        return gzip.decompress(member.data) catch {
            rt.err.print("pkg: the {s} part is damaged\n", .{which});
            return null;
        };
    }
    if (std.mem.endsWith(u8, member.name, ".tar")) return gpa.dupe(u8, member.data) catch null;
    rt.err.print("pkg: {s} is compressed with {s}, which isn't supported yet (only .gz)\n", .{ member.name, std.fs.path.extension(member.name) });
    return null;
}

/// A field out of a .deb's control file ("Package: hello" -> "hello").
fn controlField(control: []const u8, field: []const u8) ?[]const u8 {
    var lines = std.mem.splitScalar(u8, control, '\n');
    while (lines.next()) |line| {
        if (line.len > field.len + 1 and std.ascii.eqlIgnoreCase(line[0..field.len], field) and line[field.len] == ':') {
            return std.mem.trim(u8, line[field.len + 1 ..], " \r");
        }
    }
    return null;
}

/// The control file inside a .deb's control.tar.gz.
fn debControl(data: []const u8) ?[]u8 {
    const control_tar = debPart(data, "control") orelse return null;
    defer gpa.free(control_tar);
    var r = tar.Reader{ .data = control_tar };
    defer r.deinit();
    while (r.next()) |e| {
        defer r.clearLong();
        const base = e.name[if (std.mem.lastIndexOfScalar(u8, e.name, '/')) |i| i + 1 else 0..];
        if (std.mem.eql(u8, base, "control")) return gpa.dupe(u8, e.data) catch null;
    }
    return null;
}

// ---- what's installed ----

fn packageName(path: []const u8) []const u8 {
    var base = path[if (std.mem.lastIndexOfScalar(u8, path, '/')) |i| i + 1 else 0..];
    for ([_][]const u8{ ".deb", ".zip", ".exe", ".tar.gz", ".tgz" }) |ext| {
        if (std.ascii.endsWithIgnoreCase(base, ext)) base = base[0 .. base.len - ext.len];
    }
    // "hello_1.0_amd64" is the usual .deb naming: the name is the part before the first _.
    if (std.mem.indexOfScalar(u8, base, '_')) |i| base = base[0..i];
    return base;
}

fn listPath(buf: []u8, name: []const u8) ?[*:0]const u8 {
    var tmp: [1100]u8 = undefined;
    const s = std.fmt.bufPrint(&tmp, "{s}/{s}.list", .{ db, name }) catch return null;
    return rt.cstr(buf, s);
}

fn save(name: []const u8, files: []const u8, description: []const u8) void {
    file.makePath(db);
    var buf: [1100]u8 = undefined;
    if (listPath(&buf, name)) |p| file.write(p, files, 0o644) catch {};
    var tmp: [1100]u8 = undefined;
    const info_path = std.fmt.bufPrint(&tmp, "{s}/{s}.info", .{ db, name }) catch return;
    var ibuf: [1100]u8 = undefined;
    if (rt.cstr(&ibuf, info_path)) |p| file.write(p, description, 0o644) catch {};
}

fn list() u8 {
    var buf: [1100]u8 = undefined;
    const p = rt.cstr(&buf, db) orelse return 1;
    const fd: i32 = switch (sys.open(p, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => {
            rt.out.write("No packages installed.\n");
            return 0;
        },
    };
    defer sys.close(fd);
    var dents: [8192]u8 align(8) = undefined;
    var found = false;
    while (true) {
        const n = switch (sys.check(sys.syscall3(217, @bitCast(@as(isize, fd)), @intFromPtr(&dents), dents.len))) {
            .ok => |n| n,
            .err => break,
        };
        if (n == 0) break;
        var at: usize = 0;
        while (at < n) {
            const reclen = std.mem.readInt(u16, dents[at + 16 ..][0..2], .little);
            const entry = std.mem.sliceTo(dents[at + 19 .. at + reclen], 0);
            at += reclen;
            if (!std.mem.endsWith(u8, entry, ".info")) continue;
            const name = entry[0 .. entry.len - 5];
            var ibuf: [1100]u8 = undefined;
            var tmp: [1100]u8 = undefined;
            const ipath = std.fmt.bufPrint(&tmp, "{s}/{s}", .{ db, entry }) catch continue;
            const text = if (rt.cstr(&ibuf, ipath)) |ip| file.read(ip) catch null else null;
            defer if (text) |t| gpa.free(t);
            const first = if (text) |t| std.mem.sliceTo(t, '\n') else "";
            rt.out.print("{s: <20} {s}\n", .{ name, first });
            found = true;
        }
    }
    if (!found) rt.out.write("No packages installed.\n");
    return 0;
}

fn showFiles(name: []const u8) u8 {
    var buf: [1100]u8 = undefined;
    const p = listPath(&buf, name) orelse return 1;
    const text = file.read(p) catch {
        rt.err.print("pkg: {s} isn't installed\n", .{name});
        return 1;
    };
    defer gpa.free(text);
    rt.out.write(text);
    return 0;
}

fn remove(name: []const u8) u8 {
    var buf: [1100]u8 = undefined;
    const p = listPath(&buf, name) orelse return 1;
    const text = file.read(p) catch {
        rt.err.print("pkg: {s} isn't installed\n", .{name});
        return 1;
    };
    defer gpa.free(text);
    var n: usize = 0;
    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |line| {
        if (line.len == 0) continue;
        var fbuf: [1100]u8 = undefined;
        var full: [1100]u8 = undefined;
        // The list holds names as the archive had them, relative to /.
        const path = std.fmt.bufPrint(&full, "/{s}", .{std.mem.trimStart(u8, line, "/")}) catch continue;
        if (rt.cstr(&fbuf, path)) |fp| {
            file.remove(fp);
            n += 1;
        }
    }
    file.remove(p);
    var tmp: [1100]u8 = undefined;
    const ipath = std.fmt.bufPrint(&tmp, "{s}/{s}.info", .{ db, name }) catch return 0;
    var ibuf: [1100]u8 = undefined;
    if (rt.cstr(&ibuf, ipath)) |ip| file.remove(ip);
    rt.out.print("Removed {s} ({d} files).\n", .{ name, n });
    return 0;
}

// ---- looking inside, and installing ----

fn readPackage(path: []const u8) ?[]u8 {
    var buf: [1100]u8 = undefined;
    const p = rt.cstr(&buf, path) orelse return null;
    return file.read(p) catch {
        rt.err.print("pkg: can't read {s}\n", .{path});
        return null;
    };
}

fn info(path: []const u8) u8 {
    const data = readPackage(path) orelse return 1;
    defer gpa.free(data);
    if (std.ascii.endsWithIgnoreCase(path, ".deb")) {
        const control = debControl(data) orelse return 1;
        defer gpa.free(control);
        rt.out.write(control);
        return 0;
    }
    rt.out.print("{s}: a Windows package ({s})\n", .{
        packageName(path),
        if (std.ascii.endsWithIgnoreCase(path, ".zip")) "a folder of files" else "one program",
    });
    return 0;
}

fn install(path: []const u8) u8 {
    const data = readPackage(path) orelse return 1;
    defer gpa.free(data);
    const name = packageName(path);
    if (std.ascii.endsWithIgnoreCase(path, ".deb")) return installDeb(data, name);
    if (std.ascii.endsWithIgnoreCase(path, ".zip")) return installZip(data, name);
    if (std.ascii.endsWithIgnoreCase(path, ".exe")) return installExe(data, name);
    if (std.mem.startsWith(u8, data, "!<arch>")) return installDeb(data, name);
    rt.err.print("pkg: {s}: not a package this knows (.deb, .zip or .exe)\n", .{path});
    return 1;
}

fn installDeb(data: []const u8, fallback_name: []const u8) u8 {
    var name = fallback_name;
    var description: []const u8 = "";
    const control = debControl(data);
    defer if (control) |c| gpa.free(c);
    if (control) |c| {
        if (controlField(c, "Package")) |p| name = p;
        if (controlField(c, "Description")) |d| description = d;
    }
    const data_tar = debPart(data, "data") orelse return 1;
    defer gpa.free(data_tar);

    const here = pushDir("/") orelse return 1;
    var record = std.ArrayList(u8).empty;
    defer record.deinit(gpa);
    const failed = tar.extract(data_tar, &record, true);
    popDir(here);
    save(name, record.items, description);
    var lines: usize = 0;
    for (record.items) |c| {
        if (c == '\n') lines += 1;
    }
    rt.out.print("Installed {s} ({d} files).\n", .{ name, lines });
    return if (failed > 0) 1 else 0;
}

/// A Windows program kept as it is: unpacked into /opt/NAME, and each .exe at its top level
/// gets a little launcher in /bin so you can run it by name.
fn installZip(data: []const u8, name: []const u8) u8 {
    var dir: [1100]u8 = undefined;
    const where = std.fmt.bufPrint(&dir, "/opt/{s}", .{name}) catch return 1;
    file.makePath(where);
    const here = pushDir(where) orelse return 1;
    var record = std.ArrayList(u8).empty;
    defer record.deinit(gpa);
    const status = unzipInto(data, where, &record);
    popDir(here);

    // Each program it brought gets its name in /bin, as a link to the real thing.
    var lines = std.mem.splitScalar(u8, record.items, '\n');
    while (lines.next()) |line| {
        if (!std.ascii.endsWithIgnoreCase(line, ".exe")) continue;
        const base = line[if (std.mem.lastIndexOfScalar(u8, line, '/')) |i| i + 1 else 0..];
        const stem = base[0 .. base.len - 4];
        var tpath: [1200]u8 = undefined;
        const target = std.fmt.bufPrint(&tpath, "{s}/{s}", .{ where, line }) catch continue;
        var lpath: [1100]u8 = undefined;
        const launcher = std.fmt.bufPrint(&lpath, "/bin/{s}", .{stem}) catch continue;
        var tbuf: [1200]u8 = undefined;
        var lbuf: [1100]u8 = undefined;
        if (rt.cstr(&tbuf, target)) |t| if (rt.cstr(&lbuf, launcher)) |l| {
            file.remove(l);
            if (sys.check(sys.syscall3(266, @intFromPtr(t), @bitCast(@as(isize, -100)), @intFromPtr(l))) == .err) continue;
            record.appendSlice(gpa, launcher[1..]) catch {};
            record.append(gpa, '\n') catch {};
            rt.out.print("  {s} runs {s}\n", .{ stem, base });
        };
    }
    save(name, record.items, "a Windows program");
    rt.out.print("Installed {s} into {s}.\n", .{ name, where });
    return status;
}

/// One Windows program: into /bin, where the kernel will run it through the Windows layer.
fn installExe(data: []const u8, name: []const u8) u8 {
    if (!std.mem.startsWith(u8, data, "MZ")) {
        rt.err.write("pkg: that doesn't look like a Windows program\n");
        return 1;
    }
    var buf: [1100]u8 = undefined;
    var tmp: [1100]u8 = undefined;
    const dest = std.fmt.bufPrint(&tmp, "/bin/{s}.exe", .{name}) catch return 1;
    const p = rt.cstr(&buf, dest) orelse return 1;
    file.write(p, data, 0o755) catch {
        rt.err.print("pkg: can't write {s}\n", .{dest});
        return 1;
    };
    var record: [1100]u8 = undefined;
    const line = std.fmt.bufPrint(&record, "{s}\n", .{dest[1..]}) catch return 1;
    save(name, line, "a Windows program");
    rt.out.print("Installed {s} as {s}.\n", .{ name, dest });
    return 0;
}

/// Unpacks a .zip into the current directory, noting the paths relative to `prefix`.
fn unzipInto(data: []const u8, prefix: []const u8, record: *std.ArrayList(u8)) u8 {
    var eocd: ?usize = null;
    var at = data.len -| 22;
    while (true) {
        if (at + 22 <= data.len and std.mem.readInt(u32, data[at..][0..4], .little) == 0x06054b50) {
            eocd = at;
            break;
        }
        if (at == 0) break;
        at -= 1;
    }
    const end = eocd orelse {
        rt.err.write("pkg: that doesn't look like a .zip\n");
        return 1;
    };
    const count = std.mem.readInt(u16, data[end + 10 ..][0..2], .little);
    var entry: usize = std.mem.readInt(u32, data[end + 16 ..][0..4], .little);
    var status: u8 = 0;
    for (0..count) |_| {
        if (entry + 46 > data.len or std.mem.readInt(u32, data[entry..][0..4], .little) != 0x02014b50) break;
        const method = std.mem.readInt(u16, data[entry + 10 ..][0..2], .little);
        const packed_size = std.mem.readInt(u32, data[entry + 20 ..][0..4], .little);
        const name_len = std.mem.readInt(u16, data[entry + 28 ..][0..2], .little);
        const extra_len = std.mem.readInt(u16, data[entry + 30 ..][0..2], .little);
        const comment_len = std.mem.readInt(u16, data[entry + 32 ..][0..2], .little);
        const local = std.mem.readInt(u32, data[entry + 42 ..][0..4], .little);
        const name = data[entry + 46 ..][0..name_len];
        entry += 46 + @as(usize, name_len) + extra_len + comment_len;
        if (!tar.safeName(name)) continue;
        if (name[name.len - 1] == '/') {
            file.makePath(name[0 .. name.len - 1]);
            continue;
        }
        if (local + 30 > data.len) continue;
        const body = local + 30 + @as(usize, std.mem.readInt(u16, data[local + 26 ..][0..2], .little)) +
            std.mem.readInt(u16, data[local + 28 ..][0..2], .little);
        if (body + packed_size > data.len) continue;
        const raw = data[body..][0..packed_size];
        var contents: []const u8 = raw;
        var inflated: ?[]u8 = null;
        defer if (inflated) |x| gpa.free(x);
        if (method == 8) {
            inflated = deflate.inflate(raw) catch {
                status = 1;
                continue;
            };
            contents = inflated.?;
        } else if (method != 0) {
            status = 1;
            continue;
        }
        file.makeParents(name);
        var nbuf: [1100]u8 = undefined;
        const dest = rt.cstr(&nbuf, name) orelse continue;
        const mode: usize = if (std.ascii.endsWithIgnoreCase(name, ".exe") or std.ascii.endsWithIgnoreCase(name, ".dll")) 0o755 else 0o644;
        file.write(dest, contents, mode) catch {
            status = 1;
            continue;
        };
        record.appendSlice(gpa, name) catch {};
        record.append(gpa, '\n') catch {};
        _ = prefix;
    }
    return status;
}

// ---- moving about ----

fn pushDir(to: []const u8) ?[]u8 {
    var cwd: [1100]u8 = undefined;
    const here = switch (sys.check(sys.syscall2(sys.nr.getcwd, @intFromPtr(&cwd), cwd.len))) {
        .ok => |n| gpa.dupe(u8, cwd[0 .. n - 1]) catch return null,
        .err => gpa.dupe(u8, "/") catch return null,
    };
    var buf: [1100]u8 = undefined;
    const p = rt.cstr(&buf, to) orelse return null;
    if (sys.check(sys.syscall1(sys.nr.chdir, @intFromPtr(p))) == .err) {
        rt.err.print("pkg: can't work in {s}\n", .{to});
        gpa.free(here);
        return null;
    }
    return here;
}

fn popDir(here: []u8) void {
    defer gpa.free(here);
    var buf: [1100]u8 = undefined;
    if (rt.cstr(&buf, here)) |p| _ = sys.syscall1(sys.nr.chdir, @intFromPtr(p));
}
