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
use crate::vm::memory::{CtrPtr, DupPtr, NodePtr, PairPtr, TriplePtr};
use crate::vm::term::{Arity, BinaryOp, Label, MatchId, NameId, Node, Term};

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
    /// APP-USE: an erasing lambda applied to (and erasing) an argument.
    AppUse,
    /// APP-ERA: an erasure in function position, erasing its argument.
    AppEra,
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
    /// DUP-USE: duplicating an erasing lambda.
    DupUse,
    /// DUP-ERA: duplicating an erasure (yields two erasures).
    DupEra,
    /// A binary operation on two numbers.
    BopVal,
    /// A binary operation distributing over a superposed operand.
    BopSup,
    /// A binary operation with an erased operand (yields an erasure).
    BopEra,
}

impl std::fmt::Display for InteractionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InteractionType::AppLam => write!(f, "APP-LAM"),
            InteractionType::AppSup => write!(f, "APP-SUP"),
            InteractionType::AppMat => write!(f, "APP-MAT"),
            InteractionType::AppEra => write!(f, "APP-ERA"),
            InteractionType::AppUse => write!(f, "APP-USE"),
            //
            InteractionType::DupLam => write!(f, "DUP-LAM"),
            InteractionType::DupApp => write!(f, "DUP-APP"),
            InteractionType::DupWld => write!(f, "DUP-WLD"),
            InteractionType::DupNum => write!(f, "DUP-NUM"),
            InteractionType::DupCtr => write!(f, "DUP-CTR"),
            InteractionType::DupVar => write!(f, "DUP-VAR"),
            InteractionType::DupEra => write!(f, "DUP-ERA"),
            InteractionType::DupSup => write!(f, "DUP-SUP"),
            InteractionType::DupUse => write!(f, "DUP-USE"),
            //
            InteractionType::BopVal => write!(f, "BOP-VAL"),
            InteractionType::BopSup => write!(f, "BOP-SUP"),
            InteractionType::BopEra => write!(f, "BOP-ERA"),
        }
    }
}

/// Controls how an [`Executor`] accounts for reduction steps and decides when
/// to stop.
///
/// The executor calls [`stepped`](ExecPolicy::stepped) for every interaction it
/// performs and checks [`should_continue`](ExecPolicy::should_continue) in its
/// reduction loops. Keeping this behind a trait lets callers that don't need
/// fuel accounting (see [`UnlimitedBudget`]) pay nothing for it on the hot path.
pub trait ExecPolicy: Sized {
    /// Record that one interaction of the given kind was performed.
    fn next_step(executor: &mut Executor<Self>, interaction: InteractionType);
    /// Whether a reduction may continue. Checked before performing more work.
    fn should_continue(executor: &Executor<Self>) -> bool;
}

/// A policy that never limits reduction; [`stepped`] is a no-op and reduction
/// always continues. Use this when you want a term fully normalized.
pub struct UnlimitedBudget;

impl ExecPolicy for UnlimitedBudget {
    #[inline(always)]
    fn next_step(_: &mut Executor<Self>, _: InteractionType) {}
    #[inline(always)]
    fn should_continue(_: &Executor<Self>) -> bool {
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
    fn next_step(executor: &mut Executor<Self>, _: InteractionType) {
        executor.policy.itrs += 1;
    }
    #[inline]
    fn should_continue(executor: &Executor<Self>) -> bool {
        executor.policy.itrs < executor.policy.budget
    }
}

/// Drives reduction over a borrowed [`Heap`], guided by an [`ExecPolicy`].
pub struct Executor<'h, P: ExecPolicy> {
    pub heap: &'h mut Heap,
    /// Policy governing fuel accounting and the stopping condition.
    pub policy: P,
}

impl<'h, Policy: ExecPolicy> Executor<'h, Policy> {
    pub fn new(heap: &'h mut Heap, policy: Policy) -> Self {
        Executor { heap, policy }
    }

    /// Write `val` into `slot` as a substitution (consumes the binder).
    fn subst(&mut self, slot: NodePtr, val: Node) {
        self.heap.set(slot, Term::Sub(val).pack());
    }

    /// Recursively delete `t`, as if an erasure were interacting with it: every
    /// allocation reachable from `t` is returned to the arena. This is the
    /// destructive counterpart to the [`Term::Era`] node — `Era` *is* a value
    /// that bubbles through reduction, whereas `erase` actively tears a term
    /// down (e.g. when `APP-USE`/`APP-ERA` discards an argument).
    ///
    /// Binder wires are followed through their substitution slots: erasing a
    /// `Var`/`Dp` whose slot is already substituted erases the substitution and
    /// reclaims the (now fully consumed) binder, mirroring the reclamation
    /// `whnf` performs on a substitution read.
    pub fn erase(&mut self, t: Node) {
        match t.unpack() {
            Term::App(p) | Term::And(p) | Term::Or(p) | Term::Dsu(p) => {
                let (a, b) = self.heap.pair(p);
                self.heap.free_pair(p);
                self.erase(a);
                self.erase(b);
            }
            Term::Lam(p) => {
                // An unapplied lambda value: its bind slot is empty, so erasing
                // the body's `Var` is a no-op; reclaim the node afterwards.
                let body = self.heap.node(p.second());
                self.erase(body);
                self.heap.free_pair(p);
            }
            Term::Use(v) => {
                let body = self.heap.node(v);
                self.heap.free_cell(v);
                self.erase(body);
            }
            Term::Sup(p) | Term::Bop(p) | Term::Ddu(p) => {
                // leading cell is a meta-cell (no allocation of its own)
                let a = self.heap.node(p.second());
                let b = self.heap.node(p.third());
                self.heap.free_triple(p);
                self.erase(a);
                self.erase(b);
            }
            Term::Ctr(c) => {
                let (_, arity) = self.heap.ctr_head(c);
                let fields: Vec<Node> = (0..arity.0).map(|i| self.heap.node(c.field(i))).collect();
                self.heap.free_ctr(c, arity);
                for f in fields {
                    self.erase(f);
                }
            }
            Term::Var(slot) => {
                if let Term::Sub(n) = self.heap.node(slot).unpack() {
                    self.heap.free_pair(PairPtr(slot.0));
                    self.erase(n);
                }
                // else: a free/unapplied binder use — just drop the reference.
            }
            Term::Dp0(q) => {
                if let Term::Sub(n) = self.heap.node(q.sub0()).unpack() {
                    self.heap.free_dup(q);
                    self.erase(n);
                }
                // else: the dup has not fired; the live sibling still needs it.
            }
            Term::Dp1(q) => {
                if let Term::Sub(n) = self.heap.node(q.sub1()).unpack() {
                    self.heap.free_dup(q);
                    self.erase(n);
                }
            }
            Term::Sub(n) => self.erase(n),
            // leaves and table-backed / unallocated nodes: nothing to reclaim
            Term::Num(_)
            | Term::Wld
            | Term::Era
            | Term::Mat(_)
            | Term::Swi(_)
            | Term::LabelMeta(_)
            | Term::NameMeta(_)
            | Term::ArityMeta(_)
            | Term::OpMeta(_)
            | Term::Dup(_)
            | Term::Null => {}
        }
    }

    // ====================================================================
    // Interactions
    // ====================================================================

    /// APP-LAM: `(λx.body) arg`  =>  `x ← arg; body`
    ///
    /// The application node is consumed (freed). The lambda node is *not* freed
    /// here: its bind cell now holds the substitution the bound `Var` will read,
    /// and the lambda is reclaimed when that read happens (see [`Self::whnf`]).
    fn app_lam(&mut self, app: PairPtr, lam: PairPtr) -> Node {
        Policy::next_step(self, InteractionType::AppLam);
        let arg = self.heap.node(app.second());
        self.subst(lam.first(), arg);
        let body = self.heap.node(lam.second());
        self.heap.free_pair(app);
        body
    }

    /// APP-SUP: `(&L{f,g}) arg`  =>  `!a&L=arg; &L{(f a₀),(g a₁)}`
    fn app_sup(&mut self, app: PairPtr, slab: Label, sup: TriplePtr) -> Node {
        Policy::next_step(self, InteractionType::AppSup);
        let arg = self.heap.node(app.second());
        let (f, g) = self.heap.sup_args(sup);
        let d = self.heap.memory.alloc_dup(slab, arg);
        let fa = self.heap.app(f, dp0(d));
        let gb = self.heap.app(g, dp1(d));
        let result = self.heap.sup(slab, fa, gb);
        self.heap.free_pair(app);
        self.heap.free_triple(sup);
        result
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
        Policy::next_step(self, InteractionType::DupSup);
        let (a, b) = self.heap.sup_args(sup);
        if dlab == slab {
            self.subst(dp.sub0(), a);
            self.subst(dp.sub1(), b);
            self.heap.free_triple(sup);
            if is_dp0 { a } else { b }
        } else {
            let da = self.heap.memory.alloc_dup(dlab, a);
            let db = self.heap.memory.alloc_dup(dlab, b);
            let s0 = self.heap.sup(slab, dp0(da), dp0(db));
            let s1 = self.heap.sup(slab, dp1(da), dp1(db));
            self.subst(dp.sub0(), s0);
            self.subst(dp.sub1(), s1);
            self.heap.free_triple(sup);
            if is_dp0 { s0 } else { s1 }
        }
    }

    /// DUP-LAM: duplicating a lambda yields two lambdas with a superposed var.
    fn dup_lam(&mut self, is_dp0: bool, dlab: Label, dp: DupPtr, lam: PairPtr) -> Node {
        Policy::next_step(self, InteractionType::DupLam);
        let body = self.heap.node(lam.second());
        let dg = self.heap.memory.alloc_dup(dlab, body);
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
        Policy::next_step(self, InteractionType::DupNum);
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
        ctr: CtrPtr,
    ) -> Node {
        Policy::next_step(self, InteractionType::DupCtr);
        let n = arity.0 as usize;
        let mut f0 = Vec::with_capacity(n);
        let mut f1 = Vec::with_capacity(n);
        for i in 0..n {
            let field = self.heap.node(ctr.field(i as u64));
            let di = self.heap.memory.alloc_dup(dlab, field);
            f0.push(dp0(di));
            f1.push(dp1(di));
        }
        let ctr0 = self.heap.ctr(name, &f0);
        let ctr1 = self.heap.ctr(name, &f1);
        self.subst(dp.sub0(), ctr0);
        self.subst(dp.sub1(), ctr1);
        self.heap.free_ctr(ctr, arity);
        if is_dp0 { ctr0 } else { ctr1 }
    }

    /// DUP-APP: duplicating a (stuck) application duplicates both sides.
    /// `! d &L = (f x)`  =>  `d₀ ← (f₀ x₀); d₁ ← (f₁ x₁)` with `f`,`x` dup'd.
    fn dup_app(&mut self, is_dp0: bool, dlab: Label, dp: DupPtr, app: PairPtr) -> Node {
        Policy::next_step(self, InteractionType::DupApp);
        let (f, x) = self.heap.pair(app);
        let df = self.heap.memory.alloc_dup(dlab, f);
        let dx = self.heap.memory.alloc_dup(dlab, x);
        let app0 = self.heap.app(dp0(df), dp0(dx));
        let app1 = self.heap.app(dp1(df), dp1(dx));
        self.subst(dp.sub0(), app0);
        self.subst(dp.sub1(), app1);
        self.heap.free_pair(app);
        if is_dp0 { app0 } else { app1 }
    }

    /// DUP-WLD: erasure duplicates into two erasures.
    fn dup_wld(&mut self, dp: DupPtr) -> Node {
        Policy::next_step(self, InteractionType::DupWld);
        let w = self.heap.wld();
        self.subst(dp.sub0(), w);
        self.subst(dp.sub1(), w);
        w
    }

    /// APP-USE: `(\_ -> v) arg`  =>  erase `arg`, return `v`.
    fn app_use(&mut self, app: PairPtr, v: NodePtr) -> Node {
        Policy::next_step(self, InteractionType::AppUse);
        let arg = self.heap.node(app.second());
        self.erase(arg);
        self.heap.free_pair(app);
        let body = self.heap.node(v);
        self.heap.free_cell(v);
        body
    }

    /// DUP-USE: duplicating an erasing lambda duplicates its body.
    fn dup_use(&mut self, is_dp0: bool, dlab: Label, dp: DupPtr, v: NodePtr) -> Node {
        Policy::next_step(self, InteractionType::DupUse);
        let body = self.heap.node(v);
        self.heap.free_cell(v);
        let d = self.heap.memory.alloc_dup(dlab, body);
        let (u0, u1) = (self.heap.use_term(dp0(d)), self.heap.use_term(dp1(d)));
        self.subst(dp.sub0(), u0);
        self.subst(dp.sub1(), u1);
        if is_dp0 { u0 } else { u1 }
    }

    /// APP-ERA: `(* arg)`  =>  erase `arg`, return `*`.
    fn app_era(&mut self, app: PairPtr) -> Node {
        Policy::next_step(self, InteractionType::AppEra);
        let arg = self.heap.node(app.second());
        self.erase(arg);
        self.heap.free_pair(app);
        self.heap.era()
    }

    /// DUP-ERA: an erasure duplicates into two erasures.
    fn dup_era(&mut self, dp: DupPtr) -> Node {
        Policy::next_step(self, InteractionType::DupEra);
        let e = self.heap.era();
        self.subst(dp.sub0(), e);
        self.subst(dp.sub1(), e);
        e
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
            Term::Era => self.dup_era(dp),
            Term::Use(v) => self.dup_use(is_dp0, dlab, dp, v),
            // DUP-VAR: a free variable duplicates to itself.
            Term::Var(_) => {
                Policy::next_step(self, InteractionType::DupVar);
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
            Term::Ctr(ctr) => {
                let (name, arity) = self.heap.ctr_head(ctr);
                let fields: Vec<Node> =
                    (0..arity.0).map(|i| self.heap.node(ctr.field(i))).collect();
                let branch = self.heap.matches[idx]
                    .cases
                    .iter()
                    .find(|(k, _)| *k == PatKey::Ctr(name))
                    .map(|(_, t)| *t)
                    .or(self.heap.matches[idx].default)?;
                Policy::next_step(self, InteractionType::AppMat);
                // the scrutinee is consumed; its fields are reused as arguments
                self.heap.free_ctr(ctr, arity);
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
                    let value = *b;
                    Policy::next_step(self, InteractionType::AppMat);
                    return Some(value);
                }
                // numeric default receives the number
                let b = self.heap.matches[idx].default?;
                Policy::next_step(self, InteractionType::AppMat);
                Some(self.heap.app(b, arg))
            }
            // matching on an erasure yields an erasure
            Term::Era => {
                Policy::next_step(self, InteractionType::AppEra);
                Some(self.heap.era())
            }
            _ => None, // stuck
        }
    }

    /// Try to evaluate a binary op at head. Returns `None` if stuck.
    /// `ptr` is the `[OpMeta, lhs, rhs]` triple.
    fn try_bop(&mut self, op: BinaryOp, ptr: TriplePtr) -> Option<Node> {
        self.whnf_at(ptr.second());
        let lhs = self.heap.node(ptr.second());
        // op distributes over a superposed operand
        if let Term::Sup(sup) = lhs.unpack() {
            let label = self.heap.sup_label(sup);
            let result = self.bop_sup_left(op, ptr, label, sup);
            self.heap.free_triple(ptr);
            return Some(result);
        }
        // an erased operand annihilates the whole operation
        if let Term::Era = lhs.unpack() {
            Policy::next_step(self, InteractionType::BopEra);
            let rhs = self.heap.node(ptr.third());
            self.erase(rhs);
            self.heap.free_triple(ptr);
            return Some(self.heap.era());
        }
        // Budget may have been spent forcing the left operand; if so, leave the
        // operation stuck as `(lhs OP rhs)` rather than charging a second
        // interaction for the op itself without the policy's consent.
        if !Policy::should_continue(self) {
            return None;
        }
        self.whnf_at(ptr.third());
        let rhs = self.heap.node(ptr.third());
        if let Term::Sup(sup) = rhs.unpack() {
            let label = self.heap.sup_label(sup);
            let result = self.bop_sup_right(op, lhs, label, sup);
            self.heap.free_triple(ptr);
            return Some(result);
        }
        if let Term::Era = rhs.unpack() {
            Policy::next_step(self, InteractionType::BopEra);
            self.erase(lhs);
            self.heap.free_triple(ptr);
            return Some(self.heap.era());
        }
        // Likewise, forcing the right operand may have spent the budget; leave
        // `(lhs OP rhs)` stuck rather than charging the op without consent.
        if !Policy::should_continue(self) {
            return None;
        }
        if let (Term::Num(a), Term::Num(b)) = (lhs.unpack(), rhs.unpack()) {
            Policy::next_step(self, InteractionType::BopVal);
            // a failed operation (e.g. division by zero) yields an erasure
            let result = match apply_op(op, a, b) {
                Some(v) => self.heap.num(v),
                None => self.heap.era(),
            };
            self.heap.free_triple(ptr);
            return Some(result);
        }
        None
    }

    fn bop_sup_left(&mut self, op: BinaryOp, bop: TriplePtr, slab: Label, sup: TriplePtr) -> Node {
        Policy::next_step(self, InteractionType::BopSup);
        let (a, b) = self.heap.sup_args(sup);
        let rhs = self.heap.node(bop.third());
        let d = self.heap.memory.alloc_dup(slab, rhs);
        let b0 = self.heap.bop(op, a, dp0(d));
        let b1 = self.heap.bop(op, b, dp1(d));
        let result = self.heap.sup(slab, b0, b1);
        self.heap.free_triple(sup);
        result
    }

    fn bop_sup_right(&mut self, op: BinaryOp, lhs: Node, slab: Label, sup: TriplePtr) -> Node {
        Policy::next_step(self, InteractionType::BopSup);
        let (a, b) = self.heap.sup_args(sup);
        let d = self.heap.memory.alloc_dup(slab, lhs);
        let b0 = self.heap.bop(op, dp0(d), a);
        let b1 = self.heap.bop(op, dp1(d), b);
        let result = self.heap.sup(slab, b0, b1);
        self.heap.free_triple(sup);
        result
    }

    // ====================================================================
    // Reduction
    // ====================================================================

    pub fn whnf_at(&mut self, ptr: NodePtr) {
        let term = self.heap.node(ptr);
        let term = self.whnf(term);
        self.heap.set(ptr, term);
    }

    pub fn whnf(&mut self, mut term: Node) -> Node {
        // Continuations on the spine are always `App` or `Dp0`/`Dp1` nodes.
        let mut stack: Vec<Node> = Vec::new();
        'red: loop {
            if !Policy::should_continue(self) {
                while let Some(cont) = stack.pop() {
                    let slot = match cont.unpack() {
                        Term::App(p) => p.first(),
                        Term::Dp0(q) | Term::Dp1(q) => q.val(),
                        _ => unreachable!("non-spine continuation"),
                    };
                    self.heap.set(slot, term);
                    term = cont;
                }
                break 'red;
            }
            match term.unpack() {
                Term::Var(slot) => {
                    if let Term::Sub(n) = self.heap.node(slot).unpack() {
                        // The binder has been consumed; the lambda node (whose
                        // bind cell is `slot`) is now dead and can be reclaimed.
                        self.heap.free_pair(PairPtr(slot.0));
                        term = n;
                        continue;
                    }
                    // Free variable. If a duplication is forcing it, apply
                    // DUP-VAR: a free variable duplicates to itself on both
                    // sides (this is what collapses dups during readback).
                    if let Some(cont) = stack.last().copied()
                        && let Term::Dp0(q) | Term::Dp1(q) = cont.unpack()
                    {
                        Policy::next_step(self, InteractionType::DupVar);
                        stack.pop();
                        self.subst(q.sub0(), term);
                        self.subst(q.sub1(), term);
                        continue;
                    }
                }
                Term::Dp0(q) => {
                    if let Term::Sub(n) = self.heap.node(q.sub0()).unpack() {
                        // The dup already fired (the sibling projection triggered
                        // it). This is its second interaction: both slots are now
                        // consumed, so reclaim the dup node.
                        self.heap.free_dup(q);
                        term = n;
                        continue;
                    }
                    stack.push(term);
                    term = self.heap.node(q.val());
                    continue;
                }
                Term::Dp1(q) => {
                    if let Term::Sub(n) = self.heap.node(q.sub1()).unpack() {
                        self.heap.free_dup(q);
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
                Term::Use(v) => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.unpack() {
                            Term::App(app) => {
                                stack.pop();
                                term = self.app_use(app, v);
                                continue;
                            }
                            Term::Dp0(q) => {
                                stack.pop();
                                let label = self.heap.dup_label(q);
                                term = self.dup_use(true, label, q, v);
                                continue;
                            }
                            Term::Dp1(q) => {
                                stack.pop();
                                let label = self.heap.dup_label(q);
                                term = self.dup_use(false, label, q, v);
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
                                self.whnf_at(app.second());
                                let arg = self.heap.node(app.second());
                                if let Some(t) = self.app_mat(id, arg) {
                                    stack.pop();
                                    // the `(mat arg)` application node is consumed
                                    self.heap.free_pair(app);
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
                            Term::App(app) => {
                                stack.pop();
                                // (* a) => * , erasing the argument
                                let arg = self.heap.node(app.second());
                                self.erase(arg);
                                self.heap.free_pair(app);
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
                Term::Era => {
                    if let Some(cont) = stack.last().copied() {
                        match cont.unpack() {
                            Term::App(app) => {
                                stack.pop();
                                term = self.app_era(app);
                                continue;
                            }
                            Term::Dp0(q) | Term::Dp1(q) => {
                                stack.pop();
                                term = self.dup_era(q);
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
                    None => break 'red,
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
        }
        term
    }

    /// Reduce the term stored at `ptr` to strong (full) normal form, subject to
    /// the policy, writing the result back into `ptr`.

    pub fn normalize_at(&mut self, ptr: NodePtr) {
        let node = self.heap.node(ptr);
        let node = self.normalize(node);
        self.heap.set(ptr, node);
    }

    pub fn normalize(&mut self, mut node: Node) -> Node {
        node = self.whnf(node);
        if !Policy::should_continue(self) {
            return node;
        }
        match node.unpack() {
            Term::Lam(p) => self.normalize_at(p.second()),
            Term::Use(cell) => self.normalize_at(cell),
            Term::App(p) => {
                self.normalize_at(p.first());
                self.normalize_at(p.second());
            }
            Term::Sup(ptr) => {
                self.normalize_at(ptr.second());
                self.normalize_at(ptr.third());
            }
            Term::Ctr(base) => {
                let (_, arity) = self.heap.ctr_head(base);
                for i in 0..arity.0 {
                    self.normalize_at(self.heap.ctr_field(base, i));
                }
            }
            Term::Bop(ptr) => {
                self.normalize_at(ptr.second());
                self.normalize_at(ptr.third());
            }
            _ => {}
        }
        node
    }
}

/// Apply a binary operator to two numbers. Returns `None` for a failed
/// operation (division or modulo by zero), which the caller turns into an
/// erasure ([`Term::Era`]).
fn apply_op(op: BinaryOp, a: u64, b: u64) -> Option<u64> {
    Some(match op {
        BinaryOp::Add => a.wrapping_add(b),
        BinaryOp::Sub => a.wrapping_sub(b),
        BinaryOp::Mul => a.wrapping_mul(b),
        BinaryOp::Div => return (b != 0).then(|| a / b),
        BinaryOp::Mod => return (b != 0).then(|| a % b),
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
    })
}
