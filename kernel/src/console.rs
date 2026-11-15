//! The kernel's text console, drawn on a framebuffer when one is attached.
//! The i.MX boards attach none at boot (their log goes to the UART), so
//! until then it draws nothing.
//!
//! Framebuffer memory can be very slow to *read*, so we never read it back:
//! the console keeps its own character grid and redraws from that on scroll.

use core::fmt;
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};

const LINE_SPACING: usize = 2;
const MARGIN: usize = 8;
const MAX_COLS: usize = 256;
const MAX_ROWS: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

// Dark text on a light background: easier to read on a bright glossy
// screen (room reflections disappear) and on a photo of it.
pub const FG: Rgb = Rgb(0x10, 0x10, 0x10);
pub const BG: Rgb = Rgb(0xf4, 0xf4, 0xf0);
pub const RED: Rgb = Rgb(0xc0, 0x00, 0x00);
pub const YELLOW: Rgb = Rgb(0x90, 0x50, 0x00);
pub const DIM: Rgb = Rgb(0x68, 0x68, 0x68);

/// A linear 32-bpp framebuffer.
pub struct Framebuffer {
    pub base: *mut u32,
    pub width: usize,
    pub height: usize,
    /// In pixels (not bytes).
    pub stride: usize,
    pub r_shift: u8,
    pub g_shift: u8,
    pub b_shift: u8,
}

unsafe impl Send for Framebuffer {}

impl Framebuffer {
    #[inline]
    pub fn pixel(&self, c: Rgb) -> u32 {
        ((c.0 as u32) << self.r_shift)
            | ((c.1 as u32) << self.g_shift)
            | ((c.2 as u32) << self.b_shift)
    }

    #[inline]
    pub fn put(&mut self, x: usize, y: usize, px: u32) {
        if x < self.width && y < self.height {
            unsafe { self.base.add(y * self.stride + x).write_volatile(px) }
        }
    }

    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: Rgb) {
        let px = self.pixel(c);
        for yy in y..(y + h).min(self.height) {
            for xx in x..(x + w).min(self.width) {
                unsafe { self.base.add(yy * self.stride + xx).write_volatile(px) }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Cell {
    ch: u8,
    fg: Rgb,
}

const BLANK: Cell = Cell { ch: b' ', fg: FG };

pub struct Console {
    fb: Option<Framebuffer>,
    cells: [[Cell; MAX_COLS]; MAX_ROWS],
    cols: usize,
    rows: usize,
    col: usize,
    row: usize,
    fg: Rgb,
    glyph_w: usize,
    line_h: usize,
    font: RasterHeight,
    scale: usize,
}

impl Console {
    pub const fn new() -> Self {
        Self {
            fb: None,
            cells: [[BLANK; MAX_COLS]; MAX_ROWS],
            cols: 0,
            rows: 0,
            col: 0,
            row: 0,
            fg: FG,
            glyph_w: 1,
            line_h: 1,
            font: RasterHeight::Size16,
            scale: 1,
        }
    }

    pub fn attach(&mut self, mut fb: Framebuffer) {
        // On a large panel: 24 px glyphs drawn at 2x, so the log is
        // readable from a distance.
        (self.font, self.scale) = if fb.width >= 1600 {
            (RasterHeight::Size24, 2)
        } else {
            (RasterHeight::Size16, 1)
        };
        self.glyph_w = get_raster_width(FontWeight::Regular, self.font) * self.scale;
        self.line_h = (self.font.val() + LINE_SPACING) * self.scale;
        self.cols = ((fb.width - 2 * MARGIN) / self.glyph_w).min(MAX_COLS);
        self.rows = ((fb.height - 2 * MARGIN) / self.line_h).min(MAX_ROWS);
        fb.fill_rect(0, 0, fb.width, fb.height, BG);
        self.fb = Some(fb);
    }

    /// Hand the framebuffer to the UI; the console stops drawing.
    pub fn take_framebuffer(&mut self) -> Option<Framebuffer> {
        self.fb.take()
    }

    pub fn set_color(&mut self, c: Rgb) {
        self.fg = c;
    }

    fn draw_cell(&mut self, row: usize, col: usize) {
        let (glyph_w, line_h, font, scale) = (self.glyph_w, self.line_h, self.font, self.scale);
        let cell = self.cells[row][col];
        let Some(fb) = self.fb.as_mut() else { return };
        let x0 = MARGIN + col * glyph_w;
        let y0 = MARGIN + row * line_h;
        let ch = if cell.ch.is_ascii_graphic() || cell.ch == b' ' {
            cell.ch as char
        } else {
            '?'
        };
        let raster = get_raster(ch, FontWeight::Regular, font)
            .or_else(|| get_raster('?', FontWeight::Regular, font))
            .unwrap();
        for (dy, line) in raster.rows().enumerate() {
            for (dx, &a) in line.iter().enumerate() {
                let mix = |f: u8, b: u8| {
                    ((f as u16 * a as u16 + b as u16 * (255 - a as u16)) / 255) as u8
                };
                let c = Rgb(
                    mix(cell.fg.0, BG.0),
                    mix(cell.fg.1, BG.1),
                    mix(cell.fg.2, BG.2),
                );
                let px = fb.pixel(c);
                for sy in 0..scale {
                    for sx in 0..scale {
                        fb.put(x0 + dx * scale + sx, y0 + dy * scale + sy, px);
                    }
                }
            }
        }
        // Clear the inter-line gap so scrolling never leaves stale pixels.
        fb.fill_rect(
            x0,
            y0 + raster.height() * scale,
            glyph_w,
            line_h - raster.height() * scale,
            BG,
        );
    }

    fn newline(&mut self) {
        self.col = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
            return;
        }
        // Scroll: shift the grid up one line and redraw only cells that changed.
        for r in 0..self.rows - 1 {
            for c in 0..self.cols {
                let below = self.cells[r + 1][c];
                let cur = self.cells[r][c];
                if cur.ch != below.ch || cur.fg != below.fg {
                    self.cells[r][c] = below;
                    self.draw_cell(r, c);
                }
            }
        }
        let last = self.rows - 1;
        for c in 0..self.cols {
            if self.cells[last][c].ch != b' ' {
                self.cells[last][c] = BLANK;
                self.draw_cell(last, c);
            }
        }
    }

    pub fn putc(&mut self, b: u8) {
        if self.fb.is_none() {
            return;
        }
        match b {
            b'\n' => self.newline(),
            b'\r' => self.col = 0,
            _ => {
                if self.col >= self.cols {
                    self.newline();
                }
                let (r, c) = (self.row, self.col);
                self.cells[r][c] = Cell { ch: b, fg: self.fg };
                self.draw_cell(r, c);
                self.col += 1;
            }
        }
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.putc(b);
        }
        Ok(())
    }
}
