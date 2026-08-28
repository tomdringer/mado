mod config;
mod pane_tree;
mod pixel_plugin;
mod sidebar;
mod tasku;
mod terminal;
mod theme;
mod workspace;


use pane_tree::PaneTree;

use std::cell::RefCell;
use std::rc::Rc;
use std::collections::HashMap;

use config::Config;
use pane_tree::{FlatDividerData, NodeId, SplitDir};
use pixel_plugin::PixelPlugin;
use sidebar::SidebarState;
use tasku::detect as detect_tasku;
use terminal::TerminalRegistry;
use slint::{Image, Model, ModelRc, Timer, TimerMode, VecModel};

// Special NodeIds for sidebar terminals — must not collide with pane IDs.
// Pane IDs are allocated sequentially from the PaneTree counter; u32::MAX
// is a safe sentinel. External plugin IDs count down from u32::MAX - 1.
const SIDEBAR_TASKU_ID: NodeId = u32::MAX;

/// NodeId for external plugin at index `i`.
fn plugin_node_id(i: usize) -> NodeId { u32::MAX - 1 - i as u32 }

// Default panel heights (match sidebar.slint initial values).
pub const DEFAULT_TASKU_PANEL_H:  f32 = 400.0;
pub const DEFAULT_PLUGIN_PANEL_H: f32 = 300.0;

// Space consumed by toolbar + margins (must match pane_view.slint).
// 5px top gap + 28px toolbar = 33px before terminal image.
// 5px left + 5px right = 10px narrower than pane width.
const PANE_TOP_INSET: f32 = 28.0;
const PANE_H_INSET:   f32 = 40.0;

// Fixed (non-terminal) height of the Tasku panel.
// Breakdown (matches sidebar.slint TaskuContent VerticalLayout):
//   padding-top:           8 px
//   4 button rows × 32 px: 128 px
//   3 inner gaps  ×  4 px:  12 px
//   grid → terminal gap:    6 px
//   padding-bottom:         8 px  (covered by resize strip)
//   ──────────────────────────
//   total fixed:           162 px
const TASKU_PANEL_FIXED_H: f32 = 162.0;

// Fixed height of external plugin panel: top+bottom padding (8 px each) + resize handle (8 px).
const PLUGIN_PANEL_FIXED_H: f32 = 16.0;

/// Terminal rect height inside the Tasku panel.
fn tasku_terminal_h(panel_h: f32) -> f32 {
    (panel_h - TASKU_PANEL_FIXED_H).max(50.0)
}

/// Terminal rect height inside an external plugin panel.
fn plugin_terminal_h(panel_h: f32) -> f32 {
    (panel_h - PLUGIN_PANEL_FIXED_H).max(50.0)
}

slint::include_modules!();

use i_slint_backend_winit::WinitWindowAccessor;

// ── Selection state ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct Selection {
    pane_id: NodeId,
    /// Where the mouse was pressed (col, display_row).
    anchor: (usize, usize),
    /// Current drag end (col, display_row).
    head: (usize, usize),
}

impl Selection {
    /// Return a normalized ((start_col, start_row), (end_col, end_row)) where
    /// start is always above-or-equal to end in display space.
    fn normalized(&self) -> ((usize, usize), (usize, usize)) {
        let (ac, ar) = self.anchor;
        let (hc, hr) = self.head;
        if ar < hr || (ar == hr && ac <= hc) {
            ((ac, ar), (hc, hr))
        } else {
            ((hc, hr), (ac, ar))
        }
    }

    fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// Convert a pane-local logical-pixel position to a (col, row) cell coordinate.
fn px_to_cell(x: f32, y: f32, reg: &TerminalRegistry) -> (usize, usize) {
    // Terminal image starts at (20px, 28px) inside the pane — matches pane_view.slint.
    const IMG_X: f32 = 20.0;
    const IMG_Y: f32 = 28.0;
    let col = (((x - IMG_X).max(0.0) * reg.scale) / reg.font.cell_w as f32) as usize;
    let row = (((y - IMG_Y).max(0.0) * reg.scale) / reg.font.cell_h as f32) as usize;
    (col, row)
}

// ── Drag state ───────────────────────────────────────────────────────────────

#[derive(Clone)]
struct DragState {
    split_id: u32,
    start_ratio: f32,
    start_x: f32,
    start_y: f32,
    split_size: f32,
    is_vertical: bool,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn rgb([r, g, b]: [u8; 3]) -> slint::Color {
    slint::Color::from_rgb_u8(r, g, b)
}

fn apply_theme(ui: &MainWindow, t: &theme::Theme) {
    ui.set_theme_window_bg(rgb(t.window_bg));
    ui.set_theme_terminal_area_bg(rgb(t.terminal_area_bg));
    ui.set_theme_pane_bg(rgb(t.pane_bg));
    ui.set_theme_pane_toolbar(rgb(t.pane_toolbar));
    ui.set_theme_focus_border(rgb(t.focus_border));
    ui.set_theme_active_dot(rgb(t.active_dot));
    ui.set_theme_card_bg(rgb(t.card_bg));
    ui.set_theme_card_border(rgb(t.card_border));
    ui.set_theme_divider_active(rgb(t.divider_active));
    ui.set_theme_divider_inactive(rgb(t.divider_inactive));
    ui.set_theme_text_primary(rgb(t.text_primary));
    ui.set_theme_text_muted(rgb(t.text_muted));
}

fn color_from_u32(argb: u32) -> slint::Color {
    let a = ((argb >> 24) & 0xFF) as u8;
    let r = ((argb >> 16) & 0xFF) as u8;
    let g = ((argb >> 8)  & 0xFF) as u8;
    let b = (argb & 0xFF) as u8;
    slint::Color::from_argb_u8(a, r, g, b)
}

fn make_panes(
    tree: &PaneTree,
    w: f32,
    h: f32,
    images: &HashMap<NodeId, Image>,
    focused_id: Option<NodeId>,
) -> Vec<FlatPane> {
    tree.flatten(w, h).into_iter().map(|p| FlatPane {
        id: p.id as i32,
        x: p.x,
        y: p.y,
        width: p.width,
        height: p.height,
        bg_color: color_from_u32(p.color),
        is_closable: p.is_closable,
        terminal_image: images.get(&p.id).cloned().unwrap_or_default(),
        is_focused: focused_id == Some(p.id),
    }).collect()
}

fn make_dividers(dividers: &[FlatDividerData], active_id: Option<u32>) -> Vec<FlatDivider> {
    dividers.iter().map(|d| FlatDivider {
        split_id: d.split_id as i32,
        is_vertical: d.dir == SplitDir::Vertical,
        x: d.x,
        y: d.y,
        width: d.width,
        height: d.height,
        active: active_id == Some(d.split_id),
    }).collect()
}

fn hit_test(x: f32, y: f32, dividers: &[FlatDividerData]) -> Option<usize> {
    dividers.iter().position(|d| {
        x >= d.x && x <= d.x + d.width && y >= d.y && y <= d.y + d.height
    })
}

/// Full layout + image push — used when pane geometry changes (split, close, resize).
fn full_push(
    ui: &MainWindow,
    tree: &PaneTree,
    dividers_cache: &RefCell<Vec<FlatDividerData>>,
    pane_model: &VecModel<FlatPane>,
    div_model: &VecModel<FlatDivider>,
    images: &HashMap<NodeId, Image>,
    focused_id: Option<NodeId>,
    active_div: Option<u32>,
) {
    let w = ui.get_window_w();
    let h = ui.get_window_h();

    pane_model.set_vec(make_panes(tree, w, h, images, focused_id));

    let dividers = tree.flatten_dividers(w, h);
    *dividers_cache.borrow_mut() = dividers.clone();
    div_model.set_vec(make_dividers(&dividers, active_div));
}

/// Image-only update — uses set_row_data so PaneView component instances (and their
/// FocusScopes) are never recreated. Only touches rows for panes in `dirty_ids` or
/// whose focus state changed, leaving all other rows completely untouched.
fn push_images(
    pane_model: &VecModel<FlatPane>,
    images: &HashMap<NodeId, Image>,
    focused_id: Option<NodeId>,
    dirty_ids: Option<&[NodeId]>, // None = update focus state on all rows
) {
    for row in 0..pane_model.row_count() {
        if let Some(mut pane) = pane_model.row_data(row) {
            let id = pane.id as NodeId;
            let new_focused = focused_id == Some(id);
            let image_dirty = dirty_ids.map_or(false, |ids| ids.contains(&id));
            let focus_changed = pane.is_focused != new_focused;

            if image_dirty || focus_changed {
                if image_dirty {
                    pane.terminal_image = images.get(&id).cloned().unwrap_or_default();
                }
                pane.is_focused = new_focused;
                pane_model.set_row_data(row, pane);
            }
        }
    }
}

fn do_zoom(
    ui: &MainWindow,
    new_size: f32,
    font_size: &RefCell<f32>,
    registry: &RefCell<TerminalRegistry>,
    tree: &RefCell<PaneTree>,
    pane_model: &VecModel<FlatPane>,
    images: &RefCell<HashMap<NodeId, Image>>,
    focused_id: &RefCell<Option<NodeId>>,
) {
    *font_size.borrow_mut() = new_size;
    let mut reg = registry.borrow_mut();
    reg.set_font_size(new_size);
    let w = ui.get_window_w();
    let h = ui.get_window_h();
    let panes = tree.borrow().flatten(w, h);
    for p in &panes {
        reg.resize(p.id, (p.width - PANE_H_INSET).max(10.0), (p.height - PANE_TOP_INSET).max(10.0));
    }
    drop(reg);
    // Clear stale images rendered at the old font size. Use set_row_data (not
    // set_vec) so PaneView component instances survive — FocusScope keeps focus.
    images.borrow_mut().clear();
    push_images(pane_model, &images.borrow(), *focused_id.borrow(), None);
}

// ── First-launch welcome ──────────────────────────────────────────────────────

/// Write ~/.config/mado/starship.toml with theme colours and point STARSHIP_CONFIG at it.
/// Reads the user's existing ~/.config/starship.toml, strips its palette block, injects a
/// Mado-themed palette with the same colour role names, so format/segments are unchanged.
fn apply_starship_theme(t: &theme::Theme) {
    let home = match std::env::var("HOME") { Ok(h) => h, Err(_) => return };
    let dir      = std::path::PathBuf::from(&home).join(".config").join("mado");
    let out_path = dir.join("starship.toml");
    let _ = std::fs::create_dir_all(&dir);

    // Read the user's config (fall back to empty string — Starship will use defaults).
    let user_path = std::path::PathBuf::from(&home).join(".config").join("starship.toml");
    let source = std::fs::read_to_string(&user_path).unwrap_or_default();

    // Discover the active palette name (single or double quoted).
    let palette_name = source.lines()
        .find(|l| l.trim_start().starts_with("palette") && l.contains('='))
        .and_then(|l| {
            let after = l.splitn(2, '=').nth(1)?.trim();
            let q = after.chars().next()?;
            if q == '\'' || q == '"' {
                after[1..].split(q).next().map(str::to_string)
            } else { None }
        })
        .unwrap_or_default();

    let hex = |c: [u8; 3]| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
    let sp = &t.starship;

    // Strip existing palette declaration + block from user config.
    let mut out = String::new();
    let mut skip = false;
    let old_block_header = if palette_name.is_empty() {
        String::new()
    } else {
        format!("[palettes.{}]", palette_name)
    };

    for line in source.lines() {
        let trimmed = line.trim();
        // Skip the "palette = '...'" line (we inject our own at the top).
        if trimmed.starts_with("palette") && trimmed.contains('=')
            && !trimmed.starts_with("[palettes")
        {
            continue;
        }
        // Start skipping when we hit the old palette block.
        if !old_block_header.is_empty() && trimmed == old_block_header {
            skip = true;
            continue;
        }
        // Stop skipping at the next top-level section.
        if skip && trimmed.starts_with('[') {
            skip = false;
        }
        if !skip {
            out.push_str(line);
            out.push('\n');
        }
    }

    // palette = 'mado' must appear before any module config so Starship finds it.
    // [palettes.mado] goes at the END so its keys don't absorb the user's config.
    let palette_block = format!(
        "\n# Managed by Mado — edit your theme to change colours\n\
         # Disable: set theme_starship = false in ~/.config/mado/config.toml\n\
         [palettes.mado]\n\
         color_fg0    = '{fg0}'   # text on every segment\n\
         color_bg1    = '{bg1}'   # time segment (darkest)\n\
         color_bg3    = '{bg3}'   # docker/conda segment\n\
         color_blue   = '{blue}'  # language segment\n\
         color_aqua   = '{aqua}'  # git segment\n\
         color_yellow = '{yellow}'# directory segment\n\
         color_orange = '{orange}'# os/user segment (brightest)\n\
         color_green  = '{green}' # success prompt\n\
         color_red    = '#CC241D' # error prompt (kept semantic)\n\
         color_purple = '{purple}'# vim replace mode\n",
        fg0    = hex(sp.color_fg0),
        bg1    = hex(sp.color_bg1),
        bg3    = hex(sp.color_bg3),
        blue   = hex(sp.color_blue),
        aqua   = hex(sp.color_aqua),
        yellow = hex(sp.color_yellow),
        orange = hex(sp.color_orange),
        green  = hex(sp.color_green),
        purple = hex(sp.color_purple),
    );

    let final_toml = format!("palette = 'mado'\n{out}{palette_block}");

    match std::fs::write(&out_path, &final_toml) {
        Ok(_) => {
            eprintln!("mado: starship theme written to {}", out_path.display());
            // Safe: single-threaded at this point (called before any threads are spawned).
            unsafe { std::env::set_var("STARSHIP_CONFIG", &out_path); }
        }
        Err(e) => eprintln!("mado: failed to write starship theme: {e}"),
    }
}

/// Build the welcome banner as raw bytes ready to inject into a TerminalState.
/// `cols` is the terminal width so the block can be centred.
fn welcome_banner(cols: usize, t: &theme::Theme) -> Vec<u8> {
    let [br, bg, bb] = t.welcome_border;
    let [mr, mg, mb] = t.welcome_muted;
    let [dr, dg, db] = t.welcome_dim;
    let red   = format!("\x1b[38;2;{br};{bg};{bb}m");
    let muted = format!("\x1b[38;2;{mr};{mg};{mb}m");
    let dim   = format!("\x1b[38;2;{dr};{dg};{db}m");
    let red   = red.as_str();
    let muted = muted.as_str();
    let dim   = dim.as_str();
    let reset = "\x1b[0m";

    // Logo lines are exactly 36 visible chars wide.
    // Box layout: ║ + space + 36 content + space + ║ = 40 total.
    const CONTENT_W: usize = 36;
    const BOX_W:     usize = CONTENT_W + 4; // 2 border + 2 padding chars
    let pad = " ".repeat((cols.saturating_sub(BOX_W)) / 2);

    // Helper: pad a content line to CONTENT_W then close with the right border.
    // Total per row: ║(1) + sp(1) + content + padding + sp(1) + ║(1) = BOX_W.
    let row = |line: &str, visible_len: usize| -> String {
        let spaces = CONTENT_W.saturating_sub(visible_len);
        format!("{pad}{red}║{reset} {line}{} {red}║{reset}\r\n",
                " ".repeat(spaces))
    };

    let inner  = BOX_W - 2; // space between the two border chars
    let top    = format!("{pad}{red}╔{}╗{reset}\r\n", "═".repeat(inner));
    let spacer = format!("{pad}{red}║{}║{reset}\r\n", " ".repeat(inner));
    let bot    = format!("{pad}{red}╚{}╝{reset}\r\n", "═".repeat(inner));

    // Logo lines (each exactly 37 visible chars — trailing space makes 38).
    let logo = [
        "███╗   ███╗ █████╗ ██████╗  ██████╗ ",
        "████╗ ████║██╔══██╗██╔══██╗██╔═══██╗",
        "██╔████╔██║███████║██║  ██║██║   ██║",
        "██║╚██╔╝██║██╔══██║██║  ██║██║   ██║",
        "██║ ╚═╝ ██║██║  ██║██████╔╝╚██████╔╝",
        "╚═╝     ╚═╝╚═╝  ╚═╝╚═════╝  ╚═════╝ ",
    ];
    let logo_rows: String = logo.iter()
        .map(|l| row(&format!("{red}{l}{reset}"), l.chars().count()))
        .collect();

    let subtitle_text = "terminal multiplexer";
    let subtitle_pad  = (CONTENT_W.saturating_sub(subtitle_text.len())) / 2;
    let subtitle = row(
        &format!("{}{muted}{subtitle_text}{reset}", " ".repeat(subtitle_pad)),
        subtitle_pad + subtitle_text.len(),
    );

    let h1t = "mado config";
    let h1d = "open your config file";
    let h2t = "mado projects";
    let h2d = "manage project paths";
    let hint1 = row(&format!("{muted}{h1t}{reset}  {dim}{h1d}{reset}"),
                    h1t.len() + 2 + h1d.len());
    let hint2 = row(&format!("{muted}{h2t}{reset}  {dim}{h2d}{reset}"),
                    h2t.len() + 2 + h2d.len());

    let s = format!("\r\n{top}{spacer}{logo_rows}{spacer}{subtitle}{spacer}{hint1}{hint2}{spacer}{bot}\r\n");
    s.into_bytes()
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    // Handle CLI subcommands before launching the UI.
    // macOS injects a `-psn_XXXXXXXX` Process Serial Number arg when launching
    // binaries inside .app bundles — strip it so subcommand matching works.
    let args: Vec<String> = std::env::args()
        .filter(|a| !a.starts_with("-psn_"))
        .collect();
    if args.get(1).map(|s| s.as_str()) == Some("config") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());

        // `mado config <plugin-id>` → open ~/.config/mado/plugins/<id>.toml
        let config_path = if let Some(plugin_id) = args.get(2) {
            std::path::PathBuf::from(&home)
                .join(".config/mado/plugins")
                .join(format!("{plugin_id}.toml"))
        } else {
            std::path::PathBuf::from(&home)
                .join(".config")
                .join("mado")
                .join("config.toml")
        };

        // Create the file with commented defaults if it doesn't exist yet
        // (only for the main config; plugin configs are created empty).
        if !config_path.exists() && args.get(2).is_none() {
            if let Some(parent) = config_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let defaults = concat!(
                "# Mado configuration\n",
                "# font_size    = 18.0\n",
                "# font_family  = \"\"     # empty = bundled Hack Nerd Font Mono\n",
                "# shell        = \"\"     # empty = $SHELL → /bin/sh\n",
                "# sidebar_width = 300.0\n",
                "# theme        = \"gray\"\n",
                "# editor       = \"\"     # e.g. \"nvim\", \"nano\" — overrides $VISUAL/$EDITOR\n",
                "# theme_starship = false  # true = Mado themes your Starship prompt to match\n",
                "\n",
                "# Disable built-in sidebar panels:\n",
                "# disable_tasku      = false\n",
                "# disable_priorities = false\n",
                "# disable_workspaces = false\n",
                "\n",
                "# External sidebar plugins (any binary that renders ANSI to stdout):\n",
                "# [[plugins]]\n",
                "# id      = \"clock\"       # label shown in the sidebar\n",
                "# command = \"mado-clock\"  # binary on $PATH or absolute path\n",
            );
            let _ = std::fs::write(&config_path, defaults);
        }

        let cfg = Config::load();
        let editor = if !cfg.editor.is_empty() {
            cfg.editor.clone()
        } else {
            std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".into())
        };

        std::process::Command::new(&editor)
            .arg(&config_path)
            .status()
            .unwrap_or_else(|e| {
                eprintln!("mado: could not launch editor '{editor}': {e}");
                std::process::exit(1);
            });
        return;
    }

    if args.get(1).map(|s| s.as_str()) == Some("projects") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let projects_path = std::path::PathBuf::from(&home)
            .join(".config")
            .join("mado")
            .join("projects.toml");

        if !projects_path.exists() {
            if let Some(parent) = projects_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let defaults = concat!(
                "# Mado project paths\n",
                "# Map a Tasku project code to its root directory.\n",
                "#\n",
                "# [paths]\n",
                "# MYAPP = \"/Users/you/Sites/myapp\"\n",
                "# WORK  = \"/Users/you/work/project\"\n",
            );
            let _ = std::fs::write(&projects_path, defaults);
        }

        let cfg = Config::load();
        let editor = if !cfg.editor.is_empty() {
            cfg.editor.clone()
        } else {
            std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".into())
        };

        std::process::Command::new(&editor)
            .arg(&projects_path)
            .status()
            .unwrap_or_else(|e| {
                eprintln!("mado: could not launch editor '{editor}': {e}");
                std::process::exit(1);
            });
        return;
    }

    // ── mado plugin <install|add|remove|list> ───────────────────────────────
    if args.get(1).map(|s| s.as_str()) == Some("plugin") {
        match args.get(2).map(|s| s.as_str()) {
            Some("install") => {
                let name = match args.get(3) {
                    Some(s) => s.as_str(),
                    None => { eprintln!("usage: mado plugin install <name|org/repo>"); std::process::exit(1); }
                };
                plugin_install(name);
            }
            Some("add") => {
                let id = match args.get(3) {
                    Some(s) => s.as_str(),
                    None => { eprintln!("usage: mado plugin add <id> <command>"); std::process::exit(1); }
                };
                let command = match args.get(4) {
                    Some(s) => s.as_str(),
                    None => { eprintln!("usage: mado plugin add <id> <command>"); std::process::exit(1); }
                };
                plugin_add(id, command);
            }
            Some("remove") => {
                let id = match args.get(3) {
                    Some(s) => s.as_str(),
                    None => { eprintln!("usage: mado plugin remove <id>"); std::process::exit(1); }
                };
                plugin_remove(id);
            }
            Some("list") => {
                plugin_list();
            }
            Some("update") => {
                let name = match args.get(3) {
                    Some(s) => s.as_str(),
                    None => { eprintln!("usage: mado plugin update <name|org/repo>"); std::process::exit(1); }
                };
                plugin_update(name);
            }
            _ => {
                eprintln!("usage: mado plugin <install|add|remove|list|update>");
                std::process::exit(1);
            }
        }
        return;
    }

    let ui = MainWindow::new().unwrap();

    // Make the window resizable and enable the macOS full-screen green button.
    ui.window().with_winit_window(move |w| {
        w.set_resizable(true);

        #[cfg(target_os = "macos")]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = w.window_handle() {
                if let RawWindowHandle::AppKit(h) = handle.as_raw() {
                    const FULL_SCREEN_PRIMARY: u64 = 1 << 7;
                    unsafe {
                        use objc2::runtime::AnyObject;
                        let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
                        let ns_window: *mut AnyObject =
                            objc2::msg_send![ns_view, window];
                        let _: () = objc2::msg_send![
                            ns_window, setCollectionBehavior: FULL_SCREEN_PRIMARY
                        ];
                    }
                }
            }
        }
    });

    // Load config first — font size, shell, theme, etc.
    let config = Config::load();
    let loaded_theme = Rc::new(theme::Theme::load(&config.theme));
    apply_theme(&ui, &loaded_theme);
    if config.theme_starship {
        apply_starship_theme(&loaded_theme);
    }
    let default_font_size = config.font_size;
    let shell = config.resolved_shell();

    // Get device pixel ratio once at startup for HiDPI-correct rendering
    let scale = ui.window().scale_factor();
    eprintln!("mado: display scale factor = {scale}");

    let tree = Rc::new(RefCell::new(PaneTree::new()));
    let registry = Rc::new(RefCell::new(TerminalRegistry::new(default_font_size, scale, shell, config.font_family.clone())));

    // ── Sidebar + Tasku detection ────────────────────────────────────────────
    let tasku_path = detect_tasku().map(|t| t.path.to_string_lossy().into_owned());
    if let Some(ref p) = tasku_path {
        println!("mado: tasku found at {p}");
    } else {
        println!("mado: tasku not found");
    }

    let ext_plugins = config.plugins.clone();
    let num_ext_plugins = ext_plugins.len();
    let sidebar = Rc::new(RefCell::new(SidebarState::new(
        config.sidebar_width,
        tasku_path,
        ext_plugins,
        config.disable_tasku,
        config.disable_priorities,
        config.disable_workspaces,
    )));
    {
        let saved_order = workspace::load_plugin_order();
        if !saved_order.is_empty() {
            sidebar.borrow_mut().apply_order(&saved_order);
        }
    }
    ui.set_sidebar_width(config.sidebar_width);

    let tasku_fields = Rc::new(workspace::load_tasku_fields());

    // ── Workspace + priority projects ────────────────────────────────────────
    // Fetch projects once at startup.  Build:
    //   • code_to_name  – used by Tasku commands for --project filtering
    //   • ws_model      – WorkspaceProject tiles in the Workspaces panel
    //   • prio_model    – PriorityProject rows in the Priorities panel,
    //                     ordered by the saved priority order (disk) with any
    //                     new projects appended at the end.
    let fetched: Vec<workspace::FetchedProject> = {
        let s = sidebar.borrow();
        match &s.tasku_path {
            Some(p) => workspace::fetch_projects(p),
            None    => vec![],
        }
    };

    let code_to_name: Rc<HashMap<String, String>> = Rc::new(
        fetched.iter().map(|fp| (fp.code.clone(), fp.name.clone())).collect()
    );

    // Project root paths from ~/.config/mado/projects.toml
    let project_paths: Rc<HashMap<String, String>> = Rc::new(workspace::load_project_paths());

    // Workspace tiles (code + colour only)
    {
        let ws_projects: Vec<WorkspaceProject> = fetched.iter().map(|fp| WorkspaceProject {
            code:       fp.code.clone().into(),
            color:      fp.color,
            text_color: fp.text_color,
        }).collect();
        let ws_model = Rc::new(VecModel::<WorkspaceProject>::from(ws_projects));
        ui.set_ws_projects(ModelRc::new(ws_model));
    }

    // Priority list — apply saved order, append unknown projects at the end
    let prio_model: Rc<VecModel<PriorityProject>> = {
        let saved_order = workspace::load_priority_order();
        let mut remaining: Vec<&workspace::FetchedProject> = fetched.iter().collect();
        let mut ordered: Vec<&workspace::FetchedProject> = Vec::new();
        for code in &saved_order {
            if let Some(pos) = remaining.iter().position(|fp| &fp.code == code) {
                ordered.push(remaining.remove(pos));
            }
        }
        ordered.extend(remaining);

        let items: Vec<PriorityProject> = ordered.into_iter().map(|fp| PriorityProject {
            name:  fp.name.clone().into(),
            code:  fp.code.clone().into(),
            color: fp.color,
        }).collect();
        Rc::new(VecModel::<PriorityProject>::from(items))
    };
    ui.set_priority_projects(ModelRc::new(Rc::clone(&prio_model)));

    // Push initial plugin list to Slint
    {
        let items: Vec<PluginItem> = sidebar.borrow().ordered_items().into_iter().map(
            |(id, title, subtitle, icon, plugin_index)| PluginItem {
                id:           id.into(),
                title:        title.into(),
                subtitle:     subtitle.into(),
                icon:         icon.into(),
                plugin_index,
            }
        ).collect();
        let plugin_model = Rc::new(VecModel::<PluginItem>::from(items));
        ui.set_plugins(ModelRc::new(Rc::clone(&plugin_model)));
    }

    // ── External plugin state models ─────────────────────────────────────────
    let plugin_expanded_model: Rc<VecModel<bool>> =
        Rc::new(VecModel::from(vec![false; num_ext_plugins]));
    let plugin_panel_h_model: Rc<VecModel<f32>> =
        Rc::new(VecModel::from(vec![DEFAULT_PLUGIN_PANEL_H; num_ext_plugins]));
    let plugin_images_model: Rc<VecModel<Image>> =
        Rc::new(VecModel::from(vec![Image::default(); num_ext_plugins]));
    let plugin_pixel_model: Rc<VecModel<bool>> = Rc::new(VecModel::from(
        config.plugins.iter().map(|p| p.kind == "pixel").collect::<Vec<_>>()
    ));

    ui.set_plugin_expanded(ModelRc::new(Rc::clone(&plugin_expanded_model)));
    ui.set_plugin_panel_h(ModelRc::new(Rc::clone(&plugin_panel_h_model)));
    ui.set_plugin_images(ModelRc::new(Rc::clone(&plugin_images_model)));
    ui.set_plugin_pixel(ModelRc::new(Rc::clone(&plugin_pixel_model)));

    // index → PixelPlugin for plugins with kind = "pixel"
    let pixel_plugins: Rc<RefCell<std::collections::HashMap<usize, PixelPlugin>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));

    let pane_model = Rc::new(VecModel::<FlatPane>::from(vec![]));
    let div_model  = Rc::new(VecModel::<FlatDivider>::from(vec![]));
    ui.set_panes(ModelRc::new(Rc::clone(&pane_model)));
    ui.set_dividers(ModelRc::new(Rc::clone(&div_model)));

    let dividers_cache: Rc<RefCell<Vec<FlatDividerData>>> = Rc::new(RefCell::new(vec![]));
    let drag: Rc<RefCell<Option<DragState>>> = Rc::new(RefCell::new(None));

    // Image cache — id → most recent slint::Image (at physical pixel resolution)
    let images: Rc<RefCell<HashMap<NodeId, Image>>> = Rc::new(RefCell::new(HashMap::new()));
    let focused_id: Rc<RefCell<Option<NodeId>>> = Rc::new(RefCell::new(None));
    let font_size: Rc<RefCell<f32>> = Rc::new(RefCell::new(default_font_size));

    // ── Active workspace tracking ────────────────────────────────────────────
    let active_project: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // ── Text selection (for copy/paste) ──────────────────────────────────────
    let selection: Rc<RefCell<Option<Selection>>> = Rc::new(RefCell::new(None));

    // Initial terminal is spawned lazily inside on_window_resized so we have
    // real PTY dimensions and can prepend the welcome banner before the shell prompt.
    let initial_spawned: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));
    *focused_id.borrow_mut() = Some(tree.borrow().root);

    // ── 60fps render timer ──────────────────────────────────────────────────
    // Uses set_row_data so PaneView instances (and their FocusScopes) are never
    // recreated — keyboard focus survives across frame updates.
    // Sidebar terminals are routed separately via their own set_*_terminal_image calls.
    {
        let registry = Rc::clone(&registry);
        let pane_model = Rc::clone(&pane_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let selection = Rc::clone(&selection);
        let plugin_images_model = Rc::clone(&plugin_images_model);
        let pixel_plugins = Rc::clone(&pixel_plugins);
        let ui_weak = ui.as_weak();
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, std::time::Duration::from_millis(16), move || {
            let sel = {
                let s = selection.borrow();
                s.as_ref().map(|s| (s.pane_id, s.normalized()))
            };
            let dirty_bufs = registry.borrow_mut().drain_dirty(sel);

            let mut tasku_buf: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> = None;
            // plugin_index → pixel buffer for external plugins
            let mut plugin_bufs: Vec<(usize, slint::SharedPixelBuffer<slint::Rgba8Pixel>)> = Vec::new();
            let mut pane_dirty_ids: Vec<NodeId> = Vec::new();
            let mut imgs = images.borrow_mut();

            for (id, buf) in dirty_bufs {
                if id == SIDEBAR_TASKU_ID {
                    tasku_buf = Some(buf);
                } else if id < SIDEBAR_TASKU_ID && id > u32::MAX - 1 - num_ext_plugins as u32 {
                    // External plugin NodeId: u32::MAX - 1 - i
                    let i = (u32::MAX - 1 - id) as usize;
                    if i < num_ext_plugins {
                        plugin_bufs.push((i, buf));
                    }
                } else {
                    pane_dirty_ids.push(id);
                    imgs.insert(id, Image::from_rgba8(buf));
                }
            }

            // Drain dirty pixel plugins + check for paste actions
            {
                let mut pp = pixel_plugins.borrow_mut();
                for (i, plugin) in pp.iter_mut() {
                    if plugin.dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        if let Ok(guard) = plugin.image.lock() {
                            if let Some(ref buf) = *guard {
                                plugin_bufs.push((*i, buf.clone()));
                            }
                        }
                    }
                    if plugin.paste_pending.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        if let Ok(out) = std::process::Command::new("pbpaste").output() {
                            let mut data = out.stdout;
                            if data.ends_with(b"\r\n") { data.truncate(data.len() - 2); }
                            else if data.ends_with(b"\n") { data.truncate(data.len() - 1); }
                            if !data.is_empty() {
                                if let Some(id) = *focused_id.borrow() {
                                    registry.borrow_mut().write_key(id, &data);
                                }
                            }
                        }
                    }
                }
            }

            if pane_dirty_ids.is_empty() && tasku_buf.is_none() && plugin_bufs.is_empty() {
                return;
            }

            if !pane_dirty_ids.is_empty() {
                push_images(&pane_model, &imgs, *focused_id.borrow(), Some(&pane_dirty_ids));
            }

            if tasku_buf.is_some() || !plugin_bufs.is_empty() {
                if let Some(ui) = ui_weak.upgrade() {
                    if let Some(buf) = tasku_buf {
                        ui.set_tasku_terminal_image(Image::from_rgba8(buf));
                    }
                    for (i, buf) in plugin_bufs {
                        plugin_images_model.set_row_data(i, Image::from_rgba8(buf));
                    }
                }
            }
        });
        std::mem::forget(timer);
    }

    // ── Overlay: press ──────────────────────────────────────────────────────
    ui.on_overlay_press({
        let drag = Rc::clone(&drag);
        let dividers_cache = Rc::clone(&dividers_cache);
        let div_model = Rc::clone(&div_model);
        let ui_weak = ui.as_weak();
        move |x, y| {
            let dividers = dividers_cache.borrow();
            if let Some(idx) = hit_test(x, y, &dividers) {
                let d = &dividers[idx];
                *drag.borrow_mut() = Some(DragState {
                    split_id: d.split_id,
                    start_ratio: d.current_ratio,
                    start_x: x,
                    start_y: y,
                    split_size: d.split_size,
                    is_vertical: d.dir == SplitDir::Vertical,
                });
                let active_dividers = make_dividers(&dividers, Some(d.split_id));
                div_model.set_vec(active_dividers);
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_is_dragging(true);
                    ui.set_near_v_div(d.dir == SplitDir::Vertical);
                    ui.set_near_h_div(d.dir == SplitDir::Horizontal);
                }
            }
        }
    });

    // ── Overlay: move ───────────────────────────────────────────────────────
    ui.on_overlay_move({
        let drag = Rc::clone(&drag);
        let tree = Rc::clone(&tree);
        let dividers_cache = Rc::clone(&dividers_cache);
        let pane_model = Rc::clone(&pane_model);
        let div_model = Rc::clone(&div_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let registry = Rc::clone(&registry);
        let ui_weak = ui.as_weak();
        move |x, y| {
            let state = drag.borrow().clone();
            if let Some(state) = state {
                let delta = if state.is_vertical { y - state.start_y } else { x - state.start_x };
                let new_ratio = state.start_ratio + delta / state.split_size;
                tree.borrow_mut().set_ratio(state.split_id, new_ratio);
                if let Some(ui) = ui_weak.upgrade() {
                    let w = ui.get_window_w();
                    let h = ui.get_window_h();
                    let panes = tree.borrow().flatten(w, h);
                    let mut reg = registry.borrow_mut();
                    for p in &panes {
                        reg.resize(p.id, (p.width - PANE_H_INSET).max(10.0), (p.height - PANE_TOP_INSET).max(10.0));
                    }
                    drop(reg);
                    full_push(&ui, &tree.borrow(), &dividers_cache, &pane_model, &div_model,
                              &images.borrow(), *focused_id.borrow(), Some(state.split_id));
                }
            }
        }
    });

    // ── Overlay: release ────────────────────────────────────────────────────
    ui.on_overlay_release({
        let drag = Rc::clone(&drag);
        let dividers_cache = Rc::clone(&dividers_cache);
        let div_model = Rc::clone(&div_model);
        let ui_weak = ui.as_weak();
        move || {
            *drag.borrow_mut() = None;
            let dividers = dividers_cache.borrow();
            div_model.set_vec(make_dividers(&dividers, None));
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_is_dragging(false);
                ui.set_near_h_div(false);
                ui.set_near_v_div(false);
            }
        }
    });

    // ── Overlay: hover ──────────────────────────────────────────────────────
    ui.on_overlay_hover({
        let dividers_cache = Rc::clone(&dividers_cache);
        let ui_weak = ui.as_weak();
        move |x, y| {
            if let Some(ui) = ui_weak.upgrade() {
                let dividers = dividers_cache.borrow();
                match hit_test(x, y, &dividers) {
                    Some(idx) => {
                        let d = &dividers[idx];
                        ui.set_near_h_div(d.dir == SplitDir::Horizontal);
                        ui.set_near_v_div(d.dir == SplitDir::Vertical);
                    }
                    None => {
                        ui.set_near_h_div(false);
                        ui.set_near_v_div(false);
                    }
                }
            }
        }
    });

    // ── Split ───────────────────────────────────────────────────────────────
    ui.on_split_pane({
        let tree = Rc::clone(&tree);
        let registry = Rc::clone(&registry);
        let dividers_cache = Rc::clone(&dividers_cache);
        let pane_model = Rc::clone(&pane_model);
        let div_model = Rc::clone(&div_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let ui_weak = ui.as_weak();
        move |id, horizontal| {
            let dir = if horizontal { SplitDir::Horizontal } else { SplitDir::Vertical };
            let result = tree.borrow_mut().split(id as u32, dir);
            if let Some(ui) = ui_weak.upgrade() {
                if let Some((relocated_id, new_id)) = result {
                    let w = ui.get_window_w();
                    let h = ui.get_window_h();
                    let panes = tree.borrow().flatten(w, h);

                    let mut reg = registry.borrow_mut();
                    // Inherit cwd from the pane being split
                    let source_cwd = reg.get_cwd(id as NodeId);
                    if let Some(sess) = reg.sessions.remove(&(id as NodeId)) {
                        reg.sessions.insert(relocated_id, sess);
                    }
                    let new_pane = panes.iter().find(|p| p.id == new_id);
                    if let Some(p) = new_pane {
                        reg.spawn(new_id, p.width, p.height, source_cwd.as_deref());
                    }
                    let relocated_pane = panes.iter().find(|p| p.id == relocated_id);
                    if let Some(p) = relocated_pane {
                        reg.resize(relocated_id, (p.width - PANE_H_INSET).max(10.0), (p.height - PANE_TOP_INSET).max(10.0));
                    }
                    drop(reg);

                    let mut imgs = images.borrow_mut();
                    if let Some(img) = imgs.remove(&(id as NodeId)) {
                        imgs.insert(relocated_id, img);
                    }
                    drop(imgs);

                    *focused_id.borrow_mut() = Some(new_id);
                }
                full_push(&ui, &tree.borrow(), &dividers_cache, &pane_model, &div_model,
                          &images.borrow(), *focused_id.borrow(), None);
            }
        }
    });

    // ── Close ───────────────────────────────────────────────────────────────
    ui.on_close_pane({
        let tree = Rc::clone(&tree);
        let registry = Rc::clone(&registry);
        let dividers_cache = Rc::clone(&dividers_cache);
        let pane_model = Rc::clone(&pane_model);
        let div_model = Rc::clone(&div_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let ui_weak = ui.as_weak();
        move |id| {
            registry.borrow_mut().remove(id as NodeId);
            images.borrow_mut().remove(&(id as NodeId));
            tree.borrow_mut().close(id as u32);

            let remaining = tree.borrow().leaf_ids();
            let focus = remaining.first().copied();
            *focused_id.borrow_mut() = focus;

            if let Some(ui) = ui_weak.upgrade() {
                full_push(&ui, &tree.borrow(), &dividers_cache, &pane_model, &div_model,
                          &images.borrow(), *focused_id.borrow(), None);
            }
        }
    });

    // ── Key input ───────────────────────────────────────────────────────────
    ui.on_pane_key_input({
        let registry = Rc::clone(&registry);
        let focused_id = Rc::clone(&focused_id);
        let font_size = Rc::clone(&font_size);
        let tree = Rc::clone(&tree);
        let pane_model = Rc::clone(&pane_model);
        let images = Rc::clone(&images);
        let selection = Rc::clone(&selection);
        let ui_weak = ui.as_weak();
        move |id, text, ctrl, meta| {
            if *focused_id.borrow() != Some(id as NodeId) { return; }

            let zoom_mod = ctrl || meta;
            let t = text.as_str();

            // Cmd+C: copy selection to clipboard (primary shortcut on macOS).
            // Ctrl+C: smart copy — copies if selection exists, otherwise sends ^C to terminal.
            let is_copy = (meta && t == "c") || (ctrl && t == "c");
            if is_copy {
                let has_sel = {
                    let sel = selection.borrow();
                    sel.as_ref().map_or(false, |s| !s.is_empty() && s.pane_id == id as NodeId)
                };
                if has_sel {
                    let norm = {
                        let sel = selection.borrow();
                        sel.as_ref().unwrap().normalized()
                    };
                    let copied = registry.borrow().get_selection_text(id as NodeId, norm);
                    if !copied.is_empty() {
                        let _ = std::process::Command::new("/bin/sh")
                            .args(["-c", &format!("printf '%s' {} | pbcopy",
                                shell_escape(&copied))])
                            .status();
                    }
                    // Clear selection; do NOT forward the keystroke.
                    let prev_id = selection.borrow().as_ref().map(|s| s.pane_id);
                    *selection.borrow_mut() = None;
                    if let Some(sid) = prev_id {
                        registry.borrow().mark_dirty(sid);
                    }
                    return;
                }
                // No selection + Cmd+C → do nothing. No selection + Ctrl+C → fall through (^C).
                if meta { return; }
            }

            // Clear active selection on regular keystrokes, but NOT when:
            // - a bare modifier key is pressed (ctrl/shift/meta/alt alone)
            // - ctrl or meta is held (user may be mid-chord, e.g. about to press C)
            let is_modifier_only = matches!(t,
                "\u{0010}" | "\u{0015}" | // Shift L/R
                "\u{0011}" | "\u{0016}" | // Control L/R
                "\u{0012}" | "\u{0013}" | // Alt / AltGr
                "\u{0014}" |              // CapsLock
                "\u{0017}" | "\u{0018}" | // Meta L/R
                "\u{0019}"               // Backtab
            );
            if !is_modifier_only && !ctrl && !meta {
                let prev_id = selection.borrow().as_ref().map(|s| s.pane_id);
                *selection.borrow_mut() = None;
                if let Some(sid) = prev_id {
                    registry.borrow().mark_dirty(sid);
                }
            }

            // Ctrl+V / Cmd+V: paste clipboard contents.
            if (ctrl && t == "v") || (meta && t == "v") {
                if let Ok(out) = std::process::Command::new("pbpaste").output() {
                    let mut data = out.stdout;
                    // Strip one trailing newline so paste doesn't auto-execute commands.
                    if data.ends_with(b"\r\n") { data.truncate(data.len() - 2); }
                    else if data.ends_with(b"\n") { data.truncate(data.len() - 1); }
                    if !data.is_empty() {
                        registry.borrow_mut().write_key(id as NodeId, &data);
                    }
                }
                return;
            }

            // Zoom: Ctrl+= / Ctrl++ zoom in, Ctrl+- out, Ctrl+0 reset.
            if zoom_mod && (t == "=" || t == "+") {
                let new_size = (*font_size.borrow() + 1.0).min(40.0);
                if let Some(ui) = ui_weak.upgrade() {
                    do_zoom(&ui, new_size, &font_size, &registry, &tree,
                            &pane_model, &images, &focused_id);
                }
                return;
            }
            if zoom_mod && t == "-" {
                let new_size = (*font_size.borrow() - 1.0).max(7.0);
                if let Some(ui) = ui_weak.upgrade() {
                    do_zoom(&ui, new_size, &font_size, &registry, &tree,
                            &pane_model, &images, &focused_id);
                }
                return;
            }
            if zoom_mod && t == "0" {
                if let Some(ui) = ui_weak.upgrade() {
                    do_zoom(&ui, default_font_size, &font_size, &registry, &tree, &pane_model, &images, &focused_id);
                }
                return;
            }

            let bytes = key_text_to_bytes(&text);
            registry.borrow_mut().write_key(id as NodeId, &bytes);
        }
    });

    // ── Focus request ───────────────────────────────────────────────────────
    ui.on_pane_focus_request({
        let focused_id = Rc::clone(&focused_id);
        let pane_model = Rc::clone(&pane_model);
        let images = Rc::clone(&images);
        move |id| {
            *focused_id.borrow_mut() = Some(id as NodeId);
            push_images(&pane_model, &images.borrow(), Some(id as NodeId), None);
        }
    });

    // ── Pane scroll ──────────────────────────────────────────────────────────
    ui.on_pane_scroll({
        let registry = Rc::clone(&registry);
        move |id, delta| {
            let rows = (delta / 20.0).round() as i32;
            registry.borrow_mut().scroll(id as NodeId, -rows);
        }
    });

    // ── Mouse selection ──────────────────────────────────────────────────────
    ui.on_pane_mouse_pressed({
        let registry = Rc::clone(&registry);
        let selection = Rc::clone(&selection);
        move |pane_id, x, y| {
            let id = pane_id as NodeId;
            let cell = px_to_cell(x, y, &registry.borrow());
            *selection.borrow_mut() = Some(Selection { pane_id: id, anchor: cell, head: cell });
            registry.borrow().mark_dirty(id);
        }
    });

    ui.on_pane_mouse_moved({
        let registry = Rc::clone(&registry);
        let selection = Rc::clone(&selection);
        move |pane_id, x, y| {
            let id = pane_id as NodeId;
            let mut sel = selection.borrow_mut();
            if let Some(ref mut s) = *sel {
                if s.pane_id == id {
                    s.head = px_to_cell(x, y, &registry.borrow());
                    drop(sel);
                    registry.borrow().mark_dirty(id);
                }
            }
        }
    });

    ui.on_pane_mouse_released({
        let registry = Rc::clone(&registry);
        let selection = Rc::clone(&selection);
        move |pane_id| {
            let id = pane_id as NodeId;
            let mut sel = selection.borrow_mut();
            if let Some(ref s) = *sel {
                if s.pane_id == id && s.is_empty() {
                    // Plain click — no drag, clear the zero-width selection.
                    *sel = None;
                    drop(sel);
                    registry.borrow().mark_dirty(id);
                }
            }
        }
    });

    // ── Window resize + initial spawn ────────────────────────────────────────
    ui.on_window_resized({
        let tree = Rc::clone(&tree);
        let registry = Rc::clone(&registry);
        let dividers_cache = Rc::clone(&dividers_cache);
        let pane_model = Rc::clone(&pane_model);
        let div_model = Rc::clone(&div_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let initial_spawned = Rc::clone(&initial_spawned);
        let loaded_theme = Rc::clone(&loaded_theme);
        let ui_weak = ui.as_weak();
        let last_size: Rc<RefCell<(f32, f32)>> = Rc::new(RefCell::new((0.0, 0.0)));
        move |w, h| {
            let (lw, lh) = *last_size.borrow();
            if (w - lw).abs() > 0.5 || (h - lh).abs() > 0.5 {
                *last_size.borrow_mut() = (w, h);
                let panes = tree.borrow().flatten(w, h);
                let mut reg = registry.borrow_mut();

                if !initial_spawned.get() && w > 50.0 && h > 50.0 {
                    initial_spawned.set(true);
                    let root_id = tree.borrow().root;
                    if let Some(p) = panes.iter().find(|p| p.id == root_id) {
                        let pane_w = (p.width  - PANE_H_INSET).max(10.0);
                        let pane_h = (p.height - PANE_TOP_INSET).max(10.0);
                        let cols   = reg.logical_to_cols(pane_w);
                        let banner = welcome_banner(cols, &loaded_theme);
                        reg.spawn_with_banner(root_id, pane_w, pane_h, None, &banner);
                    }
                } else {
                    for p in &panes {
                        reg.resize(p.id, (p.width - PANE_H_INSET).max(10.0), (p.height - PANE_TOP_INSET).max(10.0));
                    }
                }

                drop(reg);
                if let Some(ui) = ui_weak.upgrade() {
                    full_push(&ui, &tree.borrow(), &dividers_cache, &pane_model, &div_model,
                              &images.borrow(), *focused_id.borrow(), None);
                }
            }
        }
    });

    // ── Zoom ─────────────────────────────────────────────────────────────────
    ui.on_zoom_in({
        let font_size = Rc::clone(&font_size);
        let registry = Rc::clone(&registry);
        let tree = Rc::clone(&tree);
        let pane_model = Rc::clone(&pane_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let ui_weak = ui.as_weak();
        move || {
            let new_size = (*font_size.borrow() + 1.0).min(40.0);
            if let Some(ui) = ui_weak.upgrade() {
                do_zoom(&ui, new_size, &font_size, &registry, &tree, &pane_model, &images, &focused_id);
            }
        }
    });

    ui.on_zoom_out({
        let font_size = Rc::clone(&font_size);
        let registry = Rc::clone(&registry);
        let tree = Rc::clone(&tree);
        let pane_model = Rc::clone(&pane_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let ui_weak = ui.as_weak();
        move || {
            let new_size = (*font_size.borrow() - 1.0).max(7.0);
            if let Some(ui) = ui_weak.upgrade() {
                do_zoom(&ui, new_size, &font_size, &registry, &tree, &pane_model, &images, &focused_id);
            }
        }
    });

    ui.on_zoom_reset({
        let font_size = Rc::clone(&font_size);
        let registry = Rc::clone(&registry);
        let tree = Rc::clone(&tree);
        let pane_model = Rc::clone(&pane_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let ui_weak = ui.as_weak();
        move || {
            if let Some(ui) = ui_weak.upgrade() {
                do_zoom(&ui, default_font_size, &font_size, &registry, &tree, &pane_model, &images, &focused_id);
            }
        }
    });

    // ── Plugin reorder ──────────────────────────────────────────────────────
    ui.on_plugin_reordered({
        let sidebar = Rc::clone(&sidebar);
        let ui_weak = ui.as_weak();
        move |from, to| {
            sidebar.borrow_mut().reorder(from as usize, to as usize);
            workspace::save_plugin_order(&sidebar.borrow().plugin_order);
            // Rebuild and push the updated plugin list
            if let Some(ui) = ui_weak.upgrade() {
                let items: Vec<PluginItem> = sidebar.borrow().ordered_items().into_iter().map(
                    |(id, title, subtitle, icon, plugin_index)| PluginItem {
                        id:           id.into(),
                        title:        title.into(),
                        subtitle:     subtitle.into(),
                        icon:         icon.into(),
                        plugin_index,
                    }
                ).collect();
                let plugin_model = Rc::new(VecModel::<PluginItem>::from(items));
                ui.set_plugins(ModelRc::new(Rc::clone(&plugin_model)));
            }
        }
    });

    // ── Priority reorder ─────────────────────────────────────────────────────
    ui.on_priority_reordered({
        let prio_model = Rc::clone(&prio_model);
        move |from, to| {
            let from = from as usize;
            let to   = to   as usize;
            let len  = prio_model.row_count();
            if from >= len { return; }

            // Insert-before semantics: adjust target when moving forward
            let effective_to = if to > from { to - 1 } else { to };
            if effective_to >= len || from == effective_to { return; }

            // Remove the item from its old position and reinsert
            if let Some(item) = prio_model.row_data(from) {
                prio_model.remove(from);
                prio_model.insert(effective_to, item);
            }

            // Persist the new order
            let codes: Vec<String> = (0..prio_model.row_count())
                .filter_map(|i| prio_model.row_data(i).map(|p| p.code.to_string()))
                .collect();
            workspace::save_priority_order(&codes);
        }
    });

    // ── Sidebar width persistence ────────────────────────────────────────────
    ui.on_sidebar_width_changed({
        let sidebar = Rc::clone(&sidebar);
        let registry = Rc::clone(&registry);
        let plugin_expanded_model = Rc::clone(&plugin_expanded_model);
        let plugin_panel_h_model = Rc::clone(&plugin_panel_h_model);
        let pixel_plugins = Rc::clone(&pixel_plugins);
        move |new_width| {
            let tasku_h = {
                let mut s = sidebar.borrow_mut();
                s.width = new_width;
                s.tasku_panel_h
            };
            let sidebar_w = (new_width - 24.0).max(50.0);
            let mut reg = registry.borrow_mut();
            reg.resize(SIDEBAR_TASKU_ID, sidebar_w, tasku_terminal_h(tasku_h));
            // Resize any open external plugins
            let mut pp = pixel_plugins.borrow_mut();
            for i in 0..num_ext_plugins {
                if plugin_expanded_model.row_data(i).unwrap_or(false) {
                    let h = plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H);
                    if let Some(plugin) = pp.get_mut(&i) {
                        plugin.send_resize(
                            (sidebar_w * scale) as u32,
                            (h * scale) as u32,
                        );
                    } else {
                        reg.resize(plugin_node_id(i), sidebar_w, plugin_terminal_h(h));
                    }
                }
            }
        }
    });

    // ── Tasku panel expand/collapse ──────────────────────────────────────────
    ui.on_tasku_toggled({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        let active_project = Rc::clone(&active_project);
        let code_to_name = Rc::clone(&code_to_name);
        let tasku_fields = Rc::clone(&tasku_fields);
        move |expanded| {
            let mut reg = registry.borrow_mut();
            if expanded {
                let (sidebar_w, panel_h) = {
                    let s = sidebar.borrow();
                    ((s.width - 24.0).max(50.0), s.tasku_panel_h)
                };
                reg.spawn(SIDEBAR_TASKU_ID, sidebar_w, tasku_terminal_h(panel_h), None);
                let fields = tasku_fields.as_ref();
                let cmd = active_project.borrow().as_ref()
                    .and_then(|code| code_to_name.get(code))
                    .map(|name| format!("tasku list --project {name} --fields {fields}\n"))
                    .unwrap_or_else(|| format!("tasku list --fields {fields}\n"));
                reg.write_key(SIDEBAR_TASKU_ID, cmd.as_bytes());
            } else {
                reg.remove(SIDEBAR_TASKU_ID);
            }
        }
    });

    // ── Tasku button commands ────────────────────────────────────────────────
    ui.on_tasku_command({
        let registry = Rc::clone(&registry);
        let active_project = Rc::clone(&active_project);
        let code_to_name = Rc::clone(&code_to_name);
        let tasku_fields = Rc::clone(&tasku_fields);
        move |label| {
            // Resolve active project code → full name for --project flag.
            // Unfiltered commands (Edit, Delete, SQL) ignore the project flag.
            let pf = active_project.borrow().as_ref()
                .and_then(|code| code_to_name.get(code))
                .map(|name| format!(" --project {name}"))
                .unwrap_or_default();
            let ff = format!(" --fields {}", tasku_fields.as_ref());

            let cmd = match label.as_str() {
                "List"    => format!("tasku list{pf}{ff}\n"),
                "Stats"   => format!("tasku stats{pf}\n"),
                "Today"   => format!("tasku list --due today{pf}{ff}\n"),
                "Tmrw"    => format!("tasku list --due tomorrow{pf}{ff}\n"),
                "Overdue" => format!("tasku list --overdue{pf}{ff}\n"),
                "Todo"    => format!("tasku list --status todo{pf}{ff}\n"),
                "Started" => format!("tasku list --status started{pf}{ff}\n"),
                "Done"    => format!("tasku list --status done{pf}{ff}\n"),
                // Unfiltered — these launch interactive prompts
                "Add"    => "tasku add\n".to_string(),
                "Edit"   => "tasku edit\n".to_string(),
                "Delete" => "tasku delete\n".to_string(),
                "SQL"    => "tasku sql\n".to_string(),
                _ => return,
            };
            registry.borrow_mut().write_key(SIDEBAR_TASKU_ID, cmd.as_bytes());
        }
    });

    // ── Tasku key input ──────────────────────────────────────────────────────
    ui.on_tasku_key_input({
        let registry = Rc::clone(&registry);
        move |text, _ctrl, _meta| {
            let bytes = key_text_to_bytes(&text);
            registry.borrow_mut().write_key(SIDEBAR_TASKU_ID, &bytes);
        }
    });

    // ── Workspace selection ──────────────────────────────────────────────────
    ui.on_workspace_selected({
        let tree = Rc::clone(&tree);
        let registry = Rc::clone(&registry);
        let pane_model = Rc::clone(&pane_model);
        let div_model = Rc::clone(&div_model);
        let dividers_cache = Rc::clone(&dividers_cache);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let active_project = Rc::clone(&active_project);
        let code_to_name = Rc::clone(&code_to_name);
        let project_paths = Rc::clone(&project_paths);
        let tasku_fields = Rc::clone(&tasku_fields);
        let ui_weak = ui.as_weak();
        move |code| {
            let code = code.to_string();

            // 1. Save layout to disk and park sessions for the current project.
            //    If no project was active yet (first selection), just tear down
            //    the initial terminal since it isn't associated with any project.
            {
                let current = active_project.borrow().clone();
                let leaf_ids = tree.borrow().leaf_ids();
                if let Some(ref proj) = current {
                    let cwds = registry.borrow().collect_cwds(&leaf_ids);
                    let saved_tree = tree.borrow().to_saved(&cwds);
                    workspace::save_workspace(proj, &workspace::SavedWorkspace {
                        project: proj.clone(),
                        tree: saved_tree,
                    });
                    registry.borrow_mut().park_project(proj, &leaf_ids);
                } else {
                    let mut reg = registry.borrow_mut();
                    for id in leaf_ids {
                        reg.remove(id);
                    }
                }
            }
            images.borrow_mut().clear();

            // 2. Load saved layout or create a fresh single-pane workspace.
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
            let project_root = project_paths.get(&code).cloned().unwrap_or_else(|| home.clone());

            let (new_tree, mut pane_cwds) = match workspace::load_workspace(&code) {
                Some(saved) => PaneTree::from_saved(&saved.tree),
                None => {
                    let t = PaneTree::new();
                    let root_id = t.root;
                    (t, [(root_id, project_root.clone())].into())
                }
            };

            // Replace any pane CWD that is "/" (uninitialised) with the
            // configured project root so the terminal opens somewhere useful.
            for cwd in pane_cwds.values_mut() {
                if cwd == "/" {
                    *cwd = project_root.clone();
                }
            }

            *tree.borrow_mut() = new_tree;
            let first_leaf = tree.borrow().leaf_ids().into_iter().next();
            *focused_id.borrow_mut() = first_leaf;

            // 3. Restore parked sessions (keeping terminals alive across switches)
            //    or spawn fresh ones if this project hasn't been visited before.
            if let Some(ui) = ui_weak.upgrade() {
                let w = ui.get_window_w();
                let h = ui.get_window_h();
                let panes = tree.borrow().flatten(w, h);
                {
                    let pane_tuples: Vec<(NodeId, f32, f32)> =
                        panes.iter().map(|p| (p.id, p.width, p.height)).collect();
                    registry.borrow_mut()
                        .restore_or_spawn_project(&code, &pane_tuples, &pane_cwds);
                }
                full_push(&ui, &tree.borrow(), &dividers_cache, &pane_model, &div_model,
                          &images.borrow(), *focused_id.borrow(), None);

                // 4. Update active project and reflect in UI
                *active_project.borrow_mut() = Some(code.clone());
                ui.set_active_project(code.clone().into());

                // 5. If the Tasku panel is open, refresh with project filter
                if registry.borrow().sessions.contains_key(&SIDEBAR_TASKU_ID) {
                    let name = code_to_name.get(&code).cloned().unwrap_or_else(|| code.clone());
                    let fields = tasku_fields.as_ref();
                    let cmd = format!("tasku list --project {name} --fields {fields}\n");
                    registry.borrow_mut().write_key(SIDEBAR_TASKU_ID, cmd.as_bytes());
                }
            }
        }
    });

    // ── Tasku panel height (drag-to-resize) ──────────────────────────────────
    ui.on_tasku_panel_height_changed({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        move |new_h| {
            let sidebar_w = {
                let mut s = sidebar.borrow_mut();
                s.tasku_panel_h = new_h;
                (s.width - 24.0).max(50.0)
            };
            registry.borrow_mut().resize(
                SIDEBAR_TASKU_ID,
                sidebar_w,
                tasku_terminal_h(new_h),
            );
        }
    });

    // ── Tasku terminal scroll wheel ──────────────────────────────────────────
    ui.on_tasku_scroll({
        let registry = Rc::clone(&registry);
        move |delta_px| {
            // delta_px is logical-pixel delta-y from Slint (negative = scroll up).
            // Convert to rows: scrolling up (negative delta) → positive row delta
            // (move toward older content).
            let cell_h_logical = {
                let reg = registry.borrow();
                reg.font.cell_h as f32 / reg.scale
            };
            if cell_h_logical > 0.0 {
                let delta_rows = (-delta_px / cell_h_logical).round() as i32;
                if delta_rows != 0 {
                    registry.borrow_mut().scroll(SIDEBAR_TASKU_ID, delta_rows);
                }
            }
        }
    });

    // ── External plugin panel expand/collapse ────────────────────────────────
    ui.on_plugin_toggled({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        let plugin_expanded_model = Rc::clone(&plugin_expanded_model);
        let plugin_panel_h_model = Rc::clone(&plugin_panel_h_model);
        let pixel_plugins = Rc::clone(&pixel_plugins);
        let ui_weak = ui.as_weak();
        move |idx| {
            let idx = idx as usize;
            if idx >= num_ext_plugins { return; }
            let current = plugin_expanded_model.row_data(idx).unwrap_or(false);
            let now_expanded = !current;
            plugin_expanded_model.set_row_data(idx, now_expanded);

            let sidebar_w = (sidebar.borrow().width - 24.0).max(50.0);
            let panel_h = plugin_panel_h_model.row_data(idx).unwrap_or(DEFAULT_PLUGIN_PANEL_H);
            let (command, kind, plugin_id) = {
                let s = sidebar.borrow();
                let p = s.ext_plugins.get(idx);
                (
                    p.map(|p| p.command.clone()).unwrap_or_default(),
                    p.map(|p| p.kind.clone()).unwrap_or_default(),
                    p.map(|p| p.id.clone()).unwrap_or_default(),
                )
            };
            let mut parts = command.split_whitespace();
            let program = parts.next().unwrap_or("").to_string();
            let args: Vec<String> = parts.map(|s| s.to_string()).collect();
            let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

            let mut reg = registry.borrow_mut();
            if now_expanded {
                if kind == "pixel" {
                    let phys_w = (sidebar_w * scale) as u32;
                    let phys_h = (panel_h * scale) as u32;
                    if let Some(plugin) = PixelPlugin::spawn(&program, &args_ref, phys_w, phys_h) {
                        pixel_plugins.borrow_mut().insert(idx, plugin);
                    } else {
                        eprintln!("mado: failed to spawn pixel plugin '{plugin_id}'");
                    }
                } else {
                    reg.spawn_cmd(plugin_node_id(idx), sidebar_w, plugin_terminal_h(panel_h),
                                  &program, &args_ref, None,
                                  &[("MADO_PLUGIN_ID", &plugin_id)]);
                }
            } else if kind == "pixel" {
                pixel_plugins.borrow_mut().remove(&idx);
            } else {
                reg.remove(plugin_node_id(idx));
            }
            drop(reg);

            // Update any-expanded and total-h for the viewport
            let any = (0..num_ext_plugins)
                .any(|i| plugin_expanded_model.row_data(i).unwrap_or(false));
            let total: f32 = (0..num_ext_plugins)
                .map(|i| if plugin_expanded_model.row_data(i).unwrap_or(false) {
                    plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H)
                } else { 0.0 })
                .sum();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_plugin_any_expanded(any);
                ui.set_external_plugins_total_h(total);
            }
        }
    });

    // ── External plugin panel height (drag-to-resize) ────────────────────────
    ui.on_plugin_panel_height_changed({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        let plugin_expanded_model = Rc::clone(&plugin_expanded_model);
        let plugin_panel_h_model = Rc::clone(&plugin_panel_h_model);
        let pixel_plugins = Rc::clone(&pixel_plugins);
        let ui_weak = ui.as_weak();
        move |idx, new_h| {
            let idx = idx as usize;
            if idx >= num_ext_plugins { return; }
            plugin_panel_h_model.set_row_data(idx, new_h);
            let sidebar_w = (sidebar.borrow().width - 24.0).max(50.0);
            if let Some(plugin) = pixel_plugins.borrow_mut().get_mut(&idx) {
                plugin.send_resize((sidebar_w * scale) as u32, (new_h * scale) as u32);
            } else {
                registry.borrow_mut().resize(plugin_node_id(idx), sidebar_w, plugin_terminal_h(new_h));
            }
            // Update viewport total height
            let total: f32 = (0..num_ext_plugins)
                .map(|i| if plugin_expanded_model.row_data(i).unwrap_or(false) {
                    plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H)
                } else { 0.0 })
                .sum();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_external_plugins_total_h(total);
            }
        }
    });

    // ── External plugin key input ─────────────────────────────────────────────
    ui.on_plugin_key_input({
        let registry = Rc::clone(&registry);
        let pixel_plugins = Rc::clone(&pixel_plugins);
        move |idx, text, ctrl, meta| {
            let idx = idx as usize;
            if idx >= num_ext_plugins { return; }
            if let Some(plugin) = pixel_plugins.borrow_mut().get_mut(&idx) {
                plugin.send_key(text.as_str(), ctrl, meta);
            } else {
                let zoom_mod = ctrl || meta;
                let t = text.as_str();
                if zoom_mod && (t == "=" || t == "+" || t == "-" || t == "0") { return; }
                let bytes = key_text_to_bytes(&text);
                registry.borrow_mut().write_key(plugin_node_id(idx), &bytes);
            }
        }
    });

    // ── External plugin scroll wheel ─────────────────────────────────────────
    ui.on_plugin_scroll({
        let registry = Rc::clone(&registry);
        let pixel_plugins = Rc::clone(&pixel_plugins);
        move |idx, delta_px| {
            let idx = idx as usize;
            if idx >= num_ext_plugins { return; }
            if let Some(plugin) = pixel_plugins.borrow_mut().get_mut(&idx) {
                plugin.send_scroll(delta_px);
            } else {
                let cell_h_logical = {
                    let reg = registry.borrow();
                    reg.font.cell_h as f32 / reg.scale
                };
                if cell_h_logical > 0.0 {
                    let delta_rows = (-delta_px / cell_h_logical).round() as i32;
                    if delta_rows != 0 {
                        registry.borrow_mut().scroll(plugin_node_id(idx), delta_rows);
                    }
                }
            }
        }
    });

    // ── External plugin click ────────────────────────────────────────────────
    ui.on_plugin_click({
        let pixel_plugins = Rc::clone(&pixel_plugins);
        let registry      = Rc::clone(&registry);
        let focused_id    = Rc::clone(&focused_id);
        move |idx, x, y| {
            let idx = idx as usize;
            if let Some(plugin) = pixel_plugins.borrow_mut().get_mut(&idx) {
                plugin.send_click(x, y);
                // Paste is handled in the render timer by watching paste_pending.
            }
        }
    });

    // ── External plugin focus ────────────────────────────────────────────────
    ui.on_plugin_focus_changed({
        let pixel_plugins = Rc::clone(&pixel_plugins);
        move |idx, focused| {
            let idx = idx as usize;
            if let Some(plugin) = pixel_plugins.borrow_mut().get_mut(&idx) {
                plugin.send_focus(focused);
            }
        }
    });

    // ── Initial render ───────────────────────────────────────────────────────
    full_push(&ui, &tree.borrow(), &dividers_cache, &pane_model, &div_model,
              &images.borrow(), *focused_id.borrow(), None);

    // Re-request keyboard focus after the event loop starts — the window is
    // visible by then so the OS honours the focus call.
    // Uses a dedicated force-focus-id property rather than toggling is-focused,
    // since Slint batches property updates and the toggle would be a no-op.
    {
        let ui_weak = ui.as_weak();
        let focused_id = Rc::clone(&focused_id);
        let startup_focus = Timer::default();
        startup_focus.start(
            TimerMode::SingleShot,
            std::time::Duration::from_millis(50),
            move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                if let Some(fid) = *focused_id.borrow() {
                    ui.set_force_focus_id(fid as i32);
                    // Reset so the property can fire again on future use.
                    ui.set_force_focus_id(-1);
                }
            },
        );
        std::mem::forget(startup_focus);
    }

    // ── Save workspace on quit ───────────────────────────────────────────────
    // Persist the active project's layout + CWDs so the next session can
    // restore the directories the user was in without needing a workspace switch.
    {
        let tree = Rc::clone(&tree);
        let registry = Rc::clone(&registry);
        let active_project = Rc::clone(&active_project);
        ui.window().on_close_requested(move || {
            if let Some(ref proj) = *active_project.borrow() {
                let leaf_ids = tree.borrow().leaf_ids();
                let cwds = registry.borrow().collect_cwds(&leaf_ids);
                let saved_tree = tree.borrow().to_saved(&cwds);
                workspace::save_workspace(proj, &workspace::SavedWorkspace {
                    project: proj.clone(),
                    tree: saved_tree,
                });
            }
            slint::CloseRequestResponse::HideWindow
        });
    }

    ui.run().unwrap();
}

// ── Plugin CLI helpers ───────────────────────────────────────────────────────

const REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/tomdringer/mado-plugins/main/registry.json";

fn plugin_config_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".config").join("mado").join("config.toml")
}

fn plugins_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".config").join("mado").join("plugins")
}

fn platform_triple() -> &'static str {
    #[cfg(all(target_os = "macos",   target_arch = "aarch64"))] return "aarch64-apple-darwin";
    #[cfg(all(target_os = "macos",   target_arch = "x86_64"))]  return "x86_64-apple-darwin";
    #[cfg(all(target_os = "linux",   target_arch = "aarch64"))] return "aarch64-unknown-linux-gnu";
    #[cfg(all(target_os = "linux",   target_arch = "x86_64"))]  return "x86_64-unknown-linux-gnu";
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]  return "x86_64-pc-windows-msvc";
    #[allow(unreachable_code)]
    "unknown"
}

/// Fetch a URL and return the body as a string. Uses curl, which is available
/// on macOS, all major Linux distros, and Windows 10+.
fn curl_get(url: &str) -> Result<String, String> {
    let out = std::process::Command::new("curl")
        .args(["-sL", "--fail",
               "-H", "User-Agent: mado",
               "-H", "Accept: application/vnd.github+json",
               url])
        .output()
        .map_err(|e| format!("curl not available: {e}"))?;
    if !out.status.success() {
        return Err(format!("request failed (HTTP error) for {url}"));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

/// Download a URL to a file on disk.
fn curl_download(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let status = std::process::Command::new("curl")
        .args(["-sL", "--fail",
               "-H", "User-Agent: mado",
               "-o", dest.to_str().unwrap_or(""),
               url])
        .status()
        .map_err(|e| format!("curl not available: {e}"))?;
    if !status.success() {
        return Err(format!("download failed from {url}"));
    }
    Ok(())
}

/// Resolve a short name ("clock") to an org/repo ("mado-plugins/mado-clock")
/// via the registry. If `name` already contains '/' it is returned as-is.
fn resolve_repo(name: &str) -> Result<String, String> {
    if name.contains('/') {
        return Ok(name.to_string());
    }
    let json = curl_get(REGISTRY_URL)?;
    let registry: serde_json::Value = serde_json::from_str(&json)
        .map_err(|e| format!("invalid registry JSON: {e}"))?;
    registry.get(name)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("'{name}' not found in mado-plugins registry"))
}

fn plugin_install(name: &str) {
    // 1. Resolve to org/repo
    print!("resolving '{name}'... ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let repo = resolve_repo(name).unwrap_or_else(|e| {
        eprintln!("\nmado: {e}");
        std::process::exit(1);
    });

    // Derive the plugin id (short name) and the repo binary name.
    // Short name: last segment after '/' if user passed org/repo directly, else the original name.
    let id = if name.contains('/') {
        name.split('/').last().unwrap_or(name)
            .trim_start_matches("mado-")  // strip conventional "mado-" prefix
    } else {
        name
    };
    // Repo binary name is the repo part of org/repo, e.g. "mado-clock"
    let repo_bin = repo.split('/').last().unwrap_or(id);

    println!("found {repo}");

    // 2. Fetch latest GitHub release
    let api_url = format!("https://api.github.com/repos/{repo}/releases/latest");
    print!("fetching latest release... ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let json = curl_get(&api_url).unwrap_or_else(|e| {
        eprintln!("\nmado: {e}");
        std::process::exit(1);
    });
    let release: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|e| {
        eprintln!("\nmado: invalid release JSON: {e}");
        std::process::exit(1);
    });
    let tag = release["tag_name"].as_str().unwrap_or("unknown");
    println!("{tag}");

    // 3. Find the asset matching the current platform
    let triple = platform_triple();
    let asset_suffix = if cfg!(target_os = "windows") {
        format!("{repo_bin}-{triple}.exe")
    } else {
        format!("{repo_bin}-{triple}")
    };

    let empty = vec![];
    let assets = release["assets"].as_array().unwrap_or(&empty);
    let download_url = assets.iter()
        .find(|a| a["name"].as_str().map_or(false, |n| n == asset_suffix))
        .and_then(|a| a["browser_download_url"].as_str())
        .unwrap_or_else(|| {
            eprintln!("mado: no binary for {triple} in {repo} {tag}");
            eprintln!("      expected asset name: {asset_suffix}");
            std::process::exit(1);
        });

    // 4. Download to plugins dir
    let dir = plugins_dir();
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| {
        eprintln!("mado: cannot create plugins dir: {e}");
        std::process::exit(1);
    });
    let bin_name = if cfg!(target_os = "windows") { format!("{id}.exe") } else { id.to_string() };
    let dest = dir.join(&bin_name);

    print!("downloading {asset_suffix}... ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    curl_download(download_url, &dest).unwrap_or_else(|e| {
        eprintln!("\nmado: {e}");
        std::process::exit(1);
    });
    println!("done");

    // 5. Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&dest) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&dest, perms);
        }
    }

    // 6. Fetch mado-plugin.json to determine kind and icon
    let meta_url = format!("https://raw.githubusercontent.com/{repo}/HEAD/mado-plugin.json");
    let (kind, icon) = curl_get(&meta_url)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .map(|v| (
            v["kind"].as_str().unwrap_or("").to_string(),
            v["icon"].as_str().unwrap_or("").to_string(),
        ))
        .unwrap_or_default();

    // 7. Register in config (id + absolute path as command + kind + icon)
    let command = dest.to_string_lossy();
    plugin_register(id, &command, &kind, &icon);

    println!("installed '{id}' — restart Mado to activate");
}

/// `mado plugin update <name>` — download the latest release binary for an
/// already-registered plugin, overwriting the existing binary in place.
/// Does not touch the config (the plugin is already registered).
fn plugin_update(name: &str) {
    // Resolve id the same way install does
    let id = if name.contains('/') {
        name.split('/').last().unwrap_or(name)
            .trim_start_matches("mado-")
    } else {
        name
    };

    // Ensure the plugin is actually registered
    let cfg = Config::load();
    let plugin = cfg.plugins.iter().find(|p| p.id == id).unwrap_or_else(|| {
        eprintln!("mado: plugin '{id}' not installed — use 'mado plugin install {id}' first");
        std::process::exit(1);
    });

    print!("resolving '{id}'... ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let repo = resolve_repo(name).unwrap_or_else(|e| {
        eprintln!("\nmado: {e}");
        std::process::exit(1);
    });
    let repo_bin = repo.split('/').last().unwrap_or(id);
    println!("found {repo}");

    let api_url = format!("https://api.github.com/repos/{repo}/releases/latest");
    print!("fetching latest release... ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let json = curl_get(&api_url).unwrap_or_else(|e| {
        eprintln!("\nmado: {e}");
        std::process::exit(1);
    });
    let release: serde_json::Value = serde_json::from_str(&json).unwrap_or_else(|e| {
        eprintln!("\nmado: invalid release JSON: {e}");
        std::process::exit(1);
    });
    let tag = release["tag_name"].as_str().unwrap_or("unknown");
    println!("{tag}");

    let triple = platform_triple();
    let asset_suffix = if cfg!(target_os = "windows") {
        format!("{repo_bin}-{triple}.exe")
    } else {
        format!("{repo_bin}-{triple}")
    };

    let empty = vec![];
    let assets = release["assets"].as_array().unwrap_or(&empty);
    let download_url = assets.iter()
        .find(|a| a["name"].as_str().map_or(false, |n| n == asset_suffix))
        .and_then(|a| a["browser_download_url"].as_str())
        .unwrap_or_else(|| {
            eprintln!("mado: no binary for {triple} in {repo} {tag}");
            eprintln!("      expected asset name: {asset_suffix}");
            std::process::exit(1);
        });

    let dest = std::path::Path::new(&plugin.command);
    print!("downloading {asset_suffix}... ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    curl_download(download_url, dest).unwrap_or_else(|e| {
        eprintln!("\nmado: {e}");
        std::process::exit(1);
    });
    println!("done");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(dest) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(dest, perms);
        }
    }

    println!("updated '{id}' to {tag} — restart Mado to activate");
}

/// Low-level: append a [[plugins]] entry to config.toml.
/// Called by both plugin_install and plugin_add.
fn plugin_register(id: &str, command: &str, kind: &str, icon: &str) {
    if id.is_empty() || id.contains('"') || id.contains('\n') {
        eprintln!("mado: invalid plugin id '{id}'");
        std::process::exit(1);
    }

    let path = plugin_config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Check for duplicate
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let cfg: Config = toml::from_str(&existing).unwrap_or_default();
    if cfg.plugins.iter().any(|p| p.id == id) {
        eprintln!("mado: plugin '{id}' is already registered");
        std::process::exit(1);
    }

    let kind_line = if kind == "pixel" { "kind    = \"pixel\"\n".to_string() } else { String::new() };
    let icon_line = if !icon.is_empty() { format!("icon    = \"{icon}\"\n") } else { String::new() };
    let block = format!("\n[[plugins]]\nid      = \"{id}\"\ncommand = \"{command}\"\n{kind_line}{icon_line}");
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true).append(true).open(&path)
        .unwrap_or_else(|e| { eprintln!("mado: cannot open config: {e}"); std::process::exit(1); });
    file.write_all(block.as_bytes())
        .unwrap_or_else(|e| { eprintln!("mado: cannot write config: {e}"); std::process::exit(1); });
}

/// `mado plugin add <id> <command>` — register any binary directly.
fn plugin_add(id: &str, command: &str) {
    if command.is_empty() || command.contains('"') || command.contains('\n') {
        eprintln!("mado: invalid command '{command}'");
        std::process::exit(1);
    }
    plugin_register(id, command, "", "");
    println!("registered plugin '{id}' — restart Mado to activate");
}

fn plugin_remove(id: &str) {
    let path = plugin_config_path();
    let content = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        eprintln!("mado: no config file found");
        std::process::exit(1);
    });

    let cfg: Config = toml::from_str(&content).unwrap_or_default();
    let plugin = cfg.plugins.iter().find(|p| p.id == id).unwrap_or_else(|| {
        eprintln!("mado: plugin '{id}' not found");
        std::process::exit(1);
    });

    // Delete the binary if it lives inside the managed plugins dir
    let managed = plugins_dir();
    let cmd_path = std::path::Path::new(&plugin.command);
    if cmd_path.starts_with(&managed) && cmd_path.exists() {
        let _ = std::fs::remove_file(cmd_path);
    }

    let new_content = remove_plugin_block(&content, id);
    std::fs::write(&path, &new_content)
        .unwrap_or_else(|e| { eprintln!("mado: cannot write config: {e}"); std::process::exit(1); });

    println!("removed plugin '{id}' — restart Mado to deactivate");
}

fn plugin_list() {
    let cfg = Config::load();
    if cfg.plugins.is_empty() {
        println!("no plugins installed");
        return;
    }
    for p in &cfg.plugins {
        println!("{:<20} {}", p.id, p.command);
    }
}

/// Remove the `[[plugins]]` block whose `id` field matches `target_id`.
/// Preserves all other content and comments exactly.
fn remove_plugin_block(content: &str, target_id: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let had_trailing_newline = content.ends_with('\n');
    let mut result: Vec<&str> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if lines[i].trim() == "[[plugins]]" {
            // Collect indices of this block: from [[plugins]] up to (not including) the next section header
            let mut block: Vec<usize> = vec![i];
            let mut j = i + 1;
            while j < lines.len() {
                if lines[j].trim().starts_with('[') { break; }
                block.push(j);
                j += 1;
            }
            // Check if this block contains `id = "target_id"`
            let needle = format!("\"{}\"", target_id);
            let is_target = block.iter().any(|&k| {
                let t = lines[k].trim();
                t.starts_with("id") && t.contains(needle.as_str())
            });
            if !is_target {
                for &k in &block {
                    result.push(lines[k]);
                }
            }
            i = j;
            continue;
        }
        result.push(lines[i]);
        i += 1;
    }

    let mut s = result.join("\n");
    if had_trailing_newline || s.ends_with('\n') {
        if !s.ends_with('\n') { s.push('\n'); }
    }
    // Collapse more than two consecutive blank lines
    while s.contains("\n\n\n\n") {
        s = s.replace("\n\n\n\n", "\n\n\n");
    }
    s
}

// ── Key translation ──────────────────────────────────────────────────────────

/// Translate Slint key text to terminal byte sequences.
///
/// Slint key codes (from i-slint-common/key_codes.rs):
///   Modifier keys live in the C0 control range (U+0010–U+0019) and MUST be
///   filtered — they alias Ctrl+P, Ctrl+U, Ctrl+Q etc. and cause visible damage.
///   Navigation/function keys are in the Specials range (U+F700+).
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn key_text_to_bytes(text: &str) -> Vec<u8> {
    match text {
        // ── Modifier keys — never forward to terminal ────────────────────────
        "\u{0010}" | // Shift (L)   — would send Ctrl+P
        "\u{0015}" | // Shift (R)   — would send Ctrl+U (kill line!)
        "\u{0011}" | // Control (L) — would send Ctrl+Q
        "\u{0016}" | // Control (R) — would send Ctrl+V
        "\u{0012}" | // Alt         — would send Ctrl+R
        "\u{0013}" | // AltGr       — would send Ctrl+S
        "\u{0014}" | // CapsLock    — would send Ctrl+T
        "\u{0017}" | // Meta (L)    — would send Ctrl+W
        "\u{0018}" | // Meta (R)    — would send Ctrl+X
        "\u{0019}"   // Backtab
        => vec![],

        // ── Standard keys ───────────────────────────────────────────────────
        "\u{0008}" => vec![0x7F],       // Backspace → DEL (terminal convention)
        "\u{007F}" => vec![0x1B, b'[', b'3', b'~'], // Delete → ESC [ 3 ~

        // ── Arrow keys ──────────────────────────────────────────────────────
        "\u{F700}" => vec![0x1B, b'[', b'A'], // Up
        "\u{F701}" => vec![0x1B, b'[', b'B'], // Down
        "\u{F702}" => vec![0x1B, b'[', b'D'], // Left
        "\u{F703}" => vec![0x1B, b'[', b'C'], // Right

        // ── Navigation ──────────────────────────────────────────────────────
        "\u{F729}" => vec![0x1B, b'[', b'H'],             // Home
        "\u{F72B}" => vec![0x1B, b'[', b'F'],             // End
        "\u{F72C}" => vec![0x1B, b'[', b'5', b'~'],       // PageUp
        "\u{F72D}" => vec![0x1B, b'[', b'6', b'~'],       // PageDown

        // ── Function keys ────────────────────────────────────────────────────
        "\u{F704}" => vec![0x1B, b'O', b'P'],                    // F1
        "\u{F705}" => vec![0x1B, b'O', b'Q'],                    // F2
        "\u{F706}" => vec![0x1B, b'O', b'R'],                    // F3
        "\u{F707}" => vec![0x1B, b'O', b'S'],                    // F4
        "\u{F708}" => vec![0x1B, b'[', b'1', b'5', b'~'],       // F5
        "\u{F709}" => vec![0x1B, b'[', b'1', b'7', b'~'],       // F6
        "\u{F70A}" => vec![0x1B, b'[', b'1', b'8', b'~'],       // F7
        "\u{F70B}" => vec![0x1B, b'[', b'1', b'9', b'~'],       // F8
        "\u{F70C}" => vec![0x1B, b'[', b'2', b'0', b'~'],       // F9
        "\u{F70D}" => vec![0x1B, b'[', b'2', b'1', b'~'],       // F10
        "\u{F70E}" => vec![0x1B, b'[', b'2', b'3', b'~'],       // F11
        "\u{F70F}" => vec![0x1B, b'[', b'2', b'4', b'~'],       // F12

        // ── Everything else: printable chars, Ctrl+letter combos ────────────
        // Return (\r), Tab (\t), Escape (\u{1B}), and Ctrl+X (\u{0001}–\u{001A})
        // all pass through as their raw bytes — correct terminal values.
        _ => text.as_bytes().to_vec(),
    }
}

