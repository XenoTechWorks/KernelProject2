//! ifconfig: show how the network is set up, or change it.
//!
//!     ifconfig                                      show it
//!     ifconfig 10.0.2.15 255.255.255.0 10.0.2.2 10.0.2.3    address, mask, gateway, dns
//!
//! There's one network card and no DHCP yet, so it starts with what QEMU's user-mode network
//! always uses.

const std = @import("std");
const rt = @import("lib/rt.zig");
const net = @import("lib/net.zig");
const sys = rt.sys;
pub const _start = rt._start;
comptime {
    _ = _start;
}
pub const panic = std.debug.FullPanic(rt.panicHandler);

pub fn main(args: rt.Args) u8 {
    if (args.get(1)) |_| {
        var setting = net.Settings{};
        const names = [_][]const u8{ "address", "netmask", "gateway", "dns" };
        const fields = [_]*net.Ip{ &setting.ip, &setting.netmask, &setting.gateway, &setting.dns };
        for (fields, names, 0..) |field, name, i| {
            const text = args.get(i + 1) orelse break;
            field.* = net.parseIp(text) orelse {
                rt.err.print("ifconfig: {s} isn't an address for the {s}\n", .{ text, name });
                return 2;
            };
        }
        if (!net.setSettings(setting)) {
            rt.err.write("ifconfig: can't change it (is there a network card?)\n");
            return 1;
        }
    }
    const setting = net.settings() orelse {
        rt.err.write("ifconfig: no network card\n");
        return 1;
    };
    var b: [4][24]u8 = undefined;
    rt.out.print(
        \\net0  {x:0>2}:{x:0>2}:{x:0>2}:{x:0>2}:{x:0>2}:{x:0>2}
        \\      address {s}  netmask {s}
        \\      gateway {s}  dns {s}
        \\
    , .{
        setting.mac[0], setting.mac[1], setting.mac[2], setting.mac[3], setting.mac[4], setting.mac[5],
        net.format(&b[0], setting.ip),      net.format(&b[1], setting.netmask),
        net.format(&b[2], setting.gateway), net.format(&b[3], setting.dns),
    });
    return 0;
}
