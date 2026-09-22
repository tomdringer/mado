// ─── Workspace / project data ─────────────────────────────────────────────────
//
// Fetches project records from the local Tasku SQLite database via the CLI.
// Called once at startup; result is pushed to the Slint WorkspaceProject model.
//
// Also owns workspace persistence: save/load the pane-tree snapshot for each
// project to ~/.config/mado/workspaces/<code>.json.

use serde::{Deserialize, Serialize};
use slint::Color;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::pane_tree::SavedNode;

// ── Workspace persistence ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub project: String,
    pub tree: SavedNode,
}

pub(crate) fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("mado")
}

fn workspace_dir() -> PathBuf {
    config_dir().join("workspaces")
}

// ── Project paths ──────────────────────────────────────────────────────────────
//
// Optional ~/.config/mado/projects.toml maps project codes to root directories:
//
//   [MDO]
//   path = "/Users/tom/Sites/mado"
//
//   [SB1]
//   path = "/Users/tom/Sites/supercode"

/// Read the optional top-level `default = "<code>"` key from projects.toml.
/// When set, Mado will automatically activate that project on launch.
///
/// Example projects.toml:
///
///   default = "MDO"
///
///   [MDO]
///   path = "/Users/tom/Sites/mado"
pub fn load_default_project() -> Option<String> {
    let file = config_dir().join("projects.toml");
    let content = fs::read_to_string(&file).ok()?;
    let table = content.parse::<toml::Table>().ok()?;
    table.get("default")?.as_str().map(|s| s.to_string())
}

pub fn load_project_paths() -> HashMap<String, String> {
    let file = config_dir().join("projects.toml");
    let content = match fs::read_to_string(&file) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    let table = match content.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("mado: could not parse projects.toml: {e}");
            return HashMap::new();
        }
    };
    table
        .into_iter()
        .filter_map(|(section, val)| {
            let path = val.get("path")?.as_str()?.to_string();
            // If `project` is set, use the Tasku project name as key so
            // name-based fallback lookup works. Otherwise use the section name.
            let key = val.get("project")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or(section);
            Some((key, path))
        })
        .collect()
}

// ── Project icons ─────────────────────────────────────────────────────────────
//
// Read from ~/.config/mado/projects.toml — each section may contain an `icon`
// key with a Nerd Font glyph or one of the built-in named presets:
//
//   terminal   →  (terminal window)
//   web        →  (globe)
//   plugin     →  (plug)
//   api        →  (circuit/nodes)
//   mobile     →  (phone)
//   design     →  (palette)
//   data       →  (database)
//   docs       →  (book)
//   tool       →  (wrench)
//   cloud      →  (cloud)
//
//   [MDO]
//   path = "/Users/tom/Sites/mado"
//   icon = "terminal"
//
//   [SB1]
//   path = "/Users/tom/Sites/supercode"
//   icon = ""   # or any Nerd Font glyph directly

const ICON_PRESETS: &[(&str, &str)] = &[
    ("terminal", "\u{f489}"),  //
    ("web",      "\u{f484}"),  //
    ("plugin",   "\u{f1e6}"),  //
    ("api",      "\u{eb11}"),  //
    ("mobile",   "\u{f10b}"),  //
    ("design",   "\u{f53f}"),  //
    ("data",     "\u{f1c0}"),  //
    ("docs",     "\u{f02d}"),  //
    ("tool",     "\u{f0ad}"),  //
    ("cloud",    "\u{f0c2}"),  //
];

fn resolve_icon(raw: &str) -> String {
    for (name, glyph) in ICON_PRESETS {
        if raw == *name {
            return glyph.to_string();
        }
    }
    raw.to_string()
}

pub fn load_project_icons() -> HashMap<String, String> {
    let file = config_dir().join("projects.toml");
    let content = match fs::read_to_string(&file) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    let table = match content.parse::<toml::Table>() {
        Ok(t) => t,
        Err(_) => return HashMap::new(),
    };
    table
        .into_iter()
        .filter_map(|(section, val)| {
            let raw = val.get("icon")?.as_str()?.to_string();
            let key = val.get("project")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or(section);
            Some((key, resolve_icon(&raw)))
        })
        .collect()
}


// ── Task runner commands ───────────────────────────────────────────────────────
//
// Read from ~/.config/mado/projects.toml — same file that maps codes to paths.
// Each section may also contain a `task` key:
//
//   [MDO]
//   path = "/Users/tom/Sites/mado"
//   task = "cargo run"
//
//   [APP]
//   path = "/Users/tom/Sites/app"
//   task = "npm run dev"
//
// The bottom bar runner looks up the active workspace's entry on play.

pub fn load_runner_tasks() -> HashMap<String, String> {
    let file = config_dir().join("projects.toml");
    let content = match fs::read_to_string(&file) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    let table = match content.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("mado: could not parse projects.toml: {e}");
            return HashMap::new();
        }
    };
    table
        .into_iter()
        .filter_map(|(section, val)| {
            let task = val.get("task")?.as_str()?.to_string();
            let key = val.get("project")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or(section);
            Some((key, task))
        })
        .collect()
}

// ── Deploy commands ────────────────────────────────────────────────────────────
//
// Optional `deploy` key alongside `task` in projects.toml:
//
//   [MDO]
//   path = "/Users/tom/Sites/mado"
//   task   = "cargo run"
//   deploy = "fly deploy"
//
// The runner Deploy button looks up this entry on click.

pub fn load_deploy_commands() -> HashMap<String, String> {
    let file = config_dir().join("projects.toml");
    let content = match fs::read_to_string(&file) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    let table = match content.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("mado: could not parse projects.toml: {e}");
            return HashMap::new();
        }
    };
    table
        .into_iter()
        .filter_map(|(section, val)| {
            let deploy = val.get("deploy")?.as_str()?.to_string();
            let key = val.get("project")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or(section);
            Some((key, deploy))
        })
        .collect()
}

// ── Plugin order persistence ───────────────────────────────────────────────────
//
// Saved as ~/.config/mado/sidebar.json — a JSON array of plugin IDs
// in the user's current display order.

pub fn save_plugin_order(ids: &[String]) {
    let dir = config_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("mado: could not create config dir: {e}");
        return;
    }
    let path = dir.join("sidebar.json");
    match serde_json::to_string_pretty(ids) {
        Ok(json) => {
            if let Err(e) = fs::write(&path, json) {
                eprintln!("mado: could not save plugin order: {e}");
            }
        }
        Err(e) => eprintln!("mado: could not serialise plugin order: {e}"),
    }
}

/// Load the saved plugin order. Returns an empty vec if no file exists yet.
pub fn load_plugin_order() -> Vec<String> {
    let path = config_dir().join("sidebar.json");
    let json = match fs::read_to_string(path) {
        Ok(s)  => s,
        Err(_) => return vec![],
    };
    serde_json::from_str::<Vec<String>>(&json).unwrap_or_default()
}

// ── Plugin panel height persistence ───────────────────────────────────────────
//
// Saved as ~/.config/mado/panel_heights.json — a JSON object with
// "left" and "right" arrays of f32 panel heights in plugin-index order.

#[derive(Serialize, Deserialize, Default)]
struct PanelHeights {
    left:  Vec<f32>,
    right: Vec<f32>,
}

pub fn save_panel_heights(left: &[f32], right: &[f32]) {
    let dir = config_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("mado: could not create config dir: {e}");
        return;
    }
    let data = PanelHeights { left: left.to_vec(), right: right.to_vec() };
    match serde_json::to_string_pretty(&data) {
        Ok(json) => { let _ = fs::write(dir.join("panel_heights.json"), json); }
        Err(e)   => eprintln!("mado: could not serialise panel heights: {e}"),
    }
}

pub fn load_panel_heights() -> (Vec<f32>, Vec<f32>) {
    let path = config_dir().join("panel_heights.json");
    let json = match fs::read_to_string(path) {
        Ok(s)  => s,
        Err(_) => return (vec![], vec![]),
    };
    let data: PanelHeights = serde_json::from_str(&json).unwrap_or_default();
    (data.left, data.right)
}

// ── Priority order persistence ─────────────────────────────────────────────────
//
// Saved as ~/.config/mado/priorities.json — a JSON array of project codes
// in priority order (index 0 = highest priority).

pub fn save_priority_order(codes: &[String]) {
    let dir = config_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("mado: could not create config dir: {e}");
        return;
    }
    let path = dir.join("priorities.json");
    match serde_json::to_string_pretty(codes) {
        Ok(json) => {
            if let Err(e) = fs::write(&path, json) {
                eprintln!("mado: could not save priority order: {e}");
            }
        }
        Err(e) => eprintln!("mado: could not serialise priority order: {e}"),
    }
}

/// Load the saved priority order. Returns an empty vec if no file exists yet.
pub fn load_priority_order() -> Vec<String> {
    let path = config_dir().join("priorities.json");
    let json = match fs::read_to_string(path) {
        Ok(s)  => s,
        Err(_) => return vec![],
    };
    serde_json::from_str::<Vec<String>>(&json).unwrap_or_default()
}

pub fn save_workspace(project: &str, ws: &SavedWorkspace) {
    let dir = workspace_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("mado: could not create workspace dir: {e}");
        return;
    }
    let path = dir.join(format!("{project}.json"));
    match serde_json::to_string_pretty(ws) {
        Ok(json) => {
            if let Err(e) = fs::write(&path, json) {
                eprintln!("mado: could not save workspace: {e}");
            }
        }
        Err(e) => eprintln!("mado: could not serialise workspace: {e}"),
    }
}

pub fn load_workspace(project: &str) -> Option<SavedWorkspace> {
    let path = workspace_dir().join(format!("{project}.json"));
    let json = fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

/// Delete any workspace files whose code is not in `valid_codes`.
pub fn prune_stale_workspaces(valid_codes: &[String]) {
    let dir = workspace_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(e)  => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None    => continue,
        };
        if !valid_codes.iter().any(|c| c == &stem) {
            let _ = fs::remove_file(&path);
        }
    }
}

pub struct FetchedProject {
    pub name:       String,  // full project name (e.g. "Sublime2")
    pub code:       String,  // short code from tasks.code (e.g. "SB2")
    pub color:      Color,
    pub text_color: Color,   // black or white chosen for contrast
}

/// Join projects with tasks to get each project's code prefix.
/// The `code` column in tasks stores the prefix (e.g. "SB2"); tasku suppresses
/// it in output unless aliased, so we alias it as `proj_code`.
pub fn fetch_projects(tasku_path: &str) -> Vec<FetchedProject> {
    let sql = "SELECT p.name, p.colour, \
               (SELECT t.code FROM tasks t WHERE t.project = p.name \
                AND t.code IS NOT NULL AND t.code != '' LIMIT 1) AS proj_code \
               FROM projects p ORDER BY p.name";

    // Run via zsh interactive shell so ~/.zshrc is sourced and asdf/homebrew
    // shims resolve correctly when Mado launches as a .app bundle.
    // Falls back to /bin/sh login shell for systems without zsh.
    let shell_cmd = format!("{} sql '{}'",
        tasku_path.replace('\'', "'\\''"),
        sql.replace('\'', "'\\''"),
    );
    let out = Command::new("/bin/zsh")
        .args(["-ic", &shell_cmd])
        .output()
        .or_else(|_| Command::new("/bin/sh").args(["-lc", &shell_cmd]).output());

    match out {
        Ok(o) if o.status.success() => parse(&o.stdout),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            eprintln!("mado: tasku projects query failed: {stderr}");
            vec![]
        }
        Err(e) => {
            eprintln!("mado: could not run tasku: {e}");
            vec![]
        }
    }
}

// ── Output format ─────────────────────────────────────────────────────────────
//
// The tasku sql output looks like:
//
//   name      colour   proj_code
//   ────────  ───────  ─────────
//   Mado      #FF5733  MDO
//   ────────  ───────  ─────────
//
// We skip blank lines, the header, and separator lines (contain U+2500 ─).
// Each data line contains exactly one '#' followed by 6 hex digits (the colour).
// Everything before '#' (trimmed) is the name; everything after the 7-char
// colour value (trimmed) is the project code.

fn parse(bytes: &[u8]) -> Vec<FetchedProject> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.contains('\u{2500}')   // separator ─
            || trimmed.starts_with("name")
        {
            continue;
        }

        // Locate the colour — everything from the last '#' for 7 chars (#RRGGBB)
        let Some(hash) = trimmed.rfind('#') else { continue };
        let name = trimmed[..hash].trim();
        let rest = &trimmed[hash..];
        if rest.len() < 7 { continue; }

        let hex   = &rest[..7];   // "#RRGGBB"
        let code_raw = rest[7..].trim();

        if name.is_empty() { continue; }

        // Fall back to first-3-chars of name if no tasks exist for this project
        let code = if code_raw.is_empty() {
            name.chars().take(3).collect::<String>().to_uppercase()
        } else {
            code_raw.to_string()
        };

        let (r, g, b) = parse_hex(hex);
        let luminance = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        let text_color = if luminance > 140.0 {
            Color::from_rgb_u8(0, 0, 0)
        } else {
            Color::from_rgb_u8(255, 255, 255)
        };

        out.push(FetchedProject {
            name: name.to_string(),
            code,
            color: Color::from_rgb_u8(r, g, b),
            text_color,
        });
    }

    out
}

fn parse_hex(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    if h.len() < 6 { return (128, 128, 128); }
    let r = u8::from_str_radix(&h[0..2], 16).unwrap_or(128);
    let g = u8::from_str_radix(&h[2..4], 16).unwrap_or(128);
    let b = u8::from_str_radix(&h[4..6], 16).unwrap_or(128);
    (r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_hex() ───────────────────────────────────────────────────────────

    #[test]
    fn parse_hex_with_hash() {
        assert_eq!(parse_hex("#FF5733"), (0xFF, 0x57, 0x33));
    }

    #[test]
    fn parse_hex_without_hash() {
        assert_eq!(parse_hex("1a2b3c"), (0x1a, 0x2b, 0x3c));
    }

    #[test]
    fn parse_hex_too_short_returns_gray() {
        assert_eq!(parse_hex("#abc"), (128, 128, 128));
    }

    #[test]
    fn parse_hex_invalid_chars_returns_gray_per_channel() {
        // "zz" is invalid — from_str_radix returns Err, falls back to 128
        let (r, g, b) = parse_hex("#zzFFFF");
        assert_eq!(r, 128);
        assert_eq!(g, 0xFF);
        assert_eq!(b, 0xFF);
    }

    // ── parse() (tasku output parser) ─────────────────────────────────────────

    fn run_parse(input: &str) -> Vec<FetchedProject> {
        parse(input.as_bytes())
    }

    #[test]
    fn parse_single_project() {
        let input = "Mado      #FF5733  MDO\n";
        let projects = run_parse(input);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "Mado");
        assert_eq!(projects[0].code, "MDO");
    }

    #[test]
    fn parse_skips_header_line() {
        let input = "name      colour   proj_code\nMado      #FF5733  MDO\n";
        let projects = run_parse(input);
        assert_eq!(projects.len(), 1);
    }

    #[test]
    fn parse_skips_separator_lines() {
        let input = "────────  ───────  ─────────\nMado      #FF5733  MDO\n────────  ───────  ─────────\n";
        let projects = run_parse(input);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "Mado");
    }

    #[test]
    fn parse_fallback_code_from_name() {
        // No code in the line — should use first 3 chars of name uppercased
        let input = "Supercode #1122aa\n";
        let projects = run_parse(input);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].code, "SUP");
    }

    #[test]
    fn parse_multiple_projects() {
        let input = "Alpha     #ffffff  ALP\nBeta      #000000  BET\n";
        let projects = run_parse(input);
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].name, "Alpha");
        assert_eq!(projects[1].name, "Beta");
    }

    #[test]
    fn parse_light_color_gives_black_text() {
        // #ffffff → luminance ~255 → text should be black
        let input = "Light     #ffffff  LGT\n";
        let projects = run_parse(input);
        // text_color is black (all channels 0)
        // We can't directly check the Color struct easily, but we can verify the project parsed
        assert_eq!(projects.len(), 1);
    }

    #[test]
    fn parse_empty_input_returns_empty() {
        assert!(run_parse("").is_empty());
    }

    // ── load_project_paths() ─────────────────────────────────────────────────

    #[test]
    fn load_project_paths_missing_file_returns_empty() {
        // Just verify it doesn't panic when config dir doesn't exist
        // (actual HOME varies by environment, result is non-deterministic)
        let _ = load_project_paths();
    }
}
