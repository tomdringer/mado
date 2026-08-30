// ─── Sidebar plugin system ────────────────────────────────────────────────────
//
// Built-in panels: Tasku, Priorities, Workspaces (can be disabled in config).
// External panels: any binary listed under [[plugins]] in config.toml —
//   spawned as a PTY, sized to the panel, receives SIGWINCH on resize.

// ─── Sidebar runtime state ────────────────────────────────────────────────────

pub struct SidebarState {
    /// Plugin IDs in the user's current display order — LEFT sidebar only.
    pub plugin_order: Vec<String>,
    /// Detected Tasku binary path, or None if not installed.
    pub tasku_path: Option<String>,
    /// Current left sidebar width in logical pixels.
    pub width: f32,
    /// Current right sidebar width in logical pixels.
    pub right_width: f32,
    /// Current top bar height in logical pixels.
    pub top_bar_h: f32,
    /// Current Tasku panel height in logical pixels (left sidebar case).
    pub tasku_panel_h: f32,
    /// External plugins in left sidebar (position "" or "left").
    pub left_ext_plugins: Vec<crate::config::PluginConfig>,
    /// External plugins in right sidebar (position "right").
    pub right_ext_plugins: Vec<crate::config::PluginConfig>,
    /// External plugins in top bar (position "top").
    pub top_ext_plugins: Vec<crate::config::PluginConfig>,
    /// Where Tasku lives: "left", "right", or "top".
    pub tasku_position: String,
    /// Where Priorities lives (always "left" for now).
    #[allow(dead_code)]
    pub priorities_position: String,
    /// Where Workspaces lives (always "left" for now).
    #[allow(dead_code)]
    pub workspaces_position: String,
}

impl SidebarState {
    pub fn new(
        width: f32,
        right_width: f32,
        top_bar_h: f32,
        tasku_path: Option<String>,
        left_ext_plugins: Vec<crate::config::PluginConfig>,
        right_ext_plugins: Vec<crate::config::PluginConfig>,
        top_ext_plugins: Vec<crate::config::PluginConfig>,
        disable_tasku: bool,
        disable_priorities: bool,
        disable_workspaces: bool,
        tasku_position: String,
    ) -> Self {
        // Build left sidebar order: tasku (if left), priorities, workspaces, left ext plugins.
        let mut order: Vec<String> = Vec::new();
        if !disable_tasku && (tasku_position.is_empty() || tasku_position == "left") {
            order.push("tasku".to_string());
        }
        if !disable_priorities { order.push("priorities".to_string()); }
        if !disable_workspaces { order.push("workspaces".to_string()); }
        for p in &left_ext_plugins { order.push(p.id.clone()); }

        Self {
            plugin_order: order,
            tasku_path,
            width,
            right_width,
            top_bar_h,
            tasku_panel_h: crate::DEFAULT_TASKU_PANEL_H,
            left_ext_plugins,
            right_ext_plugins,
            top_ext_plugins,
            tasku_position,
            priorities_position: "left".to_string(),
            workspaces_position: "left".to_string(),
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

    /// Produce ordered `(id, title, subtitle, icon, plugin_index)` tuples for
    /// the LEFT sidebar Slint plugin model.
    /// `plugin_index` is -1 for built-ins; for external plugins it is the index
    /// into `self.left_ext_plugins`.
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
                if let Some(pos) = self.left_ext_plugins.iter().position(|p| p.id == id) {
                    let p = &self.left_ext_plugins[pos];
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

    /// Produce items for the RIGHT sidebar (external plugins only).
    /// `plugin_index` is the index into `self.right_ext_plugins`.
    pub fn right_items(&self) -> Vec<(String, String, String, String, i32)> {
        self.right_ext_plugins.iter().enumerate().map(|(i, p)| {
            let icon = if p.icon.is_empty() {
                "\u{F489}".to_string()
            } else {
                p.icon.clone()
            };
            (
                p.id.clone(),
                p.id.to_uppercase(),
                p.command.clone(),
                icon,
                i as i32,
            )
        }).collect()
    }

    /// Produce items for the TOP bar.
    /// Tasku is first (if tasku_position == "top" and tasku_path is Some),
    /// then top_ext_plugins (plugin_index = index into top_ext_plugins).
    pub fn top_items(&self) -> Vec<(String, String, String, String, i32)> {
        let mut items: Vec<(String, String, String, String, i32)> = Vec::new();
        if self.tasku_position == "top" {
            // Only include tasku if it's not disabled (presence in tasku_path indicates enabled).
            // We always include if tasku_position == "top"; caller filters by disable_tasku.
            items.push((
                "tasku".to_string(),
                "TASKU".to_string(),
                match &self.tasku_path {
                    Some(p) => p.clone(),
                    None    => "not found — install tasku".to_string(),
                },
                "\u{F0AE}".to_string(),
                -1i32,
            ));
        }
        for (i, p) in self.top_ext_plugins.iter().enumerate() {
            let icon = if p.icon.is_empty() {
                "\u{F489}".to_string()
            } else {
                p.icon.clone()
            };
            items.push((
                p.id.clone(),
                p.id.to_uppercase(),
                p.command.clone(),
                icon,
                i as i32,
            ));
        }
        items
    }
}
