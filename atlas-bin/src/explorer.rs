//! The heap explorer model: a lazily-expanded tree over the live term graph,
//! rooted at the session's external pointers (last result, pending eval,
//! locals) plus — in leak view — the forged roots of unreachable subgraphs.

use std::collections::HashSet;

use atlas_core::vm::heap::{Addr, ArenaKind, HeapScope, TermPtr, TypeInfo};
use atlas_core::vm::printer::Printer;
use atlas_core::vm::term::Term;

/// A top-level tree heading: a name plus the live pointer it borrows.
pub struct RootEntry<'a, 'h> {
    pub label: String,
    pub ptr: &'a TermPtr<'h>,
    pub leaked: bool,
}

/// One flattened tree row.
pub struct Row {
    pub depth: usize,
    /// Root heading or the edge name leading to this node (`func`, `arg`, …).
    pub label: String,
    pub addr: Addr,
    pub summary: String,
    pub expandable: bool,
    pub expanded: bool,
    pub leaked: bool,
}

pub struct ExplorerState {
    /// Full-dump mode: scan for and show leaked (unreachable) subgraphs.
    pub show_leaked: bool,
    expanded: HashSet<u64>,
    pub selected: usize,
    pub rows: Vec<Row>,
    pub stats: Vec<(ArenaKind, usize)>,
}

impl ExplorerState {
    pub fn new() -> Self {
        ExplorerState {
            show_leaked: false,
            expanded: HashSet::new(),
            selected: 0,
            rows: Vec::new(),
            stats: Vec::new(),
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.rows.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    /// Expand/collapse the selected row. Returns true if the tree changed (the
    /// caller should rebuild).
    pub fn toggle_selected(&mut self) -> bool {
        let Some(row) = self.rows.get(self.selected) else {
            return false;
        };
        if !row.expandable {
            return false;
        }
        let key = row.addr.to_u64();
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        true
    }

    /// Rebuild the flattened rows from `roots` and refresh the arena stats.
    pub fn rebuild<'h>(&mut self, h: &'h HeapScope<'h>, roots: &[RootEntry<'_, 'h>]) {
        self.stats = ArenaKind::ALL
            .iter()
            .map(|&kind| (kind, h.arena_len(kind)))
            .collect();
        let mut rows = Vec::new();
        let mut path = Vec::new();
        for root in roots {
            if root.ptr.is_null() {
                continue;
            }
            let printer = Printer::new(h);
            let summary = truncate(printer.pretty(root.ptr).to_string());
            push_tree(
                h,
                &mut rows,
                &self.expanded,
                &mut path,
                0,
                root.label.clone(),
                root.ptr.addr(),
                Some(summary),
                root.leaked,
            );
        }
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }
}

#[allow(clippy::too_many_arguments)]
fn push_tree<'h>(
    h: &'h HeapScope<'h>,
    rows: &mut Vec<Row>,
    expanded: &HashSet<u64>,
    path: &mut Vec<u64>,
    depth: usize,
    label: String,
    addr: Addr,
    summary: Option<String>,
    leaked: bool,
) {
    let key = addr.to_u64();
    // A shared cell (dup) can lead back into the current path; cut the cycle.
    if path.contains(&key) {
        rows.push(Row {
            depth,
            label,
            addr,
            summary: "(cycle)".to_string(),
            expandable: false,
            expanded: false,
            leaked,
        });
        return;
    }
    let children = node_children(h, addr);
    let is_expanded = expanded.contains(&key) && !children.is_empty();
    rows.push(Row {
        depth,
        label,
        addr,
        summary: summary.unwrap_or_else(|| summarize(h, addr)),
        expandable: !children.is_empty(),
        expanded: is_expanded,
        leaked,
    });
    if is_expanded {
        path.push(key);
        for (edge, child) in children {
            push_tree(
                h,
                rows,
                expanded,
                path,
                depth + 1,
                edge,
                child,
                None,
                leaked,
            );
        }
        path.pop();
    }
}

/// The labeled node-arena children of the node at `addr` (the explorer's
/// one-step traversal; readback-only).
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
            let (l, r) = h.sup_addrs(ptr);
            vec![("left".into(), l), ("right".into(), r)]
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
            .map(|(i, &a)| (format!("ty[{i}]"), a))
            .collect(),
        TypeInfo::Sum { variants, .. } => variants
            .iter()
            .flat_map(|v| {
                let name = h.variant_name(v.name).to_string();
                v.args
                    .iter()
                    .enumerate()
                    .map(move |(i, &a)| (format!("ty.{name}[{i}]"), a))
                    .collect::<Vec<_>>()
            })
            .collect(),
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
        Term::Dup { ptr, .. } => {
            let value = h.dup_peek(ptr);
            let state = if value.is_some() { "pending" } else { "fired" };
            format!(
                "Dup ({}, {state})",
                if ptr.side() { "left" } else { "right" }
            )
        }
        Term::Sup { .. } => "Sup".into(),
        Term::Ctn { ty, values, .. } => {
            let ty_name = h
                .type_name(ty.addr())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "type".into());
            match h.pack_name(values) {
                Some(v) => format!("Ctn {ty_name}::{}", h.variant_name(v)),
                None => format!("Ctn {ty_name}"),
            }
        }
        Term::Partial { arity, args, .. } => {
            format!("Partial ({}/{arity} args)", h.pack_len(args))
        }
        Term::Ctr { variant, .. } => match variant {
            Some(v) => format!("Ctr ::{}", h.variant_name(*v)),
            None => "Ctr ::New".into(),
        },
        Term::VarId(v) => format!("VarId {}", h.variant_name(*v)),
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
        Term::Int(n) => format!("Int {n}"),
        Term::Float(f) => format!("Float {f}"),
        Term::Char(c) => format!("Char {c:?}"),
        Term::Bool(b) => format!("Bool {b}"),
        Term::Box(v) => match h.value_get(v) {
            atlas_core::vm::heap::Boxed::Str(s) => format!("Box {:?}", truncate(s.to_string())),
            atlas_core::vm::heap::Boxed::Bytes(b) => format!("Box [{} bytes]", b.len()),
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
