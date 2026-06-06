//! The heap and the interaction rules for the (Symmetric) Interaction Calculus
//! VM.
//!
//! Terms are packed 64-bit words ([`Term`]); their structured form is
//! [`TermValue`] (see [`crate::vm::term`]). `Term`s are never built with a raw
//! constructor here — they are always produced by packing a [`TermValue`].
//!
//! `VAL` is usually a *location* into the flat `mem` array. A binary node
//! occupies two consecutive cells (`loc`, `loc+1`); a duplication node uses
//! three (`val`, `sub0`, `sub1`).
//!
//! Variables resolve through *substitution cells*: a `Var`/`Dp0`/`Dp1` holds the
//! location of the slot that binds it. If that slot has the `SUB` bit set the
//! binder has already been consumed and the variable reads the substitution;
//! otherwise the variable is still free (the slot holds the null word `0`).

use crate::vm::term::{Label, NameId, Tag, Term, TermPtr, TermValue};

// --- leaf-term helpers (thin wrappers over `TermValue` packing) ---

fn var(loc: u64) -> Term {
    TermValue::Var(TermPtr(loc)).into()
}
fn dp0(label: u16, ptr: u64) -> Term {
    TermValue::Dp0 {
        label: Label(label),
        ptr: TermPtr(ptr),
    }
    .into()
}
fn dp1(label: u16, ptr: u64) -> Term {
    TermValue::Dp1 {
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
    /// Number of interactions performed (for stats / step limiting).
    pub itrs: u64,
    /// Interaction budget; reduction stops once `itrs` reaches it.
    pub budget: u64,
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
            itrs: 0,
            budget: u64::MAX,
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
        TermValue::App(TermPtr(loc)).into()
    }
    pub fn lam(&mut self, body: Term) -> (Term, u64) {
        // returns (lam term, binder location). The bound Var is Var(loc).
        let loc = self.node2(Term::NULL, body);
        (TermValue::Lam(TermPtr(loc)).into(), loc)
    }
    pub fn sup(&mut self, label: u16, a: Term, b: Term) -> Term {
        let loc = self.node2(a, b);
        TermValue::Sup {
            label: Label(label),
            ptr: TermPtr(loc),
        }
        .into()
    }
    pub fn bop(&mut self, op: Op, l: Term, r: Term) -> Term {
        let loc = self.node2(l, r);
        TermValue::Bop {
            op,
            ptr: TermPtr(loc),
        }
        .into()
    }
    pub fn num(&self, n: u64) -> Term {
        TermValue::Num(n).into()
    }
    pub fn wld(&self) -> Term {
        TermValue::Wld.into()
    }
    /// Build a constructor term from already-translated fields.
    pub fn ctr(&mut self, name_id: u16, fields: &[Term]) -> Term {
        let arity = fields.len();
        debug_assert!(arity < 16);
        if arity == 0 {
            return TermValue::Ctr {
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
        TermValue::Ctr {
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
    // Interactions
    // ====================================================================

    /// APP-LAM: `(λx.body) arg`  =>  `x ← arg; body`
    fn app_lam(&mut self, app: Term, lam: Term) -> Term {
        self.itrs += 1;
        let a = app.val();
        let l = lam.val();
        let arg = self.get(a + 1);
        // substitute the lambda's variable
        self.set(l, arg.with_sub());
        self.get(l + 1)
    }

    /// APP-SUP: `(&L{f,g}) arg`  =>  `!a&L=arg; &L{(f a₀),(g a₁)}`
    fn app_sup(&mut self, app: Term, sup: Term) -> Term {
        self.itrs += 1;
        let a = app.val();
        let s = sup.val();
        let lab = sup.ext();
        let arg = self.get(a + 1);
        let f = self.get(s);
        let g = self.get(s + 1);
        let d = self.dup_node(arg);
        let fa = self.app(f, dp0(lab, d));
        let gb = self.app(g, dp1(lab, d));
        self.sup(lab, fa, gb)
    }

    /// DUP-SUP. Same label annihilates; different labels commute.
    fn dup_sup(&mut self, dp: Term, sup: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        let l = dp.ext();
        let s = sup.val();
        let r = sup.ext();
        let a = self.get(s);
        let b = self.get(s + 1);
        if l == r {
            self.set(d + 1, a.with_sub());
            self.set(d + 2, b.with_sub());
            if dp.tag() == Tag::Dp0 { a } else { b }
        } else {
            let da = self.dup_node(a);
            let db = self.dup_node(b);
            let s0 = self.sup(r, dp0(l, da), dp0(l, db));
            let s1 = self.sup(r, dp1(l, da), dp1(l, db));
            self.set(d + 1, s0.with_sub());
            self.set(d + 2, s1.with_sub());
            if dp.tag() == Tag::Dp0 { s0 } else { s1 }
        }
    }

    /// DUP-LAM: duplicating a lambda yields two lambdas with a superposed var.
    fn dup_lam(&mut self, dp: Term, lam: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        let l = dp.ext();
        let lloc = lam.val();
        let body = self.get(lloc + 1);
        let dg = self.dup_node(body);
        let (lam0, l0) = self.lam(dp0(l, dg));
        let (lam1, l1) = self.lam(dp1(l, dg));
        // x ← &L{$x0, $x1}
        let sup_var = self.sup(l, var(l0), var(l1));
        self.set(lloc, sup_var.with_sub());
        self.set(d + 1, lam0.with_sub());
        self.set(d + 2, lam1.with_sub());
        if dp.tag() == Tag::Dp0 { lam0 } else { lam1 }
    }

    /// DUP-NUM: numbers are duplicated trivially.
    fn dup_num(&mut self, dp: Term, num: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        self.set(d + 1, num.with_sub());
        self.set(d + 2, num.with_sub());
        num
    }

    /// DUP-CTR: duplicate a constructor field by field.
    fn dup_ctr(&mut self, dp: Term, ctr: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        let l = dp.ext();
        let (name, arity, c) = match ctr.unpack() {
            TermValue::Ctr { name, arity, ptr } => (name, arity as usize, ptr.0),
            _ => unreachable!("dup_ctr on non-constructor"),
        };
        let c0 = self.alloc(arity);
        let c1 = self.alloc(arity);
        for i in 0..arity {
            let field = self.get(c + i as u64);
            let di = self.dup_node(field);
            self.set(c0 + i as u64, dp0(l, di));
            self.set(c1 + i as u64, dp1(l, di));
        }
        let ctr0 = TermValue::Ctr {
            name,
            arity: arity as u8,
            ptr: TermPtr(if arity == 0 { 0 } else { c0 }),
        }
        .into();
        let ctr1 = TermValue::Ctr {
            name,
            arity: arity as u8,
            ptr: TermPtr(if arity == 0 { 0 } else { c1 }),
        }
        .into();
        self.set(d + 1, Term::with_sub(ctr0));
        self.set(d + 2, Term::with_sub(ctr1));
        if dp.tag() == Tag::Dp0 { ctr0 } else { ctr1 }
    }

    /// DUP-APP: duplicating a (stuck) application duplicates both sides.
    /// `! d &L = (f x)`  =>  `d₀ ← (f₀ x₀); d₁ ← (f₁ x₁)` with `f`,`x` dup'd.
    fn dup_app(&mut self, dp: Term, app: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        let l = dp.ext();
        let a = app.val();
        let f = self.get(a);
        let x = self.get(a + 1);
        let df = self.dup_node(f);
        let dx = self.dup_node(x);
        let app0 = self.app(dp0(l, df), dp0(l, dx));
        let app1 = self.app(dp1(l, df), dp1(l, dx));
        self.set(d + 1, app0.with_sub());
        self.set(d + 2, app1.with_sub());
        if dp.tag() == Tag::Dp0 { app0 } else { app1 }
    }

    /// DUP-WLD: erasure duplicates into two erasures.
    fn dup_wld(&mut self, dp: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        let w = self.wld();
        self.set(d + 1, w.with_sub());
        self.set(d + 2, w.with_sub());
        w
    }

    /// APP-MAT / match on a value. `mat` is a `Mat` term, `arg` already WHNF.
    /// Returns `None` when no arm matches (the application is left stuck).
    fn app_mat(&mut self, mat: Term, arg: Term) -> Option<Term> {
        let idx = mat.val() as usize;
        match arg.unpack() {
            TermValue::Ctr { name, arity, ptr } => {
                let arity = arity as usize;
                let c = ptr.0;
                let fields: Vec<Term> = (0..arity).map(|i| self.get(c + i as u64)).collect();
                let branch = self.matches[idx]
                    .cases
                    .iter()
                    .find(|(k, _)| *k == PatKey::Ctr(name.0))
                    .map(|(_, t)| *t)
                    .or(self.matches[idx].default)?;
                self.itrs += 1;
                // apply the branch to the constructor's fields
                let mut b = branch;
                for f in fields {
                    b = self.app(b, f);
                }
                Some(b)
            }
            TermValue::Num(k) => {
                if let Some((_, b)) = self.matches[idx]
                    .cases
                    .iter()
                    .find(|(key, _)| *key == PatKey::Num(k))
                {
                    self.itrs += 1;
                    return Some(*b);
                }
                // numeric default receives the number
                let b = self.matches[idx].default?;
                self.itrs += 1;
                Some(self.app(b, arg))
            }
            _ => None, // stuck
        }
    }

    /// Try to evaluate a binary op at head. Returns `None` if stuck.
    fn try_bop(&mut self, bop: Term) -> Option<Term> {
        let (op, loc) = match bop.unpack() {
            TermValue::Bop { op, ptr } => (op, ptr.0),
            _ => unreachable!("try_bop on non-bop"),
        };
        let l = self.get(loc);
        let lhs = self.whnf(l);
        self.set(loc, lhs);
        // op distributes over a superposed operand
        if lhs.tag() == Tag::Sup {
            return Some(self.bop_sup_left(bop, lhs));
        }
        let r = self.get(loc + 1);
        let rhs = self.whnf(r);
        self.set(loc + 1, rhs);
        if rhs.tag() == Tag::Sup {
            return Some(self.bop_sup_right(bop, lhs, rhs));
        }
        if lhs.tag() == Tag::Num && rhs.tag() == Tag::Num {
            self.itrs += 1;
            return Some(self.num(apply_op(op, lhs.val(), rhs.val())));
        }
        None
    }

    fn bop_sup_left(&mut self, bop: Term, sup: Term) -> Term {
        self.itrs += 1;
        let loc = bop.val();
        let op = Op::from_u16(bop.ext());
        let lab = sup.ext();
        let s = sup.val();
        let a = self.get(s);
        let b = self.get(s + 1);
        let rhs = self.get(loc + 1);
        let d = self.dup_node(rhs);
        let b0 = self.bop(op, a, dp0(lab, d));
        let b1 = self.bop(op, b, dp1(lab, d));
        self.sup(lab, b0, b1)
    }

    fn bop_sup_right(&mut self, bop: Term, lhs: Term, sup: Term) -> Term {
        self.itrs += 1;
        let op = Op::from_u16(bop.ext());
        let lab = sup.ext();
        let s = sup.val();
        let a = self.get(s);
        let b = self.get(s + 1);
        let d = self.dup_node(lhs);
        let b0 = self.bop(op, dp0(lab, d), a);
        let b1 = self.bop(op, dp1(lab, d), b);
        self.sup(lab, b0, b1)
    }

    // ====================================================================
    // Reduction
    // ====================================================================

    /// Reduce a term to weak head normal form.
    pub fn whnf(&mut self, term: Term) -> Term {
        let mut stack: Vec<Term> = Vec::new();
        let mut term = term;
        'red: loop {
            if self.itrs >= self.budget {
                while let Some(cont) = stack.pop() {
                    self.set(cont.val(), term);
                    term = cont;
                }
                return term;
            }
            match term.tag() {
                Tag::Var => {
                    let sub = self.get(term.val());
                    if sub.is_sub() {
                        term = sub.clear_sub();
                        continue;
                    }
                    // Free variable. If a duplication is forcing it, apply
                    // DUP-VAR: a free variable duplicates to itself on both
                    // sides (this is what collapses dups during readback).
                    if let Some(cont) = stack.last().copied() {
                        if matches!(cont.tag(), Tag::Dp0 | Tag::Dp1) {
                            self.itrs += 1;
                            stack.pop();
                            let d = cont.val();
                            self.set(d + 1, term.with_sub());
                            self.set(d + 2, term.with_sub());
                            continue;
                        }
                    }
                }
                Tag::Dp0 | Tag::Dp1 => {
                    let d = term.val();
                    let off = if term.tag() == Tag::Dp0 { 1 } else { 2 };
                    let sub = self.get(d + off);
                    if sub.is_sub() {
                        term = sub.clear_sub();
                        continue;
                    }
                    // force the duplicated value
                    stack.push(term);
                    term = self.get(d);
                    continue;
                }
                Tag::App => {
                    stack.push(term);
                    term = self.get(term.val());
                    continue;
                }
                Tag::Bop => {
                    if let Some(v) = self.try_bop(term) {
                        term = v;
                        continue;
                    }
                    // stuck (free operand)
                }
                Tag::Lam => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.tag() {
                            Tag::App => {
                                stack.pop();
                                term = self.app_lam(cont, term);
                                continue;
                            }
                            Tag::Dp0 | Tag::Dp1 => {
                                stack.pop();
                                term = self.dup_lam(cont, term);
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                Tag::Sup => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.tag() {
                            Tag::App => {
                                stack.pop();
                                term = self.app_sup(cont, term);
                                continue;
                            }
                            Tag::Dp0 | Tag::Dp1 => {
                                stack.pop();
                                term = self.dup_sup(cont, term);
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                Tag::Num => {
                    if let Some(cont) = stack.last().copied() {
                        if matches!(cont.tag(), Tag::Dp0 | Tag::Dp1) {
                            stack.pop();
                            term = self.dup_num(cont, term);
                            continue;
                        }
                    }
                }
                Tag::Ctr => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.tag() {
                            Tag::Dp0 | Tag::Dp1 => {
                                stack.pop();
                                term = self.dup_ctr(cont, term);
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                Tag::Mat => {
                    if let Some(cont) = stack.last().copied() {
                        if cont.tag() == Tag::App {
                            let a = cont.val();
                            let arg0 = self.get(a + 1);
                            let arg = self.whnf(arg0);
                            self.set(a + 1, arg);
                            if let Some(t) = self.app_mat(term, arg) {
                                stack.pop();
                                term = t;
                                continue;
                            }
                            // no arm matched: leave the application stuck
                        }
                        // duplicating a match value: share it (affine-unsafe but
                        // adequate for top-level case functions used once).
                        else if matches!(cont.tag(), Tag::Dp0 | Tag::Dp1) {
                            stack.pop();
                            let d = cont.val();
                            self.set(d + 1, term.with_sub());
                            self.set(d + 2, term.with_sub());
                            continue;
                        }
                    }
                }
                Tag::Wld => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.tag() {
                            Tag::App => {
                                stack.pop();
                                // (* a) => *
                                term = self.wld();
                                continue;
                            }
                            Tag::Dp0 | Tag::Dp1 => {
                                stack.pop();
                                term = self.dup_wld(cont);
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            // Head is a value or stuck. Unwind the spine. A duplication
            // continuation surfacing here means we must duplicate a stuck value
            // (DUP-APP / DUP-VAR and friends) and resume reduction.
            loop {
                let cont = match stack.pop() {
                    None => return term,
                    Some(c) => c,
                };
                if matches!(cont.tag(), Tag::Dp0 | Tag::Dp1) {
                    term = match term.tag() {
                        Tag::App => self.dup_app(cont, term),
                        Tag::Lam => self.dup_lam(cont, term),
                        Tag::Sup => self.dup_sup(cont, term),
                        Tag::Num => self.dup_num(cont, term),
                        Tag::Ctr => self.dup_ctr(cont, term),
                        Tag::Wld => self.dup_wld(cont),
                        Tag::Var => {
                            // DUP-VAR: a free variable duplicates to itself.
                            self.itrs += 1;
                            let d = cont.val();
                            self.set(d + 1, term.with_sub());
                            self.set(d + 2, term.with_sub());
                            term
                        }
                        _ => {
                            // unexpected stuck head; cache and leave the dup stuck
                            self.set(cont.val(), term);
                            cont
                        }
                    };
                    continue 'red;
                }
                // application (or other) spine node: rebuild and keep unwinding
                self.set(cont.val(), term);
                term = cont;
            }
        }
    }

    /// Reduce a term to strong (full) normal form, with an interaction budget.
    pub fn normalize(&mut self, term: Term, budget: u64) -> Term {
        self.budget = budget;
        let term = self.whnf(term);
        if self.itrs >= budget {
            return term;
        }
        match term.unpack() {
            TermValue::Lam(p) => {
                let body0 = self.get(p.0 + 1);
                let body = self.normalize(body0, budget);
                self.set(p.0 + 1, body);
                term
            }
            TermValue::App(p) => {
                let f0 = self.get(p.0);
                let f = self.normalize(f0, budget);
                let x0 = self.get(p.0 + 1);
                let x = self.normalize(x0, budget);
                self.set(p.0, f);
                self.set(p.0 + 1, x);
                term
            }
            TermValue::Sup { ptr, .. } => {
                let x0 = self.get(ptr.0);
                let x = self.normalize(x0, budget);
                let y0 = self.get(ptr.0 + 1);
                let y = self.normalize(y0, budget);
                self.set(ptr.0, x);
                self.set(ptr.0 + 1, y);
                term
            }
            TermValue::Ctr { arity, ptr, .. } => {
                for i in 0..arity as u64 {
                    let f0 = self.get(ptr.0 + i);
                    let f = self.normalize(f0, budget);
                    self.set(ptr.0 + i, f);
                }
                term
            }
            TermValue::Bop { ptr, .. } => {
                let l0 = self.get(ptr.0);
                let l = self.normalize(l0, budget);
                let r0 = self.get(ptr.0 + 1);
                let r = self.normalize(r0, budget);
                self.set(ptr.0, l);
                self.set(ptr.0 + 1, r);
                term
            }
            _ => term,
        }
    }
}

fn apply_op(op: Op, a: u64, b: u64) -> u64 {
    match op {
        Op::Add => a.wrapping_add(b),
        Op::Sub => a.wrapping_sub(b),
        Op::Mul => a.wrapping_mul(b),
        Op::Div => {
            if b == 0 {
                0
            } else {
                a / b
            }
        }
        Op::Rem => {
            if b == 0 {
                0
            } else {
                a % b
            }
        }
        Op::Eq => (a == b) as u64,
        Op::Neq => (a != b) as u64,
        Op::Lt => (a < b) as u64,
        Op::Lte => (a <= b) as u64,
        Op::Gt => (a > b) as u64,
        Op::Gte => (a >= b) as u64,
        Op::And => (a != 0 && b != 0) as u64,
        Op::Or => (a != 0 || b != 0) as u64,
        Op::Xor => a ^ b,
        Op::Shl => a.wrapping_shl(b as u32),
        Op::Shr => a.wrapping_shr(b as u32),
    }
}
