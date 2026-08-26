use serde::Deserialize;
use std::path::PathBuf;

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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            font_size: 18.0,
            font_family: String::new(),
            shell: None,
            sidebar_width: 300.0,
            theme: "slate".into(),
            editor: String::new(),
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
