//! Software's packages: the catalog a server offers, and the ones installed here.
//!
//! A server (software/build.py makes one; run.sh serves it) has an `index` (one paragraph of
//! "Key: value" lines per package), NAME.tar.gz for each package and icons/NAME.png.
//!
//! An installed package is unpacked into /root/.local/software/NAME, on the disk, so it's still
//! there after a restart. Its .package/links names the paths it puts on the system; each becomes
//! a symlink into that folder. The root filesystem is rebuilt from the boot archive every boot,
//! so init has them linked again each time (`pkg activate`).

const std = @import("std");
const rt = @import("rt.zig");
const file = @import("file.zig");
const gzip = @import("gzip.zig");
const tar = @import("tar.zig");
const http = @import("http.zig");
const sys = rt.sys;

const gpa = std.heap.page_allocator;

pub const store = "/root/.local/software";
/// The build machine, as QEMU's user network shows it, where run.sh serves out/software.
pub const default_server = "http://10.0.2.2:8086/";
/// `server=URL` here points Software somewhere else.
pub const config = "/root/.config/software.conf";

pub const Package = struct {
    name: []const u8 = "",
    title: []const u8 = "",
    category: []const u8 = "",
    version: []const u8 = "",
    about: []const u8 = "",
    run: []const u8 = "",
    terminal: bool = false,
    /// Download, and unpacked, in bytes.
    size: usize = 0,
    installed_size: usize = 0,
    /// Other packages it needs, comma-separated ("mesa, neverball").
    depends: []const u8 = "",
};

/// One paragraph of an index (or a package's .package/info).
pub fn parseOne(paragraph: []const u8) Package {
    var p = Package{};
    var lines = std.mem.splitScalar(u8, paragraph, '\n');
    while (lines.next()) |line| {
        const colon = std.mem.indexOf(u8, line, ": ") orelse continue;
        const key = line[0..colon];
        const value = std.mem.trimEnd(u8, line[colon + 2 ..], "\r ");
        if (std.mem.eql(u8, key, "Package")) p.name = value;
        if (std.mem.eql(u8, key, "Title")) p.title = value;
        if (std.mem.eql(u8, key, "Category")) p.category = value;
        if (std.mem.eql(u8, key, "Version")) p.version = value;
        if (std.mem.eql(u8, key, "About")) p.about = value;
        if (std.mem.eql(u8, key, "Run")) p.run = value;
        if (std.mem.eql(u8, key, "Terminal")) p.terminal = std.mem.eql(u8, value, "yes");
        if (std.mem.eql(u8, key, "Size")) p.size = std.fmt.parseInt(usize, value, 10) catch 0;
        if (std.mem.eql(u8, key, "Installed-Size")) p.installed_size = std.fmt.parseInt(usize, value, 10) catch 0;
        if (std.mem.eql(u8, key, "Depends")) p.depends = value;
    }
    return p;
}

/// The packages in an index, into `out` (they point into `text`). Returns how many.
pub fn parseIndex(text: []const u8, out: []Package) usize {
    var n: usize = 0;
    var paragraphs = std.mem.splitSequence(u8, text, "\n\n");
    while (paragraphs.next()) |par| {
        if (n == out.len) break;
        const p = parseOne(par);
        if (validName(p.name)) {
            out[n] = p;
            n += 1;
        }
    }
    return n;
}

/// The names in a Depends list, one at a time.
pub fn dependencies(p: Package) std.mem.TokenIterator(u8, .any) {
    return std.mem.tokenizeAny(u8, p.depends, ", ");
}

/// An installed package that needs `name`, if there is one (its name into `buf`).
pub fn neededBy(name: []const u8, buf: []u8) ?[]const u8 {
    var names_buf: [8192]u8 = undefined;
    var names: [256][]const u8 = undefined;
    const n = installedNames(&names_buf, &names);
    for (names[0..n]) |other| {
        const info = installedInfo(other) orelse continue;
        defer std.heap.page_allocator.free(info);
        var deps = dependencies(parseOne(info));
        while (deps.next()) |d| {
            if (std.mem.eql(u8, d, name)) {
                const len = @min(other.len, buf.len);
                @memcpy(buf[0..len], other[0..len]);
                return buf[0..len];
            }
        }
    }
    return null;
}

/// Names go into paths: lowercase letters, digits and - . + only.
pub fn validName(name: []const u8) bool {
    if (name.len == 0 or name.len > 64 or name[0] == '.') return false;
    for (name) |c| {
        if (!(std.ascii.isLower(c) or std.ascii.isDigit(c) or c == '-' or c == '.' or c == '+')) return false;
    }
    return true;
}

// ---- the server ----

/// The server's address, ending in "/".
pub fn server(buf: []u8) []const u8 {
    var tmp: [512]u8 = undefined;
    const fd: i32 = switch (sys.open(config, sys.O_RDONLY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return default_server,
    };
    defer sys.close(fd);
    const n = switch (sys.read(fd, &tmp)) {
        .ok => |n| n,
        .err => return default_server,
    };
    var lines = std.mem.splitScalar(u8, tmp[0..n], '\n');
    while (lines.next()) |line| {
        const l = std.mem.trim(u8, line, " \r\t");
        if (!std.mem.startsWith(u8, l, "server=")) continue;
        const url = l["server=".len..];
        const slash: []const u8 = if (std.mem.endsWith(u8, url, "/")) "" else "/";
        return std.fmt.bufPrint(buf, "{s}{s}", .{ url, slash }) catch default_server;
    }
    return default_server;
}

/// Something on the server: "index", "NAME.tar.gz", "icons/NAME.png". The caller frees it.
pub fn fetch(what: []const u8, progress: ?http.Progress) http.Error![]u8 {
    var sbuf: [512]u8 = undefined;
    var ubuf: [700]u8 = undefined;
    const url = std.fmt.bufPrint(&ubuf, "{s}{s}", .{ server(&sbuf), what }) catch return error.BadUrl;
    return http.get(url, progress);
}

// ---- installed packages ----

fn storePath(buf: []u8, name: []const u8, rest: []const u8) ?[*:0]const u8 {
    return (std.fmt.bufPrintSentinel(buf, "{s}/{s}{s}", .{ store, name, rest }, 0) catch return null).ptr;
}

/// What an installed package's .package/info says (the caller frees it), or null when it isn't.
pub fn installedInfo(name: []const u8) ?[]u8 {
    if (!validName(name)) return null;
    var buf: [600]u8 = undefined;
    return file.read(storePath(&buf, name, "/.package/info") orelse return null) catch null;
}

pub fn isInstalled(name: []const u8) bool {
    const text = installedInfo(name) orelse return false;
    gpa.free(text);
    return true;
}

/// The names of the installed packages, one after another into `buf`; returns how many, each
/// in `names`.
pub fn installedNames(buf: []u8, names: [][]const u8) usize {
    const fd: i32 = switch (sys.open(store, sys.O_RDONLY | sys.O_DIRECTORY, 0)) {
        .ok => |fd| @intCast(fd),
        .err => return 0,
    };
    defer sys.close(fd);
    var count: usize = 0;
    var used: usize = 0;
    var dents: [8192]u8 align(8) = undefined;
    while (true) {
        const n = switch (sys.check(sys.syscall3(sys.nr.getdents64, @bitCast(@as(isize, fd)), @intFromPtr(&dents), dents.len))) {
            .ok => |n| n,
            .err => break,
        };
        if (n == 0) break;
        var at: usize = 0;
        while (at < n) {
            const reclen = std.mem.readInt(u16, dents[at + 16 ..][0..2], .little);
            const name = std.mem.sliceTo(dents[at + 19 .. at + reclen], 0);
            at += reclen;
            if (!validName(name) or count == names.len or used + name.len > buf.len) continue;
            if (!isInstalled(name)) continue; // half unpacked, or not a package
            @memcpy(buf[used..][0..name.len], name);
            names[count] = buf[used..][0..name.len];
            used += name.len;
            count += 1;
        }
    }
    std.mem.sort([]const u8, names[0..count], {}, struct {
        fn less(_: void, a: []const u8, b: []const u8) bool {
            return std.mem.lessThan(u8, a, b);
        }
    }.less);
    return count;
}

pub const Linked = struct { linked: usize = 0, taken: usize = 0 };

/// Puts an installed package's links in place: each path it names becomes a symlink into its
/// folder, unless something is there already. That's fine when it's another package's copy of
/// a file both have (a library); anything else is counted in `taken`.
pub fn activate(name: []const u8) Linked {
    var result = Linked{};
    var buf: [600]u8 = undefined;
    const links = file.read(storePath(&buf, name, "/.package/links") orelse return result) catch return result;
    defer gpa.free(links);
    var lines = std.mem.splitScalar(u8, links, '\n');
    while (lines.next()) |rel| {
        if (!tar.safeName(rel)) continue;
        var dbuf: [1100]u8 = undefined;
        var tbuf: [1300]u8 = undefined;
        const dest = std.fmt.bufPrintSentinel(&dbuf, "/{s}", .{rel}, 0) catch continue;
        const target = std.fmt.bufPrintSentinel(&tbuf, "{s}/{s}/{s}", .{ store, name, rel }, 0) catch continue;
        var st: sys.Stat = undefined;
        if (sys.lstat(dest, &st) == .ok) {
            var lbuf: [1300]u8 = undefined;
            const there = readLink(dest, &lbuf) orelse "";
            if (std.mem.eql(u8, there, target) or std.mem.startsWith(u8, there, store ++ "/")) {
                result.linked += 1;
            } else {
                result.taken += 1;
            }
            continue;
        }
        file.makeParents(dest);
        if (sys.check(sys.syscall3(266, @intFromPtr(target.ptr), @bitCast(@as(isize, -100)), @intFromPtr(dest.ptr))) == .ok) // symlinkat
            result.linked += 1
        else
            result.taken += 1;
    }
    return result;
}

fn readLink(path: [*:0]const u8, buf: []u8) ?[]const u8 {
    return switch (sys.check(sys.syscall3(sys.nr.readlink, @intFromPtr(path), @intFromPtr(buf.ptr), buf.len))) {
        .ok => |n| buf[0..n],
        .err => null,
    };
}

/// Links every installed package; returns how many there are.
pub fn activateAll() usize {
    var buf: [8192]u8 = undefined;
    var names: [256][]const u8 = undefined;
    const n = installedNames(&buf, &names);
    for (names[0..n]) |name| _ = activate(name);
    return n;
}

pub const InstallError = error{ NotAPackage, CantWrite, OutOfMemory };

/// Unpacks a downloaded NAME.tar.gz into the store and links it in. One already installed under
/// that name is replaced.
pub fn install(name: []const u8, gz: []const u8) InstallError!Linked {
    if (!validName(name)) return error.NotAPackage;
    const data = gzip.decompress(gz) catch return error.NotAPackage;
    defer gpa.free(data);
    // It must be one of ours: the first entry is .package/info.
    var r = tar.Reader{ .data = data };
    const first = r.next();
    r.deinit();
    if (first == null or !std.mem.eql(u8, first.?.name, ".package/info")) return error.NotAPackage;

    if (isInstalled(name)) _ = remove(name);
    var buf: [600]u8 = undefined;
    const dir = storePath(&buf, name, "") orelse return error.CantWrite;
    file.makePath(std.mem.span(dir));
    var here: [1024]u8 = undefined;
    const cwd = switch (sys.check(sys.syscall2(sys.nr.getcwd, @intFromPtr(&here), here.len))) {
        .ok => std.mem.sliceTo(&here, 0),
        .err => "/",
    };
    if (sys.check(sys.syscall1(sys.nr.chdir, @intFromPtr(dir))) != .ok) return error.CantWrite;
    // Whatever couldn't be written (a full disk), nothing stays half installed.
    const failed = tar.extract(data, null, false);
    var back: [1025]u8 = undefined;
    if (rt.cstr(&back, cwd)) |c| _ = sys.syscall1(sys.nr.chdir, @intFromPtr(c));
    if (failed > 0) {
        _ = file.removeTree(dir);
        return error.CantWrite;
    }
    return activate(name);
}

/// Takes a package's links away and deletes its folder. Files it shared with other packages
/// are linked to theirs instead.
pub fn remove(name: []const u8) bool {
    if (!isInstalled(name)) return false;
    var buf: [600]u8 = undefined;
    if (file.read(storePath(&buf, name, "/.package/links") orelse return false)) |links| {
        defer gpa.free(links);
        var lines = std.mem.splitScalar(u8, links, '\n');
        while (lines.next()) |rel| {
            if (!tar.safeName(rel)) continue;
            var dbuf: [1100]u8 = undefined;
            var tbuf: [1300]u8 = undefined;
            var lbuf: [1300]u8 = undefined;
            const dest = std.fmt.bufPrintSentinel(&dbuf, "/{s}", .{rel}, 0) catch continue;
            const target = std.fmt.bufPrint(&tbuf, "{s}/{s}/{s}", .{ store, name, rel }) catch continue;
            if (std.mem.eql(u8, readLink(dest, &lbuf) orelse "", target)) file.remove(dest);
        }
    } else |_| {}
    const ok = file.removeTree(storePath(&buf, name, "") orelse return false);
    _ = activateAll();
    return ok;
}
