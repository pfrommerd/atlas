//! Readback / printing of a heap term back into surface-ish syntax.
//!
//! Traversal follows the safe borrowed [`HeapScope::view`] of each node, so
//! nothing is consumed and no affine pointer is forged outside the heap.
//! Variables are named by the address of their binder slot.

use crate::vm::heap::{Addr, HeapScope, PatKey, TermPtr};
use crate::vm::term::Term;
use crate::util::MemoMap;
use std::cell::Cell;
use std::fmt;

pub struct Printer<'a, 'h> {
    heap: &'a HeapScope<'h>,
    var_names: MemoMap<u64, String>,
    name_counter: Cell<usize>,
}

pub struct Pretty<'a, 'h> {
    printer: &'a Printer<'a, 'h>,
    root: &'a TermPtr<'h>,
}

impl fmt::Display for Pretty<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.printer.fmt_ptr(f, self.root, true)
    }
}

impl<'a, 'h> Printer<'a, 'h> {
    pub fn new(heap: &'a HeapScope<'h>) -> Self {
        Printer {
            heap,
            var_names: MemoMap::new(),
            name_counter: Cell::new(0),
        }
    }

    pub fn pretty(&'a self, root: &'a TermPtr<'h>) -> Pretty<'a, 'h> {
        Pretty { printer: self, root }
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
            Term::Ctr {
                name,
                arity,
                values,
            } => {
                let nm = self.heap.name(name);
                if nm == "Nil" && *arity == 0 {
                    return write!(f, "[]");
                }
                if *arity == 0 {
                    return write!(f, "#{nm}");
                }
                write!(f, "#{nm}{{")?;
                for i in 0..*arity as usize {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    self.fmt_term(f, self.heap.pack_addr(values, i), &self.heap.view_pack(values, i), false)?;
                }
                write!(f, "}}")
            }
            Term::U64(n) => write!(f, "{n}"),
            Term::I64(n) => write!(f, "{n}"),
            Term::F32(x) => write!(f, "{x}"),
            Term::F64(x) => write!(f, "{x}"),
            Term::Char(c) => write!(f, "{c:?}"),
            Term::Bool(b) => write!(f, "{b}"),
            Term::Sup { ptr, .. } => {
                let (la, ra) = self.heap.sup_addrs(ptr);
                write!(f, "&{{")?;
                self.fmt_term(f, la, &self.heap.view_sup(ptr, true), false)?;
                write!(f, ", ")?;
                self.fmt_term(f, ra, &self.heap.view_sup(ptr, false), false)?;
                write!(f, "}}")
            }
            Term::Dup { ptr, .. } => {
                // A `Dup` is one projection of a duplication. Both projections
                // denote copies of the same value, so read back the value being
                // duplicated; once fired, read this side's resolved slot.
                let (inner, view) = self.heap.view_dup(ptr);
                self.fmt_term(f, inner, &view, tail)
            }
            Term::Mat { matches, branches } => {
                let data = self.heap.match_data(matches);
                write!(f, "?{{")?;
                let mut first = true;
                for (key, idx) in &data.cases {
                    if !first {
                        write!(f, "; ")?;
                    }
                    first = false;
                    match key {
                        PatKey::Ctr(a) => {
                            let nm = self.heap.name_at(*a);
                            if nm == "Nil" {
                                write!(f, "[]")?;
                            } else {
                                write!(f, "{nm}")?;
                            }
                        }
                        PatKey::Num(n) => write!(f, "{n}")?,
                    }
                    write!(f, " => ")?;
                    self.fmt_term(f, self.heap.pack_addr(branches, *idx), &self.heap.view_pack(branches, *idx), true)?;
                }
                if let Some(idx) = data.default {
                    if !first {
                        write!(f, "; ")?;
                    }
                    write!(f, "_ => ")?;
                    self.fmt_term(f, self.heap.pack_addr(branches, idx), &self.heap.view_pack(branches, idx), true)?;
                }
                write!(f, "}}")
            }
            Term::Wld => write!(f, "*"),
            Term::Pri(id) => write!(f, "%{}", id.get()),
            _ => write!(f, "<?>"),
        }
    }
}
