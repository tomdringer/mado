use std::collections::HashMap;
use serde::{Deserialize, Serialize};

pub type NodeId = u32;

// ── Workspace serialization types ─────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SavedNode {
    Leaf {
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_command: Option<String>,
    },
    Split {
        dir: String,   // "h" = horizontal, "v" = vertical
        ratio: f32,
        first: Box<SavedNode>,
        second: Box<SavedNode>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    Horizontal, // left | right
    Vertical,   // top / bottom
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavDir { Left, Right, Up, Down }

pub enum PaneNode {
    Leaf { color: u32 },
    Split { id: NodeId, dir: SplitDir, ratio: f32, first: NodeId, second: NodeId },
}

pub struct FlatPaneData {
    pub id: NodeId,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: u32,
    pub is_closable: bool,
}

#[derive(Clone)]
pub struct FlatDividerData {
    pub split_id: NodeId,
    pub dir: SplitDir,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub split_size: f32,
    pub current_ratio: f32,
}

pub struct PaneTree {
    nodes: HashMap<NodeId, PaneNode>,
    pub root: NodeId,
    next_id: NodeId,
}

const DIVIDER_HIT: f32 = 10.0; // interactive hit area — panes fill edge-to-edge, divider overlays on top

static COLORS: [u32; 6] = [
    0xFF1E2030,
    0xFF24283B,
    0xFF16161E,
    0xFF1A1B26,
    0xFF1F2335,
    0xFF222436,
];

impl PaneTree {
    pub fn new() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(0, PaneNode::Leaf { color: COLORS[0] });
        PaneTree { nodes, root: 0, next_id: 1 }
    }

    fn new_id(&mut self) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Split a leaf pane. Returns `(relocated_id, new_id)` — the original pane
    /// moved to `relocated_id` and a brand-new pane at `new_id`. Returns `None`
    /// if `target_id` is not a leaf.
    pub fn split(&mut self, target_id: NodeId, dir: SplitDir) -> Option<(NodeId, NodeId)> {
        let existing_color = match self.nodes.get(&target_id) {
            Some(PaneNode::Leaf { color, .. }) => *color,
            _ => return None,
        };

        let relocated_id = self.new_id();
        let new_leaf_id = self.new_id();
        let new_color = COLORS[(new_leaf_id as usize) % COLORS.len()];

        self.nodes.insert(relocated_id, PaneNode::Leaf { color: existing_color });
        self.nodes.insert(new_leaf_id, PaneNode::Leaf { color: new_color });

        // Replace target_id in-place with a Split node so any parent reference stays valid
        self.nodes.insert(target_id, PaneNode::Split {
            id: target_id,
            dir,
            ratio: 0.5,
            first: relocated_id,
            second: new_leaf_id,
        });

        Some((relocated_id, new_leaf_id))
    }

    /// Returns all current leaf pane IDs (order undefined).
    pub fn leaf_ids(&self) -> Vec<NodeId> {
        self.nodes.iter()
            .filter_map(|(id, node)| matches!(node, PaneNode::Leaf { .. }).then_some(*id))
            .collect()
    }

    /// Close a leaf pane.
    ///
    /// When the sibling is also a leaf, it gets promoted into the parent's slot
    /// and its node ID changes from `sibling_id` → `parent_id`.  The caller
    /// must remap any external maps (images, registry) accordingly.
    ///
    /// Returns `Some((old_id, new_id))` when a leaf was remapped, `None` otherwise.
    pub fn close(&mut self, target_id: NodeId) -> Option<(NodeId, NodeId)> {
        if target_id == self.root {
            return None;
        }

        let parent_id = match self.find_parent(target_id) {
            Some(id) => id,
            None => return None,
        };

        let sibling_id = match self.nodes.get(&parent_id) {
            Some(PaneNode::Split { first, second, .. }) => {
                if *first == target_id { *second } else { *first }
            }
            _ => return None,
        };

        self.nodes.remove(&target_id);

        let sibling = match self.nodes.remove(&sibling_id) {
            Some(n) => n,
            None => return None,
        };

        // Promote sibling into parent's slot (keeps parent_id stable for grandparent)
        match sibling {
            PaneNode::Leaf { color, .. } => {
                self.nodes.insert(parent_id, PaneNode::Leaf { color });
                Some((sibling_id, parent_id))
            }
            PaneNode::Split { dir, ratio, first, second, .. } => {
                self.nodes.insert(parent_id, PaneNode::Split { id: parent_id, dir, ratio, first, second });
                None
            }
        }
    }

    pub fn set_ratio(&mut self, split_id: NodeId, ratio: f32) {
        if let Some(PaneNode::Split { ratio: r, .. }) = self.nodes.get_mut(&split_id) {
            *r = ratio.clamp(0.05, 0.95);
        }
    }

    pub fn flatten(&self, w: f32, h: f32) -> Vec<FlatPaneData> {
        let mut result = Vec::new();
        let is_only_pane = matches!(self.nodes.get(&self.root), Some(PaneNode::Leaf { .. }));
        self.flatten_node(self.root, 0.0, 0.0, w, h, is_only_pane, &mut result);
        result
    }

    fn flatten_node(&self, id: NodeId, x: f32, y: f32, w: f32, h: f32, is_only_pane: bool, result: &mut Vec<FlatPaneData>) {
        match self.nodes.get(&id) {
            Some(PaneNode::Leaf { color, .. }) => {
                result.push(FlatPaneData { id, x, y, width: w, height: h, color: *color, is_closable: !is_only_pane });
            }
            Some(PaneNode::Split { dir, ratio, first, second, .. }) => {
                match dir {
                    SplitDir::Horizontal => {
                        let split = w * ratio;
                        self.flatten_node(*first, x, y, split, h, false, result);
                        self.flatten_node(*second, x + split, y, w - split, h, false, result);
                    }
                    SplitDir::Vertical => {
                        let split = h * ratio;
                        self.flatten_node(*first, x, y, w, split, false, result);
                        self.flatten_node(*second, x, y + split, w, h - split, false, result);
                    }
                }
            }
            None => {}
        }
    }

    /// Return the id of the nearest pane in `dir` relative to `id`,
    /// using flattened geometry. Returns `None` if no pane lies in that direction.
    pub fn neighbor(&self, id: NodeId, dir: NavDir, w: f32, h: f32) -> Option<NodeId> {
        let panes = self.flatten(w, h);
        let cur = panes.iter().find(|p| p.id == id)?;

        let candidates: Vec<&FlatPaneData> = panes.iter()
            .filter(|p| p.id != id)
            .filter(|p| match dir {
                NavDir::Left  => p.x + p.width  <= cur.x + 2.0,
                NavDir::Right => p.x             >= cur.x + cur.width - 2.0,
                NavDir::Up    => p.y + p.height  <= cur.y + 2.0,
                NavDir::Down  => p.y             >= cur.y + cur.height - 2.0,
            })
            .collect();

        // Among candidates, pick the closest by primary axis then secondary axis.
        candidates.into_iter().min_by(|a, b| {
            let primary = |p: &FlatPaneData| match dir {
                NavDir::Left  => cur.x - (p.x + p.width),
                NavDir::Right => p.x - (cur.x + cur.width),
                NavDir::Up    => cur.y - (p.y + p.height),
                NavDir::Down  => p.y - (cur.y + cur.height),
            };
            let secondary = |p: &FlatPaneData| match dir {
                NavDir::Left | NavDir::Right =>
                    ((p.y + p.height / 2.0) - (cur.y + cur.height / 2.0)).abs(),
                NavDir::Up | NavDir::Down =>
                    ((p.x + p.width / 2.0) - (cur.x + cur.width / 2.0)).abs(),
            };
            primary(a).partial_cmp(&primary(b)).unwrap_or(std::cmp::Ordering::Equal)
                .then(secondary(a).partial_cmp(&secondary(b)).unwrap_or(std::cmp::Ordering::Equal))
        }).map(|p| p.id)
    }

    pub fn flatten_dividers(&self, w: f32, h: f32) -> Vec<FlatDividerData> {
        let mut result = Vec::new();
        self.flatten_dividers_node(self.root, 0.0, 0.0, w, h, &mut result);
        result
    }

    fn flatten_dividers_node(&self, id: NodeId, x: f32, y: f32, w: f32, h: f32, result: &mut Vec<FlatDividerData>) {
        match self.nodes.get(&id) {
            Some(PaneNode::Leaf { .. }) => {}
            Some(PaneNode::Split { id: split_id, dir, ratio, first, second }) => {
                match dir {
                    SplitDir::Horizontal => {
                        let split = w * ratio;
                        result.push(FlatDividerData {
                            split_id: *split_id, dir: *dir,
                            x: x + split - DIVIDER_HIT / 2.0, y,
                            width: DIVIDER_HIT, height: h,
                            split_size: w, current_ratio: *ratio,
                        });
                        self.flatten_dividers_node(*first, x, y, split, h, result);
                        self.flatten_dividers_node(*second, x + split, y, w - split, h, result);
                    }
                    SplitDir::Vertical => {
                        let split = h * ratio;
                        result.push(FlatDividerData {
                            split_id: *split_id, dir: *dir,
                            x, y: y + split - DIVIDER_HIT / 2.0,
                            width: w, height: DIVIDER_HIT,
                            split_size: h, current_ratio: *ratio,
                        });
                        self.flatten_dividers_node(*first, x, y, w, split, result);
                        self.flatten_dividers_node(*second, x, y + split, w, h - split, result);
                    }
                }
            }
            None => {}
        }
    }

    fn find_parent(&self, target_id: NodeId) -> Option<NodeId> {
        for (id, node) in &self.nodes {
            if let PaneNode::Split { first, second, .. } = node {
                if *first == target_id || *second == target_id {
                    return Some(*id);
                }
            }
        }
        None
    }

    // ── Workspace save / restore ──────────────────────────────────────────────

    /// Serialise the tree. `cwds` maps leaf id → last known working directory
    /// (collected from OSC 7 tracking). Leaves without a cwd entry fall back
    /// to $HOME so the saved file is always valid. `snapshots` maps leaf id →
    /// ANSI bytes (base64-encoded) for visual terminal restoration on next open.
    pub fn to_saved(
        &self,
        cwds: &HashMap<NodeId, String>,
        snapshots: &HashMap<NodeId, String>,
        last_commands: &HashMap<NodeId, String>,
    ) -> SavedNode {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        self.node_to_saved(self.root, cwds, snapshots, last_commands, &home)
    }

    fn node_to_saved(
        &self,
        id: NodeId,
        cwds: &HashMap<NodeId, String>,
        snapshots: &HashMap<NodeId, String>,
        last_commands: &HashMap<NodeId, String>,
        home: &str,
    ) -> SavedNode {
        match self.nodes.get(&id) {
            Some(PaneNode::Leaf { .. }) => SavedNode::Leaf {
                cwd: cwds.get(&id).cloned().unwrap_or_else(|| home.to_string()),
                snapshot: snapshots.get(&id).cloned(),
                last_command: last_commands.get(&id).cloned(),
            },
            Some(PaneNode::Split { dir, ratio, first, second, .. }) => SavedNode::Split {
                dir: if *dir == SplitDir::Horizontal { "h".to_string() } else { "v".to_string() },
                ratio: *ratio,
                first:  Box::new(self.node_to_saved(*first,  cwds, snapshots, last_commands, home)),
                second: Box::new(self.node_to_saved(*second, cwds, snapshots, last_commands, home)),
            },
            None => SavedNode::Leaf { cwd: home.to_string(), snapshot: None, last_command: None },
        }
    }

    /// Rebuild a tree from a saved snapshot.
    /// Returns `(tree, cwd_map, snapshot_map, last_command_map)`.
    /// `last_command_map` maps leaf id → command text to pre-type on restore.
    pub fn from_saved(
        saved: &SavedNode,
    ) -> (PaneTree, HashMap<NodeId, String>, HashMap<NodeId, String>, HashMap<NodeId, String>) {
        let mut tree = PaneTree { nodes: HashMap::new(), root: 0, next_id: 0 };
        let mut cwds = HashMap::new();
        let mut snapshots = HashMap::new();
        let mut last_commands = HashMap::new();
        let root_id = tree.build_from_saved(saved, &mut cwds, &mut snapshots, &mut last_commands);
        tree.root = root_id;
        (tree, cwds, snapshots, last_commands)
    }

    fn build_from_saved(
        &mut self,
        node: &SavedNode,
        cwds: &mut HashMap<NodeId, String>,
        snapshots: &mut HashMap<NodeId, String>,
        last_commands: &mut HashMap<NodeId, String>,
    ) -> NodeId {
        let id = self.new_id();
        match node {
            SavedNode::Leaf { cwd, snapshot, last_command } => {
                let color = COLORS[id as usize % COLORS.len()];
                self.nodes.insert(id, PaneNode::Leaf { color });
                cwds.insert(id, cwd.clone());
                if let Some(s) = snapshot {
                    snapshots.insert(id, s.clone());
                }
                if let Some(cmd) = last_command {
                    last_commands.insert(id, cmd.clone());
                }
            }
            SavedNode::Split { dir, ratio, first, second } => {
                // Build children first so IDs are allocated in depth-first order
                let first_id  = self.build_from_saved(first,  cwds, snapshots, last_commands);
                let second_id = self.build_from_saved(second, cwds, snapshots, last_commands);
                let split_dir = if dir == "h" { SplitDir::Horizontal } else { SplitDir::Vertical };
                self.nodes.insert(id, PaneNode::Split {
                    id,
                    dir: split_dir,
                    ratio: *ratio,
                    first:  first_id,
                    second: second_id,
                });
            }
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── new() ─────────────────────────────────────────────────────────────────

    #[test]
    fn new_has_single_leaf_at_root() {
        let t = PaneTree::new();
        assert_eq!(t.root, 0);
        assert!(matches!(t.nodes.get(&0), Some(PaneNode::Leaf { .. })));
        assert_eq!(t.leaf_ids().len(), 1);
    }

    // ── split() ───────────────────────────────────────────────────────────────

    #[test]
    fn split_horizontal_returns_ids_and_promotes_target() {
        let mut t = PaneTree::new();
        let (relocated, new_id) = t.split(0, SplitDir::Horizontal).unwrap();
        // Root slot is now a Split
        assert!(matches!(t.nodes.get(&0), Some(PaneNode::Split { dir: SplitDir::Horizontal, .. })));
        // Both children are leaves
        assert!(matches!(t.nodes.get(&relocated), Some(PaneNode::Leaf { .. })));
        assert!(matches!(t.nodes.get(&new_id),    Some(PaneNode::Leaf { .. })));
        assert_eq!(t.leaf_ids().len(), 2);
    }

    #[test]
    fn split_vertical_works() {
        let mut t = PaneTree::new();
        t.split(0, SplitDir::Vertical).unwrap();
        assert!(matches!(t.nodes.get(&0), Some(PaneNode::Split { dir: SplitDir::Vertical, .. })));
    }

    #[test]
    fn split_non_leaf_returns_none() {
        let mut t = PaneTree::new();
        t.split(0, SplitDir::Horizontal).unwrap(); // 0 is now a Split
        assert!(t.split(0, SplitDir::Horizontal).is_none());
    }

    #[test]
    fn split_default_ratio_is_half() {
        let mut t = PaneTree::new();
        t.split(0, SplitDir::Horizontal).unwrap();
        if let Some(PaneNode::Split { ratio, .. }) = t.nodes.get(&0) {
            assert!((*ratio - 0.5).abs() < f32::EPSILON);
        } else {
            panic!("expected Split at root");
        }
    }

    // ── close() ───────────────────────────────────────────────────────────────

    #[test]
    fn close_root_returns_none() {
        let mut t = PaneTree::new();
        assert!(t.close(0).is_none());
    }

    #[test]
    fn close_leaf_promotes_sibling_leaf() {
        let mut t = PaneTree::new();
        let (relocated, new_id) = t.split(0, SplitDir::Horizontal).unwrap();
        // Close the new pane; sibling (relocated) should be promoted into slot 0
        let remap = t.close(new_id);
        assert!(matches!(remap, Some((old, new_slot)) if old == relocated && new_slot == 0));
        // Tree is back to a single leaf
        assert!(matches!(t.nodes.get(&0), Some(PaneNode::Leaf { .. })));
        assert_eq!(t.leaf_ids().len(), 1);
    }

    #[test]
    fn close_leaf_when_sibling_is_split_returns_none_remap() {
        let mut t = PaneTree::new();
        let (relocated, _new_id) = t.split(0, SplitDir::Horizontal).unwrap();
        // Split the relocated pane again
        let (_, _) = t.split(relocated, SplitDir::Vertical).unwrap();
        // Now close _new_id — sibling is a Split, so no leaf remap expected
        let remap = t.close(_new_id);
        assert!(remap.is_none());
    }

    // ── flatten() ─────────────────────────────────────────────────────────────

    #[test]
    fn flatten_single_pane_fills_viewport() {
        let t = PaneTree::new();
        let panes = t.flatten(1000.0, 600.0);
        assert_eq!(panes.len(), 1);
        let p = &panes[0];
        assert_eq!(p.id, 0);
        assert_eq!((p.x, p.y), (0.0, 0.0));
        assert_eq!((p.width, p.height), (1000.0, 600.0));
        assert!(!p.is_closable); // only pane is not closable
    }

    #[test]
    fn flatten_horizontal_split_halves_width() {
        let mut t = PaneTree::new();
        let (left_id, right_id) = t.split(0, SplitDir::Horizontal).unwrap();
        let panes = t.flatten(1000.0, 600.0);
        assert_eq!(panes.len(), 2);

        let left  = panes.iter().find(|p| p.id == left_id).unwrap();
        let right = panes.iter().find(|p| p.id == right_id).unwrap();

        assert!((left.width  - 500.0).abs() < 1.0);
        assert!((right.width - 500.0).abs() < 1.0);
        assert!((right.x     - 500.0).abs() < 1.0);
        assert!(left.is_closable && right.is_closable);
    }

    #[test]
    fn flatten_vertical_split_halves_height() {
        let mut t = PaneTree::new();
        let (top_id, bot_id) = t.split(0, SplitDir::Vertical).unwrap();
        let panes = t.flatten(1000.0, 600.0);
        let top = panes.iter().find(|p| p.id == top_id).unwrap();
        let bot = panes.iter().find(|p| p.id == bot_id).unwrap();
        assert!((top.height - 300.0).abs() < 1.0);
        assert!((bot.height - 300.0).abs() < 1.0);
        assert!((bot.y      - 300.0).abs() < 1.0);
    }

    // ── set_ratio() ───────────────────────────────────────────────────────────

    #[test]
    fn set_ratio_clamps_to_valid_range() {
        let mut t = PaneTree::new();
        t.split(0, SplitDir::Horizontal).unwrap();
        t.set_ratio(0, 0.0);
        if let Some(PaneNode::Split { ratio, .. }) = t.nodes.get(&0) {
            assert!((*ratio - 0.05).abs() < f32::EPSILON);
        }
        t.set_ratio(0, 1.0);
        if let Some(PaneNode::Split { ratio, .. }) = t.nodes.get(&0) {
            assert!((*ratio - 0.95).abs() < f32::EPSILON);
        }
    }

    // ── neighbor() ────────────────────────────────────────────────────────────

    #[test]
    fn neighbor_left_right_in_horizontal_split() {
        let mut t = PaneTree::new();
        let (left_id, right_id) = t.split(0, SplitDir::Horizontal).unwrap();
        assert_eq!(t.neighbor(left_id,  NavDir::Right, 1000.0, 600.0), Some(right_id));
        assert_eq!(t.neighbor(right_id, NavDir::Left,  1000.0, 600.0), Some(left_id));
        assert_eq!(t.neighbor(left_id,  NavDir::Left,  1000.0, 600.0), None);
        assert_eq!(t.neighbor(right_id, NavDir::Right, 1000.0, 600.0), None);
    }

    #[test]
    fn neighbor_up_down_in_vertical_split() {
        let mut t = PaneTree::new();
        let (top_id, bot_id) = t.split(0, SplitDir::Vertical).unwrap();
        assert_eq!(t.neighbor(top_id, NavDir::Down, 1000.0, 600.0), Some(bot_id));
        assert_eq!(t.neighbor(bot_id, NavDir::Up,   1000.0, 600.0), Some(top_id));
        assert_eq!(t.neighbor(top_id, NavDir::Up,   1000.0, 600.0), None);
    }

    // ── to_saved / from_saved roundtrip ──────────────────────────────────────

    #[test]
    fn save_restore_roundtrip_single_pane() {
        let t = PaneTree::new();
        let cwds = HashMap::from([(0u32, "/home/user".to_string())]);
        let saved = t.to_saved(&cwds, &HashMap::new(), &HashMap::new());
        let (t2, cwds2, snaps2, _) = PaneTree::from_saved(&saved);
        assert_eq!(t2.leaf_ids().len(), 1);
        assert_eq!(cwds2.values().next().unwrap(), "/home/user");
        assert!(snaps2.is_empty());
    }

    #[test]
    fn save_restore_roundtrip_split() {
        let mut t = PaneTree::new();
        let (left_id, right_id) = t.split(0, SplitDir::Horizontal).unwrap();
        let cwds = HashMap::from([
            (left_id,  "/left".to_string()),
            (right_id, "/right".to_string()),
        ]);
        let saved = t.to_saved(&cwds, &HashMap::new(), &HashMap::new());
        let (t2, cwds2, snaps2, _) = PaneTree::from_saved(&saved);
        assert_eq!(t2.leaf_ids().len(), 2);
        let mut paths: Vec<&String> = cwds2.values().collect();
        paths.sort();
        assert_eq!(paths, vec!["/left", "/right"]);
        assert!(snaps2.is_empty());
    }

    #[test]
    fn to_saved_uses_home_for_missing_cwd() {
        let t = PaneTree::new();
        let saved = t.to_saved(&HashMap::new(), &HashMap::new(), &HashMap::new()); // no cwd for root leaf
        if let SavedNode::Leaf { cwd, .. } = saved {
            // Falls back to $HOME or "/"
            assert!(!cwd.is_empty());
        } else {
            panic!("expected Leaf");
        }
    }

    #[test]
    fn snapshot_roundtrip_single_pane() {
        let t = PaneTree::new();
        let id = t.root;
        let cwds = HashMap::from([(id, "/tmp".to_string())]);
        let snaps_in = HashMap::from([(id, "ANSI_DATA".to_string())]);
        let saved = t.to_saved(&cwds, &snaps_in, &HashMap::new());
        let (_, _, snaps_out, _) = PaneTree::from_saved(&saved);
        assert_eq!(snaps_out.values().next().map(String::as_str), Some("ANSI_DATA"));
    }

    #[test]
    fn snapshot_roundtrip_split_two_panes() {
        let mut t = PaneTree::new();
        let (left_id, right_id) = t.split(0, SplitDir::Horizontal).unwrap();
        let cwds = HashMap::from([
            (left_id,  "/left".to_string()),
            (right_id, "/right".to_string()),
        ]);
        let snaps_in = HashMap::from([
            (left_id,  "LEFT_SNAP".to_string()),
            (right_id, "RIGHT_SNAP".to_string()),
        ]);
        let saved = t.to_saved(&cwds, &snaps_in, &HashMap::new());
        let (_, _, snaps_out, _) = PaneTree::from_saved(&saved);
        assert_eq!(snaps_out.len(), 2);
        let mut vals: Vec<&String> = snaps_out.values().collect();
        vals.sort();
        assert_eq!(vals, vec!["LEFT_SNAP", "RIGHT_SNAP"]);
    }

    #[test]
    fn snapshot_absent_when_not_provided() {
        let mut t = PaneTree::new();
        let (left_id, right_id) = t.split(0, SplitDir::Horizontal).unwrap();
        let cwds = HashMap::from([
            (left_id,  "/l".to_string()),
            (right_id, "/r".to_string()),
        ]);
        // Only provide snapshot for left pane.
        let snaps_in = HashMap::from([(left_id, "LEFT_ONLY".to_string())]);
        let saved = t.to_saved(&cwds, &snaps_in, &HashMap::new());
        let (_, _, snaps_out, _) = PaneTree::from_saved(&saved);
        assert_eq!(snaps_out.len(), 1);
        assert!(snaps_out.values().any(|v| v == "LEFT_ONLY"));
    }

    #[test]
    fn snapshot_serde_skips_none() {
        // Leaves with no snapshot should not include the key in JSON.
        let t = PaneTree::new();
        let cwds = HashMap::from([(t.root, "/x".to_string())]);
        let saved = t.to_saved(&cwds, &HashMap::new(), &HashMap::new());
        let json = serde_json::to_string(&saved).unwrap();
        assert!(!json.contains("snapshot"), "snapshot key should be absent when None: {}", json);
    }

    #[test]
    fn snapshot_serde_roundtrip_json() {
        // Snapshot is preserved through JSON serialization/deserialization.
        let t = PaneTree::new();
        let id = t.root;
        let cwds = HashMap::from([(id, "/y".to_string())]);
        let snaps_in = HashMap::from([(id, "PAYLOAD".to_string())]);
        let saved = t.to_saved(&cwds, &snaps_in, &HashMap::new());
        let json = serde_json::to_string(&saved).unwrap();
        let restored: SavedNode = serde_json::from_str(&json).unwrap();
        let (_, _, snaps_out, _) = PaneTree::from_saved(&restored);
        assert_eq!(snaps_out.values().next().map(String::as_str), Some("PAYLOAD"));
    }
}
