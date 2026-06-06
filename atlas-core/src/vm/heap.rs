//! The heap: term storage, node builders, and lowering from the desugared
//! [`Expr`] IR into packed heap [`Term`]s.
//!
//! Terms are packed 64-bit words ([`Term`]); their structured form is
//! [`TermValue`] (see [`crate::vm::term`]). `Term`s are never built with a raw
//! constructor here — they are always produced by packing a [`TermValue`].
//!
//! Evaluation (the interaction rules and normalization) lives separately in
//! [`crate::vm::exec::Executor`]; the `Heap` only owns storage.
//!
//! `VAL` is usually a *location* into the flat `mem` array. A binary node
//! occupies two consecutive cells (`loc`, `loc+1`); a duplication node uses
//! three (`val`, `sub0`, `sub1`).
//!
//! Variables resolve through *substitution cells*: a `Var`/`Dp0`/`Dp1` holds the
//! location of the slot that binds it. If that slot has the `SUB` bit set the
//! binder has already been consumed and the variable reads the substitution;
//! otherwise the variable is still free (the slot holds the null word `0`).

use crate::core::expr::{self, Expr, Pat};
use crate::vm::term::{Label, NameId, Term, TermPtr, View};

// --- leaf-term helpers (thin wrappers over `TermValue` packing) ---

pub(crate) fn var(loc: u64) -> Term {
    View::Var(TermPtr(loc)).into()
}
pub(crate) fn dp0(label: u16, ptr: u64) -> Term {
    View::Dp0 {
        label: Label(label),
        ptr: TermPtr(ptr),
    }
    .into()
}
pub(crate) fn dp1(label: u16, ptr: u64) -> Term {
    View::Dp1 {
        label: Label(label),
        ptr: TermPtr(ptr),
    }
    .into()
}

/// A compiled pattern key for a match arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatKey {
    Ctr(u16),
    Num(u64),
}

/// A compiled match (`?{ ... }`). Branch terms are heap roots.
#[derive(Debug, Clone)]
pub struct MatchData {
    pub cases: Vec<(PatKey, Term)>,
    pub default: Option<Term>,
}

// --- Heap ---

pub struct Heap {
    pub mem: Vec<u64>,
    /// Interned names (constructors and labels share this table).
    names: Vec<String>,
    name_ids: std::collections::HashMap<String, u16>,
    /// Match tables, referenced by a `Mat` term's VAL.
    pub matches: Vec<MatchData>,
    /// Counter for synthesised (auto-dup) labels.
    label_ctr: u32,
}

impl Heap {
    pub fn new() -> Self {
        // Cell 0 is reserved as the null sentinel.
        Heap {
            mem: vec![0],
            names: Vec::new(),
            name_ids: std::collections::HashMap::new(),
            matches: Vec::new(),
            label_ctr: 0,
        }
    }

    /// A fresh, unique label (interned under a synthetic name).
    pub fn fresh_label(&mut self) -> u16 {
        let n = self.label_ctr;
        self.label_ctr += 1;
        self.intern(&format!("%{}", n))
    }

    pub fn intern(&mut self, name: &str) -> u16 {
        if let Some(id) = self.name_ids.get(name) {
            return *id;
        }
        let id = self.names.len() as u16;
        self.names.push(name.to_string());
        self.name_ids.insert(name.to_string(), id);
        id
    }
    pub fn name(&self, id: u16) -> &str {
        &self.names[id as usize]
    }

    pub fn push_match(&mut self, data: MatchData) -> u64 {
        let idx = self.matches.len() as u64;
        self.matches.push(data);
        idx
    }

    /// Allocate `n` consecutive cells, returning the location of the first.
    pub fn alloc(&mut self, n: usize) -> u64 {
        let loc = self.mem.len() as u64;
        for _ in 0..n {
            self.mem.push(0);
        }
        loc
    }

    pub fn get(&self, loc: u64) -> Term {
        Term::from_raw(self.mem[loc as usize])
    }
    pub fn set(&mut self, loc: u64, t: Term) {
        self.mem[loc as usize] = t.raw();
    }

    // --- node builders ---
    fn node2(&mut self, a: Term, b: Term) -> u64 {
        let loc = self.alloc(2);
        self.set(loc, a);
        self.set(loc + 1, b);
        loc
    }
    /// A fresh duplication node holding `val`, with empty substitution slots.
    pub fn dup_node(&mut self, val: Term) -> u64 {
        let loc = self.alloc(3);
        self.set(loc, val);
        self.set(loc + 1, Term::NULL);
        self.set(loc + 2, Term::NULL);
        loc
    }

    pub fn app(&mut self, f: Term, a: Term) -> Term {
        let loc = self.node2(f, a);
        View::App(TermPtr(loc)).into()
    }
    pub fn lam(&mut self, body: Term) -> (Term, u64) {
        // returns (lam term, binder location). The bound Var is Var(loc).
        let loc = self.node2(Term::NULL, body);
        (View::Lam(TermPtr(loc)).into(), loc)
    }
    pub fn sup(&mut self, label: u16, a: Term, b: Term) -> Term {
        let loc = self.node2(a, b);
        View::Sup {
            label: Label(label),
            ptr: TermPtr(loc),
        }
        .into()
    }
    pub fn bop(&mut self, op: crate::vm::term::BinaryOp, l: Term, r: Term) -> Term {
        let loc = self.node2(l, r);
        View::Bop {
            op,
            ptr: TermPtr(loc),
        }
        .into()
    }
    pub fn num(&self, n: u64) -> Term {
        View::Num(n).into()
    }
    pub fn wld(&self) -> Term {
        View::Wld.into()
    }
    /// Build a constructor term from already-translated fields.
    pub fn ctr(&mut self, name_id: u16, fields: &[Term]) -> Term {
        let arity = fields.len();
        debug_assert!(arity < 16);
        if arity == 0 {
            return View::Ctr {
                name: NameId(name_id),
                arity: 0,
                ptr: TermPtr(0),
            }
            .into();
        }
        let loc = self.alloc(arity);
        for (i, f) in fields.iter().enumerate() {
            self.set(loc + i as u64, *f);
        }
        View::Ctr {
            name: NameId(name_id),
            arity: arity as u8,
            ptr: TermPtr(loc),
        }
        .into()
    }
    /// Build a duplication binder for `val` with the given label; returns the
    /// two projections `(Dp0, Dp1)`.
    pub fn dup(&mut self, label: u16, val: Term) -> (Term, Term) {
        let d = self.dup_node(val);
        (dp0(label, d), dp1(label, d))
    }

    // ====================================================================
    // Lowering: desugared `Expr` (de Bruijn) -> heap `Term`
    // ====================================================================

    /// Lower a desugared [`Expr`] into a heap term.
    pub fn lower(&mut self, expr: &Expr) -> Result<Term, String> {
        self.lower_rec(expr, &mut Vec::new())
    }

    fn lower_rec(&mut self, expr: &Expr, ctx: &mut Vec<Frame>) -> Result<Term, String> {
        match expr {
            Expr::Var(i) => match ctx[ctx.len() - 1 - i.0 as usize] {
                Frame::Lam(loc) => Ok(var(loc)),
                Frame::Dup(..) => Err("variable refers to a duplication binder".into()),
            },
            Expr::Dp0(i) => match ctx[ctx.len() - 1 - i.0 as usize] {
                Frame::Dup(loc, lab) => Ok(dp0(lab, loc)),
                Frame::Lam(_) => Err("dup projection refers to a lambda binder".into()),
            },
            Expr::Dp1(i) => match ctx[ctx.len() - 1 - i.0 as usize] {
                Frame::Dup(loc, lab) => Ok(dp1(lab, loc)),
                Frame::Lam(_) => Err("dup projection refers to a lambda binder".into()),
            },
            Expr::Era | Expr::Wld => Ok(self.wld()),
            Expr::Num(n) => Ok(self.num(*n)),
            Expr::Ref(name) => Err(format!("references (@{name}) are not supported yet")),
            Expr::Pri(name) => Err(format!("primitives (%{name}) are not supported yet")),
            Expr::Sup { label, left, right } => {
                let lab = self.lower_label(label);
                let a = self.lower_rec(left, ctx)?;
                let b = self.lower_rec(right, ctx)?;
                Ok(self.sup(lab, a, b))
            }
            Expr::Dup { label, val, body } => {
                let v = self.lower_rec(val, ctx)?;
                let lab = self.lower_label(label);
                let d = self.dup_node(v);
                ctx.push(Frame::Dup(d, lab));
                let b = self.lower_rec(body, ctx);
                ctx.pop();
                b
            }
            Expr::Lam { body } => {
                let (lam, loc) = self.lam(Term::NULL);
                ctx.push(Frame::Lam(loc));
                let b = self.lower_rec(body, ctx);
                ctx.pop();
                self.set(loc + 1, b?);
                Ok(lam)
            }
            Expr::App { func, arg } => {
                let f = self.lower_rec(func, ctx)?;
                let x = self.lower_rec(arg, ctx)?;
                Ok(self.app(f, x))
            }
            Expr::Ctr { name, args } => {
                let id = self.intern(name);
                let mut fields = Vec::with_capacity(args.len());
                for a in args {
                    fields.push(self.lower_rec(a, ctx)?);
                }
                Ok(self.ctr(id, &fields))
            }
            Expr::Op2 { op, left, right } => {
                let l = self.lower_rec(left, ctx)?;
                let r = self.lower_rec(right, ctx)?;
                Ok(self.bop(*op, l, r))
            }
            Expr::Mat { cases, default } => {
                let mut compiled = Vec::with_capacity(cases.len());
                for (pat, body) in cases {
                    let key = self.lower_pat(pat);
                    let t = self.lower_rec(body, ctx)?;
                    compiled.push((key, t));
                }
                let default = match default {
                    Some(d) => Some(self.lower_rec(d, ctx)?),
                    None => None,
                };
                let idx = self.push_match(MatchData {
                    cases: compiled,
                    default,
                });
                Ok(View::Mat(crate::vm::term::MatchId(idx)).into())
            }
        }
    }

    fn lower_label(&mut self, label: &expr::Label) -> u16 {
        match label {
            expr::Label::Named(s) => self.intern(s),
            expr::Label::Auto(n) => self.intern(&format!("%{n}")),
        }
    }

    fn lower_pat(&mut self, pat: &Pat) -> PatKey {
        match pat {
            Pat::Ctr(name) => PatKey::Ctr(self.intern(name)),
            Pat::Num(n) => PatKey::Num(*n),
        }
    }
}

/// A binder currently in scope while lowering, indexed by de Bruijn level.
enum Frame {
    /// a lambda binder slot (`Var` resolves to `Var(loc)`)
    Lam(u64),
    /// a duplication node + its (interned) label
    Dup(u64, u16),
}
