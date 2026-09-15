mod browser;
mod config;
mod keys;
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
use std::sync::Arc;
use std::collections::HashMap;

use config::Config;
use pane_tree::{FlatDividerData, NavDir, NodeId, SplitDir};
use pixel_plugin::PixelPlugin;
use sidebar::SidebarState;
use tasku::{detect as detect_tasku, status as tasku_status};
use terminal::TerminalRegistry;
use slint::{Image, Model, ModelRc, Timer, TimerMode, VecModel};

/// Encode raw RGBA pixel data as a base64-encoded PNG string.
#[cfg(target_os = "macos")]
fn encode_png_base64(width: u32, height: u32, rgba: &[u8]) -> Option<String> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(std::io::Cursor::new(&mut buf), width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().ok()?;
        w.write_image_data(rgba).ok()?;
    }
    Some(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &buf))
}

// Special NodeIds for sidebar terminals — must not collide with pane IDs.
// Pane IDs are allocated sequentially from the PaneTree counter; u32::MAX
// is a safe sentinel. External plugin IDs count down from u32::MAX - 1.
const SIDEBAR_TASKU_ID: NodeId = u32::MAX;

/// Path to the file where `tasku list` writes the currently selected task ID
/// when running in Mado mode (MADO=1). Mado button handlers read this to
/// inject the task ID into commands like `tasku edit <id>`.
const TASKU_SEL_FILE: &str = "/tmp/tasku_mado_sel";

/// Path to the file where Mado writes the Tasku PTY column count so that
/// tasku can read the exact width without relying on unreliable ioctl methods.
const TASKU_COLS_FILE: &str = "/tmp/tasku_mado_cols";

/// Shared log file for diagnosing Tasku button / MadoList interactions.
/// Both Mado (Rust) and tasku (Ruby) append to this file.
const TASKU_LOG_FILE: &str = "/tmp/tasku_mado.log";

fn tasku_log(msg: &str) {
    use std::io::Write;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true).open(TASKU_LOG_FILE)
    {
        let _ = writeln!(f, "[{ts}] mado: {msg}");
    }
}

/// NodeId for LEFT external plugin at index `i`.
fn plugin_node_id(i: usize) -> NodeId { u32::MAX - 1 - i as u32 }

/// NodeId for RIGHT external plugin at index `i`.
fn right_plugin_node_id(i: usize) -> NodeId { u32::MAX / 2 - i as u32 }

/// NodeId for TOP external plugin at index `i`.
fn top_plugin_node_id(i: usize) -> NodeId { u32::MAX / 4 - i as u32 }

/// NodeId for TOP-RIGHT panel plugin at index `i`.
fn top_right_plugin_node_id(i: usize) -> NodeId { u32::MAX / 16 - i as u32 }

/// NodeId for BOTTOM-RIGHT panel plugin at index `i`.
fn bottom_right_plugin_node_id(i: usize) -> NodeId { u32::MAX / 32 - i as u32 }

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

// Horizontal (top-bar) Tasku layout constants.
// Button column: 8px pad-L + 248px (3×80+2×4) + 6px spacing + 8px pad-R = 270px
// Terminal height: panel_h minus 8px top pad + 8px bot pad = panel_h - 16px
const TASKU_HORIZ_BTN_W: f32 = 270.0;
const TASKU_HORIZ_PAD_H: f32 = 16.0;

const TASKU_MAX_COLS: usize  = 500;
/// Multiply sidebar Tasku PTY width by this factor so more columns fit.
/// The PTY image is displayed with image-fit: fill, compressing it back
/// into the sidebar. Interactive commands (tasku add/edit) run in the same
/// boosted PTY — forms will be slightly compressed but fully functional.
const TASKU_SIDEBAR_COL_BOOST: f32 = 1.5;

// Runner (bottom bar) PTY node ID and layout constants.
// Button column: 8px pad + 60px btns + 6px spacing + 8px pad = 82px.
const RUNNER_ID:      NodeId = u32::MAX / 8;
const RUNNER_BTN_W:   f32   = 82.0;
const RUNNER_PAD_H:   f32   = 16.0;
const RUNNER_PANEL_H: f32   = 300.0;  // overlay height, kept fixed (matches main.slint 300px)

fn runner_size(overlay_w: f32) -> (f32, f32) {
    let term_w = (overlay_w - RUNNER_BTN_W).max(50.0).round();
    let term_h = (RUNNER_PANEL_H - RUNNER_PAD_H).max(50.0).round();
    (term_w, term_h)
}

/// PTY size for Tasku in the top bar overlay (horizontal layout).
/// `overlay_w` = full window width; `overlay_h` = window height minus the 52px header.
fn tasku_top_bar_size(overlay_w: f32, overlay_h: f32) -> (f32, f32) {
    let term_w = (overlay_w - TASKU_HORIZ_BTN_W).max(50.0).round();
    let term_h = (overlay_h - TASKU_HORIZ_PAD_H).max(50.0).round();
    (term_w, term_h)
}

/// Terminal rect height inside the Tasku panel.
fn tasku_terminal_h(panel_h: f32) -> f32 {
    (panel_h - TASKU_PANEL_FIXED_H).max(50.0)
}

/// Terminal rect height inside an external plugin panel.
fn plugin_terminal_h(panel_h: f32) -> f32 {
    (panel_h - PLUGIN_PANEL_FIXED_H).max(50.0)
}

/// Read the task ID written by `tasku list` in Mado mode.
/// Returns an empty string if the file is missing, empty, or non-numeric.
fn read_tasku_sel() -> String {
    std::fs::read_to_string(TASKU_SEL_FILE)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Build the PTY command string for a Tasku top-bar button press.
///
/// * `label`   — the button label (e.g. "List", "Edit", "Delete")
/// * `pf`      — project-filter suffix already formatted as `" --project NAME"`
///               or `""` when no project is active
/// * `sel_id`  — the currently-selected task ID (from TASKU_SEL_FILE),
///               required for Edit and Delete; ignored by all other commands
///
/// Returns `None` for unknown labels or when Edit/Delete have no selection.
fn tasku_button_command(label: &str, pf: &str, sel_id: &str) -> Option<String> {
    match label {
        // Interactive commands: interrupt MadoList with Ctrl+C so the shell
        // can run the interactive program, then restart the list via semicolon.
        "Edit" => {
            if sel_id.is_empty() { return None; }
            Some(format!("\x03tasku edit {sel_id} -i; tasku list\n"))
        }
        "Delete" => {
            if sel_id.is_empty() { return None; }
            Some(format!("\x03tasku delete {sel_id} -f; tasku list\n"))
        }
        "Add" => Some("\x03tasku add -i; tasku list\n".to_string()),

        // Filter / display commands: use the STX inline-exec protocol (\x02).
        // MadoList reads the command string and calls exec() itself, so we
        // never rely on the shell picking up buffered PTY input after exit.
        "List"     => Some(format!("\x02tasku list{pf}\n")),
        "List all" => Some("\x02tasku list\n".to_string()),
        "Today"    => Some(format!("\x02tasku list --today{pf}\n")),
        "Tmrw"     => Some(format!("\x02tasku list --tomorrow{pf}\n")),
        "Overdue"  => Some(format!("\x02tasku list --overdue{pf}\n")),
        "Todo"     => Some(format!("\x02tasku list --status todo{pf}\n")),
        "Started"  => Some(format!("\x02tasku list --status in_progress{pf}\n")),
        "Done"     => Some(format!("\x02tasku list --status done{pf}\n")),

        // SQL: tell MadoList to show a query prompt internally.
        "SQL" => Some("\x02SQL\n".to_string()),

        _ => None,
    }
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

fn runner_px_to_cell(x: f32, y: f32, reg: &TerminalRegistry) -> (usize, usize) {
    // Runner terminal image fills its rectangle with no offset.
    let col = ((x.max(0.0) * reg.scale) / reg.font.cell_w as f32) as usize;
    let row = ((y.max(0.0) * reg.scale) / reg.font.cell_h as f32) as usize;
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

/// Returns `true` when macOS natural scrolling is ON (swipescrolldirection = 1).
/// With natural scrolling OFF (traditional/inverted), Slint still receives the
/// raw CGEvent delta which uses the natural direction, so we must flip the sign.
fn macos_natural_scroll() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "-g", "com.apple.swipescrolldirection"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim() == "1")
        .unwrap_or(true) // default: treat as natural if unreadable
}

fn rgb([r, g, b]: [u8; 3]) -> slint::Color {
    slint::Color::from_rgb_u8(r, g, b)
}

fn rgba([r, g, b]: [u8; 3], a: u8) -> slint::Color {
    slint::Color::from_argb_u8(a, r, g, b)
}

fn apply_theme(ui: &MainWindow, t: &theme::Theme) {
    // Sidebar and chrome are solid; only terminal panes are glassy (~47% opacity).
    ui.set_theme_window_bg(rgb(t.window_bg));
    ui.set_theme_terminal_area_bg(rgb(t.terminal_area_bg));
    ui.set_theme_pane_bg(rgba(t.pane_bg, 120));
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
///
/// Uses `set_row_data` (not `set_vec`) whenever the pane count is unchanged, so
/// existing PaneView component instances are reused. `set_vec` recreates every
/// component, causing `init =>` to fire and `scope.focus()` to steal keyboard
/// focus away from the sidebar. `set_row_data` avoids that entirely.
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

    let new_panes = make_panes(tree, w, h, images, focused_id);
    if new_panes.len() != pane_model.row_count() {
        // Count changed (split / close) — must replace the whole model.
        pane_model.set_vec(new_panes);
    } else {
        // Count unchanged — update each row in place so PaneView instances
        // (and their FocusScopes) are never recreated.
        for (row, pane) in new_panes.into_iter().enumerate() {
            pane_model.set_row_data(row, pane);
        }
    }

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
    // Trailing \r\n moves cursor to the line below the banner so zsh's PROMPT_SP
    // doesn't print a stray "%". Safe here because the banner is at the top —
    // there's nothing above it to scroll off the viewport.
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

    let s = format!("{top}{spacer}{logo_rows}{spacer}{subtitle}{spacer}{hint1}{hint2}{spacer}{bot}");
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
                "# Mado project config\n",
                "#\n",
                "# Set a default project to activate on launch (optional):\n",
                "# default = \"MDO\"\n",
                "#\n",
                "# Each section maps a Tasku project code to its settings.\n",
                "# All fields except [CODE] are optional.\n",
                "#\n",
                "# [MDO]\n",
                "# path    = \"/Users/you/Sites/mado\"   # root directory for new panes\n",
                "# icon    = \"terminal\"                 # workspace tile icon (see presets below)\n",
                "# task    = \"cargo run\"                # command for the bottom-bar runner\n",
                "# project = \"Mado\"                     # override: match a Tasku project by name\n",
                "#\n",
                "# Icon presets:\n",
                "#   terminal  \u{f489}    web      \u{f484}    plugin   \u{f1e6}\n",
                "#   api       \u{eb11}    mobile   \u{f10b}    design   \u{f53f}\n",
                "#   data      \u{f1c0}    docs     \u{f02d}    tool     \u{f0ad}\n",
                "#   cloud     \u{f0c2}\n",
                "#\n",
                "# You can also paste any Nerd Font glyph directly as the icon value.\n",
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

    // Configure the winit backend so blur is set at window-creation time.
    // This is the only point where winit actually calls CGSSetWindowBackgroundBlurRadius;
    // calling set_blur() post-creation is too late on some macOS versions.
    {
        let backend = i_slint_backend_winit::Backend::builder()
            .with_window_attributes_hook(|attrs| attrs.with_blur(true))
            .build()
            .unwrap();
        slint::platform::set_platform(Box::new(backend)).unwrap();
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

    // Scroll direction: macOS passes raw CGEvent deltas regardless of the
    // "natural scrolling" system preference, so we must apply the flip ourselves.
    let natural_scroll = macos_natural_scroll();
    let scroll_dir: f32 = if natural_scroll { 1.0 } else { -1.0 };
    eprintln!("mado: natural scrolling = {natural_scroll}, scroll_dir = {scroll_dir}");

    // Per-pane scroll accumulator for smooth sub-row scrolling.
    let scroll_acc: Rc<RefCell<HashMap<NodeId, f32>>> =
        Rc::new(RefCell::new(HashMap::new()));

    let tree = Rc::new(RefCell::new(PaneTree::new()));
    let registry = Rc::new(RefCell::new(TerminalRegistry::new(default_font_size, scale, shell, config.font_family.clone())));
    registry.borrow().font.prewarm();

    // ── Sidebar + Tasku detection ────────────────────────────────────────────
    let tasku_install = detect_tasku();
    let tasku_status_str = tasku_status(&tasku_install).to_string();
    let tasku_path = tasku_install.map(|t| t.path.to_string_lossy().into_owned());
    if let Some(ref p) = tasku_path {
        println!("mado: tasku found at {p} (status: {tasku_status_str})");
    } else {
        println!("mado: tasku not found");
    }

    let all_plugins = config.plugins.clone();
    let left_ext_plugins: Vec<_> = all_plugins.iter()
        .filter(|p| p.position.is_empty() || p.position == "left")
        .cloned().collect();
    let right_ext_plugins: Vec<_> = all_plugins.iter()
        .filter(|p| p.position == "right")
        .cloned().collect();
    let top_ext_plugins: Vec<_> = all_plugins.iter()
        .filter(|p| p.position == "top")
        .cloned().collect();
    let num_left_ext = left_ext_plugins.len();
    let num_right_ext = right_ext_plugins.len();
    let num_top_ext = top_ext_plugins.len();
    let top_right_plugin: Option<config::PluginConfig> = all_plugins.iter()
        .find(|p| p.position == "top-right")
        .cloned();
    let bottom_right_plugin: Option<config::PluginConfig> = all_plugins.iter()
        .find(|p| p.position == "bottom-right")
        .cloned();
    let browser_plugin: Option<config::PluginConfig> = all_plugins.iter()
        .find(|p| p.position == "browser")
        .cloned();

    let sidebar = Rc::new(RefCell::new(SidebarState::new(
        config.sidebar_width,
        config.right_sidebar_width,
        config.top_bar_height,
        tasku_path,
        left_ext_plugins,
        right_ext_plugins.clone(),
        top_ext_plugins.clone(),
        config.disable_tasku,
        config.disable_priorities,
        config.disable_workspaces,
        config.tasku_position.clone(),
    )));
    {
        let saved_order = workspace::load_plugin_order();
        if !saved_order.is_empty() {
            sidebar.borrow_mut().apply_order(&saved_order);
        }
    }
    ui.set_sidebar_width(config.sidebar_width);
    ui.set_right_sidebar_width(config.right_sidebar_width);
    ui.set_top_bar_height(config.top_bar_height);

    // ── Task runner ──────────────────────────────────────────────────────────
    let runner_tasks: Rc<std::collections::HashMap<String, String>> =
        Rc::new(workspace::load_runner_tasks());
    let runner_is_running: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));

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

    // Remove workspace files for projects no longer in Tasku.
    {
        let valid_codes: Vec<String> = fetched.iter().map(|fp| fp.code.clone()).collect();
        workspace::prune_stale_workspaces(&valid_codes);
    }

    let code_to_name: Rc<HashMap<String, String>> = Rc::new(
        fetched.iter().map(|fp| (fp.code.clone(), fp.name.clone())).collect()
    );

    // Project root paths from ~/.config/mado/projects.toml
    let project_paths: Rc<HashMap<String, String>> = Rc::new(workspace::load_project_paths());

    // Optional default project to activate on launch (projects.toml `default` key)
    let default_project: Option<String> = workspace::load_default_project();

    // Per-project icons from projects.toml `icon` field
    let project_icons: HashMap<String, String> = workspace::load_project_icons();

    // Workspace tiles (code + colour + icon)
    let ws_model: Rc<VecModel<WorkspaceProject>> = {
        let ws_projects: Vec<WorkspaceProject> = fetched.iter().map(|fp| {
            let icon = project_icons.get(&fp.code)
                .or_else(|| project_icons.get(&fp.name))
                .cloned()
                .unwrap_or_default();
            WorkspaceProject {
                code:       fp.code.clone().into(),
                color:      fp.color,
                text_color: fp.text_color,
                icon:       icon.into(),
            }
        }).collect();
        Rc::new(VecModel::<WorkspaceProject>::from(ws_projects))
    };
    ui.set_ws_projects(ModelRc::new(Rc::clone(&ws_model)));
    ui.set_tasku_status(tasku_status_str.into());

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

    // Push initial left sidebar plugin list
    {
        let right_items: Vec<PluginItem> = sidebar.borrow().right_items().into_iter().map(
            |(id, title, subtitle, icon, plugin_index)| PluginItem {
                id:           id.into(),
                title:        title.into(),
                subtitle:     subtitle.into(),
                icon:         icon.into(),
                plugin_index,
            }
        ).collect();
        ui.set_right_plugins(ModelRc::new(Rc::new(VecModel::from(right_items))));
    }

    // Determine show-right-sidebar (right sidebar auto-shows when plugins exist).
    // Top and bottom bars intentionally start hidden regardless of config so that
    // the first Cmd+T / Cmd+B press always shows them (predictable first-use UX).
    let has_top_plugins = {
        let s = sidebar.borrow();
        !s.top_items().is_empty()
    };
    let has_right_plugins = {
        let s = sidebar.borrow();
        !s.right_items().is_empty()
    };
    ui.set_show_right_sidebar(has_right_plugins);
    ui.set_show_top_bar(config.should_show_top_bar(has_top_plugins));
    ui.set_show_bottom_bar(config.should_show_bottom_bar());

    // Push top bar plugin list
    {
        let top_items: Vec<PluginItem> = sidebar.borrow().top_items().into_iter().map(
            |(id, title, subtitle, icon, plugin_index)| PluginItem {
                id:           id.into(),
                title:        title.into(),
                subtitle:     subtitle.into(),
                icon:         icon.into(),
                plugin_index,
            }
        ).collect();
        ui.set_top_plugins(ModelRc::new(Rc::new(VecModel::from(top_items))));
    }

    // ── External plugin state models (left sidebar) ──────────────────────────
    let plugin_expanded_model: Rc<VecModel<bool>> =
        Rc::new(VecModel::from(vec![false; num_left_ext]));
    let (saved_left_h, saved_right_h) = workspace::load_panel_heights();
    let plugin_panel_h_model: Rc<VecModel<f32>> = Rc::new(VecModel::from(
        (0..num_left_ext).map(|i| {
            saved_left_h.get(i).copied().unwrap_or(DEFAULT_PLUGIN_PANEL_H)
        }).collect::<Vec<_>>()
    ));
    let plugin_images_model: Rc<VecModel<Image>> =
        Rc::new(VecModel::from(vec![Image::default(); num_left_ext]));
    let plugin_pixel_model: Rc<VecModel<bool>> = Rc::new(VecModel::from(
        sidebar.borrow().left_ext_plugins.iter().map(|p| p.kind == "pixel").collect::<Vec<_>>()
    ));

    ui.set_plugin_expanded(ModelRc::new(Rc::clone(&plugin_expanded_model)));
    ui.set_plugin_panel_h(ModelRc::new(Rc::clone(&plugin_panel_h_model)));
    ui.set_plugin_images(ModelRc::new(Rc::clone(&plugin_images_model)));
    ui.set_plugin_pixel(ModelRc::new(Rc::clone(&plugin_pixel_model)));

    // ── External plugin state models (right sidebar) ─────────────────────────
    let right_plugin_expanded_model: Rc<VecModel<bool>> =
        Rc::new(VecModel::from(vec![false; num_right_ext]));
    let right_plugin_panel_h_model: Rc<VecModel<f32>> = Rc::new(VecModel::from(
        (0..num_right_ext).map(|i| {
            saved_right_h.get(i).copied().unwrap_or(DEFAULT_PLUGIN_PANEL_H)
        }).collect::<Vec<_>>()
    ));
    let right_plugin_images_model: Rc<VecModel<Image>> =
        Rc::new(VecModel::from(vec![Image::default(); num_right_ext]));
    let right_plugin_pixel_model: Rc<VecModel<bool>> = Rc::new(VecModel::from(
        right_ext_plugins.iter().map(|p| p.kind == "pixel").collect::<Vec<_>>()
    ));

    ui.set_right_plugin_expanded(ModelRc::new(Rc::clone(&right_plugin_expanded_model)));
    ui.set_right_plugin_panel_h(ModelRc::new(Rc::clone(&right_plugin_panel_h_model)));
    ui.set_right_plugin_images(ModelRc::new(Rc::clone(&right_plugin_images_model)));
    ui.set_right_plugin_pixel(ModelRc::new(Rc::clone(&right_plugin_pixel_model)));

    // ── External plugin state models (top bar) ───────────────────────────────
    let top_plugin_images_model: Rc<VecModel<Image>> =
        Rc::new(VecModel::from(vec![Image::default(); num_top_ext]));
    let top_plugin_pixel_model: Rc<VecModel<bool>> = Rc::new(VecModel::from(
        top_ext_plugins.iter().map(|p| p.kind == "pixel").collect::<Vec<_>>()
    ));

    ui.set_top_plugin_images(ModelRc::new(Rc::clone(&top_plugin_images_model)));
    ui.set_top_plugin_pixel(ModelRc::new(Rc::clone(&top_plugin_pixel_model)));

    // ── Top-right / bottom-right / browser panel plugin UI setup ────────────
    let top_right_panel_w = if top_right_plugin.is_some() { config.top_right_panel_width } else { 0.0 };
    let bottom_right_panel_w = if bottom_right_plugin.is_some() { config.bottom_right_panel_width } else { 0.0 };
    let browser_panel_w = if browser_plugin.is_some() { config.browser_panel_width } else { 0.0 };
    ui.set_top_right_panel_width(top_right_panel_w);
    ui.set_bottom_right_panel_width(bottom_right_panel_w);
    ui.set_browser_panel_width(browser_panel_w);
    if let Some(ref p) = top_right_plugin {
        ui.set_top_right_plugin_icon(p.icon.as_str().into());
        ui.set_top_right_plugin_title(p.id.to_uppercase().as_str().into());
    }
    if let Some(ref p) = bottom_right_plugin {
        ui.set_bottom_right_plugin_icon(p.icon.as_str().into());
        ui.set_bottom_right_plugin_title(p.id.as_str().into());
    }

    // index → PixelPlugin for plugins with kind = "pixel" (left sidebar)
    let pixel_plugins: Rc<RefCell<std::collections::HashMap<usize, PixelPlugin>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    // index → PixelPlugin for right sidebar pixel plugins
    let right_pixel_plugins: Rc<RefCell<std::collections::HashMap<usize, PixelPlugin>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    // PixelPlugin for the top-right panel when kind = "pixel"
    let top_right_pixel: Rc<RefCell<Option<PixelPlugin>>> = Rc::new(RefCell::new(None));
    // Pending clipboard paste result for top-right pixel plugin
    let top_right_paste_result: Rc<RefCell<Option<Arc<std::sync::Mutex<Option<Vec<u8>>>>>>> =
        Rc::new(RefCell::new(None));
    // Native browser panel (WKWebView embedded in NSWindow).
    let native_browser: Rc<RefCell<Option<browser::NativeBrowser>>> =
        Rc::new(RefCell::new(None));

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

    // Keyboard-driven selection anchor. Set when Shift+Arrow starts; cleared
    // with the selection. The Selection's head tracks the moving end.
    let kbd_anchor: Rc<RefCell<Option<(usize, usize)>>> = Rc::new(RefCell::new(None));

    // ── Float plugin state ────────────────────────────────────────────────────
    let float_plugin: Rc<RefCell<Option<(usize, bool)>>> = // (index, is_right)
        Rc::new(RefCell::new(None));

    // Initial terminal is spawned lazily inside on_window_resized so we have
    // real PTY dimensions and can prepend the welcome banner before the shell prompt.
    let initial_spawned: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));
    // Tracks whether the top-bar tasku overlay is currently open.
    // Used by on_window_resized to re-run tasku list after a resize.
    let tasku_open: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));
    // True until the user types anything in the root pane. While true, window
    // resize events re-center the welcome banner to match the new column count.
    let banner_active: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(true));
    *focused_id.borrow_mut() = Some(tree.borrow().root);

    // Shared pending paste: key handler runs pbpaste on a background thread,
    // timer drains the result via try_lock. Avoids blocking the UI thread.
    let pending_paste: Rc<RefCell<Option<(NodeId, Arc<std::sync::Mutex<Option<Vec<u8>>>>)>>> =
        Rc::new(RefCell::new(None));

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
        let right_plugin_images_model = Rc::clone(&right_plugin_images_model);
        let _top_plugin_images_model = Rc::clone(&top_plugin_images_model);
        let top_right_plugin_timer = top_right_plugin.clone();
        let bottom_right_plugin_timer = bottom_right_plugin.clone();
        let pixel_plugins = Rc::clone(&pixel_plugins);
        let right_pixel_plugins = Rc::clone(&right_pixel_plugins);
        let float_plugin = Rc::clone(&float_plugin);
        let pending_paste = Rc::clone(&pending_paste);
        let top_right_pixel = Rc::clone(&top_right_pixel);
        let top_right_paste_result = Rc::clone(&top_right_paste_result);
        let ui_weak = ui.as_weak();
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, std::time::Duration::from_millis(16), move || {
            let sel = {
                let s = selection.borrow();
                s.as_ref().map(|s| (s.pane_id, s.normalized()))
            };
            // Drain async paste (background thread runs pbpaste, stores result here).
            {
                let paste_ready: Option<(NodeId, Vec<u8>)> = {
                    let guard = pending_paste.borrow();
                    if let Some((paste_id, result)) = guard.as_ref() {
                        if let Ok(mut lock) = result.try_lock() {
                            lock.take().map(|data| (*paste_id, data))
                        } else { None }
                    } else { None }
                };
                if let Some((paste_id, mut data)) = paste_ready {
                    if data.ends_with(b"\r\n") { data.truncate(data.len() - 2); }
                    else if data.ends_with(b"\n") { data.truncate(data.len() - 1); }
                    if !data.is_empty() {
                        registry.borrow_mut().write_key(paste_id, &data);
                    }
                    *pending_paste.borrow_mut() = None;
                }
            }

            // Play a sound for any pane that rang the bell while not focused.
            {
                let focused = *focused_id.borrow();
                let bells = registry.borrow_mut().drain_bells();
                for bell_id in bells {
                    if focused != Some(bell_id) {
                        let _ = std::process::Command::new("afplay")
                            .arg("/System/Library/Sounds/Glass.aiff")
                            .spawn();
                    }
                }
            }

            let dirty_bufs = registry.borrow_mut().drain_dirty(sel);

            let mut tasku_buf:       Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> = None;
            let mut runner_buf:      Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> = None;
            let mut top_right_buf:   Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> = None;
            let mut bottom_right_buf:Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> = None;
            // plugin_index → pixel buffer for left external plugins
            let mut plugin_bufs: Vec<(usize, slint::SharedPixelBuffer<slint::Rgba8Pixel>)> = Vec::new();
            // plugin_index → pixel buffer for right external plugins
            let mut right_plugin_bufs: Vec<(usize, slint::SharedPixelBuffer<slint::Rgba8Pixel>)> = Vec::new();
            let mut pane_dirty_ids: Vec<NodeId> = Vec::new();
            let mut imgs = images.borrow_mut();

            for (id, buf) in dirty_bufs {
                if id == SIDEBAR_TASKU_ID {
                    tasku_buf = Some(buf);
                } else if id == RUNNER_ID {
                    runner_buf = Some(buf);
                } else if id < SIDEBAR_TASKU_ID && id > u32::MAX - 1 - num_left_ext as u32 {
                    // Left external plugin NodeId: u32::MAX - 1 - i
                    let i = (u32::MAX - 1 - id) as usize;
                    if i < num_left_ext {
                        plugin_bufs.push((i, buf));
                    }
                } else if num_right_ext > 0
                    && id <= u32::MAX / 2
                    && id >= u32::MAX / 2 - (num_right_ext as u32).saturating_sub(1)
                {
                    // Right external plugin NodeId: u32::MAX / 2 - i
                    let i = (u32::MAX / 2 - id) as usize;
                    if i < num_right_ext {
                        right_plugin_bufs.push((i, buf));
                    }
                } else if top_right_plugin_timer.is_some() && id == top_right_plugin_node_id(0) {
                    top_right_buf = Some(buf);
                } else if bottom_right_plugin_timer.is_some() && id == bottom_right_plugin_node_id(0) {
                    bottom_right_buf = Some(buf);
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
                        if let Some(paste_id) = *focused_id.borrow() {
                            let result: Arc<std::sync::Mutex<Option<Vec<u8>>>> =
                                Arc::new(std::sync::Mutex::new(None));
                            let result2 = Arc::clone(&result);
                            std::thread::spawn(move || {
                                if let Ok(out) = std::process::Command::new("pbpaste").output() {
                                    *result2.lock().unwrap() = Some(out.stdout);
                                }
                            });
                            *pending_paste.borrow_mut() = Some((paste_id, result));
                        }
                    }
                }
            }

            // Drain dirty right sidebar pixel plugins
            {
                let mut pp = right_pixel_plugins.borrow_mut();
                for (i, plugin) in pp.iter_mut() {
                    if plugin.dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        if let Ok(guard) = plugin.image.lock() {
                            if let Some(ref buf) = *guard {
                                right_plugin_bufs.push((*i, buf.clone()));
                            }
                        }
                    }
                }
            }

            // Drain dirty top-right pixel plugin
            {
                let mut trp = top_right_pixel.borrow_mut();
                if let Some(pp) = trp.as_mut() {
                    if pp.dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        if let Ok(guard) = pp.image.lock() {
                            if let Some(ref buf) = *guard {
                                top_right_buf = Some(buf.clone());
                            }
                        }
                    }
                }
            }
            // Drain top-right pixel plugin paste
            {
                let paste = top_right_paste_result.borrow();
                if let Some(arc) = paste.as_ref() {
                    let mut locked = arc.lock().unwrap();
                    if let Some(bytes) = locked.take() {
                        drop(locked);
                        drop(paste);
                        if let Ok(text) = String::from_utf8(bytes) {
                            if let Some(pp) = top_right_pixel.borrow_mut().as_mut() {
                                pp.send_paste(&text);
                            }
                        }
                        *top_right_paste_result.borrow_mut() = None;
                    }
                }
            }
            if pane_dirty_ids.is_empty() && tasku_buf.is_none() && runner_buf.is_none()
                && plugin_bufs.is_empty() && right_plugin_bufs.is_empty()
                && top_right_buf.is_none() && bottom_right_buf.is_none()
            {
                return;
            }

            if !pane_dirty_ids.is_empty() {
                push_images(&pane_model, &imgs, *focused_id.borrow(), Some(&pane_dirty_ids));
            }

            if tasku_buf.is_some() || runner_buf.is_some()
                || !plugin_bufs.is_empty() || !right_plugin_bufs.is_empty()
                || top_right_buf.is_some() || bottom_right_buf.is_some()
            {
                if let Some(ui) = ui_weak.upgrade() {
                    if let Some(buf) = tasku_buf {
                        ui.set_tasku_terminal_image(Image::from_rgba8(buf));
                    }
                    if let Some(buf) = runner_buf {
                        ui.set_runner_terminal_image(Image::from_rgba8(buf));
                    }
                    if let Some(buf) = top_right_buf {
                        ui.set_top_right_plugin_image(Image::from_rgba8(buf));
                    }
                    if let Some(buf) = bottom_right_buf {
                        ui.set_bottom_right_plugin_image(Image::from_rgba8(buf));
                    }
                    for (i, buf) in plugin_bufs {
                        let img = Image::from_rgba8(buf);
                        // Also route to float overlay if this plugin is floated
                        if float_plugin.borrow().as_ref() == Some(&(i, false)) {
                            ui.set_float_plugin_image(img.clone());
                        }
                        plugin_images_model.set_row_data(i, img);
                    }
                    for (i, buf) in right_plugin_bufs {
                        let img = Image::from_rgba8(buf);
                        if float_plugin.borrow().as_ref() == Some(&(i, true)) {
                            ui.set_float_plugin_image(img.clone());
                        }
                        right_plugin_images_model.set_row_data(i, img);
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
            let remap = tree.borrow_mut().close(id as u32);

            // When a leaf sibling is promoted into the parent slot its node ID
            // changes.  Remap the images and PTY session to the new key so the
            // surviving pane keeps its content.
            let new_focus = if let Some((old_id, new_id)) = remap {
                // Extract first so the RefMut is dropped before the next borrow.
                let img = images.borrow_mut().remove(&old_id);
                if let Some(img) = img {
                    images.borrow_mut().insert(new_id, img);
                }
                registry.borrow_mut().remap_id(old_id, new_id);
                Some(new_id)
            } else {
                tree.borrow().leaf_ids().first().copied()
            };
            *focused_id.borrow_mut() = new_focus;

            if let Some(ui) = ui_weak.upgrade() {
                // Resize every surviving pane's PTY to its new geometry.
                // Without this the terminal renders at the old (split) size and
                // the image gets stretched, making text appear giant.
                let w = ui.get_window_w();
                let h = ui.get_window_h();
                let panes = tree.borrow().flatten(w, h);
                let mut reg = registry.borrow_mut();
                for p in &panes {
                    reg.resize(p.id,
                        (p.width  - PANE_H_INSET).max(10.0),
                        (p.height - PANE_TOP_INSET).max(10.0));
                }
                drop(reg);

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
        let kbd_anchor = Rc::clone(&kbd_anchor);
        let pending_paste = Rc::clone(&pending_paste);
        let banner_active = Rc::clone(&banner_active);
        let ui_weak = ui.as_weak();
        move |id, text, ctrl, meta, alt, shift| {
            if *focused_id.borrow() != Some(id as NodeId) { return; }

            let t = text.as_str();
            let has_sel = {
                let sel = selection.borrow();
                sel.as_ref().map_or(false, |s| !s.is_empty() && s.pane_id == id as NodeId)
            };

            // Clear active selection on regular keystrokes (before acting on the key).
            if keys::should_clear_selection(t, ctrl, meta, shift) {
                *kbd_anchor.borrow_mut() = None;
                let prev_id = selection.borrow().as_ref().map(|s| s.pane_id);
                *selection.borrow_mut() = None;
                if let Some(sid) = prev_id {
                    registry.borrow().mark_dirty(sid);
                }
            }

            match keys::classify_key(t, ctrl, meta, alt, shift, has_sel) {
                keys::KeyAction::CopySelection => {
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
                    let prev_id = selection.borrow().as_ref().map(|s| s.pane_id);
                    *selection.borrow_mut() = None;
                    if let Some(sid) = prev_id {
                        registry.borrow().mark_dirty(sid);
                    }
                }
                keys::KeyAction::Nothing => {}
                keys::KeyAction::Paste => {
                    let result: Arc<std::sync::Mutex<Option<Vec<u8>>>> =
                        Arc::new(std::sync::Mutex::new(None));
                    let result2 = Arc::clone(&result);
                    std::thread::spawn(move || {
                        if let Ok(out) = std::process::Command::new("pbpaste").output() {
                            *result2.lock().unwrap() = Some(out.stdout);
                        }
                    });
                    *pending_paste.borrow_mut() = Some((id as NodeId, result));
                }
                keys::KeyAction::ZoomIn => {
                    let new_size = (*font_size.borrow() + 1.0).min(40.0);
                    if let Some(ui) = ui_weak.upgrade() {
                        do_zoom(&ui, new_size, &font_size, &registry, &tree,
                                &pane_model, &images, &focused_id);
                    }
                }
                keys::KeyAction::ZoomOut => {
                    let new_size = (*font_size.borrow() - 1.0).max(7.0);
                    if let Some(ui) = ui_weak.upgrade() {
                        do_zoom(&ui, new_size, &font_size, &registry, &tree,
                                &pane_model, &images, &focused_id);
                    }
                }
                keys::KeyAction::ZoomReset => {
                    if let Some(ui) = ui_weak.upgrade() {
                        do_zoom(&ui, default_font_size, &font_size, &registry, &tree,
                                &pane_model, &images, &focused_id);
                    }
                }
                keys::KeyAction::ArrowSeq(seq) => {
                    registry.borrow_mut().write_key(id as NodeId, &seq);
                }
                keys::KeyAction::ShiftArrow { dcol, drow, seq } => {
                    let info = registry.borrow().cursor_info(id as NodeId);
                    if let Some((cur_col, cur_row, cols, rows, in_alt)) = info {
                        if in_alt {
                            // Alt screen (nvim/helix): let the app handle selection.
                            registry.borrow_mut().write_key(id as NodeId, &seq);
                        } else {
                            // Normal shell: drive Mado's own selection.
                            let anchor = {
                                let mut a = kbd_anchor.borrow_mut();
                                if a.is_none() {
                                    *a = Some((cur_col, cur_row));
                                }
                                a.unwrap()
                            };
                            // The selection head tracks the moving end.
                            let prev_head = selection.borrow()
                                .as_ref()
                                .filter(|s| s.pane_id == id as NodeId)
                                .map(|s| s.head)
                                .unwrap_or((cur_col, cur_row));
                            let new_col = (prev_head.0 as i32 + dcol as i32)
                                .clamp(0, cols as i32 - 1) as usize;
                            let new_row = (prev_head.1 as i32 + drow as i32)
                                .clamp(0, rows as i32 - 1) as usize;
                            *selection.borrow_mut() = Some(Selection {
                                pane_id: id as NodeId,
                                anchor,
                                head: (new_col, new_row),
                            });
                            registry.borrow().mark_dirty(id as NodeId);
                        }
                    }
                }
                keys::KeyAction::SelectToLineEdge { to_end } => {
                    let info = registry.borrow().cursor_info(id as NodeId);
                    if let Some((cur_col, cur_row, cols, _rows, _in_alt)) = info {
                        let anchor = {
                            let mut a = kbd_anchor.borrow_mut();
                            if a.is_none() { *a = Some((cur_col, cur_row)); }
                            a.unwrap()
                        };
                        let head_col = if to_end { cols.saturating_sub(1) } else { 0 };
                        *selection.borrow_mut() = Some(Selection {
                            pane_id: id as NodeId,
                            anchor,
                            head: (head_col, cur_row),
                        });
                        registry.borrow().mark_dirty(id as NodeId);
                    }
                }
                keys::KeyAction::ModifierOnly => {}
                keys::KeyAction::Forward(bytes) => {
                    banner_active.set(false);
                    registry.borrow_mut().write_key(id as NodeId, &bytes);
                }
            }
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

    // ── Directional focus (keyboard shortcuts) ───────────────────────────────
    ui.on_pane_focus_neighbor({
        let tree       = Rc::clone(&tree);
        let focused_id = Rc::clone(&focused_id);
        let pane_model = Rc::clone(&pane_model);
        let images     = Rc::clone(&images);
        let ui_weak    = ui.as_weak();
        move |id, dir| {
            let nav = match dir {
                0 => NavDir::Left,
                1 => NavDir::Right,
                2 => NavDir::Up,
                _ => NavDir::Down,
            };
            if let Some(ui) = ui_weak.upgrade() {
                let w = ui.get_window_w();
                let h = ui.get_window_h();
                if let Some(neighbor) = tree.borrow().neighbor(id as NodeId, nav, w, h) {
                    *focused_id.borrow_mut() = Some(neighbor);
                    push_images(&pane_model, &images.borrow(), Some(neighbor), None);
                }
            }
        }
    });

    // ── Pane scroll ──────────────────────────────────────────────────────────
    ui.on_pane_scroll({
        let registry  = Rc::clone(&registry);
        let scroll_acc = Rc::clone(&scroll_acc);
        move |id, delta| {
            let id = id as NodeId;
            let cell_h = {
                let reg = registry.borrow();
                reg.font.cell_h as f32 / reg.scale
            };
            if cell_h <= 0.0 { return; }
            let mut acc = scroll_acc.borrow_mut();
            let entry = acc.entry(id).or_insert(0.0);
            *entry += delta * scroll_dir;
            let rows = (*entry / cell_h) as i32;
            if rows != 0 {
                *entry -= rows as f32 * cell_h;
                registry.borrow_mut().scroll(id, rows);
            }
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
        let sidebar = Rc::clone(&sidebar);
        let dividers_cache = Rc::clone(&dividers_cache);
        let pane_model = Rc::clone(&pane_model);
        let div_model = Rc::clone(&div_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let initial_spawned = Rc::clone(&initial_spawned);
        let banner_active = Rc::clone(&banner_active);
        let loaded_theme = Rc::clone(&loaded_theme);
        let active_project_resize = Rc::clone(&active_project);
        let code_to_name_resize = Rc::clone(&code_to_name);
        let top_right_plugin_resize = top_right_plugin.clone();
        let bottom_right_plugin_resize = bottom_right_plugin.clone();
        let top_right_pixel_resize = Rc::clone(&top_right_pixel);
        let browser_plugin_resize = browser_plugin.clone();
        let native_browser_resize = Rc::clone(&native_browser);
        let tasku_open_resize = Rc::clone(&tasku_open);
        let ui_weak = ui.as_weak();
        let default_project = default_project.clone();
        let last_size: Rc<RefCell<(f32, f32)>> = Rc::new(RefCell::new((0.0, 0.0)));
        move |w, h| {
            let (lw, lh) = *last_size.borrow();
            if (w - lw).abs() > 0.5 || (h - lh).abs() > 0.5 {
                *last_size.borrow_mut() = (w, h);
                let panes = tree.borrow().flatten(w, h);
                let mut reg = registry.borrow_mut();

                // Tasku top-bar sizing: overlay covers full window width and the
                // height below the 52px header (= terminal-area h, since the
                // VerticalLayout places them sequentially).
                let (tasku_pos, tasku_found) = {
                    let s = sidebar.borrow();
                    (s.tasku_position.clone(), s.tasku_path.is_some())
                };
                let tasku_in_top = tasku_pos == "top" && tasku_found;
                // overlay_w = full window width from Slint (accounts for icon-only sidebars).
                let overlay_w = ui_weak.upgrade()
                    .map(|ui| ui.get_overlay_total_w())
                    .unwrap_or(w + 300.0);
                // overlay_h = terminal-area height (window height minus 52px header)
                let overlay_h = h - 52.0;

                if !initial_spawned.get() && w > 50.0 && h > 50.0 {
                    initial_spawned.set(true);
                    if let Some(ref code) = default_project {
                        // Activate the default project instead of showing the welcome banner.
                        // Deferred via a 0ms timer so the registry borrow above is released first.
                        let code = code.clone();
                        let ui_weak2 = ui_weak.clone();
                        Timer::single_shot(std::time::Duration::from_millis(0), move || {
                            if let Some(ui) = ui_weak2.upgrade() {
                                ui.invoke_workspace_selected(code.into());
                            }
                        });
                    } else {
                        let root_id = tree.borrow().root;
                        if let Some(p) = panes.iter().find(|p| p.id == root_id) {
                            let pane_w = (p.width  - PANE_H_INSET).max(10.0);
                            let pane_h = (p.height - PANE_TOP_INSET).max(10.0);
                            let cols   = reg.logical_to_cols(pane_w);
                            let banner = welcome_banner(cols, &loaded_theme);
                            reg.spawn_with_banner(root_id, pane_w, pane_h, None, &banner);
                        }
                    }
                    if tasku_in_top {
                        let (term_w, term_h) = tasku_top_bar_size(overlay_w, overlay_h);
                        let term_w = term_w.min(reg.max_logical_w(TASKU_MAX_COLS));
                        reg.spawn(SIDEBAR_TASKU_ID, term_w, term_h, None);
                        let tasku_cols = reg.cols(SIDEBAR_TASKU_ID);
                        eprintln!("mado: tasku PTY spawned — overlay_w={overlay_w} term_w={term_w} cols={tasku_cols}");
                        let _ = std::fs::write(TASKU_COLS_FILE, tasku_cols.to_string());
                        reg.write_key(SIDEBAR_TASKU_ID,
                            format!("export MADO=1; export TASKU_SEL_FILE={TASKU_SEL_FILE}\n").as_bytes());
                        // Do NOT run tasku list here — the window may not yet be at its
                        // final size (e.g. fullscreen startup). tasku list is deferred to
                        // the first time the user opens the overlay (on_tasku_toggled).
                    }
                    // Spawn top-right panel plugin
                    if let Some(ref plugin) = top_right_plugin_resize {
                        let mut parts = plugin.command.split_whitespace();
                        let program = parts.next().unwrap_or("").to_string();
                        let args: Vec<String> = parts.map(|s| s.to_string()).collect();
                        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        if plugin.kind == "pixel" {
                            let phys_w = ((top_right_panel_w - PANE_H_INSET as f32).max(10.0) * reg.scale) as u32;
                            let phys_h = (h.max(50.0) * reg.scale) as u32;
                            let fs_str = reg.font_size.to_string();
                            let sc_str = reg.scale.to_string();
                            if let Some(pp) = PixelPlugin::spawn(&program, &args_ref, phys_w, phys_h,
                                &[("MADO_FONT_SIZE", &fs_str), ("MADO_SCALE", &sc_str)]) {
                                *top_right_pixel_resize.borrow_mut() = Some(pp);
                            } else {
                                eprintln!("mado: failed to spawn top-right pixel plugin '{}'", plugin.id);
                            }
                        } else {
                            let plugin_id = plugin.id.clone();
                            reg.spawn_cmd(top_right_plugin_node_id(0), (top_right_panel_w - PANE_H_INSET as f32).max(10.0), h.max(50.0),
                                          &program, &args_ref, None,
                                          &[("MADO_PLUGIN_ID", &plugin_id)]);
                        }
                    }
                    // Spawn bottom-right panel plugin
                    if let Some(ref plugin) = bottom_right_plugin_resize {
                        let mut parts = plugin.command.split_whitespace();
                        let program = parts.next().unwrap_or("").to_string();
                        let args: Vec<String> = parts.map(|s| s.to_string()).collect();
                        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        let plugin_id = plugin.id.clone();
                        reg.spawn_cmd(bottom_right_plugin_node_id(0), bottom_right_panel_w, (h - 52.0).max(50.0),
                                      &program, &args_ref, None,
                                      &[("MADO_PLUGIN_ID", &plugin_id)]);
                    }
                    // Create native browser WKWebView (hidden; shown on first toggle)
                    if browser_plugin_resize.is_some() {
                        if let Some(ui) = ui_weak.upgrade() {
                            let initial_url = browser_plugin_resize.as_ref()
                                .map(|p| p.command.as_str())
                                .filter(|s| s.starts_with("http"))
                                .unwrap_or(browser::DEFAULT_BROWSER_URL)
                                .to_string();
                            #[cfg(target_os = "macos")]
                            ui.window().with_winit_window(|win| {
                                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                                if let Ok(handle) = win.window_handle() {
                                    if let RawWindowHandle::AppKit(appkit_h) = handle.as_raw() {
                                        let ns_view = appkit_h.ns_view.as_ptr()
                                            as *mut objc2::runtime::AnyObject;
                                        // Start with a placeholder frame; real frame is set
                                        // the first time the panel becomes visible.
                                        if let Some(nb) = browser::NativeBrowser::new(
                                            ns_view, 0.0, 0.0,
                                            browser_panel_w as f64, h as f64,
                                            &initial_url,
                                        ) {
                                            *native_browser_resize.borrow_mut() = Some(nb);
                                        }
                                    }
                                }
                            });
                        }
                    }
                } else {
                    for p in &panes {
                        reg.resize(p.id, (p.width - PANE_H_INSET).max(10.0), (p.height - PANE_TOP_INSET).max(10.0));
                    }
                    // Re-center the welcome banner when the window is resized before
                    // the user has typed anything (e.g. going fullscreen on launch).
                    if banner_active.get() {
                        let root_id = tree.borrow().root;
                        if let Some(p) = panes.iter().find(|p| p.id == root_id) {
                            let pane_w = (p.width - PANE_H_INSET).max(10.0);
                            let cols   = reg.logical_to_cols(pane_w);
                            let banner = welcome_banner(cols, &loaded_theme);
                            reg.reinject_banner(root_id, &banner);
                        }
                    }
                    if tasku_in_top {
                        let (term_w, term_h) = tasku_top_bar_size(overlay_w, overlay_h);
                        let term_w = term_w.min(reg.max_logical_w(TASKU_MAX_COLS));
                        reg.resize(SIDEBAR_TASKU_ID, term_w, term_h);
                        let tasku_cols = reg.cols(SIDEBAR_TASKU_ID);
                        eprintln!("mado: tasku PTY resized — overlay_w={overlay_w} term_w={term_w} cols={tasku_cols}");
                        let _ = std::fs::write(TASKU_COLS_FILE, tasku_cols.to_string());
                        // If the panel is already open, re-run tasku list at the new width.
                        if tasku_open_resize.get() {
                            let pf = active_project_resize.borrow().as_ref()
                                .and_then(|code| code_to_name_resize.get(code))
                                .map(|name| format!(" --project {name}"))
                                .unwrap_or_default();
                            reg.write_key(SIDEBAR_TASKU_ID,
                                format!("\x03tasku list{pf}\n").as_bytes());
                        }
                    }
                    // Resize top-right panel plugin
                    if let Some(ref plugin) = top_right_plugin_resize {
                        if plugin.kind == "pixel" {
                            let phys_w = ((top_right_panel_w - PANE_H_INSET as f32).max(10.0) * reg.scale) as u32;
                            let phys_h = (h.max(50.0) * reg.scale) as u32;
                            if let Some(pp) = top_right_pixel_resize.borrow_mut().as_mut() {
                                pp.send_resize(phys_w, phys_h);
                            }
                        } else {
                            reg.resize(top_right_plugin_node_id(0), (top_right_panel_w - PANE_H_INSET as f32).max(10.0), h.max(50.0));
                        }
                    }
                    // Resize bottom-right panel plugin
                    if bottom_right_plugin_resize.is_some() {
                        reg.resize(bottom_right_plugin_node_id(0), bottom_right_panel_w, (h - 52.0).max(50.0));
                    }
                    // Update native browser WKWebView frame on window resize
                    if let Some(nb) = native_browser_resize.borrow().as_ref() {
                        if let Some(ui) = ui_weak.upgrade() {
                            let x = ui.get_browser_panel_logical_x() as f64;
                            let y = ui.get_bottom_bar_logical_h() as f64;
                            nb.update_frame(x, y, browser_panel_w as f64, h as f64);
                        }
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
            let (tasku_h, tasku_pos) = {
                let mut s = sidebar.borrow_mut();
                s.width = new_width;
                (s.tasku_panel_h, s.tasku_position.clone())
            };
            let sidebar_w = (new_width - 24.0).max(50.0);
            let tasku_sidebar_w = ((new_width - 17.0).max(50.0) * TASKU_SIDEBAR_COL_BOOST).round();
            let mut reg = registry.borrow_mut();
            // Only resize tasku here if it lives in the left sidebar
            if tasku_pos == "left" || tasku_pos.is_empty() {
                reg.resize(SIDEBAR_TASKU_ID, tasku_sidebar_w, tasku_terminal_h(tasku_h));
            }
            // Resize any open left external plugins
            let mut pp = pixel_plugins.borrow_mut();
            for i in 0..num_left_ext {
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
        let tasku_open_toggle = Rc::clone(&tasku_open);
        move |expanded| {
            let tasku_pos = sidebar.borrow().tasku_position.clone();
            if tasku_pos == "top" {
                tasku_open_toggle.set(expanded);
                if expanded {
                    // Defer tasku list by 500ms so the window finishes any
                    // resize animation before we read the PTY column count.
                    // This avoids the startup race where the panel opens while
                    // the window is still animating to fullscreen at 84 cols.
                    let registry2 = Rc::clone(&registry);
                    let active_project2 = Rc::clone(&active_project);
                    let code_to_name2 = Rc::clone(&code_to_name);
                    Timer::single_shot(std::time::Duration::from_millis(500), move || {
                        let cols = registry2.borrow().cols(SIDEBAR_TASKU_ID);
                        let _ = std::fs::write(TASKU_COLS_FILE, cols.to_string());
                        let pf = active_project2.borrow().as_ref()
                            .and_then(|code| code_to_name2.get(code))
                            .map(|name| format!(" --project {name}"))
                            .unwrap_or_default();
                        registry2.borrow_mut().write_key(
                            SIDEBAR_TASKU_ID,
                            format!("\x03tasku list{pf}\n").as_bytes(),
                        );
                    });
                }
                return;
            }
            let mut reg = registry.borrow_mut();
            if expanded {
                let (tasku_w, panel_h) = {
                    let s = sidebar.borrow();
                    (((s.width - 17.0).max(50.0) * TASKU_SIDEBAR_COL_BOOST).round(), s.tasku_panel_h)
                };
                reg.spawn(SIDEBAR_TASKU_ID, tasku_w, tasku_terminal_h(panel_h), None);
                let cols = reg.cols(SIDEBAR_TASKU_ID);
                let _ = std::fs::write(TASKU_COLS_FILE, cols.to_string());
                reg.write_key(SIDEBAR_TASKU_ID,
                    format!("export MADO=1; export TASKU_SEL_FILE={TASKU_SEL_FILE}\n").as_bytes());
                reg.write_key(SIDEBAR_TASKU_ID, b"tasku list\n");
            } else {
                reg.remove(SIDEBAR_TASKU_ID);
            }
        }
    });

    // ── Tasku mouse selection ────────────────────────────────────────────────
    ui.on_tasku_mouse_press({
        let registry = Rc::clone(&registry);
        let selection = Rc::clone(&selection);
        move |x, y| {
            let cell = runner_px_to_cell(x, y, &registry.borrow());
            *selection.borrow_mut() = Some(Selection {
                pane_id: SIDEBAR_TASKU_ID, anchor: cell, head: cell,
            });
            registry.borrow().mark_dirty(SIDEBAR_TASKU_ID);
        }
    });

    ui.on_tasku_mouse_move({
        let registry = Rc::clone(&registry);
        let selection = Rc::clone(&selection);
        move |x, y| {
            let mut sel = selection.borrow_mut();
            if let Some(ref mut s) = *sel {
                if s.pane_id == SIDEBAR_TASKU_ID {
                    s.head = runner_px_to_cell(x, y, &registry.borrow());
                    drop(sel);
                    registry.borrow().mark_dirty(SIDEBAR_TASKU_ID);
                }
            }
        }
    });

    ui.on_tasku_mouse_release({
        let registry = Rc::clone(&registry);
        let selection = Rc::clone(&selection);
        move || {
            let mut sel = selection.borrow_mut();
            if let Some(ref s) = *sel {
                if s.pane_id == SIDEBAR_TASKU_ID && s.is_empty() {
                    *sel = None;
                    drop(sel);
                    registry.borrow().mark_dirty(SIDEBAR_TASKU_ID);
                }
            }
        }
    });

    // ── Tasku button commands ────────────────────────────────────────────────
    ui.on_tasku_command({
        let registry = Rc::clone(&registry);
        let active_project = Rc::clone(&active_project);
        let code_to_name = Rc::clone(&code_to_name);
        move |label| {
            // Resolve active project code → full name for --project flag.
            // Unfiltered commands (Edit, Delete, SQL) ignore the project flag.
            let pf = active_project.borrow().as_ref()
                .and_then(|code| code_to_name.get(code))
                .map(|name| format!(" --project {name}"))
                .unwrap_or_default();

            // Commands that need the selected task ID: read from the selection
            // file written by `tasku list` in Mado mode.
            let sel_id = match label.as_str() {
                "Edit" | "Delete" => read_tasku_sel(),
                _ => String::new(),
            };
            match tasku_button_command(label.as_str(), &pf, &sel_id) {
                Some(cmd) => {
                    tasku_log(&format!(
                        "button={:?} pf={:?} sel={:?} cmd={:?}",
                        label.as_str(), pf, sel_id, cmd
                    ));
                    registry.borrow_mut().write_key(SIDEBAR_TASKU_ID, cmd.as_bytes());
                }
                None => {
                    tasku_log(&format!(
                        "button={:?} → no command (sel empty or unknown label)",
                        label.as_str()
                    ));
                }
            }
        }
    });

    // ── Tasku key input ──────────────────────────────────────────────────────
    ui.on_tasku_key_input({
        let registry      = Rc::clone(&registry);
        let selection     = Rc::clone(&selection);
        let pending_paste = Rc::clone(&pending_paste);
        move |text, ctrl, meta, _alt, shift| {
            let t = text.as_str();
            let has_sel = {
                let sel = selection.borrow();
                sel.as_ref().map_or(false, |s| !s.is_empty() && s.pane_id == SIDEBAR_TASKU_ID)
            };

            // Mirror the pane handler: clear selection on plain keystrokes but
            // preserve it while a modifier chord is in progress (e.g. Cmd+C).
            if keys::should_clear_selection(t, ctrl, meta, shift) {
                let prev_id = selection.borrow().as_ref().map(|s| s.pane_id);
                *selection.borrow_mut() = None;
                if let Some(sid) = prev_id {
                    registry.borrow().mark_dirty(sid);
                }
            }

            match keys::classify_key(t, ctrl, meta, false, shift, has_sel) {
                keys::KeyAction::CopySelection => {
                    let norm = {
                        let sel = selection.borrow();
                        sel.as_ref().unwrap().normalized()
                    };
                    let copied = registry.borrow().get_selection_text(SIDEBAR_TASKU_ID, norm);
                    if !copied.is_empty() {
                        let _ = std::process::Command::new("/bin/sh")
                            .args(["-c", &format!("printf '%s' {} | pbcopy",
                                shell_escape(&copied))])
                            .status();
                    }
                    let prev_id = selection.borrow().as_ref().map(|s| s.pane_id);
                    *selection.borrow_mut() = None;
                    if let Some(sid) = prev_id {
                        registry.borrow().mark_dirty(sid);
                    }
                }
                keys::KeyAction::Nothing => {}
                keys::KeyAction::Paste => {
                    let result: Arc<std::sync::Mutex<Option<Vec<u8>>>> =
                        Arc::new(std::sync::Mutex::new(None));
                    let result2 = Arc::clone(&result);
                    std::thread::spawn(move || {
                        if let Ok(out) = std::process::Command::new("pbpaste").output() {
                            *result2.lock().unwrap() = Some(out.stdout);
                        }
                    });
                    *pending_paste.borrow_mut() = Some((SIDEBAR_TASKU_ID, result));
                }
                keys::KeyAction::Forward(bytes) => {
                    if !bytes.is_empty() {
                        registry.borrow_mut().write_key(SIDEBAR_TASKU_ID, &bytes);
                    }
                }
                _ => {
                    // Zoom, arrow sequences, etc. are not applicable in the Tasku panel.
                    let bytes = keys::key_text_to_bytes(&text);
                    if !bytes.is_empty() {
                        registry.borrow_mut().write_key(SIDEBAR_TASKU_ID, &bytes);
                    }
                }
            }
        }
    });

    // ── Workspace refresh ────────────────────────────────────────────────────
    ui.on_workspace_refresh({
        let sidebar     = Rc::clone(&sidebar);
        let ws_model    = Rc::clone(&ws_model);
        let prio_model  = Rc::clone(&prio_model);
        move || {
            let fetched: Vec<workspace::FetchedProject> = {
                let s = sidebar.borrow();
                match &s.tasku_path {
                    Some(p) => workspace::fetch_projects(p),
                    None    => return,
                }
            };

            // Remove workspace files for projects no longer in Tasku.
            {
                let valid_codes: Vec<String> = fetched.iter().map(|fp| fp.code.clone()).collect();
                workspace::prune_stale_workspaces(&valid_codes);
            }

            // Repopulate workspace tiles
            let refreshed_icons = workspace::load_project_icons();
            while ws_model.row_count() > 0 { ws_model.remove(ws_model.row_count() - 1); }
            for fp in &fetched {
                let icon = refreshed_icons.get(&fp.code)
                    .or_else(|| refreshed_icons.get(&fp.name))
                    .cloned()
                    .unwrap_or_default();
                ws_model.push(WorkspaceProject {
                    code:       fp.code.clone().into(),
                    color:      fp.color,
                    text_color: fp.text_color,
                    icon:       icon.into(),
                });
            }

            // Repopulate priority list, preserving saved order
            let saved_order = workspace::load_priority_order();
            let mut remaining: Vec<&workspace::FetchedProject> = fetched.iter().collect();
            let mut ordered:   Vec<&workspace::FetchedProject> = Vec::new();
            for code in &saved_order {
                if let Some(pos) = remaining.iter().position(|fp| &fp.code == code) {
                    ordered.push(remaining.remove(pos));
                }
            }
            ordered.extend(remaining);

            while prio_model.row_count() > 0 { prio_model.remove(prio_model.row_count() - 1); }
            for fp in ordered {
                prio_model.push(PriorityProject {
                    name:  fp.name.clone().into(),
                    code:  fp.code.clone().into(),
                    color: fp.color,
                });
            }
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
        let runner_tasks = Rc::clone(&runner_tasks);
        let top_right_pixel = Rc::clone(&top_right_pixel);
        let right_pixel_plugins_ws = Rc::clone(&right_pixel_plugins);
        let tasku_open_ws = Rc::clone(&tasku_open);
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
            let project_root = project_paths.get(&code)
                .or_else(|| code_to_name.get(&code).and_then(|n| project_paths.get(n.as_str())))
                .cloned()
                .unwrap_or_else(|| home.clone());

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

                // 4c. Tell the Claude AI plugin to cd to the project root
                if let Some(pp) = top_right_pixel.borrow_mut().as_mut() {
                    pp.send_chdir(&project_root);
                }

                // 4d. Notify all right-sidebar plugins of the workspace change
                for pp in right_pixel_plugins_ws.borrow_mut().values_mut() {
                    pp.send_workspace(&code);
                }

                // 4b. Update runner command for the new workspace
                let proj_name = code_to_name.get(&code).map(|s| s.as_str());
                let cmd = runner_tasks.get(&code)
                    .or_else(|| proj_name.and_then(|n| runner_tasks.get(n)))
                    .cloned()
                    .unwrap_or_default();
                ui.set_runner_current_command(cmd.into());

                // 5. If the Tasku overlay is open, refresh with project filter.
                //    Guard on tasku_open so we don't run tasku list with a
                //    stale TASKU_COLS_FILE before the user has opened the panel.
                if registry.borrow().sessions.contains_key(&SIDEBAR_TASKU_ID)
                    && tasku_open_ws.get()
                {
                    let name = code_to_name.get(&code).cloned().unwrap_or_else(|| code.clone());
                    let cmd = format!("\x03tasku list --project {name}\n");
                    registry.borrow_mut().write_key(SIDEBAR_TASKU_ID, cmd.as_bytes());
                }
            }
        }
    });

    // ── Runner: play ─────────────────────────────────────────────────────────
    ui.on_runner_play({
        let registry          = Rc::clone(&registry);
        let active_project    = Rc::clone(&active_project);
        let runner_tasks      = Rc::clone(&runner_tasks);
        let project_paths     = Rc::clone(&project_paths);
        let code_to_name      = Rc::clone(&code_to_name);
        let runner_is_running = Rc::clone(&runner_is_running);
        let ui_weak           = ui.as_weak();
        move || {
            let code = active_project.borrow().clone().unwrap_or_default();
            // Look up by code, fall back to project name for entries that use `project =`
            let name = code_to_name.get(&code).map(|s| s.as_str());
            let cmd = runner_tasks.get(&code)
                .or_else(|| name.and_then(|n| runner_tasks.get(n)))
                .cloned()
                .unwrap_or_default();
            if cmd.is_empty() { return; }
            let cwd = project_paths.get(&code)
                .or_else(|| name.and_then(|n| project_paths.get(n)))
                .map(|s| s.as_str());
            let overlay_w = ui_weak.upgrade()
                .map(|ui| ui.get_overlay_total_w())
                .unwrap_or(1200.0);
            let (term_w, term_h) = runner_size(overlay_w);
            // Run via the user's login shell so version managers (rbenv, asdf,
            // nvm) and homebrew shims all resolve correctly.
            let shell = registry.borrow().shell.clone();
            registry.borrow_mut().spawn_cmd(
                RUNNER_ID, term_w, term_h,
                &shell, &["-ilc", &cmd], cwd, &[],
            );
            runner_is_running.set(true);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_runner_is_running(true);
            }
        }
    });

    // ── Runner: stop ─────────────────────────────────────────────────────────
    ui.on_runner_stop({
        let registry          = Rc::clone(&registry);
        let runner_is_running = Rc::clone(&runner_is_running);
        let ui_weak           = ui.as_weak();
        move || {
            registry.borrow_mut().write_key(RUNNER_ID, &[3]); // Ctrl+C
            runner_is_running.set(false);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_runner_is_running(false);
            }
        }
    });

    // ── Runner: restart ───────────────────────────────────────────────────────
    ui.on_runner_restart({
        let registry          = Rc::clone(&registry);
        let active_project    = Rc::clone(&active_project);
        let runner_tasks      = Rc::clone(&runner_tasks);
        let project_paths     = Rc::clone(&project_paths);
        let code_to_name      = Rc::clone(&code_to_name);
        let runner_is_running = Rc::clone(&runner_is_running);
        let ui_weak           = ui.as_weak();
        move || {
            // Kill existing session if running
            registry.borrow_mut().write_key(RUNNER_ID, &[3]);
            registry.borrow_mut().sessions.remove(&RUNNER_ID);

            let code = active_project.borrow().clone().unwrap_or_default();
            let name = code_to_name.get(&code).map(|s| s.as_str());
            let cmd = runner_tasks.get(&code)
                .or_else(|| name.and_then(|n| runner_tasks.get(n)))
                .cloned()
                .unwrap_or_default();
            if cmd.is_empty() {
                runner_is_running.set(false);
                if let Some(ui) = ui_weak.upgrade() { ui.set_runner_is_running(false); }
                return;
            }
            let cwd = project_paths.get(&code)
                .or_else(|| name.and_then(|n| project_paths.get(n)))
                .map(|s| s.as_str());
            let overlay_w = ui_weak.upgrade()
                .map(|ui| ui.get_overlay_total_w())
                .unwrap_or(1200.0);
            let (term_w, term_h) = runner_size(overlay_w);
            let shell = registry.borrow().shell.clone();
            registry.borrow_mut().spawn_cmd(
                RUNNER_ID, term_w, term_h,
                &shell, &["-ilc", &cmd], cwd, &[],
            );
            runner_is_running.set(true);
            if let Some(ui) = ui_weak.upgrade() { ui.set_runner_is_running(true); }
        }
    });

    // ── Runner: key input ────────────────────────────────────────────────────
    ui.on_runner_key_input({
        let registry  = Rc::clone(&registry);
        let selection = Rc::clone(&selection);
        move |text, ctrl, meta, _alt, _shift| {
            let t = text.as_str();
            // Cmd+C or Ctrl+C with a selection → copy, don't send ^C
            if (meta && t == "c") || (ctrl && t == "c") {
                let sel = selection.borrow();
                if let Some(ref s) = *sel {
                    if s.pane_id == RUNNER_ID && !s.is_empty() {
                        let norm = s.normalized();
                        drop(sel);
                        let text = registry.borrow().get_selection_text(RUNNER_ID, norm);
                        if !text.is_empty() {
                            let _ = std::process::Command::new("/bin/sh")
                                .args(["-c", &format!("printf '%s' {} | pbcopy",
                                    shell_escape(&text))])
                                .status();
                        }
                        *selection.borrow_mut() = None;
                        registry.borrow().mark_dirty(RUNNER_ID);
                        return;
                    }
                }
            }
            let zoom_mod = ctrl || meta;
            if zoom_mod && (t == "=" || t == "+" || t == "-" || t == "0") { return; }
            let bytes = keys::key_text_to_bytes(&text);
            registry.borrow_mut().write_key(RUNNER_ID, &bytes);
        }
    });

    // ── Runner: scroll ────────────────────────────────────────────────────────
    ui.on_runner_scroll({
        let registry   = Rc::clone(&registry);
        let scroll_acc = Rc::clone(&scroll_acc);
        move |delta_px| {
            let cell_h = {
                let reg = registry.borrow();
                reg.font.cell_h as f32 / reg.scale
            };
            if cell_h <= 0.0 { return; }
            let mut acc = scroll_acc.borrow_mut();
            let entry = acc.entry(RUNNER_ID).or_insert(0.0);
            *entry += -delta_px * scroll_dir;
            let rows = (*entry / cell_h) as i32;
            if rows != 0 {
                *entry -= rows as f32 * cell_h;
                registry.borrow_mut().scroll(RUNNER_ID, rows);
            }
        }
    });

    // ── Runner: panel resize ──────────────────────────────────────────────────
    ui.on_runner_panel_height_changed({
        let registry = Rc::clone(&registry);
        let ui_weak  = ui.as_weak();
        move |new_h| {
            let overlay_w = ui_weak.upgrade()
                .map(|ui| ui.get_overlay_total_w())
                .unwrap_or(1200.0);
            let (term_w, term_h) = {
                let btn_w  = RUNNER_BTN_W;
                let pad_h  = RUNNER_PAD_H;
                let term_w = (overlay_w - btn_w).max(50.0).round();
                let term_h = (new_h      - pad_h).max(50.0).round();
                (term_w, term_h)
            };
            if registry.borrow().sessions.contains_key(&RUNNER_ID) {
                registry.borrow_mut().resize(RUNNER_ID, term_w, term_h);
            }
        }
    });

    // ── Runner mouse selection ────────────────────────────────────────────────
    ui.on_runner_mouse_pressed({
        let registry  = Rc::clone(&registry);
        let selection = Rc::clone(&selection);
        move |x, y| {
            let cell = runner_px_to_cell(x, y, &registry.borrow());
            *selection.borrow_mut() = Some(Selection { pane_id: RUNNER_ID, anchor: cell, head: cell });
            registry.borrow().mark_dirty(RUNNER_ID);
        }
    });

    ui.on_runner_mouse_moved({
        let registry  = Rc::clone(&registry);
        let selection = Rc::clone(&selection);
        move |x, y| {
            let mut sel = selection.borrow_mut();
            if let Some(ref mut s) = *sel {
                if s.pane_id == RUNNER_ID {
                    s.head = runner_px_to_cell(x, y, &registry.borrow());
                    drop(sel);
                    registry.borrow().mark_dirty(RUNNER_ID);
                }
            }
        }
    });

    ui.on_runner_mouse_released({
        let registry  = Rc::clone(&registry);
        let selection = Rc::clone(&selection);
        move || {
            let mut sel = selection.borrow_mut();
            if let Some(ref s) = *sel {
                if s.pane_id == RUNNER_ID && s.is_empty() {
                    *sel = None;
                    drop(sel);
                    registry.borrow().mark_dirty(RUNNER_ID);
                }
            }
        }
    });

    // ── Tasku panel height (drag-to-resize, left sidebar case) ───────────────
    ui.on_tasku_panel_height_changed({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        move |new_h| {
            let (sidebar_w, tasku_pos) = {
                let mut s = sidebar.borrow_mut();
                s.tasku_panel_h = new_h;
                (((s.width - 17.0).max(50.0) * TASKU_SIDEBAR_COL_BOOST).round(), s.tasku_position.clone())
            };
            // Only applies when tasku is in left sidebar
            if tasku_pos == "left" || tasku_pos.is_empty() {
                registry.borrow_mut().resize(
                    SIDEBAR_TASKU_ID,
                    sidebar_w,
                    tasku_terminal_h(new_h),
                );
            }
        }
    });

    // ── Top bar height changed (tasku resize in top bar) ─────────────────────
    ui.on_top_bar_height_changed({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        let ui_weak = ui.as_weak();
        move |new_bar_h| {
            // The Tasku overlay covers the full window width, not just the pane
            // area — use overlay_total_w (same source as the initial spawn).
            let overlay_w = {
                let mut s = sidebar.borrow_mut();
                s.top_bar_h = new_bar_h;
                ui_weak.upgrade()
                    .map(|ui| ui.get_overlay_total_w())
                    .unwrap_or(1200.0)
            };
            let (term_w, term_h) = tasku_top_bar_size(overlay_w, new_bar_h);
            let reg = registry.borrow();
            let term_w = term_w.min(reg.max_logical_w(TASKU_MAX_COLS));
            drop(reg);
            registry.borrow_mut().resize(SIDEBAR_TASKU_ID, term_w, term_h);
        }
    });

    // ── Tasku terminal scroll wheel ──────────────────────────────────────────
    ui.on_tasku_scroll({
        let registry   = Rc::clone(&registry);
        let scroll_acc = Rc::clone(&scroll_acc);
        move |delta_px| {
            let cell_h = {
                let reg = registry.borrow();
                reg.font.cell_h as f32 / reg.scale
            };
            if cell_h <= 0.0 { return; }
            let mut acc = scroll_acc.borrow_mut();
            let entry = acc.entry(SIDEBAR_TASKU_ID).or_insert(0.0);
            *entry += -delta_px * scroll_dir;
            let rows = (*entry / cell_h) as i32;
            if rows != 0 {
                *entry -= rows as f32 * cell_h;
                registry.borrow_mut().scroll(SIDEBAR_TASKU_ID, rows);
            }
        }
    });

    // ── External plugin panel expand/collapse (left sidebar) ─────────────────
    ui.on_plugin_toggled({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        let plugin_expanded_model = Rc::clone(&plugin_expanded_model);
        let plugin_panel_h_model = Rc::clone(&plugin_panel_h_model);
        let pixel_plugins = Rc::clone(&pixel_plugins);
        let ui_weak = ui.as_weak();
        move |idx| {
            let idx = idx as usize;
            if idx >= num_left_ext { return; }
            let current = plugin_expanded_model.row_data(idx).unwrap_or(false);
            let now_expanded = !current;
            plugin_expanded_model.set_row_data(idx, now_expanded);

            let sidebar_w = (sidebar.borrow().width - 24.0).max(50.0);
            let panel_h = plugin_panel_h_model.row_data(idx).unwrap_or(DEFAULT_PLUGIN_PANEL_H);
            let (command, kind, plugin_id) = {
                let s = sidebar.borrow();
                let p = s.left_ext_plugins.get(idx);
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
                    if let Some(plugin) = PixelPlugin::spawn(&program, &args_ref, phys_w, phys_h, &[]) {
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
            let any = (0..num_left_ext)
                .any(|i| plugin_expanded_model.row_data(i).unwrap_or(false));
            let total: f32 = (0..num_left_ext)
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

    // ── External plugin panel height (drag-to-resize, left sidebar) ──────────
    ui.on_plugin_panel_height_changed({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        let plugin_expanded_model = Rc::clone(&plugin_expanded_model);
        let plugin_panel_h_model = Rc::clone(&plugin_panel_h_model);
        let right_plugin_panel_h_model = Rc::clone(&right_plugin_panel_h_model);
        let pixel_plugins = Rc::clone(&pixel_plugins);
        let ui_weak = ui.as_weak();
        move |idx, new_h| {
            let idx = idx as usize;
            if idx >= num_left_ext { return; }
            plugin_panel_h_model.set_row_data(idx, new_h);
            let sidebar_w = (sidebar.borrow().width - 24.0).max(50.0);
            if let Some(plugin) = pixel_plugins.borrow_mut().get_mut(&idx) {
                plugin.send_resize((sidebar_w * scale) as u32, (new_h * scale) as u32);
            } else {
                registry.borrow_mut().resize(plugin_node_id(idx), sidebar_w, plugin_terminal_h(new_h));
            }
            // Update viewport total height
            let total: f32 = (0..num_left_ext)
                .map(|i| if plugin_expanded_model.row_data(i).unwrap_or(false) {
                    plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H)
                } else { 0.0 })
                .sum();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_external_plugins_total_h(total);
            }
            // Persist heights
            let left_h: Vec<f32> = (0..num_left_ext)
                .map(|i| plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H))
                .collect();
            let right_h: Vec<f32> = (0..num_right_ext)
                .map(|i| right_plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H))
                .collect();
            workspace::save_panel_heights(&left_h, &right_h);
        }
    });

    // ── External plugin key input (left sidebar) ──────────────────────────────
    ui.on_plugin_key_input({
        let registry = Rc::clone(&registry);
        let pixel_plugins = Rc::clone(&pixel_plugins);
        move |idx, text, ctrl, meta, alt, shift| {
            let idx = idx as usize;
            if idx >= num_left_ext { return; }
            if let Some(plugin) = pixel_plugins.borrow_mut().get_mut(&idx) {
                if keys::is_modifier_only(text.as_str()) { return; }
                plugin.send_key(text.as_str(), ctrl, meta, alt, shift);
            } else {
                let zoom_mod = ctrl || meta;
                let t = text.as_str();
                if zoom_mod && (t == "=" || t == "+" || t == "-" || t == "0") { return; }
                let bytes = keys::key_text_to_bytes(&text);
                registry.borrow_mut().write_key(plugin_node_id(idx), &bytes);
            }
        }
    });

    // ── External plugin scroll wheel (left sidebar) ───────────────────────────
    ui.on_plugin_scroll({
        let registry      = Rc::clone(&registry);
        let pixel_plugins = Rc::clone(&pixel_plugins);
        let scroll_acc    = Rc::clone(&scroll_acc);
        move |idx, delta_px| {
            let idx = idx as usize;
            if idx >= num_left_ext { return; }
            if let Some(plugin) = pixel_plugins.borrow_mut().get_mut(&idx) {
                plugin.send_scroll(delta_px * scroll_dir);
            } else {
                let cell_h = {
                    let reg = registry.borrow();
                    reg.font.cell_h as f32 / reg.scale
                };
                if cell_h <= 0.0 { return; }
                let node = plugin_node_id(idx);
                let mut acc = scroll_acc.borrow_mut();
                let entry = acc.entry(node).or_insert(0.0);
                *entry += -delta_px * scroll_dir;
                let rows = (*entry / cell_h) as i32;
                if rows != 0 {
                    *entry -= rows as f32 * cell_h;
                    registry.borrow_mut().scroll(node, rows);
                }
            }
        }
    });

    // ── External plugin click ────────────────────────────────────────────────
    ui.on_plugin_click({
        let pixel_plugins = Rc::clone(&pixel_plugins);
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

    // ── Right sidebar width persistence ──────────────────────────────────────
    ui.on_right_sidebar_width_changed({
        let sidebar = Rc::clone(&sidebar);
        let registry = Rc::clone(&registry);
        let right_plugin_expanded_model = Rc::clone(&right_plugin_expanded_model);
        let right_plugin_panel_h_model = Rc::clone(&right_plugin_panel_h_model);
        move |new_width| {
            sidebar.borrow_mut().right_width = new_width;
            let sidebar_w = (new_width - 24.0).max(50.0);
            let mut reg = registry.borrow_mut();
            for i in 0..num_right_ext {
                if right_plugin_expanded_model.row_data(i).unwrap_or(false) {
                    let h = right_plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H);
                    reg.resize(right_plugin_node_id(i), sidebar_w, plugin_terminal_h(h));
                }
            }
        }
    });

    // ── Right sidebar plugin expand/collapse ──────────────────────────────────
    ui.on_right_plugin_toggled({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        let right_plugin_expanded_model = Rc::clone(&right_plugin_expanded_model);
        let right_plugin_panel_h_model = Rc::clone(&right_plugin_panel_h_model);
        let right_pixel_plugins = Rc::clone(&right_pixel_plugins);
        let active_project_rpt = Rc::clone(&active_project);
        let ui_weak = ui.as_weak();
        move |idx| {
            let idx = idx as usize;
            if idx >= num_right_ext { return; }
            let current = right_plugin_expanded_model.row_data(idx).unwrap_or(false);
            let now_expanded = !current;
            right_plugin_expanded_model.set_row_data(idx, now_expanded);

            let sidebar_w = (sidebar.borrow().right_width - 24.0).max(50.0);
            let panel_h = right_plugin_panel_h_model.row_data(idx).unwrap_or(DEFAULT_PLUGIN_PANEL_H);
            let (command, kind, plugin_id) = {
                let s = sidebar.borrow();
                let p = s.right_ext_plugins.get(idx);
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
                    let ws_code = active_project_rpt.borrow().clone().unwrap_or_default();
                    if let Some(plugin) = PixelPlugin::spawn(&program, &args_ref, phys_w, phys_h,
                                                             &[("MADO_WORKSPACE", &ws_code)]) {
                        right_pixel_plugins.borrow_mut().insert(idx, plugin);
                    } else {
                        eprintln!("mado: failed to spawn pixel plugin '{plugin_id}'");
                    }
                } else {
                    reg.spawn_cmd(right_plugin_node_id(idx), sidebar_w, plugin_terminal_h(panel_h),
                                  &program, &args_ref, None,
                                  &[("MADO_PLUGIN_ID", &plugin_id)]);
                }
            } else if kind == "pixel" {
                right_pixel_plugins.borrow_mut().remove(&idx);
            } else {
                reg.remove(right_plugin_node_id(idx));
            }
            drop(reg);

            let any = (0..num_right_ext)
                .any(|i| right_plugin_expanded_model.row_data(i).unwrap_or(false));
            let total: f32 = (0..num_right_ext)
                .map(|i| if right_plugin_expanded_model.row_data(i).unwrap_or(false) {
                    right_plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H)
                } else { 0.0 })
                .sum();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_right_plugin_any_expanded(any);
                ui.set_right_external_plugins_total_h(total);
            }
        }
    });

    // ── Right sidebar plugin height ───────────────────────────────────────────
    ui.on_right_plugin_panel_height_changed({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        let right_plugin_expanded_model = Rc::clone(&right_plugin_expanded_model);
        let plugin_panel_h_model = Rc::clone(&plugin_panel_h_model);
        let right_plugin_panel_h_model = Rc::clone(&right_plugin_panel_h_model);
        let right_pixel_plugins = Rc::clone(&right_pixel_plugins);
        let ui_weak = ui.as_weak();
        move |idx, new_h| {
            let idx = idx as usize;
            if idx >= num_right_ext { return; }
            right_plugin_panel_h_model.set_row_data(idx, new_h);
            let sidebar_w = (sidebar.borrow().right_width - 24.0).max(50.0);
            if let Some(plugin) = right_pixel_plugins.borrow_mut().get_mut(&idx) {
                plugin.send_resize((sidebar_w * scale) as u32, (new_h * scale) as u32);
            } else {
                registry.borrow_mut().resize(right_plugin_node_id(idx), sidebar_w, plugin_terminal_h(new_h));
            }
            let total: f32 = (0..num_right_ext)
                .map(|i| if right_plugin_expanded_model.row_data(i).unwrap_or(false) {
                    right_plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H)
                } else { 0.0 })
                .sum();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_right_external_plugins_total_h(total);
            }
            // Persist heights
            let left_h: Vec<f32> = (0..num_left_ext)
                .map(|i| plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H))
                .collect();
            let right_h: Vec<f32> = (0..num_right_ext)
                .map(|i| right_plugin_panel_h_model.row_data(i).unwrap_or(DEFAULT_PLUGIN_PANEL_H))
                .collect();
            workspace::save_panel_heights(&left_h, &right_h);
        }
    });

    // ── Right sidebar plugin key input ────────────────────────────────────────
    ui.on_right_plugin_key_input({
        let registry = Rc::clone(&registry);
        let right_pixel_plugins = Rc::clone(&right_pixel_plugins);
        move |idx, text, ctrl, meta, alt, shift| {
            let idx = idx as usize;
            if idx >= num_right_ext { return; }
            if let Some(plugin) = right_pixel_plugins.borrow_mut().get_mut(&idx) {
                if keys::is_modifier_only(text.as_str()) { return; }
                plugin.send_key(text.as_str(), ctrl, meta, alt, shift);
            } else {
                let zoom_mod = ctrl || meta;
                let t = text.as_str();
                if zoom_mod && (t == "=" || t == "+" || t == "-" || t == "0") { return; }
                let bytes = keys::key_text_to_bytes(&text);
                registry.borrow_mut().write_key(right_plugin_node_id(idx), &bytes);
            }
        }
    });

    // ── Right sidebar plugin scroll ───────────────────────────────────────────
    ui.on_right_plugin_scroll({
        let registry   = Rc::clone(&registry);
        let scroll_acc = Rc::clone(&scroll_acc);
        move |idx, delta_px| {
            let idx = idx as usize;
            if idx >= num_right_ext { return; }
            let cell_h = {
                let reg = registry.borrow();
                reg.font.cell_h as f32 / reg.scale
            };
            if cell_h <= 0.0 { return; }
            let node = right_plugin_node_id(idx);
            let mut acc = scroll_acc.borrow_mut();
            let entry = acc.entry(node).or_insert(0.0);
            *entry += -delta_px * scroll_dir;
            let rows = (*entry / cell_h) as i32;
            if rows != 0 {
                *entry -= rows as f32 * cell_h;
                registry.borrow_mut().scroll(node, rows);
            }
        }
    });

    // ── Right sidebar plugin click ────────────────────────────────────────────
    ui.on_right_plugin_click({
        let right_pixel_plugins = Rc::clone(&right_pixel_plugins);
        move |idx, x, y| {
            let idx = idx as usize;
            if let Some(plugin) = right_pixel_plugins.borrow_mut().get_mut(&idx) {
                plugin.send_click(x, y);
            }
        }
    });

    // ── Right sidebar plugin focus ────────────────────────────────────────────
    ui.on_right_plugin_focus_changed({
        let right_pixel_plugins = Rc::clone(&right_pixel_plugins);
        move |idx, focused| {
            let idx = idx as usize;
            if let Some(plugin) = right_pixel_plugins.borrow_mut().get_mut(&idx) {
                plugin.send_focus(focused);
            }
        }
    });

    // ── Plugin float overlay ──────────────────────────────────────────────────
    ui.on_plugin_float({
        let pixel_plugins  = Rc::clone(&pixel_plugins);
        let float_plugin   = Rc::clone(&float_plugin);
        let ui_weak        = ui.as_weak();
        move |idx| {
            let idx = idx as usize;
            if let Some(plugin) = pixel_plugins.borrow_mut().get_mut(&idx) {
                let ui = ui_weak.upgrade().unwrap();
                let max_logical = (ui.get_window_w() - 80.0).min(ui.get_window_h() - 80.0).min(800.0);
                let size = (max_logical * scale).round() as u32;
                plugin.send_resize(size, size);
                *float_plugin.borrow_mut() = Some((idx, false));
            }
        }
    });

    ui.on_right_plugin_float({
        let right_pixel_plugins = Rc::clone(&right_pixel_plugins);
        let float_plugin        = Rc::clone(&float_plugin);
        let ui_weak             = ui.as_weak();
        move |idx| {
            let idx = idx as usize;
            if let Some(plugin) = right_pixel_plugins.borrow_mut().get_mut(&idx) {
                let ui = ui_weak.upgrade().unwrap();
                let max_logical = (ui.get_window_w() - 80.0).min(ui.get_window_h() - 80.0).min(800.0);
                let size = (max_logical * scale).round() as u32;
                plugin.send_resize(size, size);
                *float_plugin.borrow_mut() = Some((idx, true));
            }
        }
    });

    ui.on_plugin_float_dismissed({
        let pixel_plugins       = Rc::clone(&pixel_plugins);
        let right_pixel_plugins = Rc::clone(&right_pixel_plugins);
        let float_plugin        = Rc::clone(&float_plugin);
        let plugin_panel_h_model       = Rc::clone(&plugin_panel_h_model);
        let right_plugin_panel_h_model = Rc::clone(&right_plugin_panel_h_model);
        let sidebar  = Rc::clone(&sidebar);
        let ui_weak  = ui.as_weak();
        move || {
            let entry = float_plugin.borrow().clone();
            if let Some((idx, is_right)) = entry {
                if is_right {
                    let sidebar_w = (sidebar.borrow().right_width - 24.0).max(50.0);
                    let h = right_plugin_panel_h_model.row_data(idx).unwrap_or(DEFAULT_PLUGIN_PANEL_H);
                    if let Some(plugin) = right_pixel_plugins.borrow_mut().get_mut(&idx) {
                        let phys_w = (sidebar_w * scale) as u32;
                        let phys_h = (h * scale) as u32;
                        plugin.send_resize(phys_w, phys_h);
                    }
                } else {
                    let sidebar_w = (sidebar.borrow().width - 24.0).max(50.0);
                    let h = plugin_panel_h_model.row_data(idx).unwrap_or(DEFAULT_PLUGIN_PANEL_H);
                    if let Some(plugin) = pixel_plugins.borrow_mut().get_mut(&idx) {
                        let phys_w = (sidebar_w * scale) as u32;
                        let phys_h = (h * scale) as u32;
                        plugin.send_resize(phys_w, phys_h);
                    }
                }
                *float_plugin.borrow_mut() = None;
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_float_plugin_image(Default::default());
                }
            }
        }
    });

    // ── Top bar plugin callbacks ──────────────────────────────────────────────
    ui.on_top_plugin_key_input({
        let registry = Rc::clone(&registry);
        move |idx, text, ctrl, meta, _alt, _shift| {
            let idx = idx as usize;
            if idx >= num_top_ext { return; }
            let zoom_mod = ctrl || meta;
            let t = text.as_str();
            if zoom_mod && (t == "=" || t == "+" || t == "-" || t == "0") { return; }
            let bytes = keys::key_text_to_bytes(&text);
            registry.borrow_mut().write_key(top_plugin_node_id(idx), &bytes);
        }
    });

    ui.on_top_plugin_scroll({
        let registry   = Rc::clone(&registry);
        let scroll_acc = Rc::clone(&scroll_acc);
        move |idx, delta_px| {
            let idx = idx as usize;
            if idx >= num_top_ext { return; }
            let cell_h = {
                let reg = registry.borrow();
                reg.font.cell_h as f32 / reg.scale
            };
            if cell_h <= 0.0 { return; }
            let node = top_plugin_node_id(idx);
            let mut acc = scroll_acc.borrow_mut();
            let entry = acc.entry(node).or_insert(0.0);
            *entry += -delta_px * scroll_dir;
            let rows = (*entry / cell_h) as i32;
            if rows != 0 {
                *entry -= rows as f32 * cell_h;
                registry.borrow_mut().scroll(node, rows);
            }
        }
    });

    ui.on_top_plugin_click({
        move |_idx, _x, _y| { }
    });

    ui.on_top_plugin_focus_changed({
        move |_idx, _focused| { }
    });

    // ── Top-right panel plugin callbacks ──────────────────────────────────────
    ui.on_top_right_plugin_focus_changed({
        let top_right_pixel = Rc::clone(&top_right_pixel);
        move |focused| {
            if let Some(plugin) = top_right_pixel.borrow_mut().as_mut() {
                plugin.send_focus(focused);
            }
        }
    });

    ui.on_top_right_plugin_key_input({
        let registry      = Rc::clone(&registry);
        let selection     = Rc::clone(&selection);
        let pending_paste = Rc::clone(&pending_paste);
        let has_top_right = top_right_plugin.is_some();
        let top_right_pixel = Rc::clone(&top_right_pixel);
        let top_right_paste_result = Rc::clone(&top_right_paste_result);
        move |text, ctrl, meta, alt, shift| {
            // Route to pixel plugin if present
            if top_right_pixel.borrow().is_some() {
                let t = text.as_str();
                if keys::is_modifier_only(t) { return; }
                let is_paste = (meta || ctrl) && t == "v";
                if is_paste {
                    // Try clipboard image first (synchronous — fast)
                    let mut sent_image = false;
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        if let Ok(img) = cb.get_image() {
                            if let Some(b64) = encode_png_base64(
                                img.width as u32, img.height as u32, &img.bytes)
                            {
                                if let Some(pp) = top_right_pixel.borrow_mut().as_mut() {
                                    pp.send_paste_image(&b64, "image/png");
                                    sent_image = true;
                                }
                            }
                        }
                    }
                    if !sent_image {
                        // Fall back to text paste (existing pbpaste approach)
                        let result: Arc<std::sync::Mutex<Option<Vec<u8>>>> =
                            Arc::new(std::sync::Mutex::new(None));
                        let result2 = Arc::clone(&result);
                        std::thread::spawn(move || {
                            if let Ok(out) = std::process::Command::new("pbpaste").output() {
                                *result2.lock().unwrap() = Some(out.stdout);
                            }
                        });
                        *top_right_paste_result.borrow_mut() = Some(result);
                    }
                } else {
                    // Cmd+C, Cmd+X and all other keys: pass through to plugin
                    if let Some(pp) = top_right_pixel.borrow_mut().as_mut() {
                        pp.send_key(text.as_str(), ctrl, meta, alt, shift);
                    }
                }
                return;
            }
            if !has_top_right { return; }
            let node = top_right_plugin_node_id(0);
            let t = text.as_str();
            let has_sel = {
                let sel = selection.borrow();
                sel.as_ref().map_or(false, |s| !s.is_empty() && s.pane_id == node)
            };
            if keys::should_clear_selection(t, ctrl, meta, false) {
                let prev_id = selection.borrow().as_ref().map(|s| s.pane_id);
                *selection.borrow_mut() = None;
                if let Some(sid) = prev_id {
                    registry.borrow().mark_dirty(sid);
                }
            }
            match keys::classify_key(t, ctrl, meta, false, false, has_sel) {
                keys::KeyAction::CopySelection => {
                    let norm = {
                        let sel = selection.borrow();
                        sel.as_ref().unwrap().normalized()
                    };
                    let copied = registry.borrow().get_selection_text(node, norm);
                    if !copied.is_empty() {
                        let _ = std::process::Command::new("/bin/sh")
                            .args(["-c", &format!("printf '%s' {} | pbcopy",
                                shell_escape(&copied))])
                            .status();
                    }
                    let prev_id = selection.borrow().as_ref().map(|s| s.pane_id);
                    *selection.borrow_mut() = None;
                    if let Some(sid) = prev_id {
                        registry.borrow().mark_dirty(sid);
                    }
                }
                keys::KeyAction::Nothing => {}
                keys::KeyAction::Paste => {
                    let result: Arc<std::sync::Mutex<Option<Vec<u8>>>> =
                        Arc::new(std::sync::Mutex::new(None));
                    let result2 = Arc::clone(&result);
                    std::thread::spawn(move || {
                        if let Ok(out) = std::process::Command::new("pbpaste").output() {
                            *result2.lock().unwrap() = Some(out.stdout);
                        }
                    });
                    *pending_paste.borrow_mut() = Some((node, result));
                }
                keys::KeyAction::ZoomIn | keys::KeyAction::ZoomOut
                | keys::KeyAction::ZoomReset => {}
                keys::KeyAction::Forward(bytes) => {
                    if !bytes.is_empty() {
                        registry.borrow_mut().write_key(node, &bytes);
                    }
                }
                _ => {
                    let bytes = keys::key_text_to_bytes(&text);
                    if !bytes.is_empty() {
                        registry.borrow_mut().write_key(node, &bytes);
                    }
                }
            }
        }
    });

    ui.on_top_right_plugin_mouse_press({
        let registry      = Rc::clone(&registry);
        let selection     = Rc::clone(&selection);
        let top_right_pixel = Rc::clone(&top_right_pixel);
        let has_top_right = top_right_plugin.is_some();
        move |x, y| {
            // Pixel plugin: forward as physical coordinates
            if let Some(pp) = top_right_pixel.borrow_mut().as_mut() {
                let scale = registry.borrow().scale;
                pp.send_mouse_press(x * scale, y * scale);
                return;
            }
            if !has_top_right { return; }
            let node = top_right_plugin_node_id(0);
            let cell = runner_px_to_cell(x, y, &registry.borrow());
            *selection.borrow_mut() = Some(Selection { pane_id: node, anchor: cell, head: cell });
            registry.borrow().mark_dirty(node);
        }
    });

    ui.on_top_right_plugin_mouse_move({
        let registry      = Rc::clone(&registry);
        let selection     = Rc::clone(&selection);
        let top_right_pixel = Rc::clone(&top_right_pixel);
        let has_top_right = top_right_plugin.is_some();
        move |x, y| {
            // Pixel plugin: forward as physical coordinates
            if let Some(pp) = top_right_pixel.borrow_mut().as_mut() {
                let scale = registry.borrow().scale;
                pp.send_mouse_move(x * scale, y * scale);
                return;
            }
            if !has_top_right { return; }
            let node = top_right_plugin_node_id(0);
            let mut sel = selection.borrow_mut();
            if let Some(s) = sel.as_mut() {
                if s.pane_id == node {
                    s.head = runner_px_to_cell(x, y, &registry.borrow());
                    registry.borrow().mark_dirty(node);
                }
            }
        }
    });

    ui.on_top_right_plugin_mouse_release({
        let registry      = Rc::clone(&registry);
        let selection     = Rc::clone(&selection);
        let top_right_pixel = Rc::clone(&top_right_pixel);
        let has_top_right = top_right_plugin.is_some();
        move || {
            // Pixel plugin: forward release
            if let Some(pp) = top_right_pixel.borrow_mut().as_mut() {
                pp.send_mouse_release();
                return;
            }
            if !has_top_right { return; }
            let node = top_right_plugin_node_id(0);
            let mut sel = selection.borrow_mut();
            if let Some(s) = sel.as_ref() {
                if s.pane_id == node && s.is_empty() {
                    *sel = None;
                    registry.borrow().mark_dirty(node);
                }
            }
        }
    });

    ui.on_top_right_plugin_scroll({
        let registry   = Rc::clone(&registry);
        let scroll_acc = Rc::clone(&scroll_acc);
        let has_top_right = top_right_plugin.is_some();
        let top_right_pixel = Rc::clone(&top_right_pixel);
        move |delta_px| {
            if let Some(pp) = top_right_pixel.borrow_mut().as_mut() {
                pp.send_scroll(delta_px);
                return;
            }
            if !has_top_right { return; }
            let cell_h = {
                let reg = registry.borrow();
                reg.font.cell_h as f32 / reg.scale
            };
            if cell_h <= 0.0 { return; }
            let node = top_right_plugin_node_id(0);
            let mut acc = scroll_acc.borrow_mut();
            let entry = acc.entry(node).or_insert(0.0);
            *entry += -delta_px * scroll_dir;
            let rows = (*entry / cell_h) as i32;
            if rows != 0 {
                *entry -= rows as f32 * cell_h;
                registry.borrow_mut().scroll(node, rows);
            }
        }
    });

    // ── Top-right panel resize ────────────────────────────────────────────────
    ui.on_top_right_panel_width_changed({
        let registry = Rc::clone(&registry);
        let ui_weak  = ui.as_weak();
        let top_right_pixel = Rc::clone(&top_right_pixel);
        move |new_w| {
            // Resize the terminal session to the new panel width.
            if let Some(ui) = ui_weak.upgrade() {
                let h = ui.get_window_h();
                let pane_w = (new_w - PANE_H_INSET as f32).max(10.0);
                let pane_h = (h - 52.0).max(50.0);
                if let Some(pp) = top_right_pixel.borrow_mut().as_mut() {
                    let scale = registry.borrow().scale;
                    pp.send_resize((pane_w * scale) as u32, (pane_h * scale) as u32);
                    return;
                }
                registry.borrow_mut().resize(top_right_plugin_node_id(0), pane_w, pane_h);
            }
        }
    });

    // ── Native browser panel callbacks ────────────────────────────────────────
    ui.on_browser_panel_width_changed({
        let native_browser = Rc::clone(&native_browser);
        let ui_weak = ui.as_weak();
        move |new_w| {
            if let Some(nb) = native_browser.borrow().as_ref() {
                if let Some(ui) = ui_weak.upgrade() {
                    let x = ui.get_browser_panel_logical_x() as f64;
                    let y = ui.get_bottom_bar_logical_h() as f64;
                    let h = ui.get_window_h() as f64;
                    nb.update_frame(x, y, new_w as f64, h);
                }
            }
        }
    });

    ui.on_browser_visible_changed({
        let native_browser = Rc::clone(&native_browser);
        let ui_weak = ui.as_weak();
        move |visible| {
            if let Some(nb) = native_browser.borrow().as_ref() {
                nb.set_visible(visible);
                if visible {
                    // Update frame now that the Slint layout has recalculated with
                    // the browser panel occupying its correct space.
                    if let Some(ui) = ui_weak.upgrade() {
                        let x = ui.get_browser_panel_logical_x() as f64;
                        let y = ui.get_bottom_bar_logical_h() as f64;
                        let h = ui.get_window_h() as f64;
                        let w = ui.get_browser_panel_width() as f64;
                        nb.update_frame(x, y, w, h);
                    }
                }
            }
        }
    });

    // ── Bottom-right panel plugin callbacks ───────────────────────────────────
    ui.on_bottom_right_plugin_key_input({
        let registry = Rc::clone(&registry);
        let has_bottom_right = bottom_right_plugin.is_some();
        move |text, ctrl, meta, _alt, _shift| {
            if !has_bottom_right { return; }
            let zoom_mod = ctrl || meta;
            let t = text.as_str();
            if zoom_mod && (t == "=" || t == "+" || t == "-" || t == "0") { return; }
            let bytes = keys::key_text_to_bytes(&text);
            registry.borrow_mut().write_key(bottom_right_plugin_node_id(0), &bytes);
        }
    });

    ui.on_bottom_right_plugin_scroll({
        let registry   = Rc::clone(&registry);
        let scroll_acc = Rc::clone(&scroll_acc);
        let has_bottom_right = bottom_right_plugin.is_some();
        move |delta_px| {
            if !has_bottom_right { return; }
            let cell_h = {
                let reg = registry.borrow();
                reg.font.cell_h as f32 / reg.scale
            };
            if cell_h <= 0.0 { return; }
            let node = bottom_right_plugin_node_id(0);
            let mut acc = scroll_acc.borrow_mut();
            let entry = acc.entry(node).or_insert(0.0);
            *entry += -delta_px * scroll_dir;
            let rows = (*entry / cell_h) as i32;
            if rows != 0 {
                *entry -= rows as f32 * cell_h;
                registry.borrow_mut().scroll(node, rows);
            }
        }
    });

    ui.on_sidebar_escaped({
        let ui_weak = ui.as_weak();
        let focused_id = Rc::clone(&focused_id);
        move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if let Some(fid) = *focused_id.borrow() {
                ui.set_force_focus_id(fid as i32);
                ui.set_force_focus_id(-1);
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
            std::time::Duration::from_millis(100),
            move || {
                let Some(ui) = ui_weak.upgrade() else { return };

                // Bring the window to the front and make it the key window so
                // that macOS routes keyboard events here without needing a click.
                // Mirrors gpui's activate_window(): setActivationPolicy → activate
                // → makeKeyAndOrderFront.
                #[cfg(target_os = "macos")]
                ui.window().with_winit_window(|w| {
                    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                    if let Ok(handle) = w.window_handle() {
                        if let RawWindowHandle::AppKit(h) = handle.as_raw() {
                            unsafe {
                                use objc2::runtime::{AnyClass, AnyObject};
                                let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
                                let ns_window: *mut AnyObject =
                                    objc2::msg_send![ns_view, window];
                                if let Some(app_cls) = AnyClass::get("NSApplication") {
                                    let app: *mut AnyObject =
                                        objc2::msg_send![app_cls, sharedApplication];
                                    let _: () = objc2::msg_send![
                                        app, activateIgnoringOtherApps: true
                                    ];
                                }
                                let nil: *mut AnyObject = std::ptr::null_mut();
                                let _: () = objc2::msg_send![
                                    ns_window, makeKeyAndOrderFront: nil
                                ];
                            }
                        }
                    }
                });

                if let Some(fid) = *focused_id.borrow() {
                    ui.set_force_focus_id(fid as i32);
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

    // ── Auto-save workspace every 30 s ──────────────────────────────────────
    // The on_close_requested callback only fires on clean quit (red X button).
    // Force-quit (pkill, SIGTERM) bypasses it entirely. This timer ensures the
    // workspace is saved frequently so very little work is ever lost regardless
    // of how Mado exits.
    let _autosave_timer = {
        let tree = Rc::clone(&tree);
        let registry = Rc::clone(&registry);
        let active_project = Rc::clone(&active_project);
        let t = Timer::default();
        t.start(
            TimerMode::Repeated,
            std::time::Duration::from_secs(30),
            move || {
                if let Some(ref proj) = *active_project.borrow() {
                    let leaf_ids = tree.borrow().leaf_ids();
                    let cwds = registry.borrow().collect_cwds(&leaf_ids);
                    let saved_tree = tree.borrow().to_saved(&cwds);
                    workspace::save_workspace(proj, &workspace::SavedWorkspace {
                        project: proj.clone(),
                        tree: saved_tree,
                    });
                }
            },
        );
        t
    };

    // ── Fullscreen transparency recovery ─────────────────────────────────────
    // macOS resets the Metal layer's isOpaque flag and CGS blur radius during
    // the fullscreen transition animation.  Poll the style mask; when we
    // detect the transition has completed, re-stamp both so transparency and
    // blur survive fullscreen.
    let _fullscreen_timer = {
        let ui_weak = ui.as_weak();
        let mut prev_fullscreen = false;
        let t = Timer::default();
        t.start(
            TimerMode::Repeated,
            std::time::Duration::from_millis(200),
            move || {
                let Some(ui) = ui_weak.upgrade() else { return };

                let is_fullscreen = ui
                    .window()
                    .with_winit_window(|w| {
                        #[cfg(target_os = "macos")]
                        {
                            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                            if let Ok(handle) = w.window_handle() {
                                if let RawWindowHandle::AppKit(h) = handle.as_raw() {
                                    unsafe {
                                        use objc2::runtime::AnyObject;
                                        let ns_view =
                                            h.ns_view.as_ptr() as *mut AnyObject;
                                        let ns_window: *mut AnyObject =
                                            objc2::msg_send![ns_view, window];
                                        const FS_MASK: u64 = 1 << 14;
                                        let mask: u64 =
                                            objc2::msg_send![ns_window, styleMask];
                                        return (mask & FS_MASK) != 0;
                                    }
                                }
                            }
                        }
                        false
                    })
                    .unwrap_or(false);

                if is_fullscreen == prev_fullscreen {
                    return;
                }
                prev_fullscreen = is_fullscreen;

                // Re-apply immediately (catches the style-mask change),
                // then again after the animation finishes (~1.5 s).
                let ui2 = ui.as_weak();
                Timer::single_shot(std::time::Duration::from_millis(1500), move || {
                    let Some(ui) = ui2.upgrade() else { return };
                    ui.window().with_winit_window(|w| { w.set_blur(true); });
                });

                ui.window().with_winit_window(|w| {
                    w.set_blur(true);

                    #[cfg(target_os = "macos")]
                    {
                        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                        if let Ok(handle) = w.window_handle() {
                            if let RawWindowHandle::AppKit(h) = handle.as_raw() {
                                unsafe {
                                    use objc2::runtime::AnyObject;
                                    let ns_view =
                                        h.ns_view.as_ptr() as *mut AnyObject;
                                    let ns_window: *mut AnyObject =
                                        objc2::msg_send![ns_view, window];

                                    // Re-apply window non-opaque flag.
                                    let _: () = objc2::msg_send![
                                        ns_window, setOpaque: false
                                    ];

                                    // Re-apply Metal layer non-opaque so
                                    // transparent Slint pixels pass through.
                                    let layer: *mut AnyObject =
                                        objc2::msg_send![ns_view, layer];
                                    if !layer.is_null() {
                                        let _: () = objc2::msg_send![
                                            layer, setOpaque: false
                                        ];
                                    }
                                }
                            }
                        }
                    }
                });
            },
        );
        t
    };

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

#[cfg(test)]
mod tasku_button_tests {
    use super::tasku_button_command;

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Buttons whose command is `\x02tasku <subcmd>{pf}\n` — inline-exec
    /// protocol, project filter appended before the newline.
    const FILTERED_BUTTONS: &[(&str, &str)] = &[
        ("List",    "\x02tasku list"),
        ("Today",   "\x02tasku list --today"),
        ("Tmrw",    "\x02tasku list --tomorrow"),
        ("Overdue", "\x02tasku list --overdue"),
        ("Todo",    "\x02tasku list --status todo"),
        ("Started", "\x02tasku list --status in_progress"),
        ("Done",    "\x02tasku list --status done"),
    ];

    // ── list / filter buttons ────────────────────────────────────────────────

    #[test]
    fn list_no_project() {
        let cmd = tasku_button_command("List", "", "").unwrap();
        assert_eq!(cmd, "\x02tasku list\n");
    }

    #[test]
    fn list_with_project() {
        let cmd = tasku_button_command("List", " --project Mado", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --project Mado\n");
    }

    #[test]
    fn list_all_ignores_project_filter() {
        // "List all" always shows every task regardless of active project.
        let with    = tasku_button_command("List all", " --project Mado", "").unwrap();
        let without = tasku_button_command("List all", "", "").unwrap();
        assert_eq!(with, without);
        assert_eq!(with, "\x02tasku list\n");
    }

    #[test]
    fn today_no_project() {
        let cmd = tasku_button_command("Today", "", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --today\n");
    }

    #[test]
    fn today_with_project() {
        let cmd = tasku_button_command("Today", " --project Mado", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --today --project Mado\n");
    }

    #[test]
    fn tmrw_no_project() {
        let cmd = tasku_button_command("Tmrw", "", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --tomorrow\n");
    }

    #[test]
    fn tmrw_with_project() {
        let cmd = tasku_button_command("Tmrw", " --project Mado", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --tomorrow --project Mado\n");
    }

    #[test]
    fn overdue_no_project() {
        let cmd = tasku_button_command("Overdue", "", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --overdue\n");
    }

    #[test]
    fn overdue_with_project() {
        let cmd = tasku_button_command("Overdue", " --project Mado", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --overdue --project Mado\n");
    }

    #[test]
    fn todo_no_project() {
        let cmd = tasku_button_command("Todo", "", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --status todo\n");
    }

    #[test]
    fn todo_with_project() {
        let cmd = tasku_button_command("Todo", " --project Mado", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --status todo --project Mado\n");
    }

    #[test]
    fn started_no_project() {
        let cmd = tasku_button_command("Started", "", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --status in_progress\n");
    }

    #[test]
    fn started_with_project() {
        let cmd = tasku_button_command("Started", " --project Mado", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --status in_progress --project Mado\n");
    }

    #[test]
    fn done_no_project() {
        let cmd = tasku_button_command("Done", "", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --status done\n");
    }

    #[test]
    fn done_with_project() {
        let cmd = tasku_button_command("Done", " --project Mado", "").unwrap();
        assert_eq!(cmd, "\x02tasku list --status done --project Mado\n");
    }

    // ── project filter is ignored by Add / Edit / Delete / SQL ───────────────

    #[test]
    fn add_ignores_project_filter() {
        let with    = tasku_button_command("Add", " --project Mado", "").unwrap();
        let without = tasku_button_command("Add", "", "").unwrap();
        assert_eq!(with, without);
        assert_eq!(with, "\x03tasku add -i; tasku list\n");
    }

    #[test]
    fn sql_ignores_project_filter() {
        let with    = tasku_button_command("SQL", " --project Mado", "").unwrap();
        let without = tasku_button_command("SQL", "", "").unwrap();
        assert_eq!(with, without);
        // SQL uses the inline-exec protocol to open a query prompt in MadoList.
        assert_eq!(with, "\x02SQL\n");
    }

    #[test]
    fn edit_ignores_project_filter() {
        let with    = tasku_button_command("Edit", " --project Mado", "42").unwrap();
        let without = tasku_button_command("Edit", "", "42").unwrap();
        assert_eq!(with, without);
    }

    #[test]
    fn delete_ignores_project_filter() {
        let with    = tasku_button_command("Delete", " --project Mado", "42").unwrap();
        let without = tasku_button_command("Delete", "", "42").unwrap();
        assert_eq!(with, without);
    }

    // ── Edit / Delete require a selected task ID ─────────────────────────────

    #[test]
    fn edit_with_selection() {
        let cmd = tasku_button_command("Edit", "", "42").unwrap();
        assert_eq!(cmd, "\x03tasku edit 42 -i; tasku list\n");
    }

    #[test]
    fn edit_without_selection_returns_none() {
        assert!(tasku_button_command("Edit", "", "").is_none());
    }

    #[test]
    fn delete_with_selection() {
        let cmd = tasku_button_command("Delete", "", "42").unwrap();
        assert_eq!(cmd, "\x03tasku delete 42 -f; tasku list\n");
    }

    #[test]
    fn delete_without_selection_returns_none() {
        assert!(tasku_button_command("Delete", "", "").is_none());
    }

    // ── interactive commands start with Ctrl-C; filter commands use STX ────────

    #[test]
    fn interactive_commands_start_with_ctrl_c() {
        // Add / Edit / Delete interrupt MadoList and hand off to the shell.
        for (label, pf, sel) in [("Add", "", ""), ("Edit", "", "1"), ("Delete", "", "1")] {
            let cmd = tasku_button_command(label, pf, sel)
                .unwrap_or_else(|| panic!("{label} returned None"));
            assert!(cmd.starts_with('\x03'), "{label}: expected \\x03 prefix, got {cmd:?}");
        }
    }

    #[test]
    fn filter_commands_start_with_stx() {
        // Filter / display commands use the STX inline-exec protocol.
        let filter_cases = [
            ("List",     "",  ""),
            ("List all", "",  ""),
            ("Today",    "",  ""),
            ("Tmrw",     "",  ""),
            ("Overdue",  "",  ""),
            ("Todo",     "",  ""),
            ("Started",  "",  ""),
            ("Done",     "",  ""),
            ("SQL",      "",  ""),
        ];
        for (label, pf, sel) in filter_cases {
            let cmd = tasku_button_command(label, pf, sel)
                .unwrap_or_else(|| panic!("{label} returned None"));
            assert!(cmd.starts_with('\x02'), "{label}: expected \\x02 prefix, got {cmd:?}");
        }
    }

    // ── all commands end with a newline ───────────────────────────────────────

    #[test]
    fn all_commands_end_with_newline() {
        let cases = [
            ("Add",      "",  ""),
            ("List",     "",  ""),
            ("List all", "",  ""),
            ("Today",    "",  ""),
            ("Tmrw",    "",  ""),
            ("Overdue", "",  ""),
            ("Todo",    "",  ""),
            ("Started", "",  ""),
            ("Done",    "",  ""),
            ("SQL",     "",  ""),
            ("Edit",    "",  "1"),
            ("Delete",  "",  "1"),
        ];
        for (label, pf, sel) in cases {
            let cmd = tasku_button_command(label, pf, sel)
                .unwrap_or_else(|| panic!("{label} returned None"));
            assert!(
                cmd.ends_with('\n'),
                "{label}: expected trailing newline, got {cmd:?}"
            );
        }
    }

    // ── unknown label returns None ────────────────────────────────────────────

    #[test]
    fn unknown_label_returns_none() {
        assert!(tasku_button_command("Foo", "", "").is_none());
        assert!(tasku_button_command("", "", "").is_none());
    }

    // ── project filter is appended at the right position ─────────────────────

    #[test]
    fn project_filter_position_for_all_filtered_buttons() {
        let pf = " --project MyApp";
        for (label, prefix) in FILTERED_BUTTONS {
            let cmd = tasku_button_command(label, pf, "").unwrap();
            let expected = format!("{prefix}{pf}\n");
            assert_eq!(cmd, expected, "button: {label}");
        }
    }
}
