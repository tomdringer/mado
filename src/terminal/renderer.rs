use slint::SharedPixelBuffer;
use std::sync::{Arc, Mutex};

use super::terminal_state::{TerminalState, DEFAULT_BG};
use super::font::FontRaster;

/// Render terminal state into a pixel buffer. Returns None if nothing changed.
/// Pass `force=true` to always render (e.g. on resize).
pub fn render_terminal(
    state: &Arc<Mutex<TerminalState>>,
    font: &FontRaster,
    force: bool,
) -> Option<SharedPixelBuffer<slint::Rgba8Pixel>> {
    let mut st = state.lock().unwrap();
    if !force && !st.dirty {
        return None;
    }
    st.dirty = false;

    let cols = st.cols;
    let rows = st.rows;
    let cw = font.cell_w;
    let ch = font.cell_h;
    let w = cols * cw;
    let h = rows * ch;

    if w == 0 || h == 0 {
        return None;
    }

    let scroll_offset = st.scroll_offset;
    let sb_len = st.scrollback.len();
    let transparent_bg = st.theme_bg.unwrap_or(DEFAULT_BG);
    let mut buf = vec![0u8; w * h * 4];

    for row in 0..rows {
        // Map display row → virtual row (scrollback rows followed by current screen rows).
        // virtual_row 0..sb_len = scrollback (oldest first)
        // virtual_row sb_len..sb_len+rows = current screen
        let virtual_row = (sb_len as isize) - (scroll_offset as isize) + (row as isize);

        for col in 0..cols {
            let cell = if virtual_row < 0 {
                super::terminal_state::Cell::default()
            } else if (virtual_row as usize) < sb_len {
                st.scrollback[virtual_row as usize]
                    .get(col)
                    .cloned()
                    .unwrap_or_default()
            } else {
                let screen_row = (virtual_row as usize) - sb_len;
                st.cells
                    .get(screen_row * cols + col)
                    .cloned()
                    .unwrap_or_default()
            };

            // Only show cursor at live view (scroll_offset == 0)
            let is_cursor = scroll_offset == 0
                && row == st.cursor_row
                && col == st.cursor_col;

            let (fg, mut bg) = if is_cursor {
                (cell.bg, cell.fg)
            } else {
                (cell.fg, cell.bg)
            };

            // Treat the theme background as transparent so the Slint pane
            // background shows through. transparent_bg is detected automatically
            // from the first full-screen erase; falls back to DEFAULT_BG.
            if bg == transparent_bg || bg == DEFAULT_BG {
                bg[3] = 0;
            }

            let dst_x = col * cw;
            let dst_y = row * ch;
            font.render_char_into(&mut buf, cell.ch, fg, bg, dst_x, dst_y, w);
        }
    }

    let mut pixel_buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(w as u32, h as u32);
    pixel_buf.make_mut_bytes().copy_from_slice(&buf);
    Some(pixel_buf)
}
