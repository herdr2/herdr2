//! The app's shared world, the same for every layout: spaces, each with
//! tabs (named pieces of work), each tab holding its terminals and
//! how they're split. Plus which pane has focus and agent attention (done vs
//! seen) with Herdr's rollup. Pure data; the window drives it.

use crate::layout::{Axis, Content, Node};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

/// A named piece of work in a space: its terminals and how they're split.
#[derive(Clone, Debug)]
pub struct Tab {
    pub id: String,
    pub name: String,
    /// The arrangement: a row/column tree of terminal blocks. Its id is the tab's.
    pub root: Node,
    pub focused: Option<String>,
    /// Panes focused before, most recent last: closing the focused pane goes back.
    pub before: Vec<String>,
    pub zoomed: Option<String>,
    /// Put away (ms since the epoch): out of the lists, still running. Comes
    /// back when it's shown or one of its agents needs the person.
    pub settled: Option<u64>,
    /// Fields of its document entry this build doesn't read (a newer build's),
    /// written back as they were.
    pub rest: serde_json::Map<String, serde_json::Value>,
}

/// A place to work, plus the folder its new terminals start in.
#[derive(Clone, Debug)]
pub struct Space {
    pub id: String,
    /// The name someone gave it; empty: named after where its first
    /// terminal is (Herdr's way, `Model::space_name`).
    pub name: String,
    pub folder: String,
    /// The machine its terminals run on (`hc machine ls`); None: this one.
    pub machine: Option<String>,
    pub tabs: Vec<Tab>,
    /// The selected tab.
    pub active: Option<String>,
    /// Fields of its document entry this build doesn't read (a newer build's),
    /// written back as they were.
    pub rest: serde_json::Map<String, serde_json::Value>,
}

/// A closed space, so opening it again doesn't ask for its name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub name: String,
    pub folder: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Alert {
    /// Needs the person (blocked or error).
    Blocked,
    /// Finished while the person wasn't looking.
    Done,
}

/// What core and the user's attention say about one terminal.
#[derive(Clone, Debug, Default)]
pub struct Agent {
    /// The program's 7501 word (working, idle, done, blocked, error), if it reports.
    pub word: Option<String>,
    /// User-assigned status override (idle, working, blocked, done), synced via `herdr.status`.
    pub manual_status: Option<String>,
    /// The agent's name from its 7501 report (`app`), e.g. "claude-code".
    pub app: Option<String>,
    /// Whether the person has seen it since it last stopped working.
    pub seen: bool,
    /// Core's turn counter: goes up each time it stops working.
    pub turn: u64,
    /// The last turn a person looked at on any client (`herdr.seen`, docs/labels.md).
    /// None: no client shares it yet, so `seen` decides.
    pub seen_turn: Option<u64>,
    /// Bumped on every state change (for "most recent" ordering).
    pub seq: u64,
    /// When the state last changed, ms since the epoch (0: not since the app started).
    pub since: u64,
    /// Epoch ms when the current active working run began (adjusted for prior paused blocked segments).
    pub working_since: Option<u64>,
    /// Active execution duration (ms) of the current or most recently completed run.
    pub work_ms: Option<u64>,
    /// Cumulative active execution duration (ms) across all runs of this agent.
    pub total_work_ms: u64,
}

impl Agent {
    fn transition_timing(&mut self, was_eff: Option<&str>, now_eff: Option<&str>, now: u64) {
        let was_w = was_eff == Some("working");
        let is_w = now_eff == Some("working");
        if was_w && !is_w {
            if self.working_since.is_some() || self.work_ms.is_some() {
                let prev = self.work_ms.unwrap_or(0);
                let run = self.working_since.map_or(prev, |ws| now.saturating_sub(ws).max(prev));
                let delta = run.saturating_sub(prev);
                self.work_ms = Some(run);
                self.total_work_ms = self.total_work_ms.saturating_add(delta);
            }
            self.working_since = None;
        } else if !was_w && is_w {
            let resuming = matches!(was_eff, Some("blocked" | "error")) && self.work_ms.is_some();
            let prior = if resuming { self.work_ms.unwrap_or(0) } else { 0 };
            self.work_ms = Some(prior);
            self.working_since = Some(now.saturating_sub(prior));
        }
    }
}

#[derive(Default)]
pub struct Model {
    pub spaces: Vec<Space>,
    /// The selected space.
    pub active: Option<String>,
    pub recent: Vec<Recent>,
    /// Each pane's live folder, for its display name.
    pub pane_folders: BTreeMap<String, String>,
    pub agents: HashMap<String, Agent>,
    seq: u64,
    /// Where each terminal works now: its repo (worktrees included) and branch.
    pub folder_info: HashMap<String, FolderInfo>,
}

/// Herdr's attention priority: blocked > done > working > idle > nothing.
pub fn priority(status: Option<&str>) -> u8 {
    match status {
        Some("blocked") => 4,
        Some("done") => 3,
        Some("working") => 2,
        Some("idle") => 1,
        _ => 0,
    }
}

/// Flattens nested same-axis containers and clears explicit child sizes so siblings share space equally.
fn equalize_split_tree(node: &mut Node) {
    let axis = node.axis();
    if let Some(children) = node.children_mut() {
        for child in children.iter_mut() {
            equalize_split_tree(child);
        }
        if let Some(ax) = axis {
            let mut flat = Vec::with_capacity(children.len());
            for mut child in std::mem::take(children) {
                if child.axis() == Some(ax)
                    && let Some(grand) = child.children_mut()
                {
                    flat.append(grand);
                    continue;
                }
                child.size = None;
                flat.push(child);
            }
            for child in &mut flat {
                child.size = None;
            }
            *children = flat;
        }
    }
}

impl Tab {
    /// A tab with one terminal pane.
    pub fn new(id: &str, name: &str, pane_id: &str, address: &str) -> Tab {
        Tab::with_pane(id, name, pane(pane_id, address))
    }

    /// A tab with one pane (a terminal or a mod).
    pub fn with_pane(id: &str, name: &str, pane: Node) -> Tab {
        Tab {
            id: id.to_owned(),
            name: name.to_owned(),
            focused: Some(pane.id.clone()),
            before: Vec::new(),
            root: Node::container(id, Axis::Row, vec![pane]),
            zoomed: None,
            settled: None,
            rest: Default::default(),
        }
    }

    /// Focuses a pane and remembers the one it leaves. A zoomed tab shows the
    /// pane it focuses, never a hidden one.
    pub fn focus(&mut self, pane: &str) {
        if self.zoomed.is_some() {
            self.zoomed = Some(pane.to_owned());
        }
        if let Some(old) = self.focused.take().filter(|o| o != pane) {
            self.before.retain(|p| *p != old && p != pane);
            self.before.push(old);
            if self.before.len() > 32 {
                self.before.remove(0);
            }
        }
        self.focused = Some(pane.to_owned());
    }

    /// The focused pane, else the first one.
    pub fn focused_pane(&self) -> Option<String> {
        self.focused.clone().filter(|p| self.root.find(p).is_some()).or_else(|| self.root.panes().first().map(|n| n.id.clone()))
    }

    /// What's drawn: only the zoomed pane when one is zoomed.
    pub fn shown(&self) -> &Node {
        self.zoomed.as_deref().and_then(|z| self.root.find(z)).unwrap_or(&self.root)
    }
}

impl Space {
    pub fn tab(&self, id: &str) -> Option<&Tab> {
        self.tabs.iter().find(|c| c.id == id)
    }

    /// The selected tab, else the first.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.active.as_deref().and_then(|a| self.tab(a)).or(self.tabs.first())
    }

    fn select(&mut self, ctx: &str) {
        if self.tab(ctx).is_some() {
            self.active = Some(ctx.to_owned());
        }
    }
}

impl Model {
    pub fn fresh_id(&mut self, prefix: &str) -> String {
        use std::hash::{BuildHasher, Hasher};
        let random = || std::collections::hash_map::RandomState::new().build_hasher().finish();
        format!("{prefix}{:016x}{:016x}", random(), random())
    }

    pub fn fresh_id_on(&mut self, prefix: &str, machine: Option<&str>) -> String {
        crate::mirror::qualify(&self.fresh_id(prefix), machine)
    }

    /// Machine blocks have one order on every screen; order inside each stays shared.
    pub fn order_spaces(&mut self) {
        self.spaces.sort_by(|a, b| a.machine.cmp(&b.machine));
    }

    pub fn move_space(&mut self, id: &str, index: usize) -> bool {
        self.order_spaces();
        let Some(from) = self.spaces.iter().position(|s| s.id == id) else { return false };
        let machine = self.spaces[from].machine.clone();
        let first = self.spaces.iter().position(|s| s.machine == machine).unwrap();
        let last = self.spaces.iter().rposition(|s| s.machine == machine).unwrap();
        let to = index.saturating_sub(1).clamp(first, last);
        let space = self.spaces.remove(from);
        self.spaces.insert(to, space);
        true
    }

    /// Moves a tab within its space (1 = first). Only a tab id: not `space_of`, which also
    /// finds the space of a pane or container, and those have no place in `tabs`.
    pub fn move_tab(&mut self, id: &str, index: usize) -> bool {
        let Some((p, from)) = self.spaces.iter_mut().find_map(|p| p.tabs.iter().position(|c| c.id == id).map(|from| (p, from))) else { return false };
        let tab = p.tabs.remove(from);
        let to = index.saturating_sub(1).min(p.tabs.len());
        p.tabs.insert(to, tab);
        true
    }

    /// Moves a tab `by` places, skipping settled tabs as `tab.next` does, wrapping at
    /// either end.
    pub fn shift_tab(&mut self, id: &str, by: i64) -> bool {
        let Some(p) = self.spaces.iter().find(|p| p.tabs.iter().any(|c| c.id == id)) else { return false };
        let shown: Vec<_> = p.tabs.iter().enumerate().filter(|(_, c)| c.settled.is_none() || c.id == id).map(|(i, _)| i).collect();
        let from = shown.iter().position(|&i| p.tabs[i].id == id).unwrap();
        let n = shown.len() as i64;
        let to = shown[(from as i64 + by.rem_euclid(n)).rem_euclid(n) as usize];
        self.move_tab(id, to + 1)
    }

    pub fn active_index(&self) -> Option<usize> {
        let id = self.active.as_deref()?;
        self.spaces.iter().position(|p| p.id == id).or((!self.spaces.is_empty()).then_some(0))
    }

    pub fn active_space(&self) -> Option<&Space> {
        self.active_index().map(|i| &self.spaces[i])
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.active_space()?.active_tab()
    }

    pub fn space(&self, id: &str) -> Option<&Space> {
        self.spaces.iter().find(|p| p.id == id)
    }

    pub fn space_mut(&mut self, id: &str) -> Option<&mut Space> {
        self.spaces.iter_mut().find(|p| p.id == id)
    }

    pub fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.spaces.iter().flat_map(|p| p.tabs.iter())
    }

    pub fn tab(&self, id: &str) -> Option<&Tab> {
        self.tabs().find(|c| c.id == id)
    }

    pub fn tab_mut(&mut self, id: &str) -> Option<&mut Tab> {
        self.spaces.iter_mut().flat_map(|p| p.tabs.iter_mut()).find(|c| c.id == id)
    }

    /// The space holding a tab or a node inside one.
    pub fn space_of(&self, node: &str) -> Option<&Space> {
        self.spaces.iter().find(|p| p.tabs.iter().any(|c| c.id == node || c.root.find(node).is_some()))
    }

    /// The tab holding a node (pane, container).
    pub fn tab_of(&self, node: &str) -> Option<&Tab> {
        self.tabs().find(|c| c.root.find(node).is_some())
    }

    fn tab_of_mut(&mut self, node: &str) -> Option<&mut Tab> {
        self.spaces.iter_mut().flat_map(|p| p.tabs.iter_mut()).find(|c| c.root.find(node).is_some())
    }

    /// Adds a space at the end and selects it.
    /// What a space is called: its name, else its active tab's live folder
    /// (falling back to the space's first terminal or initial folder), as Herdr names workspaces.
    pub fn space_name(&self, p: &Space, home: &str) -> String {
        if !p.name.is_empty() {
            return p.name.clone();
        }
        let folder_of = |n: &Node| {
            n.terminal()
                .and_then(|a| self.folder_info.get(a))
                .map(|f| f.top.clone())
                .or_else(|| self.pane_folders.get(&n.id).cloned())
        };
        let active_folder = p.active_tab().and_then(|c| {
            let focused = c.focused_pane().and_then(|id| c.root.find(&id)).filter(|n| n.terminal().is_some());
            focused
                .and_then(folder_of)
                .or_else(|| c.root.terminals().into_iter().find_map(folder_of))
        });
        let folder = active_folder
            .or_else(|| p.tabs.iter().flat_map(|c| c.root.terminals()).find_map(folder_of))
            .unwrap_or_else(|| p.folder.clone());
        folder_name(&folder, home)
    }

    /// What a tab is called: its name, else its place in its space (1, 2, ...).
    pub fn tab_name(p: &Space, c: &Tab) -> String {
        if !c.name.is_empty() {
            return c.name.clone();
        }
        (p.tabs.iter().position(|x| x.id == c.id).unwrap_or(0) + 1).to_string()
    }

    pub fn add_space(&mut self, id: &str, name: &str, folder: &str, ctx: Tab) {
        self.insert_space(id, name, folder, ctx, true);
    }

    /// Adds a space without showing it; with no space shown yet, it is shown.
    pub fn add_space_unshown(&mut self, id: &str, name: &str, folder: &str, ctx: Tab) {
        self.insert_space(id, name, folder, ctx, false);
    }

    fn insert_space(&mut self, id: &str, name: &str, folder: &str, ctx: Tab, show: bool) {
        let show = show || self.active_index().is_none();
        let active = Some(ctx.id.clone());
        self.spaces.push(Space { id: id.to_owned(), name: name.to_owned(), folder: folder.to_owned(), machine: None, tabs: vec![ctx], active, rest: Default::default() });
        self.recent.retain(|r| r.name != name);
        if show {
            self.active = Some(id.to_owned());
        }
    }

    /// Adds a tab at the end of the space and selects it (not the space).
    pub fn add_tab(&mut self, space: &str, ctx: Tab) -> bool {
        self.insert_tab(space, ctx, true)
    }

    /// Adds a tab at the end of the space, so several keep their order; its selected tab stays.
    pub fn add_tab_unselected(&mut self, space: &str, ctx: Tab) -> bool {
        self.insert_tab(space, ctx, false)
    }

    fn insert_tab(&mut self, space: &str, ctx: Tab, select: bool) -> bool {
        let Some(p) = self.space_mut(space) else { return false };
        if select {
            p.active = Some(ctx.id.clone());
        }
        p.tabs.push(ctx);
        true
    }

    /// Splits `target` (a pane) with a new terminal pane and focuses it.
    pub fn split(&mut self, target: &str, axis: Axis, new_pane: &str, address: &str) -> bool {
        self.split_with(target, axis, pane(new_pane, address))
    }

    /// Splits a pane with any block: a terminal, or a mod (a viewer beside the shell).
    pub fn split_with(&mut self, target: &str, axis: Axis, block: Node) -> bool {
        let machine = self.space_of(target).and_then(|s| s.machine.clone());
        let container = self.fresh_id_on("s", machine.as_deref());
        let new_pane = block.id.clone();
        let Some(c) = self.tab_of_mut(target) else { return false };
        if !c.root.split(target, axis, block, container) {
            return false;
        }
        equalize_split_tree(&mut c.root);
        c.zoomed = None;
        c.focus(&new_pane);
        true
    }

    /// Rearranges existing panes in one tab without changing their terminal addresses.
    /// No edge swaps their places; an edge places the source beside the target.
    pub fn move_pane(&mut self, source: &str, target: &str, edge: Option<crate::layout::Edge>) -> bool {
        if source == target {
            return false;
        }
        let Some(tab) = self.tab_of(source) else { return false };
        if !tab.root.find(source).is_some_and(Node::is_pane) || !tab.root.find(target).is_some_and(Node::is_pane) {
            return false;
        }
        let mut root = tab.root.clone();
        if let Some(edge) = edge {
            use crate::layout::Edge;
            let mut pane = root.remove(source).unwrap();
            pane.size = None;
            let axis = if matches!(edge, Edge::Left | Edge::Right) { Axis::Row } else { Axis::Column };
            let machine = self.space_of(source).and_then(|s| s.machine.clone());
            let container = self.fresh_id_on("s", machine.as_deref());
            if !root.split(target, axis, pane, container) {
                return false;
            }
            if matches!(edge, Edge::Left | Edge::Top) {
                root.swap(source, target);
            }
        } else if !root.swap(source, target) {
            return false;
        }
        let tab = self.tab_of_mut(source).unwrap();
        tab.root = root;
        tab.focus(source);
        true
    }

    pub fn swap_panes(&mut self, pane: &str, target: &str) -> Result<(), String> {
        let is_pane = |m: &Model, id: &str| m.tab_of(id).and_then(|c| c.root.find(id)).is_some_and(Node::is_pane);
        if !is_pane(self, pane) {
            return Err(format!("no pane {pane:?}"));
        }
        if !is_pane(self, target) {
            return Err(format!("no target pane {target:?}"));
        }
        if pane == target {
            return Err("pane.swap target is the pane itself".into());
        }
        let Some(c) = self.tab_of_mut(pane) else { return Err(format!("no pane {pane:?}")) };
        if c.root.find(target).is_none() {
            return Err(format!("pane.swap target {target:?} is in another tab"));
        }
        c.root.swap(pane, target);
        Ok(())
    }

    /// Merges all panes of `source` beside `target`'s panes along `edge` and selects `target`.
    pub fn merge_tabs(&mut self, source: &str, target: &str, edge: crate::layout::Edge) -> Result<Vec<String>, String> {
        if source == target {
            return Err("tab.merge source and target are the same tab".into());
        }
        let Some(src_space) = self.spaces.iter().position(|p| p.tab(source).is_some()) else {
            return Err(format!("no tab {source:?}"));
        };
        let Some(dst_space) = self.spaces.iter().position(|p| p.tab(target).is_some()) else {
            return Err(format!("no tab {target:?}"));
        };
        if src_space != dst_space {
            return Err("tab.merge tabs must be in the same space".into());
        }
        let machine = self.spaces[dst_space].machine.clone();
        let src_index = self.spaces[dst_space].tabs.iter().position(|c| c.id == source).unwrap();
        let src_tab = self.spaces[dst_space].tabs.remove(src_index);
        let moved_addrs = any_addresses(&src_tab.root);
        let focus_pane = src_tab.focused.clone().or_else(|| src_tab.root.panes().first().map(|n| n.id.clone()));
        let axis = if edge.vertical() { Axis::Row } else { Axis::Column };

        fn take_nodes(mut root: Node, axis: Axis, split_id: Option<String>) -> Vec<Node> {
            if root.axis() == Some(axis) {
                if let Some(children) = root.children_mut() {
                    return std::mem::take(children);
                }
            } else if matches!(root.kind, crate::layout::Kind::Row { .. } | crate::layout::Kind::Column { .. })
                && root.children().len() == 1
            {
                return vec![root.children_mut().unwrap().remove(0)];
            }
            if let Some(id) = split_id {
                root.id = id;
            }
            vec![root]
        }

        let needs_split_id = |root: &Node| {
            root.axis() != Some(axis)
                && !(matches!(root.kind, crate::layout::Kind::Row { .. } | crate::layout::Kind::Column { .. })
                    && root.children().len() == 1)
        };
        let dst_split_id = {
            let dst_root = &self.spaces[dst_space].tabs.iter().find(|c| c.id == target).unwrap().root;
            needs_split_id(dst_root).then(|| self.fresh_id_on("s", machine.as_deref()))
        };
        let src_split_id = needs_split_id(&src_tab.root).then(|| self.fresh_id_on("s", machine.as_deref()));

        let dst_tab = self.spaces[dst_space].tabs.iter_mut().find(|c| c.id == target).unwrap();
        let dst_id = dst_tab.id.clone();
        let dst_root = std::mem::replace(&mut dst_tab.root, Node::container(&dst_id, axis, vec![]));
        let mut dst_children = take_nodes(dst_root, axis, dst_split_id);
        let mut src_children = take_nodes(src_tab.root, axis, src_split_id);
        let mut children = if matches!(edge, crate::layout::Edge::Left | crate::layout::Edge::Top) {
            src_children.append(&mut dst_children);
            src_children
        } else {
            dst_children.append(&mut src_children);
            dst_children
        };
        for child in &mut children {
            if matches!(child.size, Some(crate::layout::Size::Weight { .. })) {
                child.size = None;
            }
        }
        dst_tab.root = Node::container(dst_id, axis, children);
        equalize_split_tree(&mut dst_tab.root);
        dst_tab.zoomed = None;
        if let Some(fp) = focus_pane {
            dst_tab.focus(&fp);
        }
        self.select_tab(target);
        Ok(moved_addrs)
    }

    /// Moves `pane_id` out of a multi-pane tab into a new standalone tab right after it and selects it.
    /// Returns `(new_tab_id, moved_terminal_or_acp_address)`.
    pub fn detach_pane(&mut self, pane_id: &str, new_tab_name: &str) -> Result<(String, Option<String>), String> {
        let Some((space_idx, tab_idx)) = self.spaces.iter().enumerate().find_map(|(si, sp)| {
            sp.tabs
                .iter()
                .position(|c| c.root.find(pane_id).is_some_and(Node::is_pane))
                .map(|ti| (si, ti))
        }) else {
            return Err(format!("no pane {pane_id:?}"));
        };
        if self.spaces[space_idx].tabs[tab_idx].root.panes().len() < 2 {
            return Err("pane is already in its own tab".into());
        }
        let machine = self.spaces[space_idx].machine.clone();
        let new_tab_id = self.fresh_id_on("tb", machine.as_deref());
        let extracted = {
            let src_tab = &mut self.spaces[space_idx].tabs[tab_idx];
            let mut extracted = src_tab.root.remove(pane_id).ok_or_else(|| format!("no pane {pane_id:?}"))?;
            extracted.size = None;
            equalize_split_tree(&mut src_tab.root);
            src_tab.zoomed = None;
            src_tab.before.retain(|p| p != pane_id);
            let here: Vec<String> = src_tab.root.panes().iter().map(|n| n.id.clone()).collect();
            src_tab.before.retain(|p| here.contains(p));
            if src_tab.focused.as_deref() == Some(pane_id) || !src_tab.focused.as_ref().is_some_and(|f| here.contains(f)) {
                src_tab.focused = src_tab.before.pop().or_else(|| here.first().cloned());
            }
            extracted
        };
        let moved_addr = extracted.any_terminal().map(str::to_owned);
        let new_tab = Tab::with_pane(&new_tab_id, new_tab_name, extracted);
        self.spaces[space_idx].tabs.push(new_tab);
        self.select_tab(&new_tab_id);
        Ok((new_tab_id, moved_addr))
    }

    /// Removes a pane. Returns what else closed: the tab, and the space if it was its last.
    pub fn remove_pane(&mut self, pane_id: &str) -> (bool, bool) {
        let Some(c) = self.tab_of_mut(pane_id) else { return (false, false) };
        c.root.remove(pane_id);
        if c.zoomed.as_deref() == Some(pane_id) {
            c.zoomed = None;
        }
        if !c.root.panes().is_empty() {
            equalize_split_tree(&mut c.root);
            c.before.retain(|p| p != pane_id);
            if c.focused.as_deref() == Some(pane_id) {
                // Back to the pane focused before it that's still here, else the first.
                let here: Vec<String> = c.root.panes().iter().map(|n| n.id.clone()).collect();
                c.before.retain(|p| here.contains(p));
                c.focused = c.before.pop().or_else(|| here.first().cloned());
            }
            return (false, false);
        }
        let id = c.id.clone();
        let (_, space_closed) = self.remove_tab(&id);
        (true, space_closed)
    }

    /// Removes a tab. Returns its terminal addresses and whether its space closed.
    pub fn remove_tab(&mut self, ctx: &str) -> (Vec<String>, bool) {
        let Some(pi) = self.spaces.iter().position(|p| p.tab(ctx).is_some()) else { return (vec![], false) };
        let p = &mut self.spaces[pi];
        let ci = p.tabs.iter().position(|c| c.id == ctx).unwrap();
        let removed = p.tabs.remove(ci);
        if p.active.as_deref() == Some(ctx) {
            // The tab that slides into the slot, or the last one.
            p.active = p.tabs.get(ci).or(p.tabs.last()).map(|c| c.id.clone());
        }
        let closed = p.tabs.is_empty();
        if closed {
            let id = p.id.clone();
            self.remove_space(&id);
        }
        (any_addresses(&removed.root), closed)
    }

    /// Removes a space (remembered in `recent`). Returns its terminal addresses.
    pub fn remove_space(&mut self, id: &str) -> Vec<String> {
        let Some(pi) = self.spaces.iter().position(|p| p.id == id) else { return vec![] };
        let was_active = self.active_index() == Some(pi);
        let p = self.spaces.remove(pi);
        self.recent.retain(|r| r.name != p.name);
        self.recent.insert(0, Recent { name: p.name.clone(), folder: p.folder.clone() });
        self.recent.truncate(20);
        if was_active {
            self.active = self.spaces.get(pi).or(self.spaces.last()).map(|p| p.id.clone());
        }
        p.tabs.iter().flat_map(|c| any_addresses(&c.root)).collect()
    }

    /// The focused pane of the selected space's selected tab.
    pub fn focused_pane(&self) -> Option<String> {
        self.active_tab()?.focused_pane()
    }

    /// Selects a tab and its space. Shown again, a settled tab is back.
    pub fn select_tab(&mut self, ctx: &str) -> bool {
        let Some(p) = self.spaces.iter_mut().find(|p| p.tab(ctx).is_some()) else { return false };
        p.select(ctx);
        self.active = Some(p.id.clone());
        self.unsettle(ctx);
        true
    }

    /// Puts a tab away. The shown one hands over to the next unsettled
    /// tab of its space, if there is one.
    pub fn settle(&mut self, ctx: &str, now: u64) -> bool {
        let Some(c) = self.tab_mut(ctx) else { return false };
        c.settled = Some(now);
        let Some(p) = self.spaces.iter_mut().find(|p| p.tab(ctx).is_some()) else { return true };
        if p.active_tab().is_some_and(|c| c.id == ctx) {
            let i = p.tabs.iter().position(|c| c.id == ctx).unwrap_or(0);
            let n = p.tabs.len();
            if let Some(next) = (1..n).map(|k| &p.tabs[(i + k) % n]).find(|c| c.settled.is_none()) {
                p.active = Some(next.id.clone());
            }
        }
        true
    }

    pub fn unsettle(&mut self, ctx: &str) -> bool {
        self.tab_mut(ctx).is_some_and(|c| c.settled.take().is_some())
    }

    /// Zooms a pane, or unzooms it when it's the zoomed one. A tab with one
    /// pane doesn't zoom. Zooming focuses the pane: a zoomed tab never hides
    /// the pane that has the keyboard. Returns whether the pane is now zoomed.
    pub fn toggle_zoom(&mut self, pane_id: &str) -> bool {
        let Some(c) = self.tab_of_mut(pane_id) else { return false };
        if c.root.panes().len() < 2 {
            return false;
        }
        if c.zoomed.as_deref() == Some(pane_id) {
            c.zoomed = None;
            return false;
        }
        c.zoomed = Some(pane_id.to_owned());
        c.focus(pane_id);
        true
    }

    /// Focuses a pane anywhere: selects its space and tab.
    pub fn focus_pane(&mut self, pane_id: &str) -> bool {
        let Some(c) = self.tab_of_mut(pane_id) else { return false };
        c.focus(pane_id);
        let ctx = c.id.clone();
        self.select_tab(&ctx)
    }

    /// The address shown in a pane block.
    pub fn address_of(&self, pane_id: &str) -> Option<String> {
        self.tabs().find_map(|c| c.root.find(pane_id)).and_then(|n| n.terminal()).map(str::to_owned)
    }

    pub fn pane_of(&self, address: &str) -> Option<String> {
        self.tabs().flat_map(|c| c.root.terminals()).find(|n| n.terminal() == Some(address)).map(|n| n.id.clone())
    }

    /// The PTY or mounted ACP address shown in a pane block.
    pub fn any_address_of(&self, pane_id: &str) -> Option<String> {
        self.tabs().find_map(|c| c.root.find(pane_id)).and_then(Node::any_terminal).map(str::to_owned)
    }

    pub fn acp_pane_of(&self, address: &str) -> Option<String> {
        self.tabs().flat_map(|c| c.root.panes()).find(|n| n.acp_terminal() == Some(address)).map(|n| n.id.clone())
    }

    pub fn any_pane_of(&self, address: &str) -> Option<String> {
        self.pane_of(address).or_else(|| self.acp_pane_of(address))
    }

    /// Every terminal in every tab, in space → tab → pane order.
    pub fn all_addresses(&self) -> Vec<String> {
        self.tabs().flat_map(|c| addresses(&c.root)).collect()
    }

    /// Applies a status word from core (Herdr's seen rules).
    pub fn set_word(&mut self, address: &str, word: Option<String>, app: Option<String>, looking: bool) -> bool {
        let a = self.agents.entry(address.to_owned()).or_insert_with(|| Agent { seen: true, ..Default::default() });
        a.app = app.or(a.app.take());
        if a.word == word {
            return false;
        }
        let was_eff = a.manual_status.as_deref().or(a.word.as_deref()).map(str::to_owned);
        let was_busy = matches!(a.word.as_deref(), Some("working" | "blocked"));
        match word.as_deref() {
            Some("working" | "blocked") => a.seen = true,
            // A finish you didn't watch is "done" until you look at it.
            Some(_) if was_busy => a.seen = looking,
            _ => {}
        }
        a.word = word;
        let now_eff = a.manual_status.as_deref().or(a.word.as_deref()).map(str::to_owned);
        self.seq += 1;
        a.seq = self.seq;
        let now = now_ms();
        a.since = now;
        a.transition_timing(was_eff.as_deref(), now_eff.as_deref(), now);
        true
    }

    /// Overrides the inferred status with an explicit manual state (`None` returns to auto detection).
    pub fn set_manual_status(&mut self, address: &str, manual: Option<String>, app: Option<String>) -> bool {
        let a = self.agents.entry(address.to_owned()).or_insert_with(|| Agent { seen: true, ..Default::default() });
        a.app = app.or(a.app.take());
        if a.manual_status == manual {
            return false;
        }
        let was_eff = a.manual_status.as_deref().or(a.word.as_deref()).map(str::to_owned);
        a.manual_status = manual;
        let now_eff = a.manual_status.as_deref().or(a.word.as_deref()).map(str::to_owned);
        self.seq += 1;
        a.seq = self.seq;
        let now = now_ms();
        a.since = now;
        a.transition_timing(was_eff.as_deref(), now_eff.as_deref(), now);
        true
    }

    /// Clears all agent state (status, app, execution timing, turn) when a terminal exits its CLI agent back to shell.
    pub fn clear_agent(&mut self, address: &str) -> bool {
        self.agents.remove(address).is_some()
    }

    /// Syncs core-provided execution timing (`working_since`, `work_ms`, `total_work_ms`, `status_ms`).
    pub fn sync_core_timing(
        &mut self,
        address: &str,
        core_ws: Option<u64>,
        core_work_ms: Option<u64>,
        core_total_ms: Option<u64>,
        core_status_ms: Option<u64>,
        is_starting_handshake: bool,
    ) -> bool {
        let Some(a) = self.agents.get_mut(address) else { return false };
        if a.manual_status.is_some() {
            return false;
        }
        let prev = (a.working_since, a.work_ms, a.total_work_ms);
        if is_starting_handshake {
            if a.total_work_ms == 0 && core_work_ms.is_none() {
                a.working_since = None;
                a.work_ms = None;
            }
            return (a.working_since, a.work_ms, a.total_work_ms) != prev;
        }
        if core_ws.is_some() || core_work_ms.is_some() || core_total_ms.is_some_and(|t| t > 0) {
            if a.word.as_deref() == Some("working") {
                if core_ws.is_some() {
                    a.working_since = core_ws;
                }
            } else {
                a.working_since = None;
            }
            if core_work_ms.is_some() {
                a.work_ms = core_work_ms;
            }
            a.total_work_ms = a.total_work_ms.max(core_total_ms.unwrap_or(0));
        } else if a.word.is_none() {
            a.working_since = None;
            a.work_ms = None;
            a.total_work_ms = 0;
        } else if a.word.as_deref() == Some("working")
            && let Some(status_ms) = core_status_ms.filter(|ms| *ms > 0)
            && a.work_ms.unwrap_or(0) == 0
        {
            a.working_since = Some(status_ms);
        }
        (a.working_since, a.work_ms, a.total_work_ms) != prev
    }

    /// Herdr's attention rule for a change of 7501 word: needing you always
    /// alerts; finishing alerts only if the person wasn't looking.
    pub fn alert(prev: Option<&str>, new: Option<&str>, looking: bool) -> Option<Alert> {
        let blocked = |w: Option<&str>| matches!(w, Some("blocked" | "error"));
        let busy = |w: Option<&str>| matches!(w, Some("working" | "blocked" | "error"));
        if blocked(new) && !blocked(prev) {
            Some(Alert::Blocked)
        } else if busy(prev) && new.is_some() && !busy(new) && !looking {
            Some(Alert::Done)
        } else {
            None
        }
    }

    /// `alert` for one terminal: only ours ring. A sub-agent reports to its
    /// parent; a terminal in none of the spaces (another machine's, another
    /// app's) isn't the person's business here.
    pub fn alert_for(&self, addr: &str, sub_agent: bool, prev: Option<&str>, new: Option<&str>, looking: bool) -> Option<Alert> {
        if sub_agent || self.any_pane_of(addr).is_none() || self.agents.get(addr).and_then(|a| a.manual_status.as_ref()).is_some() {
            return None;
        }
        Self::alert(prev, new, looking)
    }

    /// Marks what the person is looking at as seen.
    pub fn mark_seen(&mut self, addresses: &[String]) -> bool {
        let mut changed = false;
        for addr in addresses {
            if let Some(a) = self.agents.get_mut(addr)
                && !a.seen
            {
                a.seen = true;
                changed = true;
            }
        }
        changed
    }

    /// Marks a finished non-busy agent as unseen so it surfaces as `"done"` until viewed.
    pub fn mark_unseen(&mut self, address: &str) -> bool {
        if let Some(a) = self.agents.get_mut(address)
            && a.seen
            && !matches!(a.word.as_deref(), Some("working" | "blocked" | "error") | None)
        {
            a.seen = false;
            return true;
        }
        false
    }

    /// Core's turn and the shared seen turn for an agent; true when either changed.
    pub fn set_turn(&mut self, address: &str, turn: u64, seen_turn: Option<u64>) -> bool {
        let Some(a) = self.agents.get_mut(address) else { return false };
        let changed = (a.turn, a.seen_turn) != (turn, seen_turn);
        (a.turn, a.seen_turn) = (turn, seen_turn);
        changed
    }

    /// What the UI shows: working, blocked, done (finished, unseen), idle, or None (not an agent).
    pub fn status(&self, address: &str) -> Option<&'static str> {
        let a = self.agents.get(address)?;
        if let Some(manual) = a.manual_status.as_deref() {
            return match manual {
                "working" => Some("working"),
                "blocked" | "error" => Some("blocked"),
                "done" => Some("done"),
                "idle" => Some("idle"),
                _ => None,
            };
        }
        Some(match a.word.as_deref()? {
            "working" => "working",
            "blocked" | "error" => "blocked",
            _ if a.seen_turn.filter(|_| a.turn > 0).map_or(!a.seen, |s| a.turn > s) => "done",
            _ => "idle",
        })
    }

    /// When something in a tab last changed state, ms (None: nothing since the app started).
    pub fn changed_at(&self, c: &Tab) -> Option<u64> {
        any_addresses(&c.root).iter().filter_map(|a| self.agents.get(a)).map(|a| a.since).filter(|t| *t > 0).max()
    }

    /// Since when an agent in a tab has been working, ms (the earliest, if several).
    pub fn working_since(&self, c: &Tab) -> Option<u64> {
        any_addresses(&c.root)
            .iter()
            .filter_map(|a| self.agents.get(a))
            .filter(|a| a.manual_status.as_deref().or(a.word.as_deref()) == Some("working"))
            .filter_map(|a| a.working_since.or((a.since > 0).then_some(a.since)))
            .min()
    }

    /// Active or most recent completed run duration among agents in a tab, ms.
    pub fn work_ms(&self, c: &Tab) -> Option<u64> {
        any_addresses(&c.root)
            .iter()
            .filter_map(|a| self.agents.get(a))
            .filter_map(|a| a.work_ms)
            .max()
    }

    /// Cumulative active execution duration across agents in a tab, ms.
    pub fn total_work_ms(&self, c: &Tab) -> u64 {
        any_addresses(&c.root)
            .iter()
            .filter_map(|a| self.agents.get(a))
            .map(|a| a.total_work_ms)
            .sum()
    }

    /// The most urgent status among some terminals.
    pub fn rollup<'a>(&self, addresses: impl IntoIterator<Item = &'a String>) -> Option<&'static str> {
        addresses.into_iter().filter_map(|a| self.status(a)).max_by_key(|s| priority(Some(s)))
    }


}

pub fn pane(id: &str, address: &str) -> Node {
    Node::block(id, Content::Terminal { terminal: address.to_owned() })
}

/// A folder as a name: its last part, `~` for the home folder (`/` and `\` both).
pub fn folder_name(path: &str, home: &str) -> String {
    let full = path;
    let path = path.trim_end_matches(['/', '\\']);
    let home = home.trim_end_matches(['/', '\\']);
    let same = if cfg!(windows) { path.eq_ignore_ascii_case(home) } else { path == home };
    if full.is_empty() || same {
        return "~".into();
    }
    if path.is_empty() {
        return full[..1].to_owned();
    }
    path.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(path).to_owned()
}

/// Terminal addresses in a subtree, in tree order.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn addresses(node: &Node) -> Vec<String> {
    node.terminals().iter().filter_map(|n| n.terminal().map(str::to_owned)).collect()
}

pub fn any_addresses(node: &Node) -> Vec<String> {
    node.panes().into_iter().filter_map(|n| n.any_terminal().map(str::to_owned)).collect()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FolderInfo {
    /// The checkout's top folder (a worktree's own), else the folder itself.
    pub top: String,
    pub branch: Option<String>,
}

/// A space as mods see it. `name` is what to call it; `named`: someone gave it
/// (else it's Herdr's automatic name, which a mod may show its own way).
pub fn space_json(m: &Model, p: &Space, index: usize, branch: Option<&str>, home: &str) -> Value {
    let active = p.active_tab().map(|c| c.id.clone());
    let tabs: Vec<Value> = p
        .tabs
        .iter()
        .enumerate()
        .map(|(i, c)| {
            json!({
                "id": c.id,
                "index": i + 1,
                "name": Model::tab_name(p, c),
                "named": !c.name.is_empty(),
                "active": Some(&c.id) == active.as_ref(),
                "status": m.rollup(&any_addresses(&c.root)),
                "panes": c.root.panes().len(),
                "zoomed": c.zoomed.is_some(),
                "settled": c.settled.is_some(),
                "settled_at": c.settled,
                "changed_at": m.changed_at(c),
                "working_since": m.working_since(c),
                "work_ms": m.work_ms(c),
                "total_work_ms": m.total_work_ms(c),
            })
        })
        .collect();
    let all: Vec<String> = p.tabs.iter().flat_map(|c| any_addresses(&c.root)).collect();
    json!({
        "id": p.id,
        "index": index + 1,
        "name": m.space_name(p, home),
        "named": !p.name.is_empty(),
        "folder": p.folder,
        "machine": p.machine,
        "branch": branch,
        "active": m.active.as_deref() == Some(&p.id),
        "status": m.rollup(&all),
        "tabs": tabs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_name_as_in_herdr_on_every_system() {
        assert_eq!(folder_name("/home/can", "/home/can"), "~");
        assert_eq!(folder_name("/home/can/", "/home/can"), "~");
        assert_eq!(folder_name("/Users/can/Projects/herdr", "/Users/can"), "herdr");
        assert_eq!(folder_name("/", "/home/can"), "/");
        assert_eq!(folder_name("", "/home/can"), "~");
        assert_eq!(folder_name(r"C:\Users\can\src\api", r"C:\Users\can"), "api");
        assert_eq!(folder_name(r"\\?\C:\Users\can\src\api\", r"C:\Users\can"), "api");
        assert_eq!(folder_name("C:/Users/can/src/web", r"C:\Users\can"), "web");
    }

    #[test]
    fn unnamed_spaces_follow_their_first_terminal_and_tabs_count() {
        let mut m = Model::default();
        m.add_space("p1", "", "/home/me", Tab::new("c1", "", "x1", "t1"));
        m.add_tab("p1", Tab::new("c2", "review", "x2", "t2"));
        m.add_tab("p1", Tab::new("c3", "", "x3", "t3"));
        let p = m.space("p1").unwrap().clone();
        assert_eq!(m.space_name(&p, "/home/me"), "~");
        m.pane_folders.insert("x1".into(), "/src/notes".into());
        assert_eq!(m.space_name(&p, "/home/me"), "notes");
        m.folder_info.insert("t1".into(), FolderInfo { top: "/src/herdr".into(), branch: Some("main".into()) });
        assert_eq!(m.space_name(&p, "/home/me"), "herdr");
        // When the active tab (`c3`/`t3`) has its own live folder (e.g. a Git Bash or CMD tab cd'd elsewhere),
        // unnamed space name follows the active tab's folder.
        m.pane_folders.insert("x3".into(), "/src/cmd-proj".into());
        assert_eq!(m.space_name(&p, "/home/me"), "cmd-proj");
        m.folder_info.insert("t3".into(), FolderInfo { top: "/src/gitbash-repo".into(), branch: Some("dev".into()) });
        assert_eq!(m.space_name(&p, "/home/me"), "gitbash-repo");
        let names: Vec<String> = p.tabs.iter().map(|c| Model::tab_name(&p, c)).collect();
        assert_eq!(names.iter().map(String::as_str).collect::<Vec<_>>(), ["1", "review", "3"]);
        m.space_mut("p1").unwrap().name = "api".into();
        assert_eq!(m.space_name(m.space("p1").unwrap(), "/home/me"), "api");
    }

    #[test]
    fn alerts_follow_herdr() {
        let alert = Model::alert;
        assert_eq!(alert(Some("working"), Some("blocked"), true), Some(Alert::Blocked));
        assert_eq!(alert(Some("blocked"), Some("error"), false), None);
        assert_eq!(alert(Some("working"), Some("idle"), false), Some(Alert::Done));
        assert_eq!(alert(Some("blocked"), Some("done"), false), Some(Alert::Done));
        assert_eq!(alert(Some("working"), Some("idle"), true), None);
        assert_eq!(alert(None, Some("idle"), false), None);
        assert_eq!(alert(Some("working"), None, false), None);
    }

    #[test]
    fn only_our_own_agents_alert() {
        let m = model_with_one();
        // In a space: it rings.
        assert_eq!(m.alert_for("t1", false, Some("working"), Some("done"), false), Some(Alert::Done));
        // Another machine's, or another app's, in none of the spaces: quiet.
        assert_eq!(m.alert_for("vps/t7", false, Some("working"), Some("done"), false), None);
        assert_eq!(m.alert_for("t9", false, Some("working"), Some("blocked"), false), None);
        // A sub-agent reports to its parent, not to the person.
        assert_eq!(m.alert_for("t1", true, Some("working"), Some("blocked"), false), None);
    }

    #[test]
    fn tab_move_refuses_panes_and_containers_and_leaves_the_tabs_alone() {
        let mut m = Model::default();
        m.add_space("s1", "api", "/src", Tab::new("c1", "", "x1", "t1"));
        m.split("x1", crate::layout::Axis::Column, "x2", "t2");
        m.add_tab("s1", Tab::new("c2", "", "x3", "t3"));
        // The column split wraps x1 and x2 in a container of their own, inside the tab's root.
        let inner = &m.tab("c1").unwrap().root.children()[0];
        assert!(!inner.is_pane() && inner.id != "c1");
        let container = inner.id.clone();
        let tabs = |m: &Model| m.space("s1").unwrap().tabs.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        for id in ["x1", "x3", container.as_str(), "s1", "nope"] {
            assert!(!m.move_tab(id, 2), "{id}");
            assert_eq!(tabs(&m), ["c1", "c2"], "{id}");
        }
        assert!(m.move_tab("c2", 1));
        assert_eq!(tabs(&m), ["c2", "c1"]);
        assert!(m.move_tab("c2", 9));
        assert_eq!(tabs(&m), ["c1", "c2"]);
    }

    #[test]
    fn shift_tab_steps_over_the_shown_tabs_and_wraps() {
        let mut m = Model::default();
        m.add_space("s1", "api", "/src", Tab::new("c1", "", "x1", "t1"));
        for (c, x) in [("c2", "x2"), ("c3", "x3"), ("c4", "x4")] {
            m.add_tab("s1", Tab::new(c, "", x, "t"));
        }
        let tabs = |m: &Model| m.space("s1").unwrap().tabs.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        assert!(m.shift_tab("c2", 1));
        assert_eq!(tabs(&m), ["c1", "c3", "c2", "c4"]);
        assert!(m.shift_tab("c2", -1));
        assert_eq!(tabs(&m), ["c1", "c2", "c3", "c4"]);
        // Past either end: to the other end.
        assert!(m.shift_tab("c4", 1));
        assert_eq!(tabs(&m), ["c4", "c1", "c2", "c3"]);
        assert!(m.shift_tab("c4", -1));
        assert_eq!(tabs(&m), ["c1", "c2", "c3", "c4"]);
        // A settled tab isn't shown, so a step goes past it.
        m.space_mut("s1").unwrap().tabs[2].settled = Some(1);
        assert!(m.shift_tab("c2", 1));
        assert_eq!(tabs(&m), ["c1", "c3", "c4", "c2"]);
        assert!(m.shift_tab("c2", 0));
        assert_eq!(tabs(&m), ["c1", "c3", "c4", "c2"]);
        // Past the end with the settled one before it: to the front.
        assert!(m.shift_tab("c2", 1));
        assert_eq!(tabs(&m), ["c2", "c1", "c3", "c4"]);
        // The settled tab itself moves among the shown ones.
        assert!(m.shift_tab("c3", -1));
        assert_eq!(tabs(&m), ["c2", "c3", "c1", "c4"]);
        // More places than tabs wraps round; i64's ends don't overflow.
        assert!(m.shift_tab("c4", 7));
        assert_eq!(tabs(&m), ["c4", "c2", "c3", "c1"]);
        assert!(m.shift_tab("c4", i64::MAX));
        assert!(m.shift_tab("c4", i64::MIN));
        assert!(!m.shift_tab("x1", 1));
        assert!(!m.shift_tab("nope", 1));
    }

    fn model_with_one() -> Model {
        let mut m = Model::default();
        m.add_space("p1", "api", "/src/api", Tab::new("c1", "fix login", "x1", "t1"));
        m
    }

    #[test]
    fn unshown_spaces_and_unselected_tabs_keep_what_is_shown() {
        let mut m = Model::default();
        m.add_space_unshown("p1", "", "/a", Tab::new("c1", "", "x1", "t1"));
        assert_eq!(m.active.as_deref(), Some("p1"), "with nothing shown, the new space is");
        m.add_space_unshown("p2", "", "/b", Tab::new("c2", "", "x2", "t2"));
        assert_eq!(m.active.as_deref(), Some("p1"));
        assert_eq!(m.space("p2").unwrap().active.as_deref(), Some("c2"), "shown later, it opens on its tab");
        assert!(m.add_tab_unselected("p1", Tab::new("c3", "", "x3", "t3")));
        assert!(m.add_tab_unselected("p2", Tab::new("c4", "", "x4", "t4")));
        assert_eq!(m.active_tab().map(|c| c.id.as_str()), Some("c1"));
        assert_eq!(m.space("p2").unwrap().active.as_deref(), Some("c2"));
        assert!(m.add_tab_unselected("p1", Tab::new("c5", "", "x5", "t5")));
        let order: Vec<&str> = m.space("p1").unwrap().tabs.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(order, ["c1", "c3", "c5"], "at the end, in the order they came");
        assert_eq!(m.active_tab().map(|c| c.id.as_str()), Some("c1"));

        let mut stale = Model { active: Some("gone".into()), ..Model::default() };
        stale.add_space_unshown("p9", "", "/c", Tab::new("c9", "", "x9", "t9"));
        assert_eq!(stale.active.as_deref(), Some("p9"), "a stale id shows nothing, so the new space is shown");
    }

    #[test]
    fn settling_the_shown_tab_hands_over_and_showing_it_brings_it_back() {
        let mut m = model_with_one();
        m.add_tab("p1", Tab::new("c2", "docs", "x2", "t2"));
        m.select_tab("c1");
        assert!(m.settle("c1", 5));
        assert_eq!(m.active_tab().map(|c| c.id.as_str()), Some("c2"));
        assert_eq!(m.tab("c1").unwrap().settled, Some(5));
        // The last unsettled one stays shown, settled.
        assert!(m.settle("c2", 6));
        assert_eq!(m.active_tab().map(|c| c.id.as_str()), Some("c2"));
        m.select_tab("c1");
        assert_eq!(m.tab("c1").unwrap().settled, None);
    }

    #[test]
    fn closing_the_last_pane_closes_tab_then_space_and_remembers_it() {
        let mut m = model_with_one();
        m.add_tab("p1", Tab::new("c2", "docs", "x2", "t2"));
        m.add_space("p2", "web", "/src/web", Tab::new("c3", "deploy", "x3", "t3"));
        assert_eq!(m.active.as_deref(), Some("p2"));
        assert_eq!(m.remove_pane("x3"), (true, true));
        assert_eq!(m.active.as_deref(), Some("p1"));
        assert_eq!(m.recent, [Recent { name: "web".into(), folder: "/src/web".into() }]);
        // A tab closes with its last pane; the space stays while it has another.
        assert_eq!(m.remove_pane("x2"), (true, false));
        assert_eq!(m.active_tab().map(|c| c.id.as_str()), Some("c1"));
        // Opening it again takes it off the recent list.
        m.add_space("p4", "web", "/src/web", Tab::new("c4", "x", "x4", "t4"));
        assert!(m.recent.is_empty());
    }

    #[test]
    fn a_mod_is_a_pane_beside_terminals() {
        let mut m = model_with_one();
        let viewer = Node::block("x2", Content::Mod { name: "markdown".into(), props: json!({ "file": "README.md" }) });
        assert!(m.split_with("x1", Axis::Row, viewer));
        let c = m.tab("c1").unwrap();
        assert_eq!(c.root.panes().len(), 2);
        assert_eq!(c.focused.as_deref(), Some("x2"));
        // It has no terminal: nothing to restart or kill, and the tab survives
        // its terminal pane closing.
        assert_eq!(m.address_of("x2"), None);
        assert_eq!(m.remove_pane("x1"), (false, false));
        assert_eq!(m.tab("c1").unwrap().root.panes().len(), 1);
    }

    #[test]
    fn pane_moves_preserve_terminals_and_focus_for_swap_and_every_edge() {
        use crate::layout::Edge;
        for edge in [None, Some(Edge::Left), Some(Edge::Right), Some(Edge::Top), Some(Edge::Bottom)] {
            let mut m = model_with_one();
            m.split("x1", Axis::Row, "x2", "t2");
            m.split("x2", Axis::Column, "x3", "t3");
            assert!(m.move_pane("x1", "x3", edge));
            assert_eq!(m.focused_pane().as_deref(), Some("x1"));
            for (pane, address) in [("x1", "t1"), ("x2", "t2"), ("x3", "t3")] {
                assert_eq!(m.address_of(pane).as_deref(), Some(address));
            }
            let root = &m.active_tab().unwrap().root;
            assert_eq!(root.panes().len(), 3);
            if let Some(edge) = edge {
                let parent = root.parent_of("x1").unwrap();
                let expected_axis = if matches!(edge, Edge::Left | Edge::Right) { Axis::Row } else { Axis::Column };
                assert_eq!(parent.axis(), Some(expected_axis));
                let ids: Vec<_> = parent.children().iter().map(|n| n.id.as_str()).collect();
                let source = ids.iter().position(|id| *id == "x1").unwrap();
                let target = ids.iter().position(|id| *id == "x3").unwrap();
                assert_eq!(source < target, matches!(edge, Edge::Left | Edge::Top));
                assert_eq!(source.abs_diff(target), 1);
            }
        }
    }

    #[test]
    fn cross_axis_pane_moves_split_equally_not_with_the_old_slot_size() {
        use crate::layout::{Edge, Size};
        for old_size in [Some(Size::Weight { weight: 0.5 }), Some(Size::Px { px: 200. })] {
            let mut m = model_with_one();
            m.split("x1", Axis::Row, "x2", "t2");
            m.tab_of_mut("x2").unwrap().root.find_mut("x2").unwrap().size = old_size;
            assert!(m.move_pane("x2", "x1", Some(Edge::Bottom)));
            let parent = m.active_tab().unwrap().root.parent_of("x2").unwrap();
            assert_eq!(parent.children()[0].size, None);
            assert_eq!(parent.children()[1].size, None);
        }
    }

    #[test]
    fn invalid_pane_moves_leave_the_model_alone() {
        let mut m = model_with_one();
        m.add_tab("p1", Tab::new("c2", "docs", "x2", "t2"));
        let before = format!("{:?}", m.spaces);
        for (source, target) in [("x1", "x1"), ("x1", "missing"), ("missing", "x1"), ("x1", "x2"), ("c1", "x1")] {
            assert!(!m.move_pane(source, target, None));
            assert_eq!(format!("{:?}", m.spaces), before);
        }
    }

    #[test]
    fn new_tabs_append_to_the_end() {
        let mut m = model_with_one();
        m.add_tab("p1", Tab::new("c2", "b", "x2", "t2"));
        m.select_tab("c1");
        m.add_tab("p1", Tab::new("c3", "c", "x3", "t3"));
        let order: Vec<&str> = m.spaces[0].tabs.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(order, ["c1", "c2", "c3"]);
        assert_eq!(m.active_tab().map(|c| c.id.as_str()), Some("c3"));
    }

    fn three_panes_and_another_tab() -> Model {
        let mut m = model_with_one();
        m.split("x1", Axis::Row, "x2", "t2");
        m.split("x2", Axis::Row, "x3", "t3");
        m.add_tab("p1", Tab::new("c2", "docs", "x4", "t4"));
        m.select_tab("c1");
        m.focus_pane("x1");
        m
    }

    fn pane_order(m: &Model, tab: &str) -> Vec<String> {
        m.tab(tab).unwrap().root.panes().iter().map(|n| n.id.clone()).collect()
    }

    #[test]
    fn swapping_panes_by_id_trades_places_and_keeps_focus_on_the_same_id() {
        let mut m = three_panes_and_another_tab();
        assert_eq!(m.swap_panes("x1", "x3"), Ok(()));
        assert_eq!(pane_order(&m, "c1"), ["x3", "x2", "x1"]);
        assert_eq!(m.focused_pane().as_deref(), Some("x1"));
        assert_eq!(m.address_of("x1").as_deref(), Some("t1"));
        assert_eq!(m.swap_panes("x2", "x3"), Ok(()));
        assert_eq!(pane_order(&m, "c1"), ["x2", "x3", "x1"]);
        assert_eq!(m.focused_pane().as_deref(), Some("x1"));
    }

    #[test]
    fn swapping_panes_by_id_refuses_each_bad_pair_without_changes() {
        let mut m = three_panes_and_another_tab();
        let cases = [
            ("nope", "x2", "no pane \"nope\""),
            ("x1", "nope", "no target pane \"nope\""),
            ("x1", "x1", "pane.swap target is the pane itself"),
            ("x1", "x4", "pane.swap target \"x4\" is in another tab"),
        ];
        for (pane, target, error) in cases {
            assert_eq!(m.swap_panes(pane, target), Err(error.to_owned()), "{pane} {target}");
            assert_eq!(pane_order(&m, "c1"), ["x1", "x2", "x3"], "{pane} {target}");
            assert_eq!(pane_order(&m, "c2"), ["x4"], "{pane} {target}");
            assert_eq!(m.focused_pane().as_deref(), Some("x1"), "{pane} {target}");
        }
    }

    #[test]
    fn swapping_refuses_a_container_as_pane_or_target() {
        let mut m = three_panes_and_another_tab();
        let container = m.tab("c1").unwrap().root.id.clone();
        assert_eq!(m.swap_panes(&container, "x1"), Err(format!("no pane {container:?}")));
        assert_eq!(m.swap_panes("x1", &container), Err(format!("no target pane {container:?}")));
        assert_eq!(pane_order(&m, "c1"), ["x1", "x2", "x3"]);
    }

    #[test]
    fn splits_focus_the_new_pane_and_closing_it_moves_focus() {
        let mut m = model_with_one();
        assert!(m.split("x1", Axis::Row, "x9", "t9"));
        assert_eq!(m.focused_pane().as_deref(), Some("x9"));
        assert_eq!(m.remove_pane("x9"), (false, false));
        assert_eq!(m.focused_pane().as_deref(), Some("x1"));
        // The arrangement keeps the tab's id with one pane left.
        assert_eq!(m.active_tab().unwrap().root.id, "c1");
    }

    #[test]
    fn closing_the_focused_pane_goes_back_to_the_one_before() {
        let mut m = model_with_one();
        m.split("x1", Axis::Row, "x2", "t2");
        m.split("x2", Axis::Row, "x3", "t3");
        // From x2 to x3; closing x3 lands on x2, not on the first pane.
        m.remove_pane("x3");
        assert_eq!(m.focused_pane().as_deref(), Some("x2"));
        // Clicked around: x1, then x2; close x2 and it's x1.
        m.split("x2", Axis::Row, "x4", "t4");
        m.focus_pane("x1");
        m.focus_pane("x4");
        m.remove_pane("x4");
        assert_eq!(m.focused_pane().as_deref(), Some("x1"));
        // The one before is gone too: the next one back that's still there.
        m.split("x2", Axis::Row, "x5", "t5");
        m.focus_pane("x2");
        m.remove_pane("x5");
        m.remove_pane("x2");
        assert_eq!(m.focused_pane().as_deref(), Some("x1"));
    }

    #[test]
    fn finishes_you_did_not_watch_are_done_until_seen() {
        let mut m = model_with_one();
        m.set_word("t1", Some("working".into()), Some("claude".into()), false);
        assert_eq!(m.status("t1"), Some("working"));
        m.set_word("t1", Some("idle".into()), None, false);
        assert_eq!(m.status("t1"), Some("done"));
        assert_eq!(m.rollup(&m.all_addresses()), Some("done"));
        m.mark_seen(&["t1".into()]);
        assert_eq!(m.status("t1"), Some("idle"));
        // Watching it finish: straight to idle.
        m.set_word("t1", Some("working".into()), None, true);
        m.set_word("t1", Some("done".into()), None, true);
        assert_eq!(m.status("t1"), Some("idle"));
        m.set_word("t1", Some("blocked".into()), None, true);
        assert_eq!(m.rollup(&m.all_addresses()), Some("blocked"));
    }

    #[test]
    fn a_shared_seen_turn_decides_over_this_screen() {
        let mut m = model_with_one();
        m.set_word("t1", Some("working".into()), None, true);
        m.set_word("t1", Some("idle".into()), None, true);
        assert_eq!(m.status("t1"), Some("idle"));
        // Another client hasn't seen turn 3 (marked unread): done, though we watched.
        assert!(m.set_turn("t1", 3, Some(2)));
        assert_eq!(m.status("t1"), Some("done"));
        assert!(!m.set_turn("t1", 3, Some(2)));
        // Seen elsewhere: idle, though we didn't watch.
        m.set_word("t1", Some("working".into()), None, false);
        m.set_word("t1", Some("idle".into()), None, false);
        m.set_turn("t1", 4, Some(4));
        assert_eq!(m.status("t1"), Some("idle"));
        // Needing you or working beats seen.
        m.set_word("t1", Some("blocked".into()), None, false);
        assert_eq!(m.status("t1"), Some("blocked"));
        // No label: this screen's own rule.
        m.set_word("t1", Some("idle".into()), None, false);
        m.set_turn("t1", 5, None);
        assert_eq!(m.status("t1"), Some("done"));
    }

    #[test]
    fn non_7501_pty_with_zero_turn_and_seen_zero_label_becomes_done_until_seen() {
        let mut m = model_with_one();
        // Spawned PTY terminals start with herdr.seen="0" while core turn stays 0 for non-7501 CLIs.
        m.set_word("t1", Some("working".into()), Some("claude".into()), false);
        m.set_turn("t1", 0, Some(0));
        assert_eq!(m.status("t1"), Some("working"));

        // Finishing a task transitions to "done" instead of skipping straight to "idle".
        m.set_word("t1", Some("idle".into()), None, false);
        m.set_turn("t1", 0, Some(0));
        assert_eq!(m.status("t1"), Some("done"));

        // Viewing the agent transitions "done" -> "idle".
        assert!(m.mark_seen(&["t1".into()]));
        assert_eq!(m.status("t1"), Some("idle"));

        // AI detecting a fast completion marks an idle agent unseen -> "done" until viewed.
        assert!(m.mark_unseen("t1"));
        assert_eq!(m.status("t1"), Some("done"));
        assert!(!m.mark_unseen("t1"));
    }

    #[test]
    fn focusing_another_pane_in_a_zoomed_tab_moves_the_zoom() {
        let mut m = model_with_one();
        assert!(m.split("x1", Axis::Row, "x2", "t2"));
        m.tab_mut("c1").unwrap().zoomed = Some("x2".into());
        assert!(m.focus_pane("x1"));
        assert_eq!(m.tab("c1").unwrap().zoomed.as_deref(), Some("x1"));
        // A tab nobody zoomed stays unzoomed when focus moves.
        m.tab_mut("c1").unwrap().zoomed = None;
        assert!(m.focus_pane("x2"));
        assert_eq!(m.tab("c1").unwrap().zoomed, None);
    }

    #[test]
    fn zooming_a_pane_focuses_it() {
        let mut m = model_with_one();
        // A tab with one pane doesn't zoom.
        assert!(!m.toggle_zoom("x1"));
        assert!(m.split("x1", Axis::Row, "x2", "t2"));
        assert!(m.focus_pane("x2"));
        // Zooming the other pane (its header's button) shows it and gives it the keyboard.
        assert!(m.toggle_zoom("x1"));
        assert_eq!(m.tab("c1").unwrap().zoomed.as_deref(), Some("x1"));
        assert_eq!(m.focused_pane().as_deref(), Some("x1"));
        assert_eq!(m.tab("c1").unwrap().before, ["x2"]);
        // Unzooming leaves focus where it is.
        assert!(!m.toggle_zoom("x1"));
        assert_eq!(m.tab("c1").unwrap().zoomed, None);
        assert_eq!(m.focused_pane().as_deref(), Some("x1"));
    }

    #[test]
    fn focus_follows_panes_across_tabs_and_spaces() {
        let mut m = model_with_one();
        m.add_tab("p1", Tab::new("c2", "b", "x2", "t2"));
        assert_eq!(m.focused_pane().as_deref(), Some("x2"));
        m.add_space("p2", "web", "/w", Tab::new("c3", "c", "x3", "t3"));
        assert!(m.focus_pane("x1"));
        assert_eq!(m.active.as_deref(), Some("p1"));
        assert_eq!(m.focused_pane().as_deref(), Some("x1"));
        assert_eq!(m.active_tab().map(|c| addresses(c.shown())).unwrap_or_default(), ["t1"]);
    }

    #[test]
    fn manual_status_overrides_inferred_status_until_cleared() {
        let mut m = model_with_one();
        m.set_word("t1", Some("working".into()), Some("claude".into()), true);
        assert_eq!(m.status("t1"), Some("working"));

        assert!(m.set_manual_status("t1", Some("done".into()), None));
        assert_eq!(m.status("t1"), Some("done"));
        // Marking seen does not clear an explicit manual "done" override.
        m.mark_seen(&["t1".into()]);
        assert_eq!(m.status("t1"), Some("done"));
        assert_eq!(m.alert_for("t1", false, Some("working"), Some("idle"), false), None);

        assert!(m.set_manual_status("t1", Some("idle".into()), None));
        assert_eq!(m.status("t1"), Some("idle"));

        assert!(m.set_manual_status("t1", None, None));
        assert_eq!(m.status("t1"), Some("working"));
    }

    #[test]
    fn merge_tabs_combines_panes_removes_source_and_preserves_acp_panes() {
        use crate::layout::Edge;

        let mut m = model_with_one();
        m.add_tab("p1", Tab::new("c2", "two", "x2", "t2"));
        m.add_tab(
            "p1",
            Tab::with_pane(
                "c3",
                "acp",
                Node::block("acp-1", Content::Mod { name: "peek".into(), props: json!({ "acp": true, "terminal": "a1" }) }),
            ),
        );

        let moved = m.merge_tabs("c2", "c1", Edge::Right).unwrap();
        assert_eq!(moved, ["t2"]);
        assert!(m.tab("c2").is_none());
        let c1 = m.tab("c1").unwrap();
        assert_eq!(c1.root.axis(), Some(Axis::Row));
        let ids: Vec<_> = c1.root.panes().into_iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["x1", "x2"]);
        assert_eq!(m.focused_pane().as_deref(), Some("x2"));
        assert_eq!(m.active_tab().map(|c| c.id.as_str()), Some("c1"));
        assert_eq!(m.pane_of("t1").as_deref(), Some("x1"));
        assert_eq!(m.pane_of("t2").as_deref(), Some("x2"));

        // Merging a third ACP tab flattens into the row and keeps ACP identity intact.
        let moved_acp = m.merge_tabs("c3", "c1", Edge::Right).unwrap();
        assert_eq!(moved_acp, ["a1"]);
        assert!(m.tab("c3").is_none());
        let ids: Vec<_> = m.tab("c1").unwrap().root.panes().into_iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["x1", "x2", "acp-1"]);
        assert_eq!(m.acp_pane_of("a1").as_deref(), Some("acp-1"));
        assert_eq!(m.pane_of("t1").as_deref(), Some("x1"));

        // Reject self-merge and cross-space merge.
        assert!(m.merge_tabs("c1", "c1", Edge::Right).is_err());
        m.add_space("p2", "other", "/o", Tab::new("c4", "four", "x4", "t4"));
        assert!(m.merge_tabs("c4", "c1", Edge::Right).is_err());
    }

    #[test]
    fn detach_pane_splits_merged_pane_into_standalone_tab_and_clears_zoom() {
        use crate::layout::Edge;

        let mut m = model_with_one();
        m.add_tab("p1", Tab::new("c2", "two", "x2", "t2"));
        m.add_tab(
            "p1",
            Tab::with_pane(
                "c3",
                "acp",
                Node::block("acp-1", Content::Mod { name: "peek".into(), props: json!({ "acp": true, "terminal": "a1" }) }),
            ),
        );
        m.merge_tabs("c2", "c1", Edge::Right).unwrap();
        m.merge_tabs("c3", "c1", Edge::Right).unwrap();
        // Simulate uneven divider resizing before detaching: remaining panes must rebalance equally.
        if let Some(x1) = m.tab_mut("c1").unwrap().root.find_mut("x1") {
            x1.size = Some(crate::layout::Size::Weight { weight: 1.8 });
        }
        if let Some(x2) = m.tab_mut("c1").unwrap().root.find_mut("x2") {
            x2.size = Some(crate::layout::Size::Weight { weight: 0.4 });
        }
        m.tab_mut("c1").unwrap().zoomed = Some("acp-1".into());
        m.tab_mut("c1").unwrap().focus("acp-1");

        // Detach the ACP pane from the merged tab.
        let (new_acp_tab, moved_addr) = m.detach_pane("acp-1", "Codex").unwrap();
        assert_eq!(moved_addr.as_deref(), Some("a1"));
        assert_eq!(m.active_tab().map(|c| c.id.as_str()), Some(new_acp_tab.as_str()));
        assert_eq!(m.acp_pane_of("a1").as_deref(), Some("acp-1"));
        assert_eq!(m.tab(&new_acp_tab).unwrap().name, "Codex");
        let space = m.space("p1").unwrap();
        let tab_ids: Vec<_> = space.tabs.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(tab_ids, ["c1", new_acp_tab.as_str()]);
        let src = m.tab("c1").unwrap();
        assert_eq!(src.zoomed, None);
        assert_eq!(src.focused.as_deref(), Some("x2"));
        assert_eq!(
            src.root.panes().into_iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["x1", "x2"]
        );
        assert!(src.root.children().iter().all(|ch| ch.size.is_none()), "remaining panes must share width equally: {:?}", src.root.children());

        // Detaching from a single-pane tab fails cleanly.
        assert!(m.detach_pane("acp-1", "").is_err());

        // Detach x2 out of c1 as well, leaving x1 alone in c1.
        let (new_x2_tab, moved_t2) = m.detach_pane("x2", "").unwrap();
        assert_eq!(moved_t2.as_deref(), Some("t2"));
        assert_eq!(m.pane_of("t2").as_deref(), Some("x2"));
        let tab_ids: Vec<_> = m.space("p1").unwrap().tabs.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(tab_ids, ["c1", new_acp_tab.as_str(), new_x2_tab.as_str()]);
        assert_eq!(m.tab("c1").unwrap().root.panes().len(), 1);
    }

    #[test]
    fn agent_timing_tracks_runs_pauses_and_syncs_core_values() {
        let mut m = model_with_one();
        assert!(m.set_word("t1", Some("working".into()), Some("claude".into()), true));
        let a = m.agents.get_mut("t1").unwrap();
        assert!(a.working_since.is_some());
        assert_eq!(a.work_ms, Some(0));
        a.working_since = Some(a.working_since.unwrap().saturating_sub(3_000));

        assert!(m.set_word("t1", Some("blocked".into()), None, true));
        let paused = m.agents["t1"].work_ms.expect("paused work_ms");
        assert!(paused >= 3_000);
        assert_eq!(m.agents["t1"].working_since, None);
        assert_eq!(m.agents["t1"].total_work_ms, paused);

        assert!(m.set_word("t1", Some("working".into()), None, true));
        assert!(m.agents["t1"].working_since.is_some());
        assert_eq!(m.agents["t1"].work_ms, Some(paused));

        assert!(m.set_word("t1", Some("done".into()), None, false));
        let finished = m.agents["t1"].work_ms.expect("finished work_ms");
        assert!(finished >= paused);
        assert_eq!(m.agents["t1"].working_since, None);
        assert_eq!(m.agents["t1"].total_work_ms, finished);

        let tab = m.tab("c1").unwrap();
        assert_eq!(m.work_ms(tab), Some(finished));
        assert_eq!(m.total_work_ms(tab), finished);

        // Cold ACP handshake ("starting" at turn 0) resets fake timing until real work starts.
        assert!(m.set_word("a1", Some("working".into()), Some("codex".into()), true));
        assert!(m.sync_core_timing("a1", None, None, None, None, true));
        assert_eq!(m.agents["a1"].working_since, None);
        assert_eq!(m.agents["a1"].work_ms, None);

        // Core timing sync overrides when core reports active work_ms / working_since.
        assert!(m.sync_core_timing("a1", Some(123_000), Some(4_500), Some(9_000), Some(123_000), false));
        assert_eq!(m.agents["a1"].working_since, Some(123_000));
        assert_eq!(m.agents["a1"].work_ms, Some(4_500));
        assert_eq!(m.agents["a1"].total_work_ms, 9_000);

        // Clearing an exited CLI agent removes tab-level status and execution timing.
        assert!(m.clear_agent("t1"));
        assert!(!m.clear_agent("t1"));
        assert_eq!(m.status("t1"), None);
        let tab = m.tab("c1").unwrap();
        assert_eq!(m.working_since(tab), None);
        assert_eq!(m.work_ms(tab), None);
        assert_eq!(m.total_work_ms(tab), 0);
    }
}
