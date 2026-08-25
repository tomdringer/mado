mod clock;
mod config;
mod pane_tree;
mod sidebar;
mod tasku;
mod terminal;
mod workspace;

use pane_tree::PaneTree;

use std::cell::RefCell;
use std::rc::Rc;
use std::collections::HashMap;

use config::Config;
use pane_tree::{FlatDividerData, NodeId, SplitDir};
use sidebar::SidebarState;
use tasku::detect as detect_tasku;
use terminal::TerminalRegistry;
use slint::{Image, Model, ModelRc, Timer, TimerMode, VecModel};

// Special NodeIds for sidebar terminals — must not collide with pane IDs.
// Pane IDs are allocated sequentially from the PaneTree counter; u32::MAX
// and u32::MAX-1 are safe sentinels.
const SIDEBAR_TASKU_ID: NodeId = u32::MAX;
const SIDEBAR_AI_ID:    NodeId = u32::MAX - 1;

// Default panel heights (match sidebar.slint initial values).
pub const DEFAULT_TASKU_PANEL_H: f32 = 400.0;
pub const DEFAULT_AI_PANEL_H:    f32 = 300.0;

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

// Fixed height of the AI panel: top padding (8 px) + resize handle (8 px).
const AI_PANEL_FIXED_H: f32 = 16.0;

/// Terminal rect height inside the Tasku panel.
fn tasku_terminal_h(panel_h: f32) -> f32 {
    (panel_h - TASKU_PANEL_FIXED_H).max(50.0)
}

/// Terminal rect height inside the AI panel.
fn ai_terminal_h(panel_h: f32) -> f32 {
    (panel_h - AI_PANEL_FIXED_H).max(50.0)
}

slint::include_modules!();

use i_slint_backend_winit::WinitWindowAccessor;

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

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() {
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

    // Load config first — font size, shell, etc.
    let config = Config::load();
    let default_font_size = config.font_size;
    let shell = config.resolved_shell();

    // Get device pixel ratio once at startup for HiDPI-correct rendering
    let scale = ui.window().scale_factor();

    let tree = Rc::new(RefCell::new(PaneTree::new()));
    let registry = Rc::new(RefCell::new(TerminalRegistry::new(default_font_size, scale, shell)));

    // ── Sidebar + Tasku detection ────────────────────────────────────────────
    let tasku_path = detect_tasku().map(|t| t.path.to_string_lossy().into_owned());
    if let Some(ref p) = tasku_path {
        println!("mado: tasku found at {p}");
    } else {
        println!("mado: tasku not found");
    }

    let sidebar = Rc::new(RefCell::new(SidebarState::new(config.sidebar_width, tasku_path)));
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
            |(id, title, subtitle, icon)| PluginItem {
                id:       id.into(),
                title:    title.into(),
                subtitle: subtitle.into(),
                icon:     icon.into(),
            }
        ).collect();
        let plugin_model = Rc::new(VecModel::<PluginItem>::from(items));
        ui.set_plugins(ModelRc::new(Rc::clone(&plugin_model)));
    }

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

    // ── Spawn initial terminal ──────────────────────────────────────────────
    {
        let root_id = tree.borrow().root;
        let w = ui.get_window_w();
        let h = ui.get_window_h();
        registry.borrow_mut().spawn(root_id, w, h, None);
        *focused_id.borrow_mut() = Some(root_id);
    }

    // ── 60fps render timer ──────────────────────────────────────────────────
    // Uses set_row_data so PaneView instances (and their FocusScopes) are never
    // recreated — keyboard focus survives across frame updates.
    // Sidebar terminals are routed separately via their own set_*_terminal_image calls.
    {
        let registry = Rc::clone(&registry);
        let pane_model = Rc::clone(&pane_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let ui_weak = ui.as_weak();
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, std::time::Duration::from_millis(16), move || {
            let dirty_bufs = registry.borrow_mut().drain_dirty();
            if dirty_bufs.is_empty() { return; }

            let mut tasku_buf: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> = None;
            let mut ai_buf:    Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> = None;
            let mut pane_dirty_ids: Vec<NodeId> = Vec::new();
            let mut imgs = images.borrow_mut();

            for (id, buf) in dirty_bufs {
                if id == SIDEBAR_TASKU_ID {
                    tasku_buf = Some(buf);
                } else if id == SIDEBAR_AI_ID {
                    ai_buf = Some(buf);
                } else {
                    pane_dirty_ids.push(id);
                    imgs.insert(id, Image::from_rgba8(buf));
                }
            }

            if !pane_dirty_ids.is_empty() {
                push_images(&pane_model, &imgs, *focused_id.borrow(), Some(&pane_dirty_ids));
            }

            if tasku_buf.is_some() || ai_buf.is_some() {
                if let Some(ui) = ui_weak.upgrade() {
                    if let Some(buf) = tasku_buf {
                        ui.set_tasku_terminal_image(Image::from_rgba8(buf));
                    }
                    if let Some(buf) = ai_buf {
                        ui.set_ai_terminal_image(Image::from_rgba8(buf));
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
        let ui_weak = ui.as_weak();
        move |id, text, ctrl, meta| {
            if *focused_id.borrow() != Some(id as NodeId) { return; }

            // Zoom: Ctrl+= / Ctrl++ zoom in, Ctrl+- out, Ctrl+0 reset.
            // Also check meta (Cmd on macOS) as a fallback.
            let zoom_mod = ctrl || meta;
            let t = text.as_str();
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

    // ── Window resize ────────────────────────────────────────────────────────
    ui.on_window_resized({
        let tree = Rc::clone(&tree);
        let registry = Rc::clone(&registry);
        let dividers_cache = Rc::clone(&dividers_cache);
        let pane_model = Rc::clone(&pane_model);
        let div_model = Rc::clone(&div_model);
        let images = Rc::clone(&images);
        let focused_id = Rc::clone(&focused_id);
        let ui_weak = ui.as_weak();
        let last_size: Rc<RefCell<(f32, f32)>> = Rc::new(RefCell::new((0.0, 0.0)));
        move |w, h| {
            let (lw, lh) = *last_size.borrow();
            if (w - lw).abs() > 0.5 || (h - lh).abs() > 0.5 {
                *last_size.borrow_mut() = (w, h);
                let panes = tree.borrow().flatten(w, h);
                let mut reg = registry.borrow_mut();
                for p in &panes {
                    reg.resize(p.id, (p.width - PANE_H_INSET).max(10.0), (p.height - PANE_TOP_INSET).max(10.0));
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
                    |(id, title, subtitle, icon)| PluginItem {
                        id:       id.into(),
                        title:    title.into(),
                        subtitle: subtitle.into(),
                        icon:     icon.into(),
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
        move |new_width| {
            let (tasku_h, ai_h) = {
                let mut s = sidebar.borrow_mut();
                s.width = new_width;
                (s.tasku_panel_h, s.ai_panel_h)
            };
            let sidebar_w = (new_width - 16.0).max(50.0);
            let mut reg = registry.borrow_mut();
            reg.resize(SIDEBAR_TASKU_ID, sidebar_w, tasku_terminal_h(tasku_h));
            reg.resize(SIDEBAR_AI_ID,    sidebar_w, ai_terminal_h(ai_h));
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
                    ((s.width - 16.0).max(50.0), s.tasku_panel_h)
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
            let (new_tree, pane_cwds) = match workspace::load_workspace(&code) {
                Some(saved) => PaneTree::from_saved(&saved.tree),
                None => {
                    let t = PaneTree::new();
                    let root_id = t.root;
                    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
                    (t, [(root_id, home)].into())
                }
            };

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
                (s.width - 16.0).max(50.0)
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

    // ── AI panel expand/collapse ─────────────────────────────────────────────
    ui.on_ai_toggled({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        move |expanded| {
            let mut reg = registry.borrow_mut();
            if expanded {
                let (sidebar_w, panel_h) = {
                    let s = sidebar.borrow();
                    ((s.width - 16.0).max(50.0), s.ai_panel_h)
                };
                reg.spawn(SIDEBAR_AI_ID, sidebar_w, ai_terminal_h(panel_h), None);
            } else {
                reg.remove(SIDEBAR_AI_ID);
            }
        }
    });

    // ── AI panel height (drag-to-resize) ─────────────────────────────────────
    ui.on_ai_panel_height_changed({
        let registry = Rc::clone(&registry);
        let sidebar = Rc::clone(&sidebar);
        move |new_h| {
            let sidebar_w = {
                let mut s = sidebar.borrow_mut();
                s.ai_panel_h = new_h;
                (s.width - 16.0).max(50.0)
            };
            registry.borrow_mut().resize(SIDEBAR_AI_ID, sidebar_w, ai_terminal_h(new_h));
        }
    });

    // ── AI key input ─────────────────────────────────────────────────────────
    ui.on_ai_key_input({
        let registry = Rc::clone(&registry);
        move |text, ctrl, meta| {
            let zoom_mod = ctrl || meta;
            let t = text.as_str();
            // Let zoom shortcuts still work when the AI panel has keyboard focus
            if zoom_mod && (t == "=" || t == "+" || t == "-" || t == "0") { return; }
            let bytes = key_text_to_bytes(&text);
            registry.borrow_mut().write_key(SIDEBAR_AI_ID, &bytes);
        }
    });

    // ── AI terminal scroll wheel ─────────────────────────────────────────────
    ui.on_ai_scroll({
        let registry = Rc::clone(&registry);
        move |delta_px| {
            let cell_h_logical = {
                let reg = registry.borrow();
                reg.font.cell_h as f32 / reg.scale
            };
            if cell_h_logical > 0.0 {
                let delta_rows = (-delta_px / cell_h_logical).round() as i32;
                if delta_rows != 0 {
                    registry.borrow_mut().scroll(SIDEBAR_AI_ID, delta_rows);
                }
            }
        }
    });

    // ── Clock: initial values ────────────────────────────────────────────────
    ui.set_clock_data(ClockData {
        time: clock::current_time().into(),
        date: clock::current_date().into(),
        icon: "○".into(),
        temp: "--".into(),
        desc: "fetching…".into(),
        loc:  "".into(),
    });

    // ── Clock: 10-second time/date refresh ───────────────────────────────────
    {
        let ui_weak = ui.as_weak();
        let clock_timer = Timer::default();
        clock_timer.start(TimerMode::Repeated, std::time::Duration::from_secs(10), move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut data = ui.get_clock_data();
                data.time = clock::current_time().into();
                data.date = clock::current_date().into();
                ui.set_clock_data(data);
            }
        });
        std::mem::forget(clock_timer);
    }

    // ── Clock: weather fetch (startup + every 30 min) ────────────────────────
    {
        let ui_weak = ui.as_weak();
        std::thread::spawn(move || {
            loop {
                if let Some(w) = clock::fetch_weather() {
                    let icon    = w.icon;
                    let temp    = w.temp;
                    let desc    = w.desc;
                    let loc     = w.loc;
                    let handle  = ui_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = handle.upgrade() {
                            let mut data = ui.get_clock_data();
                            data.icon = icon.into();
                            data.temp = temp.into();
                            data.desc = desc.into();
                            data.loc  = loc.into();
                            ui.set_clock_data(data);
                        }
                    });
                }
                std::thread::sleep(std::time::Duration::from_secs(30 * 60));
            }
        });
    }

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

// ── Key translation ──────────────────────────────────────────────────────────

/// Translate Slint key text to terminal byte sequences.
///
/// Slint key codes (from i-slint-common/key_codes.rs):
///   Modifier keys live in the C0 control range (U+0010–U+0019) and MUST be
///   filtered — they alias Ctrl+P, Ctrl+U, Ctrl+Q etc. and cause visible damage.
///   Navigation/function keys are in the Specials range (U+F700+).
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

