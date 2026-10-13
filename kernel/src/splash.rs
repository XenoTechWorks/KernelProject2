//! The boot splash: while the system starts on its way to the desktop, a night-blue screen with
//! Vela's glossy orb and star, the name, a progress bar, and the step the boot is on (the same
//! lines as the "[  OK  ]" messages, which are still written underneath: any key shows them).
//! console.rs decides when it's up; this draws it into a picture of the screen.

use crate::console::mix;
use crate::title_font::{SPLASH, TITLE};

/// Where the parts go, for a w x h screen: the orb's center and radius, and the bar's left, top,
/// width.
struct Layout {
    cx: i32,
    cy: i32,
    r: i32,
    bar_x: i32,
    bar_y: i32,
    bar_w: i32,
}

fn layout(w: i32, h: i32) -> Layout {
    let (cx, cy) = (w / 2, h / 2 - 70);
    let bar_w = 320;
    Layout { cx, cy, r: 46, bar_x: (w - bar_w) / 2, bar_y: cy + 46 + 92, bar_w }
}

/// The whole splash.
pub fn draw(pixels: &mut [u32], w: usize, h: usize, status: &str, done: u32, total: u32) {
    let (wi, hi) = (w as i32, h as i32);
    let l = layout(wi, hi);
    // The sky: a dark gradient with a soft blue light behind the orb.
    for y in 0..hi {
        let base = mix(0x162544, 0x080d18, (y * 255 / hi) as u32);
        for x in 0..wi {
            let (dx, dy) = ((x - l.cx) as i64, ((y - l.cy) * 2) as i64);
            let d = isqrt((dx * dx + dy * dy) as u64) as i32;
            let glow = (360 - d).max(0) as u32;
            pixels[y as usize * w + x as usize] = mix(0x1f4b8f, base, glow * glow / (360 * 360 / 140));
        }
    }
    orb(pixels, w, &l);
    let name = "KernelProject2";
    let tx = (wi - SPLASH.width(name)) / 2;
    let ty = l.cy + l.r + 58;
    SPLASH.draw(pixels, w, tx, ty + 2, name, 0x000000, 140);
    SPLASH.draw(pixels, w, tx, ty, name, 0xf2f6fc, 255);
    progress(pixels, w, h, status, done, total);
}

/// Just the bar and the line under it (each step of the boot).
pub fn progress(pixels: &mut [u32], w: usize, h: usize, status: &str, done: u32, total: u32) {
    let l = layout(w as i32, h as i32);
    // Repaint what's behind the bar and the text (the gradient there), then draw them.
    for y in l.bar_y - 6..l.bar_y + 44 {
        let base = mix(0x162544, 0x080d18, (y * 255 / h as i32) as u32);
        for x in l.bar_x - 200..l.bar_x + l.bar_w + 200 {
            if x < 0 || x >= w as i32 || y < 0 || y >= h as i32 {
                continue;
            }
            let (dx, dy) = ((x - l.cx) as i64, ((y - l.cy) * 2) as i64);
            let d = isqrt((dx * dx + dy * dy) as u64) as i32;
            let glow = (360 - d).max(0) as u32;
            pixels[y as usize * w + x as usize] = mix(0x1f4b8f, base, glow * glow / (360 * 360 / 140));
        }
    }
    let filled = if total == 0 { 0 } else { l.bar_w * done.min(total) as i32 / total as i32 };
    for y in 0..8 {
        for x in 0..l.bar_w {
            let d = rounded(x, y, l.bar_w, 8, 4);
            let cover = (8 - d).clamp(0, 16) as u32 * 255 / 16;
            let color = if x < filled { mix(0x8fd0ff, 0x3a8ee6, (x * 255 / l.bar_w.max(1)) as u32) } else { 0x25314a };
            let at = (l.bar_y + y) as usize * w + (l.bar_x + x) as usize;
            pixels[at] = mix(color, pixels[at], cover);
        }
    }
    let mut text: alloc::string::String = status.chars().take(90).collect();
    if status.chars().count() > 90 {
        text.push('…');
    }
    let tx = (w as i32 - TITLE.width(&text)) / 2;
    TITLE.draw(pixels, w, tx, l.bar_y + 32, &text, 0xa9b6cc, 255);
}

/// Vela's orb: a glossy blue sphere with a white five-pointed star, a dark rim and a shine.
fn orb(pixels: &mut [u32], w: usize, l: &Layout) {
    let r = l.r;
    for y in -r - 2..=r + 2 {
        for x in -r - 2..=r + 2 {
            let at = (l.cy + y) as usize * w + (l.cx + x) as usize;
            let d16 = isqrt(((x * 16 + 8) * (x * 16 + 8) + (y * 16 + 8) * (y * 16 + 8)) as u64) as i32;
            // Rim, then the body: brighter low in the middle (light collects at the bottom).
            let rim = (8 - (d16 - (r + 1) * 16)).clamp(0, 16) as u32 * 255 / 16;
            pixels[at] = mix(0x060c18, pixels[at], rim);
            let body = (8 - (d16 - r * 16)).clamp(0, 16) as u32 * 255 / 16;
            if body == 0 {
                continue;
            }
            let (lx, ly) = (x, y - r * 4 / 10);
            let dl = isqrt((lx * lx + ly * ly) as u64) as i32;
            let t = (dl * 255 / (r * 14 / 10)).clamp(0, 255) as u32;
            pixels[at] = mix(mix(0x0a2f78, 0x4aa3f5, t), pixels[at], body);
        }
    }
    star(pixels, w, l.cx, l.cy + 1, r * 56 / 100);
    // The shine: white fading downward inside an oval over the top half.
    let (gx, gy, rx, ry) = (l.cx, l.cy - r * 42 / 100, r * 70 / 100, r * 44 / 100);
    for y in -ry..=ry {
        for x in -rx..=rx {
            let q = (x * 1000 / rx) * (x * 1000 / rx) + (y * 1000 / ry) * (y * 1000 / ry);
            if q >= 1_000_000 {
                continue;
            }
            let fade = ((ry - y) * 255 / (2 * ry)) as u32;
            let edge = ((1_000_000 - q) / 4000).min(255) as u32;
            let at = (gy + y) as usize * w + (gx + x) as usize;
            pixels[at] = mix(0xffffff, pixels[at], fade * edge / 255 * 130 / 255);
        }
    }
}

/// The star's ten corners on a circle of 1000 (outer points, then inner, from the top).
const STAR: [(i32, i32); 10] = [
    (0, -1000), (265, -364), (951, -309), (428, 139), (588, 809),
    (0, 450), (-588, 809), (-428, 139), (-951, -309), (-265, -364),
];

/// A white star of radius `r` at (cx, cy), its edges smoothed by sampling each pixel 4 x 4 times.
fn star(pixels: &mut [u32], w: usize, cx: i32, cy: i32, r: i32) {
    let pts: [(i32, i32); 10] = STAR.map(|(x, y)| (x * r * 16 / 1000, y * r * 16 / 1000));
    for y in -r..=r {
        for x in -r..=r {
            let mut hits = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    if inside(&pts, x * 16 + sx * 4 + 2, y * 16 + sy * 4 + 2) {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let at = (cy + y) as usize * w + (cx + x) as usize;
                pixels[at] = mix(0xffffff, pixels[at], hits * 255 / 16);
            }
        }
    }
}

/// Even-odd rule: a ray to the right crosses the edges an odd number of times.
fn inside(pts: &[(i32, i32)], x: i32, y: i32) -> bool {
    let mut odd = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let ((xi, yi), (xj, yj)) = (pts[i], pts[j]);
        if (yi > y) != (yj > y) && (x as i64) < (xj - xi) as i64 * (y - yi) as i64 / (yj - yi) as i64 + xi as i64 {
            odd = !odd;
        }
        j = i;
    }
    odd
}

/// Signed distance (1/16 pixels) from a pixel's center to a rounded rectangle's edge.
fn rounded(px: i32, py: i32, w: i32, h: i32, r: i32) -> i32 {
    let (x, y) = (px * 16 + 8, py * 16 + 8);
    let (hw, hh, r) = (w * 8, h * 8, r * 16);
    let qx = (x - hw).abs() - (hw - r);
    let qy = (y - hh).abs() - (hh - r);
    let outside = if qx > 0 && qy > 0 { isqrt((qx as u64).pow(2) + (qy as u64).pow(2)) as i32 } else { qx.max(qy).max(0) };
    outside + qx.max(qy).min(0) - r
}

fn isqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}
