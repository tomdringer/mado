use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;

use vte::Parser;

use slint::SharedPixelBuffer;

use crate::pane_tree::NodeId;
use super::font::FontRaster;
use super::pty_session::PtySession;
use super::renderer::render_terminal;
use super::terminal_state::{Cell, TerminalState, VteHandler, DEFAULT_FG, DEFAULT_BG};

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

/// Emit one row of cells as 24-bit ANSI escape sequences into `out`.
/// Trailing space-with-default-colors cells are trimmed. Color change escapes
/// are suppressed when they match the previous row's last color (tracked via
/// `prev_fg` / `prev_bg`), saving bytes in typical cases.
fn emit_row(
    cells: &[Cell],
    out: &mut Vec<u8>,
    prev_fg: &mut Option<[u8; 4]>,
    prev_bg: &mut Option<[u8; 4]>,
    cols: usize,
) {
    // Find the last non-blank cell so we don't emit trailing spaces.
    let trim_end = cells.iter().rposition(|c| {
        c.ch != ' ' || c.fg != DEFAULT_FG || c.bg != DEFAULT_BG
    }).map(|i| i + 1).unwrap_or(0);

    let limit = trim_end.min(cells.len()).min(cols);
    for cell in &cells[..limit] {
        if Some(cell.fg) != *prev_fg {
            let [r, g, b, _] = cell.fg;
            out.extend_from_slice(
                format!("\x1b[38;2;{};{};{}m", r, g, b).as_bytes()
            );
            *prev_fg = Some(cell.fg);
        }
        if Some(cell.bg) != *prev_bg {
            let [r, g, b, _] = cell.bg;
            out.extend_from_slice(
                format!("\x1b[48;2;{};{};{}m", r, g, b).as_bytes()
            );
            *prev_bg = Some(cell.bg);
        }
        // Encode the character as UTF-8.
        let mut buf = [0u8; 4];
        out.extend_from_slice(cell.ch.encode_utf8(&mut buf).as_bytes());
    }
}

pub struct TerminalRegistry {
    pub sessions: HashMap<NodeId, PtySession>,
    /// Sessions parked for background projects: project-code → (node-id → session).
    /// Parked sessions are still running but not rendered. When the user switches
    /// back to a project, its sessions are moved back into `sessions` and resized.
    pub parked: HashMap<String, HashMap<NodeId, PtySession>>,
    pub font: FontRaster,
    pub scale: f32,
    pub font_size: f32,
    pub font_family: String,
    pub shell: String,
    /// Commands to inject (pre-type) into panes after a delay on restore.
    /// Maps node-id → (command text, time queued). Flushed by `flush_injects`.
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

    /// Rebuild the font at a new size. Call `resize` for each pane afterwards
    /// so PTY dimensions update to match the new cell size.
    pub fn set_font_size(&mut self, font_size: f32) {
        self.font_size = font_size;
        self.font = FontRaster::new(font_size, self.scale, &self.font_family);
        // Set the AtomicBool dirty flag — this is what drain_dirty actually checks.
        for sess in self.sessions.values_mut() {
            sess.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Spawn a shell at `id`. `cwd` sets the initial working directory; pass
    /// `None` to inherit from the current process (default shell behaviour).
    pub fn spawn(&mut self, id: NodeId, logical_w: f32, logical_h: f32, cwd: Option<&str>) {
        let (cols, rows, pw, ph) = self.logical_to_cells(logical_w, logical_h);
        let mut session = PtySession::spawn(cols as u16, rows as u16, &self.shell.clone(), cwd);
        session.phys_w = pw;
        session.phys_h = ph;
        self.sessions.insert(id, session);
    }

    /// Spawn an arbitrary program+args at `id` with optional extra env vars.
    pub fn spawn_cmd(&mut self, id: NodeId, logical_w: f32, logical_h: f32,
                     program: &str, args: &[&str], cwd: Option<&str>,
                     extra_env: &[(&str, &str)]) {
        let (cols, rows, pw, ph) = self.logical_to_cells(logical_w, logical_h);
        let mut session = PtySession::spawn_cmd(cols as u16, rows as u16, program, args, cwd, extra_env);
        session.phys_w = pw;
        session.phys_h = ph;
        self.sessions.insert(id, session);
    }

    /// Spawn the shell with `banner` already in the terminal state before the
    /// reader thread starts — guaranteed to appear before any shell output.
    pub fn spawn_with_banner(&mut self, id: NodeId, logical_w: f32, logical_h: f32,
                              cwd: Option<&str>, banner: &[u8]) {
        let (cols, rows, pw, ph) = self.logical_to_cells(logical_w, logical_h);
        let mut session = PtySession::spawn_with_banner(
            cols as u16, rows as u16, &self.shell.clone(), cwd, banner);
        session.phys_w = pw;
        session.phys_h = ph;
        self.sessions.insert(id, session);
    }

    /// Return the working directory for a session via OSC 7.
    /// OSC 7 is emitted by Starship (configured by Mado) on every prompt,
    /// so it's accurate even inside subshells and over SSH.
    pub fn get_cwd(&self, id: NodeId) -> Option<String> {
        let sess = self.sessions.get(&id)?;
        sess.state.lock().ok()?.cwd.clone()
    }

    /// Collect cwds for every id in `ids`.
    pub fn collect_cwds(&self, ids: &[NodeId]) -> HashMap<NodeId, String> {
        ids.iter()
            .filter_map(|&id| self.get_cwd(id).map(|cwd| (id, cwd)))
            .collect()
    }

    pub fn remove(&mut self, id: NodeId) {
        self.sessions.remove(&id);
    }

    /// Rename a session key — used when the pane tree promotes a sibling leaf
    /// into its parent's slot, changing the node ID without changing the PTY.
    pub fn remap_id(&mut self, old_id: NodeId, new_id: NodeId) {
        if let Some(sess) = self.sessions.remove(&old_id) {
            self.sessions.insert(new_id, sess);
        }
    }

    /// Move the given sessions out of `sessions` into the parked map for `project`.
    /// The PTY threads keep running; they just won't be rendered until restored.
    pub fn park_project(&mut self, project: &str, leaf_ids: &[NodeId]) {
        let park = self.parked.entry(project.to_string()).or_default();
        park.clear();
        for &id in leaf_ids {
            if let Some(sess) = self.sessions.remove(&id) {
                park.insert(id, sess);
            }
        }
    }

    /// Restore parked sessions for `project` into `sessions`, resizing each to
    /// match the current pane geometry. For any pane whose id is not in the park
    /// (e.g. the layout changed on disk), a fresh session is spawned from the
    /// saved cwd. If no park exists at all, every pane gets a fresh session.
    ///
    /// `panes` is a slice of `(node_id, logical_width, logical_height)`.
    /// `pane_cwds` is the cwd map returned by `PaneTree::from_saved`.
    /// `snapshots` maps leaf id → ANSI bytes (base64-encoded) to inject as a
    /// visual banner before the shell prompt appears on fresh spawns.
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
                if let Some(sess) = park.remove(&id) {
                    // Parked session still live — no banner needed.
                    self.sessions.insert(id, sess);
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

        // Queue last command for pre-typing after the shell prompt is ready.
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

        // Parse the snapshot ANSI through a *temporary* terminal state.
        // This avoids injecting extra blank-row-generating sequences into the
        // real session and prevents the banner from poisoning theme_bg detection.
        let (cols, rows, _, _) = self.logical_to_cells(w, h);
        let mut tmp = TerminalState::new(cols, rows);
        {
            let mut parser = Parser::new();
            let mut handler = VteHandler(&mut tmp);
            for &b in &banner_bytes {
                parser.advance(&mut handler, b);
            }
        }

        // Collect rows from the temp state: scrollback first (oldest→newest),
        // then any visible-screen rows that contain actual content.
        let mut to_inject: VecDeque<Vec<Cell>> = tmp.scrollback.clone();
        for r in 0..tmp.rows {
            let row_start = r * tmp.cols;
            let row_end = row_start + tmp.cols;
            if row_end <= tmp.cells.len() {
                let row = &tmp.cells[row_start..row_end];
                if row.iter().any(|c| c.ch != ' ' || c.fg != DEFAULT_FG || c.bg != DEFAULT_BG) {
                    to_inject.push_back(row.to_vec());
                }
            }
        }

        if to_inject.is_empty() {
            return;
        }

        // Prepend the restored rows to the real session's scrollback so the user
        // can scroll up to see them. The real visible screen stays clean.
        if let Some(sess) = self.sessions.get(&id) {
            if let Ok(mut st) = sess.state.lock() {
                let existing: VecDeque<Vec<Cell>> = st.scrollback.drain(..).collect();
                st.scrollback = to_inject;
                st.scrollback.extend(existing);
                while st.scrollback.len() > 3000 {
                    st.scrollback.pop_front();
                }
                st.dirty = true;
            }
        }
    }

    /// Pre-type `command` into pane `id` after ~500ms delay (user presses Enter to run).
    #[allow(dead_code)]
    pub fn queue_inject(&mut self, id: NodeId, command: String) {
        if !command.is_empty() {
            self.pending_injects.insert(id, (command, std::time::Instant::now()));
        }
    }

    /// Write any queued inject commands whose delay has elapsed into their PTY.
    /// Call this on every render tick — the delay check is internal.
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
            if let Some(sess) = self.sessions.get_mut(&id) {
                sess.write_input(cmd.as_bytes());
            }
        }
    }

    /// Return the foreground process name for `id`, or the CWD basename when at
    /// Capture the full command line of any running foreground process for `id`.
    /// Used when saving a workspace to restore the pre-typed command on next launch.
    fn get_foreground_command(&self, id: NodeId) -> Option<String> {
        let sess = self.sessions.get(&id)?;
        let shell_pid = sess.pid?;

        let pgrep = std::process::Command::new("pgrep")
            .args(["-P", &shell_pid.to_string()])
            .output()
            .ok()?;
        let stdout = String::from_utf8(pgrep.stdout).ok()?;
        let child_pid = stdout.trim().lines().next()?.trim().to_string();
        if child_pid.is_empty() { return None; }

        // Get full argument list.
        let ps = std::process::Command::new("ps")
            .args(["-o", "args=", "-p", &child_pid])
            .output()
            .ok()?;
        let cmd = String::from_utf8(ps.stdout).ok()?;
        let cmd = cmd.trim().to_string();
        if cmd.is_empty() { None } else { Some(cmd) }
    }

    /// Collect the data needed for background title computation.
    /// Returns `(id, shell_pid, cwd_basename)` for every active session.
    /// Cheap to call on the main thread — no process spawning.
    pub fn collect_title_inputs(&self) -> Vec<(NodeId, Option<u32>, Option<String>)> {
        self.sessions.iter().map(|(&id, sess)| {
            let cwd_basename = sess.state.lock().ok()
                .and_then(|st| st.cwd.clone())
                .and_then(|cwd| {
                    std::path::Path::new(&cwd)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string())
                });
            (id, sess.pid, cwd_basename)
        }).collect()
    }

    /// Collect foreground commands for each id in `ids` (skipping panes at shell prompt).
    pub fn collect_last_commands(&self, ids: &[NodeId]) -> HashMap<NodeId, String> {
        ids.iter()
            .filter_map(|&id| self.get_foreground_command(id).map(|cmd| (id, cmd)))
            .collect()
    }

    pub fn resize(&mut self, id: NodeId, logical_w: f32, logical_h: f32) {
        let (cols, rows, pw, ph) = self.logical_to_cells(logical_w, logical_h);
        if let Some(sess) = self.sessions.get_mut(&id) {
            sess.resize(cols as u16, rows as u16, pw, ph);
        }
    }

    /// Returns (cursor_col, cursor_row, cols, rows, in_alt_screen) for a pane.
    pub fn cursor_info(&self, id: NodeId) -> Option<(usize, usize, usize, usize, bool)> {
        self.sessions.get(&id).and_then(|sess| {
            sess.state.lock().ok().map(|st| {
                (st.cursor_col, st.cursor_row, st.cols, st.rows, st.in_alt_screen)
            })
        })
    }

    pub fn write_key(&mut self, id: NodeId, data: &[u8]) {
        if let Some(sess) = self.sessions.get_mut(&id) {
            // Jump back to live view when sending input so output is visible.
            if let Ok(mut st) = sess.state.lock() {
                if st.scroll_offset != 0 {
                    st.scroll_offset = 0;
                    st.dirty = true;
                }
            }
            sess.write_input(data);
        }
    }

    /// Scroll a terminal's view by `delta_rows` rows.
    /// Positive = toward older output; negative = toward live output.
    /// In alternate-screen mode (nvim, helix, etc.) scroll is forwarded as
    /// cursor-up/down key sequences so the app can handle it natively.
    pub fn scroll(&mut self, id: NodeId, delta_rows: i32) {
        if delta_rows == 0 { return; }
        if let Some(sess) = self.sessions.get_mut(&id) {
            let in_alt = sess.state.lock()
                .map(|st| st.in_alt_screen)
                .unwrap_or(false);

            if in_alt {
                // Forward to the running app as cursor up/down sequences.
                let seq: &[u8] = if delta_rows > 0 { b"\x1b[A" } else { b"\x1b[B" };
                let n = delta_rows.unsigned_abs() as usize;
                for _ in 0..n.min(10) {
                    sess.write_input(seq);
                }
            } else {
                if let Ok(mut st) = sess.state.lock() {
                    st.adjust_scroll_offset(delta_rows);
                }
                sess.dirty.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Inject raw bytes (ANSI sequences, text) directly into a session's
    /// terminal state, bypassing the PTY. Used for the welcome banner.
    #[allow(dead_code)]
    pub fn inject_bytes(&self, id: NodeId, data: &[u8]) {
        if let Some(sess) = self.sessions.get(&id) {
            sess.inject_bytes(data);
        }
    }

    /// Return the current column count for a session (defaults to 80).
    #[allow(dead_code)]
    pub fn cols(&self, id: NodeId) -> usize {
        self.sessions.get(&id)
            .and_then(|s| s.state.lock().ok().map(|st| st.cols))
            .unwrap_or(80)
    }

    /// Force the session for `id` to re-render on the next timer tick.
    pub fn mark_dirty(&self, id: NodeId) {
        if let Some(sess) = self.sessions.get(&id) {
            sess.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Extract the text covered by `sel` (already normalized, in display-row space)
    /// from the session's scrollback + screen buffer.
    pub fn get_selection_text(&self, id: NodeId, sel: ((usize, usize), (usize, usize))) -> String {
        let sess = match self.sessions.get(&id) {
            Some(s) => s,
            None => return String::new(),
        };
        let st = match sess.state.lock() {
            Ok(s) => s,
            Err(_) => return String::new(),
        };

        let ((c0, r0), (c1, r1)) = sel;
        let sb_len = st.scrollback.len();
        let scroll_offset = st.scroll_offset;
        let mut text = String::new();

        for drow in r0..=r1 {
            let virtual_row = (sb_len as isize) - (scroll_offset as isize) + (drow as isize);

            let row_chars: Vec<char> = if virtual_row < 0 {
                vec![' '; st.cols]
            } else if (virtual_row as usize) < sb_len {
                let cells = &st.scrollback[virtual_row as usize];
                let mut ch: Vec<char> = cells.iter().map(|c| c.ch).collect();
                ch.resize(st.cols, ' ');
                ch
            } else {
                let screen_row = (virtual_row as usize) - sb_len;
                if screen_row < st.rows {
                    st.cells[screen_row * st.cols..(screen_row + 1) * st.cols]
                        .iter().map(|c| c.ch).collect()
                } else {
                    vec![' '; st.cols]
                }
            };

            let start_col = if drow == r0 { c0 } else { 0 };
            let end_col   = if drow == r1 { c1 } else { st.cols.saturating_sub(1) };
            let end_col   = end_col.min(row_chars.len().saturating_sub(1));

            if start_col < row_chars.len() {
                let line: String = row_chars[start_col..=end_col].iter().collect();
                if drow < r1 {
                    text.push_str(line.trim_end_matches(' '));
                    text.push('\n');
                } else {
                    text.push_str(line.trim_end_matches(' '));
                }
            }
        }

        text
    }

    /// Capture the last `max_scroll_rows` rows of scrollback for `id` as ANSI
    /// escape bytes, then base64-encode the result. Returns `None` for
    /// alt-screen panes (nvim, helix, etc.) whose content is transient.
    ///
    /// The bytes are later parsed through a *temporary* TerminalState in
    /// `spawn_with_optional_banner`; the resulting scrollback rows are injected
    /// directly into the new session. No push-to-scrollback newlines are emitted
    /// here — those would create blank rows in the restored session.
    pub fn snapshot_ansi(&self, id: NodeId, max_scroll_rows: usize) -> Option<String> {
        let sess = self.sessions.get(&id)?;
        let st = sess.state.lock().ok()?;

        if st.in_alt_screen {
            return None;
        }

        const MAX_BYTES: usize = 64 * 1024;

        let mut out: Vec<u8> = Vec::with_capacity(8 * 1024);
        let mut prev_fg: Option<[u8; 4]> = None;
        let mut prev_bg: Option<[u8; 4]> = None;

        let sb_len = st.scrollback.len();
        let skip = sb_len.saturating_sub(max_scroll_rows);
        for row in st.scrollback.iter().skip(skip) {
            emit_row(row, &mut out, &mut prev_fg, &mut prev_bg, st.cols);
            out.extend_from_slice(b"\r\n");
            if out.len() >= MAX_BYTES { break; }
        }
        out.truncate(MAX_BYTES);

        use base64::Engine as _;
        Some(base64::engine::general_purpose::STANDARD.encode(&out))
    }

    /// Collect ANSI snapshots for every id in `ids`, skipping alt-screen panes.
    pub fn collect_snapshots(&self, ids: &[NodeId]) -> HashMap<NodeId, String> {
        ids.iter()
            .filter_map(|&id| self.snapshot_ansi(id, 100).map(|s| (id, s)))
            .collect()
    }

    /// Returns (id, physical-pixel buffer) pairs for all dirty sessions.
    /// `selection` carries the active pane selection so it is baked into the render.
    pub fn drain_dirty(
        &mut self,
        selection: Option<(NodeId, ((usize, usize), (usize, usize)))>,
    ) -> Vec<(NodeId, SharedPixelBuffer<slint::Rgba8Pixel>)> {
        let font = &self.font;
        self.sessions.iter_mut()
            .filter_map(|(id, sess)| {
                if sess.dirty.load(Ordering::Relaxed) {
                    sess.dirty.store(false, Ordering::Relaxed);
                    let sel = selection.and_then(|(sid, range)| {
                        if sid == *id { Some(range) } else { None }
                    });
                    render_terminal(&sess.state, font, true, sel, sess.phys_w, sess.phys_h)
                        .map(|buf| (*id, buf))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Returns the ids of all sessions that have a pending bell, clearing the flag.
    pub fn drain_bells(&mut self) -> Vec<NodeId> {
        self.sessions.iter_mut()
            .filter_map(|(id, sess)| {
                if sess.bell.swap(false, Ordering::Relaxed) {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Clear the visible screen for `id` and re-process `banner` bytes into it.
    /// Used to re-center the welcome banner after the window is resized before
    /// the user has typed anything.
    pub fn reinject_banner(&self, id: NodeId, banner: &[u8]) {
        use super::terminal_state::VteHandler;
        if let Some(sess) = self.sessions.get(&id) {
            if let Ok(mut st) = sess.state.lock() {
                // Clear every visible cell without touching scrollback.
                let blank = super::terminal_state::Cell::default();
                for cell in &mut st.cells {
                    *cell = blank.clone();
                }
                st.cursor_col = 0;
                st.cursor_row = 0;
                st.dirty = true;
                // Re-process the banner with updated padding for the current cols.
                let mut parser = Parser::new();
                let mut handler = VteHandler(&mut *st);
                for &b in banner {
                    parser.advance(&mut handler, b);
                }
            }
            sess.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Estimate the terminal column count for a given logical pane width.
    pub fn logical_to_cols(&self, logical_w: f32) -> usize {
        let phys_w = (logical_w * self.scale) as usize;
        (phys_w / self.font.cell_w.max(1)).max(1)
    }



    /// Maximum logical pixel width that yields at most `max_cols` columns.
    pub fn max_logical_w(&self, max_cols: usize) -> f32 {
        (max_cols * self.font.cell_w.max(1)) as f32 / self.scale.max(0.001)
    }

    /// Convert logical pixel dimensions to (cols, rows, phys_w, phys_h).
    fn logical_to_cells(&self, lw: f32, lh: f32) -> (usize, usize, usize, usize) {
        let phys_w = (lw * self.scale) as usize;
        let phys_h = (lh * self.scale) as usize;
        let cw = self.font.cell_w.max(1);
        let ch = self.font.cell_h.max(1);
        ((phys_w / cw).max(1), (phys_h / ch).max(1), phys_w.max(1), phys_h.max(1))
    }
}
