use std::collections::HashMap;
use std::sync::atomic::Ordering;

/// Query the cwd of a process by PID using `lsof`.
/// Output of `lsof -p <pid> -d cwd -Fn` looks like:
///   p12345
///   n/Users/tom/Sites/mado
/// We grab the line starting with 'n' and strip the prefix.
fn cwd_by_pid(pid: u32) -> Option<String> {
    let out = std::process::Command::new("lsof")
        .args(["-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout.lines()
        .find(|l| l.starts_with('n'))
        .map(|l| l[1..].to_string())
}

use slint::SharedPixelBuffer;

use crate::pane_tree::NodeId;
use super::font::FontRaster;
use super::pty_session::PtySession;
use super::renderer::render_terminal;

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
        let (cols, rows) = self.logical_to_cells(logical_w, logical_h);
        let session = PtySession::spawn(cols as u16, rows as u16, &self.shell.clone(), cwd);
        self.sessions.insert(id, session);
    }

    /// Spawn an arbitrary program+args at `id` with optional extra env vars.
    pub fn spawn_cmd(&mut self, id: NodeId, logical_w: f32, logical_h: f32,
                     program: &str, args: &[&str], cwd: Option<&str>,
                     extra_env: &[(&str, &str)]) {
        let (cols, rows) = self.logical_to_cells(logical_w, logical_h);
        let session = PtySession::spawn_cmd(cols as u16, rows as u16, program, args, cwd, extra_env);
        self.sessions.insert(id, session);
    }

    /// Spawn the shell with `banner` already in the terminal state before the
    /// reader thread starts — guaranteed to appear before any shell output.
    pub fn spawn_with_banner(&mut self, id: NodeId, logical_w: f32, logical_h: f32,
                              cwd: Option<&str>, banner: &[u8]) {
        let (cols, rows) = self.logical_to_cells(logical_w, logical_h);
        let session = PtySession::spawn_with_banner(
            cols as u16, rows as u16, &self.shell.clone(), cwd, banner);
        self.sessions.insert(id, session);
    }

    /// Return the working directory for a session.
    /// Prefers OSC 7 (accurate for nested shells / ssh).
    /// Falls back to querying the shell process's cwd via lsof when OSC 7
    /// hasn't fired yet — works without any shell configuration.
    pub fn get_cwd(&self, id: NodeId) -> Option<String> {
        let sess = self.sessions.get(&id)?;
        // OSC 7 is preferred: it follows the active shell even inside subshells
        if let Some(cwd) = sess.state.lock().ok()?.cwd.clone() {
            return Some(cwd);
        }
        // Fallback: ask the OS for the shell process's cwd
        sess.pid.and_then(cwd_by_pid)
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
    pub fn restore_or_spawn_project(
        &mut self,
        project: &str,
        panes: &[(NodeId, f32, f32)],
        pane_cwds: &HashMap<NodeId, String>,
    ) {
        if let Some(mut park) = self.parked.remove(project) {
            for &(id, w, h) in panes {
                if let Some(sess) = park.remove(&id) {
                    self.sessions.insert(id, sess);
                    self.resize(id, w, h);
                } else {
                    let cwd = pane_cwds.get(&id).map(|s| s.as_str());
                    self.spawn(id, w, h, cwd);
                }
            }
        } else {
            for &(id, w, h) in panes {
                let cwd = pane_cwds.get(&id).map(|s| s.as_str());
                self.spawn(id, w, h, cwd);
            }
        }
    }

    pub fn resize(&mut self, id: NodeId, logical_w: f32, logical_h: f32) {
        let (cols, rows) = self.logical_to_cells(logical_w, logical_h);
        if let Some(sess) = self.sessions.get_mut(&id) {
            sess.resize(cols as u16, rows as u16);
        }
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
    pub fn scroll(&mut self, id: NodeId, delta_rows: i32) {
        if let Some(sess) = self.sessions.get(&id) {
            if let Ok(mut st) = sess.state.lock() {
                st.adjust_scroll_offset(delta_rows);
            }
            // Mark the session dirty so the render timer picks it up.
            sess.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Inject raw bytes (ANSI sequences, text) directly into a session's
    /// terminal state, bypassing the PTY. Used for the welcome banner.
    pub fn inject_bytes(&self, id: NodeId, data: &[u8]) {
        if let Some(sess) = self.sessions.get(&id) {
            sess.inject_bytes(data);
        }
    }

    /// Return the current column count for a session (defaults to 80).
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
                    render_terminal(&sess.state, font, true, sel).map(|buf| (*id, buf))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Estimate the terminal column count for a given logical pane width.
    pub fn logical_to_cols(&self, logical_w: f32) -> usize {
        let phys_w = (logical_w * self.scale) as usize;
        (phys_w / self.font.cell_w.max(1)).max(1)
    }

    /// Convert logical pixel dimensions to terminal cols/rows using physical cell size.
    fn logical_to_cells(&self, lw: f32, lh: f32) -> (usize, usize) {
        let phys_w = (lw * self.scale) as usize;
        let phys_h = (lh * self.scale) as usize;
        let cw = self.font.cell_w.max(1);
        let ch = self.font.cell_h.max(1);
        ((phys_w / cw).max(1), (phys_h / ch).max(1))
    }
}
