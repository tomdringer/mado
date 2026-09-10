use std::cell::RefCell;
use std::collections::HashMap;

use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::Properties;
use font_kit::source::SystemSource;

use fontdue::{Font as FdFont, FontSettings};

static NERD_FONT_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/HackNerdFontMono-Regular.ttf");

// ── System font loading ───────────────────────────────────────────────────────

/// Returns raw font bytes for a system font family via font-kit.
fn system_font_bytes(family: &str) -> Option<Vec<u8>> {
    let handle = SystemSource::new()
        .select_best_match(
            &[FamilyName::Title(family.to_string())],
            &Properties::new(),
        )
        .ok()?;
    match handle {
        // Memory variant already contains the extracted font bytes (handles TTCs correctly)
        Handle::Memory { bytes, .. } => Some(bytes.to_vec()),
        Handle::Path   { path, .. }  => std::fs::read(path).ok(),
    }
}

// ── Glyph cache ───────────────────────────────────────────────────────────────

struct CachedGlyph {
    x_off: i32,   // left edge within cell (physical px, y-down)
    y_off: i32,   // top edge within cell  (physical px, y-down)
    width: u32,
    height: u32,
    alpha: Vec<u8>, // width × height grayscale coverage (0 = transparent)
}

// ── FontRaster ────────────────────────────────────────────────────────────────

pub struct FontRaster {
    primary: FdFont,
    nerd: FdFont,
    /// Last-resort system font (Menlo/Monaco). Used when both primary and nerd
    /// produce a zero-size bitmap — covers common Unicode symbols (✓ ✗ etc.)
    /// that Hack Nerd Font Mono omits from its format-12 cmap.
    system: Option<FdFont>,
    primary_is_nerd: bool,
    pub size: f32,
    pub scale: f32,
    pub cell_w: usize,
    pub cell_h: usize,
    pub baseline: usize,
    cache: RefCell<HashMap<char, CachedGlyph>>,
}

impl FontRaster {
    /// `family` — system font family name (e.g. `"Anka/Coder"`).
    /// Empty string uses the bundled Hack Nerd Font Mono.
    pub fn new(size: f32, scale: f32, family: &str) -> Self {
        let phys_size = size * scale;

        let nerd = FdFont::from_bytes(NERD_FONT_BYTES, FontSettings::default())
            .expect("mado: bundled Nerd Font load failed");

        // System fallback — loaded once at startup, used only for characters missing from
        // both primary and Nerd Font. Menlo is Apple's standard terminal font and has
        // broad Unicode symbol coverage via its BMP cmap (✓ ✗ ▶ and similar).
        let system = ["Menlo", "Monaco"]
            .iter()
            .find_map(|name| {
                system_font_bytes(name)
                    .and_then(|b| FdFont::from_bytes(b.as_slice(), FontSettings::default()).ok())
            });

        let (primary, primary_is_nerd) = if !family.is_empty() {
            match system_font_bytes(family)
                .and_then(|b| FdFont::from_bytes(b.as_slice(), FontSettings::default()).ok())
            {
                Some(font) => {
                    eprintln!("mado: font loaded — \"{family}\" + Hack Nerd Font Mono (fallback)");
                    (font, false)
                }
                None => {
                    eprintln!("mado: font \"{family}\" not found or unreadable, \
                               falling back to bundled Hack Nerd Font Mono");
                    (FdFont::from_bytes(NERD_FONT_BYTES, FontSettings::default()).unwrap(), true)
                }
            }
        } else {
            eprintln!("mado: font loaded — Hack Nerd Font Mono (bundled)");
            (FdFont::from_bytes(NERD_FONT_BYTES, FontSettings::default()).unwrap(), true)
        };

        // Cell metrics — derived from the primary font at physical size
        let (mm, _) = primary.rasterize('M', phys_size);
        let cell_w  = mm.advance_width.ceil() as usize;
        let lm      = primary.horizontal_line_metrics(phys_size).unwrap();
        let ascent  = lm.ascent.ceil() as usize;
        let descent = (-lm.descent).ceil() as usize;
        let cell_h  = ascent + descent;
        let baseline = ascent;

        eprintln!(
            "mado: cell {}×{}px, baseline {}px (phys_size={})",
            cell_w, cell_h, baseline, phys_size,
        );

        Self {
            primary,
            nerd,
            system,
            primary_is_nerd,
            size,
            scale,
            cell_w,
            cell_h,
            baseline,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Rasterize `ch` with fontdue, choosing primary → Nerd Font → system fallback.
    ///
    /// Key insight: when `lookup_glyph_index` returns 0 the char is not in the font's
    /// cmap, but calling `rasterize` still renders the .notdef box (often 19×33px in
    /// Hack Nerd Font Mono). We therefore guard every rasterize call with an index
    /// check so we never accidentally accept a .notdef as the real glyph.
    fn rasterize_and_cache(&self, ch: char) {
        let phys_size = self.size * self.scale;

        // Try each font in order, stopping at the first real glyph (idx != 0, pixels > 0).
        // Always check lookup_glyph_index first: rasterize(ch) returns the .notdef box
        // (non-zero pixels) when the char isn't in the cmap, so we must not rely on
        // a zero-size result to detect missing glyphs.
        let result = self.try_rasterize(&self.primary, ch, phys_size)
            .or_else(|| {
                if self.primary_is_nerd { None }
                else { self.try_rasterize(&self.nerd, ch, phys_size) }
            })
            .or_else(|| {
                self.system.as_ref()
                    .and_then(|s| self.try_rasterize(s, ch, phys_size))
            });

        let (m, bitmap) = match result {
            Some(r) => r,
            None => {
                self.cache.borrow_mut().insert(ch, CachedGlyph {
                    x_off: 0, y_off: 0, width: 0, height: 0, alpha: vec![],
                });
                return;
            }
        };

        // Convert fontdue metrics (y-up baseline origin) to cell-local y-down coords.
        //   y_off = distance from cell top to top of glyph bitmap
        //         = baseline_from_top - ymin - height
        let x_off = m.xmin;
        let y_off = self.baseline as i32 - m.ymin as i32 - m.height as i32;

        self.cache.borrow_mut().insert(ch, CachedGlyph {
            x_off,
            y_off,
            width:  m.width  as u32,
            height: m.height as u32,
            alpha:  bitmap,
        });
    }

    /// Rasterize `ch` from `font` only if it has a real cmap entry (`idx != 0`).
    /// Returns `None` if the glyph is absent or produces zero pixels.
    /// `skip_index_check` is true when the font IS the Nerd Font used as primary
    /// (i.e. no custom font — in that case we trust it and skip the guard).
    fn try_rasterize(&self, font: &FdFont, ch: char, size: f32)
        -> Option<(fontdue::Metrics, Vec<u8>)>
    {
        if font.lookup_glyph_index(ch) == 0 { return None; }
        let (m, bm) = font.rasterize(ch, size);
        if m.width > 0 && m.height > 0 { Some((m, bm)) } else { None }
    }

    /// Pre-warm the glyph cache with printable ASCII so the first rendered
    /// frame has no rasterization stalls.
    pub fn prewarm(&self) {
        for ch in ' '..='~' {
            if !self.cache.borrow().contains_key(&ch) {
                self.rasterize_and_cache(ch);
            }
        }
    }

    /// Render `ch` directly into `buf` at cell position (`dst_x`, `dst_y`).
    /// `stride` is the row width of `buf` in pixels.
    pub fn render_char_into(
        &self,
        buf:    &mut [u8],
        ch:     char,
        fg:     [u8; 4],
        bg:     [u8; 4],
        dst_x:  usize,
        dst_y:  usize,
        stride: usize,
    ) {
        // Fill cell background — build one row of solid-colour pixels on the
        // stack then memcpy it per row.  This lets the compiler emit a single
        // vectorised store instead of scattered 4-byte writes.
        // 160 bytes covers cell_w ≤ 40px (generous for any font size/scale combo).
        let cw = self.cell_w;
        let row_bytes = cw * 4;
        let mut row_buf = [0u8; 160];
        for cx in 0..cw {
            row_buf[cx * 4..cx * 4 + 4].copy_from_slice(&bg);
        }
        let row_start = (dst_y * stride + dst_x) * 4;
        for cy in 0..self.cell_h {
            let dst = row_start + cy * stride * 4;
            buf[dst..dst + row_bytes].copy_from_slice(&row_buf[..row_bytes]);
        }

        if !self.cache.borrow().contains_key(&ch) {
            self.rasterize_and_cache(ch);
        }

        let cache = self.cache.borrow();
        let g = &cache[&ch];

        if g.width == 0 || g.height == 0 {
            return;
        }

        // Integer alpha blend — avoids all floating-point per pixel.
        // Uses the common fast approximation: (x * a) >> 8  ≈  (x * a) / 255
        // (error ≤ 1 LSB, invisible in practice).
        for gy in 0..g.height as i32 {
            for gx in 0..g.width as i32 {
                let px = g.x_off + gx;
                let py = g.y_off + gy;
                if px < 0 || py < 0
                    || px >= self.cell_w as i32
                    || py >= self.cell_h as i32
                {
                    continue;
                }
                let a = g.alpha[gy as usize * g.width as usize + gx as usize] as u32;
                if a == 0 { continue; }
                let inv = 255 - a;
                let idx = ((dst_y + py as usize) * stride + (dst_x + px as usize)) * 4;
                buf[idx]     = ((bg[0] as u32 * inv + fg[0] as u32 * a) >> 8) as u8;
                buf[idx + 1] = ((bg[1] as u32 * inv + fg[1] as u32 * a) >> 8) as u8;
                buf[idx + 2] = ((bg[2] as u32 * inv + fg[2] as u32 * a) >> 8) as u8;
                buf[idx + 3] = 255;
            }
        }
    }
}
