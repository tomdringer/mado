// ─── Sidebar plugin system ────────────────────────────────────────────────────
//
// Built-in panels: Tasku, Priorities, Workspaces (can be disabled in config).
// External panels: any binary listed under [[plugins]] in config.toml —
//   spawned as a PTY, sized to the panel, receives SIGWINCH on resize.

// ─── Sidebar runtime state ────────────────────────────────────────────────────

pub struct SidebarState {
    /// Plugin IDs in the user's current display order.
    pub plugin_order: Vec<String>,
    /// Detected Tasku binary path, or None if not installed.
    pub tasku_path: Option<String>,
    /// Current sidebar width in logical pixels.
    pub width: f32,
    /// Current Tasku panel height in logical pixels.
    pub tasku_panel_h: f32,
    /// External plugins from config (ordered by config position).
    pub ext_plugins: Vec<crate::config::PluginConfig>,
}

impl SidebarState {
    pub fn new(
        width: f32,
        tasku_path: Option<String>,
        ext_plugins: Vec<crate::config::PluginConfig>,
        disable_tasku: bool,
        disable_priorities: bool,
        disable_workspaces: bool,
    ) -> Self {
        let mut order: Vec<String> = Vec::new();
        if !disable_tasku      { order.push("tasku".to_string()); }
        if !disable_priorities { order.push("priorities".to_string()); }
        if !disable_workspaces { order.push("workspaces".to_string()); }
        for p in &ext_plugins  { order.push(p.id.clone()); }

        Self {
            plugin_order: order,
            tasku_path,
            width,
            tasku_panel_h: crate::DEFAULT_TASKU_PANEL_H,
            ext_plugins,
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
    pub fn reorder(&mut self, from: usize, to: usize) {
        let len = self.plugin_order.len();
        if from >= len { return; }
        let effective_to = if to > from { to - 1 } else { to };
        if effective_to >= len { return; }
        if from == effective_to { return; }
        let item = self.plugin_order.remove(from);
        self.plugin_order.insert(effective_to, item);
    }

    /// Produce ordered `(id, title, subtitle, icon, plugin_index)` tuples for the
    /// Slint plugin model. `plugin_index` is -1 for built-ins; for external plugins
    /// it is the index into `self.ext_plugins`.
    pub fn ordered_items(&self) -> Vec<(String, String, String, String, i32)> {
        self.plugin_order.iter().filter_map(|id| match id.as_str() {
            "tasku" => Some((
                "tasku".to_string(),
                "TASKU".to_string(),
                match &self.tasku_path {
                    Some(p) => p.clone(),
                    None    => "not found — install tasku".to_string(),
                },
                "\u{F0AE}".to_string(),  // nf-fa-tasks
                -1i32,
            )),
            "priorities" => Some((
                "priorities".to_string(),
                "PRIORITIES".to_string(),
                "drag to rank".to_string(),
                "\u{F005}".to_string(),  // nf-fa-star
                -1i32,
            )),
            "workspaces" => Some((
                "workspaces".to_string(),
                "WORKSPACES".to_string(),
                "terminal layouts".to_string(),
                "\u{EB20}".to_string(),  // nf-cod-dashboard
                -1i32,
            )),
            id => {
                // Look up in ext_plugins by id
                if let Some(pos) = self.ext_plugins.iter().position(|p| p.id == id) {
                    let p = &self.ext_plugins[pos];
                    let icon = if p.icon.is_empty() {
                        "\u{F489}".to_string()  // nf-fa-terminal (default)
                    } else {
                        p.icon.clone()
                    };
                    Some((
                        p.id.clone(),
                        p.id.to_uppercase(),
                        p.command.clone(),
                        icon,
                        pos as i32,
                    ))
                } else {
                    None
                }
            }
        }).collect()
    }
}
