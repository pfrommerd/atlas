//! The [`Executor`]: interaction-calculus evaluation over a [`Heap`].
//!
//! Evaluation is kept separate from term storage. An `Executor` borrows a
//! `&'h mut Heap` and drives reduction, tracking the interaction count and
//! budget. The `Heap` provides storage and node builders; the `Executor`
//! provides the interaction rules, `whnf`, and `normalize`.
//!
//! The executor never inspects a [`Node`]'s packed bits: it reads cells through
//! the heap's typed readers and dispatches on [`Node::unpack`]'s [`Term`]. The
//! interaction rules take the already-unpacked payloads (e.g. the [`PairPtr`] of
//! an application) so a node is unpacked at most once per step.

use crate::vm::heap::{Heap, PatKey, dp0, dp1, var};
use crate::vm::term::{
    Arity, BinaryOp, DupPtr, Label, MatchId, NameId, Node, NodePtr, PairPtr, Term, TriplePtr,
};

/// The kind of interaction performed in a single reduction step.
///
/// Passed to [`ExecPolicy::stepped`] so a policy can account for (or ignore)
/// reductions however it likes — totalling fuel, histogramming rule usage, etc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InteractionType {
    /// APP-LAM: a lambda applied to an argument.
    AppLam,
    /// APP-SUP: a superposition applied to an argument.
    AppSup,
    /// APP-MAT: a match scrutinizing a value.
    AppMat,
    /// DUP-LAM: duplicating a lambda.
    DupLam,
    /// DUP-SUP: duplicating a superposition.
    DupSup,
    /// DUP-NUM: duplicating a number.
    DupNum,
    /// DUP-CTR: duplicating a constructor.
    DupCtr,
    /// DUP-APP: duplicating a stuck application.
    DupApp,
    /// DUP-WLD: duplicating an erasure.
    DupWld,
    /// DUP-VAR: duplicating a free variable (it duplicates to itself).
    DupVar,
    /// A binary operation on two numbers.
    BopVal,
    /// A binary operation distributing over a superposed operand.
    BopSup,
}

/// Controls how an [`Executor`] accounts for reduction steps and decides when
/// to stop.
///
/// The executor calls [`stepped`](ExecPolicy::stepped) for every interaction it
/// performs and checks [`should_continue`](ExecPolicy::should_continue) in its
/// reduction loops. Keeping this behind a trait lets callers that don't need
/// fuel accounting (see [`UnlimitedBudget`]) pay nothing for it on the hot path.
pub trait ExecPolicy {
    /// Record that one interaction of the given kind was performed.
    fn stepped(&mut self, interaction: InteractionType);
    /// Whether reduction may continue. Checked before performing more work.
    fn should_continue(&self) -> bool;
}

/// A policy that never limits reduction; [`stepped`] is a no-op and reduction
/// always continues. Use this when you want a term fully normalized.
pub struct UnlimitedBudget;

impl ExecPolicy for UnlimitedBudget {
    #[inline(always)]
    fn stepped(&mut self, _: InteractionType) {}
    #[inline(always)]
    fn should_continue(&self) -> bool {
        true
    }
}

/// A policy that stops after a fixed number of interactions.
pub struct FiniteBudget {
    /// Number of interactions performed so far.
    pub itrs: u64,
    /// Interaction budget; reduction stops once `itrs` reaches it.
    pub budget: u64,
}

impl FiniteBudget {
    pub fn new(budget: u64) -> Self {
        FiniteBudget { itrs: 0, budget }
    }
}

impl ExecPolicy for FiniteBudget {
    #[inline]
    fn stepped(&mut self, _: InteractionType) {
        self.itrs += 1;
    }
    #[inline]
    fn should_continue(&self) -> bool {
        self.itrs < self.budget
    }
}

/// Drives reduction over a borrowed [`Heap`], guided by an [`ExecPolicy`].
pub struct Executor<'h, P: ExecPolicy> {
    pub heap: &'h mut Heap,
    /// Policy governing fuel accounting and the stopping condition.
    pub policy: P,
}

impl<'h, P: ExecPolicy> Executor<'h, P> {
    pub fn new(heap: &'h mut Heap, policy: P) -> Self {
        Executor { heap, policy }
    }

    /// Write `val` into `slot` as a substitution (consumes the binder).
    fn subst(&mut self, slot: NodePtr, val: Node) {
        self.heap.set(slot, Term::Sub(val).pack());
    }

    // ====================================================================
    // Interactions
    // ====================================================================

    /// APP-LAM: `(λx.body) arg`  =>  `x ← arg; body`
    fn app_lam(&mut self, app: PairPtr, lam: PairPtr) -> Node {
        self.policy.stepped(InteractionType::AppLam);
        let arg = self.heap.node(app.second());
        self.subst(lam.first(), arg);
        self.heap.node(lam.second())
    }

    /// APP-SUP: `(&L{f,g}) arg`  =>  `!a&L=arg; &L{(f a₀),(g a₁)}`
    fn app_sup(&mut self, app: PairPtr, slab: Label, sup: TriplePtr) -> Node {
        self.policy.stepped(InteractionType::AppSup);
        let arg = self.heap.node(app.second());
        let (f, g) = self.heap.sup_args(sup);
        let d = self.heap.dup_node(slab, arg);
        let fa = self.heap.app(f, dp0(d));
        let gb = self.heap.app(g, dp1(d));
        self.heap.sup(slab, fa, gb)
    }

    /// DUP-SUP. Same label annihilates; different labels commute.
    fn dup_sup(
        &mut self,
        is_dp0: bool,
        dlab: Label,
        dp: DupPtr,
        slab: Label,
        sup: TriplePtr,
    ) -> Node {
        self.policy.stepped(InteractionType::DupSup);
        let (a, b) = self.heap.sup_args(sup);
        if dlab == slab {
            self.subst(dp.sub0(), a);
            self.subst(dp.sub1(), b);
            if is_dp0 { a } else { b }
        } else {
            let da = self.heap.dup_node(dlab, a);
            let db = self.heap.dup_node(dlab, b);
            let s0 = self.heap.sup(slab, dp0(da), dp0(db));
            let s1 = self.heap.sup(slab, dp1(da), dp1(db));
            self.subst(dp.sub0(), s0);
            self.subst(dp.sub1(), s1);
            if is_dp0 { s0 } else { s1 }
        }
    }

    /// DUP-LAM: duplicating a lambda yields two lambdas with a superposed var.
    fn dup_lam(&mut self, is_dp0: bool, dlab: Label, dp: DupPtr, lam: PairPtr) -> Node {
        self.policy.stepped(InteractionType::DupLam);
        let body = self.heap.node(lam.second());
        let dg = self.heap.dup_node(dlab, body);
        let (lam0, p0) = self.heap.lam(dp0(dg));
        let (lam1, p1) = self.heap.lam(dp1(dg));
        // x ← &L{$x0, $x1}
        let sup_var = self.heap.sup(dlab, var(p0.first()), var(p1.first()));
        self.subst(lam.first(), sup_var);
        self.subst(dp.sub0(), lam0);
        self.subst(dp.sub1(), lam1);
        if is_dp0 { lam0 } else { lam1 }
    }

    /// DUP-NUM: numbers are duplicated trivially.
    fn dup_num(&mut self, dp: DupPtr, num: Node) -> Node {
        self.policy.stepped(InteractionType::DupNum);
        self.subst(dp.sub0(), num);
        self.subst(dp.sub1(), num);
        num
    }

    /// DUP-CTR: duplicate a constructor field by field.
    fn dup_ctr(
        &mut self,
        is_dp0: bool,
        dlab: Label,
        dp: DupPtr,
        name: NameId,
        arity: Arity,
        base: NodePtr,
    ) -> Node {
        self.policy.stepped(InteractionType::DupCtr);
        let arity = arity.0 as usize;
        let mut f0 = Vec::with_capacity(arity);
        let mut f1 = Vec::with_capacity(arity);
        for i in 0..arity {
            let field = self.heap.node(self.heap.ctr_field(base, i as u64));
            let di = self.heap.dup_node(dlab, field);
            f0.push(dp0(di));
            f1.push(dp1(di));
        }
        let ctr0 = self.heap.ctr(name, &f0);
        let ctr1 = self.heap.ctr(name, &f1);
        self.subst(dp.sub0(), ctr0);
        self.subst(dp.sub1(), ctr1);
        if is_dp0 { ctr0 } else { ctr1 }
    }

    /// DUP-APP: duplicating a (stuck) application duplicates both sides.
    /// `! d &L = (f x)`  =>  `d₀ ← (f₀ x₀); d₁ ← (f₁ x₁)` with `f`,`x` dup'd.
    fn dup_app(&mut self, is_dp0: bool, dlab: Label, dp: DupPtr, app: PairPtr) -> Node {
        self.policy.stepped(InteractionType::DupApp);
        let (f, x) = self.heap.pair(app);
        let df = self.heap.dup_node(dlab, f);
        let dx = self.heap.dup_node(dlab, x);
        let app0 = self.heap.app(dp0(df), dp0(dx));
        let app1 = self.heap.app(dp1(df), dp1(dx));
        self.subst(dp.sub0(), app0);
        self.subst(dp.sub1(), app1);
        if is_dp0 { app0 } else { app1 }
    }

    /// DUP-WLD: erasure duplicates into two erasures.
    fn dup_wld(&mut self, dp: DupPtr) -> Node {
        self.policy.stepped(InteractionType::DupWld);
        let w = self.heap.wld();
        self.subst(dp.sub0(), w);
        self.subst(dp.sub1(), w);
        w
    }

    /// Duplicating a stuck head that surfaced at the end of the spine.
    fn dup_head(&mut self, is_dp0: bool, dlab: Label, dp: DupPtr, head: Node) -> Node {
        match head.unpack() {
            Term::App(app) => self.dup_app(is_dp0, dlab, dp, app),
            Term::Lam(lam) => self.dup_lam(is_dp0, dlab, dp, lam),
            Term::Sup(ptr) => {
                let slab = self.heap.sup_label(ptr);
                self.dup_sup(is_dp0, dlab, dp, slab, ptr)
            }
            Term::Num(_) => self.dup_num(dp, head),
            Term::Ctr(base) => {
                let (name, arity) = self.heap.ctr_head(base);
                self.dup_ctr(is_dp0, dlab, dp, name, arity, base)
            }
            Term::Wld => self.dup_wld(dp),
            // DUP-VAR: a free variable duplicates to itself.
            Term::Var(_) => {
                self.policy.stepped(InteractionType::DupVar);
                self.subst(dp.sub0(), head);
                self.subst(dp.sub1(), head);
                head
            }
            // unexpected stuck head; cache it and leave the dup stuck
            _ => {
                self.heap.set(dp.val(), head);
                if is_dp0 {
                    Term::Dp0(dp).pack()
                } else {
                    Term::Dp1(dp).pack()
                }
            }
        }
    }

    /// APP-MAT / match on a value. `arg` is already WHNF.
    /// Returns `None` when no arm matches (the application is left stuck).
    fn app_mat(&mut self, mat: MatchId, arg: Node) -> Option<Node> {
        let idx = mat.0 as usize;
        match arg.unpack() {
            Term::Ctr(base) => {
                let (name, arity) = self.heap.ctr_head(base);
                let fields: Vec<Node> = (0..arity.0)
                    .map(|i| self.heap.node(self.heap.ctr_field(base, i)))
                    .collect();
                let branch = self.heap.matches[idx]
                    .cases
                    .iter()
                    .find(|(k, _)| *k == PatKey::Ctr(name))
                    .map(|(_, t)| *t)
                    .or(self.heap.matches[idx].default)?;
                self.policy.stepped(InteractionType::AppMat);
                // apply the branch to the constructor's fields
                let mut b = branch;
                for f in fields {
                    b = self.heap.app(b, f);
                }
                Some(b)
            }
            Term::Num(k) => {
                if let Some((_, b)) = self.heap.matches[idx]
                    .cases
                    .iter()
                    .find(|(key, _)| *key == PatKey::Num(k))
                {
                    self.policy.stepped(InteractionType::AppMat);
                    return Some(*b);
                }
                // numeric default receives the number
                let b = self.heap.matches[idx].default?;
                self.policy.stepped(InteractionType::AppMat);
                Some(self.heap.app(b, arg))
            }
            _ => None, // stuck
        }
    }

    /// Try to evaluate a binary op at head. Returns `None` if stuck.
    /// `ptr` is the `[OpMeta, lhs, rhs]` triple.
    fn try_bop(&mut self, op: BinaryOp, ptr: TriplePtr) -> Option<Node> {
        self.whnf(ptr.second());
        let lhs = self.heap.node(ptr.second());
        // op distributes over a superposed operand
        if let Term::Sup(sup) = lhs.unpack() {
            let label = self.heap.sup_label(sup);
            return Some(self.bop_sup_left(op, ptr, label, sup));
        }
        self.whnf(ptr.third());
        let rhs = self.heap.node(ptr.third());
        if let Term::Sup(sup) = rhs.unpack() {
            let label = self.heap.sup_label(sup);
            return Some(self.bop_sup_right(op, lhs, label, sup));
        }
        if let (Term::Num(a), Term::Num(b)) = (lhs.unpack(), rhs.unpack()) {
            self.policy.stepped(InteractionType::BopVal);
            return Some(self.heap.num(apply_op(op, a, b)));
        }
        None
    }

    fn bop_sup_left(&mut self, op: BinaryOp, bop: TriplePtr, slab: Label, sup: TriplePtr) -> Node {
        self.policy.stepped(InteractionType::BopSup);
        let (a, b) = self.heap.sup_args(sup);
        let rhs = self.heap.node(bop.third());
        let d = self.heap.dup_node(slab, rhs);
        let b0 = self.heap.bop(op, a, dp0(d));
        let b1 = self.heap.bop(op, b, dp1(d));
        self.heap.sup(slab, b0, b1)
    }

    fn bop_sup_right(&mut self, op: BinaryOp, lhs: Node, slab: Label, sup: TriplePtr) -> Node {
        self.policy.stepped(InteractionType::BopSup);
        let (a, b) = self.heap.sup_args(sup);
        let d = self.heap.dup_node(slab, lhs);
        let b0 = self.heap.bop(op, dp0(d), a);
        let b1 = self.heap.bop(op, dp1(d), b);
        self.heap.sup(slab, b0, b1)
    }

    // ====================================================================
    // Reduction
    // ====================================================================

    /// Reduce the term stored at `ptr` to weak head normal form, writing the
    /// result back into `ptr`.
    pub fn whnf(&mut self, ptr: NodePtr) {
        // Continuations on the spine are always `App` or `Dp0`/`Dp1` nodes.
        let mut stack: Vec<Node> = Vec::new();
        let mut term = self.heap.node(ptr);
        let result = 'red: loop {
            if !self.policy.should_continue() {
                while let Some(cont) = stack.pop() {
                    let slot = match cont.unpack() {
                        Term::App(p) => p.first(),
                        Term::Dp0(q) | Term::Dp1(q) => q.val(),
                        _ => unreachable!("non-spine continuation"),
                    };
                    self.heap.set(slot, term);
                    term = cont;
                }
                break 'red term;
            }
            match term.unpack() {
                Term::Var(slot) => {
                    if let Term::Sub(n) = self.heap.node(slot).unpack() {
                        term = n;
                        continue;
                    }
                    // Free variable. If a duplication is forcing it, apply
                    // DUP-VAR: a free variable duplicates to itself on both
                    // sides (this is what collapses dups during readback).
                    if let Some(cont) = stack.last().copied()
                        && let Term::Dp0(q) | Term::Dp1(q) = cont.unpack()
                    {
                        self.policy.stepped(InteractionType::DupVar);
                        stack.pop();
                        self.subst(q.sub0(), term);
                        self.subst(q.sub1(), term);
                        continue;
                    }
                }
                Term::Dp0(q) => {
                    if let Term::Sub(n) = self.heap.node(q.sub0()).unpack() {
                        term = n;
                        continue;
                    }
                    stack.push(term);
                    term = self.heap.node(q.val());
                    continue;
                }
                Term::Dp1(q) => {
                    if let Term::Sub(n) = self.heap.node(q.sub1()).unpack() {
                        term = n;
                        continue;
                    }
                    stack.push(term);
                    term = self.heap.node(q.val());
                    continue;
                }
                Term::App(p) => {
                    stack.push(term);
                    term = self.heap.node(p.first());
                    continue;
                }
                Term::Bop(ptr) => {
                    let op = self.heap.node(ptr.first()).as_op();
                    if let Some(v) = self.try_bop(op, ptr) {
                        term = v;
                        continue;
                    }
                    // stuck (free operand)
                }
                Term::Lam(lam) => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.unpack() {
                            Term::App(app) => {
                                stack.pop();
                                term = self.app_lam(app, lam);
                                continue;
                            }
                            Term::Dp0(q) => {
                                stack.pop();
                                let label = self.heap.dup_label(q);
                                term = self.dup_lam(true, label, q, lam);
                                continue;
                            }
                            Term::Dp1(q) => {
                                stack.pop();
                                let label = self.heap.dup_label(q);
                                term = self.dup_lam(false, label, q, lam);
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                Term::Sup(sup) => {
                    if let Some(cont) = stack.last().copied() {
                        let slab = self.heap.sup_label(sup);
                        match cont.unpack() {
                            Term::App(app) => {
                                stack.pop();
                                term = self.app_sup(app, slab, sup);
                                continue;
                            }
                            Term::Dp0(q) => {
                                stack.pop();
                                let dlab = self.heap.dup_label(q);
                                term = self.dup_sup(true, dlab, q, slab, sup);
                                continue;
                            }
                            Term::Dp1(q) => {
                                stack.pop();
                                let dlab = self.heap.dup_label(q);
                                term = self.dup_sup(false, dlab, q, slab, sup);
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                Term::Num(_) => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.unpack() {
                            Term::Dp0(q) => {
                                stack.pop();
                                term = self.dup_num(q, term);
                                continue;
                            }
                            Term::Dp1(q) => {
                                stack.pop();
                                term = self.dup_num(q, term);
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                Term::Ctr(base) => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.unpack() {
                            Term::Dp0(q) => {
                                stack.pop();
                                let (name, arity) = self.heap.ctr_head(base);
                                let label = self.heap.dup_label(q);
                                term = self.dup_ctr(true, label, q, name, arity, base);
                                continue;
                            }
                            Term::Dp1(q) => {
                                stack.pop();
                                let (name, arity) = self.heap.ctr_head(base);
                                let label = self.heap.dup_label(q);
                                term = self.dup_ctr(false, label, q, name, arity, base);
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                Term::Mat(id) => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.unpack() {
                            Term::App(app) => {
                                self.whnf(app.second());
                                let arg = self.heap.node(app.second());
                                if let Some(t) = self.app_mat(id, arg) {
                                    stack.pop();
                                    term = t;
                                    continue;
                                }
                                // no arm matched: leave the application stuck
                            }
                            // duplicating a match value: share it (affine-unsafe
                            // but adequate for top-level case fns used once).
                            Term::Dp0(q) | Term::Dp1(q) => {
                                stack.pop();
                                self.subst(q.sub0(), term);
                                self.subst(q.sub1(), term);
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                Term::Wld => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.unpack() {
                            Term::App(_) => {
                                stack.pop();
                                // (* a) => *
                                term = self.heap.wld();
                                continue;
                            }
                            Term::Dp0(q) | Term::Dp1(q) => {
                                stack.pop();
                                term = self.dup_wld(q);
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
                    None => break 'red term,
                    Some(c) => c,
                };
                match cont.unpack() {
                    Term::Dp0(q) => {
                        let label = self.heap.dup_label(q);
                        term = self.dup_head(true, label, q, term);
                        continue 'red;
                    }
                    Term::Dp1(q) => {
                        let label = self.heap.dup_label(q);
                        term = self.dup_head(false, label, q, term);
                        continue 'red;
                    }
                    // application spine node: rebuild and keep unwinding
                    Term::App(app) => {
                        self.heap.set(app.first(), term);
                        term = cont;
                    }
                    _ => unreachable!("non-spine continuation"),
                }
            }
        };
        self.heap.set(ptr, result);
    }

    /// Reduce the term stored at `ptr` to strong (full) normal form, subject to
    /// the policy, writing the result back into `ptr`.
    pub fn normalize(&mut self, ptr: NodePtr) {
        self.whnf(ptr);
        if !self.policy.should_continue() {
            return;
        }
        match self.heap.node(ptr).unpack() {
            Term::Lam(p) => {
                self.normalize(p.second());
            }
            Term::App(p) => {
                self.normalize(p.first());
                self.normalize(p.second());
            }
            Term::Sup(ptr) => {
                self.normalize(ptr.second());
                self.normalize(ptr.third());
            }
            Term::Ctr(base) => {
                let (_, arity) = self.heap.ctr_head(base);
                for i in 0..arity.0 {
                    self.normalize(self.heap.ctr_field(base, i));
                }
            }
            Term::Bop(ptr) => {
                self.normalize(ptr.second());
                self.normalize(ptr.third());
            }
            _ => {}
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
