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
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".config")
        .join("mado")
        .join("config.toml")
}
