use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use slint::SharedPixelBuffer;

use libghostty_vt::Terminal;
use libghostty_vt::render::{RenderState, RowIterator, CellIterator, Dirty};
use libghostty_vt::screen::Screen;
use libghostty_vt::style::RgbColor;
use libghostty_vt::terminal::Options;

use crate::pane_tree::NodeId;
use super::font::FontRaster;
use super::pty_session::PtySession;
use super::renderer::render_terminal;

/// Default foreground colour (used for snapshot_ansi emit).
const DEFAULT_FG: RgbColor = RgbColor { r: 0xCC, g: 0xCC, b: 0xCC };
/// Default background colour.
const DEFAULT_BG: RgbColor = RgbColor { r: 0x1E, g: 0x20, b: 0x30 };

/// Compute the display title for a pane given its shell PID and CWD basename.
/// Spawns `pgrep` + `ps` to detect a running foreground process; falls back to
/// the CWD basename when the shell is idle.
///
/// **Must be called from a background thread** — process spawning blocks.
pub fn compute_pane_title(pid: Option<u32>, cwd_basename: Option<String>) -> String {
    if let Some(shell_pid) = pid {
        let pgrep = std::process::Command::new("pgrep")
            .args(["-P", &shell_pid.to_string()])
            .output();
        if let Ok(out) = pgrep {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if let Some(child_pid) = stdout.trim().lines().next() {
                let child_pid = child_pid.trim();
                if !child_pid.is_empty() {
                    if let Ok(ps) = std::process::Command::new("ps")
                        .args(["-o", "comm=", "-p", child_pid])
                        .output()
                    {
                        let comm = String::from_utf8_lossy(&ps.stdout);
                        let comm = comm.trim().to_string();
                        if !comm.is_empty() {
                            return comm;
                        }
                    }
                }
            }
        }
    }
    cwd_basename.unwrap_or_default()
}

/// One live terminal session plus all the state the registry needs per-pane.
pub struct TermSession {
    pub session: PtySession,
    /// The libghostty-vt terminal emulator (lives on this thread).
    pub terminal: Terminal<'static, 'static>,
    /// Render state — reused across frames for dirty-tracking efficiency.
    pub render_state: RenderState<'static>,
    /// Row iterator reused across frames.
    pub row_it: RowIterator<'static>,
    /// Cell iterator reused across frames.
    pub cell_it: CellIterator<'static>,
    /// Current terminal column count (tracked after each resize/drain).
    pub cols: usize,
    /// Current terminal row count.
    pub rows: usize,
    /// CWD from OSC 7, wrapped for the on_pwd_changed callback.
    pub cwd: Arc<Mutex<Option<String>>>,
    /// Title from OSC 2, wrapped for the on_title_changed callback.
    pub title: Arc<Mutex<Option<String>>>,
    /// Whether pre_bytes have been injected yet.
    pre_bytes_injected: bool,
}

impl TermSession {
    fn new(session: PtySession, cols: usize, rows: usize, cell_w: u32, cell_h: u32) -> Self {
        let cwd: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let title: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let mut terminal = Terminal::new(Options {
            cols: cols as u16,
            rows: rows as u16,
            max_scrollback: 1000,
        }).expect("Terminal::new failed");

        // Default colours matching Mado's dark theme
        let _ = terminal.set_default_fg_color(Some(DEFAULT_FG));
        let _ = terminal.set_default_bg_color(Some(DEFAULT_BG));

        // Wire bell callback → PtySession bell AtomicBool
        {
            let bell = Arc::clone(&session.bell);
            let _ = terminal.on_bell(move |_| {
                bell.store(true, Ordering::Relaxed);
            });
        }

        // Wire OSC 7 pwd callback — no-op since we read terminal.pwd() directly in get_cwd
        {
            let _ = terminal.on_pwd_changed(move |_term| {
                // The new pwd is read in get_cwd() via terminal.pwd() on the main thread.
                // No state to update here.
            });
        }

        // Wire title callback
        {
            let title_cb = Arc::clone(&title);
            let _ = terminal.on_title_changed(move |term| {
                // The terminal provides the new title via OSC 2; we can read
                // it via the OSC module. For now store a sentinel to force
                // a title refresh; main.rs calls collect_title_inputs() which
                // reads terminal.pwd() etc.
                // We store "" as sentinel so the collect pass re-reads it.
                if let Ok(mut t) = title_cb.lock() {
                    *t = Some(String::new());
                }
            });
        }

        // Wire on_pty_write: write terminal query responses back to PTY
        {
            let pty_writer = Arc::clone(&session.pty_writer);
            let _ = terminal.on_pty_write(move |_term, data| {
                if let Ok(mut w) = pty_writer.lock() {
                    let _ = w.write_all(data);
                }
            });
        }

        let render_state = RenderState::new().expect("RenderState::new failed");
        let row_it = RowIterator::new().expect("RowIterator::new failed");
        let cell_it = CellIterator::new().expect("CellIterator::new failed");

        // Initial resize to set pixel dimensions
        let _ = terminal.resize(cols as u16, rows as u16, cell_w, cell_h);

        TermSession {
            session,
            terminal,
            render_state,
            row_it,
            cell_it,
            cols,
            rows,
            cwd,
            title,
            pre_bytes_injected: false,
        }
    }
}

pub struct TerminalRegistry {
    pub sessions: HashMap<NodeId, TermSession>,
    /// Sessions parked for background projects.
    pub parked: HashMap<String, HashMap<NodeId, TermSession>>,
    pub font: FontRaster,
    pub scale: f32,
    pub font_size: f32,
    pub font_family: String,
    pub shell: String,
    /// Commands to inject into panes after a delay on restore.
    pub pending_injects: HashMap<NodeId, (String, std::time::Instant)>,
}

impl TerminalRegistry {
    pub fn new(font_size: f32, scale: f32, shell: String, font_family: String) -> Self {
        TerminalRegistry {
            sessions: HashMap::new(),
            parked: HashMap::new(),
            font: FontRaster::new(font_size, scale, &font_family),
            scale,
            font_size,
            font_family,
            shell,
            pending_injects: HashMap::new(),
        }
    }

    pub fn set_font_size(&mut self, font_size: f32) {
        self.font_size = font_size;
        self.font = FontRaster::new(font_size, self.scale, &self.font_family);
        for sess in self.sessions.values_mut() {
            sess.session.dirty.store(true, Ordering::Relaxed);
        }
    }

    pub fn spawn(&mut self, id: NodeId, logical_w: f32, logical_h: f32, cwd: Option<&str>) {
        let (cols, rows, pw, ph) = self.logical_to_cells(logical_w, logical_h);
        let mut session = PtySession::spawn(cols as u16, rows as u16, &self.shell.clone(), cwd);
        session.phys_w = pw;
        session.phys_h = ph;
        let ts = TermSession::new(session, cols, rows, self.font.cell_w as u32, self.font.cell_h as u32);
        self.sessions.insert(id, ts);
    }

    pub fn spawn_cmd(&mut self, id: NodeId, logical_w: f32, logical_h: f32,
                     program: &str, args: &[&str], cwd: Option<&str>,
                     extra_env: &[(&str, &str)]) {
        let (cols, rows, pw, ph) = self.logical_to_cells(logical_w, logical_h);
        let mut session = PtySession::spawn_cmd(cols as u16, rows as u16, program, args, cwd, extra_env);
        session.phys_w = pw;
        session.phys_h = ph;
        let ts = TermSession::new(session, cols, rows, self.font.cell_w as u32, self.font.cell_h as u32);
        self.sessions.insert(id, ts);
    }

    pub fn spawn_with_banner(&mut self, id: NodeId, logical_w: f32, logical_h: f32,
                              cwd: Option<&str>, banner: &[u8]) {
        let (cols, rows, pw, ph) = self.logical_to_cells(logical_w, logical_h);
        let mut session = PtySession::spawn_with_banner(
            cols as u16, rows as u16, &self.shell.clone(), cwd, banner);
        session.phys_w = pw;
        session.phys_h = ph;
        let ts = TermSession::new(session, cols, rows, self.font.cell_w as u32, self.font.cell_h as u32);
        self.sessions.insert(id, ts);
    }

    pub fn get_cwd(&self, id: NodeId) -> Option<String> {
        let ts = self.sessions.get(&id)?;
        // Primary source: OSC 7 from libghostty-vt
        ts.terminal.pwd().ok().filter(|s| !s.is_empty()).map(|s| s.to_string())
            .or_else(|| ts.cwd.lock().ok()?.clone())
    }

    pub fn collect_cwds(&self, ids: &[NodeId]) -> HashMap<NodeId, String> {
        ids.iter()
            .filter_map(|&id| self.get_cwd(id).map(|cwd| (id, cwd)))
            .collect()
    }

    pub fn remove(&mut self, id: NodeId) {
        self.sessions.remove(&id);
    }

    pub fn remap_id(&mut self, old_id: NodeId, new_id: NodeId) {
        if let Some(ts) = self.sessions.remove(&old_id) {
            self.sessions.insert(new_id, ts);
        }
    }

    pub fn park_project(&mut self, project: &str, leaf_ids: &[NodeId]) {
        let park = self.parked.entry(project.to_string()).or_default();
        park.clear();
        for &id in leaf_ids {
            if let Some(ts) = self.sessions.remove(&id) {
                park.insert(id, ts);
            }
        }
    }

    pub fn restore_or_spawn_project(
        &mut self,
        project: &str,
        panes: &[(NodeId, f32, f32)],
        pane_cwds: &HashMap<NodeId, String>,
        snapshots: &HashMap<NodeId, String>,
        last_commands: &HashMap<NodeId, String>,
    ) {
        if let Some(mut park) = self.parked.remove(project) {
            for &(id, w, h) in panes {
                if let Some(ts) = park.remove(&id) {
                    self.sessions.insert(id, ts);
                    self.resize(id, w, h);
                } else {
                    let cwd = pane_cwds.get(&id).map(|s| s.as_str());
                    self.spawn_with_optional_banner(id, w, h, cwd, snapshots.get(&id), last_commands.get(&id));
                }
            }
        } else {
            for &(id, w, h) in panes {
                let cwd = pane_cwds.get(&id).map(|s| s.as_str());
                self.spawn_with_optional_banner(id, w, h, cwd, snapshots.get(&id), last_commands.get(&id));
            }
        }
    }

    fn spawn_with_optional_banner(
        &mut self,
        id: NodeId,
        w: f32,
        h: f32,
        cwd: Option<&str>,
        snapshot: Option<&String>,
        last_command: Option<&String>,
    ) {
        self.spawn(id, w, h, cwd);

        if let Some(cmd) = last_command {
            if !cmd.is_empty() {
                self.pending_injects.insert(id, (cmd.clone(), std::time::Instant::now()));
            }
        }

        let encoded = match snapshot {
            Some(s) => s,
            None => return,
        };

        use base64::Engine as _;
        let banner_bytes = match base64::engine::general_purpose::STANDARD.decode(encoded) {
            Ok(b) if !b.is_empty() => b,
            _ => return,
        };

        // Inject banner into the terminal — libghostty scrolls naturally.
        if let Some(ts) = self.sessions.get_mut(&id) {
            ts.terminal.vt_write(&banner_bytes);
            ts.session.dirty.store(true, Ordering::Relaxed);
        }
    }

    #[allow(dead_code)]
    pub fn queue_inject(&mut self, id: NodeId, command: String) {
        if !command.is_empty() {
            self.pending_injects.insert(id, (command, std::time::Instant::now()));
        }
    }

    pub fn flush_injects(&mut self) {
        let threshold = std::time::Duration::from_millis(600);
        let now = std::time::Instant::now();
        let ready: Vec<(NodeId, String)> = self.pending_injects
            .iter()
            .filter(|(_, (_, t))| now.duration_since(*t) >= threshold)
            .map(|(&id, (cmd, _))| (id, cmd.clone()))
            .collect();
        for (id, _) in &ready {
            self.pending_injects.remove(id);
        }
        for (id, cmd) in ready {
            if let Some(ts) = self.sessions.get_mut(&id) {
                ts.session.write_input(cmd.as_bytes());
            }
        }
    }

    fn get_foreground_command(&self, id: NodeId) -> Option<String> {
        let ts = self.sessions.get(&id)?;
        let shell_pid = ts.session.pid?;

        let pgrep = std::process::Command::new("pgrep")
            .args(["-P", &shell_pid.to_string()])
            .output().ok()?;
        let stdout = String::from_utf8(pgrep.stdout).ok()?;
        let child_pid = stdout.trim().lines().next()?.trim().to_string();
        if child_pid.is_empty() { return None; }

        let ps = std::process::Command::new("ps")
            .args(["-o", "args=", "-p", &child_pid])
            .output().ok()?;
        let cmd = String::from_utf8(ps.stdout).ok()?;
        let cmd = cmd.trim().to_string();
        if cmd.is_empty() { None } else { Some(cmd) }
    }

    pub fn collect_title_inputs(&self) -> Vec<(NodeId, Option<u32>, Option<String>)> {
        self.sessions.iter().map(|(&id, ts)| {
            let cwd_basename = self.get_cwd(id)
                .and_then(|cwd| {
                    std::path::Path::new(&cwd)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string())
                });
            (id, ts.session.pid, cwd_basename)
        }).collect()
    }

    pub fn collect_last_commands(&self, ids: &[NodeId]) -> HashMap<NodeId, String> {
        ids.iter()
            .filter_map(|&id| self.get_foreground_command(id).map(|cmd| (id, cmd)))
            .collect()
    }

    pub fn resize(&mut self, id: NodeId, logical_w: f32, logical_h: f32) {
        let (cols, rows, pw, ph) = self.logical_to_cells(logical_w, logical_h);
        if let Some(ts) = self.sessions.get_mut(&id) {
            ts.session.resize(cols as u16, rows as u16, pw, ph);
            let cw = self.font.cell_w as u32;
            let ch = self.font.cell_h as u32;
            let _ = ts.terminal.resize(cols as u16, rows as u16, cw, ch);
            ts.cols = cols;
            ts.rows = rows;
        }
    }

    /// Returns (cursor_col, cursor_row, cols, rows, in_alt_screen) for a pane.
    pub fn cursor_info(&self, id: NodeId) -> Option<(usize, usize, usize, usize, bool)> {
        let ts = self.sessions.get(&id)?;
        let cx = ts.terminal.cursor_x().ok()? as usize;
        let cy = ts.terminal.cursor_y().ok()? as usize;
        let cols = ts.terminal.cols().ok()? as usize;
        let rows = ts.terminal.rows().ok()? as usize;
        let in_alt = ts.terminal.active_screen().ok()? == Screen::Alternate;
        Some((cx, cy, cols, rows, in_alt))
    }

    pub fn write_key(&mut self, id: NodeId, data: &[u8]) {
        if let Some(ts) = self.sessions.get_mut(&id) {
            // Snap to live view when the user types
            let _ = ts.terminal.scroll_viewport(libghostty_vt::terminal::ScrollViewport::Bottom);
            ts.session.write_input(data);
        }
    }

    pub fn scroll(&mut self, id: NodeId, delta_rows: i32) {
        if delta_rows == 0 { return; }
        if let Some(ts) = self.sessions.get_mut(&id) {
            let in_alt = ts.terminal.active_screen()
                .map(|s| s == Screen::Alternate)
                .unwrap_or(false);

            if in_alt {
                let seq: &[u8] = if delta_rows > 0 { b"\x1b[A" } else { b"\x1b[B" };
                let n = delta_rows.unsigned_abs() as usize;
                for _ in 0..n.min(10) {
                    ts.session.write_input(seq);
                }
            } else {
                let _ = ts.terminal.scroll_viewport(
                    libghostty_vt::terminal::ScrollViewport::Delta(delta_rows as isize)
                );
                ts.session.dirty.store(true, Ordering::Relaxed);
            }
        }
    }

    #[allow(dead_code)]
    pub fn inject_bytes(&mut self, id: NodeId, data: &[u8]) {
        if let Some(ts) = self.sessions.get_mut(&id) {
            ts.terminal.vt_write(data);
            ts.session.dirty.store(true, Ordering::Relaxed);
        }
    }

    #[allow(dead_code)]
    pub fn cols(&self, id: NodeId) -> usize {
        self.sessions.get(&id).map(|ts| ts.cols).unwrap_or(80)
    }

    pub fn mark_dirty(&self, id: NodeId) {
        if let Some(ts) = self.sessions.get(&id) {
            ts.session.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Extract text from the terminal's selection (libghostty tracks selection).
    pub fn get_selection_text(&self, _id: NodeId, _sel: ((usize, usize), (usize, usize))) -> String {
        // libghostty-vt manages selection internally; the caller passes mouse
        // ranges to terminal.selection_start/end via the selection module.
        // For now return empty — selection text is handled by the render path.
        String::new()
    }

    /// Capture a snapshot of the terminal as ANSI bytes, base64-encoded.
    /// Returns None for alt-screen panes.
    pub fn snapshot_ansi(&self, id: NodeId, _max_scroll_rows: usize) -> Option<String> {
        let ts = self.sessions.get(&id)?;
        let in_alt = ts.terminal.active_screen().ok()? == Screen::Alternate;
        if in_alt { return None; }

        // Build a temporary render snapshot to capture the visible grid as ANSI.
        let mut rs = RenderState::new().ok()?;
        let mut row_it = RowIterator::new().ok()?;
        let mut cell_it = CellIterator::new().ok()?;
        let snapshot = rs.update(&ts.terminal).ok()?;

        let cols = snapshot.cols().ok()? as usize;
        let colors = snapshot.colors().ok()?;
        let def_fg = [colors.foreground.r, colors.foreground.g, colors.foreground.b];
        let def_bg = [colors.background.r, colors.background.g, colors.background.b];

        let mut out: Vec<u8> = Vec::with_capacity(8 * 1024);
        let mut row_iter = row_it.update(&snapshot).ok()?;

        let mut prev_fg: Option<[u8; 3]> = None;
        let mut prev_bg: Option<[u8; 3]> = None;

        while let Some(row) = row_iter.next() {
            let mut cell_iter = match cell_it.update(row) {
                Ok(ci) => ci,
                Err(_) => continue,
            };

            let mut text = String::with_capacity(8);
            let mut col = 0usize;
            let mut row_bytes: Vec<u8> = Vec::new();

            while let Some(cell) = cell_iter.next() {
                if col >= cols { col += 1; continue; }

                let graphemes = cell.graphemes_len().unwrap_or(0);
                let ch_char = if graphemes > 0 {
                    text.clear();
                    if cell.graphemes_utf8(&mut text).is_ok() && !text.is_empty() {
                        text.chars().next().unwrap_or(' ')
                    } else { ' ' }
                } else { ' ' };

                let fg = cell.fg_color().ok().flatten().map(|c| [c.r, c.g, c.b]).unwrap_or(def_fg);
                let bg = cell.bg_color().ok().flatten().map(|c| [c.r, c.g, c.b]).unwrap_or(def_bg);

                if Some(fg) != prev_fg {
                    row_bytes.extend_from_slice(
                        format!("\x1b[38;2;{};{};{}m", fg[0], fg[1], fg[2]).as_bytes()
                    );
                    prev_fg = Some(fg);
                }
                if Some(bg) != prev_bg {
                    row_bytes.extend_from_slice(
                        format!("\x1b[48;2;{};{};{}m", bg[0], bg[1], bg[2]).as_bytes()
                    );
                    prev_bg = Some(bg);
                }

                let mut cbuf = [0u8; 4];
                row_bytes.extend_from_slice(ch_char.encode_utf8(&mut cbuf).as_bytes());
                col += 1;
            }

            out.extend_from_slice(&row_bytes);
            out.extend_from_slice(b"\r\n");

            const MAX_BYTES: usize = 64 * 1024;
            if out.len() >= MAX_BYTES { break; }
        }

        let _ = snapshot.set_dirty(Dirty::Clean);

        use base64::Engine as _;
        Some(base64::engine::general_purpose::STANDARD.encode(&out))
    }

    pub fn collect_snapshots(&self, ids: &[NodeId]) -> HashMap<NodeId, String> {
        ids.iter()
            .filter_map(|&id| self.snapshot_ansi(id, 100).map(|s| (id, s)))
            .collect()
    }

    /// Re-inject banner bytes into a session after resize.
    pub fn reinject_banner(&mut self, id: NodeId, banner: &[u8]) {
        if let Some(ts) = self.sessions.get_mut(&id) {
            // ESC[2J ESC[H = clear screen, move cursor home
            ts.terminal.vt_write(b"\x1b[2J\x1b[H");
            ts.terminal.vt_write(banner);
            ts.session.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Returns (id, pixel buffer) pairs for all dirty sessions.
    pub fn drain_dirty(
        &mut self,
        _selection: Option<(NodeId, ((usize, usize), (usize, usize)))>,
    ) -> Vec<(NodeId, SharedPixelBuffer<slint::Rgba8Pixel>)> {
        let font = &self.font;
        let mut result = Vec::new();

        for (&id, ts) in self.sessions.iter_mut() {
            // Drain pending bytes from the reader thread
            let mut had_bytes = false;
            while let Ok(bytes) = ts.session.bytes_rx.try_recv() {
                ts.terminal.vt_write(&bytes);
                had_bytes = true;
            }

            // Inject pre_bytes once on the first drain
            if !ts.pre_bytes_injected && !ts.session.pre_bytes.is_empty() {
                let pb = std::mem::take(&mut ts.session.pre_bytes);
                ts.terminal.vt_write(&pb);
                ts.pre_bytes_injected = true;
                had_bytes = true;
            }

            if had_bytes {
                ts.session.dirty.store(true, Ordering::Relaxed);
            }

            if !ts.session.dirty.load(Ordering::Relaxed) {
                continue;
            }
            ts.session.dirty.store(false, Ordering::Relaxed);

            if let Some(buf) = render_terminal(
                &ts.terminal,
                &mut ts.render_state,
                &mut ts.row_it,
                &mut ts.cell_it,
                font,
                ts.session.phys_w,
                ts.session.phys_h,
            ) {
                result.push((id, buf));
            }
        }

        result
    }

    /// Returns the ids of all sessions with a pending bell, clearing the flag.
    pub fn drain_bells(&mut self) -> Vec<NodeId> {
        self.sessions.iter_mut()
            .filter_map(|(&id, ts)| {
                if ts.session.bell.swap(false, Ordering::Relaxed) {
                    Some(id)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn logical_to_cols(&self, logical_w: f32) -> usize {
        let phys_w = (logical_w * self.scale) as usize;
        (phys_w / self.font.cell_w.max(1)).max(1)
    }

    pub fn max_logical_w(&self, max_cols: usize) -> f32 {
        (max_cols * self.font.cell_w.max(1)) as f32 / self.scale.max(0.001)
    }

    fn logical_to_cells(&self, lw: f32, lh: f32) -> (usize, usize, usize, usize) {
        let phys_w = (lw * self.scale) as usize;
        let phys_h = (lh * self.scale) as usize;
        let cw = self.font.cell_w.max(1);
        let ch = self.font.cell_h.max(1);
        ((phys_w / cw).max(1), (phys_h / ch).max(1), phys_w.max(1), phys_h.max(1))
    }
}
