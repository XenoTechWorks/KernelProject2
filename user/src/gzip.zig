//! gzip: compress a file to .gz, or unpack one again. `gunzip` and `zcat` are the same program.
//!
//!     gzip notes.txt        notes.txt  -> notes.txt.gz
//!     gunzip notes.txt.gz   notes.txt.gz -> notes.txt
//!     zcat notes.txt.gz     writes the contents, keeps the file
//!
//! -d unpack, -c write to standard output and keep the file, -k keep it anyway.

const std = @import("std");
const rt = @import("lib/rt.zig");
const file = @import("lib/file.zig");
const gzip = @import("lib/gzip.zig");
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

const usage =
    \\usage: gzip [-d] [-c] [-k] FILE...
    \\  -d  unpack (the same as gunzip)
    \\  -c  write to standard output, keep the file (the same as zcat)
    \\  -k  keep the file
    \\
;

pub fn main(args: rt.Args) u8 {
    const me = args.get(0) orelse "gzip";
    const name = me[if (std.mem.lastIndexOfScalar(u8, me, '/')) |i| i + 1 else 0..];
    var decompress = std.mem.startsWith(u8, name, "gunzip") or std.mem.startsWith(u8, name, "zcat");
    var to_stdout = std.mem.startsWith(u8, name, "zcat");
    var keep = to_stdout;

    var status: u8 = 0;
    var any = false;
    var i: usize = 1;
    while (args.get(i)) |arg| : (i += 1) {
        if (arg.len > 1 and arg[0] == '-' and arg[1] != '\x00') {
            for (arg[1..]) |c| switch (c) {
                'd' => decompress = true,
                'c' => {
                    to_stdout = true;
                    keep = true;
                },
                'k' => keep = true,
                'h' => {
                    rt.out.write(usage);
                    return 0;
                },
                else => {
                    rt.err.print("gzip: unknown option -{c}\n", .{c});
                    return 2;
                },
            };
            continue;
        }
        any = true;
        if (!one(arg, decompress, to_stdout, keep)) status = 1;
    }
    if (!any) {
        rt.err.write(usage);
        return 2;
    }
    return status;
}

fn one(path: []const u8, decompress: bool, to_stdout: bool, keep: bool) bool {
    var in_buf: [1024]u8 = undefined;
    const in_path = rt.cstr(&in_buf, path) orelse return false;
    const data = file.read(in_path) catch {
        rt.err.print("gzip: can't read {s}\n", .{path});
        return false;
    };
    defer std.heap.page_allocator.free(data);

    var out_name: [1100]u8 = undefined;
    var result: []u8 = undefined;
    var out_path: []const u8 = undefined;
    if (decompress) {
        if (!std.mem.endsWith(u8, path, ".gz") and !to_stdout) {
            rt.err.print("gzip: {s}: not a .gz name\n", .{path});
            return false;
        }
        result = gzip.decompress(data) catch |e| {
            rt.err.print("gzip: {s}: {s}\n", .{ path, if (e == error.NotGzip) "not a gzip file" else "damaged" });
            return false;
        };
        out_path = if (std.mem.endsWith(u8, path, ".gz")) path[0 .. path.len - 3] else path;
    } else {
        if (std.mem.endsWith(u8, path, ".gz")) {
            rt.err.print("gzip: {s} is already packed\n", .{path});
            return false;
        }
        const base = path[if (std.mem.lastIndexOfScalar(u8, path, '/')) |i| i + 1 else 0..];
        result = gzip.compress(data, base) catch {
            rt.err.write("gzip: out of memory\n");
            return false;
        };
        out_path = std.fmt.bufPrint(&out_name, "{s}.gz", .{path}) catch return false;
    }
    defer std.heap.page_allocator.free(result);

    var buf: [1100]u8 = undefined;
    const dest = rt.cstr(&buf, if (to_stdout) "-" else out_path) orelse return false;
    file.write(dest, result, 0o644) catch {
        rt.err.print("gzip: can't write {s}\n", .{out_path});
        return false;
    };
    if (!keep and !to_stdout) file.remove(in_path);
    return true;
}
