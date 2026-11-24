//! Multiplexer data model shared by every session backend.
//!
//! A tab owns a binary split tree; leaves are panes. The backends (native PTYs
//! and tmux) only differ in how panes are created and how bytes travel; the
//! geometry and the UI contract live here.

use std::path::PathBuf;

pub type PaneId = u64;

/// `Horizontal` lays the two children out side by side (vertical divider),
/// `Vertical` stacks them (horizontal divider).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

pub const MIN_RATIO: f32 = 0.1;
pub const MAX_RATIO: f32 = 0.9;

#[derive(Clone, Debug, PartialEq)]
pub enum Layout {
    Leaf(PaneId),
    Split {
        dir: SplitDir,
        /// Fraction of the available space given to `first`, clamped to
        /// [`MIN_RATIO`]..=[`MAX_RATIO`].
        ratio: f32,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    pub fn leaf(pane: PaneId) -> Self {
        Self::Leaf(pane)
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Self::Leaf(id) => *id == pane,
            Self::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    pub fn ids(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect_ids(&mut out);
        out
    }

    fn collect_ids(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Leaf(id) => out.push(*id),
            Self::Split { first, second, .. } => {
                first.collect_ids(out);
                second.collect_ids(out);
            }
        }
    }

    pub fn first_leaf(&self) -> PaneId {
        match self {
            Self::Leaf(id) => *id,
            Self::Split { first, .. } => first.first_leaf(),
        }
    }

    /// Replace the leaf `target` with a 50/50 split holding `target` and
    /// `new_pane`. Returns false when `target` is not in this tree.
    pub fn split(&mut self, target: PaneId, new_pane: PaneId, dir: SplitDir) -> bool {
        match self {
            Self::Leaf(id) if *id == target => {
                *self = Self::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(Self::Leaf(target)),
                    second: Box::new(Self::Leaf(new_pane)),
                };
                true
            }
            Self::Leaf(_) => false,
            Self::Split { first, second, .. } => {
                first.split(target, new_pane, dir) || second.split(target, new_pane, dir)
            }
        }
    }

    /// Remove a leaf, collapsing its parent split so the sibling takes the
    /// parent's place. Returns false if this tree is just the target leaf (the
    /// caller must then close the tab) or the pane is absent.
    pub fn remove(&mut self, target: PaneId) -> bool {
        let Self::Split { first, second, .. } = self else {
            return false;
        };
        if matches!(**first, Self::Leaf(id) if id == target) {
            *self = (**second).clone();
            return true;
        }
        if matches!(**second, Self::Leaf(id) if id == target) {
            *self = (**first).clone();
            return true;
        }
        first.remove(target) || second.remove(target)
    }

    /// Set the ratio of the split reached by walking `path` (false = first
    /// child, true = second child). An empty path addresses the root split.
    pub fn set_ratio_at(&mut self, path: &[bool], ratio: f32) -> bool {
        let Some((&next, rest)) = path.split_first() else {
            return match self {
                Self::Split { ratio: r, .. } => {
                    *r = clamp_ratio(ratio);
                    true
                }
                Self::Leaf(_) => false,
            };
        };
        match self {
            Self::Split { first, second, .. } => {
                if next {
                    second.set_ratio_at(rest, ratio)
                } else {
                    first.set_ratio_at(rest, ratio)
                }
            }
            Self::Leaf(_) => false,
        }
    }

    /// Absolute cell rectangles `(pane, x, y, w, h)` for a viewport. Mirrors
    /// [`Layout::sizes`] so the UI can position panes without re-deriving
    /// split arithmetic.
    pub fn rects(&self, cols: usize, rows: usize) -> Vec<(PaneId, usize, usize, usize, usize)> {
        let mut out = Vec::new();
        self.rects_into(0, 0, cols.max(1), rows.max(1), &mut out);
        out
    }

    fn rects_into(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        out: &mut Vec<(PaneId, usize, usize, usize, usize)>,
    ) {
        match self {
            Self::Leaf(id) => out.push((*id, x, y, w.max(1), h.max(1))),
            Self::Split {
                dir,
                ratio,
                first,
                second,
            } => {
                let ratio = clamp_ratio(*ratio);
                match dir {
                    SplitDir::Horizontal => {
                        let avail = w.saturating_sub(1).max(2);
                        let left = ((avail as f32 * ratio).round() as usize).clamp(1, avail - 1);
                        first.rects_into(x, y, left, h, out);
                        second.rects_into(x + left + 1, y, avail - left, h, out);
                    }
                    SplitDir::Vertical => {
                        let avail = h.saturating_sub(1).max(2);
                        let top = ((avail as f32 * ratio).round() as usize).clamp(1, avail - 1);
                        first.rects_into(x, y, w, top, out);
                        second.rects_into(x, y + top + 1, w, avail - top, out);
                    }
                }
            }
        }
    }

    /// Pixel-independent geometry: each pane gets a cell rectangle. Splits
    /// spend exactly one cell on the divider. Degenerate viewports still give
    /// every pane at least 1x1 cells; the UI clips the overflow.
    pub fn sizes(&self, cols: usize, rows: usize) -> Vec<(PaneId, usize, usize)> {
        let mut out = Vec::new();
        self.sizes_into(0, 0, cols.max(1), rows.max(1), &mut out);
        out
    }

    fn sizes_into(
        &self,
        _x: usize,
        _y: usize,
        w: usize,
        h: usize,
        out: &mut Vec<(PaneId, usize, usize)>,
    ) {
        match self {
            Self::Leaf(id) => out.push((*id, w.max(1), h.max(1))),
            Self::Split {
                dir,
                ratio,
                first,
                second,
            } => {
                let ratio = clamp_ratio(*ratio);
                match dir {
                    SplitDir::Horizontal => {
                        let avail = w.saturating_sub(1).max(2);
                        let left = ((avail as f32 * ratio).round() as usize).clamp(1, avail - 1);
                        first.sizes_into(0, 0, left, h, out);
                        second.sizes_into(left + 1, 0, avail - left, h, out);
                    }
                    SplitDir::Vertical => {
                        let avail = h.saturating_sub(1).max(2);
                        let top = ((avail as f32 * ratio).round() as usize).clamp(1, avail - 1);
                        first.sizes_into(0, 0, w, top, out);
                        second.sizes_into(0, top + 1, w, avail - top, out);
                    }
                }
            }
        }
    }
}

pub fn clamp_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(MIN_RATIO, MAX_RATIO)
    } else {
        0.5
    }
}

#[derive(Clone, Debug)]
pub struct PaneInfo {
    pub id: PaneId,
    pub title: String,
    pub cwd: Option<PathBuf>,
    /// Cells, as last computed for this pane.
    pub size: (usize, usize),
}

#[derive(Clone, Debug)]
pub struct TabInfo {
    pub id: u64,
    pub title: String,
    pub layout: Layout,
    pub active: PaneId,
    pub panes: Vec<PaneInfo>,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub tabs: Vec<TabInfo>,
    pub active_tab: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxEvent {
    /// New bytes are available in the pane's `Term`.
    Output(PaneId),
    /// Tabs, layout, titles or focus changed; refresh the snapshot.
    Tabs,
    /// The backend is finished (last shell exited, tmux severed, ...).
    Exit,
}

use std::path::Path;
use tessera_term::Term;

/// Every session backend implements this. The UI owns exactly one `Box<dyn Mux>`.
///
/// Threading: reader threads only enqueue bytes and poke the wake channel;
/// `poll()` drains queues and mutates `Term`s on the caller's thread.
pub trait Mux {
    fn snapshot(&self) -> Snapshot;
    fn term(&self, pane: PaneId) -> Option<&Term>;
    /// Drain backend queues; returns what changed.
    fn poll(&mut self) -> Vec<MuxEvent>;
    /// Notification channel: a value is sent whenever `poll()` has work.
    fn wake(&self) -> async_channel::Receiver<()>;
    fn write(&mut self, pane: PaneId, bytes: &[u8]);
    /// Client area in cells; reassigns width/height to every visible pane.
    fn resize_client(&mut self, cols: usize, rows: usize);
    fn new_tab(&mut self, cwd: Option<&Path>);
    fn close_tab(&mut self, tab: u64);
    fn select_tab(&mut self, tab: u64);
    fn split(&mut self, pane: PaneId, dir: SplitDir) -> Option<PaneId>;
    fn close_pane(&mut self, pane: PaneId);
    fn focus_pane(&mut self, pane: PaneId);
    /// Set a split's ratio; `path` walks the layout (false = first child).
    fn set_split_ratio(&mut self, tab: u64, path: &[bool], ratio: f32);
    /// Ask the backend to stop. Called on app quit; tmux detaches, native kills.
    fn shutdown(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_replaces_leaf_and_keeps_order() {
        let mut l = Layout::leaf(1);
        assert!(l.split(1, 2, SplitDir::Horizontal));
        assert_eq!(l.ids(), vec![1, 2]);
        assert!(l.split(1, 3, SplitDir::Vertical));
        assert_eq!(l.ids(), vec![1, 3, 2]);
        assert!(!l.split(99, 4, SplitDir::Horizontal));
    }

    #[test]
    fn remove_collapses_parent() {
        let mut l = Layout::leaf(1);
        l.split(1, 2, SplitDir::Horizontal);
        l.split(2, 3, SplitDir::Vertical);
        assert!(l.remove(2));
        assert_eq!(l.ids(), vec![1, 3]);
        assert!(l.remove(3));
        assert_eq!(l.ids(), vec![1]);
        // Only the root leaf remains: caller must close the tab.
        assert!(!l.remove(1));
    }

    #[test]
    fn sizes_cover_the_viewport_including_divider() {
        let mut l = Layout::leaf(1);
        l.split(1, 2, SplitDir::Horizontal);
        let sizes = l.sizes(80, 24);
        assert_eq!(sizes.len(), 2);
        let total: usize = sizes.iter().map(|(_, w, _)| *w).sum();
        assert_eq!(total + 1, 80, "one divider cell");
        assert!(sizes.iter().all(|(_, w, h)| *w > 10 && *h == 24));

        let mut v = Layout::leaf(1);
        v.split(1, 2, SplitDir::Vertical);
        let sizes = v.sizes(80, 24);
        let total: usize = sizes.iter().map(|(_, _, h)| *h).sum();
        assert_eq!(total + 1, 24, "one divider row");
        assert!(sizes.iter().all(|(_, w, _)| *w == 80));
    }

    #[test]
    fn nested_sizes_stay_within_bounds() {
        let mut l = Layout::leaf(1);
        l.split(1, 2, SplitDir::Horizontal);
        l.split(2, 3, SplitDir::Vertical);
        let sizes = l.sizes(100, 40);
        assert_eq!(sizes.len(), 3);
        let (_, w1, h1) = sizes[0];
        assert_eq!((w1, h1), (50, 40));
        let (_, w2, h2) = sizes[1];
        let (_, w3, h3) = sizes[2];
        assert_eq!(w2, 49);
        assert_eq!(w3, 49);
        assert_eq!(h2 + h3 + 1, 40);
    }

    #[test]
    fn ratio_path_and_clamping() {
        let mut l = Layout::leaf(1);
        l.split(1, 2, SplitDir::Horizontal);
        assert!(l.set_ratio_at(&[], 0.25));
        match &l {
            Layout::Split { ratio, .. } => assert!((ratio - 0.25).abs() < 1e-6),
            _ => panic!("expected split"),
        }
        assert!(l.set_ratio_at(&[], 5.0));
        match &l {
            Layout::Split { ratio, .. } => assert_eq!(*ratio, MAX_RATIO),
            _ => panic!("expected split"),
        }
        assert!(!l.set_ratio_at(&[true], 0.5));
        assert_eq!(clamp_ratio(f32::NAN), 0.5);
    }

    #[test]
    fn tiny_viewports_still_produce_one_cell_panes() {
        let mut l = Layout::leaf(1);
        l.split(1, 2, SplitDir::Horizontal);
        let sizes = l.sizes(1, 1);
        assert_eq!(sizes.len(), 2);
        assert!(sizes.iter().all(|(_, w, h)| *w >= 1 && *h >= 1));
    }
}
