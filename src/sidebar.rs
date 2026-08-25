// ─── Sidebar plugin system ────────────────────────────────────────────────────
//
// The only plugin that ships with Mado is Tasku. Additional plugins can be
// installed by the user into ~/.mado/plugins/ (future work).

// ─── Sidebar runtime state ────────────────────────────────────────────────────

pub struct SidebarState {
    /// Plugin IDs in the user's current display order.
    pub plugin_order: Vec<String>,
    /// Detected Tasku binary path, or None if not installed.
    pub tasku_path: Option<String>,
    /// Current sidebar width in logical pixels.
    pub width: f32,
    /// Current Tasku panel height in logical pixels (full panel, not just terminal).
    /// Kept in sync with the Slint `tasku-panel-h` property so that
    /// `on_sidebar_width_changed` can pass the correct terminal height to the PTY.
    pub tasku_panel_h: f32,
    /// Current AI panel height in logical pixels. Same sync contract as tasku_panel_h.
    pub ai_panel_h: f32,
}

impl SidebarState {
    pub fn new(width: f32, tasku_path: Option<String>) -> Self {
        Self {
            plugin_order: vec![
                "tasku".to_string(),
                "ai".to_string(),
                "clock".to_string(),
                "priorities".to_string(),
                "workspaces".to_string(),
            ],
            tasku_path,
            width,
            tasku_panel_h: crate::DEFAULT_TASKU_PANEL_H,
            ai_panel_h:    crate::DEFAULT_AI_PANEL_H,
        }
    }

    /// Apply a saved plugin order. Unknown IDs are ignored; any plugins missing
    /// from the saved order are appended at the end in their default positions.
    pub fn apply_order(&mut self, saved: &[String]) {
        let known: Vec<String> = self.plugin_order.clone();
        let mut ordered: Vec<String> = saved.iter()
            .filter(|id| known.contains(id))
            .cloned()
            .collect();
        for id in &known {
            if !ordered.contains(id) {
                ordered.push(id.clone());
            }
        }
        self.plugin_order = ordered;
    }

    /// Reorder: move the plugin at `from` to position `to` (insert-before semantics).
    /// No-op if indices are out of range or equivalent.
    pub fn reorder(&mut self, from: usize, to: usize) {
        let len = self.plugin_order.len();
        if from >= len { return; }
        let effective_to = if to > from { to - 1 } else { to };
        if effective_to >= len { return; }
        if from == effective_to { return; }
        let item = self.plugin_order.remove(from);
        self.plugin_order.insert(effective_to, item);
    }

    /// Produce ordered `(id, title, subtitle, icon)` tuples for the Slint plugin model.
    pub fn ordered_items(&self) -> Vec<(&'static str, &'static str, String, &'static str)> {
        self.plugin_order
            .iter()
            .filter_map(|id| match id.as_str() {
                "tasku" => Some((
                    "tasku",
                    "TASKU",
                    match &self.tasku_path {
                        Some(p) => p.clone(),
                        None    => String::from("not found — install tasku"),
                    },
                    "\u{F0AE}",  // nf-fa-tasks
                )),
                "ai" => Some((
                    "ai",
                    "AI",
                    String::from("terminal client"),
                    "\u{EB03}",  // nf-cod-hubot
                )),
                "clock" => Some((
                    "clock",
                    "CLOCK",
                    String::from("time · date · weather"),
                    "\u{F017}",  // nf-fa-clock_o
                )),
                "priorities" => Some((
                    "priorities",
                    "PRIORITIES",
                    String::from("drag to rank"),
                    "\u{F005}",  // nf-fa-star
                )),
                "workspaces" => Some((
                    "workspaces",
                    "WORKSPACES",
                    String::from("terminal layouts"),
                    "\u{EB20}",  // nf-cod-dashboard
                )),
                _ => None,
            })
            .collect()
    }
}
