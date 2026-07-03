//! Readback / printing of a heap term back into surface-ish syntax.
//!
//! Traversal follows the safe borrowed [`HeapScope::view`] of each node, so
//! nothing is consumed and no affine pointer is forged outside the heap.
//! Variables are named by the address of their binder slot.

use crate::core::printer::fmt_float;
use crate::util::MemoMap;
use crate::vm::heap::{Addr, Boxed, HeapScope, TermPtr, TypeInfo};
use crate::vm::term::Term;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fmt;

/// A hoisted duplication: one cell rendered as a single `&{left, right} = value`
/// binding. The two projections are named by `(cell, side)` via
/// [`Printer::dup_name`]; `value` is the duplicand node's address.
#[derive(Clone, Copy)]
struct DupBinding {
    cell: Addr,
    value: Addr,
}

pub struct Printer<'a, 'h> {
    heap: &'a HeapScope<'h>,
    var_names: MemoMap<u64, String>,
    name_counter: Cell<usize>,
    /// Dup cells discovered during readback, in dependency order (a cell's value
    /// dependencies precede it), emitted as `let` bindings before the body.
    ordered: RefCell<Vec<DupBinding>>,
    /// Cell addresses already collected, to bind each dup exactly once.
    seen: RefCell<HashSet<u64>>,
}

pub struct Pretty<'a, 'h> {
    printer: &'a Printer<'a, 'h>,
    root: &'a TermPtr<'h>,
}

impl fmt::Display for Pretty<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.printer;
        // Discover all (unfired) dup cells reachable from the root and hoist them
        // into `let` bindings, in dependency order, ahead of the body.
        p.ordered.borrow_mut().clear();
        p.seen.borrow_mut().clear();
        p.collect(self.root.addr(), &p.heap.view(self.root));

        let count = p.ordered.borrow().len();
        for i in 0..count {
            // `DupBinding` is `Copy`, so the borrow is released before `var_name` /
            // `fmt_term` (which never touch `ordered`/`seen`).
            let b = p.ordered.borrow()[i];
            write!(
                f,
                "&{{{}, {}}} = ",
                p.dup_name(b.cell, true),
                p.dup_name(b.cell, false)
            )?;
            p.fmt_term(f, b.value, &p.heap.view_at(b.value), true)?;
            writeln!(f, ";")?;
        }
        p.fmt_ptr(f, self.root, true)
    }
}

impl<'a, 'h> Printer<'a, 'h> {
    pub fn new(heap: &'a HeapScope<'h>) -> Self {
        Printer {
            heap,
            var_names: MemoMap::new(),
            name_counter: Cell::new(0),
            ordered: RefCell::new(Vec::new()),
            seen: RefCell::new(HashSet::new()),
        }
    }

    pub fn pretty(&'a self, root: &'a TermPtr<'h>) -> Pretty<'a, 'h> {
        Pretty {
            printer: self,
            root,
        }
    }

    fn fresh_name(&self) -> String {
        let n = self.name_counter.get();
        self.name_counter.set(n + 1);
        let letter = (b'a' + (n % 26) as u8) as char;
        if n < 26 {
            letter.to_string()
        } else {
            format!("{letter}{}", n / 26)
        }
    }

    fn var_name(&self, addr: Addr) -> &str {
        self.var_names
            .get_or_insert_with(addr.to_u64(), || self.fresh_name())
            .as_str()
    }

    /// Name a dup projection by its `(cell, side)` identity. The key is namespaced
    /// (high bit set) so it can never collide with a binder-slot address used by
    /// [`Self::var_name`].
    fn dup_name(&self, cell: Addr, side: bool) -> &str {
        let key = (1u64 << 62) | (cell.to_u64() << 1) | side as u64;
        self.var_names
            .get_or_insert_with(key, || self.fresh_name())
            .as_str()
    }

    /// Discover the dup cells reachable through `ptr` (see [`Self::collect`]).
    fn collect_ptr(&self, ptr: &TermPtr<'h>) {
        self.collect(ptr.addr(), &self.heap.view(ptr));
    }

    /// Walk a node, recording every (unfired) dup cell reachable from it into
    /// `self.ordered` in dependency order — a cell's value is walked before the
    /// cell is recorded, so any dup its value references is bound first. Mirrors
    /// the child structure of [`Self::fmt_term`].
    fn collect(&self, _addr: Addr, term: &Term<'h>) {
        match term {
            Term::Dup { ptr, .. } => {
                let (value, _, _) = self.heap.dup_peek(ptr);
                match value {
                    // Unfired: one shared duplicand -> hoist as a single binding.
                    Some(_) => {
                        if self.seen.borrow_mut().insert(ptr.addr().to_u64()) {
                            let (vaddr, vview) = self.heap.view_dup(ptr);
                            self.collect(vaddr, &vview);
                            self.ordered.borrow_mut().push(DupBinding {
                                cell: ptr.addr(),
                                value: vaddr,
                            });
                        }
                    }
                    // Fired: the two sides are independent resolved slots (inlined
                    // at print time); still walk this side for nested dups.
                    None => {
                        let (inner, view) = self.heap.view_dup(ptr);
                        self.collect(inner, &view);
                    }
                }
            }
            Term::Lam { body } => self.collect(body.body_addr(), &self.heap.view_body(body)),
            Term::Use { body } => self.collect_ptr(body),
            Term::App { func, arg } => {
                self.collect_ptr(func);
                self.collect_ptr(arg);
            }
            Term::Bop { lhs, rhs, .. } => {
                self.collect_ptr(lhs);
                self.collect_ptr(rhs);
            }
            Term::Uop { val, .. } => {
                self.collect_ptr(val);
            }
            Term::Ctn { arity, values, .. } => {
                for i in 0..*arity as usize {
                    self.collect(
                        self.heap.pack_addr(values, i),
                        &self.heap.view_pack(values, i),
                    );
                }
            }
            Term::Partial { func, args, .. } => {
                self.collect_ptr(func);
                for i in 0..self.heap.pack_len(args) {
                    self.collect(self.heap.pack_addr(args, i), &self.heap.view_pack(args, i));
                }
            }
            Term::Ctr { ty, .. } => self.collect_ptr(ty),
            Term::Type(t) => match self.heap.type_info(t) {
                // A type's (lazy, possibly-unevaluated) sub-type fields can still
                // hold unfired dups — walk them so those dups are hoisted, not
                // rendered as bare projection names.
                TypeInfo::Product { fields, .. } => {
                    for a in fields {
                        self.collect(*a, &self.heap.view_at(*a));
                    }
                }
                TypeInfo::Sum { variants, .. } => {
                    for v in variants {
                        for a in &v.args {
                            self.collect(*a, &self.heap.view_at(*a));
                        }
                    }
                }
            },
            Term::Sup { ptr, .. } => {
                // A collapse-marked sup reads back as its surviving component
                // only (the other is a tombstone).
                if let Some(side) = self.heap.sup_collapsed(ptr) {
                    let (la, ra) = self.heap.sup_addrs(ptr);
                    let live = if side { la } else { ra };
                    self.collect(live, &self.heap.view_sup(ptr, side));
                } else {
                    let (la, ra) = self.heap.sup_addrs(ptr);
                    self.collect(la, &self.heap.view_sup(ptr, true));
                    self.collect(ra, &self.heap.view_sup(ptr, false));
                }
            }
            Term::Mat { matches } => {
                let data = self.heap.match_data(matches);
                for (key_addr, branch_addr) in &data.cases {
                    self.collect(*key_addr, &self.heap.view_at(*key_addr));
                    self.collect(*branch_addr, &self.heap.view_at(*branch_addr));
                }
                if let Some(branch_addr) = data.default {
                    self.collect(branch_addr, &self.heap.view_at(branch_addr));
                }
            }
            _ => {}
        }
    }

    /// Print a match pattern key node: a `VarId` for a constructor (variant)
    /// pattern (with the `Nil` => `[]` shorthand), or a value leaf.
    fn fmt_key(&self, f: &mut fmt::Formatter<'_>, addr: Addr) -> fmt::Result {
        let view = self.heap.view_at(addr);
        if let Term::VarId(v) = &*view {
            let nm = self.heap.variant_name(*v);
            return if nm == "Nil" {
                write!(f, "[]")
            } else {
                write!(f, "{nm}")
            };
        }
        self.fmt_term(f, addr, &view, false)
    }

    /// Print the node named by `ptr`, read through the safe borrowed view.
    fn fmt_ptr(&self, f: &mut fmt::Formatter<'_>, ptr: &TermPtr<'h>, tail: bool) -> fmt::Result {
        self.fmt_term(f, ptr.addr(), &self.heap.view(ptr), tail)
    }

    /// Print an already-viewed node. `addr` is the node's own address (used to
    /// name a bare `Var`); `term` is its borrowed unpacked form.
    fn fmt_term(
        &self,
        f: &mut fmt::Formatter<'_>,
        addr: Addr,
        term: &Term<'h>,
        tail: bool,
    ) -> fmt::Result {
        match term {
            Term::Var => write!(f, "{}", self.var_name(addr)),
            Term::Lam { body } => {
                if !tail {
                    write!(f, "(")?;
                }
                write!(f, "\\{} -> ", self.var_name(body.binder_addr()))?;
                self.fmt_term(f, body.body_addr(), &self.heap.view_body(body), true)?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Term::Use { body } => {
                if !tail {
                    write!(f, "(")?;
                }
                write!(f, "\\_ -> ")?;
                self.fmt_ptr(f, body, true)?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Term::App { func, arg } => {
                write!(f, "(")?;
                self.fmt_ptr(f, func, false)?;
                write!(f, " ")?;
                self.fmt_ptr(f, arg, false)?;
                write!(f, ")")
            }
            Term::Bop { op, lhs, rhs } => {
                write!(f, "(")?;
                self.fmt_ptr(f, lhs, false)?;
                write!(f, " {} ", op.symbol())?;
                self.fmt_ptr(f, rhs, false)?;
                write!(f, ")")
            }
            Term::Uop { op, val } => {
                write!(f, "({}", op.symbol())?;
                self.fmt_ptr(f, val, false)?;
                write!(f, ")")
            }
            Term::Ctn { ty, arity, values } => {
                // Label by the variant name if there is one, else the type's name.
                let nm = match self.heap.pack_name(values) {
                    Some(v) => self.heap.variant_name(v).to_string(),
                    None => self
                        .heap
                        .type_name(ty.addr())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "<type>".to_string()),
                };
                if nm == "Nil" && *arity == 0 {
                    return write!(f, "[]");
                }
                if *arity == 0 {
                    return write!(f, "{nm}");
                }
                write!(f, "{nm}{{")?;
                for i in 0..*arity as usize {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    self.fmt_term(
                        f,
                        self.heap.pack_addr(values, i),
                        &self.heap.view_pack(values, i),
                        false,
                    )?;
                }
                write!(f, "}}")
            }
            Term::Int(n) => write!(f, "{n}"),
            Term::Float(x) => fmt_float(f, x.into_inner()),
            Term::Char(c) => write!(f, "{c:?}"),
            Term::Bool(b) => write!(f, "{b}"),
            Term::Box(v) => match self.heap.value_get(v) {
                Boxed::Str(s) => write!(f, "{s:?}"),
                Boxed::Bytes(b) => write!(f, "{b:?}"),
            },
            Term::Sup { ptr, .. } => {
                // A collapse-marked sup renders as its surviving component.
                if let Some(side) = self.heap.sup_collapsed(ptr) {
                    let (la, ra) = self.heap.sup_addrs(ptr);
                    let live = if side { la } else { ra };
                    return self.fmt_term(f, live, &self.heap.view_sup(ptr, side), false);
                }
                let (la, ra) = self.heap.sup_addrs(ptr);
                write!(f, "&{{")?;
                self.fmt_term(f, la, &self.heap.view_sup(ptr, true), false)?;
                write!(f, ", ")?;
                self.fmt_term(f, ra, &self.heap.view_sup(ptr, false), false)?;
                write!(f, "}}")
            }
            Term::Dup { ptr, .. } => {
                // A `Dup` is one projection of a duplication. While unfired, both
                // projections share one duplicand, hoisted into a top-level
                // `&{l, r} = value` binding (see `Pretty::fmt`); here we print just
                // this side's bound name. Once fired, the two sides are independent
                // resolved slots, so inline this side's slot.
                let (value, _, _) = self.heap.dup_peek(ptr);
                if value.is_some() {
                    write!(f, "{}", self.dup_name(ptr.addr(), ptr.side()))
                } else {
                    let (inner, view) = self.heap.view_dup(ptr);
                    self.fmt_term(f, inner, &view, tail)
                }
            }
            Term::Mat { matches } => {
                let data = self.heap.match_data(matches);
                write!(f, "?{{")?;
                let mut first = true;
                for (key_addr, branch_addr) in &data.cases {
                    if !first {
                        write!(f, "; ")?;
                    }
                    first = false;
                    self.fmt_key(f, *key_addr)?;
                    write!(f, " => ")?;
                    self.fmt_term(f, *branch_addr, &self.heap.view_at(*branch_addr), true)?;
                }
                if let Some(branch_addr) = data.default {
                    if !first {
                        write!(f, "; ")?;
                    }
                    write!(f, "_ => ")?;
                    self.fmt_term(f, branch_addr, &self.heap.view_at(branch_addr), true)?;
                }
                write!(f, "}}")
            }
            Term::Wld => write!(f, "*"),
            Term::Err { .. } => write!(f, "<err>"),
            Term::Pri(id) => write!(f, "%{}", id.get()),
            Term::Type(t) => {
                if let Some(n) = self.heap.type_name(t.addr()) {
                    return write!(f, "{n}");
                }
                // Anonymous type: render its shape. Sub-types are lazy, so a field
                // prints as its (unevaluated) term form.
                match self.heap.type_info(t) {
                    TypeInfo::Sum { variants, .. } => {
                        write!(f, "type{{")?;
                        for (i, v) in variants.iter().enumerate() {
                            if i > 0 {
                                write!(f, ", ")?;
                            }
                            write!(f, "{}", self.heap.variant_name(v.name))?;
                            if !v.args.is_empty() {
                                write!(f, "(")?;
                                for (j, a) in v.args.iter().enumerate() {
                                    if j > 0 {
                                        write!(f, ", ")?;
                                    }
                                    self.fmt_term(f, *a, &self.heap.view_at(*a), false)?;
                                }
                                write!(f, ")")?;
                            }
                        }
                        write!(f, "}}")
                    }
                    TypeInfo::Product { fields, .. } => {
                        write!(f, "type(")?;
                        for (i, a) in fields.iter().enumerate() {
                            if i > 0 {
                                write!(f, ", ")?;
                            }
                            self.fmt_term(f, *a, &self.heap.view_at(*a), false)?;
                        }
                        write!(f, ")")
                    }
                }
            }
            Term::VarId(v) => write!(f, "{}", self.heap.variant_name(*v)),
            // A constructor selector value: print its variant name, or `New` for
            // the product constructor.
            Term::Ctr { variant, .. } => match variant {
                Some(name) => write!(f, "{}", self.heap.variant_name(*name)),
                None => write!(f, "New"),
            },
            // A partial application: the callable followed by its args in sequence.
            Term::Partial { func, args, .. } => {
                let n = self.heap.pack_len(args);
                if n == 0 {
                    return self.fmt_ptr(f, func, false);
                }
                write!(f, "(")?;
                self.fmt_ptr(f, func, false)?;
                for i in 0..n {
                    write!(f, " ")?;
                    self.fmt_term(
                        f,
                        self.heap.pack_addr(args, i),
                        &self.heap.view_pack(args, i),
                        false,
                    )?;
                }
                write!(f, ")")
            }
            _ => write!(f, "<?>"),
        }
    }
}
