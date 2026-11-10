//! Undo history as a tree (S-20): a change after `U` starts a branch instead
//! of dropping the redo states. Every state is a node; the current one is
//! the live sheet, the others hold a [`Snapshot`].

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::*;

/// Everything `U` restores.
#[derive(Clone)]
pub struct Snapshot {
    pub plan: LazyFrame,
    pub view: View,
    pub steps: Vec<PlanStep>,
}

/// States kept per sheet; the oldest off-path branch goes first.
const UNDO_MAX: usize = 100;

#[derive(Clone)]
struct Node {
    /// `None` for the current state: the sheet itself holds it.
    snap: Option<Snapshot>,
    parent: Option<u64>,
    /// The child `R` goes to: the one last made or come back from.
    redo: Option<u64>,
    made_at: SystemTime,
}

/// Node ids count up as states are made, so id order is time order.
#[derive(Clone)]
pub struct UndoTree {
    nodes: BTreeMap<u64, Node>,
    current: u64,
}

impl Default for UndoTree {
    fn default() -> Self {
        let root = Node {
            snap: None,
            parent: None,
            redo: None,
            made_at: SystemTime::now(),
        };
        Self {
            nodes: BTreeMap::from([(0, root)]),
            current: 0,
        }
    }
}

/// One row of `:undotree`.
pub struct UndoState {
    pub id: u64,
    pub parent: Option<u64>,
    /// Steps from the first state kept.
    pub depth: usize,
    pub made_at: SystemTime,
    /// What changed from the parent state.
    pub label: String,
    pub current: bool,
}

impl UndoTree {
    fn path(&self, mut id: u64) -> Vec<u64> {
        let mut out = vec![id];
        while let Some(p) = self.nodes[&id].parent {
            out.push(p);
            id = p;
        }
        out
    }

    /// The current state's distance from the oldest kept one: `U`s to get there.
    pub fn depth(&self) -> usize {
        self.path(self.current).len() - 1
    }

    pub fn current(&self) -> u64 {
        self.current
    }

    fn evict(&mut self) {
        while self.nodes.len() > UNDO_MAX {
            let on_path = self.path(self.current);
            let leaf = self.nodes.keys().copied().find(|id| {
                !on_path.contains(id) && !self.nodes.values().any(|n| n.parent == Some(*id))
            });
            let gone = match leaf {
                Some(id) => id,
                // one line of states: the oldest goes, its child is the new root
                None => *on_path.last().expect("path has the root"),
            };
            let node = self.nodes.remove(&gone).expect("listed");
            for n in self.nodes.values_mut() {
                if n.parent == Some(gone) {
                    n.parent = None;
                }
            }
            if let Some(p) = node.parent {
                let last = self
                    .nodes
                    .iter()
                    .filter(|(_, n)| n.parent == Some(p))
                    .map(|(id, _)| *id)
                    .max();
                let parent = self.nodes.get_mut(&p).expect("parent kept");
                if parent.redo == Some(gone) {
                    parent.redo = last;
                }
            }
        }
    }
}

impl Sheet {
    /// Remember the current plan/view for `U` before a change.
    pub fn checkpoint(&mut self) {
        self.push_undo(self.snapshot());
    }

    /// `snap` (taken before a change) is the state `U` goes back to; the
    /// sheet as it is now becomes a new state. A change after `U` branches.
    pub fn push_undo(&mut self, snap: Snapshot) {
        let t = &mut self.state.undo;
        let id = t.nodes.keys().next_back().expect("never empty") + 1;
        let cur = t.nodes.get_mut(&t.current).expect("current kept");
        cur.snap = Some(snap);
        cur.redo = Some(id);
        let node = Node {
            snap: None,
            parent: Some(t.current),
            redo: None,
            made_at: SystemTime::now(),
        };
        t.nodes.insert(id, node);
        t.current = id;
        t.evict();
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            plan: self.plan.clone(),
            view: self.view.clone(),
            steps: self.state.steps.clone(),
        }
    }

    /// Back to `snap`, with no undo step; the rows may differ, so recount.
    pub(crate) fn restore(&mut self, snap: Snapshot) {
        self.plan = snap.plan;
        self.view = snap.view;
        self.state.steps = snap.steps;
        self.invalidate(Reason::Rows);
    }

    /// `U` (`undo = true`): the parent state; `R`: the child last made or
    /// come back from. False when there is none.
    pub fn time_travel(&mut self, undo: bool) -> bool {
        let t = &self.state.undo;
        let node = &t.nodes[&t.current];
        match if undo { node.parent } else { node.redo } {
            Some(id) => self.goto_state(id),
            None => false,
        }
    }

    /// `:earlier n` / `:later n` (`n < 0`): `n` states back or on in the
    /// order they were made, whichever branch they're on. False at the end.
    pub fn time_walk(&mut self, n: i64) -> bool {
        let t = &self.state.undo;
        let ids = t.nodes.keys().copied();
        let target = if n >= 0 {
            let before: Vec<u64> = ids.filter(|id| *id < t.current).collect();
            before
                .len()
                .checked_sub(n.max(1) as usize)
                .map_or(before.first(), |i| before.get(i))
                .copied()
        } else {
            let after: Vec<u64> = ids.filter(|id| *id > t.current).collect();
            after
                .get(n.unsigned_abs() as usize - 1)
                .or(after.last())
                .copied()
        };
        target.is_some_and(|id| self.goto_state(id))
    }

    /// Make state `id` current. `R` then retraces the way from the old one.
    pub fn goto_state(&mut self, id: u64) -> bool {
        let live = self.snapshot();
        let t = &mut self.state.undo;
        if id == t.current || !t.nodes.contains_key(&id) {
            return false;
        }
        t.nodes.get_mut(&t.current).expect("current kept").snap = Some(live);
        let snap = t
            .nodes
            .get_mut(&id)
            .and_then(|n| n.snap.take())
            .expect("other states hold a snapshot");
        let mut child = id;
        while let Some(p) = t.nodes[&child].parent {
            t.nodes.get_mut(&p).expect("on the path").redo = Some(child);
            child = p;
        }
        t.current = id;
        self.restore(snap);
        if matches!(self.origin, Origin::Tail { .. }) {
            // Snapshots predate rows that arrived since; keep the rows, undo the edit.
            if let Err(e) = self.rebuild_plan() {
                debug_assert!(false, "steps applied before: {e}");
            }
        }
        true
    }

    /// Every kept state, oldest first, for `:undotree`.
    pub fn undo_states(&self) -> Vec<UndoState> {
        let t = &self.state.undo;
        let live = self.snapshot();
        let snap = |id: u64| t.nodes[&id].snap.as_ref().unwrap_or(&live);
        t.nodes
            .iter()
            .map(|(&id, n)| UndoState {
                id,
                parent: n.parent,
                depth: t.path(id).len() - 1,
                made_at: n.made_at,
                label: n
                    .parent
                    .map_or("opened".into(), |p| change(snap(p), snap(id))),
                current: id == t.current,
            })
            .collect()
    }
}

impl Sheet {
    /// `:undotree`: one row per state of `of`, oldest first; `change` is
    /// indented by depth, so branches show; `*` marks the current one.
    pub fn undo_tree_of(of: &Sheet, id: SheetId) -> Self {
        let states = of.undo_states();
        let now = SystemTime::now();
        let ago = |t: SystemTime| {
            let s = now.duration_since(t).unwrap_or_default().as_secs();
            match s {
                0..60 => format!("{s}s"),
                60..3600 => format!("{}m", s / 60),
                3600..86400 => format!("{}h", s / 3600),
                _ => format!("{}d", s / 86400),
            }
        };
        let df = DataFrame::new_infer_height(vec![
            Column::new(
                "state".into(),
                states.iter().map(|s| s.id).collect::<Vec<_>>(),
            ),
            Column::new(
                "parent".into(),
                states.iter().map(|s| s.parent).collect::<Vec<_>>(),
            ),
            Column::new(
                "current".into(),
                states
                    .iter()
                    .map(|s| if s.current { "*" } else { "" })
                    .collect::<Vec<_>>(),
            ),
            Column::new(
                "ago".into(),
                states.iter().map(|s| ago(s.made_at)).collect::<Vec<_>>(),
            ),
            Column::new(
                "change".into(),
                states
                    .iter()
                    .map(|s| format!("{}{}", "  ".repeat(s.depth), s.label))
                    .collect::<Vec<_>>(),
            ),
        ])
        .expect("undo-tree dataframe");
        let origin = Origin::UndoTree { parent: of.id };
        let mut sheet = Sheet::meta(id, format!("{}_undo", of.name), df, origin);
        sheet.view.row_cursor = states.iter().position(|s| s.current).unwrap_or(0);
        sheet
    }
}

/// What turned state `a` into `b`, in a few words.
fn change(a: &Snapshot, b: &Snapshot) -> String {
    if a.steps != b.steps {
        let i = a
            .steps
            .iter()
            .zip(&b.steps)
            .take_while(|(x, y)| x == y)
            .count();
        return b
            .steps
            .get(i)
            .map_or("edit undone".into(), PlanStep::summary);
    }
    let (a, b) = (&a.view, &b.view);
    let names = |v: &[String]| v.join(", ");
    if a.sort != b.sort {
        let keys: Vec<String> = b
            .sort
            .iter()
            .map(|k| {
                format!(
                    "{}{}",
                    if k.order == SortOrder::Desc { "-" } else { "" },
                    k.column
                )
            })
            .collect();
        return if keys.is_empty() {
            "clear sort".into()
        } else {
            format!("sort {}", keys.join(", "))
        };
    }
    if a.filter != b.filter || a.filter_expr != b.filter_expr {
        return match (&b.filter_expr, &b.filter) {
            (Some(e), _) => format!("filter {e}"),
            (None, Some(_)) => "filter".into(),
            (None, None) => "clear filter".into(),
        };
    }
    if a.hidden != b.hidden {
        let shown: Vec<String> = b.hidden.difference(&a.hidden).cloned().collect();
        return if shown.is_empty() {
            "unhide".into()
        } else {
            format!("hide {}", names(&shown))
        };
    }
    if a.keys != b.keys {
        return format!("keys {}", names(&b.keys));
    }
    if a.type_casts != b.type_casts {
        let new = b.type_casts.iter().find(|c| !a.type_casts.contains(c));
        return new.map_or("type reset".into(), |(c, t)| format!("type {c} {t}"));
    }
    if a.selected != b.selected {
        return format!("{} selected", b.selected.len());
    }
    if a.frozen_left != b.frozen_left {
        return format!("freeze {}", b.frozen_left);
    }
    "no change".into()
}
