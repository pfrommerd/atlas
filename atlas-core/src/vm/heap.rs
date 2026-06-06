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
use crate::vm::term::{
    Arity, BinaryOp, DupPtr, Label, NameId, Node, NodePtr, PairPtr, Term, TriplePtr,
};
use std::borrow::Cow;
use std::collections::HashMap;

// --- leaf-term helpers (thin wrappers over `Term` packing) ---

pub(crate) fn var(slot: NodePtr) -> Node {
    Term::Var(slot).into()
}
pub(crate) fn dp0(ptr: DupPtr) -> Node {
    Term::Dp0(ptr).into()
}
pub(crate) fn dp1(ptr: DupPtr) -> Node {
    Term::Dp1(ptr).into()
}

/// A compiled pattern key for a match arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatKey {
    Ctr(NameId),
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
    /// Interned strings (constructors and labels share this table).
    interned: Vec<String>,
    interned_ids: HashMap<String, usize>,
    label_counter: usize, // Counts "fresh" labels
    name_counter: usize,  // Counts "fresh" names
    /// Match tables, referenced by a `Mat` term's VAL.
    pub matches: Vec<MatchData>,
}

impl Heap {
    pub fn new() -> Self {
        // Cell 0 is reserved as the null sentinel.
        Heap {
            mem: vec![0],
            interned: Vec::new(),
            interned_ids: HashMap::new(),
            label_counter: 0,
            name_counter: 0,
            matches: Vec::new(),
        }
    }

    pub fn intern_name(&mut self, name: &str) -> NameId {
        if let Some(id) = self.interned_ids.get(name) {
            return NameId(*id as u64);
        }
        let id = self.interned.len();
        self.interned.push(name.to_string());
        self.interned_ids.insert(name.to_string(), id);
        NameId(id as u64)
    }

    pub fn intern_label(&mut self, name: &str) -> Label {
        if let Some(id) = self.interned_ids.get(name) {
            return Label(*id as u64);
        }
        let id = self.interned.len();
        self.interned.push(name.to_string());
        self.interned_ids.insert(name.to_string(), id);
        Label(id as u64)
    }

    /// A fresh, unique label. Has the highest
    /// bit set to signify that this is a dynamic label
    pub fn fresh_label(&mut self) -> Label {
        let n = self.label_counter | (1 << 55);
        self.label_counter += 1;
        Label(n as u64)
    }
    pub fn fresh_name(&mut self) -> NameId {
        let n = self.name_counter | (1 << 55);
        self.name_counter += 1;
        NameId(n as u64)
    }
    pub fn name(&self, id: NameId) -> Cow<'_, str> {
        if id.0 & (1 << 55) != 0 {
            let base = id.0 & !(1 << 55);
            Cow::Owned(format!("{}", base))
        } else {
            Cow::Borrowed(&self.interned[id.0 as usize])
        }
    }
    pub fn label(&self, label: Label) -> Cow<'_, str> {
        if label.0 & (1 << 55) != 0 {
            let base = label.0 & !(1 << 55);
            Cow::Owned(format!("{}", base))
        } else {
            Cow::Borrowed(&self.interned[label.0 as usize])
        }
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
    /// The duplication/superposition label stored in a sup triple's leading cell.
    pub fn sup_label(&self, p: TriplePtr) -> Label {
        self.node(p.first()).as_label()
    }
    /// A superposition's two operands `(left, right)`.
    pub fn sup_args(&self, p: TriplePtr) -> (Node, Node) {
        (self.node(p.second()), self.node(p.third()))
    }
    /// The duplication label stored in a dup quad's leading cell.
    pub fn dup_label(&self, q: DupPtr) -> Label {
        self.node(q.label()).as_label()
    }
    /// A constructor allocation's `(name, arity)`, read from its leading cells.
    pub fn ctr_head(&self, base: NodePtr) -> (NameId, Arity) {
        (
            self.node(base).as_name(),
            self.node(base.offset(1)).as_arity(),
        )
    }
    /// The location of constructor field `i` (fields follow the two meta-cells).
    pub fn ctr_field(&self, base: NodePtr, i: u64) -> NodePtr {
        base.offset(2 + i)
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
    /// A fresh duplication node `[Label, val, sub0, sub1]` holding `val`, with
    /// empty substitution slots.
    pub fn dup_node(&mut self, label: Label, val: Node) -> DupPtr {
        let q = DupPtr(self.alloc(4));
        self.set(q.label(), Term::LabelMeta(label).into());
        self.set(q.val(), val);
        self.set(q.sub0(), Node::NULL);
        self.set(q.sub1(), Node::NULL);
        q
    }

    pub fn app(&mut self, f: Node, a: Node) -> Node {
        Term::App(self.node2(f, a)).into()
    }
    pub fn lam(&mut self, body: Node) -> (Node, PairPtr) {
        // returns (lam term, lam node). The bound Var is `var(p.first())`.
        let p = self.node2(Node::NULL, body);
        (Term::Lam(p).into(), p)
    }
    pub fn sup(&mut self, label: Label, a: Node, b: Node) -> Node {
        let p = TriplePtr(self.alloc(3));
        self.set(p.first(), Term::LabelMeta(label).into());
        self.set(p.second(), a);
        self.set(p.third(), b);
        Term::Sup(p).into()
    }
    pub fn bop(&mut self, op: BinaryOp, l: Node, r: Node) -> Node {
        let p = TriplePtr(self.alloc(3));
        self.set(p.first(), Term::OpMeta(op).into());
        self.set(p.second(), l);
        self.set(p.third(), r);
        Term::Bop(p).into()
    }
    pub fn num(&self, n: u64) -> Node {
        Term::Num(n).into()
    }
    pub fn wld(&self) -> Node {
        Term::Wld.into()
    }
    /// Build a constructor term from already-translated fields. The allocation
    /// is `[Label, Arity, fields..]`.
    pub fn ctr(&mut self, name_id: NameId, fields: &[Node]) -> Node {
        let arity = fields.len();
        let base = NodePtr(self.alloc(2 + arity));
        self.set(base, Term::NameMeta(name_id).into());
        self.set(base.offset(1), Term::ArityMeta(Arity(arity as u64)).into());
        for (i, f) in fields.iter().enumerate() {
            self.set(self.ctr_field(base, i as u64), *f);
        }
        Term::Ctr(base).into()
    }
    /// Build a duplication binder for `val` with the given label; returns the
    /// two projections `(Dp0, Dp1)`.
    pub fn dup(&mut self, label: Label, val: Node) -> (Node, Node) {
        let d = self.dup_node(label, val);
        (dp0(d), dp1(d))
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
                Frame::Dup(d) => Ok(dp0(d)),
                Frame::Lam(_) => Err("dup projection refers to a lambda binder".into()),
            },
            Expr::Dp1(i) => match ctx[ctx.len() - 1 - i.0 as usize] {
                Frame::Dup(d) => Ok(dp1(d)),
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
                let d = self.dup_node(lab, v);
                ctx.push(Frame::Dup(d));
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
                let id = self.intern_name(name);
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

    fn lower_label(&mut self, label: &expr::Label) -> Label {
        match label {
            expr::Label::Named(s) => self.intern_label(s),
            expr::Label::Auto(n) => self.fresh_label(),
        }
    }

    fn lower_pat(&mut self, pat: &Pat) -> PatKey {
        match pat {
            Pat::Ctr(name) => PatKey::Ctr(self.intern_name(name)),
            Pat::Num(n) => PatKey::Num(*n),
        }
    }
}

/// A binder currently in scope while lowering, indexed by de Bruijn level.
enum Frame {
    /// a lambda binder slot (`Var` resolves to `var(slot)`)
    Lam(NodePtr),
    /// a duplication node (its label lives in the node's leading cell)
    Dup(DupPtr),
}
