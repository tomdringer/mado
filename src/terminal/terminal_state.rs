use std::collections::VecDeque;
use vte::{Params, Perform};

const SCROLLBACK_LIMIT: usize = 2000;

pub const DEFAULT_FG: [u8; 4] = [0xCC, 0xCC, 0xCC, 0xFF];
pub const DEFAULT_BG: [u8; 4] = [0x1E, 0x20, 0x30, 0xFF];

#[derive(Clone)]
pub struct Cell {
    pub ch: char,
    pub fg: [u8; 4],
    pub bg: [u8; 4],
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', fg: DEFAULT_FG, bg: DEFAULT_BG }
    }
}

pub struct TerminalState {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Cell>,
    pub cursor_col: usize,
    pub cursor_row: usize,
    pub cursor_visible: bool,
    pub scroll_top: usize,
    pub scroll_bot: usize,
    pub cur_fg: [u8; 4],
    pub cur_bg: [u8; 4],
    pub dirty: bool,
    pub bell: bool,
    /// Last working directory reported via OSC 7 (file://host/path).
    pub cwd: Option<String>,
    /// Rows that have scrolled off the top (oldest first).
    pub scrollback: VecDeque<Vec<Cell>>,
    /// How many rows back from the current screen the user is viewing.
    /// 0 = live view; N = N rows back into scrollback.
    pub scroll_offset: usize,
    /// The background colour to treat as transparent. Detected automatically
    /// on the first full-screen erase (ED 2/3), which every shell theme
    /// triggers at startup after setting its background colour.
    pub theme_bg: Option<[u8; 4]>,
    /// True when the alternate screen buffer (?1049h) is active.
    pub in_alt_screen: bool,
}

impl TerminalState {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cells = vec![Cell::default(); cols * rows];
        TerminalState {
            cols,
            rows,
            cells,
            cursor_col: 0,
            cursor_row: 0,
            cursor_visible: true,
            scroll_top: 0,
            scroll_bot: rows.saturating_sub(1),
            cur_fg: DEFAULT_FG,
            cur_bg: DEFAULT_BG,
            dirty: true,
            bell: false,
            cwd: None,
            scrollback: VecDeque::new(),
            scroll_offset: 0,
            theme_bg: None,
            in_alt_screen: false,
        }
    }

    /// Adjust the scroll offset by `delta` rows (positive = older, negative = newer).
    /// Clamped to [0, scrollback.len()]. No-op in alternate screen mode.
    pub fn adjust_scroll_offset(&mut self, delta: i32) {
        if self.in_alt_screen { return; }
        let max = self.scrollback.len();
        if delta > 0 {
            self.scroll_offset = (self.scroll_offset + delta as usize).min(max);
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub((-delta) as usize);
        }
        self.dirty = true;
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        if cols == self.cols && rows == self.rows { return; }
        let mut new_cells = vec![Cell::default(); cols * rows];
        let copy_rows = rows.min(self.rows);
        let copy_cols = cols.min(self.cols);
        for r in 0..copy_rows {
            for c in 0..copy_cols {
                new_cells[r * cols + c] = self.cells[r * self.cols + c].clone();
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = new_cells;
        self.scroll_bot = rows.saturating_sub(1);
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.dirty = true;
    }

    fn cell_idx(&self, col: usize, row: usize) -> usize {
        row * self.cols + col
    }

    fn put_char(&mut self, ch: char) {
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.newline();
        }
        let idx = self.cell_idx(self.cursor_col, self.cursor_row);
        self.cells[idx] = Cell { ch, fg: self.cur_fg, bg: self.cur_bg };
        self.cursor_col += 1;
        self.dirty = true;
    }

    fn newline(&mut self) {
        if self.cursor_row >= self.scroll_bot {
            self.scroll_up(1);
        } else {
            self.cursor_row += 1;
        }
        self.dirty = true;
    }

    fn scroll_up(&mut self, n: usize) {
        let top = self.scroll_top;
        let bot = self.scroll_bot;
        if bot < top { return; }
        let region_h = bot - top + 1;
        let shift = n.min(region_h);

        // Capture rows that scroll off the top into the scrollback buffer,
        // but only when the scroll region starts at row 0 and we're not in
        // the alternate screen (where scrollback is meaningless).
        if top == 0 && !self.in_alt_screen {
            for r in 0..shift {
                let row: Vec<Cell> = self.cells[r * self.cols..(r + 1) * self.cols].to_vec();
                self.scrollback.push_back(row);
            }
            while self.scrollback.len() > SCROLLBACK_LIMIT {
                self.scrollback.pop_front();
            }
        }

        for r in top..(bot + 1 - shift) {
            for c in 0..self.cols {
                let src = self.cell_idx(c, r + shift);
                let dst = self.cell_idx(c, r);
                self.cells[dst] = self.cells[src].clone();
            }
        }
        for r in (bot + 1 - shift)..=(bot) {
            for c in 0..self.cols {
                let idx = self.cell_idx(c, r);
                self.cells[idx] = Cell { ch: ' ', fg: self.cur_fg, bg: self.cur_bg };
            }
        }
        self.dirty = true;
    }

    fn erase_in_display(&mut self, mode: u16) {
        // On a full-screen erase (mode 2/3), capture the current background
        // colour as the theme background — this runs right after the shell
        // theme sets its bg colour, so we get the real value automatically.
        // Skip in alt screen: nvim's bg colour must not overwrite the shell's.
        if (mode == 2 || mode == 3) && self.theme_bg.is_none() && !self.in_alt_screen {
            self.theme_bg = Some(self.cur_bg);
        }

        let (start, end) = match mode {
            0 => {
                // from cursor to end
                let s = self.cell_idx(self.cursor_col, self.cursor_row);
                (s, self.cols * self.rows)
            }
            1 => {
                // from start to cursor
                let e = self.cell_idx(self.cursor_col, self.cursor_row) + 1;
                (0, e)
            }
            _ => (0, self.cols * self.rows), // erase all
        };
        let blank = Cell { ch: ' ', fg: self.cur_fg, bg: self.cur_bg };
        for i in start..end.min(self.cells.len()) {
            self.cells[i] = blank.clone();
        }
        self.dirty = true;
    }

    fn erase_in_line(&mut self, mode: u16) {
        let row = self.cursor_row;
        let (start_col, end_col) = match mode {
            0 => (self.cursor_col, self.cols),
            1 => (0, self.cursor_col + 1),
            _ => (0, self.cols),
        };
        let blank = Cell { ch: ' ', fg: self.cur_fg, bg: self.cur_bg };
        for c in start_col..end_col.min(self.cols) {
            let idx = self.cell_idx(c, row);
            self.cells[idx] = blank.clone();
        }
        self.dirty = true;
    }

    fn enter_alt_screen(&mut self) {
        if self.in_alt_screen { return; }
        self.cells         = vec![Cell::default(); self.cols * self.rows];
        self.cursor_col    = 0;
        self.cursor_row    = 0;
        self.scroll_top    = 0;
        self.scroll_bot    = self.rows.saturating_sub(1);
        self.scroll_offset = 0; // always show live view in alt screen
        self.in_alt_screen = true;
        self.dirty         = true;
    }

    fn exit_alt_screen(&mut self) {
        if !self.in_alt_screen { return; }
        self.cells         = vec![Cell::default(); self.cols * self.rows];
        self.cursor_col    = 0;
        self.cursor_row    = 0;
        self.scroll_top    = 0;
        self.scroll_bot    = self.rows.saturating_sub(1);
        self.scroll_offset = 0;
        self.cur_fg        = DEFAULT_FG;
        self.cur_bg        = DEFAULT_BG;
        self.in_alt_screen = false;
        self.dirty         = true;
    }

    fn set_color_from_params(&mut self, params: &[u16]) {
        // Parse SGR color params
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    self.cur_fg = DEFAULT_FG;
                    self.cur_bg = DEFAULT_BG;
                }
                30..=37 => { self.cur_fg = ansi_color(params[i] - 30, false); }
                90..=97 => { self.cur_fg = ansi_color(params[i] - 90, true); }
                38 if i + 2 < params.len() && params[i + 1] == 5 => {
                    self.cur_fg = color_256(params[i + 2] as u8);
                    i += 2;
                }
                38 if i + 4 < params.len() && params[i + 1] == 2 => {
                    self.cur_fg = [params[i+2] as u8, params[i+3] as u8, params[i+4] as u8, 0xFF];
                    i += 4;
                }
                39 => { self.cur_fg = DEFAULT_FG; }
                40..=47 => { self.cur_bg = ansi_color(params[i] - 40, false); }
                100..=107 => { self.cur_bg = ansi_color(params[i] - 100, true); }
                48 if i + 2 < params.len() && params[i + 1] == 5 => {
                    self.cur_bg = color_256(params[i + 2] as u8);
                    i += 2;
                }
                48 if i + 4 < params.len() && params[i + 1] == 2 => {
                    self.cur_bg = [params[i+2] as u8, params[i+3] as u8, params[i+4] as u8, 0xFF];
                    i += 4;
                }
                49 => { self.cur_bg = DEFAULT_BG; }
                _ => {}
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(cols: usize, rows: usize) -> TerminalState {
        TerminalState::new(cols, rows)
    }

    // ── new() ─────────────────────────────────────────────────────────────────

    #[test]
    fn new_initializes_correctly() {
        let st = make(80, 24);
        assert_eq!(st.cols, 80);
        assert_eq!(st.rows, 24);
        assert_eq!(st.cursor_col, 0);
        assert_eq!(st.cursor_row, 0);
        assert_eq!(st.scroll_top, 0);
        assert_eq!(st.scroll_bot, 23);
        assert_eq!(st.cells.len(), 80 * 24);
        assert_eq!(st.cells[0].ch, ' ');
        assert!(st.scrollback.is_empty());
        assert_eq!(st.scroll_offset, 0);
    }

    // ── put_char ──────────────────────────────────────────────────────────────

    #[test]
    fn put_char_places_char_at_cursor() {
        let mut st = make(80, 24);
        st.put_char('X');
        assert_eq!(st.cells[0].ch, 'X');
        assert_eq!(st.cursor_col, 1);
    }

    #[test]
    fn put_char_wraps_at_column_boundary() {
        let mut st = make(5, 5);
        st.cursor_col = 4;
        st.put_char('A');
        // After placing at col 4, cursor_col == 5 >= cols, so next char wraps
        st.put_char('B');
        assert_eq!(st.cursor_col, 1);
        assert_eq!(st.cursor_row, 1);
        assert_eq!(st.cells[5].ch, 'B'); // row 1, col 0
    }

    #[test]
    fn put_char_uses_current_colors() {
        let mut st = make(80, 24);
        st.cur_fg = [0xFF, 0x00, 0x00, 0xFF];
        st.cur_bg = [0x00, 0xFF, 0x00, 0xFF];
        st.put_char('Z');
        assert_eq!(st.cells[0].fg, [0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(st.cells[0].bg, [0x00, 0xFF, 0x00, 0xFF]);
    }

    // ── scroll_up / scrollback ────────────────────────────────────────────────

    #[test]
    fn scroll_up_captures_row_to_scrollback() {
        let mut st = make(4, 3);
        st.put_char('A'); st.put_char('B'); st.put_char('C'); st.put_char('D');
        st.scroll_up(1);
        assert_eq!(st.scrollback.len(), 1);
        assert_eq!(st.scrollback[0][0].ch, 'A');
        assert_eq!(st.scrollback[0][3].ch, 'D');
    }

    #[test]
    fn scroll_up_clears_vacated_row() {
        let mut st = make(4, 3);
        st.put_char('A');
        st.scroll_up(1);
        // Row 2 (the vacated bottom row) should be blank
        let row2_start = 2 * 4;
        assert_eq!(st.cells[row2_start].ch, ' ');
    }

    #[test]
    fn scrollback_capped_at_limit() {
        let mut st = make(1, 2);
        for _ in 0..2100 {
            st.put_char('X');
            st.scroll_up(1);
        }
        assert!(st.scrollback.len() <= 2000);
    }

    #[test]
    fn scroll_up_does_not_capture_partial_region() {
        let mut st = make(4, 4);
        st.scroll_top = 1;
        st.scroll_bot = 3;
        st.put_char('A');
        st.scroll_up(1);
        // Region doesn't start at row 0, so no scrollback capture
        assert!(st.scrollback.is_empty());
    }

    // ── adjust_scroll_offset ──────────────────────────────────────────────────

    #[test]
    fn adjust_scroll_offset_positive_scrolls_up() {
        let mut st = make(4, 3);
        st.put_char('X'); st.scroll_up(1); // 1 row in scrollback
        st.adjust_scroll_offset(1);
        assert_eq!(st.scroll_offset, 1);
    }

    #[test]
    fn adjust_scroll_offset_clamped_to_max() {
        let mut st = make(4, 3);
        st.put_char('X'); st.scroll_up(1);
        st.adjust_scroll_offset(999);
        assert_eq!(st.scroll_offset, st.scrollback.len());
    }

    #[test]
    fn adjust_scroll_offset_negative_clamped_at_zero() {
        let mut st = make(4, 3);
        st.adjust_scroll_offset(-10);
        assert_eq!(st.scroll_offset, 0);
    }

    // ── resize() ──────────────────────────────────────────────────────────────

    #[test]
    fn resize_preserves_existing_content() {
        let mut st = make(5, 5);
        st.put_char('H');
        st.resize(10, 10);
        assert_eq!(st.cells[0].ch, 'H');
        assert_eq!(st.cols, 10);
        assert_eq!(st.rows, 10);
    }

    #[test]
    fn resize_clamps_cursor_to_new_bounds() {
        let mut st = make(10, 10);
        st.cursor_col = 9;
        st.cursor_row = 9;
        st.resize(5, 5);
        assert!(st.cursor_col < 5);
        assert!(st.cursor_row < 5);
    }

    #[test]
    fn resize_noop_when_same_size() {
        let mut st = make(80, 24);
        st.put_char('Q');
        st.dirty = false;
        st.resize(80, 24);
        assert!(!st.dirty); // no change, dirty not set
    }

    // ── erase_in_display ─────────────────────────────────────────────────────

    #[test]
    fn erase_in_display_mode0_from_cursor_to_end() {
        let mut st = make(4, 2);
        for ch in ['A','B','C','D','E','F','G','H'] { st.put_char(ch); }
        st.cursor_col = 2; st.cursor_row = 0;
        st.erase_in_display(0);
        assert_eq!(st.cells[0].ch, 'A');
        assert_eq!(st.cells[1].ch, 'B');
        assert_eq!(st.cells[2].ch, ' '); // erased from here
        assert_eq!(st.cells[7].ch, ' ');
    }

    #[test]
    fn erase_in_display_mode1_from_start_to_cursor() {
        let mut st = make(4, 2);
        for ch in ['A','B','C','D','E','F','G','H'] { st.put_char(ch); }
        st.cursor_col = 1; st.cursor_row = 0;
        st.erase_in_display(1);
        assert_eq!(st.cells[0].ch, ' ');
        assert_eq!(st.cells[1].ch, ' ');
        assert_eq!(st.cells[2].ch, 'C'); // after cursor, untouched
    }

    #[test]
    fn erase_in_display_mode2_clears_all() {
        let mut st = make(4, 2);
        for ch in ['A','B','C','D','E','F','G','H'] { st.put_char(ch); }
        st.erase_in_display(2);
        assert!(st.cells.iter().all(|c| c.ch == ' '));
    }

    #[test]
    fn erase_in_display_mode2_captures_theme_bg() {
        let mut st = make(4, 2);
        st.cur_bg = [0x12, 0x34, 0x56, 0xFF];
        st.erase_in_display(2);
        assert_eq!(st.theme_bg, Some([0x12, 0x34, 0x56, 0xFF]));
    }

    // ── erase_in_line ─────────────────────────────────────────────────────────

    #[test]
    fn erase_in_line_mode0_from_cursor() {
        let mut st = make(5, 1);
        for ch in ['A','B','C','D','E'] { st.put_char(ch); }
        st.cursor_col = 2;
        st.erase_in_line(0);
        assert_eq!(st.cells[0].ch, 'A');
        assert_eq!(st.cells[1].ch, 'B');
        assert_eq!(st.cells[2].ch, ' ');
        assert_eq!(st.cells[4].ch, ' ');
    }

    #[test]
    fn erase_in_line_mode1_up_to_cursor() {
        let mut st = make(5, 1);
        for ch in ['A','B','C','D','E'] { st.put_char(ch); }
        st.cursor_col = 2;
        st.erase_in_line(1);
        assert_eq!(st.cells[0].ch, ' ');
        assert_eq!(st.cells[2].ch, ' ');
        assert_eq!(st.cells[3].ch, 'D');
    }

    #[test]
    fn erase_in_line_mode2_whole_line() {
        let mut st = make(5, 1);
        for ch in ['A','B','C','D','E'] { st.put_char(ch); }
        st.erase_in_line(2);
        assert!(st.cells.iter().all(|c| c.ch == ' '));
    }

    // ── set_color_from_params ─────────────────────────────────────────────────

    #[test]
    fn sgr_0_resets_colors() {
        let mut st = make(1, 1);
        st.cur_fg = [0, 0, 0, 0xFF];
        st.cur_bg = [0, 0, 0, 0xFF];
        st.set_color_from_params(&[0]);
        assert_eq!(st.cur_fg, DEFAULT_FG);
        assert_eq!(st.cur_bg, DEFAULT_BG);
    }

    #[test]
    fn sgr_30_37_set_fg_colors() {
        let mut st = make(1, 1);
        st.set_color_from_params(&[31]); // red
        assert_ne!(st.cur_fg, DEFAULT_FG);
        assert_eq!(st.cur_fg[3], 0xFF);
    }

    #[test]
    fn sgr_38_2_sets_rgb_fg() {
        let mut st = make(1, 1);
        st.set_color_from_params(&[38, 2, 100, 150, 200]);
        assert_eq!(st.cur_fg, [100, 150, 200, 0xFF]);
    }

    #[test]
    fn sgr_48_2_sets_rgb_bg() {
        let mut st = make(1, 1);
        st.set_color_from_params(&[48, 2, 10, 20, 30]);
        assert_eq!(st.cur_bg, [10, 20, 30, 0xFF]);
    }

    #[test]
    fn sgr_39_resets_fg() {
        let mut st = make(1, 1);
        st.cur_fg = [1, 2, 3, 0xFF];
        st.set_color_from_params(&[39]);
        assert_eq!(st.cur_fg, DEFAULT_FG);
    }

    #[test]
    fn sgr_49_resets_bg() {
        let mut st = make(1, 1);
        st.cur_bg = [1, 2, 3, 0xFF];
        st.set_color_from_params(&[49]);
        assert_eq!(st.cur_bg, DEFAULT_BG);
    }

    #[test]
    fn sgr_bright_colors_brighter_than_normal() {
        let mut st = make(1, 1);
        st.set_color_from_params(&[31]); // normal red
        let normal = st.cur_fg;
        st.set_color_from_params(&[91]); // bright red
        let bright = st.cur_fg;
        // At least one channel should be >= the normal value
        assert!(bright[0] >= normal[0] || bright[1] >= normal[1] || bright[2] >= normal[2]);
    }
}

fn color_256(n: u8) -> [u8; 4] {
    match n {
        0..=7   => ansi_color(n as u16, false),
        8..=15  => ansi_color((n - 8) as u16, true),
        16..=231 => {
            let i = n - 16;
            let cube = |v: u8| if v == 0 { 0u8 } else { 55 + v * 40 };
            [cube(i / 36), cube((i / 6) % 6), cube(i % 6), 0xFF]
        }
        232..=255 => {
            let gray = 8 + (n - 232) * 10;
            [gray, gray, gray, 0xFF]
        }
    }
}

fn ansi_color(idx: u16, bright: bool) -> [u8; 4] {
    let colors: [[u8; 3]; 8] = [
        [0x1e, 0x20, 0x30], // black
        [0xf7, 0x76, 0x8e], // red
        [0x9e, 0xce, 0x6a], // green
        [0xe0, 0xaf, 0x68], // yellow
        [0x7a, 0xa2, 0xf7], // blue
        [0xbb, 0x9a, 0xf7], // magenta
        [0x7d, 0xcf, 0xff], // cyan
        [0xc0, 0xca, 0xf5], // white
    ];
    let [r, g, b] = colors[idx as usize % 8];
    let boost: u8 = if bright { 40 } else { 0 };
    [r.saturating_add(boost), g.saturating_add(boost), b.saturating_add(boost), 0xFF]
}

// ── VTE Perform implementation ───────────────────────────────────────────────
// VteHandler holds a direct mutable reference to TerminalState.
// Callers must lock the Mutex once per read buffer and pass &mut *guard —
// this eliminates per-byte lock/unlock overhead (one lock per ~4 KB instead
// of one lock per character/sequence).

pub struct VteHandler<'a>(pub &'a mut TerminalState);

impl<'a> Perform for VteHandler<'a> {
    fn print(&mut self, ch: char) {
        if ch == '\u{007F}' { return; } // DEL is a control character, not printable
        self.0.put_char(ch);
    }

    fn execute(&mut self, byte: u8) {
        let st = &mut *self.0;
        match byte {
            b'\r' => { st.cursor_col = 0; }
            b'\n' | 0x0B | 0x0C => { st.newline(); }
            0x08 => { // backspace
                if st.cursor_col > 0 { st.cursor_col -= 1; }
            }
            0x07 => { st.bell = true; } // bell
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, _intermediates: &[u8], _ignore: bool, action: char) {
        let st = &mut *self.0;
        let p: Vec<u16> = params.iter()
            .map(|sub| sub.first().copied().unwrap_or(0))
            .collect();
        let p0 = p.first().copied().unwrap_or(0);
        let p1 = p.get(1).copied().unwrap_or(0);

        match action {
            'A' => { // cursor up
                let n = p0.max(1) as usize;
                st.cursor_row = st.cursor_row.saturating_sub(n);
            }
            'B' => { // cursor down
                let n = p0.max(1) as usize;
                st.cursor_row = (st.cursor_row + n).min(st.rows.saturating_sub(1));
            }
            'C' => { // cursor forward
                let n = p0.max(1) as usize;
                st.cursor_col = (st.cursor_col + n).min(st.cols.saturating_sub(1));
            }
            'D' => { // cursor back
                let n = p0.max(1) as usize;
                st.cursor_col = st.cursor_col.saturating_sub(n);
            }
            'H' | 'f' => { // cursor position (1-indexed)
                st.cursor_row = (p0.saturating_sub(1) as usize).min(st.rows.saturating_sub(1));
                st.cursor_col = (p1.saturating_sub(1) as usize).min(st.cols.saturating_sub(1));
            }
            'J' => { st.erase_in_display(p0); }
            'K' => { st.erase_in_line(p0); }
            'm' => { st.set_color_from_params(&p); }
            'r' => { // DECSTBM — scroll region
                let top = p0.saturating_sub(1) as usize;
                let bot = if p1 == 0 { st.rows.saturating_sub(1) } else { (p1 - 1) as usize };
                st.scroll_top = top;
                st.scroll_bot = bot.min(st.rows.saturating_sub(1));
                st.cursor_col = 0;
                st.cursor_row = 0;
            }
            'S' => { // scroll up
                let n = p0.max(1) as usize;
                st.scroll_up(n);
            }
            'L' => { // insert lines
                let n = p0.max(1) as usize;
                let row = st.cursor_row;
                let bot = st.scroll_bot;
                if row <= bot {
                    let shift = n.min(bot - row + 1);
                    for r in (row..=bot - shift).rev() {
                        for c in 0..st.cols {
                            let src = r * st.cols + c;
                            let dst = (r + shift) * st.cols + c;
                            st.cells[dst] = st.cells[src].clone();
                        }
                    }
                    let cols = st.cols;
                    for r in row..row + shift {
                        for c in 0..cols {
                            st.cells[r * cols + c] = Cell::default();
                        }
                    }
                    st.dirty = true;
                }
            }
            'M' => { // delete lines
                let n = p0.max(1) as usize;
                let row = st.cursor_row;
                let bot = st.scroll_bot;
                if row <= bot {
                    let shift = n.min(bot - row + 1);
                    for r in row..=bot - shift {
                        for c in 0..st.cols {
                            let src = (r + shift) * st.cols + c;
                            let dst = r * st.cols + c;
                            st.cells[dst] = st.cells[src].clone();
                        }
                    }
                    let cols = st.cols;
                    for r in bot - shift + 1..=bot {
                        for c in 0..cols {
                            st.cells[r * cols + c] = Cell::default();
                        }
                    }
                    st.dirty = true;
                }
            }
            'P' => { // delete chars
                let n = p0.max(1) as usize;
                let row = st.cursor_row;
                let col = st.cursor_col;
                let cols = st.cols;
                for c in col..cols {
                    let src = row * cols + (c + n).min(cols - 1);
                    let dst = row * cols + c;
                    if c + n < cols {
                        st.cells[dst] = st.cells[src].clone();
                    } else {
                        st.cells[dst] = Cell::default();
                    }
                }
                st.dirty = true;
            }
            'G' => { // cursor column absolute (CHA) — used by interactive prompts
                let col = p0.saturating_sub(1) as usize;
                st.cursor_col = col.min(st.cols.saturating_sub(1));
            }
            'E' => { // cursor next line
                let n = p0.max(1) as usize;
                st.cursor_row = (st.cursor_row + n).min(st.rows.saturating_sub(1));
                st.cursor_col = 0;
            }
            'F' => { // cursor preceding line
                let n = p0.max(1) as usize;
                st.cursor_row = st.cursor_row.saturating_sub(n);
                st.cursor_col = 0;
            }
            'd' => { // line position absolute (VPA)
                let row = p0.saturating_sub(1) as usize;
                st.cursor_row = row.min(st.rows.saturating_sub(1));
            }
            'X' => { // erase characters (ECH)
                let n = p0.max(1) as usize;
                let row = st.cursor_row;
                let col = st.cursor_col;
                let cols = st.cols;
                let end = (col + n).min(cols);
                for c in col..end {
                    st.cells[row * cols + c] = Cell { ch: ' ', fg: st.cur_fg, bg: st.cur_bg };
                }
                st.dirty = true;
            }
            'h' if _intermediates == [b'?'] => { // DEC private mode set
                for &param in &p {
                    match param {
                        25         => { st.cursor_visible = true; st.dirty = true; }
                        47 | 1047 | 1049 => { st.enter_alt_screen(); }
                        _          => {}
                    }
                }
            }
            'l' if _intermediates == [b'?'] => { // DEC private mode reset
                for &param in &p {
                    match param {
                        25         => { st.cursor_visible = false; st.dirty = true; }
                        47 | 1047 | 1049 => { st.exit_alt_screen(); }
                        _          => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        let st = &mut *self.0;
        match byte {
            b'M' => { // reverse index
                if st.cursor_row == st.scroll_top {
                    // scroll down — insert blank at top
                    let top = st.scroll_top;
                    let bot = st.scroll_bot;
                    for r in (top..bot).rev() {
                        for c in 0..st.cols {
                            let src = r * st.cols + c;
                            let dst = (r + 1) * st.cols + c;
                            st.cells[dst] = st.cells[src].clone();
                        }
                    }
                    let cols = st.cols;
                    for c in 0..cols {
                        st.cells[top * cols + c] = Cell::default();
                    }
                    st.dirty = true;
                } else if st.cursor_row > 0 {
                    st.cursor_row -= 1;
                    st.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 7: shell reports cwd as "file://hostname/path"
        // Emitted by zsh (with vcs_info or prompt setup), fish, and bash with
        // the __vte_osc7 / update_terminal_cwd hook.
        if params.len() >= 2 && params[0] == b"7" {
            if let Ok(uri) = std::str::from_utf8(params[1]) {
                let path = if let Some(rest) = uri.strip_prefix("file://") {
                    // rest = "hostname/absolute/path" — skip to the first '/'
                    rest.find('/').map(|i| &rest[i..]).unwrap_or(rest)
                } else {
                    uri
                };
                if !path.is_empty() {
                    self.0.cwd = Some(path.to_string());
                }
            }
        }
    }
    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}
