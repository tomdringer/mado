use serde::Deserialize;
use std::path::PathBuf;

// ── TOML schema ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ThemeUi {
    window_bg:        String,
    terminal_area_bg: String,
    pane_bg:          String,
    pane_toolbar:     String,
    focus_border:     String,
    active_dot:       String,
    card_bg:          String,
    card_border:      String,
    divider_active:   String,
    divider_inactive: String,
    text_primary:     String,
    text_muted:       String,
}

#[derive(Deserialize)]
struct ThemeFile {
    ui: ThemeUi,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Resolved UI colour roles, stored as `[R, G, B]` bytes.
pub struct Theme {
    pub window_bg:        [u8; 3],
    pub terminal_area_bg: [u8; 3],
    pub pane_bg:          [u8; 3],
    pub pane_toolbar:     [u8; 3],
    pub focus_border:     [u8; 3],
    pub active_dot:       [u8; 3],
    pub card_bg:          [u8; 3],
    pub card_border:      [u8; 3],
    pub divider_active:   [u8; 3],
    pub divider_inactive: [u8; 3],
    pub text_primary:     [u8; 3],
    pub text_muted:       [u8; 3],
}

impl Theme {
    /// Load a theme by name. Resolution order:
    ///  1. `~/.config/mado/themes/<name>.toml`  (user override)
    ///  2. Bundled theme matching `name`
    ///  3. Bundled slate (safe fallback)
    pub fn load(name: &str) -> Self {
        load_user(name)
            .or_else(|| load_bundled(name))
            .or_else(|| load_bundled("slate"))
            .unwrap_or_else(|| {
                eprintln!("mado: theme '{name}' not found — using built-in slate defaults");
                slate_hardcoded()
            })
    }
}

// ── Loaders ───────────────────────────────────────────────────────────────────

fn load_user(name: &str) -> Option<Theme> {
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home)
        .join(".config")
        .join("mado")
        .join("themes")
        .join(format!("{name}.toml"));
    let s = std::fs::read_to_string(path).ok()?;
    parse(&s)
}

fn load_bundled(name: &str) -> Option<Theme> {
    let s: &str = match name {
        "amber"   => include_str!("../themes/amber.toml"),
        "blue"    => include_str!("../themes/blue.toml"),
        "cyan"    => include_str!("../themes/cyan.toml"),
        "emerald" => include_str!("../themes/emerald.toml"),
        "fuchsia" => include_str!("../themes/fuchsia.toml"),
        "gray"    => include_str!("../themes/gray.toml"),
        "green"   => include_str!("../themes/green.toml"),
        "indigo"  => include_str!("../themes/indigo.toml"),
        "lime"    => include_str!("../themes/lime.toml"),
        "neutral" => include_str!("../themes/neutral.toml"),
        "orange"  => include_str!("../themes/orange.toml"),
        "pink"    => include_str!("../themes/pink.toml"),
        "purple"  => include_str!("../themes/purple.toml"),
        "red"     => include_str!("../themes/red.toml"),
        "rose"    => include_str!("../themes/rose.toml"),
        "sky"     => include_str!("../themes/sky.toml"),
        "slate"   => include_str!("../themes/slate.toml"),
        "stone"   => include_str!("../themes/stone.toml"),
        "teal"    => include_str!("../themes/teal.toml"),
        "violet"  => include_str!("../themes/violet.toml"),
        "yellow"  => include_str!("../themes/yellow.toml"),
        "zinc"    => include_str!("../themes/zinc.toml"),
        _ => return None,
    };
    parse(s)
}

fn parse(s: &str) -> Option<Theme> {
    let file: ThemeFile = toml::from_str(s)
        .map_err(|e| eprintln!("mado: theme parse error: {e}"))
        .ok()?;
    let u = &file.ui;
    Some(Theme {
        window_bg:        hex3(&u.window_bg)?,
        terminal_area_bg: hex3(&u.terminal_area_bg)?,
        pane_bg:          hex3(&u.pane_bg)?,
        pane_toolbar:     hex3(&u.pane_toolbar)?,
        focus_border:     hex3(&u.focus_border)?,
        active_dot:       hex3(&u.active_dot)?,
        card_bg:          hex3(&u.card_bg)?,
        card_border:      hex3(&u.card_border)?,
        divider_active:   hex3(&u.divider_active)?,
        divider_inactive: hex3(&u.divider_inactive)?,
        text_primary:     hex3(&u.text_primary)?,
        text_muted:       hex3(&u.text_muted)?,
    })
}

fn hex3(s: &str) -> Option<[u8; 3]> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 { return None; }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

/// Hard-coded slate values — used only if the bundled TOML itself fails to parse.
fn slate_hardcoded() -> Theme {
    Theme {
        window_bg:        [0x0f, 0x17, 0x2a],
        terminal_area_bg: [0x02, 0x06, 0x17],
        pane_bg:          [0x0f, 0x17, 0x2a],
        pane_toolbar:     [0x1e, 0x29, 0x3b],
        focus_border:     [0x7a, 0xa2, 0xf7],
        active_dot:       [0x22, 0xc5, 0x5e],
        card_bg:          [0x1e, 0x29, 0x3b],
        card_border:      [0x33, 0x41, 0x55],
        divider_active:   [0x88, 0x88, 0xcc],
        divider_inactive: [0x3a, 0x3a, 0x55],
        text_primary:     [0xf1, 0xf5, 0xf9],
        text_muted:       [0x64, 0x74, 0x8b],
    }
}
