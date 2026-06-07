//! The [`Executor`]: the interaction-calculus *rules* over a [`Heap`].
//!
//! Evaluation is kept separate from term storage. An `Executor` borrows the
//! shared [`Heap`], [`ExecPolicy`], and [`Extensions`], and provides the
//! individual interaction rules (`app_lam`, `dup_*`, `combine_bop`, …). It does
//! *not* drive reduction itself: the [reduction engine](crate::vm::engine)
//! ([`Eval`](crate::vm::engine::Eval)) owns the single reduction loop, calling
//! these rules as it goes. Some rules are *suspendable* — a binary op or a match
//! must first force its operands, so its driver method
//! ([`bop`](Executor::bop), [`mat`](Executor::mat), [`pri`](Executor::pri))
//! takes the whole [`Eval`](crate::vm::engine::Eval), forks child fibers, and
//! parks itself, resuming once they are WHNF.
//!
//! The executor never inspects a [`Node`]'s packed bits: it reads cells through
//! the heap's typed readers and dispatches on [`Node::unpack`]'s [`Term`]. The
//! interaction rules take the already-unpacked payloads (e.g. the [`PairPtr`] of
//! an application) so a node is unpacked at most once per step.

use crate::vm::heap::{Heap, PatKey, dp0, dp1, var};
use crate::vm::memory::{CtrPtr, DupPtr, NodePtr, PairPtr, TriplePtr};
use crate::vm::term::{Arity, BinaryOp, Label, MatchId, NameId, Node, PrimId, Term};
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// APP-PRI: a primitive applied to (enough of) its arguments.
    AppPri,
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
    /// DUP-PRI: duplicating a primitive (it duplicates to itself).
    DupPri,
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
            InteractionType::AppPri => write!(f, "APP-PRI"),
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
            InteractionType::DupPri => write!(f, "DUP-PRI"),
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
/// Accounting and the stopping condition are taken through `&self` (not
/// `&mut`): once reduction runs across many fibers and worker threads, all of
/// them report interactions into the *same* policy concurrently. Implementations
/// therefore keep their state in atomics (or are stateless). The reduction
/// engine calls [`next_step`](ExecPolicy::next_step) per interaction and checks
/// [`should_continue`](ExecPolicy::should_continue) in its loops.
pub trait ExecPolicy: Sized {
    /// Record that one interaction of the given kind was performed.
    fn next_step(&self, interaction: InteractionType);
    /// Whether a reduction may continue. Checked before performing more work.
    fn should_continue(&self) -> bool;
}

/// A policy that never limits reduction; [`next_step`](ExecPolicy::next_step) is
/// a no-op and reduction always continues. Use this to fully normalize a term.
pub struct UnlimitedBudget;

impl ExecPolicy for UnlimitedBudget {
    #[inline(always)]
    fn next_step(&self, _: InteractionType) {}
    #[inline(always)]
    fn should_continue(&self) -> bool {
        true
    }
}

/// A policy that stops after a fixed number of interactions. The counter is an
/// [`AtomicU64`] so concurrent fibers/workers share one budget.
pub struct FiniteBudget {
    /// Number of interactions performed so far.
    itrs: AtomicU64,
    /// Interaction budget; reduction stops once `itrs` reaches it.
    budget: u64,
}

impl FiniteBudget {
    pub fn new(budget: u64) -> Self {
        FiniteBudget {
            itrs: AtomicU64::new(0),
            budget,
        }
    }

    /// Interactions performed so far.
    pub fn interactions(&self) -> u64 {
        self.itrs.load(Ordering::Relaxed)
    }
}

impl ExecPolicy for FiniteBudget {
    #[inline]
    fn next_step(&self, _: InteractionType) {
        self.itrs.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    fn should_continue(&self) -> bool {
        self.itrs.load(Ordering::Relaxed) < self.budget
    }
}

/// A future produced by an async primitive. It is `'static + Send` so it can
/// outlive the `apply` call and be driven (in the parallel phase) on any worker
/// thread; it therefore cannot borrow the heap. Its output is a finished
/// [`Node`] — in this phase a *leaf* (e.g. [`Term::Num`]/[`Term::Era`], which
/// pack without allocating); building compound results from a future needs the
/// shared heap introduced in the parallel phase.
pub type PrimFuture = Pin<Box<dyn Future<Output = Node> + Send + 'static>>;

/// The outcome of applying a primitive.
pub enum PrimResult {
    /// The primitive finished synchronously; this node re-enters reduction.
    Done(Node),
    /// The primitive started async work; the engine parks the fiber on this
    /// future (never blocking a worker) and resumes with its output.
    Pending(PrimFuture),
}

/// Translates and runs host-provided primitive functions (`%name`).
///
/// Like [`ExecPolicy`], this is a compile-time parameter of the [`Executor`] so
/// programs that use no primitives pay nothing (see [`NoExtensions`]). It plays
/// two roles:
///
/// - **lowering**: [`resolve`](Extensions::resolve) maps a source primitive name
///   to an opaque [`PrimId`], which is stored in a [`Term::Pri`] node;
/// - **execution**: once a `Pri` is applied to [`arity`](Extensions::arity)
///   arguments, the engine reduces those arguments to WHNF (concurrently — so
///   `%f a b` forces both `a` and `b` at once) and calls
///   [`apply`](Extensions::apply).
///
/// `apply` receives the already-WHNF argument nodes (which it owns — the
/// calculus is affine, so it must use or [`erase`](Executor::erase) each via the
/// heap) plus `&mut Heap` to build a result. It returns [`PrimResult::Done`] for
/// synchronous primitives or [`PrimResult::Pending`] for async I/O.
pub trait Extensions: Sized {
    /// Resolve a source primitive name (`%name`) to its [`PrimId`], or `None`
    /// if this extension set defines no such primitive.
    fn resolve(&self, name: &str) -> Option<PrimId>;
    /// How many arguments the primitive consumes before it fires.
    fn arity(&self, id: PrimId) -> usize;
    /// A display name for the primitive, used by the pretty-printer. `None`
    /// falls back to the numeric id.
    fn name(&self, id: PrimId) -> Option<Cow<'_, str>>;
    /// Run the primitive `id` on its already-WHNF `args`, returning the result.
    fn apply(&self, heap: &Heap, id: PrimId, args: &[Node]) -> PrimResult;
}

/// The empty extension set: no primitives. Resolving any name fails (so `%name`
/// is rejected at lowering, as before), and the execution hooks are never
/// reached. Zero-sized, so it is free.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoExtensions;

impl Extensions for NoExtensions {
    #[inline]
    fn resolve(&self, _: &str) -> Option<PrimId> {
        None
    }
    fn arity(&self, _: PrimId) -> usize {
        unreachable!("NoExtensions resolves no primitives")
    }
    fn name(&self, _: PrimId) -> Option<Cow<'_, str>> {
        None
    }
    fn apply(&self, _: &Heap, _: PrimId, _: &[Node]) -> PrimResult {
        unreachable!("NoExtensions resolves no primitives")
    }
}

/// A shared, zero-sized [`NoExtensions`] for executors that need no primitives.
const NO_EXTENSIONS: &NoExtensions = &NoExtensions;

/// Drives reduction over a [`Heap`], guided by an [`ExecPolicy`] and an
/// [`Extensions`] set. All three are held **by shared reference** so the same
/// heap, budget, and primitive table can back many executors running on
/// different worker threads concurrently (see `run_par`); each cell mutation
/// goes through the atomic [`Heap`] API.
pub struct Executor<'a, P: ExecPolicy, X: Extensions = NoExtensions> {
    pub heap: &'a Heap,
    /// Policy governing fuel accounting and the stopping condition.
    pub policy: &'a P,
    /// Host primitive functions reachable from the calculus.
    pub extensions: &'a X,
}

impl<'a, Policy: ExecPolicy> Executor<'a, Policy, NoExtensions> {
    /// An executor with no primitive extensions.
    pub fn new(heap: &'a Heap, policy: &'a Policy) -> Self {
        Executor {
            heap,
            policy,
            extensions: NO_EXTENSIONS,
        }
    }
}

impl<'a, Policy: ExecPolicy, X: Extensions> Executor<'a, Policy, X> {
    /// An executor with the given primitive extension set.
    pub fn with_extensions(heap: &'a Heap, policy: &'a Policy, extensions: &'a X) -> Self {
        Executor {
            heap,
            policy,
            extensions,
        }
    }

    /// Write `val` into `slot` as a substitution (consumes the binder).
    pub(crate) fn subst(&self, slot: NodePtr, val: Node) {
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
    pub fn erase(&self, t: Node) {
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
            | Term::Pri(_)
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
    /// and the lambda is reclaimed when that read happens (in the engine loop).
    pub(crate) fn app_lam(&self, app: PairPtr, lam: PairPtr) -> Node {
        self.policy.next_step(InteractionType::AppLam);
        let arg = self.heap.node(app.second());
        self.subst(lam.first(), arg);
        let body = self.heap.node(lam.second());
        self.heap.free_pair(app);
        body
    }

    /// APP-SUP: `(&L{f,g}) arg`  =>  `!a&L=arg; &L{(f a₀),(g a₁)}`
    pub(crate) fn app_sup(&self, app: PairPtr, slab: Label, sup: TriplePtr) -> Node {
        self.policy.next_step(InteractionType::AppSup);
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
    fn dup_sup(&self, is_dp0: bool, dlab: Label, dp: DupPtr, slab: Label, sup: TriplePtr) -> Node {
        self.policy.next_step(InteractionType::DupSup);
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
    fn dup_lam(&self, is_dp0: bool, dlab: Label, dp: DupPtr, lam: PairPtr) -> Node {
        self.policy.next_step(InteractionType::DupLam);
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
    fn dup_num(&self, dp: DupPtr, num: Node) -> Node {
        self.policy.next_step(InteractionType::DupNum);
        self.subst(dp.sub0(), num);
        self.subst(dp.sub1(), num);
        num
    }

    /// DUP-CTR: duplicate a constructor field by field.
    fn dup_ctr(
        &self,
        is_dp0: bool,
        dlab: Label,
        dp: DupPtr,
        name: NameId,
        arity: Arity,
        ctr: CtrPtr,
    ) -> Node {
        self.policy.next_step(InteractionType::DupCtr);
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
    fn dup_app(&self, is_dp0: bool, dlab: Label, dp: DupPtr, app: PairPtr) -> Node {
        self.policy.next_step(InteractionType::DupApp);
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
    fn dup_wld(&self, dp: DupPtr) -> Node {
        self.policy.next_step(InteractionType::DupWld);
        let w = self.heap.wld();
        self.subst(dp.sub0(), w);
        self.subst(dp.sub1(), w);
        w
    }

    /// APP-USE: `(\_ -> v) arg`  =>  erase `arg`, return `v`.
    pub(crate) fn app_use(&self, app: PairPtr, v: NodePtr) -> Node {
        self.policy.next_step(InteractionType::AppUse);
        let arg = self.heap.node(app.second());
        self.erase(arg);
        self.heap.free_pair(app);
        let body = self.heap.node(v);
        self.heap.free_cell(v);
        body
    }

    /// DUP-USE: duplicating an erasing lambda duplicates its body.
    fn dup_use(&self, is_dp0: bool, dlab: Label, dp: DupPtr, v: NodePtr) -> Node {
        self.policy.next_step(InteractionType::DupUse);
        let body = self.heap.node(v);
        self.heap.free_cell(v);
        let d = self.heap.memory.alloc_dup(dlab, body);
        let (u0, u1) = (self.heap.use_term(dp0(d)), self.heap.use_term(dp1(d)));
        self.subst(dp.sub0(), u0);
        self.subst(dp.sub1(), u1);
        if is_dp0 { u0 } else { u1 }
    }

    /// APP-ERA: `(* arg)`  =>  erase `arg`, return `*`.
    pub(crate) fn app_era(&self, app: PairPtr) -> Node {
        self.policy.next_step(InteractionType::AppEra);
        let arg = self.heap.node(app.second());
        self.erase(arg);
        self.heap.free_pair(app);
        self.heap.era()
    }

    /// DUP-ERA: an erasure duplicates into two erasures.
    fn dup_era(&self, dp: DupPtr) -> Node {
        self.policy.next_step(InteractionType::DupEra);
        let e = self.heap.era();
        self.subst(dp.sub0(), e);
        self.subst(dp.sub1(), e);
        e
    }

    /// Duplicating a stuck head that surfaced at the end of the spine.
    pub(crate) fn dup_head(&self, is_dp0: bool, dlab: Label, dp: DupPtr, head: Node) -> Node {
        match head.unpack() {
            Term::App(app) => self.dup_app(is_dp0, dlab, dp, app),
            Term::Lam(lam) => self.dup_lam(is_dp0, dlab, dp, lam),
            Term::Sup(ptr) => {
                let slab = self.heap.sup_label(ptr);
                self.dup_sup(is_dp0, dlab, dp, slab, ptr)
            }
            Term::Num(_) => self.dup_num(dp, head),
            // DUP-PRI: a primitive duplicates to itself (it is an atom).
            Term::Pri(_) => {
                self.policy.next_step(InteractionType::DupPri);
                self.subst(dp.sub0(), head);
                self.subst(dp.sub1(), head);
                head
            }
            Term::Ctr(base) => {
                let (name, arity) = self.heap.ctr_head(base);
                self.dup_ctr(is_dp0, dlab, dp, name, arity, base)
            }
            Term::Wld => self.dup_wld(dp),
            Term::Era => self.dup_era(dp),
            Term::Use(v) => self.dup_use(is_dp0, dlab, dp, v),
            // DUP-VAR: a free variable duplicates to itself.
            Term::Var(_) => {
                self.policy.next_step(InteractionType::DupVar);
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
    pub(crate) fn app_mat(&self, mat: MatchId, arg: Node) -> Option<Node> {
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
                self.policy.next_step(InteractionType::AppMat);
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
                    self.policy.next_step(InteractionType::AppMat);
                    return Some(value);
                }
                // numeric default receives the number
                let b = self.heap.matches[idx].default?;
                self.policy.next_step(InteractionType::AppMat);
                Some(self.heap.app(b, arg))
            }
            // matching on an erasure yields an erasure
            Term::Era => {
                self.policy.next_step(InteractionType::AppEra);
                Some(self.heap.era())
            }
            _ => None, // stuck
        }
    }

    fn bop_sup_left(&self, op: BinaryOp, bop: TriplePtr, slab: Label, sup: TriplePtr) -> Node {
        self.policy.next_step(InteractionType::BopSup);
        let (a, b) = self.heap.sup_args(sup);
        let rhs = self.heap.node(bop.third());
        let d = self.heap.memory.alloc_dup(slab, rhs);
        let b0 = self.heap.bop(op, a, dp0(d));
        let b1 = self.heap.bop(op, b, dp1(d));
        let result = self.heap.sup(slab, b0, b1);
        self.heap.free_triple(sup);
        result
    }

    fn bop_sup_right(&self, op: BinaryOp, lhs: Node, slab: Label, sup: TriplePtr) -> Node {
        self.policy.next_step(InteractionType::BopSup);
        let (a, b) = self.heap.sup_args(sup);
        let d = self.heap.memory.alloc_dup(slab, lhs);
        let b0 = self.heap.bop(op, dp0(d), a);
        let b1 = self.heap.bop(op, dp1(d), b);
        let result = self.heap.sup(slab, b0, b1);
        self.heap.free_triple(sup);
        result
    }

    /// Combine a binary op whose operands are *already* WHNF (the engine forks
    /// both operand fibers via [`Self::bop`] before resuming here): `Sup`
    /// distributes, `Era` annihilates, two `Num`s compute; anything else is
    /// stuck (`None`, leaving the triple live).
    pub(crate) fn combine_bop(&self, ptr: TriplePtr) -> Option<Node> {
        let op = self.heap.node(ptr.first()).as_op();
        let lhs = self.heap.node(ptr.second());
        if let Term::Sup(sup) = lhs.unpack() {
            let label = self.heap.sup_label(sup);
            let result = self.bop_sup_left(op, ptr, label, sup);
            self.heap.free_triple(ptr);
            return Some(result);
        }
        if let Term::Era = lhs.unpack() {
            self.policy.next_step(InteractionType::BopEra);
            let rhs = self.heap.node(ptr.third());
            self.erase(rhs);
            self.heap.free_triple(ptr);
            return Some(self.heap.era());
        }
        let rhs = self.heap.node(ptr.third());
        if let Term::Sup(sup) = rhs.unpack() {
            let label = self.heap.sup_label(sup);
            let result = self.bop_sup_right(op, lhs, label, sup);
            self.heap.free_triple(ptr);
            return Some(result);
        }
        if let Term::Era = rhs.unpack() {
            self.policy.next_step(InteractionType::BopEra);
            self.erase(lhs);
            self.heap.free_triple(ptr);
            return Some(self.heap.era());
        }
        if let (Term::Num(a), Term::Num(b)) = (lhs.unpack(), rhs.unpack()) {
            self.policy.next_step(InteractionType::BopVal);
            let result = match apply_op(op, a, b) {
                Some(v) => self.heap.num(v),
                None => self.heap.era(),
            };
            self.heap.free_triple(ptr);
            return Some(result);
        }
        None
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
