//! mado-claude — PTY-based terminal plugin for Mado
//! Spawns `claude` (or configured command) in a PTY and renders it as RGBA frames.
//! stdin:  newline-delimited JSON events from Mado
//! stdout: raw RGBA frames using the MADO frame protocol

use std::io::{self, BufRead, Read, Write};
use std::sync::mpsc;
use std::thread;
use fontdue::{Font, FontSettings};
use serde_json::Value;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use libghostty_vt::Terminal;
use libghostty_vt::render::{RenderState, RowIterator, CellIterator, Dirty};
use libghostty_vt::terminal::{Options, ScrollViewport};

static FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/HackNerdFontMono-Regular.ttf");

// ── Helpers ───────────────────────────────────────────────────────────────────

fn shlex_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Write a MACT action message to stdout.
fn write_mact(json: &str) {
    let bytes = json.as_bytes();
    let len = bytes.len() as u32;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(b"MACT");
    let _ = out.write_all(&len.to_le_bytes());
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

const IDLE_NOTIFY_SECS: u64 = 600; // 10 minutes

// ── Selection state ───────────────────────────────────────────────────────────

/// Mouse/keyboard selection and cursor blink state (kept outside the terminal).
struct SelState {
    sel_start: Option<(usize, usize)>,
    sel_end:   Option<(usize, usize)>,
    sel_drag:  bool,
    kbd_sel_anchor: Option<(usize, usize)>,
    kbd_cursor: Option<(usize, usize)>,
    sel_flash_pending: bool,
    sel_flash_ticks: u32,
    cursor_blink_on: bool,
    /// Col count at last resize (for selection clamping).
    cols: usize,
    /// Row count at last resize.
    rows: usize,
    /// Scroll offset (rows scrolled back into scrollback; 0 = live view).
    scroll_offset: usize,
}

impl SelState {
    fn new(cols: usize, rows: usize) -> Self {
        SelState {
            sel_start: None,
            sel_end:   None,
            sel_drag:  false,
            kbd_sel_anchor: None,
            kbd_cursor: None,
            sel_flash_pending: false,
            sel_flash_ticks: 0,
            cursor_blink_on: true,
            cols,
            rows,
            scroll_offset: 0,
        }
    }
}

// ── Messages ──────────────────────────────────────────────────────────────────

enum Msg {
    Bytes(Vec<u8>),
    Line(String),
    PtyEof,
}

// ── Font metrics ──────────────────────────────────────────────────────────────

const LINE_GAP: usize = 0;

struct Fm {
    font: Font,
    cell_w: usize,
    cell_h: usize,
    #[allow(dead_code)]
    glyph_h: usize,
    baseline: usize,
    phys_size: f32,
}

impl Fm {
    fn new(phys_size: f32) -> Self {
        let font = Font::from_bytes(FONT_BYTES, FontSettings::default())
            .expect("mado-claude: font load failed");
        let (mm, _) = font.rasterize('M', phys_size);
        let cell_w = mm.advance_width.ceil() as usize;
        let lm = font.horizontal_line_metrics(phys_size).unwrap();
        let ascent  = lm.ascent.ceil() as usize;
        let descent = (-lm.descent).ceil() as usize;
        let glyph_h = ascent + descent;
        let cell_h  = glyph_h + LINE_GAP;
        Fm { font, cell_w, cell_h, glyph_h, baseline: ascent, phys_size }
    }
}

// ── Selection text extraction ─────────────────────────────────────────────────

fn in_sel(col: usize, row: usize, s: (usize, usize), e: (usize, usize)) -> bool {
    // s and e are (row, col); normalized s <= e
    if row < s.0 || row > e.0 { return false; }
    if s.0 == e.0 { col >= s.1 && col < e.1 }
    else if row == s.0 { col >= s.1 }
    else if row == e.0 { col < e.1 }
    else { true }
}

fn selection_text_from_terminal(
    terminal: &Terminal<'static, '_>,
    render_state: &mut RenderState<'static>,
    row_it: &mut RowIterator<'static>,
    cell_it: &mut CellIterator<'static>,
    sel: &SelState,
) -> String {
    let (Some(ss), Some(se)) = (sel.sel_start, sel.sel_end) else { return String::new() };
    let (s, e) = if ss <= se { (ss, se) } else { (se, ss) };
    if s == e { return String::new(); }

    let snapshot = match render_state.update(terminal) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let rows = snapshot.rows().unwrap_or(0) as usize;
    let cols = snapshot.cols().unwrap_or(0) as usize;
    let mut row_iter = match row_it.update(&snapshot) {
        Ok(r) => r,
        Err(_) => { let _ = snapshot.set_dirty(Dirty::Clean); return String::new(); }
    };

    let mut out = String::new();
    let mut row_y: usize = 0;
    let mut text = String::with_capacity(8);

    while let Some(row) = row_iter.next() {
        if row_y > e.0 || row_y >= rows { row_y += 1; continue; }
        if row_y < s.0 { row_y += 1; continue; }

        let mut cell_iter = match cell_it.update(row) {
            Ok(ci) => ci,
            Err(_) => { row_y += 1; continue; }
        };

        let mut line = String::new();
        let mut col: usize = 0;

        while let Some(cell) = cell_iter.next() {
            if col >= cols { col += 1; continue; }
            let graphemes = cell.graphemes_len().unwrap_or(0);
            let ch_char = if graphemes > 0 {
                text.clear();
                if cell.graphemes_utf8(&mut text).is_ok() && !text.is_empty() {
                    text.chars().next().unwrap_or(' ')
                } else { ' ' }
            } else { ' ' };

            if in_sel(col, row_y, s, e) {
                line.push(ch_char);
            }
            col += 1;
        }

        let trimmed = line.trim_end_matches(' ');
        if !out.is_empty() { out.push('\n'); }
        out.push_str(trimmed);
        row_y += 1;
    }

    let _ = snapshot.set_dirty(Dirty::Clean);
    out
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn draw_glyph(buf: &mut [u8], stride: usize, fm: &Fm, ch: char,
              px: usize, baseline_y: usize, fg: [u8; 3], bg: [u8; 3]) {
    if fm.font.lookup_glyph_index(ch) == 0 { return; }
    let (m, bitmap) = fm.font.rasterize(ch, fm.phys_size);
    if m.width == 0 || m.height == 0 { return; }
    let rows_total = buf.len() / (stride * 4);
    let draw_x = px as i32 + m.xmin;
    let draw_y = baseline_y as i32 - m.ymin as i32 - m.height as i32;
    for gy in 0..m.height {
        for gx in 0..m.width {
            let ax = draw_x + gx as i32;
            let ay = draw_y + gy as i32;
            if ax < 0 || ay < 0 || ax >= stride as i32 || ay >= rows_total as i32 { continue; }
            let cov = bitmap[gy * m.width + gx] as f32 / 255.0;
            if cov < 0.001 { continue; }
            let i = (ay as usize * stride + ax as usize) * 4;
            if i + 3 >= buf.len() { continue; }
            buf[i]   = (bg[0] as f32 * (1.0 - cov) + fg[0] as f32 * cov) as u8;
            buf[i+1] = (bg[1] as f32 * (1.0 - cov) + fg[1] as f32 * cov) as u8;
            buf[i+2] = (bg[2] as f32 * (1.0 - cov) + fg[2] as f32 * cov) as u8;
            buf[i+3] = 255;
        }
    }
}

fn render(
    terminal: &Terminal<'static, '_>,
    render_state: &mut RenderState<'static>,
    row_it: &mut RowIterator<'static>,
    cell_it: &mut CellIterator<'static>,
    fm: &Fm,
    sel: &SelState,
) -> Vec<u8> {
    let snapshot = match render_state.update(terminal) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let cols = snapshot.cols().unwrap_or(0) as usize;
    let rows = snapshot.rows().unwrap_or(0) as usize;
    if cols == 0 || rows == 0 {
        let _ = snapshot.set_dirty(Dirty::Clean);
        return vec![];
    }

    let colors = snapshot.colors().unwrap();
    let def_fg = [colors.foreground.r, colors.foreground.g, colors.foreground.b];
    let def_bg = [colors.background.r, colors.background.g, colors.background.b];

    let phys_w = cols * fm.cell_w;
    let phys_h = rows * fm.cell_h;
    let mut buf = vec![0u8; phys_w * phys_h * 4];

    // Determine selection normalized
    let norm_sel = if let (Some(ss), Some(se)) = (sel.sel_start, sel.sel_end) {
        let (s, e) = if ss <= se { (ss, se) } else { (se, ss) };
        if s != e { Some((s, e)) } else { None }
    } else {
        None
    };

    // Cursor info from snapshot
    let cursor_visible_in_snap = snapshot.cursor_visible().unwrap_or(false);
    let cursor_viewport = snapshot.cursor_viewport().ok().flatten();

    // The "logical" cursor: kbd_cursor overrides during keyboard selection
    let (pty_cx, pty_cy) = if let Some(pt) = &cursor_viewport {
        (pt.x as usize, pt.y as usize)
    } else {
        (0, 0)
    };
    let (cursor_col, cursor_row) = sel.kbd_cursor.unwrap_or((pty_cy, pty_cx));
    // Note: kbd_cursor stores (row, col) order; pty cursor from viewport is (x=col, y=row)
    let cursor_col_actual = cursor_col; // actually stores row when using kbd_cursor
    let cursor_row_actual = cursor_row;

    // Re-derive correctly — pty_cx/pty_cy are col/row from viewport
    let (disp_cursor_col, disp_cursor_row) = if let Some((kr, kc)) = sel.kbd_cursor {
        (kc, kr)
    } else {
        (pty_cx, pty_cy)
    };

    // Render cells
    let mut row_iter = match row_it.update(&snapshot) {
        Ok(r) => r,
        Err(_) => { let _ = snapshot.set_dirty(Dirty::Clean); return buf; }
    };

    let mut text = String::with_capacity(8);
    let mut row_y: usize = 0;

    while let Some(row) = row_iter.next() {
        if row_y >= rows { row_y += 1; continue; }

        let mut cell_iter = match cell_it.update(row) {
            Ok(ci) => ci,
            Err(_) => { row_y += 1; continue; }
        };

        let px_y = row_y * fm.cell_h;
        let mut col: usize = 0;

        while let Some(cell) = cell_iter.next() {
            if col >= cols { col += 1; continue; }

            let graphemes = cell.graphemes_len().unwrap_or(0);
            let ch_char = if graphemes > 0 {
                text.clear();
                if cell.graphemes_utf8(&mut text).is_ok() && !text.is_empty() {
                    text.chars().next().unwrap_or(' ')
                } else { ' ' }
            } else { ' ' };

            let raw_fg = cell.fg_color().ok().flatten().map(|c| [c.r, c.g, c.b]).unwrap_or(def_fg);
            let raw_bg = cell.bg_color().ok().flatten().map(|c| [c.r, c.g, c.b]).unwrap_or(def_bg);

            let (is_bold, is_faint, is_inverse) = if cell.has_styling().unwrap_or(false) {
                match cell.style() {
                    Ok(s) => (s.bold, s.faint, s.inverse),
                    Err(_) => (false, false, false),
                }
            } else {
                (false, false, false)
            };

            let is_selected = match &norm_sel {
                Some((s, e)) => in_sel(col, row_y, *s, *e),
                None => false,
            };
            let is_cursor = sel.cursor_blink_on
                && sel.scroll_offset == 0
                && cursor_visible_in_snap
                && row_y == disp_cursor_row
                && col == disp_cursor_col;

            // Resolve fg/bg
            let (mut fg, mut bg) = if is_cursor {
                (raw_bg, raw_fg)
            } else if is_selected {
                ([0xFF_u8, 0xFF, 0xFF], [0x3a_u8, 0x5a, 0x8a])
            } else if is_inverse {
                (raw_bg, raw_fg)
            } else {
                (raw_fg, raw_bg)
            };

            // Dim/faint: blend fg 40% toward bg
            if is_faint && !is_cursor && !is_selected {
                fg = [
                    ((fg[0] as u16 * 4 + bg[0] as u16 * 6) / 10) as u8,
                    ((fg[1] as u16 * 4 + bg[1] as u16 * 6) / 10) as u8,
                    ((fg[2] as u16 * 4 + bg[2] as u16 * 6) / 10) as u8,
                ];
            }

            let px = col * fm.cell_w;

            // Fill background
            for dy in 0..fm.cell_h {
                for dx in 0..fm.cell_w {
                    let i = ((px_y + dy) * phys_w + (px + dx)) * 4;
                    if i + 3 < buf.len() {
                        buf[i]   = bg[0];
                        buf[i+1] = bg[1];
                        buf[i+2] = bg[2];
                        buf[i+3] = 255;
                    }
                }
            }

            if ch_char != ' ' {
                let baseline_y = px_y + fm.baseline;
                draw_glyph(&mut buf, phys_w, fm, ch_char, px, baseline_y, fg, bg);
                if is_bold {
                    draw_glyph(&mut buf, phys_w, fm, ch_char, px + 1, baseline_y, fg, bg);
                }
            }

            col += 1;
        }

        row_y += 1;
    }

    let _ = snapshot.set_dirty(Dirty::Clean);
    buf
}

// ── Write MADO frame ──────────────────────────────────────────────────────────

fn write_frame(phys_w: u32, phys_h: u32, pixels: &[u8]) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(b"MADO");
    let _ = out.write_all(&phys_w.to_le_bytes());
    let _ = out.write_all(&phys_h.to_le_bytes());
    let _ = out.write_all(pixels);
    let _ = out.flush();
}

// ── Key translation ───────────────────────────────────────────────────────────

fn key_text_to_bytes(text: &str) -> Vec<u8> {
    match text {
        "\u{0010}" | "\u{0015}" | "\u{0011}" | "\u{0016}" |
        "\u{0012}" | "\u{0013}" | "\u{0014}" | "\u{0017}" |
        "\u{0018}" | "\u{0019}" => vec![],
        "\u{0008}" => vec![0x7F],
        "\u{007F}" => vec![0x1B, b'[', b'3', b'~'],
        "\u{F700}" => vec![0x1B, b'[', b'A'],
        "\u{F701}" => vec![0x1B, b'[', b'B'],
        "\u{F702}" => vec![0x1B, b'[', b'D'],
        "\u{F703}" => vec![0x1B, b'[', b'C'],
        "\u{F729}" => vec![0x1B, b'[', b'H'],
        "\u{F72B}" => vec![0x1B, b'[', b'F'],
        "\u{F72C}" => vec![0x1B, b'[', b'5', b'~'],
        "\u{F72D}" => vec![0x1B, b'[', b'6', b'~'],
        "\u{F704}" => vec![0x1B, b'O', b'P'],
        "\u{F705}" => vec![0x1B, b'O', b'Q'],
        "\u{F706}" => vec![0x1B, b'O', b'R'],
        "\u{F707}" => vec![0x1B, b'O', b'S'],
        "\u{F708}" => vec![0x1B, b'[', b'1', b'5', b'~'],
        "\u{F709}" => vec![0x1B, b'[', b'1', b'7', b'~'],
        "\u{F70A}" => vec![0x1B, b'[', b'1', b'8', b'~'],
        "\u{F70B}" => vec![0x1B, b'[', b'1', b'9', b'~'],
        "\u{F70C}" => vec![0x1B, b'[', b'2', b'0', b'~'],
        "\u{F70D}" => vec![0x1B, b'[', b'2', b'1', b'~'],
        "\u{F70E}" => vec![0x1B, b'[', b'2', b'3', b'~'],
        "\u{F70F}" => vec![0x1B, b'[', b'2', b'4', b'~'],
        "\r" | "\n" => vec![0x0D],
        _ => text.as_bytes().to_vec(),
    }
}

fn key_to_bytes(text: &str, ctrl: bool, meta: bool, _shift: bool) -> Vec<u8> {
    if matches!(text,
        "\u{0010}" | "\u{0015}" | "\u{0011}" | "\u{0016}" |
        "\u{0012}" | "\u{0013}" | "\u{0014}" | "\u{0017}" | "\u{0018}" | "\u{0019}"
    ) { return vec![]; }

    if ctrl {
        if let Some(ch) = text.chars().next() {
            if text.chars().count() == 1 {
                let ctrl_byte: Option<u8> = match ch {
                    'a'..='z' => Some(ch as u8 - b'a' + 1),
                    'A'..='Z' => Some(ch as u8 - b'A' + 1),
                    '[' => Some(0x1B),
                    '\\' => Some(0x1C),
                    ']' => Some(0x1D),
                    '^' => Some(0x1E),
                    '_' => Some(0x1F),
                    ' ' => Some(0x00),
                    _ => None,
                };
                if let Some(b) = ctrl_byte {
                    let bytes = vec![b];
                    if meta {
                        let mut v = vec![0x1Bu8];
                        v.extend_from_slice(&bytes);
                        return v;
                    }
                    return bytes;
                }
            }
        }
    }

    let bytes = key_text_to_bytes(text);
    if bytes.is_empty() { return vec![]; }

    if meta {
        let mut v = vec![0x1Bu8];
        v.extend_from_slice(&bytes);
        return v;
    }
    bytes
}

// ── Config loading ────────────────────────────────────────────────────────────

fn load_command() -> String {
    let path = match std::env::var("HOME") {
        Ok(h) => std::path::PathBuf::from(h).join(".config/mado/ai.toml"),
        Err(_) => return "claude".to_string(),
    };
    let Ok(content) = std::fs::read_to_string(&path) else { return "claude".to_string() };
    let Ok(table) = content.parse::<toml::Table>() else { return "claude".to_string() };
    table.get("command").and_then(|v| v.as_str()).unwrap_or("claude").to_string()
}

// ── Event handling ────────────────────────────────────────────────────────────

fn handle_event(
    sel: &mut SelState,
    terminal: &mut Terminal<'static, 'static>,
    render_state: &mut RenderState<'static>,
    row_it: &mut RowIterator<'static>,
    cell_it: &mut CellIterator<'static>,
    json: &str,
    pty_writer: &std::sync::Arc<std::sync::Mutex<Box<dyn Write + Send>>>,
    fm: &Fm,
    master: &dyn portable_pty::MasterPty,
) -> bool {
    let v: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return false,
    };

    match v["type"].as_str().unwrap_or("") {
        "resize" => {
            let width  = v["width"].as_u64().unwrap_or(800) as usize;
            let height = v["height"].as_u64().unwrap_or(600) as usize;
            let new_cols = (width / fm.cell_w.max(1)).max(1);
            let new_rows = (height / fm.cell_h.max(1)).max(1);
            sel.cols = new_cols;
            sel.rows = new_rows;
            let _ = terminal.resize(new_cols as u16, new_rows as u16,
                                    fm.cell_w as u32, fm.cell_h as u32);
            let _ = master.resize(PtySize {
                rows: new_rows as u16,
                cols: new_cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            });
            true
        }
        "key" => {
            let text  = v["text"].as_str().unwrap_or("");
            let ctrl  = v["ctrl"].as_bool().unwrap_or(false);
            let meta  = v["meta"].as_bool().unwrap_or(false);
            let shift = v["shift"].as_bool().unwrap_or(false);

            // Cmd+C: copy selection if active, otherwise forward ^C to PTY
            if (meta || ctrl) && text == "c" {
                let sel_text = selection_text_from_terminal(
                    terminal, render_state, row_it, cell_it, sel);
                if !sel_text.is_empty() {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(sel_text);
                    }
                    sel.sel_start = None;
                    sel.sel_end   = None;
                    sel.kbd_sel_anchor = None;
                    sel.kbd_cursor = None;
                    return true;
                }
            }

            let is_modifier_only = matches!(text,
                "\u{0010}" | "\u{0015}" | "\u{0011}" | "\u{0016}" |
                "\u{0012}" | "\u{0013}" | "\u{0014}" | "\u{0017}" | "\u{0018}" | "\u{0019}"
            );
            if is_modifier_only { return true; }

            let is_shift_arrow = shift && matches!(text,
                "\u{F700}" | "\u{F701}" | "\u{F702}" | "\u{F703}" | "\u{F729}" | "\u{F72B}"
            );
            if is_shift_arrow {
                sel.sel_flash_pending = false;
                sel.sel_flash_ticks = 0;

                let cols = sel.cols;
                let rows = sel.rows;
                // kbd_cursor stores (row, col)
                let (cur_r, cur_c) = sel.kbd_cursor.unwrap_or_else(|| {
                    let cx = terminal.cursor_x().unwrap_or(0) as usize;
                    let cy = terminal.cursor_y().unwrap_or(0) as usize;
                    (cy, cx)
                });
                if sel.kbd_sel_anchor.is_none() {
                    sel.kbd_sel_anchor = Some((cur_r, cur_c));
                }
                let (new_r, new_c) = if ctrl || meta {
                    match text {
                        "\u{F702}" | "\u{F729}" => (cur_r, 0),
                        "\u{F703}" | "\u{F72B}" => (cur_r, cols.saturating_sub(1)),
                        "\u{F700}" => (0, cur_c),
                        "\u{F701}" => (rows.saturating_sub(1), cur_c),
                        _ => (cur_r, cur_c),
                    }
                } else {
                    match text {
                        "\u{F700}" => (cur_r.saturating_sub(1), cur_c),
                        "\u{F701}" => ((cur_r + 1).min(rows.saturating_sub(1)), cur_c),
                        "\u{F702}" => {
                            if cur_c > 0 { (cur_r, cur_c - 1) }
                            else if cur_r > 0 { (cur_r - 1, cols.saturating_sub(1)) }
                            else { (cur_r, cur_c) }
                        }
                        "\u{F703}" => {
                            if cur_c + 1 < cols { (cur_r, cur_c + 1) }
                            else if cur_r + 1 < rows { (cur_r + 1, 0) }
                            else { (cur_r, cur_c) }
                        }
                        "\u{F729}" => (cur_r, 0),
                        "\u{F72B}" => (cur_r, cols.saturating_sub(1)),
                        _ => (cur_r, cur_c),
                    }
                };
                sel.kbd_cursor = Some((new_r, new_c));
                let anchor = sel.kbd_sel_anchor.unwrap();
                if anchor <= (new_r, new_c) {
                    sel.sel_start = Some(anchor);
                    sel.sel_end   = Some((new_r, new_c));
                } else {
                    sel.sel_start = Some((new_r, new_c));
                    sel.sel_end   = Some(anchor);
                }
                return true;
            }

            // Any other key: clear selection state; snap to live view
            sel.kbd_sel_anchor = None;
            sel.kbd_cursor = None;
            sel.sel_start = None;
            sel.sel_end   = None;
            sel.sel_flash_pending = false;

            // Snap viewport to bottom when user types
            terminal.scroll_viewport(ScrollViewport::Bottom);
            sel.scroll_offset = 0;

            if text == "\u{F703}" && !ctrl && !meta {
                let cx = terminal.cursor_x().unwrap_or(0) as usize;
                let cy = terminal.cursor_y().unwrap_or(0) as usize;
                sel.kbd_sel_anchor = Some((cy, cx));
                sel.sel_flash_pending = true;
                sel.sel_flash_ticks = 0;
            }

            let bytes = key_to_bytes(text, ctrl, meta, shift);
            if !bytes.is_empty() {
                if let Ok(mut w) = pty_writer.lock() {
                    let _ = w.write_all(&bytes);
                    let _ = w.flush();
                }
            }
            true
        }
        "paste" => {
            if let Some(t) = v["text"].as_str() {
                terminal.scroll_viewport(ScrollViewport::Bottom);
                sel.scroll_offset = 0;
                if let Ok(mut w) = pty_writer.lock() {
                    let _ = w.write_all(t.as_bytes());
                    let _ = w.flush();
                }
                true
            } else {
                false
            }
        }
        "chdir" => {
            if let Some(path) = v["path"].as_str() {
                let cmd = format!("cd {}\r", shlex_quote(path));
                if let Ok(mut w) = pty_writer.lock() {
                    let _ = w.write_all(cmd.as_bytes());
                    let _ = w.flush();
                }
            }
            true
        }
        "scroll" => {
            let delta = v["delta"].as_f64().unwrap_or(0.0);
            let lines = (delta.abs() / fm.cell_h as f64).ceil() as isize;
            if delta > 0.0 {
                terminal.scroll_viewport(ScrollViewport::Delta(lines));
                let sb = terminal.scrollback_rows().unwrap_or(0);
                sel.scroll_offset = (sel.scroll_offset + lines as usize).min(sb);
            } else if delta < 0.0 {
                terminal.scroll_viewport(ScrollViewport::Delta(-lines));
                sel.scroll_offset = sel.scroll_offset.saturating_sub(lines as usize);
            }
            sel.kbd_sel_anchor = None;
            sel.kbd_cursor = None;
            sel.sel_start = None;
            sel.sel_end   = None;
            true
        }
        "mouse_press" => {
            let x = v["x"].as_f64().unwrap_or(0.0) as usize;
            let y = v["y"].as_f64().unwrap_or(0.0) as usize;
            let col = (x / fm.cell_w.max(1)).min(sel.cols.saturating_sub(1));
            let row = (y / fm.cell_h.max(1)).min(sel.rows.saturating_sub(1));
            sel.kbd_sel_anchor = None;
            sel.kbd_cursor = None;
            sel.sel_flash_pending = false;
            sel.sel_start = Some((row, col));
            sel.sel_end   = Some((row, col));
            sel.sel_drag  = true;
            true
        }
        "mouse_move" => {
            if sel.sel_drag {
                let x = v["x"].as_f64().unwrap_or(0.0) as usize;
                let y = v["y"].as_f64().unwrap_or(0.0) as usize;
                let col = (x / fm.cell_w.max(1)).min(sel.cols.saturating_sub(1));
                let row = (y / fm.cell_h.max(1)).min(sel.rows.saturating_sub(1));
                sel.sel_end = Some((row, col));
                true
            } else { false }
        }
        "mouse_release" => {
            sel.sel_drag = false;
            if sel.sel_start == sel.sel_end {
                sel.sel_start = None;
                sel.sel_end   = None;
            }
            true
        }
        _ => false,
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let command = load_command();

    let font_size = std::env::var("MADO_FONT_SIZE")
        .ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(18.0);
    let scale = std::env::var("MADO_SCALE")
        .ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(2.0);
    let fm = Fm::new(font_size * scale);

    let init_cols: usize = 80;
    let init_rows: usize = 24;

    // Spawn PTY
    let pty_system = native_pty_system();
    let pty_pair = match pty_system.openpty(PtySize {
        rows: init_rows as u16,
        cols: init_cols as u16,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            // Show error frame — just write blank frame with some fallback
            eprintln!("mado-claude: PTY error: {e}");
            return;
        }
    };

    let login_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut cmd = CommandBuilder::new(&login_shell);
    cmd.args(["-lc", &command]);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("LANG", "en_US.UTF-8");

    let _child = match pty_pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mado-claude: spawn error: {e}");
            return;
        }
    };

    // Single writer wrapped in Arc<Mutex> for on_pty_write callback + user input
    let pty_writer = std::sync::Arc::new(std::sync::Mutex::new(
        pty_pair.master.take_writer().expect("pty writer")
    ));
    let master = pty_pair.master;
    let mut pty_reader = master.try_clone_reader().expect("pty reader");

    // Create libghostty-vt terminal
    let mut terminal: Terminal<'static, 'static> = Terminal::new(Options {
        cols: init_cols as u16,
        rows: init_rows as u16,
        max_scrollback: 3000,
    }).expect("Terminal::new failed");

    // Wire on_pty_write — DA response bytes go back to PTY
    {
        let pw = std::sync::Arc::clone(&pty_writer);
        let _ = terminal.on_pty_write(move |_term, data| {
            if let Ok(mut w) = pw.lock() {
                let _ = w.write_all(data);
            }
        });
    }

    let mut render_state: RenderState<'static> = RenderState::new().expect("RenderState::new failed");
    let mut row_it: RowIterator<'static> = RowIterator::new().expect("RowIterator::new failed");
    let mut cell_it: CellIterator<'static> = CellIterator::new().expect("CellIterator::new failed");
    let mut sel = SelState::new(init_cols, init_rows);

    let (tx, rx) = mpsc::sync_channel::<Msg>(1024);

    // Stdin reader thread
    {
        let tx = tx.clone();
        thread::spawn(move || {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                match line {
                    Ok(l) => { if tx.send(Msg::Line(l)).is_err() { break; } }
                    Err(_) => break,
                }
            }
        });
    }

    // PTY reader thread
    {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match pty_reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Msg::PtyEof);
                        break;
                    }
                    Ok(n) => {
                        let data = buf[..n].to_vec();
                        if tx.send(Msg::Bytes(data)).is_err() { break; }
                    }
                }
            }
        });
    }

    // Initial frame
    {
        let pixels = render(&terminal, &mut render_state, &mut row_it, &mut cell_it, &fm, &sel);
        write_frame((init_cols * fm.cell_w) as u32, (init_rows * fm.cell_h) as u32, &pixels);
    }

    let mut dirty = false;
    let mut blink_ticks: u32 = 0;
    const BLINK_PERIOD: u32 = 31;
    let mut last_pty_activity: Option<std::time::Instant> = None;
    let mut idle_notified = false;

    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(16)) {
            Ok(Msg::Bytes(bytes)) => {
                // Track cursor before to detect movement for flash
                let pre_cx = terminal.cursor_x().unwrap_or(0) as usize;
                let pre_cy = terminal.cursor_y().unwrap_or(0) as usize;

                terminal.vt_write(&bytes);

                // Right Arrow flash: once the PTY cursor moves, highlight accepted text
                if sel.sel_flash_pending {
                    let post_cx = terminal.cursor_x().unwrap_or(0) as usize;
                    let post_cy = terminal.cursor_y().unwrap_or(0) as usize;
                    if post_cy != pre_cy || post_cx != pre_cx {
                        if let Some(anchor) = sel.kbd_sel_anchor {
                            sel.sel_start = Some(anchor);
                            sel.sel_end   = Some((post_cy, post_cx));
                            sel.sel_flash_ticks = 12;
                        }
                        sel.sel_flash_pending = false;
                        sel.kbd_sel_anchor = None;
                    }
                }

                blink_ticks = 0;
                sel.cursor_blink_on = true;
                dirty = true;
                last_pty_activity = Some(std::time::Instant::now());
                idle_notified = false;
            }
            Ok(Msg::Line(json)) => {
                let changed = handle_event(
                    &mut sel,
                    &mut terminal,
                    &mut render_state,
                    &mut row_it,
                    &mut cell_it,
                    &json,
                    &pty_writer,
                    &fm,
                    &*master,
                );
                if changed { dirty = true; }
            }
            Ok(Msg::PtyEof) => {
                if dirty {
                    let cols = sel.cols;
                    let rows = sel.rows;
                    let pixels = render(&terminal, &mut render_state, &mut row_it, &mut cell_it, &fm, &sel);
                    write_frame((cols * fm.cell_w) as u32, (rows * fm.cell_h) as u32, &pixels);
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if sel.sel_flash_ticks > 0 {
                    sel.sel_flash_ticks -= 1;
                    if sel.sel_flash_ticks == 0 {
                        sel.sel_start = None;
                        sel.sel_end   = None;
                        dirty = true;
                    }
                }
                blink_ticks += 1;
                if blink_ticks >= BLINK_PERIOD {
                    blink_ticks = 0;
                    sel.cursor_blink_on = !sel.cursor_blink_on;
                    dirty = true;
                }
                if !idle_notified {
                    if let Some(t) = last_pty_activity {
                        if t.elapsed().as_secs() >= IDLE_NOTIFY_SECS {
                            write_mact(r#"{"action":"notify","title":"Claude","message":"Response complete"}"#);
                            idle_notified = true;
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if dirty {
            let cols = sel.cols;
            let rows = sel.rows;
            let pixels = render(&terminal, &mut render_state, &mut row_it, &mut cell_it, &fm, &sel);
            write_frame((cols * fm.cell_w) as u32, (rows * fm.cell_h) as u32, &pixels);
            dirty = false;
        }
    }
}
