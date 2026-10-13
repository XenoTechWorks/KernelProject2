//! The font window titles are drawn in: Inter at 13 pixels, smooth (anti-aliased), rendered ahead
//! of time by `user/src/mkatlas.zig` with the toolkit's own TrueType renderer, so the kernel needs
//! none. Inter is under the SIL Open Font License (title-font-LICENSE).
//!
//! The file: "KPFA", u16 pixel size, i16 ascent, u16 glyph count; per glyph (sorted by character)
//! u32 character, i16 left, i16 top, u16 width, u16 height, u16 advance (1/64 pixels), u32 offset;
//! then the coverage bytes.

static ATLAS: &[u8] = include_bytes!("title-font.bin");
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

fn u16_at(at: usize) -> u16 {
    u16::from_le_bytes([ATLAS[at], ATLAS[at + 1]])
}

fn count() -> usize {
    if ATLAS.len() < HEADER || &ATLAS[..4] != b"KPFA" { 0 } else { u16_at(8) as usize }
}

pub fn glyph(c: char) -> Option<Glyph> {
    let n = count();
    let pixels = HEADER + n * ENTRY;
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let at = HEADER + mid * ENTRY;
        let cp = u32::from_le_bytes([ATLAS[at], ATLAS[at + 1], ATLAS[at + 2], ATLAS[at + 3]]);
        if cp == c as u32 {
            let w = u16_at(at + 8) as usize;
            let h = u16_at(at + 10) as usize;
            let offset = u32::from_le_bytes([ATLAS[at + 14], ATLAS[at + 15], ATLAS[at + 16], ATLAS[at + 17]]) as usize;
            return Some(Glyph {
                left: u16_at(at + 4) as i16 as i32,
                top: u16_at(at + 6) as i16 as i32,
                w,
                h,
                advance: u16_at(at + 12) as i32,
                alpha: ATLAS.get(pixels + offset..pixels + offset + w * h)?,
            });
        }
        if cp < c as u32 { lo = mid + 1 } else { hi = mid }
    }
    None
}

/// A character's glyph, or "?" for one the font doesn't have.
pub fn glyph_or_box(c: char) -> Option<Glyph> {
    glyph(c).or_else(|| glyph('?'))
}

/// How wide `text` is, in pixels.
pub fn width(text: &str) -> i32 {
    text.chars().filter_map(glyph_or_box).map(|g| g.advance).sum::<i32>() / 64
}
