//! The fonts the kernel draws text in: Inter, smooth (anti-aliased), rendered ahead of time by
//! `user/src/mkatlas.zig` with the toolkit's own TrueType renderer, so the kernel needs none: at
//! 13 pixels for window titles, at 34 for the boot splash. Inter is under the SIL Open Font
//! License (title-font-LICENSE).
//!
//! An atlas file: "KPFA", u16 pixel size, i16 ascent, u16 glyph count; per glyph (sorted by
//! character) u32 character, i16 left, i16 top, u16 width, u16 height, u16 advance (1/64 pixels),
//! u32 offset; then the coverage bytes.

pub struct Atlas(&'static [u8]);

pub static TITLE: Atlas = Atlas(include_bytes!("title-font.bin"));
pub static SPLASH: Atlas = Atlas(include_bytes!("splash-font.bin"));

const HEADER: usize = 10;
const ENTRY: usize = 18;

pub struct Glyph {
    /// From the pen on the baseline to the bitmap's top-left corner (top: how far above).
    pub left: i32,
    pub top: i32,
    pub w: usize,
    pub h: usize,
    /// How far the pen moves, in 1/64 pixels.
    pub advance: i32,
    /// Coverage, 0..255, w x h.
    pub alpha: &'static [u8],
}

impl Atlas {
    fn u16_at(&self, at: usize) -> u16 {
        u16::from_le_bytes([self.0[at], self.0[at + 1]])
    }

    fn count(&self) -> usize {
        if self.0.len() < HEADER || &self.0[..4] != b"KPFA" { 0 } else { self.u16_at(8) as usize }
    }

    pub fn glyph(&self, c: char) -> Option<Glyph> {
        let data = self.0;
        let n = self.count();
        let pixels = HEADER + n * ENTRY;
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let at = HEADER + mid * ENTRY;
            let cp = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
            if cp == c as u32 {
                let w = self.u16_at(at + 8) as usize;
                let h = self.u16_at(at + 10) as usize;
                let offset = u32::from_le_bytes([data[at + 14], data[at + 15], data[at + 16], data[at + 17]]) as usize;
                return Some(Glyph {
                    left: self.u16_at(at + 4) as i16 as i32,
                    top: self.u16_at(at + 6) as i16 as i32,
                    w,
                    h,
                    advance: self.u16_at(at + 12) as i32,
                    alpha: data.get(pixels + offset..pixels + offset + w * h)?,
                });
            }
            if cp < c as u32 { lo = mid + 1 } else { hi = mid }
        }
        None
    }

    /// A character's glyph, or "?" for one the font doesn't have.
    pub fn glyph_or_box(&self, c: char) -> Option<Glyph> {
        self.glyph(c).or_else(|| self.glyph('?'))
    }

    /// How wide `text` is, in pixels.
    pub fn width(&self, text: &str) -> i32 {
        text.chars().filter_map(|c| self.glyph_or_box(c)).map(|g| g.advance).sum::<i32>() / 64
    }

    /// Draws `text` into `pixels` (a `stride`-wide picture, 0xRRGGBB) with its baseline at y,
    /// starting at x, in `color`, `strength` of 255 strong.
    pub fn draw(&self, pixels: &mut [u32], stride: usize, x: i32, y: i32, text: &str, color: u32, strength: u32) {
        let rows = (pixels.len() / stride) as i32;
        let mut pen = x * 64;
        for c in text.chars() {
            let Some(g) = self.glyph_or_box(c) else { continue };
            let (gx, gy) = (pen / 64 + g.left, y - g.top);
            for row in 0..g.h as i32 {
                for col in 0..g.w as i32 {
                    let (px, py) = (gx + col, gy + row);
                    if px < 0 || py < 0 || px >= stride as i32 || py >= rows {
                        continue;
                    }
                    let a = g.alpha[(row * g.w as i32 + col) as usize] as u32 * strength / 255;
                    let at = py as usize * stride + px as usize;
                    pixels[at] = crate::console::mix(color, pixels[at], a);
                }
            }
            pen += g.advance;
        }
    }
}
