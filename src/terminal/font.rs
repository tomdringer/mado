use std::cell::RefCell;
use std::collections::HashMap;
use std::os::raw::c_void;

use core_graphics::base::CGFloat;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::data_provider::CGDataProvider;
use core_graphics::font::CGFont;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_text::font::{new_from_CGFont, new_from_name, CTFont};
use core_text::font_descriptor::kCTFontOrientationHorizontal;

use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::Properties;
use font_kit::source::SystemSource;

use fontdue::{Font as FdFont, FontSettings};

static NERD_FONT_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/HackNerdFontMono-Regular.ttf");

// ── Font loading ──────────────────────────────────────────────────────────────

fn ct_font_from_bytes(bytes: &[u8], size: f64) -> CTFont {
    let provider = unsafe { CGDataProvider::from_slice(bytes) };
    let cg_font  = CGFont::from_data_provider(provider)
        .expect("failed to create CGFont from bytes");
    new_from_CGFont(&cg_font, size)
}

fn ct_font_from_system(family: &str, size: f64) -> Option<CTFont> {
    new_from_name(family, size).ok()
}

/// Returns the raw bytes for a system font found via font-kit.
/// Used to feed into fontdue for metric calculations.
fn system_font_bytes(family: &str) -> Option<Vec<u8>> {
    let handle = SystemSource::new()
        .select_best_match(
            &[FamilyName::Title(family.to_string())],
            &Properties::new(),
        )
        .ok()?;
    match handle {
        Handle::Path { path, .. }    => std::fs::read(path).ok(),
        Handle::Memory { bytes, .. } => Some(bytes.to_vec()),
    }
}

// ── Glyph cache ───────────────────────────────────────────────────────────────

struct CachedGlyph {
    x_off: i32,  // left edge within cell (physical px, can be negative)
    y_off: i32,  // top edge within cell  (physical px, y-down from cell top)
    width: u32,
    height: u32,
    alpha: Vec<u8>,  // width × height single-channel coverage (0 = transparent)
}

// ── FontRaster ────────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub struct FontRaster {
    /// Primary CTFont — used for rasterizing most characters.
    ct_primary: CTFont,
    /// Bundled Hack Nerd Font — fallback for glyphs missing in primary.
    ct_nerd: CTFont,
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
    ///
    /// Cell metrics are computed via fontdue (reliable at any scale).
    /// Glyph rasterization uses Core Text directly with font smoothing
    /// disabled, giving clean grayscale AA without LCD-smoothing blurriness.
    pub fn new(size: f32, scale: f32, family: &str) -> Self {
        let phys_size = size * scale;

        // ── CTFont objects (for Core Text rasterization) ──────────────────────
        let ct_nerd = ct_font_from_bytes(NERD_FONT_BYTES, phys_size as f64);

        let (ct_primary, primary_bytes, primary_is_nerd) = if !family.is_empty() {
            match (ct_font_from_system(family, phys_size as f64), system_font_bytes(family)) {
                (Some(font), Some(bytes)) => {
                    eprintln!("mado: font loaded — \"{family}\" (system) + Hack Nerd Font Mono (fallback)");
                    (font, bytes, false)
                }
                _ => {
                    eprintln!("mado: font \"{family}\" not found, falling back to bundled Hack Nerd Font Mono");
                    (ct_font_from_bytes(NERD_FONT_BYTES, phys_size as f64),
                     NERD_FONT_BYTES.to_vec(), true)
                }
            }
        } else {
            eprintln!("mado: font loaded — Hack Nerd Font Mono (bundled)");
            (ct_font_from_bytes(NERD_FONT_BYTES, phys_size as f64),
             NERD_FONT_BYTES.to_vec(), true)
        };

        // ── Cell metrics from fontdue (reliable physical-pixel values) ─────────
        let fd = FdFont::from_bytes(primary_bytes.as_slice(), FontSettings::default())
            .expect("fontdue metrics");

        let (m_metrics, _) = fd.rasterize('M', phys_size);
        let cell_w = m_metrics.advance_width.ceil() as usize;
        let lm     = fd.horizontal_line_metrics(phys_size).unwrap();
        let ascent  = lm.ascent.ceil()   as usize;
        let descent = (-lm.descent).ceil() as usize;
        let cell_h  = ascent + descent;
        let baseline = ascent;

        eprintln!(
            "mado: cell {}×{}px, baseline {}px (phys_size={})",
            cell_w, cell_h, baseline, phys_size,
        );

        Self {
            ct_primary,
            ct_nerd,
            primary_is_nerd,
            size,
            scale,
            cell_w,
            cell_h,
            baseline,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Rasterize `ch` via Core Text with font smoothing disabled,
    /// then insert the result into the cache.
    fn rasterize_and_cache(&self, ch: char) {
        // ── Pick font + glyph ID ─────────────────────────────────────────────
        // CTFont works with UTF-16; most terminal chars are in the BMP.
        let mut utf16 = [0u16; 2];
        let mut glyphs = [0u16; 2];
        let units = ch.encode_utf16(&mut utf16);
        let len = units.len();

        let has_primary = if !self.primary_is_nerd {
            unsafe {
                self.ct_primary.get_glyphs_for_characters(
                    utf16.as_ptr(), glyphs.as_mut_ptr(), len as _);
            }
            glyphs[0] != 0
        } else {
            false
        };

        let (ct_font, glyph) = if has_primary || self.primary_is_nerd {
            if !has_primary {
                unsafe {
                    self.ct_primary.get_glyphs_for_characters(
                        utf16.as_ptr(), glyphs.as_mut_ptr(), len as _);
                }
            }
            (&self.ct_primary, glyphs[0])
        } else {
            // Primary doesn't have it — try Nerd Font fallback
            let mut nerd_glyphs = [0u16; 2];
            unsafe {
                self.ct_nerd.get_glyphs_for_characters(
                    utf16.as_ptr(), nerd_glyphs.as_mut_ptr(), len as _);
            }
            (&self.ct_nerd, nerd_glyphs[0])
        };

        if glyph == 0 {
            self.cache.borrow_mut().insert(ch, CachedGlyph {
                x_off: 0, y_off: 0, width: 0, height: 0, alpha: vec![],
            });
            return;
        }

        // ── Bounding rect (Core Text y-up coords, in points = physical px here) ─
        let rect = ct_font.get_bounding_rects_for_glyphs(
            kCTFontOrientationHorizontal,
            &[glyph],
        );

        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            self.cache.borrow_mut().insert(ch, CachedGlyph {
                x_off: 0, y_off: 0, width: 0, height: 0, alpha: vec![],
            });
            return;
        }

        // Add a 1px margin on all sides so antialiased edges aren't clipped.
        let margin  = 1usize;
        let g_w     = rect.size.width.ceil()  as usize + margin * 2;
        let g_h     = rect.size.height.ceil() as usize + margin * 2;

        // Cell position: rect.origin is lower-left in y-up, baseline at y=0.
        let x_off = rect.origin.x.floor() as i32 - margin as i32;
        // Top of glyph in cell (y-down): baseline_from_top − distance above baseline.
        let y_off = self.baseline as i32
            - (rect.origin.y + rect.size.height).ceil() as i32
            - margin as i32;

        // ── Create grayscale CGBitmapContext ──────────────────────────────────
        // Pre-allocate pixel buffer; pass raw pointer so we can read it back
        // without a separate CGBitmapContextGetData call.
        let mut pixels = vec![0u8; g_w * g_h];
        let data_ptr   = pixels.as_mut_ptr() as *mut c_void;

        let cs  = CGColorSpace::create_device_gray();
        let ctx = CGContext::create_bitmap_context(
            Some(data_ptr), g_w, g_h,
            8,    // bits per component
            g_w,  // bytes per row (no stride padding — 1 byte per pixel)
            &cs,
            0,    // kCGImageAlphaNone
        );

        // Disable font smoothing — this removes the LCD antialiasing blurriness
        // that occurs when macOS applies subpixel coverage to a grayscale context.
        ctx.set_should_smooth_fonts(false);
        ctx.set_allows_font_smoothing(false);
        ctx.set_should_antialias(true);
        ctx.set_allows_antialiasing(true);

        // Fill black background, set white fill for glyph
        ctx.set_gray_fill_color(0.0, 1.0);
        ctx.fill_rect(CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(g_w as CGFloat, g_h as CGFloat),
        ));
        ctx.set_gray_fill_color(1.0, 1.0);

        // Position: shift glyph so its lower-left sits at (margin, margin) in
        // context coordinates (y-up; y=0 is the bottom of the bitmap).
        let draw_x = -rect.origin.x + margin as CGFloat;
        let draw_y = -rect.origin.y + margin as CGFloat;
        ct_font.draw_glyphs(&[glyph], &[CGPoint::new(draw_x, draw_y)], ctx.clone());

        // Context is dropped here; `pixels` now contains the rendered coverage.
        drop(ctx);

        // CGBitmapContext writes pixels directly in y-down, x-left-to-right order.
        // No flip needed.
        let alpha = pixels;

        self.cache.borrow_mut().insert(ch, CachedGlyph {
            x_off,
            y_off,
            width: g_w as u32,
            height: g_h as u32,
            alpha,
        });
    }

    /// Render a character directly into `buf` at cell position (`dst_x`, `dst_y`).
    /// `stride` is the row width of `buf` in pixels (total columns × cell_w).
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
        // Fill cell background
        for cy in 0..self.cell_h {
            for cx in 0..self.cell_w {
                let idx = ((dst_y + cy) * stride + (dst_x + cx)) * 4;
                buf[idx..idx + 4].copy_from_slice(&bg);
            }
        }

        if !self.cache.borrow().contains_key(&ch) {
            self.rasterize_and_cache(ch);
        }

        let cache = self.cache.borrow();
        let g = &cache[&ch];

        if g.width == 0 || g.height == 0 {
            return;
        }

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
                let a = g.alpha[gy as usize * g.width as usize + gx as usize] as f32 / 255.0;
                if a == 0.0 { continue; }
                let idx = ((dst_y + py as usize) * stride + (dst_x + px as usize)) * 4;
                buf[idx]     = (bg[0] as f32 * (1.0 - a) + fg[0] as f32 * a) as u8;
                buf[idx + 1] = (bg[1] as f32 * (1.0 - a) + fg[1] as f32 * a) as u8;
                buf[idx + 2] = (bg[2] as f32 * (1.0 - a) + fg[2] as f32 * a) as u8;
                buf[idx + 3] = 255;
            }
        }
    }
}
