//! The [`Executor`]: interaction-calculus evaluation over a [`Heap`].
//!
//! Evaluation is kept separate from term storage. An `Executor` borrows a
//! `&'h mut Heap` and drives reduction, tracking the interaction count and
//! budget. The `Heap` provides storage and node builders; the `Executor`
//! provides the interaction rules, `whnf`, and `normalize`.

use crate::vm::heap::{Heap, PatKey, dp0, dp1, var};
use crate::vm::term::{BinaryOp, Tag, Term, TermPtr, View};

/// Drives reduction over a borrowed [`Heap`].
pub struct Executor<'h> {
    pub heap: &'h mut Heap,
    /// Number of interactions performed (for stats / step limiting).
    pub itrs: u64,
    /// Interaction budget; reduction stops once `itrs` reaches it.
    pub budget: u64,
}

impl<'h> Executor<'h> {
    pub fn new(heap: &'h mut Heap) -> Self {
        Executor {
            heap,
            itrs: 0,
            budget: u64::MAX,
        }
    }

    // ====================================================================
    // Interactions
    // ====================================================================

    /// APP-LAM: `(λx.body) arg`  =>  `x ← arg; body`
    fn app_lam(&mut self, app: Term, lam: Term) -> Term {
        self.itrs += 1;
        let a = app.val();
        let l = lam.val();
        let arg = self.heap.get(a + 1);
        // substitute the lambda's variable
        self.heap.set(l, arg.with_sub());
        self.heap.get(l + 1)
    }

    /// APP-SUP: `(&L{f,g}) arg`  =>  `!a&L=arg; &L{(f a₀),(g a₁)}`
    fn app_sup(&mut self, app: Term, sup: Term) -> Term {
        self.itrs += 1;
        let a = app.val();
        let s = sup.val();
        let lab = sup.ext();
        let arg = self.heap.get(a + 1);
        let f = self.heap.get(s);
        let g = self.heap.get(s + 1);
        let d = self.heap.dup_node(arg);
        let fa = self.heap.app(f, dp0(lab, d));
        let gb = self.heap.app(g, dp1(lab, d));
        self.heap.sup(lab, fa, gb)
    }

    /// DUP-SUP. Same label annihilates; different labels commute.
    fn dup_sup(&mut self, dp: Term, sup: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        let l = dp.ext();
        let s = sup.val();
        let r = sup.ext();
        let a = self.heap.get(s);
        let b = self.heap.get(s + 1);
        if l == r {
            self.heap.set(d + 1, a.with_sub());
            self.heap.set(d + 2, b.with_sub());
            if dp.tag() == Tag::Dp0 { a } else { b }
        } else {
            let da = self.heap.dup_node(a);
            let db = self.heap.dup_node(b);
            let s0 = self.heap.sup(r, dp0(l, da), dp0(l, db));
            let s1 = self.heap.sup(r, dp1(l, da), dp1(l, db));
            self.heap.set(d + 1, s0.with_sub());
            self.heap.set(d + 2, s1.with_sub());
            if dp.tag() == Tag::Dp0 { s0 } else { s1 }
        }
    }

    /// DUP-LAM: duplicating a lambda yields two lambdas with a superposed var.
    fn dup_lam(&mut self, dp: Term, lam: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        let l = dp.ext();
        let lloc = lam.val();
        let body = self.heap.get(lloc + 1);
        let dg = self.heap.dup_node(body);
        let (lam0, l0) = self.heap.lam(dp0(l, dg));
        let (lam1, l1) = self.heap.lam(dp1(l, dg));
        // x ← &L{$x0, $x1}
        let sup_var = self.heap.sup(l, var(l0), var(l1));
        self.heap.set(lloc, sup_var.with_sub());
        self.heap.set(d + 1, lam0.with_sub());
        self.heap.set(d + 2, lam1.with_sub());
        if dp.tag() == Tag::Dp0 { lam0 } else { lam1 }
    }

    /// DUP-NUM: numbers are duplicated trivially.
    fn dup_num(&mut self, dp: Term, num: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        self.heap.set(d + 1, num.with_sub());
        self.heap.set(d + 2, num.with_sub());
        num
    }

    /// DUP-CTR: duplicate a constructor field by field.
    fn dup_ctr(&mut self, dp: Term, ctr: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        let l = dp.ext();
        let (name, arity, c) = match ctr.unpack() {
            View::Ctr { name, arity, ptr } => (name, arity as usize, ptr.0),
            _ => unreachable!("dup_ctr on non-constructor"),
        };
        let c0 = self.heap.alloc(arity);
        let c1 = self.heap.alloc(arity);
        for i in 0..arity {
            let field = self.heap.get(c + i as u64);
            let di = self.heap.dup_node(field);
            self.heap.set(c0 + i as u64, dp0(l, di));
            self.heap.set(c1 + i as u64, dp1(l, di));
        }
        let ctr0 = View::Ctr {
            name,
            arity: arity as u8,
            ptr: TermPtr(if arity == 0 { 0 } else { c0 }),
        }
        .into();
        let ctr1 = View::Ctr {
            name,
            arity: arity as u8,
            ptr: TermPtr(if arity == 0 { 0 } else { c1 }),
        }
        .into();
        self.heap.set(d + 1, Term::with_sub(ctr0));
        self.heap.set(d + 2, Term::with_sub(ctr1));
        if dp.tag() == Tag::Dp0 { ctr0 } else { ctr1 }
    }

    /// DUP-APP: duplicating a (stuck) application duplicates both sides.
    /// `! d &L = (f x)`  =>  `d₀ ← (f₀ x₀); d₁ ← (f₁ x₁)` with `f`,`x` dup'd.
    fn dup_app(&mut self, dp: Term, app: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        let l = dp.ext();
        let a = app.val();
        let f = self.heap.get(a);
        let x = self.heap.get(a + 1);
        let df = self.heap.dup_node(f);
        let dx = self.heap.dup_node(x);
        let app0 = self.heap.app(dp0(l, df), dp0(l, dx));
        let app1 = self.heap.app(dp1(l, df), dp1(l, dx));
        self.heap.set(d + 1, app0.with_sub());
        self.heap.set(d + 2, app1.with_sub());
        if dp.tag() == Tag::Dp0 { app0 } else { app1 }
    }

    /// DUP-WLD: erasure duplicates into two erasures.
    fn dup_wld(&mut self, dp: Term) -> Term {
        self.itrs += 1;
        let d = dp.val();
        let w = self.heap.wld();
        self.heap.set(d + 1, w.with_sub());
        self.heap.set(d + 2, w.with_sub());
        w
    }

    /// APP-MAT / match on a value. `mat` is a `Mat` term, `arg` already WHNF.
    /// Returns `None` when no arm matches (the application is left stuck).
    fn app_mat(&mut self, mat: Term, arg: Term) -> Option<Term> {
        let idx = mat.val() as usize;
        match arg.unpack() {
            View::Ctr { name, arity, ptr } => {
                let arity = arity as usize;
                let c = ptr.0;
                let fields: Vec<Term> = (0..arity).map(|i| self.heap.get(c + i as u64)).collect();
                let branch = self.heap.matches[idx]
                    .cases
                    .iter()
                    .find(|(k, _)| *k == PatKey::Ctr(name.0))
                    .map(|(_, t)| *t)
                    .or(self.heap.matches[idx].default)?;
                self.itrs += 1;
                // apply the branch to the constructor's fields
                let mut b = branch;
                for f in fields {
                    b = self.heap.app(b, f);
                }
                Some(b)
            }
            View::Num(k) => {
                if let Some((_, b)) = self.heap.matches[idx]
                    .cases
                    .iter()
                    .find(|(key, _)| *key == PatKey::Num(k))
                {
                    self.itrs += 1;
                    return Some(*b);
                }
                // numeric default receives the number
                let b = self.heap.matches[idx].default?;
                self.itrs += 1;
                Some(self.heap.app(b, arg))
            }
            _ => None, // stuck
        }
    }

    /// Try to evaluate a binary op at head. Returns `None` if stuck.
    fn try_bop(&mut self, bop: Term) -> Option<Term> {
        let (op, loc) = match bop.unpack() {
            View::Bop { op, ptr } => (op, ptr.0),
            _ => unreachable!("try_bop on non-bop"),
        };
        let l = self.heap.get(loc);
        let lhs = self.whnf(l);
        self.heap.set(loc, lhs);
        // op distributes over a superposed operand
        if lhs.tag() == Tag::Sup {
            return Some(self.bop_sup_left(bop, lhs));
        }
        let r = self.heap.get(loc + 1);
        let rhs = self.whnf(r);
        self.heap.set(loc + 1, rhs);
        if rhs.tag() == Tag::Sup {
            return Some(self.bop_sup_right(bop, lhs, rhs));
        }
        if lhs.tag() == Tag::Num && rhs.tag() == Tag::Num {
            self.itrs += 1;
            return Some(self.heap.num(apply_op(op, lhs.val(), rhs.val())));
        }
        None
    }

    fn bop_sup_left(&mut self, bop: Term, sup: Term) -> Term {
        self.itrs += 1;
        let loc = bop.val();
        let op = BinaryOp::from(bop.ext());
        let lab = sup.ext();
        let s = sup.val();
        let a = self.heap.get(s);
        let b = self.heap.get(s + 1);
        let rhs = self.heap.get(loc + 1);
        let d = self.heap.dup_node(rhs);
        let b0 = self.heap.bop(op, a, dp0(lab, d));
        let b1 = self.heap.bop(op, b, dp1(lab, d));
        self.heap.sup(lab, b0, b1)
    }

    fn bop_sup_right(&mut self, bop: Term, lhs: Term, sup: Term) -> Term {
        self.itrs += 1;
        let op = BinaryOp::from(bop.ext());
        let lab = sup.ext();
        let s = sup.val();
        let a = self.heap.get(s);
        let b = self.heap.get(s + 1);
        let d = self.heap.dup_node(lhs);
        let b0 = self.heap.bop(op, dp0(lab, d), a);
        let b1 = self.heap.bop(op, dp1(lab, d), b);
        self.heap.sup(lab, b0, b1)
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
                    self.heap.set(cont.val(), term);
                    term = cont;
                }
                return term;
            }
            match term.tag() {
                Tag::Var => {
                    let sub = self.heap.get(term.val());
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
                            self.heap.set(d + 1, term.with_sub());
                            self.heap.set(d + 2, term.with_sub());
                            continue;
                        }
                    }
                }
                Tag::Dp0 | Tag::Dp1 => {
                    let d = term.val();
                    let off = if term.tag() == Tag::Dp0 { 1 } else { 2 };
                    let sub = self.heap.get(d + off);
                    if sub.is_sub() {
                        term = sub.clear_sub();
                        continue;
                    }
                    // force the duplicated value
                    stack.push(term);
                    term = self.heap.get(d);
                    continue;
                }
                Tag::App => {
                    stack.push(term);
                    term = self.heap.get(term.val());
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
                            let arg0 = self.heap.get(a + 1);
                            let arg = self.whnf(arg0);
                            self.heap.set(a + 1, arg);
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
                            self.heap.set(d + 1, term.with_sub());
                            self.heap.set(d + 2, term.with_sub());
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
                                term = self.heap.wld();
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
                            self.heap.set(d + 1, term.with_sub());
                            self.heap.set(d + 2, term.with_sub());
                            term
                        }
                        _ => {
                            // unexpected stuck head; cache and leave the dup stuck
                            self.heap.set(cont.val(), term);
                            cont
                        }
                    };
                    continue 'red;
                }
                // application (or other) spine node: rebuild and keep unwinding
                self.heap.set(cont.val(), term);
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
            View::Lam(p) => {
                let body0 = self.heap.get(p.0 + 1);
                let body = self.normalize(body0, budget);
                self.heap.set(p.0 + 1, body);
                term
            }
            View::App(p) => {
                let f0 = self.heap.get(p.0);
                let f = self.normalize(f0, budget);
                let x0 = self.heap.get(p.0 + 1);
                let x = self.normalize(x0, budget);
                self.heap.set(p.0, f);
                self.heap.set(p.0 + 1, x);
                term
            }
            View::Sup { ptr, .. } => {
                let x0 = self.heap.get(ptr.0);
                let x = self.normalize(x0, budget);
                let y0 = self.heap.get(ptr.0 + 1);
                let y = self.normalize(y0, budget);
                self.heap.set(ptr.0, x);
                self.heap.set(ptr.0 + 1, y);
                term
            }
            View::Ctr { arity, ptr, .. } => {
                for i in 0..arity as u64 {
                    let f0 = self.heap.get(ptr.0 + i);
                    let f = self.normalize(f0, budget);
                    self.heap.set(ptr.0 + i, f);
                }
                term
            }
            View::Bop { ptr, .. } => {
                let l0 = self.heap.get(ptr.0);
                let l = self.normalize(l0, budget);
                let r0 = self.heap.get(ptr.0 + 1);
                let r = self.normalize(r0, budget);
                self.heap.set(ptr.0, l);
                self.heap.set(ptr.0 + 1, r);
                term
            }
            _ => term,
        }
    }
}

fn apply_op(op: BinaryOp, a: u64, b: u64) -> u64 {
    match op {
        BinaryOp::Add => a.wrapping_add(b),
        BinaryOp::Sub => a.wrapping_sub(b),
        BinaryOp::Mul => a.wrapping_mul(b),
        BinaryOp::Div => {
            if b == 0 {
                0
            } else {
                a / b
            }
        }
        BinaryOp::Mod => {
            if b == 0 {
                0
            } else {
                a % b
            }
        }
        BinaryOp::Eq => (a == b) as u64,
        BinaryOp::Neq => (a != b) as u64,
        BinaryOp::Lt => (a < b) as u64,
        BinaryOp::Lte => (a <= b) as u64,
        BinaryOp::Gt => (a > b) as u64,
        BinaryOp::Gte => (a >= b) as u64,
        BinaryOp::And => (a != 0 && b != 0) as u64,
        BinaryOp::Or => (a != 0 || b != 0) as u64,
        BinaryOp::Xor => a ^ b,
        BinaryOp::Shl => a.wrapping_shl(b as u32),
        BinaryOp::Shr => a.wrapping_shr(b as u32),
        BinaryOp::Invalid => 0,
    }
}
