use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct PluginConfig {
    pub id:      String,
    pub command: String,
    /// "pixel" for native UI plugins using the RGBA frame protocol.
    /// Anything else (including the default "") is treated as a PTY terminal.
    pub kind:    String,
    /// Nerd Font codepoint string for the sidebar header icon.
    /// Defaults to the terminal icon if empty.
    pub icon:    String,
    /// Panel position: "left" (default), "right", or "top".
    pub position: String,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Keybindings {
    pub split_right:      String,
    pub split_down:       String,
    pub close_pane:       String,
    pub focus_left:       String,
    pub focus_right:      String,
    pub focus_up:         String,
    pub focus_down:       String,
    pub focus_left_alt:   String,
    pub focus_right_alt:  String,
    pub focus_up_alt:     String,
    pub focus_down_alt:   String,
}

impl Default for Keybindings {
    fn default() -> Self {
        Keybindings {
            split_right:     "cmd+d".into(),
            split_down:      "cmd+shift+d".into(),
            close_pane:      "cmd+w".into(),
            focus_left:      "cmd+opt+h".into(),
            focus_right:     "cmd+opt+l".into(),
            focus_up:        "cmd+opt+k".into(),
            focus_down:      "cmd+opt+j".into(),
            focus_left_alt:  "cmd+opt+left".into(),
            focus_right_alt: "cmd+opt+right".into(),
            focus_up_alt:    "cmd+opt+up".into(),
            focus_down_alt:  "cmd+opt+down".into(),
        }
    }
}

/// Parsed keybinding: which modifiers + key text to match.
#[derive(Clone, Default)]
#[allow(dead_code)]
pub struct Shortcut {
    pub meta:  bool,
    pub ctrl:  bool,
    pub alt:   bool,
    pub key:   String,  // lowercase text, e.g. "d", "w", "\u{F702}"
}

#[allow(dead_code)]
impl Shortcut {
    pub fn matches(&self, text: &str, ctrl: bool, meta: bool, alt: bool) -> bool {
        self.meta == meta && self.ctrl == ctrl && self.alt == alt
            && text.to_lowercase() == self.key
    }
}

#[allow(dead_code)]
pub fn parse_shortcut(s: &str) -> Shortcut {
    let mut sc = Shortcut::default();
    let parts: Vec<&str> = s.split('+').collect();
    for (i, part) in parts.iter().enumerate() {
        match *part {
            "cmd" | "meta" | "super" => sc.meta = true,
            "ctrl"                   => sc.ctrl = true,
            "opt" | "alt"            => sc.alt  = true,
            "shift"                  => {} // shift changes the key char (e.g. D vs d)
            key if i == parts.len() - 1 => {
                sc.key = match key {
                    "left"  => "\u{F702}".into(),
                    "right" => "\u{F703}".into(),
                    "up"    => "\u{F700}".into(),
                    "down"  => "\u{F701}".into(),
                    other   => other.to_lowercase(),
                };
            }
            _ => {}
        }
    }
    sc
}

/// All shortcuts, pre-parsed and ready to match against key events.
#[allow(dead_code)]
pub struct ParsedKeybindings {
    pub split_right:     Shortcut,
    pub split_down:      Shortcut,
    pub close_pane:      Shortcut,
    pub focus_left:      Shortcut,
    pub focus_right:     Shortcut,
    pub focus_up:        Shortcut,
    pub focus_down:      Shortcut,
    pub focus_left_alt:  Shortcut,
    pub focus_right_alt: Shortcut,
    pub focus_up_alt:    Shortcut,
    pub focus_down_alt:  Shortcut,
}

#[allow(dead_code)]
impl ParsedKeybindings {
    pub fn from(kb: &Keybindings) -> Self {
        // split_down is "cmd+shift+d" → the shifted character is uppercase "D"
        let mut split_down = parse_shortcut(&kb.split_down);
        if kb.split_down.contains("shift") {
            split_down.key = split_down.key.to_uppercase();
        }
        ParsedKeybindings {
            split_right:     parse_shortcut(&kb.split_right),
            split_down,
            close_pane:      parse_shortcut(&kb.close_pane),
            focus_left:      parse_shortcut(&kb.focus_left),
            focus_right:     parse_shortcut(&kb.focus_right),
            focus_up:        parse_shortcut(&kb.focus_up),
            focus_down:      parse_shortcut(&kb.focus_down),
            focus_left_alt:  parse_shortcut(&kb.focus_left_alt),
            focus_right_alt: parse_shortcut(&kb.focus_right_alt),
            focus_up_alt:    parse_shortcut(&kb.focus_up_alt),
            focus_down_alt:  parse_shortcut(&kb.focus_down_alt),
        }
    }
}

#[derive(Deserialize)]
#[serde(default)]
pub struct Config {
    /// Font size in points. Default: 18.0
    pub font_size: f32,
    /// Terminal font family name. Empty string uses the bundled Hack Nerd Font Mono.
    /// Must be installed on the system (e.g. "Anka/Coder", "JetBrains Mono").
    pub font_family: String,
    /// Shell to launch. Defaults to $SHELL, then /bin/sh.
    pub shell: Option<String>,
    /// Sidebar width in logical pixels. Default: 300.0
    pub sidebar_width: f32,
    /// UI theme name. Must match a bundled Tailwind palette name or a file in
    /// ~/.config/mado/themes/<name>.toml. Default: "slate".
    pub theme: String,
    /// Editor to use for `mado config`. Overrides $VISUAL/$EDITOR. Default: "".
    pub editor: String,
    /// If true, Mado writes ~/.config/mado/starship.toml with theme colours and
    /// sets STARSHIP_CONFIG so all spawned shells use it. Default: false.
    pub theme_starship: bool,
    /// External sidebar plugins. Each spawns as a PTY panel in the sidebar.
    pub plugins: Vec<PluginConfig>,
    /// Disable the built-in Tasku panel (still installed, just hidden). Default: false.
    pub disable_tasku: bool,
    /// Disable the built-in Priorities panel. Default: false.
    pub disable_priorities: bool,
    /// Disable the built-in Workspaces panel. Default: false.
    pub disable_workspaces: bool,
    /// Where the built-in Tasku panel lives: "left", "right", "top". Default: "top".
    pub tasku_position: String,
    /// Right sidebar width in logical pixels. Default: 300.0
    pub right_sidebar_width: f32,
    /// Top bar height in logical pixels. Default: 200.0
    pub top_bar_height: f32,
    /// Show the bottom task-runner bar. Default: false.
    pub show_bottom_bar: bool,
    /// Keyboard shortcuts.
    pub keybindings: Keybindings,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            font_size: 18.0,
            font_family: String::new(),
            shell: None,
            sidebar_width: 300.0,
            theme: "gray".into(),
            editor: String::new(),
            theme_starship: false,
            plugins: vec![],
            disable_tasku: false,
            disable_priorities: false,
            disable_workspaces: false,
            tasku_position: "top".into(),
            right_sidebar_width: 300.0,
            top_bar_height: 200.0,
            show_bottom_bar: false,
            keybindings: Keybindings::default(),
        }
    }
}

impl Config {
    /// Load from ~/.config/mado/config.toml. Missing file → silent defaults.
    /// Parse errors → print warning and use defaults.
    pub fn load() -> Self {
        let path = config_path();
        let contents = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return Config::default(),
        };
        match toml::from_str(&contents) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("mado: config error in {}: {e}", path.display());
                Config::default()
            }
        }
    }

    /// Resolved shell: config value → $SHELL env → /bin/sh
    pub fn resolved_shell(&self) -> String {
        self.shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/sh".into())
    }

    /// Whether the top bar should be visible at launch.
    /// True when tasku is positioned in the top bar, or top-bar plugins exist.
    pub fn should_show_top_bar(&self, has_top_plugins: bool) -> bool {
        self.tasku_position == "top" || has_top_plugins
    }

    /// Whether the bottom bar should be visible at launch.
    pub fn should_show_bottom_bar(&self) -> bool {
        self.show_bottom_bar
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_shortcut() ──────────────────────────────────────────────────────

    #[test]
    fn parse_cmd_d() {
        let sc = parse_shortcut("cmd+d");
        assert!(sc.meta);
        assert!(!sc.ctrl);
        assert!(!sc.alt);
        assert_eq!(sc.key, "d");
    }

    #[test]
    fn parse_cmd_shift_d_uppercases_key() {
        let sc = parse_shortcut("cmd+shift+d");
        assert!(sc.meta);
        assert_eq!(sc.key, "d"); // shift changes the char, not the shortcut key
    }

    #[test]
    fn parse_cmd_opt_left_arrow() {
        let sc = parse_shortcut("cmd+opt+left");
        assert!(sc.meta);
        assert!(sc.alt);
        assert_eq!(sc.key, "\u{F702}");
    }

    #[test]
    fn parse_ctrl_modifier() {
        let sc = parse_shortcut("ctrl+c");
        assert!(sc.ctrl);
        assert!(!sc.meta);
        assert_eq!(sc.key, "c");
    }

    #[test]
    fn parse_meta_alias_super() {
        let sc = parse_shortcut("super+w");
        assert!(sc.meta);
    }

    #[test]
    fn parse_arrow_keys_map_to_unicode() {
        assert_eq!(parse_shortcut("cmd+up").key,    "\u{F700}");
        assert_eq!(parse_shortcut("cmd+down").key,  "\u{F701}");
        assert_eq!(parse_shortcut("cmd+right").key, "\u{F703}");
    }

    // ── Shortcut::matches() ───────────────────────────────────────────────────

    #[test]
    fn matches_correct_combination() {
        let sc = parse_shortcut("cmd+d");
        assert!(sc.matches("d", false, true, false));
    }

    #[test]
    fn does_not_match_wrong_modifier() {
        let sc = parse_shortcut("cmd+d");
        assert!(!sc.matches("d", true, false, false)); // ctrl instead of meta
    }

    #[test]
    fn does_not_match_wrong_key() {
        let sc = parse_shortcut("cmd+d");
        assert!(!sc.matches("w", false, true, false));
    }

    #[test]
    fn matches_case_insensitive() {
        let sc = parse_shortcut("cmd+d");
        assert!(sc.matches("D", false, true, false));
    }

    // ── should_show_top_bar() ─────────────────────────────────────────────────

    fn cfg_top(pos: &str) -> Config {
        Config { tasku_position: pos.into(), ..Config::default() }
    }

    #[test]
    fn top_bar_shown_when_tasku_in_top() {
        assert!(cfg_top("top").should_show_top_bar(false));
    }

    #[test]
    fn top_bar_shown_when_top_plugins_exist() {
        assert!(cfg_top("left").should_show_top_bar(true));
    }

    #[test]
    fn top_bar_shown_when_both_true() {
        assert!(cfg_top("top").should_show_top_bar(true));
    }

    #[test]
    fn top_bar_hidden_when_tasku_left_and_no_top_plugins() {
        assert!(!cfg_top("left").should_show_top_bar(false));
    }

    #[test]
    fn top_bar_hidden_when_tasku_right_and_no_top_plugins() {
        assert!(!cfg_top("right").should_show_top_bar(false));
    }

    #[test]
    fn top_bar_default_position_is_top_so_shows() {
        // Default tasku_position is "top" → top bar visible with no plugins
        assert!(Config::default().should_show_top_bar(false));
    }

    // ── should_show_bottom_bar() ──────────────────────────────────────────────

    #[test]
    fn bottom_bar_hidden_by_default() {
        assert!(!Config::default().should_show_bottom_bar());
    }

    #[test]
    fn bottom_bar_shown_when_config_true() {
        let cfg = Config { show_bottom_bar: true, ..Config::default() };
        assert!(cfg.should_show_bottom_bar());
    }

    #[test]
    fn bottom_bar_hidden_when_config_false() {
        let cfg = Config { show_bottom_bar: false, ..Config::default() };
        assert!(!cfg.should_show_bottom_bar());
    }
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".config")
        .join("mado")
        .join("config.toml")
}
