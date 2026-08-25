use std::cell::RefCell;
use std::collections::HashMap;

use fontdue::{Font, FontSettings, Metrics};

static NERD_FONT_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/HackNerdFontMono-Regular.ttf");

pub struct FontRaster {
    pub font: Font,
    /// Logical font size in points
    pub size: f32,
    /// Device pixel ratio (e.g. 2.0 on Retina)
    pub scale: f32,
    /// Cell dimensions in PHYSICAL pixels
    pub cell_w: usize,
    pub cell_h: usize,
    /// Baseline offset from top of cell, in physical pixels
    pub baseline: usize,
    /// Coverage cache: char → (metrics, single-channel bitmap).
    /// Avoids re-rasterizing glyphs that appear on multiple cells.
    coverage_cache: RefCell<HashMap<char, (Metrics, Vec<u8>)>>,
}

impl FontRaster {
    pub fn new(size: f32, scale: f32) -> Self {
        let font = Font::from_bytes(NERD_FONT_BYTES, FontSettings::default())
            .expect("failed to load bundled Nerd Font");

        let phys_size = size * scale;
        let (metrics, _) = font.rasterize('M', phys_size);
        let cell_w = metrics.advance_width.ceil() as usize;

        let line_metrics = font.horizontal_line_metrics(phys_size).unwrap();
        let ascent = line_metrics.ascent.ceil() as usize;
        let descent = (-line_metrics.descent).ceil() as usize;
        let cell_h = ascent + descent;
        let baseline = ascent;

        Self {
            font,
            size,
            scale,
            cell_w,
            cell_h,
            baseline,
            coverage_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Rasterize a single character into an RGBA buffer of cell_w × cell_h physical pixels.
    /// Glyph shapes are cached after the first rasterization — only color blending is
    /// repeated per call, which is a simple multiply-add over the bitmap pixels.
    /// Fallback glyph for characters not present in the font (glyph index 0 = notdef).
    /// Prefers Nerd Font equivalents (guaranteed present in Hack Nerd Font Mono),
    /// then falls back to ASCII lookalikes.
    fn glyph_fallback(ch: char) -> char {
        match ch {
            // Dingbats check marks → Font Awesome check (U+F00C)
            '✓' | '✔' | '✅'               => '\u{F00C}',
            // Dingbats ballot X / cross → Font Awesome times (U+F00D)
            '✗' | '✘' | '✕' | '✖' | '❌' => '\u{F00D}',
            // General punctuation / typography
            '…'                             => '.',
            '—' | '–'                       => '-',
            _                               => '?',
        }
    }

    /// Render a character directly into `buf` at cell position (`dst_x`, `dst_y`).
    /// `stride` is the row width of `buf` in pixels (i.e. total columns * cell_w).
    /// Eliminates the per-cell Vec allocation and double-copy of the old API.
    pub fn render_char_into(
        &self,
        buf: &mut [u8],
        ch: char,
        fg: [u8; 4],
        bg: [u8; 4],
        dst_x: usize,
        dst_y: usize,
        stride: usize,
    ) {
        let ch = if self.font.lookup_glyph_index(ch) == 0 && !ch.is_ascii() {
            Self::glyph_fallback(ch)
        } else {
            ch
        };

        // Populate glyph shape cache on first sight
        {
            let mut cache = self.coverage_cache.borrow_mut();
            if !cache.contains_key(&ch) {
                let phys_size = self.size * self.scale;
                let (metrics, bitmap) = self.font.rasterize(ch, phys_size);
                cache.insert(ch, (metrics, bitmap));
            }
        }

        let cache = self.coverage_cache.borrow();
        let (metrics, bitmap) = &cache[&ch];

        // Fill cell background directly in the output buffer
        for cy in 0..self.cell_h {
            for cx in 0..self.cell_w {
                let idx = ((dst_y + cy) * stride + (dst_x + cx)) * 4;
                buf[idx..idx + 4].copy_from_slice(&bg);
            }
        }

        if metrics.width == 0 || metrics.height == 0 {
            return;
        }

        let glyph_y_off =
            self.baseline as isize
            - metrics.bounds.ymin.floor() as isize
            - metrics.height as isize;
        let glyph_x_off = metrics.bounds.xmin.floor() as isize;

        for gy in 0..metrics.height {
            for gx in 0..metrics.width {
                let px = glyph_x_off + gx as isize;
                let py = glyph_y_off + gy as isize;
                if px < 0
                    || py < 0
                    || px >= self.cell_w as isize
                    || py >= self.cell_h as isize
                {
                    continue;
                }
                let alpha = bitmap[gy * metrics.width + gx] as f32 / 255.0;
                let idx = ((dst_y + py as usize) * stride + (dst_x + px as usize)) * 4;
                buf[idx]     = (bg[0] as f32 * (1.0 - alpha) + fg[0] as f32 * alpha) as u8;
                buf[idx + 1] = (bg[1] as f32 * (1.0 - alpha) + fg[1] as f32 * alpha) as u8;
                buf[idx + 2] = (bg[2] as f32 * (1.0 - alpha) + fg[2] as f32 * alpha) as u8;
                buf[idx + 3] = (bg[3] as f32 * (1.0 - alpha) + 255.0 * alpha) as u8;
            }
        }
    }
}
