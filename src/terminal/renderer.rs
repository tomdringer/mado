use slint::SharedPixelBuffer;

use libghostty_vt::Terminal;
use libghostty_vt::render::{RenderState, RowIterator, CellIterator, Dirty};

use super::font::FontRaster;

/// Render terminal state into a pixel buffer using the libghostty-vt render API.
/// Returns None if nothing rendered.
///
/// The terminal's default background colour is treated as transparent (alpha=0)
/// so the Slint pane background shows through.
///
/// All objects use the global allocator so their `'alloc` lifetime is `'static`.
pub fn render_terminal(
    terminal: &Terminal<'static, '_>,
    render_state: &mut RenderState<'static>,
    row_it: &mut RowIterator<'static>,
    cell_it: &mut CellIterator<'static>,
    font: &FontRaster,
    phys_w: usize,
    phys_h: usize,
) -> Option<SharedPixelBuffer<slint::Rgba8Pixel>> {
    let snapshot = render_state.update(terminal).ok()?;

    let colors = snapshot.colors().ok()?;
    let def_fg: [u8; 3] = [colors.foreground.r, colors.foreground.g, colors.foreground.b];
    let def_bg: [u8; 3] = [colors.background.r, colors.background.g, colors.background.b];

    let cols = snapshot.cols().ok()? as usize;
    let rows = snapshot.rows().ok()? as usize;

    let cw = font.cell_w;
    let ch = font.cell_h;

    let cell_area_w = cols * cw;
    let cell_area_h = rows * ch;
    let w = phys_w.max(cell_area_w);
    let h = phys_h.max(cell_area_h);

    if w == 0 || h == 0 {
        let _ = snapshot.set_dirty(Dirty::Clean);
        return None;
    }

    // Cursor info — in v0.2.2 there's no combined cursor(); use separate getters
    let cursor_visible = snapshot.cursor_visible().unwrap_or(false);
    let (cursor_col, cursor_row) = if let Some(pt) = snapshot.cursor_viewport().ok().flatten() {
        (pt.x as usize, pt.y as usize)
    } else {
        (usize::MAX, usize::MAX)
    };

    let mut buf = vec![0u8; w * h * 4];

    // Reuse text buffer across cells
    let mut text = String::with_capacity(8);

    // v0.2.2: rows are iterated sequentially; position = iteration index 0..rows
    let mut row_iter = row_it.update(&snapshot).ok()?;
    let mut row_y: usize = 0;

    while let Some(row) = row_iter.next() {
        if row_y >= rows { row_y += 1; continue; }

        let mut cell_iter = match cell_it.update(row) {
            Ok(ci) => ci,
            Err(_) => { row_y += 1; continue; }
        };

        let mut col: usize = 0;
        while let Some(cell) = cell_iter.next() {
            if col >= cols { col += 1; continue; }

            let graphemes = cell.graphemes_len().unwrap_or(0);
            let ch_char = if graphemes > 0 {
                text.clear();
                if cell.graphemes_utf8(&mut text).is_ok() && !text.is_empty() {
                    text.chars().next().unwrap_or(' ')
                } else {
                    ' '
                }
            } else {
                ' '
            };

            // Resolve colours (palette already resolved by the API)
            let raw_fg = cell.fg_color().ok().flatten().map(|c| [c.r, c.g, c.b]).unwrap_or(def_fg);
            let raw_bg = cell.bg_color().ok().flatten().map(|c| [c.r, c.g, c.b]).unwrap_or(def_bg);

            let (is_bold, is_faint, is_inverse) = if cell.has_styling().unwrap_or(false) {
                match cell.style() {
                    Ok(style) => (style.bold, style.faint, style.inverse),
                    Err(_) => (false, false, false),
                }
            } else {
                (false, false, false)
            };

            let is_selected = cell.is_selected().unwrap_or(false);
            let is_cursor = cursor_visible && row_y == cursor_row && col == cursor_col;

            // Compute final fg/bg
            let (mut fg, mut bg) = if is_cursor {
                (raw_bg, raw_fg) // invert at cursor
            } else if is_selected {
                ([0xFF_u8, 0xFF, 0xFF], [0x26_u8, 0x4F, 0x78])
            } else if is_inverse {
                (raw_bg, raw_fg)
            } else {
                (raw_fg, raw_bg)
            };

            // Faint (dim/ghost text): blend fg 40% toward bg
            if is_faint && !is_cursor && !is_selected {
                fg = [
                    ((fg[0] as u16 * 4 + bg[0] as u16 * 6) / 10) as u8,
                    ((fg[1] as u16 * 4 + bg[1] as u16 * 6) / 10) as u8,
                    ((fg[2] as u16 * 4 + bg[2] as u16 * 6) / 10) as u8,
                ];
            }

            // Transparent for default bg
            let bg_alpha: u8 = if !is_cursor && !is_selected && bg == def_bg { 0 } else { 0xFF };

            let fg4 = [fg[0], fg[1], fg[2], 0xFF];
            let bg4 = [bg[0], bg[1], bg[2], bg_alpha];

            let dst_x = col * cw;
            let dst_y = row_y * ch;
            font.render_char_into(&mut buf, ch_char, fg4, bg4, dst_x, dst_y, w);

            // Fake bold: overdraw shifted 1px right
            if is_bold && ch_char != ' ' {
                font.render_char_into(&mut buf, ch_char, fg4, bg4, dst_x + 1, dst_y, w);
            }

            col += 1;
        }

        row_y += 1;
    }

    let _ = snapshot.set_dirty(Dirty::Clean);

    let mut pixel_buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(w as u32, h as u32);
    pixel_buf.make_mut_bytes().copy_from_slice(&buf);
    Some(pixel_buf)
}
