//! The [`Executor`]: interaction-calculus evaluation over a [`Heap`].

use crate::vm::heap::{Heap, PatKey, dp0, dp1, var};
use crate::vm::term::{Arity, BinaryOp, Label, MatchId, NameId, PrimId, Term};
use crate::vm::value::ValueId;
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

/// The kind of interaction performed in a single reduction step.
#[rustfmt::skip]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InteractionType {
    AppLam,
    AppSup,
    AppMat,
    AppUse,
    AppEra,
    AppPri,
    DupLam,
    DupSup,
    DupNum,
    DupCtr,
    DupApp,
    DupWld,
    DupVar,
    DupUse,
    DupEra,
    DupPri,
    DupVal,
    BopVal,
    BopSup,
    BopErr,
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
            InteractionType::DupVal => write!(f, "DUP-VAL"),
            InteractionType::BopVal => write!(f, "BOP-VAL"),
            InteractionType::BopSup => write!(f, "BOP-SUP"),
            InteractionType::BopEra => write!(f, "BOP-ERA"),
        }
    }
}

/// Controls how an [`Executor`] accounts for reduction steps and decides when to
/// stop. Taken through `&self` (atomics) so many fibers/workers share one policy.
pub trait ExecPolicy: Sized {
    fn next_step(&mut self, interaction: InteractionType);
    fn should_continue(&self) -> bool;
}

/// A policy that never limits reduction.
pub struct UnlimitedBudget;

impl ExecPolicy for UnlimitedBudget {
    #[inline(always)]
    fn next_step(&mut self, _: InteractionType) {}
    #[inline(always)]
    fn should_continue(&self) -> bool {
        true
    }
}

/// A policy that stops after a fixed number of interactions.
pub struct FiniteBudget {
    itrs: AtomicU64,
    budget: u64,
}

impl FiniteBudget {
    pub fn new(budget: u64) -> Self {
        FiniteBudget {
            itrs: AtomicU64::new(0),
            budget,
        }
    }
    pub fn interactions(&self) -> u64 {
        self.itrs.load(Ordering::Relaxed)
    }
}

impl ExecPolicy for FiniteBudget {
    #[inline]
    fn next_step(&mut self, _: InteractionType) {
        self.itrs.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    fn should_continue(&self) -> bool {
        self.itrs.load(Ordering::Relaxed) < self.budget
    }
}

// Contains the lifetime of the associated heap, as well
// as the lifetime of the executor in which it is executing.
pub type PrimFuture<'h, 'e> = Pin<Box<dyn Future<Output = Term<'h>> + Send + 'e>>;

/// The outcome of applying a primitive.
pub enum PrimResult<'e, 'h> {
    /// The primitive finished synchronously; this term re-enters reduction.
    Done(Term<'h>),
    /// The primitive started async work; the engine parks on this future and
    /// resumes with its output.
    Pending(PrimFuture<'e, 'h>),
}

/// Translates and runs host-provided primitive functions (`%name`).
pub trait Extensions: Sized {
    fn resolve(&self, name: &str) -> Option<PrimId>;
    fn arity(&self, id: PrimId) -> usize;
    fn name(&self, id: PrimId) -> Option<Cow<'_, str>>;
    fn apply<'e, 'h>(
        &'e mut self,
        heap: &Heap<'h>,
        id: PrimId,
        args: &[Term<'h>],
    ) -> PrimResult<'e, 'h>;
}

/// The empty extension set: no primitives.
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
    fn apply<'e, 'h>(&'e self, _: &Heap<'h>, _: PrimId, _: &[Term<'h>]) -> PrimResult<'e, 'h> {
        unreachable!("NoExtensions resolves no primitives")
    }
}

const NO_EXTENSIONS: &NoExtensions = &NoExtensions;

/// Drives reduction over a branded [`Heap<'h>`]. All three of the heap, policy and
/// extensions are held by shared reference so many executors can run concurrently.
pub struct Executor<'e, 'h, P: ExecPolicy, X: Extensions = NoExtensions> {
    pub heap: &'e Heap<'h>,
    pub extensions: &'e X,
    pub policy: P,
}

/// Result of forcing one side of a duplication.
enum DupForce<'h> {
    /// We won the claim; reduce this (previously cached) value.
    Reduce(Term<'h>),
    /// The dup already fired; this is our projection slot's substitution.
    Fired(Term<'h>),
}

impl<'a, 'h, Policy: ExecPolicy> Executor<'a, 'h, Policy, NoExtensions> {
    pub fn new(heap: &'a Heap<'h>, policy: Policy) -> Self {
        Executor {
            heap,
            policy,
            extensions: NO_EXTENSIONS,
        }
    }
}

impl<'a, 'h, Policy: ExecPolicy, X: Extensions> Executor<'a, 'h, Policy, X> {
    pub fn with_extensions(heap: &'a Heap<'h>, policy: Policy, extensions: &'a X) -> Self {
        Executor {
            heap,
            policy,
            extensions,
        }
    }

    /// Write `val` into a plain binder slot as a substitution.
    pub(crate) fn subst(&mut self, slot: &TermPtr<'h>, val: Term<'h>) {
        self.heap.set(slot, Term::Sub(val.pack()));
    }

    /// Fire a duplication: write both projection slots and publish `DONE`,
    /// returning this side's projection.
    fn dup_fire<const F: bool>(
        &mut self,
        q: DupPtr<'h, F>,
        s0: Term<'h>,
        s1: Term<'h>,
    ) -> Term<'h> {
        self.heap
            .dup_set_sub(q, DupSlot::Sub0, Term::Sub(s0.pack()));
        self.heap
            .dup_set_sub(q, DupSlot::Sub1, Term::Sub(s1.pack()));
        self.heap.dup_publish(q);
        if F { s0 } else { s1 }
    }

    /// Recursively delete `t`: every allocation reachable from it is reclaimed.
    pub fn erase(&mut self, t: Term<'h>) {
        match t {
            Term::App(p) | Term::And(p) | Term::Or(p) | Term::Dsu(p) => {
                let (a, b) = self.heap.pair(p);
                self.heap.free_pair(p);
                self.erase(a);
                self.erase(b);
            }
            Term::Lam(p) => {
                let body = self.heap.node(&p.second());
                self.erase(body);
                self.heap.free_pair(p);
            }
            Term::Use(v) => {
                let body = self.heap.node(&v);
                self.heap.free_cell(v);
                self.erase(body);
            }
            Term::Sup(p) | Term::Bop(p) | Term::Ddu(p) => {
                let a = self.heap.node(&p.second());
                let b = self.heap.node(&p.third());
                self.heap.free_triple(p);
                self.erase(a);
                self.erase(b);
            }
            Term::Ctr(c) => {
                let (_, arity) = self.heap.ctr_head(c);
                let fields: Vec<Term> = (0..arity.get())
                    .map(|i| self.heap.node(&c.field(i)))
                    .collect();
                self.heap.free_ctr(c, arity.get() as usize);
                for f in fields {
                    self.erase(f);
                }
            }
            Term::Var(slot) => {
                if let Term::Sub(n) = self.heap.node(&slot) {
                    // SAFETY: a bound `Var`'s slot is the first cell of its lambda.
                    self.heap
                        .free_pair(unsafe { PairPtr::new_unchecked(slot.addr()) });
                    self.erase(self.heap.view(n));
                }
            }
            Term::Dp0(q) => {
                if let Term::Sub(n) = self.heap.dup_sub(q, DupSlot::Sub0) {
                    self.heap.free_dup(q);
                    self.erase(self.heap.view(n));
                }
            }
            Term::Dp1(q) => {
                if let Term::Sub(n) = self.heap.dup_sub(q, DupSlot::Sub1) {
                    self.heap.free_dup(q);
                    self.erase(self.heap.view(n));
                }
            }
            Term::Sub(n) => self.erase(self.heap.view(n)),
            // a boxed value: drop its pool reference (Arc--).
            Term::Boxed(id) => self.heap.value_drop(id),
            Term::Num(_)
            | Term::Char(_)
            | Term::Pri(_)
            | Term::Wld
            | Term::Era
            | Term::Mat(_)
            | Term::Swi(_)
            | Term::NameMeta(_)
            | Term::ArityMeta(_)
            | Term::OpMeta(_)
            | Term::Null => {}
        }
    }

    // ====================================================================
    // Interactions
    // ====================================================================

    /// APP-LAM: `(λx.body) arg`  =>  `x ← arg; body`
    pub(crate) fn app_lam(&mut self, app: PairPtr<'h>, lam: PairPtr<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::AppLam);
        let arg = self.heap.node(&app.second());
        self.subst(&lam.first(), arg);
        let body = self.heap.node(&lam.second());
        self.heap.free_pair(app);
        body
    }

    /// APP-SUP: `(&L{f,g}) arg`  =>  `!a&L=arg; &L{(f a₀),(g a₁)}`
    pub(crate) fn app_sup(
        &mut self,
        app: PairPtr<'h>,
        slab: Label,
        sup: TriplePtr<'h>,
    ) -> Term<'h> {
        self.policy.next_step(InteractionType::AppSup);
        let arg = self.heap.node(&app.second());
        let (f, g) = self.heap.sup_args(sup);
        let (d0, d1) = self.heap.alloc_dup(slab, arg);
        let fa = self.heap.app(f, dp0(d0));
        let gb = self.heap.app(g, dp1(d1));
        let result = self.heap.sup(slab, fa, gb);
        self.heap.free_pair(app);
        self.heap.free_triple(sup);
        result
    }

    /// DUP-SUP. Same label annihilates; different labels commute.
    fn dup_sup<const F: bool>(
        &mut self,
        dlab: Label,
        dp: DupPtr<'h, F>,
        slab: Label,
        sup: TriplePtr<'h>,
    ) -> Term<'h> {
        self.policy.next_step(InteractionType::DupSup);
        let (a, b) = self.heap.sup_args(sup);
        if dlab == slab {
            self.heap.free_triple(sup);
            self.dup_fire(dp, a, b)
        } else {
            let (da0, da1) = self.heap.alloc_dup(dlab, a);
            let (db0, db1) = self.heap.alloc_dup(dlab, b);
            let s0 = self.heap.sup(slab, dp0(da0), dp0(db0));
            let s1 = self.heap.sup(slab, dp1(da1), dp1(db1));
            self.heap.free_triple(sup);
            self.dup_fire(dp, s0, s1)
        }
    }

    /// DUP-LAM: duplicating a lambda yields two lambdas with a superposed var.
    fn dup_lam<const F: bool>(
        &mut self,
        dlab: Label,
        dp: DupPtr<'h, F>,
        lam: PairPtr<'h>,
    ) -> Term<'h> {
        self.policy.next_step(InteractionType::DupLam);
        let body = self.heap.node(&lam.second());
        let (dg0, dg1) = self.heap.alloc_dup(dlab, body);
        let (lam0, p0) = self.heap.lam(dp0(dg0));
        let (lam1, p1) = self.heap.lam(dp1(dg1));
        let sup_var = self.heap.sup(dlab, var(p0.first()), var(p1.first()));
        self.subst(&lam.first(), sup_var);
        self.dup_fire(dp, lam0, lam1)
    }

    /// DUP-NUM: numbers are duplicated trivially.
    fn dup_num<const F: bool>(&mut self, dp: DupPtr<'h, F>, num: Term<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::DupNum);
        self.dup_fire(dp, num, num)
    }

    /// DUP-CHAR: an unboxed char duplicates by copying.
    fn dup_char<const F: bool>(&mut self, dp: DupPtr<'h, F>, c: Term<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::DupChar);
        self.dup_fire(dp, c, c)
    }

    /// DUP-VAL: duplicating a boxed value clones its pool entry (an `Arc` bump for
    /// `Str`/`Bytes`), giving each projection its own id over the shared buffer.
    fn dup_boxed<const F: bool>(&mut self, dp: DupPtr<'h, F>, id: ValueId) -> Term<'h> {
        self.policy.next_step(InteractionType::DupVal);
        let id2 = self.heap.value_dup(id);
        self.dup_fire(dp, Term::Boxed(id), Term::Boxed(id2))
    }

    /// DUP-CTR: duplicate a constructor field by field.
    fn dup_ctr<const F: bool>(
        &mut self,
        dlab: Label,
        dp: DupPtr<'h, F>,
        name: NameId,
        arity: Arity,
        ctr: CtrPtr<'h>,
    ) -> Term<'h> {
        self.policy.next_step(InteractionType::DupCtr);
        let n = arity.get() as usize;
        let mut f0 = Vec::with_capacity(n);
        let mut f1 = Vec::with_capacity(n);
        for i in 0..n {
            let field = self.heap.node(&ctr.field(i as u64));
            let (di0, di1) = self.heap.alloc_dup(dlab, field);
            f0.push(dp0(di0));
            f1.push(dp1(di1));
        }
        let ctr0 = self.heap.ctr(name, &f0);
        let ctr1 = self.heap.ctr(name, &f1);
        self.heap.free_ctr(ctr, n);
        self.dup_fire(dp, ctr0, ctr1)
    }

    /// DUP-APP: duplicating a (stuck) application duplicates both sides.
    fn dup_app<const F: bool>(
        &mut self,
        dlab: Label,
        dp: DupPtr<'h, F>,
        app: PairPtr<'h>,
    ) -> Term<'h> {
        self.policy.next_step(InteractionType::DupApp);
        let (f, x) = self.heap.pair(app);
        let (df0, df1) = self.heap.alloc_dup(dlab, f);
        let (dx0, dx1) = self.heap.alloc_dup(dlab, x);
        let app0 = self.heap.app(dp0(df0), dp0(dx0));
        let app1 = self.heap.app(dp1(df1), dp1(dx1));
        self.heap.free_pair(app);
        self.dup_fire(dp, app0, app1)
    }

    /// DUP-WLD: erasure duplicates into two erasures.
    fn dup_wld<const F: bool>(&mut self, dp: DupPtr<'h, F>) -> Term<'h> {
        self.policy.next_step(InteractionType::DupWld);
        let w = self.heap.wld();
        self.dup_fire(dp, w, w)
    }

    /// APP-USE: `(\_ -> v) arg`  =>  erase `arg`, return `v`.
    pub(crate) fn app_use(&mut self, app: PairPtr<'h>, v: TermPtr<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::AppUse);
        let arg = self.heap.node(&app.second());
        self.erase(arg);
        self.heap.free_pair(app);
        let body = self.heap.node(&v);
        self.heap.free_cell(v);
        body
    }

    /// DUP-USE: duplicating an erasing lambda duplicates its body.
    fn dup_use<const F: bool>(
        &mut self,
        dlab: Label,
        dp: DupPtr<'h, F>,
        v: TermPtr<'h>,
    ) -> Term<'h> {
        self.policy.next_step(InteractionType::DupUse);
        let body = self.heap.node(&v);
        self.heap.free_cell(v);
        let (d0, d1) = self.heap.alloc_dup(dlab, body);
        let u0 = self.heap.use_term(dp0(d0));
        let u1 = self.heap.use_term(dp1(d1));
        self.dup_fire(dp, u0, u1)
    }

    /// APP-ERA: `(* arg)`  =>  erase `arg`, return `*`.
    pub(crate) fn app_era(&mut self, app: PairPtr<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::AppEra);
        let arg = self.heap.node(&app.second());
        self.erase(arg);
        self.heap.free_pair(app);
        self.heap.era()
    }

    /// DUP-ERA: an erasure duplicates into two erasures.
    fn dup_era<const F: bool>(&self, dp: DupPtr<'h, F>) -> Term<'h> {
        self.policy.next_step(InteractionType::DupEra);
        let e = self.heap.era();
        self.dup_fire(dp, e, e)
    }

    /// Duplicating a stuck head that surfaced at the end of the spine.
    pub(crate) fn dup_head<const F: bool>(
        &self,
        dlab: Label,
        dp: DupPtr<'h, F>,
        head: Term<'h>,
    ) -> Term<'h> {
        match head {
            Term::App(app) => self.dup_app(dlab, dp, app),
            Term::Lam(lam) => self.dup_lam(dlab, dp, lam),
            Term::Sup(ptr) => {
                let slab = self.heap.sup_label(ptr);
                self.dup_sup(dlab, dp, slab, ptr)
            }
            Term::Num(_) => self.dup_num(dp, head),
            Term::Char(_) => self.dup_char(dp, head),
            Term::Boxed(id) => self.dup_boxed(dp, id),
            Term::Pri(_) => {
                self.policy.next_step(InteractionType::DupPri);
                self.dup_fire(dp, head, head)
            }
            Term::Ctr(base) => {
                let (name, arity) = self.heap.ctr_head(base);
                self.dup_ctr(dlab, dp, name, arity, base)
            }
            Term::Wld => self.dup_wld(dp),
            Term::Era => self.dup_era(dp),
            Term::Use(v) => self.dup_use(dlab, dp, v),
            Term::Var(_) => {
                self.policy.next_step(InteractionType::DupVar);
                self.dup_fire(dp, head, head)
            }
            // unexpected stuck head; cache it and leave the dup stuck.
            _ => {
                self.heap.dup_set_val(dp, head);
                dp.dp_term()
            }
        }
    }

    /// APP-MAT / match on a value. `arg` is already WHNF.
    pub(crate) fn app_mat(&self, mat: MatchId, arg: Term<'h>) -> Option<Term<'h>> {
        let idx = mat.get() as usize;
        match arg {
            Term::Ctr(ctr) => {
                let (name, arity) = self.heap.ctr_head(ctr);
                let fields: Vec<Term> = (0..arity.get())
                    .map(|i| self.heap.node(&ctr.field(i)))
                    .collect();
                let data = self.heap.match_data(idx);
                let branch_raw = data
                    .cases
                    .iter()
                    .find(|(k, _)| *k == PatKey::Ctr(name))
                    .map(|(_, t)| *t)
                    .or(data.default)?;
                let branch = self.heap.view_raw(branch_raw);
                self.policy.next_step(InteractionType::AppMat);
                self.heap.free_ctr(ctr, arity.get() as usize);
                let mut b = branch;
                for f in fields {
                    b = self.heap.app(b, f);
                }
                Some(b)
            }
            Term::Num(k) => {
                let data = self.heap.match_data(idx);
                if let Some((_, b)) = data
                    .cases
                    .iter()
                    .find(|(key, _)| *key == PatKey::Num(k.get()))
                {
                    let value = self.heap.view_raw(*b);
                    self.policy.next_step(InteractionType::AppMat);
                    return Some(value);
                }
                let b = self.heap.view_raw(data.default?);
                self.policy.next_step(InteractionType::AppMat);
                Some(self.heap.app(b, arg))
            }
            Term::Era => {
                self.policy.next_step(InteractionType::AppEra);
                Some(self.heap.era())
            }
            _ => None,
        }
    }

    fn bop_sup_left(
        &self,
        op: BinaryOp,
        bop: TriplePtr<'h>,
        slab: Label,
        sup: TriplePtr<'h>,
    ) -> Term<'h> {
        self.policy.next_step(InteractionType::BopSup);
        let (a, b) = self.heap.sup_args(sup);
        let rhs = self.heap.node(&bop.third());
        let (d0, d1) = self.heap.alloc_dup(slab, rhs);
        let b0 = self.heap.bop(op, a, dp0(d0));
        let b1 = self.heap.bop(op, b, dp1(d1));
        let result = self.heap.sup(slab, b0, b1);
        self.heap.free_triple(sup);
        result
    }

    fn bop_sup_right(
        &self,
        op: BinaryOp,
        lhs: Term<'h>,
        slab: Label,
        sup: TriplePtr<'h>,
    ) -> Term<'h> {
        self.policy.next_step(InteractionType::BopSup);
        let (a, b) = self.heap.sup_args(sup);
        let (d0, d1) = self.heap.alloc_dup(slab, lhs);
        let b0 = self.heap.bop(op, dp0(d0), a);
        let b1 = self.heap.bop(op, dp1(d1), b);
        let result = self.heap.sup(slab, b0, b1);
        self.heap.free_triple(sup);
        result
    }

    /// Combine a binary op whose operands are *already* WHNF.
    pub(crate) fn combine_bop(&self, ptr: TriplePtr<'h>) -> Option<Term<'h>> {
        let op = match self.heap.node(&ptr.first()) {
            Term::OpMeta(op) => op,
            _ => unreachable!("a Bop's first cell is its operator meta-cell"),
        };
        let lhs = self.heap.node(&ptr.second());
        if let Term::Sup(sup) = lhs {
            let label = self.heap.sup_label(sup);
            let result = self.bop_sup_left(op, ptr, label, sup);
            self.heap.free_triple(ptr);
            return Some(result);
        }
        if let Term::Era = lhs {
            self.policy.next_step(InteractionType::BopEra);
            let rhs = self.heap.node(&ptr.third());
            self.erase(rhs);
            self.heap.free_triple(ptr);
            return Some(self.heap.era());
        }
        let rhs = self.heap.node(&ptr.third());
        if let Term::Sup(sup) = rhs {
            let label = self.heap.sup_label(sup);
            let result = self.bop_sup_right(op, lhs, label, sup);
            self.heap.free_triple(ptr);
            return Some(result);
        }
        if let Term::Era = rhs {
            self.policy.next_step(InteractionType::BopEra);
            self.erase(lhs);
            self.heap.free_triple(ptr);
            return Some(self.heap.era());
        }
        if let (Term::Num(a), Term::Num(b)) = (lhs, rhs) {
            self.policy.next_step(InteractionType::BopVal);
            let result = match apply_op(op, a.get(), b.get()) {
                Some(v) => self.heap.num(v),
                None => self.heap.era(),
            };
            self.heap.free_triple(ptr);
            return Some(result);
        }
        None
    }
}

/// A reduction future, boxed so the drivers can recurse and `Send` so `run_par`
/// can drive them across worker threads.
type Reduce<'s, T> = Pin<Box<dyn Future<Output = T> + Send + 's>>;

impl<'a, 'h, P: ExecPolicy + Sync, X: Extensions + Sync> Executor<'a, 'h, P, X> {
    /// Reduce the term stored at `ptr` to WHNF, writing the result back.
    pub async fn whnf_at(&self, ptr: TermPtr<'h>) {
        let term = self.heap.node(&ptr);
        let term = self.whnf(term).await;
        self.heap.set(&ptr, term);
    }

    /// Reduce the term at `ptr` to WHNF, writing the result back in place (the
    /// strict callers below re-read `ptr` afterwards). Boxed so it can recurse.
    pub fn sub_whnf_at(&self, ptr: TermPtr<'h>) -> Reduce<'_, ()> {
        Box::pin(async move {
            let term = self.heap.node(&ptr);
            let r = self.whnf(term).await;
            self.heap.set(&ptr, r);
        })
    }

    /// Force one side of a duplication: claim its value (lock-free) or wait for a
    /// racing task to publish the fired projections.
    async fn dup_force<const F: bool>(&self, q: DupPtr<'h, F>) -> DupForce<'h> {
        loop {
            // The raw value word may be a `LOCKED`/`DONE` sentinel, so it is checked
            // before being viewed as a term.
            let v = self.heap.dup_val_word(q);
            match v.raw() {
                DONE => {
                    // The dup fired; the acquire on the value ordered the sub write.
                    match self.heap.dup_sub(q, q.slot()) {
                        Term::Sub(n) => return DupForce::Fired(self.heap.view(n)),
                        _ => unreachable!("a fired dup must have a written sub slot"),
                    }
                }
                LOCKED => {
                    tokio::task::yield_now().await;
                }
                _ => {
                    if self.heap.dup_claim(q, v) {
                        return DupForce::Reduce(self.heap.view(v));
                    }
                    // lost the race; reload and retry.
                }
            }
        }
    }

    /// Reduce `term` to weak head normal form.
    pub async fn whnf(&self, term: Term<'h>) -> Term<'h> {
        let mut term = term;
        // Continuations on the spine are always `App`, `Dp0`, or `Dp1` terms.
        let mut spine: Vec<Term<'h>> = Vec::new();
        loop {
            if !self.policy.should_continue() {
                // Budget spent: unroll the spine, writing the partial result back
                // into each parent application / duplication.
                while let Some(cont) = spine.pop() {
                    match cont {
                        Term::App(p) => self.heap.set(&p.first(), term),
                        Term::Dp0(q) => self.heap.dup_set_val(q, term),
                        Term::Dp1(q) => self.heap.dup_set_val(q, term),
                        _ => unreachable!("non-spine continuation"),
                    }
                    term = cont;
                }
                return term;
            }
            match term {
                Term::Var(slot) => {
                    if let Term::Sub(n) = self.heap.node(&slot) {
                        // binder consumed; reclaim its (now dead) lambda node.
                        // SAFETY: the slot is the first cell of its lambda pair.
                        self.heap
                            .free_pair(unsafe { PairPtr::new_unchecked(slot.addr()) });
                        term = self.heap.view(n);
                        continue;
                    }
                    // free variable: unwind (a DUP cont applies DUP-VAR).
                }
                Term::Dp0(q) => match self.dup_force(q).await {
                    DupForce::Fired(n) => {
                        self.heap.free_dup(q);
                        term = n;
                        continue;
                    }
                    DupForce::Reduce(v) => {
                        spine.push(term);
                        term = v;
                        continue;
                    }
                },
                Term::Dp1(q) => match self.dup_force(q).await {
                    DupForce::Fired(n) => {
                        self.heap.free_dup(q);
                        term = n;
                        continue;
                    }
                    DupForce::Reduce(v) => {
                        spine.push(term);
                        term = v;
                        continue;
                    }
                },
                Term::App(p) => {
                    spine.push(term);
                    term = self.heap.node(&p.first());
                    continue;
                }
                Term::Bop(ptr) => {
                    tokio::join!(
                        self.sub_whnf_at(ptr.second()),
                        self.sub_whnf_at(ptr.third())
                    );
                    if self.policy.should_continue()
                        && let Some(t) = self.combine_bop(ptr)
                    {
                        term = t;
                        continue;
                    }
                }
                Term::Lam(lam) => {
                    if let Some(Term::App(app)) = spine.last().copied() {
                        spine.pop();
                        term = self.app_lam(app, lam);
                        continue;
                    }
                }
                Term::Use(v) => {
                    if let Some(Term::App(app)) = spine.last().copied() {
                        spine.pop();
                        term = self.app_use(app, v);
                        continue;
                    }
                }
                Term::Sup(sup) => {
                    if let Some(Term::App(app)) = spine.last().copied() {
                        spine.pop();
                        let slab = self.heap.sup_label(sup);
                        term = self.app_sup(app, slab, sup);
                        continue;
                    }
                }
                Term::Mat(id) => match spine.last().copied() {
                    Some(Term::App(app)) => {
                        self.sub_whnf_at(app.second()).await;
                        if self.policy.should_continue()
                            && let Some(t) = self.app_mat(id, self.heap.node(&app.second()))
                        {
                            spine.pop();
                            self.heap.free_pair(app);
                            term = t;
                            continue;
                        }
                    }
                    Some(Term::Dp0(q)) => {
                        spine.pop();
                        self.dup_fire(q, term, term);
                        continue;
                    }
                    Some(Term::Dp1(q)) => {
                        spine.pop();
                        self.dup_fire(q, term, term);
                        continue;
                    }
                    _ => (),
                },
                Term::Wld => {
                    if let Some(Term::App(app)) = spine.last().copied() {
                        spine.pop();
                        let arg = self.heap.node(&app.second());
                        self.erase(arg);
                        self.heap.free_pair(app);
                        term = self.heap.wld();
                        continue;
                    }
                }
                Term::Era => {
                    if let Some(Term::App(app)) = spine.last().copied() {
                        spine.pop();
                        term = self.app_era(app);
                        continue;
                    }
                }
                Term::Pri(id) => {
                    let arity = self.extensions.arity(id);
                    let n = spine.len();
                    let ready =
                        arity <= n && spine[n - arity..].iter().all(|c| matches!(c, Term::App(_)));
                    if ready {
                        let mut apps = Vec::with_capacity(arity);
                        for _ in 0..arity {
                            let Term::App(app) = spine.pop().unwrap() else {
                                unreachable!("checked all-App above")
                            };
                            apps.push(app);
                        }
                        for app in &apps {
                            self.sub_whnf_at(app.second()).await;
                        }
                        if !self.policy.should_continue() {
                            for app in apps.into_iter().rev() {
                                spine.push(Term::App(app));
                            }
                        } else {
                            let args: Vec<Term> =
                                apps.iter().map(|a| self.heap.node(&a.second())).collect();
                            self.policy.next_step(InteractionType::AppPri);
                            term = match self.extensions.apply(self.heap, id, &args) {
                                PrimResult::Done(t) => {
                                    for a in &apps {
                                        self.heap.free_pair(*a);
                                    }
                                    t
                                }
                                PrimResult::Pending(fut) => {
                                    for a in &apps {
                                        self.heap.free_pair(*a);
                                    }
                                    for arg in args {
                                        self.erase(arg);
                                    }
                                    // the future yields a heap-independent leaf term.
                                    self.heap.adopt(fut.await)
                                }
                            };
                            continue;
                        }
                    }
                }
                _ => (),
            }
            // Unwind the spine.
            loop {
                match spine.pop() {
                    None => return term,
                    Some(cont) => match cont {
                        Term::Dp0(q) => {
                            let label = self.heap.dup_label(q);
                            term = self.dup_head(label, q, term);
                            break;
                        }
                        Term::Dp1(q) => {
                            let label = self.heap.dup_label(q);
                            term = self.dup_head(label, q, term);
                            break;
                        }
                        Term::App(app) => {
                            self.heap.set(&app.first(), term);
                            term = Term::App(app);
                        }
                        _ => unreachable!("non-spine continuation"),
                    },
                }
            }
        }
    }

    /// Reduce the term stored at `ptr` to strong (full) normal form, in place.
    pub fn normalize_at(&self, ptr: TermPtr<'h>) -> Reduce<'_, ()> {
        Box::pin(async move {
            let node = self.heap.node(&ptr);
            let node = self.normalize(node).await;
            self.heap.set(&ptr, node);
        })
    }

    /// Reduce `node` to strong (full) normal form.
    pub fn normalize(&self, node: Term<'h>) -> Reduce<'_, Term<'h>> {
        Box::pin(async move {
            let node = self.whnf(node).await;
            if !self.policy.should_continue() {
                return node;
            }
            match node {
                Term::Lam(p) => self.normalize_at(p.second()).await,
                Term::Use(cell) => self.normalize_at(cell).await,
                Term::App(p) => {
                    self.normalize_at(p.first()).await;
                    self.normalize_at(p.second()).await;
                }
                Term::Sup(ptr) => {
                    self.normalize_at(ptr.second()).await;
                    self.normalize_at(ptr.third()).await;
                }
                Term::Ctr(base) => {
                    let (_, arity) = self.heap.ctr_head(base);
                    for i in 0..arity.get() {
                        self.normalize_at(self.heap.ctr_field(base, i)).await;
                    }
                }
                Term::Bop(ptr) => {
                    self.normalize_at(ptr.second()).await;
                    self.normalize_at(ptr.third()).await;
                }
                _ => {}
            }
            node
        })
    }
}

/// Apply a binary operator to two numbers. `None` for a failed operation (div/mod
/// by zero), which the caller turns into an erasure.
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
