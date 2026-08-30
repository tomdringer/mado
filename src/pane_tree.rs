use std::collections::HashMap;
use serde::{Deserialize, Serialize};

pub type NodeId = u32;

// ── Workspace serialization types ─────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SavedNode {
    Leaf { cwd: String },
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
    /// to $HOME so the saved file is always valid.
    pub fn to_saved(&self, cwds: &HashMap<NodeId, String>) -> SavedNode {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        self.node_to_saved(self.root, cwds, &home)
    }

    fn node_to_saved(&self, id: NodeId, cwds: &HashMap<NodeId, String>, home: &str) -> SavedNode {
        match self.nodes.get(&id) {
            Some(PaneNode::Leaf { .. }) => SavedNode::Leaf {
                cwd: cwds.get(&id).cloned().unwrap_or_else(|| home.to_string()),
            },
            Some(PaneNode::Split { dir, ratio, first, second, .. }) => SavedNode::Split {
                dir: if *dir == SplitDir::Horizontal { "h".to_string() } else { "v".to_string() },
                ratio: *ratio,
                first:  Box::new(self.node_to_saved(*first,  cwds, home)),
                second: Box::new(self.node_to_saved(*second, cwds, home)),
            },
            None => SavedNode::Leaf { cwd: home.to_string() },
        }
    }

    /// Rebuild a tree from a saved snapshot.
    /// Returns the new tree plus a map of leaf-id → cwd to use when spawning PTYs.
    pub fn from_saved(saved: &SavedNode) -> (PaneTree, HashMap<NodeId, String>) {
        let mut tree = PaneTree { nodes: HashMap::new(), root: 0, next_id: 0 };
        let mut cwds = HashMap::new();
        let root_id = tree.build_from_saved(saved, &mut cwds);
        tree.root = root_id;
        (tree, cwds)
    }

    fn build_from_saved(&mut self, node: &SavedNode, cwds: &mut HashMap<NodeId, String>) -> NodeId {
        let id = self.new_id();
        match node {
            SavedNode::Leaf { cwd } => {
                let color = COLORS[id as usize % COLORS.len()];
                self.nodes.insert(id, PaneNode::Leaf { color });
                cwds.insert(id, cwd.clone());
            }
            SavedNode::Split { dir, ratio, first, second } => {
                // Build children first so IDs are allocated in depth-first order
                let first_id  = self.build_from_saved(first,  cwds);
                let second_id = self.build_from_saved(second, cwds);
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
