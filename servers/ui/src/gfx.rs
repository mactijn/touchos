//! 2D drawing into a RAM back buffer (0x00RRGGBB), then presented to the
//! hardware framebuffer. Integer-only: no floating point needed.

use alloc::vec;
use alloc::vec::Vec;

use crate::Framebuffer;

pub struct Glyph {
    pub ch: u32,
    pub w: u16,
    pub h: u16,
    pub xmin: i16,
    pub ymin: i16,
    pub adv: u16,
    pub off: u32,
}

pub struct Face {
    pub glyphs: &'static [Glyph],
    pub data: &'static [u8],
}

impl Face {
    fn glyph(&self, c: char) -> Option<&Glyph> {
        let c = c as u32;
        self.glyphs
            .binary_search_by_key(&c, |g| g.ch)
            .ok()
            .map(|i| &self.glyphs[i])
    }
    pub fn width(&self, text: &str) -> i32 {
        text.chars()
            .map(|c| {
                self.glyph(c)
                    .or_else(|| self.glyph('?'))
                    .map_or(0, |g| g.adv as i32)
            })
            .sum()
    }
}

pub mod fonts {
    use super::{Face, Glyph};
    include!(concat!(env!("OUT_DIR"), "/fonts.rs"));
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color((r as u32) << 16 | (g as u32) << 8 | b as u32)
    }
}

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    px: Vec<u32>,
}

#[inline]
fn mix(dst: u32, src: u32, a: u32) -> u32 {
    if a >= 255 {
        return src;
    }
    let inv = 255 - a;
    let ch = |s: u32| (((src >> s) & 0xff) * a + ((dst >> s) & 0xff) * inv) / 255;
    ch(16) << 16 | ch(8) << 8 | ch(0)
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Canvas {
        Canvas {
            w,
            h,
            px: vec![0; (w * h) as usize],
        }
    }

    #[inline]
    fn blend(&mut self, x: i32, y: i32, c: Color, a: u32) {
        if x >= 0 && y >= 0 && x < self.w && y < self.h && a > 0 {
            let p = &mut self.px[(y * self.w + x) as usize];
            *p = mix(*p, c.0, a);
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        for yy in y0..y1 {
            let row = (yy * self.w) as usize;
            self.px[row + x0 as usize..row + x1.max(x0) as usize].fill(c.0);
        }
    }

    /// Rounded rectangle with anti-aliased corners (4x4 supersampling on the edge).
    pub fn fill_round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Color) {
        let r = r.min(w / 2).min(h / 2).max(0);
        self.fill_rect(x + r, y, w - 2 * r, h, c);
        self.fill_rect(x, y + r, r, h - 2 * r, c);
        self.fill_rect(x + w - r, y + r, r, h - 2 * r, c);
        // Corners: (corner origin pixel, centre of the quarter circle).
        // Coverage from 4x4 samples; coordinates in 1/8 pixel units.
        let r8 = r * 8;
        for (ox, oy, cx, cy) in [
            (x, y, x + r, y + r),
            (x + w - r, y, x + w - r, y + r),
            (x, y + h - r, x + r, y + h - r),
            (x + w - r, y + h - r, x + w - r, y + h - r),
        ] {
            for py in oy..oy + r {
                for pxx in ox..ox + r {
                    let mut cov = 0;
                    for sy in 0..4 {
                        for sx in 0..4 {
                            let fx = pxx * 8 + sx * 2 + 1 - cx * 8;
                            let fy = py * 8 + sy * 2 + 1 - cy * 8;
                            if fx * fx + fy * fy <= r8 * r8 {
                                cov += 1;
                            }
                        }
                    }
                    self.blend(pxx, py, c, cov * 255 / 16);
                }
            }
        }
    }

    /// Draw text with its baseline at `y`; returns the advance width.
    pub fn text(&mut self, face: &Face, x: i32, y: i32, text: &str, c: Color) -> i32 {
        let mut pen = x;
        for ch in text.chars() {
            let Some(g) = face.glyph(ch).or_else(|| face.glyph('?')) else {
                continue;
            };
            let top = y - (g.ymin as i32 + g.h as i32);
            let left = pen + g.xmin as i32;
            for gy in 0..g.h as i32 {
                let yy = top + gy;
                if yy < 0 || yy >= self.h {
                    continue;
                }
                let row = g.off as usize + (gy * g.w as i32) as usize;
                for gx in 0..g.w as i32 {
                    let a = face.data[row + gx as usize] as u32;
                    if a != 0 {
                        self.blend(left + gx, yy, c, a);
                    }
                }
            }
            pen += g.adv as i32;
        }
        pen - x
    }

    /// Text truncated with an ellipsis to fit `max_w`.
    pub fn text_fit(&mut self, face: &Face, x: i32, y: i32, text: &str, max_w: i32, c: Color) {
        if face.width(text) <= max_w {
            self.text(face, x, y, text, c);
            return;
        }
        let mut s = alloc::string::String::new();
        for ch in text.chars() {
            s.push(ch);
            if face.width(&s) + face.width("…") > max_w {
                s.pop();
                break;
            }
        }
        s.push('…');
        self.text(face, x, y, &s, c);
    }

    pub fn circle(&mut self, cx: i32, cy: i32, r: i32, c: Color) {
        self.fill_round_rect(cx - r, cy - r, 2 * r, 2 * r, r, c);
    }

    /// Copy to the hardware framebuffer, rotating by `rot` quarter turns
    /// (see `panel_to_logical`).
    pub fn present(&self, fb: &mut Framebuffer, rot: u8) {
        let (pw, ph) = (fb.width as i32, fb.height as i32);
        let native = fb.bpp == 32 && fb.r_shift == 16 && fb.g_shift == 8 && fb.b_shift == 0;
        let convert = |p: u32| {
            ((p >> 16) & 0xff) << fb.r_shift
                | ((p >> 8) & 0xff) << fb.g_shift
                | (p & 0xff) << fb.b_shift
        };
        // RGB565: the top 5, 6 and 5 bits of each channel.
        let convert16 = |p: u32| {
            (((p >> 19) & 0x1f) << fb.r_shift
                | ((p >> 10) & 0x3f) << fb.g_shift
                | ((p >> 3) & 0x1f) << fb.b_shift) as u16
        };
        // RGB565, unrotated, even width: two pixels per 32-bit store, straight
        // from the canvas (the framebuffer is uncached: few, wide writes).
        if fb.bpp == 16 && rot == 0 && self.w == pw && pw % 2 == 0 && fb.pitch % 4 == 0 {
            for py in 0..ph as usize {
                let src = &self.px[py * pw as usize..(py + 1) * pw as usize];
                let h = row_hash(src);
                if fb.rows[py] == h {
                    continue;
                }
                fb.rows[py] = h;
                let mut dst = unsafe { fb.base.add(py * fb.pitch) } as *mut u32;
                let pair = |a: u32, b: u32| convert16(a) as u32 | (convert16(b) as u32) << 16;
                let mut sixteens = src.chunks_exact(16);
                for p in &mut sixteens {
                    let w = core::array::from_fn(|i| pair(p[2 * i], p[2 * i + 1]));
                    dst = store8(dst, w);
                }
                for two in sixteens.remainder().chunks_exact(2) {
                    unsafe {
                        dst.write_volatile(pair(two[0], two[1]));
                        dst = dst.add(1);
                    }
                }
            }
            return;
        }
        let mut row: Vec<u32> = vec![0; pw as usize];
        let mut row16: Vec<u16> = vec![0; if fb.bpp == 16 { pw as usize } else { 0 }];
        for py in 0..ph {
            if rot == 0 && self.w == pw {
                row.copy_from_slice(&self.px[(py * self.w) as usize..(py * self.w + pw) as usize]);
            } else {
                for px in 0..pw {
                    let (lx, ly) = panel_to_logical(rot, pw, ph, px, py);
                    row[px as usize] = if lx >= 0 && ly >= 0 && lx < self.w && ly < self.h {
                        self.px[(ly * self.w + lx) as usize]
                    } else {
                        0
                    };
                }
            }
            let h = row_hash(&row);
            if fb.rows[py as usize] == h {
                continue;
            }
            fb.rows[py as usize] = h;
            let dst = unsafe { fb.base.add(py as usize * fb.pitch) };
            if fb.bpp == 16 {
                for (d, &p) in row16.iter_mut().zip(row.iter()) {
                    *d = convert16(p);
                }
                unsafe {
                    core::ptr::copy_nonoverlapping(row16.as_ptr(), dst as *mut u16, pw as usize)
                };
                continue;
            }
            if !native {
                for p in row.iter_mut() {
                    *p = convert(*p);
                }
            }
            unsafe { core::ptr::copy_nonoverlapping(row.as_ptr(), dst as *mut u32, pw as usize) };
        }
    }
}

/// A 32-bit hash of a row's pixels (cheap on the ARM926, which has no fast
/// 64-bit multiply): tells `present` whether the row changed. FNV-1a over
/// whole pixels, plus a fold of the high half after each multiply: a
/// multiply only carries differences upward, so a change in the high bits
/// alone (just the red channel) would otherwise reach only a few hash bits.
pub fn row_hash(px: &[u32]) -> u32 {
    px.iter().fold(0x811c_9dc5, |h, &p| {
        let h = (h ^ p).wrapping_mul(0x0100_0193);
        h ^ (h >> 16)
    })
}

/// Store 8 words at `dst` with one STM: to uncached (bufferable) memory
/// the ARM926 sends that as a burst instead of single writes (on the Toon
/// 4-word STMs halved the time of single stores).
#[inline(always)]
fn store8(dst: *mut u32, w: [u32; 8]) -> *mut u32 {
    #[cfg(target_arch = "arm")]
    unsafe {
        let mut d = dst;
        // (STM stores in register-number order: ascending = address order.)
        core::arch::asm!(
            "stmia {d}!, {{r0, r1, r2, r3, r4, r5, r8, r12}}",
            d = inout(reg) d,
            in("r0") w[0], in("r1") w[1], in("r2") w[2], in("r3") w[3],
            in("r4") w[4], in("r5") w[5], in("r8") w[6], in("r12") w[7],
            options(nostack, preserves_flags),
        );
        d
    }
    #[cfg(not(target_arch = "arm"))]
    unsafe {
        for (i, v) in w.into_iter().enumerate() {
            dst.add(i).write_volatile(v);
        }
        dst.add(8)
    }
}

/// Logical size for a rotation of a `pw` x `ph` panel.
pub fn logical_size(rot: u8, pw: i32, ph: i32) -> (i32, i32) {
    if rot & 1 == 1 { (ph, pw) } else { (pw, ph) }
}

/// Panel pixel -> logical (content) coordinates.
/// rot 1: the panel's right edge is up; rot 3: its left edge is up.
pub fn panel_to_logical(rot: u8, pw: i32, ph: i32, px: i32, py: i32) -> (i32, i32) {
    match rot {
        1 => (py, pw - 1 - px),
        2 => (pw - 1 - px, ph - 1 - py),
        3 => (ph - 1 - py, px),
        _ => (px, py),
    }
}
