//! The heap explorer model: a sharing-aware graph over the live term graph,
//! rooted at the session's external pointers plus optional leaked subgraphs.

use std::collections::{HashMap, HashSet, VecDeque};

use atlas_core::vm::heap::{Addr, ArenaKind, HeapScope, TermPtr, TypeInfo};
use atlas_core::vm::term::Term;

/// A top-level graph root: a name plus the live pointer it borrows.
pub struct RootEntry<'a, 'h> {
    pub label: String,
    pub ptr: &'a TermPtr<'h>,
    pub leaked: bool,
}

/// One unique node in the reachable heap graph.
pub struct Node {
    pub id: usize,
    pub addr: Addr,
    pub summary: String,
    pub edges: Vec<(String, Addr)>,
    pub incoming: Vec<String>,
    pub roots: Vec<String>,
    pub leaked: bool,
    pub expanded: bool,
}

pub struct ExplorerState {
    /// Full-dump mode: scan for and show leaked (unreachable) subgraphs.
    pub show_leaked: bool,
    pub selected: usize,
    pub nodes: Vec<Node>,
    pub stats: Vec<(ArenaKind, usize)>,
}

impl ExplorerState {
    pub fn new() -> Self {
        ExplorerState {
            show_leaked: false,
            selected: 0,
            nodes: Vec::new(),
            stats: Vec::new(),
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.nodes.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.nodes.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    /// Toggle the selected node's inline port display.
    pub fn toggle_selected(&mut self) -> bool {
        let Some(node) = self.nodes.get_mut(self.selected) else {
            return false;
        };
        node.expanded = !node.expanded;
        true
    }

    pub fn selected_node(&self) -> Option<&Node> {
        self.nodes.get(self.selected)
    }

    /// Rebuild a deterministic, unique-node graph from the external roots.
    pub fn rebuild<'h>(&mut self, h: &'h HeapScope<'h>, roots: &[RootEntry<'_, 'h>]) {
        self.stats = ArenaKind::ALL
            .iter()
            .map(|&kind| (kind, h.arena_len(kind)))
            .collect();

        let old_expanded = self
            .nodes
            .iter()
            .filter(|node| node.expanded)
            .map(|node| node.addr.to_u64())
            .collect::<HashSet<_>>();
        let mut nodes = Vec::new();
        let mut by_addr = HashMap::new();
        let mut queue = VecDeque::new();

        for root in roots {
            if root.ptr.is_null() {
                continue;
            }
            let addr = root.ptr.addr();
            let index = if let Some(&index) = by_addr.get(&addr.to_u64()) {
                index
            } else {
                let index = nodes.len();
                by_addr.insert(addr.to_u64(), index);
                nodes.push(Node {
                    id: index,
                    addr,
                    summary: summarize(h, addr),
                    edges: Vec::new(),
                    incoming: Vec::new(),
                    roots: Vec::new(),
                    leaked: root.leaked,
                    expanded: old_expanded.contains(&addr.to_u64()),
                });
                queue.push_back((addr, root.leaked));
                index
            };
            if !nodes[index].roots.iter().any(|label| label == &root.label) {
                nodes[index].roots.push(root.label.clone());
            }
            nodes[index].leaked |= root.leaked;
        }

        let mut visited = HashSet::new();
        while let Some((addr, leaked)) = queue.pop_front() {
            let key = addr.to_u64();
            let Some(&index) = by_addr.get(&key) else {
                continue;
            };
            nodes[index].leaked |= leaked;
            if !visited.insert(key) {
                continue;
            }

            let edges = node_children(h, addr);
            nodes[index].edges = edges.clone();
            for (_, child) in edges {
                let child_key = child.to_u64();
                if let Some(&child_index) = by_addr.get(&child_key) {
                    nodes[child_index].leaked |= leaked;
                } else {
                    let child_index = nodes.len();
                    by_addr.insert(child_key, child_index);
                    nodes.push(Node {
                        id: child_index,
                        addr: child,
                        summary: summarize(h, child),
                        edges: Vec::new(),
                        incoming: Vec::new(),
                        roots: Vec::new(),
                        leaked,
                        expanded: old_expanded.contains(&child_key),
                    });
                }
                queue.push_back((child, leaked));
            }
        }

        for source in 0..nodes.len() {
            let source_id = nodes[source].id;
            let source_summary = nodes[source].summary.clone();
            for (edge, target) in nodes[source].edges.clone() {
                if let Some(&target_index) = by_addr.get(&target.to_u64()) {
                    let hint = interaction_hint(&source_summary, &nodes[target_index].summary);
                    let edge_name = match hint {
                        Some(hint) => format!("{edge} [{hint}]"),
                        None => edge,
                    };
                    nodes[target_index]
                        .incoming
                        .push(format!("n{source_id}.{edge_name}"));
                }
            }
        }

        self.nodes = nodes;
        self.selected = self.selected.min(self.nodes.len().saturating_sub(1));
    }
}

/// The labeled node-arena edges of the node at `addr`.
fn node_children<'h>(h: &'h HeapScope<'h>, addr: Addr) -> Vec<(String, Addr)> {
    let view = h.view_at(addr);
    match &*view {
        Term::App { func, arg } => vec![("func".into(), func.addr()), ("arg".into(), arg.addr())],
        Term::Lam { var, body } => vec![
            ("var".into(), h.var_addr(*var)),
            ("body".into(), body.addr()),
        ],
        Term::Use { body } => vec![("body".into(), body.addr())],
        Term::Dup { ptr, .. } => h
            .dup_peek(ptr)
            .map(|addr| vec![("value".to_string(), addr)])
            .unwrap_or_default(),
        Term::Sup { ptr, .. } => {
            let (left, right) = h.sup_addrs(ptr);
            vec![("left".into(), left), ("right".into(), right)]
        }
        Term::Ctn { ty, values, .. } => {
            let mut out = type_children(h, ty.addr());
            out.extend(
                (0..h.pack_len(values)).map(|i| (format!("field[{i}]"), h.pack_addr(values, i))),
            );
            out
        }
        Term::Partial { func, args, .. } => {
            let mut out = vec![("func".to_string(), func.addr())];
            out.extend((0..h.pack_len(args)).map(|i| (format!("arg[{i}]"), h.pack_addr(args, i))));
            out
        }
        Term::Ctr { ty, .. } => vec![("ty".into(), ty.addr())],
        Term::Type(ty) => type_children(h, ty.addr()),
        Term::Mat { matches } => {
            let data = h.match_data(matches);
            let mut out = Vec::new();
            for (i, &(key, branch)) in data.cases.iter().enumerate() {
                out.push((format!("case[{i}].key"), key));
                out.push((format!("case[{i}].branch"), branch));
            }
            if let Some(default) = data.default {
                out.push(("default".into(), default));
            }
            out
        }
        Term::Bop { lhs, rhs, .. } | Term::And { lhs, rhs } | Term::Or { lhs, rhs } => {
            vec![("lhs".into(), lhs.addr()), ("rhs".into(), rhs.addr())]
        }
        Term::Uop { val, .. } => vec![("val".into(), val.addr())],
        Term::Var { .. }
        | Term::VarId(_)
        | Term::Wld
        | Term::Err { .. }
        | Term::Int(_)
        | Term::Float(_)
        | Term::Char(_)
        | Term::Bool(_)
        | Term::Box(_)
        | Term::Pri(_)
        | Term::Null => vec![],
    }
}

fn type_children<'h>(h: &'h HeapScope<'h>, ty_addr: Addr) -> Vec<(String, Addr)> {
    match h.type_info_at(ty_addr) {
        TypeInfo::Product { fields, .. } => fields
            .iter()
            .enumerate()
            .map(|(i, &addr)| (format!("ty[{i}]"), addr))
            .collect(),
        TypeInfo::Sum { variants, .. } => variants
            .iter()
            .flat_map(|variant| {
                let name = h.variant_name(variant.name).to_string();
                variant
                    .args
                    .iter()
                    .enumerate()
                    .map(move |(i, &addr)| (format!("ty.{name}[{i}]"), addr))
                    .collect::<Vec<_>>()
            })
            .collect(),
    }
}

fn interaction_hint(source: &str, target: &str) -> Option<&'static str> {
    let source = source.split_whitespace().next().unwrap_or_default();
    let target = target.split_whitespace().next().unwrap_or_default();
    match (source, target) {
        ("App", "Lam") => Some("APP-LAM"),
        ("App", "Sup") => Some("APP-SUP"),
        ("Dup", "Lam") => Some("DUP-LAM"),
        ("Dup", "Sup") => Some("DUP-SUP"),
        _ => None,
    }
}

/// A one-line description of the node at `addr` (tag plus salient detail).
fn summarize<'h>(h: &'h HeapScope<'h>, addr: Addr) -> String {
    let view = h.view_at(addr);
    match &*view {
        Term::App { .. } => "App".into(),
        Term::Var { .. } => "Var (unsubstituted)".into(),
        Term::Lam { .. } => "Lam".into(),
        Term::Use { .. } => "Use".into(),
        Term::Dup { label, ptr } => {
            let state = if h.dup_peek(ptr).is_some() {
                "pending"
            } else {
                "fired"
            };
            format!("Dup label={} {}", label.get(), state)
        }
        Term::Sup { label, .. } => format!("Sup label={}", label.get()),
        Term::Ctn { ty, values, .. } => {
            let ty_name = h
                .type_name(ty.addr())
                .map(|name| name.to_string())
                .unwrap_or_else(|| "type".into());
            match h.pack_name(values) {
                Some(variant) => format!("Ctn {ty_name}::{}", h.variant_name(variant)),
                None => format!("Ctn {ty_name}"),
            }
        }
        Term::Partial { arity, args, .. } => {
            format!("Partial ({}/{arity} args)", h.pack_len(args))
        }
        Term::Ctr { variant, .. } => match variant {
            Some(variant) => format!("Ctr ::{}", h.variant_name(*variant)),
            None => "Ctr ::New".into(),
        },
        Term::VarId(variant) => format!("VarId {}", h.variant_name(*variant)),
        Term::Mat { matches } => {
            let data = h.match_data(matches);
            format!(
                "Mat ({} case{}{})",
                data.cases.len(),
                if data.cases.len() == 1 { "" } else { "s" },
                if data.default.is_some() {
                    " + default"
                } else {
                    ""
                },
            )
        }
        Term::Bop { op, .. } => format!("Bop {op:?}"),
        Term::Uop { op, .. } => format!("Uop {op:?}"),
        Term::And { .. } => "And".into(),
        Term::Or { .. } => "Or".into(),
        Term::Wld => "Wld".into(),
        Term::Err { .. } => "Err".into(),
        Term::Int(value) => format!("Int {value}"),
        Term::Float(value) => format!("Float {value}"),
        Term::Char(value) => format!("Char {value:?}"),
        Term::Bool(value) => format!("Bool {value}"),
        Term::Box(value) => match h.value_get(value) {
            atlas_core::vm::heap::Boxed::Str(value) => {
                format!("Box {:?}", truncate(value.to_string()))
            }
            atlas_core::vm::heap::Boxed::Bytes(value) => format!("Box [{} bytes]", value.len()),
        },
        Term::Type(ty) => match h.type_name(ty.addr()) {
            Some(name) => format!("Type {name}"),
            None => "Type (anonymous)".into(),
        },
        Term::Pri(_) => "Pri".into(),
        Term::Null => "Null".into(),
    }
}

fn truncate(s: String) -> String {
    const MAX: usize = 60;
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX {
        flat
    } else {
        let head: String = flat.chars().take(MAX - 1).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{LangMode, Session, SubmitResult};
    use atlas_core::vm::heap::Heap;

    #[test]
    fn graph_keeps_shared_nodes_unique_and_records_backlinks() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 1_000, false);
            let root = match session.submit(LangMode::Core, "(\\x -> x) 1") {
                SubmitResult::StartEval { root, .. } => root,
                _ => panic!("expected an evaluation root"),
            };
            let mut explorer = ExplorerState::new();
            explorer.rebuild(
                h,
                &[RootEntry {
                    label: "result".into(),
                    ptr: &root,
                    leaked: false,
                }],
            );

            let addresses = explorer
                .nodes
                .iter()
                .map(|node| node.addr.to_u64())
                .collect::<HashSet<_>>();
            assert_eq!(addresses.len(), explorer.nodes.len());
            assert!(explorer.nodes.iter().any(|node| node.summary == "App"));
            assert!(explorer.nodes.iter().any(|node| !node.incoming.is_empty()));
        });
    }
}
