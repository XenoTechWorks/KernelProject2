//! mkatlas: renders a font's characters at one size into a small file the kernel draws window
//! titles with (kernel/src/title_font.rs), smooth like the apps' text, without a font renderer of
//! its own. A build-machine tool (it runs on Linux too), using the toolkit's TrueType renderer:
//!
//!     mkatlas Inter.ttf 13 kernel/src/title-font.bin
//!
//! The file (little-endian): "KPFA", u16 pixel size, i16 ascent, u16 glyph count; then per glyph
//! (sorted by character): u32 character, i16 left, i16 top, u16 width, u16 height, u16 advance in
//! 1/64 pixels, u32 where its coverage bytes start; then the coverage bytes (0..255, row by row).

const std = @import("std");
const rt = @import("lib/rt.zig");
const file = @import("lib/file.zig");
const Font = @import("lib/gui/font.zig").Font;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

/// ASCII, Latin-1, and the punctuation titles tend to have.
fn wanted(cp: u21) bool {
    return (cp >= 32 and cp <= 126) or (cp >= 160 and cp <= 255) or switch (cp) {
        0x2013, 0x2014, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2026, 0x2190, 0x2192, 0x2605 => true,
        else => false,
    };
}

pub fn main(args: rt.Args) u8 {
    if (args.argv.len != 4) {
        rt.err.write("usage: mkatlas font.ttf size out.bin\n");
        return 2;
    }
    const font = Font.load(args.argv[1]) orelse {
        rt.err.write("mkatlas: can't read the font\n");
        return 1;
    };
    const size = std.fmt.parseInt(u16, args.get(2).?, 10) catch return 2;
    const gpa = std.heap.page_allocator;
    var table = std.ArrayList(u8).empty;
    var pixels = std.ArrayList(u8).empty;
    var count: u16 = 0;
    var cp: u21 = 0;
    while (cp < 0x2700) : (cp += 1) {
        if (!wanted(cp)) continue;
        const index = font.glyphIndex(cp);
        if (index == 0 and cp != ' ') continue; // the font doesn't have it
        const g = font.glyph(index, size);
        var entry: [20]u8 = undefined;
        std.mem.writeInt(u32, entry[0..4], cp, .little);
        std.mem.writeInt(i16, entry[4..6], @intCast(g.left), .little);
        std.mem.writeInt(i16, entry[6..8], @intCast(g.top), .little);
        std.mem.writeInt(u16, entry[8..10], @intCast(g.w), .little);
        std.mem.writeInt(u16, entry[10..12], @intCast(g.h), .little);
        std.mem.writeInt(u16, entry[12..14], @intFromFloat(@round(g.advance * 64)), .little);
        std.mem.writeInt(u32, entry[14..18], @intCast(pixels.items.len), .little);
        table.appendSlice(gpa, entry[0..18]) catch return 1;
        pixels.appendSlice(gpa, g.alpha[0 .. g.w * g.h]) catch return 1;
        count += 1;
    }
    var header: [10]u8 = undefined;
    @memcpy(header[0..4], "KPFA");
    std.mem.writeInt(u16, header[4..6], size, .little);
    std.mem.writeInt(i16, header[6..8], @intFromFloat(@round(font.ascender(@floatFromInt(size)))), .little);
    std.mem.writeInt(u16, header[8..10], count, .little);
    var out = std.ArrayList(u8).empty;
    out.appendSlice(gpa, &header) catch return 1;
    out.appendSlice(gpa, table.items) catch return 1;
    out.appendSlice(gpa, pixels.items) catch return 1;
    file.write(args.argv[3], out.items, 0o644) catch {
        rt.err.write("mkatlas: can't write the output\n");
        return 1;
    };
    rt.out.print("{d} glyphs at {d} px, {d} bytes\n", .{ count, size, out.items.len });
    return 0;
}
