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
    welcome_border:   Option<String>,
    welcome_muted:    Option<String>,
    welcome_dim:      Option<String>,
}

#[derive(Deserialize, Default)]
struct ThemeStarship {
    color_fg0:    Option<String>, // text on all segments
    color_bg1:    Option<String>, // time segment (darkest)
    color_bg3:    Option<String>, // docker/conda segment
    color_blue:   Option<String>, // language segment
    color_aqua:   Option<String>, // git segment
    color_yellow: Option<String>, // directory segment
    color_orange: Option<String>, // os/user segment (brightest)
    color_green:  Option<String>, // success prompt char
    color_purple: Option<String>, // vim replace mode
}

#[derive(Deserialize)]
struct ThemeFile {
    ui: ThemeUi,
    #[serde(default)]
    starship: ThemeStarship,
}

/// Starship powerline palette — the same role names as the gruvbox preset so the
/// user's format string and segment definitions don't need to change.
pub struct StarshipPalette {
    pub color_fg0:    [u8; 3],
    pub color_bg1:    [u8; 3],
    pub color_bg3:    [u8; 3],
    pub color_blue:   [u8; 3],
    pub color_aqua:   [u8; 3],
    pub color_yellow: [u8; 3],
    pub color_orange: [u8; 3],
    pub color_green:  [u8; 3],
    pub color_purple: [u8; 3],
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
    pub starship: StarshipPalette,
    /// Welcome banner accent (box border + logo). Defaults to a dark red.
    pub welcome_border:   [u8; 3],
    /// Welcome banner subtitle text. Defaults to text_muted.
    pub welcome_muted:    [u8; 3],
    /// Welcome banner hint text (dimmer). Defaults to one step darker than text_muted.
    pub welcome_dim:      [u8; 3],
}

impl Theme {
    /// Load a theme by name. Resolution order:
    ///  1. `~/.config/mado/themes/<name>.toml`  (user override)
    ///  2. Bundled theme matching `name`
    ///  3. Bundled slate (safe fallback)
    pub fn load(name: &str) -> Self {
        load_user(name)
            .or_else(|| load_bundled(name))
            .or_else(|| load_bundled("gray"))
            .unwrap_or_else(|| {
                eprintln!("mado: theme '{name}' not found — using built-in defaults");
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
    let focus_border    = hex3(&u.focus_border)?;
    let text_muted      = hex3(&u.text_muted)?;
    let card_border     = hex3(&u.card_border)?;
    let text_primary    = hex3(&u.text_primary)?;
    let terminal_area_bg = hex3(&u.terminal_area_bg)?;
    let window_bg       = hex3(&u.window_bg)?;
    let pane_toolbar    = hex3(&u.pane_toolbar)?;
    let divider_inactive = hex3(&u.divider_inactive)?;
    let active_dot      = hex3(&u.active_dot)?;

    let s  = &file.starship;
    let or = |opt: &Option<String>, default: [u8; 3]| -> [u8; 3] {
        opt.as_deref().and_then(hex3).unwrap_or(default)
    };

    Some(Theme {
        window_bg,
        terminal_area_bg,
        pane_bg:          hex3(&u.pane_bg)?,
        pane_toolbar,
        focus_border,
        active_dot,
        card_bg:          hex3(&u.card_bg)?,
        card_border,
        divider_active:   hex3(&u.divider_active)?,
        divider_inactive,
        text_primary,
        text_muted,
        starship: StarshipPalette {
            color_fg0:    or(&s.color_fg0,    text_primary),
            color_bg1:    or(&s.color_bg1,    terminal_area_bg),
            color_bg3:    or(&s.color_bg3,    window_bg),
            color_blue:   or(&s.color_blue,   pane_toolbar),
            color_aqua:   or(&s.color_aqua,   divider_inactive),
            color_yellow: or(&s.color_yellow, text_muted),
            color_orange: or(&s.color_orange, focus_border),
            color_green:  or(&s.color_green,  active_dot),
            color_purple: or(&s.color_purple, focus_border),
        },
        // Welcome banner: explicit overrides, otherwise inherit from theme accent colours.
        welcome_border:   u.welcome_border.as_deref().and_then(hex3).unwrap_or(focus_border),
        welcome_muted:    u.welcome_muted.as_deref().and_then(hex3).unwrap_or(text_muted),
        welcome_dim:      u.welcome_dim.as_deref().and_then(hex3).unwrap_or(text_muted),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── hex3() ────────────────────────────────────────────────────────────────

    #[test]
    fn hex3_parses_with_hash() {
        assert_eq!(hex3("#1a2b3c"), Some([0x1a, 0x2b, 0x3c]));
    }

    #[test]
    fn hex3_parses_without_hash() {
        assert_eq!(hex3("ffffff"), Some([0xff, 0xff, 0xff]));
    }

    #[test]
    fn hex3_parses_all_zeros() {
        assert_eq!(hex3("#000000"), Some([0, 0, 0]));
    }

    #[test]
    fn hex3_rejects_wrong_length() {
        assert_eq!(hex3("#abc"), None);
        assert_eq!(hex3("#abcdefg"), None);
        assert_eq!(hex3(""), None);
    }

    #[test]
    fn hex3_rejects_invalid_hex() {
        assert_eq!(hex3("#gggggg"), None);
        assert_eq!(hex3("#ZZZZZZ"), None);
    }

    #[test]
    fn hex3_case_insensitive() {
        assert_eq!(hex3("#AABBCC"), Some([0xAA, 0xBB, 0xCC]));
        assert_eq!(hex3("#aabbcc"), Some([0xAA, 0xBB, 0xCC]));
    }

    // ── load_bundled() ────────────────────────────────────────────────────────

    #[test]
    fn bundled_slate_loads() {
        let t = load_bundled("slate");
        assert!(t.is_some());
    }

    #[test]
    fn bundled_indigo_loads() {
        let t = load_bundled("indigo");
        assert!(t.is_some());
    }

    #[test]
    fn bundled_unknown_returns_none() {
        assert!(load_bundled("notatheme").is_none());
    }

    #[test]
    fn all_bundled_themes_parse() {
        let names = ["amber","blue","cyan","emerald","fuchsia","gray","green",
                     "indigo","lime","neutral","orange","pink","purple","red",
                     "rose","sky","slate","stone","teal","violet","yellow","zinc"];
        for name in names {
            assert!(load_bundled(name).is_some(), "theme '{name}' failed to parse");
        }
    }

    // ── parse() ───────────────────────────────────────────────────────────────

    fn valid_toml(focus_border: &str) -> String {
        format!(
            "[ui]\n\
             window_bg        = \"#0f172a\"\n\
             terminal_area_bg = \"#020617\"\n\
             pane_bg          = \"#0f172a\"\n\
             pane_toolbar     = \"#1e293b\"\n\
             focus_border     = \"{focus_border}\"\n\
             active_dot       = \"#22c55e\"\n\
             card_bg          = \"#1e293b\"\n\
             card_border      = \"#334155\"\n\
             divider_active   = \"#8888cc\"\n\
             divider_inactive = \"#3a3a55\"\n\
             text_primary     = \"#f1f5f9\"\n\
             text_muted       = \"#64748b\"\n"
        )
    }

    #[test]
    fn parse_valid_toml_returns_theme() {
        assert!(parse(&valid_toml("#7aa2f7")).is_some());
    }

    #[test]
    fn parse_invalid_hex_returns_none() {
        let bad = valid_toml("#7aa2f7").replace("#0f172a", "not-a-color");
        assert!(parse(&bad).is_none());
    }

    #[test]
    fn starship_fields_default_to_ui_colours() {
        let toml = valid_toml("#aaaaaa");
        let t = parse(&toml).unwrap();
        // color_orange defaults to focus_border (#aaaaaa)
        assert_eq!(t.starship.color_orange, [0xaa, 0xaa, 0xaa]);
    }
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
        starship: StarshipPalette {
            color_fg0:    [0xf1, 0xf5, 0xf9], // text_primary
            color_bg1:    [0x02, 0x06, 0x17], // terminal_area_bg
            color_bg3:    [0x0f, 0x17, 0x2a], // window_bg
            color_blue:   [0x1e, 0x29, 0x3b], // pane_toolbar
            color_aqua:   [0x3a, 0x3a, 0x55], // divider_inactive
            color_yellow: [0x64, 0x74, 0x8b], // text_muted
            color_orange: [0x7a, 0xa2, 0xf7], // focus_border
            color_green:  [0x22, 0xc5, 0x5e], // active_dot
            color_purple: [0x7a, 0xa2, 0xf7], // focus_border
        },
        welcome_border:   [0x7a, 0xa2, 0xf7], // focus_border (slate blue)
        welcome_muted:    [0x64, 0x74, 0x8b], // text_muted
        welcome_dim:      [0x64, 0x74, 0x8b], // text_muted (same — always readable)
    }
}
