// ─── Workspace / project data ─────────────────────────────────────────────────
//
// Fetches project records from the local Tasku SQLite database via the CLI.
// Called once at startup; result is pushed to the Slint WorkspaceProject model.
//
// Also owns workspace persistence: save/load the pane-tree snapshot for each
// project to ~/.config/mado/workspaces/<code>.json.

use serde::{Deserialize, Serialize};
use slint::Color;
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

fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("mado")
}

fn workspace_dir() -> PathBuf {
    config_dir().join("workspaces")
}

// ── Tasku fields preference ────────────────────────────────────────────────────
//
// Saved as ~/.config/mado/tasku_fields — a plain comma-separated string
// of field names passed to `tasku list --fields`.
// Default: "name,project,status"

pub const DEFAULT_TASKU_FIELDS: &str = "name,project,status";

#[allow(dead_code)]
pub fn save_tasku_fields(fields: &str) {
    let dir = config_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("mado: could not create config dir: {e}");
        return;
    }
    if let Err(e) = fs::write(dir.join("tasku_fields"), fields) {
        eprintln!("mado: could not save tasku fields: {e}");
    }
}

pub fn load_tasku_fields() -> String {
    fs::read_to_string(config_dir().join("tasku_fields"))
        .unwrap_or_else(|_| DEFAULT_TASKU_FIELDS.to_string())
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

    let out = Command::new(tasku_path)
        .args(["sql", sql])
        .output();

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
