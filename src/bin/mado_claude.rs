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
use vte::{Parser, Perform};

static FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/HackNerdFontMono-Regular.ttf");

// ── Helpers ───────────────────────────────────────────────────────────────────

fn shlex_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ── ANSI colour table ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rgb(u8, u8, u8);

const ANSI_COLORS: [Rgb; 16] = [
    Rgb(0x1e, 0x20, 0x30), // 0  black
    Rgb(0xcc, 0x55, 0x55), // 1  red
    Rgb(0x55, 0xbb, 0x55), // 2  green
    Rgb(0xcc, 0xbb, 0x55), // 3  yellow
    Rgb(0x55, 0x88, 0xcc), // 4  blue
    Rgb(0xcc, 0x55, 0xcc), // 5  magenta
    Rgb(0x55, 0xcc, 0xcc), // 6  cyan
    Rgb(0xcc, 0xcc, 0xcc), // 7  white
    Rgb(0x44, 0x44, 0x44), // 8  bright black
    Rgb(0xff, 0x77, 0x77), // 9  bright red
    Rgb(0x77, 0xff, 0x77), // 10 bright green
    Rgb(0xff, 0xff, 0x77), // 11 bright yellow
    Rgb(0x77, 0xaa, 0xff), // 12 bright blue
    Rgb(0xff, 0x77, 0xff), // 13 bright magenta
    Rgb(0x77, 0xff, 0xff), // 14 bright cyan
    Rgb(0xff, 0xff, 0xff), // 15 bright white
];

fn color256(n: u8) -> Rgb {
    match n {
        0..=15 => ANSI_COLORS[n as usize],
        16..=231 => {
            let n = n - 16;
            let b = n % 6;
            let g = (n / 6) % 6;
            let r = n / 36;
            let c = |v: u8| if v == 0 { 0u8 } else { 55 + v * 40 };
            Rgb(c(r), c(g), c(b))
        }
        232..=255 => {
            let v = 8u8.saturating_add((n - 232).saturating_mul(10));
            Rgb(v, v, v)
        }
    }
}

const DEFAULT_FG: Rgb = Rgb(0xcc, 0xcc, 0xcc);
const DEFAULT_BG: Rgb = Rgb(0x1e, 0x20, 0x30);
const SCROLLBACK_LIMIT: usize = 3000;

// ── Terminal cell / state ─────────────────────────────────────────────────────

#[derive(Clone)]
struct Cell {
    ch: char,
    fg: Rgb,
    bg: Rgb,
    bold: bool,
    italic: bool,
    underline: bool,
}

impl Cell {
    fn default_with_bg(bg: Rgb) -> Self {
        Cell { ch: ' ', fg: DEFAULT_FG, bg, bold: false, italic: false, underline: false }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Cell::default_with_bg(DEFAULT_BG)
    }
}

struct TermState {
    cols: usize,
    rows: usize,
    screen: Vec<Cell>,
    alt_screen: Vec<Cell>,
    in_alt: bool,
    cur_row: usize,
    cur_col: usize,
    saved_row: usize,
    saved_col: usize,
    saved_row_alt: usize,
    saved_col_alt: usize,
    scroll_top: usize,
    scroll_bot: usize,
    cur_fg: Rgb,
    cur_bg: Rgb,
    cur_bold: bool,
    cur_italic: bool,
    cur_underline: bool,
    cursor_visible: bool,
    /// Blink phase: toggled every ~500 ms so the cursor pulses.
    cursor_blink_on: bool,
    scroll_offset: usize,
    scrollback: std::collections::VecDeque<Vec<Cell>>,
    dirty: bool,
    /// Mouse selection: display-space (row, col) for start and current end.
    sel_start: Option<(usize, usize)>,
    sel_end:   Option<(usize, usize)>,
    sel_drag:  bool,
}

impl TermState {
    fn new(cols: usize, rows: usize) -> Self {
        let size = cols * rows;
        TermState {
            cols,
            rows,
            screen: vec![Cell::default(); size],
            alt_screen: vec![Cell::default(); size],
            in_alt: false,
            cur_row: 0,
            cur_col: 0,
            saved_row: 0,
            saved_col: 0,
            saved_row_alt: 0,
            saved_col_alt: 0,
            scroll_top: 0,
            scroll_bot: rows.saturating_sub(1),
            cur_fg: DEFAULT_FG,
            cur_bg: DEFAULT_BG,
            cur_bold: false,
            cur_italic: false,
            cur_underline: false,
            cursor_visible: true,
            cursor_blink_on: true,
            scroll_offset: 0,
            scrollback: std::collections::VecDeque::new(),
            dirty: true,
            sel_start: None,
            sel_end:   None,
            sel_drag:  false,
        }
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        if cols == self.cols && rows == self.rows { return; }
        let new_size = cols * rows;

        // Reflow main screen
        let old_screen = std::mem::replace(&mut self.screen, vec![Cell::default(); new_size]);
        for r in 0..rows.min(self.rows) {
            for c in 0..cols.min(self.cols) {
                self.screen[r * cols + c] = old_screen[r * self.cols + c].clone();
            }
        }

        // Reflow alt screen
        let old_alt = std::mem::replace(&mut self.alt_screen, vec![Cell::default(); new_size]);
        for r in 0..rows.min(self.rows) {
            for c in 0..cols.min(self.cols) {
                self.alt_screen[r * cols + c] = old_alt[r * self.cols + c].clone();
            }
        }

        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bot = rows.saturating_sub(1);
        self.cur_row = self.cur_row.min(rows.saturating_sub(1));
        self.cur_col = self.cur_col.min(cols.saturating_sub(1));
        self.dirty = true;
    }

    fn active_screen_mut(&mut self) -> &mut Vec<Cell> {
        if self.in_alt { &mut self.alt_screen } else { &mut self.screen }
    }

    fn active_screen(&self) -> &Vec<Cell> {
        if self.in_alt { &self.alt_screen } else { &self.screen }
    }

    fn cell_mut(&mut self, row: usize, col: usize) -> Option<&mut Cell> {
        if row >= self.rows || col >= self.cols { return None; }
        let idx = row * self.cols + col;
        Some(&mut self.active_screen_mut()[idx])
    }

    fn scroll_up_region(&mut self, n: usize) {
        let top = self.scroll_top;
        let bot = self.scroll_bot.min(self.rows.saturating_sub(1));
        if top > bot { return; }

        let n = n.min(bot - top + 1);

        // Push lost rows into scrollback (only for main screen)
        if !self.in_alt {
            for r in top..top + n {
                if r * self.cols + self.cols <= self.screen.len() {
                    let row_cells: Vec<Cell> = self.screen[r * self.cols..(r + 1) * self.cols].to_vec();
                    self.scrollback.push_back(row_cells);
                    while self.scrollback.len() > SCROLLBACK_LIMIT {
                        self.scrollback.pop_front();
                    }
                }
            }
        }

        let cols = self.cols;
        let bg = self.cur_bg;
        let screen = self.active_screen_mut();
        // Shift rows up
        for r in top..=bot.saturating_sub(n) {
            let src = (r + n) * cols;
            let dst = r * cols;
            if src + cols <= screen.len() && dst + cols <= screen.len() {
                for c in 0..cols {
                    screen[dst + c] = screen[src + c].clone();
                }
            }
        }
        // Clear bottom n rows
        for r in (bot + 1).saturating_sub(n)..=bot {
            for c in 0..cols {
                let idx = r * cols + c;
                if idx < screen.len() {
                    screen[idx] = Cell::default_with_bg(bg);
                }
            }
        }
        self.dirty = true;
    }

    fn scroll_down_region(&mut self, n: usize) {
        let top = self.scroll_top;
        let bot = self.scroll_bot.min(self.rows.saturating_sub(1));
        if top > bot { return; }

        let n = n.min(bot - top + 1);

        let cols = self.cols;
        let bg = self.cur_bg;
        let screen = self.active_screen_mut();
        // Shift rows down (from bottom upward to avoid overwrite)
        let mut r = bot;
        loop {
            if r >= top + n {
                let src = (r - n) * cols;
                let dst = r * cols;
                if src + cols <= screen.len() && dst + cols <= screen.len() {
                    for c in 0..cols {
                        screen[dst + c] = screen[src + c].clone();
                    }
                }
            }
            if r == top { break; }
            r -= 1;
        }
        // Clear top n rows
        for r in top..top + n {
            for c in 0..cols {
                let idx = r * cols + c;
                if idx < screen.len() {
                    screen[idx] = Cell::default_with_bg(bg);
                }
            }
        }
        self.dirty = true;
    }

    fn erase_region(&mut self, r0: usize, c0: usize, r1: usize, c1: usize) {
        let bg = self.cur_bg;
        let cols = self.cols;
        let screen = self.active_screen_mut();
        for r in r0..=r1.min(screen.len() / cols.max(1)) {
            let cs = if r == r0 { c0 } else { 0 };
            let ce = if r == r1 { c1 + 1 } else { cols };
            for c in cs..ce.min(cols) {
                let idx = r * cols + c;
                if idx < screen.len() {
                    screen[idx] = Cell::default_with_bg(bg);
                }
            }
        }
        self.dirty = true;
    }
}

// ── VTE Performer ─────────────────────────────────────────────────────────────

struct Performer<'a> {
    state: &'a mut TermState,
}

impl<'a> Perform for Performer<'a> {
    fn print(&mut self, c: char) {
        if c == '\u{007F}' { return; } // DEL is a control character, not printable
        let s = &mut *self.state;
        if s.cur_col >= s.cols {
            s.cur_col = 0;
            s.cur_row += 1;
        }
        if s.cur_row >= s.rows {
            s.scroll_up_region(1);
            s.cur_row = s.rows.saturating_sub(1);
        }
        let fg = s.cur_fg;
        let bg = s.cur_bg;
        let bold = s.cur_bold;
        let italic = s.cur_italic;
        let underline = s.cur_underline;
        let row = s.cur_row;
        let col = s.cur_col;
        if let Some(cell) = s.cell_mut(row, col) {
            cell.ch = c;
            cell.fg = fg;
            cell.bg = bg;
            cell.bold = bold;
            cell.italic = italic;
            cell.underline = underline;
        }
        s.cur_col += 1;
        s.dirty = true;
    }

    fn execute(&mut self, byte: u8) {
        let s = &mut *self.state;
        match byte {
            0x0D => { s.cur_col = 0; }
            0x0A | 0x0B | 0x0C => {
                s.cur_row += 1;
                if s.cur_row > s.scroll_bot {
                    let excess = s.cur_row - s.scroll_bot;
                    s.scroll_up_region(excess);
                    s.cur_row = s.scroll_bot;
                }
                s.dirty = true;
            }
            0x08 => { if s.cur_col > 0 { s.cur_col -= 1; } }
            0x09 => {
                let next = ((s.cur_col / 8) + 1) * 8;
                s.cur_col = next.min(s.cols.saturating_sub(1));
            }
            0x07 => {} // BEL: ignore
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
        let s = &mut *self.state;
        let rows = s.rows;
        let cols = s.cols;

        // Flat param list
        let p: Vec<u16> = params.iter().flat_map(|sub| sub.iter().copied()).collect();
        let p0 = p.get(0).copied().unwrap_or(0) as usize;
        let p1 = p.get(1).copied().unwrap_or(0) as usize;

        let is_priv = intermediates.contains(&b'?');

        match action {
            // ── Cursor movement ────────────────────────────────────────────────
            'A' => {
                let n = p0.max(1);
                s.cur_row = s.cur_row.saturating_sub(n).max(s.scroll_top);
            }
            'B' => {
                let n = p0.max(1);
                s.cur_row = (s.cur_row + n).min(s.scroll_bot);
            }
            'C' => {
                let n = p0.max(1);
                s.cur_col = (s.cur_col + n).min(cols.saturating_sub(1));
            }
            'D' => {
                let n = p0.max(1);
                s.cur_col = s.cur_col.saturating_sub(n);
            }
            'E' => {
                let n = p0.max(1);
                s.cur_row = (s.cur_row + n).min(rows.saturating_sub(1));
                s.cur_col = 0;
            }
            'F' => {
                let n = p0.max(1);
                s.cur_row = s.cur_row.saturating_sub(n);
                s.cur_col = 0;
            }
            'G' => {
                s.cur_col = p0.saturating_sub(1).min(cols.saturating_sub(1));
            }
            'H' | 'f' => {
                s.cur_row = p0.saturating_sub(1).min(rows.saturating_sub(1));
                s.cur_col = p1.saturating_sub(1).min(cols.saturating_sub(1));
            }
            'd' => {
                s.cur_row = p0.saturating_sub(1).min(rows.saturating_sub(1));
            }

            // ── Erase ──────────────────────────────────────────────────────────
            'J' => {
                match p0 {
                    0 => {
                        // From cursor to end
                        s.erase_region(s.cur_row, s.cur_col, rows.saturating_sub(1), cols.saturating_sub(1));
                    }
                    1 => {
                        // From start to cursor
                        s.erase_region(0, 0, s.cur_row, s.cur_col);
                    }
                    2 | 3 => {
                        // Entire screen
                        s.erase_region(0, 0, rows.saturating_sub(1), cols.saturating_sub(1));
                    }
                    _ => {}
                }
            }
            'K' => {
                let r = s.cur_row;
                let c = s.cur_col;
                match p0 {
                    0 => s.erase_region(r, c, r, cols.saturating_sub(1)),
                    1 => s.erase_region(r, 0, r, c),
                    2 => s.erase_region(r, 0, r, cols.saturating_sub(1)),
                    _ => {}
                }
            }

            // ── Scroll ─────────────────────────────────────────────────────────
            'S' => { s.scroll_up_region(p0.max(1)); }
            'T' => { s.scroll_down_region(p0.max(1)); }
            'L' => {
                // Insert blank lines at cursor row
                let n = p0.max(1);
                let r = s.cur_row;
                let top = s.scroll_top;
                let bot = s.scroll_bot.min(rows.saturating_sub(1));
                let bg = s.cur_bg;
                if r >= top && r <= bot {
                    // Shift lines down within region
                    {
                        let screen = s.active_screen_mut();
                        let mut row = bot;
                        loop {
                            if row >= r + n {
                                let src = (row - n) * cols;
                                let dst = row * cols;
                                if src + cols <= screen.len() && dst + cols <= screen.len() {
                                    for c in 0..cols { screen[dst + c] = screen[src + c].clone(); }
                                }
                            }
                            if row == r { break; }
                            row -= 1;
                        }
                    }
                    {
                        let screen = s.active_screen_mut();
                        for row in r..r + n {
                            if row > bot { break; }
                            for c in 0..cols {
                                let idx = row * cols + c;
                                if idx < screen.len() { screen[idx] = Cell::default_with_bg(bg); }
                            }
                        }
                    }
                    s.dirty = true;
                }
            }
            'M' => {
                // Delete lines at cursor row
                let n = p0.max(1);
                let r = s.cur_row;
                let top = s.scroll_top;
                let bot = s.scroll_bot.min(rows.saturating_sub(1));
                let bg = s.cur_bg;
                if r >= top && r <= bot {
                    let screen = s.active_screen_mut();
                    for row in r..=bot {
                        let src = (row + n) * cols;
                        let dst = row * cols;
                        if src + cols <= screen.len() {
                            for c in 0..cols { screen[dst + c] = screen[src + c].clone(); }
                        } else if dst + cols <= screen.len() {
                            for c in 0..cols { screen[dst + c] = Cell::default_with_bg(bg); }
                        }
                    }
                    s.dirty = true;
                }
            }

            // ── Insert/delete chars ────────────────────────────────────────────
            '@' => {
                // ICH — insert blank chars
                let n = p0.max(1);
                let r = s.cur_row;
                let c = s.cur_col;
                let bg = s.cur_bg;
                let screen = s.active_screen_mut();
                let row_start = r * cols;
                if row_start + cols <= screen.len() {
                    let end = (c + n).min(cols);
                    // Shift chars right
                    for i in (end..cols).rev() {
                        screen[row_start + i] = screen[row_start + i - n.min(i - c)].clone();
                    }
                    for i in c..end {
                        screen[row_start + i] = Cell::default_with_bg(bg);
                    }
                }
                s.dirty = true;
            }
            'P' => {
                // DCH — delete chars
                let n = p0.max(1);
                let r = s.cur_row;
                let c = s.cur_col;
                let bg = s.cur_bg;
                let screen = s.active_screen_mut();
                let row_start = r * cols;
                if row_start + cols <= screen.len() {
                    for i in c..cols {
                        let src = i + n;
                        if src < cols {
                            screen[row_start + i] = screen[row_start + src].clone();
                        } else {
                            screen[row_start + i] = Cell::default_with_bg(bg);
                        }
                    }
                }
                s.dirty = true;
            }
            'X' => {
                // ECH — erase chars
                let n = p0.max(1);
                let r = s.cur_row;
                let c = s.cur_col;
                let end = (c + n).min(cols.saturating_sub(1));
                s.erase_region(r, c, r, end);
            }

            // ── Scroll region ──────────────────────────────────────────────────
            'r' => {
                let top = p0.saturating_sub(1);
                let bot = if p1 == 0 { rows.saturating_sub(1) } else {
                    p1.saturating_sub(1).min(rows.saturating_sub(1)).max(top)
                };
                s.scroll_top = top;
                s.scroll_bot = bot;
                // Move cursor to home after DECSTBM
                s.cur_row = 0;
                s.cur_col = 0;
            }

            // ── Cursor save/restore (CSI variants) ─────────────────────────────
            's' => {
                s.saved_row = s.cur_row;
                s.saved_col = s.cur_col;
            }
            'u' => {
                s.cur_row = s.saved_row.min(rows.saturating_sub(1));
                s.cur_col = s.saved_col.min(cols.saturating_sub(1));
            }

            // ── Mode set/reset ─────────────────────────────────────────────────
            'h' | 'l' => {
                let on = action == 'h';
                if is_priv {
                    match p0 {
                        25 => { s.cursor_visible = on; s.dirty = true; }
                        1049 => {
                            if on {
                                s.saved_row_alt = s.cur_row;
                                s.saved_col_alt = s.cur_col;
                                s.in_alt = true;
                                // Clear alt screen
                                let bg = s.cur_bg;
                                let sz = s.cols * s.rows;
                                s.alt_screen = vec![Cell::default_with_bg(bg); sz];
                            } else {
                                s.in_alt = false;
                                s.cur_row = s.saved_row_alt.min(rows.saturating_sub(1));
                                s.cur_col = s.saved_col_alt.min(cols.saturating_sub(1));
                            }
                            s.dirty = true;
                        }
                        47 | 1047 => {
                            if on {
                                s.in_alt = true;
                                let bg = s.cur_bg;
                                let sz = s.cols * s.rows;
                                s.alt_screen = vec![Cell::default_with_bg(bg); sz];
                            } else {
                                s.in_alt = false;
                            }
                            s.dirty = true;
                        }
                        1 | 2004 => {} // app cursor keys / bracketed paste: ignore
                        _ => {}
                    }
                }
                // non-private modes: ignore
            }

            // ── SGR ────────────────────────────────────────────────────────────
            'm' => {
                if p.is_empty() {
                    // Reset all
                    s.cur_fg = DEFAULT_FG;
                    s.cur_bg = DEFAULT_BG;
                    s.cur_bold = false;
                    s.cur_italic = false;
                    s.cur_underline = false;
                    return;
                }
                let mut i = 0;
                while i < p.len() {
                    let v = p[i] as usize;
                    match v {
                        0 => {
                            s.cur_fg = DEFAULT_FG;
                            s.cur_bg = DEFAULT_BG;
                            s.cur_bold = false;
                            s.cur_italic = false;
                            s.cur_underline = false;
                        }
                        1  => s.cur_bold = true,
                        3  => s.cur_italic = true,
                        4  => s.cur_underline = true,
                        22 => s.cur_bold = false,
                        23 => s.cur_italic = false,
                        24 => s.cur_underline = false,
                        30..=37 => s.cur_fg = ANSI_COLORS[v - 30],
                        38 => {
                            if p.get(i + 1).copied() == Some(2) && i + 4 < p.len() {
                                s.cur_fg = Rgb(p[i+2] as u8, p[i+3] as u8, p[i+4] as u8);
                                i += 4;
                            } else if p.get(i + 1).copied() == Some(5) && i + 2 < p.len() {
                                s.cur_fg = color256(p[i+2] as u8);
                                i += 2;
                            }
                        }
                        39 => s.cur_fg = DEFAULT_FG,
                        40..=47 => s.cur_bg = ANSI_COLORS[v - 40],
                        48 => {
                            if p.get(i + 1).copied() == Some(2) && i + 4 < p.len() {
                                s.cur_bg = Rgb(p[i+2] as u8, p[i+3] as u8, p[i+4] as u8);
                                i += 4;
                            } else if p.get(i + 1).copied() == Some(5) && i + 2 < p.len() {
                                s.cur_bg = color256(p[i+2] as u8);
                                i += 2;
                            }
                        }
                        49 => s.cur_bg = DEFAULT_BG,
                        90..=97  => s.cur_fg = ANSI_COLORS[v - 90 + 8],
                        100..=107 => s.cur_bg = ANSI_COLORS[v - 100 + 8],
                        _ => {}
                    }
                    i += 1;
                }
            }

            _ => {}
        }
        s.dirty = true;
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        let s = &mut *self.state;
        match byte {
            b'7' => {
                s.saved_row = s.cur_row;
                s.saved_col = s.cur_col;
            }
            b'8' => {
                s.cur_row = s.saved_row.min(s.rows.saturating_sub(1));
                s.cur_col = s.saved_col.min(s.cols.saturating_sub(1));
            }
            b'M' => {
                // Reverse index
                if s.cur_row == s.scroll_top {
                    s.scroll_down_region(1);
                } else if s.cur_row > 0 {
                    s.cur_row -= 1;
                }
            }
            _ => {}
        }
        s.dirty = true;
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}
    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}

// ── Messages ──────────────────────────────────────────────────────────────────

enum Msg {
    PtyData(Vec<u8>),
    Line(String),
    PtyEof,
}

// ── Font metrics ──────────────────────────────────────────────────────────────

const LINE_GAP: usize = 0; // terminal emulators typically don't add line gap

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

/// Given two display-space (row, col) points, return the selected text.
fn selection_text(state: &TermState) -> String {
    let (Some(s), Some(e)) = (state.sel_start, state.sel_end) else { return String::new() };
    let (start, end) = if s <= e { (s, e) } else { (e, s) };
    if start == end { return String::new(); }

    let cols    = state.cols;
    let sb_len  = state.scrollback.len();

    let mut out = String::new();
    for disp_row in start.0..=end.0 {
        let virtual_row = sb_len as isize - state.scroll_offset as isize + disp_row as isize;
        let cells: &[Cell] = if virtual_row < 0 {
            &[]
        } else if (virtual_row as usize) < sb_len {
            &state.scrollback[virtual_row as usize]
        } else {
            let sr = virtual_row as usize - sb_len;
            if sr < state.rows {
                let base = sr * cols;
                let end_  = (base + cols).min(state.screen.len());
                &state.screen[base..end_]
            } else { &[] }
        };

        let col_start = if disp_row == start.0 { start.1 } else { 0 };
        let col_end   = if disp_row == end.0   { end.1   } else { cols };
        let col_end   = col_end.min(cols);

        let mut line = String::new();
        for c in col_start..col_end {
            if let Some(cell) = cells.get(c) {
                line.push(cell.ch);
            }
        }
        // Strip trailing spaces from each line
        let trimmed = line.trim_end_matches(' ');
        if !out.is_empty() { out.push('\n'); }
        out.push_str(trimmed);
    }
    out
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn draw_glyph(buf: &mut [u8], stride: usize, fm: &Fm, ch: char,
              px: usize, baseline_y: usize, fg: Rgb, bg: Rgb) {
    // Check glyph exists
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
            buf[i]   = (bg.0 as f32 * (1.0 - cov) + fg.0 as f32 * cov) as u8;
            buf[i+1] = (bg.1 as f32 * (1.0 - cov) + fg.1 as f32 * cov) as u8;
            buf[i+2] = (bg.2 as f32 * (1.0 - cov) + fg.2 as f32 * cov) as u8;
            buf[i+3] = 255;
        }
    }
}

fn render(state: &TermState, fm: &Fm) -> Vec<u8> {
    let cols = state.cols;
    let rows = state.rows;
    if cols == 0 || rows == 0 { return vec![]; }

    let phys_w = cols * fm.cell_w;
    let phys_h = rows * fm.cell_h;
    let mut buf = vec![0u8; phys_w * phys_h * 4];

    let sb_len = state.scrollback.len();

    for row in 0..rows {
        let virtual_row = sb_len as isize - state.scroll_offset as isize + row as isize;
        for col in 0..cols {
            let cell = if virtual_row < 0 {
                Cell::default()
            } else if (virtual_row as usize) < sb_len {
                let sb_row = &state.scrollback[virtual_row as usize];
                sb_row.get(col).cloned().unwrap_or_default()
            } else {
                let screen_row = virtual_row as usize - sb_len;
                if screen_row < rows {
                    state.active_screen().get(screen_row * cols + col).cloned().unwrap_or_default()
                } else {
                    Cell::default()
                }
            };

            let px = col * fm.cell_w;
            let py = row * fm.cell_h;

            // Fill background
            let bg = cell.bg;
            for dy in 0..fm.cell_h {
                for dx in 0..fm.cell_w {
                    let i = ((py + dy) * phys_w + (px + dx)) * 4;
                    if i + 3 < buf.len() {
                        buf[i]   = bg.0;
                        buf[i+1] = bg.1;
                        buf[i+2] = bg.2;
                        buf[i+3] = 255;
                    }
                }
            }

            if cell.ch != ' ' {
                let baseline_y = py + fm.baseline;
                draw_glyph(&mut buf, phys_w, fm, cell.ch, px, baseline_y, cell.fg, cell.bg);
                // Fake bold: overdraw
                if cell.bold {
                    draw_glyph(&mut buf, phys_w, fm, cell.ch, px + 1, baseline_y, cell.fg, cell.bg);
                }
            }
        }
    }

    // ── Selection highlight ───────────────────────────────────────────────────
    if let (Some(sel_s), Some(sel_e)) = (state.sel_start, state.sel_end) {
        let (s, e) = if sel_s <= sel_e { (sel_s, sel_e) } else { (sel_e, sel_s) };
        let sel_bg = Rgb(0x3a, 0x5a, 0x8a);
        for disp_row in s.0..=e.0 {
            if disp_row >= rows { break; }
            let col_start = if disp_row == s.0 { s.1 } else { 0 };
            let col_end   = if disp_row == e.0 { e.1  } else { cols };
            let col_end   = col_end.min(cols);
            for col in col_start..col_end {
                let px = col * fm.cell_w;
                let py = disp_row * fm.cell_h;
                // Tint background
                for dy in 0..fm.cell_h {
                    for dx in 0..fm.cell_w {
                        let i = ((py + dy) * phys_w + (px + dx)) * 4;
                        if i + 3 < buf.len() {
                            buf[i]   = sel_bg.0;
                            buf[i+1] = sel_bg.1;
                            buf[i+2] = sel_bg.2;
                            buf[i+3] = 255;
                        }
                    }
                }
                // Re-draw the glyph over the highlight
                let virtual_row = state.scrollback.len() as isize
                    - state.scroll_offset as isize + disp_row as isize;
                let cell_ch = if virtual_row >= 0 && (virtual_row as usize) < state.scrollback.len() {
                    state.scrollback[virtual_row as usize].get(col).map(|c| c.ch).unwrap_or(' ')
                } else {
                    let sr = (virtual_row as usize).saturating_sub(state.scrollback.len());
                    state.active_screen().get(sr * cols + col).map(|c| c.ch).unwrap_or(' ')
                };
                if cell_ch != ' ' {
                    draw_glyph(&mut buf, phys_w, fm, cell_ch, px, py + fm.baseline,
                               Rgb(0xff, 0xff, 0xff), sel_bg);
                }
            }
        }
    }

    // ── Cursor ────────────────────────────────────────────────────────────────
    // Show cursor if: cursor_visible AND blink phase is on. Even when claude
    // sends ?25l, we still show it during the "on" phase to avoid confusion.
    if state.cursor_blink_on && state.scroll_offset == 0 {
        let cr = state.cur_row;
        let cc = state.cur_col;
        if cr < rows && cc < cols {
            let px = cc * fm.cell_w;
            let py = cr * fm.cell_h;
            let cursor_bg = Rgb(0xd8, 0xe0, 0xf0);
            let cursor_fg = DEFAULT_BG;
            for dy in 0..fm.cell_h {
                for dx in 0..fm.cell_w {
                    let i = ((py + dy) * phys_w + (px + dx)) * 4;
                    if i + 3 < buf.len() {
                        buf[i]   = cursor_bg.0;
                        buf[i+1] = cursor_bg.1;
                        buf[i+2] = cursor_bg.2;
                        buf[i+3] = 255;
                    }
                }
            }
            let idx = cr * cols + cc;
            if let Some(cell) = state.active_screen().get(idx) {
                if cell.ch != ' ' {
                    draw_glyph(&mut buf, phys_w, fm, cell.ch, px, py + fm.baseline,
                               cursor_fg, cursor_bg);
                }
            }
        }
    }

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

/// Translate a Slint key text string to the byte sequence a terminal expects.
fn key_text_to_bytes(text: &str) -> Vec<u8> {
    match text {
        // Modifier keys — never forward
        "\u{0010}" | "\u{0015}" | "\u{0011}" | "\u{0016}" |
        "\u{0012}" | "\u{0013}" | "\u{0014}" | "\u{0017}" |
        "\u{0018}" | "\u{0019}" => vec![],

        // Standard keys
        "\u{0008}" => vec![0x7F],                              // Backspace → DEL
        "\u{007F}" => vec![0x1B, b'[', b'3', b'~'],           // Delete → ESC[3~

        // Arrow keys
        "\u{F700}" => vec![0x1B, b'[', b'A'],                 // Up
        "\u{F701}" => vec![0x1B, b'[', b'B'],                 // Down
        "\u{F702}" => vec![0x1B, b'[', b'D'],                 // Left
        "\u{F703}" => vec![0x1B, b'[', b'C'],                 // Right

        // Navigation
        "\u{F729}" => vec![0x1B, b'[', b'H'],                 // Home
        "\u{F72B}" => vec![0x1B, b'[', b'F'],                 // End
        "\u{F72C}" => vec![0x1B, b'[', b'5', b'~'],           // PageUp
        "\u{F72D}" => vec![0x1B, b'[', b'6', b'~'],           // PageDown

        // Function keys
        "\u{F704}" => vec![0x1B, b'O', b'P'],                 // F1
        "\u{F705}" => vec![0x1B, b'O', b'Q'],                 // F2
        "\u{F706}" => vec![0x1B, b'O', b'R'],                 // F3
        "\u{F707}" => vec![0x1B, b'O', b'S'],                 // F4
        "\u{F708}" => vec![0x1B, b'[', b'1', b'5', b'~'],    // F5
        "\u{F709}" => vec![0x1B, b'[', b'1', b'7', b'~'],    // F6
        "\u{F70A}" => vec![0x1B, b'[', b'1', b'8', b'~'],    // F7
        "\u{F70B}" => vec![0x1B, b'[', b'1', b'9', b'~'],    // F8
        "\u{F70C}" => vec![0x1B, b'[', b'2', b'0', b'~'],    // F9
        "\u{F70D}" => vec![0x1B, b'[', b'2', b'1', b'~'],    // F10
        "\u{F70E}" => vec![0x1B, b'[', b'2', b'3', b'~'],    // F11
        "\u{F70F}" => vec![0x1B, b'[', b'2', b'4', b'~'],    // F12

        // Enter/Return → CR
        "\r" | "\n" => vec![0x0D],

        // Everything else passes through as raw UTF-8
        _ => text.as_bytes().to_vec(),
    }
}

fn key_to_bytes(text: &str, _ctrl: bool, meta: bool, _shift: bool) -> Vec<u8> {
    // Modifier-only: return empty
    if matches!(text,
        "\u{0010}" | "\u{0015}" | "\u{0011}" | "\u{0016}" |
        "\u{0012}" | "\u{0013}" | "\u{0014}" | "\u{0017}" | "\u{0018}" | "\u{0019}"
    ) { return vec![]; }

    let bytes = key_text_to_bytes(text);
    if bytes.is_empty() { return vec![]; }

    if meta {
        // ESC-prefix for Meta/Option (standard Alt behavior)
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
    state: &mut TermState,
    json: &str,
    pty_writer: &mut dyn Write,
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
            state.resize(new_cols, new_rows);
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
                let sel = selection_text(state);
                if !sel.is_empty() {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(sel);
                    }
                    state.sel_start = None;
                    state.sel_end   = None;
                    return true;
                }
                // No selection — fall through so PTY receives the bytes
            }

            // Any non-modifier key clears selection
            if !matches!(text,
                "\u{0010}" | "\u{0015}" | "\u{0011}" | "\u{0016}" |
                "\u{0012}" | "\u{0013}" | "\u{0014}" | "\u{0017}" | "\u{0018}" | "\u{0019}"
            ) {
                state.sel_start = None;
                state.sel_end   = None;
            }

            let bytes = key_to_bytes(text, ctrl, meta, shift);
            if !bytes.is_empty() {
                let _ = pty_writer.write_all(&bytes);
                let _ = pty_writer.flush();
            }
            true
        }
        "paste" => {
            if let Some(t) = v["text"].as_str() {
                let _ = pty_writer.write_all(t.as_bytes());
                let _ = pty_writer.flush();
                true
            } else {
                false
            }
        }
        "chdir" => {
            if let Some(path) = v["path"].as_str() {
                let cmd = format!("cd {}\r", shlex_quote(path));
                let _ = pty_writer.write_all(cmd.as_bytes());
                let _ = pty_writer.flush();
            }
            true
        }
        "scroll" => {
            let delta = v["delta"].as_f64().unwrap_or(0.0);
            let lines = (delta.abs() / fm.cell_h as f64).ceil() as usize;
            if delta > 0.0 {
                // scroll up into scrollback
                state.scroll_offset = (state.scroll_offset + lines).min(state.scrollback.len());
            } else if delta < 0.0 {
                // scroll down toward live
                state.scroll_offset = state.scroll_offset.saturating_sub(lines);
            }
            state.dirty = true;
            true
        }
        "mouse_press" => {
            let x = v["x"].as_f64().unwrap_or(0.0) as usize;
            let y = v["y"].as_f64().unwrap_or(0.0) as usize;
            let col = (x / fm.cell_w.max(1)).min(state.cols.saturating_sub(1));
            let row = (y / fm.cell_h.max(1)).min(state.rows.saturating_sub(1));
            state.sel_start = Some((row, col));
            state.sel_end   = Some((row, col));
            state.sel_drag  = true;
            true
        }
        "mouse_move" => {
            if state.sel_drag {
                let x = v["x"].as_f64().unwrap_or(0.0) as usize;
                let y = v["y"].as_f64().unwrap_or(0.0) as usize;
                let col = (x / fm.cell_w.max(1)).min(state.cols.saturating_sub(1));
                let row = (y / fm.cell_h.max(1)).min(state.rows.saturating_sub(1));
                state.sel_end = Some((row, col));
                true
            } else { false }
        }
        "mouse_release" => {
            state.sel_drag = false;
            // Clear trivial zero-length selection
            if state.sel_start == state.sel_end {
                state.sel_start = None;
                state.sel_end   = None;
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

    // Initial terminal size — will be updated on first resize event
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
            // If we can't open a PTY, show error frame
            let mut state = TermState::new(init_cols, init_rows);
            let error_msg = format!("PTY error: {e}");
            for (i, ch) in error_msg.chars().enumerate() {
                if let Some(cell) = state.cell_mut(0, i) {
                    cell.ch = ch;
                    cell.fg = Rgb(0xff, 0x77, 0x77);
                }
            }
            let pixels = render(&state, &fm);
            write_frame((init_cols * fm.cell_w) as u32, (init_rows * fm.cell_h) as u32, &pixels);
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
            let mut state = TermState::new(init_cols, init_rows);
            let error_msg = format!("Spawn error: {e}");
            for (i, ch) in error_msg.chars().enumerate() {
                if let Some(cell) = state.cell_mut(0, i) {
                    cell.ch = ch;
                    cell.fg = Rgb(0xff, 0x77, 0x77);
                }
            }
            let pixels = render(&state, &fm);
            write_frame((init_cols * fm.cell_w) as u32, (init_rows * fm.cell_h) as u32, &pixels);
            return;
        }
    };

    // Get PTY writer and master (for resize)
    let mut pty_writer = pty_pair.master.take_writer().expect("pty writer");
    let master = pty_pair.master;
    let mut pty_reader = master.try_clone_reader().expect("pty reader");

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
                        if tx.send(Msg::PtyData(data)).is_err() { break; }
                    }
                }
            }
        });
    }

    let mut state = TermState::new(init_cols, init_rows);
    let mut vte_parser = Parser::new();

    // Send an initial frame immediately
    {
        let pixels = render(&state, &fm);
        write_frame((init_cols * fm.cell_w) as u32, (init_rows * fm.cell_h) as u32, &pixels);
        state.dirty = false;
    }

    // Blink: toggle cursor every ~500 ms. 16ms timeout × 31 ≈ 500 ms.
    let mut blink_ticks: u32 = 0;
    const BLINK_PERIOD: u32 = 31;

    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(16)) {
            Ok(Msg::PtyData(bytes)) => {
                let mut performer = Performer { state: &mut state };
                for byte in &bytes {
                    vte_parser.advance(&mut performer, *byte);
                }
                // Reset blink so cursor appears immediately after output
                blink_ticks = 0;
                state.cursor_blink_on = true;
                state.dirty = true;
            }
            Ok(Msg::Line(json)) => {
                if handle_event(&mut state, &json, &mut *pty_writer, &fm, &*master) {
                    state.dirty = true;
                }
            }
            Ok(Msg::PtyEof) => {
                if state.dirty {
                    let phys_w = (state.cols * fm.cell_w) as u32;
                    let phys_h = (state.rows * fm.cell_h) as u32;
                    let pixels = render(&state, &fm);
                    write_frame(phys_w, phys_h, &pixels);
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                blink_ticks += 1;
                if blink_ticks >= BLINK_PERIOD {
                    blink_ticks = 0;
                    state.cursor_blink_on = !state.cursor_blink_on;
                    state.dirty = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if state.dirty {
            let phys_w = (state.cols * fm.cell_w) as u32;
            let phys_h = (state.rows * fm.cell_h) as u32;
            let pixels = render(&state, &fm);
            write_frame(phys_w, phys_h, &pixels);
            state.dirty = false;
        }
    }
}
