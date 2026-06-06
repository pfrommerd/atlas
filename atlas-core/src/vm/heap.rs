//! The heap: term storage, node builders, and lowering from the desugared
//! [`Expr`] IR into packed heap [`Term`]s.
//!
//! Heap cells are packed 64-bit words ([`Node`]); their structured form is
//! [`Term`] (see [`crate::vm::term`]). `Node`s are never built with a raw
//! constructor here — they are always produced by packing a [`Term`].
//!
//! Evaluation (the interaction rules and normalization) lives separately in
//! [`crate::vm::exec::Executor`]; the `Heap` only owns storage.
//!
//! Variables resolve through *substitution cells*: a `Var`/`Dp0`/`Dp1` holds the
//! location of the slot that binds it. If that slot has the `SUB` bit set the
//! binder has already been consumed and the variable reads the substitution;
//! otherwise the variable is still free (the slot holds the null word `0`).

use crate::core::expr::{self, Expr, Pat};
use crate::vm::term::{Label, NameId, Node, NodePtr, PairPtr, Term, TriplePtr};

// --- leaf-term helpers (thin wrappers over `Term` packing) ---

pub(crate) fn var(slot: NodePtr) -> Node {
    Term::Var(slot).into()
}
pub(crate) fn dp0(label: u16, ptr: TriplePtr) -> Node {
    Term::Dp0 {
        label: Label(label),
        ptr,
    }
    .into()
}
pub(crate) fn dp1(label: u16, ptr: TriplePtr) -> Node {
    Term::Dp1 {
        label: Label(label),
        ptr,
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
    pub cases: Vec<(PatKey, Node)>,
    pub default: Option<Node>,
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

    /// Read a single cell.
    pub fn node(&self, p: NodePtr) -> Node {
        Node::from_raw(self.mem[p.0 as usize])
    }
    /// Read a two-cell binary node `(first, second)`.
    pub fn pair(&self, p: PairPtr) -> (Node, Node) {
        (self.node(p.first()), self.node(p.second()))
    }
    /// Read a three-cell duplication node `(value, sub0, sub1)`.
    pub fn triple(&self, p: TriplePtr) -> (Node, Node, Node) {
        (
            self.node(p.first()),
            self.node(p.second()),
            self.node(p.third()),
        )
    }
    pub fn set(&mut self, p: NodePtr, t: Node) {
        self.mem[p.0 as usize] = t.raw();
    }

    // --- node builders ---
    fn node2(&mut self, a: Node, b: Node) -> PairPtr {
        let p = PairPtr(self.alloc(2));
        self.set(p.first(), a);
        self.set(p.second(), b);
        p
    }
    /// A fresh duplication node holding `val`, with empty substitution slots.
    pub fn dup_node(&mut self, val: Node) -> TriplePtr {
        let p = TriplePtr(self.alloc(3));
        self.set(p.first(), val);
        self.set(p.second(), Node::NULL);
        self.set(p.third(), Node::NULL);
        p
    }

    pub fn app(&mut self, f: Node, a: Node) -> Node {
        Term::App(self.node2(f, a)).into()
    }
    pub fn lam(&mut self, body: Node) -> (Node, PairPtr) {
        // returns (lam term, lam node). The bound Var is `var(p.first())`.
        let p = self.node2(Node::NULL, body);
        (Term::Lam(p).into(), p)
    }
    pub fn sup(&mut self, label: u16, a: Node, b: Node) -> Node {
        let ptr = self.node2(a, b);
        Term::Sup {
            label: Label(label),
            ptr,
        }
        .into()
    }
    pub fn bop(&mut self, op: crate::vm::term::BinaryOp, l: Node, r: Node) -> Node {
        let ptr = self.node2(l, r);
        Term::Bop { op, ptr }.into()
    }
    pub fn num(&self, n: u64) -> Node {
        Term::Num(n).into()
    }
    pub fn wld(&self) -> Node {
        Term::Wld.into()
    }
    /// Build a constructor term from already-translated fields.
    pub fn ctr(&mut self, name_id: u16, fields: &[Node]) -> Node {
        let arity = fields.len();
        debug_assert!(arity < 16);
        if arity == 0 {
            return Term::Ctr {
                name: NameId(name_id),
                arity: 0,
                ptr: NodePtr(0),
            }
            .into();
        }
        let base = NodePtr(self.alloc(arity));
        for (i, f) in fields.iter().enumerate() {
            self.set(base.offset(i as u64), *f);
        }
        Term::Ctr {
            name: NameId(name_id),
            arity: arity as u8,
            ptr: base,
        }
        .into()
    }
    /// Build a duplication binder for `val` with the given label; returns the
    /// two projections `(Dp0, Dp1)`.
    pub fn dup(&mut self, label: u16, val: Node) -> (Node, Node) {
        let d = self.dup_node(val);
        (dp0(label, d), dp1(label, d))
    }

    // ====================================================================
    // Lowering: desugared `Expr` (de Bruijn) -> heap `Term`
    // ====================================================================

    /// Lower a desugared [`Expr`] into a heap term.
    pub fn lower(&mut self, expr: &Expr) -> Result<Node, String> {
        self.lower_rec(expr, &mut Vec::new())
    }

    fn lower_rec(&mut self, expr: &Expr, ctx: &mut Vec<Frame>) -> Result<Node, String> {
        match expr {
            Expr::Var(i) => match ctx[ctx.len() - 1 - i.0 as usize] {
                Frame::Lam(slot) => Ok(var(slot)),
                Frame::Dup(..) => Err("variable refers to a duplication binder".into()),
            },
            Expr::Dp0(i) => match ctx[ctx.len() - 1 - i.0 as usize] {
                Frame::Dup(d, lab) => Ok(dp0(lab, d)),
                Frame::Lam(_) => Err("dup projection refers to a lambda binder".into()),
            },
            Expr::Dp1(i) => match ctx[ctx.len() - 1 - i.0 as usize] {
                Frame::Dup(d, lab) => Ok(dp1(lab, d)),
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
                let (lam, p) = self.lam(Node::NULL);
                ctx.push(Frame::Lam(p.first()));
                let b = self.lower_rec(body, ctx);
                ctx.pop();
                self.set(p.second(), b?);
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
                Ok(Term::Mat(crate::vm::term::MatchId(idx)).into())
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
    /// a lambda binder slot (`Var` resolves to `var(slot)`)
    Lam(NodePtr),
    /// a duplication node + its (interned) label
    Dup(TriplePtr, u16),
}
